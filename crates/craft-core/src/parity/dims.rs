//! The dimensions of "ready for real work": weights from the weight table
//! (W), scores, hours and docs from the By-dimension table (D), joined by
//! name, and group weights (G) when there is no weight table.

use std::sync::LazyLock;

use regex::Regex;

use super::md::{self, contains_word, Doc, Table};
use super::value;
use super::{Canonical, Cell, Dim, Drag, Fills, Heat, Hours, Link, Note, Parity, Score, Src};

static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+(?:\.\d+)?)\s?(%)?").unwrap());

const READY: &[&str] = &["ready", "% ready", "%", "parity", "ready for real work"];
const HOURS: &[&str] = &[
    "hours",
    "hours to full",
    "hours to parity",
    "remaining (h)",
    "remaining (agent-h)",
    "remaining (opus 5.5 h)",
    "one agent (h)",
    "remaining",
];
const SUMMARY: &[&str] = &[
    "total",
    "weighted",
    "weighted sum",
    "ready for real work",
    "result",
];

/// The group a dimension (or area) name maps to: keywords matched as word
/// prefixes on the name before ` (` or `:`, first hit wins.
pub fn canonical(name: &str) -> Canonical {
    let lower = md::normalize(name);
    let cut = lower
        .split(" (")
        .next()
        .unwrap_or(&lower)
        .split(':')
        .next()
        .unwrap_or(&lower)
        .trim()
        .to_string();
    let words: Vec<&str> = cut
        .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '.'))
        .filter(|w| !w.is_empty())
        .collect();
    let prefix = |k: &str| {
        if k.contains(' ') {
            contains_word(&cut, k)
        } else {
            words.iter().any(|w| w.starts_with(k))
        }
    };
    let word = |k: &str| words.contains(&k);
    let rules: &[(&[&str], &[&str], Canonical)] = &[
        (
            &["ecosystem", "plug-in", "plugin", "add-in"],
            &[],
            Canonical::Ecosystem,
        ),
        (
            &["agent", "automation", "mcp"],
            &[],
            Canonical::AgentControl,
        ),
        (
            &["collab", "multiplayer", "sharing"],
            &[],
            Canonical::Collaboration,
        ),
        (&["specification", "standards"], &[], Canonical::Standards),
        (&[], &["ai"], Canonical::Ai),
        (&["locali", "language"], &[], Canonical::Localization),
        (&["perform"], &[], Canonical::Performance),
        (&["stabil", "reliab", "crash"], &[], Canonical::Stability),
        (&["platform"], &[], Canonical::Platforms),
        (&["hardware", "gpu"], &["pen"], Canonical::Hardware),
        (
            &[
                "codec",
                "raw",
                "decod",
                "camera coverage",
                "file",
                "format",
                "opening existing",
            ],
            &[],
            Canonical::FileFormats,
        ),
        (
            &["typograph", "layout", "paginat"],
            &[],
            Canonical::TypeLayout,
        ),
        (
            &["interaction", "behaves like"],
            &["ui", "ux"],
            Canonical::UiUx,
        ),
        (&["feature", "breadth", "depth"], &[], Canonical::Features),
    ];
    for (prefixes, words_, c) in rules {
        if prefixes.iter().any(|k| prefix(k)) || words_.iter().any(|k| word(k)) {
            return *c;
        }
    }
    Canonical::Other
}

/// A weight cell: `(weight, unweighted)`. `%` makes it a share and an
/// integer makes it points; both sum the same way. A blank, `—` or "not
/// weighted" cell is unweighted.
pub fn weight(cell: &str) -> (Option<f64>, bool) {
    let t = md::plain(cell).to_lowercase();
    if t.contains("not weighted") {
        return (None, true);
    }
    match NUMBER.captures(&t) {
        Some(c) => (c[1].parse().ok(), false),
        None => (None, t.trim().is_empty() || t.trim() == "—"),
    }
}

/// The first `dimension` (then `component`) table with a Weight column.
fn weight_table(d: &Doc) -> Option<&Table> {
    ["dimension", "component"].iter().find_map(|first| {
        d.live_tables()
            .find(|t| t.first_header() == *first && t.col(&["weight"]).is_some())
    })
}

/// The first `dimension` table under a heading starting with "by dimension"
/// or "dimensions".
fn dim_table(d: &Doc) -> Option<&Table> {
    d.live_tables().find(|t| {
        t.first_header() == "dimension" && d.under(t.section, &["by dimension", "dimensions"])
    })
}

/// A `group | weight` table under a heading starting with "weights".
fn group_table(d: &Doc) -> Option<&Table> {
    d.live_tables().find(|t| {
        t.first_header() == "group"
            && t.col(&["weight"]).is_some()
            && d.under(t.section, &["weights"])
    })
}

fn is_summary(cell: &md::MdCell) -> bool {
    let bold = cell.raw.trim_start().starts_with("**") || cell.raw.trim_start().starts_with("__");
    let t = cell.text.to_lowercase();
    bold && SUMMARY.iter().any(|s| t.starts_with(s))
}

/// The ready column of a table: the Ready aliases, and in a weight table
/// also Value and Result.
fn ready_col(t: &Table, weights: bool) -> Option<usize> {
    let mut aliases = READY.to_vec();
    if weights {
        aliases.extend(["value", "result"]);
    }
    t.col(&aliases)
}

fn hours_col(t: &Table) -> Option<usize> {
    let explicit = t.col(&HOURS[..HOURS.len() - 1]);
    explicit.or_else(|| {
        let c = t.col(&["remaining"])?;
        let cells: Vec<&md::MdCell> = t.rows.iter().filter_map(|r| r.cells.get(c)).collect();
        let parsed = cells
            .iter()
            .filter(|c| value::hours(&c.raw, "", "").is_ok())
            .count();
        (parsed * 2 >= cells.len().max(1)).then_some(c)
    })
}

/// A score cell, or why not.
pub fn score_cell(d: &Doc, c: Option<&md::MdCell>, line: u32, what: &str) -> Cell<Score> {
    let Some(c) = c else {
        return Cell::absent(format!("No {what} column in {}", d.path));
    };
    if c.is_empty() || c.text.trim() == "—" {
        return Cell::absent(format!("{what} cell is empty ({}:{line})", d.path));
    }
    match value::score(&c.raw) {
        Some(s) => Cell::ok(s, d.src(line)),
        None => Cell::Unreadable {
            why: format!("{what} cell is not a percentage"),
            raw: c.text.clone(),
            src: d.src(line),
        },
    }
}

pub fn hours_cell(
    d: &Doc,
    c: Option<&md::MdCell>,
    line: u32,
    label: &str,
    header: &str,
) -> Cell<Hours> {
    let Some(c) = c else {
        return Cell::absent(format!("No hours column in {}", d.path));
    };
    match value::hours(&c.raw, label, header) {
        Ok(h) => Cell::ok(h, d.src(line)),
        Err(why) => Cell::absent(why),
    }
}

/// The first link of a doc cell, resolved against `d`'s directory.
pub fn doc_link(d: &Doc, c: Option<&md::MdCell>) -> Option<Link> {
    let c = c?;
    if let Some((text, url)) = c.links.first() {
        return Some(Link {
            text: text.clone(),
            url: url.clone(),
            path: resolve(&d.path, url),
        });
    }
    (!c.is_empty() && c.text != "—").then(|| Link {
        text: c.text.clone(),
        url: String::new(),
        path: (c.text == "this file").then(|| d.path.clone()),
    })
}

/// The in-repo path a relative link from `from` points at, without its
/// anchor; `None` for an absolute URL.
pub fn resolve(from: &str, url: &str) -> Option<String> {
    if url.contains("://") || url.starts_with("mailto:") {
        return None;
    }
    let (path, _) = url.split_once('#').unwrap_or((url, ""));
    if path.is_empty() {
        return Some(from.to_string());
    }
    let mut parts: Vec<&str> = if path.starts_with('/') {
        Vec::new()
    } else {
        from.split('/').collect::<Vec<_>>()
    };
    parts.pop();
    for seg in path.trim_start_matches('/').split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// A row of W or D, before joining.
struct Raw {
    name: String,
    key: String,
    group: Canonical,
    fills: Fills,
    weight: Option<f64>,
    unweighted: bool,
    ready: Cell<Score>,
    breadth: Cell<Score>,
    hours: Cell<Hours>,
    kind: String,
    doc: Option<Link>,
    counted: Option<String>,
    child: bool,
    src: Src,
}

/// Which score a row fills when its group has several rows in one table:
/// a name that says breadth (and not ready or depth) fills the breadth.
fn fills(name: &str) -> Fills {
    let n = md::cut_paren(&name.to_lowercase()).to_string();
    if n.contains("breadth") && !n.contains("ready") && !n.contains("depth") {
        Fills::Breadth
    } else {
        Fills::Ready
    }
}

/// Rows alone in their group fill the ready score whatever their name says.
fn assign_fills(rows: &mut [Raw]) {
    let groups: Vec<Canonical> = rows.iter().filter(|r| !r.child).map(|r| r.group).collect();
    for r in rows.iter_mut() {
        if groups.iter().filter(|g| **g == r.group).count() < 2 {
            r.fills = Fills::Ready;
        }
    }
}

/// The name a row joins by: lowercase, before ` (` or `:`.
fn join_key(name: &str) -> String {
    let n = md::normalize(name);
    let n = n.trim_start_matches('↳').trim();
    let n = n.split(" (").next().unwrap_or(n);
    n.split(':').next().unwrap_or(n).trim().to_string()
}

/// The rows of `t` and its summary, `(rows, total)`.
fn rows(d: &Doc, t: &Table, weights: bool) -> (Vec<Raw>, Option<Cell<Score>>) {
    let ready = ready_col(t, weights);
    let breadth = t.col(&["breadth"]);
    let wcol = t.col(&["weight"]);
    let hours = hours_col(t);
    let kind = t.col(&["kind"]);
    let counted = t.col(&["counted"]);
    let doc = t.col(&["doc", "detail"]);
    let mut out = Vec::new();
    let mut total = None;
    for r in &t.rows {
        let Some(first) = r.cells.first() else {
            continue;
        };
        if r.malformed {
            continue;
        }
        if is_summary(first) {
            if total.is_none() {
                total = Some(summary_score(d, t, r, ready));
            }
            continue;
        }
        let name = first.text.trim_start_matches('↳').trim().to_string();
        let (w, unweighted) = wcol
            .and_then(|c| r.cells.get(c))
            .map_or((None, false), |c| weight(&c.raw));
        let header = hours.map(|h| t.header[h].clone()).unwrap_or_default();
        out.push(Raw {
            group: canonical(&name),
            fills: fills(&name),
            key: join_key(&name),
            weight: w,
            unweighted,
            ready: score_cell(d, t.cell(r, ready), r.line, "ready"),
            breadth: if breadth.is_some() {
                score_cell(d, t.cell(r, breadth), r.line, "breadth")
            } else {
                Cell::absent(format!("No breadth column in {}", d.path))
            },
            hours: if hours.is_some() {
                hours_cell(d, t.cell(r, hours), r.line, &name, &header)
            } else {
                Cell::absent(format!("No hours column in {}", d.path))
            },
            kind: t.cell(r, kind).map(|c| c.text.clone()).unwrap_or_default(),
            doc: doc_link(d, t.cell(r, doc)),
            counted: t
                .cell(r, counted)
                .map(|c| c.text.clone())
                .filter(|c| !c.is_empty()),
            child: first.text.trim_start().starts_with('↳'),
            src: d.src(r.line),
            name,
        });
    }
    if total.is_none() {
        total = prose_total(d, t);
    }
    assign_fills(&mut out);
    (out, total)
}

/// A summary row's score: its ready cell, or when that is empty the first
/// later cell with a percentage (the total often sits under Contribution).
pub fn summary_score(d: &Doc, t: &Table, r: &md::Row, ready: Option<usize>) -> Cell<Score> {
    let filled = t.cell(r, ready).is_some_and(|c| !c.is_empty());
    if filled {
        return score_cell(d, t.cell(r, ready), r.line, "total");
    }
    match r
        .cells
        .iter()
        .skip(1)
        .find(|c| value::pct(&c.raw).is_some_and(|p| !p.computed))
    {
        Some(c) => score_cell(d, Some(c), r.line, "total"),
        None => score_cell(d, t.cell(r, ready), r.line, "total"),
    }
}

/// The first bold `~N%` in the paragraph right after table `t`.
pub fn prose_total(d: &Doc, t: &Table) -> Option<Cell<Score>> {
    let b = d.blocks_between(t.end, d.lines.len()).into_iter().next()?;
    // Only the paragraph directly after the table.
    let gap = d.lines[t.end..]
        .iter()
        .take_while(|l| l.no < b.line)
        .all(|l| l.text.trim().is_empty());
    if !gap {
        return None;
    }
    let bold = md::spans(&b.text, "**");
    bold.iter().find_map(|s| {
        let p = value::pct(s).filter(|p| !p.computed)?;
        Some(Cell::ok(Score::Pct(p), d.src(b.line)).from("from prose"))
    })
}

/// Fills the dimension fields of `out`.
pub fn read(out: &mut Parity, rm: Option<&Doc>, tap: Option<&Doc>) {
    fn pick(d: Option<&Doc>) -> Option<&Doc> {
        d.filter(|d| weight_table(d).is_some() || dim_table(d).is_some())
    }
    let Some(d) = pick(tap).or_else(|| pick(rm)) else {
        out.dims_total = Cell::absent(
            "No weight or By-dimension table in docs/target-app-parity.md or ROADMAP.md",
        );
        out.top_drag = Cell::absent("no dimension weights in source");
        return;
    };
    let w = weight_table(d);
    let dt = dim_table(d).filter(|t| w.is_none_or(|w| w.line != t.line));
    let (wrows, wtotal) = w.map_or((Vec::new(), None), |t| rows(d, t, true));
    let (drows, dtotal) = dt.map_or((Vec::new(), None), |t| rows(d, t, false));
    let mut dims: Vec<Dim> = Vec::new();
    let mut used_w = vec![false; wrows.len()];
    // The groups of D's ready rows: a join by group needs one on each side.
    let d_groups: Vec<Canonical> = drows
        .iter()
        .filter(|x| !x.child && x.fills == Fills::Ready)
        .map(|x| x.group)
        .collect();
    for r in drows {
        let ready_rows = |rs: &[Raw], g: Canonical| {
            rs.iter()
                .enumerate()
                .filter(|(_, x)| x.group == g && !x.child && x.fills == Fills::Ready)
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        };
        let by_name = wrows
            .iter()
            .position(|x| x.key == r.key)
            .filter(|i| !used_w[*i]);
        let by_dim = (!r.child && r.fills == Fills::Ready)
            .then(|| {
                let ws = ready_rows(&wrows, r.group);
                let ds = d_groups.iter().filter(|g| **g == r.group).count();
                (ws.len() == 1 && ds == 1 && !used_w[ws[0]]).then(|| ws[0])
            })
            .flatten();
        let mut dim = Dim::from_raw(r);
        match by_name.or(by_dim) {
            Some(i) => {
                used_w[i] = true;
                let x = &wrows[i];
                dim.weight = x.weight;
                if !dim.ready.is_ok() && x.ready.is_ok() {
                    dim.ready = x.ready.clone();
                }
                if x.unweighted {
                    dim.weight = None;
                }
            }
            None => {
                if w.is_some() {
                    dim.only_in = Some("d".into());
                }
            }
        }
        dims.push(dim);
    }
    for (i, x) in wrows.into_iter().enumerate() {
        if used_w[i] {
            continue;
        }
        let mut dim = Dim::from_raw(x);
        if dt.is_some() {
            dim.only_in = Some("w".into());
        }
        dims.push(dim);
    }
    let src_label = |t: &Table| format!("{}:{}", d.path, t.line);
    out.dims_total = wtotal.or(dtotal).unwrap_or_else(|| {
        Cell::absent(format!(
            "No total row under the dimension table in {}",
            d.path
        ))
    });
    if w.is_none() {
        if let Some(g) = group_table(d) {
            group_weights(d, g, &mut dims);
            for dim in &mut dims {
                if dim.weight.is_some() {
                    dim.weight_src =
                        Some(format!("weights from ## Weights groups, {}", src_label(g)));
                }
            }
        }
    }
    shortfalls(out, d, dims);
}

impl Dim {
    fn from_raw(r: Raw) -> Dim {
        Dim {
            name: r.name,
            group: r.group,
            weight: if r.unweighted { None } else { r.weight },
            weight_src: None,
            ready: r.ready,
            breadth: r.breadth,
            hours: r.hours,
            kind: r.kind,
            doc: r.doc,
            counted: r.counted,
            child: r.child,
            fills: r.fills,
            only_in: None,
            src: r.src,
            shortfall: None,
        }
    }
}

/// Weights from a `## Weights` group table: cross-cutting rows by their
/// dimension, and the feature groups summed into Features.
fn group_weights(d: &Doc, g: &Table, dims: &mut [Dim]) {
    let wcol = g.col(&["weight"]);
    let mut cross = false;
    let mut features = 0.0;
    let mut of_features = false;
    let mut cross_sum = 0.0;
    let mut by_group: Vec<(Canonical, f64)> = Vec::new();
    for r in &g.rows {
        let Some(name) = r.cells.first() else {
            continue;
        };
        let wcell = g
            .cell(r, wcol)
            .map(|c| c.text.to_lowercase())
            .unwrap_or_default();
        let Some(w) = weight(&wcell).0 else { continue };
        let lower = name.text.to_lowercase();
        if lower.contains("cross-cutting") {
            cross = true;
        }
        if !cross {
            of_features |= wcell.contains("of features");
            let c = canonical(&name.text);
            if matches!(c, Canonical::Features | Canonical::Other) || of_features {
                features += w;
            } else {
                by_group.push((c, w));
            }
            continue;
        }
        let label = lower.replace("cross-cutting:", "").replace("*", "");
        cross_sum += w;
        by_group.push((canonical(&label), w));
    }
    let features = if of_features && cross {
        100.0 - cross_sum
    } else {
        features
    };
    by_group.push((Canonical::Features, features));
    for (c, w) in by_group {
        let rows: Vec<usize> = dims
            .iter()
            .enumerate()
            .filter(|(_, x)| x.group == c && !x.child && x.fills == Fills::Ready)
            .map(|(i, _)| i)
            .collect();
        if let Some(&i) = rows.first() {
            dims[i].weight = Some(dims[i].weight.unwrap_or(0.0) + w);
        }
    }
    let _ = d;
}

/// Shortfalls, the top drag and the check of the stated total.
fn shortfalls(out: &mut Parity, d: &Doc, mut dims: Vec<Dim>) {
    let weighted: Vec<usize> = dims
        .iter()
        .enumerate()
        .filter(|(_, x)| !x.child && x.weight.is_some_and(|w| w > 0.0))
        .map(|(i, _)| i)
        .collect();
    let sum: f64 = weighted.iter().filter_map(|&i| dims[i].weight).sum();
    if sum > 0.0 {
        for &i in &weighted {
            let x = &mut dims[i];
            let ready = match x.fills {
                Fills::Ready => x.ready.get().and_then(Score::pct),
                Fills::Breadth => None,
            };
            if let (Some(w), Some(r)) = (x.weight, ready) {
                x.shortfall = Some(w / sum * (100.0 - r));
            }
        }
    }
    let top = dims
        .iter()
        .filter(|x| x.shortfall.is_some())
        .max_by(|a, b| {
            a.shortfall
                .partial_cmp(&b.shortfall)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    out.top_drag = match top {
        Some(x) => Cell::ok(
            Drag {
                name: x.name.clone(),
                group: x.group,
                points: (x.shortfall.unwrap_or(0.0) * 100.0).round() / 100.0,
            },
            x.src.clone(),
        ),
        None => Cell::absent("no dimension weights in source"),
    };
    if let Some(stated) = out.dims_total.get().and_then(Score::pct) {
        let (mut num, mut den) = (0.0, 0.0);
        for &i in &weighted {
            if let (Some(w), Some(r)) = (dims[i].weight, dims[i].ready.get().and_then(Score::pct)) {
                num += w * r;
                den += w;
            }
        }
        if den > 0.0 && (num / den - stated).abs() > 1.0 {
            out.notes.push(Note::warn(
                format!(
                    "weighted dimensions give {:.1}%; the stated total is {}%",
                    num / den,
                    stated
                ),
                out.dims_total.src().cloned().unwrap_or_else(|| d.src(1)),
            ));
        }
    }
    out.heat = heat(&dims);
    out.dims = dims;
}

/// The overview's heat cells. A column with one ready row shows it; with
/// several, their weighted mean (the plain mean without weights). The Other
/// column shows its row with the largest shortfall, or "ahead" when every
/// row is ahead.
fn heat(dims: &[Dim]) -> Vec<Heat> {
    let mut cols: Vec<Canonical> = dims.iter().map(|d| d.group.column()).collect();
    cols.sort();
    cols.dedup();
    let mut out = Vec::new();
    for col in cols {
        let rows: Vec<&Dim> = dims
            .iter()
            .filter(|d| d.group.column() == col && !d.child && d.fills == Fills::Ready)
            .collect();
        let names = rows
            .iter()
            .map(|d| format!("{}: {}", d.name, super::score_text(&d.ready)))
            .collect::<Vec<_>>();
        let src = rows.first().map(|d| d.src.clone());
        let scored: Vec<(&Dim, f64)> = rows
            .iter()
            .filter_map(|d| Some((*d, d.ready.get()?.pct()?)))
            .collect();
        let ahead = !rows.is_empty()
            && rows
                .iter()
                .all(|d| matches!(d.ready.get(), Some(Score::Ahead { .. })));
        let (value, text, src) = if ahead {
            (None, "ahead".to_string(), src)
        } else if col == Canonical::Other {
            let worst = scored.iter().max_by(|a, b| {
                let key = |x: &(&Dim, f64)| x.0.shortfall.unwrap_or(100.0 - x.1);
                key(a)
                    .partial_cmp(&key(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            match worst {
                Some((d, v)) => (
                    Some(*v),
                    format!("{} {}", md::cut_paren(&d.name), super::fmt_pct(*v)),
                    Some(d.src.clone()),
                ),
                None => (None, "—".to_string(), src),
            }
        } else if scored.is_empty() {
            (None, "—".to_string(), src)
        } else {
            let wsum: f64 = scored.iter().filter_map(|(d, _)| d.weight).sum();
            let v = if wsum > 0.0 && scored.iter().all(|(d, _)| d.weight.is_some()) {
                scored
                    .iter()
                    .map(|(d, v)| d.weight.unwrap_or(0.0) * v)
                    .sum::<f64>()
                    / wsum
            } else {
                scored.iter().map(|(_, v)| v).sum::<f64>() / scored.len() as f64
            };
            let v = (v * 10.0).round() / 10.0;
            (Some(v), super::fmt_pct(v), src)
        };
        out.push(Heat {
            column: col,
            value,
            text,
            rows: names,
            src,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{read as read_all, Input, PARITY_DOC};
    use super::*;

    fn tap(text: &str) -> Parity {
        read_all(&[Input {
            path: PARITY_DOC,
            oid: "t",
            text,
        }])
    }

    fn by_name<'a>(p: &'a Parity, name: &str) -> &'a Dim {
        p.dims
            .iter()
            .find(|d| d.name.starts_with(name))
            .unwrap_or_else(|| panic!("no {name}"))
    }

    #[test]
    fn weight_and_dimension_tables_join() {
        let md = "# P\n## Ready for real work\n\
            | Dimension | Weight | Value | Contribution |\n|---|---|---|---|\n\
            | Features (depth) | 60 | 70% | 42 |\n| Stability | 30 | 50% | 15 |\n| Hardware | 10 | 40% | 4 |\n\
            | **Ready for real work** | **100** | | **~61%** |\n\n\
            ## By dimension\n| Dimension | % | One agent (h) | Doc |\n|---|---|---|---|\n\
            | Features (depth, weighted) | 75% | 220–360 | this file |\n| Stability | ~60% | 30–50 | [gaps.md](gaps.md#g4) |\n\
            | Agent automation (MCP) | beyond the app | — | [mcp.md](mcp.md) |\n";
        let p = tap(md);
        assert_eq!(p.dims.len(), 4);
        let f = by_name(&p, "Features");
        assert_eq!(
            (f.weight, f.ready.get().and_then(Score::pct)),
            (Some(60.0), Some(75.0))
        );
        assert_eq!(
            f.hours.get().map(|h| (h.lo, h.hi, h.basis.as_deref())),
            Some((220, 360, Some("one agent")))
        );
        assert_eq!(
            f.doc.as_ref().and_then(|l| l.path.as_deref()),
            Some(PARITY_DOC)
        );
        let s = by_name(&p, "Stability");
        assert_eq!(
            s.doc.as_ref().and_then(|l| l.path.as_deref()),
            Some("docs/gaps.md")
        );
        let a = by_name(&p, "Agent");
        assert_eq!(
            (a.group, a.only_in.as_deref(), a.weight),
            (Canonical::AgentControl, Some("d"), None)
        );
        assert!(matches!(a.ready.get(), Some(Score::Ahead { .. })));
        let h = by_name(&p, "Hardware");
        assert_eq!((h.only_in.as_deref(), h.weight), (Some("w"), Some(10.0)));
        assert!(matches!(h.hours, Cell::Absent { .. }));
        assert_eq!(
            h.ready.get().and_then(Score::pct),
            Some(40.0),
            "W gives ready when D has none"
        );
        assert_eq!(p.dims_total.get().and_then(Score::pct), Some(61.0));
        // Features loses 60 × (100 − 75) / 100 = 15 points, Stability 12, Hardware 6.
        let top = p.top_drag.get().unwrap();
        assert_eq!(
            (top.name.as_str(), top.points),
            ("Features (depth, weighted)", 15.0)
        );
    }

    #[test]
    fn a_component_table_with_a_result_row() {
        let md = "# P\n## Ready for real work: how the number is built\n\
            | Component | Weight | Value | Source |\n|---|---|---|---|\n\
            | Feature depth | 60% | 50% | x |\n| Opening and delivering work files | 15% | 35% | y |\n\
            | Performance | 25% | 35% | z |\n| **Result** | | **46.2%**, reported as **≈ 45%** (40–50%) | |\n";
        let p = tap(md);
        assert_eq!(p.dims.len(), 3);
        assert_eq!(by_name(&p, "Opening").group, Canonical::FileFormats);
        assert!(p.dims_total.is_ok());
        assert_eq!(
            p.top_drag.get().map(|d| d.name.as_str()),
            Some("Feature depth")
        );
    }

    #[test]
    fn group_weights_fill_in_without_a_weight_table() {
        let md = "# P\n## Weights\n| Group | Weight |\n|---|---|\n\
            | Editing and timeline | 60% of features |\n| Panels and UI fidelity | 40% |\n\
            | *Cross-cutting:* stability | 20% of overall |\n| Platforms | 10% |\n\n\
            ## By dimension\n| Dimension | Ready | Hours |\n|---|---|---|\n\
            | Features (editing, panels) | ~66% | 350–620 |\n| UI / UX fidelity | ~60% | 120–200 |\n\
            | Stability | ~45% | 60–120 |\n| Platforms | ~60% | 50–90 |\n| **Total** | **~56%** | |\n";
        let p = tap(md);
        let f = by_name(&p, "Features");
        assert_eq!(f.weight, Some(70.0));
        assert_eq!(
            f.weight_src.as_deref(),
            Some("weights from ## Weights groups, docs/target-app-parity.md:3")
        );
        assert_eq!(by_name(&p, "Stability").weight, Some(20.0));
        assert_eq!(by_name(&p, "UI").weight, None, "UI is a feature group here");
        assert!(p.top_drag.is_ok());
    }

    #[test]
    fn canonical_mapping() {
        assert_eq!(canonical("Ecosystem and automation"), Canonical::Ecosystem);
        assert_eq!(canonical("Opening existing files"), Canonical::FileFormats);
        assert_eq!(
            canonical("Layout and pagination fidelity"),
            Canonical::TypeLayout
        );
        assert_eq!(
            canonical("RAW decoding and camera coverage"),
            Canonical::FileFormats
        );
        assert_eq!(
            canonical("Camera colour and default rendering"),
            Canonical::Other
        );
        assert_eq!(canonical("UI / UX fidelity"), Canonical::UiUx);
        assert_eq!(canonical("AI features (generative)"), Canonical::Ai);
        assert_eq!(
            canonical("Typography and composition fidelity"),
            Canonical::TypeLayout
        );
        assert_eq!(
            canonical("Agent automation (MCP, CLI)"),
            Canonical::AgentControl
        );
        assert_eq!(
            canonical("Hardware acceleration (decode, encode)"),
            Canonical::Hardware
        );
        assert_eq!(canonical("Painting"), Canonical::Other);
        assert_eq!(
            canonical("Features: the other 11 areas"),
            Canonical::Features
        );
        assert_eq!(canonical("Behaves like the app"), Canonical::UiUx);
        assert_eq!(canonical("Collaboration"), Canonical::Collaboration);
        assert_eq!(canonical("PDF specification"), Canonical::Standards);
    }

    #[test]
    fn breadth_and_ready_rows_on_one_dimension() {
        let md = "# P\n## By dimension\n| Dimension | % | Hours |\n|---|---|---|\n\
            | Features, breadth | 91% | (in features) |\n| Features, ready for real work | 50% | 360–660 |\n\
            ## Ready: how the number is built\n| Component | Weight | Value |\n|---|---|---|\n| Feature depth | 60% | 50% |\n| Stability | 40% | 45% |\n";
        let p = tap(md);
        let b = by_name(&p, "Features, breadth");
        assert_eq!(b.fills, Fills::Breadth);
        assert_eq!(b.weight, None);
        let r = by_name(&p, "Features, ready");
        assert_eq!(
            (r.fills, r.weight),
            (Fills::Ready, Some(60.0)),
            "joined by dimension"
        );
        assert_eq!(r.hours.get().map(|h| h.lo), Some(360));
        // A row alone in its group fills the ready score, whatever its name says.
        let md = "# P\n## By dimension\n| Dimension | Weight | Ready |\n|---|---|---|\n| Features, breadth | 30% | ~62% |\n| Stability | 70% | 50% |\n";
        let p = tap(md);
        let f = by_name(&p, "Features");
        assert_eq!(
            (f.fills, f.shortfall.map(|s| s.round())),
            (Fills::Ready, Some(11.0))
        );
    }

    #[test]
    fn a_total_read_from_prose() {
        let md = "# P\n## By dimension\n| Dimension | Weight | Parity |\n|---|---|---|\n| Features | 70% | 68% |\n| Stability | 30% | 50% |\n\n\
            **Ready for real work ≈ 0.70·68 + 0.30·50 ≈ 63%.**\n";
        let p = tap(md);
        assert!(matches!(&p.dims_total, Cell::Ok { from: Some(f), .. } if f == "from prose"));
        assert_eq!(p.dims_total.get().and_then(Score::pct), Some(63.0));
        assert!(
            !p.notes
                .iter()
                .any(|n| n.text.starts_with("weighted dimensions")),
            "62.6 is within a point"
        );
    }

    #[test]
    fn resolve_links() {
        assert_eq!(
            resolve("docs/target-app-parity.md", "ui-parity.md#x").as_deref(),
            Some("docs/ui-parity.md")
        );
        assert_eq!(
            resolve("docs/target-app-parity.md", "../ROADMAP.md").as_deref(),
            Some("ROADMAP.md")
        );
        assert_eq!(
            resolve("ROADMAP.md", "docs/gaps.md").as_deref(),
            Some("docs/gaps.md")
        );
        assert_eq!(
            resolve("ROADMAP.md", "#by-dimension").as_deref(),
            Some("ROADMAP.md")
        );
        assert_eq!(resolve("ROADMAP.md", "https://x.org/a"), None);
    }
}
