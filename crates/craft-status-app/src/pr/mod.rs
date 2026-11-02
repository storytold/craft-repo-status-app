//! The PR tab: the commands its UI calls and the state its threads share (in
//! `poller`), around `pr-core`, `pr-github` and `pr-claude`. Third-party repos stay
//! out of all of this: see `pr_core::Config::watched`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{mpsc, Mutex};

use craft_core::Snapshot;
use poller::{ClaudeRequest, Status, Wake};
use pr_core::views::{self, PrFull, PrRow, RepoRow, RepoView};
use pr_core::{Config, DetailRef, StatusFacts, Store};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

mod poller;

const EV_CHANGED: &str = "pr:changed";
/// How many PRs the review queue shows.
const QUEUE_LIMIT: usize = 400;

pub struct PrReview {
    pub config: Mutex<Config>,
    store: Mutex<Store>,
    /// What the status poller knows, as of the last fetch step.
    facts: Mutex<StatusFacts>,
    status: Mutex<Status>,
    /// Where the PR cache lives; Claude runs in an empty directory inside it.
    dir: PathBuf,
    claude_dir: PathBuf,
    /// Shared with the status poller: the PR ledgers, GitHub lease and budget.
    shared_cache: craft_core::SharedCache,
    forced_lists: Mutex<Vec<String>>,
    forced_details: Mutex<Vec<DetailRef>>,
    claude_requests: Mutex<VecDeque<ClaudeRequest>>,
    refresh_tx: Mutex<Option<mpsc::Sender<Wake>>>,
    claude_tx: Mutex<Option<mpsc::Sender<()>>>,
    /// The PR half of the tray tooltip, kept by the fetch thread.
    pub tray_line: Mutex<String>,
}

/// The ends of the channels that wake the threads, until they're started.
pub struct Threads {
    fetch: mpsc::Receiver<Wake>,
    claude: mpsc::Receiver<()>,
}

/// Loads the PR cache, for repos `config` lists.
pub fn init(config: &craft_core::Config, snapshot: &Snapshot) -> (PrReview, Threads) {
    let pr_config = Config::load().unwrap_or_else(|e| {
        eprintln!("Could not read [pr_review] in the config ({e}); using defaults");
        Config::default()
    });
    let dir = Store::default_dir().unwrap_or_else(|_| std::env::temp_dir().join("craft-pr-review"));
    let mut store = Store::load(&dir);
    let shared_cache = craft_core::SharedCache::default_location()
        .unwrap_or_else(|_| craft_core::SharedCache::at(dir.join("shared")));
    store.hydrate(&shared_cache, &pr_config.watched(&config.repos));
    let (fetch_tx, fetch) = mpsc::channel();
    let (claude_tx, claude) = mpsc::channel();
    let state = PrReview {
        config: Mutex::new(pr_config),
        store: Mutex::new(store),
        facts: Mutex::new(StatusFacts::of(snapshot)),
        status: Mutex::new(Status::default()),
        claude_dir: dir.join("claude-cwd"),
        shared_cache,
        dir,
        forced_lists: Mutex::new(vec![]),
        forced_details: Mutex::new(vec![]),
        claude_requests: Mutex::new(VecDeque::new()),
        refresh_tx: Mutex::new(Some(fetch_tx)),
        claude_tx: Mutex::new(Some(claude_tx)),
        tray_line: Mutex::new(String::new()),
    };
    (state, Threads { fetch, claude })
}

pub fn spawn(app: AppHandle, threads: Threads) {
    note_tray_line(&app);
    poller::spawn_fetcher(app.clone(), threads.fetch);
    poller::spawn_claude(app, threads.claude);
}

// ───────────────────────────── commands ─────────────────────────────
// `async` so reading thousands of PRs never blocks the main thread.

#[derive(Serialize)]
pub struct Overview {
    repos: Vec<RepoRow>,
    status: Status,
}

struct Reading {
    cfg: Config,
    repos: Vec<String>,
    facts: StatusFacts,
}

fn reading(app: &AppHandle) -> Reading {
    let state = app.state::<PrReview>();
    let cfg = state.config.lock().unwrap().clone();
    let repos = cfg.watched(&shared(app).repos);
    let facts = state.facts.lock().unwrap().clone();
    Reading { cfg, repos, facts }
}

#[tauri::command(async)]
pub fn get_pr_overview(app: AppHandle, state: State<PrReview>) -> Overview {
    let r = reading(&app);
    let rows = views::overview(&state.store.lock().unwrap(), &r.repos, &r.cfg, &r.facts);
    Overview {
        repos: rows,
        status: state.status.lock().unwrap().clone(),
    }
}

#[tauri::command(async)]
pub fn get_pr_repo(app: AppHandle, state: State<PrReview>, repo: String) -> Option<RepoView> {
    let r = reading(&app);
    views::repo_view(
        &state.store.lock().unwrap(),
        &repo,
        &r.repos,
        &r.cfg,
        &r.facts,
    )
}

#[tauri::command(async)]
pub fn get_pr_queue(app: AppHandle, state: State<PrReview>) -> Vec<PrRow> {
    let r = reading(&app);
    views::queue(&state.store.lock().unwrap(), &r.repos, &r.cfg, QUEUE_LIMIT)
}

#[tauri::command(async)]
pub fn get_pr(app: AppHandle, state: State<PrReview>, repo: String, number: u64) -> Option<PrFull> {
    let r = reading(&app);
    views::pr_full(
        &state.store.lock().unwrap(),
        &repo,
        number,
        &r.repos,
        &r.cfg,
        &r.facts,
    )
}

#[tauri::command]
pub fn refresh_prs(state: State<PrReview>) {
    refresh_all(&state);
}

#[tauri::command]
pub fn refresh_pr_repo(state: State<PrReview>, repo: String) {
    let mut forced = state.forced_lists.lock().unwrap();
    if !forced.contains(&repo) {
        forced.push(repo);
    }
    drop(forced);
    wake_fetcher(&state, Wake::Repo);
}

/// Inspect one PR now.
#[tauri::command]
pub fn refresh_pr(state: State<PrReview>, repo: String, number: u64) {
    let id = state
        .store
        .lock()
        .unwrap()
        .pr(&repo, number)
        .map(|p| p.id.clone());
    if let Some(id) = id.filter(|i| !i.is_empty()) {
        let re = DetailRef { repo, number, id };
        let mut forced = state.forced_details.lock().unwrap();
        if !forced.contains(&re) {
            forced.push(re);
        }
        drop(forced);
        wake_fetcher(&state, Wake::Pr);
    }
}

/// Ask Claude about one PR now, with its diff if `deep`.
#[tauri::command]
pub fn analyze_pr(state: State<PrReview>, repo: String, number: u64, deep: bool) {
    state
        .claude_requests
        .lock()
        .unwrap()
        .push_back(ClaudeRequest { repo, number, deep });
    if let Some(tx) = state.claude_tx.lock().unwrap().as_ref() {
        let _ = tx.send(());
    }
}

// ───────────────────────────── shared state ─────────────────────────────

pub fn refresh_all(state: &PrReview) {
    wake_fetcher(state, Wake::All);
}

fn wake_fetcher(state: &PrReview, why: Wake) {
    if let Some(tx) = state.refresh_tx.lock().unwrap().as_ref() {
        let _ = tx.send(why);
    }
}

/// Tells the UI to reread whatever PR view it's showing.
pub fn emit_changed(app: &AppHandle) {
    let _ = app.emit(EV_CHANGED, ());
}

/// The parts of the app's main config the PR threads use.
#[derive(Clone, Default)]
struct Shared {
    repos: Vec<String>,
    github_token: String,
}

fn shared(app: &AppHandle) -> Shared {
    let state = app.state::<crate::AppState>();
    let cfg = state.config.lock().unwrap();
    Shared {
        repos: cfg.repos.clone(),
        github_token: cfg.github_token.clone(),
    }
}

/// "28 landable, 143 promising, of 7891 open PRs"; empty before there are any.
fn tray_line(rows: &[RepoRow]) -> String {
    let open: u64 = rows.iter().map(|r| r.open).sum();
    if open == 0 {
        return String::new();
    }
    let promising: u64 = rows.iter().map(|r| r.promising).sum();
    let land: u64 = rows.iter().map(|r| r.landable).sum();
    format!("{land} landable, {promising} promising, of {open} open PRs")
}

fn note_tray_line(app: &AppHandle) {
    let state = app.state::<PrReview>();
    let r = reading(app);
    let rows = views::overview(&state.store.lock().unwrap(), &r.repos, &r.cfg, &r.facts);
    *state.tray_line.lock().unwrap() = tray_line(&rows);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_line_counts_what_can_be_reviewed() {
        assert_eq!(tray_line(&[]), "");
        let rows = [
            RepoRow {
                open: 10,
                promising: 3,
                landable: 1,
                ..Default::default()
            },
            RepoRow {
                open: 5,
                promising: 2,
                ..Default::default()
            },
        ];
        assert_eq!(tray_line(&rows), "1 landable, 5 promising, of 15 open PRs");
    }
}
