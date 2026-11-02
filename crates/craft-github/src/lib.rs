//! Fetches one repository's [`RepoStats`] from GitHub.
//!
//! Per repo: one (usually) GraphQL query for releases + asset download
//! counts; one GraphQL query for counts, dates, each recent-activity window's
//! commits ([`WINDOWS`], plus [`SINCE_RELEASE`], which needs the releases),
//! the app icon's blob id and the open issues to rank; one GraphQL query
//! comparing the latest build's tag with `main`; one or more GraphQL queries
//! for the commit authors, and one or more for the issues opened, since a week
//! ago or the latest build, whichever is earlier, which give each window's people
//! and issue counts. No search API: GitHub limits searches far more tightly than
//! other queries, even inside GraphQL;
//! The authors and issues are kept between polls ([`RawActivity`]), so after the
//! first poll those queries fetch only what's new. PR counts come from the
//! [`PrLedger`] shared with the PR poller (see [`GitHub::sync_ledger`]), which
//! either app may have brought up to date already;
//! one (usually) REST call for the contributor list, which GraphQL lacks.
//! The icon itself is a REST blob fetch, made only when its blob id changes
//! (see [`GitHub::icon`]). Blocking I/O: callers run this on worker threads.
//! The counts query also reads when the newest pull request was updated
//! ([`RawActivity::pr_updated_at`]). The People thread reads a repository's
//! changed pull requests when that time has moved ([`prs`]).
//!
//! A client made with [`GitHub::in_background`] sends each request only
//! while the poll thread holds no [`gate::PollSession`].

pub mod gate;
pub mod prs;

pub use gate::Gate;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use craft_core::{
    rank_issues, summarize_releases, Activity, Asset, BuildLag, IssueInput, LagBasis, Lease,
    LedgerPr, PrLedger, Release, RepoStats, SharedCache, ALL_TIME, SINCE_RELEASE, WINDOWS,
};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const API: &str = "https://api.github.com";
/// How many ranked issues each repo keeps for the Urgent view.
const KEEP_URGENT: usize = 20;
/// After each GraphQL request, wait this many times as long as it took.
/// GitHub allows 60 s of GraphQL server time per minute; back-to-back heavy
/// queries exceed that within a minute (a secondary rate limit), so requests
/// from one client keep to about half the wall clock.
const GRAPHQL_PACING: f64 = 1.0;
/// How far before the previous fetch an incremental one starts, so nothing
/// that landed while it ran slips between the two.
const OVERLAP_MINUTES: i64 = 10;
/// How often [`RawActivity`] is refetched from scratch, dropping deleted
/// issues and picking up commits that reached `main` with older dates.
const FULL_REFRESH_HOURS: i64 = 6;
/// Pages of 100 each history walk may take; a busier stretch undercounts its
/// oldest windows (photocraft opens ~2000 PRs a week).
const MAX_PAGES: usize = 30;
/// How long a request may take to connect, and to finish, body included.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Pages of 100 a walk of the shared PR ledger may take. They're cheap (a point
/// each), it's kept from then on, and it has to get through every PR touched in
/// the window.
const LEDGER_MAX_PAGES: usize = 100;
/// A ledger either app walked this recently is good enough.
const LEDGER_MAX_AGE_SECS: i64 = 5 * 60;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no GitHub token: set CRAFT_STATUS_GITHUB_TOKEN or GITHUB_TOKEN, set github_token in ~/.craft_status_config.toml, or run `gh auth login`")]
    NoToken,
    #[error("GitHub rejected the token (401): it may have expired or been revoked")]
    Unauthorized,
    #[error("rate limited by GitHub until {0}")]
    RateLimited(String),
    /// Not sent: a background request was rate-limited, or the poll thread
    /// paused background requests (see [`Gate`]).
    #[error("stopped: {0}")]
    Halted(String),
    /// The repo doesn't exist or the token can't see it (GitHub answers 404 for private repos).
    #[error("repository not found or not accessible")]
    NotFound,
    #[error("GitHub HTTP {0}: {1}")]
    Http(u16, String),
    #[error("network: {0}")]
    Network(#[from] reqwest::Error),
    #[error("GitHub: {0}")]
    GraphQl(String),
}

impl Error {
    /// GitHub could not finish the query: a server error such as the 502 it
    /// sends when a query runs too long, or a GraphQL error that names a
    /// timeout. A smaller query may succeed. A request that got no answer in
    /// time is a [`Error::Network`] error instead.
    pub fn is_overload(&self) -> bool {
        match self {
            Error::Http(500..=599, _) => true,
            Error::GraphQl(msg) => msg.to_ascii_lowercase().contains("timeout"),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Environment variables checked for a token, in order, before the config.
pub const TOKEN_VARS: [&str; 3] = ["CRAFT_STATUS_GITHUB_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"];

/// A GitHub token and where it came from (`$GITHUB_TOKEN`, `config`, `gh CLI`…).
pub struct Token {
    pub value: String,
    pub source: &'static str,
}

/// Token from [`TOKEN_VARS`], then config, then the GitHub CLI.
/// Apps launched from Finder don't inherit the shell PATH, so the usual
/// install locations of `gh` are tried explicitly.
pub fn resolve_token(configured: &str) -> Result<Token> {
    if let Some(t) = env_or_config_token(configured, |var| std::env::var(var).ok()) {
        return Ok(t);
    }
    for gh in [
        "gh",
        "/opt/homebrew/bin/gh",
        "/usr/local/bin/gh",
        "/usr/bin/gh",
    ] {
        if let Ok(out) = Command::new(gh).args(["auth", "token"]).output() {
            let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() && !tok.is_empty() {
                return Ok(Token {
                    value: tok,
                    source: "gh CLI",
                });
            }
        }
    }
    Err(Error::NoToken)
}

/// The first non-blank of [`TOKEN_VARS`] (looked up with `env`), else `configured`.
fn env_or_config_token(configured: &str, env: impl Fn(&str) -> Option<String>) -> Option<Token> {
    const SOURCES: [&str; 3] = ["$CRAFT_STATUS_GITHUB_TOKEN", "$GITHUB_TOKEN", "$GH_TOKEN"];
    TOKEN_VARS
        .iter()
        .zip(SOURCES)
        .filter_map(|(var, source)| Some((env(var)?, source)))
        .chain([(configured.to_string(), "config")])
        .map(|(t, source)| Token {
            value: t.trim().to_string(),
            source,
        })
        .find(|t| !t.value.is_empty())
}

pub struct GitHub {
    http: Client,
    token: String,
    /// Base URL of the API: [`API`], or a local stub in tests.
    api: String,
    /// For a background client, the gate its requests wait at.
    gate: Option<Arc<Gate>>,
    /// For a background client, the shared cache whose lease each of its
    /// requests holds, and how long a request waits for it.
    lease: Option<(SharedCache, Duration)>,
    /// GraphQL requests sent, for the tests.
    #[cfg(test)]
    graphql_sent: std::sync::atomic::AtomicU64,
    pacing: f64,
}

/// What a walk of the shared ledger left behind and what it cost.
#[derive(Debug, Clone, Default)]
pub struct LedgerSync {
    pub ledger: PrLedger,
    pub cost: u64,
    pub remaining: Option<u64>,
    pub reset_at: Option<DateTime<Utc>>,
}

/// Rate-limit bookkeeping returned alongside each repo.
#[derive(Debug, Clone, Copy, Default)]
pub struct Budget {
    pub graphql_remaining: Option<u64>,
}

impl GitHub {
    pub fn new(token: String) -> Result<GitHub> {
        Self::with_api(token, API)
    }

    /// A client that sends its requests to `api` instead of api.github.com.
    pub fn with_api(token: impl Into<String>, api: impl Into<String>) -> Result<GitHub> {
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        Ok(GitHub {
            http,
            token: token.into(),
            api: api.into(),
            gate: None,
            lease: None,
            #[cfg(test)]
            graphql_sent: Default::default(),
            pacing: GRAPHQL_PACING,
        })
    }

    /// Wait `pacing` times as long as each GraphQL request took, after it.
    pub fn with_pacing(mut self, pacing: f64) -> GitHub {
        self.pacing = pacing;
        self
    }

    /// GraphQL requests this client has sent.
    #[cfg(test)]
    fn graphql_sent(&self) -> u64 {
        self.graphql_sent.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// This client with every request sent as a background request at
    /// `gate`: it waits while the poll thread fetches a repository, and
    /// fails with [`Error::Halted`] while background requests are stopped.
    /// A rate limit on one of its requests stops them (see [`Gate::halt`]).
    pub fn in_background(mut self, gate: Arc<Gate>) -> GitHub {
        self.gate = Some(gate);
        self
    }

    /// This background client with each request holding `shared`'s lease,
    /// taken after the gate's permit. While another poller holds the lease, a
    /// request lets the permit go and tries again every half second for up to
    /// `patience`, then fails with [`Error::Halted`] without being sent.
    pub fn with_lease(mut self, shared: SharedCache, patience: Duration) -> GitHub {
        self.lease = Some((shared, patience));
        self
    }

    /// Leave to send one request: none for a poll client, a
    /// [`gate::Permit`] for a background one, with the lease when the client
    /// has one ([`GitHub::with_lease`]).
    fn permit(&self) -> Result<(Option<gate::Permit<'_>>, Option<Lease>)> {
        let started = std::time::Instant::now();
        loop {
            let permit = self.gate.as_deref().map(Gate::background).transpose()?;
            let Some((shared, patience)) = self.lease.as_ref().filter(|_| permit.is_some()) else {
                return Ok((permit, None));
            };
            if let Some(lease) = shared.try_lease() {
                return Ok((permit, Some(lease)));
            }
            drop(permit);
            if started.elapsed() >= *patience {
                return Err(Error::Halted("the GitHub lease is held".into()));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// `err`, after a background client has stopped later background
    /// requests when it is a rate limit.
    fn note_limit(&self, err: Error) -> Error {
        if let (Error::RateLimited(until), Some(gate)) = (&err, &self.gate) {
            gate.halt(until.clone());
        }
        err
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(USER_AGENT, "craft-status".parse().unwrap());
        h.insert(ACCEPT, "application/vnd.github+json".parse().unwrap());
        if let Ok(v) = format!("Bearer {}", self.token).parse() {
            h.insert(AUTHORIZATION, v);
        }
        h
    }

    fn check(resp: reqwest::blocking::Response) -> Result<reqwest::blocking::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        if status.as_u16() == 401 {
            return Err(Error::Unauthorized);
        }
        if status.as_u16() == 404 {
            return Err(Error::NotFound);
        }
        let remaining = resp
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok())
        };
        // Secondary (burst) limits say when to retry; the hourly one when it resets.
        let reset = header("retry-after")
            .map(|secs| Utc::now() + chrono::Duration::seconds(secs))
            .or_else(|| header("x-ratelimit-reset").and_then(|t| DateTime::from_timestamp(t, 0)));
        let body = resp.text().unwrap_or_default();
        if (status.as_u16() == 403 || status.as_u16() == 429)
            && (remaining.as_deref() == Some("0") || body.contains("rate limit"))
        {
            return Err(Error::RateLimited(
                reset.map_or("later".into(), |t| t.to_rfc3339()),
            ));
        }
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["message"].as_str().map(str::to_string))
            .unwrap_or_else(|| {
                // Gateways answer with an HTML page: not something to show anyone.
                if body.trim_start().starts_with('<') {
                    "GitHub didn't respond in time".to_string()
                } else {
                    body.chars().take(200).collect()
                }
            });
        Err(Error::Http(status.as_u16(), msg))
    }

    /// The whole answer to a GraphQL request, `data` and `errors` both, or
    /// [`Error::RateLimited`] when GitHub reports a rate limit.
    fn graphql_body(&self, query: &str, variables: Value) -> Result<Value> {
        let permit = self.permit()?;
        #[cfg(test)]
        self.graphql_sent
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let started = std::time::Instant::now();
        let sent = self
            .http
            .post(format!("{}/graphql", self.api))
            .headers(self.headers())
            .json(&json!({ "query": query, "variables": variables }))
            .send();
        let took = started.elapsed();
        let answer = sent
            .map_err(Error::from)
            .and_then(Self::check)
            .and_then(|resp| Ok(resp.json::<Value>()?))
            .and_then(|v| {
                let limited = graphql_errors(&v)
                    .iter()
                    .any(|e| e["type"] == "RATE_LIMITED");
                match limited {
                    true => Err(Error::RateLimited("the top of the hour".into())),
                    false => Ok(v),
                }
            })
            // Kept before the permit goes, so no poll starts in between.
            .map_err(|e| self.note_limit(e));
        if let (Some(_), Some((shared, _)), Ok(v)) = (&permit.1, &self.lease, &answer) {
            note_shared_budget(shared, &v["data"]["rateLimit"]);
        }
        // A background client keeps its permit until the wait ends.
        std::thread::sleep(took.mul_f64(self.pacing));
        drop(permit);
        answer
    }

    /// The JSON body of a REST GET of `url`, or `None` for a 204 answer. A
    /// background client sends it at its gate, and a rate limit on it stops
    /// the client's later requests, as for [`GitHub::graphql_body`].
    fn get_json(&self, url: &str) -> Result<Option<Value>> {
        let permit = self.permit()?;
        let answer = self
            .http
            .get(url)
            .headers(self.headers())
            .send()
            .map_err(Error::from)
            .and_then(Self::check)
            .and_then(|resp| match resp.status().as_u16() {
                204 => Ok(None),
                _ => Ok(Some(resp.json::<Value>()?)),
            })
            .map_err(|e| self.note_limit(e));
        drop(permit);
        answer
    }

    /// The `data` of a query about one `repository`, which must be there.
    fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let mut v = self.graphql_body(query, variables)?;
        let errs = graphql_errors(&v);
        if !errs.is_empty() && v["data"]["repository"].is_null() {
            if errs.iter().any(|e| e["type"] == "NOT_FOUND") {
                return Err(Error::NotFound);
            }
            return Err(Error::GraphQl(error_messages(errs)));
        }
        Ok(v["data"].take())
    }

    /// Everything the UI shows for `owner/name`, measured at `now`. `raw` is
    /// the activity kept from the previous fetch; it's brought up to date
    /// only when the whole fetch succeeds.
    pub fn fetch_repo(
        &self,
        repo: &str,
        now: DateTime<Utc>,
        raw: &mut RawActivity,
        shared: &SharedCache,
    ) -> Result<(RepoStats, Budget)> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| Error::GraphQl(format!("bad repo {repo:?}")))?;
        let releases = self.releases(owner, name)?;
        let release_at = summarize_releases(&releases).latest_at;
        let main = self.graphql(
            &main_query(now, release_at),
            json!({ "owner": owner, "name": name }),
        )?;
        let since_build = self.since_build(owner, name, &main, &releases)?;
        let since = activity_start(now, release_at);
        let mut fresh = raw.clone();
        let (from, full) = fresh.fetch_from(since, now);
        if full {
            fresh = RawActivity {
                full_at: Some(now),
                ..Default::default()
            };
        }
        fresh.commits.extend(self.authors(owner, name, from)?);
        self.issues_opened(owner, name, from, &mut fresh)?;
        fresh.covers_from = Some(since);
        fresh.fetched_at = Some(now);
        fresh.prune(since);
        let authors: Vec<CommitAuthor> = fresh.commits.values().cloned().collect();
        let ledger = self.sync_ledger(repo, since, now, shared)?.ledger;
        let events = Events {
            prs_opened: ledger.opened_since(since),
            prs_merged: ledger.merged_since(since),
            issues_opened: fresh.issues_opened.values().copied().collect(),
        };
        let contributors = self.contributors(repo)?;
        fresh.pr_updated_at = first_time(&main["repository"]["lastPr"], "updatedAt");
        *raw = fresh;
        let budget = Budget {
            graphql_remaining: main["rateLimit"]["remaining"].as_u64(),
        };
        if let Err(e) = shared.note_budget(
            budget.graphql_remaining,
            time(&main["rateLimit"]["resetAt"]),
            now,
        ) {
            eprintln!("could not save the shared budget: {e}");
        }
        let stats = build_stats(
            &main,
            &releases,
            since_build,
            &contributors,
            &authors,
            &events,
            now,
        );
        Ok((stats, budget))
    }

    /// The app icon blob `oid` (from [`RepoStats::icon_oid`]) as a `data:` URI.
    pub fn icon(&self, repo: &str, oid: &str) -> Result<String> {
        let v = self
            .get_json(&format!("{}/repos/{repo}/git/blobs/{oid}", self.api))?
            .unwrap_or_default();
        let b64: String = v["content"]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        Ok(format!("data:image/png;base64,{b64}"))
    }

    /// `(oid, (committed, author))` for each commit on `main` since `since`.
    fn authors(
        &self,
        owner: &str,
        name: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<(String, CommitAuthor)>> {
        let since = since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let v = self.graphql(
                AUTHORS_QUERY,
                json!({ "owner": owner, "name": name, "since": since, "after": after }),
            )?;
            let conn = &v["repository"]["defaultBranchRef"]["target"]["history"];
            out.extend(parse_authors(conn));
            if conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                break;
            }
            after = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
        }
        Ok(out)
    }

    /// When each issue was opened since `since`, added to `raw` by number.
    fn issues_opened(
        &self,
        owner: &str,
        name: &str,
        since: DateTime<Utc>,
        raw: &mut RawActivity,
    ) -> Result<()> {
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let v = self.graphql(
                ISSUES_QUERY,
                json!({ "owner": owner, "name": name, "after": after }),
            )?;
            let conn = &v["repository"]["issues"];
            let (times, done) = parse_events(conn, "createdAt", "createdAt", since);
            raw.issues_opened.extend(times);
            if done || conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                break;
            }
            after = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
        }
        Ok(())
    }

    /// The repo's shared PR ledger, walked if it isn't already complete back to
    /// `since` and recent. Walking by last update reaches every PR opened, merged
    /// or closed since then, so this one list serves both apps. Call holding the
    /// shared lease: this reads, updates and writes the ledger.
    pub fn sync_ledger(
        &self,
        repo: &str,
        since: DateTime<Utc>,
        now: DateTime<Utc>,
        shared: &SharedCache,
    ) -> Result<LedgerSync> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| Error::GraphQl(format!("bad repo {repo:?}")))?;
        let mut out = LedgerSync {
            ledger: load_ledger(shared, repo),
            ..Default::default()
        };
        let Some(plan) = out.ledger.plan_walk(since, now, LEDGER_MAX_AGE_SECS) else {
            return Ok(out);
        };
        let mut after: Option<String> = None;
        let mut reached = false;
        let mut oldest = None;
        for _ in 0..LEDGER_MAX_PAGES {
            let v = self.graphql(
                LEDGER_QUERY,
                json!({ "owner": owner, "name": name, "after": after }),
            )?;
            note_rate_limit(&v, &mut out);
            let conn = &v["repository"]["pullRequests"];
            let (page, done, last) = parse_ledger_page(conn, plan.stop_at);
            for (n, pr) in page {
                out.ledger.record(n, pr);
            }
            oldest = last.or(oldest);
            if done || conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                reached = true;
                break;
            }
            after = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
        }
        out.ledger.finish_walk(plan, now, reached, oldest);
        if let Err(e) = shared.save_ledger(repo, &out.ledger) {
            eprintln!("{repo}: could not save the PR ledger: {e}");
        }
        if let Err(e) = shared.note_budget(out.remaining, out.reset_at, now) {
            eprintln!("could not save the shared budget: {e}");
        }
        Ok(out)
    }

    /// Up to `pages` more of a walk through every merged PR, oldest first, for
    /// when who has landed what matters. Oldest first keeps the cursor valid as
    /// new merges land, and [`GitHub::sync_ledger`] brings those. Call holding
    /// the shared lease.
    pub fn backfill_ledger(
        &self,
        repo: &str,
        pages: usize,
        shared: &SharedCache,
    ) -> Result<LedgerSync> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| Error::GraphQl(format!("bad repo {repo:?}")))?;
        let mut out = LedgerSync {
            ledger: load_ledger(shared, repo),
            ..Default::default()
        };
        for _ in 0..pages {
            if out.ledger.backfill_complete {
                break;
            }
            let v = self.graphql(
                BACKFILL_QUERY,
                json!({ "owner": owner, "name": name, "after": out.ledger.backfill_cursor }),
            )?;
            note_rate_limit(&v, &mut out);
            let conn = &v["repository"]["pullRequests"];
            let (page, _, _) = parse_ledger_page(conn, DateTime::<Utc>::MIN_UTC);
            for (n, pr) in page {
                out.ledger.record(n, pr);
            }
            if let Some(c) = conn["pageInfo"]["endCursor"].as_str() {
                out.ledger.backfill_cursor = Some(c.to_string());
            }
            out.ledger.backfill_complete = conn["pageInfo"]["hasNextPage"].as_bool() != Some(true);
        }
        if let Err(e) = shared.save_ledger(repo, &out.ledger) {
            eprintln!("{repo}: could not save the PR ledger: {e}");
        }
        Ok(out)
    }

    /// Commits on `main` since the latest build; `None` without a build, a
    /// tagged commit or a default branch.
    fn since_build(
        &self,
        owner: &str,
        name: &str,
        main: &Value,
        releases: &[Release],
    ) -> Result<Option<BuildLag>> {
        let rel = summarize_releases(releases);
        let branch = main["repository"]["defaultBranchRef"]["name"].as_str();
        let (Some(tag), Some(sha), Some(at), Some(branch)) =
            (rel.latest_tag, rel.latest_sha, rel.latest_commit_at, branch)
        else {
            return Ok(None);
        };
        let v = self.graphql(
            BUILD_QUERY,
            json!({
                "owner": owner, "name": name, "branch": branch,
                "tag": format!("refs/tags/{tag}"),
                "at": at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            }),
        )?;
        Ok(parse_since_build(&v, &sha))
    }

    fn releases(&self, owner: &str, name: &str) -> Result<Vec<Release>> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        // 20 pages × 50 = 1000 releases is plenty; the cap guards against loops.
        for _ in 0..20 {
            let v = self.graphql(
                RELEASES_QUERY,
                json!({ "owner": owner, "name": name, "after": after }),
            )?;
            let conn = &v["repository"]["releases"];
            out.extend(parse_releases(conn));
            if conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                break;
            }
            after = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
        }
        Ok(out)
    }

    /// Every contributor, keyed like [`CommitAuthor`]: login, else (for
    /// commits no account claims) email. GitHub caches this list, so it can
    /// miss the newest authors; [`recent_activity`] adds them back.
    fn contributors(&self, repo: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        // 20 pages × 100 people; the cap guards against loops.
        for page in 1..=20 {
            let url = format!(
                "{}/repos/{repo}/contributors?per_page=100&anon=1&page={page}",
                self.api
            );
            // 204: empty repository.
            let Some(body) = self.get_json(&url)? else {
                break;
            };
            let people = parse_contributors(&body);
            let done = people.len() < 100;
            out.extend(people);
            if done {
                break;
            }
        }
        Ok(out)
    }
}

/// The repo's ledger, bootstrapped from this app's cache if it has none yet.
fn load_ledger(shared: &SharedCache, repo: &str) -> PrLedger {
    let cache = craft_core::Cache::default_location().ok();
    shared.load_or_seed_ledger(repo, cache.as_ref().map(|c| c.dir()))
}

fn note_rate_limit(data: &Value, out: &mut LedgerSync) {
    let r = &data["rateLimit"];
    out.cost += r["cost"].as_u64().unwrap_or(0);
    if let Some(left) = r["remaining"].as_u64() {
        out.remaining = Some(left);
        out.reset_at = time(&r["resetAt"]);
    }
}

/// Saves the points left that an answer's `rateLimit` reports to `shared`'s
/// budget, which the PR poller reads. Call holding the lease. An answer
/// without `resetAt` keeps the reset time already saved while it is ahead.
fn note_shared_budget(shared: &SharedCache, rate: &Value) {
    let Some(left) = rate["remaining"].as_u64() else {
        return;
    };
    let now = Utc::now();
    let kept = shared.budget().reset_at.filter(|t| *t > now);
    let reset_at = time(&rate["resetAt"]).or(kept);
    if let Err(e) = shared.note_budget(Some(left), reset_at, now) {
        eprintln!("could not save the shared budget: {e}");
    }
}

/// One page of PRs, newest update first: those updated at or after `stop_at`,
/// whether the page reached older ones, and the oldest update it holds.
pub fn parse_ledger_page(
    conn: &Value,
    stop_at: DateTime<Utc>,
) -> (Vec<(u64, LedgerPr)>, bool, Option<DateTime<Utc>>) {
    let mut out = Vec::new();
    let mut oldest = None;
    for n in conn["nodes"].as_array().into_iter().flatten() {
        let updated_at = time(&n["updatedAt"]);
        if updated_at.is_some_and(|t| t < stop_at) {
            return (out, true, oldest);
        }
        oldest = updated_at.or(oldest);
        if let Some(number) = n["number"].as_u64() {
            out.push((
                number,
                LedgerPr {
                    state: n["state"].as_str().unwrap_or_default().to_string(),
                    created_at: time(&n["createdAt"]),
                    updated_at,
                    merged_at: time(&n["mergedAt"]),
                    author: n["author"]["login"].as_str().map(str::to_string),
                },
            ));
        }
    }
    (out, false, oldest)
}

pub fn parse_contributors(body: &Value) -> Vec<String> {
    body.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    c["login"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| c["email"].as_str().map(str::to_lowercase))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One commit's date and its author, keyed by GitHub login, else email, else
/// `~` and the name.
pub type CommitAuthor = (DateTime<Utc>, String);

/// `(oid, (committed, author))` per commit node.
pub fn parse_authors(conn: &Value) -> Vec<(String, CommitAuthor)> {
    conn["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|c| {
                    let a = &c["author"];
                    let who = a["user"]["login"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| {
                            a["email"]
                                .as_str()
                                .filter(|e| !e.is_empty())
                                .map(str::to_lowercase)
                        })
                        // A name is marked with `~`, so it can never read as a login.
                        .or_else(|| a["name"].as_str().map(|n| format!("~{n}")))?;
                    let at = time(&c["committedDate"])?;
                    let oid = c["oid"]
                        .as_str()
                        .map_or_else(|| format!("{at} {who}"), str::to_string);
                    Some((oid, (at, who)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()?.parse().ok()
}

/// The `errors` list of a GraphQL answer; empty when there is none.
fn graphql_errors(body: &Value) -> &[Value] {
    body["errors"].as_array().map_or(&[], Vec::as_slice)
}

fn error_messages(errs: &[Value]) -> String {
    let msg = errs
        .iter()
        .filter_map(|e| e["message"].as_str())
        .collect::<Vec<_>>()
        .join("; ");
    if msg.is_empty() {
        "no data in the answer".into()
    } else {
        msg
    }
}

fn count(v: &Value) -> u64 {
    v["totalCount"].as_u64().unwrap_or(0)
}

fn first_time(conn: &Value, field: &str) -> Option<DateTime<Utc>> {
    time(&conn["nodes"][0][field])
}

pub fn parse_releases(conn: &Value) -> Vec<Release> {
    conn["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .map(|r| Release {
                    tag: r["tagName"].as_str().unwrap_or_default().to_string(),
                    draft: r["isDraft"].as_bool().unwrap_or(false),
                    prerelease: r["isPrerelease"].as_bool().unwrap_or(false),
                    published_at: time(&r["publishedAt"]),
                    created_at: time(&r["createdAt"]),
                    commit_sha: r["tagCommit"]["oid"].as_str().map(str::to_string),
                    commit_at: time(&r["tagCommit"]["committedDate"]),
                    assets: r["releaseAssets"]["nodes"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|a| Asset {
                                    name: a["name"].as_str().unwrap_or_default().to_string(),
                                    download_count: a["downloadCount"].as_u64().unwrap_or(0),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Reads [`BUILD_QUERY`]. Prefers the ref comparison, which counts exactly;
/// when the build's tag shares no history with `main` (GitHub returns no
/// comparison), falls back to counting `main` commits newer than the build's
/// commit, from the newest `main` commit at or before it.
pub fn parse_since_build(v: &Value, build_sha: &str) -> Option<BuildLag> {
    let r = &v["repository"];
    let cmp = &r["ref"]["compare"];
    if let (Some(status), Some(ahead)) = (cmp["status"].as_str(), cmp["aheadBy"].as_u64()) {
        let exact = matches!(status, "AHEAD" | "IDENTICAL");
        return Some(BuildLag {
            commits: ahead,
            basis: if exact {
                LagBasis::Exact
            } else {
                LagBasis::Branched
            },
            from_sha: exact.then(|| build_sha.to_string()),
        });
    }
    let head = &r["defaultBranchRef"]["target"];
    if head.is_null() {
        return None;
    }
    Some(BuildLag {
        commits: count(&head["since"]),
        basis: LagBasis::ByDate,
        from_sha: head["before"]["nodes"][0]["oid"]
            .as_str()
            .map(str::to_string),
    })
}

fn parse_issues(conn: &Value) -> Vec<IssueInput> {
    conn["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .map(|i| IssueInput {
                    number: i["number"].as_u64().unwrap_or(0),
                    title: i["title"].as_str().unwrap_or_default().to_string(),
                    url: i["url"].as_str().unwrap_or_default().to_string(),
                    created_at: time(&i["createdAt"]),
                    updated_at: time(&i["updatedAt"]),
                    comments: count(&i["comments"]),
                    reactions: count(&i["reactions"]),
                    labels: i["labels"]["nodes"]
                        .as_array()
                        .map(|l| {
                            l.iter()
                                .filter_map(|l| l["name"].as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    author_association: i["authorAssociation"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// When PRs were opened and merged and issues opened, since
/// [`activity_start`]; [`recent_activity`] buckets them into windows. The PRs
/// come from the shared [`PrLedger`], the issues from [`RawActivity`].
#[derive(Debug, Clone, Default)]
pub struct Events {
    pub prs_opened: Vec<DateTime<Utc>>,
    pub prs_merged: Vec<DateTime<Utc>>,
    pub issues_opened: Vec<DateTime<Utc>>,
}

/// Activity kept between polls, in the cache directory, so each poll asks
/// GitHub only for what's new. Keyed by commit oid and issue number, so
/// overlapping fetches merge instead of double counting. PRs aren't here: they
/// are in the [`PrLedger`] shared with the PR poller.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RawActivity {
    /// Everything from here to `fetched_at` is held.
    pub covers_from: Option<DateTime<Utc>>,
    /// When the last fetch started.
    pub fetched_at: Option<DateTime<Utc>>,
    /// When the last fetch from scratch started.
    pub full_at: Option<DateTime<Utc>>,
    pub commits: BTreeMap<String, CommitAuthor>,
    pub issues_opened: BTreeMap<u64, DateTime<Utc>>,
    /// When the most recently updated pull request (any state) was updated,
    /// as the last fetch read it; `None` before a fetch and for a repository
    /// without pull requests.
    pub pr_updated_at: Option<DateTime<Utc>>,
}

impl RawActivity {
    fn path(dir: &Path, repo: &str) -> PathBuf {
        dir.join("raw")
            .join(format!("{}.json", repo.replace('/', "@")))
    }

    /// The activity kept for `repo` under `dir`; empty if missing or corrupt.
    pub fn load(dir: &Path, repo: &str) -> RawActivity {
        std::fs::read_to_string(Self::path(dir, repo))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// What the last fetch of `repo` read of its pull requests, from the
    /// activity kept under `dir` ([`prs::Seen`]). `None` before a fetch,
    /// and when the kept activity has no `pr_updated_at` field because an
    /// earlier version wrote it. A repository without pull requests gives
    /// `pr_updated_at: None`.
    pub fn seen(dir: &Path, repo: &str) -> Option<prs::Seen> {
        let text = std::fs::read_to_string(Self::path(dir, repo)).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        value.get("pr_updated_at")?;
        let raw: RawActivity = serde_json::from_value(value).ok()?;
        Some(prs::Seen {
            pr_updated_at: raw.pr_updated_at,
            read_at: raw.fetched_at?,
        })
    }

    pub fn save(&self, dir: &Path, repo: &str) -> craft_core::Result<()> {
        craft_core::atomic_write(&Self::path(dir, repo), &serde_json::to_string(self)?)
    }

    /// Where the next fetch for activity since `since` starts, and whether it
    /// is from scratch: when nothing reaches back to `since`, or the last
    /// fetch from scratch is [`FULL_REFRESH_HOURS`] old.
    pub fn fetch_from(&self, since: DateTime<Utc>, now: DateTime<Utc>) -> (DateTime<Utc>, bool) {
        let warm = self.covers_from.is_some_and(|c| c <= since)
            && self
                .full_at
                .is_some_and(|f| now - f < chrono::Duration::hours(FULL_REFRESH_HOURS));
        match self.fetched_at {
            Some(last) if warm => (
                (last - chrono::Duration::minutes(OVERLAP_MINUTES)).max(since),
                false,
            ),
            _ => (since, true),
        }
    }

    /// Drop everything before `since`.
    pub fn prune(&mut self, since: DateTime<Utc>) {
        self.commits.retain(|_, (at, _)| *at >= since);
        self.issues_opened.retain(|_, at| *at >= since);
    }
}

/// The start of the longest window, or of the latest build if that's earlier.
fn activity_start(now: DateTime<Utc>, release_at: Option<DateTime<Utc>>) -> DateTime<Utc> {
    let longest = WINDOWS.iter().map(|(_, m)| *m).max().unwrap_or(0);
    (now - chrono::Duration::minutes(longest)).min(release_at.unwrap_or(now))
}

/// `(number, field time)` for the nodes of one page of `conn` whose `field`
/// is at or after `since`; the nodes are sorted newest first by `order`.
/// `true` once the page has reached nodes older than `since`, so later pages
/// can't hold any.
pub fn parse_events(
    conn: &Value,
    order: &str,
    field: &str,
    since: DateTime<Utc>,
) -> (Vec<(u64, DateTime<Utc>)>, bool) {
    let mut out = Vec::new();
    for n in conn["nodes"].as_array().into_iter().flatten() {
        if time(&n[order]).is_some_and(|t| t < since) {
            return (out, true);
        }
        if let (Some(num), Some(t)) = (n["number"].as_u64(), time(&n[field])) {
            if t >= since {
                out.push((num, t));
            }
        }
    }
    (out, false)
}

/// Reads the per-window commit counts [`main_query`] asked for and buckets
/// `events` by window. A repo with no
/// default branch still gets every window (with zero commits).
/// The all-time totals come along as the [`ALL_TIME`] window; its people are
/// GitHub's contributors plus any newer `authors`, the others' the distinct
/// `authors` inside them. With a build published at `release_at`, the
/// [`SINCE_RELEASE`] window starts then; its commits are `unreleased` (the
/// Unreleased column's count) when that is known.
fn recent_activity(
    main: &Value,
    contributors: &[String],
    authors: &[CommitAuthor],
    events: &Events,
    release_at: Option<DateTime<Utc>>,
    unreleased: Option<u64>,
    now: DateTime<Utc>,
) -> std::collections::BTreeMap<String, Activity> {
    let r = &main["repository"];
    if r.is_null() {
        return Default::default();
    }
    let head = &r["defaultBranchRef"]["target"];
    let all = Activity {
        commits: count(&head["all"]),
        prs_opened: count(&r["allPrs"]),
        prs_merged: count(&r["mergedPrs"]),
        issues_opened: count(&r["allIssues"]),
        people: contributors
            .iter()
            .chain(authors.iter().map(|(_, who)| who))
            .collect::<std::collections::HashSet<_>>()
            .len() as u64,
    };
    let after =
        |times: &[DateTime<Utc>], since| times.iter().filter(|t| **t >= since).count() as u64;
    let window = |key: &str, since: DateTime<Utc>| Activity {
        commits: count(&head[format!("c{key}")]),
        prs_opened: after(&events.prs_opened, since),
        prs_merged: after(&events.prs_merged, since),
        issues_opened: after(&events.issues_opened, since),
        people: authors
            .iter()
            .filter(|(at, _)| *at >= since)
            .map(|(_, who)| who)
            .collect::<std::collections::HashSet<_>>()
            .len() as u64,
    };
    let mut out: std::collections::BTreeMap<_, _> = windows(now, release_at)
        .map(|(id, key, since)| (id.to_string(), window(&key, since)))
        .collect();
    if let (Some(a), Some(n)) = (out.get_mut(SINCE_RELEASE), unreleased) {
        a.commits = n;
    }
    out.insert(ALL_TIME.to_string(), all);
    out
}

/// `(id, alias suffix, start)` for each window in [`WINDOWS`] (suffixed by
/// index), then [`SINCE_RELEASE`] (suffixed `r`) when there is a build
/// published at `release_at`.
fn windows(
    now: DateTime<Utc>,
    release_at: Option<DateTime<Utc>>,
) -> impl Iterator<Item = (&'static str, String, DateTime<Utc>)> {
    WINDOWS
        .iter()
        .enumerate()
        .map(move |(i, (id, minutes))| {
            (
                *id,
                i.to_string(),
                now - chrono::Duration::minutes(*minutes),
            )
        })
        .chain(release_at.map(|at| (SINCE_RELEASE, "r".to_string(), at)))
}

/// [`MAIN_QUERY`] with one commit-history count per window, aliased by
/// window index (`c0`, `c1`…), plus the [`SINCE_RELEASE`] window (`cr`) when
/// there is a build published at `release_at`.
pub fn main_query(now: DateTime<Utc>, release_at: Option<DateTime<Utc>>) -> String {
    let mut history = String::new();
    for (_, i, since) in windows(now, release_at) {
        let since = since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        history += &format!("c{i}: history(since: \"{since}\") {{ totalCount }}\n");
    }
    MAIN_QUERY.replace("{HISTORY}", &history)
}

/// Pure: turn the GraphQL payloads into the model (unit-tested below).
pub fn build_stats(
    main: &Value,
    releases: &[Release],
    since_build: Option<BuildLag>,
    contributors: &[String],
    authors: &[CommitAuthor],
    events: &Events,
    now: DateTime<Utc>,
) -> RepoStats {
    let r = &main["repository"];
    let head = &r["defaultBranchRef"]["target"];
    let rel = summarize_releases(releases);
    let mut issues = parse_issues(&r["recentOpen"]);
    issues.extend(parse_issues(&r["hotOpen"]));
    let (urgent, urgency, critical) =
        rank_issues(&issues, now, rel.latest_tag.as_deref(), KEEP_URGENT);
    RepoStats {
        open_prs: count(&r["openPrs"]),
        open_issues: count(&r["openIssues"]),
        last_commit_at: first_time(&head["all"], "committedDate"),
        last_issue_at: first_time(&r["newestIssue"], "createdAt"),
        oldest_open_pr_at: first_time(&r["oldestOpenPr"], "createdAt"),
        newest_pr_at: first_time(&r["newestPr"], "createdAt"),
        recent: recent_activity(
            main,
            contributors,
            authors,
            events,
            rel.latest_at,
            since_build.as_ref().map(|l| l.commits),
            now,
        ),
        latest_build: rel.latest_tag,
        latest_build_at: rel.latest_at,
        latest_build_prerelease: rel.latest_prerelease,
        latest_build_sha: rel.latest_sha,
        since_build,
        latest_build_downloads: rel.latest_downloads,
        downloads_total: rel.total_downloads,
        urgency,
        critical_issues: critical,
        urgent,
        icon_oid: r["icon"]["entries"].as_array().and_then(|es| {
            es.iter()
                .find(|e| e["name"].as_str().is_some_and(|n| n.ends_with(".png")))
                .and_then(|e| e["oid"].as_str().map(str::to_string))
        }),
    }
}

const MAIN_QUERY: &str = r#"
query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    openPrs: pullRequests(states: OPEN) { totalCount }
    allPrs: pullRequests { totalCount }
    mergedPrs: pullRequests(states: MERGED) { totalCount }
    oldestOpenPr: pullRequests(states: OPEN, first: 1, orderBy: {field: CREATED_AT, direction: ASC}) { nodes { createdAt } }
    newestPr: pullRequests(first: 1, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { createdAt } }
    lastPr: pullRequests(first: 1, orderBy: {field: UPDATED_AT, direction: DESC}) { nodes { updatedAt } }
    openIssues: issues(states: OPEN) { totalCount }
    allIssues: issues { totalCount }
    newestIssue: issues(first: 1, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { createdAt } }
    icon: object(expression: "HEAD:assets/app-icon/hicolor/64x64/apps") { ... on Tree { entries { name oid } } }
    recentOpen: issues(states: OPEN, first: 100, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { ...I } }
    hotOpen: issues(states: OPEN, first: 30, orderBy: {field: COMMENTS, direction: DESC}) { nodes { ...I } }
    defaultBranchRef {
      name
      target {
        ... on Commit {
          all: history(first: 1) { totalCount nodes { committedDate } }
          {HISTORY}
        }
      }
    }
  }
  rateLimit { remaining resetAt }
}
fragment I on Issue {
  number title url createdAt updatedAt authorAssociation
  comments { totalCount }
  reactions { totalCount }
  labels(first: 10) { nodes { name } }
}
"#;

const RELEASES_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    releases(first: 50, after: $after, orderBy: {field: CREATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        tagName isDraft isPrerelease publishedAt createdAt
        tagCommit { oid committedDate }
        releaseAssets(first: 100) { nodes { name downloadCount } }
      }
    }
  }
}
"#;

const AUTHORS_QUERY: &str = r#"
query($owner: String!, $name: String!, $since: GitTimestamp!, $after: String) {
  repository(owner: $owner, name: $name) {
    defaultBranchRef {
      target {
        ... on Commit {
          history(since: $since, first: 100, after: $after) {
            pageInfo { hasNextPage endCursor }
            nodes { oid committedDate author { email name user { login } } }
          }
        }
      }
    }
  }
}
"#;

const ISSUES_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    issues(first: 100, after: $after, orderBy: {field: CREATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes { number createdAt }
    }
  }
}
"#;

/// Every PR, in any state, most recently updated first.
const LEDGER_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(first: 100, after: $after, orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes { number state createdAt updatedAt mergedAt author { login } }
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

const BACKFILL_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: MERGED, first: 100, after: $after, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes { number state createdAt updatedAt mergedAt author { login } }
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

/// The latest build's tag compared with `main`, plus a by-date count for
/// when the two share no history (see [`parse_since_build`]).
const BUILD_QUERY: &str = r#"
query($owner: String!, $name: String!, $tag: String!, $branch: String!, $at: GitTimestamp!) {
  repository(owner: $owner, name: $name) {
    ref(qualifiedName: $tag) { compare(headRef: $branch) { status aheadBy } }
    defaultBranchRef {
      target {
        ... on Commit {
          since: history(since: $at) { totalCount }
          before: history(until: $at, first: 1) { nodes { oid } }
        }
      }
    }
  }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_prefers_env_then_config() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |var: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == var)
                    .map(|(_, v)| v.to_string())
            }
        };
        let both = env(&[
            ("GITHUB_TOKEN", "generic"),
            ("CRAFT_STATUS_GITHUB_TOKEN", " mine "),
        ]);
        let got = |t: Option<Token>| t.map(|t| (t.value, t.source));
        assert_eq!(
            got(env_or_config_token("cfg", both)),
            Some(("mine".into(), "$CRAFT_STATUS_GITHUB_TOKEN"))
        );
        let generic = env(&[("GH_TOKEN", "gh"), ("GITHUB_TOKEN", "generic")]);
        assert_eq!(
            got(env_or_config_token("cfg", generic)),
            Some(("generic".into(), "$GITHUB_TOKEN"))
        );
        let blank = env(&[("CRAFT_STATUS_GITHUB_TOKEN", "  ")]);
        assert_eq!(
            got(env_or_config_token(" cfg\n", blank)),
            Some(("cfg".into(), "config"))
        );
        assert!(env_or_config_token("", env(&[])).is_none());
    }

    #[test]
    fn builds_stats_from_payloads() {
        let main = json!({
          "repository": {
            "openPrs": {"totalCount": 3}, "allPrs": {"totalCount": 40}, "mergedPrs": {"totalCount": 33},
            "icon": {"entries": [{"name": "README", "oid": "r1"}, {"name": "ai.storyteller.x.png", "oid": "i1"}]},
            "oldestOpenPr": {"nodes": [{"createdAt": "2026-09-01T00:00:00Z"}]},
            "newestPr": {"nodes": [{"createdAt": "2026-10-08T01:00:00Z"}]},
            "openIssues": {"totalCount": 7}, "allIssues": {"totalCount": 90},
            "newestIssue": {"nodes": [{"createdAt": "2026-10-08T02:00:00Z"}]},
            "recentOpen": {"nodes": [
              {"number": 5, "title": "App crashes on launch", "url": "u5", "createdAt": "2026-10-08T02:00:00Z",
               "updatedAt": "2026-10-08T02:00:00Z", "authorAssociation": "NONE",
               "comments": {"totalCount": 2}, "reactions": {"totalCount": 1}, "labels": {"nodes": [{"name": "bug"}]}}
            ]},
            "hotOpen": {"nodes": []},
            "defaultBranchRef": {"target": {
              "all": {"totalCount": 1234, "nodes": [{"committedDate": "2026-10-08T03:00:00Z"}]},
              "c0": {"totalCount": 1}, "c3": {"totalCount": 9}, "cr": {"totalCount": 99}}}
          }
        });
        let at = |t: &str, n| vec![t.parse::<DateTime<Utc>>().unwrap(); n];
        // Inside the last 4 h; inside the last week; before that but after v1.0.0.
        let events = Events {
            prs_opened: [at("2026-10-08T01:00:00Z", 2), at("2026-10-02T00:00:00Z", 9)].concat(),
            prs_merged: [
                at("2026-10-08T01:00:00Z", 3),
                at("2026-10-02T00:00:00Z", 27),
                at("2026-10-01T02:00:00Z", 1),
            ]
            .concat(),
            issues_opened: [at("2026-10-08T01:00:00Z", 4), at("2026-10-01T02:00:00Z", 9)].concat(),
        };
        let rel = parse_releases(&json!({"nodes": [
          {"tagName": "v1.1.0", "isDraft": true, "isPrerelease": false, "publishedAt": null, "createdAt": "2026-10-08T00:00:00Z",
           "releaseAssets": {"nodes": [{"name": "a.dmg", "downloadCount": 0}]}},
          {"tagName": "v1.0.0", "isDraft": false, "isPrerelease": false, "publishedAt": "2026-10-01T00:00:00Z", "createdAt": "2026-10-01T00:00:00Z",
           "releaseAssets": {"nodes": [{"name": "a.dmg", "downloadCount": 10}, {"name": "SHA256SUMS.txt", "downloadCount": 4}]}}
        ]}));
        let now: DateTime<Utc> = "2026-10-08T04:00:00Z".parse().unwrap();
        let lag = BuildLag {
            commits: 5,
            basis: LagBasis::Exact,
            from_sha: Some("abc".into()),
        };
        let authors = parse_authors(&json!({"nodes": [
          {"committedDate": "2026-10-08T03:55:00Z", "author": {"email": "A@x", "name": "A", "user": {"login": "ann"}}},
          {"committedDate": "2026-10-08T03:00:00Z", "author": {"email": "a@x", "name": "A", "user": {"login": "ann"}}},
          {"committedDate": "2026-10-08T02:00:00Z", "author": {"email": "Bo@Y", "name": "Bo", "user": null}},
          {"committedDate": "2026-10-03T00:00:00Z", "author": {"email": "", "name": "Cy", "user": null}}
        ]}));
        let authors: Vec<CommitAuthor> = authors.into_iter().map(|(_, a)| a).collect();
        assert_eq!(authors[2].1, "bo@y");
        let mut people =
            parse_contributors(&json!([{"login": "ann"}, {"email": "Old@Z", "type": "Anonymous"}]));
        people.extend((0..10).map(|i| format!("p{i}")));
        assert_eq!(people[1], "old@z");
        let s = build_stats(
            &main,
            &rel,
            Some(lag.clone()),
            &people,
            &authors,
            &events,
            now,
        );
        assert_eq!((s.open_prs, s.open_issues), (3, 7));
        assert_eq!(s.recent.len(), WINDOWS.len() + 2);
        let a = |id: &str| s.recent[id];
        assert_eq!(
            a("4h"),
            Activity {
                commits: 9,
                prs_opened: 2,
                prs_merged: 3,
                issues_opened: 4,
                people: 2,
            }
        );
        assert_eq!((a("10m").commits, a("7d").prs_merged), (1, 30));
        assert_eq!((a("10m").people, a("1h").people, a("7d").people), (1, 1, 3));
        assert_eq!(
            a(ALL_TIME),
            Activity {
                commits: 1234,
                prs_opened: 40,
                prs_merged: 33,
                issues_opened: 90,
                // 12 contributors, plus Bo and Cy, too new for GitHub's list.
                people: 14,
            }
        );
        assert_eq!(
            a(SINCE_RELEASE),
            Activity {
                // The Unreleased count, not commits dated after publishing.
                commits: 5,
                prs_opened: 11,
                prs_merged: 31,
                issues_opened: 13,
                // Cy committed on the 3rd, after v1.0.0 but before the last week.
                people: 3,
            }
        );
        let no_lag = build_stats(&main, &rel, None, &people, &authors, &events, now);
        assert_eq!(no_lag.recent[SINCE_RELEASE].commits, 99);
        assert_eq!(s.icon_oid.as_deref(), Some("i1"));
        assert_eq!(s.latest_build.as_deref(), Some("v1.0.0"));
        assert_eq!((s.latest_build_downloads, s.downloads_total), (10, 10));
        assert_eq!(s.since_build, Some(lag));
        assert_eq!(
            s.last_commit_at,
            Some("2026-10-08T03:00:00Z".parse().unwrap())
        );
        assert_eq!(s.critical_issues, 1);
        assert_eq!(s.urgent[0].number, 5);
        assert!(s.urgency > 0.0);
    }

    #[test]
    fn since_build_prefers_the_comparison() {
        let by_date = json!({"defaultBranchRef": {"target": {
            "since": {"totalCount": 7}, "before": {"nodes": [{"oid": "m1"}]}}}});
        let with = |cmp: Value| {
            let mut r = by_date.clone();
            r["ref"] = json!({ "compare": cmp });
            parse_since_build(&json!({ "repository": r }), "b1")
        };
        let lag = |commits, basis, from: Option<&str>| {
            Some(BuildLag {
                commits,
                basis,
                from_sha: from.map(str::to_string),
            })
        };
        assert_eq!(
            with(json!({"status": "AHEAD", "aheadBy": 12})),
            lag(12, LagBasis::Exact, Some("b1"))
        );
        assert_eq!(
            with(json!({"status": "IDENTICAL", "aheadBy": 0})),
            lag(0, LagBasis::Exact, Some("b1"))
        );
        assert_eq!(
            with(json!({"status": "DIVERGED", "aheadBy": 4})),
            lag(4, LagBasis::Branched, None)
        );
        // No common history: GitHub gives no comparison.
        assert_eq!(with(Value::Null), lag(7, LagBasis::ByDate, Some("m1")));
        let empty = json!({"repository": {"ref": null, "defaultBranchRef": null}});
        assert_eq!(parse_since_build(&empty, "b1"), None);
    }

    #[test]
    fn query_asks_for_every_window() {
        let now: DateTime<Utc> = "2026-10-08T04:00:00Z".parse().unwrap();
        let q = main_query(now, None);
        assert!(q.contains(r#"c3: history(since: "2026-10-08T00:00:00Z")"#));
        assert!(q.contains(r#"c6: history(since: "2026-10-01T04:00:00Z")"#));
        assert!(!q.contains("{HISTORY}") && !q.contains("search("));
        assert!(!q.contains("cr:"), "no build, no since-release window");
        let q = main_query(now, Some("2026-09-20T12:00:00Z".parse().unwrap()));
        assert!(q.contains(r#"cr: history(since: "2026-09-20T12:00:00Z")"#));
    }

    #[test]
    fn events_stop_at_the_window_start() {
        let since: DateTime<Utc> = "2026-10-01T00:00:00Z".parse().unwrap();
        let t = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        let merged = json!({"nodes": [
          {"number": 9, "updatedAt": "2026-10-08T00:00:00Z", "mergedAt": "2026-10-07T00:00:00Z"},
          // Updated lately but merged long ago: skipped, and the walk goes on.
          {"number": 8, "updatedAt": "2026-10-06T00:00:00Z", "mergedAt": "2026-09-01T00:00:00Z"},
          {"number": 7, "updatedAt": "2026-10-02T00:00:00Z", "mergedAt": "2026-10-01T00:00:00Z"},
          {"number": 6, "updatedAt": "2026-09-30T00:00:00Z", "mergedAt": "2026-09-30T00:00:00Z"},
          {"number": 5, "updatedAt": "2026-10-05T00:00:00Z", "mergedAt": "2026-10-05T00:00:00Z"}
        ]});
        assert_eq!(
            parse_events(&merged, "updatedAt", "mergedAt", since),
            (
                vec![
                    (9, t("2026-10-07T00:00:00Z")),
                    (7, t("2026-10-01T00:00:00Z"))
                ],
                true
            )
        );
        let opened = json!({"nodes": [{"number": 3, "createdAt": "2026-10-03T00:00:00Z"}]});
        assert_eq!(
            parse_events(&opened, "createdAt", "createdAt", since),
            (vec![(3, t("2026-10-03T00:00:00Z"))], false)
        );
        assert_eq!(
            parse_events(&Value::Null, "createdAt", "createdAt", since),
            (vec![], false)
        );
    }

    #[test]
    fn raw_activity_fetches_only_whats_new() {
        let t = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        let week_ago = t("2026-10-01T04:00:00Z");
        let now = t("2026-10-08T04:00:00Z");
        let cold = RawActivity::default();
        assert_eq!(cold.fetch_from(week_ago, now), (week_ago, true));
        let mut raw = RawActivity {
            covers_from: Some(week_ago),
            fetched_at: Some(t("2026-10-08T03:55:00Z")),
            full_at: Some(t("2026-10-08T01:00:00Z")),
            ..Default::default()
        };
        // Warm: from just before the previous fetch.
        assert_eq!(
            raw.fetch_from(week_ago, now),
            (t("2026-10-08T03:45:00Z"), false)
        );
        // A window reaching further back than what's held starts over.
        assert!(raw.fetch_from(t("2026-09-20T00:00:00Z"), now).1);
        // So does an old full fetch.
        assert!(raw.fetch_from(week_ago, t("2026-10-08T08:00:00Z")).1);
        raw.issues_opened.insert(1, t("2026-09-30T00:00:00Z"));
        raw.issues_opened.insert(2, t("2026-10-02T00:00:00Z"));
        raw.issues_opened.insert(2, t("2026-10-02T00:00:00Z"));
        raw.commits
            .insert("a".into(), (t("2026-09-30T00:00:00Z"), "ann".into()));
        raw.prune(week_ago);
        assert_eq!(raw.issues_opened.len(), 1);
        assert!(raw.commits.is_empty());
        let dir = std::env::temp_dir().join(format!("craft-raw-{}", std::process::id()));
        raw.save(&dir, "o/r").unwrap();
        assert_eq!(RawActivity::load(&dir, "o/r"), raw);
        assert_eq!(RawActivity::load(&dir, "o/missing"), RawActivity::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ledger_pages_stop_at_older_updates() {
        let t = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        let conn = json!({"nodes": [
          {"number": 9, "state": "MERGED", "createdAt": "2026-09-01T00:00:00Z", "updatedAt": "2026-10-08T00:00:00Z",
           "mergedAt": "2026-10-08T00:00:00Z", "author": {"login": "ann"}},
          {"number": 8, "state": "OPEN", "createdAt": "2026-10-05T00:00:00Z", "updatedAt": "2026-10-06T00:00:00Z",
           "mergedAt": null, "author": null},
          {"number": 7, "state": "CLOSED", "createdAt": "2026-09-01T00:00:00Z", "updatedAt": "2026-09-30T00:00:00Z",
           "mergedAt": null, "author": {"login": "bo"}}
        ]});
        let (page, done, oldest) = parse_ledger_page(&conn, t("2026-10-01T00:00:00Z"));
        assert!(done);
        assert_eq!(page.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [9, 8]);
        assert_eq!(page[0].1.author.as_deref(), Some("ann"));
        assert_eq!(page[0].1.merged_at, Some(t("2026-10-08T00:00:00Z")));
        assert_eq!(page[1].1.author, None);
        assert_eq!(oldest, Some(t("2026-10-06T00:00:00Z")));
        let (page, done, _) = parse_ledger_page(&conn, DateTime::<Utc>::MIN_UTC);
        assert_eq!((page.len(), done), (3, false));
    }

    #[test]
    fn empty_repository_is_all_zero() {
        let mut s = build_stats(
            &json!({"repository": {"defaultBranchRef": null}}),
            &[],
            None,
            &[],
            &[],
            &Events::default(),
            Utc::now(),
        );
        assert_eq!(s.recent.len(), WINDOWS.len() + 1);
        assert!(s.recent.values().all(|a| *a == Activity::default()));
        s.recent.clear();
        assert_eq!(s, RepoStats::default());
    }

    #[test]
    fn a_commit_author_with_no_login_or_email_is_marked_as_a_name() {
        let c = parse_authors(&json!({"nodes": [
          {"oid": "n1", "committedDate": "2026-10-08T00:00:00Z", "author": {"email": "", "name": "Cy", "user": null}},
          {"oid": "n2", "committedDate": "2026-10-08T00:00:00Z", "author": {"email": "", "name": "Cy", "user": {"login": "cy-x"}}}
        ]}));
        let who: Vec<&str> = c.iter().map(|(_, (_, w))| w.as_str()).collect();
        assert_eq!(who, ["~Cy", "cy-x"]);
    }

    #[test]
    fn a_fetch_keeps_when_the_newest_pull_request_was_updated() {
        let api = routed_stub(|request| {
            let body = if request.contains("/contributors") {
                "[]".to_string()
            } else if request.contains("openprs:") {
                json!({"data": {"repository": {
                    "lastPr": {"nodes": [{"updatedAt": "2026-10-18T09:30:00Z"}]},
                    "defaultBranchRef": null
                }}})
                .to_string()
            } else {
                // Releases, commit authors and events: none.
                json!({"data": {"repository": {"defaultBranchRef": null}}}).to_string()
            };
            Some(ok_json(&body))
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let mut raw = RawActivity::default();
        let now = "2026-10-18T10:00:00Z".parse().unwrap();
        let shared_dir =
            std::env::temp_dir().join(format!("craft-shared-pr-{}", std::process::id()));
        let shared = SharedCache::at(shared_dir.clone());
        gh.fetch_repo("o/r", now, &mut raw, &shared).unwrap();
        std::fs::remove_dir_all(&shared_dir).ok();
        assert_eq!(
            raw.pr_updated_at,
            Some("2026-10-18T09:30:00Z".parse().unwrap())
        );
        assert!(main_query(now, None)
            .contains("lastPr: pullRequests(first: 1, orderBy: {field: UPDATED_AT"));
        // A file written before the field existed loads without it.
        let dir = std::env::temp_dir().join(format!("craft-raw-pr-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        std::fs::write(RawActivity::path(&dir, "o/old"), r#"{"commits":{}}"#).unwrap();
        assert_eq!(RawActivity::load(&dir, "o/old").pr_updated_at, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_kept_activity_gives_what_the_fetch_read_of_pull_requests() {
        let dir = std::env::temp_dir().join(format!("craft-raw-seen-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        let read: DateTime<Utc> = "2026-10-18T09:40:00Z".parse().unwrap();
        let at: DateTime<Utc> = "2026-10-18T09:30:00Z".parse().unwrap();
        let with_prs = RawActivity {
            fetched_at: Some(read),
            pr_updated_at: Some(at),
            ..Default::default()
        };
        with_prs.save(&dir, "o/prs").unwrap();
        let without_prs = RawActivity {
            fetched_at: Some(read),
            ..Default::default()
        };
        without_prs.save(&dir, "o/none").unwrap();
        std::fs::write(
            RawActivity::path(&dir, "o/old"),
            r#"{"fetched_at":"2026-10-18T09:40:00Z","commits":{}}"#,
        )
        .unwrap();
        assert_eq!(
            RawActivity::seen(&dir, "o/prs"),
            Some(prs::Seen {
                pr_updated_at: Some(at),
                read_at: read,
            })
        );
        assert_eq!(
            RawActivity::seen(&dir, "o/none"),
            Some(prs::Seen {
                pr_updated_at: None,
                read_at: read,
            }),
            "a repository without pull requests"
        );
        assert_eq!(
            RawActivity::seen(&dir, "o/old"),
            None,
            "written without the field"
        );
        assert_eq!(RawActivity::seen(&dir, "o/missing"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_rate_limit_on_a_background_request_stops_the_next_ones() {
        let limited = r#"{"message": "You have exceeded a secondary rate limit."}"#;
        let secondary = format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nretry-after: 60\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{limited}",
            limited.len()
        );
        let (api, requests) = stub(vec![secondary.clone(), secondary]);
        let gate = Arc::new(Gate::new());
        let background = GitHub::with_api("t", api.clone())
            .unwrap()
            .in_background(gate.clone());
        let vars = json!({});
        assert!(matches!(
            background.graphql_body("query { viewer { login } }", vars.clone()),
            Err(Error::RateLimited(_))
        ));
        assert!(matches!(
            background.graphql_body("query { viewer { login } }", vars.clone()),
            Err(Error::Halted(_))
        ));
        requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "the second request was not sent"
        );
        let until = gate.halted().expect("kept for the poll thread");
        assert!(
            DateTime::parse_from_rfc3339(&until).is_ok(),
            "when GitHub said to retry: {until}"
        );
        gate.pause_until(std::time::Instant::now(), "paused");
        // A poll client's rate limit is the poll thread's own business.
        let poll = GitHub::with_api("t", api).unwrap();
        assert!(matches!(
            poll.graphql_body("query { viewer { login } }", vars),
            Err(Error::RateLimited(_))
        ));
        assert_eq!(gate.halted(), None);
    }

    #[test]
    fn rest_requests_of_a_background_client_wait_at_its_gate() {
        let limited = r#"{"message": "API rate limit exceeded"}"#;
        let primary = format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nx-ratelimit-remaining: 0\r\n\
             x-ratelimit-reset: 2000000000\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{limited}",
            limited.len()
        );
        let (api, requests) = stub(vec![primary.clone(), primary]);
        let gate = Arc::new(Gate::new());
        let background = GitHub::with_api("t", api)
            .unwrap()
            .in_background(gate.clone());
        assert!(matches!(
            background.icon("o/r", "b1"),
            Err(Error::RateLimited(_))
        ));
        assert!(gate.halted().is_some(), "the rate limit stops the others");
        assert!(matches!(
            background.icon("o/r", "b1"),
            Err(Error::Halted(_))
        ));
        assert!(matches!(
            background.contributors("o/r"),
            Err(Error::Halted(_))
        ));
        requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "no request after the rate limit"
        );
    }

    #[test]
    fn a_poll_waits_through_the_pacing_after_a_background_request() {
        let (arrived_tx, arrived) = std::sync::mpsc::channel();
        let api = routed_stub(move |_| {
            arrived_tx.send(()).ok();
            std::thread::sleep(Duration::from_millis(300));
            Some(ok_json(r#"{"data":{}}"#))
        });
        let gate = Arc::new(Gate::new());
        let background = GitHub::with_api("t", api)
            .unwrap()
            .in_background(gate.clone());
        let people = std::thread::spawn(move || {
            background
                .graphql_body("query { viewer { login } }", json!({}))
                .unwrap();
        });
        arrived.recv_timeout(Duration::from_secs(5)).unwrap();
        let sent = std::time::Instant::now();
        let session = gate.poll_session();
        // 300 ms for the answer, then as long again for the pacing.
        assert!(
            sent.elapsed() >= Duration::from_millis(550),
            "the poll started {:?} after the request",
            sent.elapsed()
        );
        drop(session);
        people.join().unwrap();
    }

    #[test]
    fn a_background_request_is_not_sent_while_another_poller_holds_the_lease() {
        let dir = std::env::temp_dir().join(format!("craft-lease-bg-{}", std::process::id()));
        let shared = SharedCache::at(dir.clone());
        let (api, requests) = stub(vec![ok_json(
            r#"{"data":{"rateLimit":{"cost":1,"remaining":1234}}}"#,
        )]);
        let background = GitHub::with_api("t", api)
            .unwrap()
            .with_pacing(0.0)
            .in_background(Arc::new(Gate::new()))
            .with_lease(shared.clone(), Duration::ZERO);
        let other = shared.try_lease().expect("the lease is free");
        assert!(matches!(
            background.graphql_body("query { viewer { login } }", json!({})),
            Err(Error::Halted(_))
        ));
        assert!(
            requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "nothing was sent while the lease was held"
        );
        drop(other);
        background
            .graphql_body("query { viewer { login } }", json!({}))
            .unwrap();
        requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(shared.try_lease().is_some(), "the request let the lease go");
        assert_eq!(shared.budget().remaining, Some(1234), "for the PR poller");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_background_request_waits_for_the_lease_to_come_free() {
        let dir = std::env::temp_dir().join(format!("craft-lease-wait-{}", std::process::id()));
        let shared = SharedCache::at(dir.clone());
        let (api, requests) = stub(vec![ok_json(r#"{"data":{}}"#)]);
        let gate = Arc::new(Gate::new());
        let background = GitHub::with_api("t", api)
            .unwrap()
            .with_pacing(0.0)
            .in_background(gate.clone())
            .with_lease(shared.clone(), Duration::from_secs(5));
        let other = shared.try_lease().expect("the lease is free");
        let started = std::time::Instant::now();
        let people = std::thread::spawn(move || {
            background
                .graphql_body("query { viewer { login } }", json!({}))
                .unwrap();
        });
        std::thread::sleep(Duration::from_millis(100));
        let asked = std::time::Instant::now();
        let session = gate.poll_session();
        assert!(
            asked.elapsed() < Duration::from_millis(200),
            "a poll does not wait for a request waiting for the lease"
        );
        drop(session);
        std::thread::sleep(Duration::from_millis(200));
        drop(other);
        people.join().unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "sent after the lease came free"
        );
        requests.recv_timeout(Duration::from_secs(5)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_a_query_github_could_not_finish_asks_for_less() {
        assert!(Error::Http(502, "Bad Gateway".into()).is_overload());
        assert!(Error::GraphQl("Something went wrong: timeout".into()).is_overload());
        assert!(!Error::Http(404, "Not Found".into()).is_overload());
        assert!(!Error::RateLimited("later".into()).is_overload());
    }

    pub(crate) fn ok_json(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// A server on 127.0.0.1 that answers one connection with each of
    /// `responses`, in order, and sends back each request it read, head and
    /// body, in lowercase.
    pub(crate) fn stub(responses: Vec<String>) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut conn, _)) = listener.accept() else {
                    return;
                };
                let request = read_request(&mut conn);
                conn.write_all(response.as_bytes()).ok();
                let _ = tx.send(request);
            }
        });
        (url, rx)
    }

    /// A server on 127.0.0.1 that reads each connection on a thread of its
    /// own and answers with `answer` of the request, head and body, in
    /// lowercase. `None` keeps the connection open for 30 seconds without an
    /// answer.
    pub(crate) fn routed_stub(
        answer: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> String {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let answer = Arc::new(answer);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else {
                    return;
                };
                let answer = answer.clone();
                std::thread::spawn(move || {
                    let request = read_request(&mut conn);
                    match answer(&request) {
                        Some(response) => {
                            conn.write_all(response.as_bytes()).ok();
                        }
                        None => std::thread::sleep(Duration::from_secs(30)),
                    }
                });
            }
        });
        url
    }

    /// One request read from `conn`, head and body, in lowercase.
    fn read_request(conn: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        conn.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let mut head = Vec::new();
        let mut buf = [0; 1024];
        let end = loop {
            if let Some(i) = head.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            match conn.read(&mut buf) {
                Ok(0) | Err(_) => break head.len(),
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        };
        let text = String::from_utf8_lossy(&head[..end]).to_lowercase();
        // Read a POST body too, so closing the connection doesn't reset it.
        let length = text
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|n| n.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while head.len() < end + length {
            match conn.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        String::from_utf8_lossy(&head).to_lowercase()
    }
}
