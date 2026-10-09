//! A transparent, deterministic heuristic for "what should be fixed first".
//!
//! Every open issue gets points for what it says (crash / hang / data loss
//! beats "doesn't work" beats everything else), for who filed it (a user
//! report outranks the team's own automated findings), for how many people
//! are piling on (reactions, comments), for naming the latest build, and for
//! being fresh. Feature requests, questions and docs nits are pushed down.
//! Each contribution is recorded as a short reason so the UI can show *why*.

use chrono::{DateTime, Duration, Utc};

use crate::model::UrgentIssue;

#[derive(Debug, Clone, Default)]
pub struct IssueInput {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub comments: u64,
    pub reactions: u64,
    pub labels: Vec<String>,
    /// GitHub's `authorAssociation` (`NONE`, `CONTRIBUTOR`, `MEMBER`…).
    pub author_association: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub score: f64,
    pub critical: bool,
    pub reasons: Vec<String>,
}

/// Phrases that mean the app is unusable or user data is at risk.
const CRITICAL: &[(&str, &str)] = &[
    ("crash", "crash"),
    ("crashes", "crash"),
    ("crashed", "crash"),
    ("crashing", "crash"),
    ("panic", "panic"),
    ("panics", "panic"),
    ("segfault", "crash"),
    ("data loss", "data loss"),
    ("lost my", "data loss"),
    ("lost work", "data loss"),
    ("corrupt", "corruption"),
    ("corrupted", "corruption"),
    ("corrupts", "corruption"),
    ("freeze", "freeze"),
    ("freezes", "freeze"),
    ("frozen", "freeze"),
    ("hang", "hang"),
    ("hangs", "hang"),
    ("unresponsive", "hang"),
    ("not responding", "hang"),
    ("won't open", "won't open"),
    ("wont open", "won't open"),
    ("can't open", "won't open"),
    ("cannot open", "won't open"),
    ("won't launch", "won't launch"),
    ("wont launch", "won't launch"),
    ("won't start", "won't launch"),
    ("doesn't start", "won't launch"),
    ("does not start", "won't launch"),
    ("fails to launch", "won't launch"),
    ("fails to start", "won't launch"),
    ("not launching", "won't launch"),
    ("black screen", "blank screen"),
    ("blank screen", "blank screen"),
    ("white screen", "blank screen"),
    ("security", "security"),
    ("vulnerability", "security"),
    ("malware", "security"),
    ("virus", "security"),
    ("trojan", "security"),
];

/// Phrases that mean something is broken, but not catastrophically.
const BROKEN: &[&str] = &[
    "bug",
    "broken",
    "regression",
    "doesn't work",
    "does not work",
    "not working",
    "doesn't",
    "does not",
    "don't",
    "fails",
    "failed",
    "failure",
    "error",
    "can't",
    "cannot",
    "unable",
    "wrong",
    "lost",
    "install",
    "installer",
    "slow",
    "lag",
    "glitch",
];

const FEATURE_LABELS: &[&str] = &[
    "enhancement",
    "feature",
    "feature request",
    "question",
    "documentation",
    "docs",
];
const EXCLUDE_LABELS: &[&str] = &["wontfix", "duplicate", "invalid", "not planned"];
const FEATURE_PREFIXES: &[&str] = &[
    "[feature",
    "feature request",
    "feature:",
    "request:",
    "[request",
    "suggestion",
    "[suggestion",
    "idea:",
    "add ",
    "support ",
    "please add",
    "fix(docs)",
    "docs:",
    "docs(",
    "[docs",
    "question",
    "[question",
];

/// Lowercase, normalize apostrophes, and turn punctuation into spaces, padded
/// so `" phrase "` matches whole words only.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(' ');
    let mut last_space = true;
    for c in s.chars().flat_map(char::to_lowercase) {
        let c = if c == '\u{2019}' || c == '`' { '\'' } else { c };
        if c.is_alphanumeric() || c == '\'' {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    if !last_space {
        out.push(' ');
    }
    out
}

fn has_phrase(text: &str, phrase: &str) -> bool {
    text.contains(&format!(" {phrase} "))
}

/// `"v0.5.0"` → `"0.5"`: issues name the version loosely ("0.5", "0.50", "v0.5.0").
fn major_minor(tag: &str) -> Option<String> {
    let t = tag.trim_start_matches(['v', 'V']);
    let mut parts = t.split(['.', '-']);
    let major = parts
        .next()
        .filter(|p| p.chars().all(|c| c.is_ascii_digit()))?;
    let minor = parts
        .next()
        .filter(|p| p.chars().all(|c| c.is_ascii_digit()))?;
    (!major.is_empty() && !minor.is_empty()).then(|| format!("{major}.{minor}"))
}

fn mentions_version(title: &str, mm: &str) -> bool {
    let b = title.as_bytes();
    title.match_indices(mm).any(|(i, _)| {
        let before_ok = i == 0 || !b[i - 1].is_ascii_digit();
        let after = b.get(i + mm.len()).copied();
        // "0.5" matches "0.5", "0.5.0", "0.50" (a common typo for 0.5.0) but not "0.51".
        let after_ok = match after {
            None => true,
            Some(c) if !c.is_ascii_digit() => true,
            Some(b'0') => !b.get(i + mm.len() + 1).is_some_and(|c| c.is_ascii_digit()),
            _ => false,
        };
        before_ok && after_ok
    })
}

/// `None` when the issue is closed-in-spirit (wontfix, duplicate, invalid).
pub fn score_issue(
    issue: &IssueInput,
    now: DateTime<Utc>,
    latest_tag: Option<&str>,
) -> Option<Scored> {
    let labels: Vec<String> = issue.labels.iter().map(|l| l.to_lowercase()).collect();
    if labels.iter().any(|l| EXCLUDE_LABELS.contains(&l.as_str())) {
        return None;
    }
    let title_lc = issue.title.to_lowercase();
    let text = normalize(&format!("{} {}", issue.title, issue.labels.join(" ")));

    let mut score = 0.0;
    let mut reasons: Vec<String> = Vec::new();

    let feature_like = labels.iter().any(|l| FEATURE_LABELS.contains(&l.as_str()))
        || FEATURE_PREFIXES
            .iter()
            .any(|p| title_lc.trim_start().starts_with(p));

    let mut critical_hits: Vec<&str> = Vec::new();
    for (phrase, why) in CRITICAL {
        if has_phrase(&text, phrase) && !critical_hits.contains(why) {
            critical_hits.push(why);
        }
    }
    let critical = !critical_hits.is_empty() && !feature_like;
    if !critical_hits.is_empty() {
        score += 40.0;
        reasons.push(critical_hits.join(", "));
    } else if BROKEN.iter().any(|p| has_phrase(&text, p)) {
        score += 15.0;
        reasons.push("broken".into());
    }

    if labels.iter().any(|l| l == "bug" || l == "regression") {
        score += 15.0;
        reasons.push("bug label".into());
    }
    if labels
        .iter()
        .any(|l| l == "reliability" || l == "file-compat" || l == "performance")
    {
        score += 6.0;
    }
    if feature_like {
        score -= 25.0;
        reasons.push("request/docs".into());
    }

    match issue.author_association.as_str() {
        "NONE" | "FIRST_TIMER" | "FIRST_TIME_CONTRIBUTOR" => {
            score += 10.0;
            reasons.push("user report".into());
        }
        _ => {}
    }

    let engagement = 3.0 * issue.reactions as f64 + 2.0 * issue.comments as f64;
    if engagement > 0.0 {
        score += (10.0 * (1.0 + engagement).ln()).min(40.0);
        if issue.reactions > 0 {
            reasons.push(format!("{} 👍", issue.reactions));
        }
        if issue.comments > 0 {
            reasons.push(format!("{} 💬", issue.comments));
        }
    }

    if let Some(mm) = latest_tag.and_then(major_minor) {
        if mentions_version(&title_lc, &mm) {
            score += 10.0;
            reasons.push("latest build".into());
        }
    }

    if let Some(created) = issue.created_at {
        let age = now - created;
        if age < Duration::hours(24) {
            score += 8.0;
            reasons.push("new".into());
        } else if age < Duration::hours(72) {
            score += 4.0;
        }
    }
    if let Some(updated) = issue.updated_at {
        if now - updated > Duration::days(30) {
            score -= 5.0;
        }
    }

    Some(Scored {
        score: score.max(0.0),
        critical,
        reasons,
    })
}

/// Returns the top `keep` issues (score > 0, best first, ties → newest), the
/// repo urgency (sum of the top five scores) and the count of critical issues.
pub fn rank_issues(
    issues: &[IssueInput],
    now: DateTime<Utc>,
    latest_tag: Option<&str>,
    keep: usize,
) -> (Vec<UrgentIssue>, f64, u64) {
    let mut seen = std::collections::HashSet::new();
    let mut ranked: Vec<UrgentIssue> = issues
        .iter()
        .filter(|i| seen.insert(i.number))
        .filter_map(|i| {
            let s = score_issue(i, now, latest_tag)?;
            Some(UrgentIssue {
                number: i.number,
                title: i.title.clone(),
                url: i.url.clone(),
                created_at: i.created_at,
                updated_at: i.updated_at,
                comments: i.comments,
                reactions: i.reactions,
                labels: i.labels.clone(),
                score: s.score.round(),
                critical: s.critical,
                reasons: s.reasons,
            })
        })
        .collect();
    let critical = ranked.iter().filter(|u| u.critical).count() as u64;
    ranked.retain(|u| u.score > 0.0);
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(b.created_at.cmp(&a.created_at))
    });
    let urgency = ranked.iter().take(5).map(|u| u.score).sum();
    ranked.truncate(keep);
    (ranked, urgency, critical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(title: &str, assoc: &str) -> IssueInput {
        IssueInput {
            number: title.len() as u64,
            title: title.into(),
            author_association: assoc.into(),
            ..Default::default()
        }
    }

    fn score(i: &IssueInput) -> f64 {
        score_issue(i, Utc::now(), Some("v0.5.0")).unwrap().score
    }

    #[test]
    fn normalize_is_word_bounded() {
        let t = normalize("App CRASHES—on start’s (Windows)");
        assert!(has_phrase(&t, "crashes"));
        assert!(has_phrase(&t, "start's"));
        assert!(!has_phrase(&normalize("hangar"), "hang"));
    }

    #[test]
    fn crash_report_beats_automated_docs_fix() {
        let crash = issue(
            "[Windows 11 0.50] Merge Down freezes on large canvas",
            "NONE",
        );
        let docs = issue(
            "fix(docs): layer-type errors say \"a Adjustment layer\"",
            "CONTRIBUTOR",
        );
        let s = score_issue(&crash, Utc::now(), Some("v0.5.0")).unwrap();
        assert!(s.critical);
        assert!(
            s.reasons.iter().any(|r| r == "latest build"),
            "{:?}",
            s.reasons
        );
        assert!(s.reasons.iter().any(|r| r == "user report"));
        assert!(score(&crash) > score(&docs) + 40.0);
        assert!(!score_issue(&docs, Utc::now(), None).unwrap().critical);
    }

    #[test]
    fn feature_requests_sink_and_wontfix_vanishes() {
        let bug = issue("delete button doesn't delete a layer on windows", "NONE");
        let feat = issue("Request: Gradient support for shape fill", "NONE");
        assert!(score(&bug) > score(&feat));
        let mut dup = issue("App crashes", "NONE");
        dup.labels = vec!["duplicate".into()];
        assert!(score_issue(&dup, Utc::now(), None).is_none());
        let feat_crash = issue("Add crash reporting", "NONE");
        assert!(!score_issue(&feat_crash, Utc::now(), None).unwrap().critical);
    }

    #[test]
    fn engagement_is_capped_and_counted() {
        let quiet = issue("Bug: layer locked but editing still works", "NONE");
        let mut loud = quiet.clone();
        loud.reactions = 15;
        loud.comments = 13;
        let mut louder = quiet.clone();
        louder.reactions = 5000;
        assert!(score(&loud) > score(&quiet) + 30.0);
        assert!(score(&louder) - score(&quiet) <= 40.0 + 1e-9);
    }

    #[test]
    fn version_matching() {
        assert_eq!(major_minor("v0.5.0").as_deref(), Some("0.5"));
        assert_eq!(major_minor("v0.1.0-rc.3").as_deref(), Some("0.1"));
        assert_eq!(major_minor("nightly"), None);
        assert!(mentions_version("[windows 0.5.0] x", "0.5"));
        assert!(mentions_version("[windows 11 0.50] x", "0.5"));
        assert!(!mentions_version("v10.5 x", "0.5"));
        assert!(!mentions_version("0.51 x", "0.5"));
    }

    #[test]
    fn ranking_dedupes_sorts_and_sums_top_five() {
        let now = Utc::now();
        let mut v: Vec<IssueInput> = (1..=8)
            .map(|n| IssueInput {
                number: n,
                title: if n % 2 == 0 {
                    "App crashes"
                } else {
                    "Add gradients"
                }
                .into(),
                author_association: "NONE".into(),
                ..Default::default()
            })
            .collect();
        v.push(v[1].clone());
        let (ranked, urgency, critical) = rank_issues(&v, now, None, 3);
        assert_eq!(critical, 4);
        assert_eq!(ranked.len(), 3);
        assert!(ranked.iter().all(|u| u.critical));
        assert!(ranked.windows(2).all(|w| w[0].score >= w[1].score));
        assert_eq!(
            urgency,
            4.0 * 50.0,
            "4 crashes at 50 (40 + user 10), features score 0"
        );
    }
}
