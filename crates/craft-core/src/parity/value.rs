//! Readers for the values written in parity documents: percentages,
//! fractions, "ahead" scores, hour ranges, measurement bases, dates and
//! gap kinds.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};

use super::md::{plain, spans};
use super::{Basis, Fraction, Hours, Pct, Score};

static PERCENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<approx>[~≈]\s?)?(?P<n>\d{1,3}(?:\.\d+)?)[\s\u{00A0}\u{202F}]?%").unwrap()
});
static PCT_RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+(?:\.\d+)?)\s?[–-]\s?(\d+(?:\.\d+)?)\s?%").unwrap());
static FRACTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d[\d,]*)\s*(?:/|\bof\b)\s*(\d[\d,]*)").unwrap());
static DATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap());
static ISSUE_RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"#\d+\s?[–—-]\s?#?\d+").unwrap());
static HOURS_RANGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<approx>[~≈]\s?)?(?P<lo>\d{1,3}(?:,\d{3})+|\d+(?:\.\d+)?)\s?[–—-]\s?(?P<hi>\d{1,3}(?:,\d{3})+|\d+(?:\.\d+)?)").unwrap()
});
static HOURS_ONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<approx>[~≈]\s?)?(?P<lo>\d{1,3}(?:,\d{3})+|\d+(?:\.\d+)?)").unwrap()
});
static WITH_AGENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"with \d+(?:[–-]\d+)? agents").unwrap());
static LEAD_UNIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:h|hours?|agent-h)\b\.?").unwrap());
static COUNTED_IN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(in (#?[A-Za-z]?-?\d+)\)").unwrap());

/// Whether the match starting at byte `start` of `s` begins a number, not
/// the tail of a longer one.
fn starts_number(s: &str, start: usize) -> bool {
    !s[..start]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_digit() || c == '.' || c == ',')
}

/// The parenthesis depth at byte `at` of `s`.
fn depth(s: &str, at: usize) -> i64 {
    s[..at].matches('(').count() as i64 - s[..at].matches(')').count() as i64
}

/// One percentage found in a text.
#[derive(Debug, Clone, Copy)]
struct Found {
    /// Byte range of the whole match, and where the digits start.
    start: usize,
    num: usize,
    end: usize,
    n: f64,
    approx: bool,
}

fn percents(s: &str) -> Vec<Found> {
    PERCENT
        .captures_iter(s)
        .filter_map(|c| {
            let n = c.name("n")?;
            if !starts_number(s, n.start()) {
                return None;
            }
            let all = c.get(0)?;
            Some(Found {
                start: all.start(),
                num: n.start(),
                end: all.end(),
                n: n.as_str().parse().ok()?,
                approx: c.name("approx").is_some(),
            })
        })
        .collect()
}

/// The percentage a cell gives, in this order of preference: the one after
/// `→` or `≈` (so `A% ≈ B%` reads B); the first in the first bold span; the
/// first outside parentheses; the first. With no percentage, a fraction
/// such as `487 of 502`, computed. The basis is left Unknown.
pub fn pct(raw: &str) -> Option<Pct> {
    let text = plain(raw);
    let all = percents(&text);
    let ranges: Vec<(f64, f64)> = PCT_RANGE
        .captures_iter(&text)
        .filter_map(|c| {
            if !starts_number(&text, c.get(1)?.start()) {
                return None;
            }
            Some((c[1].parse().ok()?, c[2].parse().ok()?))
        })
        .collect();
    // `within` is the string `f`'s offsets index: the cell, or a bold span.
    let make = |f: &Found, within: &str| {
        let mut note = None;
        if within.get(f.end..).is_some_and(|s| s.starts_with('+')) {
            note = Some("or more".to_string());
        }
        // The first range that holds the value.
        let range = ranges.iter().find(|r| r.0 <= f.n && f.n <= r.1);
        Pct {
            value: f.n,
            approx: f.approx,
            low: range.map(|r| r.0),
            high: range.map(|r| r.1),
            basis: Basis::Unknown,
            raw: text.clone(),
            note,
            computed: false,
        }
    };
    let arrow = all.windows(2).find(|w| {
        let between =
            text[w[0].end..w[1].num].trim_matches(|c: char| c.is_whitespace() || c == '~');
        between == "→" || between == "≈"
    });
    if let Some(w) = arrow {
        return Some(make(&w[1], &text));
    }
    let bold = spans(raw, "**");
    if let Some((f, b)) = bold
        .first()
        .and_then(|b| percents(b).first().map(|f| (*f, b)))
    {
        return Some(make(&f, b));
    }
    if let Some(f) = all
        .iter()
        .find(|f| depth(&text, f.start) <= 0)
        .or(all.first())
    {
        return Some(make(f, &text));
    }
    let f = fraction(&text)?;
    Some(Pct {
        value: (f.done / f.total * 1000.0).round() / 10.0,
        approx: false,
        low: None,
        high: None,
        basis: Basis::Unknown,
        raw: text,
        note: Some(format!("computed from {} of {}", f.done, f.total)),
        computed: true,
    })
}

/// A score: `ahead …` or `beyond …` at the start, else a percentage.
pub fn score(raw: &str) -> Option<Score> {
    let text = plain(raw);
    let lower = text.to_lowercase();
    if ["ahead", "beyond"].iter().any(|w| {
        lower.starts_with(w)
            && !lower[w.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric())
    }) {
        return Some(Score::Ahead { text });
    }
    pct(raw).map(Score::Pct)
}

/// The first `N / M` or `N of M` in `s`.
pub fn fraction(s: &str) -> Option<Fraction> {
    FRACTION.captures_iter(s).find_map(|c| {
        if !starts_number(s, c.get(1)?.start()) {
            return None;
        }
        let n = |i: usize| c[i].replace(',', "").parse::<f64>().ok();
        let (done, total) = (n(1)?, n(2)?);
        (total > 0.0).then_some(Fraction { done, total })
    })
}

/// The hours a cell gives, or why there are none (the cell's text). `label`
/// and `header` are the row label and column header, which can name the
/// basis.
pub fn hours(raw: &str, label: &str, header: &str) -> Result<Hours, String> {
    let text = plain(raw);
    let counted_in = counted_in(&text);
    // Dates, `#a–#b` ranges and `(in N)` are blanked, keeping the offsets.
    let blank = |s: &str, re: &Regex| {
        re.replace_all(s, |c: &Captures| " ".repeat(c[0].len()))
            .into_owned()
    };
    let clean = |s: &str| blank(&blank(&blank(s, &DATE), &ISSUE_RANGE), &COUNTED_IN);
    let scan = clean(&text);
    let bold: Vec<(String, String)> = spans(raw, "**")
        .into_iter()
        .map(|b| {
            let scanned = clean(&b);
            (b, scanned)
        })
        .collect();
    let read = |re: &Regex, s: &str| -> Vec<(usize, usize, u32, u32, bool)> {
        let ranges: Vec<(usize, usize)> = HOURS_RANGE
            .find_iter(s)
            .map(|m| (m.start(), m.end()))
            .collect();
        re.captures_iter(s)
            .filter_map(|c| {
                let lo = c.name("lo")?;
                let start = c.get(0)?.start();
                let glued = s[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|ch| ch.is_alphabetic() || ch == '#');
                if glued || !starts_number(s, lo.start()) {
                    return None;
                }
                let end = c.get(0)?.end();
                if s[end..].starts_with(|ch: char| ch.is_ascii_digit() || ch == '%' || ch == '.') {
                    return None;
                }
                let after = s[end..].trim_start();
                // A percentage, an agent count, or one end of a range.
                if after.starts_with('%') || after.starts_with("agents") {
                    return None;
                }
                if c.name("hi").is_none() && ranges.iter().any(|&(a, b)| a <= start && end <= b) {
                    return None;
                }
                // Hours are whole: a fraction widens the range to the hours
                // around it (`0.5–1.5` reads 0–2) and marks it approximate.
                let num = |m: &str| m.replace(',', "").parse::<f64>().ok();
                let lo_f = num(lo.as_str())?;
                let hi_f = c.name("hi").map_or(Some(lo_f), |h| num(h.as_str()))?;
                let fraction = lo_f.fract() != 0.0 || hi_f.fract() != 0.0;
                Some((
                    start,
                    end,
                    lo_f.floor() as u32,
                    hi_f.ceil() as u32,
                    c.name("approx").is_some() || fraction,
                ))
            })
            .collect()
    };
    // `(start, end, lo, hi, approx, the text around it)`.
    type Pick = (usize, usize, u32, u32, bool, String);
    let in_bold = |re: &Regex| -> Option<Pick> {
        let (b, m) = bold
            .iter()
            .find_map(|(b, scanned)| read(re, scanned).first().map(|m| (b, *m)))?;
        let at = text.find(b.as_str())?;
        Some((at + m.0, at + m.1, m.2, m.3, m.4, b.clone()))
    };
    let before = text[..text.find('(').unwrap_or(text.len())].to_string();
    let outside = |re: &Regex| -> Option<Pick> {
        let m = read(re, &scan)
            .into_iter()
            .find(|m| depth(&scan, m.0) <= 0)?;
        Some((m.0, m.1, m.2, m.3, m.4, before.clone()))
    };
    let first = |re: &Regex| -> Option<Pick> {
        let m = read(re, &scan).into_iter().next()?;
        Some((m.0, m.1, m.2, m.3, m.4, before.clone()))
    };
    // A match in a bold span, then outside parentheses, then the first; at
    // each step a range before a single number.
    let pick = [
        &in_bold as &dyn Fn(&Regex) -> Option<Pick>,
        &outside,
        &first,
    ]
    .iter()
    .find_map(|f| f(&HOURS_RANGE).or_else(|| f(&HOURS_ONE)));
    if let Some((start, end, lo, hi, approx, around)) = pick {
        let note = format!("{} {}", text[..start].trim(), text[end..].trim());
        let note = LEAD_UNIT.replace(note.trim(), "").trim().to_string();
        let basis = hours_basis(&format!("{around} {label} {header}"));
        return Ok(Hours {
            lo,
            hi,
            approx,
            basis,
            note: (!note.is_empty()).then_some(note),
            raw: text.clone(),
            counted_in,
        });
    }
    Err(if text.is_empty() {
        "empty".to_string()
    } else {
        text
    })
}

/// `N` of an `(in N)` in `text`: the hours are counted inside gap or row N.
pub fn counted_in(text: &str) -> Option<String> {
    COUNTED_IN.captures(text).map(|c| c[1].to_string())
}

/// The basis an hours figure states: one agent, wall-clock, or with N agents.
fn hours_basis(s: &str) -> Option<String> {
    let l = s.to_lowercase();
    if [
        "one agent",
        "agent-hours",
        "agent hours",
        "agent-h",
        "opus 5.5 h",
    ]
    .iter()
    .any(|w| l.contains(w))
    {
        return Some("one agent".into());
    }
    if l.contains("wall-clock") {
        return Some("wall-clock".into());
    }
    WITH_AGENTS.find(&l).map(|m| m.as_str().to_string())
}

/// What a headline number rests on, from its Kind or How cell (`kind`) and
/// its Value cell (`value`).
pub fn basis(kind: &str, value: &str) -> Basis {
    let k = kind.to_lowercase();
    if ["measured in part", "partly measured", "measured bounds"]
        .iter()
        .any(|p| k.starts_with(p))
    {
        return Basis::Partly;
    }
    if k.starts_with("measured") {
        return Basis::Measured;
    }
    if k.starts_with("estimat") {
        return Basis::Estimated;
    }
    match (k.contains("measur"), k.contains("estimat")) {
        (true, true) => Basis::Partly,
        (true, false) => Basis::Measured,
        (false, true) => Basis::Estimated,
        (false, false) if value.to_lowercase().contains("(measured)") => Basis::Measured,
        _ => Basis::Unknown,
    }
}

/// Every `YYYY-MM-DD` in `s`.
pub fn dates(s: &str) -> Vec<String> {
    DATE.find_iter(s).map(|m| m.as_str().to_string()).collect()
}

/// The canonical gap kinds.
pub const KINDS: &[&str] = &[
    "feature",
    "ui/ux",
    "file format",
    "codec",
    "typography",
    "layout",
    "hardware",
    "localization",
    "performance",
    "stability",
    "platforms",
    "ecosystem",
    "ai",
    "collaboration",
    "standards",
    "measurement",
    "fidelity",
];

/// The canonical kind `word` names, if any.
pub fn kind(word: &str) -> Option<&'static str> {
    let w = word
        .trim()
        .trim_matches(|c: char| c == '.' || c == ':' || c == '*')
        .trim()
        .to_lowercase();
    let w = w.split_whitespace().collect::<Vec<_>>().join(" ");
    // "File-format gaps" names the file format kind.
    let w = w
        .strip_suffix(" gaps")
        .or_else(|| w.strip_suffix(" gap"))
        .unwrap_or(&w)
        .replace('-', " ");
    let w = match w.as_str() {
        "ui / ux" | "ui/ux" | "ui" | "ui / ux fidelity" => "ui/ux",
        _ => w.as_str(),
    };
    let alias = |s: &str| -> Option<&'static str> {
        Some(match s {
            "features" | "feature" | "painting" => "feature",
            "formats" | "file formats" | "file format" | "format" => "file format",
            "platform" | "platforms" => "platforms",
            "spec" | "specification" | "standards" | "standard" => "standards",
            "quality" => "fidelity",
            "midi" => "hardware",
            "plugins" | "plug-ins" | "plugin" | "plug-in" => "ecosystem",
            _ => return KINDS.iter().find(|k| **k == s).copied(),
        })
    };
    alias(w).or_else(|| w.strip_suffix('s').and_then(alias))
}

/// Single-letter (or short) kind codes and the kinds they stand for.
pub type Legend = BTreeMap<String, String>;

/// The legend in `text`, a paragraph of `**X** word` items such as
/// `**F** feature · **U** UI/UX`.
pub fn legend(text: &str) -> Legend {
    static ITEM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\*\*([A-Z]{1,3})\*\*\s+([^·*]+)").unwrap());
    ITEM.captures_iter(text)
        .map(|c| {
            let word = c[2].trim().trim_end_matches(['.', ',', ';']).trim();
            let k = kind(word).map_or_else(|| word.to_lowercase(), str::to_string);
            (c[1].to_string(), k)
        })
        .collect()
}

/// The kinds a Kind cell names: the whole cell when it is one kind, else
/// its parts split on `,` and ` and` (and on `/` when both sides are kinds);
/// codes go through `legend`. An unknown part is kept as written.
pub fn kinds(cell: &str, legend: &Legend) -> Vec<String> {
    let text = plain(cell);
    let text = text.trim();
    if text.is_empty() || text == "—" {
        return Vec::new();
    }
    if let Some(k) = kind(text) {
        return vec![k.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    for part in text.split(',').flat_map(|p| p.split(" and ")) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let pieces: Vec<&str> = part.split('/').map(str::trim).collect();
        let parts: Vec<&str> = if pieces.len() > 1
            && pieces
                .iter()
                .all(|p| kind(p).is_some() || legend.contains_key(*p))
        {
            pieces
        } else {
            vec![part]
        };
        for p in parts {
            let k = legend
                .get(p)
                .cloned()
                .or_else(|| kind(p).map(str::to_string))
                .unwrap_or_else(|| p.to_string());
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Option<f64> {
        pct(s).map(|p| p.value)
    }

    #[test]
    fn percentages_follow_the_preference_order() {
        assert_eq!(v("**≈ 45%**"), Some(45.0));
        assert_eq!(v("~40 %"), Some(40.0));
        assert_eq!(v("40\u{a0}%"), Some(40.0));
        assert_eq!(v("47.2% → ~47%"), Some(47.0));
        let p = pct("**60.7% ≈ 61%**").unwrap();
        assert_eq!((p.value, p.raw.as_str()), (61.0, "60.7% ≈ 61%"));
        assert_eq!(v("56.9% unweighted (**67%** tier-weighted)"), Some(67.0));
        assert_eq!(v("(P0 94.2%) 67% overall"), Some(67.0));
        assert_eq!(v("(P0 94.2%)"), Some(94.2));
        assert!(pct("~40%").unwrap().approx);
        let r = pct("**~62%** (57–67%)").unwrap();
        assert_eq!((r.low, r.high), (Some(57.0), Some(67.0)));
        assert_eq!(pct("**~45%** (P0 98%)").and_then(|r| r.low), None);
        // A range goes with the value only when it holds the value.
        assert_eq!(pct("**40%** (was 30–35%)").and_then(|r| r.low), None);
        assert_eq!(pct("~62% (alpha 40–50%)").and_then(|r| r.high), None);
        let r = pct("**~62%** (was 40–50%, now 57–67%)").unwrap();
        assert_eq!((r.low, r.high), (Some(57.0), Some(67.0)));
    }

    #[test]
    fn more_than_and_over_a_hundred() {
        let p = pct("100%+").unwrap();
        assert_eq!((p.value, p.note.as_deref()), (100.0, Some("or more")));
        // Offsets of a bold span stay inside that span, after multibyte text.
        assert_eq!(pct("— — **9%**").map(|p| p.value), Some(9.0));
        assert_eq!(
            pct("≈ **100%+**").and_then(|p| p.note),
            Some("or more".into())
        );
        let p = pct("258/256").unwrap();
        assert_eq!(p.value, 100.8);
        assert!(p.computed);
    }

    #[test]
    fn a_fraction_alone_is_computed() {
        let p = pct("487 of 502 menu items").unwrap();
        assert_eq!(p.value, 97.0);
        assert!(p.computed);
        assert_eq!(p.note.as_deref(), Some("computed from 487 of 502"));
        assert_eq!(v("**402 / 434 (92.6%)**"), Some(92.6));
        assert_eq!(v("1,245 of 2,490"), Some(50.0));
        assert!(pct("soon").is_none());
    }

    #[test]
    fn ahead_scores() {
        assert!(matches!(
            score("beyond Illustrator"),
            Some(Score::Ahead { .. })
        ));
        assert!(matches!(
            score("**Ahead** of it"),
            Some(Score::Ahead { .. })
        ));
        assert!(matches!(
            score("95% (beyond Word on Linux)"),
            Some(Score::Pct(_))
        ));
        assert!(score("aheadish").is_none());
    }

    #[test]
    fn hours_keep_the_note_and_the_basis() {
        let h = hours("~170–270 h (~50–80 h with 4–6 agents)", "", "One agent (h)").unwrap();
        assert_eq!((h.lo, h.hi, h.approx), (170, 270, true));
        assert_eq!(h.basis.as_deref(), Some("one agent"));
        assert_eq!(h.note.as_deref(), Some("(~50–80 h with 4–6 agents)"));
        let h = hours("**Beta needs #1–#7** (~150–230 h", "", "").unwrap();
        assert_eq!((h.lo, h.hi), (150, 230));
        let h = hours("**~340–560 h**", "", "Hours").unwrap();
        assert_eq!((h.lo, h.hi, h.basis), (340, 560, None));
        let h = hours("6–10 + human", "", "").unwrap();
        assert_eq!((h.lo, h.hi, h.note.as_deref()), (6, 10, Some("+ human")));
        // A bold or unbracketed single figure comes before a range in brackets,
        // and an agent count is not hours.
        let h = hours("**100 h** (30–50 h with 4 agents)", "", "").unwrap();
        assert_eq!((h.lo, h.hi), (100, 100));
        let h = hours("100 h (with 4–6 agents)", "", "").unwrap();
        assert_eq!((h.lo, h.hi), (100, 100));
        let h = hours("(with 4–6 agents)", "", "");
        assert_eq!(h.map(|h| (h.lo, h.hi)), Err("(with 4–6 agents)".into()));
        let h = hours("Phase 2: 30–50 h", "", "").unwrap();
        assert_eq!((h.lo, h.hi), (30, 50));
        // Fractions widen to whole hours and read as approximate.
        let h = hours("0.5–1.5 h", "", "").unwrap();
        assert_eq!((h.lo, h.hi, h.approx), (0, 2, true));
        let h = hours("**2.7 h**", "", "").unwrap();
        assert_eq!((h.lo, h.hi, h.approx), (2, 3, true));
        assert_eq!(
            hours("1,200–2,000", "", "").map(|h| (h.lo, h.hi)),
            Ok((1200, 2000))
        );
        assert_eq!(hours("0", "", "").map(|h| (h.lo, h.hi)), Ok((0, 0)));
        assert_eq!(
            hours("~30–50 h wall-clock", "", "")
                .unwrap()
                .basis
                .as_deref(),
            Some("wall-clock")
        );
        assert_eq!(
            hours("40–60 (in 2)", "", "").unwrap().counted_in.as_deref(),
            Some("2")
        );
        assert_eq!(hours("(in 2)", "", "").unwrap_err(), "(in 2)");
        assert_eq!(counted_in("(in 2)").as_deref(), Some("2"));
    }

    #[test]
    fn dates_are_not_hours() {
        assert_eq!(hours("2026-10-17", "", "").unwrap_err(), "2026-10-17");
        assert_eq!(
            hours("see typography-parity.md", "", "").unwrap_err(),
            "see typography-parity.md"
        );
        assert_eq!(hours("see G4", "", "").unwrap_err(), "see G4");
        assert_eq!(hours("—", "", "").unwrap_err(), "—");
        assert_eq!(
            hours("checked 2026-10-17: 12–20", "", "").map(|h| (h.lo, h.hi)),
            Ok((12, 20))
        );
    }

    #[test]
    fn basis_reads_the_kind_cell() {
        assert_eq!(basis("measured in part: menus", ""), Basis::Partly);
        assert_eq!(
            basis("Measured bounds, estimate between", ""),
            Basis::Partly
        );
        assert_eq!(basis("measured: checklist", ""), Basis::Measured);
        assert_eq!(basis("Estimated depth", ""), Basis::Estimated);
        assert_eq!(
            basis("Menus 92% (measured), other areas estimated", ""),
            Basis::Partly
        );
        assert_eq!(
            basis("How close it is; estimated depth per area", ""),
            Basis::Estimated
        );
        assert_eq!(basis("", "**50%** (measured)"), Basis::Measured);
        assert_eq!(basis("weighted sum", "50%"), Basis::Unknown);
    }

    #[test]
    fn kinds_keep_ui_ux_together() {
        let none = Legend::new();
        assert_eq!(kinds("UI / UX", &none), ["ui/ux"]);
        assert_eq!(kinds("Hardware / MIDI", &none), ["hardware"]);
        assert_eq!(kinds("Features", &none), ["feature"]);
        assert_eq!(kinds("feature, spec", &none), ["feature", "standards"]);
        assert_eq!(
            kinds("Stability and platforms", &none),
            ["stability", "platforms"]
        );
        assert_eq!(kinds("Quality / fidelity", &none), ["fidelity"]);
        assert_eq!(kinds("tea", &none), ["tea"]);
        assert_eq!(kinds("File-format gaps", &none), ["file format"]);
        assert_eq!(
            kinds("Stability, performance gaps", &none),
            ["stability", "performance"]
        );
        assert_eq!(kinds("UI/UX gaps", &none), ["ui/ux"]);
    }

    #[test]
    fn a_wrapped_legend_is_read_in_full() {
        let l = legend("Kind: **F** feature · **U** UI/UX · **FF** file format · **H** hardware · **L** localization · **P** performance · **S** stability.");
        assert_eq!(l.get("S").map(String::as_str), Some("stability"));
        assert_eq!(l.get("FF").map(String::as_str), Some("file format"));
        assert_eq!(l.get("U").map(String::as_str), Some("ui/ux"));
        assert_eq!(kinds("F, FF", &l), ["feature", "file format"]);
        assert_eq!(kinds("P, S", &l), ["performance", "stability"]);
    }
}
