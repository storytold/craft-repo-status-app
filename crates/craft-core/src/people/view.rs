//! What the People tab shows ([`PeopleView`]): every open pull request with
//! its work state ([`Item`]), and per person their flow, the reviews and
//! comments they gave, and response times, each by activity window and by
//! repository. Open work by state, time in state and who each item waits on
//! are worked out by the UI from the items.

use std::collections::{BTreeMap, BTreeSet};

use chrono::SubsecRound;
use serde::{Deserialize, Serialize};

use super::rank::{Direct, RankInput, RankRow, Rankings};
use super::record::{Decision, MainChecks, PrCache, PrState};
use super::roster::RosterRow;
use super::states::{self, Ctx, MergeAt, WorkState};
use super::{canonical, directory, is_bot, maintainers, Directory, RepoPeople};
use crate::config::PeopleConfig;
use crate::model::{Time, ALL_TIME, SINCE_RELEASE, WINDOWS};

/// The key of the figures that cover every repository.
pub const ALL_REPOS: &str = "*";

/// One repository's records and settings.
pub struct RepoInput<'a> {
    pub repo: &'a str,
    pub cache: &'a PrCache,
    pub maintainers: &'a BTreeSet<String>,
    pub owners: &'a [String],
    /// See [`Ctx::rebase`].
    pub rebase: bool,
    /// When the latest build was published, for the since-release window.
    pub release_at: Option<Time>,
}

impl<'a> RepoInput<'a> {
    /// The rules' context, with `merges` from [`states::merge_index`].
    fn ctx<'b>(&'b self, now: Time, merges: &'b [MergeAt]) -> Ctx<'b> {
        Ctx {
            repo: self.repo,
            maintainers: self.maintainers,
            owners: self.owners,
            rebase: self.rebase,
            main: self.cache.main_checks.as_ref(),
            cache: self.cache,
            merges,
            now,
        }
    }
}

/// The work state of every record of one repository, by number.
pub fn classify_repo(input: &RepoInput, now: Time) -> BTreeMap<u64, WorkState> {
    let merges = states::merge_index(input.cache);
    let ctx = input.ctx(now, &merges);
    input
        .cache
        .records
        .iter()
        .map(|(n, r)| (*n, states::classify(r, &ctx)))
        .collect()
}

/// The view, the directory and the rankings for `caches` (`(repo,
/// records)`): each repository's maintainers, the work state of every
/// record (also kept in its `derived.state`, which the ledger reads), the
/// directory of people with the roster, [`build`]'s view, and the rankings
/// with each merged record's type and area (also kept in its `derived`).
/// `release_at` gives a repository's latest build time, and `direct` its
/// commits outside pull requests, by repository in any letter case.
pub fn assemble(
    caches: &mut [(String, PrCache)],
    people: &PeopleConfig,
    roster: &[RosterRow],
    release_at: impl Fn(&str) -> Option<Time>,
    direct: &BTreeMap<String, Direct>,
    now: Time,
) -> (PeopleView, Directory, Rankings) {
    let sets: Vec<BTreeSet<String>> = caches
        .iter()
        .map(|(_, c)| maintainers(c.records.values(), &people.maintainers, now))
        .collect();
    let works: Vec<BTreeMap<u64, WorkState>> = caches
        .iter()
        .zip(&sets)
        .map(|((repo, cache), m)| {
            let input = RepoInput {
                repo,
                cache,
                maintainers: m,
                owners: people.owners_for(repo),
                rebase: people.rebases(repo),
                release_at: None,
            };
            classify_repo(&input, now)
        })
        .collect();
    for ((_, cache), w) in caches.iter_mut().zip(&works) {
        for (n, ws) in w {
            if let Some(r) = cache.records.get_mut(n) {
                r.derived.state = Some(Decision {
                    value: ws.state.clone(),
                    rule: ws.rule.clone(),
                });
            }
        }
    }
    let repos: Vec<RepoPeople> = caches
        .iter()
        .zip(&sets)
        .map(|((repo, cache), m)| RepoPeople {
            repo,
            cache,
            owners: people.owners_for(repo),
            maintainers: m,
        })
        .collect();
    let dir = directory(&repos, roster);
    let inputs: Vec<RepoInput> = caches
        .iter()
        .zip(&sets)
        .map(|((repo, cache), m)| RepoInput {
            repo,
            cache,
            maintainers: m,
            owners: people.owners_for(repo),
            rebase: people.rebases(repo),
            release_at: release_at(repo),
        })
        .collect();
    let mut view = build(&inputs, &works, &dir, now);
    let rank_inputs: Vec<RankInput> = caches
        .iter()
        .zip(&works)
        .map(|((repo, cache), w)| RankInput {
            repo,
            cache,
            works: w,
            release_at: release_at(repo),
            batch: people.batches(repo),
            direct: direct
                .iter()
                .find(|(r, _)| r.eq_ignore_ascii_case(repo))
                .map(|(_, d)| d.clone()),
        })
        .collect();
    let rankings = Rankings::new(&rank_inputs, &dir, now);
    (view.rankings, view.co_unlinked) = rankings.rows(None);
    view.unlinked = dir
        .unlinked
        .iter()
        .map(|r| {
            r.name
                .clone()
                .or_else(|| r.discord.clone())
                .unwrap_or_default()
        })
        .collect();
    let held = |state: PrState| -> u64 {
        caches
            .iter()
            .map(|(_, c)| c.records.values().filter(|r| r.state == state).count() as u64)
            .sum()
    };
    view.load = Load {
        merged: held(PrState::Merged),
        total: caches
            .iter()
            .map(|(_, c)| c.merged_walk.total)
            .sum::<Option<u64>>(),
        closed: held(PrState::Closed),
        closed_total: caches
            .iter()
            .map(|(_, c)| c.closed_walk.total)
            .sum::<Option<u64>>(),
        done: caches
            .iter()
            .all(|(_, c)| c.merged_walk.done && c.closed_walk.done),
    };
    for c in &rankings.credits {
        let Some((_, cache)) = caches.iter_mut().find(|(r, _)| *r == c.repo) else {
            continue;
        };
        if let Some(r) = cache.records.get_mut(&c.number) {
            r.derived.kind = Some(c.kind.kind.clone());
            r.derived.area = Some(c.kind.area.clone());
        }
    }
    (view, dir, rankings)
}

/// How far the first load of merged and closed pull requests has come.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Load {
    /// Merged pull requests the records hold.
    pub merged: u64,
    /// Merged pull requests GitHub reports across the repositories; `None`
    /// until every repository's walk has read a page.
    pub total: Option<u64>,
    /// Pull requests closed without merging that the records hold.
    pub closed: u64,
    /// Closed pull requests GitHub reports across the repositories; `None`
    /// until every repository's closed walk has read a page.
    pub closed_total: Option<u64>,
    /// Every repository's merged and closed walks have finished.
    pub done: bool,
}

/// What one repository's pull requests did in one window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RepoFlow {
    pub merged: u64,
    /// Merged pull requests by subtype (`direct`, `batch`, `maintainer_followups`).
    pub merged_by: BTreeMap<String, u64>,
    pub landed_elsewhere: u64,
    /// Closed without merging, by state; folded parents and accidental
    /// quick closes are left out.
    pub closed: BTreeMap<String, u64>,
    pub median_merge_secs: Option<i64>,
    /// From ready to the first maintainer comment, review or merge.
    pub median_response_secs: Option<i64>,
    /// Merges by method: `merge commit` (two parents), `squash or rebase`
    /// (one) or `unknown`.
    pub methods: BTreeMap<String, u64>,
    /// Merges by who landed them.
    pub mergers: BTreeMap<String, u64>,
}

/// One open pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Item {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// The author's canonical login.
    pub author: Option<String>,
    pub created_at: Option<Time>,
    pub work: WorkState,
}

/// What a person's pull requests did in one window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Flow {
    pub merged: u64,
    /// Merged pull requests by subtype (`direct`, `batch`, `maintainer_followups`).
    pub merged_by: BTreeMap<String, u64>,
    pub landed_elsewhere: u64,
    /// Pull requests closed without merging, by state. Folded parents and
    /// accidental quick closes are left out.
    pub closed: BTreeMap<String, u64>,
    /// Merged, landed elsewhere and counted closes: the landed rate's divisor.
    pub closed_total: u64,
    pub median_merge_secs: Option<i64>,
    /// From `max(createdAt, the last ReadyForReviewEvent)` to the merge.
    pub median_ready_secs: Option<i64>,
}

/// What a person gave on other people's pull requests in one window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Given {
    pub comments: u64,
    pub reviews: u64,
    pub approved: u64,
    pub changes_requested: u64,
    /// Distinct pull requests commented on or reviewed.
    pub prs: u64,
    /// Comments and reviews holding a request phrase, and change requests.
    pub requests: u64,
    pub holds: u64,
    pub merges: u64,
    pub closes: u64,
}

/// Response times on a person's pull requests, over all time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Response {
    /// Their median time from a maintainer request to their answer.
    pub their_median_secs: Option<i64>,
    pub their_answered: u64,
    /// Requests on their open pull requests still unanswered, and the oldest.
    pub unanswered: u64,
    pub oldest_unanswered: Option<Time>,
    /// The median time from ready to the first maintainer comment, review or merge.
    pub m_median_secs: Option<i64>,
    pub m_answered: u64,
    /// Their open pull requests (not drafts) with no maintainer response yet.
    pub awaiting_first: u64,
    pub oldest_awaiting: Option<Time>,
}

/// One person's row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersonRow {
    /// The canonical login, as items name people.
    pub key: String,
    pub login: String,
    pub name: Option<String>,
    pub on_roster: bool,
    pub maintainer_of: Vec<String>,
    pub owner_of: Vec<String>,
    /// By window id, then by repository or [`ALL_REPOS`].
    pub flow: BTreeMap<String, BTreeMap<String, Flow>>,
    pub given: BTreeMap<String, BTreeMap<String, Given>>,
    /// By repository or [`ALL_REPOS`].
    pub response: BTreeMap<String, Response>,
}

/// Everything the People tab shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PeopleView {
    pub generated_at: Option<Time>,
    /// Every open pull request, by repository and number.
    pub items: Vec<Item>,
    /// People with open work, work waiting on them, or anything in a window.
    pub people: Vec<PersonRow>,
    /// Records held, and how many have their timeline fetched.
    pub records: u64,
    pub fetched: u64,
    /// One row per person for the Rankings view.
    pub rankings: Vec<RankRow>,
    /// By window, the names in `Co-authored-by:` trailers that match no
    /// account, with how many commits name them.
    pub co_unlinked: BTreeMap<String, BTreeMap<String, u64>>,
    /// Roster rows without a GitHub login, by name.
    pub unlinked: Vec<String>,
    pub load: Load,
    /// Each repository's flow, by repository and window.
    pub repos: BTreeMap<String, BTreeMap<String, RepoFlow>>,
    /// The checks on each repository's default branch.
    pub main: BTreeMap<String, MainChecks>,
    /// When each repository's records were last brought up to date (see
    /// `PrCache::checked_at`); `None` for a repository never checked.
    pub checked: BTreeMap<String, Option<Time>>,
}

/// The start of each window for `repo`; a repository without a build has
/// no since-release window.
pub(super) fn window_starts(now: Time, release_at: Option<Time>) -> Vec<(&'static str, Time)> {
    let mut out: Vec<(&'static str, Time)> = WINDOWS
        .iter()
        .map(|(id, minutes)| {
            (
                *id,
                (now - chrono::Duration::minutes(*minutes)).trunc_subsecs(0),
            )
        })
        .collect();
    if let Some(at) = release_at {
        out.push((SINCE_RELEASE, at));
    }
    out.push((ALL_TIME, Time::MIN_UTC));
    out
}

fn median(v: &mut [i64]) -> Option<i64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let mid = v.len() / 2;
    Some(if v.len() % 2 == 1 {
        v[mid]
    } else {
        (v[mid - 1] + v[mid]) / 2
    })
}

#[derive(Default)]
struct FlowAcc {
    flow: Flow,
    merge: Vec<i64>,
    ready: Vec<i64>,
}

#[derive(Default)]
struct GivenAcc {
    given: Given,
    prs: BTreeSet<(String, u64)>,
}

#[derive(Default)]
struct ResponseAcc {
    response: Response,
    theirs: Vec<i64>,
    m: Vec<i64>,
}

#[derive(Default)]
struct RepoFlowAcc {
    flow: RepoFlow,
    merge: Vec<i64>,
    response: Vec<i64>,
}

#[derive(Default)]
struct PersonAcc {
    flow: BTreeMap<(String, String), FlowAcc>,
    given: BTreeMap<(String, String), GivenAcc>,
    response: BTreeMap<String, ResponseAcc>,
    active: bool,
}

/// Builds the view from each repository's records and their work states
/// (`states[i]` for `repos[i]`, from [`classify_repo`]) and the directory.
pub fn build(
    repos: &[RepoInput],
    states: &[BTreeMap<u64, WorkState>],
    dir: &Directory,
    now: Time,
) -> PeopleView {
    let mut view = PeopleView {
        generated_at: Some(now),
        ..Default::default()
    };
    let mut acc: BTreeMap<String, PersonAcc> = BTreeMap::new();
    let mut repo_acc: BTreeMap<(String, &'static str), RepoFlowAcc> = BTreeMap::new();
    let empty = BTreeMap::new();
    for (i, input) in repos.iter().enumerate() {
        let repo = input.repo;
        view.checked
            .insert(repo.to_string(), input.cache.checked_at);
        if let Some(m) = &input.cache.main_checks {
            view.main.insert(repo.to_string(), m.clone());
        }
        let works = states.get(i).unwrap_or(&empty);
        let starts = window_starts(now, input.release_at);
        let merges = states::merge_index(input.cache);
        let ctx = input.ctx(now, &merges);
        let in_windows = |at: Option<Time>| -> Vec<&'static str> {
            at.map(|at| {
                starts
                    .iter()
                    .filter(|(_, s)| at >= *s)
                    .map(|(id, _)| *id)
                    .collect()
            })
            .unwrap_or_default()
        };
        for r in input.cache.records.values() {
            view.records += 1;
            if r.fetched_at.is_some() {
                view.fetched += 1;
            }
            let Some(work) = works.get(&r.number) else {
                continue;
            };
            let author = r.author.as_deref().map(canonical);
            let human_author = author
                .clone()
                .filter(|_| !r.author_bot && !r.author.as_deref().is_some_and(|a| is_bot(a, None)));
            // A record no deep fetch has read holds no timeline to time.
            let tm = human_author
                .as_ref()
                .filter(|_| r.fetched_at.is_some())
                .map(|_| states::timings(r, &ctx));
            if r.state == PrState::Open {
                view.items.push(Item {
                    repo: repo.to_string(),
                    number: r.number,
                    title: r.title.clone(),
                    // A record no deep fetch has read holds no link yet.
                    url: match r.url.is_empty() {
                        true => format!("https://github.com/{repo}/pull/{}", r.number),
                        false => r.url.clone(),
                    },
                    author: author.clone(),
                    created_at: r.created_at,
                    work: work.clone(),
                });
                if let Some(a) = &human_author {
                    acc.entry(a.clone()).or_default().active = true;
                }
                for w in &work.waits_for {
                    acc.entry(w.clone()).or_default().active = true;
                }
            }

            // The repository's flow: every merged and closed pull request.
            if matches!(r.state, PrState::Merged | PrState::Closed) {
                let landed = work.state == super::ledger::LANDED_ELSEWHERE;
                let skip = work.state == "closed_folded_split_or_moved"
                    || work.subtype.as_deref() == Some("accidental_quick_close");
                let merger = r
                    .contribution
                    .merger
                    .clone()
                    .or_else(|| r.merged_by.clone())
                    .map(|m| canonical(&m));
                let method = match r.contribution.merge_commit.as_ref().map(|m| m.parents) {
                    Some(2) => "merge commit",
                    Some(1) => "squash or rebase",
                    _ => "unknown",
                };
                for id in in_windows(r.merged_at.or(r.closed_at)) {
                    let f = repo_acc.entry((repo.to_string(), id)).or_default();
                    if r.state == PrState::Merged {
                        f.flow.merged += 1;
                        *f.flow
                            .merged_by
                            .entry(work.subtype.clone().unwrap_or_default())
                            .or_default() += 1;
                        *f.flow.methods.entry(method.to_string()).or_default() += 1;
                        if let Some(m) = &merger {
                            *f.flow.mergers.entry(m.clone()).or_default() += 1;
                        }
                        if let (Some(m), Some(c)) = (r.merged_at, r.created_at) {
                            f.merge.push((m - c).num_seconds());
                        }
                    } else if landed {
                        f.flow.landed_elsewhere += 1;
                    } else if !skip {
                        *f.flow.closed.entry(work.state.clone()).or_default() += 1;
                    }
                    if let Some((Some(ready), Some(first))) =
                        tm.as_ref().map(|tm| (tm.ready_at, tm.first_response))
                    {
                        f.response.push((first - ready).num_seconds().max(0));
                    }
                }
            }

            // Flow: the author's merged and closed pull requests.
            if let (Some(a), PrState::Merged | PrState::Closed) = (&human_author, r.state) {
                let at = r.merged_at.or(r.closed_at);
                let landed = work.state == super::ledger::LANDED_ELSEWHERE;
                let skip = work.state == "closed_folded_split_or_moved"
                    || work.subtype.as_deref() == Some("accidental_quick_close");
                let p = acc.entry(a.clone()).or_default();
                for id in in_windows(at) {
                    for key in [repo, ALL_REPOS] {
                        let f = p.flow.entry((id.to_string(), key.to_string())).or_default();
                        if r.state == PrState::Merged {
                            f.flow.merged += 1;
                            *f.flow
                                .merged_by
                                .entry(work.subtype.clone().unwrap_or_default())
                                .or_default() += 1;
                            f.flow.closed_total += 1;
                            if let (Some(m), Some(c)) = (r.merged_at, r.created_at) {
                                f.merge.push((m - c).num_seconds());
                            }
                            if let (Some(m), Some(ready)) =
                                (r.merged_at, tm.as_ref().and_then(|tm| tm.ready_at))
                            {
                                f.ready.push((m - ready).num_seconds());
                            }
                        } else if landed {
                            f.flow.landed_elsewhere += 1;
                            f.flow.closed_total += 1;
                        } else if !skip {
                            *f.flow.closed.entry(work.state.clone()).or_default() += 1;
                            f.flow.closed_total += 1;
                        }
                    }
                }
            }

            // Response times on the author's pull requests.
            if let (Some(a), Some(tm)) = (&human_author, &tm) {
                let p = acc.entry(a.clone()).or_default();
                for key in [repo, ALL_REPOS] {
                    let resp = p.response.entry(key.to_string()).or_default();
                    for q in &tm.requests {
                        match (q.at, q.answered) {
                            (Some(at), Some(ans)) => {
                                resp.theirs.push((ans - at).num_seconds());
                                resp.response.their_answered += 1;
                            }
                            (Some(at), None) if r.state == PrState::Open && !q.resolved => {
                                resp.response.unanswered += 1;
                                resp.response.oldest_unanswered =
                                    Some(resp.response.oldest_unanswered.map_or(at, |o| o.min(at)));
                            }
                            _ => {}
                        }
                    }
                    match (tm.ready_at, tm.first_response) {
                        (Some(ready), Some(first)) => {
                            resp.m.push((first - ready).num_seconds().max(0));
                            resp.response.m_answered += 1;
                        }
                        (Some(ready), None) if r.state == PrState::Open && !r.is_draft => {
                            resp.response.awaiting_first += 1;
                            resp.response.oldest_awaiting = Some(
                                resp.response
                                    .oldest_awaiting
                                    .map_or(ready, |o| o.min(ready)),
                            );
                        }
                        _ => {}
                    }
                }
            }

            // What people gave on someone else's pull request.
            // `reviewed`: a comment or review, which counts the pull request
            // among those they reviewed; merges and closes do not.
            let mut give = |who: &str, at: Option<Time>, reviewed: bool, f: &dyn Fn(&mut Given)| {
                if is_bot(who, None) {
                    return;
                }
                let key = canonical(who);
                if author.as_deref() == Some(key.as_str()) {
                    return;
                }
                let p = acc.entry(key).or_default();
                for id in in_windows(at) {
                    for k in [repo, ALL_REPOS] {
                        let g = p.given.entry((id.to_string(), k.to_string())).or_default();
                        f(&mut g.given);
                        if reviewed && g.prs.insert((repo.to_string(), r.number)) {
                            g.given.prs += 1;
                        }
                    }
                }
            };
            for t in &r.timeline {
                let Some(who) = t.actor.as_deref().filter(|_| !t.actor_bot) else {
                    continue;
                };
                let said = states::phrases(t.body.as_deref().unwrap_or(""));
                match t.kind.as_str() {
                    "IssueComment" => give(who, t.at, true, &|g| {
                        g.comments += 1;
                        g.requests += u64::from(said.request);
                        g.holds += u64::from(said.hold);
                    }),
                    "PullRequestReview" if t.state.as_deref() != Some("PENDING") => {
                        let state = t.state.clone().unwrap_or_default();
                        give(who, t.at, true, &|g| {
                            g.reviews += 1;
                            g.approved += u64::from(state == "APPROVED");
                            g.changes_requested += u64::from(state == "CHANGES_REQUESTED");
                            g.requests += u64::from(said.request || state == "CHANGES_REQUESTED");
                            g.holds += u64::from(said.hold);
                        })
                    }
                    "ClosedEvent" if r.state == PrState::Closed => {
                        give(who, t.at, false, &|g| g.closes += 1)
                    }
                    _ => {}
                }
            }
            if r.state == PrState::Merged {
                let merger = r
                    .contribution
                    .merger
                    .clone()
                    .or_else(|| r.merged_by.clone());
                if let Some(m) = merger {
                    give(&m, r.merged_at, false, &|g| g.merges += 1);
                }
            }
        }
    }
    view.items
        .sort_by(|a, b| (&a.repo, a.number).cmp(&(&b.repo, b.number)));
    for ((repo, id), mut f) in repo_acc {
        f.flow.median_merge_secs = median(&mut f.merge);
        f.flow.median_response_secs = median(&mut f.response);
        view.repos
            .entry(repo)
            .or_default()
            .insert(id.to_string(), f.flow);
    }

    for (key, mut p) in acc {
        let person = dir.people.get(&key);
        if person.is_some_and(|p| p.bot) || is_bot(&key, None) {
            continue;
        }
        let mut row = PersonRow {
            key: key.clone(),
            login: person.map_or(key.clone(), |p| p.login.clone()),
            name: person.and_then(|p| p.name.clone()),
            on_roster: person.is_some_and(|p| p.on_roster),
            maintainer_of: person.map(|p| p.maintainer_of.clone()).unwrap_or_default(),
            owner_of: person.map(|p| p.owner_of.clone()).unwrap_or_default(),
            ..Default::default()
        };
        for ((id, repo), mut f) in std::mem::take(&mut p.flow) {
            f.flow.median_merge_secs = median(&mut f.merge);
            f.flow.median_ready_secs = median(&mut f.ready);
            row.flow.entry(id).or_default().insert(repo, f.flow);
        }
        for ((id, repo), g) in std::mem::take(&mut p.given) {
            row.given.entry(id).or_default().insert(repo, g.given);
        }
        for (repo, mut r) in std::mem::take(&mut p.response) {
            r.response.their_median_secs = median(&mut r.theirs);
            r.response.m_median_secs = median(&mut r.m);
            row.response.insert(repo, r.response);
        }
        if p.active || !row.flow.is_empty() || !row.given.is_empty() {
            view.people.push(row);
        }
    }
    view
}

#[cfg(test)]
mod tests {
    use super::super::record::{Contribution, FileLines, PrCommit, PrRecord, TimelineItem, Walk};
    use super::*;

    fn t(day: u32, hour: u32) -> Time {
        format!("2026-10-{day:02}T{hour:02}:00:00Z")
            .parse()
            .unwrap()
    }

    fn comment(who: &str, at: Time, body: &str) -> TimelineItem {
        TimelineItem {
            id: format!("c-{who}-{at}"),
            kind: "IssueComment".into(),
            at: Some(at),
            actor: Some(who.into()),
            body: Some(body.into()),
            ..Default::default()
        }
    }

    fn record(number: u64, author: &str, state: PrState) -> PrRecord {
        PrRecord {
            number,
            title: format!("Change {number}"),
            url: format!("https://github.com/o/app/pull/{number}"),
            state,
            author: Some(author.into()),
            created_at: Some(t(1, 0)),
            fetched_at: Some(t(1, 0)),
            contribution: Contribution {
                commits: vec![PrCommit {
                    oid: format!("c{number}"),
                    author: Some(author.into()),
                    committed_at: Some(t(1, 0)),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn the_view_lists_open_items_and_each_persons_flow_reviews_and_response_times() {
        let mut cache = PrCache::default();
        let mut open = record(1, "Ann-Example", PrState::Open);
        open.timeline = vec![
            comment("maint-one", t(2, 0), "Held for the project owner."),
            comment("maint-one", t(2, 1), "Also, could you rebase onto main?"),
        ];
        cache.records.insert(1, open);
        let mut merged = record(2, "ann-example", PrState::Merged);
        merged.merged_at = Some(t(19, 0));
        merged.merged_by = Some("maint-one".into());
        merged.timeline = vec![comment("maint-one", t(18, 0), "Thanks, nice work.")];
        cache.records.insert(2, merged);
        let mut closed = record(3, "ann-example", PrState::Closed);
        closed.closed_at = Some(t(19, 2));
        closed.body = "x".into();
        closed.timeline = vec![TimelineItem {
            kind: "ClosedEvent".into(),
            at: Some(t(19, 2)),
            actor: Some("ann-example".into()),
            ..Default::default()
        }];
        cache.records.insert(3, closed);
        let mut stub = record(4, "bo-example", PrState::Open);
        stub.fetched_at = None;
        stub.url = String::new();
        stub.comments_total = 2;
        cache.records.insert(4, stub);
        // A change request its maintainer later approved is not unanswered.
        let mut approved = record(6, "ann-example", PrState::Open);
        approved.timeline = ["CHANGES_REQUESTED", "APPROVED"]
            .iter()
            .enumerate()
            .map(|(i, state)| TimelineItem {
                id: format!("r{i}"),
                kind: "PullRequestReview".into(),
                at: Some(t(3, i as u32)),
                actor: Some("maint-one".into()),
                state: Some(state.to_string()),
                ..Default::default()
            })
            .collect();
        cache.records.insert(6, approved);
        // A merge by someone who did not comment or review.
        let mut landed = record(7, "bo-example", PrState::Merged);
        landed.merged_at = Some(t(19, 0));
        landed.merged_by = Some("lander-two".into());
        cache.records.insert(7, landed);
        cache.main_checks = Some(MainChecks {
            oid: "head".into(),
            rollup: Some("SUCCESS".into()),
            failing: vec![],
        });
        let maintainers: BTreeSet<String> = ["maint-one".to_string()].into();
        let owners = vec!["boss-one".to_string()];
        let input = RepoInput {
            repo: "o/app",
            cache: &cache,
            maintainers: &maintainers,
            owners: &owners,
            rebase: false,
            release_at: Some(t(19, 1)),
        };
        let now = t(20, 0);
        let works = classify_repo(&input, now);
        let dir = directory(
            &[RepoPeople {
                repo: "o/app",
                cache: &cache,
                owners: &owners,
                maintainers: &maintainers,
            }],
            &[],
        );
        let v = build(&[input], &[works], &dir, now);
        assert_eq!((v.records, v.fetched), (6, 5));
        let flow = &v.repos["o/app"]["1d"];
        assert_eq!(
            (flow.merged, flow.closed["closed_withdrawn_or_abandoned"]),
            (2, 1)
        );
        assert_eq!(
            flow.mergers.keys().collect::<Vec<_>>(),
            ["lander-two", "maint-one"]
        );
        assert_eq!(flow.methods["unknown"], 2);
        assert_eq!(flow.median_merge_secs, Some(18 * 86400));
        assert_eq!(v.main["o/app"].rollup.as_deref(), Some("SUCCESS"));
        assert_eq!(
            v.items.iter().map(|i| i.number).collect::<Vec<_>>(),
            [1, 4, 6]
        );
        let item = &v.items[0];
        assert_eq!(item.author.as_deref(), Some("ann-example"));
        assert_eq!(item.work.state, "changes_requested_by_maintainer");
        assert_eq!(item.work.waits_for, ["ann-example", "boss-one"]);
        let unread = &v.items[1];
        assert_eq!(
            (unread.work.state.as_str(), unread.author.as_deref()),
            (states::NOT_READ_YET, Some("bo-example")),
            "#4: found by the sweep, not read yet"
        );
        assert_eq!(unread.url, "https://github.com/o/app/pull/4");
        let ann = v.people.iter().find(|p| p.key == "ann-example").unwrap();
        assert_eq!(ann.login, "Ann-Example");
        let day = &ann.flow["1d"][ALL_REPOS];
        assert_eq!((day.merged, day.closed_total), (1, 2));
        assert_eq!(day.merged_by["direct"], 1);
        assert_eq!(day.closed["closed_withdrawn_or_abandoned"], 1);
        assert_eq!(day.median_merge_secs, Some(18 * 86400));
        assert_eq!(
            ann.flow["release"]["o/app"].merged, 0,
            "merged before the build"
        );
        assert_eq!(ann.flow["release"]["o/app"].closed_total, 1);
        let resp = &ann.response[ALL_REPOS];
        assert_eq!(
            (resp.unanswered, resp.oldest_unanswered),
            (1, Some(t(2, 1)))
        );
        assert_eq!(
            (resp.m_answered, resp.awaiting_first),
            (3, 0),
            "#3 closed with no maintainer response"
        );
        let m = v.people.iter().find(|p| p.key == "maint-one").unwrap();
        let g = &m.given["all"]["o/app"];
        assert_eq!(
            (g.comments, g.prs, g.requests, g.holds, g.merges),
            (3, 3, 2, 1, 1)
        );
        assert!(
            v.people.iter().any(|p| p.key == "boss-one"),
            "the owner has work waiting on them"
        );
        let bo = v.people.iter().find(|p| p.key == "bo-example").unwrap();
        assert_eq!(
            bo.response[ALL_REPOS].awaiting_first, 0,
            "#4 is not read yet, so it waits for no response yet"
        );
        let lander = v.people.iter().find(|p| p.key == "lander-two").unwrap();
        let gl = &lander.given["all"]["o/app"];
        assert_eq!((gl.merges, gl.prs), (1, 0), "a merge is not a review");
    }

    #[test]
    fn assemble_ranks_people_types_each_merge_and_reports_the_load() {
        let mut cache = PrCache::default();
        let mut m = record(2, "ann-example", PrState::Merged);
        m.title = "Docs: explain the panels".into();
        m.merged_at = Some(t(19, 0));
        m.merged_by = Some("maint-one".into());
        m.contribution.files = vec![FileLines {
            path: "docs/panels.md".into(),
            additions: 10,
            deletions: 0,
        }];
        cache.records.insert(2, m);
        let mut closed = record(3, "bo-example", PrState::Closed);
        closed.closed_at = Some(t(18, 0));
        cache.records.insert(3, closed);
        cache.merged_walk = Walk {
            total: Some(5),
            done: true,
            ..Default::default()
        };
        cache.closed_walk = Walk {
            total: Some(4),
            ..Default::default()
        };
        cache.checked_at = Some(t(19, 23));
        let mut caches = vec![("o/app".to_string(), cache)];
        let roster = [RosterRow {
            name: Some("No Login".into()),
            ..Default::default()
        }];
        let (v, _, rk) = assemble(
            &mut caches,
            &PeopleConfig::default(),
            &roster,
            |_| None,
            &BTreeMap::new(),
            t(20, 0),
        );
        assert_eq!(
            v.load,
            Load {
                merged: 1,
                total: Some(5),
                closed: 1,
                closed_total: Some(4),
                done: false
            },
            "the merged walk is done, the closed walk is not"
        );
        assert_eq!(v.checked["o/app"], Some(t(19, 23)));
        let ann = v.rankings.iter().find(|r| r.key == "ann-example").unwrap();
        let day = &ann.windows["1d"];
        assert_eq!(
            (day.types["docs"], day.guessed["docs"], day.added),
            (1, 1, 10)
        );
        assert_eq!(v.unlinked, ["No Login"]);
        let rec = &caches[0].1.records[&2];
        assert_eq!(rec.derived.kind.as_ref().unwrap().value, "docs");
        assert_eq!(rec.derived.area.as_ref().unwrap().value, "docs");
        assert_eq!(rk.credits.len(), 1);
    }

    #[test]
    fn medians_take_the_middle_or_the_mean_of_the_two_middles() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [5, 1, 3]), Some(3));
        assert_eq!(median(&mut [4, 1, 3, 2]), Some(2));
    }
}
