//! The login of the user the GitHub token belongs to, as the app last read
//! it, kept in `viewer.json` in the cache directory so that readers of the
//! cache such as `craft-mcp` can list that user's pull requests.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::Time;
use crate::{atomic_write, Result};

/// The kept login and when the app read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct KeptViewer {
    pub login: String,
    pub read_at: Option<Time>,
}

/// `dir/viewer.json`.
pub fn path(dir: &Path) -> PathBuf {
    dir.join("viewer.json")
}

/// The kept login, or `None` when there is none or the file is corrupt.
pub fn load(dir: &Path) -> Option<KeptViewer> {
    let text = std::fs::read_to_string(path(dir)).ok()?;
    serde_json::from_str::<KeptViewer>(&text)
        .ok()
        .filter(|k| !k.login.is_empty())
}

/// Keeps `login`, read at `now`, unless the file already holds it in any
/// letter case. Returns whether it wrote the file.
pub fn keep(dir: &Path, login: &str, now: Time) -> Result<bool> {
    if load(dir).is_some_and(|k| k.login.eq_ignore_ascii_case(login)) {
        return Ok(false);
    }
    let kept = KeptViewer {
        login: login.to_string(),
        read_at: Some(now),
    };
    atomic_write(&path(dir), &serde_json::to_string(&kept)?)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_is_kept_once_and_replaced_when_it_changes() {
        let dir = std::env::temp_dir().join(format!("craft-viewer-{}", std::process::id()));
        let t = |h: u32| -> Time { format!("2026-10-18T{h:02}:00:00Z").parse().unwrap() };
        assert_eq!(load(&dir), None);
        assert!(keep(&dir, "Ann-Example", t(1)).unwrap());
        assert!(!keep(&dir, "ann-example", t(2)).unwrap(), "the same login");
        assert_eq!(
            load(&dir),
            Some(KeptViewer {
                login: "Ann-Example".into(),
                read_at: Some(t(1)),
            })
        );
        assert!(keep(&dir, "Bo-Example", t(3)).unwrap());
        assert_eq!(load(&dir).map(|k| k.login).as_deref(), Some("Bo-Example"));
        std::fs::write(path(&dir), "{not json").unwrap();
        assert_eq!(load(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
