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
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `owner/name` or full `https://github.com/owner/name` URLs.
    pub repos: Vec<String>,
    pub poll_minutes: f64,
    pub stale_minutes: f64,
    /// Optional. Otherwise `GITHUB_TOKEN` / `GH_TOKEN`, otherwise `gh auth token`.
    pub github_token: String,
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
            poll_minutes: 5.0,
            stale_minutes: 15.0,
            github_token: String::new(),
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
}
