//! Machine checklists (tiered rows, TOML feature records, presence
//! catalogs, scorecards) and the hand-written `docs/*-parity.md` documents.

use std::sync::LazyLock;

use regex::Regex;

use super::md::{self, Doc, Table};
use super::value;
use super::{
    Cell, CheckRow, Checklist, Counts, DocChecklist, Figure, Group, Input, Note, Parity, Pct, Src,
    MAX_ROWS, PARITY_DOC,
};

static DOC_SCORE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\*\*(?:[^*]*?)(?:dimension(?: score)?|overall|summary)[^*]*?(?:\*\*\s*(?:\*\*)?)?[~≈]?\s?\b(\d{1,3}(?:\.\d+)?)\s?%",
    )
    .unwrap()
});
static TOML_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d[\d,]*) shipped \+ (\d[\d,]*) partial of (\d[\d,]*)").unwrap()
});
static TOML_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^id\s*=\s*"([^"]*)""#).unwrap());
static TIER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^P[0-3]$").unwrap());

/// A status, from the token map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Done,
    Partial,
    Missing,
    Excluded,
    Ahead,
    NotMeasured,
}

const TOKENS: &[(&str, Status)] = &[
    ("d", Status::Done),
    ("done", Status::Done),
    ("✅", Status::Done),
    ("✓", Status::Done),
    ("yes", Status::Done),
    ("shipped", Status::Done),
    ("pass", Status::Done),
    ("good", Status::Done),
    ("full", Status::Done),
    ("same", Status::Done),
    ("p", Status::Partial),
    ("partial", Status::Partial),
    ("🟡", Status::Partial),
    ("~", Status::Partial),
    ("most", Status::Partial),
    ("menus only", Status::Partial),
    ("draft", Status::Partial),
    ("over budget", Status::Partial),
    ("m", Status::Missing),
    ("missing", Status::Missing),
    ("⬜", Status::Missing),
    ("❌", Status::Missing),
    ("✗", Status::Missing),
    ("no", Status::Missing),
    ("planned", Status::Missing),
    ("gap", Status::Missing),
    ("none", Status::Missing),
    ("bug", Status::Missing),
    ("not run", Status::Missing),
    ("not measurable", Status::Missing),
    ("🚫", Status::Excluded),
    ("oos", Status::Excluded),
    ("out of scope", Status::Excluded),
    ("na", Status::Excluded),
    ("n/a", Status::Excluded),
    ("—", Status::Excluded),
    ("ahead", Status::Ahead),
    ("beyond", Status::Ahead),
    ("tracked", Status::NotMeasured),
    ("unknown", Status::NotMeasured),
];

/// The status a cell names: the longest token the normalized cell starts
/// with, ending at the cell's end or before a space, `;`, `(`, `,`, `:` or `.`.
pub fn status(cell: &str) -> Option<Status> {
    let t = md::plain(cell).trim().to_lowercase();
    let t = t.trim_end_matches(':').trim();
    TOKENS
        .iter()
        .filter(|(tok, _)| {
            t.starts_with(tok)
                && t[tok.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| [' ', ';', '(', ',', ':', '.'].contains(&c))
        })
        .max_by_key(|(tok, _)| tok.len())
        .map(|(_, s)| *s)
}

fn status_name(s: Option<Status>) -> &'static str {
    match s {
        Some(Status::Done) => "done",
        Some(Status::Partial) => "partial",
        Some(Status::Missing) => "missing",
        Some(Status::Excluded) => "excluded",
        Some(Status::Ahead) => "ahead",
        Some(Status::NotMeasured) => "not measured",
        None => "unrecognized",
    }
}

impl Counts {
    fn add(&mut self, s: Option<Status>) {
        match s {
            Some(Status::Done) => self.done += 1,
            Some(Status::Partial) => self.partial += 1,
            Some(Status::Missing) => self.missing += 1,
            Some(Status::Excluded) => self.excluded += 1,
            Some(Status::Ahead) => self.ahead += 1,
            Some(Status::NotMeasured) => self.not_measured += 1,
            None => self.unrecognized += 1,
        }
    }

    /// (done + ½ partial) / in scope, with ahead counted done.
    fn finish(&mut self) {
        let scope = self.done + self.partial + self.missing + self.ahead;
        self.pct = (scope > 0).then(|| {
            let v = (f64::from(self.done + self.ahead) + f64::from(self.partial) / 2.0)
                / f64::from(scope)
                * 100.0;
            (v * 10.0).round() / 10.0
        });
    }
}

/// The overall percentage of several counts.
fn overall(counts: &[Counts]) -> Option<f64> {
    let mut all = Counts::default();
    for c in counts {
        all.done += c.done;
        all.partial += c.partial;
        all.missing += c.missing;
        all.ahead += c.ahead;
    }
    all.finish();
    all.pct
}

/// Fills `out.checklists` and `out.docs`.
pub fn read(out: &mut Parity, files: &[Input], docs: &[&Doc]) {
    let linked = linked_paths(out);
    for f in files {
        let is_toml = f.path.ends_with(".toml");
        let machine = f.path == "docs/parity-checklist.md"
            || f.path == "docs/scorecard.md"
            || (is_toml && f.path.starts_with("parity/"))
            || linked.iter().any(|l| l == f.path);
        if is_toml && machine {
            if let Some(c) = toml_checklist(out, f, docs) {
                out.checklists.push(c);
            }
            continue;
        }
        let Some(d) = docs.iter().find(|d| d.path == f.path) else {
            continue;
        };
        if machine {
            let found = markdown(out, d);
            if !found.is_empty() {
                out.checklists.extend(found);
                continue;
            }
        }
        if f.path.starts_with("docs/") && f.path.ends_with("-parity.md") && f.path != PARITY_DOC {
            out.docs.push(hand_written(d));
        }
    }
    let tiered: Vec<f64> = out
        .checklists
        .iter()
        .filter_map(|c| match c {
            Checklist::Tiered { overall, .. } => *overall,
            _ => None,
        })
        .collect();
    let alt = out.breadth_alts.iter().find(|f| match f {
        Figure::Ok { label, .. } => label.to_lowercase().contains("checklist"),
        _ => false,
    });
    if let (
        Some(&computed),
        Some(Figure::Ok {
            pct, path, line, ..
        }),
    ) = (tiered.first(), alt)
    {
        if (computed - pct).abs() > 0.5 {
            out.notes.push(Note::info(
                format!("tiered checklist gives {computed}%; the headline's checklist breadth is {pct}%"),
                Src::new(path, *line),
            ));
        }
    }
}

/// In-repo paths linked from the headline breadth rows' Kind cells.
pub fn linked_paths(out: &Parity) -> Vec<String> {
    out.breadth_links.clone()
}

/// The machine checklists in one Markdown file: tiered rows, a presence
/// catalog, or status tables.
fn markdown(out: &mut Parity, d: &Doc) -> Vec<Checklist> {
    let mut found = Vec::new();
    let tiered: Vec<&Table> = d
        .live_tables()
        .filter(|t| t.col(&["priority", "tier"]).is_some() && status_col(t).is_some())
        .collect();
    if !tiered.is_empty() {
        found.push(tiered_checklist(out, d, &tiered));
    }
    let catalog = d
        .live_tables()
        .find(|t| ["tab", "menu"].contains(&t.first_header()));
    let total = d.blocks().into_iter().find_map(|b| {
        let bold = md::spans(&b.text, "**");
        bold.iter()
            .find_map(|s| value::fraction(s).map(|f| (f, b.line)))
    });
    let status: Vec<&Table> = d
        .live_tables()
        .filter(|t| {
            t.col(&["id"]).is_some()
                && status_col(t).is_some()
                && t.col(&["priority", "tier"]).is_none()
        })
        .collect();
    if tiered.is_empty() && status.is_empty() && (catalog.is_some() || total.is_some()) {
        found.push(presence(d, catalog, total));
    }
    if !status.is_empty() {
        found.push(status_checklist(out, d, &status));
    }
    found
}

fn status_col(t: &Table) -> Option<usize> {
    t.col(&["status", "state"])
        .or_else(|| t.col_containing("status", true))
}

/// One partial or missing row of a tiered or status table. The row's text
/// comes from a Feature, Scenario, Title or Item column, else from column
/// `text_col`.
fn row(t: &Table, r: &md::Row, s: Option<Status>, group: &str, text_col: usize) -> CheckRow {
    let feature = t
        .col(&["feature", "scenario", "title", "item"])
        .or(Some(text_col));
    let notes = t
        .col(&["missing / notes", "notes", "evidence", "why"])
        .or_else(|| t.col_containing("notes", false));
    CheckRow {
        status: status_name(s).into(),
        group: group.to_string(),
        text: t
            .cell(r, feature)
            .map(|c| c.text.clone())
            .unwrap_or_default(),
        notes: t.cell(r, notes).map(|c| c.text.clone()).unwrap_or_default(),
        line: r.line,
    }
}

fn tiered_checklist(out: &mut Parity, d: &Doc, tables: &[&Table]) -> Checklist {
    let mut tiers: Vec<Counts> = ["P0", "P1", "P2", "P3"]
        .iter()
        .map(|n| Counts {
            name: (*n).into(),
            ..Default::default()
        })
        .collect();
    let mut untiered = Counts {
        name: "untiered".into(),
        ..Default::default()
    };
    let mut rows = Vec::new();
    for t in tables {
        let (tc, sc) = (t.col(&["priority", "tier"]), status_col(t));
        let area = t.col(&["area"]);
        for r in &t.rows {
            let tier = t
                .cell(r, tc)
                .map(|c| c.text.trim().to_uppercase())
                .unwrap_or_default();
            let s = t.cell(r, sc).map(|c| status(&c.raw)).unwrap_or(None);
            if tier == "OOS" || tier == "N/A" {
                continue;
            }
            let group = t
                .cell(r, area)
                .map(|c| c.text.clone())
                .or_else(|| d.chain(t.section).first().map(|h| md::plain(&h.text)))
                .unwrap_or_default();
            match tiers.iter_mut().find(|x| x.name == tier) {
                Some(x) if TIER.is_match(&tier) => x.add(s),
                _ => untiered.add(s),
            }
            if matches!(s, Some(Status::Partial | Status::Missing) | None) {
                rows.push(row(t, r, s, &format!("{tier} · {group}"), 1));
            }
        }
    }
    tiers.retain(|x| {
        x.done + x.partial + x.missing + x.excluded + x.ahead + x.not_measured + x.unrecognized > 0
    });
    for x in tiers.iter_mut() {
        x.finish();
    }
    let all_unrec: u32 = tiers.iter().map(|x| x.unrecognized).sum::<u32>() + untiered.unrecognized;
    if all_unrec > 0 {
        out.notes.push(Note::warn(
            format!("{all_unrec} unrecognized status values"),
            d.src(tables[0].line),
        ));
    }
    let overall = overall(&tiers);
    if untiered.done
        + untiered.partial
        + untiered.missing
        + untiered.excluded
        + untiered.ahead
        + untiered.not_measured
        + untiered.unrecognized
        > 0
    {
        untiered.finish();
        tiers.push(untiered);
    }
    generated_summary_note(out, d, &tiers);
    let more = rows.len().saturating_sub(MAX_ROWS) as u32;
    rows.truncate(MAX_ROWS);
    Checklist::Tiered {
        path: d.path.clone(),
        tiers,
        overall,
        rows,
        more,
    }
}

/// A note when the document's generated Priority summary disagrees with
/// the computed counts.
fn generated_summary_note(out: &mut Parity, d: &Doc, tiers: &[Counts]) {
    let Some(t) = d
        .live_tables()
        .find(|t| t.first_header() == "priority" && t.col(&["done"]).is_some())
    else {
        return;
    };
    let (dc, pc, mc) = (t.col(&["done"]), t.col(&["partial"]), t.col(&["missing"]));
    for r in &t.rows {
        let name = r
            .cells
            .first()
            .map(|c| c.text.trim().to_uppercase())
            .unwrap_or_default();
        let Some(c) = tiers.iter().find(|x| x.name == name) else {
            continue;
        };
        let n = |col: Option<usize>| {
            t.cell(r, col)
                .and_then(|x| x.text.trim().parse::<u32>().ok())
        };
        let stated = (n(dc), n(pc), n(mc));
        if stated != (Some(c.done), Some(c.partial), Some(c.missing)) && stated.0.is_some() {
            out.notes.push(Note::info(
                format!(
                    "{name}: the generated summary says {}/{}/{} done/partial/missing; the rows give {}/{}/{}",
                    stated.0.unwrap_or(0),
                    stated.1.unwrap_or(0),
                    stated.2.unwrap_or(0),
                    c.done,
                    c.partial,
                    c.missing
                ),
                d.src(r.line),
            ));
        }
    }
}

fn presence(d: &Doc, catalog: Option<&Table>, total: Option<(super::Fraction, u32)>) -> Checklist {
    let mut groups = Vec::new();
    if let Some(t) = catalog {
        let done = t.col(&["live", "implemented", "covered"]);
        let all = t.col(&["total", "items"]);
        for r in &t.rows {
            let num = |c: Option<usize>| {
                t.cell(r, c)
                    .and_then(|x| x.text.replace(',', "").trim().parse::<f64>().ok())
            };
            if let (Some(name), Some(dn), Some(tt)) = (r.cells.first(), num(done), num(all)) {
                groups.push(Group {
                    name: name.text.clone(),
                    done: dn,
                    total: tt,
                    line: r.line,
                });
            }
        }
    }
    let total = match total {
        Some((f, line)) => Cell::ok(f, d.src(line)),
        None if !groups.is_empty() => {
            let f = super::Fraction {
                done: groups.iter().map(|g| g.done).sum(),
                total: groups.iter().map(|g| g.total).sum(),
            };
            Cell::ok(f, d.src(catalog.map_or(1, |t| t.line))).from("computed")
        }
        None => Cell::absent(format!("no bold total line in {}", d.path)),
    };
    let mut missing = Vec::new();
    for (i, h) in d.headings.iter().enumerate() {
        let is_list = ["missing", "not yet implemented", "not yet covered"]
            .iter()
            .any(|p| h.norm.starts_with(p));
        if is_list {
            for b in d.blocks_in(i).iter().filter(|b| b.item) {
                let text = md::plain(b.text.trim_start_matches(['-', '*', ' ']));
                let sub = d
                    .chain(b.section)
                    .first()
                    .filter(|s| s.level > h.level)
                    .map(|s| md::plain(&s.text));
                let group = sub.unwrap_or_else(|| {
                    text.split(['>', '›'])
                        .next()
                        .map(str::trim)
                        .unwrap_or("")
                        .to_string()
                });
                missing.push(CheckRow {
                    status: "missing".into(),
                    group,
                    text,
                    notes: String::new(),
                    line: b.line,
                });
            }
            continue;
        }
        // A tab's own "Missing:" list.
        let blocks = d.blocks_in(i);
        let mut on = false;
        for b in &blocks {
            if !b.item {
                on = b
                    .text
                    .trim()
                    .trim_end_matches(':')
                    .eq_ignore_ascii_case("missing");
                continue;
            }
            if on && b.section == Some(i) {
                missing.push(CheckRow {
                    status: "missing".into(),
                    group: md::cut_paren(&md::plain(&h.text)).to_string(),
                    text: md::plain(b.text.trim_start_matches(['-', '*', ' '])),
                    notes: String::new(),
                    line: b.line,
                });
            }
        }
    }
    let more = missing.len().saturating_sub(MAX_ROWS) as u32;
    missing.truncate(MAX_ROWS);
    Checklist::Presence {
        path: d.path.clone(),
        total,
        groups,
        missing,
        more,
    }
}

fn status_checklist(out: &mut Parity, d: &Doc, tables: &[&Table]) -> Checklist {
    let mut groups: Vec<Counts> = Vec::new();
    let mut rows = Vec::new();
    for t in tables {
        let name = d
            .chain(t.section)
            .iter()
            .find(|h| h.level == 2)
            .map_or_else(|| d.path.clone(), |h| md::plain(&h.text));
        let sc = status_col(t);
        let idx = match groups.iter().position(|g| g.name == name) {
            Some(i) => i,
            None => {
                groups.push(Counts {
                    name: name.clone(),
                    ..Default::default()
                });
                groups.len() - 1
            }
        };
        for r in &t.rows {
            let s = t.cell(r, sc).map(|c| status(&c.raw)).unwrap_or(None);
            groups[idx].add(s);
            if matches!(s, Some(Status::Partial | Status::Missing) | None) {
                rows.push(row(t, r, s, &name, 1));
            }
        }
    }
    for g in groups.iter_mut() {
        g.finish();
    }
    // The generated Summary table: Area | Done | Partial | Missing.
    if let Some(t) = d.live_tables().find(|t| {
        t.first_header() == "area" && t.col(&["done"]).is_some() && t.col(&["partial"]).is_some()
    }) {
        let (dc, pc, mc) = (t.col(&["done"]), t.col(&["partial"]), t.col(&["missing"]));
        for r in &t.rows {
            let name = r.cells.first().map(|c| c.text.clone()).unwrap_or_default();
            let Some(g) = groups.iter().find(|g| g.name == name) else {
                continue;
            };
            let n = |col: Option<usize>| {
                t.cell(r, col)
                    .and_then(|x| x.text.trim().parse::<u32>().ok())
            };
            let stated = (n(dc), n(pc), n(mc));
            if stated.0.is_some() && stated != (Some(g.done), Some(g.partial), Some(g.missing)) {
                out.notes.push(Note::info(
                    format!(
                        "{name}: the generated summary says {}/{}/{}; the rows give {}/{}/{} done/partial/missing",
                        stated.0.unwrap_or(0),
                        stated.1.unwrap_or(0),
                        stated.2.unwrap_or(0),
                        g.done,
                        g.partial,
                        g.missing
                    ),
                    d.src(r.line),
                ));
            }
        }
    }
    let overall = overall(&groups);
    let more = rows.len().saturating_sub(MAX_ROWS) as u32;
    rows.truncate(MAX_ROWS);
    Checklist::Status {
        path: d.path.clone(),
        groups,
        overall,
        rows,
        more,
    }
}

/// TOML `[[feature]]` records with id, title, area, tier, status and notes.
fn toml_checklist(out: &mut Parity, f: &Input, docs: &[&Doc]) -> Option<Checklist> {
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct Rec {
        id: String,
        title: String,
        area: String,
        tier: String,
        status: String,
        notes: String,
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct File {
        feature: Vec<Rec>,
    }
    let parsed: File = match toml::from_str(f.text) {
        Ok(p) => p,
        Err(e) => {
            out.notes.push(Note::warn(
                format!("{} does not parse: {e}", f.path),
                Src::new(f.path, 1),
            ));
            return None;
        }
    };
    let lines: Vec<(String, u32)> = f
        .text
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            TOML_ID
                .captures(l.trim())
                .map(|c| (c[1].to_string(), i as u32 + 1))
        })
        .collect();
    let mut tiers: Vec<Counts> = Vec::new();
    let mut rows = Vec::new();
    let (mut scope, mut shipped, mut partial) = (0u32, 0u32, 0u32);
    for r in &parsed.feature {
        let s = match r.status.as_str() {
            "shipped" => Some(Status::Done),
            "partial" => Some(Status::Partial),
            "planned" => Some(Status::Missing),
            "na" => Some(Status::Excluded),
            other => status(other),
        };
        if r.status != "na" {
            scope += 1;
        }
        shipped += u32::from(r.status == "shipped");
        partial += u32::from(r.status == "partial");
        let tier = if TIER.is_match(&r.tier) {
            r.tier.clone()
        } else {
            "untiered".to_string()
        };
        if r.tier == "N/A" {
            continue;
        }
        let idx = match tiers.iter().position(|t| t.name == tier) {
            Some(i) => i,
            None => {
                tiers.push(Counts {
                    name: tier.clone(),
                    ..Default::default()
                });
                tiers.len() - 1
            }
        };
        tiers[idx].add(s);
        if matches!(s, Some(Status::Partial | Status::Missing)) {
            rows.push(CheckRow {
                status: status_name(s).into(),
                group: format!("{} · {}", r.tier, r.area),
                text: r.title.clone(),
                notes: r.notes.clone(),
                line: lines
                    .iter()
                    .find(|(id, _)| *id == r.id)
                    .map_or(0, |(_, l)| *l),
            });
        }
    }
    tiers.sort_by(|a, b| a.name.cmp(&b.name));
    for t in tiers.iter_mut() {
        t.finish();
    }
    for d in docs {
        if let Some((line, c)) = d
            .lines
            .iter()
            .find_map(|l| TOML_TEXT.captures(&l.text).map(|c| (l.no, c)))
        {
            let n = |i: usize| c[i].replace(',', "").parse::<u32>().unwrap_or(0);
            if (n(1), n(2), n(3)) != (shipped, partial, scope) {
                out.notes.push(Note::info(
                    format!(
                        "{} has {shipped} shipped + {partial} partial of {scope} in scope; {} says {} + {} of {}",
                        f.path,
                        d.path,
                        n(1),
                        n(2),
                        n(3)
                    ),
                    d.src(line),
                ));
            }
            break;
        }
    }
    let overall = overall(&tiers);
    let more = rows.len().saturating_sub(MAX_ROWS) as u32;
    rows.truncate(MAX_ROWS);
    Some(Checklist::Tiered {
        path: f.path.to_string(),
        tiers,
        overall,
        rows,
        more,
    })
}

/// A hand-written parity doc: its stated score, and its status counts or
/// per-row percentages.
fn hand_written(d: &Doc) -> DocChecklist {
    let mut out = DocChecklist {
        path: d.path.clone(),
        ..Default::default()
    };
    out.score = d
        .lines
        .iter()
        .filter(|l| !l.excluded)
        .find_map(|l| {
            let c = DOC_SCORE.captures(&l.text)?;
            let v: f64 = c[1].parse().ok()?;
            Some(Cell::ok(
                Pct {
                    value: v,
                    raw: md::plain(&c[0]),
                    ..Default::default()
                },
                d.src(l.no),
            ))
        })
        .or_else(|| {
            let h = d.heading(&["summary"])?;
            let t = d.tables_in(h).next()?;
            let r = t.rows.iter().find(|r| {
                r.cells
                    .first()
                    .is_some_and(|c| c.text.to_lowercase().contains("overall"))
            })?;
            let p = r.cells.iter().skip(1).find_map(|c| value::pct(&c.raw))?;
            Some(Cell::ok(p, d.src(r.line)))
        })
        .unwrap_or_else(|| Cell::absent("no stated score"));
    let mut counts = Counts {
        name: d.path.clone(),
        ..Default::default()
    };
    let mut any = false;
    for t in d.live_tables() {
        let Some(sc) = status_col(t) else {
            if t.col(&["weight"]).is_none() {
                if let Some(pc) = t.col(&["%", "% ready", "ready", "parity"]) {
                    for r in &t.rows {
                        if let Some(p) = t.cell(r, Some(pc)).and_then(|c| value::pct(&c.raw)) {
                            out.pct_rows.push(CheckRow {
                                status: super::fmt_pct(p.value),
                                group: d
                                    .chain(t.section)
                                    .first()
                                    .map(|h| md::plain(&h.text))
                                    .unwrap_or_default(),
                                text: r.cells.first().map(|c| c.text.clone()).unwrap_or_default(),
                                notes: String::new(),
                                line: r.line,
                            });
                        }
                    }
                }
            }
            continue;
        };
        let cells: Vec<&md::MdCell> = t
            .rows
            .iter()
            .filter_map(|r| r.cells.get(sc))
            .filter(|c| !c.is_empty())
            .collect();
        let recognized = cells.iter().filter(|c| status(&c.raw).is_some()).count();
        if cells.is_empty() || recognized * 2 < cells.len() {
            if let Some(pc) = t.col(&["%", "% ready", "parity"]) {
                for r in &t.rows {
                    if let Some(p) = t.cell(r, Some(pc)).and_then(|c| value::pct(&c.raw)) {
                        out.pct_rows.push(CheckRow {
                            status: super::fmt_pct(p.value),
                            group: d
                                .chain(t.section)
                                .first()
                                .map(|h| md::plain(&h.text))
                                .unwrap_or_default(),
                            text: r.cells.first().map(|c| c.text.clone()).unwrap_or_default(),
                            notes: String::new(),
                            line: r.line,
                        });
                    }
                }
            }
            continue;
        }
        any = true;
        let group = d
            .chain(t.section)
            .iter()
            .find(|h| h.level == 2)
            .map(|h| md::plain(&h.text))
            .unwrap_or_default();
        for r in &t.rows {
            let s = t.cell(r, Some(sc)).map(|c| status(&c.raw)).unwrap_or(None);
            counts.add(s);
            if matches!(s, Some(Status::Partial | Status::Missing)) {
                // A hand-written table names its item in the first column.
                out.rows.push(row(t, r, s, &group, 0));
            }
        }
    }
    if any {
        counts.finish();
        out.counts = Some(counts);
    }
    out.rows.truncate(MAX_ROWS);
    out.pct_rows.truncate(MAX_ROWS);
    if out.counts.is_none() && out.pct_rows.is_empty() {
        out.why = Some("no status column recognized".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{read as read_all, Input, ROADMAP};
    use super::*;

    #[test]
    fn status_tokens_take_the_longest_prefix() {
        assert_eq!(
            status("over budget (target not met yet)"),
            Some(Status::Partial)
        );
        assert_eq!(status("not run"), Some(Status::Missing));
        assert_eq!(status("**missing**:"), Some(Status::Missing));
        assert_eq!(status("menus only"), Some(Status::Partial));
        assert_eq!(status("out of scope"), Some(Status::Excluded));
        assert_eq!(status("same"), Some(Status::Done));
        assert_eq!(status("✅"), Some(Status::Done));
        assert_eq!(status("D"), Some(Status::Done));
        assert_eq!(status("nothing"), None);
        assert_eq!(status("partially"), None);
    }

    fn checklist(text: &str) -> Parity {
        read_all(&[
            Input {
                path: ROADMAP,
                oid: "r",
                text: "# R\n",
            },
            Input {
                path: "docs/parity-checklist.md",
                oid: "c",
                text,
            },
        ])
    }

    #[test]
    fn tiered_rows_with_an_untiered_bucket() {
        let md = "# C\n## Rows\n| Area | Feature | Priority | Status |\n|---|---|---|---|\n\
            | Shell | Start | P0 | D |\n| Shell | Tabs | P0 | P |\n| Files | Open | P1 | M |\n| Files | Odd | — | D |\n| Files | Cloud | OOS | M |\n";
        let p = checklist(md);
        let Some(Checklist::Tiered {
            tiers,
            rows,
            overall,
            ..
        }) = p.checklists.first()
        else {
            panic!("{:?}", p.checklists)
        };
        let got: Vec<(&str, Option<f64>)> =
            tiers.iter().map(|t| (t.name.as_str(), t.pct)).collect();
        assert_eq!(
            got,
            [
                ("P0", Some(75.0)),
                ("P1", Some(0.0)),
                ("untiered", Some(100.0))
            ]
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].group, "P0 · Shell");
        assert_eq!(*overall, Some(50.0));
    }

    #[test]
    fn a_prose_status_column_falls_back_to_percentages() {
        let md = "# UI parity\n**Dimension score: ~55%**\n## Panels\n| Panel | Status | % |\n|---|---|---|\n\
            | Layers | works with a few gaps | 80% |\n| Swatches | most of the menu | 60% |\n| Brushes | needs work | 40% |\n\n\
            ## Weighted\n| Area | Weight | Done |\n|---|---|---|\n| A | 5 | 9 |\n";
        let d = Doc::parse("docs/ui-parity.md", md);
        let h = hand_written(&d);
        assert_eq!(h.score.get().map(|p| p.value), Some(55.0));
        assert!(h.counts.is_none());
        assert_eq!(
            h.pct_rows
                .iter()
                .map(|r| r.status.as_str())
                .collect::<Vec<_>>(),
            ["80%", "60%", "40%"]
        );
        let d = Doc::parse(
            "docs/x-parity.md",
            "# X\n| A | Weight |\n|---|---|\n| a | 5 |\n",
        );
        assert_eq!(
            hand_written(&d).why.as_deref(),
            Some("no status column recognized")
        );
    }

    #[test]
    fn a_hand_written_row_is_named_by_its_first_column() {
        let md = "# Behaviour parity\n| Behaviour | Status | Evidence |\n|---|---|---|\n\
            | Snap to grid | done | test |\n| Smart guides | missing | none yet |\n| Rulers | partial | top only |\n";
        let h = hand_written(&Doc::parse("docs/ui-parity.md", md));
        let rows: Vec<(&str, &str, &str)> = h
            .rows
            .iter()
            .map(|r| (r.status.as_str(), r.text.as_str(), r.notes.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("missing", "Smart guides", "none yet"),
                ("partial", "Rulers", "top only")
            ]
        );
    }

    #[test]
    fn a_linked_hand_written_doc_keeps_its_summary() {
        let rm = "# R\n## Headline numbers\n| | Value | Kind |\n|---|---|---|\n\
            | Feature breadth | 60% | measured: [ui](docs/ui-parity.md) |\n";
        let ui = "# UI parity\n**Dimension score: ~55%**\n| Panel | Status |\n|---|---|\n| Layers | done |\n| Swatches | missing |\n";
        let p = read_all(&[
            Input {
                path: ROADMAP,
                oid: "r",
                text: rm,
            },
            Input {
                path: "docs/ui-parity.md",
                oid: "u",
                text: ui,
            },
        ]);
        assert_eq!(p.breadth_links, ["docs/ui-parity.md"]);
        assert!(p.checklists.is_empty());
        let Some(d) = p.docs.first() else {
            panic!("{:?}", p.docs)
        };
        assert_eq!(d.score.get().map(|s| s.value), Some(55.0));
        assert_eq!(d.counts.as_ref().map(|c| (c.done, c.missing)), Some((1, 1)));
    }

    #[test]
    fn a_stated_score_may_follow_its_bold_label() {
        let score = |md: &str| hand_written(&Doc::parse("docs/x-parity.md", md)).score;
        assert_eq!(
            score("# X\n**Summary (estimated):** ~50% ready. Pens work.\n")
                .get()
                .map(|p| p.value),
            Some(50.0)
        );
        assert_eq!(
            score("# X\n- **Overall (weighted):** **~86%** (menus complete)\n")
                .get()
                .map(|p| p.value),
            Some(86.0)
        );
        assert_eq!(
            score("# X\n**Overall: 74.5%**\n").get().map(|p| p.value),
            Some(74.5)
        );
        assert!(
            !score("# X\n**Summary (estimated):** breadth ~90%, ready for work ~45%\n").is_ok(),
            "a percentage after other words is not the score"
        );
    }

    #[test]
    fn toml_counts_what_is_in_scope() {
        let toml = "[schema]\nversion = 1\n\n[[feature]]\nid = \"a\"\ntitle = \"A\"\narea = \"A\"\ntier = \"P0\"\nstatus = \"shipped\"\n\n\
            [[feature]]\nid = \"b\"\ntitle = \"B\"\narea = \"A\"\ntier = \"P0\"\nstatus = \"partial\"\nnotes = \"lacks x\"\n\n\
            [[feature]]\nid = \"c\"\ntitle = \"C\"\narea = \"B\"\ntier = \"P1\"\nstatus = \"planned\"\n\n\
            [[feature]]\nid = \"d\"\ntitle = \"D\"\narea = \"B\"\ntier = \"N/A\"\nstatus = \"na\"\n";
        let rm = "# R\n## Headline numbers\n| | Value | Kind |\n|---|---|---|\n| Feature breadth | 50% (1 shipped + 1 partial of 4) | measured |\n";
        let p = read_all(&[
            Input {
                path: ROADMAP,
                oid: "r",
                text: rm,
            },
            Input {
                path: "parity/features.toml",
                oid: "t",
                text: toml,
            },
        ]);
        let Some(Checklist::Tiered { tiers, rows, .. }) = p.checklists.first() else {
            panic!()
        };
        assert_eq!(
            tiers
                .iter()
                .map(|t| (t.name.as_str(), t.done, t.partial, t.missing))
                .collect::<Vec<_>>(),
            [("P0", 1, 1, 0), ("P1", 0, 0, 1)]
        );
        assert_eq!(rows[0].line, 12);
        assert!(p.notes.iter().any(|n| n
            .text
            .contains("1 shipped + 1 partial of 3 in scope; ROADMAP.md says 1 + 1 of 4")));
    }

    #[test]
    fn a_presence_catalog_with_an_items_column() {
        let md = "# Parity checklist\n**Menu breadth: 3 / 5 items (60%)**\n\n| Menu | Items | Covered | % |\n|---|---|---|---|\n\
            | File | 3 | 2 | 66% |\n| Edit | 2 | 1 | 50% |\n\n## Not yet covered\n\n- File > Import...\n- Edit › Paste Special\n";
        let p = checklist(md);
        let Some(Checklist::Presence {
            total,
            groups,
            missing,
            ..
        }) = p.checklists.first()
        else {
            panic!()
        };
        assert_eq!(total.get().map(|f| (f.done, f.total)), Some((3.0, 5.0)));
        assert_eq!(
            groups
                .iter()
                .map(|g| (g.name.as_str(), g.done, g.total))
                .collect::<Vec<_>>(),
            [("File", 2.0, 3.0), ("Edit", 1.0, 2.0)]
        );
        assert_eq!(
            missing.iter().map(|m| m.group.as_str()).collect::<Vec<_>>(),
            ["File", "Edit"]
        );
    }

    #[test]
    fn a_catalog_and_a_scorecard_are_two_checklists() {
        let sc = "# Scorecard\n## Summary\n| Area | Done | Partial | Missing |\n|---|---|---|---|\n| Performance | 1 | 1 | 2 |\n\n\
            ## Performance\n| Id | Scenario | Status |\n|---|---|---|\n| P1 | Move | over budget (x) |\n| P2 | Pan | pass |\n| P3 | Zoom | not run |\n| P4 | Open | tracked |\n";
        let cat = "# Menu parity\n**631 of 631 menu items live (100.0%).**\n| Menu | Live | Total |\n|---|---|---|\n| File | 52 | 52 |\n";
        let p = read_all(&[
            Input {
                path: ROADMAP,
                oid: "r",
                text: "# R\n",
            },
            Input {
                path: "docs/parity-checklist.md",
                oid: "c",
                text: cat,
            },
            Input {
                path: "docs/scorecard.md",
                oid: "s",
                text: sc,
            },
        ]);
        assert_eq!(p.checklists.len(), 2);
        let Some(Checklist::Status { groups, .. }) = p.checklists.get(1) else {
            panic!()
        };
        let g = &groups[0];
        assert_eq!(
            (g.done, g.partial, g.missing, g.not_measured, g.unrecognized),
            (1, 1, 1, 1, 0)
        );
        assert!(p.notes.iter().any(|n| n
            .text
            .starts_with("Performance: the generated summary says 1/1/2")));
    }
}
