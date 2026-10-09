//! The data the UI renders. Everything here is serialized to the frontend
//! and to the cache file, so field names are part of both contracts.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub type Time = DateTime<Utc>;

/// The "recent activity" windows, as `(id, minutes)`, shortest first. Every
/// poll fetches all of them; the UI's dropdown picks one by id (`ui/model.js`
/// lists the same ids).
pub const WINDOWS: &[(&str, i64)] = &[
    ("10m", 10),
    ("30m", 30),
    ("1h", 60),
    ("4h", 4 * 60),
    ("12h", 12 * 60),
    ("1d", 24 * 60),
    ("7d", 7 * 24 * 60),
];

/// What happened in one recent window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Activity {
    /// Commits to the default branch (`main`).
    pub commits: u64,
    pub prs_opened: u64,
    /// PRs merged in the window, whenever they were opened.
    pub prs_merged: u64,
    pub issues_opened: u64,
}

/// One poll's worth of numbers for a repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RepoStats {
    pub open_prs: u64,
    pub open_issues: u64,
    /// Committer date of the head of the default branch (`main`).
    pub last_commit_at: Option<Time>,
    /// Newest issue, any state.
    pub last_issue_at: Option<Time>,
    /// Oldest PR that is still open.
    pub oldest_open_pr_at: Option<Time>,
    /// Newest PR, any state.
    pub newest_pr_at: Option<Time>,
    /// Activity per window, keyed by the ids in [`WINDOWS`].
    pub recent: BTreeMap<String, Activity>,
    pub commits_total: u64,
    pub contributors: u64,
    pub issues_total: u64,
    pub prs_total: u64,
    /// Newest published (non-draft) release.
    pub latest_build: Option<String>,
    pub latest_build_at: Option<Time>,
    pub latest_build_prerelease: bool,
    pub latest_build_downloads: u64,
    pub downloads_total: u64,
    /// Sum of the top five urgent-issue scores; the repo-level sort key.
    pub urgency: f64,
    /// Open issues whose title or labels name a crash, hang, data loss…
    pub critical_issues: u64,
    /// Highest-scoring open issues, best first.
    pub urgent: Vec<UrgentIssue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UrgentIssue {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub created_at: Option<Time>,
    pub updated_at: Option<Time>,
    pub comments: u64,
    pub reactions: u64,
    pub labels: Vec<String>,
    pub score: f64,
    pub critical: bool,
    /// Short human explanations of the score ("crash", "12 reactions"…).
    pub reasons: Vec<String>,
}

/// A configured repository and whatever we last managed to learn about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RepoEntry {
    /// `owner/name`
    pub repo: String,
    pub stats: Option<RepoStats>,
    /// When `stats` was fetched. Kept across failed polls so stale data stays visible.
    pub fetched_at: Option<Time>,
    pub last_attempt_at: Option<Time>,
    /// Error from the most recent attempt, cleared on success.
    pub error: Option<String>,
}

impl RepoEntry {
    pub fn new(repo: &str) -> Self {
        RepoEntry {
            repo: repo.to_string(),
            ..Default::default()
        }
    }

    pub fn name(&self) -> &str {
        self.repo.rsplit('/').next().unwrap_or(&self.repo)
    }

    pub fn is_stale(&self, now: Time, stale_after_secs: i64) -> bool {
        match self.fetched_at {
            Some(t) => (now - t).num_seconds() > stale_after_secs,
            None => true,
        }
    }
}

/// What the UI receives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Snapshot {
    pub repos: Vec<RepoEntry>,
    /// End of the last poll cycle that refreshed every repository.
    pub last_full_refresh_at: Option<Time>,
    pub refreshing: bool,
    pub next_refresh_at: Option<Time>,
    pub poll_secs: i64,
    pub stale_after_secs: i64,
    pub rate_limit_remaining: Option<u64>,
    /// Problems that aren't about one repository (no token, rate limited…).
    pub error: Option<String>,
}

impl Snapshot {
    /// The time of the oldest data on screen; `None` if some repo has none.
    pub fn oldest_data_at(&self) -> Option<Time> {
        self.repos
            .iter()
            .map(|r| r.fetched_at)
            .try_fold(None::<Time>, |acc, t| {
                let t = t?;
                Some(Some(acc.map_or(t, |a| a.min(t))))
            })
            .flatten()
    }

    /// False when some repo's stats predate a field the UI needs: today, a
    /// recent-activity window. Such a cache (from an older build) shouldn't
    /// delay the launch poll.
    pub fn is_complete(&self) -> bool {
        self.repos
            .iter()
            .filter_map(|r| r.stats.as_ref())
            .all(|s| WINDOWS.iter().all(|(id, _)| s.recent.contains_key(*id)))
    }

    pub fn is_stale(&self, now: Time) -> bool {
        self.repos.is_empty()
            || self
                .repos
                .iter()
                .any(|r| r.is_stale(now, self.stale_after_secs))
    }

    /// Make `repos` match the configured list, keeping data for repos that remain.
    pub fn sync_repos(&mut self, wanted: &[String]) {
        let mut old = std::mem::take(&mut self.repos);
        self.repos = wanted
            .iter()
            .map(|w| {
                old.iter()
                    .position(|e| e.repo.eq_ignore_ascii_case(w))
                    .map(|i| old.swap_remove(i))
                    .unwrap_or_else(|| RepoEntry::new(w))
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn at(mins_ago: i64, now: Time) -> Option<Time> {
        Some(now - Duration::minutes(mins_ago))
    }

    #[test]
    fn staleness_uses_the_oldest_repo() {
        let now = Utc::now();
        let mut s = Snapshot {
            stale_after_secs: 15 * 60,
            ..Default::default()
        };
        assert!(s.is_stale(now), "nothing configured counts as stale");
        s.sync_repos(&["a/x".into(), "a/y".into()]);
        assert!(s.is_stale(now), "never fetched");
        s.repos[0].fetched_at = at(2, now);
        s.repos[1].fetched_at = at(5, now);
        assert!(!s.is_stale(now));
        assert_eq!(s.oldest_data_at(), at(5, now));
        s.repos[1].fetched_at = at(16, now);
        assert!(s.is_stale(now));
        assert!(!s.repos[0].is_stale(now, s.stale_after_secs));
        s.repos[1].fetched_at = None;
        assert_eq!(s.oldest_data_at(), None);
    }

    #[test]
    fn cache_without_every_window_is_incomplete() {
        let mut s = Snapshot::default();
        s.sync_repos(&["o/a".into(), "o/b".into()]);
        assert!(s.is_complete(), "no stats yet is not a reason to refetch");
        let mut stats = RepoStats::default();
        s.repos[0].stats = Some(stats.clone());
        assert!(!s.is_complete(), "cache from before the windows existed");
        for (id, _) in WINDOWS {
            stats.recent.insert(id.to_string(), Activity::default());
        }
        s.repos[0].stats = Some(stats);
        assert!(s.is_complete());
    }

    #[test]
    fn sync_repos_keeps_data_and_order() {
        let mut s = Snapshot::default();
        s.sync_repos(&["o/a".into(), "o/b".into()]);
        s.repos[1].error = Some("kept".into());
        s.sync_repos(&["o/c".into(), "O/B".into()]);
        assert_eq!(s.repos.len(), 2);
        assert_eq!(s.repos[0].repo, "o/c");
        assert_eq!(s.repos[1].error.as_deref(), Some("kept"));
        assert_eq!(s.repos[1].name(), "b");
    }
}
