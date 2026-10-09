//! The last snapshot, persisted so the window has numbers the instant it opens
//! (flagged stale until the next poll lands). Corrupt or missing → empty.

use std::path::{Path, PathBuf};

use crate::model::Snapshot;
use crate::{atomic_write, Error, Result};

pub struct Cache {
    path: PathBuf,
}

impl Cache {
    /// `~/Library/Caches/craft-status/snapshot.json` (or the platform equivalent).
    pub fn default_location() -> Result<Cache> {
        let dir = dirs::cache_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
            .ok_or(Error::NoHome)?;
        Ok(Cache::at(dir.join("craft-status").join("snapshot.json")))
    }

    pub fn at(path: PathBuf) -> Cache {
        Cache { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Option<Snapshot> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        let mut s: Snapshot = serde_json::from_str(&text).ok()?;
        s.refreshing = false;
        Some(s)
    }

    pub fn save(&self, s: &Snapshot) -> Result<()> {
        atomic_write(&self.path, &serde_json::to_string(s)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_corrupt_fallback() {
        let dir = std::env::temp_dir().join(format!("craft-cache-{}", std::process::id()));
        let cache = Cache::at(dir.join("s.json"));
        assert!(cache.load().is_none());
        let mut s = Snapshot {
            refreshing: true,
            poll_secs: 300,
            ..Default::default()
        };
        s.sync_repos(&["o/r".into()]);
        cache.save(&s).unwrap();
        let back = cache.load().unwrap();
        assert!(!back.refreshing, "a loaded cache is never mid-refresh");
        assert_eq!(back.repos, s.repos);
        std::fs::write(cache.path(), "{not json").unwrap();
        assert!(cache.load().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
