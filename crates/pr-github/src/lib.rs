//! Fetches open PRs and who has standing in a repo, and applies it to a
//! [`RepoData`]. Token lookup and error types are craft-github's, so the two pollers
//! read the same account the same way, and so is the PR ledger that says what's
//! closed, merged and changed: whichever poller walked it last saves the other the
//! quota. Hold the shared lease around all of this. Blocking I/O: callers run it
//! on a worker thread.
//!
//! Costs, in GraphQL points: about 3 per 50 PRs listed, 1 per 100 PRs in the
//! ledger, 16 per 25 PRs inspected. No search API, which GitHub limits far more
//! tightly.

use std::time::Duration;

use chrono::{DateTime, Utc};
use craft_core::{PrLedger, SharedCache};
use pr_core::coderabbit::{actionable_count, is_coderabbit, parse_finding, truncate};
use pr_core::{
    Check, Comment, DetailRef, FileChange, Finding, History, LinkedIssue, Pr, PrDetail,
    RabbitStatus, RepoData, Review, Roster, Spend, Time,
};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde_json::{json, Value};

pub use craft_github::{resolve_token, Error, Result, Token};

const API: &str = "https://api.github.com";
/// After each GraphQL request, wait this many times as long as it took. GitHub
/// allows 60 s of GraphQL server time per minute per account. The lease means this
/// poller and the status poller never run at once, so at 1.0, like the other, the two
/// together keep to half the wall clock.
const GRAPHQL_PACING: f64 = 1.0;
/// How far before the previous list an incremental one looks, so nothing that
/// changed while it ran slips between the two.
const OVERLAP_MINUTES: i64 = 10;
/// How far back a first walk of a repo's ledger goes. The status poller needs a week
/// or more anyway, so this saves it walking the same ground again.
const LEDGER_COLD_DAYS: i64 = 7;
/// PRs per list page. GitHub answers 502 when a page takes too long, which 100 of
/// these (mergeability is slow) does on the busiest repos, so a walk steps down
/// this ladder when that happens and stays there.
const LIST_PAGE_SIZES: [u64; 3] = [50, 25, 10];
/// Guards the list walks against looping forever: 2,000 PRs at the smallest size.
const MAX_LIST_PAGES: usize = 200;
/// Pages of merge history per job, so walking it doesn't hold up other work.
const HISTORY_PAGES_PER_JOB: usize = 5;
const BODY_CHARS: usize = 6000;
const COMMENT_CHARS: usize = 500;
/// Long enough for a sizable PR's diff; beyond this, Claude is told it was cut.
pub const DIFF_CHARS: usize = 60_000;

pub struct GitHub {
    http: Client,
    token: String,
    /// The status poller's client, for what the two share.
    craft: craft_github::GitHub,
    shared: SharedCache,
}

fn time(v: &Value) -> Option<Time> {
    v.as_str()?.parse().ok()
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn total(v: &Value) -> u64 {
    v["totalCount"].as_u64().unwrap_or(0)
}

fn spend_of(data: &Value) -> Spend {
    let r = &data["rateLimit"];
    Spend {
        cost: r["cost"].as_u64().unwrap_or(0),
        remaining: r["remaining"].as_u64(),
        reset_at: time(&r["resetAt"]),
    }
}

impl GitHub {
    pub fn new(token: String, shared: SharedCache) -> Result<GitHub> {
        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        let craft = craft_github::GitHub::new(token.clone())?.with_pacing(GRAPHQL_PACING);
        Ok(GitHub {
            http,
            token,
            craft,
            shared,
        })
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(USER_AGENT, "craft-pr-review".parse().unwrap());
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
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let remaining = header("x-ratelimit-remaining");
        let as_num = |s: Option<String>| s.and_then(|s| s.parse::<i64>().ok());
        let reset = as_num(header("retry-after"))
            .map(|secs| Utc::now() + chrono::Duration::seconds(secs))
            .or_else(|| {
                as_num(header("x-ratelimit-reset")).and_then(|t| DateTime::from_timestamp(t, 0))
            });
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

    fn graphql(&self, query: &str, variables: Value) -> Result<(Value, Spend)> {
        let started = std::time::Instant::now();
        let sent = self
            .http
            .post(format!("{API}/graphql"))
            .headers(self.headers())
            .json(&json!({ "query": query, "variables": variables }))
            .send();
        std::thread::sleep(started.elapsed().mul_f64(GRAPHQL_PACING));
        let v: Value = Self::check(sent?)?.json()?;
        let data = v["data"].clone();
        if let Some(errs) = v["errors"].as_array().filter(|e| !e.is_empty()) {
            let msg = errs
                .iter()
                .filter_map(|e| e["message"].as_str())
                .collect::<Vec<_>>()
                .join("; ");
            if errs.iter().any(|e| e["type"] == "RATE_LIMITED") {
                return Err(Error::RateLimited("the top of the hour".into()));
            }
            // Some fields may still be fine: `collaborators` is refused without
            // push access, and `nodes` holds nulls for deleted PRs.
            let usable = !data.is_null()
                && !(data.get("repository").is_some() && data["repository"].is_null());
            if !usable {
                if errs.iter().any(|e| e["type"] == "NOT_FOUND") {
                    return Err(Error::NotFound);
                }
                return Err(Error::GraphQl(msg));
            }
        }
        let spend = spend_of(&data);
        Ok((data, spend))
    }

    /// One page of open PRs, most recently updated first, retrying smaller when
    /// GitHub times out. `size` is the index into [`LIST_PAGE_SIZES`] to start from.
    fn list_page(
        &self,
        owner: &str,
        name: &str,
        after: &Option<String>,
        size: &mut usize,
    ) -> Result<(Value, Spend)> {
        loop {
            let first = LIST_PAGE_SIZES[*size];
            match self.graphql(
                LIST_QUERY,
                json!({ "owner": owner, "name": name, "after": after, "first": first }),
            ) {
                Err(e) if is_timeout(&e) && *size + 1 < LIST_PAGE_SIZES.len() => *size += 1,
                other => return other,
            }
        }
    }

    /// Walks every open PR, replacing the list. PRs missing from a finished walk
    /// are gone. `on_page` sees each page as it arrives, so a long first walk can
    /// show results as it goes. Returns each page's spend.
    pub fn list_full(
        &self,
        repo: &str,
        d: &mut RepoData,
        now: Time,
        on_page: &mut dyn FnMut(&[Pr]),
    ) -> Result<Vec<Spend>> {
        let (owner, name) = split(repo)?;
        let mut size = 0;
        let mut seen = std::collections::HashSet::new();
        let mut listed = Vec::new();
        let mut spends = Vec::new();
        let mut after: Option<String> = None;
        let mut complete = false;
        for _ in 0..MAX_LIST_PAGES {
            let (v, spend) = self.list_page(owner, name, &after, &mut size)?;
            spends.push(spend);
            let conn = &v["repository"]["pullRequests"];
            let page = parse_list(conn, now);
            on_page(&page);
            for pr in page {
                seen.insert(pr.number);
                listed.push(pr);
            }
            if conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                complete = true;
                break;
            }
            after = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
        }
        for pr in listed {
            upsert(d, pr);
        }
        if complete {
            d.prs.retain(|n, _| seen.contains(n));
        } else {
            eprintln!("{repo}: more than {MAX_LIST_PAGES} pages of open PRs; list incomplete");
        }
        d.full_list_at = Some(now);
        d.list_at = Some(now);
        self.sync_ledger(repo, d, now, &mut spends)?;
        Ok(spends)
    }

    /// Brings the list up to date with what changed since `since`, which costs
    /// much less than [`GitHub::list_full`] but can't see CI finishing or
    /// conflicts appearing: GitHub doesn't count those as updates. Lists nothing
    /// at all when the shared ledger shows no open PR has changed.
    pub fn list_since(
        &self,
        repo: &str,
        d: &mut RepoData,
        since: Time,
        now: Time,
    ) -> Result<Vec<Spend>> {
        let (owner, name) = split(repo)?;
        let since = since - chrono::Duration::minutes(OVERLAP_MINUTES);
        let mut spends = Vec::new();
        if !self.sync_ledger(repo, d, now, &mut spends)? {
            d.list_at = Some(now);
            return Ok(spends);
        }
        let mut size = 0;
        let mut after: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let (v, spend) = self.list_page(owner, name, &after, &mut size)?;
            spends.push(spend);
            let conn = &v["repository"]["pullRequests"];
            let mut done = false;
            for pr in parse_list(conn, now) {
                if pr.updated_at.is_some_and(|t| t < since) {
                    done = true;
                    break;
                }
                upsert(d, pr);
            }
            if done || conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                break;
            }
            after = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
        }
        d.list_at = Some(now);
        Ok(spends)
    }

    /// Updates the shared ledger if it needs it and applies it to `d`. `true` if
    /// an open PR changed since `d` was listed.
    fn sync_ledger(
        &self,
        repo: &str,
        d: &mut RepoData,
        now: Time,
        spends: &mut Vec<Spend>,
    ) -> Result<bool> {
        let since = now - chrono::Duration::days(LEDGER_COLD_DAYS);
        // Covering a longer span than asked is fine: never ask it to narrow.
        let sync = self.craft.sync_ledger(repo, since, now, &self.shared)?;
        spends.push(Spend {
            cost: sync.cost,
            remaining: sync.remaining,
            reset_at: sync.reset_at,
        });
        Ok(apply_ledger(&sync.ledger, d))
    }

    /// Walks some more of the merge history, picking up from where the last walk
    /// by either poller stopped.
    pub fn history(&self, repo: &str, d: &mut RepoData) -> Result<Vec<Spend>> {
        let sync = self
            .craft
            .backfill_ledger(repo, HISTORY_PAGES_PER_JOB, &self.shared)?;
        apply_ledger(&sync.ledger, d);
        Ok(vec![Spend {
            cost: sync.cost,
            remaining: sync.remaining,
            reset_at: sync.reset_at,
        }])
    }

    /// Collaborators (if the token may see them) and contributors/people.toml.
    pub fn roster(&self, repo: &str, now: Time) -> Result<(Roster, Vec<Spend>)> {
        let (owner, name) = split(repo)?;
        let mut roster = Roster {
            fetched_at: Some(now),
            ..Default::default()
        };
        let mut spends = Vec::new();
        let mut after: Option<String> = None;
        for page in 0..10 {
            let (v, spend) = self.graphql(
                ROSTER_QUERY,
                json!({ "owner": owner, "name": name, "after": after }),
            )?;
            spends.push(spend);
            let r = &v["repository"];
            if page == 0 {
                roster.listed = parse_people(r["people"]["text"].as_str().unwrap_or_default());
            }
            let conn = &r["collaborators"];
            parse_collaborators(conn, &mut roster);
            if conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                break;
            }
            after = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
        }
        Ok((roster, spends))
    }

    /// The detail of `refs`. Each result is the ref's PR with its fresh fields,
    /// or `None` if GitHub no longer has it.
    pub fn details(&self, refs: &[DetailRef], now: Time) -> Result<(Vec<DetailOutcome>, Spend)> {
        let ids: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        let (v, spend) = self.graphql(DETAIL_QUERY, json!({ "ids": ids }))?;
        let empty = vec![];
        let nodes = v["nodes"].as_array().unwrap_or(&empty);
        let out = refs
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let node = nodes.get(i).filter(|n| !n.is_null());
                (r.clone(), node.map(|n| parse_detail(n, now)))
            })
            .collect();
        Ok((out, spend))
    }

    /// The PR's diff, cut to [`DIFF_CHARS`]; `true` if it was cut.
    pub fn diff(&self, repo: &str, number: u64) -> Result<(String, bool)> {
        let resp = self
            .http
            .get(format!("{API}/repos/{repo}/pulls/{number}"))
            .headers(self.headers())
            .header(ACCEPT, "application/vnd.github.diff")
            .send()?;
        let body = Self::check(resp)?.text()?;
        let cut = body.chars().count() > DIFF_CHARS;
        Ok((
            if cut {
                body.chars().take(DIFF_CHARS).collect()
            } else {
                body
            },
            cut,
        ))
    }
}

/// Whether GitHub gave up on the request's size rather than refused it.
pub fn is_timeout(e: &Error) -> bool {
    match e {
        Error::Http(502..=504, _) => true,
        Error::Network(e) => e.is_timeout(),
        Error::GraphQl(m) => m.to_lowercase().contains("timeout"),
        _ => false,
    }
}

fn split(repo: &str) -> Result<(&str, &str)> {
    repo.split_once('/')
        .ok_or_else(|| Error::GraphQl(format!("bad repo {repo:?}")))
}

/// Adds or updates a listed PR, keeping its detail and analysis.
pub fn upsert(d: &mut RepoData, listed: Pr) {
    match d.prs.get_mut(&listed.number) {
        Some(p) => p.absorb_listing(listed),
        None => {
            d.prs.insert(listed.number, listed);
        }
    }
}

pub fn parse_list(conn: &Value, now: Time) -> Vec<Pr> {
    conn["nodes"]
        .as_array()
        .map(|nodes| nodes.iter().map(|n| parse_listed_pr(n, now)).collect())
        .unwrap_or_default()
}

/// Who has an approval as their latest verdict on a PR.
fn approvers_of(n: &Value) -> Vec<String> {
    n["latestOpinionatedReviews"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["state"] == "APPROVED")
        .filter_map(|r| r["author"]["login"].as_str().map(str::to_string))
        .collect()
}

/// Those of [`approvers_of`] whose approval is of the commit at the head of the PR.
fn approvers_on_head(n: &Value) -> Vec<String> {
    let head = n["headRefOid"].as_str().unwrap_or_default();
    n["latestOpinionatedReviews"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["state"] == "APPROVED" && r["commit"]["oid"].as_str() == Some(head))
        .filter_map(|r| r["author"]["login"].as_str().map(str::to_string))
        .collect()
}

fn parse_listed_pr(n: &Value, now: Time) -> Pr {
    let a = &n["author"];
    Pr {
        id: text(&n["id"]),
        number: n["number"].as_u64().unwrap_or(0),
        title: text(&n["title"]),
        url: text(&n["url"]),
        author: a["login"].as_str().unwrap_or("ghost").to_string(),
        author_is_bot: a["__typename"] == "Bot",
        avatar: text(&a["avatarUrl"]),
        association: text(&n["authorAssociation"]),
        created_at: time(&n["createdAt"]),
        updated_at: time(&n["updatedAt"]),
        is_draft: n["isDraft"].as_bool().unwrap_or(false),
        mergeable: text(&n["mergeable"]),
        merge_state: text(&n["mergeStateStatus"]),
        review_decision: n["reviewDecision"].as_str().map(str::to_string),
        approved_by: approvers_of(n),
        approved_on_head: approvers_on_head(n),
        additions: n["additions"].as_u64().unwrap_or(0),
        deletions: n["deletions"].as_u64().unwrap_or(0),
        changed_files: n["changedFiles"].as_u64().unwrap_or(0),
        head_oid: text(&n["headRefOid"]),
        head_ref: text(&n["headRefName"]),
        base_ref: text(&n["baseRefName"]),
        cross_repo: n["isCrossRepository"].as_bool().unwrap_or(false),
        labels: n["labels"]["nodes"]
            .as_array()
            .map(|l| l.iter().map(|l| text(&l["name"])).collect())
            .unwrap_or_default(),
        ci_rollup: n["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"]
            .as_str()
            .map(str::to_string),
        comments_total: total(&n["comments"]),
        reactions: total(&n["reactions"]),
        listed_at: Some(now),
        ..Default::default()
    }
}

/// Applies what the shared ledger knows: drops PRs it has seen closed since they
/// were listed and notes who landed what. `true` if an open PR is newer there
/// than here, so the list has something to fetch.
pub fn apply_ledger(ledger: &PrLedger, d: &mut RepoData) -> bool {
    let mut changed = false;
    for (n, e) in &ledger.prs {
        let held = d.prs.get(n);
        if e.state == "OPEN" {
            changed |= held.is_none_or(|p| e.updated_at > p.updated_at);
        } else if !e.state.is_empty() && held.is_some_and(|p| e.updated_at >= p.updated_at) {
            d.prs.remove(n);
        }
    }
    d.history = History {
        merged: ledger.merged_authors(),
        cursor: ledger.backfill_cursor.clone(),
        complete: ledger.backfill_complete,
    };
    changed
}

/// The lowercase logins of a contributors/people.toml's `[people.<login>]` tables.
pub fn parse_people(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let inner = l.strip_prefix("[people.")?.strip_suffix(']')?;
            let inner = inner.trim().trim_matches('"');
            (!inner.is_empty()).then(|| inner.to_lowercase())
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn parse_collaborators(conn: &Value, roster: &mut Roster) {
    for e in conn["edges"].as_array().into_iter().flatten() {
        if let (Some(login), Some(perm)) = (e["node"]["login"].as_str(), e["permission"].as_str()) {
            roster
                .permissions
                .insert(login.to_lowercase(), perm.to_string());
        }
    }
}

/// A ref and what was fetched for it, or `None` if GitHub no longer has the PR.
pub type DetailOutcome = (DetailRef, Option<DetailResult>);

/// What a detail fetch learned, including the list fields it can refresh.
#[derive(Debug, Clone, PartialEq)]
pub struct DetailResult {
    pub detail: PrDetail,
    pub updated_at: Option<Time>,
    pub head_oid: String,
    pub mergeable: String,
    pub merge_state: String,
    pub review_decision: Option<String>,
    pub approved_by: Vec<String>,
    pub approved_on_head: Vec<String>,
    pub ci_rollup: Option<String>,
}

impl DetailResult {
    pub fn apply(self, pr: &mut Pr, now: Time) {
        pr.updated_at = self.updated_at;
        pr.head_oid = self.head_oid;
        pr.mergeable = self.mergeable;
        pr.merge_state = self.merge_state;
        pr.review_decision = self.review_decision;
        pr.approved_by = self.approved_by;
        pr.approved_on_head = self.approved_on_head;
        pr.ci_rollup = self.ci_rollup;
        pr.listed_at = Some(now);
        pr.detail = Some(self.detail);
        pr.detail_failed_at = None;
    }
}

pub fn parse_detail(n: &Value, now: Time) -> DetailResult {
    let updated_at = time(&n["updatedAt"]);
    let head_oid = text(&n["headRefOid"]);
    let rollup = &n["commits"]["nodes"][0]["commit"]["statusCheckRollup"];

    let mut checks = Vec::new();
    let mut rabbit = RabbitStatus::None;
    for c in rollup["contexts"]["nodes"].as_array().into_iter().flatten() {
        let check = if c["__typename"] == "CheckRun" {
            let state = c["conclusion"]
                .as_str()
                .or(c["status"].as_str())
                .unwrap_or("PENDING");
            Check {
                name: text(&c["name"]),
                state: state.to_string(),
                url: c["detailsUrl"].as_str().map(str::to_string),
            }
        } else {
            Check {
                name: text(&c["context"]),
                state: text(&c["state"]),
                url: c["targetUrl"].as_str().map(str::to_string),
            }
        };
        if check.name.eq_ignore_ascii_case("CodeRabbit") {
            rabbit = match check.state.as_str() {
                "SKIPPED" | "NEUTRAL" => RabbitStatus::Skipped,
                "SUCCESS" | "FAILURE" => RabbitStatus::Reviewed,
                _ => RabbitStatus::InProgress,
            };
        }
        checks.push(check);
    }

    let mut findings: Vec<Finding> = Vec::new();
    let mut human_threads_open = 0;
    let threads = &n["reviewThreads"];
    for t in threads["nodes"].as_array().into_iter().flatten() {
        let first = &t["comments"]["nodes"][0];
        let author = first["author"]["login"].as_str().unwrap_or_default();
        let resolved = t["isResolved"].as_bool().unwrap_or(false);
        let outdated = t["isOutdated"].as_bool().unwrap_or(false);
        if is_coderabbit(author) {
            let mut f = parse_finding(first["body"].as_str().unwrap_or_default());
            f.path = text(&t["path"]);
            f.line = t["line"].as_u64();
            f.url = text(&first["url"]);
            f.resolved = resolved;
            f.outdated = outdated;
            findings.push(f);
        } else if !resolved && !outdated && !author.is_empty() && !author.ends_with("[bot]") {
            human_threads_open += 1;
        }
    }

    let review_nodes = n["reviews"]["nodes"].as_array();
    let rabbit_actionable = review_nodes.and_then(|rs| {
        rs.iter()
            .rev()
            .filter(|r| is_coderabbit(r["author"]["login"].as_str().unwrap_or_default()))
            .find_map(|r| actionable_count(r["body"].as_str().unwrap_or_default()))
    });
    let reviews = review_nodes
        .map(|rs| {
            rs.iter()
                .filter(|r| !is_coderabbit(r["author"]["login"].as_str().unwrap_or_default()))
                .map(|r| Review {
                    author: r["author"]["login"].as_str().unwrap_or("ghost").to_string(),
                    state: text(&r["state"]),
                    at: time(&r["submittedAt"]),
                })
                .collect()
        })
        .unwrap_or_default();
    let comments = n["comments"]["nodes"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .filter(|c| {
                    let a = c["author"]["login"].as_str().unwrap_or_default();
                    !is_coderabbit(a) && !a.ends_with("[bot]")
                })
                .map(|c| Comment {
                    author: c["author"]["login"].as_str().unwrap_or("ghost").to_string(),
                    text: truncate(c["bodyText"].as_str().unwrap_or_default(), COMMENT_CHARS),
                    at: time(&c["createdAt"]),
                    url: text(&c["url"]),
                })
                .collect()
        })
        .unwrap_or_default();

    let detail = PrDetail {
        fetched_at: Some(now),
        for_updated_at: updated_at,
        for_head: head_oid.clone(),
        body: truncate(
            &strip_html_comments(n["body"].as_str().unwrap_or_default()),
            BODY_CHARS,
        ),
        files: n["files"]["nodes"]
            .as_array()
            .map(|fs| {
                fs.iter()
                    .map(|f| FileChange {
                        path: text(&f["path"]),
                        additions: f["additions"].as_u64().unwrap_or(0),
                        deletions: f["deletions"].as_u64().unwrap_or(0),
                        change: text(&f["changeType"]),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        files_total: total(&n["files"]),
        checks,
        rabbit,
        findings,
        threads_total: total(threads),
        rabbit_actionable,
        human_threads_open,
        reviews,
        comments,
        linked_issues: n["closingIssuesReferences"]["nodes"]
            .as_array()
            .map(|is| {
                is.iter()
                    .map(|i| LinkedIssue {
                        number: i["number"].as_u64().unwrap_or(0),
                        title: text(&i["title"]),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    DetailResult {
        detail,
        updated_at,
        head_oid,
        mergeable: text(&n["mergeable"]),
        merge_state: text(&n["mergeStateStatus"]),
        review_decision: n["reviewDecision"].as_str().map(str::to_string),
        approved_by: approvers_of(n),
        approved_on_head: approvers_on_head(n),
        ci_rollup: rollup["state"].as_str().map(str::to_string),
    }
}

/// Removes `<!-- … -->` so bots' markers and PR templates don't read as words.
pub fn strip_html_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

const LIST_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String, $first: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, first: $first, after: $after, orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        id number title url isDraft createdAt updatedAt additions deletions changedFiles
        mergeable mergeStateStatus reviewDecision authorAssociation
        headRefOid headRefName baseRefName isCrossRepository
        author { __typename login avatarUrl(size: 48) }
        labels(first: 10) { nodes { name } }
        comments { totalCount }
        reactions { totalCount }
        latestOpinionatedReviews(first: 10) { nodes { state commit { oid } author { login } } }
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      }
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

const ROSTER_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    people: object(expression: "HEAD:contributors/people.toml") { ... on Blob { text } }
    collaborators(first: 100, after: $after) {
      pageInfo { hasNextPage endCursor }
      edges { permission node { login } }
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

const DETAIL_QUERY: &str = r#"
query($ids: [ID!]!) {
  nodes(ids: $ids) {
    ... on PullRequest {
      updatedAt headRefOid body mergeable mergeStateStatus reviewDecision
      closingIssuesReferences(first: 5) { nodes { number title } }
      latestOpinionatedReviews(first: 10) { nodes { state commit { oid } author { login } } }
      files(first: 100) { totalCount nodes { path additions deletions changeType } }
      reviewThreads(first: 40) {
        totalCount
        nodes { isResolved isOutdated path line comments(first: 1) { nodes { author { login } body url } } }
      }
      reviews(last: 15) { nodes { author { login } state submittedAt body } }
      comments(last: 10) { nodes { author { login } bodyText createdAt url } }
      commits(last: 1) { nodes { commit { statusCheckRollup {
        state
        contexts(first: 60) { nodes {
          __typename
          ... on CheckRun { name conclusion status detailsUrl }
          ... on StatusContext { context state targetUrl }
        } }
      } } } }
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use pr_core::Severity;

    fn now() -> Time {
        "2026-10-31T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn parses_a_listed_pr() {
        let conn = json!({"nodes": [{
            "id": "PR_1", "number": 10521, "title": "Fix presets", "url": "u", "isDraft": false,
            "createdAt": "2026-10-30T00:00:00Z", "updatedAt": "2026-10-31T00:00:00Z",
            "additions": 49, "deletions": 4, "changedFiles": 2, "mergeable": "MERGEABLE",
            "mergeStateStatus": "UNSTABLE", "reviewDecision": null, "authorAssociation": "CONTRIBUTOR",
            "headRefOid": "abc", "headRefName": "fix", "baseRefName": "main", "isCrossRepository": true,
            "author": {"__typename": "User", "login": "wanikhawar", "avatarUrl": "https://a/x"},
            "labels": {"nodes": [{"name": "bug"}]}, "comments": {"totalCount": 3}, "reactions": {"totalCount": 1},
            "latestOpinionatedReviews": {"nodes": [
                {"state": "APPROVED", "author": {"login": "bflatastic"}, "commit": {"oid": "abc"}},
                {"state": "APPROVED", "author": {"login": "older"}, "commit": {"oid": "before"}},
                {"state": "CHANGES_REQUESTED", "author": {"login": "aiongg"}},
                {"state": "APPROVED", "author": null}
            ]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "PENDING"}}}]}
        }, {"number": 2, "author": null, "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]}}]});
        let prs = parse_list(&conn, now());
        let p = &prs[0];
        assert_eq!(
            (p.number, p.author.as_str(), p.association.as_str()),
            (10521, "wanikhawar", "CONTRIBUTOR")
        );
        assert_eq!(p.ci_rollup.as_deref(), Some("PENDING"));
        assert_eq!((p.lines(), p.comments_total, p.labels.len()), (53, 3, 1));
        assert_eq!(p.approved_by, ["bflatastic", "older"]);
        assert_eq!(
            p.approved_on_head,
            ["bflatastic"],
            "the other approved an older commit"
        );
        assert!(p.cross_repo && !p.author_is_bot);
        assert_eq!(
            (prs[1].author.as_str(), prs[1].ci_rollup.clone()),
            ("ghost", None)
        );
    }

    #[test]
    fn parses_detail_with_findings_checks_and_threads() {
        let pic = r#"<picture><img src="x" alt=""></picture>"#;
        let n = json!({
            "updatedAt": "2026-10-31T00:00:00Z", "headRefOid": "abc", "mergeable": "MERGEABLE",
            "mergeStateStatus": "CLEAN", "reviewDecision": "APPROVED",
            "body": "<!-- template -->Real words.<!-- bot -->",
            "closingIssuesReferences": {"nodes": [{"number": 7, "title": "Bug"}]},
            "files": {"totalCount": 1, "nodes": [{"path": "a.rs", "additions": 2, "deletions": 1, "changeType": "MODIFIED"}]},
            "reviewThreads": {"totalCount": 4, "nodes": [
                {"isResolved": false, "isOutdated": false, "path": "a.rs", "line": 12, "comments": {"nodes": [
                    {"author": {"login": "coderabbitai"}, "url": "t1",
                     "body": format!("**{pic} Major** · **Security & Privacy** · **Quick win**\n\n**Validate the path.**\n\nIt escapes.")}]}},
                {"isResolved": true, "isOutdated": false, "path": "a.rs", "line": 1, "comments": {"nodes": [
                    {"author": {"login": "coderabbitai"}, "url": "t2", "body": format!("**{pic} Minor** · **Functional Correctness** · **Quick win**\n\n**Nit.**")}]}},
                {"isResolved": false, "isOutdated": false, "path": "b.rs", "line": null, "comments": {"nodes": [
                    {"author": {"login": "FredApps"}, "url": "t3", "body": "Why this?"}]}},
                {"isResolved": false, "isOutdated": true, "path": "b.rs", "line": null, "comments": {"nodes": [
                    {"author": {"login": "FredApps"}, "url": "t4", "body": "Old"}]}}
            ]},
            "reviews": {"nodes": [
                {"author": {"login": "coderabbitai"}, "state": "COMMENTED", "submittedAt": "2026-10-30T00:00:00Z", "body": "**Actionable comments posted: 2**"},
                {"author": {"login": "bflatastic"}, "state": "APPROVED", "submittedAt": "2026-10-31T00:00:00Z", "body": ""}
            ]},
            "comments": {"nodes": [
                {"author": {"login": "coderabbitai"}, "bodyText": "Findings", "createdAt": "2026-10-30T00:00:00Z", "url": "c1"},
                {"author": {"login": "wanikhawar"}, "bodyText": "Rebased", "createdAt": "2026-10-30T01:00:00Z", "url": "c2"}
            ]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS", "contexts": {"nodes": [
                {"__typename": "CheckRun", "name": "test", "conclusion": "SUCCESS", "status": "COMPLETED", "detailsUrl": "d"},
                {"__typename": "CheckRun", "name": "CodeRabbit", "conclusion": "SUCCESS", "status": "COMPLETED", "detailsUrl": "e"},
                {"__typename": "CheckRun", "name": "slow", "conclusion": null, "status": "IN_PROGRESS", "detailsUrl": "f"},
                {"__typename": "StatusContext", "context": "legacy", "state": "FAILURE", "targetUrl": "g"}
            ]}}}}]}
        });
        let r = parse_detail(&n, now());
        let d = &r.detail;
        assert_eq!(d.body, "Real words.");
        assert_eq!(d.findings.len(), 2);
        let f = &d.findings[0];
        assert_eq!(
            (f.severity, f.security, f.title.as_str()),
            (Severity::Major, true, "Validate the path.")
        );
        assert_eq!(
            (f.path.as_str(), f.line, f.url.as_str()),
            ("a.rs", Some(12), "t1")
        );
        assert!(d.findings[1].resolved);
        assert_eq!(d.human_threads_open, 1);
        assert_eq!(d.rabbit, RabbitStatus::Reviewed);
        assert_eq!(d.rabbit_actionable, Some(2));
        assert_eq!(d.reviews.len(), 1);
        assert_eq!(d.comments[0].text, "Rebased");
        assert_eq!(d.linked_issues[0].number, 7);
        let states: Vec<&str> = d.checks.iter().map(|c| c.state.as_str()).collect();
        assert_eq!(states, ["SUCCESS", "SUCCESS", "IN_PROGRESS", "FAILURE"]);
        assert_eq!(r.ci_rollup.as_deref(), Some("SUCCESS"));
    }

    #[test]
    fn coderabbit_status_follows_its_check() {
        let with = |conclusion: Value, status: &str| {
            let n = json!({"commits": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": [
                {"__typename": "CheckRun", "name": "CodeRabbit", "conclusion": conclusion, "status": status}
            ]}}}}]}});
            parse_detail(&n, now()).detail.rabbit
        };
        assert_eq!(with(Value::Null, "IN_PROGRESS"), RabbitStatus::InProgress);
        assert_eq!(with(json!("SKIPPED"), "COMPLETED"), RabbitStatus::Skipped);
        assert_eq!(with(json!("SUCCESS"), "COMPLETED"), RabbitStatus::Reviewed);
    }

    #[test]
    fn a_detail_result_makes_the_pr_current() {
        let mut p = Pr {
            number: 1,
            head_oid: "old".into(),
            ..Default::default()
        };
        let n = json!({"updatedAt": "2026-10-31T00:00:00Z", "headRefOid": "new", "mergeable": "CONFLICTING"});
        parse_detail(&n, now()).apply(&mut p, now());
        assert_eq!(
            (p.head_oid.as_str(), p.mergeable.as_str()),
            ("new", "CONFLICTING")
        );
        assert!(p.current_detail().is_some());
    }

    #[test]
    fn the_ledger_drops_closed_prs_and_says_whether_to_list() {
        let t = |s: &str| s.parse::<Time>().unwrap();
        let entry = |state: &str, at: &str, who: &str| craft_core::LedgerPr {
            state: state.into(),
            updated_at: Some(t(at)),
            author: Some(who.into()),
            ..Default::default()
        };
        let held = |n: u64, at: &str| Pr {
            number: n,
            updated_at: Some(t(at)),
            ..Default::default()
        };
        let mut d = RepoData::default();
        d.prs.insert(1, held(1, "2026-10-30T00:00:00Z"));
        d.prs.insert(2, held(2, "2026-10-30T00:00:00Z"));
        d.prs.insert(3, held(3, "2026-10-30T00:00:00Z"));
        d.prs.insert(4, held(4, "2026-10-31T00:00:00Z"));
        let mut l = PrLedger {
            backfill_complete: true,
            ..Default::default()
        };
        l.record(1, entry("MERGED", "2026-10-31T00:00:00Z", "ann"));
        l.record(2, entry("CLOSED", "2026-10-31T00:00:00Z", "bo"));
        l.record(3, entry("OPEN", "2026-10-30T00:00:00Z", "cy"));
        // Reopened after the ledger last saw it closed: the list knows better.
        l.record(4, entry("CLOSED", "2026-10-30T00:00:00Z", "di"));
        assert!(!apply_ledger(&l, &mut d), "nothing open is newer");
        assert_eq!(d.prs.keys().copied().collect::<Vec<_>>(), [3, 4]);
        assert_eq!(d.history.merged[&1], "ann");
        assert!(d.history.complete);

        l.record(3, entry("OPEN", "2026-10-31T00:00:00Z", "cy"));
        assert!(apply_ledger(&l, &mut d), "an update to fetch");
        l.record(9, entry("OPEN", "2026-10-01T00:00:00Z", "new"));
        d.prs.get_mut(&3).unwrap().updated_at = Some(t("2026-10-31T00:00:00Z"));
        assert!(apply_ledger(&l, &mut d), "a PR not listed yet");
    }

    #[test]
    fn roster_parsing() {
        let toml = "# [people.your-github-username]\n[people.Echelon]\nreal_name = \"x\"\n\n[people.\"dotted.name\"]\n[other]\n";
        assert_eq!(parse_people(toml), ["dotted.name", "echelon"]);
        let mut r = Roster::default();
        parse_collaborators(
            &json!({"edges": [{"permission": "ADMIN", "node": {"login": "Echelon"}}]}),
            &mut r,
        );
        assert_eq!(r.permissions["echelon"], "ADMIN");
        parse_collaborators(&Value::Null, &mut r);
        assert_eq!(r.permissions.len(), 1);
    }

    #[test]
    fn html_comments_are_stripped() {
        assert_eq!(strip_html_comments("a<!-- b -->c<!-- d"), "ac");
        assert_eq!(strip_html_comments("  plain  "), "plain");
    }
}
