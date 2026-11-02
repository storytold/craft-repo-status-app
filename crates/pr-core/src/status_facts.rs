//! What the status poller already knows that's useful to the PR poller: app icons,
//! open-PR counts to show before the first PR list, and whether it has spotted a PR
//! newer than that list. What the two fetch in common is in `craft_core::shared`.

use std::collections::BTreeMap;

use craft_core::Snapshot;
use serde::Serialize;

use crate::model::Time;

/// Readings older than this say nothing about the current budget.
const BUDGET_FRESH_MINUTES: i64 = 10;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StatusRepo {
    pub icon: Option<String>,
    pub open_prs: Option<u64>,
    pub newest_pr_at: Option<Time>,
    pub fetched_at: Option<Time>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusFacts {
    /// When the status poller last talked to GitHub, which dates `remaining`.
    pub read_at: Option<Time>,
    pub remaining: Option<u64>,
    pub repos: BTreeMap<String, StatusRepo>,
}

impl StatusFacts {
    pub fn of(snap: &Snapshot) -> StatusFacts {
        StatusFacts {
            read_at: snap.repos.iter().filter_map(|e| e.last_attempt_at).max(),
            remaining: snap.rate_limit_remaining,
            repos: snap
                .repos
                .iter()
                .map(|e| {
                    let info = StatusRepo {
                        icon: e.icon.clone(),
                        open_prs: e.stats.as_ref().map(|s| s.open_prs),
                        newest_pr_at: e.stats.as_ref().and_then(|s| s.newest_pr_at),
                        fetched_at: e.fetched_at,
                    };
                    (e.repo.clone(), info)
                })
                .collect(),
        }
    }

    /// The GraphQL points left when the status poller last read them, if recently.
    pub fn fresh_remaining(&self, now: Time) -> Option<u64> {
        self.remaining.filter(|_| {
            self.read_at
                .is_some_and(|t| (now - t).num_minutes() < BUDGET_FRESH_MINUTES)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_budget_readings_are_ignored() {
        let now: Time = "2026-10-31T12:00:00Z".parse().unwrap();
        let mut s = StatusFacts {
            remaining: Some(900),
            read_at: Some(now - chrono::Duration::minutes(3)),
            ..Default::default()
        };
        assert_eq!(s.fresh_remaining(now), Some(900));
        s.read_at = Some(now - chrono::Duration::minutes(30));
        assert_eq!(s.fresh_remaining(now), None);
    }

    #[test]
    fn takes_what_the_pr_tab_needs_from_a_snapshot() {
        let now: Time = "2026-10-31T12:00:00Z".parse().unwrap();
        let mut snap = Snapshot {
            rate_limit_remaining: Some(4000),
            ..Default::default()
        };
        snap.sync_repos(&["o/a".to_string(), "o/b".to_string()]);
        snap.repos[0].icon = Some("data:icon".into());
        snap.repos[0].last_attempt_at = Some(now);
        let facts = StatusFacts::of(&snap);
        assert_eq!(facts.read_at, Some(now));
        assert_eq!(facts.fresh_remaining(now), Some(4000));
        assert_eq!(facts.repos["o/a"].icon.as_deref(), Some("data:icon"));
        assert_eq!(facts.repos["o/b"].icon, None);
    }
}
