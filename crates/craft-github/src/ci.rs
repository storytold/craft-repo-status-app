//! The latest result of each GitHub Actions workflow on a repository's default
//! branch ([`GitHub::main_ci`]), for the Repos table's Main CI column.
//!
//! Each read lists the repository's workflows, then the completed runs of
//! each active workflow on the branch, [`RUNS_PER_PAGE`] at a time, until the
//! pages hold its result or reach [`KEEP_DAYS`] back, up to [`RUN_PAGES`]
//! pages. Every page read last time is requested again with the ETag GitHub
//! sent for it: when all of a list's pages come back as 304 Not Modified,
//! which does not count against GitHub's primary rate limit, the result
//! read from them stands. Otherwise the list is read again from its first
//! page and its result is worked out afresh.
//!
//! A workflow's result is its newest completed run that was not canceled or
//! skipped, together with every other such run of the workflow on the same
//! commit and event created within [`GROUP_SECS`] seconds of it. Runs started
//! by pull requests, and runs from other repositories, are left out. Only
//! active workflows get a result, labeled with the workflow's own name, and a
//! result older than [`KEEP_DAYS`] days is dropped.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use craft_core::{MainCi, MainRun, MainWorkflow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{time, Conditional, GitHub, Result};

/// Completed runs one request lists.
pub const RUNS_PER_PAGE: usize = 20;
/// Pages of one workflow's runs read while none holds its result.
pub const RUN_PAGES: u32 = 10;
/// Workflows one request lists, the most GitHub allows.
const WORKFLOWS_PER_PAGE: usize = 100;
/// Pages of the workflow list read at most.
const WORKFLOW_PAGES: u32 = 10;
/// A workflow's result is dropped once its newest run is this many days old.
pub const KEEP_DAYS: i64 = 30;
/// Runs of one workflow on the same commit and event that were created within
/// this many seconds of the newest one form one result.
pub const GROUP_SECS: i64 = 60;
/// Conclusions that are not a result: the run was stopped or ran nothing.
const NOT_A_RESULT: &[&str] = &["cancelled", "skipped"];
/// Events whose runs test a pull request's branch, not the default branch,
/// even when that branch shares its name.
const PULL_REQUEST_EVENTS: &[&str] = &["pull_request", "pull_request_target"];

/// What the last read of a repository's workflow runs returned: the ETags to
/// send next time and the results they stand for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CachedRuns {
    /// The branch the runs were listed for.
    pub branch: String,
    /// The ETag of each page of the workflow list read last time.
    pub workflows_etags: Vec<String>,
    /// The repository's active workflows.
    pub workflows: Vec<Workflow>,
    /// The ETag of each page of a workflow's runs read last time, by workflow
    /// id.
    pub runs_etags: BTreeMap<u64, Vec<String>>,
    /// Each workflow's latest result, by workflow id.
    pub results: BTreeMap<u64, MainWorkflow>,
}

/// An active workflow from the repository's workflow list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workflow {
    pub id: u64,
    pub name: String,
    pub path: String,
}

/// What [`parse_runs`] found in a runs list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunsPage {
    /// Each workflow's latest result, by workflow id.
    pub results: BTreeMap<u64, MainWorkflow>,
    /// The workflows whose result may include runs past the end of the list.
    pub cut: BTreeSet<u64>,
    /// When the oldest run in the list was created.
    pub oldest: Option<DateTime<Utc>>,
}

/// The pages of a list read afresh, with the ETag of each.
#[derive(Debug, Default)]
struct Pages {
    bodies: Vec<Value>,
    etags: Vec<String>,
}

impl GitHub {
    /// The latest result of each active workflow on `branch` of `repo`, read
    /// into `cache`. The requests carry the ETags `cache` holds for the same
    /// branch; a failed request leaves `cache` as it was.
    pub fn main_ci(
        &self,
        repo: &str,
        branch: &str,
        cache: &mut Option<CachedRuns>,
        now: DateTime<Utc>,
    ) -> Result<MainCi> {
        let held = cache.take().filter(|c| c.branch == branch);
        let mut next = held.clone().unwrap_or_else(|| CachedRuns {
            branch: branch.to_string(),
            ..Default::default()
        });
        let cutoff = now - chrono::Duration::days(KEEP_DAYS);
        if let Err(e) = self.read_runs(repo, &mut next, cutoff) {
            *cache = held;
            return Err(e);
        }
        for w in &next.workflows {
            if let Some(r) = next.results.get_mut(&w.id) {
                r.name.clone_from(&w.name);
                r.path.clone_from(&w.path);
            }
        }
        let mut workflows: Vec<MainWorkflow> = next
            .results
            .values()
            .filter(|w| newest_at(w).is_none_or(|at| at >= cutoff))
            .cloned()
            .collect();
        workflows.sort_by(|a, b| {
            (a.name.to_lowercase(), a.workflow_id).cmp(&(b.name.to_lowercase(), b.workflow_id))
        });
        *cache = Some(next);
        Ok(MainCi {
            branch: branch.to_string(),
            workflows,
            checked_at: Some(now),
        })
    }

    /// Brings `c` up to date: the workflow list, then each active workflow's
    /// runs back to `cutoff` at most.
    fn read_runs(&self, repo: &str, c: &mut CachedRuns, cutoff: DateTime<Utc>) -> Result<()> {
        let base = format!("{}/repos/{repo}/actions", self.api);
        let list = format!("{base}/workflows?per_page={WORKFLOWS_PER_PAGE}");
        let more_workflows = |bodies: &[Value]| {
            let last = bodies.last().and_then(|b| b["workflows"].as_array());
            last.is_some_and(|l| l.len() >= WORKFLOWS_PER_PAGE)
        };
        let read = self.read_pages(&list, &c.workflows_etags, WORKFLOW_PAGES, more_workflows)?;
        if let Some(pages) = read {
            c.workflows = pages.bodies.iter().flat_map(parse_workflows).collect();
            c.workflows_etags = pages.etags;
        }
        let active: BTreeSet<u64> = c.workflows.iter().map(|w| w.id).collect();
        c.results.retain(|id, _| active.contains(id));
        c.runs_etags.retain(|id, _| active.contains(id));
        let query = format!(
            "branch={}&status=completed&exclude_pull_requests=true&per_page={RUNS_PER_PAGE}",
            encode(&c.branch)
        );
        for id in active {
            let url = format!("{base}/workflows/{id}/runs?{query}");
            // Read another page while the result is missing or may go on, the
            // last page was full, and it reached no further back than `cutoff`.
            let more_runs = |bodies: &[Value]| {
                let last = bodies.last().map(|b| runs_in(std::slice::from_ref(b)));
                if last.is_none_or(|l| l.len() < RUNS_PER_PAGE) {
                    return false;
                }
                let found = parse_runs(&runs_in(bodies), repo, true);
                let done = found.results.contains_key(&id) && !found.cut.contains(&id);
                !done && found.oldest.is_some_and(|t| t >= cutoff)
            };
            let held = c.runs_etags.get(&id).map_or(&[][..], Vec::as_slice);
            let Some(pages) = self.read_pages(&url, held, RUN_PAGES, more_runs)? else {
                continue;
            };
            let listed = runs_in(&pages.bodies);
            let full = pages
                .bodies
                .last()
                .is_some_and(|b| runs_in(std::slice::from_ref(b)).len() >= RUNS_PER_PAGE);
            match parse_runs(&listed, repo, full).results.remove(&id) {
                Some(w) => c.results.insert(id, w),
                None => c.results.remove(&id),
            };
            c.runs_etags.insert(id, pages.etags);
        }
        Ok(())
    }

    /// The pages of the list at `url` (`&page=N` added after the first), read
    /// until `more` says the pages so far are enough, or `max` pages are read.
    /// `held` holds the ETags of the pages read last time: when each of those
    /// pages is unchanged, nothing is read again and the answer is `None`.
    fn read_pages(
        &self,
        url: &str,
        held: &[String],
        max: u32,
        more: impl Fn(&[Value]) -> bool,
    ) -> Result<Option<Pages>> {
        let page_url = |n: usize| match n {
            1 => url.to_string(),
            n => format!("{url}&page={n}"),
        };
        let mut first = None;
        if !held.is_empty() && held.iter().all(|e| !e.is_empty()) {
            let mut unchanged = true;
            for (i, etag) in held.iter().enumerate() {
                match self.get_conditional(&page_url(i + 1), Some(etag))? {
                    Conditional::NotModified => {}
                    answer => {
                        if i == 0 {
                            first = Some(answer);
                        }
                        unchanged = false;
                        break;
                    }
                }
            }
            if unchanged {
                return Ok(None);
            }
        }
        let mut pages = Pages::default();
        for n in 1..=max as usize {
            let answer = match first.take() {
                Some(answer) => answer,
                None => self.get_conditional(&page_url(n), None)?,
            };
            let Conditional::Fresh { etag, body } = answer else {
                break;
            };
            pages.etags.push(etag.unwrap_or_default());
            pages.bodies.push(body);
            if !more(&pages.bodies) {
                break;
            }
        }
        Ok(Some(pages))
    }
}

/// The runs of every page in `bodies` (`workflow_runs`), in order.
fn runs_in(bodies: &[Value]) -> Vec<Value> {
    bodies
        .iter()
        .filter_map(|b| b["workflow_runs"].as_array())
        .flatten()
        .cloned()
        .collect()
}

/// The active workflows in a page of the workflow list (`workflows`).
/// Disabled workflows are left out.
pub fn parse_workflows(body: &Value) -> Vec<Workflow> {
    body["workflows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["state"].as_str() == Some("active"))
        .filter_map(|w| {
            Some(Workflow {
                id: w["id"].as_u64()?,
                name: w["name"].as_str().unwrap_or_default().to_string(),
                path: w["path"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

/// A run, and when it was created.
type CreatedRun = (MainRun, Option<DateTime<Utc>>);

/// The latest result of each workflow in a runs list (`listed`, newest
/// first): its newest run by run id, with its other runs on the same commit
/// and event created within [`GROUP_SECS`] of it, leaving out canceled and
/// skipped runs, runs started by a pull request, and runs from another
/// repository. `full` says the list may go on past its last run. Each result
/// is labeled with its newest run's name until [`GitHub::main_ci`] gives it
/// the workflow's own.
pub fn parse_runs(listed: &[Value], repo: &str, full: bool) -> RunsPage {
    let created = |run: &Value| time(&run["created_at"]);
    let mut found: BTreeMap<u64, Vec<CreatedRun>> = BTreeMap::new();
    for run in listed {
        let str_of = |k: &str| run[k].as_str().unwrap_or_default().to_string();
        let (Some(run_id), Some(id)) = (run["id"].as_u64(), run["workflow_id"].as_u64()) else {
            continue;
        };
        let Some(conclusion) = run["conclusion"].as_str() else {
            continue;
        };
        let event = str_of("event");
        let from = run["head_repository"]["full_name"].as_str();
        if NOT_A_RESULT.contains(&conclusion)
            || PULL_REQUEST_EVENTS.contains(&event.as_str())
            || from.is_some_and(|f| !f.eq_ignore_ascii_case(repo))
        {
            continue;
        }
        let found_run = MainRun {
            run_id,
            conclusion: conclusion.to_string(),
            event,
            head_sha: str_of("head_sha"),
            title: run["display_title"]
                .as_str()
                .map_or_else(|| str_of("name"), str::to_string),
            at: time(&run["updated_at"]).or_else(|| time(&run["run_started_at"])),
            url: str_of("html_url"),
        };
        found.entry(id).or_default().push((found_run, created(run)));
    }
    let oldest = listed.iter().filter_map(created).min();
    let near_end = |t: Option<DateTime<Utc>>| {
        full && oldest.is_some_and(|o| t.is_none_or(|t| (t - o).num_seconds() <= GROUP_SECS))
    };
    let mut page = RunsPage {
        oldest,
        ..Default::default()
    };
    let named = |id: u64| {
        let run = listed
            .iter()
            .find(|r| r["workflow_id"].as_u64() == Some(id));
        let str_of = |k: &str| {
            run.and_then(|r| r[k].as_str())
                .unwrap_or_default()
                .to_string()
        };
        (str_of("name"), str_of("path"))
    };
    for (id, mut runs) in found {
        runs.sort_by_key(|(r, _)| Reverse(r.run_id));
        let Some((newest, newest_created)) = runs.first().cloned() else {
            continue;
        };
        runs.retain(|(r, at)| {
            r.head_sha == newest.head_sha
                && r.event == newest.event
                && match (at, newest_created) {
                    (Some(at), Some(n)) => (n - *at).num_seconds().abs() <= GROUP_SECS,
                    _ => r.run_id == newest.run_id,
                }
        });
        if runs.last().is_some_and(|(_, at)| near_end(*at)) {
            page.cut.insert(id);
        }
        let (name, path) = named(id);
        page.results.insert(
            id,
            MainWorkflow {
                workflow_id: id,
                name,
                path,
                runs: runs.into_iter().map(|(r, _)| r).collect(),
            },
        );
    }
    page
}

fn newest_at(w: &MainWorkflow) -> Option<DateTime<Utc>> {
    w.runs.iter().filter_map(|r| r.at).max()
}

/// Drops the results in `ci` whose newest run is more than [`KEEP_DAYS`]
/// days older than `now`, as [`GitHub::main_ci`] does on a read.
pub fn drop_expired(ci: &mut MainCi, now: DateTime<Utc>) {
    let cutoff = now - chrono::Duration::days(KEEP_DAYS);
    ci.workflows
        .retain(|w| newest_at(w).is_none_or(|at| at >= cutoff));
}

/// `s` for a URL query value: letters, digits and `-._~/` as they are, every
/// other byte percent-encoded.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::stub;
    use serde_json::json;
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    const NOT_MODIFIED: &str =
        "HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const QUERY: &str = "?branch=main&status=completed&exclude_pull_requests=true&per_page=20";
    const LIST_URL: &str = "/repos/o/app/actions/workflows?per_page=100";

    /// `short` (`10-17T09:00`) as a time in 2026.
    fn stamp(short: &str) -> String {
        format!("2026-{short}:00Z")
    }

    fn t(short: &str) -> DateTime<Utc> {
        stamp(short).parse().unwrap()
    }

    fn ok(body: &Value, etag: &str) -> String {
        let body = body.to_string();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: {etag}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// A workflow list holding `active` workflows, as `(id, name)`.
    fn workflows(active: &[(u64, &str)]) -> Value {
        let list: Vec<Value> = active
            .iter()
            .map(|(id, name)| {
                json!({"id": id, "name": name, "path": format!(".github/workflows/w{id}.yml"), "state": "active"})
            })
            .collect();
        json!({"total_count": list.len(), "workflows": list})
    }

    /// A run of workflow `wf` created and finished at `at` (see [`stamp`]).
    fn run(id: u64, wf: u64, conclusion: Option<&str>, event: &str, at: &str) -> Value {
        json!({
            "id": id, "workflow_id": wf, "name": format!("Workflow {wf}"), "display_title": format!("Commit {id}"),
            "path": format!(".github/workflows/w{wf}.yml"), "event": event, "status": "completed",
            "conclusion": conclusion, "head_sha": format!("sha{id}"), "created_at": stamp(at), "updated_at": stamp(at),
            "html_url": format!("https://github.com/o/app/actions/runs/{id}"),
            "head_repository": {"full_name": "o/app"},
        })
    }

    fn runs(list: Vec<Value>) -> Value {
        json!({"total_count": list.len(), "workflow_runs": list})
    }

    /// `count` runs of workflow `wf` that ended as `conclusion`, newest
    /// first, with run ids from `top` down, a minute apart from `at` back.
    fn many(count: u64, top: u64, wf: u64, conclusion: &str, at: &str) -> Vec<Value> {
        (0..count)
            .map(|i| {
                let mut r = run(top - i, wf, Some(conclusion), "push", at);
                let when = t(at) - chrono::Duration::minutes(i as i64);
                let when = when.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                r["created_at"] = json!(when);
                r["updated_at"] = json!(when);
                r
            })
            .collect()
    }

    /// `(workflow id, [(run id, conclusion)])` per result.
    fn summary(ci: &MainCi) -> Vec<(u64, Vec<(u64, String)>)> {
        ci.workflows
            .iter()
            .map(|w| {
                let runs = w.runs.iter().map(|r| (r.run_id, r.conclusion.clone()));
                (w.workflow_id, runs.collect())
            })
            .collect()
    }

    /// The request targets and `If-None-Match` values of the next `n`
    /// requests the stub received.
    fn requested(requests: &Receiver<String>, n: usize) -> Vec<(String, Option<String>)> {
        (0..n)
            .map(|_| {
                let head = requests.recv_timeout(Duration::from_secs(10)).unwrap();
                let target = head.split(' ').nth(1).unwrap_or_default().to_string();
                let etag = head
                    .lines()
                    .find_map(|l| l.strip_prefix("if-none-match: "))
                    .map(str::to_string);
                (target, etag)
            })
            .collect()
    }

    fn runs_url(wf: u64) -> String {
        format!("/repos/o/app/actions/workflows/{wf}/runs{QUERY}")
    }

    fn list_url() -> String {
        LIST_URL.to_string()
    }

    fn etag(e: &str) -> Option<String> {
        Some(e.to_string())
    }

    #[test]
    fn each_workflow_keeps_its_newest_real_result() {
        let mut fork = run(9, 4, Some("failure"), "push", "10-17T09:00");
        fork["head_repository"]["full_name"] = json!("someone/app");
        let listed = [
            run(7, 1, Some("cancelled"), "push", "10-17T10:00"),
            run(6, 1, Some("failure"), "push", "10-17T09:30"),
            run(5, 1, Some("success"), "push", "10-17T09:00"),
            run(4, 2, Some("skipped"), "schedule", "10-17T08:30"),
            run(3, 2, Some("success"), "schedule", "10-17T08:00"),
            run(2, 3, Some("failure"), "pull_request", "10-17T07:00"),
            run(1, 5, None, "push", "10-17T06:00"),
            fork,
            // Listed out of order, and finished before run 3: the newer run still wins.
            run(8, 2, Some("failure"), "schedule", "10-17T07:30"),
        ];
        let page = parse_runs(&listed, "O/App", false);
        let got: Vec<(u64, Vec<u64>, &str)> = page
            .results
            .values()
            .map(|w| {
                let ids = w.runs.iter().map(|r| r.run_id).collect();
                (w.workflow_id, ids, w.runs[0].conclusion.as_str())
            })
            .collect();
        assert_eq!(
            got,
            [(1, vec![6], "failure"), (2, vec![8], "failure")],
            "canceled and skipped runs, pull request runs, unfinished runs and other repositories' runs are left out"
        );
        let r = &page.results[&1].runs[0];
        assert_eq!((r.event.as_str(), r.head_sha.as_str()), ("push", "sha6"));
        assert_eq!(r.title, "Commit 6");
        assert_eq!(r.url, "https://github.com/o/app/actions/runs/6");
        assert_eq!(r.at, Some(t("10-17T09:30")));
        assert_eq!(page.results[&1].path, ".github/workflows/w1.yml");
        assert!(page.cut.is_empty());
        assert_eq!(page.oldest, Some(t("10-17T06:00")));
    }

    #[test]
    fn main_ci_sends_the_etags_and_reuses_the_results_on_304() {
        let ci_runs = runs(vec![run(2, 1, Some("failure"), "push", "10-17T09:00")]);
        let nightly = runs(vec![run(1, 2, Some("success"), "schedule", "09-01T09:00")]);
        let fixed = runs(vec![run(3, 1, Some("success"), "push", "10-17T10:00")]);
        let (gh, requests) = stub(vec![
            ok(&workflows(&[(1, "CI"), (2, "Nightly")]), "W/\"l\""),
            ok(&ci_runs, "W/\"c\""),
            ok(&nightly, "W/\"n\""),
            NOT_MODIFIED.into(),
            NOT_MODIFIED.into(),
            NOT_MODIFIED.into(),
            NOT_MODIFIED.into(),
            ok(&fixed, "W/\"c2\""),
            NOT_MODIFIED.into(),
        ]);
        let now = t("10-17T11:00");
        let mut cache = None;
        let ci = gh.main_ci("o/app", "main", &mut cache, now).unwrap();
        assert_eq!(
            summary(&ci),
            [(1, vec![(2, "failure".into())])],
            "a result older than the keep limit is not shown"
        );
        assert_eq!((ci.checked_at, ci.branch.as_str()), (Some(now), "main"));
        assert_eq!(ci.workflows[0].name, "CI");
        let again = gh.main_ci("o/app", "main", &mut cache, now).unwrap();
        assert_eq!(summary(&again), summary(&ci), "304: the held results");
        let third = gh.main_ci("o/app", "main", &mut cache, now).unwrap();
        assert_eq!(summary(&third), [(1, vec![(3, "success".into())])]);
        assert_eq!(
            requested(&requests, 9),
            [
                (list_url(), None),
                (runs_url(1), None),
                (runs_url(2), None),
                (list_url(), etag("w/\"l\"")),
                (runs_url(1), etag("w/\"c\"")),
                (runs_url(2), etag("w/\"n\"")),
                (list_url(), etag("w/\"l\"")),
                (runs_url(1), etag("w/\"c\"")),
                (runs_url(2), etag("w/\"n\"")),
            ]
        );
        assert_eq!(cache.unwrap().runs_etags[&1], ["W/\"c2\""]);
    }

    #[test]
    fn an_unchanged_repo_sends_one_request_per_list_answered_with_304() {
        let ci_runs = runs(vec![run(5, 1, Some("success"), "push", "10-17T09:00")]);
        let weekly = runs(vec![run(2, 2, Some("failure"), "schedule", "10-12T06:00")]);
        let (gh, requests) = stub(vec![
            ok(&workflows(&[(1, "CI"), (2, "Weekly")]), "W/\"l\""),
            ok(&ci_runs, "W/\"c\""),
            ok(&weekly, "W/\"w\""),
            NOT_MODIFIED.into(),
            NOT_MODIFIED.into(),
            NOT_MODIFIED.into(),
        ]);
        let now = t("10-17T11:00");
        let mut cache = None;
        let first = gh.main_ci("o/app", "main", &mut cache, now).unwrap();
        requested(&requests, 3);
        let again = gh.main_ci("o/app", "main", &mut cache, now).unwrap();
        assert_eq!(summary(&again), summary(&first));
        assert_eq!(requested(&requests, 3).len(), 3);
        assert!(
            requests.recv_timeout(Duration::from_millis(300)).is_err(),
            "nothing else"
        );
    }

    #[test]
    fn a_workflow_behind_a_run_of_canceled_runs_still_shows_its_newest_result() {
        // Pushes in quick succession canceled FreeBSD 21 times, more than a page.
        let mut freebsd = many(21, 200, 2, "cancelled", "10-18T17:30");
        freebsd.push(run(150, 2, Some("failure"), "push", "10-18T14:26"));
        let nix = runs(vec![
            run(140, 1, Some("cancelled"), "push", "10-18T17:10"),
            run(130, 1, Some("success"), "push", "10-18T17:07"),
        ]);
        let second_page = freebsd.split_off(RUNS_PER_PAGE);
        let (gh, requests) = stub(vec![
            ok(&workflows(&[(1, "Nix"), (2, "FreeBSD")]), "W/\"l\""),
            ok(&nix, "W/\"n\""),
            ok(&runs(freebsd), "W/\"f1\""),
            ok(&runs(second_page), "W/\"f2\""),
        ]);
        let mut cache = None;
        let ci = gh
            .main_ci("o/app", "main", &mut cache, t("10-18T20:00"))
            .unwrap();
        let freebsd_first = (2, vec![(150, "failure".into())]);
        let nix_second = (1, vec![(130, "success".into())]);
        assert_eq!(summary(&ci), [freebsd_first, nix_second]);
        let got = requested(&requests, 4);
        assert_eq!(got[3], (format!("{}&page=2", runs_url(2)), None));
        assert_eq!(cache.unwrap().runs_etags[&2], ["W/\"f1\"", "W/\"f2\""]);
    }

    #[test]
    fn a_workflow_that_ran_long_ago_still_gets_its_latest_run() {
        let ci_runs = runs(vec![run(90, 1, Some("success"), "push", "10-18T17:00")]);
        let freebsd = runs(vec![run(12, 3, Some("failure"), "push", "10-07T07:48")]);
        let (gh, _requests) = stub(vec![
            ok(&workflows(&[(1, "CI"), (3, "FreeBSD")]), "W/\"l\""),
            ok(&ci_runs, "W/\"c\""),
            ok(&freebsd, "W/\"f\""),
        ]);
        let mut cache = None;
        let ci = gh
            .main_ci("o/app", "main", &mut cache, t("10-18T20:00"))
            .unwrap();
        let ci_row = (1, vec![(90, "success".into())]);
        let freebsd_row = (3, vec![(12, "failure".into())]);
        assert_eq!(summary(&ci), [ci_row, freebsd_row], "an 11-day-old failure");
    }

    #[test]
    fn a_dynamic_workflow_is_labeled_by_the_workflow_name() {
        let list = json!({"total_count": 2, "workflows": [
            {"id": 9, "name": "Dependabot Updates", "path": "dynamic/dependabot/dependabot-updates", "state": "active"},
            {"id": 4, "name": "Old", "path": ".github/workflows/old.yml", "state": "disabled_manually"},
        ]});
        let title = "github_actions in /. - Update #1634566446";
        let mut update = run(70, 9, Some("success"), "dynamic", "10-18T18:32");
        update["name"] = json!(title);
        update["display_title"] = json!(title);
        update["path"] = json!("dynamic/dependabot/dependabot-updates");
        let (gh, requests) = stub(vec![
            ok(&list, "W/\"l\""),
            ok(&runs(vec![update]), "W/\"d\""),
        ]);
        let mut cache = None;
        let ci = gh
            .main_ci("o/app", "main", &mut cache, t("10-18T20:00"))
            .unwrap();
        let w = &ci.workflows[0];
        assert_eq!(
            (w.name.as_str(), w.runs[0].title.as_str()),
            ("Dependabot Updates", title)
        );
        assert_eq!(w.path, "dynamic/dependabot/dependabot-updates");
        let got = requested(&requests, 2);
        assert_eq!(
            got[1].0,
            runs_url(9),
            "a disabled workflow's runs are not read"
        );
    }

    #[test]
    fn runs_of_one_workflow_started_together_on_the_newest_commit_count_together() {
        let mut listed = vec![
            run(301, 9, Some("success"), "dynamic", "10-18T18:32"),
            run(300, 9, Some("failure"), "dynamic", "10-18T18:39"),
            run(250, 9, Some("success"), "dynamic", "10-18T12:00"),
            run(240, 9, Some("success"), "schedule", "10-18T11:00"),
            run(220, 9, Some("failure"), "dynamic", "10-18T10:00"),
        ];
        for i in [0, 1, 3, 4] {
            listed[i]["head_sha"] = json!("0d34a58b");
        }
        for i in [0, 1] {
            listed[i]["created_at"] = json!(stamp("10-18T18:32"));
        }
        let page = parse_runs(&listed, "o/app", false);
        let ids: Vec<(u64, &str)> = page.results[&9]
            .runs
            .iter()
            .map(|r| (r.run_id, r.conclusion.as_str()))
            .collect();
        assert_eq!(
            ids,
            [(301, "success"), (300, "failure")],
            "an older commit, another event, or a run started hours earlier is not part of the result"
        );
    }

    #[test]
    fn a_group_cut_by_the_end_of_a_page_is_read_to_its_end() {
        let mut first = many(19, 400, 9, "cancelled", "10-18T19:00");
        let mut newer = run(301, 9, Some("success"), "dynamic", "10-18T17:00");
        let mut older = run(300, 9, Some("failure"), "dynamic", "10-18T17:05");
        for r in [&mut newer, &mut older] {
            r["head_sha"] = json!("0d34a58b");
            r["created_at"] = json!(stamp("10-18T17:00"));
        }
        first.push(newer);
        let (gh, _requests) = stub(vec![
            ok(&workflows(&[(9, "Dependabot Updates")]), "W/\"l\""),
            ok(&runs(first), "W/\"d1\""),
            ok(&runs(vec![older]), "W/\"d2\""),
        ]);
        let mut cache = None;
        let ci = gh
            .main_ci("o/app", "main", &mut cache, t("10-18T20:00"))
            .unwrap();
        let both = vec![(301, "success".into()), (300, "failure".into())];
        assert_eq!(summary(&ci), [(9, both)]);
    }

    #[test]
    fn a_change_on_a_later_page_reads_the_list_again() {
        let page_one = many(20, 400, 1, "cancelled", "10-18T19:00");
        let page_two = vec![run(300, 1, Some("failure"), "push", "10-18T17:00")];
        // Run 300 was re-run, and its second attempt was canceled.
        let mut rerun = page_two[0].clone();
        rerun["conclusion"] = json!("cancelled");
        let older = run(290, 1, Some("success"), "push", "10-18T16:00");
        let (gh, requests) = stub(vec![
            ok(&workflows(&[(1, "CI")]), "W/\"l\""),
            ok(&runs(page_one.clone()), "W/\"p1\""),
            ok(&runs(page_two), "W/\"p2\""),
            NOT_MODIFIED.into(),
            NOT_MODIFIED.into(),
            ok(&runs(vec![rerun.clone(), older.clone()]), "W/\"p2b\""),
            ok(&runs(page_one), "W/\"p1\""),
            ok(&runs(vec![rerun, older]), "W/\"p2b\""),
        ]);
        let now = t("10-18T20:00");
        let mut cache = None;
        let ci = gh.main_ci("o/app", "main", &mut cache, now).unwrap();
        assert_eq!(summary(&ci), [(1, vec![(300, "failure".into())])]);
        let again = gh.main_ci("o/app", "main", &mut cache, now).unwrap();
        assert_eq!(
            summary(&again),
            [(1, vec![(290, "success".into())])],
            "the canceled re-run leaves the result, and the run before it takes its place"
        );
        let page_two_url = format!("{}&page=2", runs_url(1));
        assert_eq!(
            requested(&requests, 8)[3..],
            [
                (list_url(), etag("w/\"l\"")),
                (runs_url(1), etag("w/\"p1\"")),
                (page_two_url.clone(), etag("w/\"p2\"")),
                (runs_url(1), None),
                (page_two_url, None),
            ]
        );
    }

    #[test]
    fn a_workflow_list_longer_than_a_page_is_read_to_its_end() {
        let names: Vec<String> = (1..=100).map(|id| format!("W{id}")).collect();
        let first: Vec<(u64, &str)> = (1..).zip(names.iter().map(String::as_str)).collect();
        let mut list = workflows(&first);
        for w in list["workflows"].as_array_mut().unwrap() {
            w["state"] = json!("disabled_manually");
        }
        let late = runs(vec![run(5, 101, Some("failure"), "push", "10-18T17:00")]);
        let (gh, requests) = stub(vec![
            ok(&list, "W/\"l1\""),
            ok(&workflows(&[(101, "Late")]), "W/\"l2\""),
            ok(&late, "W/\"r\""),
        ]);
        let mut cache = None;
        let ci = gh
            .main_ci("o/app", "main", &mut cache, t("10-18T20:00"))
            .unwrap();
        assert_eq!(summary(&ci), [(101, vec![(5, "failure".into())])]);
        let got = requested(&requests, 3);
        assert_eq!(got[1], (format!("{LIST_URL}&page=2"), None));
    }

    #[test]
    fn a_failed_request_keeps_the_cache_and_another_branch_starts_without_its_etag() {
        let (gh, requests) = stub(vec![
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
                .to_string(),
            ok(&workflows(&[(1, "CI")]), "W/\"l\""),
            ok(&runs(vec![]), "W/\"t\""),
        ]);
        let held = CachedRuns {
            branch: "main".into(),
            workflows_etags: vec!["W/\"a\"".into()],
            ..Default::default()
        };
        let mut cache = Some(held.clone());
        assert!(gh.main_ci("o/app", "main", &mut cache, Utc::now()).is_err());
        assert_eq!(cache, Some(held), "a failed request keeps what was held");
        let ci = gh
            .main_ci("o/app", "trunk", &mut cache, Utc::now())
            .unwrap();
        assert!(ci.workflows.is_empty());
        let got = requested(&requests, 3);
        assert_eq!(got[0], (list_url(), etag("w/\"a\"")));
        assert!(got[2].0.contains("?branch=trunk&"), "{got:?}");
        assert!(got[1].1.is_none() && got[2].1.is_none(), "{got:?}");
        assert_eq!(cache.unwrap().branch, "trunk");
        assert_eq!(encode("release/1.0 x"), "release/1.0%20x");
    }
}
