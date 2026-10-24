//! Rows the poller adds to the Repos table on its own: each repo where the
//! token's user has an open pull request, when the config lists another repo
//! of the same owner ([`Config::may_add`]).
//!
//! Every `poll_minutes`, a round of searches finds them: one search per
//! configured owner. The poll loop sends each search the way it sends a repo
//! fetch: not during the gap after a fetch or a pause, not ahead of a row
//! refreshed by hand, and not while fewer than [`OPTIONAL_FLOOR`] GraphQL
//! points are left. The gap follows each search, and a rate limit backs off
//! as after a fetch. An added row is removed once two rounds in a row find
//! none of the user's open pull requests there. Only a row's Keep and Hide
//! write the config file.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use craft_core::config::RepoList;
use craft_core::{Config, Snapshot};
use craft_github::{resolve_token, Error as GhError, GitHub, OpenPrs};
use tauri::{AppHandle, Manager};

use super::{
    clock, emit_snapshot, rate_limit_end, reload_config_if_changed, AppState, Pacing, BACKOFF_MAX,
    BACKOFF_MIN, OPTIONAL_FLOOR, REPO_GAP, REPO_RETRY,
};

/// Rounds in a row that find no open pull request in an added repo before
/// its row is removed.
const MISSES: u32 = 2;

/// What the poll thread keeps between searches.
#[derive(Default)]
pub struct Found {
    /// The token the login was read with, and the login.
    login: Option<(String, String)>,
    /// Rounds in a row that missed each added repo, for those they missed.
    misses: BTreeMap<String, u32>,
    /// The round under way.
    round: Option<Round>,
    /// When the last round started.
    ran_at: Option<DateTime<Utc>>,
    /// No round starts before this.
    next_at: Option<DateTime<Utc>>,
}

/// A round of searches, one per configured owner.
#[derive(Debug, PartialEq)]
struct Round {
    /// The owners still to search, in config order.
    owners: Vec<String>,
    /// What the searches so far found.
    open: OpenPrs,
    /// The owners whose search failed or was not read to its end.
    incomplete: Vec<String>,
}

/// A search that failed, by the request that failed.
#[derive(Debug)]
enum SearchError {
    /// Reading the token, or the login of its user, failed.
    Token(GhError),
    /// The search of the owner's pull requests failed.
    Owner(GhError),
}

impl SearchError {
    fn error(&self) -> &GhError {
        match self {
            SearchError::Token(e) | SearchError::Owner(e) => e,
        }
    }
}

/// Keep: writes an added row's repo into the config's `repos`.
#[tauri::command]
pub fn keep_repo(app: AppHandle, repo: String) -> Result<(), String> {
    write_list(&app, RepoList::Watched, &repo)
}

/// Hide: writes an added row's repo into the config's `hidden_auto_repos`,
/// so it isn't added again.
#[tauri::command]
pub fn hide_repo(app: AppHandle, repo: String) -> Result<(), String> {
    write_list(&app, RepoList::Hidden, &repo)
}

fn write_list(app: &AppHandle, list: RepoList, repo: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let added = state.snapshot.lock().unwrap().added();
    if !added.iter().any(|r| r == repo) {
        return Err(format!("{repo} is not a row the app added"));
    }
    let written = Config::path().and_then(|path| Config::add_to_file(&path, list, repo));
    if let Err(e) = written {
        let msg = format!("Could not write the config: {e}");
        state.snapshot.lock().unwrap().error = Some(msg.clone());
        emit_snapshot(app);
        return Err(msg);
    }
    reload_config_if_changed(app);
    let cfg = state.config.lock().unwrap().clone();
    if prune(&cfg, &mut state.snapshot.lock().unwrap()) {
        emit_snapshot(app);
    }
    Ok(())
}

/// Called by the poll loop before each step: drops the added rows the config
/// no longer allows, then sends the next search if one is due.
pub fn discover(app: &AppHandle, pacing: &mut Pacing, found: &mut Found) {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let now = Utc::now();
    let (pruned, run) = {
        let mut snap = state.snapshot.lock().unwrap();
        let pruned = prune(&cfg, &mut snap);
        (pruned, due(found, pacing, &cfg, &snap, now))
    };
    let owner = run.then(|| next_owner(found, &cfg, now)).flatten();
    let Some(owner) = owner else {
        if pruned {
            emit_snapshot(app);
        }
        return;
    };
    let result = search(&cfg, found, &owner);
    let now = Utc::now();
    let msg = pace(pacing, result.as_ref().err().map(SearchError::error), now);
    record(&state, found, &owner, result, msg, now);
    emit_snapshot(app);
}

/// Puts the result of the search for `owner` into the snapshot and saves
/// the cache. `msg` is the rate-limit message from [`pace`], if any.
fn record(
    state: &AppState,
    found: &mut Found,
    owner: &str,
    result: Result<OpenPrs, SearchError>,
    msg: Option<String>,
    now: DateTime<Utc>,
) {
    let mut snap = state.snapshot.lock().unwrap();
    // The config as Keep or Hide left it, if either wrote it during the
    // search. They reload it before they take this lock to prune.
    let cfg = state.config.lock().unwrap().clone();
    let done = match result {
        Ok(one) => {
            if one.graphql_remaining.is_some() {
                snap.rate_limit_remaining = one.graphql_remaining;
            }
            searched(found, one)
        }
        Err(SearchError::Owner(
            e @ (GhError::GraphQl(_) | GhError::NotFound | GhError::Http(422, _)),
        )) => {
            eprintln!("open pull request search for {owner}: {e}");
            failed(found)
        }
        Err(SearchError::Token(e) | SearchError::Owner(e)) => {
            eprintln!("open pull request search for {owner}: {e}");
            // The next round starts over, after the token problem `step`
            // reports or a wait like a failed repo's.
            found.round = None;
            let wait = match e {
                GhError::Unauthorized | GhError::NoToken => cfg.poll_secs(),
                _ => REPO_RETRY,
            };
            found.next_at = Some(now + chrono::Duration::seconds(wait));
            None
        }
    };
    if let Some(round) = done {
        apply_search(found, &cfg, &mut snap, &round.open, &round.incomplete);
        found.next_at = Some(now + chrono::Duration::seconds(cfg.poll_secs()));
    }
    // Keep a config error up until the file is fixed.
    let config_error = snap
        .error
        .as_deref()
        .is_some_and(|e| e.starts_with("Config"));
    if let (Some(msg), false) = (msg, config_error) {
        snap.error = Some(msg);
    }
    if let Some(cache) = &state.cache {
        if let Err(e) = cache.save(&snap) {
            eprintln!("could not save cache: {e}");
        }
    }
}

/// The repos of `owner` where the token's user has an open pull request.
/// The login is read once per token.
fn search(cfg: &Config, found: &mut Found, owner: &str) -> Result<OpenPrs, SearchError> {
    let token = resolve_token(&cfg.github_token).map_err(SearchError::Token)?;
    let gh = GitHub::new(token.value.clone()).map_err(SearchError::Token)?;
    let login = login(found, &token.value, || gh.token_login()).map_err(SearchError::Token)?;
    gh.open_pr_repos(&login, owner).map_err(SearchError::Owner)
}

/// The owner to search next, starting a round over the configured owners
/// when none is under way.
fn next_owner(found: &mut Found, cfg: &Config, now: DateTime<Utc>) -> Option<String> {
    if found.round.is_none() {
        found.ran_at = Some(now);
        found.round = Some(Round {
            owners: cfg.owners(),
            open: OpenPrs {
                complete: true,
                ..Default::default()
            },
            incomplete: Vec::new(),
        });
    }
    let owner = found.round.as_ref()?.owners.first().cloned();
    if owner.is_none() {
        found.round = None;
        found.next_at = Some(now + chrono::Duration::seconds(cfg.poll_secs()));
    }
    owner
}

/// Adds the next owner's search to the round, and the owner to the round's
/// incomplete owners when the search was not read to its end. Returns the
/// round once it has searched every owner.
fn searched(found: &mut Found, one: OpenPrs) -> Option<Round> {
    let round = found.round.as_mut()?;
    if !round.owners.is_empty() {
        let owner = round.owners.remove(0);
        if !one.complete {
            round.incomplete.push(owner);
        }
    }
    round.open.repos.extend(one.repos);
    round.open.graphql_remaining = one.graphql_remaining.or(round.open.graphql_remaining);
    round.open.complete &= one.complete;
    if round.owners.is_empty() {
        found.round.take()
    } else {
        None
    }
}

/// Ends the search for the round's next owner as incomplete, after an error
/// GitHub gives again on every try for that owner, so the round goes on to
/// the next owner and counts no misses for that owner's repos. Returns what
/// [`searched`] returns.
fn failed(found: &mut Found) -> Option<Round> {
    searched(found, OpenPrs::default())
}

/// The login of the user `token` belongs to: the one held for that token, or
/// else the one `read` returns, which is then held.
fn login(
    found: &mut Found,
    token: &str,
    read: impl FnOnce() -> Result<String, GhError>,
) -> Result<String, GhError> {
    if let Some((held, login)) = &found.login {
        if held == token {
            return Ok(login.clone());
        }
    }
    let login = read()?;
    found.login = Some((token.to_string(), login.clone()));
    Ok(login)
}

/// Drops the added rows that `cfg` no longer allows (switched off, hidden,
/// configured, or under an owner it no longer lists). True if it dropped any.
fn prune(cfg: &Config, snap: &mut Snapshot) -> bool {
    let added = snap.added();
    let allowed: Vec<String> = added.iter().filter(|r| cfg.may_add(r)).cloned().collect();
    if allowed.len() == added.len() {
        return false;
    }
    snap.set_added(&allowed);
    true
}

/// Whether the poll loop may search at `now`: the switch is on, pacing
/// allows a request, no row refreshed by hand is waiting, at least
/// [`OPTIONAL_FLOOR`] points are left, and a round is under way, due again,
/// or older than a manual refresh.
fn due(found: &Found, pacing: &Pacing, cfg: &Config, snap: &Snapshot, now: DateTime<Utc>) -> bool {
    let budget = snap.rate_limit_remaining;
    cfg.auto_repos
        && !cfg.repos.is_empty()
        && pacing.not_before.is_none_or(|t| now >= t)
        && snap.requested.is_empty()
        && budget.is_none_or(|r| r >= OPTIONAL_FLOOR)
        && (found.round.is_some()
            || found.next_at.is_none_or(|t| now >= t)
            || pacing
                .forced_since
                .is_some_and(|f| found.ran_at.is_none_or(|r| r < f)))
}

/// Sets `pacing` after a search that failed with `error`, if it did, as
/// `step` does after a fetch: the gap, or a backoff on a rate limit. Returns
/// the message for the window.
fn pace(pacing: &mut Pacing, error: Option<&GhError>, now: DateTime<Utc>) -> Option<String> {
    let Some(GhError::RateLimited(until)) = error else {
        pacing.strikes = 0;
        pacing.not_before = Some(now + chrono::Duration::seconds(REPO_GAP));
        return None;
    };
    pacing.strikes += 1;
    let backoff = (BACKOFF_MIN << (pacing.strikes - 1).min(8)).min(BACKOFF_MAX);
    let mut resume = now + chrono::Duration::seconds(backoff);
    if let Some(end) = rate_limit_end(until, now) {
        resume = resume.max(end);
    }
    pacing.not_before = Some(resume);
    Some(format!(
        "Rate limited by GitHub; pausing until {}",
        clock(resume)
    ))
}

/// Updates the added rows from a round that found `open`: a repo `cfg`
/// allows gets a row, and an added row is removed on its [`MISSES`]th round
/// in a row without an open pull request. A row under one of the `incomplete`
/// owners, whose search failed or was not read to its end, counts no miss.
fn apply_search(
    found: &mut Found,
    cfg: &Config,
    snap: &mut Snapshot,
    open: &OpenPrs,
    incomplete: &[String],
) {
    let allowed: BTreeSet<&String> = open.repos.iter().filter(|r| cfg.may_add(r)).collect();
    let mut keep = Vec::new();
    let mut misses = BTreeMap::new();
    for repo in snap.added().into_iter().filter(|r| cfg.may_add(r)) {
        if allowed.iter().any(|o| o.eq_ignore_ascii_case(&repo)) {
            keep.push(repo);
            continue;
        }
        let owner = repo.split('/').next().unwrap_or_default();
        let searched = !incomplete.iter().any(|o| o.eq_ignore_ascii_case(owner));
        let held = found.misses.get(&repo).copied().unwrap_or(0);
        let n = if searched { held + 1 } else { held };
        if n < MISSES {
            misses.insert(repo.clone(), n);
            keep.push(repo);
        }
    }
    found.misses = misses;
    keep.extend(allowed.into_iter().cloned());
    snap.set_added(&keep);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as CDuration;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    const CONFIG: &str = r#"repos = ["acme/one", "acme/two"]"#;

    fn config(extra: &str) -> Config {
        Config::parse(&format!("{CONFIG}\n{extra}")).unwrap()
    }

    fn snapshot(cfg: &Config) -> Snapshot {
        let mut s = Snapshot::default();
        s.sync_repos(&cfg.repos);
        s
    }

    fn open(repos: &[&str]) -> OpenPrs {
        OpenPrs {
            repos: repos.iter().map(|r| r.to_string()).collect(),
            graphql_remaining: Some(4000),
            complete: true,
        }
    }

    fn rows(s: &Snapshot) -> Vec<String> {
        let mark = |auto| if auto { " (added)" } else { "" };
        s.repos
            .iter()
            .map(|e| format!("{}{}", e.repo, mark(e.auto)))
            .collect()
    }

    #[test]
    fn a_search_adds_repos_under_a_configured_owner_only() {
        let cfg = config(r#"hidden_auto_repos = ["acme/hidden"]"#);
        let mut s = snapshot(&cfg);
        let mut found = Found::default();
        let search = open(&["acme/new", "stranger/fork", "ACME/one", "acme/hidden"]);
        apply_search(&mut found, &cfg, &mut s, &search, &[]);
        assert_eq!(rows(&s), ["acme/one", "acme/two", "acme/new (added)"]);
    }

    #[test]
    fn a_round_searches_each_owner_in_a_step_of_its_own() {
        let cfg = Config::parse(r#"repos = ["acme/one", "other-org/lib", "Acme/two"]"#).unwrap();
        let s = snapshot(&cfg);
        let now = Utc::now();
        let mut found = Found::default();
        let mut pacing = Pacing::default();
        let first = next_owner(&mut found, &cfg, now);
        assert_eq!((first.as_deref(), found.ran_at), (Some("acme"), Some(now)));
        let done = searched(&mut found, open(&["acme/new"]));
        assert_eq!(done, None, "a round goes on");
        pace(&mut pacing, None, now);
        let waits = !due(&found, &pacing, &cfg, &s, now);
        assert!(waits, "the gap after each search");
        found.next_at = Some(now + CDuration::seconds(cfg.poll_secs()));
        let later = now + CDuration::seconds(REPO_GAP);
        let goes_on = due(&found, &pacing, &cfg, &s, later);
        assert!(goes_on, "a round under way goes on");
        let second = next_owner(&mut found, &cfg, later);
        assert_eq!(second.as_deref(), Some("other-org"));
        assert_eq!(found.ran_at, Some(now), "the same round");
        let cut_short = OpenPrs {
            complete: false,
            ..open(&["other-org/tool"])
        };
        let all = searched(&mut found, cut_short).unwrap();
        let repos: Vec<&str> = all.open.repos.iter().map(String::as_str).collect();
        assert_eq!(repos, ["acme/new", "other-org/tool"]);
        assert!(!all.open.complete, "one search was cut short");
        assert_eq!(all.incomplete, ["other-org"]);
        assert!(found.round.is_none());
    }

    #[test]
    fn an_added_row_goes_after_two_rounds_without_its_pull_request() {
        let cfg = config("");
        let mut s = snapshot(&cfg);
        let mut found = Found::default();
        let both = open(&["acme/new", "acme/other"]);
        let other = open(&["acme/other"]);
        apply_search(&mut found, &cfg, &mut s, &both, &[]);
        s.repos[2].fetched_at = Some(Utc::now());
        apply_search(&mut found, &cfg, &mut s, &other, &[]);
        let all = [
            "acme/one",
            "acme/two",
            "acme/new (added)",
            "acme/other (added)",
        ];
        assert_eq!(rows(&s), all, "one miss keeps the row");
        assert!(s.repos[2].fetched_at.is_some(), "and its data");
        apply_search(&mut found, &cfg, &mut s, &both, &[]);
        apply_search(&mut found, &cfg, &mut s, &other, &[]);
        assert_eq!(rows(&s), all, "a found pull request starts the count again");
        let cut_short = OpenPrs {
            complete: false,
            ..open(&["acme/other"])
        };
        let acme = ["acme".to_string()];
        apply_search(&mut found, &cfg, &mut s, &cut_short, &acme);
        assert_eq!(rows(&s), all, "a round cut short is no miss");
        apply_search(&mut found, &cfg, &mut s, &other, &[]);
        let left = ["acme/one", "acme/two", "acme/other (added)"];
        assert_eq!(rows(&s), left, "two misses in a row");
        apply_search(&mut found, &cfg, &mut s, &open(&[]), &[]);
        apply_search(&mut found, &cfg, &mut s, &open(&[]), &[]);
        assert_eq!(rows(&s), ["acme/one", "acme/two"]);
    }

    #[test]
    fn with_auto_repos_off_nothing_is_added_or_searched() {
        let on = config("");
        let mut s = snapshot(&on);
        let mut found = Found::default();
        apply_search(&mut found, &on, &mut s, &open(&["acme/new"]), &[]);
        let off = config("auto_repos = false");
        s.sync_repos(&off.repos);
        assert!(prune(&off, &mut s), "the switch drops the rows it added");
        assert_eq!(rows(&s), ["acme/one", "acme/two"]);
        apply_search(&mut found, &off, &mut s, &open(&["acme/new"]), &[]);
        assert_eq!(rows(&s), ["acme/one", "acme/two"]);
        assert!(!due(&found, &Pacing::default(), &off, &s, Utc::now()));
    }

    #[test]
    fn kept_and_hidden_rows_follow_the_reloaded_config() {
        let cfg = config("");
        let mut s = snapshot(&cfg);
        let mut found = Found::default();
        let search = open(&["acme/kept", "acme/hidden"]);
        apply_search(&mut found, &cfg, &mut s, &search, &[]);
        let added = [
            "acme/one",
            "acme/two",
            "acme/hidden (added)",
            "acme/kept (added)",
        ];
        assert_eq!(rows(&s), added);
        s.repos[3].fetched_at = Some(Utc::now());

        // What Keep and Hide wrote, read back as the poll thread reads it.
        let written = Config::parse(
            r#"repos = ["acme/one", "acme/two", "acme/kept"]
               hidden_auto_repos = ["acme/hidden"]"#,
        )
        .unwrap();
        s.sync_repos(&written.repos);
        assert!(prune(&written, &mut s));
        assert_eq!(rows(&s), ["acme/one", "acme/two", "acme/kept"]);
        assert!(s.repos[2].fetched_at.is_some(), "Keep keeps the data");
        apply_search(&mut found, &written, &mut s, &search, &[]);
        let kept = ["acme/one", "acme/two", "acme/kept"];
        assert_eq!(rows(&s), kept, "the hidden repo stays out");
    }

    #[test]
    fn a_search_that_ends_after_a_hide_uses_the_config_hide_wrote() {
        let before = config("");
        let mut found = Found::default();
        let owner = next_owner(&mut found, &before, Utc::now()).unwrap();
        // Hide wrote the config and pruned while the search was in flight.
        let hidden = config(r#"hidden_auto_repos = ["acme/hidden"]"#);
        let state = AppState {
            config: Mutex::new(hidden),
            config_mtime: Mutex::new(None),
            snapshot: Mutex::new(snapshot(&before)),
            cache: None,
            refresh_tx: Mutex::new(None),
            window_state: Mutex::new(None),
            window_dirty: AtomicBool::new(false),
        };
        let result = Ok(open(&["acme/hidden", "acme/new"]));
        record(&state, &mut found, &owner, result, None, Utc::now());
        let s = state.snapshot.lock().unwrap();
        assert_eq!(rows(&s), ["acme/one", "acme/two", "acme/new (added)"]);
        assert_eq!(s.rate_limit_remaining, Some(4000));
        assert!(found.round.is_none() && found.next_at.is_some());
    }

    #[test]
    fn an_owner_whose_search_fails_does_not_stop_the_round() {
        let cfg = Config::parse(r#"repos = ["acme/one", "other-org/lib"]"#).unwrap();
        let mut s = snapshot(&cfg);
        let mut found = Found::default();
        apply_search(&mut found, &cfg, &mut s, &open(&["acme/old"]), &[]);
        let state = AppState {
            config: Mutex::new(cfg.clone()),
            config_mtime: Mutex::new(None),
            snapshot: Mutex::new(s),
            cache: None,
            refresh_tx: Mutex::new(None),
            window_state: Mutex::new(None),
            window_dirty: AtomicBool::new(false),
        };
        let now = Utc::now();
        let refused = GhError::GraphQl("the listed users cannot be searched".into());
        let invalid = GhError::Http(422, "Validation Failed".into());
        for error in [refused, GhError::NotFound, invalid] {
            let acme = next_owner(&mut found, &cfg, now).unwrap();
            let result = Err(SearchError::Owner(error));
            record(&state, &mut found, &acme, result, None, now);
            assert!(found.round.is_some(), "the round goes on");
            let other = next_owner(&mut found, &cfg, now).unwrap();
            assert_eq!(other, "other-org");
            let result = Ok(open(&["other-org/new"]));
            record(&state, &mut found, &other, result, None, now);
            assert!(found.round.is_none(), "the round ends");
            let poll = Some(now + CDuration::seconds(cfg.poll_secs()));
            assert_eq!(found.next_at, poll);
        }
        let all = [
            "acme/one",
            "other-org/lib",
            "acme/old (added)",
            "other-org/new (added)",
        ];
        let s = rows(&state.snapshot.lock().unwrap());
        assert_eq!(s, all, "a round with a failed search is no miss");

        let acme = next_owner(&mut found, &cfg, now).unwrap();
        let down = SearchError::Owner(GhError::Http(502, "bad gateway".into()));
        record(&state, &mut found, &acme, Err(down), None, now);
        assert!(found.round.is_none(), "any other error starts over");
        let retry = Some(now + CDuration::seconds(REPO_RETRY));
        assert_eq!(found.next_at, retry);
    }

    #[test]
    fn a_failed_login_read_starts_the_round_over() {
        let cfg = Config::parse(r#"repos = ["acme/one", "other-org/lib"]"#).unwrap();
        let mut s = snapshot(&cfg);
        let mut found = Found::default();
        apply_search(&mut found, &cfg, &mut s, &open(&["acme/old"]), &[]);
        let state = AppState {
            config: Mutex::new(cfg.clone()),
            config_mtime: Mutex::new(None),
            snapshot: Mutex::new(s),
            cache: None,
            refresh_tx: Mutex::new(None),
            window_state: Mutex::new(None),
            window_dirty: AtomicBool::new(false),
        };
        let now = Utc::now();
        let no_login = GhError::GraphQl("no viewer.login".into());
        let invalid = GhError::Http(422, "Validation Failed".into());
        for error in [no_login, GhError::NotFound, invalid] {
            let acme = next_owner(&mut found, &cfg, now).unwrap();
            let result = Err(SearchError::Token(error));
            record(&state, &mut found, &acme, result, None, now);
            assert!(found.round.is_none(), "the round starts over");
            let retry = Some(now + CDuration::seconds(REPO_RETRY));
            assert_eq!(found.next_at, retry);
        }
        let s = rows(&state.snapshot.lock().unwrap());
        let all = ["acme/one", "other-org/lib", "acme/old (added)"];
        assert_eq!(s, all, "no round ended");
    }

    #[test]
    fn a_miss_counts_only_where_the_owners_search_was_complete() {
        let cfg = Config::parse(r#"repos = ["acme/one", "other-org/lib"]"#).unwrap();
        let now = Utc::now();
        for cut_short in [false, true] {
            let mut s = snapshot(&cfg);
            let mut found = Found::default();
            let both = open(&["acme/old", "other-org/old"]);
            apply_search(&mut found, &cfg, &mut s, &both, &[]);
            let state = AppState {
                config: Mutex::new(cfg.clone()),
                config_mtime: Mutex::new(None),
                snapshot: Mutex::new(s),
                cache: None,
                refresh_tx: Mutex::new(None),
                window_state: Mutex::new(None),
                window_dirty: AtomicBool::new(false),
            };
            for _ in 0..MISSES {
                let acme = next_owner(&mut found, &cfg, now).unwrap();
                let result = if cut_short {
                    Ok(OpenPrs {
                        complete: false,
                        ..open(&[])
                    })
                } else {
                    let refused = GhError::GraphQl("cannot be searched".into());
                    Err(SearchError::Owner(refused))
                };
                record(&state, &mut found, &acme, result, None, now);
                let other = next_owner(&mut found, &cfg, now).unwrap();
                record(&state, &mut found, &other, Ok(open(&[])), None, now);
                assert!(found.round.is_none(), "the round ends");
            }
            let s = rows(&state.snapshot.lock().unwrap());
            let left = ["acme/one", "other-org/lib", "acme/old (added)"];
            assert_eq!(s, left, "acme's search cut short: {cut_short}");
        }
    }

    #[test]
    fn a_search_waits_for_the_poll_loops_gap_pauses_and_budget() {
        let cfg = config("");
        let mut s = snapshot(&cfg);
        let now = Utc::now();
        let mut found = Found::default();
        let mut pacing = Pacing::default();
        let due_now =
            |found: &Found, pacing: &Pacing, s: &Snapshot| due(found, pacing, &cfg, s, now);
        assert!(due_now(&found, &pacing, &s), "the first search goes");

        pacing.not_before = Some(now + CDuration::seconds(5));
        assert!(!due_now(&found, &pacing, &s), "the gap or a pause");
        pacing.not_before = None;

        s.rate_limit_remaining = Some(OPTIONAL_FLOOR - 1);
        assert!(!due_now(&found, &pacing, &s), "below the budget floor");
        s.rate_limit_remaining = Some(OPTIONAL_FLOOR);
        assert!(due_now(&found, &pacing, &s));

        assert!(s.request("acme/one"));
        assert!(!due_now(&found, &pacing, &s), "a hand refresh goes first");
        s.take_requested();

        found.ran_at = Some(now - CDuration::seconds(1));
        found.next_at = Some(now + CDuration::seconds(cfg.poll_secs()));
        assert!(!due_now(&found, &pacing, &s), "searched this poll_minutes");
        pacing.forced_since = Some(now);
        assert!(due_now(&found, &pacing, &s), "a manual refresh");
    }

    #[test]
    fn the_login_is_read_once_per_token() {
        let mut found = Found::default();
        let mut reads = 0;
        let mut read = |login: &str| {
            reads += 1;
            Ok::<_, GhError>(login.to_string())
        };
        let ann = login(&mut found, "t1", || read("ann-example")).unwrap();
        assert_eq!(ann, "ann-example");
        let again = login(&mut found, "t1", || read("unused")).unwrap();
        assert_eq!(again, "ann-example", "held for the same token");
        let bo = login(&mut found, "t2", || read("bo-example")).unwrap();
        assert_eq!(bo, "bo-example", "read again for another token");
        let failed = login(&mut found, "t3", || Err(GhError::Unauthorized));
        assert!(matches!(failed, Err(GhError::Unauthorized)));
        let held = login(&mut found, "t2", || read("unused")).unwrap();
        assert_eq!(held, "bo-example", "a failed read keeps the held login");
        assert_eq!(reads, 2);
    }

    #[test]
    fn the_gap_follows_a_search_and_a_rate_limit_backs_off() {
        let now: DateTime<Utc> = "2026-10-18T04:20:00Z".parse().unwrap();
        let gap = Some(now + CDuration::seconds(REPO_GAP));
        let mut pacing = Pacing {
            strikes: 2,
            ..Default::default()
        };
        assert_eq!(pace(&mut pacing, None, now), None);
        assert_eq!((pacing.not_before, pacing.strikes), (gap, 0));

        let network = GhError::Http(502, "bad gateway".into());
        assert_eq!(pace(&mut pacing, Some(&network), now), None);
        assert_eq!(pacing.not_before, gap);

        let limited = GhError::RateLimited("2026-10-18T04:58:00Z".into());
        let msg = pace(&mut pacing, Some(&limited), now).unwrap();
        assert!(msg.starts_with("Rate limited by GitHub; pausing until "));
        assert_eq!(pacing.strikes, 1);
        let github = "2026-10-18T04:58:00Z".parse().ok();
        assert_eq!(pacing.not_before, github, "GitHub's time is later");
        let later = GhError::RateLimited("later".into());
        pace(&mut pacing, Some(&later), now);
        let doubled = Some(now + CDuration::seconds(2 * BACKOFF_MIN));
        assert_eq!(pacing.not_before, doubled, "the backoff doubles");
    }
}
