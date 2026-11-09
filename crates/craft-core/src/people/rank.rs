//! Who wrote each merged pull request's lines ([`PrCredit`]), and the
//! rankings built from that and from the [`Ledger`] ([`Rankings`]): one row
//! per person with their figures in every activity window, for all
//! repositories or one, and the per-person tables of where their work went
//! (by repository, area, type and directory).
//!
//! A merged pull request's product lines (every file but `Cargo.lock`,
//! translation catalogs and generated files) are split over its non-merge
//! commits by each commit's share of the commits' lines. A commit that a
//! pull request merged earlier in the same repository already holds is
//! credited there only (stacked branches). A commit without a linked
//! account, and a pull request whose commits add no lines or that has more
//! commits than the fetch read, credit their share to the author.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::kind::{self, Kinded, PathClass};
use super::ledger::{Kind, Ledger};
use super::record::{PrCache, PrRecord, PrState};
use super::states::WorkState;
use super::{canonical, is_bot, Directory};
use crate::model::{Time, ALL_TIME, SINCE_RELEASE, WINDOWS};

/// The directory key that groups translation catalogs.
pub const TRANSLATIONS: &str = "translations";
/// The directory key that groups `Cargo.lock` and generated files.
pub const GENERATED: &str = "generated";

/// One person's part of a merged pull request.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Share {
    /// The canonical login.
    pub login: String,
    /// Product lines added and removed.
    pub product: (f64, f64),
    /// `Cargo.lock`, catalog and generated lines added and removed.
    pub generated: (f64, f64),
    /// Their part of the pull request's lines, 0 to 1.
    pub weight: f64,
    /// Commits they authored that this pull request is credited with.
    pub commits: u64,
    /// They are not the pull request's author.
    pub review: bool,
}

/// One changed file's lines, by directory (see [`dir_of`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DirLines {
    pub dir: String,
    pub module: String,
    pub added: u64,
    pub removed: u64,
}

/// A `Co-authored-by:` trailer on a credited commit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CoCredit {
    /// The canonical login, from a GitHub address or an address a commit
    /// author used; `None` when no account is known.
    pub login: Option<String>,
    pub name: String,
}

/// What one merged pull request contributed and to whom.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PrCredit {
    pub repo: String,
    pub number: u64,
    /// The canonical login of the author; `None` for a bot, AI or deleted
    /// account.
    pub author: Option<String>,
    pub merged_at: Option<Time>,
    /// Who landed it (see [`super::merger`]), canonical.
    pub merger: Option<String>,
    /// The work-state rules found it landed in a batch.
    pub batch: bool,
    pub kind: Kinded,
    pub changed_files: u64,
    pub shares: Vec<Share>,
    /// Every commit was credited to an earlier pull request: that one.
    pub stacked_in: Option<u64>,
    /// The commit list or the timeline was cut short by the fetch.
    pub truncated: bool,
    pub dirs: Vec<DirLines>,
    pub co_authors: Vec<CoCredit>,
}

/// The directory a path counts under and its module: two segments under
/// `crates/` and `apps/`, otherwise the top directory (a file at the root
/// counts under its own name); catalogs under [`TRANSLATIONS`] and
/// `Cargo.lock` and generated files under [`GENERATED`]. The module is the
/// next segment, or the one after `src/`, without `.rs`.
pub fn dir_of(path: &str) -> (String, String) {
    match kind::path_class(path) {
        PathClass::I18n => return (TRANSLATIONS.into(), String::new()),
        PathClass::Generated => return (GENERATED.into(), String::new()),
        _ if path == "Cargo.lock" || path.ends_with("/Cargo.lock") => {
            return (GENERATED.into(), String::new())
        }
        _ => {}
    }
    let segs: Vec<&str> = path.split('/').collect();
    let (dir, rest) = match segs.as_slice() {
        [top @ ("crates" | "apps"), name, rest @ ..] => (format!("{top}/{name}"), rest),
        [top, rest @ ..] => (top.to_string(), rest),
        [] => (String::new(), &[][..]),
    };
    let (module, file) = match rest {
        ["src", next, more @ ..] | [next, more @ ..] => (*next, more.is_empty()),
        [] => ("", false),
    };
    let module = if file {
        module.strip_suffix(".rs").unwrap_or(module)
    } else {
        module
    };
    (dir, module.to_string())
}

/// The account a record's author counts as, unless it is a bot, AI or
/// deleted account.
fn human_author(r: &PrRecord) -> Option<String> {
    r.author
        .as_deref()
        .filter(|a| !r.author_bot && !is_bot(a, None))
        .map(canonical)
}

/// The credit of merged record `r` in `repo`. `credited` holds every
/// commit an earlier merged pull request of the repository was credited
/// with, and takes this one's; `emails` maps commit addresses to logins.
pub fn credit(
    repo: &str,
    r: &PrRecord,
    work: Option<&WorkState>,
    credited: &mut BTreeMap<String, u64>,
    emails: &BTreeMap<String, String>,
) -> PrCredit {
    let author = human_author(r);
    let c = &r.contribution;
    let mut lp = (0u64, 0u64);
    let mut gp = (0u64, 0u64);
    let mut dirs: Vec<DirLines> = Vec::new();
    for f in &c.files {
        if kind::product_lines(&f.path) {
            lp = (lp.0 + f.additions, lp.1 + f.deletions);
        } else {
            gp = (gp.0 + f.additions, gp.1 + f.deletions);
        }
        let (dir, module) = dir_of(&f.path);
        match dirs.iter_mut().find(|d| d.dir == dir && d.module == module) {
            Some(d) => {
                d.added += f.additions;
                d.removed += f.deletions;
            }
            None => dirs.push(DirLines {
                dir,
                module,
                added: f.additions,
                removed: f.deletions,
            }),
        }
    }
    // Until the rest of a long file list is read, the lines no file read so
    // far holds count as product lines.
    let files_cut = c.files_total > c.files.len() as u64 || c.files_cursor.is_some();
    if c.files.is_empty() || files_cut {
        lp = (
            c.additions.saturating_sub(gp.0),
            c.deletions.saturating_sub(gp.1),
        );
    }
    let all: Vec<_> = c.commits.iter().filter(|k| !k.is_merge).collect();
    let total: u64 = all.iter().map(|k| k.additions + k.deletions).sum();
    let mut stacked_in = None;
    let mut fresh = Vec::new();
    for k in &all {
        match credited.get(&k.oid) {
            Some(n) if *n != r.number => {
                stacked_in.get_or_insert(*n);
            }
            _ => {
                credited.insert(k.oid.clone(), r.number);
                fresh.push(*k);
            }
        }
    }
    // Only a complete commit list shows that every commit merged earlier.
    if !fresh.is_empty() || c.commits_total > c.commits.len() as u64 {
        stacked_in = None;
    }
    let truncated = files_cut
        || c.commits_total > c.commits.len() as u64
        || r.timeline_total > r.timeline.len() as u64;
    let to_author = total == 0 || c.commits_total > c.commits.len() as u64;
    let linked = |login: Option<&str>| login.filter(|l| !is_bot(l, None)).map(canonical);
    let mut shares: Vec<Share> = Vec::new();
    let mut add = |login: &str, weight: f64, commits: u64| {
        let s = match shares.iter_mut().find(|s| s.login == login) {
            Some(s) => s,
            None => {
                shares.push(Share {
                    login: login.to_string(),
                    review: author.as_deref() != Some(login),
                    ..Default::default()
                });
                shares.last_mut().unwrap()
            }
        };
        s.weight += weight;
        s.commits += commits;
    };
    if stacked_in.is_none() || all.is_empty() {
        if to_author {
            if let Some(a) = &author {
                add(a, 1.0, 0);
            }
        }
        for k in &fresh {
            let who = linked(k.author.as_deref());
            if !to_author {
                let w = (k.additions + k.deletions) as f64 / total as f64;
                if let Some(owner) = who.as_deref().or(author.as_deref()) {
                    add(owner, w, 0);
                }
            }
            if let Some(w) = &who {
                add(w, 0.0, 1);
            }
        }
    }
    for s in &mut shares {
        s.product = (lp.0 as f64 * s.weight, lp.1 as f64 * s.weight);
        s.generated = (gp.0 as f64 * s.weight, gp.1 as f64 * s.weight);
    }
    let mut co_authors = Vec::new();
    for k in &fresh {
        let who = linked(k.author.as_deref());
        for co in &k.co_authors {
            let login = co
                .login
                .clone()
                .or_else(|| emails.get(&co.email).cloned())
                .filter(|l| !is_bot(l, None))
                .map(|l| canonical(&l));
            if login.is_some() && login == who {
                continue;
            }
            co_authors.push(CoCredit {
                login,
                name: co.name.clone(),
            });
        }
    }
    let headlines: Vec<&str> = all.iter().map(|k| k.headline.as_str()).collect();
    let merge_headline = c.merge_commit.as_ref().map_or("", |m| m.headline.as_str());
    let mut kind = kind::classify(&kind::Input {
        title: &r.title,
        head_ref: &r.head_ref_name,
        merge_headline,
        commit_headlines: headlines,
        issue_titles: &r.closing_titles,
        files: &c.files,
    });
    if files_cut {
        kind.guess = true;
        kind.kind.rule += "; the file list is not read in full yet";
        kind.area.rule += "; the file list is not read in full yet";
    }
    let merger = c
        .merger
        .clone()
        .or_else(|| super::merger(c.merge_commit.as_ref(), r.merged_by.as_deref()))
        .filter(|m| !is_bot(m, None))
        .map(|m| canonical(&m));
    PrCredit {
        repo: repo.to_string(),
        number: r.number,
        author,
        merged_at: r.merged_at,
        merger,
        batch: work.and_then(|w| w.subtype.as_deref()) == Some("batch"),
        kind,
        changed_files: c.changed_files,
        shares,
        stacked_in,
        truncated,
        dirs,
        co_authors,
    }
}

/// One person's figures in one window, for every repository or one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Rank {
    /// Their pull requests merged in the window.
    pub merged: u64,
    /// Their pull requests closed in the window because another one
    /// carried the work.
    pub landed: u64,
    /// Product lines credited to them.
    pub added: u64,
    pub removed: u64,
    /// `Cargo.lock`, catalog and generated lines credited to them.
    pub gen_added: u64,
    pub gen_removed: u64,
    /// Changed files over their merged pull requests.
    pub files: u64,
    /// Commits they authored in merged pull requests.
    pub commits: u64,
    /// Commits that name them in a `Co-authored-by:` trailer.
    pub co_authored: u64,
    /// Commits on the default branch that belong to no pull request; `None`
    /// when the history the app keeps does not cover the window.
    pub direct: Option<u64>,
    /// UTC days with a commit, a merge or a review.
    pub active_days: u64,
    /// Pull requests they landed.
    pub merges: u64,
    /// Of those, the ones that landed in a batch.
    pub batch_merges: u64,
    pub reviews: u64,
    pub approved: u64,
    pub changes_requested: u64,
    pub commented: u64,
    pub reviewed_prs: u64,
    /// Commits they authored in someone else's merged pull request, and
    /// those commits' product lines.
    pub review_commits: u64,
    pub review_added: u64,
    pub review_removed: u64,
    /// Their merged pull requests by type, and how many of each are guesses.
    pub types: BTreeMap<String, u64>,
    pub guessed: BTreeMap<String, u64>,
    /// Their merged pull requests that credited no lines (every commit was
    /// counted in an earlier one), and those the fetch cut short.
    pub stacked: u64,
    pub truncated: u64,
    /// Some of the merges or landed counts come from a repository that
    /// lands work through a batch branch, where what GitHub records was
    /// never checked.
    pub unverified: bool,
}

impl Rank {
    fn is_empty(&self) -> bool {
        Rank {
            direct: None,
            ..self.clone()
        } == Rank::default()
            && !self.direct.is_some_and(|d| d > 0)
    }
}

/// One person's row: their figures by window id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RankRow {
    pub key: String,
    pub login: String,
    pub name: Option<String>,
    pub on_roster: bool,
    pub maintainer: bool,
    pub windows: BTreeMap<String, Rank>,
}

/// One row of a person's detail table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DetailRow {
    pub key: String,
    /// Their merged pull requests counted here.
    pub merged: u64,
    pub added: u64,
    pub removed: u64,
    /// Their share of their own total lines (merged pull requests when they
    /// have no lines), 0 to 1.
    pub share: f64,
    pub merges: u64,
    pub reviews: u64,
    pub review_commits: u64,
    /// How many of the merged pull requests have a guessed type.
    pub guessed: u64,
    /// The rules that decided an area or type, at most three.
    pub rules: Vec<String>,
    /// The modules of a directory.
    pub children: Vec<DetailRow>,
}

/// Where a person's work went in one window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersonDetail {
    pub key: String,
    pub window: String,
    pub added: u64,
    pub removed: u64,
    pub merged: u64,
    pub by_repo: Vec<DetailRow>,
    pub by_area: Vec<DetailRow>,
    pub by_type: Vec<DetailRow>,
    pub by_dir: Vec<DetailRow>,
}

/// One repository's people in one window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RepoPeople {
    pub repo: String,
    pub window: String,
    /// Each person's figures in the repository, in that window only.
    pub people: Vec<RankRow>,
    /// The repository lands work through a batch branch.
    pub batch: bool,
}

/// The commits on one repository's default branch that belong to no pull
/// request, as the app's own history keeps them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Direct {
    /// The history reaches back to here.
    pub covers_from: Option<Time>,
    /// The records hold every merged pull request, so a commit missing from
    /// them belongs to none.
    pub ready: bool,
    /// `(committed, canonical login)`.
    pub commits: Vec<(Time, String)>,
}

/// How far from a pull request's merge a rebased copy of one of its commits
/// is committed.
const REBASE_SECS: i64 = 300;

/// The commits in `history` (`(oid, committed, author)`, the author a
/// login, else an address, else `~` and a name) that are neither a merge
/// commit nor a commit of any pull request in `cache`. A commit committed
/// within [`REBASE_SECS`] of the merge of a pull request landed with one
/// parent (squashed or rebased), by one of that pull request's commit
/// authors, counts as a rebased copy of its commits. Commits no account
/// claims and bot commits are left out.
pub fn direct_commits<'a>(
    history: impl IntoIterator<Item = (&'a str, Time, &'a str)>,
    cache: &PrCache,
) -> Vec<(Time, String)> {
    let mut known: BTreeSet<&str> = BTreeSet::new();
    let mut rebased: Vec<(Time, BTreeSet<String>)> = Vec::new();
    for r in cache.records.values() {
        let c = &r.contribution;
        if let Some(m) = &c.merge_commit {
            known.insert(&m.oid);
            if let (PrState::Merged, true, Some(at)) = (r.state, m.parents < 2, r.merged_at) {
                let authors = c
                    .commits
                    .iter()
                    .filter_map(|k| k.author.as_deref())
                    .map(canonical)
                    .collect();
                rebased.push((at, authors));
            }
        }
        known.extend(c.commits.iter().map(|k| k.oid.as_str()));
    }
    history
        .into_iter()
        .filter(|(oid, _, who)| {
            !known.contains(oid) && super::valid_login(who) && !is_bot(who, None)
        })
        .filter(|(_, at, who)| {
            let key = canonical(who);
            !rebased.iter().any(|(m, authors)| {
                (*at - *m).num_seconds().abs() <= REBASE_SECS && authors.contains(&key)
            })
        })
        .map(|(_, at, who)| (at, canonical(who)))
        .collect()
}

/// The direct commits in `history` (see [`direct_commits`]), which reaches
/// back to `covers_from`. They count once the records hold every merged pull
/// request: the merged walk has finished, and no pull request merged within
/// the history has more commits than the fetch read, since its other
/// commits would look like direct ones.
pub fn direct<'a>(
    history: impl IntoIterator<Item = (&'a str, Time, &'a str)>,
    covers_from: Option<Time>,
    cache: &PrCache,
) -> Direct {
    let cut = cache.records.values().any(|r| {
        let c = &r.contribution;
        r.state == PrState::Merged
            && c.commits_total > c.commits.len() as u64
            && covers_from.is_none_or(|from| r.merged_at.is_none_or(|m| m >= from))
    });
    Direct {
        covers_from,
        ready: cache.merged_walk.done && !cut,
        commits: direct_commits(history, cache),
    }
}

/// One repository's input to [`Rankings::new`].
pub struct RankInput<'a> {
    pub repo: &'a str,
    pub cache: &'a PrCache,
    /// The work state of each record, by number.
    pub works: &'a BTreeMap<u64, WorkState>,
    pub release_at: Option<Time>,
    /// The repository lands work through a batch branch.
    pub batch: bool,
    pub direct: Option<Direct>,
}

/// Every window id: [`WINDOWS`], [`SINCE_RELEASE`] and [`ALL_TIME`].
pub fn window_ids() -> Vec<&'static str> {
    WINDOWS
        .iter()
        .map(|(id, _)| *id)
        .chain([SINCE_RELEASE, ALL_TIME])
        .collect()
}

#[derive(Default)]
struct Acc {
    rank: Rank,
    lines: [f64; 6],
}

impl Acc {
    fn finish(mut self) -> Rank {
        let r = |v: f64| v.round() as u64;
        let [a, d, ga, gd, ra, rd] = self.lines;
        self.rank.added = r(a);
        self.rank.removed = r(d);
        self.rank.gen_added = r(ga);
        self.rank.gen_removed = r(gd);
        self.rank.review_added = r(ra);
        self.rank.review_removed = r(rd);
        self.rank
    }
}

/// The credits and events behind the rankings, kept so the carets' tables
/// can be worked out when they open.
#[derive(Debug, Clone, Default)]
pub struct Rankings {
    pub credits: Vec<PrCredit>,
    ledger: Ledger,
    /// Each repository's window starts, `(window id, start)`.
    starts: BTreeMap<String, Vec<(&'static str, Time)>>,
    batch: BTreeSet<String>,
    direct: BTreeMap<String, Direct>,
    /// `(login, name, on roster, maintainer)` by key; bots are left out.
    people: BTreeMap<String, (String, Option<String>, bool, bool)>,
    /// The keys the directory flags as bot or AI accounts.
    bots: BTreeSet<String>,
}

/// UTC days.
type Days = BTreeSet<chrono::NaiveDate>;

impl Rankings {
    /// Credits every merged record of every repository in `inputs`, oldest
    /// merge first, and keeps the events, window starts and people.
    pub fn new(inputs: &[RankInput], dir: &Directory, now: Time) -> Rankings {
        let mut emails: BTreeMap<String, String> = BTreeMap::new();
        for input in inputs {
            for r in input.cache.records.values() {
                for k in &r.contribution.commits {
                    if let (Some(a), false) = (&k.author, k.author_email.is_empty()) {
                        if !is_bot(a, None) {
                            emails
                                .entry(k.author_email.clone())
                                .or_insert_with(|| canonical(a));
                        }
                    }
                }
            }
        }
        let mut out = Rankings::default();
        for input in inputs {
            let mut merged: Vec<&PrRecord> = input
                .cache
                .records
                .values()
                .filter(|r| r.state == PrState::Merged)
                .collect();
            merged.sort_by_key(|r| (r.merged_at, r.number));
            let mut credited = BTreeMap::new();
            for r in merged {
                let work = input.works.get(&r.number);
                out.credits
                    .push(credit(input.repo, r, work, &mut credited, &emails));
            }
            out.starts.insert(
                input.repo.to_string(),
                super::view::window_starts(now, input.release_at),
            );
            if input.batch {
                out.batch.insert(input.repo.to_ascii_lowercase());
            }
            if let Some(d) = &input.direct {
                out.direct.insert(input.repo.to_string(), d.clone());
            }
        }
        let held: Vec<(&str, &PrCache)> = inputs.iter().map(|i| (i.repo, i.cache)).collect();
        out.ledger = Ledger::build(&held);
        for (key, p) in &dir.people {
            if p.bot {
                out.bots.insert(key.clone());
            } else {
                let maintainer = !p.maintainer_of.is_empty();
                out.people.insert(
                    key.clone(),
                    (p.login.clone(), p.name.clone(), p.on_roster, maintainer),
                );
            }
        }
        out
    }

    /// Where window `id` starts for `repo`; `None` for a repository without
    /// that window (no build for the release window).
    fn start(&self, repo: &str, id: &str) -> Option<Time> {
        self.starts
            .get(repo)?
            .iter()
            .find(|(w, _)| *w == id)
            .map(|(_, s)| *s)
    }

    fn in_window(&self, repo: &str, id: &str, at: Option<Time>) -> bool {
        matches!((self.start(repo, id), at), (Some(s), Some(at)) if at >= s)
    }

    fn matches(repo: &str, filter: Option<&str>) -> bool {
        filter.is_none_or(|f| f.eq_ignore_ascii_case(repo))
    }

    /// Each person's direct commits in window `id` and the UTC days they
    /// fall on, or `None` when the history of some repository with that
    /// window does not cover it.
    fn direct_in(&self, id: &str, filter: Option<&str>) -> Option<BTreeMap<String, (u64, Days)>> {
        let mut out: BTreeMap<String, (u64, Days)> = BTreeMap::new();
        let mut any = false;
        for repo in self.starts.keys().filter(|r| Self::matches(r, filter)) {
            // A repository without a build has no release window.
            let Some(start) = self.start(repo, id) else {
                continue;
            };
            any = true;
            let d = self.direct.get(repo)?;
            if id == ALL_TIME || !d.ready || d.covers_from.is_none_or(|c| c > start) {
                return None;
            }
            for (at, who) in &d.commits {
                if *at >= start {
                    let e = out.entry(who.clone()).or_default();
                    e.0 += 1;
                    e.1.insert(at.date_naive());
                }
            }
        }
        any.then_some(out)
    }

    /// Each person's figures in window `id`, for every repository or the
    /// one `filter` names, and the trailer names no account is known for.
    fn ranks(
        &self,
        id: &str,
        filter: Option<&str>,
    ) -> (BTreeMap<String, Rank>, BTreeMap<String, u64>) {
        let mut acc: BTreeMap<String, Acc> = BTreeMap::new();
        let mut unlinked: BTreeMap<String, u64> = BTreeMap::new();
        for c in self
            .credits
            .iter()
            .filter(|c| Self::matches(&c.repo, filter))
        {
            if !self.in_window(&c.repo, id, c.merged_at) {
                continue;
            }
            if let Some(a) = &c.author {
                let r = &mut acc.entry(a.clone()).or_default().rank;
                *r.types.entry(c.kind.kind.value.clone()).or_default() += 1;
                if c.kind.guess {
                    *r.guessed.entry(c.kind.kind.value.clone()).or_default() += 1;
                }
                r.files += c.changed_files;
                r.stacked += u64::from(c.stacked_in.is_some());
                r.truncated += u64::from(c.truncated);
            }
            if let (true, Some(m)) = (c.batch, &c.merger) {
                acc.entry(m.clone()).or_default().rank.batch_merges += 1;
            }
            for s in &c.shares {
                let a = acc.entry(s.login.clone()).or_default();
                a.lines[0] += s.product.0;
                a.lines[1] += s.product.1;
                a.lines[2] += s.generated.0;
                a.lines[3] += s.generated.1;
                a.rank.commits += s.commits;
                if s.review {
                    a.lines[4] += s.product.0;
                    a.lines[5] += s.product.1;
                    a.rank.review_commits += s.commits;
                }
            }
            for co in &c.co_authors {
                match &co.login {
                    Some(l) => acc.entry(l.clone()).or_default().rank.co_authored += 1,
                    None => *unlinked.entry(co.name.clone()).or_default() += 1,
                }
            }
        }
        let tally = self.ledger.tally(|repo| {
            Self::matches(repo, filter)
                .then(|| self.start(repo, id))
                .flatten()
        });
        for (login, t) in tally {
            let r = &mut acc.entry(login).or_default().rank;
            r.merged = t.merged;
            r.landed = t.landed_elsewhere;
            r.merges = t.merges;
            r.reviews = t.reviews;
            r.approved = t.approved;
            r.changes_requested = t.changes_requested;
            r.commented = t.commented;
            r.reviewed_prs = t.reviewed_prs;
        }
        // Active days: a commit, a merge or a review, as the ledger counts
        // them, and a direct commit to the default branch.
        let mut days: BTreeMap<String, Days> = BTreeMap::new();
        for e in &self.ledger.events {
            if !Self::matches(&e.repo, filter) || !self.in_window(&e.repo, id, Some(e.at)) {
                continue;
            }
            if matches!(e.kind, Kind::Committed | Kind::MergedBy | Kind::Reviewed) {
                days.entry(e.login.clone())
                    .or_default()
                    .insert(e.at.date_naive());
            }
            let counted = matches!(
                e.kind,
                Kind::AuthoredMerged | Kind::LandedElsewhere | Kind::MergedBy
            );
            if counted && self.batch.contains(&e.repo.to_ascii_lowercase()) {
                acc.entry(e.login.clone()).or_default().rank.unverified = true;
            }
        }
        let direct = self.direct_in(id, filter);
        for (login, (_, d)) in direct.iter().flatten() {
            days.entry(login.clone()).or_default().extend(d);
        }
        for (login, d) in &days {
            acc.entry(login.clone()).or_default().rank.active_days = d.len() as u64;
        }
        let ranks = acc
            .into_iter()
            .filter(|(login, _)| !is_bot(login, None) && !self.bots.contains(login))
            .map(|(login, a)| {
                let mut r = a.finish();
                r.direct = direct
                    .as_ref()
                    .map(|d| d.get(&login).map_or(0, |(n, _)| *n));
                (login, r)
            })
            .collect();
        (ranks, unlinked)
    }

    fn row(&self, key: &str) -> RankRow {
        let (login, name, on_roster, maintainer) = self
            .people
            .get(key)
            .cloned()
            .unwrap_or_else(|| (key.to_string(), None, false, false));
        RankRow {
            key: key.to_string(),
            login,
            name,
            on_roster,
            maintainer,
            windows: BTreeMap::new(),
        }
    }

    /// One row per person with figures in any window, for every repository
    /// or the one `filter` names, and per window the co-author names no
    /// account is known for.
    pub fn rows(
        &self,
        filter: Option<&str>,
    ) -> (Vec<RankRow>, BTreeMap<String, BTreeMap<String, u64>>) {
        let mut rows: BTreeMap<String, RankRow> = BTreeMap::new();
        let mut unlinked = BTreeMap::new();
        for id in window_ids() {
            let (ranks, names) = self.ranks(id, filter);
            if !names.is_empty() {
                unlinked.insert(id.to_string(), names);
            }
            for (login, r) in ranks {
                if r.is_empty() {
                    continue;
                }
                rows.entry(login.clone())
                    .or_insert_with(|| self.row(&login))
                    .windows
                    .insert(id.to_string(), r);
            }
        }
        (rows.into_values().collect(), unlinked)
    }

    /// The people of `repo` in window `id`.
    pub fn repo(&self, repo: &str, id: &str) -> RepoPeople {
        let (ranks, _) = self.ranks(id, Some(repo));
        let people = ranks
            .into_iter()
            .filter(|(_, r)| !r.is_empty())
            .map(|(login, r)| {
                let mut row = self.row(&login);
                row.windows.insert(id.to_string(), r);
                row
            })
            .collect();
        RepoPeople {
            repo: repo.to_string(),
            window: id.to_string(),
            people,
            batch: self.batch.contains(&repo.to_ascii_lowercase()),
        }
    }

    /// Where `key`'s work went in window `id`: their merged pull requests,
    /// the product lines credited to them, and in the repository table their
    /// merges, reviews and review commits. With `only`, the work in that
    /// repository alone (any letter case).
    pub fn person(&self, key: &str, id: &str, only: Option<&str>) -> PersonDetail {
        let key = canonical(key);
        let kept = |repo: &str| only.is_none_or(|o| o.eq_ignore_ascii_case(repo));
        let mut repo: BTreeMap<String, DetailRow> = BTreeMap::new();
        let mut area: BTreeMap<String, DetailRow> = BTreeMap::new();
        let mut ty: BTreeMap<String, DetailRow> = BTreeMap::new();
        let mut dirs: BTreeMap<String, DirAcc> = BTreeMap::new();
        let mut child_lines: BTreeMap<(String, String), [f64; 2]> = BTreeMap::new();
        let mut lines: BTreeMap<(u8, String), [f64; 2]> = BTreeMap::new();
        for c in &self.credits {
            if !kept(&c.repo) || !self.in_window(&c.repo, id, c.merged_at) {
                continue;
            }
            if c.merger.as_deref() == Some(key.as_str()) {
                repo.entry(c.repo.clone()).or_default().merges += 1;
            }
            let share = c.shares.iter().find(|s| s.login == key);
            let authored = c.author.as_deref() == Some(key.as_str());
            if share.is_none() && !authored {
                continue;
            }
            let (a, d) = share.map_or((0.0, 0.0), |s| s.product);
            let merged = u64::from(authored);
            let mut put =
                |table: u8, map: &mut BTreeMap<String, DetailRow>, k: &str, rule: Option<&str>| {
                    let row = map.entry(k.to_string()).or_default();
                    row.merged += merged;
                    row.guessed += u64::from(authored && table == 2 && c.kind.guess);
                    if let Some(rule) = rule {
                        if row.rules.len() < 3 && !row.rules.iter().any(|x| x == rule) {
                            row.rules.push(rule.to_string());
                        }
                    }
                    let l = lines.entry((table, k.to_string())).or_default();
                    l[0] += a;
                    l[1] += d;
                };
            put(0, &mut repo, &c.repo, None);
            put(1, &mut area, &c.kind.area.value, Some(&c.kind.area.rule));
            put(2, &mut ty, &c.kind.kind.value, Some(&c.kind.kind.rule));
            if let Some(s) = share {
                for dl in &c.dirs {
                    let e = dirs.entry(dl.dir.clone()).or_default();
                    e.2[0] += dl.added as f64 * s.weight;
                    e.2[1] += dl.removed as f64 * s.weight;
                    let child = child_lines
                        .entry((dl.dir.clone(), dl.module.clone()))
                        .or_default();
                    child[0] += dl.added as f64 * s.weight;
                    child[1] += dl.removed as f64 * s.weight;
                    e.1.entry(dl.module.clone()).or_default();
                }
            }
            if authored {
                let mut seen = BTreeSet::new();
                for dl in &c.dirs {
                    let e = dirs.entry(dl.dir.clone()).or_default();
                    if seen.insert(dl.dir.clone()) {
                        e.0.merged += 1;
                    }
                    let child = e.1.entry(dl.module.clone()).or_default();
                    if seen.insert(format!("{}\n{}", dl.dir, dl.module)) {
                        child.merged += 1;
                    }
                }
            }
            if let (Some(s), true) = (share, !authored) {
                repo.entry(c.repo.clone()).or_default().review_commits += s.commits;
            }
        }
        let tally = |repo_name: &str| {
            self.ledger
                .tally(|r| {
                    r.eq_ignore_ascii_case(repo_name)
                        .then(|| self.start(r, id))
                        .flatten()
                })
                .remove(&key)
        };
        let names: Vec<String> = repo.keys().cloned().collect();
        for name in names {
            if let Some(t) = tally(&name) {
                repo.entry(name).or_default().reviews = t.reviews;
            }
        }
        for r in self.starts.keys() {
            if repo.contains_key(r) || !kept(r) {
                continue;
            }
            if let Some(t) = tally(r).filter(|t| t.reviews > 0) {
                repo.entry(r.clone()).or_default().reviews = t.reviews;
            }
        }
        let round = |v: f64| v.round() as u64;
        let mut total = [0.0f64; 2];
        for ((table, _), l) in &lines {
            if *table == 0 {
                total[0] += l[0];
                total[1] += l[1];
            }
        }
        let total_lines = total[0] + total[1];
        let merged_total: u64 = repo.values().map(|r| r.merged).sum();
        let share = |l: [f64; 2], merged: u64| {
            if total_lines > 0.0 {
                (l[0] + l[1]) / total_lines
            } else if merged_total > 0 {
                merged as f64 / merged_total as f64
            } else {
                0.0
            }
        };
        let finish = |table: u8, map: BTreeMap<String, DetailRow>| -> Vec<DetailRow> {
            let mut out: Vec<DetailRow> = map
                .into_iter()
                .map(|(k, mut row)| {
                    let l = lines.get(&(table, k.clone())).copied().unwrap_or_default();
                    row.added = round(l[0]);
                    row.removed = round(l[1]);
                    row.share = share(l, row.merged);
                    row.key = k;
                    row
                })
                .collect();
            sort_rows(&mut out);
            out
        };
        let dir_total: f64 = dirs.values().map(|(_, _, l)| l[0] + l[1]).sum();
        let mut by_dir: Vec<DetailRow> = dirs
            .into_iter()
            .map(|(k, (mut row, children, l))| {
                row.added = round(l[0]);
                row.removed = round(l[1]);
                row.share = if dir_total > 0.0 {
                    (l[0] + l[1]) / dir_total
                } else {
                    0.0
                };
                let mut kids: Vec<DetailRow> = children
                    .into_iter()
                    .filter(|(m, _)| !m.is_empty())
                    .map(|(m, mut child)| {
                        let cl = child_lines
                            .get(&(k.clone(), m.clone()))
                            .copied()
                            .unwrap_or_default();
                        child.added = round(cl[0]);
                        child.removed = round(cl[1]);
                        let row_lines = l[0] + l[1];
                        child.share = if row_lines > 0.0 {
                            (cl[0] + cl[1]) / row_lines
                        } else {
                            0.0
                        };
                        child.key = m;
                        child
                    })
                    .collect();
                sort_rows(&mut kids);
                row.children = kids;
                row.key = k;
                row
            })
            .collect();
        sort_rows(&mut by_dir);
        PersonDetail {
            key: key.clone(),
            window: id.to_string(),
            added: round(total[0]),
            removed: round(total[1]),
            merged: merged_total,
            by_repo: finish(0, repo),
            by_area: finish(1, area),
            by_type: finish(2, ty),
            by_dir,
        }
    }
}

/// One directory's row, its modules' rows and its lines added and removed.
type DirAcc = (DetailRow, BTreeMap<String, DetailRow>, [f64; 2]);

/// Most lines first, then most merged, then by key.
fn sort_rows(rows: &mut [DetailRow]) {
    rows.sort_by(|a, b| {
        (b.added + b.removed, b.merged, b.merges + b.reviews)
            .cmp(&(a.added + a.removed, a.merged, a.merges + a.reviews))
            .then_with(|| a.key.cmp(&b.key))
    });
}

#[cfg(test)]
mod tests {
    use super::super::record::{
        CoAuthor, Contribution, FileLines, MergeCommit, PrCommit, TimelineItem,
    };
    use super::super::{directory, RepoPeople as DirRepo};
    use super::*;

    fn t(s: &str) -> Time {
        s.parse().unwrap()
    }

    fn commit(oid: &str, author: Option<&str>, adds: u64, dels: u64) -> PrCommit {
        PrCommit {
            oid: oid.into(),
            author: author.map(str::to_string),
            author_email: author.map_or("x@example.com".into(), |a| format!("{a}@example.com")),
            authored_at: Some(t("2026-10-10T00:00:00Z")),
            additions: adds,
            deletions: dels,
            headline: format!("Change {oid}"),
            ..Default::default()
        }
    }

    fn file(path: &str, adds: u64, dels: u64) -> FileLines {
        FileLines {
            path: path.into(),
            additions: adds,
            deletions: dels,
        }
    }

    fn merged(
        number: u64,
        author: &str,
        at: &str,
        commits: Vec<PrCommit>,
        files: Vec<FileLines>,
    ) -> PrRecord {
        let adds = files.iter().map(|f| f.additions).sum();
        PrRecord {
            number,
            title: format!("Engine: change {number}"),
            state: PrState::Merged,
            author: Some(author.into()),
            merged_at: Some(t(at)),
            merged_by: Some("lander-one".into()),
            contribution: Contribution {
                additions: adds,
                changed_files: files.len() as u64,
                commits_total: commits.len() as u64,
                commits,
                files_total: files.len() as u64,
                files,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn credit_of(r: &PrRecord, credited: &mut BTreeMap<String, u64>) -> PrCredit {
        credit("o/app", r, None, credited, &BTreeMap::new())
    }

    fn share<'a>(c: &'a PrCredit, login: &str) -> &'a Share {
        c.shares.iter().find(|s| s.login == login).unwrap()
    }

    fn rounded(v: (f64, f64)) -> (u64, u64) {
        (v.0.round() as u64, v.1.round() as u64)
    }

    #[test]
    fn lines_split_by_commit_and_a_review_commit_is_counted_apart() {
        // The author wrote 1809 of the commits' lines, a maintainer 207.
        let r = merged(
            1,
            "Ann-Example",
            "2026-10-10T00:00:00Z",
            vec![
                commit("a1", Some("ann-example"), 1805, 4),
                commit("b1", Some("maint-one"), 207, 0),
                PrCommit {
                    is_merge: true,
                    ..commit("m1", Some("ann-example"), 5000, 10)
                },
            ],
            vec![file("crates/engine/src/a.rs", 2012, 4)],
        );
        let c = credit_of(&r, &mut BTreeMap::new());
        let ann = share(&c, "ann-example");
        assert_eq!(rounded(ann.product), (1805, 4));
        assert!(!ann.review && ann.commits == 1);
        let m = share(&c, "maint-one");
        assert_eq!(rounded(m.product), (207, 0));
        assert!(m.review, "a commit by someone other than the author");
        assert_eq!(c.shares.len(), 2, "the merge commit is dropped");
    }

    #[test]
    fn catalogs_lockfiles_and_generated_files_count_apart() {
        let r = merged(
            2,
            "ann-example",
            "2026-10-10T00:00:00Z",
            vec![commit("a1", Some("ann-example"), 10, 0)],
            vec![
                file("crates/engine/src/a.rs", 10, 2),
                file("Cargo.lock", 40, 40),
                file("crates/ui-egui/src/i18n/ja.tsv", 30, 0),
                file("crates/ui-egui/src/i18n/fr.rs", 20, 0),
                file("contributors/contributors.json", 5, 1),
            ],
        );
        let c = credit_of(&r, &mut BTreeMap::new());
        let s = share(&c, "ann-example");
        assert_eq!(rounded(s.product), (10, 2));
        assert_eq!(rounded(s.generated), (95, 41));
        let dirs: Vec<&str> = c.dirs.iter().map(|d| d.dir.as_str()).collect();
        assert_eq!(dirs, ["crates/engine", GENERATED, TRANSLATIONS]);
    }

    #[test]
    fn a_file_list_not_read_in_full_counts_the_rest_as_product_and_is_flagged() {
        let mut r = merged(
            10,
            "ann-example",
            "2026-10-10T00:00:00Z",
            vec![commit("a1", Some("ann-example"), 10, 0)],
            vec![
                file("crates/engine/src/a.rs", 10, 2),
                file("Cargo.lock", 40, 40),
            ],
        );
        r.contribution.additions = 500;
        r.contribution.deletions = 50;
        r.contribution.files_total = 140;
        r.contribution.files_cursor = Some("f2".into());
        let c = credit_of(&r, &mut BTreeMap::new());
        let s = share(&c, "ann-example");
        assert_eq!(
            rounded(s.product),
            (460, 10),
            "the pull request's lines less the lockfile's"
        );
        assert_eq!(rounded(s.generated), (40, 40));
        assert!(c.truncated && c.kind.guess);
        let rule = &c.kind.kind.rule;
        assert!(
            rule.ends_with("the file list is not read in full yet"),
            "{rule}"
        );
    }

    #[test]
    fn stacked_pull_requests_count_a_commit_once() {
        let mut credited = BTreeMap::new();
        let first = merged(
            3,
            "ann-example",
            "2026-10-10T00:00:00Z",
            vec![commit("s1", Some("ann-example"), 10, 0)],
            vec![file("crates/engine/src/a.rs", 10, 0)],
        );
        let second = merged(
            4,
            "ann-example",
            "2026-10-11T00:00:00Z",
            vec![
                commit("s1", Some("ann-example"), 10, 0),
                commit("s2", Some("ann-example"), 30, 0),
            ],
            vec![file("crates/engine/src/a.rs", 40, 0)],
        );
        let c1 = credit_of(&first, &mut credited);
        let c2 = credit_of(&second, &mut credited);
        assert_eq!(share(&c1, "ann-example").commits, 1);
        let s = share(&c2, "ann-example");
        assert_eq!(
            (s.commits, rounded(s.product)),
            (1, (30, 0)),
            "only s2 is new"
        );
        // A pull request whose every commit merged earlier credits no lines.
        let third = merged(
            5,
            "ann-example",
            "2026-10-12T00:00:00Z",
            vec![commit("s2", Some("ann-example"), 30, 0)],
            vec![file("crates/engine/src/a.rs", 30, 0)],
        );
        let c3 = credit_of(&third, &mut credited);
        assert_eq!(c3.stacked_in, Some(4));
        assert!(c3.shares.is_empty());
        // When the fetch read only some commits, the rest may be new: the
        // author gets the lines.
        let mut long = merged(
            6,
            "ann-example",
            "2026-10-13T00:00:00Z",
            vec![commit("s1", Some("ann-example"), 10, 0)],
            vec![file("crates/engine/src/a.rs", 70, 0)],
        );
        long.contribution.commits_total = 150;
        let c4 = credit_of(&long, &mut credited);
        assert_eq!(c4.stacked_in, None);
        assert_eq!(rounded(share(&c4, "ann-example").product), (70, 0));
    }

    #[test]
    fn a_cut_commit_list_unlinked_commits_and_empty_commits_credit_the_author() {
        let mut r = merged(
            6,
            "ann-example",
            "2026-10-10T00:00:00Z",
            vec![commit("a1", Some("maint-one"), 10, 0)],
            vec![file("crates/engine/src/a.rs", 50, 0)],
        );
        r.contribution.commits_total = 140;
        let c = credit_of(&r, &mut BTreeMap::new());
        assert_eq!(rounded(share(&c, "ann-example").product), (50, 0));
        assert_eq!(share(&c, "maint-one").weight, 0.0, "a commit, but no lines");
        assert!(c.truncated);
        let r = merged(
            7,
            "ann-example",
            "2026-10-10T00:00:00Z",
            vec![
                commit("u1", None, 20, 0),
                commit("b1", Some("maint-one"), 20, 0),
            ],
            vec![file("crates/engine/src/a.rs", 40, 0)],
        );
        let c = credit_of(&r, &mut BTreeMap::new());
        assert_eq!(rounded(share(&c, "ann-example").product), (20, 0));
        assert_eq!(
            share(&c, "ann-example").commits,
            0,
            "the unlinked commit is not theirs"
        );
        let r = merged(
            8,
            "ann-example",
            "2026-10-10T00:00:00Z",
            vec![commit("e1", Some("maint-one"), 0, 0)],
            vec![file("crates/engine/src/a.rs", 3, 0)],
        );
        let c = credit_of(&r, &mut BTreeMap::new());
        assert_eq!(rounded(share(&c, "ann-example").product), (3, 0));
    }

    #[test]
    fn co_authors_map_addresses_to_logins_and_keep_unlinked_names() {
        let mut k = commit("a1", Some("ann-example"), 5, 0);
        k.co_authors = vec![
            CoAuthor {
                name: "Bo Sample".into(),
                email: "bo@example.com".into(),
                login: None,
            },
            CoAuthor {
                name: "Cy Example".into(),
                email: "cy@users.noreply.github.com".into(),
                login: Some("cy-example".into()),
            },
            CoAuthor {
                name: "Nobody Known".into(),
                email: "nobody@example.org".into(),
                login: None,
            },
        ];
        let r = merged(9, "ann-example", "2026-10-10T00:00:00Z", vec![k], vec![]);
        let emails: BTreeMap<String, String> =
            [("bo@example.com".to_string(), "bo-sample".to_string())].into();
        let c = credit("o/app", &r, None, &mut BTreeMap::new(), &emails);
        assert_eq!(
            c.co_authors,
            [
                CoCredit {
                    login: Some("bo-sample".into()),
                    name: "Bo Sample".into()
                },
                CoCredit {
                    login: Some("cy-example".into()),
                    name: "Cy Example".into()
                },
                CoCredit {
                    login: None,
                    name: "Nobody Known".into()
                },
            ]
        );
    }

    #[test]
    fn directories_take_two_segments_under_crates_and_modules_under_src() {
        assert_eq!(
            dir_of("crates/engine/src/cmd/fileio/load.rs"),
            ("crates/engine".into(), "cmd".into())
        );
        assert_eq!(
            dir_of("crates/engine/src/lib.rs"),
            ("crates/engine".into(), "lib".into())
        );
        assert_eq!(
            dir_of("crates/engine/tests/import.rs"),
            ("crates/engine".into(), "tests".into())
        );
        assert_eq!(dir_of("docs/mcp.md"), ("docs".into(), "mcp.md".into()));
        assert_eq!(
            dir_of(".github/workflows/ci.yml"),
            (".github".into(), "workflows".into())
        );
        assert_eq!(dir_of("README.md"), ("README.md".into(), String::new()));
        assert_eq!(dir_of("crates/ui-egui/src/i18n/fr.rs").0, TRANSLATIONS);
    }

    fn sample() -> (PrCache, BTreeMap<u64, WorkState>) {
        let mut cache = PrCache::default();
        let mut r1 = merged(
            1,
            "ann-example",
            "2026-10-16T12:00:00Z",
            vec![
                commit("a1", Some("ann-example"), 90, 10),
                commit("b1", Some("maint-one"), 100, 0),
            ],
            vec![
                file("crates/engine/src/cmd/a.rs", 150, 10),
                file("docs/mcp.md", 40, 0),
            ],
        );
        r1.title = "Fix #3: the engine keeps layers".into();
        r1.contribution.merge_commit = Some(MergeCommit {
            oid: "mc1".into(),
            committer_name: "Lands Locally".into(),
            committer_login: Some("maint-one".into()),
            parents: 2,
            ..Default::default()
        });
        r1.timeline = vec![TimelineItem {
            kind: "PullRequestReview".into(),
            actor: Some("maint-one".into()),
            at: Some(t("2026-10-16T11:00:00Z")),
            state: Some("APPROVED".into()),
            ..Default::default()
        }];
        r1.timeline_total = 1;
        cache.records.insert(1, r1);
        let mut r2 = merged(
            2,
            "ann-example",
            "2026-09-01T00:00:00Z",
            vec![commit("a2", Some("ann-example"), 20, 0)],
            vec![file("crates/ui-egui/src/panel.rs", 20, 0)],
        );
        r2.title = "Panels: a new panel".into();
        cache.records.insert(2, r2);
        let mut works = BTreeMap::new();
        works.insert(
            1,
            WorkState {
                state: "merged".into(),
                subtype: Some("batch".into()),
                ..Default::default()
            },
        );
        (cache, works)
    }

    fn rankings(
        cache: &PrCache,
        works: &BTreeMap<u64, WorkState>,
        batch: bool,
        direct: Option<Direct>,
    ) -> Rankings {
        let maint: BTreeSet<String> = ["maint-one".to_string()].into();
        let dir = directory(
            &[DirRepo {
                repo: "o/app",
                cache,
                owners: &[],
                maintainers: &maint,
            }],
            &[],
        );
        Rankings::new(
            &[RankInput {
                repo: "o/app",
                cache,
                works,
                release_at: Some(t("2026-10-01T00:00:00Z")),
                batch,
                direct,
            }],
            &dir,
            t("2026-10-17T00:00:00Z"),
        )
    }

    #[test]
    fn rows_rank_every_window_from_credits_and_events() {
        let (cache, works) = sample();
        let rk = rankings(&cache, &works, false, None);
        let (rows, _) = rk.rows(None);
        let ann = rows.iter().find(|r| r.key == "ann-example").unwrap();
        let day = &ann.windows["1d"];
        assert_eq!(
            (day.merged, day.added, day.removed, day.files),
            (1, 95, 5, 2)
        );
        assert_eq!(day.types["fix"], 1);
        assert_eq!(day.direct, None, "no history kept");
        let all = &ann.windows["all"];
        assert_eq!((all.merged, all.commits), (2, 2));
        assert_eq!(all.types["feature"], 1);
        assert_eq!(
            ann.windows["release"].merged, 1,
            "since the build on the 1st"
        );
        let m = rows.iter().find(|r| r.key == "maint-one").unwrap();
        assert!(m.maintainer);
        let md = &m.windows["1d"];
        assert_eq!(
            (md.merges, md.batch_merges, md.reviews, md.approved),
            (1, 1, 1, 1)
        );
        assert_eq!((md.review_commits, md.review_added, md.added), (1, 95, 95));
        assert!(!md.unverified);
        let lander = rows.iter().find(|r| r.key == "lander-one").unwrap();
        assert!(
            !lander.windows.contains_key("1d"),
            "the local committer landed #1"
        );
        assert_eq!(lander.windows["all"].merges, 1, "#2, merged on the web");
        // In a batch-branch repository the merges are unverified.
        let rk = rankings(&cache, &works, true, None);
        let (rows, _) = rk.rows(None);
        assert!(rows.iter().find(|r| r.key == "maint-one").unwrap().windows["1d"].unverified);
    }

    #[test]
    fn direct_commits_show_only_where_the_history_covers_the_window() {
        let (cache, works) = sample();
        let history = [
            ("mc1", t("2026-10-16T12:00:00Z"), "maint-one"),
            ("a1", t("2026-10-16T10:00:00Z"), "ann-example"),
            ("d1", t("2026-10-16T13:00:00Z"), "Maint-One"),
            ("d2", t("2026-10-16T14:00:00Z"), "someone@example.com"),
            ("d3", t("2026-10-16T15:00:00Z"), "github-actions"),
            // Authors kept by name only.
            ("n1", t("2026-10-16T16:00:00Z"), "~Cy"),
            ("n2", t("2026-10-16T16:00:00Z"), "Some Name"),
            // A rebased copy of one of #1's commits, landed with it.
            ("x1", t("2026-10-16T12:00:10Z"), "ann-example"),
        ];
        // #1 landed by a rebase: its merge commit has one parent.
        let mut rebased = cache.clone();
        if let Some(m) = rebased
            .records
            .get_mut(&1)
            .and_then(|r| r.contribution.merge_commit.as_mut())
        {
            m.parents = 1;
        }
        let commits = direct_commits(history, &rebased);
        assert_eq!(
            commits,
            [(t("2026-10-16T13:00:00Z"), "maint-one".to_string())]
        );
        let mut direct = Direct {
            covers_from: Some(t("2026-09-30T00:00:00Z")),
            ready: true,
            commits,
        };
        // A direct commit on another day, and someone who only pushes to
        // main and appears nowhere else.
        direct
            .commits
            .push((t("2026-10-14T09:00:00Z"), "maint-one".into()));
        direct
            .commits
            .push((t("2026-10-16T09:00:00Z"), "pusher-x".into()));
        let rk = rankings(&cache, &works, false, Some(direct.clone()));
        let (rows, _) = rk.rows(None);
        let m = rows.iter().find(|r| r.key == "maint-one").unwrap();
        assert_eq!(m.windows["1d"].direct, Some(1));
        assert_eq!(m.windows["release"].direct, Some(2));
        assert_eq!(
            m.windows["all"].direct, None,
            "the history does not reach back"
        );
        assert_eq!(
            m.windows["7d"].active_days, 3,
            "the 10th (a commit), the 14th (direct) and the 16th (merge and review)"
        );
        let pusher = rows.iter().find(|r| r.key == "pusher-x").unwrap();
        let p = &pusher.windows["1d"];
        assert_eq!((p.direct, p.active_days), (Some(1), 1));
        let ann = rows.iter().find(|r| r.key == "ann-example").unwrap();
        assert_eq!(ann.windows["1d"].direct, Some(0));
        // A pull request merged within the history with more commits than
        // the fetch read keeps the count off; one merged before it does not.
        let mut cut = cache.clone();
        cut.merged_walk.done = true;
        let r1 = cut.records.get_mut(&1).unwrap();
        r1.contribution.commits_total = 150;
        assert!(!super::direct([], direct.covers_from, &cut).ready);
        assert!(super::direct([], Some(t("2026-10-17T00:00:00Z")), &cut).ready);
        let mut whole = cache.clone();
        assert!(
            !super::direct([], direct.covers_from, &whole).ready,
            "the walk is not done"
        );
        whole.merged_walk.done = true;
        assert!(super::direct([], direct.covers_from, &whole).ready);
        // A repository without a build has no release window, so it does not
        // keep the other's direct commits out of it.
        let lib = PrCache::default();
        let no_works = BTreeMap::new();
        let dir = directory(&[], &[]);
        let both = Rankings::new(
            &[
                RankInput {
                    repo: "o/app",
                    cache: &cache,
                    works: &works,
                    release_at: Some(t("2026-10-01T00:00:00Z")),
                    batch: false,
                    direct: Some(direct.clone()),
                },
                RankInput {
                    repo: "o/lib",
                    cache: &lib,
                    works: &no_works,
                    release_at: None,
                    batch: false,
                    direct: Some(Direct {
                        commits: vec![],
                        ..direct.clone()
                    }),
                },
            ],
            &dir,
            t("2026-10-17T00:00:00Z"),
        );
        let (rows, _) = both.rows(None);
        let m = rows.iter().find(|r| r.key == "maint-one").unwrap();
        assert_eq!(m.windows["release"].direct, Some(2));
        // Until the merged records are complete, a commit missing from them
        // may still belong to a pull request.
        let rk = rankings(
            &cache,
            &works,
            false,
            Some(Direct {
                ready: false,
                ..direct
            }),
        );
        let (rows, _) = rk.rows(None);
        assert_eq!(
            rows.iter().find(|r| r.key == "maint-one").unwrap().windows["1d"].direct,
            None
        );
    }

    #[test]
    fn the_person_detail_splits_their_work_by_repo_area_type_and_directory() {
        let (cache, works) = sample();
        let rk = rankings(&cache, &works, false, None);
        let d = rk.person("Ann-Example", "all", None);
        assert_eq!((d.merged, d.added, d.removed), (2, 115, 5));
        assert_eq!(
            rk.person("Ann-Example", "all", Some("O/APP")),
            d,
            "one repository, in any letter case"
        );
        let other = rk.person("Ann-Example", "all", Some("o/other"));
        assert_eq!(
            (other.merged, other.added),
            (0, 0),
            "another repository's work only"
        );
        assert!(other.by_repo.is_empty() && other.by_area.is_empty() && other.by_dir.is_empty());
        assert_eq!(d.by_repo[0].key, "o/app");
        let types: Vec<(&str, u64)> = d
            .by_type
            .iter()
            .map(|r| (r.key.as_str(), r.merged))
            .collect();
        assert_eq!(types, [("fix", 1), ("feature", 1)]);
        assert!(d.by_type[0].rules[0].starts_with("R2"));
        let areas: Vec<&str> = d.by_area.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(areas, ["engine", "panels"]);
        let engine = d.by_dir.iter().find(|r| r.key == "crates/engine").unwrap();
        assert_eq!((engine.merged, engine.added), (1, 75));
        assert_eq!(engine.children[0].key, "cmd");
        let shares: f64 = d.by_repo.iter().map(|r| r.share).sum();
        assert!((shares - 1.0).abs() < 1e-9);
        // The maintainer's repo row holds their merge, review and review commit.
        let m = rk.person("maint-one", "1d", None);
        let row = &m.by_repo[0];
        assert_eq!(
            (row.merges, row.reviews, row.review_commits, row.merged),
            (1, 1, 1, 0)
        );
        // One repository's people for the caret on its row.
        let rp = rk.repo("O/App", "1d");
        assert_eq!(rp.people.len(), 2);
        assert_eq!(rp.people[0].windows["1d"].merged, 1);
    }
}
