//! Fetches one repository's [`RepoStats`] from GitHub.
//!
//! Per repo: one (usually) GraphQL query for releases + asset download
//! counts; one GraphQL query for counts, dates, every recent-activity window
//! ([`WINDOWS`], plus [`SINCE_RELEASE`], which needs the releases), the app
//! icon's blob id and the open issues to rank; one GraphQL query comparing the
//! latest build's tag with `main`; one or more GraphQL queries for the commit
//! authors since a week ago or the latest build, whichever is earlier, which
//! give each window's people;
//! one (usually) REST call for the contributor list, which GraphQL lacks.
//! The icon itself is a REST blob fetch, made only when its blob id changes
//! (see [`GitHub::icon`]). Blocking I/O: callers run this on worker threads.

use std::process::Command;
use std::time::Duration;

use chrono::{DateTime, Utc};
use craft_core::{
    rank_issues, summarize_releases, Activity, Asset, BuildLag, IssueInput, LagBasis, Release,
    RepoStats, ALL_TIME, SINCE_RELEASE, WINDOWS,
};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde_json::{json, Value};

const API: &str = "https://api.github.com";
/// How many ranked issues each repo keeps for the Urgent view.
const KEEP_URGENT: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no GitHub token: set github_token in ~/.craft_status_config.toml, export GITHUB_TOKEN, or run `gh auth login`")]
    NoToken,
    #[error("GitHub rejected the token (401); check github_token / `gh auth status`")]
    Unauthorized,
    #[error("rate limited by GitHub until {0}")]
    RateLimited(String),
    #[error("GitHub HTTP {0}: {1}")]
    Http(u16, String),
    #[error("network: {0}")]
    Network(#[from] reqwest::Error),
    #[error("GitHub: {0}")]
    GraphQl(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Token from config, then `GITHUB_TOKEN` / `GH_TOKEN`, then the GitHub CLI.
/// Apps launched from Finder don't inherit the shell PATH, so the usual
/// install locations of `gh` are tried explicitly.
pub fn resolve_token(configured: &str) -> Result<String> {
    let t = configured.trim();
    if !t.is_empty() {
        return Ok(t.to_string());
    }
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return Ok(v.trim().to_string());
            }
        }
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
                return Ok(tok);
            }
        }
    }
    Err(Error::NoToken)
}

pub struct GitHub {
    http: Client,
    token: String,
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
        Ok(GitHub { http, token })
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
        let remaining = resp
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let reset = resp
            .headers()
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
            .and_then(|t| DateTime::from_timestamp(t, 0));
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
            .unwrap_or_else(|| body.chars().take(200).collect());
        Err(Error::Http(status.as_u16(), msg))
    }

    fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let resp = self
            .http
            .post(format!("{API}/graphql"))
            .headers(self.headers())
            .json(&json!({ "query": query, "variables": variables }))
            .send()?;
        let v: Value = Self::check(resp)?.json()?;
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
                return Err(Error::GraphQl(msg));
            }
        }
        Ok(v["data"].clone())
    }

    /// Everything the UI shows for `owner/name`, measured at `now`.
    pub fn fetch_repo(&self, repo: &str, now: DateTime<Utc>) -> Result<(RepoStats, Budget)> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| Error::GraphQl(format!("bad repo {repo:?}")))?;
        let releases = self.releases(owner, name)?;
        let release_at = summarize_releases(&releases).latest_at;
        let main = self.graphql(
            &main_query(repo, now, release_at),
            json!({ "owner": owner, "name": name }),
        )?;
        let since_build = self.since_build(owner, name, &main, &releases)?;
        let authors = self.authors(owner, name, now, release_at)?;
        let contributors = self.contributors(repo)?;
        let budget = Budget {
            graphql_remaining: main["rateLimit"]["remaining"].as_u64(),
        };
        let stats = build_stats(&main, &releases, since_build, &contributors, &authors, now);
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

    /// `(committed, author)` for each commit on `main` in the longest window,
    /// or since the latest build if that's longer.
    fn authors(
        &self,
        owner: &str,
        name: &str,
        now: DateTime<Utc>,
        release_at: Option<DateTime<Utc>>,
    ) -> Result<Vec<CommitAuthor>> {
        let longest = WINDOWS.iter().map(|(_, m)| *m).max().unwrap_or(0);
        let since = (now - chrono::Duration::minutes(longest))
            .min(release_at.unwrap_or(now))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        // 10 pages × 100 commits; a busier stretch undercounts its oldest people.
        for _ in 0..10 {
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

pub fn parse_authors(conn: &Value) -> Vec<CommitAuthor> {
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
                    Some((time(&c["committedDate"])?, who))
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

fn search_count(main: &Value, alias: &str) -> u64 {
    main[alias]["issueCount"].as_u64().unwrap_or(0)
}

/// Reads the per-window aliases [`main_query`] asked for. A repo with no
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
    let window = |key: &str, since: DateTime<Utc>| Activity {
        commits: count(&head[format!("c{key}")]),
        prs_opened: search_count(main, &format!("p{key}")),
        prs_merged: search_count(main, &format!("m{key}")),
        issues_opened: search_count(main, &format!("i{key}")),
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

/// [`MAIN_QUERY`] with one commit-history count and three searches per
/// window, aliased by window index (`c0`, `p0`, `m0`, `i0`…), plus the
/// [`SINCE_RELEASE`] window (`cr`, `pr`…) when there is a build published at
/// `release_at`. GitHub charges the whole query one rate-limit point however
/// many windows it holds.
pub fn main_query(repo: &str, now: DateTime<Utc>, release_at: Option<DateTime<Utc>>) -> String {
    let mut history = String::new();
    let mut searches = String::new();
    for (_, i, since) in windows(now, release_at) {
        let since = since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        history += &format!("c{i}: history(since: \"{since}\") {{ totalCount }}\n");
        for (alias, filter) in [
            ("p", format!("is:pr created:>={since}")),
            ("m", format!("is:pr merged:>={since}")),
            ("i", format!("is:issue created:>={since}")),
        ] {
            let q = Value::from(format!("repo:{repo} {filter}"));
            searches +=
                &format!("  {alias}{i}: search(query: {q}, type: ISSUE) {{ issueCount }}\n");
        }
    }
    MAIN_QUERY
        .replace("{HISTORY}", &history)
        .replace("{SEARCHES}", &searches)
}

/// Pure: turn the GraphQL payloads into the model (unit-tested below).
pub fn build_stats(
    main: &Value,
    releases: &[Release],
    since_build: Option<BuildLag>,
    contributors: &[String],
    authors: &[CommitAuthor],
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
{SEARCHES}  rateLimit { remaining resetAt }
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
            nodes { committedDate author { email name user { login } } }
          }
        }
      }
    }
  }
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
          },
          "p3": {"issueCount": 2}, "m3": {"issueCount": 3}, "i3": {"issueCount": 4}, "m6": {"issueCount": 30},
          "pr": {"issueCount": 11}, "mr": {"issueCount": 12}, "ir": {"issueCount": 13}
        });
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
        assert_eq!(authors[2].1, "bo@y");
        let mut people =
            parse_contributors(&json!([{"login": "ann"}, {"email": "Old@Z", "type": "Anonymous"}]));
        people.extend((0..10).map(|i| format!("p{i}")));
        assert_eq!(people[1], "old@z");
        let s = build_stats(&main, &rel, Some(lag.clone()), &people, &authors, now);
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
                prs_merged: 12,
                issues_opened: 13,
                // Cy committed on the 3rd, after v1.0.0 but before the last week.
                people: 3,
            }
        );
        let no_lag = build_stats(&main, &rel, None, &people, &authors, now);
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
        let q = main_query("o/r", now, None);
        assert!(q.contains(r#"c3: history(since: "2026-10-08T00:00:00Z")"#));
        assert!(q.contains(r#"m6: search(query: "repo:o/r is:pr merged:>=2026-10-01T04:00:00Z""#));
        assert!(!q.contains("{HISTORY}") && !q.contains("{SEARCHES}"));
        assert!(!q.contains("cr:"), "no build, no since-release window");
        let q = main_query("o/r", now, Some("2026-09-20T12:00:00Z".parse().unwrap()));
        assert!(q.contains(r#"cr: history(since: "2026-09-20T12:00:00Z")"#));
        assert!(
            q.contains(r#"ir: search(query: "repo:o/r is:issue created:>=2026-09-20T12:00:00Z""#)
        );
    }

    #[test]
    fn empty_repository_is_all_zero() {
        let mut s = build_stats(
            &json!({"repository": {"defaultBranchRef": null}}),
            &[],
            None,
            &[],
            &[],
            Utc::now(),
        );
        assert_eq!(s.recent.len(), WINDOWS.len() + 1);
        assert!(s.recent.values().all(|a| *a == Activity::default()));
        s.recent.clear();
        assert_eq!(s, RepoStats::default());
    }
}
