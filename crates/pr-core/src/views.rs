//! What the UI reads: per-repo rollups for the root table, rows for the PR
//! tables and everything about one PR. Computed on demand from the store, so the
//! heuristics are always applied as currently written.

use std::collections::HashMap;

use craft_core::{category_of, RepoCategory};
use serde::{Deserialize, Serialize};

use crate::assess::{assess, author_info, landed_counts, Assessment, AuthorInfo};
use crate::config::Config;
use crate::model::{Analysis, Ease, Pr, PrDetail, PrState, RepoData, Tier, Time};
use crate::overlap::Group;
use crate::status_facts::{StatusFacts, StatusRepo};
use crate::store::Store;

/// One row of the root table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RepoRow {
    pub repo: String,
    /// What the repo is: app, library, assets, CI or tooling.
    pub category: RepoCategory,
    pub icon: Option<String>,
    pub open: u64,
    /// The status poller's open count, when the PR poller hasn't listed the repo yet.
    pub open_hint: Option<u64>,
    pub listed: bool,
    /// A first list walk is still under way, so `open` is only so far.
    pub partial: bool,
    pub inspected: u64,
    pub analysed: u64,
    pub approved: u64,
    pub landable: u64,
    pub promising: u64,
    pub reviewable: u64,
    pub ci_not_run: u64,
    pub checking: u64,
    pub not_inspected: u64,
    pub high_quality: u64,
    pub easy: u64,
    pub medium: u64,
    pub hard: u64,
    pub needs_work: u64,
    /// PRs whose headline is Conflicted: conflicts and nothing else known wrong.
    pub conflicted: u64,
    pub draft: u64,
    /// PRs with conflicts, whatever their headline.
    pub conflicts: u64,
    pub ci_failing: u64,
    /// PRs with unresolved critical or major CodeRabbit findings.
    pub rabbit_flagged: u64,
    /// PRs with only minor or trivial ones.
    pub rabbit_nits: u64,
    /// PRs with a security flag: a finding, failed check or sensitive path.
    pub security: u64,
    pub security_nits: u64,
    pub core: u64,
    pub frequent: u64,
    pub new: u64,
    pub bots: u64,
    pub interesting: u64,
    pub groups: u64,
    pub grouped_prs: u64,
    pub oldest_at: Option<Time>,
    pub newest_at: Option<Time>,
    pub list_at: Option<Time>,
    pub error: Option<String>,
    pub inaccessible: bool,
}

/// A PR that may be related to the one it's listed on. Groups are symmetric, so
/// each PR in one links to all the others.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RelatedRef {
    pub number: u64,
    /// duplicate, overlap, stack, theme, issue, files or title.
    pub relation: String,
    pub group: String,
    pub note: String,
    /// claude or local.
    pub source: String,
}

/// The other PRs in the groups `number` belongs to, each once, strongest link first.
pub fn related_refs(groups: &[Group], number: u64) -> Vec<RelatedRef> {
    let mut out: Vec<RelatedRef> = Vec::new();
    for g in groups.iter().filter(|g| g.numbers.contains(&number)) {
        for n in g.numbers.iter().copied().filter(|n| *n != number) {
            if !out.iter().any(|r| r.number == n) {
                out.push(RelatedRef {
                    number: n,
                    relation: g.relation.clone(),
                    group: g.title.clone(),
                    note: g.note.clone(),
                    source: g.source.clone(),
                });
            }
        }
    }
    out
}

/// One row of a PR table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PrRow {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub avatar: String,
    pub tier: Tier,
    pub landed_total: u64,
    pub created_at: Option<Time>,
    pub updated_at: Option<Time>,
    pub is_draft: bool,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub labels: Vec<String>,
    pub review_decision: Option<String>,
    pub approved_by: Vec<String>,
    pub approved: bool,
    pub approval_stale: bool,
    pub conflicted: bool,
    pub mergeable: String,
    pub comments_total: u64,
    pub reactions: u64,
    pub state: PrState,
    pub ease: Ease,
    pub quality: u8,
    pub high_quality: bool,
    pub inspected: bool,
    pub ci: String,
    pub rabbit_flags: u64,
    pub rabbit_nits: u64,
    pub security_flags: u64,
    pub security_nits: u64,
    pub sensitive: u64,
    pub interesting: bool,
    pub summary: String,
    pub analysed: bool,
    pub related: Vec<RelatedRef>,
    pub focus: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RepoView {
    pub row: RepoRow,
    pub prs: Vec<PrRow>,
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PrFull {
    pub repo: String,
    pub icon: Option<String>,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: AuthorInfo,
    pub avatar: String,
    pub association: String,
    pub created_at: Option<Time>,
    pub updated_at: Option<Time>,
    pub is_draft: bool,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub head_ref: String,
    pub base_ref: String,
    pub cross_repo: bool,
    pub labels: Vec<String>,
    pub review_decision: Option<String>,
    pub approved_by: Vec<String>,
    pub mergeable: String,
    pub merge_state: String,
    pub comments_total: u64,
    pub reactions: u64,
    pub assessment: Assessment,
    pub detail: Option<PrDetail>,
    pub detail_current: bool,
    pub analysis: Option<Analysis>,
    pub analysis_current: bool,
    pub groups: Vec<Group>,
    pub related_refs: Vec<RelatedRef>,
    /// Rows for every other PR in those groups.
    pub related: Vec<PrRow>,
}

struct Ctx<'a> {
    cfg: &'a Config,
    totals: HashMap<String, u64>,
    complete: bool,
    /// Per repo: each author's merged PRs. Counting per PR would be quadratic.
    here: HashMap<String, HashMap<String, u64>>,
}

impl<'a> Ctx<'a> {
    fn new(store: &Store, repos: &[String], cfg: &'a Config) -> Self {
        Ctx {
            cfg,
            totals: store.landed_totals(repos),
            complete: store.histories_complete(repos),
            here: repos
                .iter()
                .filter_map(|r| store.repos.get(r).map(|d| (r.clone(), landed_counts(d))))
                .collect(),
        }
    }

    fn judge(&self, pr: &Pr, d: &RepoData) -> (AuthorInfo, Assessment) {
        let login = pr.author.to_lowercase();
        let total = self.totals.get(&login).copied().unwrap_or(0);
        let mut author = author_info(pr, d, total, self.complete, self.cfg);
        author.landed_here = self
            .here
            .get(&d.repo)
            .and_then(|m| m.get(&login))
            .copied()
            .unwrap_or(0);
        let a = assess(pr, &author, self.cfg);
        (author, a)
    }

    fn row(&self, pr: &Pr, d: &RepoData) -> PrRow {
        let (author, a) = self.judge(pr, d);
        PrRow {
            repo: d.repo.clone(),
            number: pr.number,
            title: pr.title.clone(),
            url: pr.url.clone(),
            author: pr.author.clone(),
            avatar: pr.avatar.clone(),
            tier: author.tier,
            landed_total: author.landed_total,
            created_at: pr.created_at,
            updated_at: pr.updated_at,
            is_draft: pr.is_draft,
            additions: pr.additions,
            deletions: pr.deletions,
            changed_files: pr.changed_files,
            labels: pr.labels.clone(),
            review_decision: pr.review_decision.clone(),
            approved_by: pr.approvers(),
            approved: a.approved,
            approval_stale: a.approval_stale,
            conflicted: a.conflicted,
            mergeable: pr.mergeable.clone(),
            comments_total: pr.comments_total,
            reactions: pr.reactions,
            state: a.state,
            ease: a.ease,
            quality: a.quality,
            high_quality: a.high_quality,
            inspected: a.inspected,
            ci: a.ci,
            rabbit_flags: a.rabbit_flags,
            rabbit_nits: a.rabbit_nits,
            security_flags: a.security_flags,
            security_nits: a.security_nits,
            sensitive: a.sensitive_paths.len() as u64,
            interesting: a.interesting,
            summary: pr
                .analysis
                .as_ref()
                .map(|c| c.summary.clone())
                .unwrap_or_default(),
            analysed: pr.analysis_is_current(),
            related: related_refs(&d.groups, pr.number),
            focus: a.focus,
        }
    }

    fn repo_row(&self, d: &RepoData, sib: Option<&StatusRepo>) -> RepoRow {
        let mut r = RepoRow {
            repo: d.repo.clone(),
            category: category_of(&d.repo),
            icon: sib.and_then(|s| s.icon.clone()),
            open: d.prs.len() as u64,
            open_hint: sib.and_then(|s| s.open_prs),
            listed: d.full_list_at.is_some() || !d.prs.is_empty(),
            partial: d.full_list_at.is_none() && !d.prs.is_empty(),
            groups: d.groups.len() as u64,
            grouped_prs: d.groups.iter().map(|g| g.numbers.len() as u64).sum(),
            list_at: d.list_at,
            error: d.error.clone(),
            inaccessible: d.inaccessible,
            ..Default::default()
        };
        for pr in d.prs.values() {
            let (author, a) = self.judge(pr, d);
            r.inspected += a.inspected as u64;
            r.analysed += pr.analysis_is_current() as u64;
            r.approved += a.approved as u64;
            r.high_quality += a.high_quality as u64;
            match a.state {
                PrState::Draft => r.draft += 1,
                PrState::NeedsWork => r.needs_work += 1,
                PrState::Conflicted => r.conflicted += 1,
                PrState::NotInspected => r.not_inspected += 1,
                PrState::Checking => r.checking += 1,
                PrState::CiNotRun => r.ci_not_run += 1,
                PrState::Landable => r.landable += 1,
                PrState::Promising => r.promising += 1,
                PrState::Reviewable => r.reviewable += 1,
            }
            if a.state != PrState::Draft {
                match a.ease {
                    Ease::Easy => r.easy += 1,
                    Ease::Medium => r.medium += 1,
                    Ease::Hard => r.hard += 1,
                }
            }
            r.conflicts += a.conflicted as u64;
            r.ci_failing += (a.ci == "failure") as u64;
            r.rabbit_flagged += (a.rabbit_flags > 0) as u64;
            r.rabbit_nits += (a.rabbit_flags == 0 && a.rabbit_nits > 0) as u64;
            let sec = a.security_flags > 0 || !a.sensitive_paths.is_empty();
            r.security += sec as u64;
            r.security_nits += (!sec && a.security_nits > 0) as u64;
            match author.tier {
                Tier::Core => r.core += 1,
                Tier::Frequent => r.frequent += 1,
                Tier::New => r.new += 1,
                Tier::Bot => r.bots += 1,
                Tier::Regular => {}
            }
            r.interesting += a.interesting as u64;
            r.oldest_at = match (r.oldest_at, pr.created_at) {
                (Some(x), Some(y)) => Some(x.min(y)),
                (x, y) => x.or(y),
            };
            r.newest_at = match (r.newest_at, pr.created_at) {
                (Some(x), Some(y)) => Some(x.max(y)),
                (x, y) => x.or(y),
            };
        }
        r
    }
}

/// A row for each of `repos`, in order, listed or not.
pub fn overview(store: &Store, repos: &[String], cfg: &Config, sib: &StatusFacts) -> Vec<RepoRow> {
    let ctx = Ctx::new(store, repos, cfg);
    repos
        .iter()
        .map(|repo| match store.repos.get(repo) {
            Some(d) => ctx.repo_row(d, sib.repos.get(repo)),
            None => {
                let s = sib.repos.get(repo);
                RepoRow {
                    repo: repo.clone(),
                    category: category_of(repo),
                    icon: s.and_then(|s| s.icon.clone()),
                    open_hint: s.and_then(|s| s.open_prs),
                    ..Default::default()
                }
            }
        })
        .collect()
}

/// One repo's PRs, best first.
pub fn repo_view(
    store: &Store,
    repo: &str,
    repos: &[String],
    cfg: &Config,
    sib: &StatusFacts,
) -> Option<RepoView> {
    let d = store.repos.get(repo)?;
    let ctx = Ctx::new(store, repos, cfg);
    let mut prs: Vec<PrRow> = d.prs.values().map(|p| ctx.row(p, d)).collect();
    prs.sort_by_key(|p| std::cmp::Reverse(p.focus));
    Some(RepoView {
        row: ctx.repo_row(d, sib.repos.get(repo)),
        prs,
        groups: d.groups.clone(),
    })
}

/// The PRs someone can act on across `repos`, best first, up to `limit`.
pub fn queue(store: &Store, repos: &[String], cfg: &Config, limit: usize) -> Vec<PrRow> {
    let ctx = Ctx::new(store, repos, cfg);
    let mut rows: Vec<PrRow> = repos
        .iter()
        .filter_map(|r| store.repos.get(r))
        .flat_map(|d| d.prs.values().map(|p| ctx.row(p, d)).collect::<Vec<_>>())
        .filter(|r| r.state.is_queued())
        .collect();
    rows.sort_by_key(|p| std::cmp::Reverse(p.focus));
    rows.truncate(limit);
    rows
}

pub fn pr_full(
    store: &Store,
    repo: &str,
    number: u64,
    repos: &[String],
    cfg: &Config,
    sib: &StatusFacts,
) -> Option<PrFull> {
    let d = store.repos.get(repo)?;
    let pr = d.prs.get(&number)?;
    let ctx = Ctx::new(store, repos, cfg);
    let (author, assessment) = ctx.judge(pr, d);
    let groups: Vec<Group> = d
        .groups
        .iter()
        .filter(|g| g.numbers.contains(&number))
        .cloned()
        .collect();
    let mut related: Vec<PrRow> = groups
        .iter()
        .flat_map(|g| g.numbers.iter())
        .filter(|n| **n != number)
        .filter_map(|n| d.prs.get(n))
        .map(|p| ctx.row(p, d))
        .collect();
    related.sort_by_key(|r| r.number);
    related.dedup_by_key(|r| r.number);
    Some(PrFull {
        repo: repo.to_string(),
        icon: sib.repos.get(repo).and_then(|s| s.icon.clone()),
        number,
        title: pr.title.clone(),
        url: pr.url.clone(),
        author,
        avatar: pr.avatar.clone(),
        association: pr.association.clone(),
        created_at: pr.created_at,
        updated_at: pr.updated_at,
        is_draft: pr.is_draft,
        additions: pr.additions,
        deletions: pr.deletions,
        changed_files: pr.changed_files,
        head_ref: pr.head_ref.clone(),
        base_ref: pr.base_ref.clone(),
        cross_repo: pr.cross_repo,
        labels: pr.labels.clone(),
        review_decision: pr.review_decision.clone(),
        approved_by: pr.approvers(),
        mergeable: pr.mergeable.clone(),
        merge_state: pr.merge_state.clone(),
        comments_total: pr.comments_total,
        reactions: pr.reactions,
        assessment,
        detail: pr.detail.clone(),
        detail_current: pr.current_detail().is_some(),
        analysis: pr.analysis.clone(),
        analysis_current: pr.analysis_is_current(),
        related_refs: related_refs(&d.groups, number),
        groups,
        related,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Finding, PrDetail, Severity};

    fn store() -> (Store, Vec<String>) {
        let dir = std::env::temp_dir().join(format!("pr-views-{}-nope", std::process::id()));
        let mut s = Store::load(&dir);
        let d = s.repo_mut("o/r");
        d.full_list_at = Some(chrono::Utc::now());
        d.history
            .merged
            .extend((0..40).map(|n| (n, "fay".to_string())));
        let base = |number: u64, author: &str| Pr {
            number,
            title: format!("Change {number}"),
            author: author.into(),
            association: "CONTRIBUTOR".into(),
            mergeable: "MERGEABLE".into(),
            head_oid: "h".into(),
            additions: 10,
            changed_files: 1,
            ci_rollup: Some("SUCCESS".into()),
            ..Default::default()
        };
        let mut good = base(1, "fay");
        good.detail = Some(PrDetail {
            for_head: "h".into(),
            checks: vec![crate::model::Check {
                name: "test".into(),
                state: "SUCCESS".into(),
                url: None,
            }],
            body: "x".repeat(300),
            files: vec![crate::model::FileChange {
                path: "tests/a.rs".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut flagged = base(2, "gus");
        flagged.detail = Some(PrDetail {
            for_head: "h".into(),
            findings: vec![
                Finding {
                    severity: Severity::Major,
                    security: true,
                    ..Default::default()
                },
                Finding {
                    severity: Severity::Minor,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let mut draft = base(3, "gus");
        draft.is_draft = true;
        let mut conflict = base(5, "gus");
        conflict.mergeable = "CONFLICTING".into();
        d.prs.insert(1, good);
        d.prs.insert(2, flagged);
        d.prs.insert(3, draft);
        d.prs.insert(4, base(4, "new"));
        d.prs.insert(5, conflict);
        (s, vec!["o/r".to_string(), "o/unlisted".to_string()])
    }

    #[test]
    fn overview_says_what_each_repo_is_listed_or_not() {
        let (mut s, _) = store();
        s.repo_mut("storytold/craft-fonts");
        let repos = vec![
            "storytold/craft-fonts".to_string(),
            "storytold/craft-libs".to_string(),
            "someone/else".to_string(),
        ];
        let rows = overview(&s, &repos, &Config::default(), &StatusFacts::default());
        assert_eq!(rows[0].category, RepoCategory::Assets);
        assert_eq!(rows[1].category, RepoCategory::Library);
        assert_eq!(rows[2].category, RepoCategory::Other);
    }

    #[test]
    fn overview_rolls_up_each_repo() {
        let (s, repos) = store();
        let mut sib = StatusFacts::default();
        sib.repos.insert(
            "o/unlisted".into(),
            StatusRepo {
                open_prs: Some(12),
                ..Default::default()
            },
        );
        let rows = overview(&s, &repos, &Config::default(), &sib);
        assert_eq!(rows.len(), 2);
        let r = &rows[0];
        assert_eq!((r.open, r.inspected), (5, 2));
        assert_eq!(
            (
                r.promising,
                r.needs_work,
                r.not_inspected,
                r.conflicted,
                r.draft
            ),
            (1, 1, 1, 1, 1)
        );
        assert_eq!((r.rabbit_flagged, r.security, r.conflicts), (1, 1, 1));
        assert_eq!(r.frequent, 1);
        assert_eq!(r.easy + r.medium + r.hard, 4, "drafts aren't graded");
        assert_eq!((rows[1].listed, rows[1].open_hint), (false, Some(12)));
    }

    #[test]
    fn approvers_are_each_reviewers_latest_verdict() {
        use crate::model::Review;
        let review = |author: &str, state: &str| Review {
            author: author.into(),
            state: state.into(),
            at: None,
        };
        let mut p = Pr {
            detail: Some(PrDetail {
                reviews: vec![
                    review("ann", "APPROVED"),
                    review("bo", "APPROVED"),
                    review("bo", "CHANGES_REQUESTED"),
                    review("cy", "CHANGES_REQUESTED"),
                    review("cy", "APPROVED"),
                    review("cy", "COMMENTED"),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(p.approvers(), ["ann", "cy"]);
        p.approved_by = vec!["di".into()];
        assert_eq!(p.approvers(), ["di"], "GitHub's own answer wins");
        assert!(Pr::default().approvers().is_empty());
    }

    #[test]
    fn a_walk_in_progress_shows_what_it_has() {
        let (mut s, repos) = store();
        s.repo_mut("o/r").full_list_at = None;
        let r = &overview(&s, &repos, &Config::default(), &StatusFacts::default())[0];
        assert!(r.listed && r.partial);
        assert_eq!(r.open, 5);
    }

    #[test]
    fn repo_view_sorts_by_status_and_the_queue_holds_what_can_be_acted_on() {
        let (s, repos) = store();
        let cfg = Config::default();
        let v = repo_view(&s, "o/r", &repos, &cfg, &StatusFacts::default()).unwrap();
        let order: Vec<u64> = v.prs.iter().map(|p| p.number).collect();
        assert_eq!(
            order,
            [1, 4, 5, 2, 3],
            "promising, not inspected, conflicted, needs work, draft"
        );
        assert_eq!(v.prs[0].tier, Tier::Frequent);
        let q = queue(&s, &repos, &cfg, 10);
        assert_eq!(q.iter().map(|r| r.number).collect::<Vec<_>>(), vec![1]);
        assert!(repo_view(&s, "o/none", &repos, &cfg, &StatusFacts::default()).is_none());
    }

    #[test]
    fn related_links_go_both_ways_each_listed_once() {
        let groups = vec![
            Group {
                id: "c0".into(),
                relation: "duplicate".into(),
                title: "Tabs".into(),
                numbers: vec![1, 2],
                source: "claude".into(),
                ..Default::default()
            },
            Group {
                id: "l0".into(),
                relation: "issue".into(),
                numbers: vec![1, 2, 3],
                source: "local".into(),
                ..Default::default()
            },
        ];
        let of = |n| {
            related_refs(&groups, n)
                .iter()
                .map(|r| r.number)
                .collect::<Vec<_>>()
        };
        assert_eq!(of(1), [2, 3]);
        assert_eq!(of(2), [1, 3]);
        assert_eq!(of(3), [1, 2]);
        assert!(of(9).is_empty());
        assert_eq!(
            related_refs(&groups, 1)[0].relation,
            "duplicate",
            "the stronger link wins"
        );
        for a in 1..=3u64 {
            for b in of(a) {
                assert!(of(b).contains(&a), "#{b} must link back to #{a}");
            }
        }
    }

    #[test]
    fn pr_full_includes_groups_and_related_rows() {
        let (mut s, repos) = store();
        let d = s.repo_mut("o/r");
        d.groups = vec![Group {
            id: "c0".into(),
            relation: "overlap".into(),
            numbers: vec![1, 2, 3],
            ..Default::default()
        }];
        let f = pr_full(
            &s,
            "o/r",
            1,
            &repos,
            &Config::default(),
            &StatusFacts::default(),
        )
        .unwrap();
        assert_eq!(f.author.landed_total, 40);
        assert_eq!(f.groups.len(), 1);
        assert_eq!(
            f.related.iter().map(|r| r.number).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(f.related_refs.len(), 2);
        assert!(pr_full(
            &s,
            "o/r",
            99,
            &repos,
            &Config::default(),
            &StatusFacts::default()
        )
        .is_none());
    }
}
