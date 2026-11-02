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
//!
//! [`GitHub::probe`] asks about many repositories in one GraphQL query. When
//! a repo's [`Probe`] matches the one taken before its last fetch,
//! [`can_skip`] allows [`restat`] to measure that fetch again at a later time
//! without a request.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use chrono::{DateTime, SubsecRound, Utc};
use craft_core::{
    rank_issues, summarize_releases, Activity, Asset, BuildLag, IssueInput, LagBasis, LedgerPr,
    PrLedger, Release, RepoStats, SharedCache, ALL_TIME, SINCE_RELEASE, WINDOWS,
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
/// Repositories per [`probe_query`] request.
const PROBE_CHUNK: usize = 50;
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
    pacing: f64,
}

/// What a walk of the shared ledger left behind and what it cost.
#[derive(Debug, Clone, Default)]
pub struct LedgerSync {
    pub ledger: PrLedger,
    pub cost: u64,
    pub remaining: Option<u64>,
    pub reset_at: Option<DateTime<Utc>>,
    /// The walk updated `ledger` but could not save it, so the shared
    /// ledger on disk is older than `ledger`.
    pub unsaved: bool,
}

/// Rate-limit bookkeeping returned alongside each repo.
#[derive(Debug, Clone, Copy, Default)]
pub struct Budget {
    pub graphql_remaining: Option<u64>,
}

impl GitHub {
    pub fn new(token: String) -> Result<GitHub> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(GitHub {
            http,
            token,
            pacing: GRAPHQL_PACING,
        })
    }

    /// Wait `pacing` times as long as each GraphQL request took, after it.
    pub fn with_pacing(mut self, pacing: f64) -> GitHub {
        self.pacing = pacing;
        self
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

    fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let started = std::time::Instant::now();
        let sent = self
            .http
            .post(format!("{API}/graphql"))
            .headers(self.headers())
            .json(&json!({ "query": query, "variables": variables }))
            .send();
        std::thread::sleep(started.elapsed().mul_f64(self.pacing));
        let v: Value = Self::check(sent?)?.json()?;
        if let Some(errs) = v["errors"].as_array().filter(|e| !e.is_empty()) {
            let msg = errs
                .iter()
                .filter_map(|e| e["message"].as_str())
                .collect::<Vec<_>>()
                .join("; ");
            if errs.iter().any(|e| e["type"] == "RATE_LIMITED") {
                return Err(Error::RateLimited("the top of the hour".into()));
            }
            if v["data"]["repository"].is_null() {
                if errs.iter().any(|e| e["type"] == "NOT_FOUND") {
                    return Err(Error::NotFound);
                }
                return Err(Error::GraphQl(msg));
            }
        }
        Ok(v["data"].clone())
    }

    /// A [`Probe`] of each of `repos`, in order, from one request per
    /// [`PROBE_CHUNK`] repos. An error in any answer fails the whole probe,
    /// as does a repository GitHub can't find or show. Each answer's rate-limit
    /// reading goes to `shared`'s budget for the PR poller; pass `shared` only
    /// while holding the shared lease.
    pub fn probe(
        &self,
        repos: &[String],
        shared: Option<&SharedCache>,
    ) -> Result<(Vec<Option<Probe>>, Budget)> {
        let mut out = Vec::with_capacity(repos.len());
        let mut budget = Budget::default();
        for chunk in repos.chunks(PROBE_CHUNK) {
            let mut vars = serde_json::Map::new();
            for (i, repo) in chunk.iter().enumerate() {
                let (owner, name) = repo.split_once('/').unwrap_or((repo.as_str(), ""));
                vars.insert(format!("o{i}"), json!(owner));
                vars.insert(format!("n{i}"), json!(name));
            }
            let data = self.graphql(&probe_query(chunk.len()), Value::Object(vars))?;
            out.extend(parse_probe(&data, chunk.len()));
            let left = data["rateLimit"]["remaining"].as_u64();
            if let Some(left) = left {
                budget.graphql_remaining =
                    Some(budget.graphql_remaining.map_or(left, |r| r.min(left)));
            }
            let reset_at = time(&data["rateLimit"]["resetAt"]);
            if let Some(Err(e)) = shared.map(|s| s.note_budget(left, reset_at, Utc::now())) {
                eprintln!("could not save the shared budget: {e}");
            }
        }
        Ok((out, budget))
    }

    /// Everything the UI shows for `owner/name`, measured at `now`. `raw` is
    /// the activity kept from the previous fetch; it's brought up to date
    /// only when the whole fetch succeeds, and then also holds this fetch
    /// ([`RawActivity::last`]) with `probe`, the [`Probe`] taken before it.
    pub fn fetch_repo(
        &self,
        repo: &str,
        now: DateTime<Utc>,
        raw: &mut RawActivity,
        shared: &SharedCache,
        probe: Option<Probe>,
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
        let sync = self.sync_ledger(repo, since, now, shared)?;
        // A probe kept with this fetch lets `restat` read the shared ledger
        // later, which is older than this fetch's when the save failed.
        let probe = probe.filter(|_| !sync.unsaved);
        let ledger = sync.ledger;
        let events = Events {
            prs_opened: ledger.opened_since(since),
            prs_merged: ledger.merged_since(since),
            issues_opened: fresh.issues_opened.values().copied().collect(),
        };
        let contributors = self.contributors(repo)?;
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
            since_build.clone(),
            &contributors,
            &authors,
            &events,
            now,
        );
        fresh.last = Some(LastFetch {
            at: now,
            probe,
            main,
            releases,
            since_build,
            contributors,
        });
        *raw = fresh;
        Ok((stats, budget))
    }

    /// The app icon blob `oid` (from [`RepoStats::icon_oid`]) as a `data:` URI.
    pub fn icon(&self, repo: &str, oid: &str) -> Result<String> {
        let resp = self
            .http
            .get(format!("{API}/repos/{repo}/git/blobs/{oid}"))
            .headers(self.headers())
            .send()?;
        let v: Value = Self::check(resp)?.json()?;
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
            out.unsaved = true;
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
            let resp = self
                .http
                .get(format!(
                    "{API}/repos/{repo}/contributors?per_page=100&anon=1&page={page}"
                ))
                .headers(self.headers())
                .send()?;
            let resp = Self::check(resp)?;
            // 204: empty repository.
            if resp.status().as_u16() == 204 {
                break;
            }
            let body: Value = resp.json()?;
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

/// One commit's date and its author, keyed by GitHub login, else email, else name.
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
                        .or_else(|| a["name"].as_str().map(str::to_string))?;
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
    /// The last successful fetch, which [`restat`] measures again at a later
    /// time. Files written before it existed load with `None`.
    pub last: Option<LastFetch>,
}

/// What [`build_stats`] read on the last successful fetch, apart from the
/// activity [`RawActivity`] already holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastFetch {
    /// When the fetch started: the `now` its windows were measured at.
    pub at: DateTime<Utc>,
    /// The probe taken before the fetch, if there was one.
    pub probe: Option<Probe>,
    /// The answer to [`main_query`].
    pub main: Value,
    pub releases: Vec<Release>,
    pub since_build: Option<BuildLag>,
    pub contributors: Vec<String>,
}

/// The values one [`probe_query`] alias reads for a repository. They change
/// with every push to the default branch, every new, edited, commented,
/// labeled, closed or deleted issue or PR, every merge and every new or
/// edited latest release. Reactions and download counts do not change them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Probe {
    /// The commit the default branch points at.
    pub head_oid: Option<String>,
    /// Issues in every state.
    pub issues: u64,
    /// When the most recently updated issue was updated.
    pub issue_updated_at: Option<DateTime<Utc>>,
    /// PRs in every state.
    pub prs: u64,
    pub pr_updated_at: Option<DateTime<Utc>>,
    pub releases: u64,
    /// The newest release by creation date.
    pub release_tag: Option<String>,
    pub release_draft: bool,
    pub release_published_at: Option<DateTime<Utc>>,
    pub release_updated_at: Option<DateTime<Utc>>,
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

/// Whether `raw`'s last fetch can be measured again at `now` ([`restat`])
/// instead of fetching: `probe` matches the probe taken before that fetch,
/// the fetch from scratch every [`FULL_REFRESH_HOURS`] is not due, and the
/// commits `raw` holds match the fetch's per-window counts (see
/// [`commits_match_counts`]) and include the branch head (see
/// [`holds_head`]). The probe does not see reaction and download counts, so
/// a repo measured again keeps the counts of its last fetch until the next
/// one, which can be [`FULL_REFRESH_HOURS`] plus one poll interval later.
pub fn can_skip(raw: &RawActivity, probe: Option<&Probe>, now: DateTime<Utc>) -> bool {
    let (Some(probe), Some(last)) = (probe, raw.last.as_ref()) else {
        return false;
    };
    if last.probe.as_ref() != Some(probe) {
        return false;
    }
    let release_at = summarize_releases(&last.releases).latest_at;
    !raw.fetch_from(activity_start(now, release_at), now).1
        && commits_match_counts(&last.main, &raw.commits, last.at)
        && holds_head(last, &raw.commits, activity_start(last.at, release_at))
}

/// Whether `commits` holds the default branch's head that the probe before
/// `last` saw, when the head's date in `last.main` is `since` or later.
/// After a rewrite of the branch, an incremental fetch walks only the new
/// commits dated after it started, so a head dated earlier is missing, and
/// the commits held no longer match the branch.
fn holds_head(
    last: &LastFetch,
    commits: &BTreeMap<String, CommitAuthor>,
    since: DateTime<Utc>,
) -> bool {
    let head = last.probe.as_ref().and_then(|p| p.head_oid.as_deref());
    let target = &last.main["repository"]["defaultBranchRef"]["target"];
    match (head, first_time(&target["all"], "committedDate")) {
        (Some(head), Some(at)) if at >= since => commits.contains_key(head),
        _ => true,
    }
}

/// Whether `commits` holds exactly as many commits in each [`WINDOWS`]
/// window at `then` as GitHub counted in `main`. [`aged_window_counts`]
/// needs that. It fails when a history walk stopped at [`MAX_PAGES`], when
/// an incremental fetch missed a commit dated before it started, and when a
/// rewritten default branch left commits that are no longer on it. A
/// window with no count matches only when `main` has no branch head and
/// `commits` is empty, as for an empty repository.
fn commits_match_counts(
    main: &Value,
    commits: &BTreeMap<String, CommitAuthor>,
    then: DateTime<Utc>,
) -> bool {
    let head = &main["repository"]["defaultBranchRef"]["target"];
    windows(then, None).all(|(_, key, start)| {
        let Some(n) = head[format!("c{key}")]["totalCount"].as_u64() else {
            return !head.is_object() && commits.is_empty();
        };
        let start = start.trunc_subsecs(0);
        commits.values().filter(|(at, _)| *at >= start).count() as u64 == n
    })
}

/// The stats of `raw`'s last fetch measured at `now`, without a request:
/// the windows move to `now`, and issues are ranked by their age at `now`.
/// Valid only while nothing changed on GitHub (see [`can_skip`]). `raw` is
/// left as it is, so the next fetch is incremental from the last real one
/// and a later `restat` still has the commits it needs. The PRs come from
/// `ledger`, the repo's shared [`PrLedger`], as in [`GitHub::fetch_repo`].
pub fn restat(raw: &RawActivity, ledger: &PrLedger, now: DateTime<Utc>) -> Option<RepoStats> {
    let last = raw.last.as_ref()?;
    let release_at = summarize_releases(&last.releases).latest_at;
    // What a fetch at `now` would hold after pruning.
    let since = activity_start(now, release_at);
    // A ledger that doesn't reach back to `since` (deleted, corrupt or never
    // walked) would count too few PRs: fetch instead, which walks it.
    if !(ledger.walked_at.is_some() && ledger.covers_from.is_some_and(|c| c <= since)) {
        return None;
    }
    let mut main = last.main.clone();
    aged_window_counts(&mut main, &raw.commits, last.at, now, release_at);
    let authors: Vec<CommitAuthor> = raw
        .commits
        .values()
        .filter(|(at, _)| *at >= since)
        .cloned()
        .collect();
    let events = Events {
        prs_opened: ledger.opened_since(since),
        prs_merged: ledger.merged_since(since),
        issues_opened: raw
            .issues_opened
            .values()
            .copied()
            .filter(|at| *at >= since)
            .collect(),
    };
    Some(build_stats(
        &main,
        &last.releases,
        last.since_build.clone(),
        &last.contributors,
        &authors,
        &events,
        now,
    ))
}

/// Moves the per-window commit counts in `main`, measured at `then`, to
/// `now`: each count loses the `commits` whose dates its window has left.
/// Exact as long as no commit reached the default branch in between and
/// `commits` holds every commit since the start of the longest window at
/// `then`. The window starts are truncated to whole seconds, as
/// [`main_query`] sends them.
pub fn aged_window_counts(
    main: &mut Value,
    commits: &BTreeMap<String, CommitAuthor>,
    then: DateTime<Utc>,
    now: DateTime<Utc>,
    release_at: Option<DateTime<Utc>>,
) {
    // Indexing a missing key mutably would insert it, so look first.
    if !main["repository"]["defaultBranchRef"]["target"].is_object() {
        return;
    }
    let head = &mut main["repository"]["defaultBranchRef"]["target"];
    for ((_, key, start_then), (_, _, start_now)) in
        windows(then, release_at).zip(windows(now, release_at))
    {
        let (start_then, start_now) = (start_then.trunc_subsecs(0), start_now.trunc_subsecs(0));
        let alias = format!("c{key}");
        let Some(n) = head[&alias]["totalCount"].as_u64() else {
            continue;
        };
        let gone = commits
            .values()
            .filter(|(at, _)| start_then <= *at && *at < start_now)
            .count() as u64;
        head[&alias]["totalCount"] = json!(n.saturating_sub(gone));
    }
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

/// [`PROBE_QUERY`] for `n` repositories, aliased `r0`, `r1`… and named by
/// the variables `$o0`/`$n0`, `$o1`/`$n1`…
pub fn probe_query(n: usize) -> String {
    let mut vars = Vec::new();
    let mut repos = String::new();
    for i in 0..n {
        vars.push(format!("$o{i}: String!, $n{i}: String!"));
        repos += &format!("  r{i}: repository(owner: $o{i}, name: $n{i}) {{ ...P }}\n");
    }
    PROBE_QUERY
        .replace("{VARS}", &vars.join(", "))
        .replace("{REPOS}", &repos)
}

/// One [`Probe`] per alias of [`probe_query`]`(n)`; `None` for an alias
/// GitHub answered with null (a missing or hidden repository).
pub fn parse_probe(data: &Value, n: usize) -> Vec<Option<Probe>> {
    (0..n)
        .map(|i| {
            let r = &data[format!("r{i}")];
            if r.is_null() {
                return None;
            }
            let release = &r["releases"]["nodes"][0];
            Some(Probe {
                head_oid: r["defaultBranchRef"]["target"]["oid"]
                    .as_str()
                    .map(str::to_string),
                issues: count(&r["issues"]),
                issue_updated_at: first_time(&r["lastIssue"], "updatedAt"),
                prs: count(&r["pullRequests"]),
                pr_updated_at: first_time(&r["lastPr"], "updatedAt"),
                releases: count(&r["releases"]),
                release_tag: release["tagName"].as_str().map(str::to_string),
                release_draft: release["isDraft"].as_bool().unwrap_or(false),
                release_published_at: time(&release["publishedAt"]),
                release_updated_at: time(&release["updatedAt"]),
            })
        })
        .collect()
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

/// What can change a repository's stats, for many repositories at once (see
/// [`Probe`]).
const PROBE_QUERY: &str = r#"
query({VARS}) {
{REPOS}  rateLimit { cost remaining resetAt }
}
fragment P on Repository {
  defaultBranchRef { target { oid } }
  issues { totalCount }
  lastIssue: issues(first: 1, orderBy: {field: UPDATED_AT, direction: DESC}) { nodes { updatedAt } }
  pullRequests { totalCount }
  lastPr: pullRequests(first: 1, orderBy: {field: UPDATED_AT, direction: DESC}) { nodes { updatedAt } }
  releases(first: 1, orderBy: {field: CREATED_AT, direction: DESC}) {
    totalCount
    nodes { tagName publishedAt updatedAt isDraft }
  }
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

    /// The payloads of one fetch, shared by [`builds_stats_from_payloads`]
    /// and [`restat_at_fetch_time_matches_build_stats`].
    struct Payloads {
        main: Value,
        events: Events,
        rel: Vec<Release>,
        now: DateTime<Utc>,
        lag: BuildLag,
        commits: Vec<(String, CommitAuthor)>,
        people: Vec<String>,
    }

    fn payloads() -> Payloads {
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
        let commits = parse_authors(&json!({"nodes": [
          {"committedDate": "2026-10-08T03:55:00Z", "author": {"email": "A@x", "name": "A", "user": {"login": "ann"}}},
          {"committedDate": "2026-10-08T03:00:00Z", "author": {"email": "a@x", "name": "A", "user": {"login": "ann"}}},
          {"committedDate": "2026-10-08T02:00:00Z", "author": {"email": "Bo@Y", "name": "Bo", "user": null}},
          {"committedDate": "2026-10-03T00:00:00Z", "author": {"email": "", "name": "Cy", "user": null}}
        ]}));
        let mut people =
            parse_contributors(&json!([{"login": "ann"}, {"email": "Old@Z", "type": "Anonymous"}]));
        people.extend((0..10).map(|i| format!("p{i}")));
        Payloads {
            main,
            events,
            rel,
            now,
            lag,
            commits,
            people,
        }
    }

    #[test]
    fn builds_stats_from_payloads() {
        let Payloads {
            main,
            events,
            rel,
            now,
            lag,
            commits,
            people,
        } = payloads();
        let authors: Vec<CommitAuthor> = commits.into_iter().map(|(_, a)| a).collect();
        assert_eq!(authors[2].1, "bo@y");
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
    fn probe_query_aliases_every_repo() {
        let q = probe_query(3);
        for i in 0..3 {
            assert!(
                q.contains(&format!("$o{i}: String!, $n{i}: String!")),
                "{q}"
            );
            let alias = format!("r{i}: repository(owner: $o{i}, name: $n{i}) {{ ...P }}");
            assert!(q.contains(&alias), "{q}");
        }
        assert!(!q.contains("r3:") && !q.contains("{VARS}") && !q.contains("{REPOS}"));
        assert!(q.contains("defaultBranchRef { target { oid } }"));
        assert!(q.contains("issues(first: 1, orderBy: {field: UPDATED_AT, direction: DESC})"));
        assert!(q.contains("pullRequests(first: 1, orderBy: {field: UPDATED_AT, direction: DESC})"));
        assert!(q.contains("releases(first: 1, orderBy: {field: CREATED_AT, direction: DESC})"));
        assert!(q.contains("rateLimit { cost remaining resetAt }"));
        assert!(!q.contains("search(") && !q.contains("pushedAt"));
    }

    #[test]
    fn parse_probe_reads_values_and_skips_missing() {
        let t = |s: &str| Some(s.parse::<DateTime<Utc>>().unwrap());
        let data = json!({
          "r0": {
            "defaultBranchRef": {"target": {"oid": "h1"}},
            "issues": {"totalCount": 90},
            "lastIssue": {"nodes": [{"updatedAt": "2026-10-08T02:00:00Z"}]},
            "pullRequests": {"totalCount": 40},
            "lastPr": {"nodes": [{"updatedAt": "2026-10-08T03:00:00Z"}]},
            "releases": {"totalCount": 2, "nodes": [
              {"tagName": "v1.1.0", "isDraft": true, "publishedAt": null, "updatedAt": "2026-10-08T00:30:00Z"}
            ]}
          },
          "r1": null,
          "r2": {
            "defaultBranchRef": null, "issues": {"totalCount": 0}, "lastIssue": {"nodes": []},
            "pullRequests": {"totalCount": 0}, "lastPr": {"nodes": []},
            "releases": {"totalCount": 0, "nodes": []}
          },
          "rateLimit": {"cost": 1, "remaining": 4999, "resetAt": "2026-10-08T05:00:00Z"}
        });
        let probes = parse_probe(&data, 3);
        assert_eq!(
            probes[0],
            Some(Probe {
                head_oid: Some("h1".into()),
                issues: 90,
                issue_updated_at: t("2026-10-08T02:00:00Z"),
                prs: 40,
                pr_updated_at: t("2026-10-08T03:00:00Z"),
                releases: 2,
                release_tag: Some("v1.1.0".into()),
                release_draft: true,
                release_published_at: None,
                release_updated_at: t("2026-10-08T00:30:00Z"),
            })
        );
        assert_eq!(probes[1], None);
        // An empty repository: no branch, no issues, no releases.
        assert_eq!(probes[2], Some(Probe::default()));
        assert_eq!(probes.len(), 3);
    }

    #[test]
    fn skip_needs_a_matching_probe() {
        let t = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        let now = t("2026-10-08T04:00:00Z");
        let probe = Probe {
            head_oid: Some("h1".into()),
            issues: 3,
            ..Default::default()
        };
        let raw = RawActivity {
            covers_from: Some(t("2026-10-01T03:25:00Z")),
            fetched_at: Some(t("2026-10-08T03:25:00Z")),
            full_at: Some(t("2026-10-08T01:00:00Z")),
            last: Some(LastFetch {
                at: t("2026-10-08T03:25:00Z"),
                probe: Some(probe.clone()),
                main: Value::Null,
                releases: vec![],
                since_build: None,
                contributors: vec![],
            }),
            ..Default::default()
        };
        assert!(can_skip(&raw, Some(&probe), now));
        let no_last = RawActivity {
            last: None,
            ..raw.clone()
        };
        assert!(!can_skip(&no_last, Some(&probe), now));
        assert!(!can_skip(&raw, None, now));
        let pushed = Probe {
            head_oid: Some("h2".into()),
            ..probe.clone()
        };
        assert!(!can_skip(&raw, Some(&pushed), now));
        // The last fetch went without a probe: a row refresh, or a failed probe.
        let unprobed = RawActivity {
            last: raw.last.clone().map(|l| LastFetch { probe: None, ..l }),
            ..raw.clone()
        };
        assert!(!can_skip(&unprobed, Some(&probe), now));
        // The fetch from scratch is due six hours after the last one.
        assert!(can_skip(&raw, Some(&probe), t("2026-10-08T06:59:00Z")));
        assert!(!can_skip(&raw, Some(&probe), t("2026-10-08T07:00:00Z")));
        // The branch head with a commit count for each window, `c0` to `c6`.
        let counts = |c: [u64; 7]| -> serde_json::Map<String, Value> {
            c.iter()
                .enumerate()
                .map(|(i, n)| (format!("c{i}"), json!({"totalCount": n})))
                .collect()
        };
        // GitHub counted one commit in each window at 03:25.
        let mut counted = raw.clone();
        if let Some(last) = counted.last.as_mut() {
            last.main = json!({"repository": {"defaultBranchRef": {"target": counts([1; 7])}}});
        }
        // A history walk that stopped early doesn't hold it.
        assert!(!can_skip(&counted, Some(&probe), now));
        counted
            .commits
            .insert("a".into(), (t("2026-10-08T03:20:00Z"), "ann".into()));
        assert!(can_skip(&counted, Some(&probe), now));
        // An answer that left out a window's count can't be checked.
        let mut partial = counted.clone();
        if let Some(last) = partial.last.as_mut() {
            if let Some(target) =
                last.main["repository"]["defaultBranchRef"]["target"].as_object_mut()
            {
                target.remove("c3");
            }
        }
        assert!(!can_skip(&partial, Some(&probe), now));
        // Nor can one without the branch head, while commits are held.
        if let Some(last) = partial.last.as_mut() {
            last.main = json!({"repository": {"defaultBranchRef": {"target": null}}});
        }
        assert!(!can_skip(&partial, Some(&probe), now));
        // The cache holds a commit that a force-push removed from the branch.
        counted
            .commits
            .insert("x".into(), (t("2026-10-08T03:22:00Z"), "bo".into()));
        assert!(!can_skip(&counted, Some(&probe), now));
        // The head h1 is dated inside the week, and an incremental fetch after
        // a rewrite did not walk it: the commits held miss it.
        let head_dated = |date: &str, c: [u64; 7]| {
            let mut r = raw.clone();
            if let Some(last) = r.last.as_mut() {
                let mut target = counts(c);
                target.insert(
                    "all".into(),
                    json!({"totalCount": 9, "nodes": [{"committedDate": date}]}),
                );
                last.main = json!({"repository": {"defaultBranchRef": {"target": target}}});
            }
            r
        };
        let mut rewritten = head_dated("2026-10-06T00:00:00Z", [0, 0, 0, 0, 0, 0, 1]);
        rewritten
            .commits
            .insert("y".into(), (t("2026-10-05T00:00:00Z"), "bo".into()));
        assert!(!can_skip(&rewritten, Some(&probe), now));
        rewritten.commits.clear();
        rewritten
            .commits
            .insert("h1".into(), (t("2026-10-06T00:00:00Z"), "ann".into()));
        assert!(can_skip(&rewritten, Some(&probe), now));
        // A head older than the week isn't held, and needn't be.
        assert!(can_skip(
            &head_dated("2026-09-01T00:00:00Z", [0; 7]),
            Some(&probe),
            now
        ));
    }

    #[test]
    fn restat_at_fetch_time_matches_build_stats() {
        let p = payloads();
        let mut raw = RawActivity {
            commits: p.commits.iter().cloned().collect(),
            ..Default::default()
        };
        raw.issues_opened.extend(
            p.events
                .issues_opened
                .iter()
                .enumerate()
                .map(|(i, t)| (i as u64, *t)),
        );
        let mut ledger = PrLedger {
            walked_at: Some(p.now),
            covers_from: Some(p.now - chrono::Duration::days(365)),
            ..Default::default()
        };
        for (i, at) in p.events.prs_opened.iter().enumerate() {
            ledger.prs.insert(
                i as u64,
                LedgerPr {
                    created_at: Some(*at),
                    ..Default::default()
                },
            );
        }
        for (i, at) in p.events.prs_merged.iter().enumerate() {
            ledger.prs.insert(
                10_000 + i as u64,
                LedgerPr {
                    state: "MERGED".into(),
                    merged_at: Some(*at),
                    ..Default::default()
                },
            );
        }
        assert_eq!(
            restat(&raw, &ledger, p.now),
            None,
            "no fetch to measure again"
        );
        raw.last = Some(LastFetch {
            at: p.now,
            probe: None,
            main: p.main.clone(),
            releases: p.rel.clone(),
            since_build: Some(p.lag.clone()),
            contributors: p.people.clone(),
        });
        let authors: Vec<CommitAuthor> = p.commits.into_iter().map(|(_, a)| a).collect();
        let fetched = build_stats(
            &p.main,
            &p.rel,
            Some(p.lag),
            &p.people,
            &authors,
            &p.events,
            p.now,
        );
        assert_eq!(restat(&raw, &ledger, p.now), Some(fetched));
        // A ledger that is gone, or doesn't reach back far enough, sends the
        // repo to a fetch instead of counting no PRs.
        assert_eq!(restat(&raw, &PrLedger::default(), p.now), None);
        let short = PrLedger {
            covers_from: Some(p.now - chrono::Duration::hours(1)),
            ..ledger
        };
        assert_eq!(restat(&raw, &short, p.now), None);
    }

    #[test]
    fn restat_moves_windows_to_now() {
        let t = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        let then = t("2026-10-08T04:00:00Z");
        let main = json!({"repository": {
          "openPrs": {"totalCount": 1}, "openIssues": {"totalCount": 1},
          "recentOpen": {"nodes": [
            {"number": 5, "title": "Export loses layers", "url": "u5", "createdAt": "2026-10-07T04:10:00Z",
             "updatedAt": "2026-10-07T04:10:00Z", "authorAssociation": "NONE",
             "comments": {"totalCount": 0}, "reactions": {"totalCount": 0}, "labels": {"nodes": [{"name": "bug"}]}}
          ]},
          "hotOpen": {"nodes": []},
          "defaultBranchRef": {"target": {
            "all": {"totalCount": 500, "nodes": [{"committedDate": "2026-10-08T03:58:00Z"}]},
            "c0": {"totalCount": 2}, "c1": {"totalCount": 3}, "c2": {"totalCount": 3}, "c3": {"totalCount": 3},
            "c4": {"totalCount": 3}, "c5": {"totalCount": 3}, "c6": {"totalCount": 4}}}
        }});
        let mut raw = RawActivity {
            covers_from: Some(t("2026-10-01T04:00:00Z")),
            fetched_at: Some(then),
            full_at: Some(then),
            last: Some(LastFetch {
                at: then,
                probe: None,
                main,
                releases: vec![],
                since_build: None,
                contributors: vec![],
            }),
            ..Default::default()
        };
        for (oid, at, who) in [
            ("a", "2026-10-08T03:55:00Z", "ann"),
            ("b", "2026-10-08T03:40:00Z", "bo"),
            ("c", "2026-10-01T04:10:00Z", "cy"),
            ("d", "2026-10-08T03:58:00Z", "dee"),
        ] {
            raw.commits.insert(oid.into(), (t(at), who.into()));
        }
        // Opened inside the last 4 h at `then`; merged inside the last week.
        let ledger = PrLedger {
            prs: BTreeMap::from([
                (
                    1,
                    LedgerPr {
                        state: "OPEN".into(),
                        created_at: Some(t("2026-10-08T00:10:00Z")),
                        ..Default::default()
                    },
                ),
                (
                    2,
                    LedgerPr {
                        state: "MERGED".into(),
                        merged_at: Some(t("2026-10-01T04:10:00Z")),
                        ..Default::default()
                    },
                ),
            ]),
            walked_at: Some(then),
            covers_from: Some(t("2026-09-01T00:00:00Z")),
            ..Default::default()
        };
        let held = raw.clone();
        let before = restat(&raw, &ledger, then).unwrap();
        let w = |s: &RepoStats, id: &str| s.recent[id];
        assert_eq!(
            ["10m", "30m", "7d"].map(|id| w(&before, id).commits),
            [2, 3, 4]
        );
        assert_eq!(w(&before, "4h").prs_opened, 1);
        assert!(before.urgent[0].reasons.contains(&"new".to_string()));

        let now = t("2026-10-08T04:20:00Z");
        let after = restat(&raw, &ledger, now).unwrap();
        // a and d left the 10-minute window, b the 30-minute one, c the week.
        assert_eq!(w(&after, "10m").commits, 0);
        assert_eq!(w(&after, "30m").commits, 2);
        assert_eq!(w(&after, "1h").commits, 3);
        assert_eq!(w(&after, "7d").commits, 3);
        assert_eq!(w(&after, "4h").prs_opened, 0);
        assert_eq!(w(&after, "7d").prs_merged, 0);
        // cy's only commit is older than the week, so a fetch would drop it.
        assert_eq!(
            (w(&before, ALL_TIME).people, w(&after, ALL_TIME).people),
            (4, 3)
        );
        // Issue #5 is now more than 24 hours old.
        assert!(!after.urgent[0].reasons.contains(&"new".to_string()));
        assert_eq!(after.urgent[0].score, before.urgent[0].score - 4.0);
        assert!(after.urgency < before.urgency);
        // Unchanged: a later restat from the same fetch still has every commit.
        assert_eq!(raw, held);
        let later = restat(&raw, &ledger, t("2026-10-08T04:25:00Z")).unwrap();
        assert_eq!(w(&later, "7d").commits, 3);
        raw.last = None;
        assert_eq!(restat(&raw, &ledger, now), None);
    }

    #[test]
    fn aged_counts_start_windows_on_whole_seconds() {
        let t = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        // The query sent "since: 03:50:00Z", so GitHub counted this commit.
        let commits = BTreeMap::from([(
            "a".to_string(),
            (t("2026-10-08T03:50:00Z"), "ann".to_string()),
        )]);
        let mut main =
            json!({"repository": {"defaultBranchRef": {"target": {"c0": {"totalCount": 1}}}}});
        aged_window_counts(
            &mut main,
            &commits,
            t("2026-10-08T04:00:00.700Z"),
            t("2026-10-08T04:20:00.700Z"),
            None,
        );
        assert_eq!(
            main["repository"]["defaultBranchRef"]["target"]["c0"]["totalCount"],
            0
        );
        let mut empty = json!({"repository": {"defaultBranchRef": null}});
        aged_window_counts(
            &mut empty,
            &commits,
            t("2026-10-08T04:00:00Z"),
            t("2026-10-08T05:00:00Z"),
            None,
        );
        assert_eq!(empty, json!({"repository": {"defaultBranchRef": null}}));
    }

    #[test]
    fn raw_without_last_loads() {
        let dir = std::env::temp_dir().join(format!("craft-raw-last-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        // A file written before the last fetch was kept.
        std::fs::write(
            RawActivity::path(&dir, "o/old"),
            r#"{"fetched_at":"2026-10-08T03:55:00Z","prs_opened":{"7":"2026-10-08T03:00:00Z"}}"#,
        )
        .unwrap();
        let old = RawActivity::load(&dir, "o/old");
        assert_eq!(old.last, None);
        assert_eq!(
            old.fetched_at,
            Some("2026-10-08T03:55:00Z".parse().unwrap())
        );
        let p = payloads();
        let raw = RawActivity {
            last: Some(LastFetch {
                at: p.now,
                probe: Some(Probe {
                    head_oid: Some("h1".into()),
                    release_published_at: Some(p.now),
                    ..Default::default()
                }),
                main: p.main,
                releases: p.rel,
                since_build: Some(p.lag),
                contributors: p.people,
            }),
            ..Default::default()
        };
        raw.save(&dir, "o/new").unwrap();
        assert_eq!(RawActivity::load(&dir, "o/new"), raw);
        std::fs::remove_dir_all(&dir).ok();
    }
}
