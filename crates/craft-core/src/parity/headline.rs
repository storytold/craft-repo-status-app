//! The headline numbers of `ROADMAP.md`, the stage line, each document's
//! dates, and the checks that compare one source with another.

use std::sync::LazyLock;

use regex::Regex;

use super::md::{self, plain, Doc, Table};
use super::value::{self, fraction};
use super::{
    Basis, Cell, Checklist, DocFile, Figure, Hours, Note, Parity, Src, Stage, StageStep, GAPS,
    README, ROADMAP,
};

static STAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\*\*Stage:\s*([a-z-]+)\.?\*\*(?:\s*·\s*next:\s*(.+))?").unwrap()
});
static POINTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"~\s?(\d+(?:\.\d+)?)\s?(?:points?\b|%)").unwrap());
static STAGE_HOURS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"~?\s?(\d{1,3}(?:,\d{3})+|\d+)\s?[–-]\s?(\d{1,3}(?:,\d{3})+|\d+)\s?(?:[A-Za-z0-9.]+\s){0,3}?(?:h\b|hours?\b|agent-hours\b)").unwrap()
});
static BADGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"alt="Status:\s*([^"]+)""#).unwrap());
static GENERATED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^>\s*\*\*Generated:\*\*").unwrap());
const STAGE_WORDS: &[&str] = &["pre-alpha", "alpha", "beta", "stable"];

/// One file's dates.
pub fn doc_file(d: &Doc, oid: &str) -> DocFile {
    let mut f = DocFile {
        path: d.path.clone(),
        oid: oid.to_string(),
        ..Default::default()
    };
    let lines = d.date_lines();
    f.date_lines = lines.len() as u32;
    let field = |fields: &[(String, String)], name: &str| {
        fields
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .and_then(|(_, v)| value::dates(v).into_iter().next())
    };
    f.reviewed = lines
        .iter()
        .filter_map(|(_, l)| field(l, "Last reviewed"))
        .max();
    f.updated = lines
        .iter()
        .filter_map(|(_, l)| field(l, "Last updated"))
        .max();
    let mut by_update: Vec<&(u32, Vec<(String, String)>)> = lines.iter().collect();
    by_update.sort_by_key(|(_, l)| std::cmp::Reverse(field(l, "Last updated")));
    f.line = by_update.first().map(|(n, _)| *n);
    f.target = by_update.iter().find_map(|(_, l)| {
        l.iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("Target"))
            .map(|(_, v)| plain(v))
            .filter(|v| !v.is_empty())
    });
    f.generated = d
        .lines
        .iter()
        .filter(|l| GENERATED.is_match(&l.text))
        .flat_map(|l| value::dates(&l.text))
        .max();
    if lines.is_empty() && f.generated.is_none() {
        f.why = Some("no date line; see last commit".into());
    }
    f
}

/// The stage from the first stage line of `ROADMAP.md`, or without one from
/// `README.md`'s status badge or Status line.
pub fn stage(roadmap: Option<&Doc>, readme: Option<&Doc>) -> Cell<Stage> {
    if let Some(d) = roadmap {
        for l in &d.lines {
            let Some(c) = STAGE.captures(l.text.trim()) else {
                continue;
            };
            let steps = c
                .get(2)
                .map(|n| n.as_str().split(';').filter_map(step).collect())
                .unwrap_or_default();
            return Cell::ok(
                Stage {
                    name: c[1].to_string(),
                    steps,
                },
                d.src(l.no),
            );
        }
    }
    if let Some(d) = readme {
        for l in &d.lines {
            if let Some(c) = BADGE.captures(&l.text) {
                let name = c[1].replace("%20", " ").trim().to_string();
                return Cell::ok(
                    Stage {
                        name,
                        steps: Vec::new(),
                    },
                    d.src(l.no),
                )
                .from(format!("{README} status badge"));
            }
        }
        for l in &d.lines {
            let t = l.text.trim_start();
            let Some(rest) = t.strip_prefix('>').map(str::trim_start) else {
                continue;
            };
            if !rest.starts_with("**Status:**") {
                continue;
            }
            let lower = rest.to_lowercase();
            let name = ["pre-alpha", "early alpha", "alpha", "beta", "stable"]
                .into_iter()
                .find(|w| md::contains_word(&lower, w));
            if let Some(name) = name {
                return Cell::ok(
                    Stage {
                        name: name.to_string(),
                        steps: Vec::new(),
                    },
                    d.src(l.no),
                )
                .from(format!("{README} Status line"));
            }
        }
    }
    let why = match (roadmap, readme) {
        (Some(_), _) => format!("No **Stage:** line in {ROADMAP}"),
        (None, Some(_)) => format!("No {ROADMAP}, and no status badge or Status line in {README}"),
        (None, None) => format!("No {ROADMAP} or {README}"),
    };
    Cell::absent(why)
}

/// One clause of the `next:` text.
fn step(clause: &str) -> Option<StageStep> {
    let lower = clause.to_lowercase();
    let stage = lower
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .find(|w| STAGE_WORDS.contains(w))?
        .to_string();
    let points = POINTS.captures(clause).and_then(|c| c[1].parse().ok());
    let hours = STAGE_HOURS.captures(clause).and_then(|c| {
        let n = |i: usize| c[i].replace(',', "").parse::<u32>().ok();
        Some(Hours {
            lo: n(1)?,
            hi: n(2)?,
            approx: c[0].trim_start().starts_with('~'),
            raw: c[0].trim().to_string(),
            ..Default::default()
        })
    });
    Some(StageStep {
        stage,
        points,
        hours,
        text: clause.trim().to_string(),
    })
}

/// One row of a headline or audience table.
struct Row<'a> {
    line: u32,
    label: String,
    base: String,
    qualifier: String,
    value: &'a str,
    kind: String,
    kind_raw: &'a str,
}

fn rows<'a>(t: &'a Table, value_col: usize) -> Vec<Row<'a>> {
    let kind = t.col(&["kind", "how"]);
    t.rows
        .iter()
        .filter(|r| r.cells.len() > value_col)
        .map(|r| {
            let (base, qualifier) = split_label(&md::normalize(&r.cells[0].raw));
            Row {
                line: r.line,
                label: r.cells[0].text.clone(),
                base,
                qualifier,
                value: &r.cells[value_col].raw,
                kind: kind
                    .and_then(|k| r.cells.get(k))
                    .map(|c| c.text.clone())
                    .unwrap_or_default(),
                kind_raw: kind
                    .and_then(|k| r.cells.get(k))
                    .map_or("", |c| c.raw.as_str()),
            }
        })
        .collect()
}

/// The whole-target Ready for real work row of `rows`: a row without a
/// qualifier or with "full" in it, else the first row whose qualifier does
/// not start with "mainstream" or "essentials", such as "(whole of the target
/// app)" or a question the number answers.
fn ready_row<'r, 'a>(rows: &'r [Row<'a>]) -> Option<&'r Row<'a>> {
    let whole = |r: &&Row| {
        r.base == "ready for real work"
            && !r.qualifier.starts_with("mainstream")
            && !r.qualifier.starts_with("essentials")
    };
    rows.iter()
        .filter(whole)
        .find(|r| r.qualifier.is_empty() || r.qualifier.contains("full"))
        .or_else(|| rows.iter().find(whole))
}

impl Row<'_> {
    fn figure(&self, path: &str, from: Option<String>, lead: Option<Basis>) -> Figure {
        let text = plain(self.value);
        let Some(read) = value::pct(self.value) else {
            return Figure::Unreadable {
                why: format!("No percentage in the Value cell of {}", self.label),
                raw: text,
                path: path.to_string(),
                line: self.line,
            };
        };
        let from = [from, read.computed.then(|| read.note.clone()).flatten()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let mut basis = value::basis(&self.kind, &text);
        if basis == Basis::Unknown {
            basis = lead.unwrap_or(Basis::Unknown);
        }
        Figure::Ok {
            pct: read.value,
            low: read.low,
            high: read.high,
            label: self.label.clone(),
            basis,
            text,
            kind: self.kind.clone(),
            path: path.to_string(),
            line: self.line,
            from: (!from.is_empty()).then(|| from.join("; ")),
        }
    }

    fn hours(&self, path: &str) -> Cell<Hours> {
        match value::hours(self.value, &self.label, "") {
            Ok(h) => Cell::ok(h, Src::new(path, self.line)),
            Err(raw) => Cell::Unreadable {
                why: format!("No hours in the Value cell of {}", self.label),
                raw,
                src: Src::new(path, self.line),
            },
        }
    }
}

/// `("feature breadth", "weighted by use")` from `feature breadth (weighted
/// by use)`, and `("ready for real work", "full acrobat pro")` from `ready
/// for real work, full acrobat pro`.
pub fn split_label(label: &str) -> (String, String) {
    let paren = label.find(" (");
    let comma = label.find(',');
    let (at, skip) = match (paren, comma) {
        (Some(p), Some(c)) if c < p => (c, 1),
        (Some(p), _) => (p, 2),
        (None, Some(c)) => (c, 1),
        (None, None) => return (label.trim().to_string(), String::new()),
    };
    let rest = label[at + skip..].trim();
    let rest = if skip == 2 {
        rest.strip_suffix(')').unwrap_or(rest)
    } else {
        rest
    };
    (label[..at].trim().to_string(), rest.trim().to_string())
}

/// The headline table of `d`: the first table whose second column is Value
/// under the first heading outside excluded sections that starts with one of
/// `prefixes`.
fn headline_table<'a>(d: &'a Doc, prefixes: &[&str]) -> Result<&'a Table, String> {
    let h = d
        .heading(prefixes)
        .ok_or_else(|| format!("No Headline numbers section in {}", d.path))?;
    d.tables_in(h)
        .find(|t| t.hnorm.get(1).is_some_and(|v| v == "value"))
        .ok_or_else(|| {
            format!(
                "No table with a Value column under Headline numbers in {}",
                d.path
            )
        })
}

/// Fills the headline fields of `out`.
pub fn headline(out: &mut Parity, rm: Option<&Doc>, tap: Option<&Doc>) {
    let table = match rm {
        Some(d) => headline_table(d, &["headline numbers"]).map(|t| (d, t)),
        None => Err(format!("No {ROADMAP} at HEAD")),
    };
    let table = table.or_else(|e| match tap {
        Some(d) => headline_table(d, &["headline"])
            .map(|t| (d, t))
            .map_err(|_| e),
        None => Err(e),
    });
    let (d, t) = match table {
        Ok(x) => x,
        Err(why) => {
            out.breadth = Figure::absent(&why);
            out.ready = Figure::absent(&why);
            out.menu_breadth = Figure::absent(&why);
            out.to_alpha = Cell::absent(&why);
            out.to_beta = Cell::absent(&why);
            out.to_full = Cell::absent(&why);
            audience(out, rm, tap, &format!("{why}; no {{row}} row"));
            to_alpha_from_stage(out);
            return;
        }
    };
    let path = d.path.as_str();
    let rows = rows(t, 1);
    let base = |b: &str| rows.iter().filter(move |r| r.base == b).collect::<Vec<_>>();
    let breadth = base("feature breadth");
    let pick = breadth
        .iter()
        .position(|r| md::contains_word(&r.qualifier, "weighted"))
        .or_else(|| breadth.iter().position(|r| r.qualifier.is_empty()))
        .or(if breadth.is_empty() { None } else { Some(0) });
    match pick {
        Some(i) => {
            out.breadth = breadth[i].figure(path, None, None);
            out.breadth_alts = breadth
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, r)| r.figure(path, None, None))
                .collect();
            let b = breadth[i];
            // Each document the breadth rows link to once, though several
            // sections are linked: the shown row's first, then the others'.
            out.breadth_links = Vec::new();
            let others = breadth
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, r)| *r);
            for p in std::iter::once(b)
                .chain(others)
                .flat_map(|r| md::links(r.kind_raw))
                .filter_map(|(_, url)| super::dims::resolve(path, &url))
            {
                if p != path && !out.breadth_links.contains(&p) {
                    out.breadth_links.push(p);
                }
            }
            let menu = format!("{} {}", b.label, b.kind).to_lowercase();
            if menu.contains("menu") {
                out.menu_hint = fraction(&plain(b.value)).or_else(|| fraction(&b.kind));
            }
        }
        None => {
            out.breadth = Figure::absent(format!(
                "No Feature breadth row in {path}'s Headline numbers"
            ))
        }
    }
    let menu = rows.iter().find(|r| {
        [
            "menu breadth",
            "menu-bar coverage",
            "menu/command breadth",
            "ribbon/menu catalog coverage",
        ]
        .contains(&r.base.as_str())
    });
    out.menu_breadth = match menu {
        Some(r) => r.figure(path, None, None),
        None => Figure::absent(format!("No menu breadth row in {path}'s Headline numbers")),
    };
    if !matches!(out.menu_breadth, Figure::Absent { .. }) {
        out.menu_hint = None;
    }
    let ready = ready_row(&rows);
    let mainstream = rows.iter().find(|r| {
        r.base.starts_with("mainstream")
            || (r.base == "ready for real work" && r.qualifier.starts_with("mainstream"))
    });
    let essentials = rows.iter().find(|r| {
        r.base.starts_with("essentials")
            || (r.base == "ready for real work" && r.qualifier.starts_with("essentials"))
    });
    out.ready = ready.map_or_else(Figure::default, |r| r.figure(path, None, None));
    out.mainstream = mainstream.map_or_else(Figure::default, |r| r.figure(path, None, None));
    out.essentials = essentials.map_or_else(Figure::default, |r| r.figure(path, None, None));
    let missing = |what: &str| format!("No {what} row in {path}'s Headline numbers");
    let to = |stage: &str| {
        rows.iter().find(|r| {
            let l = format!("{} {}", r.base, r.qualifier);
            (l.contains("remaining to")
                || l.contains("hours to")
                || md::contains_word(&r.base, "to"))
                && l.contains(stage)
        })
    };
    out.to_alpha = to("alpha").map_or_else(
        || Cell::absent(missing("Remaining to alpha")),
        |r| r.hours(path),
    );
    out.to_beta = to("beta").map_or_else(
        || Cell::absent(missing("Remaining to beta")),
        |r| r.hours(path),
    );
    out.to_full = to("full parity").map_or_else(
        || Cell::absent(missing("Remaining to full parity")),
        |r| r.hours(path),
    );
    if ready.is_none() || mainstream.is_none() || essentials.is_none() {
        audience(out, rm, tap, &missing("{row}"));
    }
    if ready.is_none() && matches!(out.ready, Figure::Absent { .. }) {
        if let Some(r) = base("feature depth").first() {
            out.ready = r.figure(
                path,
                Some("Feature depth row (no Ready for real work row)".into()),
                None,
            );
        }
    }
    if !out.to_alpha.is_ok() {
        let stage_row = rows.iter().find(|r| r.base == "stage");
        if let Some(r) = stage_row {
            let text = plain(r.value);
            if let Some(c) = STAGE_HOURS
                .captures(&text)
                .filter(|_| text.contains("to alpha"))
            {
                if let Ok(h) = value::hours(&c[0], "", "") {
                    out.to_alpha =
                        Cell::ok(h, Src::new(path, r.line)).from("the headline Stage row");
                }
            }
        }
    }
    to_alpha_from_stage(out);
    audience_hours_note(out, rm);
}

/// To alpha from the stage line's alpha step, when the headline has none.
fn to_alpha_from_stage(out: &mut Parity) {
    if out.to_alpha.is_ok() {
        return;
    }
    if let Some(Stage { steps, .. }) = out.stage.get() {
        if let (Some(h), Some(src)) = (
            steps
                .iter()
                .find(|s| s.stage == "alpha")
                .and_then(|s| s.hours.clone()),
            out.stage.src().cloned(),
        ) {
            out.to_alpha = Cell::ok(h, src).from("the stage line");
        }
    }
}

/// A note when the audience table's full-target hours differ from To full.
fn audience_hours_note(out: &mut Parity, rm: Option<&Doc>) {
    let Some(d) = rm else { return };
    let Some(t) = d.live_tables().find(|t| t.first_header() == "audience") else {
        return;
    };
    let (Some(hc), Some(to_full)) = (t.col_containing("hours", false), out.to_full.get().cloned())
    else {
        return;
    };
    let Some(col) = t.col(&["ready %", "ready"]) else {
        return;
    };
    let full = rows(t, col)
        .into_iter()
        .find(|r| r.base.starts_with("full") || r.qualifier.starts_with("ready for real work"));
    let Some(r) = full else { return };
    let cell = t
        .rows
        .iter()
        .find(|row| row.line == r.line)
        .and_then(|row| row.cells.get(hc));
    if let Some(Ok(a)) = cell.map(|c| value::hours(&c.raw, "", "")) {
        if (a.lo, a.hi) != (to_full.lo, to_full.hi) {
            out.notes.push(Note::info(
                format!(
                    "audience table's full-target hours {}–{} differ from To full {}–{}",
                    a.lo, a.hi, to_full.lo, to_full.hi
                ),
                Src::new(&d.path, r.line),
            ));
        }
    }
}

/// Ready, Mainstream and Essentials from the "Readiness by audience" table
/// of `ROADMAP.md`, then the parity doc's headline, for the ones the
/// headline table lacks.
fn audience(out: &mut Parity, rm: Option<&Doc>, tap: Option<&Doc>, why: &str) {
    let mut sources: Vec<(&Doc, &Table, Option<Basis>)> = Vec::new();
    if let Some(d) = rm {
        if let Some(t) = d.live_tables().find(|t| t.first_header() == "audience") {
            sources.push((d, t, lead_basis(d, t)));
        }
    }
    if let Some(d) = tap {
        if let Ok(t) = headline_table(d, &["headline"]) {
            sources.push((d, t, None));
        }
    }
    for (d, t, lead) in sources {
        let col = if t.first_header() == "audience" {
            t.col(&["ready %", "ready"])
        } else {
            Some(1)
        };
        let Some(col) = col else { continue };
        let rows = rows(t, col);
        let label = |r: &Row| {
            let name = if t.first_header() == "audience" {
                "Readiness by audience".to_string()
            } else {
                d.headings
                    .iter()
                    .rev()
                    .find(|h| h.line < t.line)
                    .map_or_else(|| d.path.clone(), |h| plain(&h.text))
            };
            format!("{name}, {}:{}", d.path, r.line)
        };
        let full = rows
            .iter()
            .find(|r| r.base.starts_with("full") || r.qualifier.starts_with("ready for real work"))
            .or_else(|| ready_row(&rows));
        let fill = |f: &mut Figure, r: Option<&Row>| {
            if let (Figure::Absent { .. }, Some(r)) = (&*f, r) {
                *f = r.figure(&d.path, Some(label(r)), lead);
            }
        };
        fill(&mut out.ready, full);
        fill(
            &mut out.mainstream,
            rows.iter().find(|r| {
                r.base.starts_with("mainstream") || r.qualifier.starts_with("mainstream")
            }),
        );
        fill(
            &mut out.essentials,
            rows.iter().find(|r| {
                r.base.starts_with("essentials") || r.qualifier.starts_with("essentials")
            }),
        );
    }
    let had_table = rm.is_some_and(|d| d.live_tables().any(|t| t.first_header() == "audience"));
    let names = ["Ready for real work", "Mainstream", "Essentials"];
    for (f, name) in [&mut out.ready, &mut out.mainstream, &mut out.essentials]
        .into_iter()
        .zip(names)
    {
        if matches!(f, Figure::Absent { why } if why == "Not read yet") {
            let tail = if had_table {
                "or in the Readiness by audience table"
            } else {
                "and no Readiness by audience table"
            };
            *f = Figure::absent(format!("{} {tail}", why.replace("{row}", name)));
        }
    }
}

/// Estimated, when the paragraph before an audience table says "(all
/// estimated".
fn lead_basis(d: &Doc, t: &Table) -> Option<Basis> {
    let from = t.at.saturating_sub(6);
    d.lines[from..t.at]
        .iter()
        .any(|l| l.text.to_lowercase().contains("(all estimated"))
        .then_some(Basis::Estimated)
}

/// Notes about each file's dates: several date lines, revision rows newer
/// than Last updated or out of order.
pub fn date_notes(out: &mut Parity, docs: &[&Doc]) {
    for d in docs {
        if d.path == README {
            continue;
        }
        let Some(f) = out.files.iter().find(|f| f.path == d.path) else {
            continue;
        };
        let (updated, line, lines) = (f.updated.clone(), f.line, f.date_lines);
        if lines > 1 {
            out.notes.push(Note::info(
                format!("{lines} date lines (merge residue)"),
                d.src(line.unwrap_or(1)),
            ));
        }
        let Some(h) = d
            .headings
            .iter()
            .position(|h| h.norm.starts_with("revision history"))
        else {
            continue;
        };
        let (at, end) = (d.headings[h].at, d.headings[h].end);
        let Some(t) = d.tables.iter().find(|t| t.at > at && t.at < end) else {
            continue;
        };
        let dates: Vec<(String, u32)> = t
            .rows
            .iter()
            .filter_map(|r| {
                let first = r.cells.first()?;
                Some((value::dates(&first.text).into_iter().next()?, r.line))
            })
            .collect();
        if let (Some(newest), Some(updated)) = (dates.iter().max(), &updated) {
            if &newest.0 > updated {
                out.notes.push(Note::warn(
                    format!(
                        "revision row {} is newer than Last updated {updated}",
                        newest.0
                    ),
                    d.src(newest.1),
                ));
            }
        }
        let asc = dates.windows(2).filter(|w| w[1].0 > w[0].0).count();
        let desc = dates.windows(2).filter(|w| w[1].0 < w[0].0).count();
        let out_of_order = asc.min(desc);
        if out_of_order > 0 {
            out.notes.push(Note::info(
                format!("{out_of_order} revision rows out of date order"),
                d.src(t.line),
            ));
        }
    }
}

/// Notes comparing one source with another: the stage line's hours with
/// To beta, the headline menu figure with the checklist total, and the beta
/// gaps' hours with To beta.
pub fn cross_checks(out: &mut Parity) {
    if let (Some(stage), Some(to_beta)) = (out.stage.get(), out.to_beta.get()) {
        if let Some(h) = stage
            .steps
            .iter()
            .find(|s| s.stage == "beta")
            .and_then(|s| s.hours.as_ref())
        {
            if (h.lo, h.hi) != (to_beta.lo, to_beta.hi) {
                out.notes.push(Note::info(
                    format!(
                        "stage line says {}–{} h to beta; the headline says {}–{} h",
                        h.lo, h.hi, to_beta.lo, to_beta.hi
                    ),
                    out.stage.src().cloned().unwrap_or_default(),
                ));
            }
        }
    }
    let menu = match &out.menu_breadth {
        Figure::Ok {
            text, path, line, ..
        } => fraction(text).map(|f| (f, Src::new(path, *line))),
        _ => None,
    }
    .or_else(|| {
        let src = match &out.breadth {
            Figure::Ok { path, line, .. } => Src::new(path, *line),
            _ => Src::default(),
        };
        out.menu_hint.map(|f| (f, src))
    });
    if let Some((menu, src)) = menu {
        for c in &out.checklists {
            if let Checklist::Presence { total, path, .. } = c {
                if let Some(t) = total.get() {
                    if (t.done, t.total) != (menu.done, menu.total) {
                        out.notes.push(Note::info(
                            format!(
                                "headline menu figure {}/{} differs from {path}'s {}/{}",
                                menu.done, menu.total, t.done, t.total
                            ),
                            src.clone(),
                        ));
                    }
                }
            }
        }
    }
    if let (Some(beta), Some(to_beta)) = (&out.gap_totals.beta, out.to_beta.get()) {
        if (beta.lo, beta.hi) != (to_beta.lo, to_beta.hi) {
            out.notes.push(Note::info(
                format!(
                    "Σ beta gaps {}–{} h; the headline says {}–{} h to beta",
                    beta.lo, beta.hi, to_beta.lo, to_beta.hi
                ),
                Src::new(GAPS, 1),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{read, Input};
    use super::*;

    const ROADMAP_MD: &str = "# Ripple roadmap

**Stage: alpha** · next: beta, ~13 points (ready for real work ~62% → ~75%) and ~170–270 h away

> **Last reviewed:** 2026-09-30 · **Last updated:** 2026-10-02 · **Target:** Some App 3

```md
## Headline numbers
| | Value | Kind |
|---|---|---|
| **Feature breadth** | **~11%** | estimated |
```

## Headline numbers (2026-10-01)

| | Value | Kind |
|---|---|---|
| **Feature breadth** (checklist) | **81.1%** of 400 rows | measured: checklist recount |
| **Feature breadth** (weighted by use) | **~77%** | partly measured: menus 90/100, tools estimated |
| Ready for real work, mainstream user | **~40%** | estimated |
| **Ready for real work** (full target) | **~61%** (56–66%) | Estimated: weighted sum |
| Remaining to **beta** | **~300–500 h** | estimated |

> **Last reviewed:** 2026-10-04 · **Last updated:** 2026-10-04

## By dimension

| | Value | Kind |
|---|---|---|
| **Feature breadth** | **5%** | measured |
";

    fn rm(text: &str) -> Parity {
        read(&[Input {
            path: ROADMAP,
            oid: "a1",
            text,
        }])
    }

    type Got<'a> = (
        f64,
        Option<f64>,
        Option<f64>,
        &'a str,
        Basis,
        u32,
        Option<&'a str>,
    );

    fn ok(f: &Figure) -> Got<'_> {
        match f {
            Figure::Ok {
                pct,
                low,
                high,
                label,
                basis,
                line,
                from,
                ..
            } => (
                *pct,
                *low,
                *high,
                label.as_str(),
                *basis,
                *line,
                from.as_deref(),
            ),
            other => panic!("not read: {other:?}"),
        }
    }

    fn why(f: &Figure) -> String {
        match f {
            Figure::Absent { why } => why.clone(),
            other => panic!("not absent: {other:?}"),
        }
    }

    #[test]
    fn reads_breadth_ready_and_the_review_date() {
        let p = rm(ROADMAP_MD);
        assert_eq!(p.version, super::super::PARSER_VERSION);
        assert_eq!(
            ok(&p.breadth),
            (
                77.0,
                None,
                None,
                "Feature breadth (weighted by use)",
                Basis::Partly,
                19,
                None
            )
        );
        assert_eq!(
            p.breadth_alts.len(),
            1,
            "the checklist row is an alternative"
        );
        assert_eq!(
            ok(&p.ready),
            (
                61.0,
                Some(56.0),
                Some(66.0),
                "Ready for real work (full target)",
                Basis::Estimated,
                21,
                None
            )
        );
        assert_eq!(ok(&p.mainstream).0, 40.0);
        assert_eq!(p.to_beta.get().map(|h| (h.lo, h.hi)), Some((300, 500)));
        assert_eq!(p.last_reviewed.as_deref(), Some("2026-10-04"));
        let f = &p.files[0];
        assert_eq!(
            (f.date_lines, f.updated.as_deref()),
            (2, Some("2026-10-04"))
        );
        assert_eq!(
            f.target.as_deref(),
            Some("Some App 3"),
            "the newest line has no Target"
        );
        assert!(p
            .notes
            .iter()
            .any(|n| n.text == "2 date lines (merge residue)"));
    }

    #[test]
    fn a_missing_ready_row_falls_back_to_the_audience_table() {
        let md = "## Headline numbers\n\n| | Value | Kind |\n|---|---|---|\n\
                  | **Feature breadth** (menus, tools) | **~88%** | measured in part: menus 348/375 |\n\
                  | **Feature depth** (weighted by use) | **~75%** | estimated |\n\
                  | **To full parity** | **~360–590 h** one agent | estimated |\n\n\
                  ## Readiness by audience\n\nHours to ~95% (all estimated):\n\n\
                  | Audience | Ready % | Hours to ~95% (one agent) |\n|---|---|---|\n\
                  | **Full App** (ready for real work; decides the stage) | **~62%** (57–67%) | **~340–560 h** |\n\
                  | **Mainstream user** | **~65%** | 230–380 |\n";
        let p = rm(md);
        assert_eq!(
            ok(&p.ready),
            (
                62.0,
                Some(57.0),
                Some(67.0),
                "Full App (ready for real work; decides the stage)",
                Basis::Estimated,
                15,
                Some("Readiness by audience, ROADMAP.md:15")
            )
        );
        assert_eq!(ok(&p.mainstream).0, 65.0);
        assert_eq!(
            why(&p.essentials),
            "No Essentials row in ROADMAP.md's Headline numbers or in the Readiness by audience table"
        );
        assert!(p.notes.iter().any(|n| n
            .text
            .contains("full-target hours 340–560 differ from To full 360–590")));
        assert_eq!(ok(&p.breadth).4, Basis::Partly);
        assert_eq!(p.menu_hint.map(|f| (f.done, f.total)), Some((348.0, 375.0)));
    }

    #[test]
    fn an_unreadable_ready_row_stays_unreadable() {
        let md = "## Headline numbers\n\n| | Value | Kind |\n|---|---|---|\n\
                  | **Feature breadth** | **~88%** | measured |\n\
                  | **Ready for real work** | see below | estimated |\n\n\
                  ## Readiness by audience\n\n\
                  | Audience | Ready % |\n|---|---|\n\
                  | **Full App** (ready for real work) | **~62%** |\n";
        assert_eq!(
            rm(md).ready,
            Figure::Unreadable {
                why: "No percentage in the Value cell of Ready for real work".into(),
                raw: "see below".into(),
                path: ROADMAP.into(),
                line: 6,
            }
        );
    }

    #[test]
    fn a_row_without_its_trailing_kind_cell_keeps_its_value() {
        let md = "## Headline numbers\n\n| | Value | Kind |\n|---|---|---|\n\
                  | **Feature breadth** | **~88%** |\n\
                  | **Ready for real work** | **~45%** |\n";
        let p = rm(md);
        assert_eq!(ok(&p.breadth).0, 88.0);
        assert_eq!(ok(&p.ready).0, 45.0);
    }

    #[test]
    fn feature_depth_is_the_last_fallback() {
        let md = "## Headline numbers\n\n| | Value | Kind |\n|---|---|---|\n\
                  | **Feature depth** (weighted by use) | **~75%** | estimated |\n";
        assert_eq!(
            ok(&rm(md).ready).6,
            Some("Feature depth row (no Ready for real work row)")
        );
    }

    #[test]
    fn breadth_links_name_each_document_once() {
        let md = "## Headline numbers\n\n| | Value | Kind |\n|---|---|---|\n\
                  | Feature breadth | 60% | measured: [menus](docs/menus.md#file) and [tools](docs/menus.md#tools), \
                  [this](#headline-numbers), [ui](docs/ui.md) |\n";
        assert_eq!(rm(md).breadth_links, ["docs/menus.md", "docs/ui.md"]);
        // The shown row's links come first, then the other breadth rows'.
        let md = "## Headline numbers\n\n| | Value | Kind |\n|---|---|---|\n\
                  | Feature breadth (checklist) | 81% | measured: [checklist](docs/features.md) |\n\
                  | Feature breadth (weighted) | 77% | estimated: [menus](docs/menus.md) |\n";
        let p = rm(md);
        assert_eq!(ok(&p.breadth).0, 77.0);
        assert_eq!(p.breadth_links, ["docs/menus.md", "docs/features.md"]);
    }

    #[test]
    fn a_described_ready_row_is_the_whole_target() {
        let md = "## Headline numbers\n| | Value |\n|---|---|\n\
                  | Ready for real work, mainstream user | 40% |\n\
                  | **Ready for real work** (could a professional switch for client work?) | **~45%** |\n";
        assert_eq!(ok(&rm(md).ready).0, 45.0);
        let tap = "# Parity\n\n## Headline\n\n| Number | Value | Kind |\n|---|---|---|\n\
                   | **Ready for real work** (whole of Some App) | **~40 %** | estimated |\n\
                   | **Mainstream practitioner** | **~36 %** | estimated |\n";
        // ROADMAP.md has no headline table: the parity document's is read.
        let p = read(&[
            Input {
                path: ROADMAP,
                oid: "a1",
                text: "# Roadmap\n",
            },
            Input {
                path: super::super::PARITY_DOC,
                oid: "b1",
                text: tap,
            },
        ]);
        assert_eq!(ok(&p.ready).0, 40.0);
        // ROADMAP.md's headline table has no Ready row: the parity
        // document's headline table fills it.
        let p = read(&[
            Input {
                path: ROADMAP,
                oid: "a1",
                text: "## Headline numbers\n| | Value |\n|---|---|\n| Feature breadth | 70% |\n",
            },
            Input {
                path: super::super::PARITY_DOC,
                oid: "b1",
                text: tap,
            },
        ]);
        assert_eq!(
            ok(&p.ready).6,
            Some("Headline, docs/target-app-parity.md:7")
        );
        assert_eq!(ok(&p.ready).0, 40.0);
        assert_eq!(ok(&p.mainstream).0, 36.0);
    }

    #[test]
    fn two_breadth_rows_prefer_the_weighted_one() {
        let md = "## Headline numbers\n| | Value |\n|---|---|\n| Feature breadth (checklist) | 81% |\n\
                  | Feature breadth, mainstream practitioner | 70% |\n| Feature breadth (weighted) | 77% |\n";
        let p = rm(md);
        assert_eq!(ok(&p.breadth).0, 77.0);
        assert_eq!(
            p.breadth_alts.iter().map(|f| f.pct()).collect::<Vec<_>>(),
            [Some(81.0), Some(70.0)]
        );
        let md = "## Headline numbers\n| | Value |\n|---|---|\n| Feature breadth, mainstream | 70% |\n| Feature breadth | 67% |\n";
        assert_eq!(
            ok(&rm(md).breadth).0,
            67.0,
            "no qualifier beats a qualifier"
        );
        let md =
            "## Headline numbers\n| | Value |\n|---|---|\n| Feature breadth (unweighted) | 81% |\n\
                  | Feature breadth (tier-weighted) | 74% |\n";
        assert_eq!(ok(&rm(md).breadth).0, 74.0, "unweighted is not weighted");
    }

    #[test]
    fn a_superseded_headline_section_is_skipped() {
        let md = "## Headline numbers (superseded 2026-09-01)\n| | Value |\n|---|---|\n| Feature breadth | 10% |\n\n\
                  ## Headline numbers\n| | Value |\n|---|---|\n| Feature breadth | 50% |\n";
        assert_eq!(ok(&rm(md).breadth).0, 50.0);
    }

    #[test]
    fn a_headline_table_quoted_in_a_fence_is_not_read() {
        let quoted = "## Headline numbers\n| | Value |\n|---|---|\n| Feature breadth | 11% |\n";
        let real = "## Headline numbers\n| | Value |\n|---|---|\n| Feature breadth | 50% |\n";
        for (open, inner, close) in [("````md", "```", "````"), ("```", "~~~", "```")] {
            let md = format!("# R\n\n{open}\n{inner}\n{quoted}{inner}\n{close}\n\n{real}");
            assert_eq!(ok(&rm(&md).breadth).0, 50.0, "{open} {inner}");
        }
    }

    #[test]
    fn missing_parts_give_a_reason() {
        let p = rm("# Roadmap\n\nNothing here.\n");
        assert_eq!(why(&p.breadth), "No Headline numbers section in ROADMAP.md");
        let p =
            rm("## Headline numbers\n\nSoon.\n\n## Next\n| a | Value |\n|---|---|\n| x | 1% |\n");
        assert_eq!(
            why(&p.ready),
            "No table with a Value column under Headline numbers in ROADMAP.md"
        );
        let p = rm("## Headline numbers\n| | Value |\n|---|---|\n| Menu breadth | 90% |\n");
        assert_eq!(
            why(&p.breadth),
            "No Feature breadth row in ROADMAP.md's Headline numbers"
        );
        assert_eq!(
            why(&p.ready),
            "No Ready for real work row in ROADMAP.md's Headline numbers and no Readiness by audience table"
        );
        assert_eq!(p.menu_breadth.pct(), Some(90.0));
        assert_eq!(
            rm("## Headline numbers\n| | Value |\n|---|---|\n| Feature breadth | most of it |\n")
                .breadth,
            Figure::Unreadable {
                why: "No percentage in the Value cell of Feature breadth".into(),
                raw: "most of it".into(),
                path: ROADMAP.into(),
                line: 4,
            }
        );
    }

    #[test]
    fn stage_clauses_carry_their_own_points_and_hours() {
        let p = rm("**Stage: pre-alpha** · next: alpha, ~5–7 h away; beta ~28 points and ~420–750 h beyond that (ready is ~47%)\n");
        let s = p.stage.get().unwrap();
        assert_eq!(s.name, "pre-alpha");
        let got: Vec<_> = s
            .steps
            .iter()
            .map(|st| {
                (
                    st.stage.as_str(),
                    st.points,
                    st.hours.as_ref().map(|h| (h.lo, h.hi)),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("alpha", None, Some((5, 7))),
                ("beta", Some(28.0), Some((420, 750)))
            ]
        );
        assert_eq!(p.to_alpha.get().map(|h| (h.lo, h.hi)), Some((5, 7)));
        assert!(matches!(&p.to_alpha, Cell::Ok { from: Some(f), .. } if f == "the stage line"));
        let p = rm("**Stage: pre-alpha** · next: alpha, ~5% and ~110–180 h away (the gate)\n");
        let st = &p.stage.get().unwrap().steps[0];
        assert_eq!(
            (st.points, st.hours.as_ref().map(|h| h.hi)),
            (Some(5.0), Some(180))
        );
        let p = rm("**Stage: pre-alpha** · next: alpha, ~2 points (38% → 40%) and ~125–190 Opus 5.5 hours away\n");
        let st = &p.stage.get().unwrap().steps[0];
        assert_eq!(
            (st.points, st.hours.as_ref().map(|h| h.lo)),
            (Some(2.0), Some(125))
        );
    }

    #[test]
    fn generated_lines_with_prose_before_the_date() {
        let d = Doc::parse(
            "docs/parity-checklist.md",
            "# C\n> **Generated:** by `cargo xtask parity` on 2026-10-09 from 2026-10-01 data\n",
        );
        let f = doc_file(&d, "x");
        assert_eq!(f.generated.as_deref(), Some("2026-10-09"));
        assert_eq!(f.why, None);
        let d = Doc::parse("docs/scorecard.md", "# S\n");
        assert_eq!(
            doc_file(&d, "x").why.as_deref(),
            Some("no date line; see last commit")
        );
    }

    #[test]
    fn stacked_date_lines_give_the_newest_of_each_field() {
        let mut md = String::from("# P\n");
        for i in 1..=25 {
            md.push_str(&format!(
                "> **Last reviewed:** 2026-09-{i:02} · **Last updated:** 2026-10-{:02} · **Target:** T{i}\n",
                26 - i
            ));
        }
        let p = rm(&md);
        let f = &p.files[0];
        assert_eq!(
            (
                f.reviewed.as_deref(),
                f.updated.as_deref(),
                f.date_lines,
                f.target.as_deref()
            ),
            (Some("2026-09-25"), Some("2026-10-25"), 25, Some("T1"))
        );
        assert_eq!(
            p.notes
                .iter()
                .filter(|n| n.text.contains("date lines"))
                .count(),
            1
        );
    }

    #[test]
    fn revision_rows_newer_than_last_updated_are_noted() {
        let md = "# P\n> **Last reviewed:** 2026-10-01 · **Last updated:** 2026-10-02\n## Headline numbers\n| | Value |\n|---|---|\n| Feature breadth | 5% |\n\
                  ## Revision history\n| Date | Change |\n|---|---|\n| 2026-10-05 | x |\n| 2026-10-01 | y |\n| 2026-10-03 | z |\n";
        let p = rm(md);
        assert!(p
            .notes
            .iter()
            .any(|n| n.text == "revision row 2026-10-05 is newer than Last updated 2026-10-02"));
        assert!(p
            .notes
            .iter()
            .any(|n| n.text == "1 revision rows out of date order"));
    }

    #[test]
    fn split_labels() {
        assert_eq!(
            split_label("feature breadth (weighted, incl. styles)"),
            ("feature breadth".into(), "weighted, incl. styles".into())
        );
        assert_eq!(
            split_label("ready for real work, full acrobat pro"),
            ("ready for real work".into(), "full acrobat pro".into())
        );
    }
}
