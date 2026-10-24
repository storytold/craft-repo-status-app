//! Timestamped events derived from the pull request records: who had a
//! pull request merged or landed elsewhere, who merged, reviewed, pushed
//! commits to someone else's pull request or was named as a co-author, and
//! when each person authored a commit. A window is a filter over these
//! events, so changing the window needs no request to GitHub.

use std::collections::{BTreeMap, BTreeSet};

use chrono::SubsecRound;
use serde::{Deserialize, Serialize};

use super::record::{PrCache, PrRecord, PrState};
use super::{canonical, is_bot};
use crate::model::{Time, ALL_TIME, SINCE_RELEASE, WINDOWS};

/// The derived state of a pull request that landed through another one.
pub const LANDED_ELSEWHERE: &str = "closed_landed_elsewhere";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The author's pull request merged, at `mergedAt`.
    AuthoredMerged,
    /// The author's pull request closed because another one carried it: it
    /// was closed by a pull request or a commit, or a rule decided
    /// [`LANDED_ELSEWHERE`]. At `closedAt`.
    LandedElsewhere,
    /// The person landed a pull request (see [`super::merger`]).
    MergedBy,
    /// A review of someone else's pull request, at `submittedAt`; `detail`
    /// holds its state.
    Reviewed,
    /// A commit in someone else's merged pull request, at `mergedAt`.
    ReviewCommit,
    /// A `Co-authored-by:` trailer in a merged pull request, at `mergedAt`.
    CoAuthored,
    /// A commit authored in any pull request, at `authoredDate`.
    Committed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub at: Time,
    /// The canonical login (see [`canonical`]).
    pub login: String,
    pub repo: String,
    pub number: u64,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Every event, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Ledger {
    pub events: Vec<Event>,
}

/// One person's events in a window.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tally {
    pub merged: u64,
    pub landed_elsewhere: u64,
    pub merges: u64,
    pub reviews: u64,
    pub approved: u64,
    pub changes_requested: u64,
    pub commented: u64,
    /// Distinct pull requests reviewed.
    pub reviewed_prs: u64,
    pub review_commits: u64,
    pub co_authored: u64,
    /// Distinct UTC days with a commit, a merge or a review.
    pub active_days: u64,
}

impl Ledger {
    /// The events of every record in `repos` (`(repo, cache)`). Bot and AI
    /// accounts and deleted accounts get none. A commit that several pull
    /// requests of one repository hold (stacked branches) is authored once,
    /// and its review-commit and co-author credits go to the pull request
    /// that merged first.
    pub fn build(repos: &[(&str, &PrCache)]) -> Ledger {
        let mut events = Vec::new();
        for (repo, cache) in repos {
            let mut authored: BTreeSet<&str> = BTreeSet::new();
            for r in cache.records.values() {
                let author = author_of(r);
                let author_key = author.map(canonical);
                let mut push =
                    |at: Option<Time>, login: &str, kind: Kind, detail: Option<String>| {
                        events.extend(event(repo, r.number, at, login, kind, detail));
                    };
                let merged = r.state == PrState::Merged;
                if let (true, Some(a)) = (merged, author) {
                    push(r.merged_at, a, Kind::AuthoredMerged, None);
                }
                if merged {
                    if let Some(m) = r.contribution.merger.clone().or_else(|| {
                        super::merger(r.contribution.merge_commit.as_ref(), r.merged_by.as_deref())
                    }) {
                        push(r.merged_at, &m, Kind::MergedBy, None);
                    }
                }
                // The work state decides when there is one; otherwise the
                // latest close, since a reopened pull request may have been
                // closed again by hand.
                let landed = match &r.derived.state {
                    Some(d) => d.value == LANDED_ELSEWHERE,
                    None => r
                        .timeline
                        .iter()
                        .rev()
                        .find(|t| t.kind == "ClosedEvent")
                        .is_some_and(|t| t.closer.is_some()),
                };
                if let (PrState::Closed, true, Some(a)) = (r.state, landed, author) {
                    push(r.closed_at, a, Kind::LandedElsewhere, None);
                }
                for t in r.timeline.iter().filter(|t| t.kind == "PullRequestReview") {
                    let Some(actor) = t.actor.as_deref().filter(|_| !t.actor_bot) else {
                        continue;
                    };
                    if Some(canonical(actor)) == author_key || t.state.as_deref() == Some("PENDING")
                    {
                        continue;
                    }
                    push(t.at, actor, Kind::Reviewed, t.state.clone());
                }
                for c in &r.contribution.commits {
                    if c.is_merge || !authored.insert(c.oid.as_str()) {
                        continue;
                    }
                    if let Some(who) = c.author.as_deref() {
                        push(c.authored_at, who, Kind::Committed, None);
                    }
                }
            }
            let mut merged: Vec<&PrRecord> = cache
                .records
                .values()
                .filter(|r| r.state == PrState::Merged)
                .collect();
            merged.sort_by_key(|r| (r.merged_at, r.number));
            let mut credited: BTreeSet<&str> = BTreeSet::new();
            for r in merged {
                let author_key = author_of(r).map(canonical);
                for c in &r.contribution.commits {
                    if c.is_merge || !credited.insert(c.oid.as_str()) {
                        continue;
                    }
                    if let Some(who) = c.author.as_deref() {
                        if Some(canonical(who)) != author_key {
                            let e =
                                event(repo, r.number, r.merged_at, who, Kind::ReviewCommit, None);
                            events.extend(e);
                        }
                    }
                    for login in c.co_authors.iter().filter_map(|co| co.login.as_deref()) {
                        let e = event(repo, r.number, r.merged_at, login, Kind::CoAuthored, None);
                        events.extend(e);
                    }
                }
            }
        }
        events.sort_by(|a, b| {
            (a.at, &a.repo, a.number, a.kind).cmp(&(b.at, &b.repo, b.number, b.kind))
        });
        Ledger { events }
    }

    /// Each person's tally of the events at or after the start `since`
    /// gives their repository; a repository `since` gives `None` for is
    /// left out.
    pub fn tally(&self, since: impl Fn(&str) -> Option<Time>) -> BTreeMap<String, Tally> {
        let mut out: BTreeMap<String, Tally> = BTreeMap::new();
        let mut reviewed: BTreeSet<(&str, &str, u64)> = BTreeSet::new();
        let mut days: BTreeSet<(&str, chrono::NaiveDate)> = BTreeSet::new();
        let mut starts: BTreeMap<&str, Option<Time>> = BTreeMap::new();
        for e in &self.events {
            let start = *starts
                .entry(e.repo.as_str())
                .or_insert_with(|| since(&e.repo));
            if start.is_none_or(|s| e.at < s) {
                continue;
            }
            let t = out.entry(e.login.clone()).or_default();
            match e.kind {
                Kind::AuthoredMerged => t.merged += 1,
                Kind::LandedElsewhere => t.landed_elsewhere += 1,
                Kind::MergedBy => t.merges += 1,
                Kind::Reviewed => {
                    t.reviews += 1;
                    match e.detail.as_deref() {
                        Some("APPROVED") => t.approved += 1,
                        Some("CHANGES_REQUESTED") => t.changes_requested += 1,
                        _ => t.commented += 1,
                    }
                    if reviewed.insert((e.login.as_str(), e.repo.as_str(), e.number)) {
                        t.reviewed_prs += 1;
                    }
                }
                Kind::ReviewCommit => t.review_commits += 1,
                Kind::CoAuthored => t.co_authored += 1,
                Kind::Committed => {}
            }
            let active = matches!(e.kind, Kind::Committed | Kind::MergedBy | Kind::Reviewed);
            if active && days.insert((e.login.as_str(), e.at.date_naive())) {
                t.active_days += 1;
            }
        }
        out
    }

    /// [`Ledger::tally`] for each activity window at `now`: the ids in
    /// [`WINDOWS`], [`SINCE_RELEASE`] (each repository from its own latest
    /// build in `release_at`; repositories without one are left out) and
    /// [`ALL_TIME`]. Window starts are truncated to whole seconds, as the
    /// activity counts' are.
    pub fn windows(
        &self,
        now: Time,
        release_at: &BTreeMap<String, Time>,
    ) -> BTreeMap<String, BTreeMap<String, Tally>> {
        let mut out = BTreeMap::new();
        for (id, minutes) in WINDOWS {
            let start = (now - chrono::Duration::minutes(*minutes)).trunc_subsecs(0);
            out.insert(id.to_string(), self.tally(|_| Some(start)));
        }
        out.insert(
            SINCE_RELEASE.to_string(),
            self.tally(|repo| {
                release_at
                    .iter()
                    .find(|(r, _)| r.eq_ignore_ascii_case(repo))
                    .map(|(_, at)| *at)
            }),
        );
        out.insert(ALL_TIME.to_string(), self.tally(|_| Some(Time::MIN_UTC)));
        out
    }
}

/// A record's author, unless the account is a bot or AI account or deleted.
fn author_of(r: &PrRecord) -> Option<&str> {
    r.author
        .as_deref()
        .filter(|a| !r.author_bot && !is_bot(a, None))
}

/// The event of `kind` for `login` at `at`; none without a time or for a
/// bot or AI account.
fn event(
    repo: &str,
    number: u64,
    at: Option<Time>,
    login: &str,
    kind: Kind,
    detail: Option<String>,
) -> Option<Event> {
    if is_bot(login, None) {
        return None;
    }
    Some(Event {
        at: at?,
        login: canonical(login),
        repo: repo.to_string(),
        number,
        kind,
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::super::record::{
        Closer, CoAuthor, Contribution, Decision, Derived, PrCommit, TimelineItem,
    };
    use super::*;

    fn t(s: &str) -> Time {
        s.parse().unwrap()
    }

    fn review(actor: &str, at: &str, state: &str) -> TimelineItem {
        TimelineItem {
            kind: "PullRequestReview".into(),
            actor: Some(actor.into()),
            at: Some(t(at)),
            state: Some(state.into()),
            ..Default::default()
        }
    }

    fn commit(oid: &str, author: &str, at: &str) -> PrCommit {
        PrCommit {
            oid: oid.into(),
            author: Some(author.into()),
            authored_at: Some(t(at)),
            ..Default::default()
        }
    }

    fn records() -> PrCache {
        let mut c = PrCache::default();
        c.records.insert(
            1,
            PrRecord {
                number: 1,
                state: PrState::Merged,
                author: Some("Ann".into()),
                merged_at: Some(t("2026-10-16T12:00:00Z")),
                merged_by: Some("Maint-One".into()),
                timeline: vec![
                    review("maint-two", "2026-10-16T10:00:00Z", "CHANGES_REQUESTED"),
                    review("maint-two", "2026-10-16T11:00:00Z", "APPROVED"),
                    review("ann", "2026-10-16T11:30:00Z", "COMMENTED"),
                    review("claude", "2026-10-16T11:31:00Z", "COMMENTED"),
                    review("bo", "2026-10-16T11:32:00Z", "PENDING"),
                ],
                contribution: Contribution {
                    commits: vec![
                        PrCommit {
                            co_authors: vec![
                                CoAuthor {
                                    name: "Cy".into(),
                                    email: "cy@users.noreply.github.com".into(),
                                    login: Some("cy".into()),
                                },
                                CoAuthor {
                                    name: "Unlinked".into(),
                                    ..Default::default()
                                },
                            ],
                            ..commit("a1", "ann", "2026-10-15T09:00:00Z")
                        },
                        commit("b1", "maint-two", "2026-10-16T09:00:00Z"),
                        PrCommit {
                            is_merge: true,
                            ..commit("m1", "ann", "2026-10-16T09:30:00Z")
                        },
                    ],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        // Stacked on #1: its first commit is #1's.
        c.records.insert(
            2,
            PrRecord {
                number: 2,
                state: PrState::Open,
                author: Some("ann".into()),
                contribution: Contribution {
                    commits: vec![
                        commit("a1", "ann", "2026-10-15T09:00:00Z"),
                        commit("a2", "ann", "2026-10-17T09:00:00Z"),
                    ],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        c.records.insert(
            3,
            PrRecord {
                number: 3,
                state: PrState::Closed,
                author: Some("bo".into()),
                closed_at: Some(t("2026-10-14T00:00:00Z")),
                timeline: vec![TimelineItem {
                    kind: "ClosedEvent".into(),
                    closer: Some(Closer {
                        kind: "PullRequest".into(),
                        number: Some(1),
                        oid: None,
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        c.records.insert(
            4,
            PrRecord {
                number: 4,
                state: PrState::Closed,
                author: Some("bo".into()),
                closed_at: Some(t("2026-10-13T00:00:00Z")),
                derived: Derived {
                    state: Some(Decision {
                        value: LANDED_ELSEWHERE.into(),
                        rule: "landed".into(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        // Closed by hand: no event. A deleted author: none either.
        c.records.insert(
            5,
            PrRecord {
                number: 5,
                state: PrState::Closed,
                author: Some("bo".into()),
                closed_at: Some(t("2026-10-13T00:00:00Z")),
                ..Default::default()
            },
        );
        c.records.insert(
            6,
            PrRecord {
                number: 6,
                state: PrState::Merged,
                author: None,
                merged_at: Some(t("2026-10-13T00:00:00Z")),
                ..Default::default()
            },
        );
        // #9, still open, holds a commit that #10 merged: #10 gets its credit.
        let shared = commit("s1", "dee", "2026-10-01T00:00:00Z");
        c.records.insert(
            9,
            PrRecord {
                number: 9,
                author: Some("fay".into()),
                contribution: Contribution {
                    commits: vec![shared.clone()],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        // A commit no account claims still credits its linked co-author.
        let unlinked = PrCommit {
            oid: "u1".into(),
            authored_at: Some(t("2026-10-01T01:00:00Z")),
            co_authors: vec![CoAuthor {
                name: "Eve".into(),
                email: "eve@users.noreply.github.com".into(),
                login: Some("eve".into()),
            }],
            ..Default::default()
        };
        c.records.insert(
            10,
            PrRecord {
                number: 10,
                state: PrState::Merged,
                author: Some("fay".into()),
                merged_at: Some(t("2026-10-02T00:00:00Z")),
                contribution: Contribution {
                    commits: vec![shared, unlinked],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        c
    }

    #[test]
    fn events_credit_authors_mergers_reviewers_and_commit_authors() {
        let c = records();
        let ledger = Ledger::build(&[("o/app", &c)]);
        let all = ledger.tally(|_| Some(Time::MIN_UTC));
        let ann = &all["ann"];
        assert_eq!((ann.merged, ann.reviews, ann.review_commits), (1, 0, 0));
        assert_eq!(
            ann.active_days, 2,
            "15th and 17th; the merge commit and the stacked copy are not counted"
        );
        let bf = &all["maint-two"];
        let reviews = (bf.reviews, bf.approved, bf.changes_requested);
        assert_eq!((reviews, bf.reviewed_prs), ((2, 1, 1), 1));
        assert_eq!(bf.review_commits, 1);
        assert_eq!(all["maint-one"].merges, 1, "counted in any letter case");
        assert_eq!(all["cy"].co_authored, 1);
        assert_eq!(all["bo"].landed_elsewhere, 2);
        assert!(!all.contains_key("claude"));
        assert_eq!(all["bo"].reviews, 0, "a pending review is not submitted");
        assert_eq!(all["dee"].review_commits, 1, "credited where it merged");
        assert_eq!(all["eve"].co_authored, 1);
        assert_eq!(all["fay"].merged, 1);
        let committed = ledger
            .events
            .iter()
            .filter(|e| e.kind == Kind::Committed)
            .count();
        assert_eq!(committed, 4, "a1 once, b1, a2 and s1 once");
        assert!(ledger.events.windows(2).all(|w| w[0].at <= w[1].at));
    }

    #[test]
    fn landed_credit_follows_the_work_state_or_else_the_latest_close() {
        let close = |at: &str, by_pr: bool| TimelineItem {
            kind: "ClosedEvent".into(),
            at: Some(t(at)),
            closer: by_pr.then(|| Closer {
                kind: "PullRequest".into(),
                number: Some(1),
                oid: None,
            }),
            ..Default::default()
        };
        let closed = |number: u64, timeline: Vec<TimelineItem>| PrRecord {
            number,
            state: PrState::Closed,
            author: Some("gil".into()),
            closed_at: Some(t("2026-10-14T00:00:00Z")),
            timeline,
            ..Default::default()
        };
        let mut c = PrCache::default();
        // Closed by another pull request, reopened, then closed by hand.
        let reopened = closed(
            1,
            vec![
                close("2026-10-12T00:00:00Z", true),
                close("2026-10-14T00:00:00Z", false),
            ],
        );
        c.records.insert(1, reopened);
        // Closed by another pull request, but the work state says declined.
        let mut declined = closed(2, vec![close("2026-10-14T00:00:00Z", true)]);
        declined.derived.state = Some(Decision {
            value: "closed_declined".into(),
            rule: "declined".into(),
        });
        c.records.insert(2, declined);
        // Closed by hand, then reopened and closed by another pull request.
        let carried = closed(
            3,
            vec![
                close("2026-10-12T00:00:00Z", false),
                close("2026-10-14T00:00:00Z", true),
            ],
        );
        c.records.insert(3, carried);
        let all = Ledger::build(&[("o/app", &c)]).tally(|_| Some(Time::MIN_UTC));
        assert_eq!(all["gil"].landed_elsewhere, 1, "#3 only");
    }

    #[test]
    fn windows_filter_the_events_without_fetching() {
        let c = records();
        let ledger = Ledger::build(&[("o/app", &c)]);
        let now = t("2026-10-17T12:00:00.750Z");
        let mut release = BTreeMap::new();
        release.insert("O/App".to_string(), t("2026-10-16T11:00:00Z"));
        let w = ledger.windows(now, &release);
        assert_eq!(w["1d"]["ann"].merged, 1);
        let bf = &w["1d"]["maint-two"];
        assert_eq!(
            (bf.reviews, bf.review_commits),
            (0, 1),
            "the reviews are over a day old"
        );
        assert!(!w["1d"].contains_key("bo"));
        assert_eq!(w["7d"]["bo"].landed_elsewhere, 2);
        let release = &w["release"]["maint-two"];
        assert_eq!(release.reviews, 1, "from the build at 11:00");
        assert_eq!(w["all"]["maint-two"].reviews, 2);
        // A repository without a build has no release window.
        let none = ledger.windows(now, &BTreeMap::new());
        assert!(none["release"].is_empty());
        // The day window starts at a whole second: 12:00:00, so the
        // approval at exactly 12:00:00 on the 16th would count.
        let at_start = Ledger {
            events: vec![Event {
                at: t("2026-10-16T12:00:00Z"),
                login: "x".into(),
                repo: "o/app".into(),
                number: 9,
                kind: Kind::MergedBy,
                detail: None,
            }],
        };
        assert_eq!(at_start.windows(now, &BTreeMap::new())["1d"]["x"].merges, 1);
    }
}
