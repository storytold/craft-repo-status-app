//! `craft-fetch [owner/name …] [--snapshot PATH] [--parity]`: fetch stats
//! once and print a table (debugging aid). `--snapshot` also writes a
//! Snapshot JSON the UI can load in a plain browser (`ui/dev-snapshot.json`).
//! `--parity` also downloads each repo's parity documents and prints the
//! numbers read from them, which the snapshot then carries too.

use chrono::Utc;
use craft_core::{Cache, Config, Figure, LagBasis, RepoEntry, SharedCache, Snapshot, ALL_TIME};
use craft_github::{resolve_token, Error, GitHub, RawActivity};

/// How long to wait for the PR poller to finish talking to GitHub.
const LEASE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(180);

/// A parity number as one line: the percentage and its row, or why not.
fn figure(f: &Figure) -> String {
    match f {
        Figure::Ok {
            pct,
            label,
            basis,
            path,
            line,
            from,
            ..
        } => format!(
            "{pct}% ({basis:?}, {label}, {path}:{line}{})",
            from.as_ref().map_or(String::new(), |f| format!("; {f}"))
        ),
        Figure::Absent { why } => format!("- ({why})"),
        Figure::Unreadable {
            why,
            raw,
            path,
            line,
        } => format!("- ({why}, {path}:{line}: {raw:?})"),
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let snapshot_path = args.iter().position(|a| a == "--snapshot").map(|i| {
        args.remove(i);
        args.remove(i)
    });
    let parity = args
        .iter()
        .position(|a| a == "--parity")
        .map(|i| args.remove(i))
        .is_some();
    let cfg = Config::load_from(&Config::path().unwrap()).unwrap_or_default();
    let repos = if args.is_empty() {
        cfg.repos.clone()
    } else {
        args
    };
    let gh = match resolve_token(&cfg.github_token).and_then(|t| GitHub::new(t.value)) {
        Ok(gh) => gh,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
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
                if parity {
                    match gh.parity(repo, s.head_oid.as_deref(), &s.parity_oids) {
                        Ok(p) => {
                            println!("    breadth  {}", figure(&p.breadth));
                            println!("    ready    {}", figure(&p.ready));
                            println!("    reviewed {}", p.last_reviewed.as_deref().unwrap_or("-"));
                            entry.parity = Some(p);
                        }
                        Err(e) => eprintln!("{repo}: parity: {e}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use craft_core::parity::{read, Input, ROADMAP};

    #[test]
    fn parity_lines_give_the_number_and_its_row_or_why_not() {
        let p = read(&[Input {
            path: ROADMAP,
            oid: "a1",
            text: "## Headline numbers\n| | Value | Kind |\n|---|---|---|\n\
                   | **Feature breadth** | **~70%** | measured |\n| Ready for real work | soon | estimated |\n",
        }]);
        assert_eq!(
            figure(&p.breadth),
            "70% (Measured, Feature breadth, ROADMAP.md:4)"
        );
        assert_eq!(
            figure(&p.ready),
            "- (No percentage in the Value cell of Ready for real work, ROADMAP.md:5: \"soon\")"
        );
        assert_eq!(
            figure(&read(&[]).breadth),
            "- (No ROADMAP.md or docs/target-app-parity.md at HEAD)"
        );
    }
}
