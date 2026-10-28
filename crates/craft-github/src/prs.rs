//! Keeps each repository's pull request records ([`PrCache`]) up to date,
//! once per call of [`GitHub::update_prs`], in four kinds of request:
//!
//! - The changed list: pull requests in every state, newest update first,
//!   read down to a little before the newest update the previous call saw.
//!   It is skipped when the repository's last fetch saw no pull request
//!   updated since ([`Seen`]). A call reads at most [`MAX_CHANGED_PAGES`]
//!   pages; the rest is read by the next call ([`Gap`]).
//! - The light sweep: a few fields of every open pull request, the author
//!   and the title included, for a share of the repositories each call, so
//!   that each one is swept every [`SWEEP_MINUTES`]. It finds open pull
//!   requests the cache lacks, newer updates and pull requests that left the
//!   open list. It also reads the token's login ([`PrReport::viewer`]).
//! - The backfill by node id: the full fields of the pull requests the sweep
//!   found, the token user's first, then the most recently updated.
//! - The first-load walks over merged pull requests and over pull requests
//!   closed without merging, newest update first, each continued from where
//!   the previous call stopped.
//!
//! Commit lists and the checks on a head are read in pages of 100: the
//! records the backfill leaves fetched get the rest of theirs after it, and
//! the sweep reads all of the checks on the default branch's head.
//!
//! The requests go one at a time, through the client given, which in the
//! app sends them as background requests (see [`crate::gate`]). Once GitHub
//! reports fewer points left than the floor, only the changed list runs.
//!
//! The backfill, the commit and check lists, the walks and the file lists
//! send at most [`PrInput::max_requests`] requests in one call
//! ([`RUN_REQUESTS`] in the app), so a first load is spread over many calls; the changed lists and
//! the sweep keep their own page limits ([`MAX_CHANGED_PAGES`],
//! [`MAX_SWEEP_PAGES`]), so they finish whatever the cap leaves. The changed
//! lists of the repositories whose records were checked longest ago go
//! first. When GitHub could not finish a query (a server error, or a GraphQL error
//! that names a timeout), a page of the changed list or of a walk is asked
//! for again with fewer pull requests, and a backfill batch is split in two;
//! for the walks and the backfill, only while the call has requests left.
//! The changed list starts each call at the full page. A walk keeps its
//! smaller page for later calls ([`PrCache::page_size`]) and asks for the
//! next larger one again after [`GROW_AFTER`] pages.
//! Any other failed request, one that got no answer in time included, is
//! asked again by a later call. The records are saved
//! after each stage, and [`PrInput::on_event`] is called when each stage
//! starts and ends and when each request is sent.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use craft_core::people::record::{
    Closer, ContextsCursor, Contribution, CrossSource, FileLines, Gap, Light, MainChecks,
    MergeCommit, PrCache, PrCommit, PrRecord, PrState, Suite, TimelineItem,
};
use craft_core::people::roster::Fetched;
use craft_core::people::{is_merge_commit, merger, parse_co_authors, parse_reverts};
use serde_json::{json, Value};

use crate::{
    count, error_messages, graphql_errors, time, Error, GitHub, Result, CONNECT_TIMEOUT,
    OVERLAP_MINUTES, REQUEST_TIMEOUT,
};

/// Pages of [`CHANGED_QUERY`] one call reads per repository.
pub const MAX_CHANGED_PAGES: usize = 4;
/// Timeline items read for a pull request already held, and for one read
/// for the first time.
const TL_CHANGED: u64 = 50;
const TL_NEW: u64 = 100;
/// How often each repository's open pull requests are swept.
pub const SWEEP_MINUTES: i64 = 30;
/// Pages of 100 one sweep reads at most.
const MAX_SWEEP_PAGES: usize = 60;
/// Pull requests per backfill request.
const NODES_PER_REQUEST: usize = 25;
/// Backfill, walk and file-list requests one call sends at most in the app.
pub const RUN_REQUESTS: usize = 30;
/// Pull requests per page of the changed list and the walks, and the smaller
/// pages asked for in turn when GitHub could not finish the query.
const PAGE_SIZES: [u64; 4] = [25, 10, 4, 1];
/// Pages read at a smaller size before the next larger one is asked for
/// again.
const GROW_AFTER: u32 = 10;

/// The stages of one [`GitHub::update_prs`] call, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Changed,
    Sweep,
    Backfill,
    Lists,
    MergedWalk,
    ClosedWalk,
    FileLists,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Changed => "changed lists",
            Stage::Sweep => "sweep",
            Stage::Backfill => "backfill",
            Stage::Lists => "commit and check lists",
            Stage::MergedWalk => "merged walk",
            Stage::ClosedWalk => "closed walk",
            Stage::FileLists => "file lists",
        }
    }
}

/// What [`GitHub::update_prs`] reports to [`PrInput::on_event`] as it goes.
pub enum PrEvent<'a> {
    Start(Stage),
    /// A request is about to go; `sent` counts the call's requests so far.
    Request {
        sent: usize,
    },
    /// A stage ended. `caches` holds every repository's records as they
    /// stand, saved to disk; `changed` says whether the stage changed any.
    End {
        stage: Stage,
        caches: &'a mut [(String, PrCache)],
        changed: bool,
    },
}

/// What [`GitHub::update_prs`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrReport {
    /// Records the changed lists updated.
    pub changed: usize,
    /// Repositories whose open pull requests were swept.
    pub swept: usize,
    /// Records the backfill fetched by id.
    pub backfilled: usize,
    /// Records the merged walk read.
    pub merged: usize,
    /// Records the closed walk read.
    pub closed: usize,
    /// File-list pages read.
    pub file_pages: usize,
    /// Commit-list and check-list pages read after the first.
    pub list_pages: usize,
    /// Requests sent in all, and how many times a smaller query was asked
    /// for because GitHub could not finish one.
    pub requests: usize,
    pub retries: usize,
    /// Records held across the repositories, and how many of them still
    /// need fetching.
    pub records: usize,
    pub pending: usize,
    /// Repositories whose merged and closed walks have both finished.
    pub walks_done: usize,
    /// The optional requests stopped: fewer points left than the floor.
    pub low_budget: bool,
    /// The lowest GraphQL budget an answer reported.
    pub remaining: Option<u64>,
    /// GitHub rate-limited a request, or background requests were stopped;
    /// nothing after it was sent.
    pub halted: bool,
    /// The token user's login, as the sweep read it; `None` when no sweep
    /// answered.
    pub viewer: Option<String>,
    pub errors: Vec<String>,
}

/// What the poll thread's last fetch of a repository saw of its pull
/// requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Seen {
    /// When the most recently updated pull request was updated
    /// ([`crate::RawActivity::pr_updated_at`]).
    pub pr_updated_at: Option<DateTime<Utc>>,
    /// When the fetch read it ([`crate::RawActivity::fetched_at`]): a changed
    /// list skipped because nothing changed is up to date as of then.
    pub read_at: DateTime<Utc>,
}

/// What [`GitHub::update_prs`] works from.
pub struct PrInput<'a> {
    pub repos: &'a [String],
    /// What the last fetch of each repository saw; `None` for one GitHub
    /// cannot show, which is left out. A repository missing from the map
    /// (not fetched yet) gets its changed list read.
    pub seen: &'a HashMap<String, Option<Seen>>,
    /// The cache directory.
    pub dir: &'a Path,
    pub now: DateTime<Utc>,
    /// About how long until the next call, in seconds; it sets the sweep's
    /// share (see [`sweep_pick`]).
    pub interval_secs: i64,
    /// The lowest budget seen so far.
    pub remaining: Option<u64>,
    /// Below this many points left, only the changed lists run.
    pub floor: u64,
    /// Backfill, walk and file-list requests the call sends at most.
    pub max_requests: usize,
    /// The token user's login, as an earlier call read it; their records go
    /// first in the backfill. A sweep in this call reads it again.
    pub viewer: Option<&'a str>,
    pub on_event: Option<&'a (dyn Fn(PrEvent<'_>) + Sync)>,
}

/// The repositories to sweep in one call: those not swept for
/// [`SWEEP_MINUTES`], longest ago first (never swept before all), at most
/// the share that sweeps every repository once per [`SWEEP_MINUTES`] at
/// `interval_secs` between calls.
pub fn sweep_pick(
    swept: &[(String, Option<DateTime<Utc>>)],
    now: DateTime<Utc>,
    interval_secs: i64,
) -> Vec<String> {
    let every = SWEEP_MINUTES * 60;
    let share = (swept.len() as u64 * interval_secs.max(1) as u64)
        .div_ceil(every as u64)
        .max(1) as usize;
    let mut due: Vec<&(String, Option<DateTime<Utc>>)> = swept
        .iter()
        .filter(|(_, at)| at.is_none_or(|at| (now - at).num_seconds() >= every))
        .collect();
    due.sort_by_key(|(_, at)| *at);
    due.into_iter()
        .take(share)
        .map(|(r, _)| r.clone())
        .collect()
}

/// The bookkeeping of one [`GitHub::update_prs`] call.
struct Pass<'a> {
    gh: &'a GitHub,
    now: DateTime<Utc>,
    /// When the call started, `now` on the clock [`Pass::clock`] reads.
    started: std::time::Instant,
    floor: u64,
    /// Requests the call may still send, of every kind.
    left: AtomicUsize,
    report: Mutex<PrReport>,
    on_event: Option<&'a (dyn Fn(PrEvent<'_>) + Sync)>,
}

impl<'a> Pass<'a> {
    fn new(gh: &'a GitHub, now: DateTime<Utc>, floor: u64, max_requests: usize) -> Pass<'a> {
        Pass {
            gh,
            now,
            started: std::time::Instant::now(),
            floor,
            left: AtomicUsize::new(max_requests),
            report: Mutex::new(PrReport::default()),
            on_event: None,
        }
    }

    fn report(&self) -> PrReport {
        self.report.lock().unwrap().clone()
    }

    /// The time now: `now` plus the time since the call started. The sweep
    /// dates what a response shows by this time.
    fn clock(&self) -> DateTime<Utc> {
        self.now + chrono::Duration::from_std(self.started.elapsed()).unwrap_or_default()
    }

    fn update(&self, f: impl FnOnce(&mut PrReport)) {
        f(&mut self.report.lock().unwrap());
    }

    fn error(&self, msg: String) {
        self.update(|r| r.errors.push(msg));
    }

    fn event(&self, event: PrEvent<'_>) {
        if let Some(f) = self.on_event {
            f(event);
        }
    }

    /// Takes one request from those the call has left; `false` when none is
    /// left.
    fn slot(&self) -> bool {
        let taken = self
            .left
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_sub(1))
            .is_ok();
        if !taken {
            return false;
        }
        self.count();
        true
    }

    /// Counts one request in the report and tells [`Pass::on_event`],
    /// without taking it from those the call has left. The report stays
    /// locked until the event is told, so the events arrive in order.
    fn count(&self) {
        let mut report = self.report.lock().unwrap();
        report.requests += 1;
        self.event(PrEvent::Request {
            sent: report.requests,
        });
    }

    /// Keeps the lowest budget an answer reports.
    fn note(&self, v: &Value) {
        if let Some(left) = v["rateLimit"]["remaining"].as_u64() {
            self.update(|report| {
                let r = &mut report.remaining;
                *r = Some(r.map_or(left, |r| r.min(left)));
            });
        }
    }

    fn low(&self) -> bool {
        self.report
            .lock()
            .unwrap()
            .remaining
            .is_some_and(|r| r < self.floor)
    }

    /// Notes that the optional requests stopped for a low budget.
    fn note_low(&self) {
        self.update(|r| r.low_budget = true);
    }

    /// The `data` of a query about one repository. An answer with an error
    /// about its pull request list (a node GitHub could not read) fails, so
    /// the caller does not move past the pull requests it lacks.
    fn repo_query(
        &self,
        query: &str,
        repo: &str,
        mut vars: serde_json::Map<String, Value>,
    ) -> Result<Value> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| Error::GraphQl(format!("bad repo {repo:?}")))?;
        vars.insert("owner".into(), json!(owner));
        vars.insert("name".into(), json!(name));
        let body = self.gh.graphql_body(query, Value::Object(vars))?;
        let errs = graphql_errors(&body);
        if body["data"]["repository"].is_null() && !errs.is_empty() {
            if errs.iter().any(|e| e["type"] == "NOT_FOUND") {
                return Err(Error::NotFound);
            }
            return Err(Error::GraphQl(error_messages(errs)));
        }
        let in_list: Vec<Value> = errs
            .iter()
            .filter(|e| e["path"][0] == "repository" && e["path"][1] == "pullRequests")
            .cloned()
            .collect();
        if !in_list.is_empty() {
            return Err(Error::GraphQl(error_messages(&in_list)));
        }
        let v = body["data"].clone();
        self.note(&v);
        Ok(v)
    }

    /// [`Pass::repo_query`] for one page of `query`, whose `$first` is the
    /// page size: `page_size`, or the largest of [`PAGE_SIZES`]. The caller
    /// has counted the request. When GitHub could not finish the query
    /// ([`Error::is_overload`]) the page is asked for again with the next
    /// smaller size, and `page_size` keeps the smaller size for later pages;
    /// with `capped`, only while the call has requests left. `ok` counts the
    /// pages read at a smaller size, and after [`GROW_AFTER`] of them
    /// `page_size` moves to the next larger size. A request that got no
    /// answer in time is not asked again in this call.
    fn page_query(
        &self,
        query: &str,
        repo: &str,
        vars: serde_json::Map<String, Value>,
        page_size: &mut Option<u64>,
        ok: &mut u32,
        capped: bool,
    ) -> Result<Value> {
        let mut size = page_size
            .and_then(|p| PAGE_SIZES.iter().position(|s| *s <= p))
            .unwrap_or(0);
        loop {
            let mut v = vars.clone();
            v.insert("first".into(), json!(PAGE_SIZES[size]));
            match self.repo_query(query, repo, v) {
                Ok(v) => {
                    if size > 0 {
                        *ok += 1;
                        if *ok >= GROW_AFTER {
                            *ok = 0;
                            *page_size = (size > 1).then(|| PAGE_SIZES[size - 1]);
                        }
                    }
                    return Ok(v);
                }
                Err(e) if e.is_overload() && size + 1 < PAGE_SIZES.len() => {
                    size += 1;
                    *page_size = Some(PAGE_SIZES[size]);
                    *ok = 0;
                    if !capped {
                        self.count();
                    } else if !self.slot() {
                        return Err(e);
                    }
                    self.update(|r| r.retries += 1);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Puts each pull request in `nodes`, read with `tl` timeline items,
    /// into `cache`. A pull request read for the first time (not held, or
    /// held only as a sweep's record that was never fetched) with fewer
    /// than [`TL_NEW`] items whose timeline filled them all is marked for
    /// the backfill, which reads more. So is a fetched one whose items,
    /// only the newest of its timeline, include none of the items it kept:
    /// the items between them were missed, so the kept ones go and the
    /// backfill reads the newest [`TL_NEW`]. A record marked for the
    /// backfill stays marked through such reads while it holds fewer than
    /// the newest [`TL_NEW`] items.
    /// Returns how many there were and the newest and oldest `updatedAt`
    /// among them.
    fn take(
        &self,
        cache: &mut PrCache,
        nodes: &Value,
        tl: u64,
    ) -> (usize, Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
        let (mut n, mut newest, mut oldest) = (0, None, None);
        for node in nodes.as_array().into_iter().flatten() {
            let Some(rec) = parse_pr(node) else {
                continue;
            };
            newest = newest.max(rec.updated_at);
            oldest = match (oldest, rec.updated_at) {
                (None, u) => u,
                (Some(o), Some(u)) => Some(o.min(u)),
                (o, None) => o,
            };
            let number = rec.number;
            let full = tl < TL_NEW && rec.timeline.len() as u64 >= tl;
            let held = cache.records.get_mut(&number);
            let new = held.as_ref().is_none_or(|r| r.fetched_at.is_none());
            let was_stale = held.as_ref().is_some_and(|r| r.stale);
            let gap = match held {
                Some(r) if !new && misses_kept_items(r, &rec) => {
                    r.timeline.clear();
                    true
                }
                _ => false,
            };
            cache.put(rec, self.now);
            if let Some(r) = cache.records.get_mut(&number) {
                // A record still waiting for the backfill stays marked while
                // it holds fewer than the newest items the backfill reads.
                let short = (r.timeline.len() as u64) < r.timeline_total.min(TL_NEW);
                if full && (new || gap || (was_stale && short)) {
                    r.stale = true;
                }
            }
            n += 1;
        }
        (n, newest, oldest)
    }

    /// Reads `repo`'s changed list (see the module docs). Sets the cache's
    /// `checked_at` when the list has nothing left unread: to now, or, when
    /// the fetch `seen` comes from showed nothing new, to when it read that,
    /// never earlier than before.
    fn changed(&self, repo: &str, cache: &mut PrCache, seen: Option<&Seen>) -> Result<()> {
        let first = cache.high_water.is_none();
        let unchanged = seen.is_some_and(|s| s.pr_updated_at <= cache.high_water);
        let as_of = match seen {
            Some(s) if unchanged => s.read_at.min(self.now),
            _ => self.now,
        };
        let checked = |cache: &mut PrCache| {
            cache.checked_at = Some(cache.checked_at.map_or(as_of, |c| c.max(as_of)));
        };
        if unchanged && cache.gap.is_none() {
            checked(cache);
            return Ok(());
        }
        let stop = cache
            .high_water
            .map(|h| h - chrono::Duration::minutes(OVERLAP_MINUTES));
        // The rest of the list, down to `stop`, is read by later calls; an
        // older gap is read on the way.
        let defer = |cache: &mut PrCache, after: &Option<String>| {
            let until = match &cache.gap {
                Some(g) => g.until.min(stop),
                None => stop,
            };
            cache.gap = after.clone().map(|cursor| Gap { cursor, until });
        };
        let tl = if first { TL_NEW } else { TL_CHANGED };
        // Each call starts at the full page; a smaller one lasts the call.
        let (mut page_size, mut page_ok) = (None, 0);
        let mut pages = 0;
        let mut after: Option<String> = None;
        let mut newest = cache.high_water;
        if !unchanged {
            loop {
                self.count();
                let mut vars = serde_json::Map::new();
                vars.insert("after".into(), json!(after));
                vars.insert("tl".into(), json!(tl));
                let v = self.page_query(
                    &changed_query(),
                    repo,
                    vars,
                    &mut page_size,
                    &mut page_ok,
                    false,
                )?;
                pages += 1;
                let conn = &v["repository"]["pullRequests"];
                let (n, top, bottom) = self.take(cache, &conn["nodes"], tl);
                self.update(|r| r.changed += n);
                newest = newest.max(top);
                let reached = match (stop, bottom) {
                    (Some(s), Some(b)) => b < s,
                    _ => false,
                };
                let more = conn["pageInfo"]["hasNextPage"].as_bool() == Some(true);
                after = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
                if first || reached || !more {
                    break;
                }
                if pages >= MAX_CHANGED_PAGES {
                    defer(cache, &after);
                    break;
                }
            }
            cache.high_water = newest;
        }
        while pages < MAX_CHANGED_PAGES {
            let Some(gap) = cache.gap.clone() else {
                break;
            };
            self.count();
            let mut vars = serde_json::Map::new();
            vars.insert("after".into(), json!(gap.cursor));
            vars.insert("tl".into(), json!(TL_CHANGED));
            let v = self.page_query(
                &changed_query(),
                repo,
                vars,
                &mut page_size,
                &mut page_ok,
                false,
            )?;
            pages += 1;
            let conn = &v["repository"]["pullRequests"];
            let (n, _, bottom) = self.take(cache, &conn["nodes"], TL_CHANGED);
            self.update(|r| r.changed += n);
            let reached = match (gap.until, bottom) {
                (Some(u), Some(b)) => b < u,
                _ => false,
            };
            let more = conn["pageInfo"]["hasNextPage"].as_bool() == Some(true);
            cache.gap = match conn["pageInfo"]["endCursor"].as_str() {
                Some(cursor) if more && !reached => Some(Gap {
                    cursor: cursor.to_string(),
                    until: gap.until,
                }),
                _ => None,
            };
        }
        if cache.gap.is_none() {
            checked(cache);
        }
        Ok(())
    }

    /// Sweeps `repo`'s open pull requests. Returns whether every page was
    /// read; only then are records missing from the open list marked for
    /// the backfill and `swept_at` set. Its pages count in the report but
    /// not against [`PrInput::max_requests`]: a sweep starts from the first
    /// page each time, so it reads up to [`MAX_SWEEP_PAGES`] pages whatever
    /// the cap leaves.
    fn sweep(&self, repo: &str, cache: &mut PrCache) -> Result<bool> {
        let mut after: Option<String> = None;
        let mut seen = BTreeSet::new();
        for page in 0..MAX_SWEEP_PAGES {
            if self.low() {
                self.note_low();
                return Ok(false);
            }
            self.count();
            let mut vars = serde_json::Map::new();
            vars.insert("after".into(), json!(after));
            let v = self.repo_query(SWEEP_QUERY, repo, vars)?;
            let r = &v["repository"];
            if page == 0 {
                // The checks on main are kept once every page is read; below
                // the floor the checks read before stay.
                let mut target = r["defaultBranchRef"]["target"].clone();
                let mut next = contexts_cursor(&target);
                while let Some(at) = next.take() {
                    if self.low() {
                        self.note_low();
                        return Ok(false);
                    }
                    self.count();
                    let (rollup, more) = self.contexts_page(&at)?;
                    let page = &rollup["contexts"];
                    let contexts = &mut target["statusCheckRollup"]["contexts"];
                    if let (Some(nodes), Some(read)) =
                        (contexts["nodes"].as_array_mut(), page["nodes"].as_array())
                    {
                        nodes.extend(read.iter().cloned());
                    }
                    if page["pageInfo"].is_object() {
                        contexts["pageInfo"] = page["pageInfo"].clone();
                    }
                    next = more;
                }
                cache.note_main(parse_main(&target), self.clock());
                if let Some(me) = login(&v["viewer"]) {
                    self.update(|r| r.viewer = Some(me));
                }
            }
            let conn = &r["pullRequests"];
            for light in conn["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(parse_light)
            {
                seen.insert(light.number);
                match cache.records.get_mut(&light.number) {
                    Some(rec) => rec.apply_light(&light, self.now),
                    None => {
                        cache
                            .records
                            .insert(light.number, PrRecord::from_light(&light, self.now));
                    }
                }
            }
            if conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                for rec in cache.records.values_mut() {
                    if rec.state == PrState::Open && !seen.contains(&rec.number) {
                        rec.stale = true;
                    }
                }
                cache.swept_at = Some(self.now);
                return Ok(true);
            }
            after = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
        }
        Ok(false)
    }

    /// Fetches the records that need it across `caches`, by node id, the
    /// token user's first ([`PrReport::viewer`]), then the most recently
    /// updated. A batch that GitHub could not finish
    /// ([`Error::is_overload`]) is split in two and asked for again; any
    /// other failure, or one of a single record, leaves the records to be
    /// fetched by a later call.
    fn backfill(&self, caches: &mut [(String, PrCache)]) -> Result<()> {
        let viewer = self.report().viewer;
        let mine = |author: Option<&str>| {
            author.is_some_and(|a| viewer.as_deref().is_some_and(|v| v.eq_ignore_ascii_case(a)))
        };
        // (not the viewer's, newest update first, cache index, node id)
        let mut want = Vec::new();
        for (i, (_, c)) in caches.iter().enumerate() {
            for r in c
                .records
                .values()
                .filter(|r| r.needs_fetch() && !r.id.is_empty())
            {
                want.push((
                    !mine(r.author.as_deref()),
                    std::cmp::Reverse(r.updated_at),
                    i,
                    r.id.clone(),
                ));
            }
        }
        want.sort();
        let batches: Vec<Vec<(usize, String)>> = want
            .chunks(NODES_PER_REQUEST)
            .map(|c| c.iter().map(|(.., i, id)| (*i, id.clone())).collect())
            .collect();
        let caches = Mutex::new(caches);
        run_tasks(batches, |batch: Vec<(usize, String)>| {
            if self.low() {
                self.note_low();
                return Ok(vec![]);
            }
            if !self.slot() {
                return Ok(vec![]);
            }
            let ids: Vec<&str> = batch.iter().map(|(_, id)| id.as_str()).collect();
            let answer = self
                .gh
                .graphql_body(&nodes_query(), json!({ "ids": ids, "tl": TL_NEW }))
                .and_then(|v| match v["data"]["nodes"].is_array() {
                    true => Ok(v),
                    false => Err(Error::GraphQl(error_messages(graphql_errors(&v)))),
                });
            let v = match answer {
                Ok(v) => v,
                Err(e @ (Error::Halted(_) | Error::RateLimited(_))) => return Err(e),
                Err(e) if e.is_overload() && batch.len() > 1 => {
                    self.update(|r| r.retries += 1);
                    let (a, b) = batch.split_at(batch.len() / 2);
                    return Ok(vec![a.to_vec(), b.to_vec()]);
                }
                Err(e) => {
                    let first = batch.first().map_or("", |(_, id)| id.as_str());
                    let more = match batch.len() {
                        1 => String::new(),
                        n => format!(" and {} more", n - 1),
                    };
                    self.error(format!("backfill of {first}{more}: {e}"));
                    return Ok(vec![]);
                }
            };
            let data = &v["data"];
            self.note(data);
            let mut caches = caches.lock().unwrap();
            for (k, (i, id)) in batch.iter().enumerate() {
                let cache = &mut caches[*i].1;
                match parse_pr(&data["nodes"][k]) {
                    // A field GitHub could not read: the record stays as it
                    // was, to be fetched again.
                    Some(_) if !errors_at(&v, k).is_empty() => self.error(format!(
                        "backfill of {id}: {}",
                        error_messages(errors_at(&v, k).as_slice())
                    )),
                    Some(rec) => {
                        cache.put(rec, self.now);
                        self.update(|r| r.backfilled += 1);
                    }
                    // Deleted, or no longer visible.
                    None if not_found_at(&v, k) => cache.records.retain(|_, r| r.id != *id),
                    // Any other error: the record stays, to be fetched again.
                    None => self.error(format!(
                        "backfill of {id}: {}",
                        error_messages(errors_at(&v, k).as_slice())
                    )),
                }
            }
            Ok(vec![])
        })
    }

    /// Reads one page of the checks on a commit, from `at`: the page's
    /// `statusCheckRollup`, and where the next page starts. The caller has
    /// counted the request.
    fn contexts_page(&self, at: &ContextsCursor) -> Result<(Value, Option<ContextsCursor>)> {
        let vars = json!({ "id": at.commit, "after": at.after });
        let v = self.gh.graphql_body(CONTEXTS_QUERY, vars)?;
        self.update(|r| r.list_pages += 1);
        let data = &v["data"];
        self.note(data);
        let errs = graphql_errors(&v);
        if !errs.is_empty() {
            return Err(Error::GraphQl(error_messages(errs)));
        }
        let rollup = data["node"]["statusCheckRollup"].clone();
        let next = next_cursor(&rollup["contexts"]).map(|after| ContextsCursor {
            commit: at.commit.clone(),
            after,
        });
        Ok((rollup, next))
    }

    /// Reads the rest of the commit lists of `cache`'s records with more
    /// than 100 commits, and the rest of the checks on the heads of its open
    /// records with more than 100, while the call has requests left. A
    /// record that needs fetching is read in full by a later call, after its
    /// fetch.
    fn lists(&self, cache: &mut PrCache) -> Result<()> {
        for rec in cache.records.values_mut().filter(|r| !r.needs_fetch()) {
            while let Some(after) = rec.contribution.commits_cursor.clone() {
                if self.low() {
                    self.note_low();
                    return Ok(());
                }
                if !self.slot() {
                    return Ok(());
                }
                let vars = json!({ "id": rec.id, "after": after });
                let v = self.gh.graphql_body(COMMITS_QUERY, vars)?;
                self.update(|r| r.list_pages += 1);
                let data = &v["data"];
                self.note(data);
                let commits = &data["node"]["commits"];
                let errs = graphql_errors(&v);
                // The pull request is gone: nothing more to read.
                if !commits.is_object() && errs.iter().any(|e| e["type"] == "NOT_FOUND") {
                    rec.contribution.commits_cursor = None;
                    break;
                }
                if !errs.is_empty() {
                    return Err(Error::GraphQl(error_messages(errs)));
                }
                let additions = rec.contribution.additions;
                rec.contribution
                    .commits
                    .extend(parse_commits(commits, additions));
                rec.contribution.commits_cursor = next_cursor(commits);
            }
            while let Some(at) = rec.checks.contexts_cursor.clone() {
                if self.low() {
                    self.note_low();
                    return Ok(());
                }
                if !self.slot() {
                    return Ok(());
                }
                let (rollup, next) = self.contexts_page(&at)?;
                add_names(&mut rec.checks.failing, failing_checks(&rollup));
                if let Some(statuses) = rec.checks.failing_statuses.as_mut() {
                    add_names(statuses, failing_statuses(&rollup));
                }
                rec.checks.contexts_cursor = next;
            }
        }
        Ok(())
    }

    /// Continues `repo`'s walk over merged pull requests, or over those
    /// closed without merging when `closed`, while the call has requests
    /// left.
    fn walk(&self, repo: &str, cache: &mut PrCache, closed: bool) -> Result<()> {
        let query = walk_query(if closed { "CLOSED" } else { "MERGED" });
        loop {
            let w = if closed {
                &cache.closed_walk
            } else {
                &cache.merged_walk
            };
            if w.done {
                break;
            }
            if self.low() {
                self.note_low();
                break;
            }
            if !self.slot() {
                break;
            }
            let mut vars = serde_json::Map::new();
            vars.insert("after".into(), json!(w.cursor));
            vars.insert("tl".into(), json!(TL_NEW));
            let v = self.page_query(
                &query,
                repo,
                vars,
                &mut cache.page_size,
                &mut cache.page_ok,
                true,
            )?;
            let conn = &v["repository"]["pullRequests"];
            let (n, _, _) = self.take(cache, &conn["nodes"], TL_NEW);
            if closed {
                self.update(|r| r.closed += n);
            } else {
                self.update(|r| r.merged += n);
            }
            let w = if closed {
                &mut cache.closed_walk
            } else {
                &mut cache.merged_walk
            };
            if let Some(total) = conn["totalCount"].as_u64() {
                w.total = Some(total);
            }
            match conn["pageInfo"]["endCursor"].as_str() {
                Some(c) if conn["pageInfo"]["hasNextPage"].as_bool() == Some(true) => {
                    w.cursor = Some(c.to_string())
                }
                _ => {
                    w.cursor = None;
                    w.done = true;
                    if cache.merged_walk.done && cache.closed_walk.done {
                        cache.page_size = None;
                        cache.page_ok = 0;
                    }
                }
            }
        }
        Ok(())
    }

    /// Reads the rest of the file list of `cache`'s merged records with more
    /// than 100 changed files while the call has requests left.
    fn file_pages(&self, cache: &mut PrCache) -> Result<()> {
        for rec in cache.records.values_mut() {
            while rec.state == PrState::Merged && rec.contribution.files_cursor.is_some() {
                if self.low() {
                    self.note_low();
                    return Ok(());
                }
                if !self.slot() {
                    return Ok(());
                }
                let vars = json!({ "id": rec.id, "after": rec.contribution.files_cursor });
                let v = self.gh.graphql_body(FILES_QUERY, vars)?;
                self.update(|r| r.file_pages += 1);
                let data = &v["data"];
                self.note(data);
                let files = &data["node"]["files"];
                let errs = graphql_errors(&v);
                if !files.is_object() {
                    // The pull request is gone: nothing more to read.
                    if errs.is_empty() || errs.iter().any(|e| e["type"] == "NOT_FOUND") {
                        rec.contribution.files_cursor = None;
                        break;
                    }
                }
                // Any other error, one about some of the files included:
                // the cursor stays for the next call.
                if !errs.is_empty() {
                    let msg = error_messages(errs);
                    self.error(format!("file list of #{}: {msg}", rec.number));
                    return Ok(());
                }
                rec.contribution.files.extend(parse_files(files));
                rec.contribution.files_cursor = next_cursor(files);
            }
        }
        Ok(())
    }
}

impl GitHub {
    /// Brings every repository's [`PrCache`] in `input.dir` up to date (see
    /// the module docs), saves the caches it changed after each stage and
    /// returns them, by repository. A rate limit, or background requests
    /// being stopped, ends the pass; an error about one repository skips the
    /// rest of that repository's requests in that stage. A repository whose
    /// last fetch found it hidden is left out.
    pub fn update_prs(&self, input: &PrInput) -> (PrReport, Vec<(String, PrCache)>) {
        let mut pass = Pass::new(self, input.now, input.floor, input.max_requests);
        pass.on_event = input.on_event;
        pass.update(|r| {
            r.remaining = input.remaining;
            r.viewer = input.viewer.map(str::to_string);
        });
        let mut caches: Vec<(String, PrCache)> = input
            .repos
            .iter()
            .filter(|r| !matches!(input.seen.get(*r), Some(None)))
            .map(|r| (r.clone(), PrCache::load(input.dir, r)))
            .collect();
        let mut saved: Vec<PrCache> = caches.iter().map(|(_, c)| c.clone()).collect();
        let mut end = |stage: Stage, caches: &mut [(String, PrCache)]| {
            let mut changed = false;
            for ((repo, cache), old) in caches.iter().zip(saved.iter_mut()) {
                if cache == old {
                    continue;
                }
                changed = true;
                if let Err(e) = cache.save(input.dir, repo) {
                    pass.error(format!("{repo}: could not save pull requests: {e}"));
                }
                *old = cache.clone();
            }
            pass.event(PrEvent::End {
                stage,
                caches,
                changed,
            });
        };
        let result = run_pass(&pass, &mut caches, input, &mut end);
        if let Err(e) = result {
            pass.update(|r| {
                r.halted = matches!(e, Error::Halted(_) | Error::RateLimited(_));
                r.errors.push(e.to_string());
            });
        }
        let mut report = pass.report();
        for (_, cache) in &caches {
            report.records += cache.records.len();
            report.pending += cache.records.values().filter(|r| r.needs_fetch()).count();
            report.walks_done += usize::from(cache.merged_walk.done && cache.closed_walk.done);
        }
        (report, caches)
    }
}

/// What [`run_pass`] calls after a stage, with the stage and every
/// repository's records.
type StageEnd<'a> = dyn FnMut(Stage, &mut [(String, PrCache)]) + 'a;

/// The stages of one pass, each over the repositories that have work in it,
/// one at a time; `end` runs after each stage that had any. An `Err` is a
/// rate limit or a stop, which ends the pass.
fn run_pass(
    pass: &Pass,
    caches: &mut [(String, PrCache)],
    input: &PrInput,
    end: &mut StageEnd<'_>,
) -> Result<()> {
    let stop = |e: &Error| matches!(e, Error::Halted(_) | Error::RateLimited(_));
    // Runs `work` on each of `repos` in a stage; an error that is not a rate
    // limit is noted as `what` failed for that repository.
    let stage = |stage: Stage,
                 repos: Vec<&mut (String, PrCache)>,
                 what: &str,
                 work: &dyn Fn(&str, &mut PrCache) -> Result<()>| {
        if repos.is_empty() {
            return None;
        }
        pass.event(PrEvent::Start(stage));
        Some(run_tasks(repos, |(repo, cache)| {
            match work(repo, cache) {
                Err(e) if stop(&e) => return Err(e),
                Err(e) => pass.error(format!("{repo}: {what}: {e}")),
                Ok(()) => {}
            }
            Ok(vec![])
        }))
    };

    // The changed lists of the repositories checked longest ago go first.
    let mut repos: Vec<&mut (String, PrCache)> = caches.iter_mut().collect();
    repos.sort_by_key(|(_, c)| c.checked_at);
    let result = stage(
        Stage::Changed,
        repos,
        "changed pull requests",
        &|repo, cache| {
            let seen = input.seen.get(repo).and_then(Option::as_ref);
            pass.changed(repo, cache, seen)
        },
    );
    if let Some(result) = result {
        end(Stage::Changed, caches);
        result?;
    }
    if pass.low() {
        pass.note_low();
        return Ok(());
    }

    let swept: Vec<(String, Option<DateTime<Utc>>)> = caches
        .iter()
        .map(|(r, c)| (r.clone(), c.swept_at))
        .collect();
    let pick = sweep_pick(&swept, input.now, input.interval_secs);
    let mut repos: Vec<&mut (String, PrCache)> = caches
        .iter_mut()
        .filter(|(r, _)| pick.contains(r))
        .collect();
    repos.sort_by_key(|(r, _)| pick.iter().position(|p| p == r));
    let result = stage(Stage::Sweep, repos, "sweep", &|repo, cache| {
        if pass.sweep(repo, cache)? {
            pass.update(|r| r.swept += 1);
        }
        Ok(())
    });
    if let Some(result) = result {
        end(Stage::Sweep, caches);
        result?;
    }

    if caches.iter().any(|(_, c)| {
        c.records
            .values()
            .any(|r| r.needs_fetch() && !r.id.is_empty())
    }) {
        pass.event(PrEvent::Start(Stage::Backfill));
        let result = pass.backfill(caches);
        end(Stage::Backfill, caches);
        result?;
    }

    let repos: Vec<&mut (String, PrCache)> = caches
        .iter_mut()
        .filter(|(_, c)| {
            c.records.values().any(|r| {
                !r.needs_fetch()
                    && (r.contribution.commits_cursor.is_some()
                        || r.checks.contexts_cursor.is_some())
            })
        })
        .collect();
    let result = stage(
        Stage::Lists,
        repos,
        "commit and check lists",
        &|_, cache| pass.lists(cache),
    );
    if let Some(result) = result {
        end(Stage::Lists, caches);
        result?;
    }

    for (kind, closed) in [(Stage::MergedWalk, false), (Stage::ClosedWalk, true)] {
        let repos: Vec<&mut (String, PrCache)> = caches
            .iter_mut()
            .filter(|(_, c)| match closed {
                true => !c.closed_walk.done,
                false => !c.merged_walk.done,
            })
            .collect();
        let what = if closed {
            "closed pull requests"
        } else {
            "merged pull requests"
        };
        let result = stage(kind, repos, what, &|repo, cache| {
            pass.walk(repo, cache, closed)
        });
        if let Some(result) = result {
            end(kind, caches);
            result?;
        }
    }

    let repos: Vec<&mut (String, PrCache)> = caches
        .iter_mut()
        .filter(|(_, c)| {
            c.records
                .values()
                .any(|r| r.state == PrState::Merged && r.contribution.files_cursor.is_some())
        })
        .collect();
    let result = stage(Stage::FileLists, repos, "file lists", &|_, cache| {
        pass.file_pages(cache)
    });
    if let Some(result) = result {
        end(Stage::FileLists, caches);
        result?;
    }
    Ok(())
}

/// Runs `work` on each of `tasks` in turn, and on the further tasks each
/// call returns, until none is left. The further tasks run before the tasks
/// still waiting, so the halves of a split backfill batch go before the next
/// batch. The first error stops the tasks not yet started and is returned.
fn run_tasks<T>(tasks: Vec<T>, work: impl Fn(T) -> Result<Vec<T>>) -> Result<()> {
    let mut queue = VecDeque::from(tasks);
    while let Some(task) = queue.pop_front() {
        for more in work(task)?.into_iter().rev() {
            queue.push_front(more);
        }
    }
    Ok(())
}

/// Downloads the roster CSV at `url`, sending `etag` as `If-None-Match`.
/// The request carries no GitHub token.
pub fn download_roster(url: &str, etag: Option<&str>) -> std::result::Result<Fetched, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client
        .get(url)
        .timeout(REQUEST_TIMEOUT)
        .header(reqwest::header::USER_AGENT, "craft-status");
    if let Some(etag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    if resp.status().as_u16() == 304 {
        return Ok(Fetched::NotModified);
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let text = resp.text().map_err(|e| e.to_string())?;
    Ok(Fetched::Fresh { etag, text })
}

/// The errors of a [`NODES_QUERY`] answer about its `k`th node.
fn errors_at(v: &Value, k: usize) -> Vec<Value> {
    graphql_errors(v)
        .iter()
        .filter(|e| e["path"][0] == "nodes" && e["path"][1].as_u64() == Some(k as u64))
        .cloned()
        .collect()
}

/// Whether GitHub answered that the `k`th node of a [`NODES_QUERY`] does
/// not exist (or the token cannot see it).
fn not_found_at(v: &Value, k: usize) -> bool {
    errors_at(v, k).iter().any(|e| e["type"] == "NOT_FOUND")
}

fn login(v: &Value) -> Option<String> {
    v["login"].as_str().map(str::to_string)
}

fn bot(v: &Value) -> bool {
    v["__typename"] == "Bot"
}

fn string(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn next_cursor(conn: &Value) -> Option<String> {
    if conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
        return None;
    }
    conn["pageInfo"]["endCursor"].as_str().map(str::to_string)
}

/// Whether `fresh` read only the newest items of its timeline, fewer than
/// were added since `kept` was read, and none of them is an item `kept`
/// holds: the items between them were not read.
fn misses_kept_items(kept: &PrRecord, fresh: &PrRecord) -> bool {
    let kept_ids: BTreeSet<&str> = kept
        .timeline
        .iter()
        .map(|t| t.id.as_str())
        .filter(|id| !id.is_empty())
        .collect();
    let read = fresh.timeline.len() as u64;
    read + kept.timeline_total < fresh.timeline_total
        && !kept_ids.is_empty()
        && !fresh
            .timeline
            .iter()
            .any(|t| kept_ids.contains(t.id.as_str()))
}

fn parse_files(conn: &Value) -> Vec<FileLines> {
    conn["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| FileLines {
            path: string(&f["path"]),
            additions: f["additions"].as_u64().unwrap_or(0),
            deletions: f["deletions"].as_u64().unwrap_or(0),
        })
        .collect()
}

fn parse_suites(conn: &Value) -> Vec<Suite> {
    conn["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| Suite {
            status: string(&s["status"]),
            conclusion: s["conclusion"].as_str().map(str::to_string),
            created_at: time(&s["createdAt"]),
            app: s["app"]["slug"].as_str().map(str::to_string),
        })
        .collect()
}

/// The conclusions and states of a failed check.
const FAILED: &[&str] = &["FAILURE", "ERROR", "TIMED_OUT", "STARTUP_FAILURE"];

/// The names of the failing checks in a `statusCheckRollup`.
fn failing_checks(rollup: &Value) -> Vec<String> {
    checks_with(rollup, FAILED, |_| true)
}

/// The names of the failing commit statuses (status contexts) in a
/// `statusCheckRollup`.
fn failing_statuses(rollup: &Value) -> Vec<String> {
    checks_with(rollup, FAILED, |c| {
        c["__typename"].as_str() != Some("CheckRun")
    })
}

/// The names of the checks in a `statusCheckRollup` that `keep` accepts and
/// whose conclusion (a check run's) or state (a status context's) is one of
/// `outcomes`.
fn checks_with(rollup: &Value, outcomes: &[&str], keep: impl Fn(&Value) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    for c in rollup["contexts"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| keep(c))
    {
        let (name, outcome) = match c["__typename"].as_str() {
            Some("CheckRun") => (&c["name"], &c["conclusion"]),
            _ => (&c["context"], &c["state"]),
        };
        let hit = outcome.as_str().is_some_and(|o| outcomes.contains(&o));
        if let (true, Some(name)) = (hit, name.as_str()) {
            if !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Adds the names in `names` that `into` lacks.
fn add_names(into: &mut Vec<String>, names: Vec<String>) {
    for name in names {
        if !into.contains(&name) {
            into.push(name);
        }
    }
}

/// The default branch's head commit and its checks, from the sweep.
fn parse_main(target: &Value) -> Option<MainChecks> {
    let oid = target["oid"].as_str()?;
    let rollup = &target["statusCheckRollup"];
    Some(MainChecks {
        oid: oid.to_string(),
        rollup: rollup["state"].as_str().map(str::to_string),
        failing: failing_checks(rollup),
        // With pages of checks left unread, a failure under a passing
        // check's name may be unread, so no check is kept as passing.
        passing: if rollup["contexts"]["pageInfo"]["hasNextPage"].as_bool() == Some(true) {
            Vec::new()
        } else {
            checks_with(rollup, &["SUCCESS"], |_| true)
        },
        ..Default::default()
    })
}

/// One node of the sweep's open list.
pub fn parse_light(v: &Value) -> Option<Light> {
    let commit = &v["commits"]["nodes"][0]["commit"];
    Some(Light {
        id: v["id"].as_str()?.to_string(),
        number: v["number"].as_u64()?,
        title: string(&v["title"]),
        author: login(&v["author"]),
        author_bot: bot(&v["author"]),
        updated_at: time(&v["updatedAt"]),
        is_draft: v["isDraft"].as_bool().unwrap_or(false),
        head_ref_oid: string(&v["headRefOid"]),
        mergeable: string(&v["mergeable"]),
        merge_state: string(&v["mergeStateStatus"]),
        maintainer_can_modify: v["maintainerCanModify"].as_bool().unwrap_or(false),
        comments_total: count(&v["comments"]) + count(&v["reviews"]),
        rollup: commit["statusCheckRollup"]["state"]
            .as_str()
            .map(str::to_string),
        suites: parse_suites(&commit["checkSuites"]),
        suites_total: commit["checkSuites"]["totalCount"].as_u64(),
    })
}

/// One pull request read with [`PR_FRAGMENT`]; `None` for a null node.
pub fn parse_pr(v: &Value) -> Option<PrRecord> {
    let number = v["number"].as_u64()?;
    let state = match v["state"].as_str() {
        Some("MERGED") => PrState::Merged,
        Some("CLOSED") => PrState::Closed,
        _ => PrState::Open,
    };
    let head = &v["head"]["nodes"][0]["commit"];
    let rollup = &head["statusCheckRollup"];
    let additions = v["additions"].as_u64().unwrap_or(0);
    let commits = parse_commits(&v["commits"], additions);
    let mc = &v["mergeCommit"];
    let merge_commit = mc["oid"].as_str().map(|oid| MergeCommit {
        oid: oid.to_string(),
        headline: string(&mc["messageHeadline"]),
        parents: count(&mc["parents"]),
        committer_name: string(&mc["committer"]["name"]),
        committer_login: login(&mc["committer"]["user"]),
    });
    let merged_by = login(&v["mergedBy"]);
    let resolved = (state == PrState::Merged)
        .then(|| merger(merge_commit.as_ref(), merged_by.as_deref()))
        .flatten();
    let issues = v["closingIssuesReferences"]["nodes"].as_array();
    let mut rec = PrRecord {
        id: string(&v["id"]),
        number,
        url: string(&v["url"]),
        title: string(&v["title"]),
        state,
        is_draft: v["isDraft"].as_bool().unwrap_or(false),
        author: login(&v["author"]),
        author_bot: bot(&v["author"]),
        author_association: string(&v["authorAssociation"]),
        created_at: time(&v["createdAt"]),
        updated_at: time(&v["updatedAt"]),
        closed_at: time(&v["closedAt"]),
        merged_at: time(&v["mergedAt"]),
        merged_by,
        head_ref_name: string(&v["headRefName"]),
        head_ref_oid: string(&v["headRefOid"]),
        base_ref_name: string(&v["baseRefName"]),
        base_ref_oid: string(&v["baseRefOid"]),
        is_cross_repository: v["isCrossRepository"].as_bool().unwrap_or(false),
        maintainer_can_modify: v["maintainerCanModify"].as_bool().unwrap_or(false),
        body: string(&v["body"]),
        assignees: v["assignees"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(login)
            .collect(),
        closing_issues: issues
            .into_iter()
            .flatten()
            .filter_map(|i| i["number"].as_u64())
            .collect(),
        closing_titles: issues
            .into_iter()
            .flatten()
            .map(|i| string(&i["title"]))
            .collect(),
        closing_repos: issues
            .into_iter()
            .flatten()
            .map(|i| string(&i["repository"]["nameWithOwner"]))
            .collect(),
        timeline: parse_timeline(&v["timelineItems"]),
        timeline_total: count(&v["timelineItems"]),
        contribution: Contribution {
            additions,
            deletions: v["deletions"].as_u64().unwrap_or(0),
            changed_files: v["changedFiles"].as_u64().unwrap_or(0),
            files: parse_files(&v["files"]),
            files_total: count(&v["files"]),
            files_cursor: next_cursor(&v["files"]),
            files_head: string(&v["headRefOid"]),
            commits,
            commits_total: count(&v["commits"]),
            commits_cursor: next_cursor(&v["commits"]),
            merge_commit,
            merger: resolved,
        },
        ..Default::default()
    };
    rec.checks.mergeable = string(&v["mergeable"]);
    rec.checks.merge_state = string(&v["mergeStateStatus"]);
    rec.checks.rollup = rollup["state"].as_str().map(str::to_string);
    rec.checks.failing = failing_checks(rollup);
    rec.checks.failing_statuses = Some(failing_statuses(rollup));
    if rec.state == PrState::Open {
        rec.checks.contexts_cursor = contexts_cursor(head);
    }
    rec.checks.suites = parse_suites(&head["checkSuites"]);
    rec.checks.suites_total = head["checkSuites"]["totalCount"].as_u64();
    Some(rec)
}

/// The commits of a `commits` connection of a pull request with
/// `additions` lines added.
fn parse_commits(conn: &Value, additions: u64) -> Vec<PrCommit> {
    conn["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| {
            let c = &n["commit"];
            let headline = string(&c["messageHeadline"]);
            let adds = c["additions"].as_u64().unwrap_or(0);
            PrCommit {
                oid: string(&c["oid"]),
                authored_at: time(&c["authoredDate"]),
                committed_at: time(&c["committedDate"]),
                additions: adds,
                deletions: c["deletions"].as_u64().unwrap_or(0),
                author: login(&c["author"]["user"]),
                author_email: string(&c["author"]["email"]).to_lowercase(),
                is_merge: is_merge_commit(&headline, adds, additions),
                co_authors: parse_co_authors(c["message"].as_str().unwrap_or_default()),
                reverts: parse_reverts(&headline, c["message"].as_str().unwrap_or_default()),
                headline,
            }
        })
        .collect()
}

/// Where the rest of `commit`'s checks start, when they fill more than the
/// page read.
fn contexts_cursor(commit: &Value) -> Option<ContextsCursor> {
    Some(ContextsCursor {
        commit: commit["id"].as_str()?.to_string(),
        after: next_cursor(&commit["statusCheckRollup"]["contexts"])?,
    })
}

/// The timeline items of a `timelineItems` connection, oldest first.
fn parse_timeline(conn: &Value) -> Vec<TimelineItem> {
    let mut out: Vec<TimelineItem> = conn["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let kind = n["__typename"].as_str()?.to_string();
            let who = if n["author"].is_object() {
                &n["author"]
            } else {
                &n["actor"]
            };
            let source = &n["source"];
            let closer = &n["closer"];
            Some(TimelineItem {
                id: string(&n["id"]),
                at: time(&n["createdAt"]).or_else(|| time(&n["submittedAt"])),
                actor: login(who),
                actor_bot: bot(who),
                association: n["authorAssociation"].as_str().map(str::to_string),
                body: n["body"].as_str().map(str::to_string),
                state: n["state"]
                    .as_str()
                    .or_else(|| n["stateReason"].as_str())
                    .map(str::to_string),
                commit: n["commit"]["oid"].as_str().map(str::to_string),
                assignee: login(&n["assignee"]),
                will_close: n["willCloseTarget"].as_bool().unwrap_or(false),
                source: source["number"].as_u64().map(|number| CrossSource {
                    kind: string(&source["__typename"]),
                    repo: string(&source["repository"]["nameWithOwner"]),
                    number,
                    state: string(if source["prState"].is_string() {
                        &source["prState"]
                    } else {
                        &source["issueState"]
                    }),
                    title: string(&source["title"]),
                    merged_at: time(&source["mergedAt"]),
                    author: login(&source["author"]),
                }),
                closer: closer["__typename"].as_str().map(|k| Closer {
                    kind: k.to_string(),
                    number: closer["number"].as_u64(),
                    oid: closer["oid"].as_str().map(str::to_string),
                }),
                url: n["url"].as_str().map(str::to_string),
                kind,
            })
        })
        .collect();
    out.sort_by_key(|t| t.at);
    out
}

/// The changed list: one page of a repository's pull requests in every
/// state, newest update first.
pub fn changed_query() -> String {
    format!("{CHANGED_QUERY}{PR_FRAGMENT}")
}

/// One page of a repository's pull requests in `state` (`MERGED` or
/// `CLOSED`), newest update first.
pub fn walk_query(state: &str) -> String {
    format!("{}{PR_FRAGMENT}", WALK_QUERY.replace("$STATE", state))
}

/// Pull requests by node id.
pub fn nodes_query() -> String {
    format!("{NODES_QUERY}{PR_FRAGMENT}")
}

const CHANGED_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String, $tl: Int!, $first: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequests(first: $first, after: $after, orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes { ...R }
    }
  }
  rateLimit { cost remaining }
}
"#;

/// [`walk_query`] puts the state in place of `$STATE`.
const WALK_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String, $tl: Int!, $first: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: $STATE, first: $first, after: $after, orderBy: {field: UPDATED_AT, direction: DESC}) {
      totalCount pageInfo { hasNextPage endCursor }
      nodes { ...R }
    }
  }
  rateLimit { cost remaining }
}
"#;

const NODES_QUERY: &str = r#"
query($ids: [ID!]!, $tl: Int!) {
  nodes(ids: $ids) { ...R }
  rateLimit { cost remaining }
}
"#;

/// Every open pull request's light fields, 100 a page, the checks on the
/// head of the default branch, and the token user's login.
const SWEEP_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    defaultBranchRef { target { ... on Commit { id oid statusCheckRollup { state contexts(first: 100) { pageInfo { hasNextPage endCursor } nodes { __typename ... on CheckRun { name conclusion } ... on StatusContext { context state } } } } } } }
    pullRequests(states: OPEN, first: 100, after: $after, orderBy: {field: CREATED_AT, direction: ASC}) {
      totalCount pageInfo { hasNextPage endCursor }
      nodes { id number title author { __typename login } updatedAt isDraft headRefOid mergeable mergeStateStatus maintainerCanModify
        comments { totalCount } reviews { totalCount }
        commits(last: 1) { nodes { commit { statusCheckRollup { state } checkSuites(first: 8) { totalCount nodes { status conclusion createdAt app { slug } } } } } } }
    }
  }
  viewer { login }
  rateLimit { cost remaining }
}
"#;

/// The next page of one pull request's changed files.
const FILES_QUERY: &str = r#"
query($id: ID!, $after: String) {
  node(id: $id) { ... on PullRequest { files(first: 100, after: $after) { totalCount pageInfo { hasNextPage endCursor } nodes { path additions deletions } } } }
  rateLimit { cost remaining }
}
"#;

/// The next page of one pull request's commits.
const COMMITS_QUERY: &str = r#"
query($id: ID!, $after: String) {
  node(id: $id) { ... on PullRequest { commits(first: 100, after: $after) { totalCount pageInfo { hasNextPage endCursor } nodes { commit { oid authoredDate committedDate additions deletions
    messageHeadline message author { email name user { login } } } } } } }
  rateLimit { cost remaining }
}
"#;

/// The next page of the checks on one commit.
const CONTEXTS_QUERY: &str = r#"
query($id: ID!, $after: String) {
  node(id: $id) { ... on Commit { statusCheckRollup { state contexts(first: 100, after: $after) { pageInfo { hasNextPage endCursor }
    nodes { __typename ... on CheckRun { name conclusion status } ... on StatusContext { context state } } } } } }
  rateLimit { cost remaining }
}
"#;

/// The fields of one pull request that the records keep: its own fields,
/// the head commit's checks, the changed files and commits (with their
/// messages, for co-author trailers), and the newest `$tl` timeline items.
pub const PR_FRAGMENT: &str = r#"
fragment R on PullRequest {
  id number url title state isDraft createdAt updatedAt closedAt mergedAt
  author { __typename login } authorAssociation mergedBy { login }
  maintainerCanModify isCrossRepository headRefName headRefOid baseRefName baseRefOid
  mergeable mergeStateStatus body additions deletions changedFiles
  mergeCommit { oid messageHeadline parents(first: 2) { totalCount } committer { name user { login } } }
  assignees(first: 3) { nodes { login } }
  closingIssuesReferences(first: 5) { nodes { number title repository { nameWithOwner } } }
  files(first: 100) { totalCount pageInfo { hasNextPage endCursor } nodes { path additions deletions } }
  commits(first: 100) { totalCount pageInfo { hasNextPage endCursor } nodes { commit { oid authoredDate committedDate additions deletions
    messageHeadline message author { email name user { login } } } } }
  head: commits(last: 1) { nodes { commit { id oid
    statusCheckRollup { state contexts(first: 100) { pageInfo { hasNextPage endCursor } nodes { __typename ... on CheckRun { name conclusion status } ... on StatusContext { context state } } } }
    checkSuites(first: 8) { totalCount nodes { status conclusion createdAt app { slug } } } } } }
  timelineItems(last: $tl, itemTypes: [ISSUE_COMMENT, PULL_REQUEST_REVIEW, HEAD_REF_FORCE_PUSHED_EVENT,
      CONVERT_TO_DRAFT_EVENT, READY_FOR_REVIEW_EVENT, CROSS_REFERENCED_EVENT, ASSIGNED_EVENT, CLOSED_EVENT, MERGED_EVENT, REOPENED_EVENT]) {
    totalCount
    nodes { __typename
      ... on IssueComment { id createdAt author { __typename login } authorAssociation body url }
      ... on PullRequestReview { id submittedAt state author { __typename login } authorAssociation body url commit { oid } }
      ... on HeadRefForcePushedEvent { id createdAt actor { __typename login } }
      ... on ConvertToDraftEvent { id createdAt actor { __typename login } }
      ... on ReadyForReviewEvent { id createdAt actor { __typename login } }
      ... on CrossReferencedEvent { id createdAt actor { __typename login } willCloseTarget
        source { __typename
          ... on PullRequest { number prState: state title mergedAt author { login } repository { nameWithOwner } }
          ... on Issue { number issueState: state repository { nameWithOwner } } } }
      ... on AssignedEvent { id createdAt actor { __typename login } assignee { ... on User { login } } }
      ... on ClosedEvent { id createdAt actor { __typename login } stateReason closer { __typename ... on PullRequest { number } ... on Commit { oid } } }
      ... on MergedEvent { id createdAt actor { __typename login } }
      ... on ReopenedEvent { id createdAt actor { __typename login } } } }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{ok_json, stub};
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    /// A pull request node with the fields the tests look at.
    fn node(number: u64, state: &str, updated: &str) -> Value {
        json!({
            "id": format!("PR_{number}"), "number": number, "state": state, "updatedAt": updated,
            "createdAt": "2026-10-01T00:00:00Z", "author": {"__typename": "User", "login": "ann"},
            "authorAssociation": "CONTRIBUTOR", "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
            "timelineItems": {"totalCount": 0, "nodes": []},
            "files": {"totalCount": 0, "nodes": []}, "commits": {"totalCount": 0, "nodes": []}
        })
    }

    /// A page of the changed list or the merged walk; the list holds 40
    /// more pull requests than the page.
    fn page(nodes: Vec<Value>, next: Option<&str>, remaining: u64) -> String {
        ok_json(
            &json!({"data": {
                "repository": {"pullRequests": {
                    "totalCount": nodes.len() + 40,
                    "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next},
                    "nodes": nodes
                }},
                "rateLimit": {"cost": 2, "remaining": remaining}
            }})
            .to_string(),
        )
    }

    fn pass(gh: &GitHub, now: DateTime<Utc>) -> Pass<'_> {
        Pass::new(gh, now, 1_500, RUN_REQUESTS)
    }

    #[test]
    fn a_pass_dates_what_it_reads_by_the_time_since_it_started() {
        let gh = GitHub::with_api("t", "http://127.0.0.1:1").unwrap();
        let now: DateTime<Utc> = "2026-10-17T00:00:00Z".parse().unwrap();
        let p = pass(&gh, now);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let at = p.clock();
        assert!(
            at >= now + chrono::Duration::milliseconds(30)
                && at < now + chrono::Duration::seconds(30),
            "{at}"
        );
    }

    #[test]
    fn the_sweep_keeps_how_many_check_suites_the_head_has() {
        let node = json!({"id": "PR_1", "number": 1, "commits": {"nodes": [{"commit": {
            "statusCheckRollup": {"state": "FAILURE"},
            "checkSuites": {"totalCount": 9, "nodes": [
                {"status": "COMPLETED", "conclusion": "FAILURE", "createdAt": "2026-10-17T11:00:00Z",
                 "app": {"slug": "github-actions"}}]}}}]}});
        let light = parse_light(&node).unwrap();
        assert_eq!((light.suites.len(), light.suites_total), (1, Some(9)));
    }

    #[test]
    fn mains_passing_checks_are_kept_only_when_one_page_holds_every_check() {
        let target = |more: bool| {
            json!({"oid": "m", "statusCheckRollup": {"state": "FAILURE", "contexts": {
                "pageInfo": {"hasNextPage": more},
                "nodes": [{"__typename": "CheckRun", "name": "test", "conclusion": "SUCCESS"},
                          {"__typename": "CheckRun", "name": "lint", "conclusion": "FAILURE"}]}}})
        };
        let all = parse_main(&target(false)).unwrap();
        assert_eq!(
            (all.passing, all.failing),
            (vec!["test".to_string()], vec!["lint".to_string()])
        );
        let part = parse_main(&target(true)).unwrap();
        assert!(
            part.passing.is_empty(),
            "a failing `test` may be on the next page"
        );
        assert_eq!(part.failing, ["lint"]);
    }

    /// The input of a call about `repos` with the app's limits.
    fn input<'a>(
        repos: &'a [String],
        seen: &'a HashMap<String, Option<Seen>>,
        dir: &'a Path,
        now: DateTime<Utc>,
    ) -> PrInput<'a> {
        PrInput {
            repos,
            seen,
            dir,
            now,
            interval_secs: 300,
            remaining: None,
            floor: 1_500,
            max_requests: RUN_REQUESTS,
            viewer: None,
            on_event: None,
        }
    }

    /// What a fetch at 11:55 saw: the newest pull request update at `updated`.
    fn seen_at(updated: &str) -> Seen {
        Seen {
            pr_updated_at: Some(t(updated)),
            read_at: t("2026-10-17T11:55:00Z"),
        }
    }

    #[test]
    fn a_pull_request_node_fills_every_section() {
        let v: Value = serde_json::from_str(
            r#"{
          "id": "PR_kw1", "number": 42, "url": "https://github.com/o/r/pull/42", "title": "Pen: smoother curves",
          "state": "MERGED", "isDraft": false, "createdAt": "2026-10-10T00:00:00Z", "updatedAt": "2026-10-12T00:00:00Z",
          "closedAt": "2026-10-12T00:00:00Z", "mergedAt": "2026-10-12T00:00:00Z",
          "author": {"__typename": "User", "login": "Ann-Example"}, "authorAssociation": "CONTRIBUTOR",
          "mergedBy": {"login": "push-account"}, "maintainerCanModify": true, "isCrossRepository": true,
          "headRefName": "fix/pen", "headRefOid": "abc", "mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN",
          "body": "Fixes #7", "additions": 120, "deletions": 4, "changedFiles": 101,
          "mergeCommit": {"oid": "m1", "messageHeadline": "Merge pull request #42 from ann-example/fix-pen",
                          "parents": {"totalCount": 2},
                          "committer": {"name": "Maint One", "user": {"login": "Maint-One"}}},
          "assignees": {"nodes": [{"login": "maint-two"}]},
          "closingIssuesReferences": {"nodes": [{"number": 7, "title": "Pen jitters", "repository": {"nameWithOwner": "o/r"}}]},
          "files": {"totalCount": 101, "pageInfo": {"hasNextPage": true, "endCursor": "f1"},
                    "nodes": [{"path": "crates/pen/src/lib.rs", "additions": 100, "deletions": 4}]},
          "commits": {"totalCount": 3, "nodes": [
            {"commit": {"oid": "c1", "authoredDate": "2026-10-10T01:00:00Z", "committedDate": "2026-10-11T01:00:00Z",
                        "additions": 100, "deletions": 4, "messageHeadline": "Smooth the pen",
                        "message": "Smooth the pen\n\nCo-authored-by: Bo Sample <7+bo-sample@users.noreply.github.com>\nCo-authored-by: Claude <noreply@anthropic.com>",
                        "author": {"email": "Ann@Example.com", "name": "Ann", "user": {"login": "Ann-Example"}}}},
            {"commit": {"oid": "c2", "authoredDate": "2026-10-11T01:00:00Z", "additions": 900, "deletions": 0,
                        "messageHeadline": "Merge branch 'main' into fix/pen", "message": "",
                        "author": {"email": "ann@example.com", "user": {"login": "Ann-Example"}}}},
            {"commit": {"oid": "c3", "authoredDate": "2026-10-11T02:00:00Z", "additions": 20, "deletions": 0,
                        "messageHeadline": "Revert \"Pen: rough curves\"",
                        "message": "Revert \"Pen: rough curves\"\n\nThis reverts commit 0123456789abcdef.",
                        "author": {"email": "m2@example.com", "user": {"login": "maint-two"}}}}
          ]},
          "head": {"nodes": [{"commit": {"oid": "abc",
            "statusCheckRollup": {"state": "FAILURE", "contexts": {"nodes": [
              {"__typename": "CheckRun", "name": "build", "conclusion": "FAILURE", "status": "COMPLETED"},
              {"__typename": "CheckRun", "name": "lint", "conclusion": "SUCCESS", "status": "COMPLETED"},
              {"__typename": "StatusContext", "context": "ci/legacy", "state": "ERROR"}]}},
            "checkSuites": {"totalCount": 1, "nodes": [{"status": "COMPLETED", "conclusion": "FAILURE",
                                       "createdAt": "2026-10-11T03:00:00Z", "app": {"slug": "github-actions"}}]}}}]},
          "timelineItems": {"totalCount": 4, "nodes": [
            {"__typename": "PullRequestReview", "id": "r1", "submittedAt": "2026-10-11T05:00:00Z", "state": "APPROVED",
             "author": {"__typename": "User", "login": "maint-two"}, "authorAssociation": "MEMBER", "body": "", "commit": {"oid": "abc"}},
            {"__typename": "IssueComment", "id": "ic1", "createdAt": "2026-10-10T05:00:00Z",
             "author": {"__typename": "Bot", "login": "helper"}, "authorAssociation": "NONE", "body": "Thanks"},
            {"__typename": "CrossReferencedEvent", "id": "x1", "createdAt": "2026-10-11T06:00:00Z", "willCloseTarget": false,
             "actor": {"__typename": "User", "login": "bo-sample"},
             "source": {"__typename": "PullRequest", "number": 50, "prState": "OPEN", "title": "Follow-up",
                        "mergedAt": null, "author": {"login": "bo-sample"}, "repository": {"nameWithOwner": "o/r"}}},
            {"__typename": "ClosedEvent", "id": "cl1", "createdAt": "2026-10-12T00:00:00Z", "stateReason": null,
             "actor": {"__typename": "User", "login": "Maint-One"}, "closer": {"__typename": "Commit", "oid": "m1"}}
          ]}
        }"#,
        )
        .unwrap();
        let r = parse_pr(&v).unwrap();
        assert_eq!(
            (r.number, r.state, r.id.as_str()),
            (42, PrState::Merged, "PR_kw1")
        );
        assert_eq!(r.author.as_deref(), Some("Ann-Example"));
        assert_eq!(r.merged_by.as_deref(), Some("push-account"));
        assert_eq!(r.contribution.merger.as_deref(), Some("maint-one"));
        assert_eq!(r.closing_issues, [7]);
        assert_eq!(r.closing_titles, ["Pen jitters"]);
        assert_eq!(r.closing_repos, ["o/r"]);
        assert_eq!(r.assignees, ["maint-two"]);
        let c = &r.contribution;
        assert_eq!(
            (c.additions, c.changed_files, c.files_total, c.commits_total),
            (120, 101, 101, 3)
        );
        assert_eq!(c.files_cursor.as_deref(), Some("f1"));
        let co = &c.commits[0].co_authors;
        assert_eq!(co.len(), 1, "the AI co-author is left out");
        assert_eq!(co[0].login.as_deref(), Some("bo-sample"));
        assert_eq!(c.commits[0].author_email, "ann@example.com");
        assert!(!c.commits[0].is_merge && c.commits[1].is_merge && !c.commits[2].is_merge);
        assert_eq!(c.commits[2].reverts, ["0123456789abcdef"]);
        assert!(c.commits[0].reverts.is_empty());
        assert_eq!(c.merge_commit.as_ref().map(|m| m.parents), Some(2));
        assert_eq!(
            c.merge_commit.as_ref().map(|m| m.headline.as_str()),
            Some("Merge pull request #42 from ann-example/fix-pen")
        );
        assert_eq!(r.checks.rollup.as_deref(), Some("FAILURE"));
        assert_eq!(r.checks.failing, ["build", "ci/legacy"]);
        assert_eq!(
            r.checks.failing_statuses,
            Some(vec!["ci/legacy".to_string()])
        );
        assert_eq!(r.checks.suites[0].app.as_deref(), Some("github-actions"));
        assert_eq!(r.checks.suites_total, Some(1));
        let kinds: Vec<&str> = r.timeline.iter().map(|i| i.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "IssueComment",
                "PullRequestReview",
                "CrossReferencedEvent",
                "ClosedEvent"
            ],
            "oldest first"
        );
        assert!(r.timeline[0].actor_bot);
        assert_eq!(r.timeline[1].state.as_deref(), Some("APPROVED"));
        assert_eq!(r.timeline[1].commit.as_deref(), Some("abc"));
        let source = r.timeline[2].source.as_ref().unwrap();
        assert_eq!((source.number, source.state.as_str()), (50, "OPEN"));
        let closer = r.timeline[3].closer.as_ref();
        assert_eq!(closer.and_then(|c| c.oid.as_deref()), Some("m1"));
        assert!(parse_pr(&Value::Null).is_none());
    }

    #[test]
    fn each_call_sweeps_a_share_of_the_repos_longest_ago_first() {
        let now = t("2026-10-17T12:00:00Z");
        let ago = |m: i64| Some(now - chrono::Duration::minutes(m));
        let mut swept: Vec<(String, Option<DateTime<Utc>>)> =
            (0..15).map(|i| (format!("o/r{i}"), ago(5))).collect();
        swept[3].1 = None;
        swept[7].1 = ago(40);
        swept[9].1 = ago(31);
        swept[11].1 = ago(30);
        // 15 repos every 30 minutes, 5 minutes between calls: 3 a call.
        assert_eq!(sweep_pick(&swept, now, 300), ["o/r3", "o/r7", "o/r9"]);
        // An hour between calls: every due repo goes at once.
        assert_eq!(sweep_pick(&swept, now, 3600).len(), 4);
        assert!(sweep_pick(&[("o/a".into(), ago(1))], now, 300).is_empty());
        assert_eq!(sweep_pick(&[("o/a".into(), None)], now, 60), ["o/a"]);
    }

    #[test]
    fn the_changed_list_reads_down_to_the_last_update_seen() {
        let now = t("2026-10-17T12:00:00Z");
        let mut cache = PrCache {
            high_water: Some(t("2026-10-17T10:00:00Z")),
            ..Default::default()
        };
        // Nothing listens on port 1: no pull request updated since, no request.
        let offline = GitHub::with_api("t", "http://127.0.0.1:1").unwrap();
        pass(&offline, now)
            .changed("o/r", &mut cache, Some(&seen_at("2026-10-17T10:00:00Z")))
            .unwrap();
        assert_eq!(offline.graphql_sent(), 0);
        // A repository without pull requests: no request either.
        let mut empty = PrCache::default();
        let none = Seen {
            pr_updated_at: None,
            read_at: t("2026-10-17T11:50:00Z"),
        };
        pass(&offline, now)
            .changed("o/empty", &mut empty, Some(&none))
            .unwrap();
        assert_eq!(offline.graphql_sent(), 0);
        assert_eq!(empty.checked_at, Some(t("2026-10-17T11:50:00Z")));
        let (api, requests) = stub(vec![
            page(
                vec![
                    node(9, "OPEN", "2026-10-17T11:30:00Z"),
                    node(8, "MERGED", "2026-10-17T11:00:00Z"),
                ],
                Some("c1"),
                4000,
            ),
            // 09:45 is before 10:00 less the overlap: the read stops here.
            page(
                vec![
                    node(7, "CLOSED", "2026-10-17T09:55:00Z"),
                    node(6, "OPEN", "2026-10-17T09:45:00Z"),
                ],
                Some("c2"),
                3990,
            ),
        ]);
        let gh = GitHub::with_api("t", api).unwrap();
        let p = pass(&gh, now);
        p.changed("o/r", &mut cache, Some(&seen_at("2026-10-17T11:30:00Z")))
            .unwrap();
        assert_eq!(gh.graphql_sent(), 2);
        assert_eq!(p.report().changed, 4);
        assert_eq!(p.report().remaining, Some(3990));
        assert_eq!(cache.high_water, Some(t("2026-10-17T11:30:00Z")));
        assert_eq!(cache.gap, None);
        let numbers: Vec<u64> = cache.records.keys().copied().collect();
        assert_eq!(numbers, [6, 7, 8, 9]);
        assert_eq!(cache.records[&8].state, PrState::Merged);
        let wait = Duration::from_secs(10);
        let first = requests.recv_timeout(wait).unwrap();
        let order = "orderby: {field: updated_at, direction: desc}";
        assert!(first.contains(order), "{first}");
        assert!(first.contains("\"after\":null"), "{first}");
        assert!(first.contains("\"tl\":50"), "{first}");
        let second = requests.recv_timeout(wait).unwrap();
        assert!(second.contains("\"after\":\"c1\""), "{second}");
    }

    #[test]
    fn a_first_load_reads_one_page_and_a_long_list_is_finished_next_poll() {
        let now = t("2026-10-17T12:00:00Z");
        let mut cache = PrCache::default();
        let (api, requests) = stub(vec![page(
            vec![node(5, "OPEN", "2026-10-17T11:00:00Z")],
            Some("c1"),
            4000,
        )]);
        let gh = GitHub::with_api("t", api).unwrap();
        pass(&gh, now)
            .changed("o/r", &mut cache, Some(&seen_at("2026-10-17T11:00:00Z")))
            .unwrap();
        // The sweep and the backfill find the rest.
        assert_eq!(gh.graphql_sent(), 1);
        assert_eq!(cache.high_water, Some(t("2026-10-17T11:00:00Z")));
        let head = requests.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(head.contains("\"tl\":100"), "{head}");

        // A burst: more changes than the page limit.
        let pages: Vec<String> = (0..MAX_CHANGED_PAGES)
            .map(|i| {
                let at = format!("2026-10-17T11:{:02}:00Z", 50 - i * 5);
                page(
                    vec![node(100 + i as u64, "OPEN", &at)],
                    Some(&format!("p{i}")),
                    4000,
                )
            })
            .chain([page(
                vec![
                    node(90, "OPEN", "2026-10-17T10:52:00Z"),
                    node(91, "OPEN", "2026-10-17T10:40:00Z"),
                ],
                Some("p9"),
                3900,
            )])
            .collect();
        let (api, requests) = stub(pages);
        let gh = GitHub::with_api("t", api).unwrap();
        assert_eq!(cache.checked_at, Some(now), "the first load read its page");
        let later = now + chrono::Duration::minutes(5);
        pass(&gh, later)
            .changed("o/r", &mut cache, Some(&seen_at("2026-10-17T11:50:00Z")))
            .unwrap();
        assert_eq!(gh.graphql_sent(), MAX_CHANGED_PAGES as u64);
        assert_eq!(cache.high_water, Some(t("2026-10-17T11:50:00Z")));
        let gap = cache.gap.clone().unwrap();
        assert_eq!(gap.cursor, format!("p{}", MAX_CHANGED_PAGES - 1));
        assert_eq!(gap.until, Some(t("2026-10-17T10:50:00Z")));
        assert_eq!(cache.checked_at, Some(now), "part of the list is unread");
        // The next fetch, at 12:08, saw nothing new, but the gap is read.
        let next = now + chrono::Duration::minutes(10);
        let fetched = Seen {
            pr_updated_at: Some(t("2026-10-17T11:50:00Z")),
            read_at: t("2026-10-17T12:08:00Z"),
        };
        pass(&gh, next)
            .changed("o/r", &mut cache, Some(&fetched))
            .unwrap();
        assert_eq!(gh.graphql_sent(), MAX_CHANGED_PAGES as u64 + 1);
        assert_eq!(cache.gap, None, "10:40 is past the gap's end");
        assert_eq!(
            cache.checked_at,
            Some(t("2026-10-17T12:08:00Z")),
            "up to date as of the fetch that saw nothing new"
        );
        assert!(cache.records.contains_key(&91));
        let last = requests.iter().take(MAX_CHANGED_PAGES + 1).last().unwrap();
        let cursor = format!("\"after\":\"p{}\"", MAX_CHANGED_PAGES - 1);
        assert!(last.contains(&cursor), "{last}");
    }

    #[test]
    fn a_sweep_record_first_read_by_the_changed_list_is_marked_for_the_backfill() {
        let now = t("2026-10-17T12:00:00Z");
        let mut cache = PrCache {
            high_water: Some(t("2026-10-17T10:00:00Z")),
            ..Default::default()
        };
        let stub_of = |number: u64| Light {
            id: format!("PR_{number}"),
            number,
            ..Default::default()
        };
        cache
            .records
            .insert(5, PrRecord::from_light(&stub_of(5), now));
        // #4 was fetched before; its full page of items is the usual read.
        let held = parse_pr(&node(4, "OPEN", "2026-10-17T09:00:00Z")).unwrap();
        cache.put(held, now);
        let full = |number: u64, updated: &str| {
            let mut n = node(number, "OPEN", updated);
            let items: Vec<Value> = (0..TL_CHANGED)
                .map(|i| json!({"__typename": "IssueComment", "id": format!("c{number}-{i}"),
                                "createdAt": "2026-10-16T00:00:00Z", "author": {"__typename": "User", "login": "ann"},
                                "authorAssociation": "NONE", "body": "hi"}))
                .collect();
            n["timelineItems"] = json!({"totalCount": 120, "nodes": items});
            n
        };
        let (api, _requests) = stub(vec![page(
            vec![
                full(5, "2026-10-17T11:30:00Z"),
                full(4, "2026-10-17T11:20:00Z"),
            ],
            None,
            4000,
        )]);
        let gh = GitHub::with_api("t", api).unwrap();
        pass(&gh, now)
            .changed("o/r", &mut cache, Some(&seen_at("2026-10-17T11:30:00Z")))
            .unwrap();
        let five = &cache.records[&5];
        assert_eq!(five.timeline.len() as u64, TL_CHANGED);
        assert!(
            five.needs_fetch(),
            "never fetched before: the backfill reads its older items"
        );
        assert!(!cache.records[&4].needs_fetch());
    }

    #[test]
    fn a_changed_list_page_that_misses_the_kept_items_is_read_again() {
        let now = t("2026-10-17T12:00:00Z");
        let mut cache = PrCache {
            high_water: Some(t("2026-10-17T10:00:00Z")),
            ..Default::default()
        };
        let start = t("2026-10-16T00:00:00Z");
        let items = |number: u64, from: usize, to: usize| -> Vec<Value> {
            (from..to)
                .map(|i| {
                    let at = (start + chrono::Duration::minutes(i as i64)).to_rfc3339();
                    json!({"__typename": "IssueComment", "id": format!("c{number}-{i}"), "createdAt": at,
                           "author": {"__typename": "User", "login": "ann"}, "authorAssociation": "NONE", "body": "hi"})
                })
                .collect()
        };
        let read = |number: u64, total: usize, updated: &str| {
            let mut n = node(number, "OPEN", updated);
            n["timelineItems"] = json!({"totalCount": total, "nodes": items(number, total.saturating_sub(50), total)});
            parse_pr(&n).unwrap()
        };
        // #4 and #6 were read with their first 10 items.
        cache.put(read(4, 10, "2026-10-17T09:00:00Z"), now);
        cache.put(read(6, 10, "2026-10-17T09:00:00Z"), now);
        // Since then #4 got 70 more items and #6 got 50; the changed list
        // reads the newest 50 of each.
        let (api, _requests) = stub(vec![page(
            vec![
                {
                    let mut n = node(4, "OPEN", "2026-10-17T11:30:00Z");
                    n["timelineItems"] = json!({"totalCount": 80, "nodes": items(4, 30, 80)});
                    n
                },
                {
                    let mut n = node(6, "OPEN", "2026-10-17T11:20:00Z");
                    n["timelineItems"] = json!({"totalCount": 60, "nodes": items(6, 10, 60)});
                    n
                },
            ],
            None,
            4000,
        )]);
        let gh = GitHub::with_api("t", api).unwrap();
        pass(&gh, now)
            .changed("o/r", &mut cache, Some(&seen_at("2026-10-17T11:30:00Z")))
            .unwrap();
        let four = &cache.records[&4];
        assert!(four.needs_fetch(), "items 10 to 29 were missed");
        assert_eq!(
            four.timeline.first().map(|i| i.id.as_str()),
            Some("c4-30"),
            "the items before the gap go"
        );
        let six = &cache.records[&6];
        assert!(
            !six.needs_fetch(),
            "the page starts right after the kept items"
        );
        assert_eq!(six.timeline.len(), 60);
        // Before the backfill reads #4, the changed list reads its newest 50
        // again, which now overlap the items it holds: it stays marked.
        let (api, _requests) = stub(vec![page(
            vec![{
                let mut n = node(4, "OPEN", "2026-10-17T11:40:00Z");
                n["timelineItems"] = json!({"totalCount": 85, "nodes": items(4, 35, 85)});
                n
            }],
            None,
            4000,
        )]);
        let gh = GitHub::with_api("t", api).unwrap();
        pass(&gh, now)
            .changed("o/r", &mut cache, Some(&seen_at("2026-10-17T11:40:00Z")))
            .unwrap();
        let four = &cache.records[&4];
        assert_eq!(four.timeline.len(), 55);
        assert!(four.needs_fetch(), "still waiting for the backfill");
    }

    fn sweep_page(open: Vec<Value>) -> String {
        ok_json(
            &json!({"data": {
                "repository": {
                    "defaultBranchRef": {"target": {"oid": "main1", "statusCheckRollup": {"state": "FAILURE",
                        "contexts": {"nodes": [{"__typename": "CheckRun", "name": "test", "conclusion": "FAILURE"},
                                               {"__typename": "CheckRun", "name": "lint", "conclusion": "SUCCESS"},
                                               {"__typename": "StatusContext", "context": "ci/legacy", "state": "SUCCESS"},
                                               {"__typename": "CheckRun", "name": "docs", "conclusion": "SKIPPED"}]}}}},
                    "pullRequests": {"totalCount": open.len(), "pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": open}
                },
                "rateLimit": {"cost": 1, "remaining": 3800}
            }})
            .to_string(),
        )
    }

    fn light(number: u64, updated: &str, comments: u64) -> Value {
        json!({"id": format!("PR_{number}"), "number": number, "updatedAt": updated, "isDraft": false,
               "headRefOid": "h", "mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY", "maintainerCanModify": true,
               "comments": {"totalCount": comments}, "reviews": {"totalCount": 0},
               "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "PENDING"},
                   "checkSuites": {"nodes": [{"status": "QUEUED", "conclusion": null, "createdAt": "2026-10-17T11:00:00Z", "app": {"slug": "github-actions"}}]}}}]}})
    }

    #[test]
    fn a_full_pass_fills_an_empty_cache_and_the_next_one_sends_nothing() {
        let dir = std::env::temp_dir().join(format!("craft-prs-pass-{}", std::process::id()));
        let now = t("2026-10-17T12:00:00Z");
        let repos = vec!["o/r".to_string()];
        let mut seen = HashMap::new();
        seen.insert("o/r".to_string(), Some(seen_at("2026-10-17T11:00:00Z")));
        let missing = json!([{"type": "NOT_FOUND", "path": ["nodes", 0],
                              "message": "Could not resolve to a node with the global id of 'PR_5'"}]);
        let nodes = |list: Vec<Value>| {
            ok_json(
                &json!({"data": {"nodes": list, "rateLimit": {"cost": 1, "remaining": 3700}},
                        "errors": missing})
                .to_string(),
            )
        };
        let (api, requests) = stub(vec![
            // The changed list's first page.
            page(vec![node(3, "OPEN", "2026-10-17T11:00:00Z")], None, 4000),
            // The sweep: #3 again, #5 and #6 new; #6 has comments.
            sweep_page(vec![
                light(3, "2026-10-17T11:00:00Z", 0),
                light(5, "2026-10-17T10:00:00Z", 0),
                light(6, "2026-10-17T09:00:00Z", 2),
            ]),
            // The backfill, newest update first; #5 was deleted meanwhile.
            nodes(vec![Value::Null, node(6, "OPEN", "2026-10-17T09:00:00Z")]),
            // The merged walk, in one page.
            page(
                vec![
                    node(1, "MERGED", "2026-10-16T00:00:00Z"),
                    node(2, "MERGED", "2026-10-15T00:00:00Z"),
                ],
                None,
                3600,
            ),
            // The closed walk, which finds #7 closed before the first poll.
            page(vec![node(7, "CLOSED", "2026-10-14T00:00:00Z")], None, 3590),
        ]);
        let gh = GitHub::with_api("t", api).unwrap();
        let input = PrInput {
            remaining: Some(4100),
            ..input(&repos, &seen, &dir, now)
        };
        let (report, held) = gh.update_prs(&input);
        assert_eq!(report.errors, Vec::<String>::new());
        assert_eq!(gh.graphql_sent(), 5);
        let r = &report;
        assert_eq!(
            (r.changed, r.swept, r.backfilled, r.merged, r.closed),
            (1, 1, 1, 2, 1)
        );
        assert_eq!((r.records, r.pending, r.walks_done), (5, 0, 1));
        assert_eq!(report.remaining, Some(3590));
        assert!(!report.low_budget && !report.halted);
        let wait = Duration::from_secs(10);
        let bodies: Vec<String> = (0..5)
            .map(|_| requests.recv_timeout(wait).unwrap())
            .collect();
        let ids = "\"ids\":[\"pr_5\",\"pr_6\"]";
        assert!(bodies[2].contains(ids), "{}", bodies[2]);
        assert!(bodies[3].contains("states: merged"), "{}", bodies[3]);
        assert!(bodies[4].contains("states: closed"), "{}", bodies[4]);
        let cache = PrCache::load(&dir, "o/r");
        assert_eq!(held, vec![("o/r".to_string(), cache.clone())]);
        let numbers: Vec<u64> = cache.records.keys().copied().collect();
        assert_eq!(numbers, [1, 2, 3, 6, 7]);
        assert_eq!(cache.records[&7].state, PrState::Closed);
        assert!(cache.closed_walk.done);
        assert_eq!(cache.checked_at, Some(now), "the changed list was read");
        // The sweep's value.
        assert_eq!(cache.records[&3].checks.mergeable, "CONFLICTING");
        assert_eq!(cache.records[&3].checks.conflict_since, Some(now));
        assert_eq!(cache.records[&6].comments_total, 2);
        assert_eq!(cache.swept_at, Some(now));
        assert!(cache.merged_walk.done);
        assert_eq!(
            cache.merged_walk.total,
            Some(42),
            "GitHub's count of merged pull requests"
        );
        let main = cache.main_checks.clone().unwrap();
        assert_eq!(main.oid, "main1");
        assert_eq!(main.failing, ["test"]);
        assert_eq!(
            main.passing,
            ["lint", "ci/legacy"],
            "a skipped check is neither"
        );
        let seen = cache.main_history.last().unwrap();
        assert_eq!(seen.oid, "main1", "the sweep keeps the head in the history");
        // The sweep answers after the changed list's request, so the pass's
        // clock has moved past `now` by then.
        assert!(
            seen.first_seen.is_some_and(|at| at > now),
            "dated by the pass's clock: {:?}",
            seen.first_seen
        );

        // Nothing changed, the sweep is not due and the walk is done.
        let offline = GitHub::with_api("t", "http://127.0.0.1:1").unwrap();
        let mut later_seen = HashMap::new();
        let fetched = Seen {
            pr_updated_at: Some(t("2026-10-17T11:00:00Z")),
            read_at: t("2026-10-17T12:04:00Z"),
        };
        later_seen.insert("o/r".to_string(), Some(fetched));
        let (again, _) = offline.update_prs(&PrInput {
            now: now + chrono::Duration::minutes(5),
            seen: &later_seen,
            ..input
        });
        assert_eq!(offline.graphql_sent(), 0);
        assert_eq!(again.errors, Vec::<String>::new());
        let later = PrCache::load(&dir, "o/r");
        assert_eq!(
            later.checked_at,
            Some(t("2026-10-17T12:04:00Z")),
            "a fetch that saw nothing new is a check, as of that fetch"
        );
        assert_eq!(
            PrCache {
                checked_at: cache.checked_at,
                ..later
            },
            cache
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn below_the_floor_only_the_changed_list_runs() {
        let dir = std::env::temp_dir().join(format!("craft-prs-low-{}", std::process::id()));
        let now = t("2026-10-17T12:00:00Z");
        let repos = vec!["o/r".to_string(), "o/hidden".to_string()];
        let mut seen = HashMap::new();
        seen.insert("o/r".to_string(), Some(seen_at("2026-10-17T11:00:00Z")));
        seen.insert("o/hidden".to_string(), None);
        // The answer reports 1,400 points left.
        let (api, _requests) = stub(vec![page(
            vec![node(3, "OPEN", "2026-10-17T11:00:00Z")],
            None,
            1_400,
        )]);
        let gh = GitHub::with_api("t", api).unwrap();
        let (report, held) = gh.update_prs(&input(&repos, &seen, &dir, now));
        assert_eq!(held.len(), 1, "the hidden repo is left out");
        assert_eq!(gh.graphql_sent(), 1, "no sweep, backfill or walk");
        assert!(report.low_budget);
        assert_eq!((report.changed, report.swept, report.merged), (1, 0, 0));
        // A hidden repo is not asked about.
        assert!(!PrCache::path(&dir, "o/hidden").exists());
        assert!(PrCache::load(&dir, "o/r").records.contains_key(&3));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_rate_limit_ends_the_pass_and_keeps_what_was_read() {
        let dir = std::env::temp_dir().join(format!("craft-prs-halt-{}", std::process::id()));
        let now = t("2026-10-17T12:00:00Z");
        let repos = vec!["o/a".to_string(), "o/b".to_string()];
        let limited = r#"{"message": "You have exceeded a secondary rate limit."}"#;
        let secondary = format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nretry-after: 60\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{limited}",
            limited.len()
        );
        let (api, _requests) = stub(vec![
            page(vec![node(1, "OPEN", "2026-10-17T11:00:00Z")], None, 4000),
            secondary,
        ]);
        let gh = GitHub::with_api("t", api).unwrap();
        let (report, _) = gh.update_prs(&input(&repos, &HashMap::new(), &dir, now));
        assert!(report.halted, "{report:?}");
        assert_eq!(gh.graphql_sent(), 2);
        assert!(PrCache::load(&dir, "o/a").records.contains_key(&1));
        assert_eq!(PrCache::load(&dir, "o/a").checked_at, Some(now));
        assert_eq!(
            PrCache::load(&dir, "o/b").checked_at,
            None,
            "its changed list was not read"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_lists_past_100_are_paged() {
        let now = t("2026-10-17T12:00:00Z");
        let mut rec = parse_pr(&node(4, "MERGED", "2026-10-16T00:00:00Z")).unwrap();
        rec.contribution.files_total = 3;
        rec.contribution.files = vec![FileLines {
            path: "a".into(),
            ..Default::default()
        }];
        rec.contribution.files_cursor = Some("f1".into());
        let mut cache = PrCache::default();
        cache.put(rec, now);
        let files = |paths: &[&str], next: Option<&str>| {
            let nodes: Vec<Value> = paths
                .iter()
                .map(|p| json!({"path": p, "additions": 1, "deletions": 0}))
                .collect();
            ok_json(
                &json!({"data": {"node": {"files": {"totalCount": 3,
                    "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next}, "nodes": nodes}},
                    "rateLimit": {"cost": 1, "remaining": 4000}}})
                .to_string(),
            )
        };
        let (api, requests) = stub(vec![files(&["b"], Some("f2")), files(&["c"], None)]);
        let gh = GitHub::with_api("t", api).unwrap();
        let mut caches = [("o/r".to_string(), cache)];
        let p = pass(&gh, now);
        p.file_pages(&mut caches[0].1).unwrap();
        let c = &caches[0].1.records[&4].contribution;
        let paths: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a", "b", "c"]);
        assert_eq!(c.files_cursor, None);
        assert_eq!(p.report().file_pages, 2);
        assert_eq!(p.left.load(Ordering::Relaxed), RUN_REQUESTS - 2);
        let wait = Duration::from_secs(10);
        let first = requests.recv_timeout(wait).unwrap();
        assert!(first.contains("\"after\":\"f1\""), "{first}");
        let second = requests.recv_timeout(wait).unwrap();
        assert!(second.contains("\"after\":\"f2\""), "{second}");
        // A later fetch of the same head keeps the pages read.
        let mut again = parse_pr(&node(4, "MERGED", "2026-10-16T00:00:00Z")).unwrap();
        again.contribution.files_total = 3;
        again.contribution.files = vec![FileLines {
            path: "a".into(),
            ..Default::default()
        }];
        again.contribution.files_cursor = Some("f1".into());
        caches[0].1.put(again, now);
        assert_eq!(caches[0].1.records[&4].contribution.files.len(), 3);
        assert_eq!(caches[0].1.records[&4].contribution.files_cursor, None);
    }

    /// The number after `key` in `request`: a `first:` limit, or where one
    /// of these tests' cursors points.
    fn number_after(request: &str, key: &str) -> usize {
        let at = request.find(key).map_or(request.len(), |i| i + key.len());
        let digits: String = request[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().unwrap_or(0)
    }

    /// A GraphQL answer with `data`.
    fn answer(mut data: Value) -> String {
        data["rateLimit"] = json!({"cost": 1, "remaining": 4000});
        ok_json(&json!({ "data": data }).to_string())
    }

    /// Checks `from..` of a commit with `total`, as many as `first` asks
    /// for. Those in `failing` fail; the others pass.
    fn contexts(total: usize, failing: &[(usize, &str)], from: usize, first: usize) -> Value {
        let end = total.min(from + first);
        let nodes: Vec<Value> = (from..end)
            .map(|i| {
                let (name, conclusion) = match failing.iter().find(|(at, _)| *at == i) {
                    Some((_, name)) => (name.to_string(), "FAILURE"),
                    None => (format!("check-{i}"), "SUCCESS"),
                };
                json!({"__typename": "CheckRun", "name": name, "conclusion": conclusion, "status": "COMPLETED"})
            })
            .collect();
        json!({"pageInfo": {"hasNextPage": end < total, "endCursor": format!("x{end}")}, "nodes": nodes})
    }

    /// Commits `from..` of a pull request with `total`, as many as `first`
    /// asks for, by ann, one a minute from 2026-10-16 00:00.
    fn commits(total: usize, from: usize, first: usize) -> Value {
        let end = total.min(from + first);
        let start = t("2026-10-16T00:00:00Z");
        let nodes: Vec<Value> = (from..end)
            .map(|i| {
                let at = (start + chrono::Duration::minutes(i as i64)).to_rfc3339();
                json!({"commit": {"oid": format!("k{i}"), "authoredDate": at, "committedDate": at,
                    "additions": 1, "deletions": 0, "messageHeadline": format!("Step {i}"), "message": format!("Step {i}"),
                    "author": {"email": "ann@example.com", "name": "Ann", "user": {"login": "ann"}}}})
            })
            .collect();
        json!({"totalCount": total, "pageInfo": {"hasNextPage": end < total, "endCursor": format!("k{end}")}, "nodes": nodes})
    }

    /// #9, open, with 130 commits and 130 checks on its head, of which
    /// `flaky` (the 4th) and `e2e` (the 121st) fail; the first page of each
    /// holds as many as `request` asks for.
    fn long_pr(request: &str) -> Value {
        let mut n = node(9, "OPEN", "2026-10-17T11:30:00Z");
        n["headRefOid"] = json!("k129");
        n["commits"] = commits(130, 0, number_after(request, "commits(first: "));
        let first = number_after(request, "contexts(first: ");
        let checks = contexts(130, &LONG_FAILING, 0, first);
        n["head"] = json!({"nodes": [{"commit": {"id": "C_h9", "oid": "k129",
            "statusCheckRollup": {"state": "FAILURE", "contexts": checks}}}]});
        n
    }

    const LONG_FAILING: [(usize, &str); 2] = [(3, "flaky"), (120, "e2e")];

    #[test]
    fn commit_and_check_lists_past_the_first_page_are_read() {
        let dir = std::env::temp_dir().join(format!("craft-prs-lists-{}", std::process::id()));
        let now = t("2026-10-17T12:00:00Z");
        let repos = vec!["o/r".to_string()];
        let mut seen = HashMap::new();
        seen.insert("o/r".to_string(), Some(seen_at("2026-10-17T11:30:00Z")));
        backlog(0, now).save(&dir, "o/r").unwrap();
        let api = crate::tests::routed_stub(|request| {
            let data = if request.contains("commits(first: 100, after:") {
                let from = number_after(request, "\"after\":\"k");
                json!({"node": {"commits": commits(130, from, 100)}})
            } else if request.contains("contexts(first: 100, after:") {
                let from = number_after(request, "\"after\":\"x");
                json!({"node": {"statusCheckRollup": {"contexts": contexts(130, &LONG_FAILING, from, 100)}}})
            } else {
                json!({"repository": {"pullRequests": {"pageInfo": {"hasNextPage": false, "endCursor": null},
                    "nodes": [long_pr(request)]}}})
            };
            Some(answer(data))
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let (report, _) = gh.update_prs(&input(&repos, &seen, &dir, now));
        assert_eq!(report.errors, Vec::<String>::new());
        let cache = PrCache::load(&dir, "o/r");
        let rec = &cache.records[&9];
        assert_eq!(rec.checks.failing, ["flaky", "e2e"]);
        let c = &rec.contribution;
        assert_eq!((c.commits.len(), c.commits_total), (130, 130));
        assert_eq!(
            c.commits.last().map(|k| k.oid.as_str()),
            Some("k129"),
            "the newest push"
        );
        assert_eq!(
            gh.graphql_sent(),
            3,
            "the changed list, a commit page and a check page"
        );
        assert_eq!(report.list_pages, 2);
        assert_eq!(c.commits_cursor, None);
        assert_eq!(rec.checks.contexts_cursor, None);
        // A later fetch of the same head keeps the commit pages read, and
        // its check list is read again.
        let first_pages = long_pr("commits(first: 100) contexts(first: 100)");
        let mut cache = cache.clone();
        let again = parse_pr(&first_pages).unwrap();
        assert_eq!(
            again.checks.contexts_cursor,
            Some(ContextsCursor {
                commit: "C_h9".into(),
                after: "x100".into()
            })
        );
        cache.put(again, now);
        let rec = &cache.records[&9];
        assert_eq!(rec.contribution.commits.len(), 130);
        assert_eq!(rec.contribution.commits_cursor, None);
        assert_eq!(rec.checks.failing, ["flaky"]);
        assert!(rec.checks.contexts_cursor.is_some());
        // After a force-push to a new head with as many commits, which the
        // sweep sees first, the pages are read again.
        let light = Light {
            id: "PR_9".into(),
            number: 9,
            head_ref_oid: "k999".into(),
            ..Default::default()
        };
        cache.records.get_mut(&9).unwrap().apply_light(&light, now);
        let mut pushed = first_pages;
        pushed["headRefOid"] = json!("k999");
        cache.put(parse_pr(&pushed).unwrap(), now);
        let c = &cache.records[&9].contribution;
        assert_eq!(c.commits.len(), 100);
        assert_eq!(c.commits_cursor.as_deref(), Some("k100"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_refresh_keeps_commit_pages_read_part_way_while_the_head_stays() {
        let now = t("2026-10-17T12:00:00Z");
        let fetch = |head: &str| {
            let mut n = node(9, "OPEN", "2026-10-17T11:30:00Z");
            n["headRefOid"] = json!(head);
            n["commits"] = commits(250, 0, 100);
            parse_pr(&n).unwrap()
        };
        let mut cache = PrCache::default();
        cache.put(fetch("k249"), now);
        // A call read one more page before the request cap ran out.
        let rec = cache.records.get_mut(&9).unwrap();
        let page = parse_commits(&commits(250, 100, 100), 0);
        rec.contribution.commits.extend(page);
        rec.contribution.commits_cursor = Some("k200".into());
        // A comment brings a fetch of the same head: the pages stay.
        cache.put(fetch("k249"), now);
        let c = &cache.records[&9].contribution;
        assert_eq!(
            (c.commits.len(), c.commits_cursor.as_deref()),
            (200, Some("k200"))
        );
        // A fetch of a new head starts over, whether the changed list or
        // the sweep sees the head first.
        let mut direct = cache.clone();
        direct.put(fetch("k999"), now);
        let c = &direct.records[&9].contribution;
        assert_eq!(
            (c.commits.len(), c.commits_cursor.as_deref()),
            (100, Some("k100"))
        );
        let light = Light {
            id: "PR_9".into(),
            number: 9,
            head_ref_oid: "k999".into(),
            ..Default::default()
        };
        cache.records.get_mut(&9).unwrap().apply_light(&light, now);
        cache.put(fetch("k999"), now);
        let c = &cache.records[&9].contribution;
        assert_eq!(
            (c.commits.len(), c.commits_cursor.as_deref()),
            (100, Some("k100"))
        );
    }

    #[test]
    fn pages_read_against_another_base_or_another_first_page_are_not_kept() {
        let now = t("2026-10-17T12:00:00Z");
        let fetch = |base: &str, total: usize| {
            let mut n = node(9, "OPEN", "2026-10-17T11:30:00Z");
            n["headRefOid"] = json!(format!("k{}", total - 1));
            n["baseRefName"] = json!(base);
            n["commits"] = commits(total, 0, 100);
            parse_pr(&n).unwrap()
        };
        let read_on = |cache: &mut PrCache| {
            let rec = cache.records.get_mut(&9).unwrap();
            let page = parse_commits(&commits(250, 100, 100), 0);
            rec.contribution.commits.extend(page);
            rec.contribution.commits_cursor = Some("k200".into());
        };
        let pages = |cache: &PrCache| {
            let c = &cache.records[&9].contribution;
            (c.commits.len(), c.commits_cursor.clone())
        };
        let first_page = (100, Some("k100".to_string()));
        // Pages read part way, then the base branch changes.
        let mut cache = PrCache::default();
        cache.put(fetch("main", 250), now);
        read_on(&mut cache);
        cache.put(fetch("main", 250), now);
        assert_eq!(pages(&cache), (200, Some("k200".into())), "the same base");
        cache.put(fetch("dev", 250), now);
        assert_eq!(pages(&cache), first_page, "a new base");
        // A whole list that ends at the head, then a base change.
        let mut whole = PrCache::default();
        whole.put(fetch("main", 150), now);
        let rec = whole.records.get_mut(&9).unwrap();
        rec.contribution
            .commits
            .extend(parse_commits(&commits(150, 100, 100), 0));
        rec.contribution.commits_cursor = None;
        whole.put(fetch("main", 150), now);
        assert_eq!(pages(&whole), (150, None), "the same base");
        whole.put(fetch("dev", 150), now);
        assert_eq!(pages(&whole), first_page, "a new base");
        // The same base and head, but a first page that starts differently.
        let mut whole2 = whole.clone();
        let rec = whole2.records.get_mut(&9).unwrap();
        rec.contribution
            .commits
            .extend(parse_commits(&commits(150, 100, 100), 0));
        rec.contribution.commits_cursor = None;
        let mut moved = fetch("dev", 150);
        moved.contribution.commits[0].oid = "j0".into();
        whole2.put(moved, now);
        let c = &whole2.records[&9].contribution;
        assert_eq!(
            (c.commits.len(), c.commits[0].oid.as_str()),
            (100, "j0"),
            "the new first page"
        );
        // File pages read against one base are not kept for another.
        let with_files = |base: &str, base_oid: &str| {
            let mut n = node(9, "OPEN", "2026-10-17T11:30:00Z");
            n["headRefOid"] = json!("k249");
            n["baseRefName"] = json!(base);
            n["baseRefOid"] = json!(base_oid);
            n["commits"] = commits(250, 0, 100);
            let mut r = parse_pr(&n).unwrap();
            r.contribution.files_total = 2;
            r.contribution.files = vec![FileLines {
                path: "a".into(),
                ..Default::default()
            }];
            r
        };
        let read_second = |files: &mut PrCache| {
            let second = FileLines {
                path: "b".into(),
                ..Default::default()
            };
            files
                .records
                .get_mut(&9)
                .unwrap()
                .contribution
                .files
                .push(second);
        };
        let mut files = PrCache::default();
        files.put(with_files("main", "b1"), now);
        read_second(&mut files);
        files.put(with_files("main", "b1"), now);
        assert_eq!(files.records[&9].contribution.files.len(), 2);
        files.put(with_files("main", "b2"), now);
        assert_eq!(
            files.records[&9].contribution.files.len(),
            1,
            "the base branch moved"
        );
        read_second(&mut files);
        files.put(with_files("dev", "b2"), now);
        assert_eq!(
            files.records[&9].contribution.files.len(),
            1,
            "another base branch"
        );
    }

    /// A sweep answer with 130 checks on main, of which `flaky` (the 111th)
    /// fails, as many in the first page as `request` asks for, and no open
    /// pull request.
    fn long_main_sweep(request: &str) -> Value {
        let first = number_after(request, "contexts(first: ");
        json!({"repository": {
            "defaultBranchRef": {"target": {"id": "C_main", "oid": "main1", "statusCheckRollup": {"state": "FAILURE",
                "contexts": contexts(130, &MAIN_FAILING, 0, first)}}},
            "pullRequests": {"totalCount": 0, "pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}},
            "viewer": {"login": "ann"}})
    }

    const MAIN_FAILING: [(usize, &str); 1] = [(110, "flaky")];

    #[test]
    fn the_checks_on_main_are_read_past_the_first_page() {
        let now = t("2026-10-17T12:00:00Z");
        let api = crate::tests::routed_stub(|request| {
            let data = if request.contains("contexts(first: 100, after:") {
                let from = number_after(request, "\"after\":\"x");
                json!({"node": {"statusCheckRollup": {"contexts": contexts(130, &MAIN_FAILING, from, 100)}}})
            } else {
                long_main_sweep(request)
            };
            Some(answer(data))
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let mut cache = PrCache::default();
        assert!(pass(&gh, now).sweep("o/r", &mut cache).unwrap());
        let main = cache.main_checks.unwrap();
        assert_eq!(main.failing, ["flaky"]);
        assert_eq!(
            main.passing.len(),
            129,
            "every page read: the passing checks are kept"
        );
        assert_eq!(gh.graphql_sent(), 2, "the sweep and a check page");

        // Below the floor no check page is asked for, and the checks read
        // before stay.
        let api = crate::tests::routed_stub(|request| {
            let mut data = long_main_sweep(request);
            data["rateLimit"] = json!({"cost": 1, "remaining": 1_000});
            Some(ok_json(&json!({ "data": data }).to_string()))
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let mut cache = PrCache {
            main_checks: Some(MainChecks::default()),
            ..Default::default()
        };
        let p = pass(&gh, now);
        assert!(!p.sweep("o/r", &mut cache).unwrap());
        assert!(p.report().low_budget);
        assert_eq!(cache.main_checks, Some(MainChecks::default()));
        assert_eq!(gh.graphql_sent(), 1);
    }

    #[test]
    fn a_failed_answer_keeps_the_record_and_the_file_cursor_for_the_next_poll() {
        let now = t("2026-10-17T12:00:00Z");
        let mut cache = PrCache::default();
        let light = Light {
            id: "PR_7".into(),
            number: 7,
            ..Default::default()
        };
        cache.records.insert(7, PrRecord::from_light(&light, now));
        let mut merged = parse_pr(&node(4, "MERGED", "2026-10-16T00:00:00Z")).unwrap();
        merged.contribution.files_cursor = Some("f1".into());
        cache.put(merged, now);
        let failed = |path: Value| {
            ok_json(
                &json!({"data": {"nodes": [null], "node": null, "rateLimit": {"cost": 1, "remaining": 4000}},
                        "errors": [{"type": "INTERNAL", "path": path, "message": "Something went wrong"}]})
                .to_string(),
            )
        };
        let (api, _requests) = stub(vec![failed(json!(["nodes", 0])), failed(json!(["node"]))]);
        let gh = GitHub::with_api("t", api).unwrap();
        let mut caches = vec![("o/r".to_string(), cache)];
        let p = pass(&gh, now);
        p.backfill(&mut caches).unwrap();
        p.file_pages(&mut caches[0].1).unwrap();
        let records = &caches[0].1.records;
        assert!(records[&7].needs_fetch(), "kept, to be fetched again");
        let cursor = records[&4].contribution.files_cursor.as_deref();
        assert_eq!(cursor, Some("f1"));
        let errors = p.report().errors;
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("Something went wrong"));
    }

    #[test]
    fn the_sweep_reads_each_authors_login_and_the_title_and_the_token_users_login() {
        let now = t("2026-10-17T12:00:00Z");
        let mut open = light(4, "2026-10-17T11:00:00Z", 2);
        open["title"] = json!("Ruler: snap to guides");
        open["author"] = json!({"__typename": "User", "login": "Ann-Example"});
        let mut bot = light(5, "2026-10-17T11:00:00Z", 0);
        bot["author"] = json!({"__typename": "Bot", "login": "helper"});
        let mut answer: Value = serde_json::from_str(
            sweep_page(vec![open, bot])
                .split("\r\n\r\n")
                .nth(1)
                .unwrap(),
        )
        .unwrap();
        answer["data"]["viewer"] = json!({"login": "Ann-Example"});
        let (api, requests) = stub(vec![ok_json(&answer.to_string())]);
        let gh = GitHub::with_api("t", api).unwrap();
        let p = pass(&gh, now);
        let mut cache = PrCache::default();
        assert!(p.sweep("o/r", &mut cache).unwrap());
        let four = &cache.records[&4];
        assert_eq!(four.author.as_deref(), Some("Ann-Example"));
        assert_eq!(four.title, "Ruler: snap to guides");
        assert!(four.needs_fetch(), "not read yet");
        assert!(cache.records[&5].author_bot);
        assert_eq!(p.report().viewer.as_deref(), Some("Ann-Example"));
        let sent = requests.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(sent.contains("author { __typename login }"), "{sent}");
        assert!(sent.contains("viewer { login }"), "{sent}");
    }

    #[test]
    fn the_backfill_reads_the_token_users_records_first_then_the_newest() {
        let now = t("2026-10-17T12:00:00Z");
        let mut cache = backlog(3, now);
        let records = [
            (1, "bo-example", "2026-10-17T11:00:00Z"),
            (2, "Ann-Example", "2026-10-10T00:00:00Z"),
            (3, "cy-example", "2026-10-16T00:00:00Z"),
        ];
        for (number, author, updated) in records {
            let r = cache.records.get_mut(&number).unwrap();
            r.author = Some(author.into());
            r.updated_at = Some(t(updated));
        }
        let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
        let api = crate::tests::routed_stub({
            let sent = sent.clone();
            move |request| {
                sent.lock().unwrap().push(request.to_string());
                Some(nodes_answer(request))
            }
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let p = pass(&gh, now);
        p.update(|r| r.viewer = Some("ann-example".into()));
        let mut caches = vec![("o/r".to_string(), cache)];
        p.backfill(&mut caches).unwrap();
        assert_eq!(p.report().backfilled, 3);
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "one batch");
        // The token user's record, then the newest update first.
        let ids = "\"ids\":[\"pr_2\",\"pr_1\",\"pr_3\"]";
        assert!(sent[0].contains(ids), "{}", sent[0]);
    }

    /// A cache whose changed list, sweep and walks have nothing to do this
    /// call, with `n` pull requests the sweep found and the backfill reads.
    fn backlog(n: u64, now: DateTime<Utc>) -> PrCache {
        let mut cache = PrCache {
            high_water: Some(t("2026-10-17T11:00:00Z")),
            swept_at: Some(now),
            ..Default::default()
        };
        cache.merged_walk.done = true;
        cache.closed_walk.done = true;
        for number in 1..=n {
            let light = Light {
                id: format!("PR_{number}"),
                number,
                ..Default::default()
            };
            cache
                .records
                .insert(number, PrRecord::from_light(&light, now));
        }
        cache
    }

    /// An answer to a backfill request: a node for each id it asks for.
    fn nodes_answer(request: &str) -> String {
        let ids = request.split("\"ids\":[").nth(1).unwrap_or("");
        let ids = &ids[..ids.find(']').unwrap_or(0)];
        let nodes: Vec<Value> = ids
            .split(',')
            .filter_map(|id| id.trim_matches('"').strip_prefix("pr_")?.parse().ok())
            .map(|n| node(n, "OPEN", "2026-10-17T10:00:00Z"))
            .collect();
        ok_json(
            &json!({"data": {"nodes": nodes, "rateLimit": {"cost": 1, "remaining": 3800}}})
                .to_string(),
        )
    }

    #[test]
    fn the_cap_spreads_a_first_load_over_calls_that_send_one_request_at_a_time() {
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("craft-prs-cap-{}", std::process::id()));
        let now = t("2026-10-17T12:00:00Z");
        let repos = vec!["o/r".to_string()];
        let mut seen = HashMap::new();
        seen.insert("o/r".to_string(), Some(seen_at("2026-10-17T11:00:00Z")));
        // 80 records to read: four requests of 25, 25, 25 and 5.
        backlog(80, now).save(&dir, "o/r").unwrap();
        let (running, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let api = crate::tests::routed_stub({
            let (running, most) = (running.clone(), most.clone());
            move |request| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(50));
                running.fetch_sub(1, Ordering::SeqCst);
                Some(nodes_answer(request))
            }
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let sent = Mutex::new(Vec::new());
        let on_event = |e: PrEvent<'_>| {
            if let PrEvent::Request { sent: n } = e {
                sent.lock().unwrap().push(n);
            }
        };
        let call = PrInput {
            max_requests: 3,
            on_event: Some(&on_event),
            ..input(&repos, &seen, &dir, now)
        };
        let (first, _) = gh.update_prs(&call);
        assert_eq!(first.errors, Vec::<String>::new());
        assert_eq!((first.requests, first.backfilled), (3, 75));
        assert_eq!((first.records, first.pending), (80, 5));
        assert_eq!(gh.graphql_sent(), 3, "the cap held");
        assert_eq!(most.load(Ordering::SeqCst), 1, "one at a time");
        assert_eq!(*sent.lock().unwrap(), [1, 2, 3]);
        let saved = PrCache::load(&dir, "o/r");
        assert_eq!(
            saved.records.values().filter(|r| r.needs_fetch()).count(),
            5
        );
        // The next call reads the rest.
        let (second, _) = gh.update_prs(&call);
        assert_eq!(
            (second.requests, second.backfilled, second.pending),
            (1, 5, 0)
        );
        assert_eq!(gh.graphql_sent(), 4);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_backfill_keeps_pace_between_the_poll_threads_fetches() {
        use crate::Gate;
        use std::sync::Arc;
        use std::time::Instant;
        // Time runs 100 times faster than in the app: a repo fetch of 10 s
        // takes 100 ms, the 20 s gap after it 200 ms, and a request of 1 s
        // 10 ms.
        let dir = std::env::temp_dir().join(format!("craft-prs-pace-{}", std::process::id()));
        let now = t("2026-10-17T12:00:00Z");
        let repos = vec!["o/r".to_string()];
        let mut seen = HashMap::new();
        seen.insert("o/r".to_string(), Some(seen_at("2026-10-17T11:00:00Z")));
        // 750 records: the 30 requests of 25 one call may send.
        backlog(750, now).save(&dir, "o/r").unwrap();
        let gate = Arc::new(Gate::new());
        let fetching = Arc::new(AtomicBool::new(false));
        let overlaps = Arc::new(AtomicUsize::new(0));
        let api = crate::tests::routed_stub({
            let (fetching, overlaps) = (fetching.clone(), overlaps.clone());
            move |request| {
                if fetching.load(Ordering::SeqCst) {
                    overlaps.fetch_add(1, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(10));
                Some(nodes_answer(request))
            }
        });
        // The poll thread: a fetch, then the gap, until the call ends.
        let stop = Arc::new(AtomicBool::new(false));
        let poll = {
            let (gate, fetching, stop) = (gate.clone(), fetching.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let session = gate.poll_session();
                    fetching.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(100));
                    fetching.store(false, Ordering::SeqCst);
                    drop(session);
                    std::thread::sleep(Duration::from_millis(200));
                }
            })
        };
        let gh = GitHub::with_api("t", api)
            .unwrap()
            .in_background(gate.clone());
        let started = Instant::now();
        let (report, _) = gh.update_prs(&input(&repos, &seen, &dir, now));
        let took = started.elapsed();
        stop.store(true, Ordering::SeqCst);
        poll.join().unwrap();
        assert_eq!(report.errors, Vec::<String>::new());
        assert_eq!((report.backfilled, report.pending), (750, 0));
        assert_eq!(
            overlaps.load(Ordering::SeqCst),
            0,
            "no request went during a fetch"
        );
        // 30 requests of 10 ms, each followed by a pause as long, need 0.6 s
        // of the two thirds of the time the fetches leave: about 0.9 s, or
        // 1.5 minutes in the app. The bound allows a little over three times
        // that, 5 minutes in the app.
        assert!(took < Duration::from_millis(3_000), "{took:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_query_github_could_not_finish_is_asked_for_in_smaller_parts() {
        let now = t("2026-10-17T12:00:00Z");
        let bad_gateway = "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 11\r\nConnection: close\r\n\r\nBad Gateway"
            .to_string();
        // A walk page of 25, then of 10; a backfill batch of 25, then its
        // halves.
        let api = crate::tests::routed_stub(move |request| {
            Some(
                match request.contains("\"first\":25")
                    || request.contains("pr_1\"") && request.contains("pr_25\"")
                {
                    true => bad_gateway.clone(),
                    false if request.contains("\"ids\"") => nodes_answer(request),
                    false => page(vec![node(9, "MERGED", "2026-10-16T00:00:00Z")], None, 3900),
                },
            )
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let p = pass(&gh, now);
        let mut cache = PrCache::default();
        p.walk("o/r", &mut cache, false).unwrap();
        assert!(cache.records.contains_key(&9) && cache.merged_walk.done);
        let mut caches = vec![("o/r".to_string(), backlog(25, now))];
        p.backfill(&mut caches).unwrap();
        let report = p.report();
        assert_eq!(report.errors, Vec::<String>::new());
        assert_eq!((report.retries, report.backfilled), (2, 25));
        // 2 for the walk and 3 for the backfill.
        assert_eq!(report.requests, 5);
        assert!(caches[0].1.records.values().all(|r| !r.needs_fetch()));
    }

    #[test]
    fn a_page_with_a_pull_request_github_could_not_read_is_not_moved_past() {
        let now = t("2026-10-17T12:00:00Z");
        let partial = ok_json(
            &json!({
                "data": {
                    "repository": {"pullRequests": {
                        "totalCount": 42, "pageInfo": {"hasNextPage": false, "endCursor": "c9"},
                        "nodes": [node(9, "MERGED", "2026-10-16T00:00:00Z"), null]
                    }},
                    "rateLimit": {"cost": 1, "remaining": 3900}
                },
                "errors": [{"type": "INTERNAL", "path": ["repository", "pullRequests", "nodes", 1, "files"],
                            "message": "Something went wrong"}]
            })
            .to_string(),
        );
        let (api, _requests) = stub(vec![partial.clone(), partial]);
        let gh = GitHub::with_api("t", api).unwrap();
        let p = pass(&gh, now);
        let mut cache = PrCache::default();
        cache.closed_walk.done = true;
        assert!(p.walk("o/r", &mut cache, false).is_err());
        assert!(
            !cache.merged_walk.done && cache.merged_walk.cursor.is_none(),
            "read again next call"
        );
        assert!(cache.records.is_empty());
        let mut cache = PrCache {
            high_water: Some(t("2026-10-15T00:00:00Z")),
            ..PrCache::default()
        };
        assert!(p.changed("o/r", &mut cache, None).is_err());
        assert_eq!(
            cache.high_water,
            Some(t("2026-10-15T00:00:00Z")),
            "the high-water mark stays"
        );
        assert_eq!(cache.checked_at, None);
    }

    #[test]
    fn a_file_page_with_an_error_keeps_its_cursor() {
        let now = t("2026-10-17T12:00:00Z");
        let mut rec = parse_pr(&node(4, "MERGED", "2026-10-16T00:00:00Z")).unwrap();
        rec.contribution.files_cursor = Some("f1".into());
        let mut cache = PrCache::default();
        cache.put(rec, now);
        let partial = ok_json(
            &json!({
                "data": {"node": {"files": {"totalCount": 3, "pageInfo": {"hasNextPage": false, "endCursor": "f2"},
                                            "nodes": [{"path": "b", "additions": 1, "deletions": 0}, null]}},
                         "rateLimit": {"cost": 1, "remaining": 4000}},
                "errors": [{"type": "INTERNAL", "path": ["node", "files", "nodes", 1], "message": "Something went wrong"}]
            })
            .to_string(),
        );
        let (api, _requests) = stub(vec![partial]);
        let gh = GitHub::with_api("t", api).unwrap();
        let p = pass(&gh, now);
        p.file_pages(&mut cache).unwrap();
        let c = &cache.records[&4].contribution;
        assert_eq!(
            c.files_cursor.as_deref(),
            Some("f1"),
            "read again next call"
        );
        assert!(c.files.is_empty(), "the partial page is not kept");
        assert!(p.report().errors[0].contains("Something went wrong"));
    }

    #[test]
    fn a_backfilled_record_with_a_field_github_could_not_read_stays_pending() {
        let now = t("2026-10-17T12:00:00Z");
        let answer = ok_json(
            &json!({
                "data": {"nodes": [node(1, "OPEN", "2026-10-17T10:00:00Z"), node(2, "OPEN", "2026-10-17T09:00:00Z")],
                         "rateLimit": {"cost": 1, "remaining": 3800}},
                "errors": [{"type": "INTERNAL", "path": ["nodes", 0, "files"], "message": "Something went wrong"}]
            })
            .to_string(),
        );
        let (api, _requests) = stub(vec![answer]);
        let gh = GitHub::with_api("t", api).unwrap();
        let p = pass(&gh, now);
        let mut caches = vec![("o/r".to_string(), backlog(2, now))];
        p.backfill(&mut caches).unwrap();
        let records = &caches[0].1.records;
        assert!(records[&1].needs_fetch(), "fetched again by a later call");
        assert!(!records[&2].needs_fetch());
        assert_eq!(p.report().backfilled, 1);
        assert!(p.report().errors[0].contains("Something went wrong"));
    }

    #[test]
    fn a_smaller_page_is_kept_for_later_calls_until_the_walks_are_done() {
        let now = t("2026-10-17T12:00:00Z");
        let bad_gateway = "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 11\r\nConnection: close\r\n\r\nBad Gateway"
            .to_string();
        let sizes = std::sync::Arc::new(Mutex::new(Vec::new()));
        let api = crate::tests::routed_stub({
            let sizes = sizes.clone();
            move |request| {
                let first = request
                    .split("\"first\":")
                    .nth(1)
                    .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next());
                sizes.lock().unwrap().push(first.unwrap_or("").to_string());
                Some(if request.contains("\"first\":25") {
                    bad_gateway.clone()
                } else if request.contains("\"after\":null") {
                    page(
                        vec![node(9, "MERGED", "2026-10-16T00:00:00Z")],
                        Some("c1"),
                        3900,
                    )
                } else {
                    page(vec![node(8, "MERGED", "2026-10-15T00:00:00Z")], None, 3900)
                })
            }
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let mut cache = PrCache::default();
        cache.closed_walk.done = true;
        // The first call has two requests: the page of 25 and the page of 10.
        let p = Pass::new(&gh, now, 1_500, 2);
        p.walk("o/r", &mut cache, false).unwrap();
        assert_eq!(cache.page_size, Some(10));
        assert_eq!(cache.merged_walk.cursor.as_deref(), Some("c1"));
        // The next call starts at 10, and the size is dropped once the walks end.
        let p = pass(&gh, now);
        p.walk("o/r", &mut cache, false).unwrap();
        assert!(cache.merged_walk.done && cache.records.contains_key(&8));
        assert_eq!(cache.page_size, None);
        assert_eq!(*sizes.lock().unwrap(), ["25", "10", "10"]);
        assert_eq!(p.report().retries, 0);
        // A page that fails at every size leaves the smallest for next time.
        let api = crate::tests::routed_stub(|_| {
            Some(
                "HTTP/1.1 504 Gateway Timeout\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string(),
            )
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let mut cache = PrCache::default();
        let p = pass(&gh, now);
        assert!(p.walk("o/r", &mut cache, false).unwrap_err().is_overload());
        assert_eq!((cache.page_size, p.report().requests), (Some(1), 4));
    }

    /// The `$first` of each request `api` gets, with the stub that answers
    /// them by `answer`.
    fn sized_stub(
        answer: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> (String, std::sync::Arc<Mutex<Vec<String>>>) {
        let sizes = std::sync::Arc::new(Mutex::new(Vec::new()));
        let api = crate::tests::routed_stub({
            let sizes = sizes.clone();
            move |request| {
                let first = request
                    .split("\"first\":")
                    .nth(1)
                    .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next());
                sizes.lock().unwrap().push(first.unwrap_or("").to_string());
                Some(answer(request))
            }
        });
        (api, sizes)
    }

    #[test]
    fn a_walk_asks_for_a_larger_page_again_after_reading_some() {
        let now = t("2026-10-17T12:00:00Z");
        let (api, sizes) = sized_stub(|_| {
            page(
                vec![node(9, "MERGED", "2026-10-16T00:00:00Z")],
                Some("c"),
                3900,
            )
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let mut cache = PrCache {
            page_size: Some(1),
            ..PrCache::default()
        };
        cache.closed_walk.done = true;
        let p = pass(&gh, now);
        p.walk("o/r", &mut cache, false).unwrap();
        let want: Vec<&str> = [["1"; 10], ["4"; 10], ["10"; 10]].concat();
        assert_eq!(*sizes.lock().unwrap(), want);
        assert_eq!((cache.page_size, cache.page_ok), (None, 0));
        // While pages of 25 keep failing, a call asks for one only every
        // ten pages.
        let bad_gateway =
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 11\r\nConnection: close\r\n\r\nBad Gateway";
        let (api, sizes) = sized_stub(move |request| {
            if request.contains("\"first\":25") {
                bad_gateway.to_string()
            } else {
                page(
                    vec![node(9, "MERGED", "2026-10-16T00:00:00Z")],
                    Some("c"),
                    3900,
                )
            }
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let mut cache = PrCache {
            page_size: Some(10),
            ..PrCache::default()
        };
        cache.closed_walk.done = true;
        let p = pass(&gh, now);
        p.walk("o/r", &mut cache, false).unwrap();
        let sizes = sizes.lock().unwrap();
        assert_eq!(sizes.len(), RUN_REQUESTS);
        assert_eq!(sizes.iter().filter(|s| *s == "25").count(), 2, "{sizes:?}");
        assert_eq!((cache.page_size, cache.page_ok), (Some(10), 8));
    }

    #[test]
    fn a_changed_list_starts_each_call_at_the_full_page() {
        let now = t("2026-10-17T12:00:00Z");
        let failed = std::sync::Arc::new(AtomicBool::new(false));
        let (api, sizes) = sized_stub({
            let failed = failed.clone();
            move |request| {
                if request.contains("\"first\":25") && !failed.swap(true, Ordering::SeqCst) {
                    "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 11\r\nConnection: close\r\n\r\nBad Gateway"
                        .to_string()
                } else {
                    page(vec![node(9, "OPEN", "2026-10-17T11:00:00Z")], None, 3900)
                }
            }
        });
        let gh = GitHub::with_api("t", api).unwrap();
        let mut cache = PrCache {
            high_water: Some(t("2026-10-17T10:00:00Z")),
            ..PrCache::default()
        };
        cache.merged_walk.done = true;
        cache.closed_walk.done = true;
        let p = pass(&gh, now);
        p.changed("o/r", &mut cache, None).unwrap();
        assert_eq!(cache.page_size, None, "the walks' size is left alone");
        p.changed("o/r", &mut cache, None).unwrap();
        assert_eq!(*sizes.lock().unwrap(), ["25", "10", "25"]);
        assert_eq!(cache.page_size, None);
    }

    #[test]
    fn a_sweep_of_many_pages_finishes_whatever_requests_the_call_has_left() {
        let now = t("2026-10-17T12:00:00Z");
        let open_page = |number: u64, next: Option<&str>| {
            ok_json(
                &json!({"data": {
                    "repository": {"defaultBranchRef": null, "pullRequests": {"totalCount": 3,
                        "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next},
                        "nodes": [light(number, "2026-10-17T11:00:00Z", 0)]}},
                    "rateLimit": {"cost": 1, "remaining": 3800}}})
                .to_string(),
            )
        };
        let (api, _requests) = stub(vec![
            open_page(1, Some("s1")),
            open_page(2, Some("s2")),
            open_page(3, None),
        ]);
        let gh = GitHub::with_api("t", api).unwrap();
        let p = Pass::new(&gh, now, 1_500, 1);
        let mut cache = PrCache::default();
        assert!(p.sweep("o/r", &mut cache).unwrap(), "every page was read");
        assert_eq!(cache.swept_at, Some(now));
        assert_eq!(cache.records.keys().copied().collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(p.report().requests, 3);
        assert_eq!(p.left.load(Ordering::Relaxed), 1, "left for the backfill");
    }

    #[test]
    fn a_changed_list_is_read_when_the_cap_leaves_nothing() {
        let now = t("2026-10-17T12:00:00Z");
        let (api, _requests) = stub(vec![page(
            vec![node(1, "OPEN", "2026-10-17T11:00:00Z")],
            None,
            3900,
        )]);
        let gh = GitHub::with_api("t", api).unwrap();
        let p = Pass::new(&gh, now, 1_500, 0);
        let mut cache = PrCache::default();
        p.changed("o/r", &mut cache, Some(&seen_at("2026-10-17T11:00:00Z")))
            .unwrap();
        assert!(cache.records.contains_key(&1));
        assert_eq!(cache.checked_at, Some(now));
        assert_eq!(p.report().requests, 1);
    }

    #[test]
    fn run_tasks_runs_added_tasks_and_stops_at_an_error() {
        let done = Mutex::new(Vec::new());
        run_tasks(vec![4u32], |n| {
            done.lock().unwrap().push(n);
            Ok(if n > 1 { vec![n / 2, n / 2] } else { vec![] })
        })
        .unwrap();
        assert_eq!(done.into_inner().unwrap(), [4, 2, 1, 1, 2, 1, 1]);
        // The tasks a task returns go before those still waiting.
        let order = Mutex::new(Vec::new());
        run_tasks(vec![40u32, 50], |n| {
            order.lock().unwrap().push(n);
            Ok(if n == 40 { vec![41, 42] } else { vec![] })
        })
        .unwrap();
        assert_eq!(order.into_inner().unwrap(), [40, 41, 42, 50]);
        let started = AtomicUsize::new(0);
        let result = run_tasks((0..5).collect(), |n: u32| {
            started.fetch_add(1, Ordering::Relaxed);
            match n {
                1 => Err(Error::Halted("later".into())),
                _ => Ok(vec![]),
            }
        });
        assert!(matches!(result, Err(Error::Halted(_))));
        assert_eq!(started.into_inner(), 2, "nothing starts after the error");
    }

    #[test]
    fn the_roster_download_sends_its_etag_and_no_token() {
        let csv = "GitHub\nann-example\n";
        let ok = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/csv\r\nETag: \"r1\"\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{csv}",
            csv.len()
        );
        let not_modified = "HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\n\
                            Connection: close\r\n\r\n"
            .to_string();
        let (api, requests) = stub(vec![ok, not_modified]);
        let url = format!("{api}/sheet.csv");
        assert_eq!(
            download_roster(&url, None),
            Ok(Fetched::Fresh {
                etag: Some("\"r1\"".into()),
                text: csv.into()
            })
        );
        let again = download_roster(&url, Some("\"r1\""));
        assert_eq!(again, Ok(Fetched::NotModified));
        let wait = Duration::from_secs(10);
        let first = requests.recv_timeout(wait).unwrap();
        assert!(!first.contains("authorization"), "{first}");
        assert!(!first.contains("if-none-match"), "{first}");
        let second = requests.recv_timeout(wait).unwrap();
        assert!(second.contains("if-none-match: \"r1\"\r\n"), "{second}");
    }
}
