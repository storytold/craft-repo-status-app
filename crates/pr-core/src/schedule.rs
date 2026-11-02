//! Decides what to fetch next. There are far more open PRs than the hourly
//! GraphQL budget can inspect in one go (about 7,900, at 16 points per 25), so
//! keeping the cheap lists current comes first and everything else is doled out,
//! most useful first, within `max_points_per_hour`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::model::{Pr, RepoData, Time};
use crate::status_facts::StatusFacts;

/// A PR fetched this recently isn't retried, however it failed.
const DETAIL_RETRY_HOURS: i64 = 6;
/// However eager the status poller's news makes this one, never relist a repo sooner.
const MIN_RELIST_SECS: i64 = 5 * 60;
const ROSTER_MAX_AGE_HOURS: i64 = 24;
/// After a failed repo, wait this long.
const RETRY_SECS: i64 = 10 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailRef {
    pub repo: String,
    pub number: u64,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    ListFull(String),
    ListSince(String, Time),
    Roster(String),
    History(String),
    Details(Vec<DetailRef>),
}

pub struct SchedInput<'a> {
    pub now: Time,
    pub cfg: &'a Config,
    /// Watched repos, in the order to fetch them.
    pub repos: &'a [String],
    pub data: &'a BTreeMap<String, RepoData>,
    pub facts: &'a StatusFacts,
    /// GraphQL points this poller spent in the last hour.
    pub points_last_hour: u64,
    /// Repos whose list was asked for by hand.
    pub forced_lists: &'a [String],
    /// PRs asked for by hand.
    pub forced_details: &'a [DetailRef],
    /// How many PRs to fetch in one request.
    pub detail_batch: usize,
    /// The last job listed a repo for the first time. Inspecting some PRs next
    /// means the first pass, which takes a while, shows real results as it goes.
    pub just_listed: bool,
}

fn older_than(t: Option<Time>, secs: i64, now: Time) -> bool {
    t.is_none_or(|t| (now - t).num_seconds() >= secs)
}

/// How much a PR's detail is worth fetching next. Higher is sooner; `None` if
/// it's current or recently failed.
pub fn detail_priority(pr: &Pr, now: Time) -> Option<i32> {
    if pr.current_detail().is_some() || pr.id.is_empty() {
        return None;
    }
    if pr
        .detail_failed_at
        .is_some_and(|t| (now - t).num_hours() < DETAIL_RETRY_HOURS)
    {
        return None;
    }
    let mut score = 0;
    if !pr.is_draft {
        score += 100;
    }
    if pr.detail.is_none() {
        score += 50;
    }
    score += match pr.ci_rollup.as_deref() {
        Some("SUCCESS") => 30,
        Some("FAILURE" | "ERROR") => 0,
        _ => 10,
    };
    if pr.mergeable == "CONFLICTING" {
        score -= 40;
    }
    if pr.lines() < 500 {
        score += 20;
    }
    // Closest to done, and the only way to learn who approved a PR not relisted yet.
    if pr.review_decision.as_deref() == Some("APPROVED") {
        score += 60;
    }
    if let Some(updated) = pr.updated_at {
        let days = (now - updated).num_days();
        score += match days {
            ..=0 => 30,
            1..=7 => 20,
            8..=30 => 10,
            _ => 0,
        };
    }
    if matches!(pr.association.as_str(), "OWNER" | "MEMBER" | "COLLABORATOR") {
        score += 10;
    }
    Some(score)
}

/// How many PRs are waiting to be inspected, across `repos`.
pub fn detail_backlog(input: &SchedInput) -> usize {
    input
        .repos
        .iter()
        .filter_map(|r| input.data.get(r))
        .flat_map(|d| d.prs.values())
        .filter(|p| detail_priority(p, input.now).is_some())
        .count()
}

/// The most useful thing to fetch now, if anything is due.
pub fn next_job(input: &SchedInput) -> Option<Job> {
    let now = input.now;
    let cfg = input.cfg;
    let data = |r: &String| input.data.get(r);
    let ready_to_try = |r: &String| {
        let d = data(r);
        !d.is_some_and(|d| d.inaccessible)
            && (d.is_none_or(|d| d.error.is_none())
                || older_than(d.and_then(|d| d.last_attempt_at), RETRY_SECS, now))
    };

    if let Some(r) = input.forced_lists.first() {
        return Some(Job::ListFull(r.clone()));
    }
    if !input.forced_details.is_empty() {
        return Some(Job::Details(
            input
                .forced_details
                .iter()
                .take(input.detail_batch)
                .cloned()
                .collect(),
        ));
    }
    // Who can write decides a PR's tier, and costs a point or two.
    if let Some(r) = input.repos.iter().filter(|r| ready_to_try(r)).find(|r| {
        data(r).is_some_and(|d| d.full_list_at.is_some())
            && older_than(
                data(r).and_then(|d| d.roster.fetched_at),
                ROSTER_MAX_AGE_HOURS * 3600,
                now,
            )
    }) {
        return Some(Job::Roster(r.clone()));
    }
    let under_budget = input.points_last_hour < cfg.max_points_per_hour;
    if input.just_listed && under_budget {
        if let Some(batch) = next_details(input) {
            return Some(Job::Details(batch));
        }
    }
    // Smallest first, going by the status poller's counts, to fill the table fastest.
    if let Some(r) = input
        .repos
        .iter()
        .filter(|r| ready_to_try(r) && data(r).is_none_or(|d| d.full_list_at.is_none()))
        .min_by_key(|r| {
            input
                .facts
                .repos
                .get(r.as_str())
                .and_then(|s| s.open_prs)
                .unwrap_or(u64::MAX)
        })
    {
        return Some(Job::ListFull(r.clone()));
    }
    if let Some(r) = input.repos.iter().filter(|r| ready_to_try(r)).find(|r| {
        older_than(
            data(r).and_then(|d| d.full_list_at),
            cfg.full_list_secs(),
            now,
        )
    }) {
        return Some(Job::ListFull(r.clone()));
    }
    let due = input
        .repos
        .iter()
        .filter(|r| ready_to_try(r))
        .filter_map(|r| {
            let d = data(r)?;
            let list_at = d.list_at?;
            let age = (now - list_at).num_seconds();
            let news = input
                .facts
                .repos
                .get(r.as_str())
                .and_then(|s| s.newest_pr_at)
                .is_some_and(|t| t > list_at);
            (age >= cfg.poll_secs() || (news && age >= MIN_RELIST_SECS)).then_some((list_at, r))
        })
        .min_by_key(|(at, _)| *at);
    if let Some((list_at, r)) = due {
        return Some(Job::ListSince(r.clone(), list_at));
    }
    if !under_budget {
        return None;
    }
    if let Some(r) = input
        .repos
        .iter()
        .filter(|r| ready_to_try(r))
        .find(|r| data(r).is_some_and(|d| !d.history.complete))
    {
        return Some(Job::History(r.clone()));
    }
    next_details(input).map(Job::Details)
}

/// The PRs most worth inspecting next, if any are waiting.
fn next_details(input: &SchedInput) -> Option<Vec<DetailRef>> {
    let now = input.now;
    let mut waiting: Vec<(i32, Option<Time>, DetailRef)> = input
        .repos
        .iter()
        .filter_map(|r| input.data.get(r).map(|d| (r, d)))
        .flat_map(|(r, d)| {
            d.prs.values().filter_map(move |p| {
                detail_priority(p, now).map(|score| {
                    let at = p.updated_at;
                    let re = DetailRef {
                        repo: r.clone(),
                        number: p.number,
                        id: p.id.clone(),
                    };
                    (score, at, re)
                })
            })
        })
        .collect();
    waiting.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let batch: Vec<DetailRef> = waiting
        .into_iter()
        .take(input.detail_batch)
        .map(|(_, _, r)| r)
        .collect();
    (!batch.is_empty()).then_some(batch)
}

/// Whether the budget is too low to fetch anything, leaving the rest for the status poller.
pub fn budget_exhausted(remaining: Option<u64>, facts: Option<u64>, floor: u64) -> bool {
    remaining.into_iter().chain(facts).any(|r| r < floor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PrDetail, Roster};
    use chrono::Duration;

    fn now() -> Time {
        "2026-10-31T12:00:00Z".parse().unwrap()
    }

    fn fresh_repo(name: &str) -> RepoData {
        RepoData {
            repo: name.into(),
            list_at: Some(now()),
            full_list_at: Some(now()),
            roster: Roster {
                fetched_at: Some(now()),
                ..Default::default()
            },
            history: crate::model::History {
                complete: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn pr(number: u64) -> Pr {
        Pr {
            number,
            id: format!("id{number}"),
            head_oid: "h".into(),
            updated_at: Some(now()),
            ..Default::default()
        }
    }

    struct World {
        cfg: Config,
        repos: Vec<String>,
        data: BTreeMap<String, RepoData>,
        facts: StatusFacts,
        spent: u64,
        lists: Vec<String>,
        details: Vec<DetailRef>,
        just_listed: bool,
    }

    impl World {
        fn new(names: &[&str]) -> World {
            let repos: Vec<String> = names.iter().map(|s| s.to_string()).collect();
            World {
                cfg: Config::default(),
                data: repos.iter().map(|r| (r.clone(), fresh_repo(r))).collect(),
                repos,
                facts: StatusFacts::default(),
                spent: 0,
                lists: vec![],
                details: vec![],
                just_listed: false,
            }
        }

        fn next(&self) -> Option<Job> {
            next_job(&SchedInput {
                now: now(),
                cfg: &self.cfg,
                repos: &self.repos,
                data: &self.data,
                facts: &self.facts,
                points_last_hour: self.spent,
                forced_lists: &self.lists,
                forced_details: &self.details,
                detail_batch: 2,
                just_listed: self.just_listed,
            })
        }

        fn repo(&mut self, name: &str) -> &mut RepoData {
            self.data.get_mut(name).unwrap()
        }
    }

    #[test]
    fn nothing_due_when_everything_is_fresh() {
        assert_eq!(World::new(&["o/a"]).next(), None);
    }

    #[test]
    fn unlisted_repos_come_first() {
        let mut w = World::new(&["o/a", "o/b"]);
        w.data.remove("o/b");
        w.repo("o/a").prs.insert(1, pr(1));
        assert_eq!(w.next(), Some(Job::ListFull("o/b".into())));
    }

    #[test]
    fn smaller_repos_are_listed_first() {
        let mut w = World::new(&["o/big", "o/small"]);
        w.data.clear();
        assert_eq!(
            w.next(),
            Some(Job::ListFull("o/big".into())),
            "config order without counts"
        );
        for (name, open) in [("o/big", 1200), ("o/small", 50)] {
            w.facts.repos.insert(
                name.into(),
                crate::status_facts::StatusRepo {
                    open_prs: Some(open),
                    ..Default::default()
                },
            );
        }
        assert_eq!(w.next(), Some(Job::ListFull("o/small".into())));
    }

    #[test]
    fn inspecting_alternates_with_a_first_pass_of_lists() {
        let mut w = World::new(&["o/a", "o/b"]);
        w.data.remove("o/b");
        w.repo("o/a").prs.insert(1, pr(1));
        assert_eq!(w.next(), Some(Job::ListFull("o/b".into())));
        w.just_listed = true;
        assert!(matches!(w.next(), Some(Job::Details(_))));
        // But only within the budget.
        w.spent = 2500;
        assert_eq!(w.next(), Some(Job::ListFull("o/b".into())));
    }

    #[test]
    fn a_listed_repos_roster_comes_before_the_next_list() {
        let mut w = World::new(&["o/a", "o/b"]);
        w.data.remove("o/b");
        w.repo("o/a").roster.fetched_at = None;
        assert_eq!(w.next(), Some(Job::Roster("o/a".into())));
    }

    #[test]
    fn stale_lists_get_incremental_then_full_walks() {
        let mut w = World::new(&["o/a"]);
        let then = now() - Duration::minutes(31);
        w.repo("o/a").list_at = Some(then);
        assert_eq!(w.next(), Some(Job::ListSince("o/a".into(), then)));
        w.repo("o/a").full_list_at = Some(now() - Duration::hours(5));
        assert_eq!(w.next(), Some(Job::ListFull("o/a".into())));
    }

    #[test]
    fn news_from_craft_status_brings_a_relist_forward() {
        let mut w = World::new(&["o/a"]);
        let then = now() - Duration::minutes(10);
        w.repo("o/a").list_at = Some(then);
        assert_eq!(w.next(), None);
        w.facts.repos.insert(
            "o/a".into(),
            crate::status_facts::StatusRepo {
                newest_pr_at: Some(now() - Duration::minutes(2)),
                ..Default::default()
            },
        );
        assert_eq!(w.next(), Some(Job::ListSince("o/a".into(), then)));
        // But not more often than every few minutes.
        w.repo("o/a").list_at = Some(now() - Duration::minutes(3));
        assert_eq!(w.next(), None);
    }

    #[test]
    fn budget_limits_details_and_history_but_not_lists() {
        let mut w = World::new(&["o/a"]);
        w.repo("o/a").prs.insert(1, pr(1));
        assert!(matches!(w.next(), Some(Job::Details(_))));
        w.spent = 2500;
        assert_eq!(w.next(), None);
        w.repo("o/a").history.complete = false;
        assert_eq!(w.next(), None);
        w.repo("o/a").list_at = Some(now() - Duration::hours(1));
        assert!(matches!(w.next(), Some(Job::ListSince(..))));
    }

    #[test]
    fn history_comes_before_inspecting_prs() {
        let mut w = World::new(&["o/a"]);
        w.repo("o/a").prs.insert(1, pr(1));
        w.repo("o/a").history.complete = false;
        assert_eq!(w.next(), Some(Job::History("o/a".into())));
    }

    #[test]
    fn details_go_best_first_and_batch_across_repos() {
        let mut w = World::new(&["o/a", "o/b"]);
        let mut draft = pr(1);
        draft.is_draft = true;
        w.repo("o/a").prs.insert(1, draft);
        let mut green = pr(2);
        green.ci_rollup = Some("SUCCESS".into());
        w.repo("o/a").prs.insert(2, green);
        let mut other = pr(3);
        other.ci_rollup = Some("PENDING".into());
        w.repo("o/b").prs.insert(3, other);
        let Some(Job::Details(batch)) = w.next() else {
            panic!("expected details");
        };
        assert_eq!(
            batch.iter().map(|d| d.number).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn current_or_recently_failed_details_are_skipped() {
        let mut p = pr(1);
        assert!(detail_priority(&p, now()).is_some());
        p.detail_failed_at = Some(now() - Duration::hours(1));
        assert!(detail_priority(&p, now()).is_none());
        p.detail_failed_at = Some(now() - Duration::hours(7));
        assert!(detail_priority(&p, now()).is_some());
        p.detail = Some(PrDetail {
            for_head: "h".into(),
            for_updated_at: p.updated_at,
            ..Default::default()
        });
        assert!(detail_priority(&p, now()).is_none());
        p.updated_at = Some(now() + Duration::minutes(1));
        assert!(detail_priority(&p, now()).is_some());
    }

    #[test]
    fn approved_prs_are_inspected_sooner() {
        let mut approved = pr(1);
        approved.review_decision = Some("APPROVED".into());
        assert!(detail_priority(&approved, now()) > detail_priority(&pr(2), now()));
    }

    #[test]
    fn requests_by_hand_jump_the_queue() {
        let mut w = World::new(&["o/a"]);
        w.repo("o/a").list_at = Some(now() - Duration::hours(1));
        w.details = vec![DetailRef {
            repo: "o/a".into(),
            number: 9,
            id: "x".into(),
        }];
        assert!(matches!(w.next(), Some(Job::Details(_))));
        w.lists = vec!["o/a".into()];
        assert_eq!(w.next(), Some(Job::ListFull("o/a".into())));
    }

    #[test]
    fn failed_repos_wait_and_inaccessible_ones_are_skipped() {
        let mut w = World::new(&["o/a"]);
        w.repo("o/a").full_list_at = None;
        w.repo("o/a").error = Some("boom".into());
        w.repo("o/a").last_attempt_at = Some(now() - Duration::minutes(2));
        assert_eq!(w.next(), None);
        w.repo("o/a").last_attempt_at = Some(now() - Duration::minutes(11));
        assert!(w.next().is_some());
        w.repo("o/a").inaccessible = true;
        assert_eq!(w.next(), None);
    }

    #[test]
    fn the_floor_applies_to_either_apps_reading() {
        assert!(!budget_exhausted(Some(4000), None, 1000));
        assert!(budget_exhausted(Some(900), None, 1000));
        assert!(budget_exhausted(Some(4000), Some(800), 1000));
        assert!(!budget_exhausted(None, None, 1000));
    }
}
