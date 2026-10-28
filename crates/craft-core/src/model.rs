//! The data the UI renders. Everything here is serialized to the frontend
//! and to the cache file, so field names are part of both contracts.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::BuildLag;

pub type Time = DateTime<Utc>;

/// The "recent activity" windows, as `(id, minutes)`, shortest first. Every
/// poll fetches all of them; the UI's dropdown picks one by id (`ui/src/model.ts`
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

/// The id of the window that covers the repository's whole history. It isn't
/// in [`WINDOWS`] (it has no length); its counts come from totals, not searches.
pub const ALL_TIME: &str = "all";

/// The id of the window that starts when the repository's latest build was
/// published, so its length differs per repository. Like [`ALL_TIME`] it isn't
/// in [`WINDOWS`]; repos without a build don't have it.
pub const SINCE_RELEASE: &str = "release";

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
    /// Distinct commit authors on `main` (by GitHub login, else email); all
    /// time, GitHub's contributor count including unlinked emails.
    pub people: u64,
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
    /// Activity per window, keyed by the ids in [`WINDOWS`] plus [`ALL_TIME`]
    /// and, when there is a build, [`SINCE_RELEASE`].
    pub recent: BTreeMap<String, Activity>,
    /// Newest published (non-draft) release.
    pub latest_build: Option<String>,
    pub latest_build_at: Option<Time>,
    pub latest_build_prerelease: bool,
    /// Commit the latest build's tag points at.
    pub latest_build_sha: Option<String>,
    /// Commits on `main` since the latest build.
    pub since_build: Option<BuildLag>,
    pub latest_build_downloads: u64,
    pub downloads_total: u64,
    /// Sum of the top five urgent-issue scores; the repo-level sort key.
    pub urgency: f64,
    /// Open issues whose title or labels name a crash, hang, data loss…
    pub critical_issues: u64,
    /// Highest-scoring open issues, best first.
    pub urgent: Vec<UrgentIssue>,
    /// Blob id of the app icon (`assets/app-icon/hicolor/64x64/apps/*.png`).
    pub icon_oid: Option<String>,
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
    /// GitHub reported the repo missing (e.g. private and not visible to the
    /// token); shown as an empty row rather than an error.
    pub inaccessible: bool,
    /// The app icon as a `data:` URI, downloaded once per blob id.
    pub icon: Option<String>,
    /// The blob `icon` was downloaded from; compared with `stats.icon_oid`.
    pub icon_oid: Option<String>,
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

    /// No data yet, or stats from an older build that lack a field the UI
    /// shows: today, a recent-activity window (all time included, and since
    /// the release for repos with one).
    pub fn is_hole(&self) -> bool {
        self.fetched_at.is_none()
            || self.stats.as_ref().is_some_and(|s| {
                !WINDOWS
                    .iter()
                    .map(|(id, _)| *id)
                    .chain([ALL_TIME])
                    .chain(s.latest_build_at.map(|_| SINCE_RELEASE))
                    .all(|id| s.recent.contains_key(id))
            })
    }

    /// When this repo should next be fetched: at once if it's a hole or older
    /// than a manual refresh, else once its data is `refresh_secs` old; never
    /// sooner than `retry_secs` after a failed attempt.
    pub fn due_at(&self, schedule: &Schedule) -> Time {
        let due = match self.fetched_at {
            Some(t) if !self.is_hole() && schedule.forced_since.is_none_or(|f| t >= f) => {
                t + chrono::Duration::seconds(schedule.refresh_secs)
            }
            _ => Time::MIN_UTC,
        };
        match (&self.error, self.last_attempt_at) {
            (Some(_), Some(t)) => due.max(t + chrono::Duration::seconds(schedule.retry_secs)),
            _ => due,
        }
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
    /// The GitHub token the last poll used; `None` before the first poll.
    pub token: Option<TokenStatus>,
    /// The repo being fetched right now. Not restored from the cache.
    #[serde(skip_deserializing)]
    pub fetching: Option<String>,
    /// Repos refreshed by hand from their row, waiting their turn (first
    /// asked, first fetched). Not restored from the cache.
    #[serde(skip_deserializing)]
    pub requested: Vec<String>,
}

/// How often the poller fetches each repository (see [`RepoEntry::due_at`]).
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    /// Refetch a repo once its data is this old.
    pub refresh_secs: i64,
    /// Wait this long after a failed attempt before retrying that repo.
    pub retry_secs: i64,
    /// A manual refresh: data older than this is due at once.
    pub forced_since: Option<Time>,
}

/// Where the poller's GitHub token came from, and whether GitHub took it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TokenStatus {
    /// `$GITHUB_TOKEN`, `config`, `gh CLI`…; `None` when no token was found.
    pub source: Option<String>,
    /// GitHub answered 401: expired, revoked or mistyped.
    pub rejected: bool,
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

    /// The repo to fetch next at `now`: holes first, in config order, then
    /// whichever due repo has the oldest data. `None` when nothing is due.
    pub fn next_due(&self, now: Time, schedule: &Schedule) -> Option<&RepoEntry> {
        self.repos
            .iter()
            .filter(|e| e.due_at(schedule) <= now)
            .min_by_key(|e| (!e.is_hole(), e.fetched_at))
    }

    /// Queue `repo` to be fetched on its own, ahead of the schedule. False if
    /// it isn't configured, or is already queued or being fetched. Names
    /// match in any letter case, as in [`Snapshot::entry`].
    pub fn request(&mut self, repo: &str) -> bool {
        let same = |r: &str| r.eq_ignore_ascii_case(repo);
        let known = self.entry(repo).is_some();
        let pending =
            self.fetching.as_deref().is_some_and(same) || self.requested.iter().any(|r| same(r));
        if !known || pending {
            return false;
        }
        self.requested.push(repo.to_string());
        true
    }

    /// The next repo refreshed by hand, spelled as its entry is now,
    /// dropping any no longer configured.
    pub fn take_requested(&mut self) -> Option<String> {
        while !self.requested.is_empty() {
            let repo = self.requested.remove(0);
            if let Some(e) = self.entry(&repo) {
                return Some(e.repo.clone());
            }
        }
        None
    }

    /// The entry for `repo`, in any letter case: a fetch that started before
    /// [`Snapshot::sync_repos`] changed an entry's spelling still finds it.
    pub fn entry(&self, repo: &str) -> Option<&RepoEntry> {
        self.repos
            .iter()
            .find(|e| e.repo.eq_ignore_ascii_case(repo))
    }

    /// As [`Snapshot::entry`], for changing the entry.
    pub fn entry_mut(&mut self, repo: &str) -> Option<&mut RepoEntry> {
        self.repos
            .iter_mut()
            .find(|e| e.repo.eq_ignore_ascii_case(repo))
    }

    /// When the next repo falls due; `None` with nothing configured.
    pub fn next_due_at(&self, schedule: &Schedule) -> Option<Time> {
        self.repos.iter().map(|e| e.due_at(schedule)).min()
    }

    pub fn is_stale(&self, now: Time) -> bool {
        self.repos.is_empty()
            || self
                .repos
                .iter()
                .any(|r| r.is_stale(now, self.stale_after_secs))
    }

    /// Make `repos` match the configured list, keeping data for repos that
    /// remain. A repo listed again in other letter case keeps its data and
    /// takes the configured spelling.
    pub fn sync_repos(&mut self, wanted: &[String]) {
        let mut old = std::mem::take(&mut self.repos);
        self.repos = wanted
            .iter()
            .map(|w| {
                old.iter()
                    .position(|e| e.repo.eq_ignore_ascii_case(w))
                    .map(|i| RepoEntry {
                        repo: w.clone(),
                        ..old.swap_remove(i)
                    })
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
    fn cache_without_every_window_is_a_hole() {
        let mut e = RepoEntry::new("o/a");
        assert!(e.is_hole(), "never fetched");
        e.fetched_at = Some(Utc::now());
        assert!(
            !e.is_hole(),
            "fetched, but inaccessible: no stats to complete"
        );
        let mut stats = RepoStats::default();
        e.stats = Some(stats.clone());
        assert!(e.is_hole(), "cache from before the windows existed");
        for (id, _) in WINDOWS {
            stats.recent.insert(id.to_string(), Activity::default());
        }
        e.stats = Some(stats.clone());
        assert!(e.is_hole(), "cache from before all time was a window");
        stats.recent.insert(ALL_TIME.into(), Activity::default());
        e.stats = Some(stats.clone());
        assert!(!e.is_hole(), "no build, so no since-release window");
        stats.latest_build_at = Some(Utc::now());
        e.stats = Some(stats.clone());
        assert!(e.is_hole(), "cache from before since-release was a window");
        stats
            .recent
            .insert(SINCE_RELEASE.into(), Activity::default());
        e.stats = Some(stats);
        assert!(!e.is_hole());
    }

    #[test]
    fn schedule_fills_holes_then_the_stalest() {
        let now = Utc::now();
        let sched = Schedule {
            refresh_secs: 30 * 60,
            retry_secs: 10 * 60,
            forced_since: None,
        };
        let mut s = Snapshot::default();
        s.sync_repos(&["o/fresh".into(), "o/old".into(), "o/older".into()]);
        s.repos[0].fetched_at = at(5, now);
        s.repos[1].fetched_at = at(40, now);
        s.repos[2].fetched_at = at(50, now);
        let next =
            |s: &Snapshot, sched: &Schedule| s.next_due(now, sched).map(|e| e.name().to_string());
        assert_eq!(next(&s, &sched).as_deref(), Some("older"));

        s.sync_repos(&[
            "o/fresh".into(),
            "o/old".into(),
            "o/older".into(),
            "o/new".into(),
        ]);
        assert_eq!(next(&s, &sched).as_deref(), Some("new"), "holes first");

        s.repos[3].fetched_at = Some(now);
        s.repos[2].fetched_at = Some(now);
        s.repos[1].error = Some("network".into());
        s.repos[1].last_attempt_at = at(1, now);
        assert_eq!(next(&s, &sched), None, "a failed repo waits out its retry");
        assert_eq!(
            s.next_due_at(&sched),
            at(1, now).map(|t| t + Duration::minutes(10))
        );

        let forced = Schedule {
            forced_since: Some(now),
            ..sched
        };
        assert_eq!(
            next(&s, &forced).as_deref(),
            Some("fresh"),
            "a manual refresh makes everything due, stalest first"
        );
        assert!(Snapshot::default().next_due_at(&sched).is_none());
    }

    #[test]
    fn rows_refreshed_by_hand_queue_once_each() {
        let mut s = Snapshot::default();
        s.sync_repos(&["o/a".into(), "o/b".into(), "o/c".into()]);
        assert!(s.request("o/b"));
        assert!(!s.request("o/b"), "already queued");
        assert!(!s.request("o/zzz"), "not configured");
        assert!(s.request("o/c"));
        assert!(s.request("o/a"));
        s.sync_repos(&["o/b".into(), "o/a".into()]);
        assert_eq!(s.take_requested().as_deref(), Some("o/b"));
        s.fetching = Some("o/b".into());
        assert!(!s.request("o/b"), "being fetched");
        assert_eq!(
            s.take_requested().as_deref(),
            Some("o/a"),
            "o/c left the config"
        );
        assert_eq!(s.take_requested(), None);
    }

    #[test]
    fn hand_refreshes_are_not_restored_from_the_cache() {
        let mut s = Snapshot::default();
        s.sync_repos(&["o/a".into()]);
        s.request("o/a");
        s.fetching = Some("o/a".into());
        let back: Snapshot = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(back.requested.is_empty());
        assert_eq!(back.fetching, None);
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
        assert_eq!(s.repos[1].repo, "O/B", "the configured spelling");
        assert_eq!(s.repos[1].name(), "B");
        // A fetch that started with the old spelling still finds the entry.
        s.entry_mut("o/b").unwrap().error = None;
        assert_eq!(s.repos[1].error, None);
        assert_eq!(s.entry("o/B").map(|e| e.repo.as_str()), Some("O/B"));
        assert!(s.entry("o/x").is_none());
    }

    #[test]
    fn a_respelled_repo_keeps_its_pending_refresh() {
        let mut s = Snapshot::default();
        s.sync_repos(&["o/a".into(), "o/b".into()]);
        assert!(s.request("o/a"));
        s.fetching = Some("o/b".into());
        // Settings saves the list with both repos in other letter case.
        s.sync_repos(&["O/A".into(), "O/B".into()]);
        assert!(!s.request("O/A"), "already queued");
        assert!(!s.request("O/B"), "being fetched");
        assert_eq!(
            s.take_requested().as_deref(),
            Some("O/A"),
            "the queued refresh survives, under the new spelling"
        );
        assert_eq!(s.take_requested(), None);
    }
}
