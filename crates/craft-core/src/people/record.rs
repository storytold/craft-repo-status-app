//! One pull request as the People tab and the rankings read it
//! ([`PrRecord`]), and the per-repository file that keeps every record
//! between polls ([`PrCache`]).
//!
//! A record holds the fields GitHub reports for the pull request, its
//! timeline (comments, reviews, pushes, cross-references, closes), the
//! state of its checks, and what it contributed (lines, files, commits).
//! The work state, type and area derived from it come later and say which
//! rule decided them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::Time;
use crate::{atomic_write, Result};

/// `mergeable` when GitHub has not computed it yet.
pub const UNKNOWN: &str = "UNKNOWN";
/// `mergeable` when the head conflicts with the base branch.
pub const CONFLICTING: &str = "CONFLICTING";

/// A pull request's state as GitHub names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum PrState {
    #[default]
    Open,
    Closed,
    Merged,
}

/// One pull request, keyed by its repository and number in a [`PrCache`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PrRecord {
    /// GitHub's node id, for fetching the record again by id.
    pub id: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub state: PrState,
    pub is_draft: bool,
    /// The author's login as GitHub spells it; `None` for a deleted account.
    pub author: Option<String>,
    /// GitHub reports the author as a bot account.
    pub author_bot: bool,
    /// `MEMBER`, `OWNER`, `CONTRIBUTOR`, `NONE`…
    pub author_association: String,
    pub created_at: Option<Time>,
    /// GitHub's `updatedAt` at the last deep fetch.
    pub updated_at: Option<Time>,
    pub closed_at: Option<Time>,
    pub merged_at: Option<Time>,
    pub merged_by: Option<String>,
    pub head_ref_name: String,
    pub head_ref_oid: String,
    /// The branch the pull request merges into; its commit and file lists
    /// hold what the head has that this branch does not.
    pub base_ref_name: String,
    /// The commit that branch pointed to at the last deep fetch.
    pub base_ref_oid: String,
    pub is_cross_repository: bool,
    pub maintainer_can_modify: bool,
    pub body: String,
    pub assignees: Vec<String>,
    /// The issues the pull request closes, by number.
    pub closing_issues: Vec<u64>,
    /// The titles of those issues, in the same order.
    pub closing_titles: Vec<String>,
    /// The repository (`owner/name`) of each of those issues, in the same
    /// order; empty in records fetched before it was kept.
    pub closing_repos: Vec<String>,
    /// Comments and reviews, as the light sweep counts them.
    pub comments_total: u64,
    pub checks: Checks,
    /// The timeline, oldest first, merged across fetches by item id.
    pub timeline: Vec<TimelineItem>,
    /// How many timeline items GitHub holds; more than `timeline` holds
    /// when a fetch read only the newest ones.
    pub timeline_total: u64,
    pub contribution: Contribution,
    pub derived: Derived,
    /// When the deep fields were fetched; `None` for a record the light
    /// sweep found that has not been fetched yet.
    pub fetched_at: Option<Time>,
    /// The light sweep saw a newer `updatedAt`, or the pull request left the
    /// open list: the deep fields need fetching again.
    pub stale: bool,
}

/// The head commit's checks and the pull request's mergeability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Checks {
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`. An `UNKNOWN` answer keeps
    /// the value read before it.
    pub mergeable: String,
    /// GitHub's `mergeStateStatus` (`CLEAN`, `BLOCKED`, `DIRTY`…).
    pub merge_state: String,
    /// The first fetch that saw `CONFLICTING`; GitHub records no event.
    pub conflict_since: Option<Time>,
    /// The head commit's status rollup (`SUCCESS`, `FAILURE`, `PENDING`…).
    pub rollup: Option<String>,
    /// The names of the head commit's failing checks.
    pub failing: Vec<String>,
    /// Where the rest of the head commit's checks start, while `failing`
    /// holds only the pages read so far.
    pub contexts_cursor: Option<ContextsCursor>,
    pub suites: Vec<Suite>,
}

/// Where the rest of a commit's checks start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ContextsCursor {
    /// The commit's node id.
    pub commit: String,
    pub after: String,
}

/// One check suite on the head commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Suite {
    pub status: String,
    pub conclusion: Option<String>,
    pub created_at: Option<Time>,
    /// The app that ran it (`github-actions`…).
    pub app: Option<String>,
}

/// One timeline item. `kind` is GitHub's type name (`IssueComment`,
/// `PullRequestReview`, `ClosedEvent`…); the other fields hold what that
/// type carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TimelineItem {
    pub id: String,
    pub kind: String,
    pub at: Option<Time>,
    /// The author of a comment or review, the actor of an event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub actor_bot: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub association: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// A review's state, or a close's `stateReason`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// The commit a review was submitted on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub will_close: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<CrossSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closer: Option<Closer>,
    /// The link to a comment or review.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The issue or pull request a cross-reference came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CrossSource {
    /// `PullRequest` or `Issue`.
    pub kind: String,
    pub repo: String,
    pub number: u64,
    pub state: String,
    pub title: String,
    pub merged_at: Option<Time>,
    pub author: Option<String>,
}

/// What closed a pull request, when it was not closed by hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Closer {
    /// `PullRequest` or `Commit`.
    pub kind: String,
    pub number: Option<u64>,
    pub oid: Option<String>,
}

/// What the pull request changed and who wrote it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Contribution {
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub files: Vec<FileLines>,
    /// GitHub's count of changed files; more than `files` holds until the
    /// rest of the list is fetched.
    pub files_total: u64,
    /// Where the rest of the file list starts.
    pub files_cursor: Option<String>,
    /// The head commit the file list was read at. The light sweep moves
    /// [`PrRecord::head_ref_oid`] to a new head before a fetch reads the
    /// new head's files.
    pub files_head: String,
    pub commits: Vec<PrCommit>,
    /// GitHub's count of commits; more than `commits` holds until the rest
    /// of the list is fetched.
    pub commits_total: u64,
    /// Where the rest of the commit list starts.
    pub commits_cursor: Option<String>,
    pub merge_commit: Option<MergeCommit>,
    /// Who landed it: see [`super::merger`].
    pub merger: Option<String>,
}

/// One changed file's lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FileLines {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

/// One commit in the pull request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PrCommit {
    pub oid: String,
    /// `authoredDate`: rebasing a branch resets `committedDate`, not this.
    pub authored_at: Option<Time>,
    pub committed_at: Option<Time>,
    pub additions: u64,
    pub deletions: u64,
    /// The author's login, when a GitHub account claims the commit.
    pub author: Option<String>,
    pub author_email: String,
    pub headline: String,
    /// The human co-authors its `Co-authored-by:` trailers name.
    pub co_authors: Vec<CoAuthor>,
    /// A merge of the base branch into the head (see [`super::is_merge_commit`]).
    pub is_merge: bool,
}

/// One `Co-authored-by:` trailer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CoAuthor {
    pub name: String,
    pub email: String,
    /// From a `users.noreply.github.com` address; `None` when the address
    /// names no login.
    pub login: Option<String>,
}

/// The commit a merge produced on the base branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MergeCommit {
    pub oid: String,
    /// Its headline: a squash merge's title, or "Merge pull request #N…".
    pub headline: String,
    pub parents: u64,
    pub committer_name: String,
    pub committer_login: Option<String>,
}

/// What later rules derive from the record, each with the rule that
/// decided it; empty until a rule runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Derived {
    pub state: Option<Decision>,
    pub kind: Option<Decision>,
    pub area: Option<Decision>,
}

/// A derived value and the rule that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Decision {
    pub value: String,
    pub rule: String,
}

/// The fields the light sweep reads for an open pull request.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Light {
    pub id: String,
    pub number: u64,
    pub title: String,
    /// The author's login; `None` for a deleted account.
    pub author: Option<String>,
    pub author_bot: bool,
    pub updated_at: Option<Time>,
    pub is_draft: bool,
    pub head_ref_oid: String,
    pub mergeable: String,
    pub merge_state: String,
    pub maintainer_can_modify: bool,
    pub comments_total: u64,
    pub rollup: Option<String>,
    pub suites: Vec<Suite>,
}

impl PrRecord {
    /// A record for a pull request the light sweep found before any deep
    /// fetch; [`PrRecord::needs_fetch`] holds for it.
    pub fn from_light(light: &Light, now: Time) -> PrRecord {
        let mut r = PrRecord {
            id: light.id.clone(),
            number: light.number,
            updated_at: light.updated_at,
            ..Default::default()
        };
        r.apply_light(light, now);
        r.stale = false;
        r
    }

    /// Whether the deep fields have to be fetched (again).
    pub fn needs_fetch(&self) -> bool {
        self.fetched_at.is_none() || self.stale
    }

    /// Takes the light sweep's fields. A newer `updatedAt` than the deep
    /// fetch saw marks the record stale; `updated_at` itself stays, so the
    /// next deep fetch is still due. The sweep does not read the names of
    /// failing checks: a new head drops the old head's names, and a rollup
    /// that turns to `FAILURE` or `ERROR` on a fetched record marks it stale,
    /// so the next deep fetch reads them. A check finishing does not change
    /// `updatedAt`. The title and the author fill a record that no deep
    /// fetch has read yet.
    pub fn apply_light(&mut self, light: &Light, now: Time) {
        if self.fetched_at.is_none() {
            self.title = light.title.clone();
            self.author = light.author.clone();
            self.author_bot = light.author_bot;
        }
        let new_head = !light.head_ref_oid.is_empty() && light.head_ref_oid != self.head_ref_oid;
        let failed = matches!(light.rollup.as_deref(), Some("FAILURE" | "ERROR"));
        if new_head {
            self.checks.failing.clear();
            self.checks.contexts_cursor = None;
            // The old head's commit pages are not read on.
            self.contribution.commits_cursor = None;
        }
        // A check suite that failed, recovered or ran again changes the
        // failing names, though the rollup may stay failed.
        let moved =
            new_head || light.rollup != self.checks.rollup || light.suites != self.checks.suites;
        if self.fetched_at.is_some() && failed && moved {
            self.stale = true;
        }
        self.state = PrState::Open;
        self.is_draft = light.is_draft;
        self.head_ref_oid = light.head_ref_oid.clone();
        self.maintainer_can_modify = light.maintainer_can_modify;
        self.comments_total = light.comments_total;
        self.checks
            .set_mergeable(&light.mergeable, &light.merge_state, now);
        self.checks.rollup = light.rollup.clone();
        self.checks.suites = light.suites.clone();
        if light.updated_at > self.updated_at {
            self.stale = true;
        }
    }

    /// Replaces this record's fields with those of `new`, a deep fetch made
    /// at `now`. When `new` read the whole timeline, its timeline replaces
    /// the kept one, so a deleted comment goes. Otherwise the timelines are
    /// merged by item id, so items an earlier fetch read stay when `new`
    /// read only the newest ones. The derived values, the conflict start and
    /// the comment count stay.
    pub fn merge_from(&mut self, mut new: PrRecord, now: Time) {
        let mut timeline = std::mem::take(&mut self.timeline);
        if new.timeline.len() as u64 >= new.timeline_total {
            timeline.clear();
        }
        for item in new.timeline.drain(..) {
            match timeline
                .iter_mut()
                .find(|t| !t.id.is_empty() && t.id == item.id)
            {
                Some(old) => *old = item,
                None => timeline.push(item),
            }
        }
        timeline.sort_by_key(|t| t.at);
        let mut checks = std::mem::take(&mut self.checks);
        checks.set_mergeable(&new.checks.mergeable, &new.checks.merge_state, now);
        checks.rollup = new.checks.rollup.take();
        checks.failing = std::mem::take(&mut new.checks.failing);
        checks.contexts_cursor = new.checks.contexts_cursor.take();
        checks.suites = std::mem::take(&mut new.checks.suites);
        // File pages fetched after the first stay while the head they were
        // read at, the base branch and its commit are the same.
        let same_base = self.base_ref_name == new.base_ref_name;
        let (old, fresh) = (&self.contribution, &new.contribution);
        if old.files.len() > fresh.files.len()
            && old.files_total == fresh.files_total
            && old.files_head == new.head_ref_oid
            && same_base
            && self.base_ref_oid == new.base_ref_oid
        {
            new.contribution.files = std::mem::take(&mut self.contribution.files);
            new.contribution.files_cursor = self.contribution.files_cursor.take();
        }
        new.contribution.files_head = new.head_ref_oid.clone();
        // So do commit pages, while the base is the same and the new first
        // page starts them: a whole list while it ends at the new head, and
        // pages read part way while the head is the same.
        let (old, fresh) = (&self.contribution, &new.contribution);
        let starts_alike = old
            .commits
            .iter()
            .zip(&fresh.commits)
            .all(|(o, f)| o.oid == f.oid);
        let ends_at_head = old
            .commits
            .last()
            .is_some_and(|k| k.oid == new.head_ref_oid);
        let part_way = old.commits_cursor.is_some() && self.head_ref_oid == new.head_ref_oid;
        if old.commits.len() > fresh.commits.len()
            && old.commits_total == fresh.commits_total
            && same_base
            && starts_alike
            && (ends_at_head || part_way)
        {
            new.contribution.commits = std::mem::take(&mut self.contribution.commits);
            new.contribution.commits_cursor = self.contribution.commits_cursor.take();
        }
        let derived = std::mem::take(&mut self.derived);
        let comments_total = self.comments_total;
        *self = PrRecord {
            timeline_total: new.timeline_total.max(timeline.len() as u64),
            timeline,
            checks,
            derived,
            comments_total: comments_total.max(new.comments_total),
            fetched_at: Some(now),
            stale: false,
            ..new
        };
        if self.state != PrState::Open {
            self.checks.conflict_since = None;
        }
    }
}

impl Checks {
    /// Takes a fetch's `mergeable` and `mergeStateStatus`. `UNKNOWN` keeps
    /// the values read before; a conflict keeps the time it was first seen.
    pub fn set_mergeable(&mut self, mergeable: &str, merge_state: &str, now: Time) {
        if mergeable == UNKNOWN && !self.mergeable.is_empty() {
            return;
        }
        if mergeable == CONFLICTING {
            if self.mergeable != CONFLICTING || self.conflict_since.is_none() {
                self.conflict_since = Some(now);
            }
        } else {
            self.conflict_since = None;
        }
        self.mergeable = mergeable.to_string();
        self.merge_state = merge_state.to_string();
    }
}

/// Every record of one repository, with where the fetches stand. One file
/// per repository, `<cache>/prs/<owner>@<repo>.json`, apart from the
/// activity in `raw/`, so the six-hourly fetch from scratch leaves it alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PrCache {
    pub records: BTreeMap<u64, PrRecord>,
    /// The newest `updatedAt` the changed list has read. The next poll
    /// reads the list down to a little before it.
    pub high_water: Option<Time>,
    /// Part of the changed list a poll left unread (it read its page limit
    /// first): where to continue, and how far down to read.
    pub gap: Option<Gap>,
    /// When the light sweep last read every open pull request.
    pub swept_at: Option<Time>,
    /// The checks on the head of the default branch, from the light sweep.
    pub main_checks: Option<MainChecks>,
    /// The first-load walk over merged pull requests.
    pub merged_walk: Walk,
    /// The first-load walk over pull requests closed without merging, which
    /// the changed list reads only from the first poll on.
    pub closed_walk: Walk,
    /// When a poll last read the changed list (or found it unchanged)
    /// without an error and left none of it unread (no `gap`): the records
    /// are up to date as of then.
    pub checked_at: Option<Time>,
    /// Pull requests per page of the walks after GitHub could not finish a
    /// fuller page; `None` for the full page. Later pages, in later calls
    /// too, start at this size. After a number of pages read at it
    /// (`page_ok`) the next larger size is asked for again, and the size is
    /// dropped once both walks are done.
    pub page_size: Option<u64>,
    /// Walk pages read at `page_size` since it last changed.
    pub page_ok: u32,
}

/// Where an unfinished read of the changed list continues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Gap {
    pub cursor: String,
    /// Read down to pull requests updated before this.
    pub until: Option<Time>,
}

/// Where a walk over merged or closed pull requests stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Walk {
    pub cursor: Option<String>,
    pub done: bool,
    /// How many pull requests in the walk's state GitHub reported on the
    /// last page read.
    pub total: Option<u64>,
}

/// The status of the default branch's head commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MainChecks {
    pub oid: String,
    pub rollup: Option<String>,
    pub failing: Vec<String>,
}

impl PrCache {
    pub fn path(dir: &Path, repo: &str) -> PathBuf {
        dir.join("prs")
            .join(format!("{}.json", repo.replace('/', "@")))
    }

    /// The records kept for `repo` under `dir`; empty if missing or corrupt.
    /// Each merged record's merger is worked out again from its merge
    /// commit (see [`super::merger`]), so a file written by an earlier rule
    /// gets the current one.
    pub fn load(dir: &Path, repo: &str) -> PrCache {
        let mut cache: PrCache = std::fs::read_to_string(Self::path(dir, repo))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        for r in cache.records.values_mut() {
            if r.state == PrState::Merged {
                r.contribution.merger =
                    super::merger(r.contribution.merge_commit.as_ref(), r.merged_by.as_deref());
            }
        }
        cache
    }

    pub fn save(&self, dir: &Path, repo: &str) -> Result<()> {
        atomic_write(&Self::path(dir, repo), &serde_json::to_string(self)?)
    }

    /// The record with node id `id`.
    pub fn by_id_mut(&mut self, id: &str) -> Option<&mut PrRecord> {
        self.records.values_mut().find(|r| r.id == id)
    }

    /// Takes a deep fetch of one pull request, made at `now`.
    pub fn put(&mut self, record: PrRecord, now: Time) {
        match self.records.get_mut(&record.number) {
            Some(old) => old.merge_from(record, now),
            None => {
                let mut fresh = PrRecord::default();
                fresh.merge_from(record, now);
                self.records.insert(fresh.number, fresh);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Time {
        s.parse().unwrap()
    }

    fn item(id: &str, at: &str, body: &str) -> TimelineItem {
        TimelineItem {
            id: id.into(),
            kind: "IssueComment".into(),
            at: Some(t(at)),
            body: Some(body.into()),
            ..Default::default()
        }
    }

    #[test]
    fn merging_a_fetch_keeps_older_timeline_items_and_derived_values() {
        let mut r = PrRecord {
            number: 7,
            timeline: vec![
                item("a", "2026-10-01T00:00:00Z", "first"),
                item("b", "2026-10-02T00:00:00Z", "second"),
            ],
            derived: Derived {
                state: Some(Decision {
                    value: "draft".into(),
                    rule: "isDraft".into(),
                }),
                ..Default::default()
            },
            comments_total: 4,
            stale: true,
            ..Default::default()
        };
        // The new fetch read only the newest items, one of them edited.
        let new = PrRecord {
            number: 7,
            title: "New title".into(),
            timeline: vec![
                item("c", "2026-10-03T00:00:00Z", "third"),
                item("b", "2026-10-02T00:00:00Z", "second, edited"),
            ],
            timeline_total: 3,
            ..Default::default()
        };
        let now = t("2026-10-04T00:00:00Z");
        r.merge_from(new, now);
        assert_eq!(r.title, "New title");
        let bodies: Vec<_> = r.timeline.iter().map(|i| i.body.clone().unwrap()).collect();
        assert_eq!(bodies, ["first", "second, edited", "third"]);
        assert_eq!(r.timeline_total, 3);
        assert_eq!(r.derived.state.as_ref().unwrap().value, "draft");
        assert_eq!(r.comments_total, 4);
        assert_eq!(r.fetched_at, Some(now));
        assert!(!r.stale && !r.needs_fetch());
    }

    #[test]
    fn a_fetch_of_the_whole_timeline_drops_deleted_items() {
        let mut r = PrRecord {
            number: 7,
            timeline: vec![
                item("a", "2026-10-01T00:00:00Z", "first"),
                item("b", "2026-10-02T00:00:00Z", "hold this for now"),
            ],
            timeline_total: 2,
            ..Default::default()
        };
        // Comment b was deleted, and the fetch read every item left.
        let new = PrRecord {
            number: 7,
            timeline: vec![
                item("a", "2026-10-01T00:00:00Z", "first"),
                item("c", "2026-10-03T00:00:00Z", "third"),
            ],
            timeline_total: 2,
            ..Default::default()
        };
        r.merge_from(new, t("2026-10-04T00:00:00Z"));
        let bodies: Vec<_> = r.timeline.iter().map(|i| i.body.clone().unwrap()).collect();
        assert_eq!(bodies, ["first", "third"]);
        assert_eq!(r.timeline_total, 2);
    }

    #[test]
    fn unknown_mergeability_keeps_the_last_value_and_conflicts_keep_their_start() {
        let mut c = Checks::default();
        let (t1, t2, t3) = (
            t("2026-10-01T00:00:00Z"),
            t("2026-10-02T00:00:00Z"),
            t("2026-10-03T00:00:00Z"),
        );
        c.set_mergeable(UNKNOWN, "UNKNOWN", t1);
        assert_eq!(c.mergeable, UNKNOWN, "nothing earlier to keep");
        c.set_mergeable(CONFLICTING, "DIRTY", t1);
        c.set_mergeable(UNKNOWN, "UNKNOWN", t2);
        assert_eq!(
            (c.mergeable.as_str(), c.merge_state.as_str()),
            (CONFLICTING, "DIRTY")
        );
        c.set_mergeable(CONFLICTING, "DIRTY", t3);
        assert_eq!(c.conflict_since, Some(t1), "the first poll that saw it");
        c.set_mergeable("MERGEABLE", "CLEAN", t3);
        assert_eq!(c.conflict_since, None);
    }

    #[test]
    fn the_light_sweep_marks_newer_records_stale() {
        let now = t("2026-10-05T00:00:00Z");
        let light = Light {
            id: "PR_1".into(),
            number: 1,
            title: "Fix the ruler".into(),
            author: Some("ann-example".into()),
            updated_at: Some(t("2026-10-04T00:00:00Z")),
            mergeable: "MERGEABLE".into(),
            comments_total: 2,
            ..Default::default()
        };
        let stub = PrRecord::from_light(&light, now);
        assert!(stub.needs_fetch() && !stub.stale, "never fetched");
        assert_eq!(
            (stub.title.as_str(), stub.author.as_deref()),
            ("Fix the ruler", Some("ann-example")),
            "an unread record carries its title and author"
        );
        let mut r = PrRecord {
            number: 1,
            title: "Fix the ruler, read in full".into(),
            author: Some("Ann-Example".into()),
            updated_at: light.updated_at,
            fetched_at: Some(now),
            ..Default::default()
        };
        r.apply_light(&light, now);
        assert!(!r.needs_fetch(), "the same updatedAt");
        assert_eq!(
            (r.title.as_str(), r.author.as_deref()),
            ("Fix the ruler, read in full", Some("Ann-Example")),
            "a read record keeps the deep fetch's fields"
        );
        let newer = Light {
            updated_at: Some(now),
            ..light
        };
        r.apply_light(&newer, now);
        assert!(r.stale && r.needs_fetch());
        assert_eq!(r.updated_at, Some(t("2026-10-04T00:00:00Z")));
        assert_eq!(r.comments_total, 2);
    }

    #[test]
    fn a_failure_the_sweep_sees_is_fetched_again_for_its_check_names() {
        let now = t("2026-10-05T00:00:00Z");
        let updated = Some(t("2026-10-04T00:00:00Z"));
        // Read while the checks ran: no failing names yet.
        let mut r = PrRecord {
            number: 1,
            updated_at: updated,
            head_ref_oid: "h1".into(),
            fetched_at: Some(now),
            checks: Checks {
                rollup: Some("PENDING".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let light = Light {
            number: 1,
            updated_at: updated,
            head_ref_oid: "h1".into(),
            rollup: Some("PENDING".into()),
            ..Default::default()
        };
        r.apply_light(&light, now);
        assert!(!r.needs_fetch(), "nothing changed");
        // The checks finished and failed; updatedAt stays the same.
        let failed = Light {
            rollup: Some("FAILURE".into()),
            ..light.clone()
        };
        r.apply_light(&failed, now);
        assert!(r.stale && r.needs_fetch());
        assert_eq!(r.checks.rollup.as_deref(), Some("FAILURE"));
        // The deep fetch read the names; the next sweep leaves the record alone.
        r.checks.failing = vec!["test".into()];
        r.stale = false;
        r.apply_light(&failed, now);
        assert!(!r.needs_fetch());
        assert_eq!(r.checks.failing, ["test"]);
        // Another suite fails too: the rollup stays FAILURE, the suites move.
        let suite = |conclusion: &str| Suite {
            status: "COMPLETED".into(),
            conclusion: Some(conclusion.into()),
            ..Default::default()
        };
        r.checks.suites = vec![suite("FAILURE"), suite("SUCCESS")];
        let both = Light {
            suites: vec![suite("FAILURE"), suite("FAILURE")],
            ..failed.clone()
        };
        r.apply_light(&both, now);
        assert!(r.needs_fetch(), "the failing names are read again");
        r.stale = false;
        r.apply_light(&both, now);
        assert!(!r.needs_fetch(), "nothing moved since");
        // A new head drops the old head's names and is fetched again.
        let pushed = Light {
            head_ref_oid: "h2".into(),
            ..failed
        };
        r.apply_light(&pushed, now);
        assert!(r.checks.failing.is_empty() && r.needs_fetch());
    }

    #[test]
    fn file_pages_read_at_an_earlier_head_are_not_kept() {
        let now = t("2026-10-05T00:00:00Z");
        let fetch = |head: &str| PrRecord {
            number: 5,
            head_ref_oid: head.into(),
            contribution: Contribution {
                files: vec![FileLines {
                    path: format!("{head}/a"),
                    ..Default::default()
                }],
                files_total: 2,
                files_cursor: Some("f1".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut r = PrRecord::default();
        r.merge_from(fetch("h1"), now);
        // A later call read the second page.
        r.contribution.files.push(FileLines {
            path: "h1/b".into(),
            ..Default::default()
        });
        r.contribution.files_cursor = None;
        r.merge_from(fetch("h1"), now);
        assert_eq!(r.contribution.files.len(), 2, "the same head keeps both");
        // The sweep sees a push before the fetch of the new head.
        let pushed = Light {
            number: 5,
            head_ref_oid: "h2".into(),
            ..Default::default()
        };
        r.apply_light(&pushed, now);
        r.merge_from(fetch("h2"), now);
        let paths: Vec<_> = r
            .contribution
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(paths, ["h2/a"]);
        assert_eq!(r.contribution.files_cursor.as_deref(), Some("f1"));
    }

    #[test]
    fn cache_round_trips_and_loads_files_without_the_new_sections() {
        let dir = std::env::temp_dir().join(format!("craft-prs-{}", std::process::id()));
        assert_eq!(PrCache::load(&dir, "o/r"), PrCache::default());
        let mut c = PrCache {
            high_water: Some(t("2026-10-01T00:00:00Z")),
            ..Default::default()
        };
        c.put(
            PrRecord {
                id: "PR_9".into(),
                number: 9,
                state: PrState::Merged,
                ..Default::default()
            },
            t("2026-10-02T00:00:00Z"),
        );
        c.save(&dir, "o/r").unwrap();
        assert!(PrCache::path(&dir, "o/r").ends_with("prs/o@r.json"));
        assert_eq!(PrCache::load(&dir, "o/r"), c);
        assert_eq!(c.by_id_mut("PR_9").map(|r| r.number), Some(9));
        // A file written before records had contributions or derived values.
        let old = r#"{"records":{"3":{"id":"PR_3","number":3,"state":"OPEN"}}}"#;
        std::fs::write(PrCache::path(&dir, "o/old"), old).unwrap();
        let back = PrCache::load(&dir, "o/old");
        assert_eq!(back.records[&3].contribution, Contribution::default());
        assert!(back.records[&3].needs_fetch());
        std::fs::write(PrCache::path(&dir, "o/bad"), "{oops").unwrap();
        assert_eq!(PrCache::load(&dir, "o/bad"), PrCache::default());
        // A merger kept by an earlier rule: the author committed the
        // one-parent head that a maintainer pushed.
        let kept = r#"{"records":{"4":{"id":"PR_4","number":4,"state":"MERGED","merged_by":"maint-one",
            "contribution":{"merger":"author-four","merge_commit":{"oid":"h4","headline":"Fix it","parents":1,
            "committer_name":"Author Four","committer_login":"author-four"}}}}}"#;
        std::fs::write(PrCache::path(&dir, "o/kept"), kept).unwrap();
        let back = PrCache::load(&dir, "o/kept");
        assert_eq!(
            back.records[&4].contribution.merger.as_deref(),
            Some("maint-one")
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
