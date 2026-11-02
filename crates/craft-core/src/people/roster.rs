//! The team roster: a CSV export of a sheet, named by `people.roster` in
//! the config as an `https://` URL or a file path. It is read while the app
//! runs and never stored in the repository. A downloaded copy is kept in
//! the cache directory (`roster.json`) and downloaded again once it is an
//! hour old, or when the configured source changes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::valid_login;
use crate::model::Time;
use crate::{atomic_write, Result};

/// How long a downloaded roster is used before it is downloaded again.
pub const REFRESH_MINUTES: i64 = 60;

/// One roster row. Columns are found by their header: `GitHub` for the
/// login, `Discord` with `user` or `handle` for the Discord name, the first
/// other column with `name` for the display name, `time zone` or
/// `timezone`, and `focus` for the apps (split at commas). Other columns
/// are not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RosterRow {
    pub name: Option<String>,
    pub discord: Option<String>,
    /// The GitHub login; `None` when the cell is empty or holds no login.
    pub github: Option<String>,
    pub timezone: Option<String>,
    pub focus: Vec<String>,
}

/// Where the roster comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    None,
    Url(String),
    File(PathBuf),
}

/// The source `people.roster` names: empty for none, an `https://` URL, or
/// a file path (`~/` is the home directory). `http://` is refused.
pub fn source(configured: &str) -> std::result::Result<Source, String> {
    let s = configured.trim();
    if s.is_empty() {
        return Ok(Source::None);
    }
    if s.starts_with("https://") {
        return Ok(Source::Url(s.to_string()));
    }
    if s.contains("://") {
        return Err(format!(
            "roster: {s:?} is not an https:// URL or a file path"
        ));
    }
    let path = match s.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .ok_or("roster: no home directory")?
            .join(rest),
        None => PathBuf::from(s),
    };
    Ok(Source::File(path))
}

/// The answer to a roster download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// The copy sent with the ETag is still current.
    NotModified,
    Fresh {
        etag: Option<String>,
        text: String,
    },
}

/// A downloaded roster, kept in `<cache>/roster.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
struct Kept {
    source: String,
    fetched_at: Option<Time>,
    etag: Option<String>,
    text: String,
}

/// What [`load`] read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Loaded {
    pub rows: Vec<RosterRow>,
    /// Why the source could not be read; `rows` then holds the kept copy,
    /// when there is one.
    pub error: Option<String>,
    /// Whether this call downloaded the roster.
    pub downloaded: bool,
}

/// The roster `configured` names. A file is read each time. A URL is
/// downloaded with `fetch` (given the URL and the kept copy's ETag) when the
/// kept copy is from another source or [`REFRESH_MINUTES`] old; otherwise,
/// and when the download fails, the kept copy is used.
pub fn load(
    configured: &str,
    dir: &Path,
    now: Time,
    fetch: impl FnOnce(&str, Option<&str>) -> std::result::Result<Fetched, String>,
) -> Loaded {
    let url = match source(configured) {
        Ok(Source::None) => return Loaded::default(),
        Ok(Source::File(path)) => {
            return match std::fs::read_to_string(&path) {
                Ok(text) => Loaded {
                    rows: rows(&text),
                    ..Default::default()
                },
                Err(e) => Loaded {
                    error: Some(format!("roster: {}: {e}", path.display())),
                    ..Default::default()
                },
            }
        }
        Ok(Source::Url(url)) => url,
        Err(e) => {
            return Loaded {
                error: Some(e),
                ..Default::default()
            }
        }
    };
    let path = dir.join("roster.json");
    let kept: Option<Kept> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|k: &Kept| k.source == url);
    let fresh = kept.as_ref().is_some_and(|k| {
        k.fetched_at
            .is_some_and(|at| now - at < chrono::Duration::minutes(REFRESH_MINUTES))
    });
    if let (true, Some(k)) = (fresh, &kept) {
        return Loaded {
            rows: rows(&k.text),
            ..Default::default()
        };
    }
    let etag = kept.as_ref().and_then(|k| k.etag.as_deref());
    let (text, etag) = match fetch(&url, etag) {
        Ok(Fetched::Fresh { etag, text }) => (text, etag),
        Ok(Fetched::NotModified) if kept.is_some() => {
            let k = kept.unwrap_or_default();
            (k.text, k.etag)
        }
        Ok(Fetched::NotModified) => {
            return Loaded {
                error: Some("roster: 304 Not Modified with no kept copy".into()),
                ..Default::default()
            }
        }
        Err(e) => {
            return Loaded {
                rows: kept.map(|k| rows(&k.text)).unwrap_or_default(),
                error: Some(format!("roster: {e}")),
                downloaded: false,
            }
        }
    };
    let k = Kept {
        source: url,
        fetched_at: Some(now),
        etag,
        text,
    };
    let error = save(&path, &k).err().map(|e| format!("roster: {e}"));
    Loaded {
        rows: rows(&k.text),
        error,
        downloaded: true,
    }
}

/// What [`load_kept`] passes [`load`] in place of a download.
const KEPT_ONLY: &str = "not downloaded: only the kept copy is read";

/// The roster `configured` names, without a download: a URL gives the kept
/// copy whatever its age, or no rows when there is none. A file is read as
/// [`load`] reads it.
pub fn load_kept(configured: &str, dir: &Path, now: Time) -> Loaded {
    let out = load(configured, dir, now, |_, _| Err(KEPT_ONLY.to_string()));
    let kept_only = out.error.as_deref() == Some(format!("roster: {KEPT_ONLY}").as_str());
    Loaded {
        error: if kept_only { None } else { out.error },
        ..out
    }
}

fn save(path: &Path, k: &Kept) -> Result<()> {
    atomic_write(path, &serde_json::to_string(k)?)
}

/// The rows of a roster CSV whose first line holds the headers. Rows with
/// no cell filled are left out.
pub fn rows(text: &str) -> Vec<RosterRow> {
    let mut table = parse_csv(text).into_iter();
    let Some(header) = table.next() else {
        return Vec::new();
    };
    let lower: Vec<String> = header.iter().map(|h| h.to_lowercase()).collect();
    let mut taken = Vec::new();
    let mut find = |test: &dyn Fn(&str) -> bool| {
        let i = lower
            .iter()
            .enumerate()
            .position(|(i, h)| !taken.contains(&i) && test(h))?;
        taken.push(i);
        Some(i)
    };
    let github = find(&|h| h.contains("github"));
    let discord = find(&|h| h.contains("discord") && (h.contains("user") || h.contains("handle")));
    let name = find(&|h| h.contains("name") && !h.contains("user"));
    let timezone = find(&|h| h.contains("timezone") || h.contains("time zone"));
    let focus = find(&|h| h.contains("focus"));
    let cell = |row: &[String], i: Option<usize>| {
        i.and_then(|i| row.get(i))
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
    };
    table
        .filter(|row| row.iter().any(|c| !c.trim().is_empty()))
        .map(|row| RosterRow {
            name: cell(&row, name),
            discord: cell(&row, discord),
            github: cell(&row, github).and_then(|g| github_login(&g)),
            timezone: cell(&row, timezone),
            focus: cell(&row, focus)
                .map(|f| {
                    f.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// The login in a roster cell: `login`, `@login` or a profile URL.
fn github_login(cell: &str) -> Option<String> {
    let s = cell.trim();
    let s = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"))
        .unwrap_or(s);
    let s = s.trim_start_matches('@').trim_end_matches('/');
    valid_login(s).then(|| s.to_string())
}

/// The cells of a CSV text (RFC 4180): fields split at commas, quoted
/// fields may hold commas, line breaks and doubled quotes, and lines end
/// with LF or CRLF.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            (true, '"') => quoted = false,
            (true, c) => field.push(c),
            (false, '"') if field.is_empty() => quoted = true,
            (false, ',') => row.push(std::mem::take(&mut field)),
            (false, '\r') if chars.peek() == Some(&'\n') => {}
            (false, '\n') => {
                row.push(std::mem::take(&mut field));
                out.push(std::mem::take(&mut row));
            }
            (false, c) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        out.push(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "\u{feff}Name,Discord handle,GitHub,Time zone,Focus,Comments\r\n\
        Ann Example,ann_d,ann-example,Lisbon,\"App One, App Two\",\"likes \"\"quotes\"\"\"\r\n\
        Bo Sample,bo_d,https://github.com/bo-sample/,Oslo,App Three,\"two\nlines\"\r\n\
        No Login,nl_d,,Lima,,\r\n\
        ,,,,,\r\n\
        Cy Test,cy_d,@Cy-Test,,,\n";

    #[test]
    fn csv_fields_keep_quoted_commas_quotes_and_line_breaks() {
        let t = parse_csv("a,\"b,c\",\"d \"\"e\"\"\"\r\n\"f\ng\",h\nlast");
        assert_eq!(
            t,
            vec![vec!["a", "b,c", "d \"e\""], vec!["f\ng", "h"], vec!["last"]]
        );
        assert!(parse_csv("").is_empty());
    }

    #[test]
    fn roster_columns_are_found_by_header() {
        let r = rows(CSV);
        assert_eq!(r.len(), 4, "the empty row is left out");
        assert_eq!(
            r[0],
            RosterRow {
                name: Some("Ann Example".into()),
                discord: Some("ann_d".into()),
                github: Some("ann-example".into()),
                timezone: Some("Lisbon".into()),
                focus: vec!["App One".into(), "App Two".into()],
            }
        );
        assert_eq!(r[1].github.as_deref(), Some("bo-sample"));
        assert_eq!(r[2].github, None, "not linked");
        assert_eq!(r[3].github.as_deref(), Some("Cy-Test"));
        assert!(rows("").is_empty());
        // A username column before the display name is not the name.
        let other = rows("Slack Username,Display Name\nslack-one,Dee\n");
        assert_eq!(other[0].name.as_deref(), Some("Dee"));
    }

    #[test]
    fn roster_source_is_https_or_a_path() {
        assert_eq!(source(" "), Ok(Source::None));
        assert_eq!(
            source("https://example.com/r.csv"),
            Ok(Source::Url("https://example.com/r.csv".into()))
        );
        assert!(source("http://example.com/r.csv").is_err());
        assert_eq!(source("/tmp/r.csv"), Ok(Source::File("/tmp/r.csv".into())));
        if let Ok(Source::File(p)) = source("~/r.csv") {
            assert!(p.ends_with("r.csv") && p.is_absolute());
        } else {
            panic!("~/ is the home directory");
        }
    }

    #[test]
    fn a_downloaded_roster_is_kept_for_an_hour_and_when_a_download_fails() {
        let dir = std::env::temp_dir().join(format!("craft-roster-{}", std::process::id()));
        let url = "https://example.com/r.csv";
        let t0: Time = "2026-10-17T10:00:00Z".parse().unwrap();
        let first = load(url, &dir, t0, |u, etag| {
            assert_eq!((u, etag), (url, None));
            Ok(Fetched::Fresh {
                etag: Some("W/\"1\"".into()),
                text: CSV.into(),
            })
        });
        assert!(first.downloaded && first.error.is_none());
        assert_eq!(first.rows.len(), 4);
        // Within the hour: no download.
        let later = t0 + chrono::Duration::minutes(30);
        let kept = load(url, &dir, later, |_, _| panic!("downloaded again"));
        assert_eq!((kept.rows.len(), kept.downloaded), (4, false));
        // After it: the ETag goes along, and 304 reuses the kept copy.
        let hour = t0 + chrono::Duration::minutes(REFRESH_MINUTES);
        let same = load(url, &dir, hour, |_, etag| {
            assert_eq!(etag, Some("W/\"1\""));
            Ok(Fetched::NotModified)
        });
        assert_eq!((same.rows.len(), same.downloaded), (4, true));
        // A failed download keeps the copy and says why.
        let next = hour + chrono::Duration::minutes(REFRESH_MINUTES);
        let failed = load(url, &dir, next, |_, _| Err("offline".into()));
        assert_eq!(failed.rows.len(), 4);
        assert_eq!(failed.error.as_deref(), Some("roster: offline"));
        // Another source does not use this copy.
        let other = load("https://example.com/other.csv", &dir, later, |_, _| {
            Err("offline".into())
        });
        assert!(other.rows.is_empty() && other.error.is_some());
        // Reading only the kept copy never downloads, whatever its age.
        let far = next + chrono::Duration::days(3);
        let offline = load_kept(url, &dir, far);
        assert_eq!(
            (offline.rows.len(), offline.downloaded, offline.error),
            (4, false, None)
        );
        let none = load_kept("https://example.com/other.csv", &dir, far);
        assert!(none.rows.is_empty() && none.error.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_roster_file_is_read_each_time() {
        let dir = std::env::temp_dir().join(format!("craft-roster-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("team.csv");
        std::fs::write(&file, CSV).unwrap();
        let now: Time = "2026-10-17T10:00:00Z".parse().unwrap();
        let path = file.display().to_string();
        let read = load(&path, &dir, now, |_, _| panic!("a file is not downloaded"));
        assert_eq!(read.rows.len(), 4);
        std::fs::write(&file, "GitHub\nonly-one\n").unwrap();
        assert_eq!(load(&path, &dir, now, |_, _| unreachable!()).rows.len(), 1);
        let missing = load("/no/such/roster.csv", &dir, now, |_, _| unreachable!());
        assert!(missing.rows.is_empty() && missing.error.is_some());
        assert!(!dir.join("roster.json").exists(), "a file is not copied");
        std::fs::remove_dir_all(&dir).ok();
    }
}
