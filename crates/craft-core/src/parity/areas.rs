//! Feature areas: the area table of the parity doc (or of `ROADMAP.md`),
//! with each area's weight, scores, hours and Missing chips taken from its
//! gaps text.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;

use super::dims::{doc_link, hours_cell, prose_total, score_cell, weight as weight_of};
use super::md::{self, contains_word, Doc, Table};
use super::{AltTotal, Area, AreaCounts, Cell, Chip, ChipsFrom, Note, Parity, Score};

/// Most chips a row shows; the rest are counted.
const MAX_CHIPS: usize = 12;

static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:open bugs|main gaps|not yet|missing|remaining|open|gaps|bugs|left)\s*:|\bopen\b[^:;.]{0,30}:|\bmissing\s+(?:[a-z])").unwrap()
});
static END_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:Done|Have|Present)\s*:").unwrap());
static COMMIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9a-f]{7,40}\b").unwrap());
static ISSUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#\d+").unwrap());
static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+(?:\.\d+)?").unwrap());
static INLINE_WEIGHT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[,:;(]\s*)([A-Za-z][A-Za-z /&'-]*?)\s+(\d+(?:\.\d+)?)\s?%").unwrap()
});
static WEIGHT_ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[-*]\s+(.+?)\s*[:—–-]?\s*\**(\d+(?:\.\d+)?)\s?%").unwrap());

const DONE_WORDS: &[&str] = &[
    "landed",
    "done",
    "shipped",
    "fixed",
    "implemented",
    "corrected",
    "closed",
    "resolved",
    "verified",
    "regression-tested",
    "included",
];
const DONE_PHRASES: &[&str] = &["is consistent", "are done", "is done"];
const NEGATIVE: &[&str] = &[
    "missing",
    "no",
    "not",
    "lacks",
    "lacking",
    "still",
    "open",
    "remain",
    "remains",
    "remaining",
    "absent",
    "unsupported",
];
const STOP: &[&str] = &[
    "and", "the", "of", "a", "an", "to", "in", "for", "with", "or", "no", "not",
];

/// An area name's display form: before ` (` or a spaced em dash.
pub fn display_name(name: &str) -> String {
    let n = name.trim();
    let n = n.split(" (").next().unwrap_or(n);
    let n = n.split(" \u{2014} ").next().unwrap_or(n);
    n.trim().trim_end_matches(['✱', '*']).trim().to_string()
}

/// The key two rows (or files) pair areas by.
pub fn area_key(name: &str) -> String {
    static LETTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-N] ").unwrap());
    let n = display_name(&md::plain(name));
    let n = LETTER
        .replace(&n, "")
        .to_lowercase()
        .replace('&', "and")
        .replace("colour", "color")
        .replace("behaviour", "behavior");
    let n: String = n
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c
            } else {
                ' '
            }
        })
        .collect();
    n.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tokens(s: &str) -> BTreeSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !STOP.contains(w))
        .map(str::to_string)
        .collect()
}

/// The overlap of two names' words (without stop words), over the smaller
/// set. Two words match when they are equal or one of four or more letters
/// starts the other (`sign`, `signatures`). One shared word counts only
/// when it is all of the smaller name.
pub fn overlap(a: &str, b: &str) -> f64 {
    let (a, b) = (tokens(a), tokens(b));
    let (small, large) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    let same = |x: &str, y: &str| {
        x == y || (x.len().min(y.len()) >= 4 && (x.starts_with(y) || y.starts_with(x)))
    };
    let shared = small
        .iter()
        .filter(|x| large.iter().any(|y| same(x, y)))
        .count();
    if small.is_empty() || (shared < 2 && small.len() > 1) {
        return 0.0;
    }
    shared as f64 / small.len() as f64
}

/// Which column holds the gaps text, and whether it is mixed (holds what
/// exists as well as what is missing).
fn gaps_col(t: &Table, hours_is_remaining: bool) -> Option<(usize, bool)> {
    let level1 = t
        .hnorm
        .iter()
        .position(|h| h == "main gaps" || h == "main shortfalls" || h.starts_with("biggest gaps"))
        .or_else(|| {
            // A prose Remaining column, not Remaining (h) or the hours.
            (!hours_is_remaining)
                .then(|| t.hnorm.iter().position(|h| h == "remaining"))
                .flatten()
        });
    if let Some(c) = level1 {
        return Some((c, false));
    }
    let level2 = t.hnorm.iter().position(|h| {
        h.contains("what's missing")
            || h.contains("what’s missing")
            || h.contains("evidence and main gaps")
    });
    if let Some(c) = level2 {
        return Some((c, true));
    }
    ["evidence", "notes", "detail"]
        .iter()
        .find_map(|a| t.col(&[a]))
        .map(|c| (c, true))
}

/// Fills the area fields of `out`.
pub fn read(out: &mut Parity, rm: Option<&Doc>, tap: Option<&Doc>) {
    let found = tap.and_then(|d| area_table(d).map(|t| (d, t)));
    let found = found.or_else(|| rm.and_then(|d| area_table(d).map(|t| (d, t))));
    let Some((d, t)) = found else {
        out.areas_total =
            Cell::absent("No feature-area table in docs/target-app-parity.md or ROADMAP.md");
        return;
    };
    let mut areas = rows(out, d, t);
    if !areas.iter().any(|a| a.weight.is_some()) && t.col(&["weight"]).is_none() {
        match prose_weights(d) {
            Some((items, line)) => {
                let label = format!("weights from prose, {}:{line}", d.path);
                for a in &mut areas {
                    let best = items
                        .iter()
                        .map(|(k, w)| (overlap(&a.key, k), *w))
                        .filter(|(o, _)| *o >= 0.6)
                        .max_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
                    if let Some((_, w)) = best {
                        a.weight = Some(w);
                        a.weight_src = Some(label.clone());
                    }
                }
            }
            None => out.area_weights_why = Some("no weight column in the area table".into()),
        }
    }
    let weighted: Vec<&Area> = areas
        .iter()
        .filter(|a| !a.unweighted && a.dup_lines.first().is_none_or(|l| *l == a.src.line))
        .collect();
    let sum: f64 = weighted.iter().filter_map(|a| a.weight).sum();
    if sum > 0.0 {
        out.area_weight_sum = Some(sum);
        out.notes.push(Note::info(
            format!("area weights sum to {}", (sum * 100.0).round() / 100.0),
            d.src(t.line),
        ));
        let (mut num, mut den) = (0.0, 0.0);
        for a in &weighted {
            if let (Some(w), Some(r)) = (a.weight, a.ready.get().and_then(Score::pct)) {
                num += w * r;
                den += w;
            }
        }
        if let (true, Some(stated)) = (den > 0.0, out.areas_total.get().and_then(Score::pct)) {
            if (num / den - stated).abs() > 1.0 {
                out.notes.push(Note::warn(
                    format!(
                        "weighted areas give {:.1}%; the stated total is {stated}%",
                        num / den
                    ),
                    out.areas_total
                        .src()
                        .cloned()
                        .unwrap_or_else(|| d.src(t.line)),
                ));
            }
        }
        for a in &mut areas {
            if let (Some(w), Some(r), false) =
                (a.weight, a.ready.get().and_then(Score::pct), a.unweighted)
            {
                a.shortfall = Some(w / sum * (100.0 - r));
            }
        }
    }
    // ROADMAP's Features rows against the parity doc's areas.
    if let (Some(rm), true) = (rm, d.path != super::ROADMAP) {
        if let Some(rt) = area_table(rm) {
            let mut scratch = Parity::default();
            let others = rows(&mut scratch, rm, rt);
            for o in &others {
                let pair = areas.iter().find(|a| a.key == o.key).or_else(|| {
                    areas
                        .iter()
                        .map(|a| (overlap(&a.key, &o.key), a))
                        .filter(|(x, _)| *x >= 0.6)
                        .max_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal))
                        .map(|(_, a)| a)
                });
                let Some(a) = pair else { continue };
                let (x, y) = (
                    a.ready.get().and_then(Score::pct),
                    o.ready.get().and_then(Score::pct),
                );
                if let (Some(x), Some(y)) = (x, y) {
                    if (x - y).abs() > 0.5 {
                        out.notes.push(Note::info(
                            format!("{}: {} says {y}%, {} says {x}%", a.name, rm.path, d.path),
                            o.src.clone(),
                        ));
                    }
                }
            }
        }
    }
    out.areas = areas;
}

/// The area table: the first table with an Area column in the first
/// section headed "feature areas", "by feature area", "features by area"
/// or "features"; else any table with an Area column outside the
/// mainstream and essentials sections.
fn area_table(d: &Doc) -> Option<&Table> {
    let has_area = |t: &&Table| t.col(&["area"]).is_some();
    let section = d.headings.iter().position(|h| {
        !h.excluded
            && (["feature areas", "by feature area", "features by area"]
                .iter()
                .any(|p| h.norm.starts_with(p))
                || h.norm == "features")
    });
    if let Some(t) = section.and_then(|h| d.tables_in(h).find(has_area)) {
        return Some(t);
    }
    d.live_tables()
        .filter(has_area)
        .find(|t| !d.under(t.section, &["mainstream", "essentials"]))
}

fn is_summary(t: &Table, r: &md::Row, num_col: Option<usize>) -> bool {
    let Some(first) = r.cells.first() else {
        return false;
    };
    if let Some(c) = num_col {
        if r.cells.get(c).is_none_or(|x| x.is_empty()) {
            return true;
        }
    }
    let name_col = t.col(&["area"]).unwrap_or(0);
    let cell = r.cells.get(name_col).unwrap_or(first);
    let bold = cell.raw.trim_start().starts_with("**");
    let tl = cell.text.to_lowercase();
    bold && [
        "weighted",
        "total",
        "feature depth",
        "features dimension",
        "features, weighted",
        "full parity",
        "all",
    ]
    .iter()
    .any(|s| tl.starts_with(s))
}

fn rows(out: &mut Parity, d: &Doc, t: &Table) -> Vec<Area> {
    let name_col = t.col(&["area"]).unwrap_or(0);
    let num_col = (t.hnorm.first().map(String::as_str) == Some("#")).then_some(0);
    let weight = t.col(&["weight"]);
    let breadth = t.col(&["breadth", "breadth (measured)", "breadth (checklist)"]);
    let mut ready = t.col(&["ready", "ready (est.)", "ready (estimated)"]);
    let mut ready_from = None;
    if ready.is_none() && breadth.is_none() {
        ready = t.col(&["done", "parity", "%", "% ready"]);
        ready_from = ready.map(|c| t.header[c].clone());
    } else if ready.is_none() {
        ready = t.col(&["done", "parity", "% ready"]);
        ready_from = ready.map(|c| t.header[c].clone());
    }
    let explicit_hours = t.col(&[
        "hours",
        "one agent (h)",
        "remaining (h)",
        "remaining (opus 5.5 h)",
    ]);
    let remaining_hours = explicit_hours.is_none()
        && t.col(&["remaining"]).is_some_and(|c| {
            let cells: Vec<_> = t.rows.iter().filter_map(|r| r.cells.get(c)).collect();
            let ok = cells
                .iter()
                .filter(|x| super::value::hours(&x.raw, "", "").is_ok())
                .count();
            ok * 2 >= cells.len().max(1)
        });
    let hours = explicit_hours.or_else(|| remaining_hours.then(|| t.col(&["remaining"])).flatten());
    let gaps = gaps_col(t, remaining_hours);
    let kind = t.col(&["kind"]);
    let doc = t.col(&["doc", "detail"]);
    let counts = (
        t.col(&["features"]),
        t.col(&["shipped"]),
        t.col(&["partial"]),
    );
    let mut areas: Vec<Area> = Vec::new();
    let mut total_seen = false;
    for r in &t.rows {
        if r.malformed {
            continue;
        }
        if is_summary(t, r, num_col) {
            let score_col = ready.or(breadth);
            let label = r
                .cells
                .get(name_col)
                .map(|c| c.text.clone())
                .unwrap_or_default();
            let score = score_cell(d, t.cell(r, score_col), r.line, "total");
            if !total_seen {
                out.areas_total = score;
                total_seen = true;
            } else {
                out.area_alts.push(AltTotal { label, score });
            }
            continue;
        }
        let Some(name_cell) = r.cells.get(name_col) else {
            continue;
        };
        let full_name = name_cell.text.clone();
        let name = display_name(&full_name);
        let key = area_key(&full_name);
        let (w, wblank) = t
            .cell(r, weight)
            .map_or((None, false), |c| weight_of(&c.raw));
        let gaps_text = gaps
            .and_then(|(c, _)| r.cells.get(c))
            .map(|c| c.raw.clone())
            .unwrap_or_default();
        let (missing, more, chips_from) = match gaps {
            Some((_, mixed)) => chips(&gaps_text, mixed),
            None => (Vec::new(), 0, ChipsFrom::None),
        };
        let num = |c: Option<usize>| {
            let text = t.cell(r, c)?.text.replace(',', "");
            NUMBER.find(&text)?.as_str().parse::<f64>().ok()
        };
        let area = Area {
            unweighted: (weight.is_some() && wblank) || full_name.contains('✱'),
            weight: w,
            weight_src: None,
            breadth: if breadth.is_some() {
                score_cell(d, t.cell(r, breadth), r.line, "breadth")
            } else {
                Cell::absent(format!("No breadth column in {}", d.path))
            },
            ready: if ready.is_some() {
                score_cell(d, t.cell(r, ready), r.line, "ready")
            } else {
                Cell::absent(format!("No ready column in {}", d.path))
            },
            ready_from: ready_from.clone(),
            hours: if hours.is_some() {
                let header = hours.map(|h| t.header[h].clone()).unwrap_or_default();
                hours_cell(d, t.cell(r, hours), r.line, &name, &header)
            } else {
                Cell::absent(format!("No hours column in {}", d.path))
            },
            missing,
            more,
            chips_from,
            raw_gaps: md::plain(&gaps_text),
            gaps_header: gaps.map(|(c, _)| t.header[c].clone()),
            dup_lines: Vec::new(),
            conflict: None,
            kind: t.cell(r, kind).map(|c| c.text.clone()).unwrap_or_default(),
            doc: doc_link(d, t.cell(r, doc)),
            counts: (counts.0.is_some() || counts.1.is_some()).then(|| AreaCounts {
                features: num(counts.0),
                shipped: num(counts.1),
                partial: num(counts.2),
            }),
            src: d.src(r.line),
            shortfall: None,
            name,
            full_name,
            key,
        };
        match areas.iter_mut().find(|a| a.key == area.key) {
            Some(first) => {
                let same = first.weight == area.weight
                    && super::score_text(&first.ready) == super::score_text(&area.ready)
                    && super::score_text(&first.breadth) == super::score_text(&area.breadth)
                    && super::hours_text(&first.hours) == super::hours_text(&area.hours);
                if first.dup_lines.is_empty() {
                    first.dup_lines.push(first.src.line);
                }
                first.dup_lines.push(area.src.line);
                if !same {
                    first.conflict = Some(format!(
                        "line {}: weight {:?}, {} ready, {}; line {}: weight {:?}, {} ready, {}",
                        first.src.line,
                        first.weight,
                        super::score_text(&first.ready),
                        super::hours_text(&first.hours),
                        area.src.line,
                        area.weight,
                        super::score_text(&area.ready),
                        super::hours_text(&area.hours)
                    ));
                } else if first.raw_gaps != area.raw_gaps {
                    for c in area.missing {
                        if !first.missing.iter().any(|m| m.text == c.text) {
                            first.missing.push(c);
                        }
                    }
                }
                let lines: Vec<String> = first.dup_lines.iter().map(u32::to_string).collect();
                out.notes.push(Note::warn(
                    format!("{} listed twice (lines {})", first.name, lines.join(", ")),
                    d.src(area.src.line),
                ));
            }
            None => areas.push(area),
        }
    }
    if !total_seen {
        if let Some(total) = prose_total(d, t) {
            out.areas_total = total;
        } else {
            out.areas_total =
                Cell::absent(format!("No total row under the area table in {}", d.path));
        }
    }
    for a in &mut areas {
        if a.missing.len() > MAX_CHIPS {
            a.more += (a.missing.len() - MAX_CHIPS) as u32;
            a.missing.truncate(MAX_CHIPS);
        }
    }
    areas
}

/// Area weights from `name N%` items under a heading starting with
/// "weights": a bullet per item, or a paragraph listing them separated by
/// commas. Returns `(key, weight)` pairs and the first item's line.
fn prose_weights(d: &Doc) -> Option<(Vec<(String, f64)>, u32)> {
    let h = d.heading(&["weights"])?;
    let mut items: Vec<(String, f64, u32)> = Vec::new();
    for b in d.blocks_in(h) {
        if b.item {
            if let Some(c) = WEIGHT_ITEM.captures(&b.text) {
                if let Ok(w) = c[2].parse() {
                    items.push((area_key(&c[1]), w, b.line));
                }
            }
            continue;
        }
        for c in INLINE_WEIGHT.captures_iter(&md::plain(&b.text)) {
            if let Ok(w) = c[2].parse() {
                items.push((area_key(c[1].trim()), w, b.line));
            }
        }
    }
    let line = items.first()?.2;
    Some((items.into_iter().map(|(k, w, _)| (k, w)).collect(), line))
}

/// The Missing chips of a gaps cell: `(chips, more, where from)`.
pub fn chips(raw: &str, mixed: bool) -> (Vec<Chip>, u32, ChipsFrom) {
    let text = md::plain(raw);
    // `missing foo` without a colon marks a clause only where one starts.
    let marks: Vec<regex::Match> = MARKER
        .find_iter(&text)
        .filter(|m| {
            m.as_str().contains(':') || {
                let before = text[..m.start()].trim_end();
                before.is_empty() || before.ends_with([';', '.', ':', ','])
            }
        })
        .collect();
    let (spans, from): (Vec<String>, ChipsFrom) = if !marks.is_empty() {
        let mut spans = Vec::new();
        for (i, m) in marks.iter().enumerate() {
            let start = if m.as_str().ends_with(':') {
                m.end()
            } else {
                // `missing foo` without a colon: the clause starts at the noun.
                m.start() + m.as_str().find(char::is_whitespace).unwrap_or(0)
            };
            let mut end = marks.get(i + 1).map_or(text.len(), |n| n.start());
            if let Some(e) = END_MARKER.find(&text[start..end]) {
                end = start + e.start();
            }
            if let Some(e) = sentence_end(&text[start..end]) {
                end = start + e;
            }
            spans.push(text[start..end].to_string());
        }
        (spans, ChipsFrom::Marker)
    } else if !mixed {
        (vec![text.clone()], ChipsFrom::GapsColumn)
    } else {
        let clauses: Vec<String> = split_sentences(&text)
            .into_iter()
            .flat_map(|s| split_top(&s, ';'))
            .filter(|c| {
                let l = c.to_lowercase();
                NEGATIVE.iter().any(|w| contains_word(&l, w)) || l.contains("n't")
            })
            .collect();
        if clauses.is_empty() {
            return (Vec::new(), 0, ChipsFrom::None);
        }
        (clauses, ChipsFrom::NegativeClauses)
    };
    let mut out = Vec::new();
    for span in spans {
        for piece in split_top(&span, ';') {
            for part in split_top_str(&piece, ", ") {
                for item in split_sentences(&part) {
                    if let Some(chip) = chip(&item) {
                        out.push(chip);
                    }
                }
            }
        }
    }
    let more = out.len().saturating_sub(MAX_CHIPS) as u32;
    out.truncate(MAX_CHIPS);
    (out, more, from)
}

/// One item as a chip, or `None` when it is done, out of scope or empty.
fn chip(item: &str) -> Option<Chip> {
    let item = item.trim().trim_end_matches(['.', ';', ',']).trim();
    if item.is_empty() {
        return None;
    }
    let lower = item.to_lowercase();
    if lower.ends_with(" oos") || lower == "oos" || lower.ends_with("out of scope") {
        return None;
    }
    // Text outside parentheses, and each top-level parenthetical.
    let (outside, parens) = parentheticals(item);
    let ol = outside.to_lowercase();
    if is_done(&ol) {
        return None;
    }
    let mut hover = Vec::new();
    for p in &parens {
        let rest = ISSUE.replace_all(p, "");
        let rest = COMMIT.replace_all(&rest, "");
        let mut rest = rest.to_lowercase();
        let had_done = is_done(&rest);
        for w in DONE_WORDS.iter().chain(DONE_PHRASES) {
            rest = remove_word(&rest, w);
        }
        let left: String = rest.chars().filter(|c| c.is_alphanumeric()).collect();
        // `(#494, closed)` says the item is done; `(#885)` only links it.
        if left.is_empty() && had_done {
            return None;
        }
        hover.push(p.clone());
    }
    let text = outside.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = text
        .trim()
        .trim_end_matches(['.', ';', ',', ':'])
        .trim()
        .to_string();
    if text.is_empty() {
        return None;
    }
    Some(Chip {
        text,
        hover: (!hover.is_empty()).then(|| hover.join(" ")),
    })
}

/// Whether text (outside parentheses, lowercase) says the item is done.
pub(super) fn is_done(l: &str) -> bool {
    if DONE_PHRASES.iter().any(|p| l.contains(p)) {
        return true;
    }
    DONE_WORDS.iter().any(|w| {
        l.match_indices(w).any(|(i, _)| {
            let before = l[..i].chars().next_back();
            let after = &l[i + w.len()..];
            let whole = !before.is_some_and(|c| c.is_alphanumeric() || c == '-')
                && !after
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '-');
            // "fixed 3×4" names a size, not a fix.
            let size =
                *w == "fixed" && after.trim_start().starts_with(|c: char| c.is_ascii_digit());
            whole && !size && !negated(&l[..i])
        })
    })
}

/// Whether the clause that `before` ends says "not" within its last three
/// words ("not implemented", "has not been fixed", "never shipped").
fn negated(before: &str) -> bool {
    let clause = before
        .rsplit([',', ';', ':', '.', '(', ')'])
        .next()
        .unwrap_or(before);
    clause
        .split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’')
        .filter(|w| !w.is_empty())
        .rev()
        .take(3)
        .any(|w| matches!(w, "not" | "never" | "yet") || w.ends_with("n't") || w.ends_with("n’t"))
}

fn remove_word(s: &str, w: &str) -> String {
    let mut out = s.to_string();
    while let Some(i) = out.find(w) {
        out.replace_range(i..i + w.len(), " ");
    }
    out
}

/// `item` with its top-level parentheticals removed, and their contents.
fn parentheticals(item: &str) -> (String, Vec<String>) {
    let mut outside = String::new();
    let mut parens = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in item.chars() {
        match c {
            '(' => {
                if depth > 0 {
                    cur.push(c);
                }
                depth += 1;
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    parens.push(std::mem::take(&mut cur));
                } else {
                    cur.push(c);
                }
            }
            _ if depth > 0 => cur.push(c),
            _ => outside.push(c),
        }
    }
    if !cur.is_empty() {
        parens.push(cur);
    }
    (outside, parens)
}

/// Where the first sentence of `s` ends: `. ` followed by an uppercase
/// letter, outside parentheses.
fn sentence_end(s: &str) -> Option<usize> {
    let b: Vec<(usize, char)> = s.char_indices().collect();
    let mut depth = 0i32;
    for (k, &(i, c)) in b.iter().enumerate() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            '.' if depth <= 0 => {
                let next = b.get(k + 1).map(|x| x.1);
                let after = b.get(k + 2).map(|x| x.1);
                if next == Some(' ') && after.is_some_and(char::is_uppercase) {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_sentences(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = sentence_end(rest) {
        out.push(rest[..i].to_string());
        rest = &rest[i + 1..];
    }
    out.push(rest.to_string());
    out.into_iter().filter(|x| !x.trim().is_empty()).collect()
}

/// `s` split on `sep` outside parentheses and brackets.
fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        if c == sep && depth <= 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

fn split_top_str(s: &str, sep: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        if depth <= 0 && i >= start && s[i..].starts_with(sep) {
            out.push(s[start..i].to_string());
            start = i + sep.len();
        }
    }
    out.push(s[start.min(s.len())..].to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::super::{read as read_all, Input, PARITY_DOC, ROADMAP};
    use super::*;

    fn texts(raw: &str, mixed: bool) -> Vec<String> {
        chips(raw, mixed).0.into_iter().map(|c| c.text).collect()
    }

    #[test]
    fn a_remaining_marker_starts_the_chips() {
        assert_eq!(
            texts("Auto-leading resets are implemented; remaining: hyphenation options, ruby. Done: kerning", false),
            ["hyphenation options", "ruby"]
        );
        assert_eq!(chips("x; Missing: a, b", true).2, ChipsFrom::Marker);
    }

    #[test]
    fn parentheticals_decide_whether_an_item_is_done() {
        assert_eq!(
            texts("Pencil nuances, Blob Brush options (#494, closed)", false),
            ["Pencil nuances"]
        );
        let (c, _, _) = chips("Preprocessor (`#include` landed, #759)", false);
        assert_eq!(c[0].text, "Preprocessor");
        assert_eq!(c[0].hover.as_deref(), Some("#include landed, #759"));
        assert_eq!(
            texts("soft text at Fit (#417, mostly fixed by #444)", false),
            ["soft text at Fit"]
        );
        assert_eq!(
            texts("grid fixed 3×4 layout", false),
            ["grid fixed 3×4 layout"]
        );
        assert_eq!(texts("Snap fixed in #12", false), Vec::<String>::new());
        // A negated completion word leaves the item missing.
        assert_eq!(
            texts(
                "PDF export is not implemented; Snap has not been fixed",
                false
            ),
            ["PDF export is not implemented", "Snap has not been fixed"]
        );
        assert_eq!(
            texts("Ink isn't shipped, Snap landed", false),
            ["Ink isn't shipped"]
        );
        assert_eq!(
            texts("ghost borders (#983); TIFF settings are implemented", false),
            ["ghost borders"]
        );
        assert_eq!(
            texts(
                "Outline Object, the Transform effect's missing options (#885)",
                false
            ),
            ["Outline Object", "the Transform effect's missing options"]
        );
        assert_eq!(
            texts("Pen works; missing tilt, rotation", false),
            ["tilt", "rotation"]
        );
        assert_eq!(
            texts("Adobe Color Themes OOS, swatch groups", false),
            ["swatch groups"]
        );
    }

    #[test]
    fn a_mixed_cell_without_a_marker_keeps_negative_clauses() {
        assert_eq!(
            texts(
                "All 306 effects exist. No `.ffx` presets; scripting works; still slow scrubbing",
                true
            ),
            ["No .ffx presets", "still slow scrubbing"]
        );
        assert_eq!(
            chips("Everything works well", true),
            (Vec::new(), 0, ChipsFrom::None)
        );
    }

    #[test]
    fn at_most_twelve_chips() {
        let raw = (1..=15)
            .map(|i| format!("item{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let (c, more, _) = chips(&raw, false);
        assert_eq!((c.len(), more), (12, 3));
    }

    fn tap(text: &str) -> Parity {
        read_all(&[Input {
            path: PARITY_DOC,
            oid: "t",
            text,
        }])
    }

    #[test]
    fn duplicate_rows_count_their_weight_once() {
        let md = "# P\n## Feature areas\n| Area | Weight | Done | Main gaps |\n|---|---|---|---|\n\
            | Drawing (Pen, Pencil) | 50 | 80% | a; b |\n| Type core | 30 | 70% | c |\n| Drawing (Pen, Pencil) | 50 | 80% | d |\n\
            | Print | 25 | 90% | e |\n| Type core | 11 | 60% | c |\n| **Feature depth** | **105** | **~78%** | |\n\
            | Interaction pass (modifiers) | | | [ui.md](ui.md) |\n| **Full parity** | | **~55%** | |\n";
        let p = tap(md);
        assert_eq!(p.area_weight_sum, Some(105.0));
        assert_eq!(p.areas.iter().filter(|a| !a.unweighted).count(), 3);
        let draw = &p.areas[0];
        assert_eq!(draw.name, "Drawing");
        assert_eq!(draw.dup_lines, [5, 7]);
        assert_eq!(
            draw.missing
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "d"]
        );
        assert_eq!(draw.ready_from.as_deref(), Some("Done"));
        assert!(
            p.areas[1].conflict.is_some(),
            "the second Type core row differs"
        );
        assert_eq!(
            p.notes
                .iter()
                .filter(|n| n.text.contains("listed twice"))
                .count(),
            2
        );
        assert!(p
            .areas
            .iter()
            .any(|a| a.unweighted && a.name == "Interaction pass"));
        assert_eq!(p.areas_total.get().and_then(Score::pct), Some(78.0));
        assert_eq!(p.area_alts.len(), 1);
        assert_eq!(p.area_alts[0].label, "Full parity");
    }

    #[test]
    fn an_empty_number_cell_is_a_summary_row() {
        let md = "# P\n## Feature areas\n| # | Area | Weight | Ready |\n|---|---|---|---|\n\
            | 1 | Canvas | 60 | 50% |\n| 2 | Layers | 53 | 40% |\n| | Core editor only | 70 | 48% |\n";
        let p = tap(md);
        assert_eq!(p.areas.len(), 2);
        assert_eq!(p.areas_total.get().and_then(Score::pct), Some(48.0));
        assert_eq!(p.area_weight_sum, Some(113.0));
    }

    #[test]
    fn roadmap_names_pair_with_the_parity_doc() {
        let rm = "# R\n## Features\n| Area | % |\n|---|---|\n| [Selection, transform & align](docs/x.md) \u{2014} tools | 80% |\n";
        let md = "# P\n## Feature areas\n| Area | Weight | Ready |\n|---|---|---|\n| Selection, transform & align tools | 6 | 85% |\n";
        let p = read_all(&[
            Input {
                path: ROADMAP,
                oid: "r",
                text: rm,
            },
            Input {
                path: PARITY_DOC,
                oid: "t",
                text: md,
            },
        ]);
        assert!(p.notes.iter().any(|n| n.text == "Selection, transform & align tools: ROADMAP.md says 80%, docs/target-app-parity.md says 85%"));
        assert_eq!(
            area_key("[Selection, transform & align](x.md) \u{2014} tools"),
            "selection transform and align"
        );
    }

    #[test]
    fn weights_read_from_a_prose_list() {
        let md = "# P\n## Weights\n- Viewing and navigation: 20%\n- Editing text — 30%\n- Forms 50%\n\n\
            ## By feature area\n| Area | Breadth | Ready |\n|---|---|---|\n| A Viewing & navigation | 90% | 80% |\n| B Editing text | 50% | 40% |\n| C Forms | 60% | 30% |\n";
        let p = tap(md);
        let w: Vec<_> = p.areas.iter().map(|a| a.weight).collect();
        assert_eq!(w, [Some(20.0), Some(30.0), Some(50.0)]);
        assert_eq!(
            p.areas[0].weight_src.as_deref(),
            Some("weights from prose, docs/target-app-parity.md:3")
        );
        let none = tap("# P\n## Feature areas\n| Area | Ready |\n|---|---|\n| X | 5% |\n");
        assert_eq!(
            none.area_weights_why.as_deref(),
            Some("no weight column in the area table")
        );
    }
}
