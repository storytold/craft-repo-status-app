//! Each merged pull request's type (feature, fix, docs, tests, ci,
//! packaging, dependency, i18n, refactor, perf or release) and area, read
//! from its title, branch, squash headline, commits, closing issues and
//! changed files. Every answer names the rule that gave it.
//!
//! Feature versus fix was right for about 95% of pull requests the rules
//! were not tuned on. The other types, and a type a fallback rule (F1 to
//! F3) decided, were never checked on such pull requests, so they are
//! marked as guesses.

use std::sync::LazyLock;

use regex::Regex;

use super::record::{Decision, FileLines};

/// The types, in the order the rankings list them.
pub const TYPES: &[&str] = &[
    "feature",
    "fix",
    "docs",
    "tests",
    "ci",
    "packaging",
    "dependency",
    "i18n",
    "refactor",
    "perf",
    "release",
];

/// What a changed file is, by its path; [`path_class`] checks them in this
/// order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathClass {
    /// `ROADMAP.md`, `ASSETS.md`, `contributors/people.toml` and the parity
    /// documents; left out when the type is read from paths.
    Bookkeeping,
    /// `contributors/contributors.json`.
    Generated,
    /// Translation catalogs: under an `i18n`, `l10n` or `locales` directory,
    /// or a `.tsv`, `.po` or `.ftl` file.
    I18n,
    Ci,
    Packaging,
    Dependency,
    Tests,
    Docs,
    Code,
}

fn re(s: &str) -> Regex {
    Regex::new(s).unwrap()
}

static BOOKKEEPING: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^(ROADMAP\.md|ASSETS\.md|contributors/people\.toml|docs/(roadmap|target-app-parity|[\w-]*-parity|gaps)\.md)$",
    )
});
static GENERATED: LazyLock<Regex> = LazyLock::new(|| re(r"^contributors/contributors\.json$"));
static I18N_PATH: LazyLock<Regex> =
    LazyLock::new(|| re(r"(^|/)(i18n|l10n|locales?)(/|$)|\.(tsv|po|ftl)$"));
static CI_PATH: LazyLock<Regex> = LazyLock::new(|| re(r"^\.github/"));
static PACK_PATH: LazyLock<Regex> =
    LazyLock::new(|| re(r"^packaging/|\.wxs$|nfpm\.ya?ml$|\.plist(\.in)?$|^flake\.(nix|lock)$"));
static DEP_PATH: LazyLock<Regex> = LazyLock::new(|| re(r"(^|/)Cargo\.(toml|lock)$"));
static TEST_PATH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(^|/)tests?(/|\.rs$)|(^|/)tests_\w+\.rs$|_tests?\.rs$|(^|/)test_\w+\.rs$|(^|/)fixtures?/")
});
static DOC_PATH: LazyLock<Regex> = LazyLock::new(|| re(r"\.md$|(^|/)docs?/|(^|/)README"));

static REFS: LazyLock<Regex> = LazyLock::new(|| re(r"\s*\((#\d+([,\s]+(x\s+)?#?\d+)*)\)\s*"));
static CC: LazyLock<Regex> = LazyLock::new(|| re(r"^([A-Za-z]+)(\(([^)]+)\))?!?:\s+(.*)$"));
static FIX_N: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^Fix #\d+:\s+(.*)$"));
static PART_OF: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^Part of #\d+:\s+(.*)$"));
static AREA: LazyLock<Regex> = LazyLock::new(|| re(r"^([^:]{1,40}?):\s+(.*)$"));
static MERGE_HEAD: LazyLock<Regex> =
    LazyLock::new(|| re(r"^Merge (pull request|PR|branch|remote|#)"));
static RELEASE_TITLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^release( prep)?\b[^:]*?(:\s*)?.*?\bv?\d+\.\d+\.\d+"));
static I18N_TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(i18n|l10n)\b"));
static BUMP: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(bump|upgrade|update|require)\b"));
static REFACTOR_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b(refactor\w*|clean ?up|clippy|rustfmt|dead code|rename\w*|simplif\w+)\b")
});
static PERF_WORDS: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(\bperf\b|\bfaster\b|\bspeed ?up\b|\d+\s*[x×]\s*faster)"));
static FIX_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(fix(es|ed)?|no longer|instead of|crash(es)?|panics?|deadlocks?|hangs?|regression|dangling|leaks?|wrong|broken|incorrect|refuses?|avoid|prevent|preserve[sd]?|again|stale|missing|lost|losing|corrupt\w*)\b",
    )
});
static COMMIT_FIX: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^fix(\(|:|\b)"));
static BRANCH_PREFIX: LazyLock<Regex> = LazyLock::new(|| re(r"^(codex|claude|copilot|pr)/"));
static BRANCH_TYPES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"^(fix|bugfix|hotfix)[/-]", "fix"),
        (r"^(feat|feature)[/-]", "feature"),
        (r"^perf[/-]", "perf"),
        (r"^ci[/-]", "ci"),
        (r"^docs?[/-]", "docs"),
        (r"^tests?[/-]", "tests"),
        (r"^refactor[/-]", "refactor"),
        (r"^(i18n|l10n)[/-]", "i18n"),
        (r"^deps[/-]", "dependency"),
    ]
    .into_iter()
    .map(|(p, t)| (re(p), t))
    .collect()
});
/// A title that starts with a robustness verb (F1). Check counts only when
/// the title goes on to name a limit, memory, size or bounds, so "Check
/// spelling as you type" stays a feature.
static ROBUST: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^(bound|cap|limit|clamp|validate|guard|harden|reject|refuse)\b|^check\b.*\b(limit|memory|size|bounds)\b",
    )
});
static ISSUE_FEATURE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(doesn'?t exist|does not exist|missing feature|add support|support for|feature request)\b",
    )
});
static ISSUE_BUG: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(not (\w+ )?(work\w*|properly|correct\w*)|doesn'?t work|misses|broken|crash\w*|silently|stale|dangling|wrong|fails?|error|incorrect\w*|disabled|ignored?|lost|panic\w*|hang\w*)\b",
    )
});
static SPACES: LazyLock<Regex> = LazyLock::new(|| re(r"\s+"));

/// Conventional Commits types, and the type each gives; `chore` gives none.
const CC_TYPES: &[(&str, Option<&str>)] = &[
    ("feat", Some("feature")),
    ("feature", Some("feature")),
    ("fix", Some("fix")),
    ("bugfix", Some("fix")),
    ("hotfix", Some("fix")),
    ("docs", Some("docs")),
    ("doc", Some("docs")),
    ("test", Some("tests")),
    ("tests", Some("tests")),
    ("ci", Some("ci")),
    ("build", Some("packaging")),
    ("packaging", Some("packaging")),
    ("refactor", Some("refactor")),
    ("style", Some("refactor")),
    ("perf", Some("perf")),
    ("deps", Some("dependency")),
    ("release", Some("release")),
    ("i18n", Some("i18n")),
    ("l10n", Some("i18n")),
    ("revert", Some("fix")),
    ("chore", None),
];

/// `Area:` prefixes that name a kind of work rather than a product area,
/// and the type each gives; `contributors` gives none.
const AREA_TYPES: &[(&str, Option<&str>)] = &[
    ("docs", Some("docs")),
    ("doc", Some("docs")),
    ("readme", Some("docs")),
    ("documentation", Some("docs")),
    ("tests", Some("tests")),
    ("test", Some("tests")),
    ("regression test", Some("tests")),
    ("ci", Some("ci")),
    ("packaging", Some("packaging")),
    ("dependencies", Some("dependency")),
    ("deps", Some("dependency")),
    ("i18n", Some("i18n")),
    ("l10n", Some("i18n")),
    ("translations", Some("i18n")),
    ("translation", Some("i18n")),
    ("fmt", Some("refactor")),
    ("clippy", Some("refactor")),
    ("refactor", Some("refactor")),
    ("lints", Some("refactor")),
    ("contributors", None),
];

/// Spellings of one area, as `(spelling, area)`, after [`norm_area`].
const AREA_ALIASES: &[(&str, &str)] = &[("image trace", "trace")];

/// What a path is (see [`PathClass`]).
pub fn path_class(path: &str) -> PathClass {
    if BOOKKEEPING.is_match(path) {
        PathClass::Bookkeeping
    } else if GENERATED.is_match(path) {
        PathClass::Generated
    } else if I18N_PATH.is_match(path) {
        PathClass::I18n
    } else if CI_PATH.is_match(path) {
        PathClass::Ci
    } else if PACK_PATH.is_match(path) {
        PathClass::Packaging
    } else if DEP_PATH.is_match(path) {
        PathClass::Dependency
    } else if TEST_PATH.is_match(path) {
        PathClass::Tests
    } else if DOC_PATH.is_match(path) {
        PathClass::Docs
    } else {
        PathClass::Code
    }
}

/// Whether a file's lines count as product lines: everything but
/// `Cargo.lock`, translation catalogs and generated files, whose lines the
/// rankings show in a column of their own.
pub fn product_lines(path: &str) -> bool {
    let lock = path == "Cargo.lock" || path.ends_with("/Cargo.lock");
    !lock && !matches!(path_class(path), PathClass::Generated | PathClass::I18n)
}

/// How a title was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// A Conventional Commits `type(scope):` prefix.
    Cc,
    /// The title has no prefix; the squash headline has a Conventional
    /// Commits prefix.
    CcMerge,
    /// `Fix #N:`.
    FixN,
    /// An `Area:` prefix of four words or fewer.
    Area,
    None,
}

struct Parsed {
    ty: Option<&'static str>,
    area: Option<String>,
    rest: String,
    form: Form,
}

/// The title without its `(#N)` references.
fn strip_refs(t: &str) -> String {
    REFS.replace_all(t, " ").trim().to_string()
}

fn parse_title(title: &str) -> Parsed {
    let t = strip_refs(title);
    if let Some(m) = CC.captures(&t) {
        let word = m[1].to_ascii_lowercase();
        if let Some((_, ty)) = CC_TYPES.iter().find(|(w, _)| *w == word) {
            return Parsed {
                ty: *ty,
                area: m.get(3).map(|s| s.as_str().to_string()),
                rest: m[4].to_string(),
                form: Form::Cc,
            };
        }
    }
    if let Some(m) = FIX_N.captures(&t) {
        return Parsed {
            ty: Some("fix"),
            area: None,
            rest: m[1].to_string(),
            form: Form::FixN,
        };
    }
    let t = match PART_OF.captures(&t) {
        Some(m) => m[1].to_string(),
        None => t,
    };
    if let Some(m) = AREA.captures(&t) {
        let prefix = &m[1];
        if prefix.split_whitespace().count() <= 4 && !prefix.contains(['#', '(']) {
            return Parsed {
                ty: None,
                area: Some(prefix.trim().to_string()),
                rest: m[2].to_string(),
                form: Form::Area,
            };
        }
    }
    Parsed {
        ty: None,
        area: None,
        rest: t,
        form: Form::None,
    }
}

/// An area as the rankings group it: lowercase, without a leading dot,
/// spaces collapsed, and [`AREA_ALIASES`] applied.
pub fn norm_area(a: &str) -> String {
    let n = SPACES
        .replace_all(a.trim().trim_start_matches('.'), " ")
        .to_lowercase();
    AREA_ALIASES
        .iter()
        .find(|(from, _)| *from == n)
        .map_or(n, |(_, to)| to.to_string())
}

/// The type an `Area:` prefix names, if it names a kind of work: the
/// outer `Option` is whether it does.
fn area_type(area: &str) -> Option<Option<&'static str>> {
    let a = area.to_lowercase();
    AREA_TYPES.iter().find(|(w, _)| *w == a).map(|(_, t)| *t)
}

fn ends_in_tests(area: &str) -> bool {
    matches!(
        area.split_whitespace()
            .last()
            .map(str::to_lowercase)
            .as_deref(),
        Some("tests" | "test")
    )
}

/// Everything [`classify`] reads from a pull request.
#[derive(Debug, Clone, Default)]
pub struct Input<'a> {
    pub title: &'a str,
    pub head_ref: &'a str,
    /// The merge commit's headline: a squash merge's title.
    pub merge_headline: &'a str,
    /// The headlines of its commits.
    pub commit_headlines: Vec<&'a str>,
    /// The titles of the issues it closes.
    pub issue_titles: &'a [String],
    pub files: &'a [FileLines],
}

/// A pull request's type and area, each with the rule that decided it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Kinded {
    pub kind: Decision,
    /// The type was decided by a fallback rule, or is one that was never
    /// checked on pull requests the rules were not tuned on.
    pub guess: bool,
    pub area: Decision,
}

fn decision(value: &str, rule: impl Into<String>) -> Decision {
    Decision {
        value: value.to_string(),
        rule: rule.into(),
    }
}

/// The type and area of a merged pull request (see the module docs). The
/// rules, first match wins: R1 a release title; R2 an explicit type in the
/// title; R3 every file in one class; R4 refactor or perf words; R5 the
/// branch prefix, fix words in the title or a commit, otherwise a feature;
/// and when R5 ended at feature, the fallbacks F1 (a robustness verb), F2
/// (the closing issue reports something missing or broken) and F3 (a small
/// code change with at least as many test lines).
pub fn classify(input: &Input) -> Kinded {
    let mut p = parse_title(input.title);
    let merge = if MERGE_HEAD.is_match(input.merge_headline) {
        ""
    } else {
        input.merge_headline
    };
    if p.form == Form::None && !merge.is_empty() {
        let m = parse_title(merge);
        if m.form == Form::Cc {
            p.ty = m.ty;
            p.area = m.area;
            p.form = Form::CcMerge;
        }
    }
    let (kind, rule) = kind_of(input, &p);
    let area = if kind == "release" {
        decision("release", "a release")
    } else {
        area_of(&p, input.files)
    };
    let guess = !matches!(kind, "feature" | "fix") || rule.starts_with('F');
    Kinded {
        kind: decision(kind, rule),
        guess,
        area,
    }
}

fn kind_of(input: &Input, p: &Parsed) -> (&'static str, String) {
    let title = strip_refs(input.title);
    if RELEASE_TITLE.is_match(&title) || p.ty == Some("release") {
        return (
            "release",
            "R1: a release title with a version number".into(),
        );
    }
    let area = p.area.as_deref().unwrap_or_default();
    let explicit = if let Some(t) = p.ty {
        let how = match p.form {
            Form::FixN => "R2: the title starts with Fix #N".to_string(),
            Form::CcMerge => "R2: the squash headline's Conventional Commits type".to_string(),
            _ => "R2: the title's Conventional Commits type".to_string(),
        };
        Some((Some(t), how))
    } else if let (Form::Area, Some(t)) = (p.form, area_type(area)) {
        Some((t, format!("R2: the area {area:?} names a kind of work")))
    } else if p.form == Form::Area && ends_in_tests(area) {
        Some((
            Some("tests"),
            format!("R2: the area {area:?} ends in tests"),
        ))
    } else if I18N_TITLE.is_match(input.title) {
        Some((Some("i18n"), "R2: the title starts with i18n".to_string()))
    } else {
        None
    };
    if let Some((Some(t), how)) = explicit {
        return (t, how);
    }

    let mut by_class: Vec<(PathClass, u64)> = Vec::new();
    for f in input.files {
        let c = path_class(&f.path);
        let n = f.additions + f.deletions;
        match by_class.iter_mut().find(|(k, _)| *k == c) {
            Some((_, v)) => *v += n,
            None => by_class.push((c, n)),
        }
    }
    let lines = |c: PathClass| {
        by_class
            .iter()
            .find(|(k, _)| *k == c)
            .map_or(0, |(_, n)| *n)
    };
    let real: Vec<PathClass> = by_class
        .iter()
        .map(|(c, _)| *c)
        .filter(|c| !matches!(c, PathClass::Bookkeeping | PathClass::Generated))
        .collect();
    let real_lines: u64 = real.iter().map(|c| lines(*c)).sum();
    let only = |allowed: &[PathClass]| !real.is_empty() && real.iter().all(|c| allowed.contains(c));
    let path_rules: [(&[PathClass], &str, &str); 6] = [
        (&[PathClass::Ci], "ci", "R3: every file is under .github"),
        (
            &[PathClass::Ci, PathClass::Packaging],
            "packaging",
            "R3: every file is packaging or under .github",
        ),
        (
            &[PathClass::Dependency],
            "dependency",
            "R3: every file is Cargo.toml or Cargo.lock",
        ),
        (
            &[PathClass::I18n],
            "i18n",
            "R3: every file is a translation catalog",
        ),
        (
            &[PathClass::Docs],
            "docs",
            "R3: every file is documentation",
        ),
        (&[PathClass::Tests], "tests", "R3: every file is a test"),
    ];
    for (allowed, t, why) in path_rules {
        if only(allowed) {
            return (t, why.into());
        }
    }
    if real_lines > 0 && lines(PathClass::I18n) as f64 / real_lines as f64 >= 0.8 {
        return (
            "i18n",
            "R3: translation catalogs hold 80% or more of the lines".into(),
        );
    }
    if only(&[PathClass::Dependency, PathClass::Code])
        && BUMP.is_match(&p.rest)
        && lines(PathClass::Code) == 0
    {
        return (
            "dependency",
            "R3: a dependency bump with no code lines".into(),
        );
    }
    if REFACTOR_WORDS.is_match(&p.rest) {
        return (
            "refactor",
            "R4: the title says refactor, clean up, rename or simplify".into(),
        );
    }
    if PERF_WORDS.is_match(&p.rest) {
        return ("perf", "R4: the title says faster or perf".into());
    }

    let head = input.head_ref.to_lowercase();
    let head = BRANCH_PREFIX.replace(&head, "");
    if let Some((_, t)) = BRANCH_TYPES.iter().find(|(r, _)| r.is_match(&head)) {
        return (t, format!("R5: the branch {head:?}"));
    }
    if let Some(m) = FIX_WORDS.find(&p.rest) {
        return ("fix", format!("R5: the title says {:?}", m.as_str()));
    }
    if input
        .commit_headlines
        .iter()
        .any(|h| COMMIT_FIX.is_match(h))
    {
        return ("fix", "R5: a commit headline starts with fix".into());
    }

    if let Some(m) = ROBUST.find(&p.rest) {
        let verb = m.as_str().split_whitespace().next().unwrap_or_default();
        return ("fix", format!("F1: the title starts with {verb:?}"));
    }
    for t in input.issue_titles {
        if ISSUE_FEATURE.is_match(t) {
            return (
                "feature",
                "F2: the closing issue asks for something missing".into(),
            );
        }
        if ISSUE_BUG.is_match(t) {
            return (
                "fix",
                "F2: the closing issue reports something broken".into(),
            );
        }
    }
    let code = lines(PathClass::Code);
    let tests = lines(PathClass::Tests);
    if code <= 30 && tests >= code && tests > 0 {
        return (
            "fix",
            "F3: 30 or fewer code lines and at least as many test lines".into(),
        );
    }
    ("feature", "R5: no fix signal".into())
}

fn area_of(p: &Parsed, files: &[FileLines]) -> Decision {
    if let (Form::Cc | Form::CcMerge, Some(a)) = (p.form, &p.area) {
        let from = if p.form == Form::Cc {
            "the title's Conventional Commits scope"
        } else {
            "the squash headline's Conventional Commits scope"
        };
        return decision(&norm_area(a), from);
    }
    if let (Form::Area, Some(a)) = (p.form, &p.area) {
        let lower = a.to_lowercase();
        let rejected = starts_word(&lower, "fix") || starts_word(&lower, "release");
        if !rejected && area_type(a).is_none() && !ends_in_tests(a) {
            return decision(&norm_area(a), "the title's Area: prefix");
        }
    }
    path_area(files)
}

/// Whether `s` starts with the word `w`.
fn starts_word(s: &str, w: &str) -> bool {
    s.strip_prefix(w)
        .is_some_and(|r| !r.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}

/// The key a file's lines count under for the path area: the crate or app
/// under `crates/` or `apps/`, `ci` for `.github`, `docs` or `workspace`
/// for files at the root, otherwise the top directory.
fn area_key(path: &str) -> String {
    let segs: Vec<&str> = path.split('/').collect();
    match segs.as_slice() {
        ["crates" | "apps", name, ..] => name.to_string(),
        [".github", ..] => "ci".into(),
        [_] if DOC_PATH.is_match(path) => "docs".into(),
        [_] if DEP_PATH.is_match(path) => "workspace".into(),
        [_] => "(root)".into(),
        [top, ..] => top.to_string(),
        [] => String::new(),
    }
}

/// The area that holds the most lines (a file without lines counts one),
/// leaving out bookkeeping, generated files, `.tsv` catalogs and
/// `Cargo.lock`. When three or more areas share the lines and none holds
/// 40%, it reads "several (N): top".
fn path_area(files: &[FileLines]) -> Decision {
    let mut lines: Vec<(String, u64)> = Vec::new();
    for f in files {
        let c = path_class(&f.path);
        let skip = matches!(c, PathClass::Bookkeeping | PathClass::Generated)
            || (c == PathClass::I18n && f.path.ends_with(".tsv"))
            || f.path.ends_with("Cargo.lock");
        if skip {
            continue;
        }
        let key = area_key(&f.path);
        let n = (f.additions + f.deletions).max(1);
        match lines.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => *v += n,
            None => lines.push((key, n)),
        }
    }
    let Some((top, n)) = lines
        .iter()
        .fold(None::<&(String, u64)>, |best, x| match best {
            Some(b) if b.1 >= x.1 => Some(b),
            _ => Some(x),
        })
    else {
        return decision("(none)", "no product files");
    };
    let total: u64 = lines.iter().map(|(_, v)| v).sum();
    if lines.len() >= 3 && (*n as f64) / (total as f64) < 0.4 {
        let value = format!("several ({}): {}", lines.len(), norm_area(top));
        return decision(&value, "the files: no directory holds 40% of the lines");
    }
    decision(
        &norm_area(top),
        "the files: the directory with the most lines",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    fn files(list: &[(&str, u64, u64)]) -> Vec<FileLines> {
        list.iter()
            .map(|(path, additions, deletions)| FileLines {
                path: path.to_string(),
                additions: *additions,
                deletions: *deletions,
            })
            .collect()
    }

    fn kind(title: &str, f: &[FileLines]) -> Kinded {
        classify(&Input {
            title,
            files: f,
            ..Default::default()
        })
    }

    #[test]
    fn paths_fall_into_their_classes_and_catalogs_are_not_product_lines() {
        let cases = [
            ("ROADMAP.md", PathClass::Bookkeeping),
            ("docs/effects-parity.md", PathClass::Bookkeeping),
            ("contributors/contributors.json", PathClass::Generated),
            ("crates/ui-egui/src/i18n/ja.tsv", PathClass::I18n),
            ("crates/ui-egui/src/i18n/fr.rs", PathClass::I18n),
            ("po/de.po", PathClass::I18n),
            (".github/workflows/ci.yml", PathClass::Ci),
            ("packaging/macos/Info.plist.in", PathClass::Packaging),
            ("installer/app.wxs", PathClass::Packaging),
            ("Cargo.lock", PathClass::Dependency),
            ("crates/engine/Cargo.toml", PathClass::Dependency),
            ("crates/engine/tests/import.rs", PathClass::Tests),
            ("crates/engine/src/tests.rs", PathClass::Tests),
            ("crates/engine/src/layers_tests.rs", PathClass::Tests),
            ("docs/mcp.md", PathClass::Docs),
            ("README.md", PathClass::Docs),
            ("crates/engine/src/lib.rs", PathClass::Code),
        ];
        for (path, want) in cases {
            assert_eq!(path_class(path), want, "{path}");
        }
        for path in [
            "Cargo.lock",
            "crates/x/Cargo.lock",
            "crates/ui-egui/src/i18n/ja.tsv",
            "crates/ui-egui/src/i18n/fr.rs",
            "contributors/contributors.json",
        ] {
            assert!(!product_lines(path), "{path}");
        }
        for path in ["Cargo.toml", "ROADMAP.md", "crates/engine/src/lib.rs"] {
            assert!(product_lines(path), "{path}");
        }
    }

    #[test]
    fn check_is_a_robustness_verb_only_before_a_limit() {
        let code = files(&[("crates/engine/src/spell.rs", 300, 10)]);
        let k = kind("Check spelling as you type", &code);
        assert_eq!((k.kind.value.as_str(), k.guess), ("feature", false));
        let k = kind("Check peak memory before decoding", &code);
        assert_eq!(k.kind.value, "fix");
        assert!(k.kind.rule.starts_with("F1") && k.guess, "{k:?}");
    }

    #[test]
    fn areas_normalize_and_fall_back_to_the_files() {
        let code = files(&[("crates/ai/src/read.rs", 50, 5)]);
        assert_eq!(
            kind(".ai import: read layers", &code).area.value,
            "ai import"
        );
        assert_eq!(
            kind("AI import: read  layers", &code).area.value,
            "ai import"
        );
        assert_eq!(kind("Image Trace: centerlines", &code).area.value, "trace");
        // "Fix the build" has no prefix: the area comes from the files.
        let k = kind("Fix the build", &files(&[("crates/engine/src/a.rs", 3, 1)]));
        assert_eq!(
            (k.kind.value.as_str(), k.area.value.as_str()),
            ("fix", "engine")
        );
        assert!(k.area.rule.starts_with("the files"));
        // A prefix that starts with Fix or Release is not an area.
        let k = kind(
            "Fix layers: keep order",
            &files(&[("crates/engine/src/a.rs", 3, 1)]),
        );
        assert_eq!(k.area.value, "engine");
        // Three directories, none with 40% of the lines.
        let spread = files(&[
            ("crates/a/src/x.rs", 30, 0),
            ("crates/b/src/x.rs", 30, 0),
            ("crates/c/src/x.rs", 35, 0),
            ("Cargo.lock", 900, 0),
        ]);
        assert_eq!(kind("Tidy things", &spread).area.value, "several (3): c");
        // A release gets the area "release".
        let k = kind("Release: App v1.2.0", &code);
        assert_eq!(
            (k.kind.value.as_str(), k.area.value.as_str()),
            ("release", "release")
        );
        assert!(
            k.guess,
            "release was never checked on held-out pull requests"
        );
    }

    #[derive(Deserialize)]
    struct Golden {
        pr: String,
        title: String,
        head: String,
        merge: String,
        commits: Vec<String>,
        issues: Vec<String>,
        files: Vec<(String, u64, u64)>,
        #[serde(rename = "type")]
        kind: String,
        area: String,
        rule: String,
    }

    /// 106 merged pull requests from five apps: the spec's sample and
    /// held-out sets, with the type and area its measured classifier gave
    /// (the three pull requests the fallbacks were tuned on included).
    /// Files of one class and area are summed into one entry.
    #[test]
    fn the_golden_table_classifies_as_measured() {
        let rows: Vec<Golden> = serde_json::from_str(include_str!("kind_golden.json")).unwrap();
        assert_eq!(rows.len(), 106);
        let mut wrong = Vec::new();
        for g in &rows {
            let f: Vec<FileLines> = g
                .files
                .iter()
                .map(|(path, additions, deletions)| FileLines {
                    path: path.clone(),
                    additions: *additions,
                    deletions: *deletions,
                })
                .collect();
            let k = classify(&Input {
                title: &g.title,
                head_ref: &g.head,
                merge_headline: &g.merge,
                commit_headlines: g.commits.iter().map(String::as_str).collect(),
                issue_titles: &g.issues,
                files: &f,
            });
            let rule = k.kind.rule.split(':').next().unwrap_or_default();
            // The measured classifier had no area aliases.
            let area = norm_area(&g.area);
            if k.kind.value != g.kind || k.area.value != area || rule != g.rule {
                wrong.push(format!(
                    "{}: {} {} {} (want {} {} {})",
                    g.pr, k.kind.value, k.area.value, rule, g.kind, g.area, g.rule
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
        for (pr, want) in [
            ("wordcraft#1139", "fix"),
            ("photocraft#4597", "fix"),
            ("filmcraft#1547", "fix"),
        ] {
            let g = rows.iter().find(|g| g.pr == pr).unwrap();
            assert_eq!(g.kind, want, "{pr}");
            assert!(g.rule.starts_with('F'), "{pr} is decided by a fallback");
        }
    }
}
