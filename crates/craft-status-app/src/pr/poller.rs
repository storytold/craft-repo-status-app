//! The PR tab's two background threads. One fetches from GitHub, one job at a time,
//! as `pr_core::schedule` decides. One calls Claude, which can take minutes, so it
//! never holds up fetching.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::Duration;

use chrono::{DateTime, Local, Timelike, Utc};
use craft_core::SharedCache;
use pr_core::schedule::{budget_exhausted, detail_backlog, next_job, Job, SchedInput};
use pr_core::store::refresh_groups;
use pr_core::{
    views, ClaudeConfig, Config, DetailRef, Pr, PrState, RepoData, Spend, StatusFacts, Time,
};
use pr_github::{resolve_token, Error as GhError, GitHub};
use serde::Serialize;
use tauri::{AppHandle, Manager};

use super::{emit_changed, note_tray_line, shared, PrReview};

/// How often the thread wakes to check the clock and the config.
const TICK: Duration = Duration::from_secs(15);
/// Gaps after each kind of job, on top of the pacing in pr-github, so a backlog
/// spreads over hours rather than arriving as a burst.
const GAP_AFTER_LIST: i64 = 15;
const GAP_AFTER_HISTORY: i64 = 5;
const GAP_AFTER_ROSTER: i64 = 5;
const GAP_AFTER_DETAILS: i64 = 8;
const REPO_RETRY_GAP: i64 = 30;
const BACKOFF_MIN: i64 = 5 * 60;
const BACKOFF_MAX: i64 = 60 * 60;
const LOW_BUDGET_PAUSE: i64 = 15 * 60;
/// PRs per detail request. GitHub times out on some heavy batches, so this halves
/// on a timeout and creeps back up.
const DETAIL_BATCH: usize = 25;
const MANUAL_DEBOUNCE: i64 = 15;
/// Readings of the shared budget older than this say little about it now.
const BUDGET_FRESH_MINUTES: i64 = 10;
/// When the status poller holds the shared lease, try again this soon.
const LEASE_RETRY_SECS: i64 = 5;

const CLAUDE_TICK: Duration = Duration::from_secs(20);
/// After a failed call, leave Claude alone for this long.
const CLAUDE_BACKOFF: i64 = 10 * 60;
/// Group a repo only once this many of its PRs are listed.
const MIN_PRS_TO_GROUP: usize = 3;

/// Why the fetch thread is woken early.
#[derive(Clone, Copy, PartialEq)]
pub enum Wake {
    All,
    Repo,
    Pr,
}

/// What the app is up to, for the UI's status bar and banners.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub fetching: Option<String>,
    pub next_at: Option<Time>,
    pub paused_until: Option<Time>,
    pub error: Option<String>,
    pub token_source: Option<String>,
    pub token_rejected: bool,
    pub rate_remaining: Option<u64>,
    pub points_last_hour: u64,
    pub max_points_per_hour: u64,
    /// PRs waiting to be inspected.
    pub backlog: usize,
    pub histories_pending: usize,
    pub poll_secs: i64,
    pub stale_after_secs: i64,
    pub claude: ClaudeStatus,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClaudeStatus {
    pub enabled: bool,
    pub available: bool,
    pub model: String,
    pub running: Option<String>,
    pub spent_today: f64,
    pub max_usd_per_day: f64,
    pub runs_last_hour: u32,
    pub max_runs_per_hour: u32,
    pub last_error: Option<String>,
}

/// A hand-requested Claude pass over one PR.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeRequest {
    pub repo: String,
    pub number: u64,
    pub deep: bool,
}

#[derive(Default)]
struct Pacing {
    not_before: Option<Time>,
    strikes: u32,
    spends: VecDeque<(Time, u64)>,
    remaining: Option<u64>,
    reset_at: Option<Time>,
    detail_batch: Option<usize>,
    just_listed: bool,
}

impl Pacing {
    fn note(&mut self, now: Time, spends: &[Spend]) {
        for s in spends {
            self.spends.push_back((now, s.cost));
            if s.remaining.is_some() {
                self.remaining = s.remaining;
                self.reset_at = s.reset_at;
            }
        }
    }

    fn points_last_hour(&mut self, now: Time) -> u64 {
        while self
            .spends
            .front()
            .is_some_and(|(t, _)| (now - *t).num_minutes() >= 60)
        {
            self.spends.pop_front();
        }
        self.spends.iter().map(|(_, c)| c).sum()
    }

    fn batch(&self) -> usize {
        self.detail_batch.unwrap_or(DETAIL_BATCH)
    }
}

fn shared_cache(state: &PrReview) -> SharedCache {
    state.shared_cache.clone()
}

fn clock(t: Time) -> String {
    t.with_timezone(&Local).format("%H:%M").to_string()
}

fn rate_limit_end(until: &str, now: Time) -> Option<Time> {
    if let Ok(t) = DateTime::parse_from_rfc3339(until) {
        return Some(t.with_timezone(&Utc));
    }
    (until == "the top of the hour").then(|| {
        let next = now + chrono::Duration::hours(1);
        next.with_minute(0)
            .and_then(|t| t.with_second(0))
            .unwrap_or(next)
    })
}

fn job_label(job: &Job) -> String {
    let short = |r: &str| r.split('/').next_back().unwrap_or(r).to_string();
    match job {
        Job::ListFull(r) => format!("Listing {}", short(r)),
        Job::ListSince(r, _) => format!("Updating {}", short(r)),
        Job::Roster(r) => format!("Checking who can write to {}", short(r)),
        Job::History(r) => format!("Counting merged PRs in {}", short(r)),
        Job::Details(refs) => format!("Inspecting {} PRs", refs.len()),
    }
}

/// Puts a repo fetched on a copy back, keeping anything Claude wrote meanwhile.
fn merge_fetched(current: &mut RepoData, mut fetched: RepoData) {
    for (n, p) in fetched.prs.iter_mut() {
        if let Some(c) = current.prs.get(n) {
            p.analysis = c.analysis.clone();
        }
    }
    fetched.claude_groups = std::mem::take(&mut current.claude_groups);
    fetched.groups_at = current.groups_at;
    refresh_groups(&mut fetched);
    *current = fetched;
}

/// Runs a job that fetches into a copy of the repo, so the UI can keep reading
/// the original.
fn run_on_copy(
    app: &AppHandle,
    repo: &str,
    now: Time,
    work: impl FnOnce(&mut RepoData) -> Result<Vec<Spend>, GhError>,
) -> Result<Vec<Spend>, GhError> {
    let state = app.state::<PrReview>();
    let mut copy = {
        let mut store = state.store.lock().unwrap();
        store.repo_mut(repo).clone()
    };
    let result = work(&mut copy);
    let mut store = state.store.lock().unwrap();
    let current = store.repo_mut(repo);
    current.last_attempt_at = Some(now);
    let out = match result {
        Ok(spends) => {
            copy.error = None;
            copy.inaccessible = false;
            copy.last_attempt_at = Some(now);
            merge_fetched(current, copy);
            Ok(spends)
        }
        Err(GhError::NotFound) => {
            current.inaccessible = true;
            current.error = None;
            Ok(vec![])
        }
        Err(e @ (GhError::Unauthorized | GhError::NoToken)) => Err(e),
        Err(e) => {
            current.error = Some(e.to_string());
            Err(e)
        }
    };
    if let Err(e) = store.save(repo) {
        eprintln!("{repo}: could not save: {e}");
    }
    out
}

/// Puts a page of a list walk on screen, ahead of the walk finishing.
fn show_page(app: &AppHandle, repo: &str, page: &[Pr]) {
    {
        let state = app.state::<PrReview>();
        let mut store = state.store.lock().unwrap();
        let d = store.repo_mut(repo);
        for pr in page {
            pr_github::upsert(d, pr.clone());
        }
    }
    emit_changed(app);
}

fn run_details(
    app: &AppHandle,
    gh: &GitHub,
    refs: &[DetailRef],
    now: Time,
) -> Result<Vec<Spend>, GhError> {
    let state = app.state::<PrReview>();
    let (results, spend) = gh.details(refs, now)?;
    let mut store = state.store.lock().unwrap();
    let mut touched = std::collections::BTreeSet::new();
    for (r, res) in results {
        let d = store.repo_mut(&r.repo);
        match res {
            Some(res) => {
                if let Some(p) = d.prs.get_mut(&r.number) {
                    res.apply(p, now);
                }
            }
            None => {
                d.prs.remove(&r.number);
            }
        }
        touched.insert(r.repo);
    }
    for repo in touched {
        refresh_groups(store.repo_mut(&repo));
        if let Err(e) = store.save(&repo) {
            eprintln!("{repo}: could not save: {e}");
        }
    }
    Ok(vec![spend])
}

fn run_job(
    app: &AppHandle,
    job: &Job,
    gh: &GitHub,
    now: Time,
) -> Result<(Vec<Spend>, i64), GhError> {
    let out = match job {
        Job::ListFull(repo) => (
            run_on_copy(app, repo, now, |d| {
                gh.list_full(repo, d, now, &mut |page| show_page(app, repo, page))
            })?,
            GAP_AFTER_LIST,
        ),
        Job::ListSince(repo, since) => (
            run_on_copy(app, repo, now, |d| gh.list_since(repo, d, *since, now))?,
            GAP_AFTER_LIST,
        ),
        Job::Roster(repo) => (
            run_on_copy(app, repo, now, |d| {
                let (roster, spends) = gh.roster(repo, now)?;
                d.roster = roster;
                Ok(spends)
            })?,
            GAP_AFTER_ROSTER,
        ),
        Job::History(repo) => (
            run_on_copy(app, repo, now, |d| gh.history(repo, d))?,
            GAP_AFTER_HISTORY,
        ),
        Job::Details(refs) => (run_details(app, gh, refs, now)?, GAP_AFTER_DETAILS),
    };
    Ok(out)
}

/// Fetch whatever's most useful, if the budget and pacing allow.
fn step(app: &AppHandle, pacing: &mut Pacing) {
    let state = app.state::<PrReview>();
    let cfg = state.config.lock().unwrap().clone();
    let shared = shared(app);
    let repos = cfg.watched(&shared.repos);
    let now = Utc::now();
    let facts = StatusFacts::of(&app.state::<crate::AppState>().snapshot.lock().unwrap());
    *state.facts.lock().unwrap() = facts.clone();
    let spent = pacing.points_last_hour(now);
    let paused = pacing.not_before.is_some_and(|t| now < t);

    // The status poller may have been rate-limited, or read the budget, since we did.
    let shared_budget = state.shared_cache.budget();
    if let Some(until) = shared_budget.backoff(now) {
        pacing.not_before = Some(pacing.not_before.map_or(until, |t| t.max(until)));
    }
    let paused = paused || pacing.not_before.is_some_and(|t| now < t);
    // It only reaches the shared budget when rate-limited; its reading is in its snapshot.
    let others = shared_budget
        .remaining_now(now, BUDGET_FRESH_MINUTES)
        .or(facts.fresh_remaining(now));

    // A pause already holds; otherwise see whether the shared budget allows a start.
    let low = budget_exhausted(pacing.remaining, others, cfg.budget_floor);
    let reset_passed = pacing.reset_at.is_some_and(|t| now >= t);
    if low && !paused && !reset_passed {
        let resume = pacing
            .reset_at
            .filter(|t| *t > now)
            .unwrap_or(now + chrono::Duration::seconds(LOW_BUDGET_PAUSE));
        pacing.not_before = Some(resume);
        let mut st = state.status.lock().unwrap();
        st.paused_until = Some(resume);
        st.error = Some(format!(
            "GitHub budget is low; leaving the rest for the Downloads tab until {}",
            clock(resume)
        ));
    }
    if reset_passed {
        pacing.remaining = None;
        pacing.reset_at = None;
    }
    let paused = pacing.not_before.is_some_and(|t| now < t);

    let forced_lists = state.forced_lists.lock().unwrap().clone();
    let forced_details = state.forced_details.lock().unwrap().clone();
    let job = {
        let store = state.store.lock().unwrap();
        let input = SchedInput {
            now,
            cfg: &cfg,
            repos: &repos,
            data: &store.repos,
            facts: &facts,
            points_last_hour: spent,
            forced_lists: &forced_lists,
            forced_details: &forced_details,
            detail_batch: pacing.batch(),
            just_listed: pacing.just_listed,
        };
        let backlog = detail_backlog(&input);
        let pending = repos
            .iter()
            .filter(|r| !store.repos.get(*r).is_some_and(|d| d.history.complete))
            .count();
        let mut st = state.status.lock().unwrap();
        st.backlog = backlog;
        st.histories_pending = pending;
        st.points_last_hour = spent;
        st.max_points_per_hour = cfg.max_points_per_hour;
        st.poll_secs = cfg.poll_secs();
        st.stale_after_secs = cfg.stale_secs();
        if !paused {
            st.paused_until = None;
        }
        (!paused).then(|| next_job(&input)).flatten()
    };

    let Some(job) = job else {
        emit_changed(app);
        return;
    };

    // One poller talks to GitHub at a time. The status poller waits for this; this
    // one gives way, since it's the one with hours of background work to do.
    let Some(lease) = state.shared_cache.try_lease() else {
        pacing.not_before = Some(now + chrono::Duration::seconds(LEASE_RETRY_SECS));
        state.status.lock().unwrap().fetching =
            Some("Waiting for the Downloads tab's fetch".to_string());
        emit_changed(app);
        return;
    };

    state.status.lock().unwrap().fetching = Some(job_label(&job));
    pacing.just_listed = match &job {
        Job::ListFull(r) => {
            let first = !state
                .store
                .lock()
                .unwrap()
                .repos
                .get(r)
                .is_some_and(|d| d.full_list_at.is_some());
            first
        }
        // A roster follows the list; the inspecting comes after both.
        Job::Roster(_) => pacing.just_listed,
        _ => false,
    };
    emit_changed(app);

    let token = resolve_token(&shared.github_token);
    {
        let mut st = state.status.lock().unwrap();
        st.token_source = token.as_ref().ok().map(|t| t.source.to_string());
        st.token_rejected = false;
    }
    let result = token
        .and_then(|t| GitHub::new(t.value, shared_cache(&state)))
        .and_then(|gh| run_job(app, &job, &gh, now));
    drop(lease);

    let now = Utc::now();
    let mut error = None;
    let gap = match result {
        Ok((spends, g)) => {
            pacing.note(now, &spends);
            pacing.strikes = 0;
            if matches!(job, Job::Details(_)) {
                pacing.detail_batch = Some((pacing.batch() + 1).min(DETAIL_BATCH));
            }
            g
        }
        Err(GhError::RateLimited(until)) => {
            pacing.strikes += 1;
            let backoff = (BACKOFF_MIN << (pacing.strikes - 1).min(8)).min(BACKOFF_MAX);
            let mut resume = now + chrono::Duration::seconds(backoff);
            if let Some(end) = rate_limit_end(&until, now) {
                resume = resume.max(end);
            }
            // The status poller shares the account: it should stop too.
            if let Err(e) = state.shared_cache.note_backoff(resume) {
                eprintln!("could not share the backoff: {e}");
            }
            error = Some(format!(
                "Rate limited by GitHub; pausing until {}",
                clock(resume)
            ));
            (resume - now).num_seconds()
        }
        Err(GhError::Unauthorized) => {
            state.status.lock().unwrap().token_rejected = true;
            error = Some(
                "GitHub rejected the token (401): it may have expired or been revoked".to_string(),
            );
            cfg.poll_secs()
        }
        Err(e @ GhError::NoToken) => {
            error = Some(e.to_string());
            cfg.poll_secs()
        }
        Err(e) => {
            if let Job::Details(refs) = &job {
                if pr_github::is_timeout(&e) && refs.len() > 1 {
                    pacing.detail_batch = Some((refs.len() / 2).max(1));
                } else {
                    // One PR GitHub can't serve mustn't hold up the others.
                    let mut store = state.store.lock().unwrap();
                    for r in refs {
                        if let Some(p) = store.repo_mut(&r.repo).prs.get_mut(&r.number) {
                            p.detail_failed_at = Some(now);
                        }
                    }
                }
                error = Some(format!("Couldn't inspect PRs: {e}"));
            } else {
                error = Some(e.to_string());
            }
            REPO_RETRY_GAP
        }
    };
    pacing.not_before = Some(now + chrono::Duration::seconds(gap));
    if let Err(e) = state
        .shared_cache
        .note_budget(pacing.remaining, pacing.reset_at, now)
    {
        eprintln!("could not save the shared budget: {e}");
    }

    // Hand-made requests are done, whatever happened, or they'd loop.
    match &job {
        Job::ListFull(r) => state.forced_lists.lock().unwrap().retain(|x| x != r),
        Job::Details(refs) => state
            .forced_details
            .lock()
            .unwrap()
            .retain(|x| !refs.contains(x)),
        _ => {}
    }

    let mut st = state.status.lock().unwrap();
    st.fetching = None;
    st.error = error;
    st.rate_remaining = pacing.remaining.or(facts.fresh_remaining(now));
    st.next_at = pacing.not_before;
    if gap > 120 {
        st.paused_until = pacing.not_before;
    }
    drop(st);
    emit_changed(app);
}

pub fn spawn_fetcher(app: AppHandle, rx: mpsc::Receiver<Wake>) {
    std::thread::Builder::new()
        .name("pr-fetcher".into())
        .spawn(move || {
            let mut pacing = Pacing::default();
            let mut last_manual: Option<Time> = None;
            loop {
                step(&app, &mut pacing);
                note_tray_line(&app);
                let wait = pacing
                    .not_before
                    .and_then(|t| (t - Utc::now()).to_std().ok())
                    .filter(|d| !d.is_zero())
                    .map_or(TICK, |d| d.min(TICK));
                match rx.recv_timeout(wait) {
                    Ok(first) => {
                        let all = std::iter::once(first)
                            .chain(rx.try_iter())
                            .any(|w| w == Wake::All);
                        let now = Utc::now();
                        let fresh =
                            last_manual.is_none_or(|t| (now - t).num_seconds() >= MANUAL_DEBOUNCE);
                        if all && fresh {
                            last_manual = Some(now);
                            let state = app.state::<PrReview>();
                            let cfg = state.config.lock().unwrap().clone();
                            *state.forced_lists.lock().unwrap() = cfg.watched(&shared(&app).repos);
                        }
                        // By hand skips the gap but never a rate-limit backoff or a
                        // low-budget pause.
                        let budget_ok = !pacing.remaining.is_some_and(|r| {
                            r < app.state::<PrReview>().config.lock().unwrap().budget_floor
                        });
                        if (fresh || !all) && pacing.strikes == 0 && budget_ok {
                            pacing.not_before = None;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(TICK),
                }
            }
        })
        .expect("spawn fetch thread");
}

// ───────────────────────────── Claude ─────────────────────────────

/// What to ask Claude next.
#[derive(Debug, PartialEq)]
enum ClaudeJob {
    Batch { repo: String, numbers: Vec<u64> },
    Group { repo: String },
}

/// The next batch: the best inspected, unanalysed non-draft PR across `repos`,
/// and the best others in its repo. Only those closest to landing are worth
/// paying to summarize before the rest.
fn next_batch(
    state: &PrReview,
    cfg: &Config,
    repos: &[String],
    batch_size: usize,
) -> Option<ClaudeJob> {
    let store = state.store.lock().unwrap();
    // The best candidate's score, its repo, and that repo's (score, number)s.
    type Best = (i32, String, Vec<(i32, u64)>);
    let mut best: Option<Best> = None;
    for repo in repos {
        let Some(view) =
            views::repo_view(&store, repo, repos, cfg, &pr_core::StatusFacts::default())
        else {
            continue;
        };
        let mut cands: Vec<(i32, u64)> = view
            .prs
            .iter()
            .filter(|r| r.inspected && !r.analysed && r.state != PrState::Draft)
            .map(|r| (r.focus, r.number))
            .collect();
        cands.sort_by_key(|(f, _)| std::cmp::Reverse(*f));
        if let Some((top, _)) = cands.first().copied() {
            if best.as_ref().is_none_or(|(f, _, _)| top > *f) {
                best = Some((top, repo.clone(), cands));
            }
        }
    }
    best.map(|(_, repo, cands)| ClaudeJob::Batch {
        repo,
        numbers: cands.into_iter().take(batch_size).map(|(_, n)| n).collect(),
    })
}

fn next_group(
    state: &PrReview,
    c: &ClaudeConfig,
    repos: &[String],
    now: Time,
) -> Option<ClaudeJob> {
    let store = state.store.lock().unwrap();
    repos
        .iter()
        .filter_map(|r| store.repos.get(r))
        .filter(|d| d.full_list_at.is_some() && d.prs.len() >= MIN_PRS_TO_GROUP)
        .filter(|d| {
            d.groups_at
                .is_none_or(|t| (now - t).num_minutes() as f64 >= c.group_refresh_hours * 60.0)
        })
        .min_by_key(|d| d.groups_at)
        .map(|d| ClaudeJob::Group {
            repo: d.repo.clone(),
        })
}

fn apply_analyses(state: &PrReview, repo: &str, results: Vec<(u64, pr_core::Analysis)>) {
    let mut store = state.store.lock().unwrap();
    let d = store.repo_mut(repo);
    for (n, a) in results {
        if let Some(p) = d.prs.get_mut(&n) {
            // A deep look is kept until the PR's head moves.
            let keep = p
                .analysis
                .as_ref()
                .is_some_and(|old| old.deep && !a.deep && old.for_head == a.for_head);
            if !keep {
                p.analysis = Some(a);
            }
        }
    }
    if let Err(e) = store.save(repo) {
        eprintln!("{repo}: could not save: {e}");
    }
}

fn run_batch(
    state: &PrReview,
    c: &ClaudeConfig,
    repo: &str,
    numbers: &[u64],
) -> Result<f64, String> {
    let prs: Vec<Pr> = {
        let store = state.store.lock().unwrap();
        numbers
            .iter()
            .filter_map(|n| store.pr(repo, *n).cloned())
            .collect()
    };
    let refs: Vec<&Pr> = prs.iter().collect();
    let cwd = state.claude_dir.clone();
    let (results, cost) =
        pr_claude::analyse_batch(c, &cwd, repo, &refs).map_err(|e| e.to_string())?;
    // A PR the reply left out is marked done for this head, so it can't be
    // retried, and paid for, forever.
    let mut out = results;
    for p in &refs {
        if !out.iter().any(|(n, _)| *n == p.number) {
            out.push((
                p.number,
                pr_core::Analysis {
                    at: Some(Utc::now()),
                    model: c.model.clone(),
                    for_head: p.head_oid.clone(),
                    ..Default::default()
                },
            ));
        }
    }
    apply_analyses(state, repo, out);
    Ok(cost)
}

fn run_group(state: &PrReview, c: &ClaudeConfig, repo: &str) -> Result<f64, String> {
    let (prs, local) = {
        let store = state.store.lock().unwrap();
        let d = store.repos.get(repo).ok_or("repo is gone")?;
        let mut prs: Vec<Pr> = d.prs.values().filter(|p| !p.is_draft).cloned().collect();
        prs.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
        prs.truncate(pr_claude::MAX_GROUPING_PRS);
        (prs, d.groups.clone())
    };
    let refs: Vec<&Pr> = prs.iter().collect();
    let cwd = state.claude_dir.clone();
    let (groups, cost) =
        pr_claude::group_repo(c, &cwd, repo, &refs, &local).map_err(|e| e.to_string())?;
    let mut store = state.store.lock().unwrap();
    let d = store.repo_mut(repo);
    d.claude_groups = groups;
    d.groups_at = Some(Utc::now());
    refresh_groups(d);
    if let Err(e) = store.save(repo) {
        eprintln!("{repo}: could not save: {e}");
    }
    Ok(cost)
}

fn run_request(
    state: &PrReview,
    c: &ClaudeConfig,
    shared_token: &str,
    req: &ClaudeRequest,
) -> Result<f64, String> {
    let pr = {
        let store = state.store.lock().unwrap();
        store
            .pr(&req.repo, req.number)
            .cloned()
            .ok_or("the PR is no longer open")?
    };
    let cwd = state.claude_dir.clone();
    if req.deep {
        let gh = resolve_token(shared_token)
            .and_then(|t| GitHub::new(t.value, shared_cache(state)))
            .map_err(|e| e.to_string())?;
        let (diff, cut) = gh.diff(&req.repo, req.number).map_err(|e| e.to_string())?;
        let (a, cost) = pr_claude::analyse_deep(c, &cwd, &req.repo, &pr, &diff, cut)
            .map_err(|e| e.to_string())?;
        apply_analyses(state, &req.repo, vec![(req.number, a)]);
        Ok(cost)
    } else {
        run_batch(state, c, &req.repo, &[req.number])
    }
}

pub fn spawn_claude(app: AppHandle, rx: mpsc::Receiver<()>) {
    std::thread::Builder::new()
        .name("pr-claude".into())
        .spawn(move || {
            let state = app.state::<PrReview>();
            let mut ledger = pr_claude::Ledger::load(&state.dir);
            let mut backoff_until: Option<Time> = None;
            let mut groups_turn = true;
            loop {
                let now = Utc::now();
                let cfg = state.config.lock().unwrap().clone();
                let shared = shared(&app);
                let repos = cfg.watched(&shared.repos);
                ledger.roll(now);
                let c = &cfg.claude;
                let available = pr_claude::find_binary(&c.binary).is_some();
                let request = state.claude_requests.lock().unwrap().pop_front();
                let allowed = c.enabled
                    && available
                    && ledger.allows(c)
                    && backoff_until.is_none_or(|t| now >= t);
                let job = if allowed && request.is_none() {
                    let pick = |groups: bool| {
                        if groups {
                            next_group(&state, c, &repos, now)
                        } else {
                            next_batch(&state, &cfg, &repos, c.batch_size)
                        }
                    };
                    let first = pick(groups_turn);
                    let job = first.or_else(|| pick(!groups_turn));
                    if job.is_some() {
                        groups_turn = !groups_turn;
                    }
                    job
                } else {
                    None
                };
                let label = match (&request, &job) {
                    (Some(r), _) => Some(format!("#{} in {}", r.number, r.repo)),
                    (_, Some(ClaudeJob::Batch { repo, numbers })) => {
                        Some(format!("{} PRs in {repo}", numbers.len()))
                    }
                    (_, Some(ClaudeJob::Group { repo })) => Some(format!("Grouping {repo}")),
                    _ => None,
                };
                let publish = |ledger: &pr_claude::Ledger,
                               running: Option<String>,
                               last_error: Option<Option<String>>| {
                    let mut st = state.status.lock().unwrap();
                    let cs = &mut st.claude;
                    cs.enabled = c.enabled;
                    cs.available = available;
                    cs.model = c.model.clone();
                    cs.running = running;
                    cs.spent_today = ledger.usd;
                    cs.max_usd_per_day = c.max_usd_per_day;
                    cs.runs_last_hour = ledger.runs.len() as u32;
                    cs.max_runs_per_hour = c.max_runs_per_hour;
                    if let Some(e) = last_error {
                        cs.last_error = e;
                    }
                };
                publish(&ledger, label.clone(), None);
                if label.is_some() {
                    emit_changed(&app);
                    let result = match (&request, &job) {
                        (Some(r), _) => run_request(&state, c, &shared.github_token, r),
                        (_, Some(ClaudeJob::Batch { repo, numbers })) => {
                            run_batch(&state, c, repo, numbers)
                        }
                        (_, Some(ClaudeJob::Group { repo })) => run_group(&state, c, repo),
                        _ => Ok(0.0),
                    };
                    let done = Utc::now();
                    let outcome = match result {
                        Ok(cost) => {
                            ledger.record(done, cost);
                            None
                        }
                        Err(e) => {
                            eprintln!("claude: {e}");
                            backoff_until = Some(done + chrono::Duration::seconds(CLAUDE_BACKOFF));
                            Some(e)
                        }
                    };
                    if let Err(e) = ledger.save(&state.dir) {
                        eprintln!("could not save the Claude ledger: {e}");
                    }
                    publish(&ledger, None, Some(outcome));
                    emit_changed(&app);
                    // Asking by hand goes straight on to any others queued.
                    if !state.claude_requests.lock().unwrap().is_empty() {
                        continue;
                    }
                }
                let _ = rx.recv_timeout(CLAUDE_TICK);
            }
        })
        .expect("spawn Claude thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use pr_core::{Analysis, ClaudeGroup};

    #[test]
    fn rate_limits_end_when_github_says() {
        let now = "2026-10-18T04:20:00Z".parse::<Time>().unwrap();
        assert_eq!(
            rate_limit_end("the top of the hour", now)
                .unwrap()
                .to_rfc3339(),
            "2026-10-18T05:00:00+00:00"
        );
        assert!(rate_limit_end("later", now).is_none());
    }

    #[test]
    fn spending_ages_out_after_an_hour() {
        let now = "2026-10-18T04:20:00Z".parse::<Time>().unwrap();
        let mut p = Pacing::default();
        p.note(
            now - chrono::Duration::minutes(70),
            &[Spend {
                cost: 100,
                ..Default::default()
            }],
        );
        p.note(
            now - chrono::Duration::minutes(5),
            &[Spend {
                cost: 16,
                remaining: Some(4000),
                reset_at: None,
            }],
        );
        assert_eq!(p.points_last_hour(now), 16);
        assert_eq!(p.remaining, Some(4000));
    }

    #[test]
    fn a_fetch_keeps_what_claude_wrote_meanwhile() {
        let mut current = RepoData {
            repo: "o/r".into(),
            claude_groups: vec![ClaudeGroup {
                title: "New".into(),
                numbers: vec![1, 2],
                ..Default::default()
            }],
            ..Default::default()
        };
        current.prs.insert(
            1,
            Pr {
                number: 1,
                analysis: Some(Analysis {
                    summary: "fresh".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let mut fetched = RepoData {
            repo: "o/r".into(),
            ..Default::default()
        };
        fetched.prs.insert(
            1,
            Pr {
                number: 1,
                title: "Updated".into(),
                ..Default::default()
            },
        );
        fetched.prs.insert(
            2,
            Pr {
                number: 2,
                ..Default::default()
            },
        );
        merge_fetched(&mut current, fetched);
        assert_eq!(current.prs[&1].title, "Updated");
        assert_eq!(current.prs[&1].analysis.as_ref().unwrap().summary, "fresh");
        assert_eq!(current.groups[0].title, "New");
    }
}
