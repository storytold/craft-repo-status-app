//! Tauri shell around `craft-core` + `craft-github`: the poll loop, the
//! on-disk cache, tray icon, global shortcut and window management.
//!
//! One background thread owns polling. Every `poll_minutes` (and on demand)
//! it fetches all repositories with a few worker threads, publishing the
//! snapshot to the UI after each repository lands and to the cache at the end.
//! Failed repositories keep their previous numbers; staleness is derived from
//! each repository's `fetched_at`, so old data is always visibly old.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local, Utc};
use craft_core::{Cache, Config, Snapshot, WindowState};
use craft_github::{resolve_token, Error as GhError, GitHub};
use serde::Serialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, State, WebviewWindow, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

const MAIN: &str = "main";
const TRAY: &str = "main";
const EV_SNAPSHOT: &str = "craft:snapshot";
const EV_FOCUS: &str = "craft:focus";
const EV_META: &str = "craft:meta";
/// Repositories fetched concurrently. Small on purpose: GitHub frowns on
/// bursts (secondary rate limits) and 12 repos finish in well under 15 s.
const WORKERS: usize = 4;
/// How often the poll thread wakes to check the clock, the config file and
/// the tray tooltip. Wall-clock based, so a sleeping laptop polls on wake.
const TICK: Duration = Duration::from_secs(15);
/// Retry sooner than the poll interval when everything failed (offline, asleep…).
const RETRY_AFTER_FAILURE: i64 = 60;
/// Ignore manual refreshes this soon after the previous poll finished.
const MANUAL_DEBOUNCE: i64 = 15;
/// Below this many GraphQL points left, wait for the hourly reset.
const BUDGET_FLOOR: u64 = 150;

pub struct AppState {
    config: Mutex<Config>,
    config_mtime: Mutex<Option<SystemTime>>,
    snapshot: Mutex<Snapshot>,
    cache: Option<Cache>,
    refresh_tx: Mutex<Option<mpsc::Sender<()>>>,
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
    request_refresh(&state);
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

fn request_refresh(state: &AppState) {
    if let Some(tx) = state.refresh_tx.lock().unwrap().as_ref() {
        let _ = tx.send(());
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

/// Fetch every configured repository once. Returns seconds until the next poll.
fn run_cycle(app: &AppHandle) -> i64 {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    {
        let mut snap = state.snapshot.lock().unwrap();
        snap.sync_repos(&cfg.repos);
        snap.refreshing = true;
        // Measured afresh each cycle (the minimum any repo reported).
        snap.rate_limit_remaining = None;
        snap.poll_secs = cfg.poll_secs();
        snap.stale_after_secs = cfg.stale_secs();
    }
    emit_snapshot(app);

    let finish = |error: Option<String>, next_in: i64, all_ok: bool| {
        {
            let mut snap = state.snapshot.lock().unwrap();
            snap.refreshing = false;
            snap.error = error;
            if all_ok {
                snap.last_full_refresh_at = Some(Utc::now());
            }
            snap.next_refresh_at = Some(Utc::now() + chrono::Duration::seconds(next_in));
            if let Some(cache) = &state.cache {
                if let Err(e) = cache.save(&snap) {
                    eprintln!("could not save cache: {e}");
                }
            }
        }
        emit_snapshot(app);
        update_tray(app);
        next_in
    };

    let gh = match resolve_token(&cfg.github_token).and_then(GitHub::new) {
        Ok(gh) => gh,
        Err(e) => return finish(Some(e.to_string()), RETRY_AFTER_FAILURE * 5, false),
    };

    let queue = Mutex::new(cfg.repos.iter().rev().cloned().collect::<Vec<_>>());
    let failures = Mutex::new(Vec::<GhError>::new());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(cfg.repos.len()) {
            scope.spawn(|| loop {
                let Some(repo) = queue.lock().unwrap().pop() else {
                    break;
                };
                let now = Utc::now();
                let result = gh.fetch_repo(&repo, now);
                {
                    let mut snap = state.snapshot.lock().unwrap();
                    let Some(entry) = snap.repos.iter_mut().find(|e| e.repo == repo) else {
                        continue;
                    };
                    entry.last_attempt_at = Some(now);
                    match result {
                        Ok((stats, budget)) => {
                            entry.stats = Some(stats);
                            entry.fetched_at = Some(now);
                            entry.error = None;
                            if let Some(left) = budget.graphql_remaining {
                                snap.rate_limit_remaining =
                                    Some(snap.rate_limit_remaining.map_or(left, |r| r.min(left)));
                            }
                        }
                        Err(e) => {
                            entry.error = Some(e.to_string());
                            failures.lock().unwrap().push(e);
                        }
                    }
                }
                emit_snapshot(app);
            });
        }
    });

    let failures = failures.into_inner().unwrap();
    let poll = cfg.poll_secs();
    let rate_limited = failures
        .iter()
        .find(|e| matches!(e, GhError::RateLimited(_)));
    if let Some(e) = rate_limited {
        return finish(Some(e.to_string()), poll * 3, false);
    }
    if failures.iter().any(|e| matches!(e, GhError::Unauthorized)) {
        return finish(Some(GhError::Unauthorized.to_string()), poll, false);
    }
    let low_budget = state
        .snapshot
        .lock()
        .unwrap()
        .rate_limit_remaining
        .is_some_and(|r| r < BUDGET_FLOOR);
    if failures.len() == cfg.repos.len() && !cfg.repos.is_empty() {
        let msg = failures.first().map(|e| e.to_string());
        return finish(msg, RETRY_AFTER_FAILURE, false);
    }
    let next = if low_budget { poll.max(30 * 60) } else { poll };
    let error = low_budget.then(|| "GitHub API budget low; polling less often".to_string());
    finish(error, next, failures.is_empty())
}

/// When the cached data is younger than one interval, wait out the rest of it
/// instead of polling right at launch (relaunching shouldn't cost a poll).
fn first_due(snap: &Snapshot, poll_secs: i64) -> DateTime<Utc> {
    let now = Utc::now();
    match snap.oldest_data_at() {
        Some(t) if (now - t).num_seconds() < poll_secs => t + chrono::Duration::seconds(poll_secs),
        _ => now,
    }
}

fn spawn_poller(app: AppHandle, rx: mpsc::Receiver<()>) {
    std::thread::Builder::new()
        .name("craft-poller".into())
        .spawn(move || {
            let state = app.state::<AppState>();
            let mut next_due = {
                let poll = state.config.lock().unwrap().poll_secs();
                let mut snap = state.snapshot.lock().unwrap();
                let due = first_due(&snap, poll);
                snap.next_refresh_at = Some(due);
                due
            };
            let mut last_done: Option<DateTime<Utc>> = None;
            let mut manual = false;
            loop {
                reload_config_if_changed(&app);
                let now = Utc::now();
                let debounced =
                    last_done.is_some_and(|t| (now - t).num_seconds() < MANUAL_DEBOUNCE);
                if now >= next_due || (manual && !debounced) {
                    let next_in = run_cycle(&app);
                    last_done = Some(Utc::now());
                    next_due = Utc::now() + chrono::Duration::seconds(next_in);
                } else {
                    // Staleness changes with the clock alone.
                    update_tray(&app);
                }
                manual = false;
                flush_window_state(&app);
                match rx.recv_timeout(TICK) {
                    Ok(()) => {
                        manual = true;
                        while rx.try_recv().is_ok() {}
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
            "refresh" => request_refresh(&app.state::<AppState>()),
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
            open_url,
            open_config,
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
    fn first_poll_waits_for_fresh_cache_only() {
        let now = Utc::now();
        let fresh = snap_with(&[1, 2], now);
        let due = first_due(&fresh, 300);
        assert!(due > now + CDuration::seconds(150) && due <= now + CDuration::seconds(181));
        let old = snap_with(&[1, 20], now);
        assert!(first_due(&old, 300) <= Utc::now());
        assert!(first_due(&Snapshot::default(), 300) <= Utc::now());
    }

    #[test]
    fn tooltip_reports_staleness() {
        let now = Utc::now();
        assert!(tray_tooltip(&Snapshot::default(), now).contains("no data"));
        assert!(tray_tooltip(&snap_with(&[1], now), now).contains("updated"));
        assert!(tray_tooltip(&snap_with(&[1, 30], now), now).contains("STALE"));
    }
}
