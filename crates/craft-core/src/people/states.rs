//! The work state of each pull request: the state, who it waits on and
//! since when, decided by the rules of the work-state spec in their order.
//! The first rule that matches gives the state, and every later match is
//! listed in [`WorkState::also`].
//!
//! A state read from a GitHub field (draft, mergeability, checks, merged)
//! is reliable. A state read from comment text is a guess: it carries the
//! sentence that decided it ([`Evidence`]), and the rules that rest on one
//! example are also marked low confidence.
//!
//! Text is matched one sentence at a time, lowercase with straight quotes,
//! without quoted lines (`>`) and fenced code. Comments posted after a
//! merge are left out.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::record::{CrossSource, MainChecks, PrCache, PrRecord, PrState, CONFLICTING, UNKNOWN};
use super::{canonical, is_bot};
use crate::model::Time;

/// Who a pull request waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Author,
    Maintainer,
    Owner,
    Ci,
    OtherPr,
}

/// The sentence a text rule matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Evidence {
    /// The sentence as written, cut to [`QUOTE_CHARS`] characters.
    pub quote: String,
    pub author: Option<String>,
    pub at: Option<Time>,
    /// The comment, or the pull request when GitHub gave no comment link.
    pub url: String,
    /// The words of the phrase that matched.
    pub phrase: String,
}

/// A hold for the owner's decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Hold {
    pub since: Option<Time>,
    /// The maintainer who placed it.
    pub by: Option<String>,
    /// design, policy, brand, packaging, library, dependency or owner decision.
    pub category: String,
    /// The latest later sentence that holds it again.
    pub reaffirmed: Option<Time>,
    /// The owners it waits on; empty when none is configured or named.
    pub owners: Vec<String>,
}

/// A pull request's work state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WorkState {
    /// The state id, such as `held_for_owner` or `merged`.
    pub state: String,
    pub subtype: Option<String>,
    /// Who it waits on; empty for a closed or merged pull request.
    pub waits_on: Vec<Role>,
    /// The logins it waits on (canonical): the author, the maintainer who
    /// made the last request or comment, the owners.
    pub waits_for: Vec<String>,
    /// When the state began.
    pub since: Option<Time>,
    /// Read from comment text, so a guess.
    pub guess: bool,
    /// Low confidence: the rule rests on one example or a weak signal.
    pub low: bool,
    /// The rule that decided it.
    pub rule: String,
    /// Why it is a guess, in words.
    pub why: String,
    pub evidence: Option<Evidence>,
    /// The other states that also matched, in rule order.
    pub also: Vec<String>,
    /// Badges that never replace the state (`stale`, `conflict`…).
    pub flags: Vec<String>,
    pub hold: Option<Hold>,
    /// Open pull requests this one waits for.
    pub blocked_by: Vec<u64>,
    /// Pull requests a maintainer has to choose between with this one.
    pub competing: Vec<u64>,
}

/// One maintainer request on a pull request and when the author answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Request {
    pub at: Option<Time>,
    pub by: String,
    pub subtype: String,
    /// The comment or review, or the pull request when GitHub gave no link.
    pub url: String,
    pub answered: Option<Time>,
    /// The maintainer approved the pull request later, so it no longer
    /// waits for an answer.
    pub resolved: bool,
}

/// The times a pull request's response figures come from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Timings {
    /// Every maintainer request, oldest first.
    pub requests: Vec<Request>,
    /// `max(createdAt, the last ReadyForReviewEvent)`.
    pub ready_at: Option<Time>,
    /// The first maintainer comment, review or merge after `ready_at`.
    pub first_response: Option<Time>,
}

/// What the rules need besides the pull request.
pub struct Ctx<'a> {
    pub repo: &'a str,
    /// The repository's maintainers, canonical.
    pub maintainers: &'a BTreeSet<String>,
    /// The repository's configured owners.
    pub owners: &'a [String],
    /// The maintainers rebase contributors' branches themselves, so a
    /// conflict on a contributor's pull request is only a flag.
    pub rebase: bool,
    pub main: Option<&'a MainChecks>,
    /// The repository's other records, for the pull requests a sentence names.
    pub cache: &'a PrCache,
    /// The repository's merges, from [`merge_index`].
    pub merges: &'a [MergeAt],
    pub now: Time,
}

/// Quotes are cut to this many characters.
pub const QUOTE_CHARS: usize = 200;
/// Days without human activity before an open pull request is aging or stale.
pub const AGING_DAYS: i64 = 3;
pub const STALE_DAYS: i64 = 7;

fn list(patterns: &[&str]) -> Regex {
    Regex::new(&patterns.join("|")).unwrap()
}

static HOLD: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"holding (?:it|this|this one|this pr)\b.{0,40}?\b(?:for|until)",
        r"held for the (?:project )?owners?",
        r"owner(?:s|'s|s')? (?:decision|call|sign-off)",
        r"(?:product|design|architecture|policy|brand|library|maintainer|distribution|behaviou?r) (?:decision|call)",
        r"(?:calls?|decisions?) for the project(?:'s)? owners?",
        r"call the project owner makes",
        r"leaving this open for that decision",
        r"checking with the project owner",
        r"needs the project owner",
        r"flagged it for the maintainer",
        r"with the maintainers for review",
        r"leave the decision to the owners",
        r"design-direction change",
    ])
});
static RELEASE: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"landed on main",
        r"after the follow-ups since the hold",
        r"(?:owner|maintainers?) (?:approved|signed off|agreed|decided)",
        r"go ahead",
    ])
});
static REQ: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"before (?:it|this) can (?:land|go in|merge)",
        r"can't land (?:it )?yet",
        r"couldn't land it in this round",
        r"to land it:",
        r"a few things (?:need fixing|before merging)",
        r"needs a rebase",
        r"please rebase",
        r"could you (?:please )?(?:rebase|merge (?:current )?main|rework|split|add)",
        r"conflicts with main",
        r"split (?:it|them|this|out)",
        r"be split",
        r"could this be split",
        r"splitting (?:it|this)",
        r"several prs in one",
        r"its own pr",
        r"allow edits by maintainers",
        r"we can't push to your branch",
        r"needs (?:a )?process version",
        r"evidence",
        r"couldn't be reproduced",
        r"can't merge (?:it|this) yet",
        r"(?:two|a few|some) (?:changes|things) before",
        r"please take out",
        r"before this goes in",
        r"is there a public page",
        r"add its url",
    ])
});
static REQ_DROP: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"no need to",
        r"don't need to",
        r"nothing for you to change",
    ])
});
static ANSWERED: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"addressed",
        r"\bdone\b",
        r"fixed in",
        r"rebased onto",
        r"merged (?:current )?main",
        r"split (?:as suggested|done)",
    ])
});
static MTASK: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"sign-off near (?:macos|reviewer)",
        r"dry run",
        r"tried on (?:a few )?more",
        r"couldn't (?:check|test) it on",
        r"release pipeline",
    ])
});
static MTASK_LIFT: LazyLock<Regex> =
    LazyLock::new(|| list(&[r"dry run (?:passed|done)", r"signed off"]));
static DECLINE: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"can't take this (?:approach|version)",
        r"can't merge this (?:one|version)",
        r"not something we can accept",
        r"conflicts with agents\.md",
        r"clean-room",
        r"not permitted",
    ])
});
static SUPERSEDE_OPEN: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"carry the work forward",
        r"now included in #\d+",
        r"main (?:now )?(?:already )?(?:handles|has|does) (?:this|it)\b",
        r"superseded by #\d+",
    ])
});
static DEP: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"stacked on #?\d+",
        r"depends on",
        r"once (?:that|#\d+|it) (?:lands|merges|is merged)",
        r"after #\d+ (?:lands|merges)",
        r"merge #\d+ first",
        r"until .{0,40} lands",
        r"builds? on #\d+",
        r"part \d+ ?/ ?\d+",
        r"prerequisite",
    ])
});
static NOTREADY: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"needs more work",
        r"not ready",
        r"still (?:fixing|working)",
        r"in process to be fixed",
    ])
});
static COMPETE: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"(?:the one|which one) we'll take forward",
        r"#\d+ (?:vs\.?|versus) #\d+",
        r"one thing to decide",
        r"whichever (?:merges|lands) (?:first|second)",
        r"overlaps #\d+",
    ])
});
static CRAFTLIBS: LazyLock<Regex> =
    LazyLock::new(|| list(&[r"craft-libs", r"shared crate", r"put .{0,40} in craft-libs"]));
static AUTHOR_HOLD: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"hold (?:this|it) .{0,60}pending",
        r"pending (?:a|the|an) .{0,60}decision",
        r"keep .{0,20}draft pending",
    ])
});
static STEPPED_BACK: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"not in this to",
        r"won't be doing",
        r"choose to implement it or ignore it",
        r"your show",
    ])
});
static LANDED: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"landed (?:on main|in #\d+|through #\d+)",
        r"came in as part of #\d+",
        r"closing this one as merged there",
        r"with a batch of community prs",
        r"integration batch",
        r"maintainer carry pr",
        r"went into #\d+",
        r"through #\d+",
        r"credited",
    ])
});
static DUP: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"duplicate of #\d+",
        r"closing (?:this|it)(?: one)? as (?:a )?duplicate",
        r"identical to #\d+",
        r"line for line",
        r"which was first",
        r"makes the same change",
    ])
});
static SUPERSEDE: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"superseded",
        r"supersedes",
        r"already (?:been )?(?:implemented|added)",
        r"main (?:already )?fixed",
        r"already fixed (?:on|in) main",
        r"fixed on main by #\d+",
        r"(?:has|have) (?:now )?been implemented",
        r"is implemented",
        r"fixed #\d+ another way",
        r"nothing left for this pr to add",
        r"covers (?:this|the) (?:pr|change)",
        r"covered by #\d+",
        r"no longer applies",
        r"owner's restyle",
    ])
});
static FAVOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:in favou?r of|closing for) #(\d+)").unwrap());
static FOLD: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"folded into #\d+",
        r"split into",
        r"moved to \S+",
        r"opened (?:here )?by mistake",
        r"merged as separate prs",
        r"up as separate prs",
        r"everything in it has a home",
        r"replaced by (?:your|#\d+)",
    ])
});
static FINDINGS: LazyLock<Regex> =
    LazyLock::new(|| list(&[r"## \[p[12]\]", r"requesting changes"]));
static GATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"cargo xtask ci is incomplete or unchecked").unwrap());
static COMMUNITY_OK: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"ready to merge",
        r"approval still stands",
        r"no actionable findings",
    ])
});
static DISPUTE: LazyLock<Regex> =
    LazyLock::new(|| list(&[r"not a duplicate", r"clarif", r"reopen"]));
static WIP_TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*\[?(?:wip|do not merge)\b").unwrap());
static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([\w.-]+/[\w.-]+)?#(\d+)").unwrap());
static OWNER_AT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"owners?\b.{0,20}?@([a-z0-9-]+)").unwrap());
static THIS_IT_HERE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:this|it|here)\b").unwrap());
static REBASE: LazyLock<Regex> = LazyLock::new(|| {
    list(&[
        r"rebase",
        r"merge (?:current )?main",
        r"conflicts with main",
    ])
});
static SPLIT: LazyLock<Regex> =
    LazyLock::new(|| list(&[r"split", r"splitting", r"several prs in one", r"its own pr"]));
static EVIDENCE: LazyLock<Regex> =
    LazyLock::new(|| list(&[r"evidence", r"reproduced", r"public page", r"add its url"]));
static ALLOW_EDITS: LazyLock<Regex> =
    LazyLock::new(|| list(&[r"allow edits", r"push to your branch"]));
static LANDING_ROUND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"couldn't land it in this round").unwrap());
static TAKE_OUT: LazyLock<Regex> =
    LazyLock::new(|| list(&[r"take out", r"\bpin\b", r"leave out", r"remove"]));
static BATCH_TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)batch|integration batch|carry").unwrap());
static MERGED_DIRECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"merged (?:after review|the narrowed version)").unwrap());

/// Each sentence of `text` as `(written, lowercase)`, with curly quotes made
/// straight, lines starting with `>` and fenced code blocks left out, split
/// on `.`, `!` or `?` before whitespace or the end, and on newlines.
pub fn sentences(text: &str) -> Vec<(String, String)> {
    let text = text
        .replace(['\u{2018}', '\u{2019}'], "'")
        .replace(['\u{201c}', '\u{201d}'], "\"");
    let mut out = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || trimmed.starts_with('>') {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut start = 0;
        for i in 0..chars.len() {
            let end = matches!(chars[i], '.' | '!' | '?')
                && chars.get(i + 1).is_none_or(|c| c.is_whitespace());
            if end || i + 1 == chars.len() {
                let s: String = chars[start..=i].iter().collect();
                let s = s.trim();
                if !s.is_empty() {
                    out.push((s.to_string(), s.to_lowercase()));
                }
                start = i + 1;
            }
        }
    }
    out
}

/// Which kinds of sentence a comment holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Phrases {
    /// A request phrase, without "no need to".
    pub request: bool,
    pub hold: bool,
}

/// The kinds of sentence in `body`.
pub fn phrases(body: &str) -> Phrases {
    let s = sentences(body);
    Phrases {
        request: s
            .iter()
            .any(|(_, l)| REQ.is_match(l) && !REQ_DROP.is_match(l)),
        hold: s.iter().any(|(_, l)| HOLD.is_match(l)),
    }
}

/// The same-repository pull request numbers a sentence names (`#N`, not
/// `owner/repo#N`).
fn numbers(lower: &str) -> Vec<u64> {
    NUMBER
        .captures_iter(lower)
        .filter(|c| c.get(1).is_none())
        .filter_map(|c| c[2].parse().ok())
        .collect()
}

/// The issues `r` closes as `(repo, number)`, the repository lowercase; an
/// issue with no repository on record is taken to be in `repo`.
fn closing_keys(r: &PrRecord, repo: &str) -> Vec<(String, u64)> {
    r.closing_issues
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let issue_repo = r.closing_repos.get(i).filter(|s| !s.is_empty());
            (issue_repo.map_or(repo, |s| s).to_ascii_lowercase(), *n)
        })
        .collect()
}

/// One merge, for finding merges by the same person close in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeAt {
    pub at: Time,
    /// Canonical login (see [`super::merger`]).
    pub merger: String,
    pub number: u64,
    /// The merge commit has two parents (not a squash or rebase).
    pub two_parents: bool,
}

/// Who landed `r`: its resolved merger, or `mergedBy`.
fn merger_of(r: &PrRecord) -> Option<String> {
    r.contribution
        .merger
        .clone()
        .or_else(|| r.merged_by.as_deref().map(canonical))
}

/// Every merge in `cache`, oldest first, for [`Ctx::merges`].
pub fn merge_index(cache: &PrCache) -> Vec<MergeAt> {
    let mut out: Vec<MergeAt> = cache
        .records
        .values()
        .filter(|r| r.state == PrState::Merged)
        .filter_map(|r| {
            Some(MergeAt {
                at: r.merged_at?,
                merger: merger_of(r)?,
                number: r.number,
                two_parents: r
                    .contribution
                    .merge_commit
                    .as_ref()
                    .is_some_and(|m| m.parents == 2),
            })
        })
        .collect();
    out.sort_by_key(|m| m.at);
    out
}

/// The reason category of a hold sentence.
fn hold_category(lower: &str) -> &'static str {
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if has(&["product", "design", "architecture", "behavior", "behaviour"]) {
        "design"
    } else if has(&["policy", "clean-room", "license"]) {
        "policy"
    } else if has(&["brand"]) {
        "brand"
    } else if has(&["distribution", "packaging"]) {
        "packaging"
    } else if has(&["library"]) {
        "library"
    } else if DEP.is_match(lower) {
        "dependency"
    } else {
        "owner decision"
    }
}

/// The subtype of a maintainer request sentence.
fn req_subtype(lower: &str) -> &'static str {
    if REBASE.is_match(lower) {
        "rebase"
    } else if SPLIT.is_match(lower) {
        "split"
    } else if EVIDENCE.is_match(lower) {
        "evidence_or_repro"
    } else if ALLOW_EDITS.is_match(lower) {
        "allow_edits"
    } else if LANDING_ROUND.is_match(lower) {
        "landing_round"
    } else {
        "fixes"
    }
}

/// Whether only a push answers a request: rebase and split requests, and
/// fixes that take something out.
fn push_answers(subtype: &str, lower: &str) -> bool {
    matches!(subtype, "rebase" | "split") || (subtype == "fixes" && TAKE_OUT.is_match(lower))
}

/// One human comment or review.
struct Said {
    at: Time,
    /// Canonical login.
    who: String,
    m: bool,
    author: bool,
    /// A review's state; `None` for a comment.
    review: Option<String>,
    commit: Option<String>,
    url: String,
    sents: Vec<(String, String)>,
}

impl Said {
    fn community(&self) -> bool {
        !self.m && !self.author
    }

    /// Whether this only asks for the gate to run: a sentence matches the
    /// gate phrase, and the others are at most two lines of up to 80
    /// characters that neither list findings nor make a request.
    fn gate_only(&self) -> bool {
        let (gate, rest): (Vec<_>, Vec<_>) = self.sents.iter().partition(|(_, l)| GATE.is_match(l));
        !gate.is_empty()
            && rest.len() <= 2
            && rest.iter().all(|(written, l)| {
                written.chars().count() <= 80 && !FINDINGS.is_match(l) && !REQ.is_match(l)
            })
    }
}

/// A sentence a phrase list matched.
#[derive(Clone, Copy)]
struct Hit<'a> {
    said: &'a Said,
    i: usize,
}

impl<'a> Hit<'a> {
    fn lower(&self) -> &'a str {
        &self.said.sents[self.i].1
    }

    fn at(&self) -> Time {
        self.said.at
    }

    fn evidence(&self, re: &Regex) -> Evidence {
        let written = &self.said.sents[self.i].0;
        let quote: String = written.chars().take(QUOTE_CHARS).collect();
        Evidence {
            quote: if written.chars().count() > QUOTE_CHARS {
                format!("{quote}…")
            } else {
                quote
            },
            author: Some(self.said.who.clone()),
            at: Some(self.said.at),
            url: self.said.url.clone(),
            phrase: re
                .find(self.lower())
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
        }
    }

    /// "matched the phrase 'owner's call' in ann's comment of 10-12".
    fn why(&self, re: &Regex) -> String {
        let phrase = re.find(self.lower()).map(|m| m.as_str()).unwrap_or("");
        let kind = if self.said.review.is_some() {
            "review"
        } else {
            "comment"
        };
        format!(
            "matched the phrase '{phrase}' in {}'s {kind} of {}",
            self.said.who,
            self.said.at.format("%m-%d")
        )
    }
}

/// The sentences of `said` that `re` matches, oldest first.
fn hits<'a>(said: impl IntoIterator<Item = &'a Said>, re: &Regex) -> Vec<Hit<'a>> {
    let mut out = Vec::new();
    for s in said {
        for (i, (_, lower)) in s.sents.iter().enumerate() {
            if re.is_match(lower) {
                out.push(Hit { said: s, i });
            }
        }
    }
    out
}

/// The first sentence of one comment that `re` matches.
fn hit_in<'a>(s: &'a Said, re: &Regex) -> Option<Hit<'a>> {
    hits([s], re).into_iter().next()
}

/// What the rules read from one record, worked out once.
struct Facts<'a> {
    r: &'a PrRecord,
    ctx: &'a Ctx<'a>,
    author: Option<String>,
    author_m: bool,
    said: Vec<Said>,
    /// The author's events (comments, reviews, pushes, ready for review), sorted.
    events: Vec<Time>,
    /// The author's commits and force-pushes, sorted.
    pushes: Vec<Time>,
    /// Commits and force-pushes by maintainers other than the author.
    m_pushes: Vec<Time>,
    m_force: Vec<Time>,
    /// Every close, and the last one's actor.
    closes: Vec<Time>,
    closed_by: Option<String>,
    /// Human activity of any kind, for staleness.
    human: Vec<Time>,
    ready_at: Option<Time>,
    draft_at: Option<Time>,
}

impl<'a> Facts<'a> {
    fn new(r: &'a PrRecord, ctx: &'a Ctx<'a>) -> Facts<'a> {
        let is_m = |login: &str| ctx.maintainers.contains(&canonical(login));
        let author = r
            .author
            .as_deref()
            .filter(|a| !r.author_bot && !is_bot(a, None))
            .map(canonical);
        let author_m = r.author.as_deref().is_some_and(is_m);
        let is_author = |login: &str| author.as_deref() == Some(canonical(login).as_str());
        let mut f = Facts {
            r,
            ctx,
            author_m,
            said: Vec::new(),
            events: Vec::new(),
            pushes: Vec::new(),
            m_pushes: Vec::new(),
            m_force: Vec::new(),
            closes: Vec::new(),
            closed_by: None,
            // Opening the pull request is its first human activity.
            human: r.created_at.into_iter().collect(),
            ready_at: r.created_at,
            draft_at: None,
            author: author.clone(),
        };
        for t in &r.timeline {
            let Some(at) = t.at else { continue };
            let actor = t
                .actor
                .as_deref()
                .filter(|a| !t.actor_bot && !is_bot(a, None));
            match t.kind.as_str() {
                "IssueComment" | "PullRequestReview" => {
                    let Some(who) = actor else { continue };
                    if t.state.as_deref() == Some("PENDING") {
                        continue;
                    }
                    f.human.push(at);
                    if r.merged_at.is_some_and(|m| at > m) {
                        continue;
                    }
                    if is_author(who) {
                        f.events.push(at);
                    }
                    f.said.push(Said {
                        at,
                        who: canonical(who),
                        m: is_m(who),
                        author: is_author(who),
                        review: (t.kind == "PullRequestReview")
                            .then(|| t.state.clone().unwrap_or_default()),
                        commit: t.commit.clone(),
                        url: t.url.clone().unwrap_or_else(|| r.url.clone()),
                        sents: sentences(t.body.as_deref().unwrap_or("")),
                    });
                }
                "HeadRefForcePushedEvent" => {
                    let Some(who) = actor else { continue };
                    f.human.push(at);
                    if is_author(who) {
                        f.events.push(at);
                        f.pushes.push(at);
                    } else if is_m(who) {
                        f.m_pushes.push(at);
                        f.m_force.push(at);
                    }
                }
                "ReadyForReviewEvent" => {
                    f.ready_at = f.ready_at.max(Some(at));
                    // An author event, but not a push: the branch is unchanged.
                    if actor.is_some_and(is_author) {
                        f.events.push(at);
                    }
                }
                "ConvertToDraftEvent" => f.draft_at = f.draft_at.max(Some(at)),
                "ClosedEvent" => {
                    f.closes.push(at);
                    f.closed_by = t.actor.as_deref().map(canonical);
                }
                _ => {}
            }
        }
        for c in &r.contribution.commits {
            let Some(at) = c.committed_at else { continue };
            match c.author.as_deref() {
                Some(who) if is_bot(who, None) => continue,
                Some(who) if is_m(who) && !is_author(who) => f.m_pushes.push(at),
                _ => {
                    f.events.push(at);
                    f.pushes.push(at);
                }
            }
            f.human.push(at);
        }
        for v in [
            &mut f.events,
            &mut f.pushes,
            &mut f.m_pushes,
            &mut f.m_force,
        ] {
            v.sort();
        }
        f
    }

    /// The first time in `times` after `t`.
    fn after(times: &[Time], t: Time) -> Option<Time> {
        times.iter().copied().find(|x| *x > t)
    }

    /// A maintainer other than the author commented or reviewed.
    fn m_engaged(&self) -> bool {
        self.said.iter().any(|s| s.m && !s.author)
    }

    /// The latest comment or review by a maintainer other than the author.
    fn last_m(&self) -> Option<&Said> {
        self.said.iter().rev().find(|s| s.m && !s.author)
    }

    fn last_m_who(&self) -> Vec<String> {
        self.last_m()
            .map(|s| vec![s.who.clone()])
            .unwrap_or_default()
    }

    fn author_vec(&self) -> Vec<String> {
        self.author.clone().into_iter().collect()
    }

    fn owners(&self) -> Vec<String> {
        self.ctx.owners.iter().map(|o| canonical(o)).collect()
    }

    /// The record for pull request `n` of the same repository.
    fn lookup(&self, n: u64) -> Option<&'a PrRecord> {
        self.ctx.cache.records.get(&n)
    }

    /// The latest close by a maintainer, or one after a merge.
    fn closed_after(&self, t: Time) -> bool {
        self.closes.iter().any(|c| *c > t)
    }

    /// The author's latest comment or review.
    fn author_last(&self) -> Option<&Said> {
        self.said.iter().rev().find(|s| s.author)
    }

    /// Maintainer requests, oldest first: sentences matching the request
    /// phrases, and reviews that request changes. A review that only asks
    /// for the gate to run is left out (see rule 41).
    fn requests(&self) -> Vec<Req<'_>> {
        let mut out = Vec::new();
        for s in self.said.iter().filter(|s| s.m && !s.author) {
            let found = hits([s], &REQ)
                .into_iter()
                .rfind(|h| !REQ_DROP.is_match(h.lower()));
            let subtype = match found {
                Some(h) => req_subtype(h.lower()),
                None if s.review.as_deref() == Some("CHANGES_REQUESTED") && !s.gate_only() => {
                    "fixes"
                }
                None => continue,
            };
            let lower = found.map(|h| h.lower()).unwrap_or("");
            // An answer counts only until the approval that resolved it.
            let approved = self.approved_after(&s.who, s.at);
            out.push(Req {
                said: s,
                hit: found,
                subtype,
                resolved: approved.is_some(),
                answered: self
                    .answered(s.at, subtype, lower)
                    .filter(|a| approved.is_none_or(|ap| *a <= ap)),
            });
        }
        out
    }

    /// When `who` first approved the pull request after `at`, which resolves
    /// the requests they made before.
    fn approved_after(&self, who: &str, at: Time) -> Option<Time> {
        self.said
            .iter()
            .find(|s| s.who == who && s.at > at && s.review.as_deref() == Some("APPROVED"))
            .map(|s| s.at)
    }

    /// When the author answered a request made at `at`: their next push for
    /// a request only a push answers, otherwise their next event. A
    /// maintainer force-push after a rebase request answers it too.
    fn answered(&self, at: Time, subtype: &str, lower: &str) -> Option<Time> {
        let by_author = if push_answers(subtype, lower) {
            Self::after(&self.pushes, at)
        } else {
            Self::after(&self.events, at)
        };
        let by_m = (subtype == "rebase")
            .then(|| Self::after(&self.m_force, at))
            .flatten();
        let first = match (by_author, by_m) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        // Nothing after a close without a merge answers a request.
        let closed = (self.r.state == PrState::Closed)
            .then_some(self.r.closed_at)
            .flatten();
        first.filter(|a| closed.is_none_or(|c| *a <= c))
    }
}

/// One maintainer request, as [`Facts::requests`] finds it.
struct Req<'a> {
    said: &'a Said,
    /// The request sentence; `None` for a change-request review without one.
    hit: Option<Hit<'a>>,
    subtype: &'static str,
    /// The maintainer approved the pull request later.
    resolved: bool,
    /// When the author answered it (see [`Facts::answered`]).
    answered: Option<Time>,
}

impl Req<'_> {
    fn at(&self) -> Time {
        self.said.at
    }
}

/// A rule that matched, before the first one is picked.
#[derive(Default)]
struct Cand {
    order: u32,
    state: &'static str,
    subtype: Option<String>,
    waits: Vec<Role>,
    waits_for: Vec<String>,
    since: Option<Time>,
    guess: bool,
    low: bool,
    rule: String,
    why: String,
    evidence: Option<Evidence>,
    hold: Option<Hold>,
    blocked_by: Vec<u64>,
    competing: Vec<u64>,
}

/// The state of a record whose timeline no deep fetch has read yet.
pub const NOT_READ_YET: &str = "not_read_yet";

/// The work state of `r`. An open record that no deep fetch has read yet
/// (one the light sweep found) gets [`NOT_READ_YET`] instead of a rule's
/// state, since the rules read the timeline it does not hold yet.
pub fn classify(r: &PrRecord, ctx: &Ctx) -> WorkState {
    if r.state == PrState::Open && r.fetched_at.is_none() {
        return WorkState {
            state: NOT_READ_YET.into(),
            since: r.updated_at,
            rule: "the timeline has not been read yet".into(),
            ..Default::default()
        };
    }
    let f = Facts::new(r, ctx);
    match r.state {
        PrState::Open => open_state(&f),
        PrState::Merged | PrState::Closed => closed_state(&f),
    }
}

/// The request and response times of `r`.
pub fn timings(r: &PrRecord, ctx: &Ctx) -> Timings {
    let f = Facts::new(r, ctx);
    let requests = f
        .requests()
        .into_iter()
        .map(|q| Request {
            at: Some(q.at()),
            by: q.said.who.clone(),
            subtype: q.subtype.to_string(),
            url: q.said.url.clone(),
            answered: q.answered,
            resolved: q.resolved,
        })
        .collect();
    let ready = f.ready_at;
    let after_ready = |t: Time| ready.is_none_or(|r| t >= r);
    // A comment after a close without a merge is not a response.
    let closed = (r.state == PrState::Closed)
        .then_some(r.closed_at)
        .flatten();
    let first_comment = f
        .said
        .iter()
        .filter(|s| closed.is_none_or(|c| s.at <= c))
        .find(|s| s.m && !s.author && after_ready(s.at))
        .map(|s| s.at);
    // A merge answers only when a maintainer other than the author made it.
    let merger = merger_of(r);
    let by_other_m =
        merger.is_some_and(|m| ctx.maintainers.contains(&m) && f.author.as_ref() != Some(&m));
    let merged = r.merged_at.filter(|_| by_other_m);
    let first_response = match (first_comment, merged) {
        (Some(c), Some(m)) => Some(c.min(m)),
        (c, m) => c.or(m),
    };
    Timings {
        requests,
        ready_at: ready,
        first_response,
    }
}

fn open_state(f: &Facts) -> WorkState {
    let r = f.r;
    let ctx = f.ctx;
    let mut c: Vec<Cand> = Vec::new();
    let mut flags: Vec<String> = Vec::new();
    let maintainer_said: Vec<&Said> = f.said.iter().filter(|s| s.m).collect();
    let others_m: Vec<&Said> = f.said.iter().filter(|s| s.m && !s.author).collect();

    // 5. The author steps back and no maintainer has replied.
    if let Some(last) = f.author_last() {
        if let Some(h) = hit_in(last, &STEPPED_BACK) {
            if !others_m.iter().any(|s| s.at > last.at) {
                c.push(Cand {
                    order: 5,
                    state: "author_stepped_back",
                    waits: vec![Role::Maintainer],
                    waits_for: f.last_m_who(),
                    since: Some(last.at),
                    guess: true,
                    low: true,
                    rule: "5: the author's latest comment says they are leaving the work".into(),
                    why: format!("{}; no maintainer reply since", h.why(&STEPPED_BACK)),
                    evidence: Some(h.evidence(&STEPPED_BACK)),
                    ..Default::default()
                });
            }
        }
    }

    // 10. Held for the owner: the earliest hold no release, merge or close lifted.
    // Maintainers place holds; maintainers and the configured owners release them.
    let owner_logins = f.owners();
    let deciders = f
        .said
        .iter()
        .filter(|s| s.m || owner_logins.contains(&s.who));
    let mut hold: Option<(Hit, Option<Time>)> = None;
    for s in deciders {
        if let Some((h, _)) = &hold {
            if f.closes.iter().any(|t| *t > h.at() && *t <= s.at) {
                hold = None;
            }
        }
        for (i, (_, lower)) in s.sents.iter().enumerate() {
            if hold.is_some() && RELEASE.is_match(lower) {
                hold = None;
                continue;
            }
            if !s.m || !HOLD.is_match(lower) {
                continue;
            }
            let other = numbers(lower).iter().any(|n| *n != r.number);
            if other && !THIS_IT_HERE.is_match(lower) {
                continue;
            }
            match &mut hold {
                None => hold = Some((Hit { said: s, i }, None)),
                Some((_, again)) => *again = Some(s.at),
            }
        }
    }
    if hold.as_ref().is_some_and(|(h, _)| f.closed_after(h.at())) {
        hold = None;
    }
    let hold_info = hold.as_ref().map(|(h, again)| {
        let mut owners = f.owners();
        if owners.is_empty() {
            owners = OWNER_AT
                .captures(h.lower())
                .map(|m| vec![canonical(&m[1])])
                .unwrap_or_default();
        }
        Hold {
            since: Some(h.at()),
            by: Some(h.said.who.clone()),
            category: hold_category(h.lower()).to_string(),
            reaffirmed: *again,
            owners,
        }
    });
    if let (Some((h, _)), Some(info)) = (&hold, &hold_info) {
        c.push(Cand {
            order: 10,
            state: "held_for_owner",
            waits: vec![Role::Owner],
            waits_for: info.owners.clone(),
            since: Some(h.at()),
            guess: true,
            rule: "10: a maintainer's hold phrase, not lifted since".into(),
            why: format!("{}; no later release phrase found", h.why(&HOLD)),
            evidence: Some(h.evidence(&HOLD)),
            hold: Some(info.clone()),
            ..Default::default()
        });
    }

    // 11. Held for a maintainer's own task.
    let lifts = hits(f.said.iter(), &MTASK_LIFT);
    let task = hits(maintainer_said.iter().copied(), &MTASK)
        .into_iter()
        .rfind(|h| !MTASK_LIFT.is_match(h.lower()));
    if let Some(h) = task {
        let lifted = lifts.iter().any(|l| l.at() > h.at()) || f.closed_after(h.at());
        if !lifted {
            c.push(Cand {
                order: 11,
                state: "held_for_maintainer_task",
                waits: vec![Role::Maintainer],
                waits_for: vec![h.said.who.clone()],
                since: Some(h.at()),
                guess: true,
                rule: "11: a maintainer named a task of their own (sign-off, dry run, release pipeline)".into(),
                why: format!("{}; no later sign-off or passed dry run", h.why(&MTASK)),
                evidence: Some(h.evidence(&MTASK)),
                ..Default::default()
            });
        }
    }

    // 12. Declined in a comment, and the author has not reworked it since.
    if let Some(h) = hits(others_m.iter().copied(), &DECLINE).pop() {
        if Facts::after(&f.events, h.at()).is_none() {
            c.push(Cand {
                order: 12,
                state: "declined_in_comment_still_open",
                waits: vec![Role::Author],
                waits_for: f.author_vec(),
                since: Some(h.at()),
                guess: true,
                rule: "12: a maintainer declined it in a comment".into(),
                why: format!("{}; the author has not answered since", h.why(&DECLINE)),
                evidence: Some(h.evidence(&DECLINE)),
                ..Default::default()
            });
        }
    }

    // 13. Superseded but still open.
    if let Some(h) = hits(others_m.iter().copied(), &SUPERSEDE_OPEN).pop() {
        c.push(Cand {
            order: 13,
            state: "superseded_still_open",
            waits: vec![Role::Maintainer],
            waits_for: vec![h.said.who.clone()],
            since: Some(h.at()),
            guess: true,
            rule: "13: a maintainer said other work carries or covers it".into(),
            why: h.why(&SUPERSEDE_OPEN),
            evidence: Some(h.evidence(&SUPERSEDE_OPEN)),
            ..Default::default()
        });
    }

    // 15. The author parked it pending a decision, after a maintainer comment.
    if let Some(last) = f.author_last() {
        if let Some(h) = hit_in(last, &AUTHOR_HOLD) {
            let before = others_m.iter().any(|s| s.at < last.at);
            let since = others_m.iter().any(|s| s.at > last.at);
            if before && !since {
                let mut waits_for = f.owners();
                waits_for.extend(f.last_m_who());
                c.push(Cand {
                    order: 15,
                    state: "author_declared_hold",
                    waits: vec![Role::Maintainer, Role::Owner],
                    waits_for,
                    since: Some(last.at),
                    guess: true,
                    low: true,
                    rule: "15: the author parked it pending a decision".into(),
                    why: format!("{}; no maintainer comment since", h.why(&AUTHOR_HOLD)),
                    evidence: Some(h.evidence(&AUTHOR_HOLD)),
                    ..Default::default()
                });
            }
        }
    }

    // 18. Depends on another open pull request.
    let body = Said {
        at: r.created_at.unwrap_or(ctx.now),
        who: f.author.clone().unwrap_or_default(),
        m: f.author_m,
        author: true,
        review: None,
        commit: None,
        url: r.url.clone(),
        sents: sentences(&r.body),
    };
    let dep_said = std::iter::once(&body).chain(f.said.iter().filter(|s| s.m || s.author));
    let mut blocked: Vec<u64> = Vec::new();
    let mut dep_hit = None;
    for h in hits(dep_said, &DEP) {
        let open: Vec<u64> = numbers(h.lower())
            .into_iter()
            .filter(|n| *n != r.number)
            .filter(|n| f.lookup(*n).is_some_and(|o| o.state == PrState::Open))
            .collect();
        if !open.is_empty() {
            for n in open {
                if !blocked.contains(&n) {
                    blocked.push(n);
                }
            }
            dep_hit = Some(h);
        }
    }
    if let Some(h) = dep_hit {
        let named = blocked.iter().map(|n| format!("#{n}")).collect::<Vec<_>>();
        c.push(Cand {
            order: 18,
            state: "depends_on_other_pr",
            waits: vec![Role::OtherPr],
            since: Some(h.at()),
            guess: true,
            rule: "18: the text names a pull request this one waits for, and it is open".into(),
            why: format!("{}; {} still open", h.why(&DEP), named.join(", ")),
            evidence: Some(h.evidence(&DEP)),
            blocked_by: blocked.clone(),
            ..Default::default()
        });
    }

    // 20. Draft.
    let author_role = if f.author_m {
        Role::Maintainer
    } else {
        Role::Author
    };
    if r.is_draft {
        c.push(Cand {
            order: 20,
            state: "draft",
            waits: vec![author_role],
            waits_for: f.author_vec(),
            since: f.draft_at.or(r.created_at),
            rule: "20: isDraft".into(),
            ..Default::default()
        });
    }

    // 21. Marked not ready: a WIP title, or the author's latest sentence.
    if WIP_TITLE.is_match(&r.title) {
        c.push(Cand {
            order: 21,
            state: "author_marked_not_ready",
            waits: vec![Role::Author],
            waits_for: f.author_vec(),
            since: r.created_at,
            rule: "21: the title starts with WIP or do not merge".into(),
            ..Default::default()
        });
    } else if let Some(h) = f.author_last().and_then(|s| hits([s], &NOTREADY).pop()) {
        if Facts::after(&f.pushes, h.at()).is_none() {
            c.push(Cand {
                order: 21,
                state: "author_marked_not_ready",
                waits: vec![Role::Author],
                waits_for: f.author_vec(),
                since: Some(h.at()),
                guess: true,
                rule: "21: the author said it is not ready".into(),
                why: format!("{}; no push since", h.why(&NOTREADY)),
                evidence: Some(h.evidence(&NOTREADY)),
                ..Default::default()
            });
        }
    }

    // 31. Competing pull requests for a maintainer to choose between.
    let depends_on_this = |n: u64| {
        f.lookup(n).is_some_and(|o| {
            sentences(&o.body)
                .iter()
                .any(|(_, l)| DEP.is_match(l) && numbers(l).contains(&r.number))
        })
    };
    // Named competitors that closed or merged leave no choice; a number the
    // records do not hold counts as open.
    let text_choice = hits(others_m.iter().copied(), &COMPETE)
        .pop()
        .and_then(|h| {
            let named: Vec<u64> = numbers(h.lower())
                .into_iter()
                .filter(|n| *n != r.number && !blocked.contains(n) && !depends_on_this(*n))
                .collect();
            let competing: Vec<u64> = named
                .iter()
                .copied()
                .filter(|n| f.lookup(*n).is_none_or(|o| o.state == PrState::Open))
                .collect();
            (named.is_empty() || !competing.is_empty()).then_some((h, competing))
        });
    if let Some((h, competing)) = text_choice {
        c.push(Cand {
            order: 31,
            state: "competing_prs_maintainer_choice",
            waits: vec![Role::Maintainer],
            waits_for: vec![h.said.who.clone()],
            since: Some(h.at()),
            guess: true,
            rule: "31: a maintainer has to choose between pull requests".into(),
            why: h.why(&COMPETE),
            evidence: Some(h.evidence(&COMPETE)),
            competing,
            ..Default::default()
        });
    } else {
        let mine = closing_keys(r, ctx.repo);
        let shared: Vec<u64> = ctx
            .cache
            .records
            .values()
            .filter(|o| o.number != r.number && o.state == PrState::Open)
            .filter(|o| closing_keys(o, ctx.repo).iter().any(|k| mine.contains(k)))
            .map(|o| o.number)
            .filter(|n| !blocked.contains(n) && !depends_on_this(*n))
            .collect();
        if !shared.is_empty() {
            let issues = mine
                .iter()
                .map(|(repo, n)| {
                    if repo.eq_ignore_ascii_case(ctx.repo) {
                        format!("#{n}")
                    } else {
                        format!("{repo}#{n}")
                    }
                })
                .collect::<Vec<_>>();
            c.push(Cand {
                order: 31,
                state: "competing_prs_maintainer_choice",
                subtype: Some("possible_competitor".into()),
                waits: vec![Role::Maintainer],
                since: r.created_at,
                low: true,
                rule: "31: another open pull request closes the same issue".into(),
                why: format!(
                    "a possible competitor: it shares {} with another open pull request",
                    issues.join(", ")
                ),
                competing: shared,
                ..Default::default()
            });
        }
    }

    // 32. Asked to move the code to the shared library.
    if let Some(h) = hits(others_m.iter().copied(), &CRAFTLIBS).pop() {
        if Facts::after(&f.pushes, h.at()).is_none() {
            c.push(Cand {
                order: 32,
                state: "redirected_to_shared_library",
                waits: vec![Role::Author],
                waits_for: f.author_vec(),
                since: Some(h.at()),
                guess: true,
                rule: "32: a maintainer asked for the code to move to craft-libs".into(),
                why: format!("{}; no push since", h.why(&CRAFTLIBS)),
                evidence: Some(h.evidence(&CRAFTLIBS)),
                ..Default::default()
            });
        }
    }

    // 40. A maintainer request the author has not answered: the latest
    // unanswered one that no later approval by its maintainer resolved.
    let requests: Vec<Req> = f.requests().into_iter().filter(|q| !q.resolved).collect();
    let unanswered = requests.iter().rev().find(|q| q.answered.is_none());
    let req_open = unanswered.is_some();
    let mut answered_req: Option<(Time, Time, &Said, Option<Hit>)> = None;
    if let Some(q) = unanswered {
        let h = q.hit;
        let (evidence, why) = match h {
            Some(h) => (
                Some(h.evidence(&REQ)),
                format!("{}; no answer since", h.why(&REQ)),
            ),
            None => (None, String::new()),
        };
        c.push(Cand {
            order: 40,
            state: "changes_requested_by_maintainer",
            subtype: Some(q.subtype.to_string()),
            waits: vec![author_role],
            waits_for: f.author_vec(),
            since: Some(q.at()),
            guess: h.is_some(),
            rule: if h.is_some() {
                "40: a maintainer's request phrase, not answered since".into()
            } else {
                "40: a maintainer review requested changes".into()
            },
            why,
            evidence,
            ..Default::default()
        });
    } else if let Some(q) = requests.last() {
        answered_req = q.answered.map(|answer| (q.at(), answer, q.said, q.hit));
    }

    // 41. A community review requests changes.
    let community: Vec<&Said> = f.said.iter().filter(|s| s.community()).collect();
    // A change request that only asks for the gate to run counts here
    // whoever wrote it, and never as a maintainer verdict.
    let community_req = f.said.iter().rev().find(|s| {
        let changes = s.review.as_deref() == Some("CHANGES_REQUESTED");
        let findings = s.sents.iter().any(|(_, l)| FINDINGS.is_match(l));
        let asked = (s.community() && (changes || findings)) || (changes && s.gate_only());
        !s.author && asked && f.approved_after(&s.who, s.at).is_none()
    });
    let mut community_answered: Option<(Time, Time, Option<Hit>)> = None;
    if let Some(s) = community_req {
        let gate = s.gate_only();
        match Facts::after(&f.events, s.at) {
            None if !req_open => {
                let h = hit_in(s, &FINDINGS);
                let review = s.review.as_deref() == Some("CHANGES_REQUESTED");
                c.push(Cand {
                    order: 41,
                    state: "community_changes_requested",
                    subtype: gate.then(|| "gate_request".to_string()),
                    waits: vec![Role::Author],
                    waits_for: f.author_vec(),
                    since: Some(s.at),
                    guess: !review,
                    rule: if review {
                        "41: a community review requested changes".into()
                    } else {
                        "41: a community review or comment lists findings".into()
                    },
                    why: h
                        .map(|h| format!("{}; no author answer since", h.why(&FINDINGS)))
                        .unwrap_or_default(),
                    evidence: h.map(|h| h.evidence(&FINDINGS)),
                    ..Default::default()
                });
                if gate {
                    flags.push("community_gate_request".into());
                }
            }
            None => {}
            Some(answer) => {
                // A findings comment, not a review state, is text.
                let text = (s.review.as_deref() != Some("CHANGES_REQUESTED"))
                    .then(|| hit_in(s, &FINDINGS))
                    .flatten();
                community_answered = Some((s.at, answer, text));
            }
        }
    }

    // 50 to 54. Conflicts, then checks while the head does not conflict.
    let conflicting = r.checks.mergeable == CONFLICTING;
    let rebase_flag = conflicting && ctx.rebase && r.maintainer_can_modify && !f.author_m;
    if rebase_flag {
        flags.push("conflict".into());
    } else if conflicting {
        c.push(Cand {
            order: 50,
            state: "merge_conflict",
            waits: vec![author_role],
            waits_for: f.author_vec(),
            since: r.checks.conflict_since,
            rule: "50: mergeable is CONFLICTING".into(),
            ..Default::default()
        });
    }
    let actions: Vec<_> = r
        .checks
        .suites
        .iter()
        .filter(|s| s.app.as_deref() == Some("github-actions"))
        .collect();
    if !conflicting {
        let approval = !actions.is_empty()
            && actions.iter().all(|s| {
                s.status == "COMPLETED" && s.conclusion.as_deref() == Some("ACTION_REQUIRED")
            });
        if approval {
            c.push(Cand {
                order: 51,
                state: "ci_awaiting_workflow_approval",
                waits: vec![Role::Maintainer],
                since: actions.iter().filter_map(|s| s.created_at).min(),
                rule: "51: every github-actions suite on the head waits for approval".into(),
                ..Default::default()
            });
        }
        let rollup = r.checks.rollup.as_deref();
        if matches!(rollup, Some("FAILURE" | "ERROR")) {
            let main_failing = ctx.main.map(|m| m.failing.as_slice()).unwrap_or_default();
            let own: Vec<&String> = r
                .checks
                .failing
                .iter()
                .filter(|n| !main_failing.contains(n))
                .collect();
            // Until the rest of a long check list is read, a check of the
            // pull request's own may be failing among the unread ones.
            let cut = r.checks.contexts_cursor.is_some();
            let inherited = !r.checks.failing.is_empty() && own.is_empty() && !cut;
            let names = r.checks.failing.join(", ");
            // The newest suite on the head started the run that failed.
            let since = r.checks.suites.iter().filter_map(|s| s.created_at).max();
            if inherited {
                c.push(Cand {
                    order: 53,
                    state: "ci_failing_inherited_from_main",
                    waits: vec![Role::Maintainer],
                    since,
                    rule: format!("53: every failing check also fails on main ({names})"),
                    ..Default::default()
                });
            } else {
                c.push(Cand {
                    order: 52,
                    state: "ci_failing_from_pr",
                    waits: vec![Role::Author],
                    waits_for: f.author_vec(),
                    since,
                    low: ctx.main.is_none() || cut,
                    rule: format!("52: checks fail that pass on main ({names})"),
                    ..Default::default()
                });
            }
        }
        let running = actions
            .iter()
            .any(|s| matches!(s.status.as_str(), "QUEUED" | "IN_PROGRESS"));
        if rollup == Some("PENDING") && running {
            c.push(Cand {
                order: 54,
                state: "ci_running",
                waits: vec![Role::Ci],
                since: actions.iter().filter_map(|s| s.created_at).max(),
                rule: "54: a github-actions suite is queued or running".into(),
                ..Default::default()
            });
        }
    }

    // 60. The author answered a request.
    // A request read from text keeps this state a guess, with its sentence.
    let answer = match (answered_req, community_answered) {
        (Some((at, answer, s, h)), _) if !req_open => Some((at, answer, Some(s), h, &*REQ)),
        (_, Some((at, answer, h))) if !req_open => Some((at, answer, None, h, &*FINDINGS)),
        _ => None,
    };
    if let Some((at, answer, by, asked, re)) = answer {
        let after: Vec<&Said> = f.said.iter().filter(|s| s.author && s.at > at).collect();
        let confirm = hits(after.iter().copied(), &ANSWERED).into_iter().next();
        let replied = others_m.iter().any(|s| s.at > answer);
        if !replied {
            let evidence = confirm
                .map(|h| h.evidence(&ANSWERED))
                .or_else(|| asked.map(|h| h.evidence(re)));
            let why = asked
                .map(|h| format!("the request {}; the author answered after it", h.why(re)))
                .unwrap_or_default();
            c.push(Cand {
                order: 60,
                state: "author_responded_awaiting_maintainer",
                waits: vec![Role::Maintainer],
                waits_for: by
                    .map(|s| vec![s.who.clone()])
                    .unwrap_or_else(|| f.last_m_who()),
                since: Some(answer),
                guess: asked.is_some(),
                low: confirm.is_none(),
                rule: if confirm.is_some() {
                    "60: the author answered a request and said so".into()
                } else {
                    "60: the author pushed or commented after a request".into()
                },
                why,
                evidence,
                ..Default::default()
            });
        }
    }

    // 61. A community approval of the current head, after maintainer contact.
    let last_push = f.pushes.last().copied().or(r.created_at);
    let m_since_push = others_m.iter().any(|s| last_push.is_none_or(|p| s.at > p));
    let approval = community
        .iter()
        .rev()
        .find(|s| s.review.as_deref() == Some("APPROVED"))
        .copied();
    let current_approval =
        approval.filter(|s| s.commit.as_deref() == Some(r.head_ref_oid.as_str()));
    if approval.is_some() && current_approval.is_none() {
        flags.push("stale_approval".into());
    }
    let ok_said = hits(community.iter().copied(), &COMMUNITY_OK)
        .into_iter()
        .rfind(|h| last_push.is_none_or(|p| h.at() > p));
    if f.m_engaged() && !m_since_push && (current_approval.is_some() || ok_said.is_some()) {
        let (since, guess, why, evidence) = match (current_approval, ok_said) {
            (Some(s), _) => (Some(s.at), false, String::new(), None),
            (None, Some(h)) => (
                Some(h.at()),
                true,
                h.why(&COMMUNITY_OK),
                Some(h.evidence(&COMMUNITY_OK)),
            ),
            (None, None) => (None, false, String::new(), None),
        };
        c.push(Cand {
            order: 61,
            state: "community_approved_awaiting_maintainer",
            waits: vec![Role::Maintainer],
            waits_for: f.last_m_who(),
            since,
            guess,
            rule: "61: a community reviewer approved the current head".into(),
            why,
            evidence,
            ..Default::default()
        });
    }

    // 62. Ready to merge after maintainer contact.
    let no_ci = actions.is_empty() && r.checks.suites.is_empty() && r.checks.rollup.is_none();
    if no_ci {
        flags.push("no_ci_signal".into());
    }
    let green = r.checks.rollup.as_deref() == Some("SUCCESS") || no_ci;
    if f.m_engaged() && !req_open && r.checks.merge_state == "CLEAN" && green {
        c.push(Cand {
            order: 62,
            state: "maintainer_engaged_ready_to_merge",
            waits: vec![Role::Maintainer],
            waits_for: f.last_m_who(),
            since: f.last_m().map(|s| s.at).max(last_push),
            rule: "62: CLEAN with green or no checks, after maintainer contact".into(),
            ..Default::default()
        });
    }

    // 63. No maintainer response yet.
    let m_closed = f
        .closed_by
        .as_deref()
        .is_some_and(|who| ctx.maintainers.contains(who) && f.author.as_deref() != Some(who));
    let touched = f.m_engaged() || !f.m_pushes.is_empty() || m_closed;
    if !touched {
        if approval.is_some() {
            flags.push("community_approved".into());
        }
        c.push(Cand {
            order: 63,
            state: "awaiting_first_maintainer_response",
            waits: vec![author_role_for_first(f)],
            waits_for: if f.author_m {
                f.author_vec()
            } else {
                Vec::new()
            },
            since: f.ready_at,
            rule: "63: no maintainer comment, review, push or close yet".into(),
            ..Default::default()
        });
    }

    // Anything else waits on a maintainer.
    if c.is_empty() {
        c.push(Cand {
            order: 99,
            state: "awaiting_maintainer",
            waits: vec![Role::Maintainer],
            waits_for: f.last_m_who(),
            since: f.events.last().copied().max(r.created_at),
            rule: "no other rule matched after maintainer contact".into(),
            ..Default::default()
        });
    }

    // A request at or after the hold outranks it, and waits on both.
    let hold_at = hold.as_ref().map(|(h, _)| h.at());
    if let (Some(h), Some(req)) = (hold_at, c.iter_mut().find(|x| x.order == 40)) {
        if req.since.is_some_and(|s| s >= h) {
            req.order = 9;
            req.waits = vec![author_role, Role::Owner];
            req.waits_for.extend(
                hold_info
                    .as_ref()
                    .map(|i| i.owners.clone())
                    .unwrap_or_default(),
            );
            req.hold = hold_info.clone();
            flags.push("also_held_for_owner".into());
        }
    }

    // Flags that never replace the state.
    if let Some(last) = f.human.iter().max() {
        let idle = ctx.now - *last;
        if idle >= chrono::Duration::days(STALE_DAYS) {
            flags.push("stale".into());
        } else if idle >= chrono::Duration::days(AGING_DAYS) {
            flags.push("aging".into());
        }
    }
    if r.checks.mergeable.is_empty() || r.checks.mergeable == UNKNOWN {
        flags.push("mergeability_unknown".into());
    }
    if f.author_m {
        flags.push("maintainer_authored".into());
    }
    if !r.assignees.is_empty() {
        flags.push("assigned".into());
    }
    finish(c, flags)
}

/// Who a pull request with no maintainer response waits on: a maintainer,
/// or the author when the author is one.
fn author_role_for_first(f: &Facts) -> Role {
    if f.author_m {
        Role::Author
    } else {
        Role::Maintainer
    }
}

/// The first candidate by rule order, with the others as `also`.
fn finish(mut c: Vec<Cand>, mut flags: Vec<String>) -> WorkState {
    c.sort_by_key(|x| x.order);
    let mut rest = c.split_off(1);
    let Some(first) = c.pop() else {
        return WorkState::default();
    };
    let mut also: Vec<String> = Vec::new();
    for x in rest.drain(..) {
        if x.state != first.state && !also.iter().any(|a| a == x.state) {
            also.push(x.state.to_string());
        }
    }
    flags.dedup();
    let mut waits_for: Vec<String> = Vec::new();
    for w in first.waits_for {
        if !w.is_empty() && !waits_for.contains(&w) {
            waits_for.push(w);
        }
    }
    WorkState {
        state: first.state.to_string(),
        subtype: first.subtype,
        waits_on: first.waits,
        waits_for,
        since: first.since,
        guess: first.guess,
        low: first.low,
        rule: first.rule,
        why: first.why,
        evidence: first.evidence,
        also,
        flags,
        hold: first.hold,
        blocked_by: first.blocked_by,
        competing: first.competing,
    }
}

fn closed_state(f: &Facts) -> WorkState {
    let r = f.r;
    let ctx = f.ctx;
    let mut flags: Vec<String> = Vec::new();
    let closed_at = r.closed_at.or(r.merged_at);
    // Maintainer comments over its life: up to the close.
    let others_m: Vec<&Said> = f
        .said
        .iter()
        .filter(|s| s.m && !s.author && closed_at.is_none_or(|c| s.at <= c))
        .collect();
    if others_m.is_empty() {
        flags.push("no_m_response_over_life".into());
    }
    if let Some(at) = r.closed_at.filter(|_| r.state == PrState::Closed) {
        let disputed = f
            .said
            .iter()
            .filter(|s| s.author && s.at > at)
            .any(|s| s.sents.iter().any(|(_, l)| DISPUTE.is_match(l)));
        if disputed {
            flags.push("disputed_after_close".into());
        }
    }
    let merged = |n: u64| f.lookup(n).is_some_and(|o| o.state == PrState::Merged);
    let open = |n: u64| f.lookup(n).is_some_and(|o| o.state == PrState::Open);
    let decider: Vec<&Said> = f
        .said
        .iter()
        .filter(|s| (s.m || s.author) && closed_at.is_none_or(|c| s.at <= c))
        .collect();
    let mut c = Cand {
        since: closed_at,
        ..Default::default()
    };
    let from_hit = |c: &mut Cand, h: &Hit, re: &Regex| {
        c.guess = true;
        c.evidence = Some(h.evidence(re));
        c.why = h.why(re);
    };
    // A cross-reference from this repository, and one from this pull request.
    let local = |s: &CrossSource| s.repo.is_empty() || s.repo.eq_ignore_ascii_case(ctx.repo);
    let is_self = |s: &CrossSource| local(s) && s.number == r.number;

    // 1. Merged, with its subtype.
    if r.state == PrState::Merged {
        c.state = "merged";
        c.since = r.merged_at;
        c.rule = "1: state is MERGED".into();
        let m_at = r.merged_at;
        let near = |t: Option<Time>, mins: i64| match (t, m_at) {
            (Some(t), Some(m)) => (t - m).num_seconds().abs() <= mins * 60,
            _ => false,
        };
        let batch_ref = r.timeline.iter().any(|t| {
            t.kind == "CrossReferencedEvent"
                && t.source.as_ref().is_some_and(|s| {
                    s.kind == "PullRequest"
                        && !is_self(s)
                        && s.merged_at.is_some()
                        && BATCH_TITLE.is_match(&s.title)
                        && near(s.merged_at, 15)
                })
        });
        let batch_said = hits(others_m.iter().copied(), &LANDED)
            .into_iter()
            .any(|h| h.lower().contains("batch"));
        // Other merges by the same person within a minute, from the index.
        let merger = merger_of(r);
        let minute = chrono::Duration::minutes(1);
        let same_minute: Vec<&MergeAt> = match (m_at, &merger) {
            (Some(m), Some(by)) => {
                let from = ctx.merges.partition_point(|x| x.at < m - minute);
                ctx.merges[from..]
                    .iter()
                    .take_while(|x| x.at <= m + minute)
                    .filter(|x| x.number != r.number && &x.merger == by)
                    .collect()
            }
            _ => Vec::new(),
        };
        let two_parents = r
            .contribution
            .merge_commit
            .as_ref()
            .is_some_and(|m| m.parents == 2);
        let batch_merge =
            same_minute.len() >= 2 && two_parents && same_minute.iter().all(|x| x.two_parents);
        let followups = r.contribution.commits.iter().any(|c| {
            c.author.as_deref().is_some_and(|a| {
                ctx.maintainers.contains(&canonical(a))
                    && f.author.as_deref() != Some(canonical(a).as_str())
            })
        }) || f
            .m_force
            .iter()
            .any(|t| m_at.is_some_and(|m| *t <= m && m - *t <= chrono::Duration::minutes(30)));
        let subtype = if batch_ref || batch_said || batch_merge {
            "batch"
        } else if followups {
            "maintainer_followups"
        } else {
            "direct"
        };
        c.subtype = Some(subtype.into());
        if subtype == "direct" {
            if let Some(h) = hits(others_m.iter().copied(), &MERGED_DIRECT).pop() {
                c.evidence = Some(h.evidence(&MERGED_DIRECT));
            }
        }
        return finish(vec![c], flags);
    }

    let closer = r.timeline.iter().rev().find(|t| t.kind == "ClosedEvent");
    let closed_by = f.closed_by.clone();
    let by_m = closed_by
        .as_deref()
        .is_some_and(|w| ctx.maintainers.contains(w));
    let by_author = closed_by.is_some() && closed_by == f.author;

    // 2. Landed elsewhere.
    let m_merged_ref = r.timeline.iter().find_map(|t| {
        let s = t.source.as_ref()?;
        let by = s.author.as_deref().map(canonical)?;
        let in_window = match (s.merged_at, r.closed_at) {
            (Some(m), Some(c)) => m <= c && c - m <= chrono::Duration::minutes(60),
            _ => false,
        };
        (t.kind == "CrossReferencedEvent"
            && s.kind == "PullRequest"
            && ctx.maintainers.contains(&by)
            && in_window)
            .then_some(s.number)
    });
    let landed_said = hits(decider.iter().copied(), &LANDED)
        .into_iter()
        .rfind(|h| {
            let ns = numbers(h.lower());
            ns.iter().all(|n| merged(*n)) && (!h.lower().contains("credited") || !ns.is_empty())
        });
    if let Some(cl) = closer.and_then(|t| t.closer.as_ref()) {
        c.state = "closed_landed_elsewhere";
        c.rule = format!(
            "2: closed by a {}",
            if cl.kind == "Commit" {
                "commit"
            } else {
                "pull request"
            }
        );
    } else if let Some(n) = m_merged_ref {
        c.state = "closed_landed_elsewhere";
        c.rule = format!("2: maintainer pull request #{n} merged within the hour before the close");
    } else if let Some(h) = landed_said {
        c.state = "closed_landed_elsewhere";
        c.rule = "2: a sentence says it landed through a merged pull request".into();
        from_hit(&mut c, &h, &LANDED);
    }
    if !c.state.is_empty() {
        return finish(vec![c], flags);
    }

    // 3. Duplicate, or yielded to an open pull request.
    let last = decider.last().copied();
    let favor = decider.iter().rev().find_map(|s| {
        s.sents.iter().enumerate().find_map(|(i, (_, l))| {
            FAVOR
                .captures(l)
                .and_then(|m| m[1].parse::<u64>().ok())
                .map(|n| (Hit { said: s, i }, n))
        })
    });
    if let Some(h) = last.and_then(|s| hit_in(s, &DUP)) {
        c.state = "closed_duplicate";
        c.rule = "3: the last comment before the close calls it a duplicate".into();
        from_hit(&mut c, &h, &DUP);
        return finish(vec![c], flags);
    }
    if let Some((h, n)) = favor.filter(|(_, n)| open(*n)) {
        c.state = "closed_duplicate";
        c.subtype = Some("author_yielded".into());
        c.rule = format!("3: closed in favor of #{n}, which is open");
        from_hit(&mut c, &h, &FAVOR);
        c.competing = vec![n];
        return finish(vec![c], flags);
    }

    // 4. Folded, split or moved by the author.
    if by_author {
        // The author's last comment at or before the close.
        let fold = decider
            .iter()
            .rev()
            .find(|s| s.author)
            .and_then(|s| hit_in(s, &FOLD));
        let split_at = f
            .requests()
            .into_iter()
            .filter(|q| q.subtype == "split")
            .map(|q| q.at())
            .next_back()
            .or(r.created_at);
        let mut children: BTreeSet<(String, u64)> = BTreeSet::new();
        for t in r
            .timeline
            .iter()
            .filter(|t| t.kind == "CrossReferencedEvent")
        {
            let Some(s) = &t.source else { continue };
            let same = s.author.as_deref().map(canonical) == f.author
                && !is_self(s)
                && s.kind == "PullRequest";
            // Only a source in this repository has its record here.
            let record = local(s).then(|| f.lookup(s.number)).flatten();
            let opened = record.and_then(|o| o.created_at).or(t.at);
            let inside = opened.is_some_and(|o| {
                split_at.is_none_or(|a| o >= a) && r.closed_at.is_none_or(|c| o <= c)
            });
            if same && inside {
                children.insert((s.repo.to_ascii_lowercase(), s.number));
            }
        }
        if let Some(h) = fold {
            c.state = "closed_folded_split_or_moved";
            c.rule = "4: the author's last comment says the work moved".into();
            from_hit(&mut c, &h, &FOLD);
        } else if children.len() >= 2 {
            c.state = "closed_folded_split_or_moved";
            c.rule = format!(
                "4: {} pull requests by the same author reference it",
                children.len()
            );
        }
        if !c.state.is_empty() {
            return finish(vec![c], flags);
        }
    }

    // 5. Superseded.
    let names_merged = last.and_then(|s| {
        s.sents
            .iter()
            .enumerate()
            .find(|(_, (_, l))| numbers(l).iter().any(|n| *n != r.number && merged(*n)))
            .map(move |(i, _)| Hit { said: s, i })
    });
    let superseded = last
        .and_then(|s| {
            hit_in(s, &SUPERSEDE).map(|h| {
                (
                    h,
                    "5: the last comment before the close says it is superseded",
                )
            })
        })
        .or_else(|| {
            favor
                .filter(|(_, n)| merged(*n))
                .map(|(h, _)| (h, "5: closed in favor of a merged pull request"))
        })
        .or_else(|| {
            by_author
                .then(|| {
                    hits(others_m.iter().copied(), &SUPERSEDE)
                        .into_iter()
                        .rfind(|h| numbers(h.lower()).iter().any(|n| merged(*n)))
                })
                .flatten()
                .map(|h| {
                    (
                        h,
                        "5: a maintainer said a merged pull request supersedes it",
                    )
                })
        })
        .or_else(|| names_merged.map(|h| (h, "5: the last comment names a merged pull request")));
    let m_closed_merged_ref = by_m
        && r.timeline.iter().any(|t| {
            t.kind == "CrossReferencedEvent"
                && t.source.as_ref().is_some_and(|s| {
                    s.kind == "PullRequest" && s.merged_at.is_some() && !is_self(s)
                })
        });
    if superseded.is_some() || m_closed_merged_ref {
        c.state = "closed_superseded";
        c.subtype = Some(if by_m { "by_maintainer" } else { "by_author" }.into());
        match superseded {
            Some((h, rule)) => {
                c.rule = rule.into();
                let re: &Regex = if SUPERSEDE.is_match(h.lower()) {
                    &SUPERSEDE
                } else if FAVOR.is_match(h.lower()) {
                    &FAVOR
                } else {
                    &NUMBER
                };
                from_hit(&mut c, &h, re);
            }
            None => {
                c.rule = "5: a maintainer closed it and a merged pull request references it".into()
            }
        }
        return finish(vec![c], flags);
    }

    // 6. Declined by a maintainer.
    let declined = others_m
        .iter()
        .filter(|s| closed_at.is_none_or(|c| s.at <= c))
        .flat_map(|s| hits([*s], &DECLINE).into_iter().chain(hits([*s], &REQ)))
        .max_by_key(|h| h.at());
    if let Some(h) = declined.filter(|h| {
        Facts::after(&f.events, h.at()).is_none_or(|e| closed_at.is_some_and(|c| e > c))
    }) {
        c.state = "closed_declined";
        c.rule =
            "6: a maintainer's decline or request with no author answer before the close".into();
        let re: &Regex = if DECLINE.is_match(h.lower()) {
            &DECLINE
        } else {
            &REQ
        };
        from_hit(&mut c, &h, re);
        return finish(vec![c], flags);
    }

    // 7. Withdrawn or abandoned.
    c.state = "closed_withdrawn_or_abandoned";
    let quick = match (r.created_at, r.closed_at) {
        (Some(o), Some(cl)) => cl - o <= chrono::Duration::hours(1),
        _ => false,
    };
    let community_push = f.said.iter().any(|s| {
        s.community()
            && (s.review.as_deref() == Some("CHANGES_REQUESTED")
                || s.sents.iter().any(|(_, l)| FINDINGS.is_match(l)))
    });
    let (subtype, rule) =
        if quick && r.body.trim().is_empty() && f.said.is_empty() && r.comments_total == 0 {
            (
                "accidental_quick_close",
                "7: closed within an hour of opening, with no body or comments",
            )
        } else if community_push && by_author {
            (
                "after_community_pushback",
                "7: the author closed it after a community review asked for changes",
            )
        } else if !others_m.is_empty() && by_author {
            (
                "after_pushback",
                "7: the author closed it after maintainer comments",
            )
        } else if others_m.is_empty() && by_author {
            c.low = true;
            (
                "no_maintainer_response",
                "7: the author closed it with no maintainer comment or review",
            )
        } else if quick {
            ("quick_close", "7: closed within an hour of opening")
        } else {
            ("unexplained", "7: no other closing rule matched")
        };
    c.subtype = Some(subtype.into());
    c.rule = rule.into();
    finish(vec![c], flags)
}

#[cfg(test)]
mod tests {
    use super::super::record::{
        Checks, Closer, ContextsCursor, Contribution, CrossSource, MergeCommit, PrCommit, Suite,
        TimelineItem,
    };
    use super::*;

    const AUTHOR: &str = "ann-example";
    const M: &str = "maint-one";
    const M2: &str = "maint-two";
    const OWNER: &str = "boss-one";
    const PEER: &str = "peer-reviewer";

    fn t(day: u32, hour: u32) -> Time {
        format!("2026-10-{day:02}T{hour:02}:00:00Z")
            .parse()
            .unwrap()
    }

    fn comment(who: &str, at: Time, body: &str) -> TimelineItem {
        TimelineItem {
            id: format!("c-{who}-{at}"),
            kind: "IssueComment".into(),
            at: Some(at),
            actor: Some(who.into()),
            body: Some(body.into()),
            url: Some(format!(
                "https://github.com/o/app/pull/1#issuecomment-{}",
                at.timestamp()
            )),
            ..Default::default()
        }
    }

    fn review(who: &str, at: Time, state: &str, body: &str, commit: &str) -> TimelineItem {
        TimelineItem {
            id: format!("r-{who}-{at}"),
            kind: "PullRequestReview".into(),
            at: Some(at),
            actor: Some(who.into()),
            state: Some(state.into()),
            body: Some(body.into()),
            commit: Some(commit.into()),
            ..Default::default()
        }
    }

    fn event(kind: &str, who: &str, at: Time) -> TimelineItem {
        TimelineItem {
            id: format!("e-{kind}-{at}"),
            kind: kind.into(),
            at: Some(at),
            actor: Some(who.into()),
            ..Default::default()
        }
    }

    fn commit(who: &str, at: Time) -> PrCommit {
        PrCommit {
            oid: format!("c{}", at.timestamp()),
            author: Some(who.into()),
            committed_at: Some(at),
            authored_at: Some(at),
            ..Default::default()
        }
    }

    fn pr(number: u64, timeline: Vec<TimelineItem>) -> PrRecord {
        PrRecord {
            number,
            url: format!("https://github.com/o/app/pull/{number}"),
            title: "Pen: smoother curves".into(),
            author: Some(AUTHOR.into()),
            author_association: "CONTRIBUTOR".into(),
            created_at: Some(t(1, 0)),
            fetched_at: Some(t(1, 0)),
            head_ref_oid: "head1".into(),
            checks: Checks {
                mergeable: "MERGEABLE".into(),
                merge_state: "BLOCKED".into(),
                ..Default::default()
            },
            contribution: Contribution {
                commits: vec![commit(AUTHOR, t(1, 0))],
                ..Default::default()
            },
            timeline,
            ..Default::default()
        }
    }

    struct World {
        maintainers: BTreeSet<String>,
        owners: Vec<String>,
        cache: PrCache,
        merges: Vec<MergeAt>,
        main: Option<MainChecks>,
        rebase: bool,
    }

    impl World {
        fn new() -> World {
            World {
                maintainers: [M, M2].iter().map(|s| s.to_string()).collect(),
                owners: vec![OWNER.into()],
                cache: PrCache::default(),
                merges: Vec::new(),
                main: None,
                rebase: false,
            }
        }

        fn with(mut self, r: PrRecord) -> World {
            self.cache.records.insert(r.number, r);
            self.merges = merge_index(&self.cache);
            self
        }

        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                repo: "o/app",
                maintainers: &self.maintainers,
                owners: &self.owners,
                rebase: self.rebase,
                main: self.main.as_ref(),
                cache: &self.cache,
                merges: &self.merges,
                now: t(20, 0),
            }
        }

        fn state(&self, r: &PrRecord) -> WorkState {
            classify(r, &self.ctx())
        }
    }

    fn state_of(r: &PrRecord) -> WorkState {
        World::new().state(r)
    }

    #[test]
    fn sentences_are_lowercase_without_quotes_or_code() {
        let s = sentences("Thanks! I’ll rebase. Version 1.2 is fine?\n> quoted owner's call\n```\nholding this for x\n```\nLast line");
        let lower: Vec<&str> = s.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(
            lower,
            [
                "thanks!",
                "i'll rebase.",
                "version 1.2 is fine?",
                "last line"
            ]
        );
        assert_eq!(s[1].0, "I'll rebase.", "the written form keeps its case");
        assert_eq!(numbers("see #12 and o/r#13, #14"), [12, 14]);
    }

    #[test]
    fn a_new_pull_request_waits_for_its_first_maintainer_response() {
        let mut r = pr(1, vec![review(PEER, t(2, 0), "APPROVED", "", "head1")]);
        r.timeline
            .push(event("ReadyForReviewEvent", AUTHOR, t(1, 5)));
        let s = state_of(&r);
        assert_eq!(s.state, "awaiting_first_maintainer_response");
        assert_eq!(s.waits_on, [Role::Maintainer]);
        assert_eq!(s.since, Some(t(1, 5)), "from the last ready-for-review");
        assert!(!s.guess);
        assert!(
            s.flags.contains(&"community_approved".into()),
            "{:?}",
            s.flags
        );
    }

    #[test]
    fn an_unread_record_is_not_read_yet_whatever_its_comment_count() {
        // The light sweep found it: two comments, an approval and a hold on
        // GitHub, none of them read yet.
        let mut r = pr(1, vec![]);
        r.fetched_at = None;
        r.comments_total = 2;
        r.updated_at = Some(t(15, 19));
        let s = state_of(&r);
        assert_eq!(s.state, NOT_READ_YET);
        assert_ne!(s.state, "awaiting_first_maintainer_response");
        assert!(s.waits_on.is_empty() && s.waits_for.is_empty());
        assert_eq!(s.since, Some(t(15, 19)));
        assert!(!s.guess && s.also.is_empty());
        // Read in full, the same record gets a rule's state.
        r.fetched_at = Some(t(16, 0));
        r.timeline = vec![comment(M, t(15, 19), "Held for the project owner.")];
        assert_eq!(state_of(&r).state, "held_for_owner");
    }

    #[test]
    fn an_owner_hold_waits_on_the_owner_with_its_sentence_and_category() {
        let r = pr(1, vec![
            comment(M, t(2, 0), "Thanks for this. It is the owner’s call on the brand question, so I’m holding it."),
            comment(M2, t(4, 0), "Still the owner's call."),
        ]);
        let s = state_of(&r);
        assert_eq!(s.state, "held_for_owner");
        assert_eq!(s.waits_on, [Role::Owner]);
        assert_eq!(s.waits_for, [OWNER]);
        assert!(s.guess);
        let e = s.evidence.as_ref().unwrap();
        assert!(e.quote.starts_with("It is the owner's call"), "{}", e.quote);
        assert_eq!(e.author.as_deref(), Some(M));
        assert!(e.url.contains("#issuecomment-"));
        assert_eq!(e.phrase, "owner's call");
        assert!(
            s.why.contains("owner's call") && s.why.contains(M),
            "{}",
            s.why
        );
        let h = s.hold.unwrap();
        assert_eq!(
            (h.category.as_str(), h.since, h.reaffirmed),
            ("brand", Some(t(2, 0)), Some(t(4, 0)))
        );
        // A release phrase lifts it.
        let mut lifted = pr(
            1,
            vec![comment(
                M,
                t(2, 0),
                "Holding this for the owner's decision.",
            )],
        );
        lifted
            .timeline
            .push(comment(M, t(3, 0), "The owner approved it, go ahead."));
        assert_ne!(state_of(&lifted).state, "held_for_owner");
        // So does the owner's own go-ahead, though the owner is not a maintainer.
        let held = |who: &str| pr(1, vec![comment(who, t(2, 0), "Held for the owner.")]);
        let mut decided = held(M);
        decided.timeline.push(comment(OWNER, t(3, 0), "Go ahead."));
        assert_ne!(state_of(&decided).state, "held_for_owner");
        // Someone else's go-ahead does not, and an owner who is not a
        // maintainer does not place a hold.
        let mut other = held(M);
        other.timeline.push(comment(PEER, t(3, 0), "Go ahead!"));
        assert_eq!(state_of(&other).state, "held_for_owner");
        assert_ne!(state_of(&held(OWNER)).state, "held_for_owner");
        // A hold that names only another pull request is not this one's.
        let elsewhere = pr(
            1,
            vec![comment(M, t(2, 0), "Policy decision on #77 pending.")],
        );
        assert_ne!(state_of(&elsewhere).state, "held_for_owner");
    }

    #[test]
    fn without_configured_owners_an_at_login_after_owner_names_one() {
        let mut w = World::new();
        w.owners.clear();
        let r = pr(
            1,
            vec![comment(
                M,
                t(2, 0),
                "This needs the project owner @Boss-Two to decide.",
            )],
        );
        assert_eq!(w.state(&r).waits_for, ["boss-two"]);
    }

    #[test]
    fn an_unanswered_request_after_a_hold_waits_on_author_and_owner() {
        let r = pr(
            1,
            vec![
                comment(M, t(2, 0), "Held for the project owner."),
                comment(M, t(3, 0), "Also, could you rebase onto main?"),
            ],
        );
        let s = state_of(&r);
        assert_eq!(s.state, "changes_requested_by_maintainer");
        assert_eq!(s.subtype.as_deref(), Some("rebase"));
        assert_eq!(s.waits_on, [Role::Author, Role::Owner]);
        assert_eq!(s.waits_for, [AUTHOR, OWNER]);
        assert!(s.flags.contains(&"also_held_for_owner".into()));
        assert!(s.also.contains(&"held_for_owner".into()));
        assert!(s.hold.is_some());
    }

    #[test]
    fn requests_are_answered_by_the_right_kind_of_author_event() {
        // A rebase request is answered only by a push.
        let mut r = pr(
            1,
            vec![
                comment(M, t(2, 0), "This needs a rebase."),
                comment(AUTHOR, t(3, 0), "Will do."),
            ],
        );
        let s = state_of(&r);
        assert_eq!(
            (s.state.as_str(), s.since),
            ("changes_requested_by_maintainer", Some(t(2, 0)))
        );
        r.contribution.commits.push(commit(AUTHOR, t(4, 0)));
        let s = state_of(&r);
        assert_eq!(s.state, "author_responded_awaiting_maintainer");
        assert_eq!(
            (s.since, s.waits_for.clone()),
            (Some(t(4, 0)), vec![M.to_string()])
        );
        // Other requests are answered by a comment too; "no need to" is not a request.
        let fixes = pr(
            1,
            vec![
                comment(M, t(2, 0), "A few things need fixing before merging."),
                comment(AUTHOR, t(3, 0), "Addressed in the latest push."),
            ],
        );
        let s = state_of(&fixes);
        assert_eq!(s.state, "author_responded_awaiting_maintainer");
        assert!(!s.low, "an ANSWERED sentence raises confidence");
        assert!(s.guess, "the request was read from text");
        let e = s.evidence.as_ref().unwrap();
        assert_eq!(e.quote, "Addressed in the latest push.");
        assert!(s.why.contains("a few things need fixing"), "{}", s.why);
        let none = pr(1, vec![comment(M, t(2, 0), "No need to split it.")]);
        assert_ne!(state_of(&none).state, "changes_requested_by_maintainer");
        // Marking it ready is not a push, so it leaves a rebase request open.
        let mut ready = pr(1, vec![comment(M, t(2, 0), "Please rebase.")]);
        ready
            .timeline
            .push(event("ReadyForReviewEvent", AUTHOR, t(3, 0)));
        assert_eq!(state_of(&ready).state, "changes_requested_by_maintainer");
        // A maintainer force-push answers a rebase request.
        let mut forced = pr(1, vec![comment(M, t(2, 0), "Please rebase.")]);
        forced
            .timeline
            .push(event("HeadRefForcePushedEvent", M, t(3, 0)));
        assert_ne!(state_of(&forced).state, "changes_requested_by_maintainer");
    }

    #[test]
    fn request_subtypes_follow_the_first_matching_pattern() {
        for (text, want) in [
            ("Could you split this into its own PR?", "split"),
            (
                "Is there a public page for this? Please add its URL.",
                "evidence_or_repro",
            ),
            ("Please allow edits by maintainers.", "allow_edits"),
            ("Sorry, we couldn't land it in this round.", "landing_round"),
            (
                "Please take out the unrelated file before this goes in.",
                "fixes",
            ),
        ] {
            let s = state_of(&pr(1, vec![comment(M, t(2, 0), text)]));
            assert_eq!(s.subtype.as_deref(), Some(want), "{text}");
        }
        let formal = state_of(&pr(
            1,
            vec![review(M, t(2, 0), "CHANGES_REQUESTED", "", "head1")],
        ));
        assert_eq!(
            (formal.state.as_str(), formal.guess),
            ("changes_requested_by_maintainer", false)
        );
    }

    #[test]
    fn community_reviews_request_changes_and_gate_requests_are_marked() {
        let r = pr(
            1,
            vec![review(
                PEER,
                t(2, 0),
                "CHANGES_REQUESTED",
                "## [P1] Panics on empty input",
                "head1",
            )],
        );
        let s = state_of(&r);
        assert_eq!(s.state, "community_changes_requested");
        assert!(!s.guess, "a formal review state is a field");
        let gate = pr(
            1,
            vec![review(
                M,
                t(2, 0),
                "CHANGES_REQUESTED",
                "cargo xtask ci is incomplete or unchecked",
                "head1",
            )],
        );
        let s = state_of(&gate);
        assert_ne!(
            s.state, "changes_requested_by_maintainer",
            "never a maintainer verdict"
        );
        assert_eq!(s.subtype.as_deref(), Some("gate_request"));
        assert!(s.flags.contains(&"community_gate_request".into()));
        // The gate phrase beside a finding is a maintainer request.
        let mixed = pr(
            1,
            vec![review(
                M,
                t(2, 0),
                "CHANGES_REQUESTED",
                "cargo xtask ci is incomplete or unchecked.\nSaving an empty layer loses every \
                 later layer in the file, which we have reproduced twice here.",
                "head1",
            )],
        );
        assert_eq!(state_of(&mixed).state, "changes_requested_by_maintainer");
        let mut answered = r.clone();
        answered.contribution.commits.push(commit(AUTHOR, t(3, 0)));
        assert_eq!(
            state_of(&answered).state,
            "author_responded_awaiting_maintainer"
        );
    }

    #[test]
    fn a_later_approval_by_the_same_reviewer_resolves_their_request() {
        let community = pr(
            1,
            vec![
                review(PEER, t(2, 0), "CHANGES_REQUESTED", "## [P2] Typo", "head1"),
                review(PEER, t(3, 0), "APPROVED", "", "head1"),
            ],
        );
        let s = state_of(&community);
        assert_ne!(s.state, "community_changes_requested");
        assert!(!s.also.contains(&"community_changes_requested".into()));
        let maintainer = pr(
            1,
            vec![
                review(M, t(2, 0), "CHANGES_REQUESTED", "Please rebase.", "head1"),
                review(M, t(3, 0), "APPROVED", "", "head1"),
            ],
        );
        let s = state_of(&maintainer);
        assert_ne!(s.state, "changes_requested_by_maintainer");
        let tm = timings(&maintainer, &World::new().ctx());
        assert_eq!(tm.requests.len(), 1, "kept for the response history");
        assert!(tm.requests[0].resolved && tm.requests[0].answered.is_none());
        // A push after the approval is not an answer to it.
        let mut later = maintainer.clone();
        later.contribution.commits.push(commit(AUTHOR, t(4, 0)));
        let tm = timings(&later, &World::new().ctx());
        assert_eq!(tm.requests[0].answered, None);
        // An answered request stays answered after the approval.
        let answered = pr(
            1,
            vec![
                comment(M, t(2, 0), "Could you add a test?"),
                comment(AUTHOR, t(3, 0), "Added."),
                review(M, t(4, 0), "APPROVED", "", "head1"),
            ],
        );
        let tm = timings(&answered, &World::new().ctx());
        assert_eq!(tm.requests[0].answered, Some(t(3, 0)));
        // Another reviewer's approval leaves the request open.
        let other = pr(
            1,
            vec![
                review(M, t(2, 0), "CHANGES_REQUESTED", "Please rebase.", "head1"),
                review(M2, t(3, 0), "APPROVED", "", "head1"),
            ],
        );
        assert_eq!(state_of(&other).state, "changes_requested_by_maintainer");
    }

    #[test]
    fn an_earlier_unanswered_request_keeps_it_on_the_author() {
        // The comment answers the evidence request, not the rebase request.
        let r = pr(
            1,
            vec![
                comment(M, t(2, 0), "Please rebase."),
                comment(M, t(3, 0), "Is there a public page for this?"),
                comment(AUTHOR, t(4, 0), "Here it is."),
            ],
        );
        let s = state_of(&r);
        assert_eq!(s.state, "changes_requested_by_maintainer");
        assert_eq!(
            (s.subtype.as_deref(), s.since),
            (Some("rebase"), Some(t(2, 0)))
        );
        assert!(!s
            .also
            .contains(&"author_responded_awaiting_maintainer".into()));
    }

    #[test]
    fn a_draft_waits_on_its_author_from_the_last_conversion() {
        let mut r = pr(1, vec![event("ConvertToDraftEvent", AUTHOR, t(3, 0))]);
        r.is_draft = true;
        let s = state_of(&r);
        assert_eq!(
            (s.state.as_str(), s.since, s.guess),
            ("draft", Some(t(3, 0)), false)
        );
        assert_eq!(s.waits_on, [Role::Author]);
        r.author = Some(M.into());
        assert_eq!(
            state_of(&r).waits_on,
            [Role::Maintainer],
            "a maintainer's own draft"
        );
    }

    #[test]
    fn wip_titles_and_not_ready_sentences_mark_it_not_ready() {
        let mut r = pr(1, vec![]);
        r.title = "[WIP] Pen: curves".into();
        let s = state_of(&r);
        assert_eq!(
            (s.state.as_str(), s.guess),
            ("author_marked_not_ready", false)
        );
        let said = pr(
            1,
            vec![comment(
                AUTHOR,
                t(2, 0),
                "This is not ready yet, still fixing tests.",
            )],
        );
        let s = state_of(&said);
        assert_eq!(
            (s.state.as_str(), s.guess),
            ("author_marked_not_ready", true)
        );
        let mut pushed = said.clone();
        pushed.contribution.commits.push(commit(AUTHOR, t(3, 0)));
        assert_ne!(state_of(&pushed).state, "author_marked_not_ready");
    }

    #[test]
    fn the_author_stepping_back_and_parking_are_low_confidence_guesses() {
        let back = pr(
            1,
            vec![
                comment(M, t(2, 0), "Could you rework the menu?"),
                comment(
                    AUTHOR,
                    t(3, 0),
                    "I won't be doing more on this, it's your show now.",
                ),
            ],
        );
        let s = state_of(&back);
        assert_eq!(
            (s.state.as_str(), s.low, s.guess),
            ("author_stepped_back", true, true)
        );
        assert!(
            s.also
                .contains(&"author_responded_awaiting_maintainer".into()),
            "{:?}",
            s.also
        );
        let parked = pr(
            1,
            vec![
                comment(M, t(2, 0), "Interesting idea."),
                comment(
                    AUTHOR,
                    t(3, 0),
                    "I'll keep it as a draft pending the owners' decision.",
                ),
            ],
        );
        let s = state_of(&parked);
        assert_eq!((s.state.as_str(), s.low), ("author_declared_hold", true));
        assert_eq!(s.waits_on, [Role::Maintainer, Role::Owner]);
    }

    #[test]
    fn maintainer_tasks_declines_supersedes_and_craft_libs_are_read_from_text() {
        let task = pr(
            1,
            vec![comment(
                M2,
                t(2, 0),
                "I'll do a dry run on the release pipeline first.",
            )],
        );
        let s = state_of(&task);
        assert_eq!(
            (s.state.as_str(), s.waits_for.clone()),
            ("held_for_maintainer_task", vec![M2.to_string()])
        );
        let mut done = task.clone();
        done.timeline.push(comment(M2, t(3, 0), "Dry run passed."));
        assert_ne!(state_of(&done).state, "held_for_maintainer_task");
        let declined = pr(
            1,
            vec![comment(M, t(2, 0), "Sorry, we can't take this approach.")],
        );
        assert_eq!(state_of(&declined).state, "declined_in_comment_still_open");
        let sup = pr(
            1,
            vec![comment(M, t(2, 0), "Main already handles this now.")],
        );
        assert_eq!(state_of(&sup).state, "superseded_still_open");
        let libs = pr(1, vec![comment(M, t(2, 0), "This belongs in craft-libs.")]);
        let s = state_of(&libs);
        assert_eq!(
            (s.state.as_str(), s.waits_on.clone()),
            ("redirected_to_shared_library", vec![Role::Author])
        );
    }

    #[test]
    fn a_named_dependency_counts_only_while_it_is_open() {
        let mut r = pr(2, vec![]);
        r.body = "Stacked on #1; merge #1 first.".into();
        let open = World::new().with(pr(1, vec![]));
        let s = open.state(&r);
        assert_eq!(
            (s.state.as_str(), s.blocked_by.clone()),
            ("depends_on_other_pr", vec![1])
        );
        assert_eq!(s.waits_on, [Role::OtherPr]);
        let mut merged = pr(1, vec![]);
        merged.state = PrState::Merged;
        assert_ne!(
            World::new().with(merged).state(&r).state,
            "depends_on_other_pr"
        );
    }

    #[test]
    fn competing_pull_requests_come_from_text_or_a_shared_issue() {
        let r = pr(
            2,
            vec![comment(
                M,
                t(2, 0),
                "#2 vs #3: which one we'll take forward?",
            )],
        );
        let s = World::new().with(pr(3, vec![])).state(&r);
        assert_eq!(
            (s.state.as_str(), s.competing.clone()),
            ("competing_prs_maintainer_choice", vec![3])
        );
        let mut a = pr(2, vec![]);
        a.closing_issues = vec![50];
        let mut b = pr(3, vec![]);
        b.closing_issues = vec![50];
        let s = World::new().with(b).state(&a);
        assert_eq!(s.state, "competing_prs_maintainer_choice");
        assert_eq!(
            (s.subtype.as_deref(), s.low, s.guess),
            (Some("possible_competitor"), true, false)
        );
        assert_eq!(s.competing, [3]);
        assert!(
            s.also
                .contains(&"awaiting_first_maintainer_response".into()),
            "{:?}",
            s.also
        );
        // The same issue number in another repository is another issue.
        let mut x = pr(2, vec![]);
        x.closing_issues = vec![50];
        x.closing_repos = vec!["o/app".into()];
        let mut y = pr(3, vec![]);
        y.closing_issues = vec![50];
        y.closing_repos = vec!["o/elsewhere".into()];
        assert_ne!(
            World::new().with(y).state(&x).state,
            "competing_prs_maintainer_choice"
        );
        // A named competitor that merged leaves no choice.
        let mut gone = pr(3, vec![]);
        gone.state = PrState::Merged;
        let s = World::new().with(gone).state(&r);
        assert_ne!(s.state, "competing_prs_maintainer_choice");
        assert!(!s.also.contains(&"competing_prs_maintainer_choice".into()));
    }

    #[test]
    fn conflicts_wait_on_the_author_except_in_rebase_repos() {
        let mut r = pr(1, vec![comment(M, t(2, 0), "Looks good.")]);
        r.checks.mergeable = CONFLICTING.into();
        r.checks.conflict_since = Some(t(3, 0));
        r.checks.rollup = Some("FAILURE".into());
        r.checks.failing = vec!["test".into()];
        r.maintainer_can_modify = true;
        let s = state_of(&r);
        assert_eq!(
            (s.state.as_str(), s.since),
            ("merge_conflict", Some(t(3, 0)))
        );
        assert!(
            !s.also.iter().any(|a| a.starts_with("ci_")),
            "checks are skipped while it conflicts"
        );
        let mut w = World::new();
        w.rebase = true;
        let s = w.state(&r);
        assert_ne!(s.state, "merge_conflict");
        assert!(s.flags.contains(&"conflict".into()));
        r.maintainer_can_modify = false;
        assert_eq!(
            w.state(&r).state,
            "merge_conflict",
            "maintainers cannot push to the branch"
        );
    }

    #[test]
    fn checks_decide_approval_failures_and_running_states() {
        let suite = |status: &str, conclusion: Option<&str>| Suite {
            status: status.into(),
            conclusion: conclusion.map(str::to_string),
            created_at: Some(t(2, 0)),
            app: Some("github-actions".into()),
        };
        let mut r = pr(1, vec![comment(M, t(2, 0), "Looks good.")]);
        r.checks.suites = vec![suite("COMPLETED", Some("ACTION_REQUIRED"))];
        assert_eq!(state_of(&r).state, "ci_awaiting_workflow_approval");
        r.checks.suites = vec![suite("COMPLETED", Some("FAILURE"))];
        r.checks.rollup = Some("FAILURE".into());
        r.checks.failing = vec!["clippy".into()];
        let mut w = World::new();
        w.main = Some(MainChecks {
            failing: vec!["clippy".into()],
            ..Default::default()
        });
        assert_eq!(w.state(&r).state, "ci_failing_inherited_from_main");
        // A check list not read in full may hide a failure of its own.
        let mut cut = r.clone();
        cut.checks.contexts_cursor = Some(ContextsCursor::default());
        let s = w.state(&cut);
        assert_eq!((s.state.as_str(), s.low), ("ci_failing_from_pr", true));
        w.main = Some(MainChecks::default());
        let s = w.state(&r);
        assert_eq!(s.state, "ci_failing_from_pr");
        assert_eq!(s.since, Some(t(2, 0)), "from the head's newest suite");
        r.checks.rollup = Some("PENDING".into());
        r.checks.suites = vec![suite("IN_PROGRESS", None)];
        let s = state_of(&r);
        assert_eq!(
            (s.state.as_str(), s.waits_on.clone()),
            ("ci_running", vec![Role::Ci])
        );
        // A suite from another app does not count as running.
        r.checks.suites[0].app = Some("claude".into());
        assert_ne!(state_of(&r).state, "ci_running");
    }

    #[test]
    fn community_approval_and_clean_checks_wait_on_a_maintainer() {
        let mut r = pr(
            1,
            vec![
                comment(M, t(2, 0), "Interesting."),
                review(PEER, t(4, 0), "APPROVED", "", "head1"),
            ],
        );
        r.contribution.commits.push(commit(AUTHOR, t(3, 0)));
        let s = state_of(&r);
        assert_eq!(
            (s.state.as_str(), s.since),
            ("community_approved_awaiting_maintainer", Some(t(4, 0)))
        );
        // A maintainer comment after the last push takes it out of the queue.
        let mut seen = r.clone();
        seen.timeline.push(comment(M, t(5, 0), "Looking."));
        assert_ne!(
            state_of(&seen).state,
            "community_approved_awaiting_maintainer"
        );
        // An approval of an older head is stale.
        r.head_ref_oid = "head2".into();
        let s = state_of(&r);
        assert_ne!(s.state, "community_approved_awaiting_maintainer");
        assert!(s.flags.contains(&"stale_approval".into()));
        let mut clean = pr(1, vec![comment(M, t(2, 0), "Nice.")]);
        clean.checks.merge_state = "CLEAN".into();
        let s = state_of(&clean);
        assert_eq!(s.state, "maintainer_engaged_ready_to_merge");
        assert!(s.flags.contains(&"no_ci_signal".into()));
        clean.checks.rollup = Some("SUCCESS".into());
        clean.checks.suites = vec![Suite {
            status: "COMPLETED".into(),
            conclusion: Some("SUCCESS".into()),
            app: Some("github-actions".into()),
            ..Default::default()
        }];
        let s = state_of(&clean);
        assert_eq!(s.state, "maintainer_engaged_ready_to_merge");
        assert!(!s.flags.contains(&"no_ci_signal".into()));
    }

    #[test]
    fn open_flags_mark_staleness_unknown_mergeability_and_maintainer_authors() {
        let mut r = pr(1, vec![comment(AUTHOR, t(15, 0), "ping")]);
        r.checks.mergeable = UNKNOWN.into();
        r.author = Some(M.into());
        let s = state_of(&r);
        assert!(
            s.flags.contains(&"aging".into()) && !s.flags.contains(&"stale".into()),
            "{:?}",
            s.flags
        );
        assert!(s.flags.contains(&"mergeability_unknown".into()));
        assert!(s.flags.contains(&"maintainer_authored".into()));
        assert_eq!(
            s.waits_on,
            [Role::Author],
            "a maintainer's own pull request with no other maintainer"
        );
        let old = pr(1, vec![comment(AUTHOR, t(2, 0), "ping")]);
        assert!(state_of(&old).flags.contains(&"stale".into()));
        // A pull request opened today with older commits is not stale.
        let mut fresh = pr(1, vec![]);
        fresh.created_at = Some(t(19, 0));
        let s = state_of(&fresh);
        assert!(
            !s.flags.iter().any(|f| f == "stale" || f == "aging"),
            "{:?}",
            s.flags
        );
        // One with no commits or comments ages from its opening.
        let mut bare = pr(1, vec![]);
        bare.contribution.commits.clear();
        assert!(state_of(&bare).flags.contains(&"stale".into()));
        // A bot's commit is not human activity.
        bare.contribution
            .commits
            .push(commit("github-actions", t(19, 0)));
        assert!(state_of(&bare).flags.contains(&"stale".into()));
    }

    fn merged_pr(number: u64, at: Time, by: &str, parents: u64) -> PrRecord {
        let mut r = pr(number, vec![]);
        r.state = PrState::Merged;
        r.merged_at = Some(at);
        r.closed_at = Some(at);
        r.merged_by = Some(by.into());
        r.contribution.merge_commit = Some(MergeCommit {
            parents,
            ..Default::default()
        });
        r
    }

    #[test]
    fn merged_subtypes_are_batch_maintainer_followups_or_direct() {
        let at = t(5, 12);
        let r = merged_pr(1, at, M, 2);
        assert_eq!(state_of(&r).subtype.as_deref(), Some("direct"));
        let w = World::new()
            .with(merged_pr(2, at + chrono::Duration::seconds(20), M, 2))
            .with(merged_pr(3, at + chrono::Duration::seconds(40), M, 2));
        assert_eq!(w.state(&r).subtype.as_deref(), Some("batch"));
        let squash = World::new()
            .with(merged_pr(2, at + chrono::Duration::seconds(20), M, 1))
            .with(merged_pr(3, at + chrono::Duration::seconds(40), M, 1));
        assert_eq!(
            squash.state(&merged_pr(1, at, M, 1)).subtype.as_deref(),
            Some("direct"),
            "squash merges are not a batch"
        );
        let mut follow = merged_pr(1, at, M, 2);
        follow.contribution.commits.push(commit(M2, t(5, 11)));
        assert_eq!(
            state_of(&follow).subtype.as_deref(),
            Some("maintainer_followups")
        );
        let mut via = merged_pr(1, at, M, 2);
        via.timeline.push(TimelineItem {
            kind: "CrossReferencedEvent".into(),
            at: Some(t(5, 0)),
            source: Some(CrossSource {
                kind: "PullRequest".into(),
                number: 9,
                title: "Integration batch 12".into(),
                merged_at: Some(at + chrono::Duration::minutes(5)),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(state_of(&via).subtype.as_deref(), Some("batch"));
    }

    fn closed_pr(number: u64, mut timeline: Vec<TimelineItem>, closer: &str, at: Time) -> PrRecord {
        timeline.push(event("ClosedEvent", closer, at));
        let mut r = pr(number, timeline);
        r.state = PrState::Closed;
        r.closed_at = Some(at);
        r.body = "Adds smoother curves.".into();
        r.comments_total = 1;
        r
    }

    #[test]
    fn closes_by_a_pull_request_or_a_landed_sentence_landed_elsewhere() {
        let mut r = closed_pr(1, vec![], M, t(5, 0));
        r.timeline.last_mut().unwrap().closer = Some(Closer {
            kind: "PullRequest".into(),
            number: Some(9),
            oid: None,
        });
        let s = state_of(&r);
        assert_eq!(
            (s.state.as_str(), s.guess),
            ("closed_landed_elsewhere", false)
        );
        assert!(s.waits_on.is_empty());
        let said = closed_pr(
            1,
            vec![comment(M, t(4, 0), "This landed in #9, thanks.")],
            M,
            t(5, 0),
        );
        let w = World::new().with(merged_pr(9, t(4, 0), M, 2));
        let s = w.state(&said);
        assert_eq!(
            (s.state.as_str(), s.guess),
            ("closed_landed_elsewhere", true)
        );
        assert_ne!(
            World::new().with(pr(9, vec![])).state(&said).state,
            "closed_landed_elsewhere",
            "#9 is still open"
        );
    }

    #[test]
    fn closing_comments_decide_duplicate_yielded_folded_and_superseded() {
        let dup = closed_pr(
            1,
            vec![comment(M, t(4, 0), "Closing this as a duplicate of #7.")],
            M,
            t(5, 0),
        );
        assert_eq!(state_of(&dup).state, "closed_duplicate");
        let yielded = closed_pr(
            1,
            vec![comment(AUTHOR, t(4, 0), "Closing in favor of #7.")],
            AUTHOR,
            t(5, 0),
        );
        let s = World::new().with(pr(7, vec![])).state(&yielded);
        assert_eq!(
            (s.state.as_str(), s.subtype.as_deref()),
            ("closed_duplicate", Some("author_yielded"))
        );
        let s = World::new()
            .with(merged_pr(7, t(3, 0), M, 2))
            .state(&yielded);
        assert_eq!(
            (s.state.as_str(), s.subtype.as_deref()),
            ("closed_superseded", Some("by_author"))
        );
        let folded = closed_pr(
            1,
            vec![comment(
                AUTHOR,
                t(4, 0),
                "Split into smaller pull requests.",
            )],
            AUTHOR,
            t(5, 0),
        );
        assert_eq!(state_of(&folded).state, "closed_folded_split_or_moved");
        // A comment after the close does not change the outcome.
        let mut thanked = folded.clone();
        thanked.timeline.push(comment(AUTHOR, t(6, 0), "Thanks!"));
        assert_eq!(state_of(&thanked).state, "closed_folded_split_or_moved");
        // Two pull requests by the author in other repos reference it, one
        // with this pull request's number and both with the same number.
        let child = |repo: &str, number: u64| TimelineItem {
            kind: "CrossReferencedEvent".into(),
            at: Some(t(3, 0)),
            source: Some(CrossSource {
                kind: "PullRequest".into(),
                repo: repo.into(),
                number,
                author: Some(AUTHOR.into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let moved = closed_pr(
            1,
            vec![child("o/other", 1), child("o/third", 1)],
            AUTHOR,
            t(5, 0),
        );
        let s = World::new().with(pr(5, vec![])).state(&moved);
        assert_eq!(s.state, "closed_folded_split_or_moved", "{}", s.rule);
        let sup = closed_pr(
            1,
            vec![comment(
                M,
                t(4, 0),
                "This has been implemented on main already.",
            )],
            M,
            t(5, 0),
        );
        let s = state_of(&sup);
        assert_eq!(
            (s.state.as_str(), s.subtype.as_deref()),
            ("closed_superseded", Some("by_maintainer"))
        );
    }

    #[test]
    fn declines_and_leftover_closes_get_their_subtypes() {
        let declined = closed_pr(
            1,
            vec![comment(M, t(4, 0), "This is not something we can accept.")],
            M,
            t(5, 0),
        );
        assert_eq!(state_of(&declined).state, "closed_declined");
        let mut accidental = closed_pr(1, vec![], AUTHOR, t(1, 0) + chrono::Duration::minutes(5));
        accidental.body.clear();
        accidental.comments_total = 0;
        assert_eq!(
            state_of(&accidental).subtype.as_deref(),
            Some("accidental_quick_close")
        );
        let pushback = closed_pr(
            1,
            vec![review(PEER, t(3, 0), "CHANGES_REQUESTED", "", "head1")],
            AUTHOR,
            t(5, 0),
        );
        assert_eq!(
            state_of(&pushback).subtype.as_deref(),
            Some("after_community_pushback")
        );
        let after = closed_pr(
            1,
            vec![comment(M, t(3, 0), "Hmm, interesting.")],
            AUTHOR,
            t(5, 0),
        );
        assert_eq!(state_of(&after).subtype.as_deref(), Some("after_pushback"));
        let silent = closed_pr(1, vec![], AUTHOR, t(5, 0));
        let s = state_of(&silent);
        assert_eq!(
            (s.subtype.as_deref(), s.low),
            (Some("no_maintainer_response"), true)
        );
        assert!(s.flags.contains(&"no_m_response_over_life".into()));
        // A maintainer comment after the close is not a response over its life.
        let mut late = silent.clone();
        late.timeline
            .push(comment(M, t(6, 0), "Sorry we missed this."));
        let s = state_of(&late);
        assert_eq!(s.subtype.as_deref(), Some("no_maintainer_response"));
        assert!(s.flags.contains(&"no_m_response_over_life".into()));
        assert_eq!(timings(&late, &World::new().ctx()).first_response, None);
        let mut disputed = closed_pr(1, vec![comment(M, t(4, 0), "Duplicate of #7.")], M, t(5, 0));
        disputed.timeline.push(comment(
            AUTHOR,
            t(6, 0),
            "This is not a duplicate, please reopen.",
        ));
        assert!(state_of(&disputed)
            .flags
            .contains(&"disputed_after_close".into()));
    }

    #[test]
    fn comments_after_a_merge_are_left_out() {
        let mut r = merged_pr(1, t(5, 0), M, 2);
        r.timeline.push(comment(M, t(6, 0), "Merged after review."));
        assert!(state_of(&r).evidence.is_none());
        r.timeline.push(comment(M, t(4, 0), "Merged after review."));
        assert!(state_of(&r).evidence.is_some());
        // The author's thanks after the merge does not answer a request.
        let mut asked = merged_pr(1, t(5, 0), M, 2);
        asked.timeline = vec![
            comment(M, t(3, 0), "Could you add a test?"),
            comment(AUTHOR, t(6, 0), "Thanks!"),
        ];
        let tm = timings(&asked, &World::new().ctx());
        assert_eq!(tm.requests[0].answered, None);
        // Nor does one after a close without a merge.
        let mut closed = closed_pr(
            1,
            vec![comment(M, t(3, 0), "Could you add a test?")],
            AUTHOR,
            t(5, 0),
        );
        closed
            .timeline
            .push(comment(AUTHOR, t(6, 0), "Thanks anyway!"));
        let tm = timings(&closed, &World::new().ctx());
        assert_eq!(tm.requests[0].answered, None);
    }

    #[test]
    fn timings_give_request_answers_and_the_first_maintainer_response() {
        let mut r = pr(
            1,
            vec![
                comment(PEER, t(2, 0), "Nice."),
                comment(M, t(3, 0), "Could you split this?"),
                comment(AUTHOR, t(4, 0), "Sure."),
            ],
        );
        r.contribution.commits.push(commit(AUTHOR, t(5, 0)));
        let w = World::new();
        let tm = timings(&r, &w.ctx());
        assert_eq!(tm.first_response, Some(t(3, 0)));
        assert_eq!(tm.requests.len(), 1);
        let q = &tm.requests[0];
        assert_eq!(
            (q.subtype.as_str(), q.by.as_str(), q.answered),
            ("split", M, Some(t(5, 0))),
            "only a push answers a split"
        );
        // A merge answers when another maintainer made it, not the author.
        let mut merged = merged_pr(1, t(6, 0), M, 2);
        merged.created_at = Some(t(1, 0));
        assert_eq!(timings(&merged, &w.ctx()).first_response, Some(t(6, 0)));
        merged.author = Some(M.into());
        assert_eq!(
            timings(&merged, &w.ctx()).first_response,
            None,
            "a self-merge"
        );
    }
}
