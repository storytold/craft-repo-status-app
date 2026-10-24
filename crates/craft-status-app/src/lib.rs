//! Tauri shell around `craft-core` + `craft-github`: the poll loop, the
//! on-disk cache, tray icon, global shortcut and window management.
//!
//! One background thread owns polling. Every `poll_minutes` (and on demand)
//! it fetches all repositories one at a time, publishing the snapshot to the
//! UI and the cache after each repository lands.
//! Failed repositories keep their previous numbers; staleness is derived from
//! each repository's `fetched_at`, so old data is always visibly old.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local, Timelike, Utc};
use craft_core::{Cache, Config, Schedule, Snapshot, TokenStatus, WindowState};
use craft_github::{resolve_token, Error as GhError, GitHub, RawActivity};
use serde::Serialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, State, WebviewWindow, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

mod added;

const MAIN: &str = "main";
const TRAY: &str = "main";
const EV_SNAPSHOT: &str = "craft:snapshot";
const EV_FOCUS: &str = "craft:focus";
const EV_META: &str = "craft:meta";
/// How often the poll thread wakes to check the clock, the config file and
/// the tray tooltip. Wall-clock based, so a sleeping laptop polls on wake.
const TICK: Duration = Duration::from_secs(15);
/// Repositories are fetched one at a time, this long apart, on top of the
/// per-request pacing in craft-github: GitHub's secondary rate limits punish
/// bursts, so a pass over every repo is spread over minutes.
const REPO_GAP: i64 = 20;
/// Wait this long before retrying a repo whose last fetch failed.
const REPO_RETRY: i64 = 10 * 60;
/// The first pause after GitHub rate-limits the app, doubling (up to
/// `BACKOFF_MAX`) while it keeps doing so; never shorter than GitHub asks.
const BACKOFF_MIN: i64 = 5 * 60;
const BACKOFF_MAX: i64 = 60 * 60;
/// Below this many GraphQL points left, pause for `LOW_BUDGET_PAUSE`.
const BUDGET_FLOOR: u64 = 150;
const LOW_BUDGET_PAUSE: i64 = 30 * 60;
/// Below this many GraphQL points left, skip requests nobody asked for.
const OPTIONAL_FLOOR: u64 = 1_500;
/// Ignore manual refreshes this soon after the previous one.
const MANUAL_DEBOUNCE: i64 = 15;

pub struct AppState {
    config: Mutex<Config>,
    config_mtime: Mutex<Option<SystemTime>>,
    snapshot: Mutex<Snapshot>,
    cache: Option<Cache>,
    refresh_tx: Mutex<Option<mpsc::Sender<Wake>>>,
    window_state: Mutex<Option<WindowState>>,
    window_dirty: AtomicBool,
}

struct TrayItems {
    always_on_top: CheckMenuItem<Wry>,
}

/// Static-ish facts the UI needs besides the snapshot.
#[derive(Serialize, Clone)]
struct Meta {
    config_path: String,
    cache_path: String,
    opacity: f64,
    always_on_top: bool,
    platform: &'static str,
    version: &'static str,
    shortcut: String,
}

fn meta(app: &AppHandle) -> Meta {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap();
    Meta {
        config_path: Config::path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        cache_path: state
            .cache
            .as_ref()
            .map(|c| c.path().display().to_string())
            .unwrap_or_default(),
        opacity: cfg.window.opacity,
        always_on_top: cfg.window.always_on_top,
        platform: std::env::consts::OS,
        version: env!("CARGO_PKG_VERSION"),
        shortcut: cfg.shortcuts.toggle_window.clone(),
    }
}

// ───────────────────────────── commands ─────────────────────────────

#[tauri::command]
fn get_snapshot(state: State<AppState>) -> Snapshot {
    state.snapshot.lock().unwrap().clone()
}

#[tauri::command]
fn get_meta(app: AppHandle) -> Meta {
    meta(&app)
}

#[tauri::command]
fn refresh_now(state: State<AppState>) {
    wake_poller(&state, Wake::All);
}

/// Refetch one repo, leaving the others to the schedule.
#[tauri::command]
fn refresh_repo(app: AppHandle, state: State<AppState>, repo: String) {
    if state.snapshot.lock().unwrap().request(&repo) {
        emit_snapshot(&app);
        wake_poller(&state, Wake::Repo);
    }
}

/// Only GitHub links leave the app, and they go to the default browser.
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !url.starts_with("https://github.com/") {
        return Err(format!("refusing to open {url:?}"));
    }
    open_external(&url)
}

#[tauri::command]
fn open_config() -> Result<(), String> {
    let path = Config::path().map_err(|e| e.to_string())?;
    open_external(&path.display().to_string())
}

#[tauri::command]
fn set_always_on_top(app: AppHandle, on: bool) {
    set_pinned(&app, on);
}

/// Called once the UI has painted, so the window never flashes empty.
#[tauri::command]
fn window_ready(app: AppHandle, state: State<AppState>) {
    let start_hidden = state.config.lock().unwrap().window.start_hidden;
    if !start_hidden {
        if let Some(w) = app.get_webview_window(MAIN) {
            show_window(&w);
        }
    }
}

#[tauri::command]
fn hide_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.hide();
    }
}

#[tauri::command]
fn log(msg: String) {
    eprintln!("[ui] {msg}");
}

#[tauri::command]
fn quit(app: AppHandle) {
    flush_window_state(&app);
    app.exit(0);
}

// ───────────────────────────── polling ─────────────────────────────

/// Why the poll thread is woken early.
#[derive(Clone, Copy, PartialEq)]
enum Wake {
    /// Refresh everything (the toolbar button, ⌘R).
    All,
    /// A row's refresh button queued its repo in `Snapshot::requested`.
    Repo,
}

fn wake_poller(state: &AppState, why: Wake) {
    if let Some(tx) = state.refresh_tx.lock().unwrap().as_ref() {
        let _ = tx.send(why);
    }
}

fn emit_snapshot(app: &AppHandle) {
    let snap = app.state::<AppState>().snapshot.lock().unwrap().clone();
    let _ = app.emit(EV_SNAPSHOT, &snap);
}

fn config_mtime() -> Option<SystemTime> {
    Config::path()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

/// Re-read the config if it changed on disk. A broken file keeps the old
/// config and surfaces the parse error in the window.
fn reload_config_if_changed(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mtime = config_mtime();
    if *state.config_mtime.lock().unwrap() == mtime {
        return;
    }
    *state.config_mtime.lock().unwrap() = mtime;
    match Config::load_or_create() {
        Ok(cfg) => {
            {
                let mut snap = state.snapshot.lock().unwrap();
                snap.sync_repos(&cfg.repos);
                snap.poll_secs = cfg.poll_secs();
                snap.stale_after_secs = cfg.stale_secs();
                if snap
                    .error
                    .as_deref()
                    .is_some_and(|e| e.starts_with("Config"))
                {
                    snap.error = None;
                }
            }
            *state.config.lock().unwrap() = cfg.clone();
            apply_window_config(app, &cfg);
            let _ = app.emit(EV_META, meta(app));
        }
        Err(e) => {
            state.snapshot.lock().unwrap().error = Some(format!("Config error: {e}"));
        }
    }
    emit_snapshot(app);
}

/// The poll thread's own bookkeeping between steps.
#[derive(Default)]
struct Pacing {
    /// No request before this: the gap after a fetch, or a pause.
    not_before: Option<DateTime<Utc>>,
    /// Fetches in a row that GitHub rate-limited, for the backoff.
    strikes: u32,
    /// The last manual refresh, while repos older than it remain.
    forced_since: Option<DateTime<Utc>>,
}

/// Fetch `repo` into the snapshot. Errors about this repo land on its row;
/// a missing or rejected token doesn't, since it isn't the repo's fault.
fn fetch_into_snapshot(app: &AppHandle, cfg: &Config, repo: &str) -> Result<(), GhError> {
    let state = app.state::<AppState>();
    let token = resolve_token(&cfg.github_token);
    state.snapshot.lock().unwrap().token = Some(TokenStatus {
        source: token.as_ref().ok().map(|t| t.source.to_string()),
        rejected: false,
    });
    let gh = token.and_then(|t| GitHub::new(t.value))?;

    let now = Utc::now();
    let raw_dir = state.cache.as_ref().map(|c| c.dir().to_path_buf());
    let mut raw = raw_dir
        .as_deref()
        .map(|d| RawActivity::load(d, repo))
        .unwrap_or_default();
    let result = gh.fetch_repo(repo, now, &mut raw);
    if let (Ok(_), Some(dir)) = (&result, &raw_dir) {
        if let Err(e) = raw.save(dir, repo) {
            eprintln!("{repo}: could not save raw cache: {e}");
        }
    }
    // The icon only changes with its blob, so most fetches skip the download.
    let held_icon = {
        let snap = state.snapshot.lock().unwrap();
        snap.repos
            .iter()
            .find(|e| e.repo == repo)
            .and_then(|e| e.icon_oid.clone())
    };
    let icon = match &result {
        Ok((stats, _)) => stats
            .icon_oid
            .as_ref()
            .filter(|oid| Some(*oid) != held_icon.as_ref())
            .map(|oid| (oid.clone(), gh.icon(repo, oid))),
        Err(_) => None,
    };

    let mut snap = state.snapshot.lock().unwrap();
    let outcome = {
        let Some(entry) = snap.repos.iter_mut().find(|e| e.repo == repo) else {
            return Ok(());
        };
        entry.last_attempt_at = Some(now);
        match result {
            Ok((stats, budget)) => {
                match icon {
                    Some((oid, Ok(uri))) => {
                        entry.icon = Some(uri);
                        entry.icon_oid = Some(oid);
                    }
                    Some((_, Err(e))) => eprintln!("{repo}: icon: {e}"),
                    None if stats.icon_oid.is_none() => {
                        entry.icon = None;
                        entry.icon_oid = None;
                    }
                    None => {}
                }
                entry.stats = Some(stats);
                entry.fetched_at = Some(now);
                entry.error = None;
                entry.inaccessible = false;
                Ok(budget.graphql_remaining)
            }
            Err(GhError::NotFound) => {
                entry.stats = None;
                entry.icon = None;
                entry.icon_oid = None;
                entry.fetched_at = Some(now);
                entry.error = None;
                entry.inaccessible = true;
                Ok(None)
            }
            Err(GhError::Unauthorized) => Err(GhError::Unauthorized),
            Err(e) => {
                entry.error = Some(e.to_string());
                Err(e)
            }
        }
    };
    match outcome {
        Ok(left) => {
            if left.is_some() {
                snap.rate_limit_remaining = left;
            }
            Ok(())
        }
        Err(GhError::Unauthorized) => {
            if let Some(t) = snap.token.as_mut() {
                t.rejected = true;
            }
            Err(GhError::Unauthorized)
        }
        Err(e) => Err(e),
    }
}

/// When a rate limit GitHub reported (`RateLimited`'s text) lifts.
fn rate_limit_end(until: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(until) {
        return Some(t.with_timezone(&Utc));
    }
    // "the top of the hour": GraphQL's hourly budget resets on the hour.
    (until == "the top of the hour").then(|| {
        let next = now + chrono::Duration::hours(1);
        next.with_minute(0)
            .and_then(|t| t.with_second(0))
            .unwrap_or(next)
    })
}

fn clock(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M").to_string()
}

/// Fetch the most overdue repository, if one is due and pacing allows, and
/// decide how long to wait before the next.
fn step(app: &AppHandle, pacing: &mut Pacing) {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let schedule = |pacing: &Pacing| Schedule {
        refresh_secs: cfg.poll_secs(),
        retry_secs: REPO_RETRY,
        forced_since: pacing.forced_since,
    };
    let now = Utc::now();
    let due = {
        let mut snap = state.snapshot.lock().unwrap();
        snap.sync_repos(&cfg.repos);
        snap.poll_secs = cfg.poll_secs();
        snap.stale_after_secs = cfg.stale_secs();
        let paused = pacing.not_before.is_some_and(|t| now < t);
        // Rows refreshed by hand go first.
        let due = (!paused)
            .then(|| {
                snap.take_requested().or_else(|| {
                    snap.next_due(now, &schedule(pacing))
                        .map(|e| e.repo.clone())
                })
            })
            .flatten();
        snap.refreshing = due.is_some();
        snap.fetching = due.clone();
        due
    };
    if let Some(repo) = &due {
        emit_snapshot(app);
        let result = fetch_into_snapshot(app, &cfg, repo);
        let now = Utc::now();
        let mut snap = state.snapshot.lock().unwrap();
        let (pause, error) = match result {
            Err(GhError::RateLimited(until)) => {
                pacing.strikes += 1;
                let backoff = (BACKOFF_MIN << (pacing.strikes - 1).min(8)).min(BACKOFF_MAX);
                let mut resume = now + chrono::Duration::seconds(backoff);
                if let Some(end) = rate_limit_end(&until, now) {
                    resume = resume.max(end);
                }
                let msg = format!("Rate limited by GitHub; pausing until {}", clock(resume));
                (resume - now, Some(msg))
            }
            Err(GhError::Unauthorized) => {
                let from = snap
                    .token
                    .as_ref()
                    .and_then(|t| t.source.clone())
                    .map_or(String::new(), |s| format!(" from {s}"));
                let msg = format!(
                    "GitHub rejected the token{from} (401): it may have expired or been revoked; replace it or unset it"
                );
                (chrono::Duration::seconds(cfg.poll_secs()), Some(msg))
            }
            Err(e @ GhError::NoToken) => (
                chrono::Duration::seconds(cfg.poll_secs()),
                Some(e.to_string()),
            ),
            _ => {
                pacing.strikes = 0;
                if snap.rate_limit_remaining.is_some_and(|r| r < BUDGET_FLOOR) {
                    let resume = now + chrono::Duration::seconds(LOW_BUDGET_PAUSE);
                    let msg = format!("GitHub API budget low; pausing until {}", clock(resume));
                    (resume - now, Some(msg))
                } else {
                    (chrono::Duration::seconds(REPO_GAP), None)
                }
            }
        };
        pacing.not_before = Some(now + pause);
        snap.refreshing = false;
        snap.fetching = None;
        // Keep a config error up until the file is fixed.
        if !snap
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("Config"))
        {
            snap.error = error;
        }
        if snap.next_due(now, &schedule(pacing)).is_none() {
            pacing.forced_since = None;
            if snap.repos.iter().all(|e| e.error.is_none()) && snap.error.is_none() {
                snap.last_full_refresh_at = Some(now);
            }
        }
        if let Some(cache) = &state.cache {
            if let Err(e) = cache.save(&snap) {
                eprintln!("could not save cache: {e}");
            }
        }
    }
    {
        let mut snap = state.snapshot.lock().unwrap();
        let next = snap
            .next_due_at(&schedule(pacing))
            .map(|t| t.max(Utc::now()))
            .map(|t| pacing.not_before.map_or(t, |p| t.max(p)));
        if snap.next_refresh_at == next && due.is_none() {
            return;
        }
        snap.next_refresh_at = next;
    }
    emit_snapshot(app);
}

fn spawn_poller(app: AppHandle, rx: mpsc::Receiver<Wake>) {
    std::thread::Builder::new()
        .name("craft-poller".into())
        .spawn(move || {
            let mut pacing = Pacing::default();
            let mut last_manual: Option<DateTime<Utc>> = None;
            let mut found = added::Found::default();
            loop {
                reload_config_if_changed(&app);
                added::discover(&app, &mut pacing, &mut found);
                step(&app, &mut pacing);
                // Staleness changes with the clock alone.
                update_tray(&app);
                flush_window_state(&app);
                let until_next = pacing
                    .not_before
                    .and_then(|t| (t - Utc::now()).to_std().ok())
                    .filter(|d| !d.is_zero())
                    .map_or(TICK, |d| d.min(TICK));
                match rx.recv_timeout(until_next) {
                    Ok(first) => {
                        let all = std::iter::once(first)
                            .chain(rx.try_iter())
                            .any(|w| w == Wake::All);
                        let now = Utc::now();
                        let fresh =
                            last_manual.is_none_or(|t| (now - t).num_seconds() >= MANUAL_DEBOUNCE);
                        if all && fresh {
                            last_manual = Some(now);
                            pacing.forced_since = Some(now);
                        }
                        // A manual refresh, of everything or of one row, skips
                        // the gap or a token pause, but never a rate-limit backoff.
                        if (fresh || !all) && pacing.strikes == 0 {
                            pacing.not_before = None;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(TICK),
                }
            }
        })
        .expect("spawn poll thread");
}

// ───────────────────────────── window & tray ─────────────────────────────

fn open_external(target: &str) -> Result<(), String> {
    use std::process::Command;
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(target);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]).arg(target);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(target);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn show_window(w: &WebviewWindow) {
    let _ = w.show();
    let _ = w.unminimize();
    let _ = w.set_focus();
    let _ = w.emit(EV_FOCUS, true);
}

/// Tray click / global hotkey: hide if focused, otherwise bring to front on
/// the current desktop.
fn toggle_window(app: &AppHandle) {
    let Some(w) = app.get_webview_window(MAIN) else {
        return;
    };
    if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) {
        let _ = w.hide();
    } else {
        show_window(&w);
    }
}

fn set_pinned(app: &AppHandle, on: bool) {
    let state = app.state::<AppState>();
    state.config.lock().unwrap().window.always_on_top = on;
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.set_always_on_top(on);
    }
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.always_on_top.set_checked(on);
    }
    let _ = app.emit(EV_META, meta(app));
}

fn apply_window_config(app: &AppHandle, cfg: &Config) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.set_always_on_top(cfg.window.always_on_top);
        let _ = w.set_visible_on_all_workspaces(cfg.tray.visible_on_all_workspaces);
        #[cfg(not(target_os = "macos"))]
        let _ = w.set_skip_taskbar(true);
    }
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.always_on_top.set_checked(cfg.window.always_on_top);
    }
    apply_shortcut(app, cfg);
}

fn apply_shortcut(app: &AppHandle, cfg: &Config) {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let accel = cfg.shortcuts.toggle_window.trim();
    if accel.is_empty() {
        return;
    }
    let result = gs.on_shortcut(accel, |app, _shortcut, event| {
        if event.state() == ShortcutState::Pressed {
            toggle_window(app);
        }
    });
    if let Err(e) = result {
        eprintln!("Could not register shortcut {accel:?}: {e}");
    }
}

/// "Craft Status — updated 14:05" or "… — STALE (updated 13:20)".
fn tray_tooltip(snap: &Snapshot, now: DateTime<Utc>) -> String {
    let when = snap
        .oldest_data_at()
        .map(|t| t.with_timezone(&Local).format("%H:%M").to_string());
    match (when, snap.is_stale(now)) {
        (None, _) => "Craft Status — no data yet".into(),
        (Some(w), true) => format!("Craft Status — STALE (data from {w})"),
        (Some(w), false) => format!("Craft Status — updated {w}"),
    }
}

fn update_tray(app: &AppHandle) {
    let tip = tray_tooltip(
        &app.state::<AppState>().snapshot.lock().unwrap(),
        Utc::now(),
    );
    if let Some(tray) = app.tray_by_id(TRAY) {
        let _ = tray.set_tooltip(Some(tip));
    }
}

fn note_window_geometry(app: &AppHandle) {
    let Some(w) = app.get_webview_window(MAIN) else {
        return;
    };
    let (Ok(pos), Ok(size)) = (w.outer_position(), w.outer_size()) else {
        return;
    };
    if size.width == 0 || size.height == 0 || !w.is_visible().unwrap_or(false) {
        return;
    }
    let state = app.state::<AppState>();
    *state.window_state.lock().unwrap() = Some(WindowState {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
    });
    state.window_dirty.store(true, Ordering::Relaxed);
}

fn flush_window_state(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.window_dirty.swap(false, Ordering::Relaxed) {
        return;
    }
    let saved = *state.window_state.lock().unwrap();
    if let Some(ws) = saved {
        if let Err(e) = ws.save() {
            eprintln!("could not save window state: {e}");
        }
    }
}

fn restore_window(app: &tauri::App, cfg: &Config) {
    let Some(w) = app.get_webview_window(MAIN) else {
        return;
    };
    let monitors: Vec<(i32, i32, u32, u32)> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            (
                m.position().x,
                m.position().y,
                m.size().width,
                m.size().height,
            )
        })
        .collect();
    if let Some(ws) = WindowState::load().filter(|ws| ws.visible_on(&monitors)) {
        let _ = w.set_size(tauri::PhysicalSize::new(ws.width, ws.height));
        let _ = w.set_position(tauri::PhysicalPosition::new(ws.x, ws.y));
        *app.state::<AppState>().window_state.lock().unwrap() = Some(ws);
    } else {
        let _ = w.set_size(LogicalSize::new(cfg.window.width, cfg.window.height));
        let _ = w.center();
    }
}

fn build_tray(app: &tauri::App, cfg: &Config) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "Show / Hide", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "Refresh Now", true, None::<&str>)?;
    let on_top = CheckMenuItem::with_id(
        app,
        "on_top",
        "Always on Top",
        true,
        cfg.window.always_on_top,
        None::<&str>,
    )?;
    let open_cfg = MenuItem::with_id(app, "open_config", "Edit Config…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Craft Status", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &toggle,
            &refresh,
            &on_top,
            &PredefinedMenuItem::separator(app)?,
            &open_cfg,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    app.manage(TrayItems {
        always_on_top: on_top.clone(),
    });

    #[cfg(target_os = "macos")]
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    #[cfg(not(target_os = "macos"))]
    let icon = app
        .default_window_icon()
        .cloned()
        .unwrap_or(tauri::image::Image::from_bytes(include_bytes!(
            "../icons/32x32.png"
        ))?);

    TrayIconBuilder::with_id(TRAY)
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Craft Status")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle" => toggle_window(app),
            "refresh" => wake_poller(&app.state::<AppState>(), Wake::All),
            "on_top" => {
                let checked = app
                    .try_state::<TrayItems>()
                    .and_then(|t| t.always_on_top.is_checked().ok())
                    .unwrap_or(false);
                set_pinned(app, checked);
            }
            "open_config" => {
                let _ = open_config();
            }
            "quit" => {
                flush_window_state(app);
                app.exit(0)
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn run() {
    let config = Config::load_or_create().unwrap_or_else(|e| {
        eprintln!("Could not load ~/.craft_status_config.toml ({e}); using defaults");
        Config::default()
    });
    let cache = Cache::default_location().ok();
    let mut snapshot = cache.as_ref().and_then(Cache::load).unwrap_or_default();
    snapshot.sync_repos(&config.repos);
    snapshot.poll_secs = config.poll_secs();
    snapshot.stale_after_secs = config.stale_secs();

    let (tx, rx) = mpsc::channel();
    let state = AppState {
        config: Mutex::new(config),
        config_mtime: Mutex::new(config_mtime()),
        snapshot: Mutex::new(snapshot),
        cache,
        refresh_tx: Mutex::new(Some(tx)),
        window_state: Mutex::new(None),
        window_dirty: AtomicBool::new(false),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_meta,
            refresh_now,
            refresh_repo,
            open_url,
            open_config,
            set_always_on_top,
            window_ready,
            hide_window,
            log,
            quit,
            added::keep_repo,
            added::hide_repo,
        ])
        .setup(move |app| {
            let cfg = app.state::<AppState>().config.lock().unwrap().clone();

            #[cfg(target_os = "macos")]
            if cfg.tray.hide_dock_icon {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }

            restore_window(app, &cfg);
            build_tray(app, &cfg)?;
            apply_window_config(app.handle(), &cfg);
            update_tray(app.handle());
            spawn_poller(app.handle().clone(), rx);
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                let close_to_tray = window
                    .state::<AppState>()
                    .config
                    .lock()
                    .unwrap()
                    .tray
                    .close_to_tray;
                flush_window_state(window.app_handle());
                if close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            tauri::WindowEvent::Focused(focused) => {
                let _ = window.emit(EV_FOCUS, *focused);
            }
            tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Moved(_) => {
                note_window_geometry(window.app_handle())
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running Craft Status");
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as CDuration;

    fn snap_with(ages_mins: &[i64], now: DateTime<Utc>) -> Snapshot {
        let mut s = Snapshot {
            stale_after_secs: 15 * 60,
            ..Default::default()
        };
        let names: Vec<String> = (0..ages_mins.len()).map(|i| format!("o/r{i}")).collect();
        s.sync_repos(&names);
        for (e, m) in s.repos.iter_mut().zip(ages_mins) {
            e.fetched_at = Some(now - CDuration::minutes(*m));
        }
        s
    }

    #[test]
    fn rate_limits_end_when_github_says() {
        let now = "2026-10-18T04:20:00Z".parse::<DateTime<Utc>>().unwrap();
        let at = rate_limit_end("2026-10-18T04:58:36.4+00:00", now).unwrap();
        assert_eq!(at.to_rfc3339(), "2026-10-18T04:58:36.400+00:00");
        let hour = rate_limit_end("the top of the hour", now).unwrap();
        assert_eq!(hour.to_rfc3339(), "2026-10-18T05:00:00+00:00");
        assert!(rate_limit_end("later", now).is_none());
    }

    #[test]
    fn tooltip_reports_staleness() {
        let now = Utc::now();
        assert!(tray_tooltip(&Snapshot::default(), now).contains("no data"));
        assert!(tray_tooltip(&snap_with(&[1], now), now).contains("updated"));
        assert!(tray_tooltip(&snap_with(&[1, 30], now), now).contains("STALE"));
    }
}
