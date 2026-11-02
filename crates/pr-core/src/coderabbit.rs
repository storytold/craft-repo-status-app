//! Reads CodeRabbit's inline comments and reviews. Comments start with a header
//! like `**<picture…> Minor** · **Functional Correctness** · **Quick win**`, or,
//! on older ones, `🎯 Functional Correctness | 🟡 Minor | ⚡ Quick win`, then a
//! bold title.

use crate::model::{Finding, Severity};

pub fn is_coderabbit(login: &str) -> bool {
    login.to_lowercase().starts_with("coderabbit")
}

/// Drops HTML tags and markdown emphasis.
fn plain(line: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in line.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            '*' => {}
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Drops leading emoji and punctuation from a header token.
fn token(s: &str) -> &str {
    s.trim_matches(|c: char| !c.is_alphanumeric() && c != '&')
        .trim()
}

fn severity(tok: &str) -> Option<Severity> {
    match tok.to_lowercase().as_str() {
        "critical" | "blocker" => Some(Severity::Critical),
        "major" | "high" | "potential issue" => Some(Severity::Major),
        "minor" | "medium" | "refactor suggestion" => Some(Severity::Minor),
        "trivial" | "nitpick" | "nit" | "info" | "low" => Some(Severity::Trivial),
        _ => None,
    }
}

struct Header {
    severity: Severity,
    category: String,
    effort: String,
}

fn header(line: &str) -> Option<Header> {
    let text = plain(line);
    let tokens: Vec<&str> = text
        .split(['·', '|'])
        .map(token)
        .filter(|t| !t.is_empty())
        .collect();
    let severity = tokens.iter().find_map(|t| severity(t))?;
    let mut effort = String::new();
    let mut category = String::new();
    for t in tokens {
        let lower = t.to_lowercase();
        if self::severity(t).is_some() || lower.starts_with("detected with") {
            continue;
        }
        if lower == "quick win" || lower == "heavy lift" {
            effort = t.to_string();
        } else if category.is_empty() {
            category = t.to_string();
        }
    }
    Some(Header {
        severity,
        category,
        effort,
    })
}

fn bold_line(line: &str) -> Option<&str> {
    let inner = line.trim().strip_prefix("**")?.strip_suffix("**")?;
    (!inner.is_empty() && !inner.contains("**") && !inner.contains('<')).then_some(inner)
}

fn is_security(category: &str, title: &str) -> bool {
    let category = category.to_lowercase();
    if category.contains("security") || category.contains("privacy") {
        return true;
    }
    let title = title.to_lowercase();
    [
        "security",
        "vulnerab",
        "injection",
        "credential",
        "secret",
        "xss",
        "path traversal",
        "privilege",
    ]
    .iter()
    .any(|w| title.contains(w))
}

/// Parses the first comment of a CodeRabbit review thread. The location and
/// state are left for the caller. A comment with no recognizable header counts
/// as a minor finding, titled by its first line.
pub fn parse_finding(body: &str) -> Finding {
    let lines: Vec<&str> = body.lines().collect();
    let found = lines
        .iter()
        .enumerate()
        .find_map(|(i, l)| header(l).map(|h| (i, h)));
    let (start, head) = match found {
        Some((i, h)) => (i + 1, h),
        None => (
            0,
            Header {
                severity: Severity::Minor,
                category: String::new(),
                effort: String::new(),
            },
        ),
    };
    let rest = &lines[start..];
    let is_text = |l: &&&str| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('<') && !plain(t).is_empty()
    };
    let bold = rest
        .iter()
        .enumerate()
        .find_map(|(i, l)| bold_line(l).map(|t| (i, t.to_string())));
    let (title, after) = match bold {
        Some((i, t)) => (t, i + 1),
        None => match rest.iter().position(|l| is_text(&l)) {
            Some(i) => (plain(rest[i]), i + 1),
            None => (String::new(), rest.len()),
        },
    };
    let mut snippet = String::new();
    for l in &rest[after.min(rest.len())..] {
        let t = l.trim();
        if t.starts_with("<details") || t.starts_with("<!--") || t.starts_with("```") {
            break;
        }
        if t.is_empty() || t.starts_with('<') {
            continue;
        }
        if !snippet.is_empty() {
            snippet.push(' ');
        }
        snippet.push_str(t);
        if snippet.chars().count() >= 320 {
            break;
        }
    }
    let security = is_security(&head.category, &title);
    Finding {
        severity: head.severity,
        category: head.category,
        effort: head.effort,
        title,
        snippet: truncate(&snippet, 320),
        security,
        ..Default::default()
    }
}

/// `s` cut to at most `max` characters, marked if cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut cut: String = s.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The N in a review body's "Actionable comments posted: N".
pub fn actionable_count(review_body: &str) -> Option<u64> {
    let (_, after) = review_body.split_once("Actionable comments posted:")?;
    after
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIC: &str = r#"<picture><img src="https://x/amber-16.svg" width="16" height="16" align="absmiddle" alt=""></picture>"#;

    #[test]
    fn reads_the_current_format() {
        let body = format!(
            "<!-- coderabbit-ai-generated:v1 -->\n\n**{PIC} Minor** · **Maintainability & Code Quality** · **Quick win**\n\n**Rename the link to match its target.**\n\nThe link text says \"validation notes\" but\npoints elsewhere.\n\n<details>\n<summary>Fix</summary>\ncode\n</details>"
        );
        let f = parse_finding(&body);
        assert_eq!(f.severity, Severity::Minor);
        assert_eq!(f.category, "Maintainability & Code Quality");
        assert_eq!(f.effort, "Quick win");
        assert_eq!(f.title, "Rename the link to match its target.");
        assert_eq!(
            f.snippet,
            "The link text says \"validation notes\" but points elsewhere."
        );
        assert!(!f.security);
    }

    #[test]
    fn skips_collapsed_sections_before_the_title() {
        let body = format!(
            "**{PIC} Major** · **Data Integrity & Integration** · **Heavy lift**\n\n<details>\n<summary>Supported by static analysis</summary>\n<details>\n\n**Reserve existing names before assigning suffixes.**\n\nFor saved names."
        );
        let f = parse_finding(&body);
        assert_eq!(f.severity, Severity::Major);
        assert_eq!(f.effort, "Heavy lift");
        assert_eq!(f.title, "Reserve existing names before assigning suffixes.");
    }

    #[test]
    fn spots_security_findings_and_ignores_the_tier_badge() {
        let body = format!(
            "**{PIC} Major** · **Security & Privacy** · **<picture><img src=\"x\" alt=\"\"></picture> Detected with Advanced Tier**\n\n**Validate the archive path.**"
        );
        let f = parse_finding(&body);
        assert!(f.security && f.severity.is_flag());
        assert_eq!(f.category, "Security & Privacy");
        assert_eq!(f.effort, "");
    }

    #[test]
    fn reads_the_older_pipe_format() {
        let f = parse_finding(
            "🎯 **Functional Correctness** | 🟡 **Minor** | ⚡ **Quick win**\n\n**Guard against empty tabs.**",
        );
        assert_eq!(
            (f.severity, f.category.as_str(), f.effort.as_str()),
            (Severity::Minor, "Functional Correctness", "Quick win")
        );
        assert_eq!(f.title, "Guard against empty tabs.");
        let f = parse_finding("_🧹 Nitpick_ | _🔵 Trivial_\n\n**Use a const.**");
        assert_eq!(f.severity, Severity::Trivial);
        assert!(!f.severity.is_flag());
    }

    #[test]
    fn a_title_can_mark_an_uncategorized_finding_as_security() {
        assert!(parse_finding("🔴 **Critical**\n\n**SQL injection in the search box.**").security);
    }

    #[test]
    fn a_comment_without_a_header_is_a_minor_finding() {
        let f = parse_finding("<!-- x -->\nThis looks off, please check.\nThanks");
        assert_eq!(f.severity, Severity::Minor);
        assert_eq!(f.title, "This looks off, please check.");
        assert_eq!(parse_finding("").title, "");
    }

    #[test]
    fn counts_actionable_comments() {
        assert_eq!(
            actionable_count("<!-- x -->\n\n**Actionable comments posted: 12**\n"),
            Some(12)
        );
        assert_eq!(actionable_count("Looks good"), None);
    }

    #[test]
    fn recognizes_the_bot() {
        assert!(is_coderabbit("coderabbitai") && is_coderabbit("CodeRabbitAI[bot]"));
        assert!(!is_coderabbit("rabbit"));
    }

    #[test]
    fn truncates_on_characters() {
        assert_eq!(truncate("héllo", 10), "héllo");
        assert_eq!(truncate("héllo", 3), "hé…");
    }
}
