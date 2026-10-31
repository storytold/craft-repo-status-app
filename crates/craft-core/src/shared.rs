//! The cache this app shares with Craft PR Review, in
//! `~/Library/Caches/craft-github-cache/`, so the two can run side by side on one
//! GitHub account without fetching the same things or overwriting each other:
//!
//! - `prs/<owner>@<name>.json` ([`PrLedger`]): every PR either app has seen, kept
//!   up to date by one walk that serves both. Whichever app walks saves the other
//!   the quota.
//! - `lease.lock` ([`Lease`]): an OS file lock, held while talking to GitHub. It
//!   keeps the two from bursting at once and makes updating a ledger safe. The OS
//!   releases it if an app dies.
//! - `budget.json` ([`SharedBudget`]): the last rate-limit reading, and how long
//!   to back off once GitHub has limited either app.
//!
//! All of it can be deleted: the next fetch rebuilds it. A missing or corrupt
//! file reads as empty.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::model::Time;
use crate::{atomic_write, Error, Result};

/// How far before the previous walk a later one reaches, so nothing that changed
/// while it ran slips between the two.
const OVERLAP_MINUTES: i64 = 10;

/// One PR, as far as either app needs to know without fetching it in full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LedgerPr {
    /// OPEN, CLOSED or MERGED.
    pub state: String,
    pub created_at: Option<Time>,
    pub updated_at: Option<Time>,
    pub merged_at: Option<Time>,
    pub author: Option<String>,
}

/// What's known about a repo's PRs. Newest changes come from walking by last
/// update, which reaches every PR opened or merged in a span of time; older
/// merges come from a one-off backfill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PrLedger {
    /// When the last walk started. Everything from `covers_from` to here is held.
    pub walked_at: Option<Time>,
    pub covers_from: Option<Time>,
    pub prs: BTreeMap<u64, LedgerPr>,
    /// Where the walk through all merged PRs, oldest first, has reached.
    pub backfill_cursor: Option<String>,
    pub backfill_complete: bool,
}

/// How far a walk has to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkPlan {
    /// Stop at PRs last updated before this.
    pub stop_at: Time,
    /// Whether it starts from nothing, rather than extending the last walk.
    pub cold: bool,
}

impl PrLedger {
    /// The walk needed to know about everything since `since`, if the ledger
    /// isn't already that complete and less than `max_age_secs` old.
    pub fn plan_walk(&self, since: Time, now: Time, max_age_secs: i64) -> Option<WalkPlan> {
        match (self.walked_at, self.covers_from) {
            (Some(walked), Some(covers)) if covers <= since => {
                ((now - walked).num_seconds() >= max_age_secs).then(|| WalkPlan {
                    stop_at: (walked - chrono::Duration::minutes(OVERLAP_MINUTES)).max(covers),
                    cold: false,
                })
            }
            _ => Some(WalkPlan {
                stop_at: since,
                cold: true,
            }),
        }
    }

    /// Adds or updates a PR, keeping whichever record is newer: a backfill can
    /// bring back older news than a walk already has.
    pub fn record(&mut self, number: u64, pr: LedgerPr) {
        match self.prs.get(&number) {
            Some(held) if held.updated_at > pr.updated_at => {}
            _ => {
                self.prs.insert(number, pr);
            }
        }
    }

    /// Notes a finished walk that started at `now`. `reached_stop` is false when
    /// it gave up early, having got back only as far as `oldest_seen`.
    pub fn finish_walk(
        &mut self,
        plan: WalkPlan,
        now: Time,
        reached_stop: bool,
        oldest_seen: Option<Time>,
    ) {
        self.covers_from = if reached_stop {
            if plan.cold {
                Some(plan.stop_at)
            } else {
                self.covers_from
            }
        } else {
            oldest_seen.or(Some(now))
        };
        self.walked_at = Some(now);
    }

    /// When each PR opened since `since` was opened.
    pub fn opened_since(&self, since: Time) -> Vec<Time> {
        self.prs
            .values()
            .filter_map(|p| p.created_at)
            .filter(|t| *t >= since)
            .collect()
    }

    /// When each PR merged since `since` was merged.
    pub fn merged_since(&self, since: Time) -> Vec<Time> {
        self.prs
            .values()
            .filter_map(|p| p.merged_at)
            .filter(|t| *t >= since)
            .collect()
    }

    /// Who wrote each merged PR the ledger knows of.
    pub fn merged_authors(&self) -> BTreeMap<u64, String> {
        self.prs
            .iter()
            .filter(|(_, p)| p.state == "MERGED")
            .filter_map(|(n, p)| p.author.clone().map(|a| (*n, a)))
            .collect()
    }
}

/// A ledger bootstrapped from a Craft Status `raw/<repo>.json` written before PRs
/// moved here: what it held about the last week's PRs, so the first walk only has
/// to fetch what's new since. It has no authors, and no state for unmerged PRs;
/// walks and the backfill fill those in. `None` unless the file has what's needed.
pub fn ledger_from_old_raw(text: &str) -> Option<PrLedger> {
    #[derive(Deserialize)]
    struct OldRaw {
        covers_from: Time,
        fetched_at: Time,
        prs_opened: BTreeMap<u64, Time>,
        prs_merged: BTreeMap<u64, Time>,
    }
    let old: OldRaw = serde_json::from_str(text).ok()?;
    let mut ledger = PrLedger {
        walked_at: Some(old.fetched_at),
        covers_from: Some(old.covers_from),
        ..Default::default()
    };
    for (n, at) in old.prs_opened {
        ledger.prs.entry(n).or_default().created_at = Some(at);
    }
    for (n, at) in old.prs_merged {
        let pr = ledger.prs.entry(n).or_default();
        pr.state = "MERGED".to_string();
        pr.merged_at = Some(at);
    }
    Some(ledger)
}

/// The last rate-limit reading either app saw, and any backoff either asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SharedBudget {
    pub remaining: Option<u64>,
    pub reset_at: Option<Time>,
    pub read_at: Option<Time>,
    /// GitHub rate-limited one of the apps: neither should fetch before this.
    pub backoff_until: Option<Time>,
}

impl SharedBudget {
    /// When a backoff still in force lifts.
    pub fn backoff(&self, now: Time) -> Option<Time> {
        self.backoff_until.filter(|t| *t > now)
    }

    /// The points left, unless the reading is from before the budget last reset
    /// or more than `max_age_minutes` old.
    pub fn remaining_now(&self, now: Time, max_age_minutes: i64) -> Option<u64> {
        let fresh = self
            .read_at
            .is_some_and(|t| (now - t).num_minutes() < max_age_minutes);
        let not_reset = self.reset_at.is_none_or(|t| t > now);
        self.remaining.filter(|_| fresh && not_reset)
    }
}

/// Held while talking to GitHub; released when dropped, or if the app dies.
#[derive(Debug)]
pub struct Lease {
    _file: File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedCache {
    dir: PathBuf,
}

impl SharedCache {
    /// `~/Library/Caches/craft-github-cache/` (or the platform equivalent).
    pub fn default_location() -> Result<SharedCache> {
        let dir = dirs::cache_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
            .ok_or(Error::NoHome)?;
        Ok(SharedCache::at(dir.join("craft-github-cache")))
    }

    pub fn at(dir: PathBuf) -> SharedCache {
        SharedCache { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn ledger_path(&self, repo: &str) -> PathBuf {
        self.dir
            .join("prs")
            .join(format!("{}.json", repo.replace('/', "@")))
    }

    pub fn load_ledger(&self, repo: &str) -> PrLedger {
        read_json(&self.ledger_path(repo))
    }

    /// The repo's ledger, or if it has none yet, one bootstrapped from Craft
    /// Status's cache in `old_raw_dir`.
    pub fn load_or_seed_ledger(&self, repo: &str, old_raw_dir: Option<&Path>) -> PrLedger {
        let held = self.load_ledger(repo);
        if held != PrLedger::default() {
            return held;
        }
        old_raw_dir
            .and_then(|d| {
                std::fs::read_to_string(
                    d.join("raw")
                        .join(format!("{}.json", repo.replace('/', "@"))),
                )
                .ok()
            })
            .and_then(|t| ledger_from_old_raw(&t))
            .unwrap_or_default()
    }

    /// Call holding the lease, having loaded the ledger under the same lease.
    pub fn save_ledger(&self, repo: &str, ledger: &PrLedger) -> Result<()> {
        atomic_write(&self.ledger_path(repo), &serde_json::to_string(ledger)?)
    }

    fn budget_path(&self) -> PathBuf {
        self.dir.join("budget.json")
    }

    pub fn budget(&self) -> SharedBudget {
        read_json(&self.budget_path())
    }

    /// Records a rate-limit reading, keeping any backoff in force.
    pub fn note_budget(
        &self,
        remaining: Option<u64>,
        reset_at: Option<Time>,
        now: Time,
    ) -> Result<()> {
        let mut b = self.budget();
        if remaining.is_some() {
            b.remaining = remaining;
            b.reset_at = reset_at;
            b.read_at = Some(now);
        }
        atomic_write(&self.budget_path(), &serde_json::to_string(&b)?)
    }

    /// Tells both apps to leave GitHub alone until `until`.
    pub fn note_backoff(&self, until: Time) -> Result<()> {
        let mut b = self.budget();
        b.backoff_until = Some(b.backoff_until.map_or(until, |t| t.max(until)));
        atomic_write(&self.budget_path(), &serde_json::to_string(&b)?)
    }

    fn lock_file(&self) -> Option<File> {
        std::fs::create_dir_all(&self.dir).ok()?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join("lease.lock"))
            .ok()
    }

    /// The lease, if it's free right now. `Ok(None)` means the other app has it.
    pub fn try_lease(&self) -> Option<Lease> {
        let file = self.lock_file()?;
        match file.try_lock() {
            Ok(()) => Some(Lease { _file: file }),
            Err(TryLockError::WouldBlock) | Err(TryLockError::Error(_)) => None,
        }
    }

    /// The lease, waiting up to `patience` for the other app to finish.
    pub fn lease(&self, patience: Duration) -> Option<Lease> {
        let started = Instant::now();
        loop {
            if let Some(lease) = self.try_lease() {
                return Some(lease);
            }
            if started.elapsed() >= patience {
                return None;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

fn read_json<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Time {
        s.parse().unwrap()
    }

    fn cache(name: &str) -> SharedCache {
        let dir = std::env::temp_dir().join(format!("craft-shared-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        SharedCache::at(dir)
    }

    #[test]
    fn walks_extend_the_last_one_and_skip_when_it_is_fresh() {
        let now = t("2026-10-31T12:00:00Z");
        let week_ago = t("2026-10-24T12:00:00Z");
        let mut l = PrLedger::default();
        let cold = l.plan_walk(week_ago, now, 300).unwrap();
        assert_eq!((cold.stop_at, cold.cold), (week_ago, true));
        l.finish_walk(cold, now, true, None);
        assert_eq!(l.covers_from, Some(week_ago));

        // Just walked, by this app or the other: nothing to do.
        assert_eq!(
            l.plan_walk(week_ago, now + chrono::Duration::seconds(299), 300),
            None
        );
        let later = now + chrono::Duration::minutes(30);
        let warm = l.plan_walk(week_ago, later, 300).unwrap();
        assert_eq!(
            (warm.stop_at, warm.cold),
            (t("2026-10-31T11:50:00Z"), false)
        );
        l.finish_walk(warm, later, true, None);
        assert_eq!((l.covers_from, l.walked_at), (Some(week_ago), Some(later)));

        // Asked to reach further back than what's covered: walk from the top.
        let further = t("2026-09-01T00:00:00Z");
        assert_eq!(
            l.plan_walk(further, later, 300),
            Some(WalkPlan {
                stop_at: further,
                cold: true
            })
        );
    }

    #[test]
    fn a_narrow_walk_never_hides_an_older_gap() {
        let now = t("2026-10-31T12:00:00Z");
        let mut l = PrLedger::default();
        let day_ago = t("2026-10-30T12:00:00Z");
        l.finish_walk(l.plan_walk(day_ago, now, 300).unwrap(), now, true, None);
        // Covered back a day, so a request for a week is still cold.
        assert!(
            l.plan_walk(t("2026-10-24T12:00:00Z"), now, 300)
                .unwrap()
                .cold
        );
    }

    #[test]
    fn a_walk_that_gave_up_only_covers_what_it_reached() {
        let now = t("2026-10-31T12:00:00Z");
        let mut l = PrLedger::default();
        let since = t("2026-10-01T00:00:00Z");
        let oldest = t("2026-10-20T00:00:00Z");
        l.finish_walk(
            l.plan_walk(since, now, 300).unwrap(),
            now,
            false,
            Some(oldest),
        );
        assert_eq!(l.covers_from, Some(oldest));
        assert!(l.plan_walk(since, now, 300).unwrap().cold);
    }

    #[test]
    fn bootstraps_from_craft_status_raw_activity() {
        let text = r#"{"covers_from": "2026-10-24T16:00:00Z", "fetched_at": "2026-10-31T11:00:00Z", "full_at": "2026-10-31T10:00:00Z",
            "commits": {}, "issues_opened": {"5": "2026-10-30T00:00:00Z"},
            "prs_opened": {"1": "2026-10-25T00:00:00Z", "2": "2026-10-26T00:00:00Z"},
            "prs_merged": {"1": "2026-10-27T00:00:00Z", "900": "2026-10-28T00:00:00Z"}}"#;
        let l = ledger_from_old_raw(text).unwrap();
        let since = t("2026-10-24T17:00:00Z");
        assert_eq!(l.opened_since(since).len(), 2);
        assert_eq!(l.merged_since(since).len(), 2);
        assert_eq!(l.prs[&1].state, "MERGED");
        // Only what's new needs walking.
        let now = t("2026-10-31T12:00:00Z");
        let plan = l.plan_walk(since, now, 300).unwrap();
        assert_eq!(
            (plan.stop_at, plan.cold),
            (t("2026-10-31T10:50:00Z"), false)
        );
        // A real walk's record replaces a seeded one.
        let mut l = l;
        l.record(
            1,
            LedgerPr {
                state: "MERGED".into(),
                updated_at: Some(now),
                author: Some("ann".into()),
                ..Default::default()
            },
        );
        assert_eq!(l.prs[&1].author.as_deref(), Some("ann"));
        // Files from after PRs moved, or unrelated ones, don't count.
        assert_eq!(
            ledger_from_old_raw(
                r#"{"covers_from": "2026-10-24T16:00:00Z", "fetched_at": "2026-10-31T11:00:00Z"}"#
            ),
            None
        );
        assert_eq!(ledger_from_old_raw("{nope"), None);
    }

    #[test]
    fn counts_come_from_the_entries() {
        let mut l = PrLedger::default();
        let pr = |state: &str, created: &str, merged: Option<&str>, who: &str| LedgerPr {
            state: state.into(),
            created_at: Some(t(created)),
            updated_at: Some(t(created)),
            merged_at: merged.map(t),
            author: Some(who.into()),
        };
        l.record(
            1,
            pr(
                "MERGED",
                "2026-09-01T00:00:00Z",
                Some("2026-10-30T00:00:00Z"),
                "ann",
            ),
        );
        l.record(2, pr("OPEN", "2026-10-30T00:00:00Z", None, "bo"));
        l.record(3, pr("CLOSED", "2026-09-01T00:00:00Z", None, "cy"));
        let since = t("2026-10-01T00:00:00Z");
        assert_eq!(l.opened_since(since).len(), 1);
        assert_eq!(l.merged_since(since).len(), 1);
        assert_eq!(
            l.merged_authors().into_iter().collect::<Vec<_>>(),
            [(1, "ann".to_string())]
        );
    }

    #[test]
    fn older_records_never_overwrite_newer_ones() {
        let mut l = PrLedger::default();
        let at = |s: &str, state: &str| LedgerPr {
            state: state.into(),
            updated_at: Some(t(s)),
            ..Default::default()
        };
        l.record(1, at("2026-10-30T00:00:00Z", "OPEN"));
        l.record(1, at("2026-10-01T00:00:00Z", "MERGED"));
        assert_eq!(l.prs[&1].state, "OPEN");
        l.record(1, at("2026-10-31T00:00:00Z", "MERGED"));
        assert_eq!(l.prs[&1].state, "MERGED");
    }

    #[test]
    fn ledgers_round_trip_and_corrupt_files_read_as_empty() {
        let c = cache("ledger");
        assert_eq!(c.load_ledger("o/r"), PrLedger::default());
        let mut l = PrLedger::default();
        l.record(
            7,
            LedgerPr {
                state: "OPEN".into(),
                ..Default::default()
            },
        );
        c.save_ledger("o/r", &l).unwrap();
        assert_eq!(c.load_ledger("o/r"), l);
        std::fs::write(c.ledger_path("o/r"), "{nope").unwrap();
        assert_eq!(c.load_ledger("o/r"), PrLedger::default());
        std::fs::remove_dir_all(c.dir()).ok();
    }

    #[test]
    fn a_backoff_from_either_app_binds_both_and_survives_new_readings() {
        let c = cache("budget");
        let now = t("2026-10-31T12:00:00Z");
        assert_eq!(c.budget().backoff(now), None);
        c.note_backoff(now + chrono::Duration::minutes(5)).unwrap();
        c.note_backoff(now + chrono::Duration::minutes(2)).unwrap();
        c.note_budget(Some(100), Some(now + chrono::Duration::hours(1)), now)
            .unwrap();
        let b = c.budget();
        assert_eq!(
            b.backoff(now),
            Some(now + chrono::Duration::minutes(5)),
            "the later one wins"
        );
        assert_eq!(b.backoff(now + chrono::Duration::minutes(6)), None);
        assert_eq!(b.remaining_now(now, 10), Some(100));
        assert_eq!(
            b.remaining_now(now + chrono::Duration::minutes(11), 10),
            None,
            "too old"
        );
        c.note_budget(None, None, now).unwrap();
        assert_eq!(
            c.budget().remaining,
            Some(100),
            "no reading keeps the last one"
        );
        std::fs::remove_dir_all(c.dir()).ok();
    }

    #[test]
    fn a_budget_reading_lapses_when_the_budget_resets() {
        let now = t("2026-10-31T12:00:00Z");
        let b = SharedBudget {
            remaining: Some(5),
            read_at: Some(now),
            reset_at: Some(now + chrono::Duration::minutes(1)),
            ..Default::default()
        };
        assert_eq!(b.remaining_now(now, 10), Some(5));
        assert_eq!(
            b.remaining_now(now + chrono::Duration::minutes(2), 10),
            None
        );
    }

    #[test]
    fn only_one_holder_of_the_lease_at_a_time() {
        let c = cache("lease");
        let held = c.try_lease().expect("free");
        assert!(
            c.try_lease().is_none(),
            "a second open of the file can't lock it"
        );
        assert!(
            c.lease(Duration::from_millis(50)).is_none(),
            "patience runs out"
        );
        drop(held);
        assert!(c.lease(Duration::from_millis(50)).is_some());
        std::fs::remove_dir_all(c.dir()).ok();
    }
}
