//! Debugging: fetch one or more repos into the app's cache, print a table and
//! write `ui/dev-data.json` for `make preview`.
//!
//!     pr-fetch storytold/figcraft [--details N] [--dump ui/dev-data.json]
//!
//! With no repo it reads what's already cached and fetches nothing.

use std::collections::BTreeMap;

use chrono::Utc;
use pr_core::schedule::detail_priority;
use pr_core::store::{refresh_groups, Store};
use pr_core::{views, Config, DetailRef, StatusFacts};
use pr_github::{resolve_token, GitHub};
use serde_json::{json, Value};

const BATCH: usize = 25;

fn main() {
    let mut repos = Vec::new();
    let mut details = 0usize;
    let mut dump = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--details" => details = args.next().and_then(|n| n.parse().ok()).unwrap_or(25),
            "--dump" => dump = args.next(),
            _ => repos.push(a),
        }
    }
    if let Err(e) = run(&repos, details, dump.as_deref()) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Lists `repo` and who can write to it, then inspects its `details` best PRs.
fn fetch_repo(
    gh: &GitHub,
    store: &mut Store,
    repo: &String,
    details: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let now = Utc::now();
    let d = store.repo_mut(repo);
    let spends = gh.list_full(repo, d, now, &mut |_| {})?;
    let (roster, more) = gh.roster(repo, now)?;
    d.roster = roster;
    let left = more.last().and_then(|s| s.remaining);
    let cost: u64 = spends.iter().chain(&more).map(|s| s.cost).sum();
    println!("{repo}: {} open, {cost} points, {left:?} left", d.prs.len());
    let mut pending: Vec<(i32, DetailRef)> = d
        .prs
        .values()
        .filter_map(|p| {
            detail_priority(p, now).map(|s| {
                (
                    s,
                    DetailRef {
                        repo: repo.clone(),
                        number: p.number,
                        id: p.id.clone(),
                    },
                )
            })
        })
        .collect();
    pending.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
    pending.truncate(details);
    for chunk in pending.chunks(BATCH) {
        let refs: Vec<DetailRef> = chunk.iter().map(|(_, r)| r.clone()).collect();
        let (results, spend) = gh.details(&refs, now)?;
        println!("  inspected {} ({} points)", refs.len(), spend.cost);
        let d = store.repo_mut(repo);
        for (r, res) in results {
            if let (Some(res), Some(p)) = (res, d.prs.get_mut(&r.number)) {
                res.apply(p, now);
            }
        }
    }
    refresh_groups(store.repo_mut(repo));
    store.save(repo)?;
    Ok(())
}

fn run(
    fetch: &[String],
    details: usize,
    dump: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load()?;
    let shared_path = craft_core::Config::path()?;
    let shared = craft_core::Config::load_from(&shared_path).unwrap_or_default();
    let watched = cfg.watched(&shared.repos);
    let mut store = Store::load(&Store::default_dir()?);
    let shared_cache = craft_core::SharedCache::default_location()?;
    store.hydrate(&shared_cache, &watched);
    let facts = craft_core::Cache::default_location()?
        .load()
        .map(|snap| StatusFacts::of(&snap))
        .unwrap_or_default();

    if !fetch.is_empty() {
        let gh = GitHub::new(
            resolve_token(&shared.github_token)?.value,
            shared_cache.clone(),
        )?;
        for repo in fetch {
            let _lease = shared_cache.lease(std::time::Duration::from_secs(180));
            fetch_repo(&gh, &mut store, repo, details)?;
        }
    }

    let rows = views::overview(&store, &watched, &cfg, &facts);
    println!(
        "{:<26} {:>5} {:>5} {:>5} {:>5} {:>4} {:>4} {:>4} {:>5} {:>4} {:>6}",
        "repo", "open", "insp", "revw", "land", "easy", "hard", "work", "flags", "sec", "groups"
    );
    for r in &rows {
        println!(
            "{:<26} {:>5} {:>5} {:>5} {:>5} {:>4} {:>4} {:>4} {:>5} {:>4} {:>6}",
            r.repo,
            r.open,
            r.inspected,
            r.reviewable,
            r.landable,
            r.easy,
            r.hard,
            r.needs_work,
            r.rabbit_flagged,
            r.security,
            r.groups
        );
    }

    if let Some(path) = dump {
        let mut repo_views: BTreeMap<String, Value> = BTreeMap::new();
        let mut prs: BTreeMap<String, Value> = BTreeMap::new();
        for repo in &watched {
            if let Some(v) = views::repo_view(&store, repo, &watched, &cfg, &facts) {
                for row in v.prs.iter().filter(|r| r.inspected).take(60) {
                    if let Some(full) =
                        views::pr_full(&store, repo, row.number, &watched, &cfg, &facts)
                    {
                        prs.insert(
                            format!("{repo}#{}", row.number),
                            serde_json::to_value(full)?,
                        );
                    }
                }
                repo_views.insert(repo.clone(), serde_json::to_value(v)?);
            }
        }
        let out = json!({
            "overview": rows,
            "queue": views::queue(&store, &watched, &cfg, 300),
            "repos": repo_views,
            "prs": prs,
        });
        std::fs::write(path, serde_json::to_string(&out)?)?;
        println!("wrote {path}");
    }
    Ok(())
}
