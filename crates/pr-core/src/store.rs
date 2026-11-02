//! What's kept between runs: one JSON file per repo in
//! `~/Library/Caches/craft-pr-review/repos/`. Corrupt or missing files are empty.
//! Who landed what lives in the cache shared with the status poller (`craft_core::shared`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use craft_core::{Error, Result};

use crate::model::{Pr, RepoData};
use crate::overlap::{local_groups, merged_groups};

pub struct Store {
    dir: PathBuf,
    pub repos: BTreeMap<String, RepoData>,
}

impl Store {
    pub fn default_dir() -> Result<PathBuf> {
        let base = dirs::cache_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
            .ok_or(Error::NoHome)?;
        Ok(base.join("craft-pr-review"))
    }

    /// Everything kept under `dir`.
    pub fn load(dir: &Path) -> Store {
        let mut repos = BTreeMap::new();
        let files = std::fs::read_dir(dir.join("repos"));
        for entry in files.into_iter().flatten().flatten() {
            let Some(data) = std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|t| serde_json::from_str::<RepoData>(&t).ok())
            else {
                continue;
            };
            if !data.repo.is_empty() {
                let mut data = data;
                refresh_groups(&mut data);
                repos.insert(data.repo.clone(), data);
            }
        }
        Store {
            dir: dir.to_path_buf(),
            repos,
        }
    }

    /// Fills in who landed what from the ledgers shared with the status poller.
    pub fn hydrate(&mut self, shared: &craft_core::SharedCache, repos: &[String]) {
        for repo in repos {
            let ledger = shared.load_ledger(repo);
            self.repo_mut(repo).history = crate::model::History {
                merged: ledger.merged_authors(),
                cursor: ledger.backfill_cursor,
                complete: ledger.backfill_complete,
            };
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, repo: &str) -> PathBuf {
        self.dir
            .join("repos")
            .join(format!("{}.json", repo.replace('/', "@")))
    }

    pub fn save(&self, repo: &str) -> Result<()> {
        match self.repos.get(repo) {
            Some(data) => craft_core::atomic_write(&self.path(repo), &serde_json::to_string(data)?),
            None => Ok(()),
        }
    }

    pub fn repo_mut(&mut self, repo: &str) -> &mut RepoData {
        self.repos
            .entry(repo.to_string())
            .or_insert_with(|| RepoData {
                repo: repo.to_string(),
                ..Default::default()
            })
    }

    /// Each author's merged PRs (lowercase login) across `repos`.
    pub fn landed_totals(&self, repos: &[String]) -> HashMap<String, u64> {
        let mut out: HashMap<String, u64> = HashMap::new();
        for d in repos.iter().filter_map(|r| self.repos.get(r)) {
            for author in d.history.merged.values() {
                *out.entry(author.to_lowercase()).or_insert(0) += 1;
            }
        }
        out
    }

    /// Whether every one of `repos` has had its merge history walked to the end.
    pub fn histories_complete(&self, repos: &[String]) -> bool {
        repos
            .iter()
            .all(|r| self.repos.get(r).is_some_and(|d| d.history.complete))
    }

    /// The PR, if it's still open.
    pub fn pr(&self, repo: &str, number: u64) -> Option<&Pr> {
        self.repos.get(repo)?.prs.get(&number)
    }
}

/// Recomputes `data.groups`. Call after anything that changes its PRs.
pub fn refresh_groups(data: &mut RepoData) {
    let prs: Vec<&Pr> = data.prs.values().filter(|p| !p.is_draft).collect();
    let local = local_groups(&prs);
    let open: HashSet<u64> = data.prs.keys().copied().collect();
    data.groups = merged_groups(&data.claude_groups, local, &open);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ClaudeGroup;

    #[test]
    fn round_trips_and_skips_corrupt_files() {
        let dir = std::env::temp_dir().join(format!("pr-store-{}", std::process::id()));
        let mut s = Store::load(&dir);
        assert!(s.repos.is_empty());
        let d = s.repo_mut("o/r");
        d.prs.insert(
            1,
            Pr {
                number: 1,
                title: "One (#7)".into(),
                ..Default::default()
            },
        );
        d.prs.insert(
            2,
            Pr {
                number: 2,
                title: "Two (#7)".into(),
                ..Default::default()
            },
        );
        d.claude_groups.push(ClaudeGroup {
            numbers: vec![1, 2],
            title: "Pair".into(),
            ..Default::default()
        });
        s.save("o/r").unwrap();
        std::fs::write(dir.join("repos/bad.json"), "{not json").unwrap();
        let back = Store::load(&dir);
        assert_eq!(back.repos.len(), 1);
        assert_eq!(back.pr("o/r", 1).unwrap().title, "One (#7)");
        assert_eq!(
            back.repos["o/r"].groups[0].title, "Pair",
            "groups are recomputed on load"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn history_is_read_from_the_shared_ledger() {
        let dir = std::env::temp_dir().join(format!("pr-store-h-{}", std::process::id()));
        let shared = craft_core::SharedCache::at(dir.join("shared"));
        let mut l = craft_core::PrLedger {
            backfill_complete: true,
            ..Default::default()
        };
        l.record(
            1,
            craft_core::LedgerPr {
                state: "MERGED".into(),
                author: Some("ann".into()),
                ..Default::default()
            },
        );
        shared.save_ledger("o/r", &l).unwrap();
        let mut s = Store::load(&dir);
        s.hydrate(&shared, &["o/r".to_string()]);
        assert!(s.repos["o/r"].history.complete);
        assert_eq!(s.landed_totals(&["o/r".to_string()])["ann"], 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn landed_totals_add_up_across_repos() {
        let dir = std::env::temp_dir().join(format!("pr-store-t-{}", std::process::id()));
        let mut s = Store::load(&dir);
        s.repo_mut("o/a").history.merged.insert(1, "Ann".into());
        s.repo_mut("o/b").history.merged.insert(1, "ann".into());
        s.repo_mut("o/x").history.merged.insert(1, "ann".into());
        let watched = ["o/a".to_string(), "o/b".to_string()];
        assert_eq!(s.landed_totals(&watched)["ann"], 2);
        assert!(!s.histories_complete(&watched));
        s.repo_mut("o/a").history.complete = true;
        s.repo_mut("o/b").history.complete = true;
        assert!(s.histories_complete(&watched));
    }
}
