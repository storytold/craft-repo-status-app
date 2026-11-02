//! `craft-fetch [owner/name …] [--snapshot PATH] [--people] [--states]
//! [--roster SOURCE]` — fetch stats once and print a table (debugging aid).
//! `--snapshot` also writes a Snapshot JSON the UI can load in a plain
//! browser (`ui/dev-snapshot.json`). `--people` runs one update of the
//! repos' pull request records in the app's cache and prints counts and work
//! states; quit the app first, since both write the same record files.
//! `--states` prints the work states of the records already in the
//! cache, without a request to GitHub. `--roster` reads that roster in place
//! of the config's `people.roster`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use craft_core::people::ledger::Ledger;
use craft_core::people::record::PrCache;
use craft_core::people::roster::{self, RosterRow};
use craft_core::people::view::assemble;
use craft_core::people::{maintainers, Directory};
use craft_core::{Cache, Config, LagBasis, RepoEntry, SharedCache, Snapshot, ALL_TIME};
use craft_github::prs::{download_roster, PrInput, Seen, RUN_REQUESTS};
use craft_github::{resolve_token, Error, GitHub, RawActivity};

/// The app's cache directory, or the end of the program.
fn cache_dir() -> PathBuf {
    match Cache::default_location() {
        Ok(c) => c.dir().to_path_buf(),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// Works out the work state of every record in `caches` and prints, per
/// state, how many pull requests are in it and how many of those are
/// guesses from comment text, then how many merged pull requests have each
/// type. Returns the directory of people.
fn states(
    caches: &mut [(String, PrCache)],
    cfg: &Config,
    rows: &[RosterRow],
    now: DateTime<Utc>,
) -> Directory {
    let (view, dir, rankings) =
        assemble(caches, &cfg.people, rows, |_| None, &BTreeMap::new(), now);
    let mut counts: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    for (_, cache) in caches.iter() {
        for r in cache.records.values() {
            let state = r.derived.state.as_ref().map_or("", |d| d.value.as_str());
            counts.entry(state.to_string()).or_default().0 += 1;
        }
    }
    let mut types: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for c in &rankings.credits {
        let t = types.entry(c.kind.kind.value.as_str()).or_default();
        t.0 += 1;
        t.1 += usize::from(c.kind.guess);
    }
    for (kind, (n, guessed)) in &types {
        println!("  type {kind}: {n} merged ({guessed} guessed)");
    }
    for item in &view.items {
        let c = counts.entry(item.work.state.clone()).or_default();
        c.1 += usize::from(item.work.guess);
        c.2 += usize::from(item.work.low);
    }
    for (state, (n, guessed, low)) in &counts {
        println!("  {state}: {n} ({guessed} open guessed, {low} open low confidence)");
    }
    println!(
        "people rows {}, open items {}",
        view.people.len(),
        view.items.len()
    );
    dir
}

/// Reads the records of `repos` from the app's cache, without a request to
/// GitHub or a roster download, and prints their work states (see
/// [`states`]). A roster URL gives its kept copy.
fn states_offline(repos: &[String], cfg: &Config, roster_source: &str) {
    let dir = cache_dir();
    let now = Utc::now();
    let mut caches: Vec<(String, PrCache)> = repos
        .iter()
        .map(|r| (r.clone(), PrCache::load(&dir, r)))
        .collect();
    let loaded = roster::load_kept(roster_source, &dir, now);
    let d = states(&mut caches, cfg, &loaded.rows, now);
    println!(
        "people {} (bots {}), on the roster {}",
        d.people.values().filter(|p| !p.bot).count(),
        d.people.values().filter(|p| p.bot).count(),
        d.people.values().filter(|p| p.on_roster).count(),
    );
}

/// Runs one update of `repos`' pull request records in the app's cache, the
/// way the app's People thread does, and prints what it did, how many people
/// the records and the roster (`roster_source`) name, and the last week's
/// event counts. It reads every repo's changed list, since the raw cache
/// may be older than the records.
fn people(gh: &GitHub, repos: &[String], cfg: &Config, roster_source: &str) {
    let dir = cache_dir();
    let seen: HashMap<String, Option<Seen>> = HashMap::new();
    let now = Utc::now();
    let (report, caches) = gh.update_prs(&PrInput {
        repos,
        seen: &seen,
        dir: &dir,
        now,
        interval_secs: cfg.poll_secs(),
        remaining: None,
        floor: 1_500,
        max_requests: RUN_REQUESTS,
        viewer: None,
        on_event: None,
    });
    println!("{report:#?}");
    let loaded = roster::load(roster_source, &dir, now, download_roster);
    if let Some(e) = &loaded.error {
        println!("roster error: {e}");
    }
    for (repo, cache) in &caches {
        let m = maintainers(cache.records.values(), &cfg.people.maintainers, now);
        println!(
            "{repo}: {} records, {} maintainers, merged walk done {}, closed walk done {}",
            cache.records.len(),
            m.len(),
            cache.merged_walk.done,
            cache.closed_walk.done
        );
    }
    let mut caches = caches;
    let d = states(&mut caches, cfg, &loaded.rows, now);
    println!(
        "people {} (bots {}), on the roster {}, roster rows {} ({} not linked)",
        d.people.values().filter(|p| !p.bot).count(),
        d.people.values().filter(|p| p.bot).count(),
        d.people.values().filter(|p| p.on_roster).count(),
        loaded.rows.len(),
        d.unlinked.len()
    );
    let held: Vec<(&str, &PrCache)> = caches.iter().map(|(r, c)| (r.as_str(), c)).collect();
    let ledger = Ledger::build(&held);
    let week = &ledger.windows(now, &Default::default())["7d"];
    let sum = |f: fn(&craft_core::people::ledger::Tally) -> u64| week.values().map(f).sum::<u64>();
    println!(
        "events {}; last 7 days: merged {}, merges {}, reviews {}, review commits {}, people {}",
        ledger.events.len(),
        sum(|t| t.merged),
        sum(|t| t.merges),
        sum(|t| t.reviews),
        sum(|t| t.review_commits),
        week.len()
    );
}

/// How long to wait for the PR poller to finish talking to GitHub.
const LEASE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(180);

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let snapshot_path = args.iter().position(|a| a == "--snapshot").map(|i| {
        args.remove(i);
        args.remove(i)
    });
    let people_only = args
        .iter()
        .position(|a| a == "--people")
        .map(|i| args.remove(i));
    let roster_source = args.iter().position(|a| a == "--roster").map(|i| {
        args.remove(i);
        if i >= args.len() {
            eprintln!("--roster needs a URL or a file path");
            std::process::exit(2);
        }
        args.remove(i)
    });
    let states_only = args
        .iter()
        .position(|a| a == "--states")
        .map(|i| args.remove(i));
    let cfg = Config::load_from(&Config::path().unwrap()).unwrap_or_default();
    let repos = if args.is_empty() {
        cfg.repos.clone()
    } else {
        args
    };
    if states_only.is_some() {
        let source = roster_source.unwrap_or_else(|| cfg.people.roster.clone());
        states_offline(&repos, &cfg, &source);
        return;
    }
    let gh = match resolve_token(&cfg.github_token).and_then(|t| GitHub::new(t.value)) {
        Ok(gh) => gh,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if people_only.is_some() {
        let source = roster_source.unwrap_or_else(|| cfg.people.roster.clone());
        people(&gh, &repos, &cfg, &source);
        return;
    }
    let now = Utc::now();
    let shared = SharedCache::default_location().unwrap();
    let mut snap = Snapshot {
        poll_secs: cfg.poll_secs(),
        stale_after_secs: cfg.stale_secs(),
        ..Default::default()
    };
    println!(
        "{:<12} {:>5} {:>6} {:>4} {:>4} {:>4} {:>4} {:>7} {:>5} {:>7} {:>7} {:<12} {:>5} {:>7} {:>8} {:>6} {:>4}",
        "repo", "oPR", "oIss", "c4h", "p4h", "m4h", "i4h", "commits", "ppl", "issues", "prs", "build", "since", "bld dl",
        "total dl", "urg", "crit"
    );
    for repo in &repos {
        let mut entry = RepoEntry::new(repo);
        entry.last_attempt_at = Some(now);
        // Shares the app's raw cache, so either warms it for the other.
        let raw_dir = Cache::default_location()
            .ok()
            .map(|c| c.dir().to_path_buf());
        let mut raw = raw_dir
            .as_deref()
            .map(|d| RawActivity::load(d, repo))
            .unwrap_or_default();
        let _lease = shared.lease(LEASE_PATIENCE);
        match gh.fetch_repo(repo, now, &mut raw, &shared) {
            Ok((s, budget)) => {
                if let Some(dir) = &raw_dir {
                    if let Err(e) = raw.save(dir, repo) {
                        eprintln!("{repo}: could not save raw cache: {e}");
                    }
                }
                // The table shows the 4-hour window; the snapshot carries them all.
                let r4 = s.recent.get("4h").copied().unwrap_or_default();
                let all = s.recent.get(ALL_TIME).copied().unwrap_or_default();
                let since = s
                    .since_build
                    .as_ref()
                    .map_or("-".into(), |l| match l.basis {
                        LagBasis::Exact => l.commits.to_string(),
                        _ => format!("~{}", l.commits),
                    });
                println!(
                    "{:<12} {:>5} {:>6} {:>4} {:>4} {:>4} {:>4} {:>7} {:>5} {:>7} {:>7} {:<12} {:>5} {:>7} {:>8} {:>6} {:>4}",
                    entry.name(), s.open_prs, s.open_issues, r4.commits, r4.prs_opened, r4.prs_merged, r4.issues_opened,
                    all.commits, all.people, all.issues_opened, all.prs_opened,
                    s.latest_build.clone().unwrap_or_default(), since, s.latest_build_downloads,
                    s.downloads_total, s.urgency, s.critical_issues
                );
                for u in s.urgent.iter().take(3) {
                    println!(
                        "    {:>4}  #{:<5} {}  [{}]",
                        u.score,
                        u.number,
                        u.title,
                        u.reasons.join(", ")
                    );
                }
                snap.rate_limit_remaining = budget.graphql_remaining;
                if let Some(oid) = &s.icon_oid {
                    match gh.icon(repo, oid) {
                        Ok(uri) => (entry.icon, entry.icon_oid) = (Some(uri), Some(oid.clone())),
                        Err(e) => eprintln!("{repo}: icon: {e}"),
                    }
                }
                entry.stats = Some(s);
                entry.fetched_at = Some(now);
            }
            Err(Error::NotFound) => {
                println!("{:<12} (not accessible)", entry.name());
                entry.fetched_at = Some(now);
                entry.inaccessible = true;
            }
            Err(e) => {
                println!("{:<12} ERROR {e}", entry.name());
                entry.error = Some(e.to_string());
            }
        }
        snap.repos.push(entry);
    }
    snap.last_full_refresh_at = Some(Utc::now());
    eprintln!("graphql budget remaining: {:?}", snap.rate_limit_remaining);
    if let Some(p) = snapshot_path {
        std::fs::write(&p, serde_json::to_string_pretty(&snap).unwrap()).unwrap();
        eprintln!("wrote {p}");
    }
}
