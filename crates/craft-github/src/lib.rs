//! Fetches one repository's [`RepoStats`] from GitHub.
//!
//! Per repo: one GraphQL query for counts, dates, every recent-activity window
//! ([`WINDOWS`]) and the open issues to rank; one (usually) GraphQL query for releases + asset
//! download counts; one REST call for the contributor count (GraphQL has no
//! such field — we ask for one per page and read the last page number from
//! the `Link` header). Blocking I/O: callers run this on worker threads.

use std::process::Command;
use std::time::Duration;

use chrono::{DateTime, Utc};
use craft_core::{
    rank_issues, summarize_releases, Activity, Asset, IssueInput, Release, RepoStats, WINDOWS,
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
        let main = self.graphql(
            &main_query(repo, now),
            json!({ "owner": owner, "name": name }),
        )?;
        let releases = self.releases(owner, name)?;
        let contributors = self.contributors(repo)?;
        let budget = Budget {
            graphql_remaining: main["rateLimit"]["remaining"].as_u64(),
        };
        Ok((build_stats(&main, &releases, contributors, now), budget))
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

    fn contributors(&self, repo: &str) -> Result<u64> {
        let resp = self
            .http
            .get(format!("{API}/repos/{repo}/contributors?per_page=1"))
            .headers(self.headers())
            .send()?;
        let resp = Self::check(resp)?;
        // 204: empty repository.
        if resp.status().as_u16() == 204 {
            return Ok(0);
        }
        let link = resp
            .headers()
            .get("link")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if let Some(n) = link.as_deref().and_then(last_page) {
            return Ok(n);
        }
        let body: Value = resp.json()?;
        Ok(body.as_array().map_or(0, |a| a.len() as u64))
    }
}

/// `<…&page=37>; rel="last"` → 37.
pub fn last_page(link: &str) -> Option<u64> {
    link.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        if !rel.contains("rel=\"last\"") {
            return None;
        }
        let url = url.trim().trim_start_matches('<').trim_end_matches('>');
        let query = url.split_once('?')?.1;
        query
            .split('&')
            .find_map(|kv| kv.strip_prefix("page="))
            .and_then(|n| n.parse().ok())
    })
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
fn recent_activity(main: &Value) -> std::collections::BTreeMap<String, Activity> {
    if main["repository"].is_null() {
        return Default::default();
    }
    let head = &main["repository"]["defaultBranchRef"]["target"];
    WINDOWS
        .iter()
        .enumerate()
        .map(|(i, (id, _))| {
            let a = Activity {
                commits: count(&head[format!("c{i}")]),
                prs_opened: search_count(main, &format!("p{i}")),
                prs_merged: search_count(main, &format!("m{i}")),
                issues_opened: search_count(main, &format!("i{i}")),
            };
            (id.to_string(), a)
        })
        .collect()
}

/// [`MAIN_QUERY`] with one commit-history count and three searches per
/// window, aliased by window index (`c0`, `p0`, `m0`, `i0`…). GitHub charges
/// the whole query one rate-limit point however many windows it holds.
pub fn main_query(repo: &str, now: DateTime<Utc>) -> String {
    let mut history = String::new();
    let mut searches = String::new();
    for (i, (_, minutes)) in WINDOWS.iter().enumerate() {
        let since = (now - chrono::Duration::minutes(*minutes))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
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
    contributors: u64,
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
        recent: recent_activity(main),
        commits_total: count(&head["all"]),
        contributors,
        issues_total: count(&r["allIssues"]),
        prs_total: count(&r["allPrs"]),
        latest_build: rel.latest_tag,
        latest_build_at: rel.latest_at,
        latest_build_prerelease: rel.latest_prerelease,
        latest_build_downloads: rel.latest_downloads,
        downloads_total: rel.total_downloads,
        urgency,
        critical_issues: critical,
        urgent,
    }
}

const MAIN_QUERY: &str = r#"
query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    openPrs: pullRequests(states: OPEN) { totalCount }
    allPrs: pullRequests { totalCount }
    oldestOpenPr: pullRequests(states: OPEN, first: 1, orderBy: {field: CREATED_AT, direction: ASC}) { nodes { createdAt } }
    newestPr: pullRequests(first: 1, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { createdAt } }
    openIssues: issues(states: OPEN) { totalCount }
    allIssues: issues { totalCount }
    newestIssue: issues(first: 1, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { createdAt } }
    recentOpen: issues(states: OPEN, first: 100, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { ...I } }
    hotOpen: issues(states: OPEN, first: 30, orderBy: {field: COMMENTS, direction: DESC}) { nodes { ...I } }
    defaultBranchRef {
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
        releaseAssets(first: 100) { nodes { name downloadCount } }
      }
    }
  }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_header_last_page() {
        let l = r#"<https://api.github.com/repositories/1/contributors?per_page=1&page=2>; rel="next", <https://api.github.com/repositories/1/contributors?per_page=1&page=37>; rel="last""#;
        assert_eq!(last_page(l), Some(37));
        assert_eq!(last_page(r#"<https://x?page=2>; rel="next""#), None);
    }

    #[test]
    fn builds_stats_from_payloads() {
        let main = json!({
          "repository": {
            "openPrs": {"totalCount": 3}, "allPrs": {"totalCount": 40},
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
              "c0": {"totalCount": 1}, "c3": {"totalCount": 9}}}
          },
          "p3": {"issueCount": 2}, "m3": {"issueCount": 3}, "i3": {"issueCount": 4}, "m6": {"issueCount": 30}
        });
        let rel = parse_releases(&json!({"nodes": [
          {"tagName": "v1.1.0", "isDraft": true, "isPrerelease": false, "publishedAt": null, "createdAt": "2026-10-08T00:00:00Z",
           "releaseAssets": {"nodes": [{"name": "a.dmg", "downloadCount": 0}]}},
          {"tagName": "v1.0.0", "isDraft": false, "isPrerelease": false, "publishedAt": "2026-10-01T00:00:00Z", "createdAt": "2026-10-01T00:00:00Z",
           "releaseAssets": {"nodes": [{"name": "a.dmg", "downloadCount": 10}, {"name": "SHA256SUMS.txt", "downloadCount": 4}]}}
        ]}));
        let now: DateTime<Utc> = "2026-10-08T04:00:00Z".parse().unwrap();
        let s = build_stats(&main, &rel, 12, now);
        assert_eq!(
            (s.open_prs, s.prs_total, s.open_issues, s.issues_total),
            (3, 40, 7, 90)
        );
        assert_eq!(s.commits_total, 1234);
        assert_eq!(s.recent.len(), WINDOWS.len());
        let a = |id: &str| s.recent[id];
        assert_eq!(
            a("4h"),
            Activity {
                commits: 9,
                prs_opened: 2,
                prs_merged: 3,
                issues_opened: 4
            }
        );
        assert_eq!((a("10m").commits, a("7d").prs_merged), (1, 30));
        assert_eq!(s.contributors, 12);
        assert_eq!(s.latest_build.as_deref(), Some("v1.0.0"));
        assert_eq!((s.latest_build_downloads, s.downloads_total), (10, 10));
        assert_eq!(
            s.last_commit_at,
            Some("2026-10-08T03:00:00Z".parse().unwrap())
        );
        assert_eq!(s.critical_issues, 1);
        assert_eq!(s.urgent[0].number, 5);
        assert!(s.urgency > 0.0);
    }

    #[test]
    fn query_asks_for_every_window() {
        let now: DateTime<Utc> = "2026-10-08T04:00:00Z".parse().unwrap();
        let q = main_query("o/r", now);
        assert!(q.contains(r#"c3: history(since: "2026-10-08T00:00:00Z")"#));
        assert!(q.contains(r#"m6: search(query: "repo:o/r is:pr merged:>=2026-10-01T04:00:00Z""#));
        assert!(!q.contains("{HISTORY}") && !q.contains("{SEARCHES}"));
    }

    #[test]
    fn empty_repository_is_all_zero() {
        let mut s = build_stats(
            &json!({"repository": {"defaultBranchRef": null}}),
            &[],
            0,
            Utc::now(),
        );
        assert_eq!(s.recent.len(), WINDOWS.len());
        assert!(s.recent.values().all(|a| *a == Activity::default()));
        s.recent.clear();
        assert_eq!(s, RepoStats::default());
    }
}
