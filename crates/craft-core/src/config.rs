//! `~/.craft_status_config.toml` (created from a commented template on first
//! launch) and `~/.craft_status_state.toml` (remembered window geometry).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value};

use crate::{atomic_write, atomic_write_private, Error, Result};

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

/// The longest poll interval, in minutes.
pub const MAX_POLL_MINUTES: f64 = 24.0 * 60.0;
/// The longest stale threshold a save accepts, in minutes (a week).
pub const MAX_STALE_MINUTES: f64 = 7.0 * 24.0 * 60.0;
/// The window's smallest size, as `minWidth` and `minHeight` in tauri.conf.json.
pub const MIN_WIDTH: f64 = 520.0;
pub const MIN_HEIGHT: f64 = 240.0;
pub const MAX_SIZE: f64 = 10_000.0;
pub const MIN_OPACITY: f64 = 0.2;

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
        c.poll_minutes = c.poll_minutes.clamp(1.0, MAX_POLL_MINUTES);
        c.stale_minutes = c.stale_minutes.max(c.poll_minutes);
        c.window.opacity = c.window.opacity.clamp(MIN_OPACITY, 1.0);
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

    /// What keeps this config from being saved, one `field: problem` each.
    /// [`Config::parse`] clamps the same values in a hand-edited file instead.
    /// The shortcut is checked by the app, which has the parser for it.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for r in &self.repos {
            match normalize_repo(r) {
                None => out.push(format!("repos: {r:?} is not owner/name or a GitHub URL")),
                Some(n) if seen.iter().any(|s| s.eq_ignore_ascii_case(&n)) => {
                    out.push(format!("repos: {n} is listed twice"))
                }
                Some(n) => seen.push(n),
            }
        }
        let mut range = |field: &str, v: f64, lo: f64, hi: f64| {
            if !(lo..=hi).contains(&v) {
                out.push(format!("{field}: must be from {lo} to {hi}"));
            }
        };
        range("poll_minutes", self.poll_minutes, 1.0, MAX_POLL_MINUTES);
        range(
            "stale_minutes",
            self.stale_minutes,
            self.poll_minutes.max(1.0),
            MAX_STALE_MINUTES,
        );
        range("window.width", self.window.width, MIN_WIDTH, MAX_SIZE);
        range("window.height", self.window.height, MIN_HEIGHT, MAX_SIZE);
        range("window.opacity", self.window.opacity, MIN_OPACITY, 1.0);
        out
    }

    /// `text`, a config file, with this config's values written into it.
    /// Comments, formatting, unknown keys and unchanged values stay as they
    /// are. A key the file lacks is added only when its value differs from
    /// the default.
    pub fn update_text(&self, text: &str) -> Result<String> {
        let new: DocumentMut = toml::to_string(self)?.parse()?;
        write_into(text, new.as_table())
    }

    /// This config's changes from `base`, applied to `current`: each value
    /// that differs from `base` replaces `current`'s, and every other value
    /// stays as `current` has it. The repo list counts as one value. The
    /// Settings view's save passes the values the view opened with as
    /// `base` and the file's values as `current`, so edits made to the file
    /// since then stay unless the view changed the same value.
    pub fn rebase(&self, base: &Config, current: &Config) -> Result<Config> {
        let mut out = toml::Value::try_from(current)?;
        overlay(
            &mut out,
            &toml::Value::try_from(self)?,
            &toml::Value::try_from(base)?,
        );
        Ok(out.try_into()?)
    }

    /// Checks this config with [`Config::problems`] and writes it into the
    /// file at `path` (into the template when there is none) with
    /// [`Config::update_text`], atomically. Repos are written as
    /// `owner/name`, except those the file already lists in another form.
    /// A value the file holds in a form that reads as this config's, such
    /// as an interval that [`Config::parse`] clamps or a repo listed twice,
    /// stays as the file has it.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let problems = self.problems();
        if !problems.is_empty() {
            return Err(Error::Invalid(problems));
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => TEMPLATE.to_string(),
            Err(source) => {
                return Err(Error::Io {
                    path: path.display().to_string(),
                    source,
                })
            }
        };
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Listed {
            repos: Vec<String>,
        }
        let listed = toml::from_str::<Listed>(&text).unwrap_or_default().repos;
        let mut saved = self.clone();
        saved.github_token = saved.github_token.trim().to_string();
        saved.repos = self
            .repos
            .iter()
            .filter_map(|r| normalize_repo(r))
            .collect();
        let spell = |repos: &[String]| -> Vec<String> {
            repos
                .iter()
                .map(|n| {
                    listed
                        .iter()
                        .find(|l| normalize_repo(l).as_ref() == Some(n))
                        .unwrap_or(n)
                        .clone()
                })
                .collect()
        };
        let mut written = saved.clone();
        written.repos = spell(&saved.repos);
        let mut out = written.update_text(&text)?;
        // Leave out the values the file already reads as, and put back each
        // one whose absence makes the file read as another value.
        if let Ok(mut read) = Config::parse(&text) {
            read.repos = spell(&read.repos);
            let full: DocumentMut = toml::to_string(&written)?.parse()?;
            let want: DocumentMut = toml::to_string(&saved)?.parse()?;
            let old: DocumentMut = toml::to_string(&read)?.parse()?;
            let mut new = full.clone();
            prune(new.as_table_mut(), old.as_table());
            loop {
                let kept = write_into(&text, new.as_table())?;
                let Ok(back) = Config::parse(&kept) else {
                    break;
                };
                if back == saved {
                    out = kept;
                    break;
                }
                let back: DocumentMut = toml::to_string(&back)?.parse()?;
                if !restore(
                    new.as_table_mut(),
                    full.as_table(),
                    back.as_table(),
                    want.as_table(),
                ) {
                    break;
                }
            }
        }
        if Config::parse(&out)? != saved {
            return Err(Error::Invalid(vec![
                "the edited file does not read back as the values saved".into(),
            ]));
        }
        if saved.github_token.is_empty() {
            atomic_write(path, &out)
        } else {
            atomic_write_private(path, &out)
        }
    }
}

/// Puts each value of `new` that differs from the one at the same place in
/// `base` into `dst`; see [`Config::rebase`].
fn overlay(dst: &mut toml::Value, new: &toml::Value, base: &toml::Value) {
    match (dst, new, base) {
        (toml::Value::Table(dst), toml::Value::Table(new), toml::Value::Table(base)) => {
            for (key, item) in new {
                match (dst.get_mut(key), base.get(key)) {
                    (Some(old), Some(was)) => overlay(old, item, was),
                    (_, was) => {
                        if was != Some(item) {
                            dst.insert(key.clone(), item.clone());
                        }
                    }
                }
            }
        }
        (dst, new, base) => {
            if new != base {
                *dst = new.clone();
            }
        }
    }
}

/// `text`, a config file, with `new`'s values written into it; see
/// [`Config::update_text`].
fn write_into(text: &str, new: &dyn TableLike) -> Result<String> {
    let mut doc: DocumentMut = text.parse()?;
    let defaults: DocumentMut = toml::to_string(&Config::default())?.parse()?;
    merge(doc.as_table_mut(), new, Some(defaults.as_table()));
    Ok(doc.to_string())
}

/// Removes from `new` each value that `old` holds at the same place, and
/// each table that is left empty.
fn prune(new: &mut dyn TableLike, old: &dyn TableLike) {
    let keys: Vec<String> = new.iter().map(|(k, _)| k.to_string()).collect();
    for key in keys {
        let (Some(item), Some(was)) = (new.get_mut(&key), old.get(&key)) else {
            continue;
        };
        let same = same_item(item, was)
            || match (item.as_table_like_mut(), was.as_table_like()) {
                (Some(table), Some(was)) => {
                    prune(table, was);
                    table.is_empty()
                }
                _ => false,
            };
        if same {
            new.remove(&key);
        }
    }
}

/// Puts into `new` the value from `full` at each place where `back` differs
/// from `want`, and returns whether `new` changed.
fn restore(
    new: &mut dyn TableLike,
    full: &dyn TableLike,
    back: &dyn TableLike,
    want: &dyn TableLike,
) -> bool {
    let mut changed = false;
    for (key, item) in want.iter() {
        let was = back.get(key);
        if was.is_some_and(|b| same_item(b, item)) {
            continue;
        }
        let Some(from) = full.get(key) else {
            continue;
        };
        match (
            item.as_table_like(),
            was.and_then(Item::as_table_like),
            from.as_table_like(),
        ) {
            (Some(item), Some(was), Some(from)) => {
                if new.get(key).is_none() {
                    new.insert(key, Item::Table(Table::new()));
                }
                if let Some(table) = new.get_mut(key).and_then(Item::as_table_like_mut) {
                    changed |= restore(table, from, was, item);
                }
            }
            _ => {
                if !new.get(key).is_some_and(|n| same_item(n, from)) {
                    new.insert(key, from.clone());
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Writes `src`'s values into `dst`, keeping `dst`'s formatting; see
/// [`Config::update_text`]. `defaults` holds the default values at the same
/// place, for keys `dst` lacks.
fn merge(dst: &mut dyn TableLike, src: &dyn TableLike, defaults: Option<&dyn TableLike>) {
    for (key, item) in src.iter() {
        let default = defaults.and_then(|d| d.get(key));
        match dst.get_mut(key) {
            Some(old) => match (old.as_table_like_mut(), item.as_table_like()) {
                (Some(old), Some(new)) => merge(old, new, default.and_then(Item::as_table_like)),
                _ => replace(old, item),
            },
            None => {
                if let Some(new) = item.as_table_like() {
                    let mut table = Table::new();
                    merge(&mut table, new, default.and_then(Item::as_table_like));
                    if !table.is_empty() {
                        dst.insert(key, Item::Table(table));
                    }
                } else if !default.is_some_and(|d| same_item(d, item)) {
                    dst.insert(key, item.clone());
                }
            }
        }
    }
}

/// Puts `new` in place of `old` unless they hold the same value. A value
/// keeps `old`'s spacing and trailing comment, and an array keeps the layout
/// and comments of the elements it still has.
fn replace(old: &mut Item, new: &Item) {
    if same_item(old, new) {
        return;
    }
    match (old, new) {
        (Item::Value(old), Item::Value(new)) => {
            let mut value = match (&*old, new) {
                (Value::Array(o), Value::Array(n)) => Value::Array(relist(o, n)),
                _ => new.clone(),
            };
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        (old, new) => *old = new.clone(),
    }
}

/// `new`'s elements laid out like `old`'s: one per line if `old` spans lines,
/// each keeping the comments around an equal element of `old`.
fn relist(old: &Array, new: &Array) -> Array {
    // The indent of the first element that starts a line.
    let indent = old
        .iter()
        .filter_map(|v| v.decor().prefix().and_then(|p| p.as_str()))
        .find_map(|p| p.rfind('\n').map(|i| p[i..].to_string()));
    let multiline = indent.is_some() || old.trailing().as_str().is_some_and(|t| t.contains('\n'));
    let indent = indent.unwrap_or_else(|| "\n  ".into());
    let mut out = Array::new();
    for v in new.iter() {
        let mut v = v.clone();
        match old.iter().find(|o| same_value(o, &v)) {
            Some(o) => *v.decor_mut() = o.decor().clone(),
            None if multiline => *v.decor_mut() = toml_edit::Decor::new(indent.as_str(), ""),
            None => v.decor_mut().clear(),
        }
        out.push_formatted(v);
    }
    if multiline {
        out.set_trailing(old.trailing().clone());
        out.set_trailing_comma(true);
    } else {
        out.fmt();
    }
    out
}

fn same_item(a: &Item, b: &Item) -> bool {
    match (a, b) {
        (Item::Value(a), Item::Value(b)) => same_value(a, b),
        (Item::ArrayOfTables(a), Item::ArrayOfTables(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_table(a, b))
        }
        _ => match (a.as_table_like(), b.as_table_like()) {
            (Some(a), Some(b)) => same_table(a, b),
            _ => false,
        },
    }
}

fn same_table(a: &dyn TableLike, b: &dyn TableLike) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|(k, v)| b.get(k).is_some_and(|w| same_item(v, w)))
}

/// Whether two values are equal, ignoring formatting. An integer equals the
/// same number written as a float.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Float(a), Value::Float(b)) => a.value() == b.value(),
        (Value::Integer(i), Value::Float(f)) | (Value::Float(f), Value::Integer(i)) => {
            *i.value() as f64 == *f.value()
        }
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        (Value::Datetime(a), Value::Datetime(b)) => a.value() == b.value(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_value(a, b))
        }
        (Value::InlineTable(a), Value::InlineTable(b)) => same_table(a, b),
        _ => false,
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
    fn saving_unchanged_values_leaves_the_file_alone() {
        let text = format!("{TEMPLATE}\n[extra]\nnote = 1 # mine\n");
        let c = Config::parse(&text).unwrap();
        assert_eq!(c.update_text(&text).unwrap(), text);
        assert_eq!(Config::default().update_text("").unwrap(), "");
    }

    #[test]
    fn saving_keeps_comments_and_unknown_keys() {
        let text = format!(
            "{}\nextra = \"kept\"\n[custom]\nanswer = 42 # also kept\n",
            TEMPLATE.replace("opacity = 0.96", "opacity = 0.96\nfuture = true")
        );
        let mut c = Config::parse(&text).unwrap();
        c.poll_minutes = 2.0;
        c.window.opacity = 0.8;
        c.tray.hide_dock_icon = false;
        c.shortcuts.toggle_window = String::new();
        let out = c.update_text(&text).unwrap();
        for kept in [
            "# How old a repo's data gets before it's refetched (minutes, at least 1).",
            "extra = \"kept\"",
            "answer = 42 # also kept",
            "future = true",
            "poll_minutes = 2.0\n",
            "opacity = 0.8\n",
            "hide_dock_icon = false            # macOS: menu-bar-only app (restart to apply)",
            "toggle_window = \"\"   # \"\" disables",
        ] {
            assert!(out.contains(kept), "{kept:?} missing from:\n{out}");
        }
        assert_eq!(Config::parse(&out).unwrap(), c);
        assert_eq!(
            out.lines().count(),
            text.lines().count(),
            "no line added or lost"
        );
    }

    #[test]
    fn saving_adds_only_keys_that_differ_from_the_default() {
        let mut c = Config::parse("poll_minutes = 5\n").unwrap();
        assert_eq!(
            c.update_text("poll_minutes = 5\n").unwrap(),
            "poll_minutes = 5\n"
        );
        c.stale_minutes = 90.0;
        c.window.always_on_top = true;
        let out = c.update_text("poll_minutes = 5\n").unwrap();
        assert_eq!(
            out,
            "poll_minutes = 5\nstale_minutes = 90.0\n\n[window]\nalways_on_top = true\n"
        );
        assert_eq!(Config::parse(&out).unwrap(), c);
    }

    #[test]
    fn saving_repos_keeps_the_comments_of_those_still_listed() {
        let text = "repos = [\n    # the app\n    \"a/app\",\n    \"a/old\",\n]\n";
        let mut c = Config::parse(text).unwrap();
        c.repos = vec!["a/new".into(), "a/app".into()];
        assert_eq!(
            c.update_text(text).unwrap(),
            "repos = [\n    \"a/new\",\n    # the app\n    \"a/app\",\n]\n"
        );
        let mut c = Config::parse("repos = [\"a/b\"]").unwrap();
        c.repos.push("c/d".into());
        assert_eq!(
            c.update_text("repos = [\"a/b\"]\n").unwrap(),
            "repos = [\"a/b\", \"c/d\"]\n"
        );
    }

    #[test]
    fn rebase_keeps_values_changed_elsewhere_since_base() {
        let base = Config::default();
        // Settings changed the opacity and the repo list.
        let mut edits = base.clone();
        edits.window.opacity = 0.8;
        edits.repos = vec!["a/b".into()];
        // Meanwhile the file got another stale threshold, another poll
        // interval and another opacity.
        let mut file = base.clone();
        file.stale_minutes = 90.0;
        file.poll_minutes = 2.0;
        file.window.opacity = 0.5;
        let out = edits.rebase(&base, &file).unwrap();
        assert_eq!(out.stale_minutes, 90.0);
        assert_eq!(out.poll_minutes, 2.0);
        assert_eq!(out.window.opacity, 0.8, "the edit wins where both changed");
        assert_eq!(out.repos, ["a/b"]);
        assert_eq!(out.window.width, base.window.width);
        assert_eq!(base.rebase(&base, &file).unwrap(), file, "no edits");
    }

    fn temp_config(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("craft-config-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn save_refuses_invalid_values_and_leaves_the_file() {
        let text = "# mine\npoll_minutes = 5\n";
        let path = temp_config("invalid", text);
        let ok = Config::load_from(&path).unwrap();
        type Spoil = fn(&mut Config);
        let bad: &[(&str, Spoil)] = &[
            ("poll_minutes", |c| c.poll_minutes = 0.5),
            ("poll_minutes", |c| c.poll_minutes = f64::NAN),
            ("stale_minutes", |c| c.stale_minutes = 4.0),
            ("stale_minutes", |c| c.stale_minutes = 10_081.0),
            ("window.width", |c| c.window.width = 100.0),
            ("window.height", |c| c.window.height = 0.0),
            ("window.opacity", |c| c.window.opacity = 1.5),
            ("repos", |c| c.repos.push("nonsense".into())),
            ("repos", |c| c.repos.push("STORYTOLD/PhotoCraft".into())),
        ];
        for (field, spoil) in bad {
            let mut c = ok.clone();
            spoil(&mut c);
            match c.save_to(&path) {
                Err(Error::Invalid(p)) => {
                    assert!(p.len() == 1 && p[0].starts_with(field), "{field}: {p:?}")
                }
                other => panic!("{field}: {other:?}"),
            }
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn save_writes_the_file_and_keeps_its_repo_spellings() {
        let text = "repos = [\"https://github.com/a/b\", \"c/d\"]\nmystery = 1\n";
        let path = temp_config("save", text);
        let mut c = Config::load_from(&path).unwrap();
        assert_eq!(c.repos, ["a/b", "c/d"]);
        c.repos = vec!["https://github.com/e/f.git".into(), "a/b".into()];
        c.github_token = " tok \n".into();
        c.save_to(&path).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.starts_with(
            "repos = [\"e/f\", \"https://github.com/a/b\"]\nmystery = 1\ngithub_token = \"tok\"\n"
        ));
        let back = Config::load_from(&path).unwrap();
        assert_eq!(
            (back.repos, back.github_token),
            (vec!["e/f".to_string(), "a/b".into()], "tok".into())
        );
        // A repo listed again in other letter case is written as given.
        c.repos = vec!["e/f".into(), "A/B".into()];
        c.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap().repos, ["e/f", "A/B"]);
        assert!(!path
            .with_extension(format!("tmp{}", std::process::id()))
            .exists());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn save_keeps_values_the_file_holds_in_another_form() {
        let text = "repos = [\"a/b\", \"A/B\", \"nonsense\"]\npoll_minutes = 0.5\n\n[window]\nopacity = 0.1\n";
        let path = temp_config("forms", text);
        let mut c = Config::load_from(&path).unwrap();
        assert_eq!(
            (c.repos.clone(), c.poll_minutes, c.window.opacity),
            (vec!["a/b".to_string()], 1.0, MIN_OPACITY)
        );
        c.window.always_on_top = true;
        c.save_to(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            text.replace("opacity = 0.1\n", "opacity = 0.1\nalways_on_top = true\n")
        );
        // Here the file's stale threshold would read as the new interval, so
        // the save writes it too, and leaves the file's other values as they are.
        let stale = temp_config(
            "forms-stale",
            "poll_minutes = 5\nstale_minutes = 2\n\n[window]\nopacity = 0.1\n",
        );
        let mut c = Config::load_from(&stale).unwrap();
        assert_eq!((c.poll_minutes, c.stale_minutes), (5.0, 5.0));
        c.poll_minutes = 3.0;
        c.save_to(&stale).unwrap();
        let back = Config::load_from(&stale).unwrap();
        assert_eq!((back.poll_minutes, back.stale_minutes), (3.0, 5.0));
        assert_eq!(
            std::fs::read_to_string(&stale).unwrap(),
            "poll_minutes = 3.0\nstale_minutes = 5.0\n\n[window]\nopacity = 0.1\n"
        );
        for p in [path, stale] {
            std::fs::remove_dir_all(p.parent().unwrap()).ok();
        }
    }

    #[test]
    fn save_creates_a_missing_file_from_the_template() {
        let path = temp_config("missing", "");
        std::fs::remove_file(&path).unwrap();
        let c = Config {
            poll_minutes: 45.0,
            ..Config::default()
        };
        c.save_to(&path).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            out,
            TEMPLATE.replace("poll_minutes = 30.0", "poll_minutes = 45.0")
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[cfg(unix)]
    #[test]
    fn save_keeps_permissions_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_config("perm", "poll_minutes = 20\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = path.with_file_name("link.toml");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let c = Config {
            poll_minutes: 25.0,
            ..Config::default()
        };
        c.save_to(&link).unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "poll_minutes = 25.0\n"
        );
        let mode = || std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(), 0o600);
        // A file that gets a token becomes private to its owner.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        c.save_to(&path).unwrap();
        assert_eq!(mode(), 0o644, "no token, so the permissions stay");
        let tokened = Config {
            github_token: "tok".into(),
            ..c
        };
        tokened.save_to(&path).unwrap();
        assert_eq!(mode(), 0o600);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
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
