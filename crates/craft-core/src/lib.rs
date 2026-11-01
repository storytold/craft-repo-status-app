//! What the Downloads and Fix first tabs know that isn't network or UI: the per-repo
//! statistics model, release/download accounting, the urgency heuristic for
//! open issues, the config file, the on-disk cache and window geometry.

pub mod cache;
pub mod config;
pub mod model;
pub mod releases;
pub mod shared;
pub mod urgency;

pub use cache::Cache;
pub use config::{category_of, Config, RepoCategory, WindowState};
pub use model::{
    Activity, RepoEntry, RepoStats, Schedule, Snapshot, TokenStatus, UrgentIssue, ALL_TIME,
    SINCE_RELEASE, WINDOWS,
};
pub use releases::{summarize_releases, Asset, BuildLag, LagBasis, Release, ReleaseSummary};
pub use shared::{
    ledger_from_old_raw, Lease, LedgerPr, PrLedger, SharedBudget, SharedCache, WalkPlan,
};
pub use urgency::{rank_issues, score_issue, IssueInput, Scored};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no home directory")]
    NoHome,
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("config: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("cache: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

use std::sync::atomic::{AtomicU64, Ordering};

/// Write via a temp file + rename so readers never see a half-written file.
pub fn atomic_write(path: &std::path::Path, text: &str) -> Result<()> {
    let io = |source| Error::Io {
        path: path.display().to_string(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    // Per call, since the status and PR pollers write some of the same files.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp{}-{n}", std::process::id()));
    std::fs::write(&tmp, text).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threads_writing_one_file_do_not_collide() {
        let dir = std::env::temp_dir().join(format!("craft-atomic-{}", std::process::id()));
        let path = dir.join("budget.json");
        let writers: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        atomic_write(&path, &i.to_string().repeat(1000)).unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.len(), 1000);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp left");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
