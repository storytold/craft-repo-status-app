//! What's kept for each repo and PR. It is all serialized to the cache, and the
//! UI's types in `ui/src/model.ts` mirror the parts it receives: change both.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub type Time = DateTime<Utc>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Major,
    #[default]
    Minor,
    Trivial,
}

impl Severity {
    /// Critical and major findings are flags; the rest are nits.
    pub fn is_flag(self) -> bool {
        matches!(self, Severity::Critical | Severity::Major)
    }
}

/// One inline CodeRabbit comment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Finding {
    pub severity: Severity,
    /// "Functional Correctness", "Security & Privacy"…
    pub category: String,
    /// "Quick win", "Heavy lift" or empty.
    pub effort: String,
    pub title: String,
    pub snippet: String,
    pub path: String,
    pub line: Option<u64>,
    pub url: String,
    pub resolved: bool,
    pub outdated: bool,
    pub security: bool,
}

impl Finding {
    /// Whether it still needs acting on.
    pub fn active(&self) -> bool {
        !self.resolved && !self.outdated
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FileChange {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub change: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Check {
    pub name: String,
    /// SUCCESS, FAILURE, PENDING, SKIPPED, IN_PROGRESS…
    pub state: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Review {
    pub author: String,
    pub state: String,
    pub at: Option<Time>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Comment {
    pub author: String,
    pub text: String,
    pub at: Option<Time>,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LinkedIssue {
    pub number: u64,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RabbitStatus {
    /// No CodeRabbit check on this PR.
    #[default]
    None,
    InProgress,
    Reviewed,
    Skipped,
}

/// The expensive half of a PR, fetched by priority; see [`crate::schedule`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PrDetail {
    pub fetched_at: Option<Time>,
    /// The PR's `updatedAt` and head when this was fetched.
    pub for_updated_at: Option<Time>,
    pub for_head: String,
    pub body: String,
    pub files: Vec<FileChange>,
    pub files_total: u64,
    pub checks: Vec<Check>,
    pub rabbit: RabbitStatus,
    pub findings: Vec<Finding>,
    /// Review threads beyond those fetched aren't in `findings`.
    pub threads_total: u64,
    /// The latest CodeRabbit review's "Actionable comments posted: N".
    pub rabbit_actionable: Option<u64>,
    /// Unresolved threads started by people.
    pub human_threads_open: u64,
    pub reviews: Vec<Review>,
    pub comments: Vec<Comment>,
    pub linked_issues: Vec<LinkedIssue>,
}

/// What Claude said about a PR. Advisory: see `assess` for how little it can move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Analysis {
    pub at: Option<Time>,
    pub model: String,
    /// The head commit this describes.
    pub for_head: String,
    /// Whether Claude read the diff, not just the metadata.
    pub deep: bool,
    /// The diff was too long to send whole.
    pub diff_truncated: bool,
    pub summary: String,
    pub ease: Option<Ease>,
    pub interesting: bool,
    pub why_interesting: String,
    pub review_focus: String,
    pub risk: String,
    pub security_note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Pr {
    pub id: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub author_is_bot: bool,
    pub avatar: String,
    /// GitHub's authorAssociation: FIRST_TIME_CONTRIBUTOR, MEMBER…
    pub association: String,
    pub created_at: Option<Time>,
    pub updated_at: Option<Time>,
    pub is_draft: bool,
    /// MERGEABLE, CONFLICTING or UNKNOWN.
    pub mergeable: String,
    /// CLEAN, BEHIND, DIRTY, UNSTABLE, BLOCKED…
    pub merge_state: String,
    /// APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED or none.
    pub review_decision: Option<String>,
    /// Whose latest review is an approval.
    pub approved_by: Vec<String>,
    /// Those who approved the commit now at the head of the PR.
    pub approved_on_head: Vec<String>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub head_oid: String,
    pub head_ref: String,
    pub base_ref: String,
    pub cross_repo: bool,
    pub labels: Vec<String>,
    /// The head commit's check rollup when last listed: SUCCESS, FAILURE, PENDING…
    pub ci_rollup: Option<String>,
    pub comments_total: u64,
    pub reactions: u64,
    pub listed_at: Option<Time>,
    pub detail: Option<PrDetail>,
    /// The last failed attempt at the detail, so one bad PR can't stall the queue.
    pub detail_failed_at: Option<Time>,
    pub analysis: Option<Analysis>,
}

impl Pr {
    /// The detail, if it describes the PR as last listed.
    pub fn current_detail(&self) -> Option<&PrDetail> {
        self.detail
            .as_ref()
            .filter(|d| d.for_head == self.head_oid && d.for_updated_at >= self.updated_at)
    }

    pub fn analysis_is_current(&self) -> bool {
        self.analysis
            .as_ref()
            .is_some_and(|a| a.for_head == self.head_oid)
    }

    /// Who currently approves this. PRs cached before `approved_by` was fetched,
    /// and not relisted since, fall back on the reviews in their detail.
    pub fn approvers(&self) -> Vec<String> {
        if !self.approved_by.is_empty() {
            return self.approved_by.clone();
        }
        let Some(d) = &self.detail else {
            return vec![];
        };
        let mut latest: Vec<(&str, &str)> = Vec::new();
        for r in d.reviews.iter().filter(|r| r.state != "COMMENTED") {
            match latest.iter_mut().find(|(who, _)| *who == r.author) {
                Some(entry) => entry.1 = &r.state,
                None => latest.push((&r.author, &r.state)),
            }
        }
        latest
            .into_iter()
            .filter(|(_, state)| *state == "APPROVED")
            .map(|(who, _)| who.to_string())
            .collect()
    }

    pub fn lines(&self) -> u64 {
        self.additions + self.deletions
    }

    /// The fields the list query fills in, keeping detail and analysis.
    pub fn absorb_listing(&mut self, listed: Pr) {
        let detail = self.detail.take();
        let failed = self.detail_failed_at.take();
        let analysis = self.analysis.take();
        *self = Pr {
            detail,
            detail_failed_at: failed,
            analysis,
            ..listed
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Ease {
    Easy,
    #[default]
    Medium,
    Hard,
}

/// A PR's headline status, worked out by `assess::headline` from independent facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Draft,
    /// A failed check, requested changes, a CodeRabbit flag or an open conversation.
    NeedsWork,
    /// Merge conflicts, and nothing else known to be wrong.
    Conflicted,
    #[default]
    NotInspected,
    /// CI or CodeRabbit is running, or GitHub hasn't worked out mergeability.
    Checking,
    /// Often a first-time contributor's workflows, waiting on a maintainer.
    CiNotRun,
    /// Approved on the latest commit, CI passed, nothing in the way.
    Landable,
    /// No current approval, but it passes every automated check and scores well.
    Promising,
    /// No current approval, nothing in the way, and an unremarkable score.
    Reviewable,
}

impl PrState {
    /// Whether a reviewer or maintainer can do something about it now.
    pub fn is_queued(self) -> bool {
        matches!(
            self,
            PrState::Landable | PrState::Promising | PrState::Reviewable | PrState::CiNotRun
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Write access, or listed in the repo's contributors/people.toml.
    Core,
    /// More than `frequent_over` PRs landed across the craft apps.
    Frequent,
    #[default]
    Regular,
    /// First-time contributor, by GitHub's reckoning.
    New,
    Bot,
}

/// Who has standing in a repo. Refreshed daily.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Roster {
    pub fetched_at: Option<Time>,
    /// Login (lowercase) → WRITE, MAINTAIN, ADMIN, TRIAGE or READ.
    pub permissions: BTreeMap<String, String>,
    /// Logins (lowercase) in the repo's contributors/people.toml.
    pub listed: Vec<String>,
}

/// Every merged PR's author, read from the ledger shared with the status poller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct History {
    /// PR number → author login, keyed so overlapping walks can't double count.
    pub merged: BTreeMap<u64, String>,
    pub cursor: Option<String>,
    pub complete: bool,
}

/// A set of PRs Claude judged related, from its last pass over the repo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ClaudeGroup {
    pub title: String,
    /// duplicate, overlap, stack or theme.
    pub relation: String,
    pub note: String,
    pub numbers: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RepoData {
    pub repo: String,
    pub prs: BTreeMap<u64, Pr>,
    /// When the open-PR list last came up to date, by any walk.
    pub list_at: Option<Time>,
    /// When it was last walked from end to end.
    pub full_list_at: Option<Time>,
    pub last_attempt_at: Option<Time>,
    pub error: Option<String>,
    pub inaccessible: bool,
    pub roster: Roster,
    /// From the shared ledger, which is what's kept on disk.
    #[serde(skip)]
    pub history: History,
    pub claude_groups: Vec<ClaudeGroup>,
    pub groups_at: Option<Time>,
    /// Computed from the PRs, on load and after every change.
    #[serde(skip)]
    pub groups: Vec<crate::overlap::Group>,
}

/// Rate-limit bookkeeping from one GraphQL response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spend {
    pub cost: u64,
    pub remaining: Option<u64>,
    pub reset_at: Option<Time>,
}
