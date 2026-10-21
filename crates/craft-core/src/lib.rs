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
pub use model::{
    Activity, RepoEntry, RepoStats, Schedule, Snapshot, TokenStatus, UrgentIssue, ALL_TIME,
    SINCE_RELEASE, WINDOWS,
};
pub use releases::{summarize_releases, Asset, BuildLag, LagBasis, Release, ReleaseSummary};
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
    #[error("config: {0}")]
    TomlEdit(#[from] toml_edit::TomlError),
    #[error("config: {0}")]
    TomlSer(#[from] toml::ser::Error),
    /// Values a save refused, one `field: problem` each.
    #[error("{}", .0.join("; "))]
    Invalid(Vec<String>),
    #[error("cache: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Write via a temp file + rename so readers never see a half-written file.
/// A symlink is followed, so the file it points at is replaced (or created,
/// when it does not exist yet), and the new file gets the permissions of the
/// one it replaces.
pub fn atomic_write(path: &std::path::Path, text: &str) -> Result<()> {
    write_replacing(path, text, false)
}

/// As [`atomic_write`], for text that holds a secret: on Unix the file is
/// readable and writable by its owner only.
pub fn atomic_write_private(path: &std::path::Path, text: &str) -> Result<()> {
    write_replacing(path, text, true)
}

/// Symlinks one write can follow before it gives up, as Linux's `ELOOP` limit.
const MAX_LINKS: usize = 40;

/// The file a write to `path` replaces: `path`, or the file its symlinks lead
/// to, even when that file does not exist yet.
fn write_path(path: &std::path::Path) -> Result<std::path::PathBuf> {
    let mut path = path.to_path_buf();
    for _ in 0..MAX_LINKS {
        let is_link = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
        if !is_link {
            return Ok(path);
        }
        let target = std::fs::read_link(&path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        path = match path.parent() {
            Some(dir) => dir.join(target),
            None => target,
        };
    }
    Err(Error::Io {
        path: path.display().to_string(),
        source: std::io::Error::other("too many levels of symbolic links"),
    })
}

fn write_replacing(path: &std::path::Path, text: &str, private: bool) -> Result<()> {
    let path = &write_path(path)?;
    let io = |source| Error::Io {
        path: path.display().to_string(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = path.with_extension("tmp");
    // The temporary file is created with its final mode, before the text
    // goes in: 0600 when private, else the mode of the file it replaces.
    let replaced = std::fs::metadata(path).ok().map(|m| m.permissions());
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mode = if private {
            Some(0o600)
        } else {
            replaced.as_ref().map(|p| p.mode() & 0o777)
        };
        if let Some(m) = mode {
            options.mode(m);
        }
        mode
    };
    let mut file = options.open(&tmp).map_err(io)?;
    // The umask can narrow the mode a file is created with.
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(m))
            .map_err(io)?;
    }
    #[cfg(not(unix))]
    if let (false, Some(p)) = (private, replaced) {
        file.set_permissions(p).map_err(io)?;
    }
    std::io::Write::write_all(&mut file, text.as_bytes()).map_err(io)?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(io)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_keeps_a_dangling_symlink() {
        let dir = std::env::temp_dir().join(format!("craft-dangling-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The link names a file, in a folder, that neither exists yet.
        let target = dir.join("dotfiles").join("config.toml");
        let link = dir.join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        atomic_write(&link, "poll_minutes = 20\n").unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "poll_minutes = 20\n"
        );
        // A relative link that leads through a second one.
        let hop = dir.join("hop.toml");
        std::os::unix::fs::symlink("config.toml", &hop).unwrap();
        atomic_write_private(&hop, "poll_minutes = 25\n").unwrap();
        assert!(std::fs::symlink_metadata(&hop)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "poll_minutes = 25\n"
        );
        // Links that lead back to themselves are refused and left in place.
        let a = dir.join("a.toml");
        let b = dir.join("b.toml");
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&a, &b).unwrap();
        assert!(atomic_write(&a, "x").is_err());
        assert!(std::fs::symlink_metadata(&a)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(&dir).ok();
    }
}
