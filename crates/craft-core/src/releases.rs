//! "Builds" are GitHub releases. Downloads are summed over the assets people
//! actually install (installers, archives, packages), not over checksum lists,
//! signatures, AppImage delta files or updater manifests, which are fetched by
//! machines and would inflate the numbers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Asset {
    pub name: String,
    pub download_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Release {
    pub tag: String,
    pub draft: bool,
    pub prerelease: bool,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: Option<DateTime<Utc>>,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReleaseSummary {
    pub latest_tag: Option<String>,
    pub latest_at: Option<DateTime<Utc>>,
    pub latest_prerelease: bool,
    pub latest_downloads: u64,
    pub total_downloads: u64,
}

/// Assets that are not something a person downloads to run the app.
pub fn is_counted_asset(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const SKIP_SUFFIXES: &[&str] = &[
        ".zsync",
        ".sha256",
        ".sha512",
        ".sha256sum",
        ".sig",
        ".asc",
        ".minisig",
        ".blockmap",
        ".json",
        ".yml",
        ".yaml",
        ".txt",
        ".intoto.jsonl",
    ];
    !(n.starts_with("sha256sums")
        || n.starts_with("checksums")
        || SKIP_SUFFIXES.iter().any(|s| n.ends_with(s)))
}

fn downloads(r: &Release) -> u64 {
    r.assets
        .iter()
        .filter(|a| is_counted_asset(&a.name))
        .map(|a| a.download_count)
        .sum()
}

/// Latest = newest published, non-draft release (prereleases count: they are
/// builds too). Drafts are invisible to users and are ignored entirely.
pub fn summarize_releases(releases: &[Release]) -> ReleaseSummary {
    let published: Vec<&Release> = releases.iter().filter(|r| !r.draft).collect();
    let latest = published
        .iter()
        .max_by_key(|r| r.published_at.or(r.created_at));
    ReleaseSummary {
        latest_tag: latest.map(|r| r.tag.clone()),
        latest_at: latest.and_then(|r| r.published_at.or(r.created_at)),
        latest_prerelease: latest.is_some_and(|r| r.prerelease),
        latest_downloads: latest.map_or(0, |r| downloads(r)),
        total_downloads: published.iter().map(|r| downloads(r)).sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, draft: bool, day: u32, assets: &[(&str, u64)]) -> Release {
        Release {
            tag: tag.into(),
            draft,
            prerelease: tag.contains("rc"),
            published_at: (!draft).then(|| format!("2026-10-{day:02}T00:00:00Z").parse().unwrap()),
            created_at: Some(format!("2026-10-{day:02}T00:00:00Z").parse().unwrap()),
            assets: assets
                .iter()
                .map(|(n, c)| Asset {
                    name: n.to_string(),
                    download_count: *c,
                })
                .collect(),
        }
    }

    #[test]
    fn filters_machine_assets() {
        assert!(is_counted_asset("photocraft-0.5.0-macos-universal.dmg"));
        assert!(is_counted_asset("photocraft-web-0.5.0.zip"));
        assert!(is_counted_asset("x-linux-x86_64.AppImage"));
        assert!(!is_counted_asset("x-linux-x86_64.AppImage.zsync"));
        assert!(!is_counted_asset("SHA256SUMS.txt"));
        assert!(!is_counted_asset("latest.json"));
        assert!(!is_counted_asset("x.dmg.sig"));
    }

    #[test]
    fn latest_skips_drafts_and_totals_published() {
        let rs = vec![
            rel("v0.4.1", true, 8, &[("a.dmg", 0)]),
            rel(
                "v0.5.0",
                false,
                7,
                &[("a.dmg", 10), ("a.msi", 5), ("SHA256SUMS.txt", 99)],
            ),
            rel(
                "v0.3.0",
                false,
                3,
                &[("a.dmg", 100), ("a.AppImage.zsync", 50)],
            ),
        ];
        let s = summarize_releases(&rs);
        assert_eq!(s.latest_tag.as_deref(), Some("v0.5.0"));
        assert_eq!(s.latest_downloads, 15);
        assert_eq!(s.total_downloads, 115);
        assert!(!s.latest_prerelease);
    }

    #[test]
    fn prerelease_can_be_latest_and_empty_is_empty() {
        let s = summarize_releases(&[
            rel("v0.1.0", false, 1, &[]),
            rel("v0.2.0-rc.1", false, 2, &[]),
        ]);
        assert_eq!(s.latest_tag.as_deref(), Some("v0.2.0-rc.1"));
        assert!(s.latest_prerelease);
        assert_eq!(summarize_releases(&[]), ReleaseSummary::default());
    }
}
