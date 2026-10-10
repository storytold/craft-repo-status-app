//! `craft-fetch [owner/name …] [--snapshot PATH]` — fetch stats once and print
//! a table (debugging aid). `--snapshot` also writes a Snapshot JSON the UI
//! can load in a plain browser (`ui/dev-snapshot.json`).

use chrono::Utc;
use craft_core::{Config, LagBasis, RepoEntry, Snapshot, ALL_TIME};
use craft_github::{resolve_token, Error, GitHub};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let snapshot_path = args.iter().position(|a| a == "--snapshot").map(|i| {
        args.remove(i);
        args.remove(i)
    });
    let cfg = Config::load_from(&Config::path().unwrap()).unwrap_or_default();
    let repos = if args.is_empty() {
        cfg.repos.clone()
    } else {
        args
    };
    let gh = match resolve_token(&cfg.github_token).and_then(GitHub::new) {
        Ok(gh) => gh,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let now = Utc::now();
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
        match gh.fetch_repo(repo, now) {
            Ok((s, budget)) => {
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
