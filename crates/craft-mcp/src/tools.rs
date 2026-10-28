//! The tools `craft-mcp` offers. Each call reads the app's files again: the
//! config, `snapshot.json` and, for the tools about pull requests, the
//! records in `prs/`, the kept roster and `viewer.json`. The People view is
//! assembled with [`assemble`], as the app assembles it, and the Work and My
//! PRs rows come from [`craft_core::people::rows`]. Nothing is written, and
//! the config's `github_token` is cleared as soon as the file is read.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use craft_core::config::normalize_repo;
use craft_core::people::record::{PrCache, PrRecord, PrState};
use craft_core::people::rows::{self, MineRow};
use craft_core::people::view::{assemble, PeopleView};
use craft_core::people::{roster, viewer};
use craft_core::{Cache, Config, RepoEntry, Snapshot, ALL_TIME, SINCE_RELEASE, WINDOWS};
use serde_json::{json, Map, Value};

pub type Time = DateTime<Utc>;

/// Where the server reads from.
#[derive(Debug, Clone)]
pub struct Sources {
    /// The app's cache directory: `snapshot.json`, `prs/`, `roster.json`
    /// and `viewer.json`.
    pub dir: PathBuf,
    /// The config file. With `None`, or when the file does not exist, the
    /// defaults apply, as on the app's first launch.
    pub config: Option<PathBuf>,
}

/// The Repos table's window when none is given.
const REPOS_WINDOW: &str = "4h";
/// The People tab's window when none is given.
const PEOPLE_WINDOW: &str = "7d";

/// The tool names, in the order `tools/list` gives them.
pub const NAMES: &[&str] = &["repos", "repo", "my_prs", "people", "pull_request"];

/// Every window id: the recent windows, since the latest release, all time.
fn window_ids() -> Vec<&'static str> {
    WINDOWS
        .iter()
        .map(|(id, _)| *id)
        .chain([SINCE_RELEASE, ALL_TIME])
        .collect()
}

fn window_schema(default: &str) -> Value {
    json!({
        "type": "string",
        "enum": window_ids(),
        "description": format!("Activity window id (default {default})"),
    })
}

fn repo_schema(what: &str) -> Value {
    json!({
        "type": "string",
        "description": format!("{what}: owner/name, a github.com URL, or the name alone when it is unique"),
    })
}

/// The `tools/list` entries.
pub fn list() -> Vec<Value> {
    let read_only = json!({"readOnlyHint": true, "openWorldHint": false});
    let tool =
        |name: &str, title: &str, description: &str, properties: Value, required: &[&str]| {
            json!({
                "name": name,
                "title": title,
                "description": description,
                "inputSchema": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                    "additionalProperties": false,
                },
                "annotations": read_only,
            })
        };
    vec![
        tool(
            "repos",
            "Repos",
            "Each configured repo's row as the Repos table shows it, with the age of its data, most urgent first.",
            json!({"window": window_schema(REPOS_WINDOW)}),
            &[],
        ),
        tool(
            "repo",
            "Repo",
            "One repo's row with its stats, urgent issues, open pull requests by work state, checks on main and pull request flow.",
            json!({"repo": repo_schema("The repo"), "window": window_schema(REPOS_WINDOW)}),
            &["repo"],
        ),
        tool(
            "my_prs",
            "My PRs",
            "Your open pull requests as the My PRs tab lists them: blocker, reply needed, held, waits on, time in state, failing checks and mergeable state.",
            json!({"needs_me_only": {"type": "boolean", "description": "Only the pull requests whose blocker waits on you or that need your reply (default false)"}}),
            &[],
        ),
        tool(
            "people",
            "People",
            "The People tab's Work rows: each person's open work, work waiting on them, flow and response times in a window.",
            json!({"window": window_schema(PEOPLE_WINDOW), "repo": repo_schema("Only this repo (default every repo)")}),
            &[],
        ),
        tool(
            "pull_request",
            "Pull request",
            "One pull request's record by repo and number, with its work state and the rule that set it.",
            json!({"repo": repo_schema("The repo"), "number": {"type": "integer", "minimum": 1, "description": "The pull request number"}}),
            &["repo", "number"],
        ),
    ]
}

/// Runs tool `name` with `args` at `now`: the answer, or the message of a
/// tool error. `None` for a tool that does not exist.
pub fn call(
    name: &str,
    args: &Map<String, Value>,
    src: &Sources,
    now: Time,
) -> Option<Result<Value, String>> {
    Some(match name {
        "repos" => repos(args, src, now),
        "repo" => repo(args, src, now),
        "my_prs" => my_prs(args, src, now),
        "people" => people(args, src, now),
        "pull_request" => pull_request(args, src, now),
        _ => return None,
    })
}

/// What one call reads.
struct Data {
    cfg: Config,
    snap: Snapshot,
    found_snapshot: bool,
    /// Each configured repo's records; empty unless asked for.
    caches: Vec<(String, PrCache)>,
    /// The People view; `None` without records.
    view: Option<PeopleView>,
    viewer: Option<viewer::KeptViewer>,
    now: Time,
}

impl Data {
    /// Reads the config and the snapshot, and with `people` the records,
    /// the kept roster and the kept login.
    fn load(src: &Sources, now: Time, people: bool) -> Result<Data, String> {
        let mut cfg = match &src.config {
            Some(path) if path.exists() => read_config(path)?,
            _ => Config::default(),
        };
        cfg.github_token.clear();
        let loaded = Cache::at(src.dir.join("snapshot.json")).load();
        let found_snapshot = loaded.is_some();
        let mut snap = loaded.unwrap_or_default();
        snap.sync_repos(&cfg.repos);
        snap.poll_secs = cfg.poll_secs();
        snap.stale_after_secs = cfg.stale_secs();
        let mut data = Data {
            cfg,
            snap,
            found_snapshot,
            caches: Vec::new(),
            view: None,
            viewer: None,
            now,
        };
        if !people {
            return Ok(data);
        }
        let dir = &src.dir;
        let mut caches: Vec<(String, PrCache)> = data
            .cfg
            .repos
            .iter()
            .map(|r| (r.clone(), PrCache::load(dir, r)))
            .collect();
        data.viewer = viewer::load(dir);
        let written = caches.iter().any(|(_, c)| {
            !c.records.is_empty() || c.main_checks.is_some() || c.checked_at.is_some()
        });
        if written {
            let roster = roster::load_kept(&data.cfg.people.roster, dir, now).rows;
            let builds: HashMap<String, Time> = data
                .snap
                .repos
                .iter()
                .filter_map(|e| {
                    Some((
                        e.repo.to_ascii_lowercase(),
                        e.stats.as_ref()?.latest_build_at?,
                    ))
                })
                .collect();
            let (mut view, _, _) = assemble(
                &mut caches,
                &data.cfg.people,
                &roster,
                |repo| builds.get(&repo.to_ascii_lowercase()).copied(),
                &BTreeMap::new(),
                now,
            );
            view.viewer = data.viewer.as_ref().map(|k| k.login.clone());
            data.view = Some(view);
        }
        data.caches = caches;
        Ok(data)
    }

    /// The record of pull request `number` in `repo`, in any letter case.
    fn record(&self, repo: &str, number: u64) -> Option<&PrRecord> {
        self.caches
            .iter()
            .find(|(r, _)| r.eq_ignore_ascii_case(repo))
            .and_then(|(_, c)| c.records.get(&number))
    }

    /// The configured repo `arg` names: `owner/name` or a URL in any letter
    /// case, or a name alone that one configured repo has.
    fn repo(&self, arg: &str) -> Result<String, String> {
        let wanted = normalize_repo(arg);
        let by_full = wanted
            .as_ref()
            .and_then(|w| self.cfg.repos.iter().find(|r| r.eq_ignore_ascii_case(w)));
        if let Some(r) = by_full {
            return Ok(r.clone());
        }
        let by_name: Vec<&String> = self
            .cfg
            .repos
            .iter()
            .filter(|r| {
                r.rsplit('/')
                    .next()
                    .is_some_and(|n| n.eq_ignore_ascii_case(arg.trim()))
            })
            .collect();
        match by_name.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(format!(
                "No configured repo matches {arg:?}. Configured: {}",
                self.cfg.repos.join(", ")
            )),
            many => Err(format!(
                "{arg:?} names more than one configured repo: {}",
                many.iter()
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// The login My PRs lists: `people.my_login`, otherwise the token's
    /// user as the app last read it.
    fn login(&self) -> Option<(String, &'static str)> {
        if let Some(l) = self.cfg.people.my_login() {
            return Some((l, "people.my_login in the config"));
        }
        self.viewer.as_ref().map(|k| {
            (
                k.login.clone(),
                "the GitHub token's user, as the app last read it",
            )
        })
    }

    /// Seconds from `t` to now, never negative.
    fn secs_since(&self, t: Option<Time>) -> Option<i64> {
        t.map(|t| (self.now - t).num_seconds().max(0))
    }

    /// The number of records held and the number whose timeline was read.
    fn record_counts(&self) -> Value {
        let view = self.view.as_ref();
        json!({
            "held": view.map_or(0, |v| v.records),
            "read": view.map_or(0, |v| v.fetched),
        })
    }
}

/// The config at `path`. For a file that does not parse, the error gives
/// the path and the line number, without the parser's message.
fn read_config(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    Config::parse(&text).map_err(|e| {
        let line = match &e {
            craft_core::Error::Toml(t) => t
                .span()
                .and_then(|s| text.get(..s.start))
                .map(|before| before.matches('\n').count() + 1),
            _ => None,
        };
        match line {
            Some(n) => format!("{} is not valid TOML at line {n}", path.display()),
            None => format!("{} is not a valid config", path.display()),
        }
    })
}

/// Fails on an argument the tool does not take.
fn only(args: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match args.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!(
            "Unknown argument {k:?}; this tool takes: {}",
            allowed.join(", ")
        )),
        None => Ok(()),
    }
}

fn opt_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn window(args: &Map<String, Value>, default: &str) -> Result<String, String> {
    let w = opt_str(args, "window")?.unwrap_or(default);
    if window_ids().contains(&w) {
        Ok(w.to_string())
    } else {
        Err(format!(
            "Unknown window {w:?}; use one of: {}",
            window_ids().join(", ")
        ))
    }
}

/// The repo a tool requires.
fn required_repo(args: &Map<String, Value>, data: &Data) -> Result<String, String> {
    let arg = opt_str(args, "repo")?.ok_or("repo is required")?;
    data.repo(arg)
}

/// One row of the Repos table for `window`.
fn repo_row(e: &RepoEntry, window: &str, d: &Data) -> Value {
    let s = e.stats.as_ref();
    let lag = s.and_then(|s| s.since_build.as_ref());
    json!({
        "repo": e.repo,
        "name": e.name(),
        "fetched_at": e.fetched_at,
        "age_secs": d.secs_since(e.fetched_at),
        "stale": e.is_stale(d.now, d.snap.stale_after_secs),
        "last_attempt_at": e.last_attempt_at,
        "error": e.error,
        "inaccessible": e.inaccessible,
        "urgency": s.map(|s| s.urgency),
        "critical_issues": s.map(|s| s.critical_issues),
        "open_prs": s.map(|s| s.open_prs),
        "open_issues": s.map(|s| s.open_issues),
        "oldest_open_pr_at": s.and_then(|s| s.oldest_open_pr_at),
        "last_commit_at": s.and_then(|s| s.last_commit_at),
        "last_issue_at": s.and_then(|s| s.last_issue_at),
        "newest_pr_at": s.and_then(|s| s.newest_pr_at),
        "recent": s.and_then(|s| s.recent.get(window)),
        "latest_build": s.and_then(|s| s.latest_build.clone()),
        "latest_build_at": s.and_then(|s| s.latest_build_at),
        "latest_build_prerelease": s.map(|s| s.latest_build_prerelease),
        "commits_since_build": lag.map(|l| l.commits),
        "commits_since_build_basis": lag.map(|l| l.basis),
        "latest_build_downloads": s.map(|s| s.latest_build_downloads),
        "downloads_total": s.map(|s| s.downloads_total),
    })
}

/// What the whole snapshot says: whether the app has saved one, the window,
/// the staleness limit, the GitHub budget and any error.
fn snapshot_head(d: &Data, window: &str) -> Map<String, Value> {
    let head = json!({
        "now": d.now,
        "snapshot_found": d.found_snapshot,
        "window": window,
        "stale_after_secs": d.snap.stale_after_secs,
        "rate_limit_remaining": d.snap.rate_limit_remaining,
        "error": d.snap.error,
        "token": d.snap.token,
    });
    match head {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn repos(args: &Map<String, Value>, src: &Sources, now: Time) -> Result<Value, String> {
    only(args, &["window"])?;
    let window = window(args, REPOS_WINDOW)?;
    let d = Data::load(src, now, false)?;
    let mut entries: Vec<&RepoEntry> = d.snap.repos.iter().collect();
    entries.sort_by(|a, b| {
        let ua = a.stats.as_ref().map(|s| s.urgency);
        let ub = b.stats.as_ref().map(|s| s.urgency);
        let by_urgency = match (ua, ub) {
            (Some(x), Some(y)) => y.total_cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        by_urgency.then_with(|| {
            a.name()
                .to_ascii_lowercase()
                .cmp(&b.name().to_ascii_lowercase())
        })
    });
    let mut out = snapshot_head(&d, &window);
    out.insert(
        "repos".into(),
        entries.iter().map(|e| repo_row(e, &window, &d)).collect(),
    );
    Ok(Value::Object(out))
}

fn repo(args: &Map<String, Value>, src: &Sources, now: Time) -> Result<Value, String> {
    only(args, &["repo", "window"])?;
    let window = window(args, REPOS_WINDOW)?;
    let d = Data::load(src, now, true)?;
    let name = required_repo(args, &d)?;
    let Some(entry) = d
        .snap
        .repos
        .iter()
        .find(|e| e.repo.eq_ignore_ascii_case(&name))
    else {
        return Err(format!("{name} is not in the snapshot"));
    };
    let mut row = match repo_row(entry, &window, &d) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let s = entry.stats.as_ref();
    row.insert("recent_by_window".into(), json!(s.map(|s| &s.recent)));
    row.insert(
        "latest_build_sha".into(),
        json!(s.and_then(|s| s.latest_build_sha.clone())),
    );
    row.insert("urgent_issues".into(), json!(s.map(|s| &s.urgent)));
    let view = d.view.as_ref();
    let items: Vec<Value> = view
        .map(|v| {
            v.items
                .iter()
                .filter(|i| i.repo.eq_ignore_ascii_case(&name))
                .map(|i| {
                    json!({
                        "number": i.number,
                        "title": i.title,
                        "author": i.author,
                        "state": i.work.state,
                        "state_label": rows::state_label(&i.work.state),
                        "waits_on": i.work.waits_on,
                        "since": i.work.since,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut by_state: BTreeMap<String, u64> = BTreeMap::new();
    for i in view.into_iter().flat_map(|v| &v.items) {
        if i.repo.eq_ignore_ascii_case(&name) {
            *by_state
                .entry(rows::state_label(&i.work.state))
                .or_default() += 1;
        }
    }
    let mine = |r: &String| r.eq_ignore_ascii_case(&name);
    let main = view.and_then(|v| v.main.iter().find(|(r, _)| mine(r)).map(|(_, m)| m));
    let flow = view.and_then(|v| v.repos.iter().find(|(r, _)| mine(r)).map(|(_, f)| f));
    let checked = view.and_then(|v| {
        v.checked
            .iter()
            .find(|(r, _)| mine(r))
            .and_then(|(_, c)| *c)
    });
    row.insert(
        "pull_requests".into(),
        json!({
            "records_checked_at": checked,
            "open": items.len(),
            "by_state": by_state,
            "open_items": items,
            "main_checks": main,
            "flow_by_window": flow,
        }),
    );
    let mut out = snapshot_head(&d, &window);
    out.insert("repo".into(), Value::Object(row));
    Ok(Value::Object(out))
}

/// The names of `rec`'s failing checks while its rollup is `FAILURE` or
/// `ERROR`, as the work states read them; none for any other rollup.
fn failing_checks(rec: &PrRecord) -> Vec<String> {
    match rec.checks.rollup.as_deref() {
        Some("FAILURE" | "ERROR") => rec.checks.failing.clone(),
        _ => Vec::new(),
    }
}

/// One My PRs row with what the records say about its checks.
fn mine_row(r: &MineRow, login: &str, d: &Data) -> Value {
    let i = &r.item;
    let rec = d.record(&i.repo, i.number);
    let nonempty = |s: &str| (!s.is_empty()).then(|| s.to_string());
    json!({
        "repo": i.repo,
        "number": i.number,
        "title": i.title,
        "url": i.url,
        "created_at": i.created_at,
        "blocker": r.blocker.label,
        "blocker_state": r.blocker.state,
        "tone": r.blocker.tone,
        "needs_me": r.needs_me,
        "reply_needed": r.reply,
        "held": r.held,
        "hold": i.work.hold,
        "waits_on": rows::waits_text(i, login),
        "waits_for": i.work.waits_for,
        "in_state_since": i.work.since,
        "in_state_secs": d.secs_since(i.work.since),
        "state": i.work.state,
        "state_label": rows::state_label(&i.work.state),
        "rule": i.work.rule,
        "guess": i.work.guess,
        "failing_checks": rec.map(failing_checks).unwrap_or_default(),
        "checks_rollup": rec.and_then(|r| r.checks.rollup.clone()),
        "mergeable": rec.and_then(|r| nonempty(&r.checks.mergeable)),
        "merge_state": rec.and_then(|r| nonempty(&r.checks.merge_state)),
        "draft": rec.map(|r| r.is_draft),
        "read": rec.is_some_and(|r| r.fetched_at.is_some()),
    })
}

fn my_prs(args: &Map<String, Value>, src: &Sources, now: Time) -> Result<Value, String> {
    only(args, &["needs_me_only"])?;
    let needs_me_only = match args.get("needs_me_only") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("needs_me_only must be true or false".into()),
    };
    let d = Data::load(src, now, true)?;
    let Some((login, from)) = d.login() else {
        return Ok(json!({
            "now": now,
            "login": null,
            "note": "No login is known yet: set people.my_login in the config, or let the app run until it has read the GitHub token's user.",
            "rows": [],
        }));
    };
    let Some(view) = &d.view else {
        return Ok(json!({
            "now": now,
            "login": login,
            "login_from": from,
            "note": "The app has not saved any pull request records yet.",
            "rows": [],
        }));
    };
    let all = rows::mine_rows(view, &d.cfg.repos, &login);
    let needs_me = all.iter().filter(|r| r.needs_me).count();
    let shown: Vec<Value> = all
        .iter()
        .filter(|r| !needs_me_only || r.needs_me)
        .map(|r| mine_row(r, &login, &d))
        .collect();
    Ok(json!({
        "now": now,
        "login": login,
        "login_from": from,
        "login_read_at": d.viewer.as_ref().filter(|_| d.cfg.people.my_login().is_none()).and_then(|k| k.read_at),
        "needs_me_only": needs_me_only,
        "needs_me": needs_me,
        "count": shown.len(),
        "records": d.record_counts(),
        "records_checked_at": view.checked,
        "rows": shown,
    }))
}

fn people(args: &Map<String, Value>, src: &Sources, now: Time) -> Result<Value, String> {
    only(args, &["window", "repo"])?;
    let window = window(args, PEOPLE_WINDOW)?;
    let d = Data::load(src, now, true)?;
    let repo = match opt_str(args, "repo")? {
        Some(r) => Some(d.repo(r)?),
        None => None,
    };
    let rows = d
        .view
        .as_ref()
        .map(|v| rows::people_rows(v, &window, repo.as_deref().unwrap_or("")))
        .unwrap_or_default();
    let mut out = json!({
        "now": now,
        "window": window,
        "repo": repo,
        "count": rows.len(),
        "records": d.record_counts(),
        "records_checked_at": d.view.as_ref().map(|v| &v.checked),
        "rows": rows,
    });
    if d.view.is_none() {
        out["note"] = json!("The app has not saved any pull request records yet.");
    }
    Ok(out)
}

fn pull_request(args: &Map<String, Value>, src: &Sources, now: Time) -> Result<Value, String> {
    only(args, &["repo", "number"])?;
    let number = match args.get("number") {
        Some(Value::Number(n)) => n.as_u64().filter(|n| *n > 0),
        _ => None,
    }
    .ok_or("number must be a pull request number")?;
    let d = Data::load(src, now, true)?;
    let repo = required_repo(args, &d)?;
    let Some(rec) = d.record(&repo, number) else {
        return Err(format!(
            "No record of {repo}#{number}: the app keeps records of open pull requests and of the merged and closed ones it has loaded so far."
        ));
    };
    let item = d.view.as_ref().and_then(|v| {
        v.items
            .iter()
            .find(|i| i.repo.eq_ignore_ascii_case(&repo) && i.number == number)
    });
    let work = match item {
        Some(i) => json!(i.work),
        None => {
            let decided = rec.derived.state.as_ref();
            json!({
                "state": decided.map(|s| s.value.clone()),
                "rule": decided.map(|s| s.rule.clone()),
            })
        }
    };
    let state = item
        .map(|i| i.work.state.clone())
        .or_else(|| rec.derived.state.as_ref().map(|s| s.value.clone()));
    let login = d.login().map(|(l, _)| l);
    let blocker = match (item, &login) {
        (Some(i), Some(l)) => json!(rows::blocker_of(i, l)),
        _ => Value::Null,
    };
    let github_state = match rec.state {
        PrState::Open => "open",
        PrState::Merged => "merged",
        PrState::Closed => "closed",
    };
    Ok(json!({
        "now": now,
        "repo": repo,
        "number": number,
        "title": rec.title,
        "url": if rec.url.is_empty() { format!("https://github.com/{repo}/pull/{number}") } else { rec.url.clone() },
        "github_state": github_state,
        "draft": rec.is_draft,
        "author": rec.author,
        "created_at": rec.created_at,
        "updated_at": rec.updated_at,
        "merged_at": rec.merged_at,
        "closed_at": rec.closed_at,
        "read_at": rec.fetched_at,
        "state_label": state.as_deref().map(rows::state_label),
        "work": work,
        "reply_needed": item.and_then(|i| i.reply.clone()),
        "held": item.map(rows::is_held),
        "blocker_for": login.filter(|_| item.is_some()),
        "blocker": blocker,
        "checks": {
            "mergeable": rec.checks.mergeable,
            "merge_state": rec.checks.merge_state,
            "rollup": rec.checks.rollup,
            "failing": failing_checks(rec),
        },
        "closing_issues": rec.closing_issues,
    }))
}
