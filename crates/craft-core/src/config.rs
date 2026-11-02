//! `~/.craft_status_config.toml` (created from a commented template on first
//! launch) and `~/.craft_status_state.toml` (remembered window geometry).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::people::valid_login;
use crate::{atomic_write, Error, Result};

const CONFIG_FILE_NAME: &str = ".craft_status_config.toml";
const STATE_FILE_NAME: &str = ".craft_status_state.toml";
pub const TEMPLATE: &str = include_str!("../../../craft_status_config.template.toml");

/// What a repository is, shown beside it in both apps' tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RepoCategory {
    App,
    Library,
    Assets,
    Ci,
    Tooling,
    /// Anything not in [`DEFAULT_REPOS`], such as a repo added by hand.
    #[default]
    Other,
}

/// The repos watched by default, in the template's order, and what each one is.
pub const DEFAULT_REPOS: &[(&str, RepoCategory)] = &[
    ("storytold/photocraft", RepoCategory::App),
    ("storytold/lightcraft", RepoCategory::App),
    ("storytold/filmcraft", RepoCategory::App),
    ("storytold/designcraft", RepoCategory::App),
    ("storytold/vectorcraft", RepoCategory::App),
    ("storytold/pdfcraft", RepoCategory::App),
    ("storytold/effectcraft", RepoCategory::App),
    ("storytold/wordcraft", RepoCategory::App),
    ("storytold/deckcraft", RepoCategory::App),
    ("storytold/gridcraft", RepoCategory::App),
    ("storytold/cadcraft", RepoCategory::App),
    ("storytold/soundcraft", RepoCategory::App),
    ("storytold/figcraft", RepoCategory::App),
    ("storytold/craft-launcher", RepoCategory::App),
    ("storytold/craft-anything", RepoCategory::App),
    ("tangency-labs/solidcraft", RepoCategory::App),
    ("storytold/craft-libs", RepoCategory::Library),
    ("storytold/craft-i18n", RepoCategory::Library),
    ("storytold/craft-ai-integrations", RepoCategory::Library),
    ("storytold/indd-utils", RepoCategory::Library),
    ("storytold/craft-fonts", RepoCategory::Assets),
    ("storytold/photocraft-corpus", RepoCategory::Assets),
    ("storytold/craft-launcher-assets", RepoCategory::Assets),
    ("storytold/craft-common-actions", RepoCategory::Ci),
    ("storytold/craft-contrib", RepoCategory::Tooling),
    ("storytold/craft-skills", RepoCategory::Tooling),
];

/// What `repo` (`owner/name`, any case) is; [`RepoCategory::Other`] if unknown.
pub fn category_of(repo: &str) -> RepoCategory {
    DEFAULT_REPOS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(repo))
        .map_or(RepoCategory::Other, |(_, category)| *category)
}

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
    pub window: WindowConfig,
    pub tray: TrayConfig,
    pub shortcuts: Shortcuts,
    pub people: PeopleConfig,
}

/// The `[people]` table: where the team roster comes from, and who owns and
/// maintains each repo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PeopleConfig {
    /// An `https://` URL of the roster sheet's CSV export, or a CSV file
    /// path; empty for none. The app reads it at runtime.
    pub roster: String,
    /// The owners of every repo that `repo_owners` does not list.
    pub owner_logins: Vec<String>,
    /// Owners by repo (`owner/name`), in place of `owner_logins`.
    pub repo_owners: BTreeMap<String, Vec<String>>,
    /// Logins counted as maintainers of every repo, besides the ones found
    /// from membership, merges and closes.
    pub maintainers: Vec<String>,
    /// Repos (`owner/name`) whose maintainers rebase contributors' branches
    /// themselves: there a conflict on a contributor's pull request that
    /// maintainers can push to is only a flag.
    pub rebase_repos: Vec<String>,
    /// Repos (`owner/name`) whose maintainers land contributors' work
    /// through a batch branch, so a landed pull request may close without
    /// merging. The rankings mark their merge and landed counts unverified.
    pub batch_repos: Vec<String>,
}

/// The repos whose maintainers rebase contributors' branches, by default.
pub const DEFAULT_REBASE_REPOS: &[&str] = &["storytold/vectorcraft", "storytold/figcraft"];
/// The repos that land work through a batch branch, by default.
pub const DEFAULT_BATCH_REPOS: &[&str] = &["storytold/designcraft", "storytold/deckcraft"];

impl Default for PeopleConfig {
    fn default() -> Self {
        PeopleConfig {
            roster: String::new(),
            owner_logins: Vec::new(),
            repo_owners: BTreeMap::new(),
            maintainers: Vec::new(),
            rebase_repos: DEFAULT_REBASE_REPOS.iter().map(|s| s.to_string()).collect(),
            batch_repos: DEFAULT_BATCH_REPOS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl PeopleConfig {
    /// The owners of `repo`: its `repo_owners` entry, in any letter case,
    /// otherwise `owner_logins`.
    pub fn owners_for(&self, repo: &str) -> &[String] {
        self.repo_owners
            .iter()
            .find(|(r, _)| r.eq_ignore_ascii_case(repo))
            .map_or(&self.owner_logins, |(_, o)| o)
    }

    /// Whether `repo`'s maintainers rebase contributors' branches, in any
    /// letter case.
    pub fn rebases(&self, repo: &str) -> bool {
        self.rebase_repos
            .iter()
            .any(|r| r.eq_ignore_ascii_case(repo))
    }

    /// Whether `repo` lands work through a batch branch, in any letter case.
    pub fn batches(&self, repo: &str) -> bool {
        self.batch_repos
            .iter()
            .any(|r| r.eq_ignore_ascii_case(repo))
    }

    /// This table as [`Config::parse`] reads it: the roster trimmed, logins
    /// trimmed with invalid ones and repeats dropped, and `repo_owners`
    /// keyed by `owner/name`, a repo listed twice keeping its first entry.
    fn normalized(&self) -> PeopleConfig {
        let mut repo_owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (repo, list) in &self.repo_owners {
            if let Some(n) = normalize_repo(repo) {
                if !repo_owners.keys().any(|k| k.eq_ignore_ascii_case(&n)) {
                    repo_owners.insert(n, logins(list));
                }
            }
        }
        PeopleConfig {
            roster: self.roster.trim().to_string(),
            owner_logins: logins(&self.owner_logins),
            repo_owners,
            maintainers: logins(&self.maintainers),
            rebase_repos: repo_list(&self.rebase_repos),
            batch_repos: repo_list(&self.batch_repos),
        }
    }
}

/// `list` as `owner/name`, without entries that are not repos and without
/// repeats in any letter case.
fn repo_list(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in list.iter().filter_map(|r| normalize_repo(r)) {
        if !out.iter().any(|x| x.eq_ignore_ascii_case(&r)) {
            out.push(r);
        }
    }
    out
}

/// `list` trimmed, without invalid logins and without repeats in any letter case.
fn logins(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in list.iter().map(|l| l.trim()) {
        if valid_login(l) && !out.iter().any(|o| o.eq_ignore_ascii_case(l)) {
            out.push(l.to_string());
        }
    }
    out
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
            repos: DEFAULT_REPOS.iter().map(|(r, _)| r.to_string()).collect(),
            poll_minutes: 30.0,
            stale_minutes: 60.0,
            github_token: String::new(),
            window: WindowConfig::default(),
            tray: TrayConfig::default(),
            shortcuts: Shortcuts::default(),
            people: PeopleConfig::default(),
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
        c.people = c.people.normalized();
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
    fn every_default_repo_has_a_category() {
        for (repo, category) in DEFAULT_REPOS {
            assert_ne!(*category, RepoCategory::Other, "{repo}");
        }
    }

    #[test]
    fn categories_ignore_case_and_unknown_repos_are_other() {
        assert_eq!(category_of("Storytold/Craft-Fonts"), RepoCategory::Assets);
        assert_eq!(
            category_of("storytold/craft-common-actions"),
            RepoCategory::Ci
        );
        assert_eq!(category_of("someone/else"), RepoCategory::Other);
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
    fn people_has_no_owners_by_default_and_normalizes_logins_and_repos() {
        let c = Config::parse("").unwrap();
        assert!(c.people.owner_logins.is_empty());
        assert!(c.people.owners_for("storytold/vectorcraft").is_empty());
        let c = Config::parse(
            "[people]\nroster = \" ~/team.csv \"\nowner_logins = [\" Ann \", \"ann\", \"bad login\", \"bo\"]\n\
             [people.repo_owners]\n\"https://github.com/o/One\" = [\"cy\"]\n\"o/two\" = []\n\"nonsense\" = [\"x\"]\n",
        )
        .unwrap();
        assert_eq!(c.people.roster, "~/team.csv");
        assert_eq!(c.people.owner_logins, ["Ann", "bo"]);
        assert_eq!(
            c.people.repo_owners.keys().collect::<Vec<_>>(),
            ["o/One", "o/two"]
        );
        assert_eq!(c.people.owners_for("O/one"), ["cy"]);
        assert!(
            c.people.owners_for("o/two").is_empty(),
            "an empty entry means no owners"
        );
        assert_eq!(c.people.owners_for("o/three"), ["Ann", "bo"]);
        assert!(
            c.people.rebases("Storytold/VectorCraft"),
            "the default list"
        );
        assert!(!c.people.rebases("storytold/photocraft"));
        let c = Config::parse(
            "[people]\nrebase_repos = [\"https://github.com/o/One\", \"o/one\", \"nonsense\"]\n",
        )
        .unwrap();
        assert_eq!(c.people.rebase_repos, ["o/One"]);
        assert!(!c.people.rebases("storytold/vectorcraft"));
        assert!(
            c.people.batches("Storytold/DesignCraft"),
            "the default list"
        );
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
}
