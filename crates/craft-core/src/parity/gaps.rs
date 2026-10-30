//! Gaps from `docs/gaps.md`: the ranked table (form A) or numbered H3
//! headings (form B), the unranked gaps under category headings, which
//! ones block alpha or beta, and their sums.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;

use super::areas::overlap;
use super::dims::{doc_link, resolve};
use super::md::{self, Doc, Table};
use super::value::{self, Legend};
use super::{Cell, Flag, Gap, Hours, HoursSum, Link, Note, Parity, Src};

static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(G-?\d+|[A-Z]{1,2}\d+)\b").unwrap());
static NUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+)").unwrap());
static DETAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#{2,3} (G-?\d+|[A-Z]{1,2}\d+|\d+)\.?\s+(.+)$").unwrap());
static FORM_B: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d+)\.\s+(.+?)(?:\s+\(([A-Z, ]+|[a-z ]+)\))?(?:\s+·\s+\*\*ALPHA BLOCKER\*\*)?$")
        .unwrap()
});
static ALPHA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\*{0,2}\[?alpha[ -]blocker\]?\.?\*{0,2}").unwrap());
static BETA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\*{0,2}\[?beta[ -]blocker\]?\.?\*{0,2}").unwrap());
static DECIDES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)gaps? (\d+)(?: and (\d+))? decides? beta").unwrap());
static NEEDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)beta needs #(\d+)\s?[–-]\s?#?(\d+)").unwrap());
static ALPHA_NONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)alpha blockers:\s*none|no alpha blockers|none blocks \**alpha|no gap blocks the \[?alpha").unwrap()
});
static BY_KIND_ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-*]\s+\*\*([^*]+?):?\*\*:?\s*(.+)$").unwrap());
static BOLD_KIND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\*\*([a-zA-Z /-]+?) gaps\*\*:?\s*((?:G?\d+[, ]*(?:and )?)+)").unwrap()
});
static ITEM_HOURS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+(?:\.\d+)?)\s?[–-]\s?(\d+(?:\.\d+)?)\s?h\b").unwrap());
static ESTIMATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*Estimate:\*\*\s*(.*)").unwrap());

const PART_WORDS: &[&str] = &["part", "partly", "partial", "only", "core"];

fn is_part(q: &str) -> bool {
    let l = q.to_lowercase();
    PART_WORDS.iter().any(|w| md::contains_word(&l, w))
}

/// What the document says about beta in general: whether unmarked gaps
/// read as not blocking beta.
#[derive(Default)]
struct Ctx {
    legend: Legend,
    beta_known: bool,
}

/// Fills `out.gaps` and `out.gap_totals` from `docs/gaps.md`.
pub fn read(out: &mut Parity, d: Option<&Doc>) {
    let Some(d) = d else {
        out.gap_totals.stated = Cell::absent("No docs/gaps.md at HEAD");
        out.gap_totals.beta_unknown = true;
        return;
    };
    let blocks = d.blocks();
    let mut ctx = Ctx {
        legend: blocks
            .iter()
            .filter(|b| !b.excluded)
            .map(|b| value::legend(&b.text))
            .find(|l| l.len() >= 2)
            .unwrap_or_default(),
        ..Default::default()
    };
    // The prose outside excluded sections (revision history and the like).
    let all_text: String = d
        .lines
        .iter()
        .filter(|l| !l.excluded)
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    out.gap_totals.alpha_none = ALPHA_NONE.is_match(&all_text);
    let tables: Vec<&Table> = d
        .live_tables()
        .filter(|t| t.col(&["gap"]).is_some() && !d.under(t.section, &["closed"]))
        .collect();
    let mut gaps = if tables.is_empty() {
        out.gap_totals.form = if d
            .headings
            .iter()
            .any(|h| h.level == 3 && FORM_B.is_match(&h.text))
        {
            "headings".into()
        } else {
            String::new()
        };
        form_b(d, &ctx)
    } else {
        out.gap_totals.form = "table".into();
        let t0 = tables[0];
        let has_beta_col = t0.col(&["beta", "b", "blocks beta"]).is_some();
        let impact_beta = t0.col(&["impact", "user impact"]).is_some_and(|c| {
            t0.rows.iter().any(|r| {
                r.cells
                    .get(c)
                    .is_some_and(|x| impact_beta(&x.text) == Some(true))
            })
        });
        // A prose sentence names the gaps that block beta; it says nothing
        // about the others, which stay unknown.
        let markers = BETA.is_match(&all_text);
        let rank_b = t0.rows.iter().any(|r| {
            r.cells
                .first()
                .is_some_and(|c| c.text.trim().ends_with(" B"))
        });
        ctx.beta_known = has_beta_col
            || impact_beta
            || markers
            || rank_b
            || all_text.to_lowercase().contains("beta blockers are marked");
        let mut gaps = table_gaps(d, t0, &ctx, 1, None);
        let top_n = d.under(t0.section, &["top "]);
        for t in &tables[1..] {
            let group = d.chain(t.section).first().map(|h| md::plain(&h.text));
            let mut more = table_gaps(d, t, &ctx, 0, group.clone());
            if let (Some(g), true) = (&group, t.col(&["kind", "category", "dimension"]).is_none()) {
                let k = value::kind(g)
                    .map(str::to_string)
                    .unwrap_or_else(|| super::dims::canonical(g).key().replace('_', " "));
                for gap in &mut more {
                    if gap.kinds.is_empty() {
                        gap.kinds = vec![k.clone()];
                    }
                }
            }
            if top_n {
                pair_top(&mut gaps, &mut more);
            }
            gaps.extend(more);
        }
        prose_beta_flags(&all_text, &mut gaps);
        unranked_headings(d, &ctx, &mut gaps, t0);
        gaps
    };
    by_kind(d, &blocks, &mut gaps);
    details(d, &mut gaps);
    numbering(out, d, &gaps);
    keys(&mut gaps, out, d);
    totals(out, d, &tables, &gaps);
    out.gaps = gaps;
}

/// `(title, alpha, beta, scope)` from a gap cell, with blocker markers
/// removed: an alpha marker sets Alpha (Partial with a part qualifier), a
/// beta marker sets Beta. A leading token is removed only when it is `id`.
fn title_and_markers(raw: &str, id: &str) -> (String, Option<Flag>, Option<Flag>, Option<String>) {
    let mut alpha = None;
    let mut beta = None;
    let mut scope = None;
    // A parenthetical that holds a marker goes, its text kept.
    let mut text = raw.to_string();
    let mut search = 0;
    while let Some(open) = text[search..].find('(').map(|i| search + i) {
        let Some(close) = text[open..].find(')').map(|i| open + i) else {
            break;
        };
        let inner = text[open + 1..close].to_string();
        let has_alpha = ALPHA.is_match(&inner);
        let has_beta = BETA.is_match(&inner);
        if has_alpha || has_beta {
            let q = md::plain(&inner);
            if has_alpha {
                alpha = Some(if is_part(&q) {
                    Flag::Partial(q.clone())
                } else {
                    Flag::Yes
                });
            }
            if has_beta {
                beta = Some(if is_part(&q) {
                    Flag::Partial(q.clone())
                } else {
                    Flag::Yes
                });
            }
            scope = Some(q);
            text.replace_range(open..=close, "");
        } else {
            search = close + 1;
        }
    }
    if ALPHA.is_match(&text) {
        alpha.get_or_insert(Flag::Yes);
        text = ALPHA.replace_all(&text, "").into_owned();
    }
    if BETA.is_match(&text) {
        beta.get_or_insert(Flag::Yes);
        text = BETA.replace_all(&text, "").into_owned();
    }
    let text = text.trim().to_string();
    (title(&text, id), alpha, beta, scope)
}

/// The title of a gap cell (markers already removed), without a leading
/// `id` token.
fn title(raw: &str, id: &str) -> String {
    let raw = raw.trim();
    let text = md::plain(raw);
    let lead = ID
        .find(&text)
        .is_some_and(|m| !id.is_empty() && m.as_str() == id);
    let raw = if lead {
        raw.replacen(id, "", 1)
            .trim_start_matches(['.', ':', ' '])
            .to_string()
    } else {
        raw.to_string()
    };
    let raw = raw.trim();
    // A struck lead span (a closed gap) is the title.
    if let Some(rest) = raw.trim_start_matches("**").strip_prefix("~~") {
        if let Some(end) = rest.find("~~") {
            return title(&rest[..end], id);
        }
    }
    let lead_bold = raw.trim_start_matches("~~").starts_with("**");
    if lead_bold {
        let body = raw.trim_start_matches("~~");
        let after_open = &body[2..];
        if let Some(close) = after_open.find("**") {
            let span = md::plain(&after_open[..close]);
            let rest = md::plain(after_open[close + 2..].trim_start_matches("~~"));
            // `**Content controls** landed (#381)` keeps its own title.
            let next = rest.split_whitespace().next().unwrap_or("").to_lowercase();
            let done = super::areas::is_done(&next);
            let extend = !done
                && (rest.starts_with(|c: char| c.is_lowercase())
                    || rest.starts_with(',')
                    || rest.starts_with(':'));
            if !extend {
                return span.trim_end_matches(['.', ':']).trim().to_string();
            }
            let full = md::plain(raw);
            return cut(&full, &[" (", ". ", "; "]);
        }
    }
    if raw.starts_with('[') {
        if let Some((text, _, _)) = md::link(raw) {
            let t = md::plain(text);
            return match ID.find(&t).filter(|m| m.as_str() == id) {
                Some(m) => t[m.end()..].trim().to_string(),
                None => t,
            };
        }
    }
    cut(&md::plain(raw), &[" (", ". "])
}

/// `s` up to the first of `stops`, at most 120 characters (cut at a word,
/// with `…`).
fn cut(s: &str, stops: &[&str]) -> String {
    let at = stops
        .iter()
        .filter_map(|p| s.find(p))
        .min()
        .unwrap_or(s.len());
    let t = s[..at].trim();
    if t.chars().count() <= 120 {
        return t.trim_end_matches(['.', ':']).to_string();
    }
    let mut end = 0;
    for (i, c) in t.char_indices() {
        if i > 120 {
            break;
        }
        if c == ' ' {
            end = i;
        }
    }
    format!("{}…", t[..end].trim_end_matches([',', ';', ':']))
}

/// The gaps of one table. `rank_from` 1 ranks them in row order; 0 leaves
/// them unranked in `group`.
fn table_gaps(d: &Doc, t: &Table, ctx: &Ctx, rank_from: u32, group: Option<String>) -> Vec<Gap> {
    let rank_col = t.col(&["#", "rank"]);
    let gap_col = t.col(&["gap"]);
    let hours_col = t.col(&["hours", "estimate", "est.", "est. (h)"]);
    let beta_col = t.col(&["beta", "b", "blocks beta"]);
    let kind_col = t.col(&["kind", "category", "dimension"]);
    let impact_col = t.col(&["impact", "user impact"]);
    let doc_col = t.col(&["doc", "parity doc", "work · doc"]);
    let mut out = Vec::new();
    let mut rank = rank_from;
    for r in &t.rows {
        let Some(cell) = t.cell(r, gap_col) else {
            continue;
        };
        let rank_text = t
            .cell(r, rank_col)
            .map(|c| c.text.trim().to_string())
            .unwrap_or_default();
        let link_id = cell
            .raw
            .trim_start()
            .starts_with('[')
            .then(|| md::link(cell.raw.trim_start()))
            .flatten()
            .and_then(|(text, _, _)| ID.find(&md::plain(text)).map(|m| m.as_str().to_string()));
        let id = ID
            .find(&rank_text)
            .map(|m| m.as_str().to_string())
            .or(link_id)
            .or_else(|| NUM.find(&rank_text).map(|m| m.as_str().to_string()))
            .unwrap_or_else(|| rank.max(out.len() as u32 + 1).to_string());
        let (title, mut alpha, mut beta, scope) = title_and_markers(&cell.raw, &id);
        let mut src = None;
        let mut scope_note = scope;
        let mut priority = None;
        if beta.is_some() {
            src = Some("marker".to_string());
        }
        if alpha.is_some() {
            src = Some("marker".to_string());
        }
        if rank_text.ends_with(" B") || rank_text == "B" {
            beta = Some(Flag::Yes);
            src = Some("column".into());
        }
        if let Some(c) = t.cell(r, beta_col) {
            let (f, note) = beta_cell(&c.text);
            if beta.is_none() || !matches!(f, Flag::No) {
                beta = Some(f);
            }
            if note.is_some() {
                scope_note = note;
            }
            src = Some("column".into());
        }
        if let Some(c) = t.cell(r, impact_col) {
            let l = c.text.trim().to_lowercase();
            let says = impact_beta(&l);
            if says == Some(true) {
                beta = Some(Flag::Yes);
                src = Some("impact".into());
            } else if !l.is_empty() {
                if beta.is_none() && (ctx.beta_known || says == Some(false)) {
                    beta = Some(Flag::No);
                    src = Some("impact".into());
                }
                // A rating such as "high"; a sentence describing the impact is not one.
                if says.is_none() && c.text.split_whitespace().count() <= 3 {
                    priority = Some(c.text.trim().to_string());
                }
            }
        }
        let beta = beta.unwrap_or(if ctx.beta_known {
            Flag::No
        } else {
            Flag::Unknown
        });
        let alpha = alpha.take().unwrap_or(Flag::No);
        let hcell = t.cell(r, hours_col);
        let hours = match hcell {
            Some(c) => match value::hours(
                &c.raw,
                &title,
                &hours_col.map(|h| t.header[h].clone()).unwrap_or_default(),
            ) {
                Ok(h) => Cell::ok(h, d.src(r.line)),
                Err(why) => Cell::absent(why),
            },
            None => Cell::absent(format!("No hours column in {}", d.path)),
        };
        let (doc, workstream) = match t.cell(r, doc_col) {
            Some(c) if c.raw.contains(" · ") => {
                let (w, l) = c.raw.split_once(" · ").unwrap_or(("", &c.raw));
                (doc_link(d, Some(&md::MdCell::new(l))), Some(md::plain(w)))
            }
            other => (doc_link(d, other), None),
        };
        let row_text = r
            .cells
            .iter()
            .map(|c| c.text.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        let zero = hours.get().is_some_and(|h: &Hours| h.hi == 0)
            || hcell.is_some_and(|c| c.text.trim() == "—");
        let closed = cell.struck_lead
            || row_text.contains("none left")
            || (zero
                && ["closed", "fixed", "landed"]
                    .iter()
                    .any(|w| md::contains_word(&row_text, w)));
        let kinds = t
            .cell(r, kind_col)
            .map(|c| value::kinds(&c.raw, &ctx.legend))
            .unwrap_or_default();
        out.push(Gap {
            key: String::new(),
            id,
            rank,
            group: group.clone(),
            title,
            alpha,
            beta,
            blocks_src: src,
            scope_note,
            priority,
            kinds,
            counted_in: hcell.and_then(|c| value::counted_in(&c.text)),
            hours,
            doc,
            workstream,
            src: d.src(r.line),
            detail_src: None,
            detail: None,
            closed,
            paired_with: Vec::new(),
            issues: cell.issues.clone(),
        });
        if rank_from > 0 {
            rank += 1;
        }
    }
    out
}

/// What an Impact cell says about beta: `Some(true)` for "blocks beta",
/// `Some(false)` for "no longer blocks beta", "never blocks beta", "not
/// blocks beta" or "nothing blocks beta", and `None` when it says neither.
/// "blocks beta" counts only as whole words, so "unblocks beta" says neither.
fn impact_beta(text: &str) -> Option<bool> {
    const PHRASE: &str = "blocks beta";
    let l = text.to_lowercase();
    let mut negated = false;
    for (i, _) in l.match_indices(PHRASE) {
        let before = &l[..i];
        let after = &l[i + PHRASE.len()..];
        if before
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
            || after.chars().next().is_some_and(char::is_alphanumeric)
        {
            continue;
        }
        let before = before.trim_end();
        if ["no longer", "never", "not", "nothing"]
            .iter()
            .any(|n| before.ends_with(n))
        {
            negated = true;
        } else {
            return Some(true);
        }
    }
    negated.then_some(false)
}

/// A Beta cell's flag, and a note: `B (pane)` is Beta with a scope note,
/// `yes (part)` Partial, `no, but …` No with a note.
fn beta_cell(text: &str) -> (Flag, Option<String>) {
    let t = md::plain(text).trim().to_string();
    let l = t.to_lowercase();
    if l.is_empty() || l == "no" || l == "—" {
        return (Flag::No, None);
    }
    if l.starts_with("no,") || l.starts_with("no ") {
        return (Flag::No, Some(t));
    }
    if l == "yes" || l == "b" {
        return (Flag::Yes, None);
    }
    for head in ["yes (", "b ("] {
        if l.starts_with(head) {
            let q = t[head.len()..].trim_end_matches(')').to_string();
            return if is_part(&q) {
                (Flag::Partial(q), None)
            } else {
                (Flag::Yes, Some(q))
            };
        }
    }
    (Flag::Partial(t), None)
}

/// Beta from prose: "gap 1 decides beta", "beta needs #1–#7".
fn prose_beta_flags(text: &str, gaps: &mut [Gap]) {
    let mut ids: Vec<String> = Vec::new();
    for c in DECIDES.captures_iter(text) {
        ids.extend(
            [c.get(1), c.get(2)]
                .into_iter()
                .flatten()
                .map(|m| m.as_str().to_string()),
        );
    }
    for c in NEEDS.captures_iter(text) {
        if let (Ok(a), Ok(b)) = (c[1].parse::<u32>(), c[2].parse::<u32>()) {
            ids.extend((a..=b.min(a + 200)).map(|n| n.to_string()));
        }
    }
    for g in gaps.iter_mut() {
        if ids.contains(&g.id) && !matches!(g.beta, Flag::Yes) {
            g.beta = Flag::Yes;
            g.blocks_src = Some("prose".into());
        }
    }
}

/// Pairs a Top-N list with the section rows that repeat its gaps: a paired
/// section row is dropped and named in the Top-N row's `paired_with`.
fn pair_top(top: &mut [Gap], section: &mut Vec<Gap>) {
    section.retain(|s| {
        let found = top.iter_mut().filter(|t| t.rank > 0).find(|t| {
            let issue = !t.issues.is_empty() && t.issues.iter().any(|i| s.issues.contains(i));
            let anchor = t
                .doc
                .as_ref()
                .zip(s.doc.as_ref())
                .is_some_and(|(a, b)| a.url.contains('#') && a.url == b.url);
            issue || anchor || overlap(&md::normalize(&t.title), &md::normalize(&s.title)) >= 0.5
        });
        match found {
            Some(t) => {
                t.paired_with.push(s.id.clone());
                if !t.hours.is_ok() && s.hours.is_ok() {
                    t.hours = s.hours.clone();
                    if t.counted_in.is_none() {
                        t.counted_in = s.counted_in.clone();
                    }
                }
                let alpha = merge_flag(&mut t.alpha, &s.alpha);
                if merge_flag(&mut t.beta, &s.beta) || alpha {
                    t.blocks_src = s.blocks_src.clone();
                }
                for k in &s.kinds {
                    if !t.kinds.contains(k) {
                        t.kinds.push(k.clone());
                    }
                }
                false
            }
            None => true,
        }
    });
}

/// Sets a ranked gap's flag `top` to the flag `section` of a row or heading
/// that repeats the gap, when that one marks a blocker (yes or in part) and
/// the ranked gap does not (no or unknown). Returns whether it did.
fn merge_flag(top: &mut Flag, section: &Flag) -> bool {
    let merge =
        matches!(top, Flag::No | Flag::Unknown) && matches!(section, Flag::Yes | Flag::Partial(_));
    if merge {
        *top = section.clone();
    }
    merge
}

/// Unranked gaps under category headings: H3s and bold-lead bullets with an
/// estimate, and paragraphs of `name (A–B h)` items. An H3 or bullet that
/// matches a ranked row is that row's detail instead.
fn unranked_headings(d: &Doc, ctx: &Ctx, gaps: &mut Vec<Gap>, ranked: &Table) {
    let ranked_section = ranked.section;
    let mut found = Vec::new();
    for (hi, h) in d.headings.iter().enumerate() {
        if h.excluded || h.level != 2 || Some(hi) == ranked_section || h.norm.starts_with("closed")
        {
            continue;
        }
        let group = md::plain(&h.text);
        // H3s directly under this H2.
        for (si, s) in d.headings.iter().enumerate() {
            if s.parent != Some(hi) || s.level != 3 || DETAIL.is_match(&format!("### {}", s.text)) {
                continue;
            }
            let blocks = d.blocks_in(si);
            if s.norm.starts_with("smaller missing") {
                for b in &blocks {
                    found.extend(
                        item_gaps(d, &b.text, b.line, &group, ctx)
                            .into_iter()
                            .map(|g| (g, false)),
                    );
                }
                continue;
            }
            let estimate = blocks
                .iter()
                .find_map(|b| ESTIMATE.captures(&b.text).map(|c| c[1].to_string()));
            let fields = blocks.iter().any(|b| {
                b.text.starts_with("- **Impact:**") || b.text.starts_with("- **Evidence:**")
            });
            if estimate.is_none() && !fields {
                continue;
            }
            let body: String = blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let beta = if BETA.is_match(&body) {
                Flag::Yes
            } else if ctx.beta_known {
                Flag::No
            } else {
                Flag::Unknown
            };
            let hours = estimate
                .as_deref()
                .map(|e| value::hours(e, "", ""))
                .or_else(|| {
                    ITEM_HOURS
                        .find(&body)
                        .map(|m| value::hours(m.as_str(), "", ""))
                });
            found.push((
                Gap {
                    id: String::new(),
                    group: Some(group.clone()),
                    title: md::plain(&s.text),
                    alpha: if ALPHA.is_match(&body) {
                        Flag::Yes
                    } else {
                        Flag::No
                    },
                    blocks_src: matches!(beta, Flag::Yes).then(|| "marker".to_string()),
                    beta,
                    counted_in: hours
                        .as_ref()
                        .and_then(|h| h.as_ref().ok())
                        .and_then(|h| h.counted_in.clone()),
                    hours: match hours {
                        Some(Ok(h)) => Cell::ok(h, d.src(s.line)),
                        Some(Err(why)) => Cell::absent(why),
                        None => Cell::absent("no estimate under the heading"),
                    },
                    src: d.src(s.line),
                    issues: md::MdCell::new(&s.text).issues,
                    kinds: value::kind(&group)
                        .map(|k| vec![k.to_string()])
                        .unwrap_or_default(),
                    ..Default::default()
                },
                true,
            ));
        }
        // Bold-lead bullets in the H2's own text, before its first H3.
        let first_sub = d
            .headings
            .iter()
            .skip(hi + 1)
            .find(|s| s.parent == Some(hi))
            .map_or(h.end, |s| s.at);
        for b in d.blocks_between(h.at + 1, first_sub) {
            if !b.item || !b.text.starts_with("- **") {
                continue;
            }
            let rest = &b.text[4..];
            let Some(close) = rest.find("**") else {
                continue;
            };
            let head = &rest[..close];
            if head.trim_end().ends_with(':') || rest[close + 2..].starts_with(':') {
                continue;
            }
            let hours = ITEM_HOURS
                .find(&b.text)
                .map(|m| value::hours(m.as_str(), "", ""));
            let Some(Ok(hours)) = hours else { continue };
            let beta = if BETA.is_match(&b.text) {
                Flag::Yes
            } else if ctx.beta_known {
                Flag::No
            } else {
                Flag::Unknown
            };
            found.push((
                Gap {
                    group: Some(group.clone()),
                    title: md::plain(head).trim_end_matches('.').to_string(),
                    blocks_src: matches!(beta, Flag::Yes).then(|| "marker".to_string()),
                    beta,
                    alpha: if ALPHA.is_match(&b.text) {
                        Flag::Yes
                    } else {
                        Flag::No
                    },
                    hours: Cell::ok(hours, d.src(b.line)),
                    src: d.src(b.line),
                    issues: md::MdCell::new(&b.text).issues,
                    kinds: value::kind(&group)
                        .map(|k| vec![k.to_string()])
                        .unwrap_or_default(),
                    ..Default::default()
                },
                true,
            ));
        }
    }
    for (f, matchable) in found {
        if !matchable {
            gaps.push(f);
            continue;
        }
        let ranked_match = gaps.iter_mut().filter(|g| g.rank > 0).find(|g| {
            (!f.issues.is_empty() && f.issues.iter().any(|i| g.issues.contains(i)))
                || overlap(&md::normalize(&g.title), &md::normalize(&f.title)) >= 0.5
        });
        match ranked_match {
            Some(g) => {
                if g.detail_src.is_none() {
                    g.detail_src = Some(f.src.clone());
                }
                let alpha = merge_flag(&mut g.alpha, &f.alpha);
                if merge_flag(&mut g.beta, &f.beta) || alpha {
                    g.blocks_src = Some("marker".into());
                }
            }
            None => gaps.push(f),
        }
    }
}

/// Gaps from a paragraph of `name (A–B h)` items.
fn item_gaps(d: &Doc, text: &str, line: u32, group: &str, ctx: &Ctx) -> Vec<Gap> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut parts = Vec::new();
    for c in text.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if c == ',' && depth <= 0 {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    for p in parts {
        let p = p.trim().trim_end_matches('.');
        let Some(open) = p.find('(') else { continue };
        let name = p[..open].trim();
        let inner = &p[open..];
        if name.is_empty() || !ITEM_HOURS.is_match(inner) {
            continue;
        }
        let hours = value::hours(inner.trim_matches(['(', ')']), "", "");
        out.push(Gap {
            group: Some(group.to_string()),
            title: name.to_string(),
            alpha: Flag::No,
            beta: if ctx.beta_known {
                Flag::No
            } else {
                Flag::Unknown
            },
            hours: match hours {
                Ok(h) => Cell::ok(h, d.src(line)),
                Err(why) => Cell::absent(why),
            },
            src: d.src(line),
            ..Default::default()
        });
    }
    out
}

/// Form B: numbered H3 headings, blocking taken from the parent H2.
fn form_b(d: &Doc, ctx: &Ctx) -> Vec<Gap> {
    let mut out = Vec::new();
    for (i, h) in d.headings.iter().enumerate() {
        if h.level != 3 || h.excluded {
            continue;
        }
        let Some(c) = FORM_B.captures(&h.text) else {
            continue;
        };
        let parent = h.parent.map(|p| &d.headings[p]);
        let pn = parent.map(|p| p.norm.clone()).unwrap_or_default();
        // Closed gaps are left out, as in the table form.
        if pn.contains("out of scope") || d.under(Some(i), &["closed"]) {
            continue;
        }
        let alpha = if h.text.contains("ALPHA BLOCKER") || pn.contains("alpha blocker") {
            Flag::Yes
        } else {
            Flag::No
        };
        let non_blocking = ["non-blocking", "nonblocking", "not blocking"]
            .iter()
            .any(|w| pn.contains(w));
        let (beta, priority) =
            if !non_blocking && (pn.contains("blocking") || pn.contains("beta blockers")) {
                (Flag::Yes, None)
            } else if non_blocking
                || ["high", "medium", "lower", "after beta"]
                    .iter()
                    .any(|w| pn.starts_with(w))
            {
                (Flag::No, parent.map(|p| md::plain(&p.text)))
            } else if matches!(alpha, Flag::Yes) {
                (Flag::Unknown, None)
            } else {
                (Flag::Unknown, parent.map(|p| md::plain(&p.text)))
            };
        let title = c[2].trim().trim_end_matches(['.', ':']).to_string();
        let kinds = c
            .get(3)
            .map(|k| value::kinds(k.as_str(), &ctx.legend))
            .unwrap_or_default();
        let blocks = d.blocks_in(i);
        let estimate = blocks
            .iter()
            .find_map(|b| ESTIMATE.captures(&b.text).map(|c| c[1].to_string()));
        // The supporting document: the first link of a `**Doc:**` item, else
        // the first link after the Estimate.
        let doc_item = blocks
            .iter()
            .find_map(|b| b.text.strip_prefix("- **Doc:**").map(str::to_string));
        let doc = [doc_item.as_deref(), estimate.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|e| md::MdCell::new(e).links.into_iter().next())
            .map(|(text, url)| Link {
                path: resolve(&d.path, &url),
                text,
                url,
            });
        let parsed = estimate.map(|e| value::hours(&e, "", ""));
        let counted_in = parsed
            .as_ref()
            .and_then(|h| h.as_ref().ok())
            .and_then(|h| h.counted_in.clone());
        let hours = match parsed {
            Some(Ok(hh)) => Cell::ok(hh, d.src(h.line)),
            Some(Err(why)) => Cell::Unreadable {
                why: "the Estimate is not a number".into(),
                raw: why,
                src: d.src(h.line),
            },
            None => Cell::absent("no **Estimate:** under the heading"),
        };
        out.push(Gap {
            id: c[1].to_string(),
            rank: out.len() as u32 + 1,
            title: md::plain(&title),
            alpha,
            beta,
            blocks_src: Some("heading".into()),
            priority,
            kinds,
            counted_in,
            hours,
            doc,
            src: d.src(h.line),
            detail_src: Some(d.src(h.line)),
            detail: blocks
                .iter()
                .find(|b| b.text.starts_with("- **Missing:**"))
                .map(|b| md::plain(b.text.trim_start_matches("- **Missing:**"))),
            issues: md::MdCell::new(&h.text).issues,
            ..Default::default()
        });
    }
    out
}

/// Kinds for gaps without a Kind column: a "by kind" list, or bold
/// `**feature gaps** G1, G2` paragraphs.
fn by_kind(d: &Doc, blocks: &[md::Block], gaps: &mut [Gap]) {
    if gaps.iter().all(|g| !g.kinds.is_empty()) {
        return;
    }
    let mut map: Vec<(String, String)> = Vec::new();
    if let Some(h) = d.headings.iter().position(|h| h.norm.contains("by kind")) {
        for b in d.blocks_in(h) {
            if let Some(c) = BY_KIND_ITEM.captures(&b.text) {
                let k = value::kinds(&c[1], &Legend::new());
                for id in c[2].split(|ch: char| ch == ',' || ch.is_whitespace()) {
                    let id = id.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '-');
                    if id
                        .chars()
                        .next()
                        .is_some_and(|ch| ch.is_ascii_digit() || ch == 'G')
                        && id.chars().any(|ch| ch.is_ascii_digit())
                    {
                        for k in &k {
                            map.push((id.to_string(), k.clone()));
                        }
                    }
                }
            }
        }
    }
    for b in blocks {
        for c in BOLD_KIND.captures_iter(&b.text) {
            let k = value::kind(&c[1]).map_or_else(|| c[1].to_lowercase(), str::to_string);
            for id in c[2]
                .split([',', ' '])
                .map(str::trim)
                .filter(|x| !x.is_empty() && *x != "and")
            {
                map.push((id.to_string(), k.clone()));
            }
        }
    }
    for g in gaps.iter_mut().filter(|g| g.kinds.is_empty()) {
        for (id, k) in &map {
            if *id == g.id && !g.kinds.contains(k) {
                g.kinds.push(k.clone());
            }
        }
    }
}

/// Links each gap to its own heading (`## G1 …`, `### 2.`), with the
/// first `**Missing:**` bullet there as its description.
fn details(d: &Doc, gaps: &mut [Gap]) {
    for (i, h) in d.headings.iter().enumerate() {
        if h.level < 2 || h.level > 3 {
            continue;
        }
        let line = format!("{} {}", "#".repeat(h.level), h.text);
        let Some(c) = DETAIL.captures(&line) else {
            continue;
        };
        let id = c[1].to_string();
        let Some(g) = gaps.iter_mut().find(|g| g.id == id && g.rank > 0) else {
            continue;
        };
        g.detail_src = Some(d.src(h.line));
        if g.detail.is_none() {
            g.detail = d
                .blocks_in(i)
                .iter()
                .find(|b| b.text.starts_with("- **Missing:**"))
                .map(|b| md::plain(b.text.trim_start_matches("- **Missing:**")));
        }
    }
    for g in gaps.iter_mut() {
        if let Some(l) = g.doc.as_mut() {
            if l.path.is_none() && !l.url.is_empty() {
                l.path = resolve(&d.path, &l.url);
            }
        }
    }
}

/// Notes on the ranked ids: out of order, or with numbers not listed.
fn numbering(out: &mut Parity, d: &Doc, gaps: &[Gap]) {
    // Letter ids keep their number when the ranking moves them; only plain
    // numbers are expected in order.
    let nums: Vec<(u32, u32)> = gaps
        .iter()
        .filter(|g| g.rank > 0)
        .filter_map(|g| Some((g.id.parse().ok()?, g.src.line)))
        .collect();
    if nums.len() < 2 {
        return;
    }
    if let Some(w) = nums.windows(2).find(|w| w[1].0 < w[0].0) {
        out.notes
            .push(Note::info("source numbering out of order", d.src(w[1].1)));
    } else {
        let listed: BTreeSet<u32> = nums.iter().map(|n| n.0).collect();
        let (lo, hi) = (nums[0].0, nums[nums.len() - 1].0);
        let missing = (lo..=hi).filter(|n| !listed.contains(n)).count();
        if missing > 0 {
            out.notes.push(Note::info(
                format!("{missing} ids not listed"),
                d.src(nums[0].1),
            ));
        }
    }
}

/// Each gap's key: its id when the ids are stable, else its normalized
/// title, with `#rank` added on a collision. Letter ids are stable; so are
/// numbers that skip a closed gap's number, and numbers that name the
/// gaps' own `### N.` headings.
fn keys(gaps: &mut [Gap], out: &mut Parity, d: &Doc) {
    let letters = gaps
        .iter()
        .filter(|g| g.rank > 0)
        .all(|g| g.id.chars().next().is_some_and(|c| c.is_ascii_alphabetic()));
    let nums: Vec<u32> = gaps
        .iter()
        .filter(|g| g.rank > 0)
        .filter_map(|g| g.id.parse().ok())
        .collect();
    let holes = nums.len() > 1
        && nums.windows(2).all(|w| w[1] > w[0])
        && nums.last().copied().unwrap_or(0) as usize > nums.len() + nums[0] as usize - 1;
    let headed = nums
        .iter()
        .filter(|n| {
            d.headings
                .iter()
                .any(|h| (2..=3).contains(&h.level) && h.text.starts_with(&format!("{n}.")))
        })
        .count();
    let anchored = !nums.is_empty() && headed * 2 >= nums.len();
    let stable = gaps.iter().any(|g| g.rank > 0) && (letters || holes || anchored);
    out.gap_totals.ids_stable = stable;
    let mut seen: Vec<String> = Vec::new();
    for g in gaps.iter_mut() {
        let base = if stable && g.rank > 0 && !g.id.is_empty() {
            g.id.clone()
        } else {
            let t = md::normalize(&g.title);
            let t: String = t
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                .collect();
            t.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        let key = if seen.contains(&base) {
            format!("{base}#{}", if g.rank > 0 { g.rank } else { g.src.line })
        } else {
            base
        };
        seen.push(key.clone());
        g.key = key;
    }
}

/// Σ beta, Σ partial and the totals sentence after the ranked table.
fn totals(out: &mut Parity, d: &Doc, tables: &[&Table], gaps: &[Gap]) {
    let open: Vec<&Gap> = gaps
        .iter()
        .filter(|g| !g.closed && g.counted_in.is_none())
        .collect();
    out.gap_totals.alpha = open
        .iter()
        .filter(|g| matches!(g.alpha, Flag::Yes | Flag::Partial(_)))
        .count() as u32;
    out.gap_totals.beta_unknown = gaps.iter().all(|g| matches!(g.beta, Flag::Unknown));
    let sum = |pick: &dyn Fn(&Gap) -> bool| -> Option<HoursSum> {
        let mut s = HoursSum::default();
        for g in open.iter().filter(|g| pick(g)) {
            s.count += 1;
            match g.hours.get() {
                Some(h) => {
                    s.lo += h.lo;
                    s.hi += h.hi;
                }
                None => s.unknown += 1,
            }
        }
        (s.count > 0).then_some(s)
    };
    if !out.gap_totals.beta_unknown {
        out.gap_totals.beta = sum(&|g| matches!(g.beta, Flag::Yes));
        out.gap_totals.partial = sum(&|g| matches!(g.beta, Flag::Partial(_)));
    }
    let stated = tables.first().and_then(|t| {
        let after = d.blocks_between(t.end, d.lines.len());
        let b = after.first()?;
        let scan = Regex::new(r"#\d+\s?[–-]\s?#?\d+")
            .ok()?
            .replace_all(&b.text, "")
            .into_owned();
        let scan = md::plain(&scan);
        let m = ITEM_HOURS.find(&scan)?;
        let h = value::hours(m.as_str(), "", "").ok()?;
        Some(Cell::ok(h, d.src(b.line)).from("the sentence after the ranked table"))
    });
    out.gap_totals.stated =
        stated.unwrap_or_else(|| Cell::absent("no totals sentence after the ranked table"));
    if let (Some(b), Some(s)) = (&out.gap_totals.beta, out.gap_totals.stated.get()) {
        if (b.lo, b.hi) != (s.lo, s.hi) {
            out.notes.push(Note::info(
                format!(
                    "Σ beta gaps {}–{} h; the sentence after the table says {}–{} h",
                    b.lo, b.hi, s.lo, s.hi
                ),
                out.gap_totals
                    .stated
                    .src()
                    .cloned()
                    .unwrap_or_else(|| Src::new(&d.path, 1)),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{read as read_all, Input, GAPS};
    use super::*;

    fn gaps(text: &str) -> Parity {
        read_all(&[
            Input {
                path: super::super::ROADMAP,
                oid: "r",
                text: "# R\n",
            },
            Input {
                path: GAPS,
                oid: "g",
                text,
            },
        ])
    }

    fn t(raw: &str) -> String {
        title_and_markers(raw, "G1").0
    }

    #[test]
    fn titles_lose_their_markers() {
        assert_eq!(t("**Alpha blocker.** Foo bar"), "Foo bar");
        assert_eq!(
            t("**[alpha blocker]** **Saving drops data** (Unknown entities)"),
            "Saving drops data"
        );
        assert_eq!(t("Foo (**beta blocker**)"), "Foo");
        assert_eq!(
            t("**CR3 beyond four verified bodies** (LR-IMP)"),
            "CR3 beyond four verified bodies"
        );
        let (title, alpha, _, scope) = title_and_markers("(Entitlement part: alpha blocker for the mix row) Third-party VST3s blocked; AU editors missing", "G-05");
        assert_eq!(title, "Third-party VST3s blocked; AU editors missing");
        assert_eq!(
            alpha,
            Some(Flag::Partial(
                "Entitlement part: alpha blocker for the mix row".into()
            ))
        );
        assert!(scope.is_some());
        assert_eq!(
            t("Languages: Hindi, Arabic, Vietnamese"),
            "Languages: Hindi, Arabic, Vietnamese"
        );
        assert_eq!(
            t("**Pen pressure**, tilt, rotation (Windows only). More text"),
            "Pen pressure, tilt, rotation"
        );
        assert_eq!(
            t("**Fidelity unmeasured** beyond scalar ease"),
            "Fidelity unmeasured beyond scalar ease"
        );
        assert_eq!(
            t("[G1 Interaction fidelity](#g1-interaction-fidelity)"),
            "Interaction fidelity"
        );
        assert_eq!(
            t("**Camera colour is estimated** (LR-PROF). Raws start"),
            "Camera colour is estimated"
        );
        assert_eq!(
            t("**Content controls** landed (#381): kept"),
            "Content controls"
        );
    }

    #[test]
    fn a_struck_lead_closes_a_gap() {
        let p = gaps("| # | Gap | Est. |\n|---|---|---|\n| S1 | Open thing | 3 |\n| S2 | ~~`cargo fmt --check` fails~~ fixed on main (8fceaaf) | 0 |\n");
        assert!(!p.gaps[0].closed);
        assert!(p.gaps[1].closed);
        assert_eq!(p.gaps[1].title, "cargo fmt --check fails");
    }

    #[test]
    fn beta_cells() {
        assert_eq!(beta_cell("B"), (Flag::Yes, None));
        assert_eq!(beta_cell("B (pane)"), (Flag::Yes, Some("pane".into())));
        assert_eq!(
            beta_cell("yes (part)"),
            (Flag::Partial("part".into()), None)
        );
        assert_eq!(beta_cell("no, but every number depends on it").0, Flag::No);
        assert!(beta_cell("no, but every number depends on it").1.is_some());
        assert_eq!(beta_cell("partly"), (Flag::Partial("partly".into()), None));
        assert_eq!(beta_cell("**yes**"), (Flag::Yes, None));
    }

    #[test]
    fn rank_cells_and_impact_columns() {
        let p = gaps(
            "| # | Gap | Hours |\n|---|---|---|\n| 1 B | **One** | 10–20 |\n| 2 | **Two** | 5 |\n",
        );
        assert_eq!(
            (p.gaps[0].beta.clone(), p.gaps[1].beta.clone()),
            (Flag::Yes, Flag::No)
        );
        assert_eq!(p.gaps[0].id, "1");
        let p = gaps("| # | Gap | Impact | Estimate |\n|---|---|---|---|\n| G1 | A | blocks beta | 150–250 h |\n| G4 | B | high | 100 h |\n| G5 | C | medium | 1 h |\n");
        assert_eq!(p.gaps[0].beta, Flag::Yes);
        assert_eq!(
            (p.gaps[1].beta.clone(), p.gaps[1].priority.as_deref()),
            (Flag::No, Some("high"))
        );
        assert_eq!(
            p.gap_totals.beta.map(|b| (b.count, b.lo, b.hi)),
            Some((1, 150, 250))
        );
    }

    #[test]
    fn an_impact_that_negates_blocks_beta_does_not_block_it() {
        let p = gaps("| # | Gap | Impact | Estimate |\n|---|---|---|---|\n| G1 | A | blocks beta | 10 h |\n| G2 | B | No longer blocks beta | 20 h |\n| G3 | C | never blocks beta | 30 h |\n");
        assert_eq!(
            p.gaps.iter().map(|g| g.beta.clone()).collect::<Vec<_>>(),
            [Flag::Yes, Flag::No, Flag::No]
        );
        assert_eq!(
            p.gap_totals.beta.map(|b| (b.count, b.lo, b.hi)),
            Some((1, 10, 10))
        );
        let p = gaps("| # | Gap | Beta | Impact |\n|---|---|---|---|\n| G1 | A | no | no longer blocks beta |\n");
        assert_eq!(p.gaps[0].beta, Flag::No, "the Beta column's no stays");
        let p = gaps("| # | Gap | Impact |\n|---|---|---|\n| G1 | A | no longer blocks beta |\n| G2 | B | crashes |\n| G3 | C | never blocks beta |\n");
        assert_eq!(
            p.gaps.iter().map(|g| g.beta.clone()).collect::<Vec<_>>(),
            [Flag::No, Flag::Unknown, Flag::No],
            "a table whose Impact cells only negate it says nothing about the others"
        );
        assert_eq!(p.gaps[2].priority, None);
        let p = gaps("| # | Gap | Impact |\n|---|---|---|\n| G1 | A | unblocks beta |\n");
        assert_eq!(
            p.gaps[0].beta,
            Flag::Unknown,
            "\"unblocks\" is not \"blocks\""
        );
    }

    #[test]
    fn alpha_markers_alone_leave_beta_unknown() {
        let p = gaps("| # | Gap | Est. (h) |\n|---|---|---|\n| G-01 | **Alpha blocker.** No device | 25–40 |\n| G-02 | Plugins | 5 |\n");
        assert_eq!(p.gaps[0].alpha, Flag::Yes);
        assert_eq!(p.gaps[0].title, "No device");
        assert_eq!(p.gaps[1].beta, Flag::Unknown);
        assert!(p.gap_totals.beta_unknown);
        assert_eq!(p.gap_totals.beta, None);
        assert_eq!(p.gap_totals.alpha, 1);
    }

    #[test]
    fn no_gaps_document_leaves_beta_unknown() {
        let mut p = Parity::default();
        read(&mut p, None);
        assert!(p.gap_totals.beta_unknown);
        assert_eq!(p.gap_totals.beta, None);
    }

    #[test]
    fn a_gaps_document_without_readable_gaps_leaves_beta_unknown() {
        let p = gaps("# Gaps\n\nWe track these in the issue tracker now.\n");
        assert!(p.gaps.is_empty());
        assert!(
            p.gap_totals.beta_unknown,
            "no gap read is not zero blockers"
        );
        assert_eq!(p.gap_totals.beta, None);
    }

    #[test]
    fn prose_says_which_gap_decides_beta() {
        let p = gaps("Entries below stand between alpha and beta (gap 1 decides beta).\n\n| # | Gap | Estimate |\n|---|---|---|\n| 1 | **A** | 10 |\n| 2 | **B** | 5 |\n");
        assert_eq!(p.gaps[0].beta, Flag::Yes);
        assert_eq!(p.gaps[0].blocks_src.as_deref(), Some("prose"));
        assert_eq!(
            p.gaps[1].beta,
            Flag::Unknown,
            "the sentence names gap 1 only"
        );
        // The same sentence in the revision history says nothing about today.
        let p = gaps("| # | Gap | Estimate |\n|---|---|---|\n| 1 | **A** | 10 |\n| 2 | **B** | 5 |\n\n## Revision history\n\n- Gap 1 decides beta.\n");
        assert_eq!(p.gaps[0].beta, Flag::Unknown);
    }

    #[test]
    fn numbering_notes() {
        let p = gaps("| # | Gap |\n|---|---|\n| 1 | a |\n| 3 | b |\n| 4 | c |\n| 2 | d |\n");
        assert!(p
            .notes
            .iter()
            .any(|n| n.text == "source numbering out of order"));
        let p = gaps("| # | Gap |\n|---|---|\n| 2 | a |\n| 3 | b |\n| 5 | c |\n");
        assert!(p.notes.iter().any(|n| n.text == "1 ids not listed"));
    }

    #[test]
    fn repeated_titles_get_distinct_keys() {
        let p = gaps("| # | Gap |\n|---|---|\n| 1 | Same thing |\n| 2 | Same thing |\n");
        assert_eq!(p.gaps[0].key, "same thing");
        assert_eq!(p.gaps[1].key, "same thing#2");
    }

    #[test]
    fn letter_headings_never_link_numeric_rows() {
        let p = gaps("| # | Gap |\n|---|---|\n| 1 | Thing |\n\n### G1. Thing in detail\n- **Missing:** x\n\n### 1. Thing\n- **Missing:** the real one\n");
        assert_eq!(p.gaps[0].detail_src.as_ref().map(|s| s.line), Some(8));
        assert_eq!(p.gaps[0].detail.as_deref(), Some("the real one"));
    }

    #[test]
    fn a_top_list_and_its_sections_count_once() {
        let md = "## Top 10 across everything\n| # | Gap | Est. |\n|---|---|---|\n\
            | 1 | **No xrefs or block editor** (#12) | 80–120 |\n| 2 | **No autosave** | 10–15 |\n\n\
            ## Features\n| # | Gap | Est. |\n|---|---|---|\n| F1 | No xrefs (XATTACH, #12) | 80–120 |\n| F2 | Missing hatch editing | 4–6 |\n";
        let p = gaps(md);
        let ids: Vec<&str> = p.gaps.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "F2"]);
        assert_eq!(p.gaps[0].paired_with, ["F1"]);
        assert_eq!(p.gaps[0].kinds, ["feature"], "from the paired Features row");
        assert_eq!(p.gaps[2].kinds, ["feature"]);
        assert_eq!(p.gaps[2].group.as_deref(), Some("Features"));
    }

    #[test]
    fn a_paired_section_row_keeps_its_blocker_flag() {
        let md = "## Top 10 across everything\n| # | Gap | Est. |\n|---|---|---|\n\
            | 1 | **No toolkit** (#392) | 40–60 |\n| 2 | **No autosave** | 10–15 |\n\n\
            ## UI\n| # | Gap | Est. |\n|---|---|---|\n| U3 | **Alpha blocker.** Toolkit missing (#392) | 40–60 |\n";
        let p = gaps(md);
        assert_eq!(p.gaps.len(), 2);
        assert_eq!(p.gaps[0].paired_with, ["U3"]);
        assert_eq!(p.gaps[0].alpha, Flag::Yes, "from the paired UI row");
        assert_eq!(p.gaps[1].alpha, Flag::No);
        assert_eq!(p.gap_totals.alpha, 1);
    }

    #[test]
    fn a_paired_section_row_lends_its_counted_in_estimate() {
        let md = "## Top 10 across everything\n| # | Gap | Est. |\n|---|---|---|\n\
            | 1 | **No toolkit** (#392) | |\n| 2 | **No autosave** | 10–15 |\n\n\
            ## UI\n| # | Gap | Est. |\n|---|---|---|\n| U3 | Toolkit missing (#392) | 40–60 (in 2) |\n";
        let p = gaps(md);
        assert_eq!(p.gaps[0].paired_with, ["U3"]);
        assert_eq!(p.gaps[0].hours.get().map(|h| (h.lo, h.hi)), Some((40, 60)));
        assert_eq!(p.gaps[0].counted_in.as_deref(), Some("2"));
        assert_eq!(p.gaps[1].counted_in, None);
    }

    #[test]
    fn form_b_headings_with_a_wrapped_estimate() {
        let md = "Kind: **F** feature · **L** localization ·\n**P** performance.\n\n## Blocking (beta can't be declared)\n\n\
            ### 1. Basic editing fails (F, P)\n- **Missing:** paste\n- It is slow. **Estimate:**\n  30–50 h (reproduce).\n\n\
            ## Lower\n\n### 2. No localization (L) · **ALPHA BLOCKER**\n- See [notes](notes.md). **Estimate:** 5–7 h. [loc-parity.md](loc-parity.md#scripts)\n\n\
            ### 3. Mobile (platform)\n- owner decision\n- **Doc:** [platform-parity.md](platform-parity.md)\n- **Estimate:** see [notes](notes.md)\n";
        let p = gaps(md);
        assert_eq!(p.gap_totals.form, "headings");
        let g = &p.gaps;
        assert_eq!(
            (g[0].id.as_str(), g[0].title.as_str()),
            ("1", "Basic editing fails")
        );
        assert_eq!(g[0].kinds, ["feature", "performance"]);
        assert_eq!(g[0].beta, Flag::Yes);
        assert_eq!(g[0].hours.get().map(|h| (h.lo, h.hi)), Some((30, 50)));
        assert_eq!(g[0].detail.as_deref(), Some("paste"));
        assert_eq!(
            (
                g[1].alpha.clone(),
                g[1].beta.clone(),
                g[1].priority.as_deref()
            ),
            (Flag::Yes, Flag::No, Some("Lower"))
        );
        assert_eq!(g[1].hours.get().map(|h| (h.lo, h.hi)), Some((5, 7)));
        let doc = g[1]
            .doc
            .as_ref()
            .map(|l| (l.text.as_str(), l.path.as_deref()));
        assert_eq!(doc, Some(("loc-parity.md", Some("docs/loc-parity.md"))));
        assert_eq!(g[0].doc, None);
        let doc = g[2].doc.as_ref().and_then(|l| l.path.as_deref());
        assert_eq!(doc, Some("docs/platform-parity.md"), "a Doc item first");
        assert_eq!(g[2].kinds, ["platforms"]);
        assert!(matches!(g[2].hours, Cell::Unreadable { .. }));
    }

    #[test]
    fn form_b_estimates_counted_in_another_gap() {
        let md = "## Blocking\n\n### 1. Toolkit missing\n- **Estimate:** 40–60 h\n\n\
            ### 2. Toolkit menus\n- **Estimate:** 10–20 h (in 1)\n";
        let p = gaps(md);
        assert_eq!(p.gaps[0].counted_in, None);
        assert_eq!(p.gaps[1].counted_in.as_deref(), Some("1"));
        assert_eq!(
            p.gap_totals.beta.map(|b| (b.count, b.lo, b.hi)),
            Some((1, 40, 60))
        );
    }

    #[test]
    fn non_blocking_and_closed_headings_do_not_block_beta() {
        let md = "## Blocking\n\n### 1. Saving drops layers\n- **Estimate:** 10–20 h\n\n\
            ## Non-blocking\n\n### 2. No plugin store\n- **Estimate:** 5 h\n\n\
            ## After beta (not blocking)\n\n### 3. No cloud sync\n- **Estimate:** 8 h\n\n\
            ## Closed\n\n### 4. Crash on open\n- **Estimate:** 3 h\n";
        let p = gaps(md);
        let flags: Vec<(&str, Flag)> = p
            .gaps
            .iter()
            .map(|g| (g.id.as_str(), g.beta.clone()))
            .collect();
        assert_eq!(flags, [("1", Flag::Yes), ("2", Flag::No), ("3", Flag::No)]);
        assert_eq!(p.gaps[1].priority.as_deref(), Some("Non-blocking"));
        assert_eq!(
            p.gap_totals.beta.map(|b| (b.count, b.lo, b.hi)),
            Some((1, 10, 20))
        );
    }

    #[test]
    fn unranked_headings_bullets_and_item_paragraphs() {
        let md = "| # | Gap | Beta | Hours |\n|---|---|---|---|\n| 1 | No native macOS menu bar | B | 3–5 |\n| 2 | Close discards edits (#60) | B | 1 |\n\n\
            ## Stability\n\n### Close discards an edited presentation (#60)\n- **Estimate:** 0.5–1 h. **Beta blocker.**\n\n\
            ### No soak testing\n- **Impact:** unknown.\n- **Estimate:** 4–8 h.\n\n\
            ## UI/UX\n\n- **No native macOS menu bar.** Nothing installs it. 3–5 h. **Beta blocker.**\n- **Window chrome** (#71). 2–4 h.\n- **Evidence:** not a gap.\n\n\
            ## Features\n\n### Smaller missing rows\nZoom (summary/section/slide, 6–10 h), reuse slides (2–4 h), laser pointer (1–2 h).\n";
        let p = gaps(md);
        let titles: Vec<&str> = p.gaps.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "No native macOS menu bar",
                "Close discards edits",
                "No soak testing",
                "Window chrome",
                "Zoom",
                "reuse slides",
                "laser pointer"
            ]
        );
        assert_eq!(p.gaps[1].detail_src.as_ref().map(|s| s.line), Some(8));
        assert_eq!(p.gaps[2].hours.get().map(|h| (h.lo, h.hi)), Some((4, 8)));
        assert_eq!(p.gaps[2].beta, Flag::No);
        assert_eq!(p.gaps[4].hours.get().map(|h| (h.lo, h.hi)), Some((6, 10)));
    }

    #[test]
    fn unranked_bullets_and_headings_keep_alpha_markers_and_counted_in() {
        let md = "| # | Gap | Hours |\n|---|---|---|\n| 1 | No toolkit | 40–60 |\n| 2 | Window chrome | 2–4 |\n\n\
            ## UI/UX\n\n- **Autosave missing** 3–5 h. **Alpha blocker.**\n- **Window chrome** (#71). 2–4 h. **Alpha blocker.**\n\n\
            ## Features\n\n### Context menus incomplete\n- **Estimate:** 10–20 h (in 1).\n";
        let p = gaps(md);
        let rows: Vec<(&str, Flag, Option<&str>)> = p
            .gaps
            .iter()
            .map(|g| (g.title.as_str(), g.alpha.clone(), g.counted_in.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [
                ("No toolkit", Flag::No, None),
                ("Window chrome", Flag::Yes, None),
                ("Autosave missing", Flag::Yes, None),
                ("Context menus incomplete", Flag::No, Some("1")),
            ]
        );
        assert_eq!(p.gap_totals.alpha, 2);
    }

    #[test]
    fn a_detail_heading_keeps_its_blocker_marker() {
        let md = "| # | Gap | Hours |\n|---|---|---|\n| 1 | No toolkit (#392) | 40–60 |\n| 2 | No autosave | 10 |\n\n\
            ## UI\n\n### Toolkit missing (#392)\n- **Estimate:** 40–60 h. **Beta blocker.** **Alpha blocker.**\n";
        let p = gaps(md);
        assert_eq!(p.gaps.len(), 2, "the heading is gap 1's detail");
        assert_eq!(p.gaps[0].detail_src.as_ref().map(|s| s.line), Some(8));
        assert_eq!(
            (p.gaps[0].alpha.clone(), p.gaps[0].beta.clone()),
            (Flag::Yes, Flag::Yes)
        );
        assert_eq!(p.gaps[1].beta, Flag::No);
        assert_eq!(
            p.gap_totals.beta.map(|b| (b.count, b.lo, b.hi)),
            Some((1, 40, 60))
        );
    }

    #[test]
    fn a_by_kind_list_fills_kinds() {
        let md = "| # | Gap |\n|---|---|\n| 1 | a |\n| 2 | b |\n\n## Gaps by kind\n\n- **Feature:** 1\n- **UI / UX:** 2\n- **Stability and platforms:** 2\n";
        let p = gaps(md);
        assert_eq!(p.gaps[0].kinds, ["feature"]);
        assert_eq!(p.gaps[1].kinds, ["ui/ux", "stability", "platforms"]);
    }

    #[test]
    fn beta_sums_count_partial_rows_apart() {
        let md = "| # | Gap | Beta | Hours |\n|---|---|---|---|\n| 1 | a | B | 10–20 |\n| 2 | b | B (pane) | 5–8 |\n| 3 | c | yes (part) | 4–6 |\n| 4 | d | | 1 |\n\n\
            Beta-blocking gaps (B) total **~15–28 h**.\n";
        let p = gaps(md);
        assert_eq!(
            p.gap_totals.beta.map(|b| (b.count, b.lo, b.hi)),
            Some((2, 15, 28))
        );
        assert_eq!(p.gap_totals.partial.map(|b| (b.count, b.hi)), Some((1, 6)));
        assert_eq!(
            p.gap_totals.stated.get().map(|h| (h.lo, h.hi)),
            Some((15, 28))
        );
        assert_eq!(p.gaps[1].scope_note.as_deref(), Some("pane"));
    }
}
