//! Tauri shell around `craft-core` + `craft-github`: the poll loop, the
//! on-disk cache, tray icon, global shortcut and window management.
//!
//! One background thread owns polling. Every `poll_minutes` (and on demand)
//! it fetches all repositories one at a time, publishing the snapshot to the
//! UI and the cache after each repository lands.
//! Failed repositories keep their previous numbers; staleness is derived from
//! each repository's `fetched_at`, so old data is always visibly old.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Local, Timelike, Utc};
use craft_core::{Cache, Config, Schedule, Snapshot, TokenStatus, WindowState};
use craft_github::{resolve_token, Error as GhError, GitHub, RawActivity};
use serde::Serialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, RunEvent, State, WebviewWindow, WebviewWindowBuilder,
    Wry,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

mod shrink;

const MAIN: &str = "main";
const TRAY: &str = "main";
const EV_SNAPSHOT: &str = "craft:snapshot";
const EV_FOCUS: &str = "craft:focus";
const EV_META: &str = "craft:meta";
/// Tells the window to show its Settings view.
const EV_SETTINGS: &str = "craft:settings";
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
/// Ignore manual refreshes this soon after the previous one.
const MANUAL_DEBOUNCE: i64 = 15;
/// After the window gains focus, wait this long (past any Mission Control
/// exit animation) before checking it for the half-size glitch, and again
/// before acting on it.
const SHRINK_SETTLE: Duration = Duration::from_millis(1500);

pub struct AppState {
    config: Mutex<Config>,
    config_mtime: Mutex<Option<SystemTime>>,
    snapshot: Mutex<Snapshot>,
    cache: Option<Cache>,
    refresh_tx: Mutex<Option<mpsc::Sender<Wake>>>,
    window_state: Mutex<Option<WindowState>>,
    window_dirty: AtomicBool,
    /// The main window is being destroyed to be recreated (see `shrink`);
    /// the app must not exit for want of windows meanwhile.
    rebuilding: AtomicBool,
    /// Show the recreated window once its UI is ready, even if
    /// `start_hidden`.
    show_on_ready: AtomicBool,
    shrink_check_pending: AtomicBool,
    last_rebuild: Mutex<Option<Instant>>,
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

/// What the Settings view edits: the config file's values, with the token
/// left out and `token_set` saying whether the file has one. Always on top
/// is the window's current state, which the toolbar's pin and the tray item
/// change without writing the file.
#[derive(Serialize)]
struct Settings {
    config: Config,
    token_set: bool,
    path: String,
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Result<Settings, String> {
    let path = Config::path().map_err(|e| e.to_string())?;
    let mut config = Config::load_or_create().map_err(|e| e.to_string())?;
    let token_set = !config.github_token.trim().is_empty();
    config.github_token.clear();
    config.window.always_on_top = state.config.lock().unwrap().window.always_on_top;
    Ok(Settings {
        config,
        token_set,
        path: path.display().to_string(),
    })
}

/// Writes the Settings view's config to the file and applies it at once.
/// `base` holds the values the view opened with: only the values the view
/// changed from it replace the file's. With `keep_token` the file keeps its
/// token, which the view never receives; otherwise `config.github_token`
/// replaces it. The work runs off the main thread: applying the config waits
/// for the main thread, and so may the poll thread's reload, which this one
/// waits for.
#[tauri::command]
async fn save_settings(
    app: AppHandle,
    config: Config,
    base: Config,
    keep_token: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || save_and_apply(&app, config, &base, keep_token))
        .await
        .map_err(|e| e.to_string())?
}

fn save_and_apply(
    app: &AppHandle,
    config: Config,
    base: &Config,
    keep_token: bool,
) -> Result<(), String> {
    let path = Config::path().map_err(|e| e.to_string())?;
    let state = app.state::<AppState>();
    let live = state.config.lock().unwrap().window.always_on_top;
    let pinned = pinned_after_save(&config, base, live);
    let config = prepare_save(config, base, keep_token, &path)?;
    config.save_to(&path).map_err(|e| e.to_string())?;
    reload_config_if_changed(app, true);
    let reloaded = state.config.lock().unwrap().window.always_on_top;
    if reloaded != pinned {
        set_pinned(app, pinned);
    }
    // A repo the save added is a hole, so it is fetched before repos that
    // are only due, once the gap or a pause has passed. The step works out
    // the next fetch from the new interval.
    wake_poller(&state, Wake::Saved);
    Ok(())
}

/// Whether the window is pinned after a save: as the view set Always on top
/// when it changed it, otherwise as the window was before the save.
fn pinned_after_save(view: &Config, base: &Config, live: bool) -> bool {
    if view.window.always_on_top != base.window.always_on_top {
        view.window.always_on_top
    } else {
        live
    }
}

/// The config `save_settings` writes: `config`'s changes from `base` applied
/// to the file's values (see [`Config::rebase`]), with the file's token when
/// `keep_token`, and refused when its shortcut does not parse.
fn prepare_save(
    config: Config,
    base: &Config,
    keep_token: bool,
    path: &Path,
) -> Result<Config, String> {
    let current = if path.exists() {
        Config::load_from(path).map_err(|e| e.to_string())?
    } else {
        Config::default()
    };
    let mut merged = config.rebase(base, &current).map_err(|e| e.to_string())?;
    merged.github_token = if keep_token {
        current.github_token
    } else {
        config.github_token
    };
    let accel = merged.shortcuts.toggle_window.trim().to_string();
    if !accel.is_empty() {
        if let Err(e) = accel.parse::<Shortcut>() {
            return Err(format!("shortcuts.toggle_window: {e}"));
        }
    }
    merged.shortcuts.toggle_window = accel;
    Ok(merged)
}

#[tauri::command]
fn set_always_on_top(app: AppHandle, on: bool) {
    set_pinned(&app, on);
}

/// Called once the UI has painted, so the window never flashes empty.
#[tauri::command]
fn window_ready(app: AppHandle, state: State<AppState>) {
    let start_hidden = state.config.lock().unwrap().window.start_hidden;
    if !start_hidden || state.show_on_ready.swap(false, Ordering::Relaxed) {
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
    /// A Settings save changed the config: the next step reads the new repo
    /// list and interval at once, without a refresh and without cutting the
    /// gap or a pause short.
    Saved,
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

/// Whether the config is read again: always when `force`, else when its
/// modification time differs from the one last read.
fn needs_reload(seen: Option<SystemTime>, now: Option<SystemTime>, force: bool) -> bool {
    force || seen != now
}

/// Runs `load` when `needs_reload` says so. `seen` takes `now` only when the
/// load succeeds, so a failed read is tried again even when the file keeps
/// its modification time.
fn read_if_changed<T, E>(
    seen: &mut Option<SystemTime>,
    now: Option<SystemTime>,
    force: bool,
    load: impl FnOnce() -> Result<T, E>,
) -> Option<Result<T, E>> {
    if !needs_reload(*seen, now, force) {
        return None;
    }
    let read = load();
    if read.is_ok() {
        *seen = now;
    }
    Some(read)
}

/// Re-read the config if it changed on disk, or with `force` in any case: a
/// Settings save can leave the file with the modification time it had, on a
/// file system that stores coarse times. A broken file keeps the old config
/// and surfaces the parse error in the window, and the next poll reads it
/// again. The poll thread and a Settings save both call it; holding
/// `config_mtime` throughout makes one wait for the other, so an older read
/// never replaces a newer one.
fn reload_config_if_changed(app: &AppHandle, force: bool) {
    let state = app.state::<AppState>();
    let mut seen = state.config_mtime.lock().unwrap();
    let mtime = config_mtime();
    let Some(read) = read_if_changed(&mut seen, mtime, force, Config::load_or_create) else {
        return;
    };
    match read {
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
        snap.entry(repo).and_then(|e| e.icon_oid.clone())
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
        // A Settings save may have respelled the entry since the fetch began.
        let Some(entry) = snap.entry_mut(repo) else {
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

/// Applies the wakes that arrived together to the poll thread's pacing.
fn woken(
    pacing: &mut Pacing,
    last_manual: &mut Option<DateTime<Utc>>,
    wakes: &[Wake],
    now: DateTime<Utc>,
) {
    let all = wakes.contains(&Wake::All);
    let fresh = last_manual.is_none_or(|t| (now - t).num_seconds() >= MANUAL_DEBOUNCE);
    if all && fresh {
        *last_manual = Some(now);
        pacing.forced_since = Some(now);
    }
    // A manual refresh, of everything or of one row, skips the gap or a
    // token pause, but never a rate-limit backoff. A save skips neither. A
    // row's refresh counts even when it arrives with a Refresh All that came
    // too soon after the last one.
    let manual = (all && fresh) || wakes.contains(&Wake::Repo);
    if manual && pacing.strikes == 0 {
        pacing.not_before = None;
    }
}

fn spawn_poller(app: AppHandle, rx: mpsc::Receiver<Wake>) {
    std::thread::Builder::new()
        .name("craft-poller".into())
        .spawn(move || {
            let mut pacing = Pacing::default();
            let mut last_manual: Option<DateTime<Utc>> = None;
            loop {
                reload_config_if_changed(&app, false);
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
                        let wakes: Vec<Wake> =
                            std::iter::once(first).chain(rx.try_iter()).collect();
                        woken(&mut pacing, &mut last_manual, &wakes, Utc::now());
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

/// Brings the window to front with its Settings view showing.
fn open_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        show_window(&w);
        let _ = w.emit(EV_SETTINGS, ());
    }
}

/// Tray click / global hotkey: hide if focused, otherwise bring to front on
/// the current desktop.
fn toggle_window(app: &AppHandle) {
    let Some(w) = app.get_webview_window(MAIN) else {
        // Only if recreating it failed earlier; try again.
        recreate_main_window(app);
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

fn restore_window(app: &AppHandle, cfg: &Config) {
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

/// Watch for the half-size glitch (see `shrink`) shortly after the window
/// gains focus. It has to persist across two checks, so a Mission Control
/// thumbnail in passing never counts, and then the window is recreated: a
/// fresh WindowServer window starts without the stale scale, whatever left
/// it there.
fn schedule_shrink_check(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.shrink_check_pending.swap(true, Ordering::Relaxed) {
        return;
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("shrink-check".into())
        .spawn(move || {
            let confirmed = (0..2).all(|_| {
                std::thread::sleep(SHRINK_SETTLE);
                shrunk_now(&app)
            });
            if confirmed {
                let later = app.clone();
                let _ = app.run_on_main_thread(move || rebuild_main_window(&later));
            }
            app.state::<AppState>()
                .shrink_check_pending
                .store(false, Ordering::Relaxed);
        });
    if spawned.is_err() {
        state.shrink_check_pending.store(false, Ordering::Relaxed);
    }
}

/// `shrink::window_shrunk` on the main thread; false if it can't tell.
fn shrunk_now(app: &AppHandle) -> bool {
    let (tx, rx) = mpsc::channel();
    let handle = app.clone();
    let asked = app.run_on_main_thread(move || {
        let shrunk = handle
            .get_webview_window(MAIN)
            .is_some_and(|w| shrink::window_shrunk(&w));
        let _ = tx.send(shrunk);
    });
    asked.is_ok() && rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false)
}

/// Destroy the main window, remembering its (correct) AppKit geometry; the
/// `Destroyed` handler recreates it. Main thread.
fn rebuild_main_window(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Ok(mut last) = state.last_rebuild.lock() else {
        return;
    };
    let now = Instant::now();
    if !shrink::may_rebuild(*last, now) {
        return;
    }
    let Some(w) = app.get_webview_window(MAIN) else {
        return;
    };
    *last = Some(now);
    drop(last);
    eprintln!("main window is shown shrunk; recreating it");
    note_window_geometry(app);
    flush_window_state(app);
    state.rebuilding.store(true, Ordering::Relaxed);
    if let Err(e) = w.destroy() {
        eprintln!("could not destroy the shrunk window: {e}");
        state.rebuilding.store(false, Ordering::Relaxed);
    }
}

/// Build the main window afresh from tauri.conf.json, at the remembered
/// geometry. It shows itself once its UI is ready (`window_ready`). Main
/// thread.
fn recreate_main_window(app: &AppHandle) {
    let state = app.state::<AppState>();
    if app.get_webview_window(MAIN).is_none() {
        // Before building: the new UI may report ready at any point after.
        state.show_on_ready.store(true, Ordering::Relaxed);
        let built = app
            .config()
            .app
            .windows
            .iter()
            .find(|c| c.label == MAIN)
            .map(|c| WebviewWindowBuilder::from_config(app, c).and_then(|b| b.build()));
        match built {
            Some(Ok(_)) => {
                let cfg = state.config.lock().map(|c| c.clone()).unwrap_or_default();
                restore_window(app, &cfg);
                apply_window_config(app, &cfg);
            }
            Some(Err(e)) => eprintln!("could not recreate the main window: {e}"),
            None => eprintln!("no {MAIN:?} window in tauri.conf.json"),
        }
        if app.get_webview_window(MAIN).is_none() {
            state.show_on_ready.store(false, Ordering::Relaxed);
        }
    }
    state.rebuilding.store(false, Ordering::Relaxed);
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
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let open_cfg = MenuItem::with_id(app, "open_config", "Open Config File", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Craft Status", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &toggle,
            &refresh,
            &on_top,
            &PredefinedMenuItem::separator(app)?,
            &settings,
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
            "settings" => open_settings(app),
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
        rebuilding: AtomicBool::new(false),
        show_on_ready: AtomicBool::new(false),
        shrink_check_pending: AtomicBool::new(false),
        last_rebuild: Mutex::new(None),
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
            get_settings,
            save_settings,
            set_always_on_top,
            window_ready,
            hide_window,
            log,
            quit,
        ])
        .setup(move |app| {
            let cfg = app.state::<AppState>().config.lock().unwrap().clone();

            #[cfg(target_os = "macos")]
            if cfg.tray.hide_dock_icon {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }

            restore_window(app.handle(), &cfg);
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
                if *focused && window.label() == MAIN {
                    schedule_shrink_check(window.app_handle());
                }
            }
            tauri::WindowEvent::Destroyed if window.label() == MAIN => {
                let app = window.app_handle().clone();
                if app.state::<AppState>().rebuilding.load(Ordering::Relaxed) {
                    // Not from inside this event, nor queued onto the main
                    // thread: a webview built there never loads its page.
                    // From another thread, Tauri builds it on the event loop.
                    let later = app.clone();
                    let spawned = std::thread::Builder::new()
                        .name("recreate-window".into())
                        .spawn(move || recreate_main_window(&later));
                    if spawned.is_err() {
                        app.state::<AppState>()
                            .rebuilding
                            .store(false, Ordering::Relaxed);
                    }
                }
            }
            tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Moved(_) => {
                note_window_geometry(window.app_handle())
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while running Craft Status")
        .run(|app, event| {
            // Destroying the only window asks to exit (code None); a
            // window rebuild must survive that. Quit (code Some) still exits.
            if let RunEvent::ExitRequested {
                code: None, api, ..
            } = event
            {
                if app.state::<AppState>().rebuilding.load(Ordering::Relaxed) {
                    api.prevent_exit();
                }
            }
        });
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
    fn a_save_wakes_the_poller_without_a_refresh() {
        let now = "2026-10-18T04:20:00Z".parse::<DateTime<Utc>>().unwrap();
        let gap = now + CDuration::seconds(REPO_GAP);
        let paced = || Pacing {
            not_before: Some(gap),
            ..Pacing::default()
        };
        let mut last_manual = None;

        let mut pacing = paced();
        woken(&mut pacing, &mut last_manual, &[Wake::Saved], now);
        assert_eq!(pacing.not_before, Some(gap), "the gap stays");
        assert_eq!(pacing.forced_since, None, "nothing is refetched");
        assert_eq!(last_manual, None, "and it is not a manual refresh");

        let mut pacing = paced();
        woken(
            &mut pacing,
            &mut last_manual,
            &[Wake::Saved, Wake::Repo],
            now,
        );
        assert_eq!(pacing.not_before, None, "a row refresh still skips the gap");
        assert_eq!(pacing.forced_since, None);

        let mut pacing = paced();
        woken(
            &mut pacing,
            &mut last_manual,
            &[Wake::Saved, Wake::All],
            now,
        );
        assert_eq!(
            (pacing.not_before, pacing.forced_since, last_manual),
            (None, Some(now), Some(now)),
            "a refresh still refreshes everything"
        );
    }

    #[test]
    fn a_row_refresh_skips_the_gap_beside_a_refresh_all_that_came_too_soon() {
        let now = "2026-10-18T04:20:00Z".parse::<DateTime<Utc>>().unwrap();
        let gap = now + CDuration::seconds(REPO_GAP);
        let mut last_manual = Some(now);
        let mut pacing = Pacing {
            not_before: Some(gap),
            ..Pacing::default()
        };
        woken(
            &mut pacing,
            &mut last_manual,
            &[Wake::All, Wake::Repo],
            now + CDuration::seconds(1),
        );
        assert_eq!(pacing.not_before, None, "the row refresh skips the gap");
        assert_eq!(
            pacing.forced_since, None,
            "the early Refresh All refetches nothing"
        );
        assert_eq!(last_manual, Some(now));
    }

    #[test]
    fn a_save_reloads_the_config_when_its_time_did_not_change() {
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_790_000_000);
        let later = t + std::time::Duration::from_secs(1);
        assert!(!needs_reload(Some(t), Some(t), false), "the poll skips it");
        assert!(needs_reload(Some(t), Some(t), true), "a save reads it");
        assert!(needs_reload(Some(t), Some(later), false));
        assert!(needs_reload(Some(t), None, false), "the file went away");
    }

    #[test]
    fn a_failed_config_read_is_tried_again_at_the_same_time() {
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_790_000_000);
        let mut seen = None;
        let broken = read_if_changed(&mut seen, Some(t), false, || Err::<(), _>("parse error"));
        assert_eq!(broken, Some(Err("parse error")));
        assert_eq!(seen, None, "a failed read does not record the time");
        let fixed = read_if_changed(&mut seen, Some(t), false, || Ok::<_, &str>(()));
        assert_eq!(fixed, Some(Ok(())), "the next poll reads the repaired file");
        assert_eq!(seen, Some(t));
        let skipped = read_if_changed(&mut seen, Some(t), false, || Ok::<_, &str>(()));
        assert_eq!(skipped, None, "a good read is not repeated");
    }

    #[test]
    fn a_save_keeps_the_pin_unless_the_view_changed_it() {
        let pin = |on: bool| {
            let mut c = Config::default();
            c.window.always_on_top = on;
            c
        };
        // The window was pinned from the toolbar, and the view left it as is.
        assert!(pinned_after_save(&pin(true), &pin(true), true));
        assert!(
            !pinned_after_save(&pin(true), &pin(true), false),
            "unpinned since"
        );
        assert!(
            !pinned_after_save(&pin(false), &pin(true), true),
            "unchecked in the view"
        );
        assert!(
            pinned_after_save(&pin(true), &pin(false), false),
            "checked in the view"
        );
    }

    #[test]
    fn a_save_keeps_the_file_token_and_checks_the_shortcut() {
        let dir = std::env::temp_dir().join(format!("craft-app-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let with = |token: &str, accel: &str| Config {
            github_token: token.into(),
            shortcuts: craft_core::config::Shortcuts {
                toggle_window: accel.into(),
            },
            ..Config::default()
        };
        let base = Config::default();
        // No file yet: nothing to keep.
        assert_eq!(
            prepare_save(with("", ""), &base, true, &path)
                .unwrap()
                .github_token,
            ""
        );
        std::fs::write(&path, "github_token = \"secret\"\n").unwrap();
        let kept = prepare_save(with("", " CmdOrCtrl+Shift+K "), &base, true, &path).unwrap();
        assert_eq!(kept.github_token, "secret");
        assert_eq!(kept.shortcuts.toggle_window, "CmdOrCtrl+Shift+K");
        let replaced = prepare_save(with("new", ""), &base, false, &path).unwrap();
        assert_eq!(replaced.github_token, "new");
        let cleared = prepare_save(with("", ""), &base, false, &path).unwrap();
        assert_eq!(cleared.github_token, "");
        let err = prepare_save(with("", "Ctrl+Nope"), &base, true, &path).unwrap_err();
        assert!(err.starts_with("shortcuts.toggle_window: "), "{err}");
        // A file that does not load must not lose its token to a save.
        std::fs::write(&path, "github_token = \"secret\"\npoll_minutes = \"x\"\n").unwrap();
        assert!(prepare_save(with("", ""), &base, true, &path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_save_keeps_file_edits_made_after_settings_opened() {
        let dir = std::env::temp_dir().join(format!("craft-app-rebase-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        // Settings opened on the default stale threshold; the file was then
        // edited to 90 minutes.
        let base = Config::default();
        std::fs::write(&path, "stale_minutes = 90\n").unwrap();
        let mut edits = base.clone();
        edits.window.opacity = 0.8;
        let saved = prepare_save(edits, &base, true, &path).unwrap();
        assert_eq!((saved.stale_minutes, saved.window.opacity), (90.0, 0.8));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tooltip_reports_staleness() {
        let now = Utc::now();
        assert!(tray_tooltip(&Snapshot::default(), now).contains("no data"));
        assert!(tray_tooltip(&snap_with(&[1], now), now).contains("updated"));
        assert!(tray_tooltip(&snap_with(&[1, 30], now), now).contains("STALE"));
    }
}
