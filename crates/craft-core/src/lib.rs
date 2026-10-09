//! Everything Craft Status knows that isn't network or UI: the per-repo
//! statistics model, release/download accounting, the urgency heuristic for
//! open issues, the config file, the on-disk cache and window geometry.

pub mod cache;
pub mod config;
pub mod model;
pub mod releases;
pub mod urgency;

pub use cache::Cache;
pub use config::{Config, WindowState};
pub use model::{RepoEntry, RepoStats, Snapshot, UrgentIssue};
pub use releases::{summarize_releases, Asset, Release, ReleaseSummary};
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

/// Write via a temp file + rename so readers never see a half-written file.
pub fn atomic_write(path: &std::path::Path, text: &str) -> Result<()> {
    let io = |source| Error::Io {
        path: path.display().to_string(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)
}
