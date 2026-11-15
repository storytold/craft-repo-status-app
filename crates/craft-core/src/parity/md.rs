//! The Markdown scanner every parity parser reads through: numbered lines
//! without fenced code, headings with the sections they open, tables with
//! their cells, and paragraphs and list items joined with their
//! continuation lines.

use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use super::{Note, Src};

/// Words that mark a heading's section as history or archive. Its tables,
/// and those of every heading nested under it, are left out of searches.
const EXCLUDED: &[&str] = &[
    "stale",
    "superseded",
    "earlier",
    "appendix",
    "audit history",
    "closed recently",
    "revision history",
    "detailed missing items",
    "honest assessment",
    "history of the numbers",
    "kept for the record",
];

/// A parsed document.
pub struct Doc {
    pub path: String,
    /// The parsable region: from the first line to before a second
    /// level-1 heading, without fenced code.
    pub lines: Vec<Line>,
    pub headings: Vec<Heading>,
    pub tables: Vec<Table>,
    pub notes: Vec<Note>,
    /// The last line number of the file.
    pub last_line: u32,
}

pub struct Line {
    pub no: u32,
    pub text: String,
    /// The heading whose section the line is in, as an index into
    /// [`Doc::headings`].
    pub section: Option<usize>,
    pub excluded: bool,
}

pub struct Heading {
    pub line: u32,
    pub level: usize,
    /// The heading text as written, and normalized (see [`normalize`]).
    pub text: String,
    pub norm: String,
    pub parent: Option<usize>,
    pub excluded: bool,
    /// The index in [`Doc::lines`] of the heading line.
    pub at: usize,
    /// The index in [`Doc::lines`] where its section ends (exclusive): the
    /// next heading of the same or a higher level.
    pub end: usize,
}

pub struct Table {
    pub line: u32,
    pub header: Vec<String>,
    /// Header cells lowercased, without markup.
    pub hnorm: Vec<String>,
    pub rows: Vec<Row>,
    pub section: Option<usize>,
    pub excluded: bool,
    /// The index in [`Doc::lines`] of the header line, and of the line after
    /// the table.
    pub at: usize,
    pub end: usize,
}

pub struct Row {
    pub line: u32,
    pub cells: Vec<MdCell>,
    /// Fewer cells than the header: shown, but left out of sums.
    pub malformed: bool,
}

/// One table cell, or one inline span of text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MdCell {
    pub raw: String,
    /// Without bold, italic underscores, strike-through, code ticks or HTML
    /// tags; links keep their text.
    pub text: String,
    /// `(text, url)` of each link.
    pub links: Vec<(String, String)>,
    pub bold: Vec<String>,
    pub struck: Vec<String>,
    /// The text starts with a struck span.
    pub struck_lead: bool,
    pub struck_all: bool,
    /// Every `#N` issue reference.
    pub issues: Vec<u32>,
}

/// A paragraph or a list item, joined with its continuation lines.
pub struct Block {
    pub line: u32,
    pub text: String,
    pub item: bool,
    pub section: Option<usize>,
    pub excluded: bool,
}

static ISSUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#(\d+)").unwrap());
static DATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap());
static ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(?:[-*+]|\d+[.)])\s+").unwrap());

/// `text` as the parsers compare it: NFC, without U+FE0F, a BOM or `\r`.
pub fn clean(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.nfc()
        .filter(|c| *c != '\u{fe0f}' && *c != '\r')
        .collect()
}

impl Doc {
    pub fn parse(path: &str, text: &str) -> Doc {
        let text = clean(text);
        let mut lines = Vec::new();
        // The open fence's marker character and length. It closes on a line
        // of the same character, at least as long, with nothing after it.
        let mut fence: Option<(char, usize)> = None;
        let mut h1 = false;
        let last_line = text.split('\n').count() as u32;
        for (i, line) in text.split('\n').enumerate() {
            let lead = line.trim_start();
            if let Some(m) = lead.chars().next().filter(|c| *c == '`' || *c == '~') {
                let n = lead.chars().take_while(|c| *c == m).count();
                let rest = &lead[n..];
                if n >= 3 {
                    match fence {
                        // A backtick fence's info string holds no backtick.
                        None if m == '~' || !rest.contains('`') => {
                            fence = Some((m, n));
                            continue;
                        }
                        Some((fm, fl)) if fm == m && n >= fl && rest.trim().is_empty() => {
                            fence = None;
                            continue;
                        }
                        _ => {}
                    }
                }
            }
            if fence.is_some() {
                continue;
            }
            if heading(line).is_some_and(|(level, _)| level == 1) {
                if h1 {
                    break;
                }
                h1 = true;
            }
            lines.push(Line {
                no: i as u32 + 1,
                text: line.to_string(),
                section: None,
                excluded: false,
            });
        }
        let mut doc = Doc {
            path: path.to_string(),
            lines,
            headings: Vec::new(),
            tables: Vec::new(),
            notes: Vec::new(),
            last_line,
        };
        doc.find_headings();
        doc.find_tables();
        doc
    }

    fn find_headings(&mut self) {
        let mut open: Vec<usize> = Vec::new();
        for i in 0..self.lines.len() {
            if let Some((level, text)) = heading(&self.lines[i].text) {
                while let Some(&h) = open.last() {
                    if self.headings[h].level >= level {
                        self.headings[h].end = i;
                        open.pop();
                    } else {
                        break;
                    }
                }
                let parent = open.last().copied();
                let norm = normalize(text);
                let excluded = parent.is_some_and(|p| self.headings[p].excluded)
                    || EXCLUDED.iter().any(|w| norm.contains(w));
                self.headings.push(Heading {
                    line: self.lines[i].no,
                    level,
                    text: text.to_string(),
                    norm,
                    parent,
                    excluded,
                    at: i,
                    end: self.lines.len(),
                });
                open.push(self.headings.len() - 1);
            }
            let section = open.last().copied();
            self.lines[i].section = section;
            self.lines[i].excluded = section.is_some_and(|h| self.headings[h].excluded);
        }
    }

    fn find_tables(&mut self) {
        let mut i = 0;
        while i < self.lines.len() {
            let head = &self.lines[i];
            if !head.text.trim_start().starts_with('|') {
                i += 1;
                continue;
            }
            if !self.lines.get(i + 1).is_some_and(|l| is_separator(&l.text)) {
                // Rows right after a table, cut off from it by one stray
                // line, with the table's number of cells: the table goes on.
                let width = cells(&head.text).len();
                let resumes = self.tables.last().filter(|t| {
                    t.end + 1 == i
                        && t.header.len() == width
                        && !self.lines[t.end].text.trim().is_empty()
                });
                if resumes.is_some() {
                    let start = i;
                    let mut j = i;
                    let mut rows = Vec::new();
                    while let Some(line) = self.lines.get(j) {
                        if !line.text.trim_start().starts_with('|') {
                            break;
                        }
                        if !is_separator(&line.text) {
                            let raw = cells(&line.text);
                            rows.push(Row {
                                line: line.no,
                                malformed: raw.len() < width,
                                cells: raw.iter().map(|c| MdCell::new(c)).collect(),
                            });
                        }
                        j += 1;
                    }
                    if let Some(t) = self.tables.last_mut() {
                        t.rows.extend(rows);
                        t.end = j;
                    }
                    self.notes.push(Note::info(
                        format!(
                            "table interrupted by line {}; the rows after it are read as the same table",
                            self.lines[start - 1].no
                        ),
                        Src::new(&self.path, self.lines[start].no),
                    ));
                    i = j;
                    continue;
                }
                // A header line with no separator under it: not a table.
                let prev_is_row = i > 0 && self.lines[i - 1].text.trim_start().starts_with('|');
                if !prev_is_row && !head.excluded && !is_separator(&head.text) {
                    self.notes.push(Note::info(
                        "table header without a separator line; not read as a table",
                        Src::new(&self.path, head.no),
                    ));
                }
                let mut j = i + 1;
                while self
                    .lines
                    .get(j)
                    .is_some_and(|l| l.text.trim_start().starts_with('|'))
                {
                    j += 1;
                }
                i = j;
                continue;
            }
            let header = cells(&head.text);
            let hnorm: Vec<String> = header.iter().map(|h| header_norm(h)).collect();
            let dated_ok = hnorm
                .first()
                .is_some_and(|h| ["date", "revision", "version"].contains(&h.as_str()));
            let mut rows = Vec::new();
            let mut j = i + 2;
            while let Some(line) = self.lines.get(j) {
                if !line.text.trim_start().starts_with('|') {
                    break;
                }
                if !is_separator(&line.text) {
                    let raw = cells(&line.text);
                    let first = raw.first().map(|c| plain(c)).unwrap_or_default();
                    if !dated_ok && DATE.is_match(first.trim()) {
                        if !self.lines[i].excluded {
                            self.notes.push(Note::warn(
                                "revision row inside table; left out of the data",
                                Src::new(&self.path, line.no),
                            ));
                        }
                    } else {
                        rows.push(Row {
                            line: line.no,
                            malformed: raw.len() < header.len(),
                            cells: raw.iter().map(|c| MdCell::new(c)).collect(),
                        });
                    }
                }
                j += 1;
            }
            self.tables.push(Table {
                line: self.lines[i].no,
                header,
                hnorm,
                rows,
                section: self.lines[i].section,
                excluded: self.lines[i].excluded,
                at: i,
                end: j,
            });
            i = j;
        }
    }

    pub fn src(&self, line: u32) -> Src {
        Src::new(&self.path, line)
    }

    /// The headings `t` sits under, nearest first.
    pub fn chain(&self, section: Option<usize>) -> Vec<&Heading> {
        let mut out = Vec::new();
        let mut at = section;
        while let Some(h) = at {
            out.push(&self.headings[h]);
            at = self.headings[h].parent;
        }
        out
    }

    /// Whether `section` or a heading it is nested in starts with one of
    /// `prefixes` (normalized).
    pub fn under(&self, section: Option<usize>, prefixes: &[&str]) -> bool {
        self.chain(section)
            .iter()
            .any(|h| prefixes.iter().any(|p| h.norm.starts_with(p)))
    }

    /// The first heading, outside excluded sections, whose normalized text
    /// starts with one of `prefixes`.
    pub fn heading(&self, prefixes: &[&str]) -> Option<usize> {
        self.headings
            .iter()
            .position(|h| !h.excluded && prefixes.iter().any(|p| h.norm.starts_with(p)))
    }

    /// The tables in heading `h`'s section, nested sections included.
    pub fn tables_in(&self, h: usize) -> impl Iterator<Item = &Table> {
        let (at, end) = (self.headings[h].at, self.headings[h].end);
        self.tables
            .iter()
            .filter(move |t| t.at > at && t.at < end && !t.excluded)
    }

    /// The tables outside excluded sections.
    pub fn live_tables(&self) -> impl Iterator<Item = &Table> {
        self.tables.iter().filter(|t| !t.excluded)
    }

    /// Paragraphs and list items, each joined with its continuation lines.
    /// Headings, tables and blank lines end a block.
    pub fn blocks(&self) -> Vec<Block> {
        let mut out: Vec<Block> = Vec::new();
        let mut open = false;
        for line in &self.lines {
            let t = line.text.trim_end();
            let blank = t.trim().is_empty();
            if blank || heading(t).is_some() || t.trim_start().starts_with('|') {
                open = false;
                continue;
            }
            let item = ITEM.is_match(t);
            let indented = t.starts_with(' ') || t.starts_with('\t');
            if let Some(b) = out
                .last_mut()
                .filter(|b| open && !item && (indented || !b.item))
            {
                b.text.push(' ');
                b.text.push_str(t.trim());
                continue;
            }
            out.push(Block {
                line: line.no,
                text: t.trim().to_string(),
                item,
                section: line.section,
                excluded: line.excluded,
            });
            open = true;
        }
        out
    }

    /// The blocks between lines `from` and `to` (exclusive), by index in
    /// [`Doc::lines`].
    pub fn blocks_between(&self, from: usize, to: usize) -> Vec<Block> {
        let (a, b) = (
            self.lines.get(from).map_or(u32::MAX, |l| l.no),
            self.lines.get(to).map_or(u32::MAX, |l| l.no),
        );
        self.blocks()
            .into_iter()
            .filter(|bl| bl.line >= a && bl.line < b)
            .collect()
    }

    /// The blocks in heading `h`'s section.
    pub fn blocks_in(&self, h: usize) -> Vec<Block> {
        self.blocks_between(self.headings[h].at + 1, self.headings[h].end)
    }

    /// Every date line (`> **Last reviewed:** …`) in the region, with its
    /// fields: `(line, [(name, value)])`.
    pub fn date_lines(&self) -> Vec<(u32, Vec<(String, String)>)> {
        self.lines
            .iter()
            .filter_map(|l| {
                let rest = l.text.trim_start().strip_prefix('>')?.trim_start();
                rest.starts_with("**Last reviewed:**")
                    .then(|| (l.no, date_fields(rest)))
            })
            .collect()
    }
}

/// The `**Name:** value` fields of a date line, split on ` · ` before `**`.
pub fn date_fields(line: &str) -> Vec<(String, String)> {
    let mut parts = Vec::new();
    let mut rest = line;
    loop {
        let next = rest
            .match_indices('·')
            .find(|(i, _)| {
                let before = rest[..*i].ends_with(char::is_whitespace);
                let after = rest[i + '·'.len_utf8()..].trim_start();
                before
                    && rest[i + '·'.len_utf8()..].starts_with(char::is_whitespace)
                    && after.starts_with("**")
            })
            .map(|(i, _)| i);
        match next {
            Some(i) => {
                parts.push(rest[..i].trim());
                rest = &rest[i + '·'.len_utf8()..];
            }
            None => {
                parts.push(rest.trim());
                break;
            }
        }
    }
    parts
        .into_iter()
        .filter_map(|p| {
            let p = p.strip_prefix("**")?;
            let (name, value) = p.split_once("**")?;
            let name = name.trim().trim_end_matches(':').trim().to_string();
            Some((name, value.trim().to_string()))
        })
        .collect()
}

impl Table {
    /// The column whose header matches one of `aliases`: each alias is
    /// tried against the whole normalized header, then against the header
    /// cut at ` (`, in alias order.
    pub fn col(&self, aliases: &[&str]) -> Option<usize> {
        for a in aliases {
            if let Some(i) = self.hnorm.iter().position(|h| h == a) {
                return Some(i);
            }
        }
        for a in aliases {
            if let Some(i) = self.hnorm.iter().position(|h| cut_paren(h) == *a) {
                return Some(i);
            }
        }
        None
    }

    /// The column whose header contains `needle` (as a whole word when
    /// `word`).
    pub fn col_containing(&self, needle: &str, word: bool) -> Option<usize> {
        self.hnorm.iter().position(|h| {
            if word {
                contains_word(h, needle)
            } else {
                h.contains(needle)
            }
        })
    }

    /// Row `r`'s cell `c`, or an empty one for a short row.
    pub fn cell<'a>(&self, r: &'a Row, c: Option<usize>) -> Option<&'a MdCell> {
        c.and_then(|c| r.cells.get(c))
    }

    /// The first header, without `#` and spaces.
    pub fn first_header(&self) -> &str {
        self.hnorm
            .first()
            .map(|h| h.trim_start_matches('#').trim())
            .unwrap_or("")
    }
}

impl MdCell {
    pub fn new(raw: &str) -> MdCell {
        let text = plain(raw);
        let struck = spans(raw, "~~");
        let lead = raw.trim_start().trim_start_matches("**").starts_with("~~");
        let all = !struck.is_empty() && {
            let outside: String = raw.split("~~").step_by(2).collect();
            plain(&outside).trim().is_empty()
        };
        MdCell {
            raw: raw.to_string(),
            links: links(raw),
            bold: spans(raw, "**"),
            struck_lead: lead && !struck.is_empty(),
            struck_all: all,
            struck,
            issues: ISSUE
                .captures_iter(raw)
                .filter_map(|c| c[1].parse().ok())
                .collect(),
            text,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// `(level, text)` of an ATX heading such as `## Headline numbers ##`. The
/// space after the `#`s is required, so `#38). …` is not a heading.
pub fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    if !rest.starts_with(' ') {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim_end();
    (!text.is_empty()).then_some((level, text))
}

/// `s` without Markdown markup: links keep their text, and bold, italic
/// underscores, strike-through, code ticks and HTML tags go.
pub fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '[' {
            if let Some((text, _, after)) = link(rest) {
                out.push_str(&plain(text));
                rest = after;
                continue;
            }
        }
        if c == '<' {
            let tag = rest[1..]
                .chars()
                .next()
                .is_some_and(|n| n.is_ascii_alphabetic() || n == '/');
            if let (true, Some(end)) = (tag, rest.find('>')) {
                rest = &rest[end + 1..];
                continue;
            }
        }
        if let Some(after) = ["**", "__", "~~"].iter().find_map(|m| rest.strip_prefix(m)) {
            rest = after;
            continue;
        }
        if c != '`' {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    out.trim().to_string()
}

/// The text and url of a link `[text](url)` at the start of `s`, and what
/// follows it.
pub fn link(s: &str) -> Option<(&str, &str, &str)> {
    let mut depth = 0;
    let mut close = None;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    if !s[close..].starts_with("](") {
        return None;
    }
    let end = s[close..].find(')')? + close;
    Some((&s[1..close], &s[close + 2..end], &s[end + 1..]))
}

/// Every `(text, url)` link in `s`.
pub fn links(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        match link(&rest[i..]) {
            Some((text, url, after)) => {
                out.push((plain(text), url.to_string()));
                rest = after;
            }
            None => rest = &rest[i + 1..],
        }
    }
    out
}

/// The text inside each closed `marker…marker` pair of `s`, without markup.
pub fn spans(s: &str, marker: &str) -> Vec<String> {
    let parts: Vec<&str> = s.split(marker).collect();
    (1..parts.len().saturating_sub(1))
        .step_by(2)
        .map(|i| plain(parts[i]))
        .filter(|t| !t.is_empty())
        .collect()
}

/// Lowercase plain text, with `&` read as "and".
pub fn normalize(s: &str) -> String {
    plain(s).to_lowercase().replace('&', "and")
}

/// A header as columns are looked up by: lowercase, without markup.
pub fn header_norm(s: &str) -> String {
    plain(s)
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `s` before ` (`.
pub fn cut_paren(s: &str) -> &str {
    s.split(" (").next().unwrap_or(s).trim()
}

/// Whether `word` occurs in `s` with no letter or digit on either side.
pub fn contains_word(s: &str, word: &str) -> bool {
    s.match_indices(word).any(|(i, _)| {
        let before = s[..i].chars().next_back();
        let after = s[i + word.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric()) && !after.is_some_and(|c| c.is_alphanumeric())
    })
}

/// Whether `line` separates a table's header from its rows: `|---|:--:|`.
pub fn is_separator(line: &str) -> bool {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    !t.is_empty()
        && t.split('|').all(|c| {
            let c = c.trim();
            let c = c.strip_prefix(':').unwrap_or(c);
            let c = c.strip_suffix(':').unwrap_or(c);
            !c.is_empty() && c.bytes().all(|b| b == b'-')
        })
}

/// A table row's cells, split on each `|` that is neither escaped nor inside
/// backticks.
pub fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = if t.ends_with('|') && !t.ends_with("\\|") {
        &t[..t.len() - 1]
    } else {
        t
    };
    let mut out = Vec::new();
    let mut cell = String::new();
    let mut code = false;
    let mut escaped = false;
    for c in t.chars() {
        match c {
            '|' if !code && !escaped => out.push(std::mem::take(&mut cell).trim().to_string()),
            _ => {
                if c == '`' && !escaped {
                    code = !code;
                }
                cell.push(c);
            }
        }
        escaped = c == '\\' && !escaped;
    }
    out.push(cell.trim().to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_references_are_not_headings() {
        assert_eq!(heading("#1859; fixed"), None);
        assert_eq!(heading("#38). Fixed in"), None);
        assert_eq!(
            heading("## Headline numbers ##"),
            Some((2, "Headline numbers"))
        );
        let doc = Doc::parse("d.md", "# T\n#1859; one\n#38). two\n## Real\n");
        assert_eq!(doc.headings.len(), 2);
    }

    #[test]
    fn a_header_without_a_separator_is_a_note() {
        let doc = Doc::parse("d.md", "# T\n\n| M | When |\n| a | b |\n\nText.\n");
        assert!(doc.tables.is_empty());
        assert_eq!(doc.notes.len(), 1);
        assert_eq!(doc.notes[0].src.as_ref().map(|s| s.line), Some(3));
        assert!(doc.notes[0].text.contains("without a separator"));
    }

    #[test]
    fn short_rows_are_malformed_and_separators_skipped() {
        let md = "| A | B | C |\n|---|---|---|\n| 1 | 2 | 3 |\n|---|---|---|\n| 4 | 5 |\n| 6 | 7 | 8 |\nafter\n";
        let doc = Doc::parse("d.md", md);
        let t = &doc.tables[0];
        assert_eq!(t.rows.len(), 3);
        assert!(!t.rows[0].malformed && t.rows[1].malformed && !t.rows[2].malformed);
        assert_eq!(t.rows[2].line, 6);
    }

    #[test]
    fn rows_after_a_stray_line_continue_the_table() {
        let md = "| Id | Tier | Status |\n|---|---|---|\n| a | P1 | ✅ |\n)\n| b | P1 | ✅ |\n| c | P2 | ⬜ |\n\n| X | Y |\n| 1 | 2 |\n";
        let doc = Doc::parse("d.md", md);
        assert_eq!(doc.tables.len(), 1);
        assert_eq!(doc.tables[0].rows.len(), 3);
        assert!(doc.notes[0].text.starts_with("table interrupted by line 4"));
        assert!(
            doc.notes[1].text.contains("without a separator"),
            "a blank line ends the table"
        );
    }

    #[test]
    fn date_first_rows_leave_a_value_table() {
        let md = "| Number | Value |\n|---|---|\n| 2026-10-15 | minor | x |\n| Breadth | 5% |\n\n| Date | Change |\n|---|---|\n| 2026-10-15 | minor |\n";
        let doc = Doc::parse("d.md", md);
        assert_eq!(doc.tables[0].rows.len(), 1);
        assert_eq!(doc.tables[1].rows.len(), 1, "a Date table keeps its rows");
        assert_eq!(doc.notes.len(), 1);
        assert!(doc.notes[0].text.starts_with("revision row inside table"));
    }

    #[test]
    fn an_excluded_heading_hides_its_nested_tables() {
        let md = "# T\n## Appendix: old numbers\n### Sub\n#### Deeper\n| A |\n|---|\n| 1 |\n## Live\n| B |\n|---|\n| 2 |\n";
        let doc = Doc::parse("d.md", md);
        assert_eq!(doc.tables.len(), 2);
        assert!(doc.tables[0].excluded);
        assert!(!doc.tables[1].excluded);
        assert_eq!(doc.live_tables().count(), 1);
        assert!(doc.headings[2].excluded && doc.headings[3].excluded);
        assert!(!doc.headings[4].excluded);
    }

    #[test]
    fn a_second_h1_ends_the_region_and_fences_are_skipped() {
        let md = "# A\n```\n## Hidden\n```\n## Shown\n# Earlier assessments\n## Gone\n";
        let doc = Doc::parse("d.md", md);
        let names: Vec<&str> = doc.headings.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(names, ["A", "Shown"]);
        assert_eq!(doc.last_line, 8);
    }

    #[test]
    fn a_fence_closes_only_on_its_own_marker() {
        for md in [
            "# A\n````\n```\n## Hidden\n```\n````\n## Shown\n",
            "# A\n```\n~~~\n## Hidden\n```\n## Shown\n",
            "# A\n```\n```md\n## Hidden\n```\n## Shown\n",
            "# A\n``` a`b\n## Shown\n",
        ] {
            let doc = Doc::parse("d.md", md);
            let names: Vec<&str> = doc.headings.iter().map(|h| h.text.as_str()).collect();
            assert_eq!(names, ["A", "Shown"], "{md:?}");
        }
    }

    #[test]
    fn blocks_join_continuation_lines() {
        let md = "Para one\ncontinues.\n\n- **F** feature · **U** UI/UX\n  · **S** stability\n- next\n\nTail\n";
        let doc = Doc::parse("d.md", md);
        let b = doc.blocks();
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].text, "Para one continues.");
        assert_eq!(b[1].text, "- **F** feature · **U** UI/UX · **S** stability");
        assert!(b[1].item && b[2].item);
        assert_eq!(b[3].line, 8);
    }

    #[test]
    fn cells_record_spans_links_and_issues() {
        let c = MdCell::new("~~**Old gap**~~ fixed in #12 and [x](a.md#b) **bold**");
        assert!(c.struck_lead && !c.struck_all);
        assert_eq!(c.struck, ["Old gap"]);
        assert_eq!(c.bold, ["Old gap", "bold"]);
        assert_eq!(c.issues, [12]);
        assert_eq!(c.links, [("x".to_string(), "a.md#b".to_string())]);
        assert_eq!(c.text, "Old gap fixed in #12 and x bold");
        assert!(MdCell::new("~~gone~~").struck_all);
        assert_eq!(
            cells("| a | `x|y` | b \\| c |"),
            vec!["a", "`x|y`", "b \\| c"]
        );
        assert_eq!(
            cells("| keys (`[` `]`, \\`) | P2 | x |"),
            vec!["keys (`[` `]`, \\`)", "P2", "x"]
        );
        assert_eq!(clean("\u{feff}a\u{fe0f}\r\n"), "a\n");
    }

    #[test]
    fn date_fields_split_before_bold_names() {
        let f = date_fields(
            "**Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-12 · **Change:** minor (a · b) · **Target:** App 3",
        );
        let names: Vec<&str> = f.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Last reviewed", "Last updated", "Change", "Target"]);
        assert_eq!(f[2].1, "minor (a · b)");
    }

    #[test]
    fn columns_match_the_whole_header_before_the_cut_one() {
        let doc = Doc::parse(
            "d.md",
            "| Area | Remaining (h) | Remaining | Status (gen) |\n|---|---|---|---|\n| a | 1 | b | c |\n",
        );
        let t = &doc.tables[0];
        assert_eq!(t.col(&["remaining (h)"]), Some(1));
        assert_eq!(t.col(&["remaining"]), Some(2));
        assert_eq!(t.col_containing("status", true), Some(3));
    }
}
