//! The Track record view of the People tab ([`TrackRecord`]): for each
//! contributor in each repository, a review level set only by confirmed
//! faults that GitHub fields show, with every fault and the rule that
//! matched it.
//!
//! A fault is one of four kinds, F1, F3, F4 and F5, at most one per pull
//! request; F2 is listed only:
//!
//! - **F1 revert**: a merged pull request reverts the person's merged work,
//!   unless a later pull request replaced it, its merge commit also landed
//!   other authors' pull requests (a batch push), the revert was itself
//!   reverted, or it was one of a batch of reverts with no failure named.
//! - **F2 follow-up**: someone else's merged pull request, created after the
//!   work landed and merged within 14 days, changes the same product lines
//!   and names it in a sentence with a defect word, without a precedent
//!   phrase or a term outside the repository's gate. The records hold the
//!   changed files but not the changed lines, so a follow-up that changes a
//!   product file the work changed is listed as a possible follow-up and
//!   does not count. A follow-up by whoever merged or pushed to the work is
//!   a maintainer revision, and one by the viewing user is listed as found
//!   by them.
//! - **F3 checklist**: the body ticks "`cargo xtask ci`" while a check on the
//!   head failed that passed on the head of `main` when the check ran, is
//!   not flaky and was not waiting for approval.
//! - **F4 declined**: a maintainer closed the pull request, it landed
//!   nowhere, no merged or competing pull request took its place, and the
//!   work states did not find it superseded, yielded or closed by accident.
//! - **F5 late duplicate**: it closed as the copy of a pull request opened
//!   more than 12 hours earlier, linked to it and sharing an issue.
//!
//! The fault rate is faults ÷ counted pull requests, with a Wilson 90%
//! interval, against the repository's baseline: the pooled rate of the
//! other eligible authors there (or across the repositories when fewer
//! than five such authors exist), with the Wilson 90% interval of the
//! pool's faults and counted pull requests. A person is Untrusted when
//! their interval's lower bound is above the baseline's upper bound,
//! Trusted when their rate is at or below the baseline and no fault came
//! in the last 30 days, and Review otherwise, which includes a lower bound
//! above the baseline that is not above its upper bound. A person needs
//! five counted pull requests, and 80% of the pull requests at least a week
//! old decided, before any level but Not enough record. Maintainers and
//! bots get no row.
//!
//! The levels are read from the records the People work already holds; no
//! request is made for them, and no other view reads them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::kind::{path_class, product_lines, PathClass};
use super::ledger::LANDED_ELSEWHERE;
use super::record::{PrCache, PrRecord, PrState};
use super::states::sentences;
use super::{canonical, is_bot, Directory};
use crate::config::PeopleConfig;
use crate::model::Time;

/// A merge younger than this is pending: too recent for a fault to show.
pub const PENDING_HOURS: i64 = 72;
/// Only pull requests at least this old count toward a rate.
pub const MATURE_DAYS: i64 = 7;
/// Counted pull requests a level needs.
pub const MIN_COUNTED: u64 = 5;
/// The share of pull requests at least [`MATURE_DAYS`] old that must be
/// decided before a level.
pub const MIN_DECIDED: f64 = 0.8;
/// Other eligible authors a baseline needs.
pub const MIN_AUTHORS: usize = 5;
/// A fault this recent keeps a person from Trusted.
pub const QUIET_DAYS: i64 = 30;
/// A fault this recent shows a dated badge.
pub const BADGE_DAYS: i64 = 7;
/// A follow-up merges within this many days of the work landing.
pub const FOLLOW_UP_DAYS: i64 = 14;
/// Maintainers are the logins with [`MAINTAINER_ACTS`] merges or closes
/// among the last [`MAINTAINER_PRS`] merged or closed pull requests.
pub const MAINTAINER_PRS: usize = 200;
pub const MAINTAINER_ACTS: usize = 3;
/// A duplicate's original was opened more than this many hours earlier.
pub const EARLIER_HOURS: i64 = 12;
/// An author closing their own pull request within this many minutes of
/// opening it is not closing a duplicate.
pub const SELF_CLOSE_MINUTES: i64 = 60;
/// [`BATCH_REVERTS`] reverts merged within [`BATCH_MINUTES`] are a batch.
pub const BATCH_REVERTS: usize = 3;
pub const BATCH_MINUTES: i64 = 10;
/// A maintainer comment this soon after a revert can name its replacement.
pub const REPLACED_HOURS: i64 = 24;
/// A merged pull request that references a closed one within this many
/// days of the close took its place.
pub const DECLINE_DAYS: i64 = 7;
/// z for a two-sided 90% interval.
pub const Z90: f64 = 1.6449;
/// Automation accounts left out besides [`is_bot`]'s.
pub const BOTS: &[&str] = &["cursoragent", "copilot-swe-agent"];

/// A review level. The order is the order the view lists the groups in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// The interval's lower bound lies above the baseline's upper bound.
    Untrusted,
    /// Neither of the others.
    Review,
    /// At or below the baseline, with no fault in [`QUIET_DAYS`].
    Trusted,
    /// Too little record, or no baseline.
    #[default]
    NotEnough,
}

impl Level {
    /// The label the view shows.
    pub fn label(self) -> &'static str {
        match self {
            Level::Untrusted => "Untrusted",
            Level::Review => "Review",
            Level::Trusted => "Trusted",
            Level::NotEnough => "Not enough record",
        }
    }
}

/// Another pull request an event names: the revert, the follow-up or the
/// original of a duplicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TrackLink {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub author: Option<String>,
}

/// One event on a person's pull request: a counted fault, or something
/// listed beside the faults that does not count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TrackEvent {
    /// `revert`, `follow_up`, `checklist`, `declined` and `duplicate` are
    /// faults (no rule sets `follow_up` while the records hold no changed
    /// lines); `process_revert`, `possible_follow_up`,
    /// `maintainer_revision`, `found_by_you`, `precedent` and `close` are
    /// listed only.
    pub kind: String,
    /// It counts toward the fault rate.
    pub fault: bool,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub at: Option<Time>,
    /// The rule that matched, in words.
    pub rule: String,
    pub other: Option<TrackLink>,
    /// The other pull request is the viewing user's.
    pub involves_you: bool,
    /// No check ran on the pull request's head.
    pub ci_did_not_run: bool,
}

/// The fault rate a level is measured against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Baseline {
    pub rate: f64,
    pub faults: u64,
    pub counted: u64,
    /// `repo` for the repository's own authors, `all` for every repository's.
    pub scope: String,
    /// The authors it pools, canonical.
    pub authors: Vec<String>,
}

/// One person in one repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TrackRow {
    /// The canonical login.
    pub key: String,
    pub login: String,
    pub name: Option<String>,
    pub repo: String,
    /// The viewing user's own row, shown apart from the others.
    pub you: bool,
    /// Their first pull request here was their first contribution.
    pub new_to_repo: bool,
    pub level: Level,
    /// Why the level, in words.
    pub rule: String,
    /// Pull requests opened in the window.
    pub prs: u64,
    /// Of those, opened at least [`MATURE_DAYS`] ago, and decided among them.
    pub mature: u64,
    pub decided: u64,
    pub counted: u64,
    pub faults: u64,
    /// The fault rate's 90% interval; `None` with nothing counted, and for
    /// Not enough record, which shows no rate.
    pub low: Option<f64>,
    pub high: Option<f64>,
    pub baseline: Option<Baseline>,
    /// Open pull requests.
    pub waiting: u64,
    /// Merged in the last [`PENDING_HOURS`].
    pub pending: u64,
    /// Counted landings with no fault and no maintainer commit.
    pub clean: u64,
    /// Counted landings, and the commits maintainers pushed to them.
    pub landed: u64,
    pub maintainer_commits: u64,
    /// Decided pull requests whose checks ran, and those whose own checks
    /// failed (shown from three runs on).
    pub ci_ran: u64,
    pub ci_own: u64,
    /// Their first pull request in the window.
    pub first_activity: Option<Time>,
    /// Their newest counted fault, and the same when within [`BADGE_DAYS`].
    pub last_fault: Option<Time>,
    pub recent_fault: Option<Time>,
    pub events: Vec<TrackEvent>,
}

/// Everything the Track record view shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TrackRecord {
    pub window_days: u32,
    /// In the view's order: by level, then the newest fault first; Not
    /// enough record by first activity. The viewing user's rows last.
    pub rows: Vec<TrackRow>,
}

static DEFECT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:fail|fails|failed|failing|flaky|intermittent|intermittently|broke|broken|breaks|regress|regression|regressed|panic|panics|crash|crashes|wrong|bug)\b",
    )
    .unwrap()
});
/// `#N`, `owner/repo#N` or a pull request or issue URL.
static REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"https://github\.com/([\w.-]+/[\w.-]+)/(?:pull|issues)/(\d+)|(?:([\w.-]+/[\w.-]+))?#(\d+)",
    )
    .unwrap()
});
/// The line GitHub's Revert button writes into the body.
static AUTO_REVERT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*reverts\s+(\S*?)#(\d+)\s*$").unwrap());
static REVERT_OID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)this reverts commit ([0-9a-f]{7,40})").unwrap());
static TICKED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^\s*[-*+]?\s*\[[xX]\][^\n]*cargo xtask ci").unwrap());
/// The precedent phrases: "in the same way", and "as", "like", "following",
/// "same as" or "matching" before a reference in any form [`REF`] reads.
static PRECEDENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"in the same way|\b(?:as|like|following|matching) (?:https://github\.com/([\w.-]+/[\w.-]+)/(?:pull|issues)/(\d+)|(?:([\w.-]+/[\w.-]+))?#(\d+))",
    )
    .unwrap()
});

/// Whether sentence `s` (lowercase) cites pull request `n` of `repo` as a
/// precedent.
fn cites(s: &str, n: u64, repo: &str) -> bool {
    PRECEDENT.captures_iter(s).any(|c| {
        let Some(num) = c.get(2).or(c.get(4)) else {
            return true;
        };
        let owner = c.get(1).or(c.get(3));
        owner.is_none_or(|o| o.as_str().eq_ignore_ascii_case(repo))
            && num.as_str().parse::<u64>() == Ok(n)
    })
}

/// Whether `login` is a bot or automation account.
fn bot(login: &str) -> bool {
    is_bot(login, None) || BOTS.iter().any(|b| b.eq_ignore_ascii_case(login.trim()))
}

/// The canonical login of a record's human author.
fn author_of(r: &PrRecord) -> Option<String> {
    let a = r.author.as_deref()?;
    (!r.author_bot && !bot(a)).then(|| canonical(a))
}

/// Who landed `r`: its resolved merger, or `mergedBy`.
fn merger_of(r: &PrRecord) -> Option<String> {
    r.contribution
        .merger
        .clone()
        .or_else(|| r.merged_by.as_deref().map(canonical))
}

/// Who closed `r` last, canonical.
fn closer_of(r: &PrRecord) -> Option<String> {
    r.timeline
        .iter()
        .rev()
        .find(|t| t.kind == "ClosedEvent")
        .and_then(|t| t.actor.as_deref())
        .map(canonical)
}

/// The numbers `text` names in `repo`: `#N`, `repo#N` and URLs into `repo`.
pub fn refs(text: &str, repo: &str) -> Vec<u64> {
    REF.captures_iter(text)
        .filter_map(|c| {
            let (owner, n) = match (c.get(1), c.get(2), c.get(3), c.get(4)) {
                (Some(o), Some(n), ..) => (Some(o.as_str()), n.as_str()),
                (_, _, o, Some(n)) => (o.map(|m| m.as_str()), n.as_str()),
                _ => return None,
            };
            if owner.is_some_and(|o| !o.eq_ignore_ascii_case(repo)) {
                return None;
            }
            n.parse().ok()
        })
        .collect()
}

/// The issues `r` is tied to in `repo`: those it closes and the `#N` in its
/// title, as `(repo lowercase, number)`.
fn issue_keys(r: &PrRecord, repo: &str) -> BTreeSet<(String, u64)> {
    let mut out: BTreeSet<(String, u64)> = r
        .closing_issues
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let issue_repo = r.closing_repos.get(i).filter(|s| !s.is_empty());
            (issue_repo.map_or(repo, |s| s).to_ascii_lowercase(), *n)
        })
        .collect();
    for n in refs(&r.title, repo) {
        if n != r.number {
            out.insert((repo.to_ascii_lowercase(), n));
        }
    }
    out
}

/// The product files `r` changed: not bookkeeping, docs, translation
/// catalogs, generated files or `Cargo.lock`.
fn product_files(r: &PrRecord) -> BTreeSet<&str> {
    r.contribution
        .files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| {
            product_lines(p) && !matches!(path_class(p), PathClass::Bookkeeping | PathClass::Docs)
        })
        .collect()
}

/// When `r` landed: its merge, or its close when the work states found it
/// landed by another route.
fn landed_at(r: &PrRecord) -> Option<Time> {
    match r.state {
        PrState::Merged => r.merged_at.or(r.closed_at),
        PrState::Closed if landed_elsewhere(r) => r.closed_at,
        _ => None,
    }
}

fn landed_elsewhere(r: &PrRecord) -> bool {
    r.derived
        .state
        .as_ref()
        .is_some_and(|d| d.value == LANDED_ELSEWHERE)
}

/// No check ran on `r`'s head.
fn ci_did_not_run(r: &PrRecord) -> bool {
    r.checks.rollup.is_none() && r.checks.suites.is_empty()
}

/// When the checks on `r`'s head ran: from the oldest check suite's start
/// to the newest one's; `None` without a dated suite (a commit status
/// carries no time the records keep), and when the record does not hold
/// every suite on the head. The records do not say which suite ran a
/// failing check, so a check counts as passing on `main` only when it
/// passed there throughout this span.
fn ci_run_span(r: &PrRecord) -> Option<(Time, Time)> {
    if r.checks.suites_total != Some(r.checks.suites.len() as u64) {
        return None;
    }
    let starts = r.checks.suites.iter().filter_map(|s| s.created_at);
    Some((starts.clone().min()?, starts.max()?))
}

/// The Wilson 90% interval of `k` faults in `n`, clamped to 0 to 1; `None`
/// for `n` = 0. With no faults the lower bound is exactly 0, and with every
/// pull request at fault the upper bound is exactly 1, where the formula
/// leaves a rounding residue.
pub fn wilson(k: u64, n: u64) -> Option<(f64, f64)> {
    if n == 0 {
        return None;
    }
    let (kf, nf) = (k as f64, n as f64);
    let p = kf / nf;
    let z2 = Z90 * Z90;
    let center = p + z2 / (2.0 * nf);
    let half = Z90 * (p * (1.0 - p) / nf + z2 / (4.0 * nf * nf)).sqrt();
    let d = 1.0 + z2 / nf;
    let low = if k == 0 { 0.0 } else { (center - half) / d };
    let high = if k >= n { 1.0 } else { (center + half) / d };
    Some((low.clamp(0.0, 1.0), high.clamp(0.0, 1.0)))
}

/// The level of an eligible person with `k` faults in `n` against a
/// baseline of rate `baseline` whose own 90% interval has the upper bound
/// `baseline_high`, the newest fault dated `last`. Untrusted needs the
/// person's lower bound above `baseline_high`, and Trusted needs the rate
/// at or below `baseline` with no fault in [`QUIET_DAYS`]; any other record
/// is Review.
pub fn level(
    k: u64,
    n: u64,
    baseline: f64,
    baseline_high: f64,
    last: Option<Time>,
    now: Time,
) -> Level {
    let Some((low, _)) = wilson(k, n) else {
        return Level::NotEnough;
    };
    let quiet = last.is_none_or(|at| now - at > chrono::Duration::days(QUIET_DAYS));
    if low > baseline_high {
        Level::Untrusted
    } else if (k as f64) / (n as f64) <= baseline && quiet {
        Level::Trusted
    } else {
        Level::Review
    }
}

/// A repository's maintainers for the Track record: the logins with
/// [`MAINTAINER_ACTS`] or more merges, or closes of others' pull requests,
/// among its last [`MAINTAINER_PRS`] merged or closed pull requests, the
/// configured extra maintainers and the repository's owners, canonical.
/// Bots are left out, and membership does not count.
pub fn maintainers(cache: &PrCache, extra: &[String], owners: &[String]) -> BTreeSet<String> {
    let mut decided: Vec<&PrRecord> = cache
        .records
        .values()
        .filter(|r| r.state != PrState::Open)
        .collect();
    decided.sort_by_key(|r| std::cmp::Reverse(r.merged_at.or(r.closed_at)));
    let mut acts: BTreeMap<String, usize> = BTreeMap::new();
    for r in decided.into_iter().take(MAINTAINER_PRS) {
        // Closing one's own pull request is not a maintainer's act.
        let actor = if r.state == PrState::Merged {
            merger_of(r)
        } else {
            closer_of(r).filter(|c| r.author.as_deref().map(canonical).as_ref() != Some(c))
        };
        if let Some(a) = actor.filter(|a| !bot(a)) {
            *acts.entry(a).or_default() += 1;
        }
    }
    acts.into_iter()
        .filter(|(_, n)| *n >= MAINTAINER_ACTS)
        .map(|(a, _)| a)
        .chain(extra.iter().chain(owners).map(|l| canonical(l)))
        .filter(|a| !bot(a))
        .collect()
}

/// What the rules read about one repository.
struct Repo<'a> {
    repo: &'a str,
    cache: &'a PrCache,
    m: BTreeSet<String>,
    viewer: Option<String>,
    flaky: &'a [String],
    gate_terms: &'a [String],
    now: Time,
    /// Merged reverts, by the number of the pull request they revert.
    reverts: HashMap<u64, Vec<&'a PrRecord>>,
    /// Every merged revert's merge time, sorted.
    revert_times: Vec<Time>,
    /// Merged pull requests that name a number, with the sentences that do.
    names: HashMap<u64, Vec<(&'a PrRecord, Vec<String>)>>,
    /// Each record's issues ([`issue_keys`]), by number.
    keys: HashMap<u64, BTreeSet<(String, u64)>>,
}

impl<'a> Repo<'a> {
    fn new(
        repo: &'a str,
        cache: &'a PrCache,
        cfg: &'a PeopleConfig,
        viewer: Option<String>,
        now: Time,
    ) -> Repo<'a> {
        let m = maintainers(cache, &cfg.maintainers, cfg.owners_for(repo));
        let mut out = Repo {
            repo,
            cache,
            m,
            viewer,
            flaky: cfg.flaky_for(repo),
            gate_terms: cfg.gate_terms_for(repo),
            now,
            reverts: HashMap::new(),
            revert_times: Vec::new(),
            names: HashMap::new(),
            keys: cache
                .records
                .values()
                .map(|r| (r.number, issue_keys(r, repo)))
                .collect(),
        };
        // Commit oids of merged work, for `This reverts commit <oid>`: the
        // merge commit, and the branch's own commits by the pull request's
        // author. A commit someone else pushed to the branch, such as a
        // maintainer's review commit, is not the author's work.
        let mut oids: Vec<(&str, u64)> = Vec::new();
        for r in cache
            .records
            .values()
            .filter(|r| r.state == PrState::Merged)
        {
            if let Some(mc) = &r.contribution.merge_commit {
                oids.push((&mc.oid, r.number));
            }
            let Some(author) = author_of(r) else { continue };
            for c in r.contribution.commits.iter().filter(|c| {
                !c.is_merge
                    && c.author.as_deref().map(canonical).as_deref() == Some(author.as_str())
            }) {
                oids.push((&c.oid, r.number));
            }
        }
        for r in cache
            .records
            .values()
            .filter(|r| r.state == PrState::Merged)
        {
            if is_revert(r) {
                if let Some(at) = r.merged_at {
                    out.revert_times.push(at);
                }
                for n in revert_targets(r, cache, repo, &oids) {
                    out.reverts.entry(n).or_default().push(r);
                }
                // A revert names what it reverts; it is no follow-up.
                continue;
            }
            if author_of(r).is_none() {
                continue;
            }
            let mut by_number: BTreeMap<u64, Vec<String>> = BTreeMap::new();
            for (_, lower) in sentences(&r.title).into_iter().chain(sentences(&r.body)) {
                for n in refs(&lower, repo) {
                    if n != r.number {
                        by_number.entry(n).or_default().push(lower.clone());
                    }
                }
            }
            for (n, said) in by_number {
                out.names.entry(n).or_default().push((r, said));
            }
        }
        out.revert_times.sort();
        out
    }

    fn get(&self, n: u64) -> Option<&'a PrRecord> {
        self.cache.records.get(&n)
    }

    /// The issues record `n` is tied to.
    fn keys_of(&self, n: u64) -> &BTreeSet<(String, u64)> {
        static NONE: BTreeSet<(String, u64)> = BTreeSet::new();
        self.keys.get(&n).unwrap_or(&NONE)
    }

    fn link(&self, r: &PrRecord) -> TrackLink {
        TrackLink {
            number: r.number,
            url: r.url.clone(),
            title: r.title.clone(),
            author: author_of(r).or_else(|| r.author.clone()),
        }
    }

    fn is_viewer(&self, login: Option<&str>) -> bool {
        login.is_some_and(|l| self.viewer.as_deref() == Some(l))
    }

    fn event(
        &self,
        p: &PrRecord,
        kind: &str,
        fault: bool,
        at: Option<Time>,
        rule: String,
        other: Option<&PrRecord>,
    ) -> TrackEvent {
        TrackEvent {
            kind: kind.into(),
            fault,
            number: p.number,
            url: p.url.clone(),
            title: p.title.clone(),
            at,
            rule,
            other: other.map(|o| self.link(o)),
            involves_you: other.is_some_and(|o| self.is_viewer(author_of(o).as_deref())),
            ci_did_not_run: ci_did_not_run(p),
        }
    }

    /// Maintainer comments on `r` within `from` to `to`.
    fn m_comments<'r>(
        &self,
        r: &'r PrRecord,
        from: Time,
        to: Time,
    ) -> impl Iterator<Item = &'r str> + 'r {
        let m = self.m.clone();
        r.timeline.iter().filter_map(move |t| {
            let who = canonical(t.actor.as_deref()?);
            let at = t.at?;
            (matches!(t.kind.as_str(), "IssueComment" | "PullRequestReview")
                && m.contains(&who)
                && at >= from
                && at <= to)
                .then_some(t.body.as_deref().unwrap_or(""))
        })
    }

    /// F1: the reverts of landed `p`, each a fault or a process revert.
    fn reverts_of(&self, p: &PrRecord, out: &mut Vec<TrackEvent>) {
        let Some(landed) = landed_at(p) else { return };
        for r in self.reverts.get(&p.number).into_iter().flatten() {
            let Some(at) = r.merged_at.filter(|at| *at >= landed) else {
                continue;
            };
            match self.revert_neutral(r, p, landed, at) {
                Some(why) => out.push(self.event(
                    p,
                    "process_revert",
                    false,
                    Some(at),
                    format!("F1 does not count: {why}"),
                    Some(r),
                )),
                None => out.push(self.event(
                    p,
                    "revert",
                    true,
                    Some(at),
                    format!("F1: #{} reverted it", r.number),
                    Some(r),
                )),
            }
        }
    }

    /// Why revert `r` of `p` (landed at `landed`, reverted at `at`) is not
    /// a fault, or `None` when it is one.
    fn revert_neutral(&self, r: &PrRecord, p: &PrRecord, landed: Time, at: Time) -> Option<String> {
        if let Some(again) = self
            .reverts
            .get(&r.number)
            .into_iter()
            .flatten()
            .find(|x| x.merged_at.is_some_and(|m| m >= at))
        {
            return Some(format!("#{} reverted the revert", again.number));
        }
        if let Some(oid) = p
            .contribution
            .merge_commit
            .as_ref()
            .map(|m| m.oid.as_str())
            .filter(|o| !o.is_empty())
        {
            let authors: BTreeSet<String> = self
                .cache
                .records
                .values()
                .filter(|x| {
                    x.contribution
                        .merge_commit
                        .as_ref()
                        .is_some_and(|m| m.oid == oid)
                })
                .filter_map(author_of)
                .collect();
            if authors.len() > 1 {
                return Some("the reverted commit carried several authors' pull requests".into());
            }
        }
        let body = without_auto_line(&r.body);
        let soon = at + chrono::Duration::hours(REPLACED_HOURS);
        // A pull request named in a sentence with a defect word is what the
        // revert blames, not what replaced the work.
        let replacing = |text: &str| -> Vec<u64> {
            sentences(text)
                .into_iter()
                .filter(|(_, lower)| !DEFECT.is_match(lower))
                .flat_map(|(written, _)| refs(&written, self.repo))
                .collect()
        };
        let mut named: Vec<u64> = replacing(&body);
        for c in self
            .m_comments(p, at, soon)
            .chain(self.m_comments(r, at, soon))
        {
            named.extend(replacing(c));
        }
        if let Some(q) = named
            .into_iter()
            .filter(|n| *n != p.number && *n != r.number)
            .filter_map(|n| self.get(n))
            .find(|q| {
                q.state == PrState::Merged
                    && (q.created_at.is_some_and(|c| c > landed)
                        || q.merged_at.is_some_and(|m| m > landed))
            })
        {
            return Some(format!("#{} replaced the work", q.number));
        }
        let batch = self.batch_at(at);
        if batch >= BATCH_REVERTS {
            let lower = body.to_lowercase();
            let named_p = refs(&lower, self.repo).contains(&p.number);
            let said = DEFECT.is_match(&lower)
                || named_p
                || self
                    .m_comments(p, at, self.now)
                    .any(|c| DEFECT.is_match(&c.to_lowercase()));
            if !said {
                return Some(format!(
                    "one of {batch} reverts within {BATCH_MINUTES} minutes, with no failure named"
                ));
            }
        }
        None
    }

    /// The most reverts merged within one [`BATCH_MINUTES`] span that holds
    /// `at`.
    fn batch_at(&self, at: Time) -> usize {
        let span = chrono::Duration::minutes(BATCH_MINUTES);
        self.revert_times
            .iter()
            .filter(|start| **start <= at && at - **start <= span)
            .map(|start| {
                self.revert_times
                    .iter()
                    .filter(|t| **t >= *start && **t - *start <= span)
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    /// F2: the follow-ups that name landed `p`.
    fn follow_ups(&self, p: &PrRecord, author: &str, out: &mut Vec<TrackEvent>) {
        let Some(landed) = landed_at(p) else { return };
        let Some(naming) = self.names.get(&p.number) else {
            return;
        };
        let files = product_files(p);
        let revisers: BTreeSet<String> = merger_of(p)
            .into_iter()
            .chain(
                p.contribution
                    .commits
                    .iter()
                    .filter_map(|c| c.author.as_deref())
                    .map(canonical),
            )
            .chain(
                p.timeline
                    .iter()
                    .filter(|t| t.kind == "HeadRefForcePushedEvent")
                    .filter_map(|t| t.actor.as_deref())
                    .map(canonical),
            )
            .filter(|l| l != author)
            .collect();
        let precedent = |s: &str| cites(s, p.number, self.repo);
        for (q, said) in naming {
            let (Some(created), Some(merged)) = (q.created_at, q.merged_at) else {
                continue;
            };
            if created <= landed || merged - landed > chrono::Duration::days(FOLLOW_UP_DAYS) {
                continue;
            }
            let Some(qa) = author_of(q) else { continue };
            let gate = |s: &str| {
                self.gate_terms
                    .iter()
                    .any(|t| s.contains(&t.to_lowercase()))
            };
            let cited = said.iter().any(|s| precedent(s));
            let defect = said
                .iter()
                .find(|s| DEFECT.is_match(s) && !precedent(s) && !gate(s));
            let overlap: Vec<&str> = product_files(q).intersection(&files).copied().collect();
            let at = Some(merged);
            let n = q.number;
            if defect.is_some() && !overlap.is_empty() {
                let file = overlap[0];
                if self.is_viewer(Some(&qa)) {
                    out.push(self.event(
                        p,
                        "found_by_you",
                        false,
                        at,
                        format!("#{n} is your follow-up, so it does not count (it changes {file})"),
                        Some(q),
                    ));
                } else if revisers.contains(&qa) {
                    out.push(self.event(
                        p,
                        "maintainer_revision",
                        false,
                        at,
                        format!("#{n} is a revision by {qa}, who merged or pushed to it"),
                        Some(q),
                    ));
                } else if qa == author {
                    out.push(self.event(
                        p,
                        "possible_follow_up",
                        false,
                        at,
                        format!("#{n} is the author's own follow-up"),
                        Some(q),
                    ));
                } else {
                    // F2 needs the changed lines to overlap, and the records
                    // hold only the changed files.
                    out.push(self.event(
                        p,
                        "possible_follow_up",
                        false,
                        at,
                        format!(
                            "#{n} by {qa} names it with a defect word and also changes {file}, but F2 needs the changed lines to overlap, and the records do not show the lines"
                        ),
                        Some(q),
                    ));
                }
            } else if cited && defect.is_none() {
                out.push(self.event(
                    p,
                    "precedent",
                    false,
                    at,
                    format!("#{n} cites it as a precedent"),
                    Some(q),
                ));
            } else {
                let missing = match (defect.is_some(), overlap.is_empty()) {
                    (false, _) if said.iter().any(|s| gate(s)) => {
                        "its sentence names a failure outside the gate"
                    }
                    (false, _) => "no sentence naming it has a defect word",
                    _ => "it changes no product file in common",
                };
                out.push(self.event(
                    p,
                    "possible_follow_up",
                    false,
                    at,
                    format!("#{n} names it, but {missing}"),
                    Some(q),
                ));
            }
        }
    }

    /// The checks of `p`'s head that failed because of `p` itself: failed,
    /// passing on the head of `main` while the checks ran ([`ci_run_span`],
    /// [`PrCache::main_over`]) and not failing there under the same name, not
    /// flaky, not waiting for approval, and not failing because `main` fails.
    /// `None` when there are none or that cannot be told: a check is never
    /// the pull request's own when no entry of `main`'s history spans the
    /// run, or that entry does not list it as passing. A failing commit
    /// status is never the pull request's own, and no failure is when the
    /// record does not say which failures are commit statuses.
    fn own_failures(&self, p: &PrRecord) -> Option<Vec<String>> {
        if !matches!(p.checks.rollup.as_deref(), Some("FAILURE" | "ERROR")) {
            return None;
        }
        if p.derived
            .state
            .as_ref()
            .is_some_and(|d| d.value == "ci_failing_inherited_from_main")
        {
            return None;
        }
        let (from, to) = ci_run_span(p)?;
        let main = self.cache.main_over(from, to)?;
        let actions: Vec<_> = p
            .checks
            .suites
            .iter()
            .filter(|s| s.app.as_deref() == Some("github-actions"))
            .collect();
        // As the work state `ci_awaiting_workflow_approval` reads it.
        let approval = !actions.is_empty()
            && actions.iter().all(|s| {
                s.status == "COMPLETED" && s.conclusion.as_deref() == Some("ACTION_REQUIRED")
            });
        if approval {
            return None;
        }
        let statuses = p.checks.failing_statuses.as_ref()?;
        let own: Vec<String> = p
            .checks
            .failing
            .iter()
            .filter(|c| !statuses.iter().any(|s| s.eq_ignore_ascii_case(c)))
            .filter(|c| main.passing.iter().any(|m| m.eq_ignore_ascii_case(c)))
            // Two checks with one name can pass and fail on the same head of
            // `main`; such a name may be the failure the pull request inherited.
            .filter(|c| !main.failing.iter().any(|m| m.eq_ignore_ascii_case(c)))
            .filter(|c| !self.flaky.iter().any(|f| f.eq_ignore_ascii_case(c)))
            .cloned()
            .collect();
        (!own.is_empty()).then_some(own)
    }

    /// F3: a ticked `cargo xtask ci` box over the pull request's own failure.
    fn checklist(&self, p: &PrRecord, out: &mut Vec<TrackEvent>) {
        if !TICKED.is_match(&p.body) {
            return;
        }
        if let Some(own) = self.own_failures(p) {
            out.push(self.event(
                p,
                "checklist",
                true,
                p.merged_at.or(p.closed_at),
                format!("F3: the body ticks cargo xtask ci, but {} failed on its head and passed on main when it ran", own.join(", ")),
                None,
            ));
        }
    }

    /// F5: closed `p` as a late copy of an earlier pull request.
    fn duplicate(&self, p: &PrRecord, author: &str, out: &mut Vec<TrackEvent>) -> bool {
        let (Some(created), Some(closed)) = (p.created_at, p.closed_at) else {
            return false;
        };
        let closer = closer_of(p);
        let by_m = closer.as_ref().is_some_and(|c| self.m.contains(c));
        let late_self = closer.as_deref() == Some(author)
            && closed - created > chrono::Duration::minutes(SELF_CLOSE_MINUTES);
        if !by_m && !late_self {
            return false;
        }
        let keys = self.keys_of(p.number);
        if keys.is_empty() {
            return false;
        }
        let linked = |a: &PrRecord, b: u64| {
            a.timeline.iter().any(|t| {
                t.kind == "CrossReferencedEvent"
                    && t.source
                        .as_ref()
                        .is_some_and(|s| s.number == b && s.repo.eq_ignore_ascii_case(self.repo))
            })
        };
        // A pull request opened after the other one closed without landing
        // resubmits it: that other one was no longer existing work.
        let abandoned = |c: &PrRecord| {
            c.state == PrState::Closed
                && landed_at(c).is_none()
                && c.closed_at.is_some_and(|at| at <= created)
        };
        let original = self.cache.records.values().find(|c| {
            c.number != p.number
                && !abandoned(c)
                && c.created_at
                    .is_some_and(|o| created - o > chrono::Duration::hours(EARLIER_HOURS))
                && !self.keys_of(c.number).is_disjoint(keys)
                && (linked(p, c.number) || linked(c, p.number))
        });
        let Some(c) = original else { return false };
        out.push(self.event(
            p,
            "duplicate",
            true,
            Some(closed),
            format!(
                "F5: closed by {} as a copy of #{}, opened more than {EARLIER_HOURS} hours earlier for the same issue",
                closer.unwrap_or_default(),
                c.number
            ),
            Some(c),
        ));
        true
    }

    /// F4: closed `p` declined by a maintainer, with nothing taking its place.
    fn declined(&self, p: &PrRecord, author: &str, out: &mut Vec<TrackEvent>) -> Option<String> {
        let closed = p.closed_at?;
        let closer = closer_of(p);
        if !closer
            .as_ref()
            .is_some_and(|c| self.m.contains(c) && c != author)
        {
            return Some("no maintainer closed it".into());
        }
        if let Some(what) = listed_close(p) {
            return Some(format!("the work states found it {what}"));
        }
        // A merged pull request that references it took its place when the
        // reference or the merge came within the week after the close.
        let until = closed + chrono::Duration::days(DECLINE_DAYS);
        let within = |t: Option<Time>| t.is_some_and(|t| t >= closed && t <= until);
        let referenced = p.timeline.iter().find_map(|t| {
            let s = t.source.as_ref()?;
            (t.kind == "CrossReferencedEvent"
                && s.kind == "PullRequest"
                && s.merged_at.is_some()
                && s.repo.eq_ignore_ascii_case(self.repo)
                && (within(t.at) || within(s.merged_at)))
            .then_some(s.number)
        });
        let named = self
            .names
            .get(&p.number)
            .into_iter()
            .flatten()
            .find_map(|(q, _)| {
                (q.merged_at.is_some() && (within(q.created_at) || within(q.merged_at)))
                    .then_some(q.number)
            });
        if let Some(n) = referenced.or(named) {
            return Some(format!(
                "#{n}, a merged pull request, references it and came within {DECLINE_DAYS} days of the close"
            ));
        }
        let keys = self.keys_of(p.number);
        if !keys.is_empty() {
            for q in self.cache.records.values().filter(|q| q.number != p.number) {
                if self.keys_of(q.number).is_disjoint(keys) {
                    continue;
                }
                if q.state == PrState::Merged || landed_elsewhere(q) {
                    return Some(format!("#{} landed for the same issue", q.number));
                }
                let other = author_of(q).is_some_and(|a| a != author);
                let opened_before = q.created_at.is_some_and(|c| c < closed);
                let open_then =
                    q.state == PrState::Open || q.closed_at.is_some_and(|c| c >= closed);
                if other && opened_before && open_then {
                    return Some(format!(
                        "#{} by another author was open for the same issue",
                        q.number
                    ));
                }
            }
        }
        let class = p
            .derived
            .state
            .as_ref()
            .map(|d| d.value.replace('_', " "))
            .unwrap_or_default();
        out.push(self.event(
            p,
            "declined",
            true,
            Some(closed),
            format!(
                "F4: {} closed it, it landed nowhere and nothing took its place{}",
                closer.unwrap_or_default(),
                if class.is_empty() {
                    String::new()
                } else {
                    format!(" (work state: {class})")
                }
            ),
            None,
        ));
        None
    }
}

/// What the work states found closed `r` to be, in words, when that close
/// is listed only and never F4: superseded (by a maintainer or by the
/// author), yielded to an open pull request, or closed by accident within
/// an hour of opening.
fn listed_close(r: &PrRecord) -> Option<&'static str> {
    let d = r.derived.state.as_ref()?;
    match (d.value.as_str(), d.subtype.as_deref()) {
        ("closed_superseded", _) => Some("superseded"),
        ("closed_duplicate", Some("author_yielded")) => Some("yielded to an open pull request"),
        ("closed_withdrawn_or_abandoned", Some("accidental_quick_close")) => {
            Some("closed by accident within an hour of opening")
        }
        _ => None,
    }
}

/// Whether `r` is a revert: its title or a commit headline starts with
/// `Revert "`, as GitHub's Revert button and `git revert` write it.
fn is_revert(r: &PrRecord) -> bool {
    r.title.starts_with("Revert \"")
        || r.contribution
            .commits
            .iter()
            .any(|c| c.headline.starts_with("Revert \""))
}

/// `body` without the `Reverts owner/repo#N` lines GitHub writes.
fn without_auto_line(body: &str) -> String {
    body.lines()
        .filter(|l| !AUTO_REVERT.is_match(l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The pull requests revert `r` reverts: named by GitHub's `Reverts
/// owner/repo#N` line, by a reverted commit's oid (`oids` maps the commits
/// of merged work to their pull request), or, when `r` names neither, by a
/// `Revert "<title>"` headline that matches a merged pull request's title
/// or squash headline. An oid that maps to no pull request, such as a
/// commit pushed straight to `main`, names no target.
fn revert_targets(r: &PrRecord, cache: &PrCache, repo: &str, oids: &[(&str, u64)]) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::new();
    // A `Reverts` line that names another repository still names the target,
    // so no title is matched in this one.
    let mut named = false;
    for line in r.body.lines() {
        if let Some(c) = AUTO_REVERT.captures(line) {
            named = true;
            let owner = c[1].trim_end_matches('/');
            if owner.is_empty() || owner.eq_ignore_ascii_case(repo) {
                if let Ok(n) = c[2].parse() {
                    out.push(n);
                }
            }
        }
    }
    let mut reverted: Vec<String> = REVERT_OID
        .captures_iter(&r.body)
        .map(|c| c[1].to_ascii_lowercase())
        .collect();
    for c in &r.contribution.commits {
        reverted.extend(c.reverts.iter().cloned());
    }
    for oid in &reverted {
        if let Some((_, n)) = oids.iter().find(|(o, n)| {
            *n != r.number
                && o.len() >= oid.len()
                && o.to_ascii_lowercase().starts_with(oid.as_str())
        }) {
            out.push(*n);
        }
    }
    if !named && out.is_empty() && reverted.is_empty() {
        let quoted: Vec<&str> = std::iter::once(r.title.as_str())
            .chain(r.contribution.commits.iter().map(|c| c.headline.as_str()))
            .filter_map(|h| h.strip_prefix("Revert \"")?.strip_suffix('"'))
            .collect();
        let target = cache
            .records
            .values()
            .filter(|p| {
                p.number != r.number && p.state == PrState::Merged && p.merged_at <= r.merged_at
            })
            .filter(|p| {
                quoted.iter().any(|q| {
                    *q == p.title
                        || p.contribution
                            .merge_commit
                            .as_ref()
                            .is_some_and(|m| m.headline == *q)
                })
            })
            .max_by_key(|p| p.merged_at);
        out.extend(target.map(|p| p.number));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// What one person did in one repository, before the baseline.
#[derive(Default)]
struct Acc {
    row: TrackRow,
    first_seen: Option<(Time, String)>,
}

/// One repository's rows before levels.
fn repo_rows(rp: &Repo, window_start: Time) -> Vec<TrackRow> {
    let mut acc: BTreeMap<String, Acc> = BTreeMap::new();
    let now = rp.now;
    // The association a person had on their first pull request here.
    for r in rp.cache.records.values() {
        let (Some(a), Some(c)) = (author_of(r), r.created_at) else {
            continue;
        };
        let e = acc.entry(a).or_default();
        if e.first_seen.as_ref().is_none_or(|(t, _)| c < *t) {
            e.first_seen = Some((c, r.author_association.clone()));
        }
    }
    for p in rp.cache.records.values() {
        let Some(author) = author_of(p) else { continue };
        if rp.m.contains(&author) {
            continue;
        }
        let Some(created) = p.created_at.filter(|c| *c >= window_start) else {
            continue;
        };
        let row = &mut acc.entry(author.clone()).or_default().row;
        row.prs += 1;
        row.first_activity = Some(row.first_activity.map_or(created, |f| f.min(created)));
        let landed = landed_at(p);
        let closed = p.state == PrState::Closed && landed.is_none();
        let mature = now - created >= chrono::Duration::days(MATURE_DAYS);
        if mature {
            row.mature += 1;
        }
        if p.state == PrState::Open {
            row.waiting += 1;
            continue;
        }
        // Without its timeline and commits, no fault can show on it, so a
        // record not read yet is not decided.
        if p.fetched_at.is_none() {
            continue;
        }
        let pending = landed.is_some_and(|l| now - l < chrono::Duration::hours(PENDING_HOURS));
        if pending {
            row.pending += 1;
        }
        if mature {
            row.decided += 1;
        }
        if !ci_did_not_run(p) {
            row.ci_ran += 1;
            if rp.own_failures(p).is_some() {
                row.ci_own += 1;
            }
        }
        let mut events: Vec<TrackEvent> = Vec::new();
        let mut counted = false;
        if landed.is_some() {
            rp.reverts_of(p, &mut events);
            rp.follow_ups(p, &author, &mut events);
            rp.checklist(p, &mut events);
            counted = mature && !pending;
        } else if closed {
            let dup = rp.duplicate(p, &author, &mut events);
            let why = if dup {
                None
            } else {
                rp.declined(p, &author, &mut events)
            };
            counted = mature && (dup || why.is_none());
            if !dup {
                if let Some(why) = why {
                    let class = p
                        .derived
                        .state
                        .as_ref()
                        .map(|d| d.value.replace('_', " "))
                        .unwrap_or_else(|| "closed".into());
                    events.push(rp.event(
                        p,
                        "close",
                        false,
                        p.closed_at,
                        format!("Listed only, {class}: {why}"),
                        None,
                    ));
                }
            }
            rp.checklist(p, &mut events);
        }
        // One fault per pull request: the earliest counts, the rest are listed.
        // The newest of them still dates the person's last fault.
        let newest = events.iter().filter(|e| e.fault).filter_map(|e| e.at).max();
        let mut first = true;
        events.sort_by_key(|e| (!e.fault, e.at));
        for e in events.iter_mut().filter(|e| e.fault) {
            if !counted {
                e.fault = false;
                e.rule = format!(
                    "{} (not counted: {})",
                    e.rule,
                    not_counted_why(mature, pending)
                );
            } else if !first {
                e.fault = false;
                e.rule = format!(
                    "{} (also matched; one fault per pull request counts)",
                    e.rule
                );
            }
            first = false;
        }
        if counted {
            row.counted += 1;
            let fault = events.iter().find(|e| e.fault);
            if fault.is_some() {
                row.faults += 1;
                row.last_fault = row.last_fault.max(newest);
            }
            if landed.is_some() {
                row.landed += 1;
                let mc = p
                    .contribution
                    .commits
                    .iter()
                    .filter(|c| !c.is_merge)
                    .filter_map(|c| c.author.as_deref().map(canonical))
                    .filter(|a| *a != author && rp.m.contains(a))
                    .count() as u64;
                row.maintainer_commits += mc;
                if fault.is_none() && mc == 0 {
                    row.clean += 1;
                }
            }
        }
        row.events.extend(events);
    }
    acc.into_iter()
        .filter(|(_, a)| a.row.prs > 0)
        .map(|(key, mut a)| {
            a.row.key = key;
            a.row.repo = rp.repo.to_string();
            a.row.you = rp.viewer.as_deref() == Some(a.row.key.as_str());
            a.row.new_to_repo = a.first_seen.as_ref().is_some_and(|(_, assoc)| {
                matches!(assoc.as_str(), "FIRST_TIME_CONTRIBUTOR" | "FIRST_TIMER")
            });
            a.row.recent_fault = a
                .row
                .last_fault
                .filter(|at| now - *at <= chrono::Duration::days(BADGE_DAYS));
            a.row
                .events
                .sort_by_key(|e| (!e.fault, std::cmp::Reverse(e.at), e.number));
            a.row
        })
        .collect()
}

fn not_counted_why(mature: bool, pending: bool) -> &'static str {
    if !mature {
        "opened less than 7 days ago"
    } else if pending {
        "merged less than 72 hours ago"
    } else {
        "a close counts only as a maintainer's decline or a late duplicate"
    }
}

/// Whether a row's record is long enough for a level.
fn eligible(r: &TrackRow) -> bool {
    r.counted >= MIN_COUNTED
        && r.mature > 0
        && (r.decided as f64) / (r.mature as f64) >= MIN_DECIDED
}

fn pct(x: f64) -> String {
    let p = x * 100.0;
    if p > 0.0 && p < 1.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

/// The pooled baseline of `rows` (eligible, not the viewer), leaving out
/// `key`; `None` with fewer than [`MIN_AUTHORS`] authors.
fn pool<'a>(rows: impl Iterator<Item = &'a TrackRow>, key: &str, scope: &str) -> Option<Baseline> {
    let mut b = Baseline {
        scope: scope.into(),
        ..Default::default()
    };
    let mut authors: BTreeSet<String> = BTreeSet::new();
    for r in rows.filter(|r| r.key != key && !r.you && eligible(r)) {
        b.faults += r.faults;
        b.counted += r.counted;
        authors.insert(r.key.clone());
    }
    if authors.len() < MIN_AUTHORS || b.counted == 0 {
        return None;
    }
    b.rate = b.faults as f64 / b.counted as f64;
    b.authors = authors.into_iter().collect();
    Some(b)
}

/// The Track record of `caches` (`(repo, records)`), each record's
/// `derived.state` already set by the work states. `viewer` is the viewing
/// user's login, kept out of every baseline and listed apart.
pub fn build(
    caches: &[(String, PrCache)],
    cfg: &PeopleConfig,
    dir: &Directory,
    viewer: Option<&str>,
    now: Time,
) -> TrackRecord {
    let viewer = viewer.map(canonical);
    let window_start = now - chrono::Duration::days(i64::from(cfg.track_record_window_days));
    let mut rows: Vec<TrackRow> = Vec::new();
    for (repo, cache) in caches {
        let rp = Repo::new(repo, cache, cfg, viewer.clone(), now);
        rows.extend(repo_rows(&rp, window_start));
    }
    let levels: Vec<(Level, String, Option<Baseline>)> = rows
        .iter()
        .map(|r| {
            if !eligible(r) {
                let rule = format!(
                    "Not enough record: {} counted pull requests ({MIN_COUNTED} needed), and {} of {} pull requests at least {MATURE_DAYS} days old decided ({}% needed).",
                    r.counted,
                    r.decided,
                    r.mature,
                    (MIN_DECIDED * 100.0) as u32
                );
                return (Level::NotEnough, rule, None);
            }
            let here = pool(rows.iter().filter(|x| x.repo == r.repo), &r.key, "repo");
            let Some(base) = here.or_else(|| pool(rows.iter(), &r.key, "all")) else {
                let rule = format!(
                    "No baseline yet: fewer than {MIN_AUTHORS} other authors with a long enough record, in this repo or across the repos."
                );
                return (Level::NotEnough, rule, None);
            };
            let (low, high) = wilson(r.faults, r.counted).unwrap_or_default();
            let base_high = wilson(base.faults, base.counted).map_or(1.0, |(_, h)| h);
            let lv = level(r.faults, r.counted, base.rate, base_high, r.last_fault, now);
            let rate = r.faults as f64 / r.counted as f64;
            let from = if base.scope == "repo" {
                format!("{} other authors in this repo", base.authors.len())
            } else {
                format!("{} authors across the repos", base.authors.len())
            };
            let figures = format!(
                "{} of {} counted pull requests had a confirmed fault ({}, 90% interval {} to {}). The baseline is {} ({from}), with a 90% upper bound of {}",
                r.faults,
                r.counted,
                pct(rate),
                pct(low),
                pct(high),
                pct(base.rate),
                pct(base_high)
            );
            let rule = match lv {
                Level::Untrusted => format!(
                    "Untrusted: {figures}. The interval's lower bound is above the baseline's upper bound."
                ),
                Level::Trusted => format!(
                    "Trusted: {figures}. The rate is at or below the baseline, with no fault in the last {QUIET_DAYS} days."
                ),
                _ if rate > base.rate => format!(
                    "Review: {figures}. The rate is above the baseline, and the interval's lower bound is not above the baseline's upper bound."
                ),
                _ => format!(
                    "Review: {figures}. The rate is at or below the baseline, but a fault came in the last {QUIET_DAYS} days."
                ),
            };
            (lv, rule, Some(base))
        })
        .collect();
    for (r, (lv, rule, base)) in rows.iter_mut().zip(levels) {
        r.level = lv;
        r.rule = rule;
        r.baseline = base;
        // Not enough record shows no rate.
        if let Some((low, high)) = wilson(r.faults, r.counted).filter(|_| lv != Level::NotEnough) {
            r.low = Some(low);
            r.high = Some(high);
        }
        if let Some(p) = dir.people.get(&r.key) {
            r.login = p.login.clone();
            r.name = p.name.clone();
        } else {
            r.login = r.key.clone();
        }
    }
    rows.sort_by(|a, b| {
        a.you
            .cmp(&b.you)
            .then(a.level.cmp(&b.level))
            .then_with(|| match a.level {
                Level::NotEnough => a.first_activity.cmp(&b.first_activity),
                _ => b.last_fault.cmp(&a.last_fault),
            })
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.repo.cmp(&b.repo))
    });
    TrackRecord {
        window_days: cfg.track_record_window_days,
        rows,
    }
}

/// [`build`] when the config turns the Track record on, otherwise `None`.
pub fn for_config(
    caches: &[(String, PrCache)],
    cfg: &PeopleConfig,
    dir: &Directory,
    viewer: Option<&str>,
    now: Time,
) -> Option<TrackRecord> {
    cfg.track_record
        .then(|| build(caches, cfg, dir, viewer, now))
}

#[cfg(test)]
mod tests {
    use super::super::record::{
        Checks, Contribution, CrossSource, Decision, FileLines, MainChecks, MergeCommit, PrCommit,
        Suite, TimelineItem,
    };
    use super::*;

    const REPO: &str = "o/app";
    const M: &str = "maint-one";
    const ANN: &str = "ann-example";
    const BO: &str = "bo-example";
    const ME: &str = "me-example";

    fn now() -> Time {
        "2026-10-17T00:00:00Z".parse().unwrap()
    }
    fn d(days: i64) -> Time {
        now() - chrono::Duration::days(days)
    }
    fn hrs(h: i64) -> Time {
        now() - chrono::Duration::hours(h)
    }

    fn base(n: u64, author: &str, created: Time) -> PrRecord {
        PrRecord {
            number: n,
            url: format!("https://github.com/o/app/pull/{n}"),
            title: format!("Change {n}"),
            author: Some(author.into()),
            author_association: "CONTRIBUTOR".into(),
            created_at: Some(created),
            fetched_at: Some(created),
            contribution: Contribution {
                files: vec![FileLines {
                    path: format!("crates/app/src/f{n}.rs"),
                    additions: 10,
                    deletions: 1,
                }],
                ..Default::default()
            },
            ..Default::default()
        }
    }
    /// A merge commit oid that starts with the pull request's number.
    fn oid(n: u64) -> String {
        format!("{n:04}{}", "a".repeat(36))
    }
    fn merged(n: u64, author: &str, created: Time, at: Time) -> PrRecord {
        let mut r = base(n, author, created);
        r.state = PrState::Merged;
        r.merged_at = Some(at);
        r.closed_at = Some(at);
        r.merged_by = Some(M.into());
        r.contribution.merger = Some(M.into());
        r.contribution.merge_commit = Some(MergeCommit {
            oid: oid(n),
            headline: format!("Change {n} (#{n})"),
            parents: 1,
            ..Default::default()
        });
        r
    }
    fn closed(n: u64, author: &str, created: Time, at: Time, closer: &str) -> PrRecord {
        let mut r = base(n, author, created);
        r.state = PrState::Closed;
        r.closed_at = Some(at);
        r.timeline.push(TimelineItem {
            id: format!("cl{n}"),
            kind: "ClosedEvent".into(),
            at: Some(at),
            actor: Some(closer.into()),
            ..Default::default()
        });
        r
    }
    fn open(n: u64, author: &str, created: Time) -> PrRecord {
        base(n, author, created)
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
    fn xref(at: Time, number: u64, merged_at: Option<Time>) -> TimelineItem {
        TimelineItem {
            id: format!("x{number}-{at}"),
            kind: "CrossReferencedEvent".into(),
            at: Some(at),
            source: Some(CrossSource {
                kind: "PullRequest".into(),
                repo: REPO.into(),
                number,
                merged_at,
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    /// `count` other authors with five clean merges each, numbered from
    /// `from`, named after it.
    fn peers(from: u64, count: u64) -> Vec<PrRecord> {
        (0..count)
            .flat_map(|i| {
                (0..5).map(move |j| {
                    merged(from + i * 10 + j, &format!("peer-{from}-{i}"), d(40), d(39))
                })
            })
            .collect()
    }
    fn cfg() -> PeopleConfig {
        PeopleConfig {
            track_record: true,
            maintainers: vec![M.into()],
            ..Default::default()
        }
    }
    fn cache(records: Vec<PrRecord>) -> PrCache {
        PrCache {
            records: records.into_iter().map(|r| (r.number, r)).collect(),
            ..Default::default()
        }
    }
    fn run(records: Vec<PrRecord>, viewer: Option<&str>) -> TrackRecord {
        build(
            &[(REPO.into(), cache(records))],
            &cfg(),
            &Directory::default(),
            viewer,
            now(),
        )
    }
    fn row<'a>(t: &'a TrackRecord, key: &str) -> &'a TrackRow {
        t.rows.iter().find(|r| r.key == key).unwrap_or_else(|| {
            panic!(
                "no row for {key}: {:?}",
                t.rows.iter().map(|r| &r.key).collect::<Vec<_>>()
            )
        })
    }
    /// `(kind, fault, number)` of each event on a row.
    fn events(r: &TrackRow) -> Vec<(&str, bool, u64)> {
        r.events
            .iter()
            .map(|e| (e.kind.as_str(), e.fault, e.number))
            .collect()
    }

    #[test]
    fn wilson_bounds_are_the_90_percent_interval() {
        let close = |(lo, hi): (f64, f64), want: (f64, f64)| {
            assert!(
                (lo - want.0).abs() < 0.0005 && (hi - want.1).abs() < 0.0005,
                "{lo} {hi}"
            )
        };
        close(wilson(1, 20).unwrap(), (0.011, 0.196));
        close(wilson(2, 5).unwrap(), (0.143, 0.728));
        close(wilson(0, 5).unwrap(), (0.0, 0.351));
        assert_eq!(wilson(0, 0), None);
        // No rounding residue at the ends: a clean record against a clean
        // baseline is Trusted.
        assert_eq!(wilson(0, 79).unwrap().0, 0.0);
        assert_eq!(wilson(79, 79).unwrap().1, 1.0);
        assert_eq!(level(0, 79, 0.0, 0.0, None, now()), Level::Trusted);
    }

    #[test]
    fn levels_follow_the_interval_the_baseline_and_the_last_fault() {
        // 10 faults in 200: a 5% baseline with an upper bound near 8.2%.
        let (b, bh) = (0.05, wilson(10, 200).unwrap().1);
        assert!((bh - 0.082).abs() < 0.0005, "{bh}");
        assert_eq!(level(2, 5, b, bh, Some(d(40)), now()), Level::Untrusted);
        assert_eq!(level(1, 5, b, bh, Some(d(40)), now()), Level::Review);
        assert_eq!(level(0, 5, b, bh, None, now()), Level::Trusted);
        assert_eq!(level(1, 20, b, bh, Some(d(40)), now()), Level::Trusted);
        assert_eq!(level(1, 20, b, bh, Some(d(10)), now()), Level::Review);
        // The labels the view shows.
        let labels: Vec<&str> = [
            Level::Trusted,
            Level::Review,
            Level::Untrusted,
            Level::NotEnough,
        ]
        .iter()
        .map(|l| l.label())
        .collect();
        assert_eq!(
            labels,
            ["Trusted", "Review", "Untrusted", "Not enough record"]
        );
        assert_eq!(
            serde_json::to_string(&[Level::Untrusted, Level::NotEnough]).unwrap(),
            r#"["untrusted","not_enough"]"#
        );
    }

    /// A merged revert of `of` by a maintainer, numbered `n`.
    fn revert_of(n: u64, of: u64, at: Time) -> PrRecord {
        let mut r = merged(n, M, at, at);
        r.title = format!("Revert \"Change {of}\"");
        r.body = format!("Reverts o/app#{of}");
        r
    }

    #[test]
    fn a_zero_baseline_leaves_one_fault_in_thirty_at_review() {
        // No fault in 100: a baseline of 0 with an upper bound near 2.6%.
        let bh = wilson(0, 100).unwrap().1;
        assert!((bh - 0.0264).abs() < 0.0005, "{bh}");
        // One fault in 30: a lower bound near 0.74%, above 0 and below the
        // upper bound.
        let low = wilson(1, 30).unwrap().0;
        assert!(low > 0.0 && low < bh, "{low}");
        assert_eq!(level(1, 30, 0.0, bh, Some(d(40)), now()), Level::Review);
        assert_eq!(level(0, 30, 0.0, bh, None, now()), Level::Trusted);
        // The same in the view, against five peers with clean records.
        let mut records = peers(100, 5);
        records.extend((1..=30).map(|n| merged(n, ANN, d(50), d(49))));
        records.push(revert_of(40, 1, d(40)));
        let t = run(records, None);
        let ann = row(&t, ANN);
        assert_eq!((ann.faults, ann.counted), (1, 30));
        assert_eq!(
            ann.baseline.as_ref().map(|b| (b.faults, b.counted, b.rate)),
            Some((0, 25, 0.0))
        );
        assert_eq!(ann.level, Level::Review, "{}", ann.rule);
        assert!(
            ann.rule.ends_with(
                "The baseline is 0% (5 other authors in this repo), with a 90% upper bound of 10%. The rate is above the baseline, and the interval's lower bound is not above the baseline's upper bound."
            ),
            "{}",
            ann.rule
        );
    }

    #[test]
    fn a_person_clearly_worse_than_the_pool_is_still_untrusted() {
        // Five faults in ten: a lower bound near 27%, against a zero
        // baseline and against a 5% one.
        assert_eq!(
            level(5, 10, 0.0, wilson(0, 100).unwrap().1, Some(d(40)), now()),
            Level::Untrusted
        );
        assert_eq!(
            level(5, 10, 0.05, wilson(10, 200).unwrap().1, Some(d(40)), now()),
            Level::Untrusted
        );
        // Three faults in five against five peers with clean records: a
        // lower bound near 27% against an upper bound near 9.8%. The
        // reverts are days apart, so they are not a batch.
        let mut records = peers(100, 5);
        records.extend((1..=5).map(|n| merged(n, ANN, d(50), d(49))));
        records.extend((1..=3).map(|n| revert_of(10 + n, n, d(40 - 2 * n as i64))));
        let t = run(records, None);
        let ann = row(&t, ANN);
        assert_eq!((ann.faults, ann.counted), (3, 5));
        assert_eq!(ann.level, Level::Untrusted, "{}", ann.rule);
        assert!(
            ann.rule
                .ends_with("The interval's lower bound is above the baseline's upper bound."),
            "{}",
            ann.rule
        );
    }

    #[test]
    fn a_lower_bound_at_the_baselines_upper_bound_is_review() {
        let low = wilson(2, 5).unwrap().0;
        // Above a 5% baseline: Review while not above its upper bound.
        assert!(low > 0.05, "{low}");
        assert_eq!(level(2, 5, 0.05, low, Some(d(40)), now()), Level::Review);
        assert_eq!(level(2, 5, 0.05, 0.2, Some(d(40)), now()), Level::Review);
        assert_eq!(
            level(2, 5, 0.05, low - 1e-9, Some(d(40)), now()),
            Level::Untrusted
        );
        // A rate at the baseline: Trusted with no fault in 30 days, Review
        // with one.
        let bh = wilson(1, 20).unwrap().1;
        assert_eq!(level(1, 20, 0.05, bh, Some(d(31)), now()), Level::Trusted);
        assert_eq!(level(1, 20, 0.05, bh, Some(d(30)), now()), Level::Review);
    }

    #[test]
    fn a_level_needs_five_counted_pull_requests_and_most_of_them_decided() {
        let mut records = peers(100, 5);
        // Four counted merges.
        records.extend((1..=4).map(|n| merged(n, ANN, d(20), d(19))));
        // Five, with one listed close and two open, both a week old: 6 of 8.
        records.extend((11..=15).map(|n| merged(n, BO, d(20), d(19))));
        records.push(closed(16, BO, d(20), d(19), BO));
        records.extend((17..=18).map(|n| open(n, BO, d(20))));
        // Five, with three listed closes and two open: 8 of 10.
        records.extend((21..=25).map(|n| merged(n, "cy-example", d(20), d(19))));
        records.extend((26..=28).map(|n| closed(n, "cy-example", d(20), d(19), "cy-example")));
        records.extend((29..=30).map(|n| open(n, "cy-example", d(20))));
        let t = run(records, None);
        let ann = row(&t, ANN);
        assert_eq!((ann.counted, ann.level), (4, Level::NotEnough));
        assert_eq!(
            (ann.low, ann.high),
            (None, None),
            "Not enough record shows no rate"
        );
        assert!(
            ann.rule.starts_with("Not enough record: 4 counted"),
            "{}",
            ann.rule
        );
        let bo = row(&t, BO);
        assert_eq!(
            (bo.counted, bo.decided, bo.mature, bo.waiting),
            (5, 6, 8, 2)
        );
        assert_eq!(bo.level, Level::NotEnough);
        let cy = row(&t, "cy-example");
        assert_eq!((cy.counted, cy.decided, cy.mature), (5, 8, 10));
        assert_eq!(cy.level, Level::Trusted, "{}", cy.rule);
        assert!(cy.low.is_some() && cy.high.is_some());
        assert_eq!(cy.baseline.as_ref().map(|b| b.authors.len()), Some(5));
        assert_eq!(cy.clean, 5);
    }

    #[test]
    fn recent_merges_and_young_pull_requests_do_not_count() {
        let mut records = peers(100, 5);
        records.push(merged(1, ANN, d(20), hrs(13)));
        records.push(merged(2, ANN, d(5), d(4)));
        records.push(merged(3, ANN, d(20), d(10)));
        let t = run(records, None);
        let ann = row(&t, ANN);
        assert_eq!(
            (ann.prs, ann.pending, ann.counted, ann.mature),
            (3, 1, 1, 2)
        );
    }

    #[test]
    fn a_record_not_read_yet_is_not_decided() {
        let mut records = peers(100, 5);
        records.extend((1..=5).map(|n| merged(n, ANN, d(20), d(19))));
        let mut unread = closed(6, ANN, d(20), d(19), M);
        unread.fetched_at = None;
        unread.timeline.clear();
        records.push(unread);
        let t = run(records, None);
        let ann = row(&t, ANN);
        assert_eq!(
            (ann.prs, ann.mature, ann.decided, ann.counted),
            (6, 6, 5, 5)
        );
        assert!(events(ann).is_empty(), "{:?}", events(ann));
    }

    #[test]
    fn a_revert_counts_unless_replaced_reverted_again_or_part_of_a_quiet_batch() {
        let revert = |n: u64, of: u64, at: Time, body: &str| {
            let mut r = merged(n, M, at - chrono::Duration::hours(1), at);
            r.title = format!("Revert \"Change {of}\"");
            r.body = format!("Reverts o/app#{of}\n{body}");
            r
        };
        let mut records = peers(100, 5);
        records.push(merged(1, ANN, d(30), d(29)));
        records.push(revert(2, 1, d(28), ""));
        // Replaced by a later merged pull request the revert names.
        records.push(merged(3, ANN, d(30), d(29)));
        records.push(revert(4, 3, d(27), "Superseded by #5."));
        records.push(merged(5, BO, d(28), d(26)));
        // The revert was itself reverted.
        records.push(merged(6, ANN, d(30), d(29)));
        records.push(revert(7, 6, d(25), ""));
        let mut again = merged(8, M, d(24), d(24));
        again.title = "Revert \"Revert \"Change 6\"\"".into();
        again.body = "Reverts o/app#7".into();
        records.push(again);
        // Three reverts within ten minutes: one names a failure.
        records.push(merged(9, ANN, d(30), d(29)));
        records.push(merged(10, ANN, d(30), d(29)));
        records.push(merged(11, BO, d(30), d(29)));
        let at = d(20);
        records.push(revert(12, 9, at, ""));
        records.push(revert(
            13,
            10,
            at + chrono::Duration::minutes(4),
            "The keyboard test fails since this.",
        ));
        records.push(revert(14, 11, at + chrono::Duration::minutes(8), ""));
        // A revert found by the reverted commit's oid, with no Reverts line.
        records.push(merged(15, ANN, d(30), d(29)));
        let mut by_oid = merged(16, M, d(18), d(18));
        by_oid.title = "Undo the pen change".into();
        by_oid.contribution.commits = vec![PrCommit {
            oid: "f".repeat(40),
            // A headline that matches no pull request: only the oid names #15.
            headline: "Revert \"Pen: rough curves\"".into(),
            reverts: vec![oid(15)[..12].to_string()],
            ..Default::default()
        }];
        records.push(by_oid);
        // A squash commit that quotes "This reverts commit" is no revert.
        records.push(merged(17, ANN, d(30), d(29)));
        let mut squash = merged(18, M, d(17), d(17));
        squash.body = format!("This reverts commit {}.", oid(17));
        records.push(squash);
        // A maintainer comment the day of the revert names the replacement.
        let mut replaced = merged(19, ANN, d(30), d(29));
        replaced.timeline.push(comment(
            M,
            d(15) + chrono::Duration::hours(2),
            "Replaced by #21.",
        ));
        records.push(replaced);
        records.push(revert(20, 19, d(15), ""));
        records.push(merged(21, BO, d(16), d(14)));
        // Three reverts 9 minutes apart: no 10-minute span holds all three.
        let spread = d(12);
        for (i, n) in [30u64, 31, 32].into_iter().enumerate() {
            records.push(merged(n, BO, d(30), d(29)));
            let at = spread + chrono::Duration::minutes(9 * i as i64);
            records.push(revert(n + 10, n, at, ""));
        }
        // `git revert` of commits on #50's branch: the maintainer's review
        // commit is not Ann's work, her own commit is.
        let mut reviewed = merged(50, ANN, d(30), d(29));
        reviewed.contribution.commits = vec![
            PrCommit {
                oid: format!("{}1", "b".repeat(39)),
                author: Some(ANN.into()),
                headline: "Pen: smoother curves".into(),
                ..Default::default()
            },
            PrCommit {
                oid: format!("{}2", "b".repeat(39)),
                author: Some(M.into()),
                headline: "Pen: tidy (review of #50)".into(),
                ..Default::default()
            },
        ];
        records.push(reviewed);
        let git_revert = |n: u64, at: Time, headline: &str, oid: &str| {
            let mut r = merged(n, M, at, at);
            r.title = format!("Revert \"{headline}\"");
            r.body = format!("This reverts commit {oid}.");
            r
        };
        records.push(git_revert(
            51,
            d(11),
            "Pen: tidy (review of #50)",
            &format!("{}2", "b".repeat(39)),
        ));
        // A revert of a commit pushed straight to main, whose headline is
        // also the title of Bo's #52: the oid names no pull request.
        let mut same_title = merged(52, BO, d(30), d(29));
        same_title.title = "Docs: update ROADMAP".into();
        records.push(same_title);
        records.push(git_revert(
            53,
            d(10),
            "Docs: update ROADMAP",
            &"c".repeat(40),
        ));
        // A `git revert` of Ann's own commit on #54's branch.
        let mut own = merged(54, ANN, d(30), d(29));
        own.contribution.commits = vec![PrCommit {
            oid: "d".repeat(40),
            author: Some(ANN.into()),
            headline: "Brush: pressure".into(),
            ..Default::default()
        }];
        records.push(own);
        records.push(git_revert(55, d(9), "Brush: pressure", &"d".repeat(12)));
        let t = run(records, None);
        let ann = row(&t, ANN);
        let ev = events(ann);
        assert!(ev.contains(&("revert", true, 1)), "{ev:?}");
        assert!(ev.contains(&("process_revert", false, 3)), "{ev:?}");
        assert!(ev.contains(&("process_revert", false, 6)), "{ev:?}");
        assert!(ev.contains(&("process_revert", false, 9)), "{ev:?}");
        assert!(
            ev.contains(&("revert", true, 10)),
            "a failure named: {ev:?}"
        );
        assert!(ev.contains(&("revert", true, 15)), "found by oid: {ev:?}");
        assert!(!ev.iter().any(|e| e.2 == 17), "{ev:?}");
        assert!(
            !ev.iter().any(|e| e.0 == "possible_follow_up"),
            "a revert is no follow-up: {ev:?}"
        );
        let rule = |n: u64| &ann.events.iter().find(|e| e.number == n).unwrap().rule;
        assert!(rule(3).contains("#5 replaced the work"), "{}", rule(3));
        assert!(rule(6).contains("#8 reverted the revert"), "{}", rule(6));
        assert!(rule(9).contains("one of 3 reverts"), "{}", rule(9));
        assert!(rule(19).contains("#21 replaced the work"), "{}", rule(19));
        assert!(
            !ev.iter().any(|e| e.2 == 50),
            "a revert of a maintainer's commit on the branch: {ev:?}"
        );
        assert!(ev.contains(&("revert", true, 54)), "her own commit: {ev:?}");
        assert_eq!(ann.faults, 4);
        let bo = row(&t, BO);
        assert!(
            !events(bo).iter().any(|e| e.2 == 52),
            "an oid outside every pull request: {:?}",
            events(bo)
        );
        assert!(events(bo).contains(&("process_revert", false, 11)));
        for n in [30, 31, 32] {
            assert!(
                events(bo).contains(&("revert", true, n)),
                "{:?}",
                events(bo)
            );
        }
    }

    #[test]
    fn a_follow_up_is_listed_and_never_counts_without_the_changed_lines() {
        let fix = |n: u64, author: &str, of: u64, created: Time, body: &str| {
            let mut q = merged(n, author, created, created + chrono::Duration::hours(2));
            q.body = body.into();
            q.contribution.files = vec![FileLines {
                path: format!("crates/app/src/f{of}.rs"),
                ..Default::default()
            }];
            q
        };
        let mut records = peers(100, 5);
        records.push(merged(1, ANN, d(30), d(29)));
        records.push(fix(
            2,
            BO,
            1,
            d(28),
            "The test from #1 was intermittently failing; this fixes it.",
        ));
        // A precedent citation is not a follow-up.
        records.push(merged(3, ANN, d(30), d(29)));
        records.push(fix(
            4,
            BO,
            3,
            d(28),
            "Handles the wrong offset in the same way as #3.",
        ));
        // By the maintainer who merged #5.
        records.push(merged(5, ANN, d(30), d(29)));
        records.push(fix(6, M, 5, d(28), "#5 broke the pen."));
        // By the viewing user.
        records.push(merged(7, ANN, d(30), d(29)));
        records.push(fix(8, ME, 7, d(28), "#7 broke the pen."));
        // Opened before #9 landed.
        records.push(merged(9, ANN, d(30), d(25)));
        records.push(fix(10, BO, 9, d(27), "#9 broke the pen."));
        // A failure outside the gate.
        records.push(merged(11, ANN, d(30), d(29)));
        records.push(fix(12, BO, 11, d(28), "wasm32 clippy fails since #11."));
        // No product file in common.
        records.push(merged(13, ANN, d(30), d(29)));
        let mut far = fix(14, BO, 13, d(28), "#13 broke the pen.");
        far.contribution.files[0].path = "docs/pen.md".into();
        records.push(far);
        // By a bot or an agent account.
        records.push(merged(15, ANN, d(30), d(29)));
        records.push(fix(16, "dependabot[bot]", 15, d(28), "#15 broke the pen."));
        records.push(fix(17, "cursoragent", 15, d(28), "#15 broke the pen."));
        let mut c = cfg();
        c.gate_excluded_terms
            .insert(REPO.into(), vec!["wasm32 clippy".into()]);
        let t = build(
            &[(REPO.into(), cache(records))],
            &c,
            &Directory::default(),
            Some(ME),
            now(),
        );
        let ann = row(&t, ANN);
        let ev = events(ann);
        // Someone else's fix that names #1 with a defect word and changes its
        // file: the records hold no changed lines, so it is not confirmed.
        assert!(ev.contains(&("possible_follow_up", false, 1)), "{ev:?}");
        assert!(!ev.iter().any(|e| e.0 == "follow_up"), "{ev:?}");
        let one = ann.events.iter().find(|e| e.number == 1).unwrap();
        assert!(
            one.rule
                .contains("#2 by bo-example names it with a defect word and also changes crates/app/src/f1.rs, but F2 needs the changed lines to overlap"),
            "{}",
            one.rule
        );
        assert_eq!(one.other.as_ref().map(|o| o.number), Some(2));
        assert_eq!(one.at, Some(d(28) + chrono::Duration::hours(2)));
        assert!(ev.contains(&("precedent", false, 3)), "{ev:?}");
        assert!(ev.contains(&("maintainer_revision", false, 5)), "{ev:?}");
        let mine = ann.events.iter().find(|e| e.number == 7).unwrap();
        assert_eq!(
            (mine.kind.as_str(), mine.fault, mine.involves_you),
            ("found_by_you", false, true)
        );
        assert!(!ev.iter().any(|e| e.2 == 9), "{ev:?}");
        assert!(ev.contains(&("possible_follow_up", false, 11)), "{ev:?}");
        assert!(ev.contains(&("possible_follow_up", false, 13)), "{ev:?}");
        assert!(
            !ev.iter().any(|e| e.2 == 15),
            "bots are never the follow-up author: {ev:?}"
        );
        assert_eq!(ann.faults, 0);
    }

    #[test]
    fn a_ticked_gate_box_counts_only_over_the_pull_requests_own_failure() {
        let ticked = |n: u64, failing: &[&str], approval: bool| {
            let mut r = merged(n, ANN, d(30), d(29));
            r.body = "## Checklist\n- [x] `cargo xtask ci` passes\n".into();
            r.checks = Checks {
                rollup: Some("FAILURE".into()),
                failing: failing.iter().map(|s| s.to_string()).collect(),
                failing_statuses: Some(Vec::new()),
                suites_total: Some(1),
                suites: vec![Suite {
                    status: "COMPLETED".into(),
                    conclusion: Some(
                        if approval {
                            "ACTION_REQUIRED"
                        } else {
                            "FAILURE"
                        }
                        .into(),
                    ),
                    created_at: Some(d(30) + chrono::Duration::hours(1)),
                    app: Some("github-actions".into()),
                }],
                ..Default::default()
            };
            r
        };
        let mut records = peers(100, 5);
        records.push(ticked(1, &["test"], false));
        records.push(ticked(2, &["FreeBSD"], false));
        records.push(ticked(3, &["Flaky one"], false));
        records.push(ticked(4, &["test"], true));
        let mut unticked = ticked(5, &["test"], false);
        unticked.body = "- [ ] `cargo xtask ci` passes".into();
        records.push(unticked);
        // `lint` failed on #6's head, but main has no passing result for it.
        records.push(ticked(6, &["lint"], false));
        // Closed by its author: the box is listed, and the close does not count.
        let mut withdrawn = ticked(7, &["test"], false);
        withdrawn.state = PrState::Closed;
        withdrawn.merged_at = None;
        withdrawn.timeline.push(TimelineItem {
            kind: "ClosedEvent".into(),
            at: withdrawn.closed_at,
            actor: Some(ANN.into()),
            ..Default::default()
        });
        records.push(withdrawn);
        // #8's checks ran when no sweep saw main's head.
        let mut unseen = ticked(8, &["test"], false);
        unseen.checks.suites[0].created_at = Some(d(35));
        records.push(unseen);
        // #9's FreeBSD failed in one of two suites, one started while main
        // failed FreeBSD and one after main passed it.
        let mut two = ticked(9, &["FreeBSD"], false);
        let mut later = two.checks.suites[0].clone();
        later.created_at = Some(d(28));
        two.checks.suites.push(later);
        two.checks.suites_total = Some(2);
        records.push(two);
        // #10's failure came from a commit status: no suite dates the run.
        let mut status = ticked(10, &["test"], false);
        status.checks.suites.clear();
        status.contribution.commits = vec![PrCommit {
            oid: "e".repeat(40),
            author: Some(ANN.into()),
            committed_at: Some(d(30) + chrono::Duration::hours(1)),
            ..Default::default()
        }];
        records.push(status);
        // #11's `test` is a failing commit status beside a dated suite: the
        // suite's span does not date the status.
        let mut beside = ticked(11, &["test"], false);
        beside.checks.failing_statuses = Some(vec!["Test".into()]);
        records.push(beside);
        // #12's record does not say which of its failures are commit
        // statuses.
        let mut unclassified = ticked(12, &["test"], false);
        unclassified.checks.failing_statuses = None;
        records.push(unclassified);
        // #13's head has nine check suites, and the record holds one.
        let mut partial = ticked(13, &["test"], false);
        partial.checks.suites_total = Some(9);
        records.push(partial);
        let mut c = cfg();
        c.flaky_checks.insert(REPO.into(), vec!["flaky one".into()]);
        let mut cache = cache(records);
        // Main failed FreeBSD while the checks ran, and passes it now.
        cache.main_history = vec![MainChecks {
            oid: "h".into(),
            rollup: Some("FAILURE".into()),
            failing: vec!["FreeBSD".into()],
            passing: vec!["Test".into(), "flaky one".into()],
            first_seen: Some(d(31)),
            last_seen: Some(d(29)),
        }];
        cache.note_main(
            Some(MainChecks {
                oid: "h2".into(),
                rollup: Some("SUCCESS".into()),
                passing: vec!["Test".into(), "FreeBSD".into()],
                ..Default::default()
            }),
            d(29) + chrono::Duration::hours(1),
        );
        cache.note_main(
            Some(MainChecks {
                oid: "h2".into(),
                rollup: Some("SUCCESS".into()),
                passing: vec!["Test".into(), "FreeBSD".into()],
                ..Default::default()
            }),
            d(27),
        );
        cache.note_main(
            Some(MainChecks {
                oid: "now".into(),
                rollup: Some("SUCCESS".into()),
                passing: vec![
                    "Test".into(),
                    "FreeBSD".into(),
                    "lint".into(),
                    "flaky one".into(),
                ],
                ..Default::default()
            }),
            now(),
        );
        let t = build(
            &[(REPO.into(), cache.clone())],
            &c,
            &Directory::default(),
            None,
            now(),
        );
        let ann = row(&t, ANN);
        let faults: Vec<u64> = ann
            .events
            .iter()
            .filter(|e| e.fault)
            .map(|e| e.number)
            .collect();
        assert_eq!(faults, [1]);
        assert!(
            ann.events[0].rule.contains("test failed"),
            "{}",
            ann.events[0].rule
        );
        assert!(
            ann.events[0].rule.ends_with("passed on main when it ran"),
            "{}",
            ann.events[0].rule
        );
        // #1, #5 (unticked) and #7 (closed); FreeBSD on #2 failed on main
        // too, no sweep saw main while #8's checks ran, main did not pass
        // FreeBSD throughout #9's, nothing dates #10's or #11's, #12's record
        // does not tell a commit status from a check run, and #13's holds
        // only some of its suites.
        assert_eq!((ann.ci_ran, ann.ci_own), (13, 3));
        let box7 = ann
            .events
            .iter()
            .find(|e| e.number == 7 && e.kind == "checklist")
            .unwrap();
        assert!(!box7.fault);
        assert!(
            box7.rule.ends_with(
                "(not counted: a close counts only as a maintainer's decline or a late duplicate)"
            ),
            "{}",
            box7.rule
        );
        // Without a history of main nothing is confirmed.
        cache.main_history.clear();
        let t = build(
            &[(REPO.into(), cache)],
            &c,
            &Directory::default(),
            None,
            now(),
        );
        assert_eq!(row(&t, ANN).faults, 0);
    }

    #[test]
    fn a_check_name_that_also_failed_on_main_is_not_the_pull_requests_own() {
        let mut p = merged(1, ANN, d(30), d(29));
        p.body = "- [x] `cargo xtask ci` passes\n".into();
        p.checks = Checks {
            rollup: Some("FAILURE".into()),
            failing: vec!["test".into()],
            failing_statuses: Some(Vec::new()),
            suites_total: Some(1),
            suites: vec![Suite {
                status: "COMPLETED".into(),
                conclusion: Some("FAILURE".into()),
                created_at: Some(d(30) + chrono::Duration::hours(1)),
                app: Some("github-actions".into()),
            }],
            ..Default::default()
        };
        let mut records = peers(100, 5);
        records.push(p);
        let mut cache = cache(records);
        // Two workflows on main run a check named `test`: one passed, one failed.
        let main = |failing: &[&str]| MainChecks {
            oid: "h".into(),
            rollup: Some("FAILURE".into()),
            failing: failing.iter().map(|s| s.to_string()).collect(),
            passing: vec!["test".into()],
            first_seen: Some(d(31)),
            last_seen: Some(d(29)),
        };
        let faults = |cache: &PrCache| {
            let t = build(
                &[(REPO.into(), cache.clone())],
                &cfg(),
                &Directory::default(),
                None,
                now(),
            );
            row(&t, ANN).faults
        };
        cache.main_history = vec![main(&["test"])];
        assert_eq!(faults(&cache), 0);
        cache.main_history = vec![main(&[])];
        assert_eq!(faults(&cache), 1, "passing alone on main");
    }

    #[test]
    fn a_revert_that_blames_a_later_pull_request_still_counts() {
        let mut records = peers(100, 5);
        records.push(merged(1, ANN, d(30), d(29)));
        records.push(merged(2, BO, d(29), d(28)));
        let mut revert = merged(3, M, d(27), d(27));
        revert.title = "Revert \"Change 1\"".into();
        revert.body = "Reverts o/app#1\nThis broke the feature added in #2.".into();
        records.push(revert);
        records.push(merged(4, ANN, d(30), d(29)));
        records.push(merged(5, BO, d(29), d(26)));
        let mut replaced = merged(6, M, d(27), d(27));
        replaced.title = "Revert \"Change 4\"".into();
        replaced.body = "Reverts o/app#4\nThe work moved to #5.".into();
        records.push(replaced);
        let t = run(records, None);
        let ev = events(row(&t, ANN));
        assert!(ev.contains(&("revert", true, 1)), "{ev:?}");
        assert!(ev.contains(&("process_revert", false, 4)), "{ev:?}");
    }

    #[test]
    fn a_revert_of_another_repos_pull_request_matches_no_title_here() {
        let mut records = peers(100, 5);
        records.push(merged(1, ANN, d(30), d(29)));
        let mut elsewhere = merged(2, M, d(27), d(27));
        elsewhere.title = "Revert \"Change 1\"".into();
        elsewhere.body = "Reverts other/lib#1".into();
        records.push(elsewhere);
        let t = run(records, None);
        let ev = events(row(&t, ANN));
        assert!(ev.is_empty(), "{ev:?}");
    }

    #[test]
    fn a_maintainer_close_counts_when_nothing_took_its_place() {
        let mut records = peers(100, 5);
        let mut declined = closed(1, ANN, d(30), d(25), M);
        declined.derived.state = Some(Decision {
            value: "closed_declined".into(),
            rule: String::new(),
            subtype: None,
        });
        records.push(declined);
        // The same close, with a merged pull request for the same issue
        // within the week.
        let mut superseded = closed(2, ANN, d(30), d(25), M);
        superseded.closing_issues = vec![77];
        records.push(superseded);
        let mut fix = merged(3, BO, d(26), d(23));
        fix.title = "Fix #77: the pen".into();
        records.push(fix);
        // Closed by the author.
        records.push(closed(4, ANN, d(30), d(25), ANN));
        // Landed by another route, as the work states found it.
        let mut landed = closed(5, ANN, d(30), d(25), M);
        landed.derived.state = Some(Decision {
            value: LANDED_ELSEWHERE.into(),
            rule: String::new(),
            subtype: None,
        });
        records.push(landed);
        // Referenced by a merged pull request three days after the close.
        let mut referenced = closed(6, ANN, d(30), d(25), M);
        referenced.timeline.push(xref(d(22), 9, Some(d(22))));
        records.push(referenced);
        // Referenced before the close by a pull request merged a day after it.
        let mut early = closed(7, ANN, d(30), d(25), M);
        early.timeline.push(xref(d(27), 10, Some(d(24))));
        records.push(early);
        // A maintainer's #12, which says it supersedes #11, merged two hours
        // before the maintainer closed #11: the work states found it
        // superseded.
        let mut superseded_early = closed(11, ANN, d(30), d(25), M);
        superseded_early.timeline.push(xref(
            d(25) - chrono::Duration::hours(3),
            12,
            Some(d(25) - chrono::Duration::hours(2)),
        ));
        superseded_early.derived.state = Some(Decision {
            value: "closed_superseded".into(),
            rule: String::new(),
            subtype: Some("by_maintainer".into()),
        });
        records.push(superseded_early);
        let mut by_m = merged(12, M, d(26), d(25) - chrono::Duration::hours(2));
        by_m.body = "Supersedes #11.".into();
        records.push(by_m);
        // An empty pull request a maintainer closed within the hour.
        let mut quick = closed(13, ANN, d(30), d(30) + chrono::Duration::minutes(20), M);
        quick.derived.state = Some(Decision {
            value: "closed_withdrawn_or_abandoned".into(),
            rule: String::new(),
            subtype: Some("accidental_quick_close".into()),
        });
        records.push(quick);
        let t = run(records, None);
        let ann = row(&t, ANN);
        let ev = events(ann);
        assert!(ev.contains(&("declined", true, 1)), "{ev:?}");
        assert!(ann
            .events
            .iter()
            .any(|e| e.number == 1 && e.rule.contains("closed declined")));
        assert!(ev.contains(&("close", false, 2)), "{ev:?}");
        let rule = |n: u64| &ann.events.iter().find(|e| e.number == n).unwrap().rule;
        assert!(
            rule(2).contains("#3 landed for the same issue"),
            "{}",
            rule(2)
        );
        assert!(ev.contains(&("close", false, 4)), "{ev:?}");
        assert!(
            rule(6).contains("#9, a merged pull request, references it and came within 7 days"),
            "{}",
            rule(6)
        );
        assert!(
            rule(7).contains("#10, a merged pull request"),
            "{}",
            rule(7)
        );
        assert!(!ev.iter().any(|e| e.2 == 5), "{ev:?}");
        assert!(ev.contains(&("close", false, 11)), "{ev:?}");
        assert!(
            rule(11).contains("closed superseded: the work states found it superseded"),
            "{}",
            rule(11)
        );
        assert!(ev.contains(&("close", false, 13)), "{ev:?}");
        assert!(
            rule(13).contains("closed by accident within an hour of opening"),
            "{}",
            rule(13)
        );
        assert_eq!((ann.counted, ann.faults, ann.landed), (2, 1, 1));
    }

    #[test]
    fn a_late_duplicate_counts_and_a_quick_self_close_does_not() {
        let mut records = peers(100, 5);
        let mut original = open(1, BO, d(30));
        original.title = "Fix #50: the pen".into();
        records.push(original);
        let mut dup = closed(2, ANN, d(29), d(28), M);
        dup.closing_issues = vec![50];
        dup.timeline.push(xref(d(29), 1, None));
        records.push(dup);
        // Closed by its author 13 minutes after opening.
        let mut quick = closed(3, ANN, d(29), d(29) + chrono::Duration::minutes(13), ANN);
        quick.closing_issues = vec![50];
        quick.timeline.push(xref(d(29), 1, None));
        records.push(quick);
        // The other pull request is only five hours older.
        let mut close_in = open(4, BO, d(20));
        close_in.title = "Fix #60: the brush".into();
        records.push(close_in);
        let mut near = closed(5, ANN, d(20) + chrono::Duration::hours(5), d(19), M);
        near.closing_issues = vec![60];
        near.timeline.push(xref(d(20), 4, None));
        records.push(near);
        // A resubmission of a pull request that had closed without landing.
        let mut gone = closed(6, BO, d(40), d(35), BO);
        gone.title = "Fix #70: the knife".into();
        records.push(gone);
        let mut again = closed(7, ANN, d(30), d(28), ANN);
        again.closing_issues = vec![70];
        again.timeline.push(xref(d(30), 6, None));
        records.push(again);
        let t = run(records, Some(BO));
        let ann = row(&t, ANN);
        let ev = events(ann);
        assert!(ev.contains(&("duplicate", true, 2)), "{ev:?}");
        let dup = ann.events.iter().find(|e| e.number == 2).unwrap();
        assert!(dup.involves_you, "the original is the viewing user's");
        assert!(ev.contains(&("close", false, 3)), "{ev:?}");
        assert!(ev.contains(&("close", false, 5)), "{ev:?}");
        assert!(ev.contains(&("close", false, 7)), "a resubmission: {ev:?}");
        let near = ann.events.iter().find(|e| e.number == 5).unwrap();
        assert!(
            near.rule.contains("#4 by another author was open"),
            "{}",
            near.rule
        );
    }

    #[test]
    fn baselines_pool_other_authors_and_leave_out_the_viewer() {
        // Two other authors here: the baseline comes from every repo.
        let mut here = peers(100, 2);
        here.extend((1..=5).map(|n| merged(n, ANN, d(20), d(19))));
        here.extend((6..=10).map(|n| merged(n, ME, d(20), d(19))));
        let there = peers(200, 3);
        let caches = vec![
            (REPO.to_string(), cache(here)),
            ("o/other".to_string(), cache(there)),
        ];
        let t = build(&caches, &cfg(), &Directory::default(), Some(ME), now());
        let ann = t.rows.iter().find(|r| r.key == ANN).unwrap();
        let b = ann.baseline.as_ref().unwrap();
        assert_eq!((b.scope.as_str(), b.authors.len()), ("all", 5));
        assert!(!b.authors.contains(&ME.to_string()), "{:?}", b.authors);
        let me = t.rows.iter().find(|r| r.key == ME).unwrap();
        assert!(me.you);
        assert_eq!(
            t.rows.last().map(|r| r.key.as_str()),
            Some(ME),
            "the viewer's rows come last"
        );
        // Fewer than five other authors anywhere.
        let t = build(&caches[..1], &cfg(), &Directory::default(), Some(ME), now());
        let ann = t.rows.iter().find(|r| r.key == ANN).unwrap();
        assert_eq!(ann.level, Level::NotEnough);
        assert!(ann.rule.starts_with("No baseline yet"), "{}", ann.rule);
    }

    #[test]
    fn rows_go_by_level_then_newest_fault_and_leave_out_maintainers_and_bots() {
        let mut records = peers(100, 5);
        // Two people with three faults in five, a day apart: Untrusted, the
        // newer fault first.
        for (who, from, at) in [("ux-old", 1, 40), ("ux-new", 20, 20)] {
            records.extend((from..from + 5).map(|n| merged(n, who, d(50), d(49))));
            for i in 0..3 {
                records.push(revert_of(from + 10 + i, from + i, d(at + 2 - i as i64)));
            }
        }
        // One fault in five: Review.
        records.extend((50..55).map(|n| merged(n, "rv-one", d(50), d(49))));
        records.push(revert_of(60, 50, d(30)));
        // Little record: by first activity.
        records.push(merged(70, "late-one", d(10), d(9)));
        records.push(merged(71, "early-one", d(50), d(49)));
        // Bots and maintainers get no row.
        records.push(merged(80, "cursoragent", d(20), d(19)));
        records.push(merged(81, "dependabot[bot]", d(20), d(19)));
        records.push(merged(82, M, d(20), d(19)));
        let t = run(records, None);
        let order: Vec<(&str, Level)> = t
            .rows
            .iter()
            .filter(|r| !r.key.starts_with("peer-"))
            .map(|r| (r.key.as_str(), r.level))
            .collect();
        assert_eq!(
            order,
            [
                ("ux-new", Level::Untrusted),
                ("ux-old", Level::Untrusted),
                ("rv-one", Level::Review),
                ("early-one", Level::NotEnough),
                ("late-one", Level::NotEnough),
            ]
        );
        assert!(t.rows.iter().take(3).all(|r| r.last_fault.is_some()));
        assert_eq!(
            row(&t, "ux-new").recent_fault,
            None,
            "20 days old: no badge"
        );
    }

    #[test]
    fn a_fault_in_the_last_week_shows_a_badge() {
        let mut records = peers(100, 5);
        records.extend((1..=5).map(|n| merged(n, ANN, d(30), d(29))));
        let mut r = merged(6, M, d(3), d(3));
        r.title = "Revert \"Change 1\"".into();
        r.body = "Reverts o/app#1".into();
        records.push(r);
        // An earlier revert of #1 is the fault that counts; the later one
        // still dates the last fault.
        let mut first = merged(7, M, d(27), d(27));
        first.title = "Revert \"Change 1\"".into();
        first.body = "Reverts o/app#1".into();
        records.push(first);
        let t = run(records, None);
        let ann = row(&t, ANN);
        let counted: Vec<(&str, Option<Time>)> = ann
            .events
            .iter()
            .filter(|e| e.fault)
            .map(|e| (e.kind.as_str(), e.at))
            .collect();
        assert_eq!(counted, [("revert", Some(d(27)))]);
        assert_eq!(ann.last_fault, Some(d(3)));
        assert_eq!(ann.recent_fault, Some(d(3)));
        // One fault in five against five clean peers: the lower bound, near
        // 4.6%, is not above the baseline's upper bound, near 9.8%.
        assert_eq!(ann.level, Level::Review, "{}", ann.rule);
    }

    #[test]
    fn new_contributors_are_marked_and_the_switch_turns_the_view_off() {
        let mut first = merged(1, ANN, d(90), d(89));
        first.author_association = "FIRST_TIME_CONTRIBUTOR".into();
        // No association with the repo says nothing about a first contribution.
        let mut outside = merged(3, BO, d(20), d(19));
        outside.author_association = "NONE".into();
        let records = vec![first, merged(2, ANN, d(20), d(19)), outside];
        let caches = vec![(REPO.to_string(), cache(records))];
        let mut c = cfg();
        let t = for_config(&caches, &c, &Directory::default(), None, now()).unwrap();
        let ann = row(&t, ANN);
        assert!(ann.new_to_repo);
        assert_eq!(ann.prs, 1, "the window is 60 days");
        assert!(!row(&t, BO).new_to_repo);
        c.track_record = false;
        assert_eq!(
            for_config(&caches, &c, &Directory::default(), None, now()),
            None
        );
    }

    #[test]
    fn maintainers_are_whoever_merged_or_closed_three_of_the_last_two_hundred() {
        let mut records: Vec<PrRecord> = (1..=3).map(|n| merged(n, ANN, d(9), d(8))).collect();
        records.extend((4..=6).map(|n| closed(n, ANN, d(9), d(8), "closer-x")));
        records.extend((7..=8).map(|n| closed(n, ANN, d(9), d(8), "two-only")));
        let mut member = open(9, "member-y", d(5));
        member.author_association = "MEMBER".into();
        records.push(member);
        records.extend((10..=12).map(|n| closed(n, ANN, d(9), d(8), "github-actions")));
        let m = maintainers(
            &cache(records),
            &["extra-one".into()],
            &["Owner-One".into()],
        );
        let want: BTreeSet<String> = [M, "closer-x", "extra-one", "owner-one"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(m, want);
    }

    #[test]
    fn a_precedent_is_cited_in_any_reference_form_of_this_repo() {
        for s in [
            "fixes the crash like #12.",
            "fixes the crash like o/app#12.",
            "fixes the crash as https://github.com/o/app/pull/12 did.",
            "fixes the crash in the same way.",
        ] {
            assert!(cites(s, 12, REPO), "{s}");
        }
        for s in [
            "fixes the crash like #120.",
            "fixes the crash like other/app#12.",
            "fixes the crash from #12.",
        ] {
            assert!(!cites(s, 12, REPO), "{s}");
        }
    }

    #[test]
    fn references_name_numbers_in_this_repo_only() {
        assert_eq!(
            refs("see #12, o/app#13, Other/X#14, O/App#15 and https://github.com/o/app/pull/16 or https://github.com/x/y/issues/17", REPO),
            [12, 13, 15, 16]
        );
    }
}
