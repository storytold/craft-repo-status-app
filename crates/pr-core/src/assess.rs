//! The landing heuristics: deterministic, cheap to recompute and simple to read.
//! Each score carries the reasons behind it, which the PR view shows.
//!
//! A PR has independent flags (approved, conflicted, CI failed…) and one headline
//! status worked out from them by [`headline`]. The help panel in `ui/src/model.ts`
//! explains the same table: change both together.
//!
//! PR titles and bodies are written by whoever opened the PR, and Claude reads
//! them, so Claude's opinion can't improve a PR's standing: the headline and
//! quality ignore it, and it can only make a PR's ease harder.

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::model::{Ease, Pr, PrDetail, PrState, RabbitStatus, RepoData, Severity, Tier};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Reason {
    pub text: String,
    /// Points added to the score; 0 for reasons that only explain.
    pub points: i32,
}

fn why(text: impl Into<String>, points: i32) -> Reason {
    Reason {
        text: text.into(),
        points,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AuthorInfo {
    pub login: String,
    pub tier: Tier,
    /// Write access or better, if the roster says so.
    pub permission: Option<String>,
    /// In the repo's contributors/people.toml.
    pub listed: bool,
    pub landed_here: u64,
    pub landed_total: u64,
    /// Whether every repo's merge history has been walked, so the counts are final.
    pub counts_complete: bool,
}

/// `landed_total` is the login's merged PRs across the watched repos.
pub fn author_info(
    pr: &Pr,
    repo: &RepoData,
    landed_total: u64,
    counts_complete: bool,
    cfg: &Config,
) -> AuthorInfo {
    let login = pr.author.to_lowercase();
    let permission = repo.roster.permissions.get(&login).cloned();
    let can_write = permission
        .as_deref()
        .is_some_and(|p| matches!(p, "WRITE" | "MAINTAIN" | "ADMIN"));
    let listed = repo.roster.listed.contains(&login);
    let member = matches!(pr.association.as_str(), "OWNER" | "MEMBER" | "COLLABORATOR");
    let tier = if pr.author_is_bot || pr.author.ends_with("[bot]") {
        Tier::Bot
    } else if can_write || member || (listed && cfg.people_toml_is_core) {
        Tier::Core
    } else if landed_total > cfg.frequent_over {
        Tier::Frequent
    } else if pr.association == "FIRST_TIME_CONTRIBUTOR" || pr.association == "FIRST_TIMER" {
        Tier::New
    } else {
        Tier::Regular
    };
    AuthorInfo {
        login: pr.author.clone(),
        tier,
        permission,
        listed,
        // Filled in by the caller, which counts a whole repo at once.
        landed_here: 0,
        landed_total,
        counts_complete,
    }
}

/// Each author's merged PRs in `repo`, by lowercase login.
pub fn landed_counts(repo: &RepoData) -> std::collections::HashMap<String, u64> {
    let mut out = std::collections::HashMap::new();
    for author in repo.history.merged.values() {
        *out.entry(author.to_lowercase()).or_insert(0) += 1;
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Assessment {
    pub state: PrState,
    pub state_reasons: Vec<String>,
    pub ease: Ease,
    pub ease_score: i32,
    pub ease_reasons: Vec<Reason>,
    /// Claude thought it harder than the heuristics did, and that won.
    pub ease_raised_by_claude: bool,
    pub quality: u8,
    pub quality_reasons: Vec<Reason>,
    pub high_quality: bool,
    /// GitHub reports merge conflicts.
    pub conflicted: bool,
    /// GitHub's review decision is approved.
    pub approved: bool,
    /// Approved, but not on the commit now at the head of the PR.
    pub approval_stale: bool,
    pub changes_requested: bool,
    /// Whether the detail (CodeRabbit, files, tests) informed this.
    pub inspected: bool,
    pub ci: String,
    pub rabbit_flags: u64,
    pub rabbit_nits: u64,
    pub security_flags: u64,
    pub security_nits: u64,
    /// Security-relevant paths changed by someone without write access.
    pub sensitive_paths: Vec<String>,
    pub security_reasons: Vec<String>,
    pub deps_changed: bool,
    pub tests_touched: bool,
    pub interesting: bool,
    /// Sort key for "what to look at next".
    pub focus: i32,
}

fn is_rabbit_check(name: &str) -> bool {
    name.eq_ignore_ascii_case("CodeRabbit")
}

fn is_security_check(name: &str) -> bool {
    let n = name.to_lowercase();
    [
        "secur",
        "audit",
        "cargo-deny",
        "cargo deny",
        "codeql",
        "gitleaks",
        "semgrep",
        "osv",
        "supply chain",
        "git dependencies",
    ]
    .iter()
    .any(|w| n.contains(w))
}

/// Cancelled runs say nothing about the code, usually just that a newer one started.
fn check_failed(state: &str) -> bool {
    matches!(
        state,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED"
    )
}

/// Whether a check says anything about this commit.
fn check_counts(c: &crate::model::Check) -> bool {
    !is_rabbit_check(&c.name) && !matches!(c.state.as_str(), "CANCELLED" | "SKIPPED")
}

fn check_running(state: &str) -> bool {
    matches!(
        state,
        "PENDING" | "IN_PROGRESS" | "QUEUED" | "WAITING" | "REQUESTED" | "EXPECTED"
    )
}

/// `failure`, `pending`, `success` or `none`. The checks in the detail leave out
/// CodeRabbit, which is judged by its findings, and cancelled or skipped runs, but
/// they may be older than the last listing's rollup: finished checks don't bump a
/// PR's `updatedAt`. The rollup can't tell cancelled runs from failed ones, so a
/// PR not yet inspected may read as failing until it is.
pub fn ci_state(pr: &Pr) -> String {
    let from_detail = pr.current_detail().filter(|d| d.fetched_at >= pr.listed_at);
    if let Some(d) = from_detail {
        let checks: Vec<_> = d.checks.iter().filter(|c| check_counts(c)).collect();
        let state = if checks.iter().any(|c| check_failed(&c.state)) {
            "failure"
        } else if checks.iter().any(|c| check_running(&c.state)) {
            "pending"
        } else if checks.is_empty() {
            "none"
        } else {
            "success"
        };
        return state.to_string();
    }
    match pr.ci_rollup.as_deref() {
        Some("SUCCESS") => "success",
        Some("FAILURE" | "ERROR") => "failure",
        Some(_) => "pending",
        None => "none",
    }
    .to_string()
}

fn is_lock_or_generated(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "Cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "contributors.json"
    ) || path.contains("/generated/")
        || path.starts_with("vendor/")
}

fn is_test(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains("test") || p.contains("/spec") || p.contains("fixture") || p.contains("corpus")
}

fn is_code(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or_default();
    matches!(
        ext,
        "rs" | "ts" | "tsx" | "js" | "py" | "wgsl" | "c" | "cpp" | "h" | "swift" | "kt" | "go"
    )
}

/// The facts the headline depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Signals {
    pub draft: bool,
    /// A failed check, requested changes, a CodeRabbit flag or an open conversation.
    pub problem: bool,
    pub conflicted: bool,
    pub inspected: bool,
    /// CI or CodeRabbit is running, or GitHub hasn't worked out mergeability.
    pub running: bool,
    /// CI ran and passed, as opposed to not running at all. Failing is a problem and
    /// running is `running`, so neither gets this far.
    pub ci_passed: bool,
    /// Approved on the latest commit.
    pub approved: bool,
    pub high_quality: bool,
}

/// The truth table: the first row that matches wins. The help panel shows this.
pub fn headline(s: &Signals) -> PrState {
    if s.draft {
        PrState::Draft
    } else if s.problem {
        PrState::NeedsWork
    } else if s.conflicted {
        PrState::Conflicted
    } else if !s.inspected {
        PrState::NotInspected
    } else if s.running {
        PrState::Checking
    } else if !s.ci_passed {
        PrState::CiNotRun
    } else if s.approved {
        PrState::Landable
    } else if s.high_quality {
        PrState::Promising
    } else {
        PrState::Reviewable
    }
}

/// What the inspected detail of a PR adds up to, for the heuristics below.
struct Read {
    critical: i32,
    major: i32,
    minor: i32,
    trivial: i32,
    heavy: i32,
    human_open: u64,
    security_checks_failed: Vec<String>,
    rabbit: RabbitStatus,
    inspected_ever: bool,
    has_body: bool,
    long_body: bool,
    linked: bool,
    touches_code: bool,
    /// Lines changed, less lockfiles and generated files.
    real_lines: u64,
    /// How many top-level areas of the repo it changes.
    areas: usize,
}

impl Read {
    fn flags(&self) -> i32 {
        self.critical + self.major
    }

    fn findings(&self) -> i32 {
        self.flags() + self.minor + self.trivial
    }
}

/// Whether the author can already change anything `sensitive_paths` guards.
fn can_write(pr: &Pr, author: &AuthorInfo) -> bool {
    author.tier == Tier::Bot
        || author
            .permission
            .as_deref()
            .is_some_and(|p| matches!(p, "WRITE" | "MAINTAIN" | "ADMIN"))
        || matches!(pr.association.as_str(), "OWNER" | "MEMBER" | "COLLABORATOR")
}

fn count_findings(d: &PrDetail, r: &mut Read, a: &mut Assessment) {
    for f in d.findings.iter().filter(|f| f.active()) {
        match f.severity {
            Severity::Critical => r.critical += 1,
            Severity::Major => r.major += 1,
            Severity::Minor => r.minor += 1,
            Severity::Trivial => r.trivial += 1,
        }
        if f.effort.eq_ignore_ascii_case("heavy lift") {
            r.heavy += 1;
        }
        match (f.severity.is_flag(), f.security) {
            (true, true) => a.security_flags += 1,
            (false, true) => a.security_nits += 1,
            _ => {}
        }
    }
    a.rabbit_flags = r.flags() as u64;
    a.rabbit_nits = (r.minor + r.trivial) as u64;
}

fn read_files(d: &PrDetail, r: &mut Read, a: &mut Assessment) {
    a.tests_touched = d.files.iter().any(|f| is_test(&f.path));
    a.deps_changed = d.files.iter().any(|f| {
        let name = f.path.rsplit('/').next().unwrap_or(&f.path);
        matches!(name, "Cargo.lock" | "package-lock.json")
    });
    r.touches_code = d.files.iter().any(|f| is_code(&f.path));
    let generated: u64 = d
        .files
        .iter()
        .filter(|f| is_lock_or_generated(&f.path))
        .map(|f| f.additions + f.deletions)
        .sum();
    r.real_lines = r.real_lines.saturating_sub(generated);
    let areas: std::collections::BTreeSet<String> = d
        .files
        .iter()
        .filter(|f| !is_lock_or_generated(&f.path))
        .map(|f| f.path.split('/').take(2).collect::<Vec<_>>().join("/"))
        .collect();
    r.areas = areas.len();
}

fn read_detail(pr: &Pr, author: &AuthorInfo, cfg: &Config, a: &mut Assessment) -> Read {
    let mut r = Read {
        critical: 0,
        major: 0,
        minor: 0,
        trivial: 0,
        heavy: 0,
        human_open: 0,
        security_checks_failed: vec![],
        rabbit: RabbitStatus::None,
        inspected_ever: false,
        has_body: false,
        long_body: false,
        linked: false,
        touches_code: false,
        real_lines: pr.lines(),
        areas: 0,
    };
    let Some(d) = pr.current_detail().or(pr.detail.as_ref()) else {
        return r;
    };
    r.inspected_ever = true;
    count_findings(d, &mut r, a);
    read_files(d, &mut r, a);
    r.human_open = d.human_threads_open;
    r.rabbit = d.rabbit;
    r.security_checks_failed = d
        .checks
        .iter()
        .filter(|c| is_security_check(&c.name) && check_failed(&c.state))
        .map(|c| c.name.clone())
        .collect();
    let len = d.body.trim().chars().count();
    r.has_body = len >= 40;
    r.long_body = len >= 200;
    r.linked = !d.linked_issues.is_empty();
    if !can_write(pr, author) {
        a.sensitive_paths = d
            .files
            .iter()
            .filter(|f| {
                cfg.sensitive_paths
                    .iter()
                    .any(|s| f.path.contains(s.as_str()))
            })
            .map(|f| f.path.clone())
            .collect();
    }
    r
}

fn note_security(r: &Read, a: &mut Assessment) {
    if a.security_flags > 0 {
        a.security_reasons.push(format!(
            "{} CodeRabbit security finding(s)",
            a.security_flags
        ));
    }
    for c in &r.security_checks_failed {
        a.security_flags += 1;
        a.security_reasons.push(format!("{c} failed"));
    }
    if !a.sensitive_paths.is_empty() {
        a.security_reasons.push(format!(
            "Changes {} sensitive path(s) without write access",
            a.sensitive_paths.len()
        ));
    }
}

fn problems(r: &Read, a: &Assessment) -> Vec<String> {
    let mut out = vec![];
    if a.ci == "failure" {
        out.push("CI is failing".to_string());
    }
    if a.changes_requested {
        out.push("A reviewer requested changes".to_string());
    }
    if r.flags() > 0 {
        out.push(format!("{} unresolved CodeRabbit flag(s)", r.flags()));
    }
    if !r.security_checks_failed.is_empty() {
        out.push(format!(
            "Security check failed: {}",
            r.security_checks_failed.join(", ")
        ));
    }
    if r.human_open > 0 {
        out.push(format!(
            "{} unresolved review conversation(s)",
            r.human_open
        ));
    }
    out
}

fn still_running(pr: &Pr, r: &Read, a: &Assessment) -> Vec<String> {
    let mut out = vec![];
    if a.ci == "pending" {
        out.push("CI is still running".to_string());
    }
    if r.rabbit == RabbitStatus::InProgress {
        out.push("CodeRabbit is still reviewing".to_string());
    }
    if pr.mergeable == "UNKNOWN" || pr.mergeable.is_empty() {
        out.push("GitHub hasn't worked out mergeability yet".to_string());
    }
    out
}

/// The size of the change, in lines and files.
fn ease_of_size(pr: &Pr, r: &Read) -> Vec<Reason> {
    let lines = r.real_lines;
    let (pts, label) = match lines {
        0..50 => (0, "Tiny"),
        50..200 => (1, "Small"),
        200..600 => (2, "Moderate"),
        600..2000 => (4, "Large"),
        _ => (6, "Huge"),
    };
    let mut ease = vec![why(format!("{label} change: {lines} lines"), pts)];
    let files = pr.changed_files;
    let pts = match files {
        0..=3 => 0,
        4..=10 => 1,
        11..=30 => 2,
        _ => 4,
    };
    if pts > 0 {
        ease.push(why(format!("{files} files"), pts));
    }
    if r.areas > 4 {
        ease.push(why(format!("Spreads across {} areas", r.areas), 1));
    }
    ease
}

fn ease_reasons(pr: &Pr, r: &Read, a: &Assessment, author: &AuthorInfo) -> Vec<Reason> {
    let mut ease = ease_of_size(pr, r);
    if a.conflicted {
        ease.push(why("Needs a rebase", 2));
    }
    if a.ci == "failure" {
        ease.push(why("CI has to be fixed first", 1));
    }
    if r.flags() > 0 {
        ease.push(why(
            format!("{} CodeRabbit flag(s) to address", r.flags()),
            r.flags().min(3),
        ));
    }
    if r.heavy > 0 {
        ease.push(why(format!("{} heavy-lift finding(s)", r.heavy), 1));
    }
    if !a.sensitive_paths.is_empty() {
        ease.push(why("Touches sensitive paths", 2));
    }
    match author.tier {
        Tier::Core | Tier::Frequent => ease.push(why("Author knows the codebase", -1)),
        Tier::New => ease.push(why("First-time contributor", 1)),
        _ => {}
    }
    ease
}

/// What only an inspected PR can say about its quality.
fn quality_of_detail(r: &Read, a: &Assessment, q: &mut Vec<Reason>) {
    if r.long_body {
        q.push(why("Explains itself", 8));
    } else if !r.has_body {
        q.push(why("Little or no description", -15));
    }
    if r.linked {
        q.push(why("Closes an issue", 6));
    }
    if a.tests_touched {
        q.push(why("Includes tests", 12));
    } else if r.touches_code {
        q.push(why("Changes code without tests", -8));
    }
    let penalties = [
        (r.critical, "critical", (-20 * r.critical).max(-40)),
        (r.major, "major", (-10 * r.major).max(-30)),
        (r.minor, "minor", (-3 * r.minor).max(-12)),
        (r.trivial, "trivial", (-r.trivial).max(-4)),
    ];
    for (count, name, points) in penalties {
        if count > 0 {
            q.push(why(format!("{count} {name} finding(s)"), points));
        }
    }
    if r.rabbit == RabbitStatus::Reviewed && r.findings() == 0 {
        q.push(why("CodeRabbit had nothing to add", 8));
    }
    if a.security_flags > 0 {
        q.push(why("Security flags", -15));
    }
}

fn quality_reasons(r: &Read, a: &Assessment, author: &AuthorInfo) -> Vec<Reason> {
    let mut q = vec![why("Baseline", 60)];
    match a.ci.as_str() {
        "success" => q.push(why("CI passes", 10)),
        "failure" => q.push(why("CI fails", -20)),
        _ => {}
    }
    if a.conflicted {
        q.push(why("Merge conflicts", -15));
    }
    if a.changes_requested {
        q.push(why("Changes requested", -15));
    }
    if a.approved {
        q.push(why("Approved", 10));
    }
    if r.real_lines > 10_000 {
        q.push(why("Too large to review well", -20));
    } else if r.real_lines > 2_000 {
        q.push(why("Very large", -10));
    }
    if r.inspected_ever {
        quality_of_detail(r, a, &mut q);
    }
    if matches!(author.tier, Tier::Core | Tier::Frequent) {
        q.push(why("Track record", 4));
    }
    q
}

fn state_reasons(
    pr: &Pr,
    a: &Assessment,
    problems: Vec<String>,
    running: Vec<String>,
) -> Vec<String> {
    let stale_note = "An approval predates the latest commit, so it doesn't count";
    match a.state {
        PrState::Draft => vec!["Marked as a draft".to_string()],
        PrState::NeedsWork => problems,
        PrState::Conflicted => {
            let mut why =
                vec!["Has merge conflicts, and nothing else known to be wrong".to_string()];
            if !a.inspected {
                why.push(
                    "Not inspected yet, so CodeRabbit and conversations are unknown".to_string(),
                );
            }
            why
        }
        PrState::NotInspected => vec![if pr.detail.is_some() {
            "Changed since it was last inspected".to_string()
        } else {
            "Not inspected yet, so CodeRabbit, files and conversations are unknown".to_string()
        }],
        PrState::Checking => running,
        PrState::CiNotRun => {
            vec!["CI hasn't run: its workflows may need approving".to_string()]
        }
        PrState::Landable => {
            vec!["Approved on the latest commit, CI passed, nothing in the way".to_string()]
        }
        PrState::Promising | PrState::Reviewable => {
            let mut why = vec![if a.state == PrState::Promising {
                "Passes every automated check and scores well, but no person has approved it"
                    .to_string()
            } else {
                "CI passed and nothing is in the way, but no person has approved it".to_string()
            }];
            if a.approval_stale {
                why.push(stale_note.to_string());
            }
            why
        }
    }
}

/// Runs the heuristics over `pr`, written by `author`.
pub fn assess(pr: &Pr, author: &crate::assess::AuthorInfo, cfg: &Config) -> Assessment {
    let mut a = Assessment {
        ci: ci_state(pr),
        inspected: pr.current_detail().is_some(),
        conflicted: pr.mergeable == "CONFLICTING" || pr.merge_state == "DIRTY",
        changes_requested: pr.review_decision.as_deref() == Some("CHANGES_REQUESTED"),
        approved: pr.review_decision.as_deref() == Some("APPROVED"),
        ..Default::default()
    };
    let r = read_detail(pr, author, cfg, &mut a);
    note_security(&r, &mut a);
    a.approval_stale = a.approved && pr.approved_on_head.is_empty();
    let problems = problems(&r, &a);
    let running = still_running(pr, &r, &a);

    a.ease_reasons = ease_reasons(pr, &r, &a, author);
    a.ease_score = a.ease_reasons.iter().map(|r| r.points).sum();
    a.ease = match a.ease_score {
        i32::MIN..=2 => Ease::Easy,
        3..=5 => Ease::Medium,
        _ => Ease::Hard,
    };
    if let Some(claude) = pr.analysis.as_ref().and_then(|c| c.ease) {
        if claude > a.ease {
            a.ease = claude;
            a.ease_raised_by_claude = true;
        }
    }

    a.quality_reasons = quality_reasons(&r, &a, author);
    a.quality = a
        .quality_reasons
        .iter()
        .map(|r| r.points)
        .sum::<i32>()
        .clamp(0, 100) as u8;
    a.high_quality = a.inspected && a.quality >= cfg.high_quality;

    a.state = headline(&Signals {
        draft: pr.is_draft,
        problem: !problems.is_empty(),
        conflicted: a.conflicted,
        inspected: a.inspected,
        running: !running.is_empty(),
        ci_passed: a.ci == "success",
        approved: a.approved && !a.approval_stale,
        high_quality: a.high_quality,
    });
    a.state_reasons = state_reasons(pr, &a, problems, running);

    let notable = pr.reactions >= 3 || pr.comments_total >= 8;
    a.interesting = notable || pr.analysis.as_ref().is_some_and(|c| c.interesting);

    a.focus = focus(pr, &a, author);
    a
}

/// Sorts first by status, which the State column shows. The rest only breaks ties
/// within a status and adds up to far less than the 1000 between statuses.
fn focus(pr: &Pr, a: &Assessment, author: &AuthorInfo) -> i32 {
    let mut f = match a.state {
        PrState::Landable => 8000,
        PrState::Promising => 7000,
        PrState::Reviewable => 6000,
        PrState::CiNotRun => 5000,
        PrState::Checking => 4000,
        PrState::NotInspected => 3000,
        PrState::Conflicted => 2000,
        PrState::NeedsWork => 1000,
        PrState::Draft => 0,
    };
    f += a.quality as i32;
    f += match a.ease {
        Ease::Easy => 60,
        Ease::Medium => 30,
        Ease::Hard => 0,
    };
    f += match author.tier {
        Tier::Core => 40,
        Tier::Frequent => 30,
        _ => 0,
    };
    if a.interesting {
        f += 50;
    }
    if pr.review_decision.as_deref() == Some("APPROVED") {
        f += 50;
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Analysis, Check, FileChange, Finding, PrDetail, RepoData};
    use chrono::{DateTime, Utc};

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn pr() -> Pr {
        Pr {
            number: 1,
            author: "ann".into(),
            association: "CONTRIBUTOR".into(),
            mergeable: "MERGEABLE".into(),
            merge_state: "CLEAN".into(),
            head_oid: "h".into(),
            additions: 60,
            deletions: 5,
            changed_files: 2,
            ci_rollup: Some("SUCCESS".into()),
            updated_at: Some(t("2026-10-30T00:00:00Z")),
            listed_at: Some(t("2026-10-30T01:00:00Z")),
            ..Default::default()
        }
    }

    fn inspected(mut p: Pr, d: PrDetail) -> Pr {
        p.detail = Some(PrDetail {
            fetched_at: p.listed_at,
            for_updated_at: p.updated_at,
            for_head: p.head_oid.clone(),
            body: "A thorough description of what changes here and why, with enough words in it to read as a real explanation of the change, its motivation, its alternatives, and how it was tested by hand on all platforms.".into(),
            rabbit: RabbitStatus::Reviewed,
            files: vec![
                FileChange { path: "crates/a/src/lib.rs".into(), additions: 40, ..Default::default() },
                FileChange { path: "crates/a/tests/lib.rs".into(), additions: 20, ..Default::default() },
            ],
            checks: vec![check("test", "SUCCESS")],
            ..d
        });
        p
    }

    fn check(name: &str, state: &str) -> Check {
        Check {
            name: name.into(),
            state: state.into(),
            url: None,
        }
    }

    fn clean() -> Pr {
        inspected(pr(), PrDetail::default())
    }

    fn approved(mut p: Pr) -> Pr {
        p.review_decision = Some("APPROVED".into());
        p.approved_by = vec!["bo".into()];
        p.approved_on_head = vec!["bo".into()];
        p
    }

    fn finding(sev: Severity, security: bool) -> Finding {
        Finding {
            severity: sev,
            security,
            title: "x".into(),
            ..Default::default()
        }
    }

    fn run(p: &Pr) -> Assessment {
        let cfg = Config::default();
        let author = author_info(p, &RepoData::default(), 0, true, &cfg);
        assess(p, &author, &cfg)
    }

    /// The truth table, row by row. `None` means that column doesn't matter, and is
    /// tried both ways. Keep this in step with `STATUS_HELP` in ui/src/model.ts.
    #[test]
    fn the_truth_table() {
        type Row = ([Option<bool>; 8], PrState);
        let (y, n, x) = (Some(true), Some(false), None);
        // draft, problem, conflicted, inspected, running, ci passed, approved, high quality
        let table: [Row; 9] = [
            ([y, x, x, x, x, x, x, x], PrState::Draft),
            ([n, y, x, x, x, x, x, x], PrState::NeedsWork),
            ([n, n, y, x, x, x, x, x], PrState::Conflicted),
            ([n, n, n, n, x, x, x, x], PrState::NotInspected),
            ([n, n, n, y, y, x, x, x], PrState::Checking),
            ([n, n, n, y, n, n, x, x], PrState::CiNotRun),
            ([n, n, n, y, n, y, y, x], PrState::Landable),
            ([n, n, n, y, n, y, n, y], PrState::Promising),
            ([n, n, n, y, n, y, n, n], PrState::Reviewable),
        ];
        let mut seen = 0;
        for bits in 0..256u32 {
            let b: Vec<bool> = (0..8).map(|i| bits & (1 << i) != 0).collect();
            let s = Signals {
                draft: b[0],
                problem: b[1],
                conflicted: b[2],
                inspected: b[3],
                running: b[4],
                ci_passed: b[5],
                approved: b[6],
                high_quality: b[7],
            };
            let rows: Vec<&Row> = table
                .iter()
                .filter(|(cols, _)| cols.iter().zip(&b).all(|(c, v)| c.is_none_or(|c| c == *v)))
                .collect();
            assert_eq!(
                rows.len(),
                1,
                "every combination matches exactly one row: {s:?}"
            );
            assert_eq!(headline(&s), rows[0].1, "{s:?}");
            seen += 1;
        }
        assert_eq!(seen, 256);
    }

    #[test]
    fn a_clean_pr_with_no_approval_is_promising_not_landable() {
        let a = run(&clean());
        assert_eq!(a.state, PrState::Promising);
        assert_eq!(a.ease, Ease::Easy);
        assert!(
            a.quality >= 75 && a.high_quality && a.tests_touched,
            "{a:#?}"
        );
    }

    #[test]
    fn landable_takes_a_current_approval() {
        let a = run(&approved(clean()));
        assert_eq!(a.state, PrState::Landable);
        assert!(a.approved && !a.approval_stale);
    }

    #[test]
    fn a_person_outranks_the_score() {
        let mut p = approved(clean());
        p.detail.as_mut().unwrap().body = String::new();
        p.detail.as_mut().unwrap().files.truncate(1);
        let a = run(&p);
        assert!(a.quality < 75, "{}", a.quality);
        assert_eq!(a.state, PrState::Landable);
    }

    #[test]
    fn a_low_score_with_no_approval_is_reviewable() {
        let mut p = clean();
        p.detail.as_mut().unwrap().body = String::new();
        p.detail.as_mut().unwrap().files.truncate(1);
        assert_eq!(run(&p).state, PrState::Reviewable);
    }

    #[test]
    fn an_approval_of_an_older_commit_doesnt_count() {
        let mut p = approved(clean());
        p.approved_on_head.clear();
        let a = run(&p);
        assert!(a.approved && a.approval_stale);
        assert_eq!(a.state, PrState::Promising);
        assert!(a.state_reasons.iter().any(|r| r.contains("predates")));
    }

    #[test]
    fn approval_is_a_flag_that_conflicts_and_problems_override() {
        let mut p = approved(clean());
        p.mergeable = "CONFLICTING".into();
        let a = run(&p);
        assert_eq!(a.state, PrState::Conflicted);
        assert!(a.approved && a.conflicted);

        let p = approved(inspected(
            pr(),
            PrDetail {
                human_threads_open: 1,
                ..Default::default()
            },
        ));
        assert_eq!(run(&p).state, PrState::NeedsWork);
    }

    #[test]
    fn conflicts_alone_are_conflicted_not_needs_work() {
        let mut p = pr();
        p.mergeable = "CONFLICTING".into();
        let a = run(&p);
        assert_eq!(a.state, PrState::Conflicted);
        assert!(a.state_reasons.iter().any(|r| r.contains("Not inspected")));

        p.merge_state = "DIRTY".into();
        p.ci_rollup = Some("FAILURE".into());
        let a = run(&p);
        assert_eq!(a.state, PrState::NeedsWork);
        assert!(a.conflicted, "the flag still shows");
    }

    #[test]
    fn ci_that_never_ran_outranks_approval() {
        let mut p = approved(clean());
        p.detail.as_mut().unwrap().checks = vec![check("CodeRabbit", "SUCCESS")];
        let a = run(&p);
        assert_eq!((a.ci.as_str(), a.state), ("none", PrState::CiNotRun));
        assert!(a.state_reasons[0].contains("approving"));
    }

    #[test]
    fn an_uninspected_pr_says_so_and_never_counts_as_high_quality() {
        let a = run(&pr());
        assert_eq!(a.state, PrState::NotInspected);
        assert!(!a.high_quality);
        assert_eq!(run(&approved(pr())).state, PrState::NotInspected);
    }

    #[test]
    fn a_pr_that_changed_since_inspection_is_not_inspected() {
        let mut p = clean();
        p.head_oid = "new".into();
        let a = run(&p);
        assert_eq!(a.state, PrState::NotInspected);
        assert_eq!(a.state_reasons, ["Changed since it was last inspected"]);
    }

    #[test]
    fn drafts_are_drafts_whatever_else_is_wrong() {
        let mut p = approved(pr());
        p.is_draft = true;
        p.mergeable = "CONFLICTING".into();
        assert_eq!(run(&p).state, PrState::Draft);
    }

    #[test]
    fn each_problem_means_needs_work() {
        let mut p = pr();
        p.ci_rollup = Some("FAILURE".into());
        assert_eq!(run(&p).state, PrState::NeedsWork);

        let mut p = pr();
        p.review_decision = Some("CHANGES_REQUESTED".into());
        assert_eq!(run(&p).state, PrState::NeedsWork);

        let p = inspected(
            pr(),
            PrDetail {
                findings: vec![finding(Severity::Major, false)],
                ..Default::default()
            },
        );
        let a = run(&p);
        assert_eq!(a.state, PrState::NeedsWork);
        assert_eq!((a.rabbit_flags, a.rabbit_nits), (1, 0));

        let p = inspected(
            pr(),
            PrDetail {
                human_threads_open: 2,
                ..Default::default()
            },
        );
        assert_eq!(run(&p).state, PrState::NeedsWork);
    }

    #[test]
    fn running_things_mean_checking() {
        let mut p = clean();
        p.detail.as_mut().unwrap().checks = vec![check("test", "IN_PROGRESS")];
        assert_eq!(run(&p).state, PrState::Checking);
        let mut p = clean();
        p.mergeable = "UNKNOWN".into();
        assert_eq!(run(&p).state, PrState::Checking);
        let mut p = clean();
        p.detail.as_mut().unwrap().rabbit = RabbitStatus::InProgress;
        let a = run(&p);
        assert_eq!(a.state, PrState::Checking);
        assert_eq!(a.state_reasons, ["CodeRabbit is still reviewing"]);
    }

    #[test]
    fn cancelled_and_skipped_checks_say_nothing() {
        let mut p = clean();
        p.detail.as_mut().unwrap().checks = vec![
            check("test", "SUCCESS"),
            check("lint", "CANCELLED"),
            check("fuzz", "SKIPPED"),
        ];
        assert_eq!(run(&p).ci, "success");
        p.detail.as_mut().unwrap().checks = vec![check("lint", "CANCELLED")];
        assert_eq!(run(&p).state, PrState::CiNotRun);
        p.detail.as_mut().unwrap().checks =
            vec![check("test", "FAILURE"), check("lint", "CANCELLED")];
        assert_eq!(run(&p).ci, "failure");
    }

    #[test]
    fn nits_lower_quality_without_being_a_problem() {
        let base = run(&clean());
        let p = inspected(
            pr(),
            PrDetail {
                findings: vec![
                    finding(Severity::Minor, false),
                    finding(Severity::Trivial, false),
                ],
                ..Default::default()
            },
        );
        let a = run(&p);
        assert_eq!((a.rabbit_flags, a.rabbit_nits), (0, 2));
        assert!(a.quality < base.quality);
        assert_ne!(a.state, PrState::NeedsWork);
    }

    #[test]
    fn resolved_and_outdated_findings_dont_count() {
        let mut gone = finding(Severity::Critical, true);
        gone.resolved = true;
        let mut moved = finding(Severity::Major, false);
        moved.outdated = true;
        let a = run(&inspected(
            pr(),
            PrDetail {
                findings: vec![gone, moved],
                ..Default::default()
            },
        ));
        assert_eq!((a.rabbit_flags, a.security_flags), (0, 0));
        assert_eq!(a.state, PrState::Promising);
    }

    #[test]
    fn security_findings_and_checks_are_flagged_separately() {
        let p = inspected(
            pr(),
            PrDetail {
                findings: vec![
                    finding(Severity::Major, true),
                    finding(Severity::Minor, true),
                ],
                ..Default::default()
            },
        );
        let a = run(&p);
        assert_eq!((a.security_flags, a.security_nits), (1, 1));
        assert_eq!(a.security_reasons, ["1 CodeRabbit security finding(s)"]);

        let mut p = clean();
        p.detail
            .as_mut()
            .unwrap()
            .checks
            .push(check("Git dependencies come from storytold", "FAILURE"));
        let a = run(&p);
        assert_eq!(a.security_flags, 1);
        assert_eq!(a.state, PrState::NeedsWork);
    }

    #[test]
    fn sensitive_paths_flag_outsiders_only_and_arent_a_problem() {
        let mut p = clean();
        p.detail.as_mut().unwrap().files.push(FileChange {
            path: ".github/workflows/ci.yml".into(),
            ..Default::default()
        });
        let a = run(&p);
        assert_eq!(a.sensitive_paths, [".github/workflows/ci.yml"]);
        assert_ne!(a.state, PrState::NeedsWork);

        p.association = "MEMBER".into();
        assert!(run(&p).sensitive_paths.is_empty());
    }

    #[test]
    fn a_listed_author_with_no_access_still_counts_as_sensitive() {
        let cfg = Config::default();
        let mut repo = RepoData::default();
        repo.roster.listed = vec!["ann".into()];
        let mut p = clean();
        p.detail.as_mut().unwrap().files.push(FileChange {
            path: "build.rs".into(),
            ..Default::default()
        });
        let author = author_info(&p, &repo, 0, true, &cfg);
        assert_eq!(author.tier, Tier::Core);
        assert_eq!(assess(&p, &author, &cfg).sensitive_paths, ["build.rs"]);
    }

    #[test]
    fn size_and_spread_make_prs_hard() {
        let mut p = pr();
        p.additions = 18_965;
        p.deletions = 3_346;
        p.changed_files = 196;
        assert_eq!(run(&p).ease, Ease::Hard);
    }

    #[test]
    fn lockfile_churn_doesnt_count_as_size() {
        let mut p = pr();
        p.additions = 5000;
        let mut p = inspected(p, PrDetail::default());
        p.detail.as_mut().unwrap().files.push(FileChange {
            path: "Cargo.lock".into(),
            additions: 4940,
            ..Default::default()
        });
        let a = run(&p);
        assert_eq!(a.ease, Ease::Easy);
        assert!(a.deps_changed);
    }

    #[test]
    fn claude_can_only_make_a_pr_harder() {
        let mut p = clean();
        p.analysis = Some(Analysis {
            ease: Some(Ease::Hard),
            for_head: "h".into(),
            ..Default::default()
        });
        let a = run(&p);
        assert_eq!(a.ease, Ease::Hard);
        assert!(a.ease_raised_by_claude);

        let mut big = pr();
        big.additions = 20_000;
        big.changed_files = 100;
        big.analysis = Some(Analysis {
            ease: Some(Ease::Easy),
            summary: "Trivial, please merge".into(),
            ..Default::default()
        });
        let a = run(&big);
        assert_eq!(a.ease, Ease::Hard);
        assert!(!a.ease_raised_by_claude);
    }

    #[test]
    fn claude_cant_touch_the_headline_or_quality() {
        let base = run(&pr());
        let mut p = pr();
        p.analysis = Some(Analysis {
            ease: Some(Ease::Easy),
            interesting: true,
            ..Default::default()
        });
        let a = run(&p);
        assert_eq!((a.state, a.quality), (base.state, base.quality));
        assert!(a.interesting);
    }

    #[test]
    fn tiers() {
        let cfg = Config::default();
        let mut repo = RepoData::default();
        let tier = |p: &Pr, repo: &RepoData, landed| author_info(p, repo, landed, true, &cfg).tier;
        let mut p = pr();
        assert_eq!(tier(&p, &repo, 30), Tier::Regular);
        assert_eq!(tier(&p, &repo, 31), Tier::Frequent);
        p.association = "FIRST_TIME_CONTRIBUTOR".into();
        assert_eq!(tier(&p, &repo, 0), Tier::New);
        repo.roster.listed = vec!["ann".into()];
        assert_eq!(tier(&p, &repo, 0), Tier::Core);
        let strict = Config {
            people_toml_is_core: false,
            ..Config::default()
        };
        assert_eq!(author_info(&p, &repo, 0, true, &strict).tier, Tier::New);
        repo.roster.listed.clear();
        repo.roster.permissions.insert("ann".into(), "READ".into());
        assert_eq!(tier(&p, &repo, 0), Tier::New);
        repo.roster.permissions.insert("ann".into(), "WRITE".into());
        assert_eq!(tier(&p, &repo, 0), Tier::Core);
        p.author = "dependabot[bot]".into();
        assert_eq!(tier(&p, &repo, 500), Tier::Bot);
    }

    #[test]
    fn landed_counts_are_case_insensitive() {
        let mut repo = RepoData::default();
        repo.history.merged.insert(1, "Ann".into());
        repo.history.merged.insert(2, "ann".into());
        repo.history.merged.insert(3, "bo".into());
        assert_eq!(landed_counts(&repo)["ann"], 2);
    }

    #[test]
    fn ci_prefers_whichever_is_newer() {
        let mut p = clean();
        p.detail.as_mut().unwrap().checks = vec![check("test", "IN_PROGRESS")];
        p.ci_rollup = Some("PENDING".into());
        assert_eq!(ci_state(&p), "pending");
        // A later listing saw the checks finish; the detail didn't.
        p.ci_rollup = Some("SUCCESS".into());
        p.listed_at = Some(t("2026-10-30T02:00:00Z"));
        assert_eq!(ci_state(&p), "success");
    }

    #[test]
    fn coderabbit_doesnt_hold_up_ci() {
        let mut p = clean();
        let d = p.detail.as_mut().unwrap();
        d.rabbit = RabbitStatus::InProgress;
        d.checks.push(check("CodeRabbit", "IN_PROGRESS"));
        p.ci_rollup = Some("PENDING".into());
        assert_eq!(run(&p).ci, "success");
    }

    #[test]
    fn nothing_lifts_a_pr_past_the_next_status() {
        // As much of a boost as a PR can get, at the bottom of a status.
        let mut p = pr();
        p.mergeable = "CONFLICTING".into();
        p.reactions = 5;
        p.review_decision = Some("APPROVED".into());
        p.association = "MEMBER".into();
        let lifted = run(&p);
        assert_eq!(lifted.state, PrState::Conflicted);
        // As little as one can get, one status up.
        let mut q = pr();
        q.additions = 50_000;
        q.changed_files = 500;
        let low = run(&q);
        assert_eq!(low.state, PrState::NotInspected);
        assert!(
            low.focus > lifted.focus,
            "{} vs {}",
            low.focus,
            lifted.focus
        );
    }

    #[test]
    fn better_statuses_sort_first() {
        let landable = run(&approved(clean()));
        let promising = run(&clean());
        let mut p = pr();
        p.mergeable = "CONFLICTING".into();
        let conflicted = run(&p);
        assert!(landable.focus > promising.focus && promising.focus > conflicted.focus);
    }
}
