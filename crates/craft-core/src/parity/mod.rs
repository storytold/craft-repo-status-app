//! A repo's parity with its target app, read from its parity documents:
//! `ROADMAP.md`, `docs/target-app-parity.md`, `docs/gaps.md`, the machine
//! checklists and every `docs/*-parity.md`. Pure: `craft-github` fetches
//! the text, and only when a file's blob changes.
//!
//! Every value is a [`Cell`] (or a [`Figure`] for the headline
//! percentages): read with the line it came from, absent with the reason,
//! or present but unreadable with the raw text. A value taken from a
//! fallback source names that source.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

mod areas;
mod checklist;
mod dims;
mod gaps;
mod headline;
pub mod md;
pub mod value;

pub use checklist::linked_paths;

/// Changes whenever the parser reads a file differently, so that text read
/// by an older parser is read again.
pub const PARSER_VERSION: u32 = 2;

/// The file the headline numbers come from.
pub const ROADMAP: &str = "ROADMAP.md";
/// The document a click on a Repos parity number opens.
pub const PARITY_DOC: &str = "docs/target-app-parity.md";
pub const GAPS: &str = "docs/gaps.md";
pub const README: &str = "README.md";
/// Most missing checklist rows a report keeps per checklist.
pub const MAX_ROWS: usize = 500;

/// Where a value was read: a path in the repo and a line from 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Src {
    pub path: String,
    pub line: u32,
}

impl Src {
    pub fn new(path: &str, line: u32) -> Src {
        Src {
            path: path.to_string(),
            line,
        }
    }
}

/// A parsed value: read, missing from the document, or present but not in a
/// form the parser reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Cell<T> {
    Ok {
        v: T,
        src: Src,
        /// The source when the value does not come from the place that
        /// names it, such as "from prose" or "computed".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    Absent {
        why: String,
    },
    Unreadable {
        why: String,
        raw: String,
        src: Src,
    },
}

impl<T> Default for Cell<T> {
    fn default() -> Self {
        Cell::Absent {
            why: "Not read yet".into(),
        }
    }
}

impl<T> Cell<T> {
    pub fn ok(v: T, src: Src) -> Cell<T> {
        Cell::Ok { v, src, from: None }
    }

    pub fn absent(why: impl Into<String>) -> Cell<T> {
        Cell::Absent { why: why.into() }
    }

    pub fn get(&self) -> Option<&T> {
        match self {
            Cell::Ok { v, .. } => Some(v),
            _ => None,
        }
    }

    pub fn src(&self) -> Option<&Src> {
        match self {
            Cell::Ok { src, .. } | Cell::Unreadable { src, .. } => Some(src),
            Cell::Absent { .. } => None,
        }
    }

    /// The same cell with `from` as its source label.
    pub fn from(self, label: impl Into<String>) -> Cell<T> {
        match self {
            Cell::Ok { v, src, .. } => Cell::Ok {
                v,
                src,
                from: Some(label.into()),
            },
            other => other,
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Cell::Ok { .. })
    }
}

/// The basis a number's Kind or How cell states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Measured,
    /// Partly measured, or measured bounds with an estimate between them.
    Partly,
    Estimated,
    #[default]
    Unknown,
}

/// A percentage as written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Pct {
    pub value: f64,
    /// Written with `~` or `≈`.
    pub approx: bool,
    /// The range written next to it, such as `(57–67%)`.
    pub low: Option<f64>,
    pub high: Option<f64>,
    pub basis: Basis,
    /// The cell's text without markup.
    pub raw: String,
    /// "or more" for `100%+`, or how a computed value was computed.
    pub note: Option<String>,
    /// Computed from a fraction such as `487 of 502`.
    pub computed: bool,
}

/// A score: a percentage, or "ahead of" or "beyond" the target app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Score {
    Pct(Pct),
    Ahead { text: String },
}

impl Score {
    pub fn pct(&self) -> Option<f64> {
        match self {
            Score::Pct(p) => Some(p.value),
            Score::Ahead { .. } => None,
        }
    }
}

/// An hours estimate: a range, or one number in both ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Hours {
    pub lo: u32,
    pub hi: u32,
    pub approx: bool,
    /// "one agent", "wall-clock" or "with N agents", when the text states it.
    pub basis: Option<String>,
    /// The rest of the cell; never added into the range.
    pub note: Option<String>,
    pub raw: String,
    /// `(in N)`: the hours are counted inside gap or row N.
    pub counted_in: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Fraction {
    pub done: f64,
    pub total: f64,
}

/// One headline percentage: read, missing from the document, or present but
/// not in a form the parser reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Figure {
    Ok {
        /// The percentage, 0 to 100 (more when the document says so).
        pct: f64,
        /// The range written next to it, such as `(57–67%)`.
        low: Option<f64>,
        high: Option<f64>,
        /// The row's label and its Value and Kind cells, without markup.
        label: String,
        text: String,
        kind: String,
        basis: Basis,
        /// The file and its line, from 1.
        path: String,
        line: u32,
        /// Where the number came from when it is not the row it names, such
        /// as the "Readiness by audience" table standing in for Ready.
        from: Option<String>,
    },
    Absent {
        why: String,
    },
    Unreadable {
        why: String,
        raw: String,
        path: String,
        line: u32,
    },
}

impl Default for Figure {
    fn default() -> Self {
        Figure::Absent {
            why: "Not read yet".into(),
        }
    }
}

impl Figure {
    pub fn pct(&self) -> Option<f64> {
        match self {
            Figure::Ok { pct, .. } => Some(*pct),
            _ => None,
        }
    }

    pub fn absent(why: impl Into<String>) -> Figure {
        Figure::Absent { why: why.into() }
    }
}

/// The stage line: the current stage and the steps to the next ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Stage {
    pub name: String,
    pub steps: Vec<StageStep>,
}

/// One clause of the stage line's `next:` text, such as `beta ~28 points
/// and ~420–750 h`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StageStep {
    pub stage: String,
    /// Points of "ready for real work" still to go, from `~N points` or `~N%`.
    pub points: Option<f64>,
    pub hours: Option<Hours>,
    pub text: String,
}

/// One parity document and its dates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DocFile {
    pub path: String,
    pub oid: String,
    /// The newest `Last reviewed` and `Last updated` dates, `YYYY-MM-DD`.
    pub reviewed: Option<String>,
    pub updated: Option<String>,
    /// The newest date on a `> **Generated:**` line.
    pub generated: Option<String>,
    /// The `Target:` field of the date line with the newest Last updated.
    pub target: Option<String>,
    /// How many date lines the file has.
    pub date_lines: u32,
    /// The line of the date line with the newest Last updated.
    pub line: Option<u32>,
    /// Set when the file has no date line.
    pub why: Option<String>,
    /// The last commit to the path: its date (RFC 3339) and id.
    pub commit_date: Option<String>,
    pub commit: Option<String>,
}

/// The dimension groups the overview has a column for (the last four and
/// agent control share the Other column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Canonical {
    Features,
    UiUx,
    FileFormats,
    TypeLayout,
    Hardware,
    Localization,
    Performance,
    Stability,
    Platforms,
    Ecosystem,
    Ai,
    AgentControl,
    Collaboration,
    Standards,
    #[default]
    Other,
}

impl Canonical {
    /// The overview column it is shown in.
    pub fn column(self) -> Canonical {
        match self {
            Canonical::AgentControl | Canonical::Collaboration | Canonical::Standards => {
                Canonical::Other
            }
            c => c,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Canonical::Features => "features",
            Canonical::UiUx => "ui_ux",
            Canonical::FileFormats => "file_formats",
            Canonical::TypeLayout => "type_layout",
            Canonical::Hardware => "hardware",
            Canonical::Localization => "localization",
            Canonical::Performance => "performance",
            Canonical::Stability => "stability",
            Canonical::Platforms => "platforms",
            Canonical::Ecosystem => "ecosystem",
            Canonical::Ai => "ai",
            Canonical::AgentControl => "agent_control",
            Canonical::Collaboration => "collaboration",
            Canonical::Standards => "standards",
            Canonical::Other => "other",
        }
    }
}

/// Which score a dimension row fills when two rows share a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Fills {
    #[default]
    Ready,
    Breadth,
}

/// A link in a document: its text, its target as written, and the in-repo
/// path it resolves to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Link {
    pub text: String,
    pub url: String,
    pub path: Option<String>,
}

/// One dimension of "ready for real work".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Dim {
    pub name: String,
    pub group: Canonical,
    pub weight: Option<f64>,
    /// Where the weight came from when not this row's Weight cell.
    pub weight_src: Option<String>,
    pub ready: Cell<Score>,
    pub breadth: Cell<Score>,
    pub hours: Cell<Hours>,
    pub kind: String,
    pub doc: Option<Link>,
    pub counted: Option<String>,
    /// A `↳` row under the row above; left out of sums.
    pub child: bool,
    pub fills: Fills,
    /// `"w"` or `"d"` when the row is in only the weight table or only the
    /// By-dimension table.
    pub only_in: Option<String>,
    pub src: Src,
    /// Weighted points lost: weight share × (100 − ready).
    pub shortfall: Option<f64>,
}

/// One heat cell of the overview: the dimension rows of one column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Heat {
    pub column: Canonical,
    /// The ready percentage: one row's, or the weighted mean of several.
    pub value: Option<f64>,
    /// What the cell shows: `75%`, `ahead`, or for Other the row's name too.
    pub text: String,
    /// The rows it comes from, as `name: score`.
    pub rows: Vec<String>,
    pub src: Option<Src>,
}

/// The dimension that loses the most weighted points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Drag {
    pub name: String,
    pub group: Canonical,
    pub points: f64,
}

/// Where an area's Missing chips came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ChipsFrom {
    /// A marker such as `Missing:` or `remaining:`.
    Marker,
    /// A column of gaps, used whole.
    GapsColumn,
    /// Clauses with a negative word, from a mixed column.
    NegativeClauses,
    #[default]
    None,
}

/// One missing item from an area's gaps text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Chip {
    pub text: String,
    /// The parenthetical removed from the text, kept for the tooltip.
    pub hover: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AreaCounts {
    pub features: Option<f64>,
    pub shipped: Option<f64>,
    pub partial: Option<f64>,
}

/// One feature area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Area {
    pub name: String,
    pub full_name: String,
    pub key: String,
    pub weight: Option<f64>,
    /// "weights from prose, file:line" when not from a Weight column.
    pub weight_src: Option<String>,
    pub breadth: Cell<Score>,
    pub ready: Cell<Score>,
    /// The header ready was read from when it was not Ready (Done, Parity, %).
    pub ready_from: Option<String>,
    pub hours: Cell<Hours>,
    pub missing: Vec<Chip>,
    /// Chips beyond the twelve shown.
    pub more: u32,
    pub chips_from: ChipsFrom,
    /// The gaps cell as written, the authoritative text.
    pub raw_gaps: String,
    pub gaps_header: Option<String>,
    pub unweighted: bool,
    /// Lines of the rows with the same key, when listed twice.
    pub dup_lines: Vec<u32>,
    /// Both values when the duplicate rows disagree.
    pub conflict: Option<String>,
    pub kind: String,
    pub doc: Option<Link>,
    pub counts: Option<AreaCounts>,
    pub src: Src,
    pub shortfall: Option<f64>,
}

/// A summary row other than the first one, such as "Core editor only".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AltTotal {
    pub label: String,
    pub score: Cell<Score>,
}

/// Whether a gap blocks a stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Flag {
    Yes,
    No,
    /// Only part of the gap blocks, as the text says.
    Partial(String),
    #[default]
    Unknown,
}

/// One gap from `docs/gaps.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Gap {
    /// Stable across reads: the id when the document's ids are stable, else
    /// the normalized title (with `#rank` on a collision).
    pub key: String,
    pub id: String,
    /// Row order in the ranked list, from 1; 0 for unranked gaps.
    pub rank: u32,
    /// The section heading of an unranked gap.
    pub group: Option<String>,
    pub title: String,
    pub alpha: Flag,
    pub beta: Flag,
    /// Where the flags came from: column, marker, impact or prose.
    pub blocks_src: Option<String>,
    pub scope_note: Option<String>,
    pub priority: Option<String>,
    pub kinds: Vec<String>,
    pub hours: Cell<Hours>,
    pub counted_in: Option<String>,
    pub doc: Option<Link>,
    pub workstream: Option<String>,
    pub src: Src,
    /// The gap's own heading further down, when it has one.
    pub detail_src: Option<Src>,
    /// The first `**Missing:**` bullet under that heading.
    pub detail: Option<String>,
    pub closed: bool,
    /// Section rows paired with this Top-N row (or the Top-N row a section
    /// row is paired with).
    pub paired_with: Vec<String>,
    pub issues: Vec<u32>,
}

/// Sums over the open gaps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GapTotals {
    /// Open gaps that block beta, not counted inside another gap.
    pub beta: Option<HoursSum>,
    /// Gaps that block beta in part.
    pub partial: Option<HoursSum>,
    pub alpha: u32,
    /// "alpha blockers: none".
    pub alpha_none: bool,
    /// The totals sentence after the ranked table.
    pub stated: Cell<Hours>,
    pub ids_stable: bool,
    /// "table" or "headings"; empty without gaps.
    pub form: String,
    /// No gap was read, every gap's beta is unknown, or there is no
    /// `docs/gaps.md`.
    pub beta_unknown: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HoursSum {
    pub count: u32,
    pub lo: u32,
    pub hi: u32,
    /// Gaps without hours.
    pub unknown: u32,
}

/// Status counts for one tier, group or document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Counts {
    pub name: String,
    pub done: u32,
    pub partial: u32,
    pub missing: u32,
    pub excluded: u32,
    pub ahead: u32,
    pub not_measured: u32,
    pub unrecognized: u32,
    /// (done + ½ partial) / in scope, in percent.
    pub pct: Option<f64>,
}

/// One partial or missing checklist row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CheckRow {
    pub status: String,
    /// The tier, menu or section.
    pub group: String,
    pub text: String,
    pub notes: String,
    pub line: u32,
}

/// A group of a presence catalog: a menu or tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Group {
    pub name: String,
    pub done: f64,
    pub total: f64,
    pub line: u32,
}

/// A machine checklist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Checklist {
    /// Rows with a tier and a status, or TOML feature records.
    Tiered {
        path: String,
        tiers: Vec<Counts>,
        overall: Option<f64>,
        rows: Vec<CheckRow>,
        more: u32,
    },
    /// A catalog of menu or ribbon items that exist.
    Presence {
        path: String,
        total: Cell<Fraction>,
        groups: Vec<Group>,
        missing: Vec<CheckRow>,
        more: u32,
    },
    /// Tables with an id and a status, such as a scorecard.
    Status {
        path: String,
        groups: Vec<Counts>,
        overall: Option<f64>,
        rows: Vec<CheckRow>,
        more: u32,
    },
}

/// A hand-written `docs/*-parity.md`, shown in the detail pane only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DocChecklist {
    pub path: String,
    pub score: Cell<Pct>,
    pub counts: Option<Counts>,
    pub rows: Vec<CheckRow>,
    /// Rows with a per-row percentage, when there is no status column.
    pub pct_rows: Vec<CheckRow>,
    /// Why nothing was recognized.
    pub why: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    #[default]
    Info,
    Warn,
}

/// A note the parser adds about a document: a duplicate row, a sum that does
/// not add up, two sources that disagree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Note {
    pub level: Level,
    pub text: String,
    pub src: Option<Src>,
}

impl Note {
    pub fn info(text: impl Into<String>, src: Src) -> Note {
        Note {
            level: Level::Info,
            text: text.into(),
            src: Some(src),
        }
    }

    pub fn warn(text: impl Into<String>, src: Src) -> Note {
        Note {
            level: Level::Warn,
            text: text.into(),
            src: Some(src),
        }
    }
}

/// What the app shows from a repo's parity documents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Parity {
    /// The [`PARSER_VERSION`] that read it.
    pub version: u32,
    /// The commit the files were read at, which links point to.
    pub commit: Option<String>,
    /// The blob of `ROADMAP.md` that was read; `None` without one.
    pub roadmap_oid: Option<String>,
    /// Whether [`PARITY_DOC`] exists at that commit.
    pub parity_doc: bool,
    /// Why the repo has no parity numbers: neither `ROADMAP.md` nor
    /// [`PARITY_DOC`] exists.
    pub na: Option<String>,
    pub breadth: Figure,
    pub ready: Figure,
    /// The newest `Last reviewed` date in `ROADMAP.md`, `YYYY-MM-DD`.
    pub last_reviewed: Option<String>,
    /// The other Feature breadth rows.
    pub breadth_alts: Vec<Figure>,
    pub menu_breadth: Figure,
    /// A menu fraction in the breadth row, for comparing with the checklist.
    pub menu_hint: Option<Fraction>,
    /// The in-repo paths the breadth rows' Kind cells link to, the shown
    /// row's first.
    pub breadth_links: Vec<String>,
    pub mainstream: Figure,
    pub essentials: Figure,
    pub to_alpha: Cell<Hours>,
    pub to_beta: Cell<Hours>,
    pub to_full: Cell<Hours>,
    pub stage: Cell<Stage>,
    /// The documents read, each with its blob id and dates.
    pub files: Vec<DocFile>,
    /// The oldest Last reviewed among ROADMAP.md, the parity doc and gaps.md.
    pub reviewed: Option<String>,
    pub dims: Vec<Dim>,
    /// One cell per overview column that has dimension rows.
    pub heat: Vec<Heat>,
    pub dims_total: Cell<Score>,
    pub top_drag: Cell<Drag>,
    pub areas: Vec<Area>,
    pub areas_total: Cell<Score>,
    pub area_alts: Vec<AltTotal>,
    /// The sum of the area weights, each key counted once.
    pub area_weight_sum: Option<f64>,
    /// Why the areas have no weights.
    pub area_weights_why: Option<String>,
    pub gaps: Vec<Gap>,
    pub gap_totals: GapTotals,
    pub checklists: Vec<Checklist>,
    pub docs: Vec<DocChecklist>,
    pub notes: Vec<Note>,
}

impl Parity {
    /// The documents the breadth row links to that `files` (blob ids by
    /// path) does not hold: `.md` and `.toml` paths inside the repo.
    pub fn linked_outside(&self, files: &BTreeMap<String, String>) -> Vec<String> {
        linked_paths(self)
            .into_iter()
            .filter(|p| !files.contains_key(p) && is_document(p))
            .collect()
    }

    /// Whether this parser read this report at commit `head` from exactly
    /// `files` (blob ids by path) and the documents the breadth row links
    /// to. `files` does not hold a linked document's blob id, so a report
    /// that links outside `files` is current only at the commit it was read
    /// at.
    pub fn is_current(&self, files: &BTreeMap<String, String>, head: Option<&str>) -> bool {
        let linked = self.linked_outside(files);
        self.version == PARSER_VERSION
            && (linked.is_empty() || (head.is_some() && self.commit.as_deref() == head))
            && files
                .iter()
                .all(|(path, oid)| self.files.iter().any(|f| &f.path == path && &f.oid == oid))
            && self
                .files
                .iter()
                .all(|f| files.contains_key(&f.path) || linked.contains(&f.path))
    }
}

/// Whether a linked path names a document the parser reads.
fn is_document(path: &str) -> bool {
    !path.contains("..") && (path.ends_with(".md") || path.ends_with(".toml"))
}

/// One document's text, for [`read`].
pub struct Input<'a> {
    pub path: &'a str,
    pub oid: &'a str,
    pub text: &'a str,
}

/// The parity report from `files` (every parity document the repo has).
/// The caller fills in [`Parity::commit`] and each file's last commit.
pub fn read(files: &[Input]) -> Parity {
    let get = |p: &str| files.iter().find(|f| f.path == p);
    let roadmap = get(ROADMAP);
    let tap = get(PARITY_DOC);
    let mut out = Parity {
        version: PARSER_VERSION,
        parity_doc: tap.is_some(),
        roadmap_oid: roadmap.map(|f| f.oid.to_string()),
        ..Default::default()
    };
    let docs: Vec<(md::Doc, &str)> = files
        .iter()
        .filter(|f| f.path.ends_with(".md"))
        .map(|f| (md::Doc::parse(f.path, f.text), f.oid))
        .collect();
    let doc = |p: &str| docs.iter().find(|(d, _)| d.path == p).map(|(d, _)| d);
    for (d, oid) in &docs {
        if d.path == README && roadmap.is_some() {
            continue;
        }
        out.files.push(headline::doc_file(d, oid));
    }
    for f in files.iter().filter(|f| !f.path.ends_with(".md")) {
        out.files.push(DocFile {
            path: f.path.to_string(),
            oid: f.oid.to_string(),
            ..Default::default()
        });
    }
    out.stage = headline::stage(doc(ROADMAP), doc(README));
    if roadmap.is_none() && tap.is_none() {
        let why = format!("No {ROADMAP} or {PARITY_DOC} at HEAD");
        out.na = Some(why.clone());
        out.breadth = Figure::absent(&why);
        out.ready = Figure::absent(&why);
        return out;
    }
    let (rm, tap_doc) = (doc(ROADMAP), doc(PARITY_DOC));
    out.last_reviewed = out
        .files
        .iter()
        .find(|f| f.path == ROADMAP)
        .and_then(|f| f.reviewed.clone());
    headline::headline(&mut out, rm, tap_doc);
    headline::date_notes(&mut out, &docs.iter().map(|(d, _)| d).collect::<Vec<_>>());
    dims::read(&mut out, rm, tap_doc);
    areas::read(&mut out, rm, tap_doc);
    gaps::read(&mut out, doc(GAPS));
    checklist::read(
        &mut out,
        files,
        &docs.iter().map(|(d, _)| d).collect::<Vec<_>>(),
    );
    headline::cross_checks(&mut out);
    for (d, _) in &docs {
        if d.path == README {
            continue;
        }
        out.notes.extend(d.notes.iter().cloned());
    }
    out.reviewed = [ROADMAP, PARITY_DOC, GAPS]
        .iter()
        .filter_map(|p| out.files.iter().find(|f| f.path == *p)?.reviewed.clone())
        .min();
    out
}

/// The text a score shows: `75%` or `ahead`.
pub fn score_text(s: &Cell<Score>) -> String {
    match s.get() {
        Some(Score::Pct(p)) => fmt_pct(p.value),
        Some(Score::Ahead { .. }) => "ahead".into(),
        None => "—".into(),
    }
}

pub fn fmt_pct(v: f64) -> String {
    format!("{}%", (v * 10.0).round() / 10.0)
}

pub fn hours_text(h: &Cell<Hours>) -> String {
    match h.get() {
        Some(h) if h.lo == h.hi => format!("{} h", h.lo),
        Some(h) => format!("{}–{} h", h.lo, h.hi),
        None => "—".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repo_without_documents_is_not_applicable() {
        let p = read(&[Input {
            path: README,
            oid: "r1",
            text: "# Lib\n\n> **Status:** early alpha, APIs move\n",
        }]);
        assert_eq!(
            p.na.as_deref(),
            Some("No ROADMAP.md or docs/target-app-parity.md at HEAD")
        );
        assert_eq!(p.stage.get().map(|s| s.name.as_str()), Some("early alpha"));
        assert!(matches!(p.ready, Figure::Absent { .. }));
        let none = read(&[]);
        assert_eq!(none.na, p.na);
        assert!(none.files.is_empty());
    }

    #[test]
    fn a_report_is_current_only_for_the_same_blobs_and_parser() {
        let rm = "## Headline numbers\n| | Value |\n|---|---|\n| Feature breadth | 50% |\n";
        let p = read(&[
            Input {
                path: ROADMAP,
                oid: "a1",
                text: rm,
            },
            Input {
                path: PARITY_DOC,
                oid: "b1",
                text: "# Parity\n",
            },
        ]);
        let files = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(p, o)| (p.to_string(), o.to_string()))
                .collect()
        };
        let head = Some("h1");
        assert!(p.is_current(&files(&[(ROADMAP, "a1"), (PARITY_DOC, "b1")]), head));
        assert!(
            p.is_current(&files(&[(ROADMAP, "a1"), (PARITY_DOC, "b1")]), Some("h2")),
            "a commit that changes no document"
        );
        assert!(
            !p.is_current(&files(&[(ROADMAP, "a2"), (PARITY_DOC, "b1")]), head),
            "ROADMAP.md changed"
        );
        assert!(
            !p.is_current(&files(&[(ROADMAP, "a1")]), head),
            "the parity document was removed"
        );
        assert!(
            !p.is_current(
                &files(&[(ROADMAP, "a1"), (PARITY_DOC, "b1"), ("docs/x.md", "c1")]),
                head
            ),
            "a path the report did not read"
        );
        let older = Parity {
            version: PARSER_VERSION - 1,
            ..p.clone()
        };
        assert!(!older.is_current(&files(&[(ROADMAP, "a1"), (PARITY_DOC, "b1")]), head));
        assert!(
            read(&[]).is_current(&BTreeMap::new(), head),
            "no documents, still none"
        );
    }

    #[test]
    fn a_report_with_linked_documents_is_current_at_its_commit() {
        let rm = "## Headline numbers\n| | Value | Kind |\n|---|---|---|\n\
                  | Feature breadth | 60% | measured: [menus](docs/menus.md), [code](src/lib.rs) |\n";
        let mut p = read(&[
            Input {
                path: ROADMAP,
                oid: "a1",
                text: rm,
            },
            Input {
                path: "docs/menus.md",
                oid: "m1",
                text: "# Menus\n",
            },
        ]);
        p.commit = Some("h1".into());
        let files = BTreeMap::from([(ROADMAP.to_string(), "a1".to_string())]);
        assert_eq!(
            p.linked_outside(&files),
            ["docs/menus.md"],
            "not src/lib.rs"
        );
        assert!(p.is_current(&files, Some("h1")));
        assert!(
            !p.is_current(&files, Some("h2")),
            "the linked document may have changed"
        );
        assert!(!p.is_current(&files, None));
        // A linked document the probe also lists is compared by its blob.
        let both = BTreeMap::from([
            (ROADMAP.to_string(), "a1".to_string()),
            ("docs/menus.md".to_string(), "m1".to_string()),
        ]);
        assert!(p.linked_outside(&both).is_empty());
        assert!(p.is_current(&both, Some("h2")));
    }

    #[test]
    fn a_report_survives_the_cache() {
        let rm = "## Headline numbers\n| | Value | Kind |\n|---|---|---|\n\
                  | Feature breadth | ~70% | estimated |\n| Ready for real work | **~62%** (57–67%) | estimated |\n";
        let full = read(&[Input {
            path: ROADMAP,
            oid: "a",
            text: rm,
        }]);
        assert_eq!(full.ready.pct(), Some(62.0));
        let text = serde_json::to_string(&full).unwrap();
        let back: Parity = serde_json::from_str(&text).unwrap();
        assert_eq!(back, full);
    }
}
