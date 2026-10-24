//! Tauri shell around `craft-core` + `craft-github`: the poll loop, the
//! on-disk cache, tray icon, global shortcut and window management.
//!
//! One background thread owns polling. Every `poll_minutes` (and on demand)
//! it fetches all repositories one at a time, publishing the snapshot to the
//! UI and the cache after each repository lands.
//! Failed repositories keep their previous numbers; staleness is derived from
//! each repository's `fetched_at`, so old data is always visibly old.
//!
//! A second thread, the People thread ([`spawn_people`]), keeps the pull
//! request records behind the People tab up to date. After each repository
//! fetch the poll thread hands it a job ([`PeopleWork`]) and goes on. Its
//! requests are background requests at the [`Gate`]: they wait while the
//! poll thread fetches a repository, so they go out in the gaps between
//! fetches. A rate limit on one of them pauses the poll thread's fetches as
//! its own would. People data never goes into the [`Snapshot`]; the window
//! reads it with [`get_people`], [`person_detail`] and [`repo_detail`].

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Local, Timelike, Utc};
use craft_core::people::ledger::Ledger;
use craft_core::people::rank::{direct, Direct, PersonDetail, Rankings, RepoPeople};
use craft_core::people::record::PrCache;
use craft_core::people::roster::{self, RosterRow};
use craft_core::people::view::{assemble, PeopleView};
use craft_core::people::Directory;
use craft_core::{Cache, Config, Schedule, Snapshot, TokenStatus, WindowState};
use craft_github::gate::PollSession;
use craft_github::prs::{download_roster, PrEvent, PrInput, Seen, RUN_REQUESTS};
use craft_github::{resolve_token, Error as GhError, Gate, GitHub, RawActivity};
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
/// Carries the People tab's view after each update.
const EV_PEOPLE: &str = "craft:people";
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
    /// The People tab's view; `None` until the records are read.
    people: Mutex<Option<Arc<PeopleView>>>,
    /// The credits and events behind the rankings, for the carets' tables.
    rankings: Mutex<Option<Arc<Rankings>>>,
    /// Keeps the People thread's requests out of the poll thread's fetches.
    gate: Arc<Gate>,
    /// The People thread's jobs.
    people_work: Arc<PeopleWork>,
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

/// The People tab's view, or `null` before the records are read.
#[tauri::command]
fn get_people(state: State<AppState>) -> Option<PeopleView> {
    state.people.lock().unwrap().as_deref().cloned()
}

/// Where `login`'s work went in window `window`: by repository, area, type
/// and directory, in `repo` alone when it is given; `null` before the
/// records are read.
#[tauri::command]
fn person_detail(
    state: State<AppState>,
    login: String,
    window: String,
    repo: Option<String>,
) -> Option<PersonDetail> {
    let rankings = state.rankings.lock().unwrap().clone()?;
    Some(rankings.person(&login, &window, repo.as_deref()))
}

/// Each person's figures in `repo` for window `window`; `null` before the
/// records are read.
#[tauri::command]
fn repo_detail(state: State<AppState>, repo: String, window: String) -> Option<RepoPeople> {
    let rankings = state.rankings.lock().unwrap().clone()?;
    Some(rankings.repo(&repo, &window))
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
            // The People tab follows the new repos and `[people]` settings.
            state.people_work.submit(PeopleJob {
                cfg: cfg.clone(),
                fetch: false,
            });
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

/// The [`Instant`] at wall-clock time `t`, or now when `t` has passed.
fn instant_at(t: DateTime<Utc>) -> Instant {
    Instant::now() + (t - Utc::now()).to_std().unwrap_or_default()
}

/// The pause after GitHub rate-limited a request until `until`
/// (`RateLimited`'s text), counted as one more strike, with the message for
/// the window.
fn rate_limited(
    pacing: &mut Pacing,
    until: &str,
    now: DateTime<Utc>,
) -> (chrono::Duration, String) {
    pacing.strikes += 1;
    let backoff = (BACKOFF_MIN << (pacing.strikes - 1).min(8)).min(BACKOFF_MAX);
    let mut resume = now + chrono::Duration::seconds(backoff);
    if let Some(end) = rate_limit_end(until, now) {
        resume = resume.max(end);
    }
    let msg = format!("Rate limited by GitHub; pausing until {}", clock(resume));
    (resume - now, msg)
}

/// Runs `fetch` in a poll session at `gate`, so the People thread's requests
/// wait. The session first waits for a People request in flight; when GitHub
/// rate-limited that one, returns its rate limit without running `fetch`.
/// The People thread's requests wait until the caller drops the session it
/// gets back, after it has set any pause.
fn fetch_in_session(
    gate: &Gate,
    fetch: impl FnOnce() -> Result<(), GhError>,
) -> (PollSession<'_>, Result<(), GhError>) {
    let session = gate.poll_session();
    let result = match gate.halted() {
        Some(until) => Err(GhError::RateLimited(until)),
        None => fetch(),
    };
    (session, result)
}

/// Pauses the fetches and the background requests for the rate limit the
/// People thread hit since the last step, if any, as for a rate limit on a
/// fetch. Returns the message for the window.
fn people_rate_limit(gate: &Gate, pacing: &mut Pacing, now: DateTime<Utc>) -> Option<String> {
    let until = gate.halted()?;
    let (pause, msg) = rate_limited(pacing, &until, now);
    let resume = now + pause;
    pacing.not_before = Some(pacing.not_before.map_or(resume, |t| t.max(resume)));
    gate.pause_until(instant_at(resume), msg.clone());
    Some(msg)
}

/// Holds the People thread's requests back after a fetch for as long as the
/// fetches: until the pause the step set, `pause`, with its message. A
/// successful fetch (`fetched`) first ends a pause set before it, as when a
/// manual refresh skipped a token or low-budget pause.
fn hold_people(gate: &Gate, fetched: bool, pause: Option<(Instant, &str)>) {
    if fetched {
        gate.resume();
    }
    if let Some((until, msg)) = pause {
        gate.pause_until(until, msg);
    }
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
    let people_limit = people_rate_limit(&state.gate, pacing, now);
    let due = {
        let mut snap = state.snapshot.lock().unwrap();
        snap.sync_repos(&cfg.repos);
        snap.poll_secs = cfg.poll_secs();
        snap.stale_after_secs = cfg.stale_secs();
        let config_error = snap
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("Config"));
        if let (Some(msg), false) = (&people_limit, config_error) {
            snap.error = Some(msg.clone());
        }
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
        let (session, result) =
            fetch_in_session(&state.gate, || fetch_into_snapshot(app, &cfg, repo));
        let fetched = result.is_ok();
        let now = Utc::now();
        let mut snap = state.snapshot.lock().unwrap();
        let (pause, error) = match result {
            Err(GhError::RateLimited(until)) => {
                let (pause, msg) = rate_limited(pacing, &until, now);
                (pause, Some(msg))
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
        // Set before the session ends.
        let people_pause = error
            .as_ref()
            .map(|msg| (instant_at(now + pause), msg.as_str()));
        hold_people(&state.gate, fetched, people_pause);
        drop(session);
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
        drop(snap);
        // The fetch read when the repo's newest pull request was updated.
        if fetched {
            state.people_work.submit(PeopleJob {
                cfg: cfg.clone(),
                fetch: true,
            });
        }
    }
    {
        let mut snap = state.snapshot.lock().unwrap();
        let next = snap
            .next_due_at(&schedule(pacing))
            .map(|t| t.max(Utc::now()))
            .map(|t| pacing.not_before.map_or(t, |p| t.max(p)));
        if snap.next_refresh_at == next && due.is_none() && people_limit.is_none() {
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
            loop {
                reload_config_if_changed(&app);
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

// ───────────────────────────── people ─────────────────────────────

/// One run of the People work, with the config of the step that asked for it.
struct PeopleJob {
    cfg: Config,
    /// Whether the job asks GitHub for changes ([`run_people`]). A job that
    /// does not works out the view again from the records on disk
    /// ([`load_people`]), as after a change to the config file.
    fetch: bool,
}

/// The People thread's queue: the poll thread hands it a job without
/// waiting, and the People thread runs the jobs one at a time. A job that
/// arrives while one runs waits for it, and a newer job replaces it.
#[derive(Default)]
struct PeopleWork {
    queue: Mutex<PeopleQueue>,
    wake: Condvar,
}

#[derive(Default)]
struct PeopleQueue {
    next: Option<PeopleJob>,
}

impl PeopleWork {
    /// Queues `job` in place of any job still waiting. The queued job asks
    /// GitHub for changes when either of the two does.
    fn submit(&self, job: PeopleJob) {
        let mut q = self.queue.lock().unwrap();
        let fetch = job.fetch || q.next.as_ref().is_some_and(|waiting| waiting.fetch);
        q.next = Some(PeopleJob { fetch, ..job });
        self.wake.notify_all();
    }

    /// Waits for a job and takes it off the queue.
    fn take(&self) -> PeopleJob {
        let mut q = self.queue.lock().unwrap();
        loop {
            if let Some(job) = q.next.take() {
                return job;
            }
            q = self.wake.wait(q).unwrap();
        }
    }
}

/// What the People thread keeps from one job to the next.
#[derive(Default)]
struct PeopleRuns {
    /// When the last job started.
    last_start: Option<DateTime<Utc>>,
    /// The token user's login.
    viewer: Viewer,
}

/// The login of the user a GitHub token belongs to, kept while the token
/// stays the same.
#[derive(Default)]
struct Viewer {
    /// The token the login belongs to.
    token: String,
    /// The login: read with one query for the token, then as each sweep
    /// reads it again; `None` until a query answered.
    login: Option<String>,
}

impl Viewer {
    /// The login for `token`. A new token drops the kept login, and with no
    /// login kept, `ask` reads it; a failed `ask` is logged and asked again
    /// by the next call.
    fn for_token(
        &mut self,
        token: &str,
        ask: impl FnOnce() -> Result<String, GhError>,
    ) -> Option<&str> {
        if self.token != token {
            self.token = token.to_string();
            self.login = None;
        }
        if self.login.is_none() {
            match ask() {
                Ok(login) => self.login = Some(login),
                Err(e) => eprintln!("people: the token's login: {e}"),
            }
        }
        self.login.as_deref()
    }
}

/// Starts the People thread, which shows the records on disk
/// ([`load_people`]), runs a first job, then runs the jobs the poll thread
/// hands it ([`run_people`] or [`load_people`]).
fn spawn_people(app: AppHandle) {
    std::thread::Builder::new()
        .name("craft-people".into())
        .spawn(move || {
            let state = app.state::<AppState>();
            let cfg = state.config.lock().unwrap().clone();
            load_people(&app, &cfg, None);
            state.people_work.submit(PeopleJob { cfg, fetch: true });
            let mut runs = PeopleRuns::default();
            loop {
                let job = state.people_work.take();
                if job.fetch {
                    run_people(&app, &job, &mut runs);
                } else {
                    load_people(&app, &job.cfg, runs.viewer.login.as_deref());
                }
            }
        })
        .expect("spawn people thread");
}

/// Runs one People job ([`update_people`]) with a client whose requests are
/// background requests at the app's [`Gate`].
fn run_people(app: &AppHandle, job: &PeopleJob, runs: &mut PeopleRuns) {
    let state = app.state::<AppState>();
    let Some(cache) = &state.cache else {
        return;
    };
    let now = Utc::now();
    // The sweep takes its share for about as long until the next job.
    let interval = runs
        .last_start
        .map_or(job.cfg.poll_secs(), |t| (now - t).num_seconds())
        .max(REPO_GAP);
    runs.last_start = Some(now);
    let client = resolve_token(&job.cfg.github_token)
        .and_then(|t| Ok((GitHub::new(t.value.clone())?, t.value)));
    match client {
        Ok((gh, token)) => {
            let gh = gh.in_background(state.gate.clone());
            runs.viewer.for_token(&token, || gh.viewer_login());
            update_people(
                app,
                &gh,
                &job.cfg,
                cache.dir(),
                now,
                interval,
                &mut runs.viewer.login,
            );
        }
        Err(e) => eprintln!("people: {e}"),
    }
}

/// What the last fetch of each of `repos` saw of its pull requests, for
/// [`PrInput::seen`] ([`RawActivity::seen`]): `None` for a repo the fetch
/// could not see, and no entry for one not fetched yet, or whose kept
/// activity in `dir` an earlier version wrote. Each carries when its repo's
/// last successful fetch read it.
fn seen_by_fetches(snap: &Snapshot, repos: &[String], dir: &Path) -> HashMap<String, Option<Seen>> {
    repos
        .iter()
        .filter_map(|repo| {
            let entry = snap.repos.iter().find(|e| e.repo == *repo)?;
            if entry.inaccessible {
                return Some((repo.clone(), None));
            }
            entry.fetched_at?;
            Some((repo.clone(), Some(RawActivity::seen(dir, repo)?)))
        })
        .collect()
}

/// Brings the pull request records in `dir` up to date for the People tab
/// (see [`GitHub::update_prs`]): the changed lists always, the sweep, the
/// backfill and the merged and closed walks only while at least
/// [`OPTIONAL_FLOOR`] GraphQL points are left, with at most [`RUN_REQUESTS`]
/// requests for the backfill, the walks and the file lists. After each stage
/// that changed records, sends the People tab its view with the roster kept
/// from before. At the end reads the roster, sends the People tab its view
/// ([`publish_people`]) and logs what the job did. `viewer` is the token
/// user's login, kept from one job to the next.
fn update_people(
    app: &AppHandle,
    gh: &GitHub,
    cfg: &Config,
    dir: &Path,
    now: DateTime<Utc>,
    interval_secs: i64,
    viewer: &mut Option<String>,
) {
    let state = app.state::<AppState>();
    let started = Instant::now();
    let (remaining, seen) = {
        let snap = state.snapshot.lock().unwrap().clone();
        let seen = seen_by_fetches(&snap, &cfg.repos, dir);
        (snap.rate_limit_remaining, seen)
    };
    let kept = roster::load_kept(&cfg.people.roster, dir, now);
    let on_event = |event: PrEvent<'_>| {
        if let PrEvent::End {
            stage,
            caches,
            changed: true,
        } = event
        {
            eprintln!("people: {} changed records", stage.label());
            publish_people(app, caches, cfg, &kept.rows, dir, now, viewer.as_deref());
        }
    };
    let (report, mut caches) = gh.update_prs(&PrInput {
        repos: &cfg.repos,
        seen: &seen,
        dir,
        now,
        interval_secs,
        remaining,
        floor: OPTIONAL_FLOOR,
        max_requests: RUN_REQUESTS,
        viewer: viewer.as_deref(),
        on_event: Some(&on_event),
    });
    if report.viewer.is_some() {
        viewer.clone_from(&report.viewer);
    }
    for e in &report.errors {
        eprintln!("people: {e}");
    }
    let roster = roster::load(&cfg.people.roster, dir, now, download_roster);
    if let Some(e) = &roster.error {
        eprintln!("people: {e}");
    }
    let people = publish_people(
        app,
        &mut caches,
        cfg,
        &roster.rows,
        dir,
        now,
        viewer.as_deref(),
    );
    let held: Vec<(&str, &PrCache)> = caches.iter().map(|(r, c)| (r.as_str(), c)).collect();
    let events = Ledger::build(&held).events.len();
    eprintln!(
        "people in {:.1}s: {} requests ({} smaller retries), {} changed, {} repos swept, {} backfilled, {} commit and check pages, {} merged read, {} closed read, {} file pages{}; {} records ({} to fetch), merged and closed walks done {}/{}; {} people, {} on the roster, {} roster rows not linked; {} events",
        started.elapsed().as_secs_f64(),
        report.requests,
        report.retries,
        report.changed,
        report.swept,
        report.backfilled,
        report.list_pages,
        report.merged,
        report.closed,
        report.file_pages,
        if report.low_budget { ", optional requests skipped: GitHub budget low" } else { "" },
        report.records,
        report.pending,
        report.walks_done,
        caches.len(),
        people.people.values().filter(|p| !p.bot).count(),
        people.people.values().filter(|p| p.on_roster).count(),
        people.unlinked.len(),
        events,
    );
}

/// Works out the People tab's view and the rankings from `caches` (see
/// [`craft_core::people::view::assemble`]), with each repo's latest build
/// from the snapshot and its commits outside pull requests from the
/// activity kept in `dir` ([`direct_of`]), keeps both and sends the view to
/// the window. The view carries `viewer`, the token user's login, for the
/// My PRs tab. Returns the directory of people.
fn publish_people(
    app: &AppHandle,
    caches: &mut [(String, PrCache)],
    cfg: &Config,
    roster: &[RosterRow],
    dir: &Path,
    now: DateTime<Utc>,
    viewer: Option<&str>,
) -> Directory {
    let state = app.state::<AppState>();
    let builds: HashMap<String, DateTime<Utc>> = {
        let snap = state.snapshot.lock().unwrap();
        snap.repos
            .iter()
            .filter_map(|e| {
                Some((
                    e.repo.to_ascii_lowercase(),
                    e.stats.as_ref()?.latest_build_at?,
                ))
            })
            .collect()
    };
    let direct: BTreeMap<String, Direct> = caches
        .iter()
        .map(|(repo, cache)| {
            (
                repo.clone(),
                direct_of(&RawActivity::load(dir, repo), cache),
            )
        })
        .collect();
    let (mut view, people, rankings) = assemble(
        caches,
        &cfg.people,
        roster,
        |repo| builds.get(&repo.to_ascii_lowercase()).copied(),
        &direct,
        now,
    );
    view.viewer = viewer.map(str::to_string);
    let view = Arc::new(view);
    *state.people.lock().unwrap() = Some(view.clone());
    *state.rankings.lock().unwrap() = Some(Arc::new(rankings));
    let _ = app.emit(EV_PEOPLE, &*view);
    people
}

/// A repo's commits outside pull requests: the commits on its default
/// branch that the kept activity `raw` holds, less every merge commit and
/// pull request commit in `cache` (see [`direct`]).
fn direct_of(raw: &RawActivity, cache: &PrCache) -> Direct {
    let history = raw
        .commits
        .iter()
        .map(|(oid, (at, who))| (oid.as_str(), *at, who.as_str()));
    direct(history, raw.covers_from, cache)
}

/// The People tab's view from the records on disk for the repos and the
/// `[people]` settings of `cfg`, with no request to GitHub: at launch,
/// before the first job updates the records, and after a change to the
/// config file. With no records it sends nothing until a view has been
/// sent. The roster comes from its kept copy or file, without a download
/// (see [`roster::load_kept`]). `viewer` is the token user's login, as the
/// last job read it.
fn load_people(app: &AppHandle, cfg: &Config, viewer: Option<&str>) {
    let state = app.state::<AppState>();
    let Some(cache) = &state.cache else { return };
    let dir = cache.dir();
    let mut caches: Vec<(String, PrCache)> = cfg
        .repos
        .iter()
        .map(|r| (r.clone(), PrCache::load(dir, r)))
        .collect();
    let shown = state.people.lock().unwrap().is_some();
    if !shown && caches.iter().all(|(_, c)| c.records.is_empty()) {
        return;
    }
    let now = Utc::now();
    let roster = roster::load_kept(&cfg.people.roster, dir, now);
    publish_people(app, &mut caches, cfg, &roster.rows, dir, now, viewer);
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
        people: Mutex::new(None),
        rankings: Mutex::new(None),
        gate: Arc::new(Gate::new()),
        people_work: Arc::new(PeopleWork::default()),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_people,
            person_detail,
            repo_detail,
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
            spawn_people(app.handle().clone());
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
    fn a_rate_limit_on_the_people_work_pauses_the_fetches_and_the_people_work() {
        let gate = Gate::new();
        let now = Utc::now().with_nanosecond(0).unwrap();
        let mut pacing = Pacing::default();
        assert_eq!(people_rate_limit(&gate, &mut pacing, now), None);
        assert_eq!(pacing.not_before, None, "nothing happened");
        let end = now + CDuration::minutes(40);
        gate.halt(end.to_rfc3339());
        let msg = people_rate_limit(&gate, &mut pacing, now).unwrap();
        assert!(
            msg.starts_with("Rate limited by GitHub; pausing until"),
            "{msg}"
        );
        assert_eq!(pacing.strikes, 1);
        assert_eq!(
            pacing.not_before,
            Some(end),
            "no fetch before GitHub's reset, later than the backoff"
        );
        assert!(
            matches!(gate.background(), Err(GhError::Halted(_))),
            "background requests wait as long"
        );
        assert_eq!(gate.halted(), None, "replaced by the pause");
        assert_eq!(
            people_rate_limit(&gate, &mut pacing, now),
            None,
            "counted once"
        );
        // A second limit doubles the backoff, as on a fetch.
        gate.halt("later");
        people_rate_limit(&gate, &mut pacing, now).unwrap();
        assert_eq!(pacing.strikes, 2);
        assert_eq!(pacing.not_before, Some(end), "a longer pause stays");
    }

    #[test]
    fn a_fetch_does_not_start_after_a_people_request_in_flight_was_rate_limited() {
        let gate = Arc::new(Gate::new());
        let permit_taken = Arc::new(std::sync::Barrier::new(2));
        let people = {
            let (gate, permit_taken) = (gate.clone(), permit_taken.clone());
            std::thread::spawn(move || {
                let permit = gate.background().unwrap();
                permit_taken.wait();
                std::thread::sleep(Duration::from_millis(100));
                // GitHub answers with a rate limit; it is kept before the
                // permit goes.
                gate.halt("later");
                drop(permit);
            })
        };
        permit_taken.wait();
        let mut fetched = false;
        let (session, result) = fetch_in_session(&gate, || {
            fetched = true;
            Ok(())
        });
        people.join().unwrap();
        assert!(matches!(result, Err(GhError::RateLimited(u)) if u == "later"));
        assert!(!fetched, "no fetch request went out");
        // Until the poll thread drops the session, People requests wait;
        // it sets the pause first.
        assert!(gate.poll_running());
        gate.pause_until(
            Instant::now() + Duration::from_secs(60),
            "Rate limited by GitHub",
        );
        drop(session);
        assert!(
            matches!(gate.background(), Err(GhError::Halted(r)) if r == "Rate limited by GitHub"),
            "the next People request meets the pause"
        );
        assert!(
            fetch_in_session(&gate, || Ok(())).1.is_ok(),
            "the next fetch runs"
        );
    }

    #[test]
    fn a_successful_fetch_ends_the_pause_of_the_people_work() {
        let gate = Gate::new();
        let later = Instant::now() + Duration::from_secs(30 * 60);
        // A token pause, which a manual refresh skips for the fetches.
        hold_people(&gate, false, Some((later, "GitHub rejected the token")));
        assert!(matches!(gate.background(), Err(GhError::Halted(_))));
        hold_people(&gate, true, None);
        assert!(gate.background().is_ok(), "the next fetch succeeded");
        // A fetch that leaves the budget low pauses them again.
        hold_people(&gate, true, Some((later, "GitHub API budget low")));
        assert!(
            matches!(gate.background(), Err(GhError::Halted(r)) if r == "GitHub API budget low")
        );
    }

    #[test]
    fn the_people_work_keeps_only_the_newest_waiting_job() {
        let work = PeopleWork::default();
        let job = |minutes: f64, fetch: bool| PeopleJob {
            cfg: Config {
                poll_minutes: minutes,
                ..Config::default()
            },
            fetch,
        };
        work.submit(job(1.0, true));
        work.submit(job(2.0, true));
        assert_eq!(work.take().cfg.poll_minutes, 2.0);
        work.submit(job(3.0, true));
        assert_eq!(work.take().cfg.poll_minutes, 3.0, "queued while one ran");
        // A changed config file replaces a waiting fetch, which still runs
        // with the new config.
        work.submit(job(4.0, true));
        work.submit(job(5.0, false));
        let next = work.take();
        assert_eq!((next.cfg.poll_minutes, next.fetch), (5.0, true));
        work.submit(job(6.0, false));
        assert!(!work.take().fetch, "only the view is worked out again");
    }

    #[test]
    fn the_token_users_login_is_read_once_per_token() {
        let asked = std::cell::Cell::new(0);
        let ask = |answer: Result<&str, GhError>| {
            asked.set(asked.get() + 1);
            answer.map(str::to_string)
        };
        let mut v = Viewer::default();
        assert_eq!(
            v.for_token("t1", || ask(Ok("Ann-Example"))),
            Some("Ann-Example")
        );
        assert_eq!(v.for_token("t1", || ask(Ok("other"))), Some("Ann-Example"));
        assert_eq!(asked.get(), 1, "kept while the token stays the same");
        // The sweep read it again.
        v.login = Some("ann-example".into());
        assert_eq!(v.for_token("t1", || ask(Ok("other"))), Some("ann-example"));
        assert_eq!(
            v.for_token("t2", || ask(Err(GhError::Unauthorized))),
            None,
            "a new token drops the old login"
        );
        assert_eq!(
            v.for_token("t2", || ask(Ok("Bo-Example"))),
            Some("Bo-Example")
        );
        assert_eq!(asked.get(), 3, "a failed query is asked again");
    }

    #[test]
    fn the_changed_lists_follow_what_each_fetch_saw() {
        let dir = std::env::temp_dir().join(format!("craft-seen-{}", std::process::id()));
        let now = Utc::now();
        let repos: Vec<String> = ["o/new", "o/hidden", "o/old", "o/seen", "o/empty"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut snap = Snapshot::default();
        snap.sync_repos(&repos);
        for e in &mut snap.repos {
            if e.repo != "o/new" {
                e.fetched_at = Some(now);
            }
            e.inaccessible = e.repo == "o/hidden";
        }
        let at = "2026-10-18T09:30:00Z".parse::<DateTime<Utc>>().unwrap();
        // Its last successful fetch; a failed one since leaves this time.
        let read = "2026-10-18T09:40:00Z".parse::<DateTime<Utc>>().unwrap();
        let seen_raw = RawActivity {
            fetched_at: Some(read),
            pr_updated_at: Some(at),
            ..Default::default()
        };
        seen_raw.save(&dir, "o/seen").unwrap();
        // A repo without pull requests.
        let empty_raw = RawActivity {
            fetched_at: Some(read),
            ..Default::default()
        };
        empty_raw.save(&dir, "o/empty").unwrap();
        // Kept by a version that did not read the field.
        std::fs::write(
            dir.join("raw").join("o@old.json"),
            r#"{"fetched_at":"2026-10-18T09:40:00Z","commits":{}}"#,
        )
        .unwrap();
        let seen = seen_by_fetches(&snap, &repos, &dir);
        assert_eq!(seen.get("o/hidden"), Some(&None), "left out of the update");
        assert_eq!(
            seen.get("o/seen"),
            Some(&Some(Seen {
                pr_updated_at: Some(at),
                read_at: read,
            }))
        );
        assert_eq!(
            seen.get("o/empty"),
            Some(&Some(Seen {
                pr_updated_at: None,
                read_at: read,
            })),
            "its changed list is not read"
        );
        assert!(
            !seen.contains_key("o/new") && !seen.contains_key("o/old"),
            "their changed lists are read"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_snapshot_holds_nothing_about_people() {
        let mut snap = snap_with(&[1], Utc::now());
        snap.repos[0].stats = Some(Default::default());
        let json = serde_json::to_value(&snap).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.extend(
            json["repos"][0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str),
        );
        keys.extend(
            json["repos"][0]["stats"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str),
        );
        for word in ["people", "viewer", "login", "state", "work", "rank"] {
            assert!(!keys.iter().any(|k| k.contains(word)), "{word} in {keys:?}");
        }
    }

    #[test]
    fn tooltip_reports_staleness() {
        let now = Utc::now();
        assert!(tray_tooltip(&Snapshot::default(), now).contains("no data"));
        assert!(tray_tooltip(&snap_with(&[1], now), now).contains("updated"));
        assert!(tray_tooltip(&snap_with(&[1, 30], now), now).contains("STALE"));
    }
}
