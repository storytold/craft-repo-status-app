//! `~/.craft_status_config.toml` (created from a commented template on first
//! launch) and `~/.craft_status_state.toml` (remembered window geometry).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{atomic_write, Error, Result};

const CONFIG_FILE_NAME: &str = ".craft_status_config.toml";
const STATE_FILE_NAME: &str = ".craft_status_state.toml";
pub const TEMPLATE: &str = include_str!("../../../craft_status_config.template.toml");

pub const DEFAULT_REPOS: &[&str] = &[
    "storytold/photocraft",
    "storytold/lightcraft",
    "storytold/filmcraft",
    "storytold/designcraft",
    "storytold/vectorcraft",
    "storytold/pdfcraft",
    "storytold/effectcraft",
    "storytold/wordcraft",
    "storytold/deckcraft",
    "storytold/gridcraft",
    "storytold/cadcraft",
    "storytold/soundcraft",
    "storytold/craft-libs",
    "storytold/craft-launcher",
    "storytold/figcraft",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `owner/name` or full `https://github.com/owner/name` URLs.
    pub repos: Vec<String>,
    pub poll_minutes: f64,
    pub stale_minutes: f64,
    /// Optional, used only when `CRAFT_STATUS_GITHUB_TOKEN`, `GITHUB_TOKEN`
    /// and `GH_TOKEN` are unset; otherwise `gh auth token`.
    pub github_token: String,
    /// Add a row for each repo where the viewer has an open pull request,
    /// when `repos` lists another repo of the same owner ([`Config::may_add`]).
    pub auto_repos: bool,
    /// Repos never added that way: `owner/name` or full GitHub URLs.
    pub hidden_auto_repos: Vec<String>,
    pub window: WindowConfig,
    pub tray: TrayConfig,
    pub shortcuts: Shortcuts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    pub width: f64,
    pub height: f64,
    pub always_on_top: bool,
    pub start_hidden: bool,
    pub opacity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrayConfig {
    pub hide_dock_icon: bool,
    pub visible_on_all_workspaces: bool,
    pub close_to_tray: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    pub toggle_window: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            repos: DEFAULT_REPOS.iter().map(|s| s.to_string()).collect(),
            poll_minutes: 30.0,
            stale_minutes: 60.0,
            github_token: String::new(),
            auto_repos: true,
            hidden_auto_repos: Vec::new(),
            window: WindowConfig::default(),
            tray: TrayConfig::default(),
            shortcuts: Shortcuts::default(),
        }
    }
}

impl Default for WindowConfig {
    fn default() -> Self {
        WindowConfig {
            width: 1340.0,
            height: 560.0,
            always_on_top: false,
            start_hidden: false,
            opacity: 0.96,
        }
    }
}

impl Default for TrayConfig {
    fn default() -> Self {
        TrayConfig {
            hide_dock_icon: true,
            visible_on_all_workspaces: true,
            close_to_tray: true,
        }
    }
}

impl Default for Shortcuts {
    fn default() -> Self {
        Shortcuts {
            toggle_window: "CmdOrCtrl+Shift+G".into(),
        }
    }
}

/// `https://github.com/o/r(.git)(/…)` or `o/r` → `o/r`.
pub fn normalize_repo(s: &str) -> Option<String> {
    let s = s.trim();
    let s = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"))
        .unwrap_or(s);
    let mut parts = s.split('/').filter(|p| !p.is_empty());
    let owner = parts.next()?;
    let name = parts.next()?.trim_end_matches(".git");
    let ok = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    (ok(owner) && ok(name)).then(|| format!("{owner}/{name}"))
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        Ok(dirs::home_dir()
            .ok_or(Error::NoHome)?
            .join(CONFIG_FILE_NAME))
    }

    pub fn parse(text: &str) -> Result<Config> {
        let mut c: Config = toml::from_str(text)?;
        c.poll_minutes = c.poll_minutes.clamp(1.0, 24.0 * 60.0);
        c.stale_minutes = c.stale_minutes.max(c.poll_minutes);
        c.window.opacity = c.window.opacity.clamp(0.2, 1.0);
        let mut repos: Vec<String> = Vec::new();
        for r in c.repos.iter().filter_map(|r| normalize_repo(r)) {
            if !repos.iter().any(|x| x.eq_ignore_ascii_case(&r)) {
                repos.push(r);
            }
        }
        c.repos = repos;
        Ok(c)
    }

    pub fn load_from(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text)
    }

    /// Load the config, writing the template first if there is none.
    pub fn load_or_create() -> Result<Config> {
        let path = Self::path()?;
        if !path.exists() {
            atomic_write(&path, TEMPLATE)?;
        }
        Self::load_from(&path)
    }

    pub fn poll_secs(&self) -> i64 {
        (self.poll_minutes * 60.0) as i64
    }

    pub fn stale_secs(&self) -> i64 {
        (self.stale_minutes * 60.0) as i64
    }
}

/// A repo list in the config file that a row added by the poller writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoList {
    /// `repos`: the row's Keep.
    Watched,
    /// `hidden_auto_repos`: the row's Hide.
    Hidden,
}

impl RepoList {
    fn key(self) -> &'static str {
        match self {
            RepoList::Watched => "repos",
            RepoList::Hidden => "hidden_auto_repos",
        }
    }

    fn of(self, config: &Config) -> &[String] {
        match self {
            RepoList::Watched => &config.repos,
            RepoList::Hidden => &config.hidden_auto_repos,
        }
    }
}

impl Config {
    /// The owners of the configured repos, each once, in config order.
    pub fn owners(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (owner, _) in self.repos.iter().filter_map(|r| r.split_once('/')) {
            if !out.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
                out.push(owner.to_string());
            }
        }
        out
    }

    /// Whether the poller may add `repo` to the table on its own:
    /// `auto_repos` is on, `repos` lists another repo of the same owner, and
    /// neither `repos` nor `hidden_auto_repos` lists `repo`.
    pub fn may_add(&self, repo: &str) -> bool {
        let Some(repo) = normalize_repo(repo) else {
            return false;
        };
        let owner = repo.split('/').next().unwrap_or_default();
        self.auto_repos
            && self.owners().iter().any(|o| o.eq_ignore_ascii_case(owner))
            && !lists(&self.repos, &repo)
            && !lists(&self.hidden_auto_repos, &repo)
    }

    /// Adds `repo` to `list` in the config file at `path` (written from the
    /// template first when there is none) with [`add_to_list`], through a
    /// temporary file and a rename. A symlink is followed, and the file keeps
    /// its permissions: the temporary file gets them before any text goes in.
    pub fn add_to_file(path: &Path, list: RepoList, repo: &str) -> Result<()> {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let io = |source| Error::Io {
            path: path.display().to_string(),
            source,
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return atomic_write(&path, &add_to_list(TEMPLATE, list, repo)?);
            }
            Err(e) => return Err(io(e)),
        };
        let out = add_to_list(&text, list, repo)?;
        let mode = std::fs::metadata(&path).map_err(io)?.permissions();
        let tmp = path.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(io)?;
        file.set_permissions(mode).map_err(io)?;
        std::io::Write::write_all(&mut file, out.as_bytes()).map_err(io)?;
        drop(file);
        std::fs::rename(&tmp, &path).map_err(io)
    }
}

/// Whether `list` holds `repo` (`owner/name`), in any spelling.
fn lists(list: &[String], repo: &str) -> bool {
    list.iter()
        .filter_map(|r| normalize_repo(r))
        .any(|r| r.eq_ignore_ascii_case(repo))
}

/// `text`, a config file, with `repo` added to `list`. Comments and
/// formatting stay; on a list written one repo per line, the new repo gets a
/// line of its own and a trailing comma, and a comment on the last repo's
/// line stays on that line. A list the file lacks is written with the repos
/// it reads as (for `repos`, the defaults), then `repo`, above the first
/// table. A repo the list already holds is not added again.
pub fn add_to_list(text: &str, list: RepoList, repo: &str) -> Result<String> {
    use toml_edit::{Array, DocumentMut, Item, RawString, TomlError, Value};
    let invalid = |what: &str, e: String| Error::Io {
        path: what.to_string(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    };
    let raw = |s: Option<&RawString>| {
        s.and_then(RawString::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let held = Config::parse(text)?;
    let repo = normalize_repo(repo)
        .ok_or_else(|| invalid(repo, "not owner/name or a GitHub URL".into()))?;
    let held = list.of(&held);
    if lists(held, &repo) {
        return Ok(text.to_string());
    }
    let wanted = repo.clone();
    // `Config::parse` has read the same text, so this is not expected to fail.
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: TomlError| invalid("config", e.to_string()))?;
    match doc.get_mut(list.key()).and_then(Item::as_array_mut) {
        Some(array) if !array.is_empty() => {
            // On a list without a trailing comma, the line break after the
            // last repo moves after a new comma.
            let last = array.iter().last();
            let suffix = raw(last.and_then(|v| v.decor().suffix()));
            if suffix.contains('\n') {
                if let Some(last) = array.iter_mut().last() {
                    last.decor_mut().set_suffix("");
                }
                let trailing = raw(Some(array.trailing()));
                array.set_trailing(format!("{suffix}{trailing}"));
                array.set_trailing_comma(true);
            }
            // The indent of the last repo, without any comment above it.
            let indent = array
                .iter()
                .last()
                .and_then(|v| v.decor().prefix())
                .and_then(|p| p.as_str())
                .and_then(|p| p.rfind('\n').map(|i| p[i..].to_string()))
                .unwrap_or_else(|| " ".to_string());
            // A comment on the last repo's line stays on that line.
            let trailing = raw(Some(array.trailing()));
            let (line, rest) = trailing.split_at(trailing.find('\n').unwrap_or(trailing.len()));
            let mut value = Value::from(repo);
            if line.contains('#') {
                let indent = if indent.starts_with('\n') {
                    indent
                } else {
                    "\n  ".to_string()
                };
                value.decor_mut().set_prefix(format!("{line}{indent}"));
                array.set_trailing(rest);
            } else {
                value.decor_mut().set_prefix(indent);
            }
            array.push_formatted(value);
        }
        Some(array) => array.push(repo),
        None => {
            let mut array = Array::new();
            for r in held.iter().cloned().chain([repo]) {
                let mut value = Value::from(r);
                value.decor_mut().set_prefix("\n  ");
                array.push_formatted(value);
            }
            array.set_trailing_comma(true);
            array.set_trailing("\n");
            doc.insert(list.key(), Item::Value(Value::Array(array)));
            // The new list goes above the file's first table, with a blank
            // line between them.
            let first_table = doc
                .as_table_mut()
                .iter_mut()
                .filter_map(|(_, item)| item.as_table_mut())
                .filter(|t| !t.is_implicit())
                .min_by_key(|t| t.position());
            if let Some(table) = first_table {
                let prefix = raw(table.decor().prefix());
                if !prefix.starts_with('\n') {
                    table.decor_mut().set_prefix(format!("\n{prefix}"));
                }
            }
        }
    }
    let out = doc.to_string();
    // Fail when the new text does not list the repo.
    if !lists(list.of(&Config::parse(&out)?), &wanted) {
        return Err(invalid(
            "config",
            format!("could not add {wanted} to {}", list.key()),
        ));
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
struct StateFile {
    window: Option<WindowState>,
}

impl WindowState {
    pub fn path() -> Result<PathBuf> {
        Ok(dirs::home_dir().ok_or(Error::NoHome)?.join(STATE_FILE_NAME))
    }

    pub fn load() -> Option<WindowState> {
        let text = std::fs::read_to_string(Self::path().ok()?).ok()?;
        let f: StateFile = toml::from_str(&text).ok()?;
        f.window.filter(|w| w.width > 0 && w.height > 0)
    }

    pub fn save(&self) -> Result<()> {
        let text = format!(
            "# Remembered by Craft Status (window position and size). Safe to delete.\n{}",
            toml::to_string(&StateFile {
                window: Some(*self)
            })
            .unwrap_or_default()
        );
        atomic_write(&Self::path()?, &text)
    }

    /// Whether enough of the window to grab is on one of the monitors `(x, y, w, h)`.
    pub fn visible_on(&self, monitors: &[(i32, i32, u32, u32)]) -> bool {
        const GRAB: i32 = 40;
        monitors.iter().any(|&(mx, my, mw, mh)| {
            let (mr, mb) = (mx + mw as i32, my + mh as i32);
            let (wr, wb) = (self.x + self.width as i32, self.y + self.height as i32);
            wr.min(mr) - self.x.max(mx) >= GRAB && wb.min(mb) - self.y.max(my) >= GRAB
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_defaults() {
        assert_eq!(Config::parse(TEMPLATE).unwrap(), Config::default());
    }

    #[test]
    fn empty_file_is_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn repos_are_normalized_and_deduped() {
        let c = Config::parse(
            r#"repos = ["https://github.com/storytold/photocraft", "storytold/photocraft.git",
                        "github.com/a/b/pulls", "nonsense", "x/y z"]"#,
        )
        .unwrap();
        assert_eq!(c.repos, vec!["storytold/photocraft", "a/b"]);
    }

    #[test]
    fn intervals_are_clamped() {
        let c = Config::parse("poll_minutes = 0.1\nstale_minutes = 0.5").unwrap();
        assert_eq!(c.poll_minutes, 1.0);
        assert_eq!(
            c.stale_minutes, 1.0,
            "never stale before the next poll is due"
        );
        assert_eq!(c.poll_secs(), 60);
    }

    #[test]
    fn window_state_visibility() {
        let w = WindowState {
            x: 100,
            y: 100,
            width: 800,
            height: 500,
        };
        assert!(w.visible_on(&[(0, 0, 1920, 1080)]));
        assert!(!w.visible_on(&[(2000, 0, 1920, 1080)]));
    }

    #[test]
    fn repos_are_added_only_under_a_configured_owner() {
        let c = Config::parse(
            r#"repos = ["https://github.com/Acme/one", "acme/two", "other-org/lib"]
               hidden_auto_repos = ["https://github.com/acme/hidden"]"#,
        )
        .unwrap();
        assert_eq!(c.owners(), vec!["Acme", "other-org"]);
        assert!(c.auto_repos, "on by default");
        assert!(c.may_add("acme/three"));
        assert!(c.may_add("OTHER-ORG/tool"));
        assert!(!c.may_add("stranger/three"), "an owner the config lacks");
        assert!(!c.may_add("ACME/Two"), "already configured");
        assert!(!c.may_add("acme/hidden"));
        assert!(!c.may_add("not a repo"));
        let off = Config::parse("auto_repos = false\nrepos = [\"acme/one\"]").unwrap();
        assert!(!off.may_add("acme/three"));
    }

    #[test]
    fn keep_and_hide_add_to_the_lists_and_keep_the_comments() {
        let kept = add_to_list(
            TEMPLATE,
            RepoList::Watched,
            "https://github.com/storytold/newcraft",
        )
        .unwrap();
        assert!(kept.contains("# Repositories to watch"));
        assert!(kept.contains("  \"storytold/figcraft\",\n  \"storytold/newcraft\",\n]"));
        let c = Config::parse(&kept).unwrap();
        assert_eq!(
            c.repos.last().map(String::as_str),
            Some("storytold/newcraft")
        );
        assert_eq!(c.repos.len(), DEFAULT_REPOS.len() + 1);
        assert_eq!(
            add_to_list(&kept, RepoList::Watched, "Storytold/NewCraft").unwrap(),
            kept,
            "already listed"
        );

        let hidden = add_to_list(&kept, RepoList::Hidden, "storytold/scratch").unwrap();
        assert!(hidden.contains("hidden_auto_repos = [\"storytold/scratch\"]"));
        let c = Config::parse(&hidden).unwrap();
        assert_eq!(c.hidden_auto_repos, vec!["storytold/scratch"]);
        assert!(!c.may_add("storytold/scratch"));

        // A comment above the last repo stays where it is, once.
        let commented = "repos = [\n  \"a/one\",\n  # libraries\n  \"a/lib\",\n]\n";
        assert_eq!(
            add_to_list(commented, RepoList::Watched, "a/two").unwrap(),
            "repos = [\n  \"a/one\",\n  # libraries\n  \"a/lib\",\n  \"a/two\",\n]\n"
        );
        assert_eq!(
            add_to_list("repos = [\"a/one\"]\n", RepoList::Watched, "a/two").unwrap(),
            "repos = [\"a/one\", \"a/two\"]\n"
        );
    }

    #[test]
    fn a_list_without_a_trailing_comma_or_with_a_comment_on_its_last_line_keeps_its_shape() {
        let add = |text: &str| add_to_list(text, RepoList::Watched, "a/two").unwrap();
        assert_eq!(
            add("repos = [\n  \"a/one\",\n  \"a/lib\"\n]\n"),
            "repos = [\n  \"a/one\",\n  \"a/lib\",\n  \"a/two\",\n]\n"
        );
        assert_eq!(
            add("repos = [\n  \"a/one\",\n  \"a/lib\", # libraries\n]\n"),
            "repos = [\n  \"a/one\",\n  \"a/lib\", # libraries\n  \"a/two\",\n]\n",
            "the comment stays on the line of the repo it follows"
        );
        assert_eq!(
            add("repos = [\n  \"a/one\",\n  \"a/lib\" # libraries\n]\n"),
            "repos = [\n  \"a/one\",\n  \"a/lib\", # libraries\n  \"a/two\",\n]\n"
        );
        assert_eq!(
            add("repos = [\n  \"a/one\",\n  \"a/lib\", # libraries\n  # the end\n]\n"),
            "repos = [\n  \"a/one\",\n  \"a/lib\", # libraries\n  \"a/two\",\n  # the end\n]\n"
        );
        // The last repo on the bracket's line, with a comment after it.
        for text in [
            "repos = [\"a/one\", # one\n]\n",
            "repos = [\"a/one\" # one\n]\n",
        ] {
            let out = add(text);
            assert_eq!(out, "repos = [\"a/one\", # one\n  \"a/two\",\n]\n");
            assert_eq!(Config::parse(&out).unwrap().repos, ["a/one", "a/two"]);
        }
    }

    #[test]
    fn a_list_the_file_lacks_is_followed_by_a_blank_line_before_a_table() {
        let out = add_to_list("[window]\nwidth = 900.0\n", RepoList::Hidden, "a/two").unwrap();
        assert_eq!(
            out,
            "hidden_auto_repos = [\n  \"a/two\",\n]\n\n[window]\nwidth = 900.0\n"
        );
        let spaced = "poll_minutes = 5.0\n\n[window]\nwidth = 900.0\n";
        let out = add_to_list(spaced, RepoList::Hidden, "a/two").unwrap();
        assert_eq!(
            out,
            "poll_minutes = 5.0\nhidden_auto_repos = [\n  \"a/two\",\n]\n\n[window]\nwidth = 900.0\n",
            "one blank line, not two"
        );
    }

    #[test]
    fn a_list_the_file_lacks_starts_from_the_repos_it_reads_as() {
        let text = "# mine\npoll_minutes = 5.0\n\n[window]\nwidth = 900.0\n";
        let out = add_to_list(text, RepoList::Watched, "storytold/newcraft").unwrap();
        assert!(out.starts_with("# mine\npoll_minutes = 5.0\n"));
        let c = Config::parse(&out).unwrap();
        let mut want: Vec<String> = DEFAULT_REPOS.iter().map(|s| s.to_string()).collect();
        want.push("storytold/newcraft".into());
        assert_eq!(c.repos, want, "the defaults stay");
        assert_eq!((c.poll_minutes, c.window.width), (5.0, 900.0));
        let out = add_to_list(text, RepoList::Hidden, "storytold/scratch").unwrap();
        assert_eq!(
            Config::parse(&out).unwrap().hidden_auto_repos,
            vec!["storytold/scratch"]
        );
        assert!(add_to_list("repos = [", RepoList::Watched, "a/b").is_err());
        assert!(add_to_list("", RepoList::Watched, "not a repo").is_err());
    }

    #[test]
    fn keep_and_hide_survive_a_reload_and_keep_the_files_permissions() {
        let dir = std::env::temp_dir().join(format!("craft-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "# keep me\nrepos = [\"acme/one\"]\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        Config::add_to_file(&path, RepoList::Watched, "acme/two").unwrap();
        Config::add_to_file(&path, RepoList::Hidden, "acme/three").unwrap();
        let c = Config::load_from(&path).unwrap();
        assert_eq!(c.repos, vec!["acme/one", "acme/two"]);
        assert_eq!(c.hidden_auto_repos, vec!["acme/three"]);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .starts_with("# keep me\n"));
        assert!(!path.with_extension("tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            // Through a symlink, the file it points at is written.
            let link = dir.join("link.toml");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            Config::add_to_file(&link, RepoList::Watched, "acme/four").unwrap();
            assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
            assert_eq!(Config::load_from(&path).unwrap().repos.len(), 3);
        }
        let fresh = dir.join("missing.toml");
        Config::add_to_file(&fresh, RepoList::Hidden, "storytold/scratch").unwrap();
        let c = Config::load_from(&fresh).unwrap();
        assert_eq!(
            c.repos.len(),
            DEFAULT_REPOS.len(),
            "written from the template"
        );
        assert_eq!(c.hidden_auto_repos, vec!["storytold/scratch"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
