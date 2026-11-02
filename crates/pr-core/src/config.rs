//! The `[pr_review]` section of `~/.craft_status_config.toml`: settings for the PR
//! tab. The repo list and GitHub token are the file's top-level ones.

use craft_core::config::normalize_repo;
use craft_core::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub first_party_owners: Vec<String>,
    pub exclude_repos: Vec<String>,
    pub poll_minutes: f64,
    pub full_list_hours: f64,
    pub stale_minutes: f64,
    pub max_points_per_hour: u64,
    pub budget_floor: u64,
    pub frequent_over: u64,
    pub high_quality: u8,
    pub people_toml_is_core: bool,
    pub sensitive_paths: Vec<String>,
    pub claude: ClaudeConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    pub enabled: bool,
    pub binary: String,
    pub model: String,
    pub effort: String,
    pub batch_size: usize,
    pub max_runs_per_hour: u32,
    pub max_usd_per_day: f64,
    pub group_refresh_hours: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            first_party_owners: vec!["storytold".into()],
            exclude_repos: vec![],
            poll_minutes: 30.0,
            full_list_hours: 4.0,
            stale_minutes: 90.0,
            max_points_per_hour: 2500,
            budget_floor: 1000,
            frequent_over: 30,
            high_quality: 75,
            people_toml_is_core: true,
            sensitive_paths: [
                ".github/workflows/",
                ".github/actions/",
                ".github/CODEOWNERS",
                "CODEOWNERS",
                "build.rs",
                ".cargo/config",
                "deny.toml",
                ".coderabbit.yaml",
                "sonar-project.properties",
                "Dockerfile",
                "tauri.conf.json",
                "/capabilities/",
            ]
            .map(String::from)
            .to_vec(),
            claude: ClaudeConfig::default(),
        }
    }
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        ClaudeConfig {
            enabled: true,
            binary: String::new(),
            model: "claude-opus-5-5".into(),
            effort: "low".into(),
            batch_size: 12,
            max_runs_per_hour: 12,
            max_usd_per_day: 5.0,
            group_refresh_hours: 24.0,
        }
    }
}

/// The whole config file, of which only this section matters here.
#[derive(Deserialize, Default)]
#[serde(default)]
struct File {
    pr_review: Config,
}

impl Config {
    /// The `[pr_review]` section of the config file's text; defaults if it has none.
    pub fn parse(text: &str) -> Result<Config> {
        let mut c = toml::from_str::<File>(text)?.pr_review;
        c.poll_minutes = c.poll_minutes.clamp(1.0, 24.0 * 60.0);
        c.full_list_hours = c.full_list_hours.max(c.poll_minutes / 60.0);
        c.stale_minutes = c.stale_minutes.max(c.poll_minutes);
        c.claude.batch_size = c.claude.batch_size.clamp(1, 40);
        Ok(c)
    }

    /// The section as it is on disk; defaults if there's no file yet.
    pub fn load() -> Result<Config> {
        let path = craft_core::Config::path()?;
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(source) => Err(Error::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn poll_secs(&self) -> i64 {
        (self.poll_minutes * 60.0) as i64
    }

    pub fn full_list_secs(&self) -> i64 {
        (self.full_list_hours * 3600.0) as i64
    }

    pub fn stale_secs(&self) -> i64 {
        (self.stale_minutes * 60.0) as i64
    }

    /// `shared_repos` without third-party and excluded repos: what gets reviewed.
    pub fn watched(&self, shared_repos: &[String]) -> Vec<String> {
        let excluded: Vec<String> = self
            .exclude_repos
            .iter()
            .filter_map(|r| normalize_repo(r))
            .map(|r| r.to_lowercase())
            .collect();
        shared_repos
            .iter()
            .filter(|r| {
                let owner = r.split('/').next().unwrap_or_default();
                self.first_party_owners
                    .iter()
                    .any(|o| o.eq_ignore_ascii_case(owner))
                    && !excluded.contains(&r.to_lowercase())
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(body: &str) -> Config {
        Config::parse(&format!("[pr_review]\n{body}")).unwrap()
    }

    #[test]
    fn template_parses_to_defaults() {
        assert_eq!(
            Config::parse(craft_core::config::TEMPLATE).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn a_file_without_the_section_is_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert_eq!(
            Config::parse("poll_minutes = 5.0\n[window]\nwidth = 900.0").unwrap(),
            Config::default(),
            "top-level settings belong to the status poller"
        );
    }

    #[test]
    fn third_party_and_excluded_repos_are_left_out() {
        let c = section(r#"exclude_repos = ["https://github.com/storytold/craft-libs"]"#);
        let shared = [
            "storytold/photocraft",
            "storytold/craft-libs",
            "tangency-labs/solidcraft",
        ]
        .map(String::from);
        assert_eq!(c.watched(&shared), vec!["storytold/photocraft"]);
    }

    #[test]
    fn third_party_repos_in_the_default_list_are_not_reviewed() {
        let shared = craft_core::Config::default().repos;
        let watched = Config::default().watched(&shared);
        assert!(shared.iter().any(|r| r == "tangency-labs/solidcraft"));
        assert!(!watched.iter().any(|r| r == "tangency-labs/solidcraft"));
        assert_eq!(watched.len(), shared.len() - 1);
    }

    #[test]
    fn intervals_are_clamped() {
        let c = section("poll_minutes = 0.1\nstale_minutes = 0.5\nfull_list_hours = 0");
        assert_eq!(c.poll_secs(), 60);
        assert_eq!(c.stale_secs(), 60);
        assert_eq!(c.full_list_secs(), 60);
    }
}
