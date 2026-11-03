//! A repo's parity with its target app, read from its parity documents:
//! `ROADMAP.md` and `docs/target-app-parity.md`. Pure: `craft-github`
//! fetches the text, and only when a file's blob changes.
//!
//! Every value is a [`Cell`] (or a [`Figure`] for the headline
//! percentages): read with the line it came from, absent with the reason,
//! or present but unreadable with the raw text. A value taken from a
//! fallback source names that source.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

mod headline;
pub mod md;
pub mod value;

/// Changes whenever the parser reads a file differently, so that text read
/// by an older parser is read again.
pub const PARSER_VERSION: u32 = 1;

/// The file the headline numbers come from.
pub const ROADMAP: &str = "ROADMAP.md";
/// The document a click on a Repos parity number opens.
pub const PARITY_DOC: &str = "docs/target-app-parity.md";
pub const README: &str = "README.md";

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
    /// A menu fraction in the breadth row.
    pub menu_hint: Option<Fraction>,
    pub mainstream: Figure,
    pub essentials: Figure,
    pub to_alpha: Cell<Hours>,
    pub to_beta: Cell<Hours>,
    pub to_full: Cell<Hours>,
    pub stage: Cell<Stage>,
    /// The documents read, each with its blob id and dates.
    pub files: Vec<DocFile>,
    pub notes: Vec<Note>,
}

impl Parity {
    /// Whether this parser read this report from exactly `files` (blob ids
    /// by path).
    pub fn is_current(&self, files: &BTreeMap<String, String>) -> bool {
        self.version == PARSER_VERSION
            && self.files.len() == files.len()
            && self
                .files
                .iter()
                .all(|f| files.get(&f.path) == Some(&f.oid))
    }
}

/// One document's text, for [`read`].
pub struct Input<'a> {
    pub path: &'a str,
    pub oid: &'a str,
    pub text: &'a str,
}

/// The parity report from `files` (every parity document the repo has).
/// The caller fills in [`Parity::commit`].
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
    for (d, _) in &docs {
        if d.path == README {
            continue;
        }
        out.notes.extend(d.notes.iter().cloned());
    }
    out
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
        assert!(p.is_current(&files(&[(ROADMAP, "a1"), (PARITY_DOC, "b1")])));
        assert!(
            !p.is_current(&files(&[(ROADMAP, "a2"), (PARITY_DOC, "b1")])),
            "ROADMAP.md changed"
        );
        assert!(
            !p.is_current(&files(&[(ROADMAP, "a1")])),
            "the parity document was removed"
        );
        assert!(
            !p.is_current(&files(&[
                (ROADMAP, "a1"),
                (PARITY_DOC, "b1"),
                ("docs/x.md", "c1")
            ])),
            "a path the report did not read"
        );
        let older = Parity {
            version: PARSER_VERSION - 1,
            ..p.clone()
        };
        assert!(!older.is_current(&files(&[(ROADMAP, "a1"), (PARITY_DOC, "b1")])));
        assert!(
            read(&[]).is_current(&BTreeMap::new()),
            "no documents, still none"
        );
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
