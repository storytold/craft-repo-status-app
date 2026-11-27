//! The rows of the People tab's Work view and of the My PRs tab, worked out
//! from the People view ([`PeopleView`]) as `ui/src/model.ts` works them out
//! (`personFigures`, `peopleRows`, `blockerOf` and `mineRows`), for readers
//! outside the window such as `craft-mcp`. Change both together.

use std::collections::BTreeMap;

use serde::Serialize;

use super::states::{Role, NOT_READ_YET};
use super::view::{Flow, Given, Item, PeopleView, PersonRow, Reply, Response, ALL_REPOS};

/// The work states in rule order, with their labels (`STATE_LABELS`).
pub const STATE_LABELS: &[(&str, &str)] = &[
    ("author_stepped_back", "Author stepped back"),
    ("held_for_owner", "Held for owner"),
    ("held_for_maintainer_task", "Maintainer task"),
    ("declined_in_comment_still_open", "Declined, still open"),
    ("superseded_still_open", "Superseded, still open"),
    ("author_declared_hold", "Author parked it"),
    ("depends_on_other_pr", "Depends on another PR"),
    ("draft", "Draft"),
    ("author_marked_not_ready", "Not ready"),
    ("competing_prs_maintainer_choice", "Competing PRs"),
    ("redirected_to_shared_library", "Move to craft-libs"),
    ("changes_requested_by_maintainer", "Changes requested"),
    ("community_changes_requested", "Community asked for changes"),
    ("merge_conflict", "Merge conflict"),
    ("ci_awaiting_workflow_approval", "Workflow approval"),
    ("ci_failing_from_pr", "CI failing"),
    ("ci_failing_inherited_from_main", "CI failing on main"),
    ("ci_running", "CI running"),
    ("author_responded_awaiting_maintainer", "Author answered"),
    (
        "community_approved_awaiting_maintainer",
        "Community approved",
    ),
    ("maintainer_engaged_ready_to_merge", "Ready to merge"),
    (
        "awaiting_first_maintainer_response",
        "Awaiting first response",
    ),
    ("awaiting_maintainer", "Awaiting maintainer"),
    (NOT_READ_YET, "Not read yet"),
    ("merged", "Merged"),
    ("closed_landed_elsewhere", "Landed elsewhere"),
    ("closed_duplicate", "Duplicate"),
    ("closed_folded_split_or_moved", "Folded or split"),
    ("closed_superseded", "Superseded"),
    ("closed_declined", "Declined"),
    ("closed_withdrawn_or_abandoned", "Withdrawn"),
];

/// Short blocker names for the work states (`BLOCKER_LABELS`); the other
/// states keep their [`STATE_LABELS`] label.
pub const BLOCKER_LABELS: &[(&str, &str)] = &[
    ("merge_conflict", "Conflicting"),
    ("ci_failing_from_pr", "Failing checks"),
    ("ci_failing_inherited_from_main", "Failing on main"),
    ("ci_awaiting_workflow_approval", "Waiting for approval"),
    ("ci_running", "Checks running"),
    ("held_for_owner", "Held"),
    ("maintainer_engaged_ready_to_merge", "Ready"),
    ("community_approved_awaiting_maintainer", "Approved"),
    ("changes_requested_by_maintainer", "Changes requested"),
    ("community_changes_requested", "Review findings"),
    ("author_marked_not_ready", "Not ready"),
    ("awaiting_first_maintainer_response", "Awaiting review"),
    ("author_responded_awaiting_maintainer", "Answered"),
    ("author_declared_hold", "Parked"),
    ("competing_prs_maintainer_choice", "Competing"),
];

const READY_STATES: &[&str] = &[
    "maintainer_engaged_ready_to_merge",
    "community_approved_awaiting_maintainer",
];

/// The label of work state `id`, or the id with spaces for underscores.
pub fn state_label(id: &str) -> String {
    STATE_LABELS
        .iter()
        .find(|(s, _)| *s == id)
        .map_or_else(|| id.replace('_', " "), |(_, l)| l.to_string())
}

/// Where work state `id` comes in rule order; 99 for a state not listed.
fn state_order(id: &str) -> usize {
    STATE_LABELS
        .iter()
        .position(|(s, _)| *s == id)
        .unwrap_or(99)
}

/// The words for who an item waits on (`ROLE_LABELS`).
pub fn role_label(role: Role) -> &'static str {
    match role {
        Role::Author => "the author",
        Role::Maintainer => "a maintainer",
        Role::Owner => "the owner",
        Role::Ci => "CI",
        Role::OtherPr => "another PR",
    }
}

/// Who a blocker waits on: you, CI, someone else, nobody because it is
/// approved or ready to merge, or not known because the timeline has not
/// been read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Me,
    Ci,
    Them,
    Ready,
    Unread,
}

/// A pull request's blocker for one login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Blocker {
    pub state: String,
    pub label: String,
    pub tone: Tone,
}

/// Whether `item`'s work state waits on `login`.
fn waits_on_login(item: &Item, login: &str) -> bool {
    item.work.waits_for.contains(&login.to_ascii_lowercase())
}

/// `item`'s blocker for `login`: the label of its work state, and whether
/// that state is approved or ready to merge, waits on CI, on them, on
/// someone else, or has not been read yet.
pub fn blocker_of(item: &Item, login: &str) -> Blocker {
    let w = &item.work;
    let tone = if w.state == NOT_READ_YET {
        Tone::Unread
    } else if READY_STATES.contains(&w.state.as_str()) {
        Tone::Ready
    } else if w.state == "ci_running" {
        Tone::Ci
    } else if waits_on_login(item, login) {
        Tone::Me
    } else {
        Tone::Them
    };
    let label = if w.state == "depends_on_other_pr" && !w.blocked_by.is_empty() {
        let numbers: Vec<String> = w.blocked_by.iter().map(|n| format!("#{n}")).collect();
        format!("Depends on {}", numbers.join(", "))
    } else {
        BLOCKER_LABELS
            .iter()
            .find(|(s, _)| *s == w.state)
            .map_or_else(|| state_label(&w.state), |(_, l)| l.to_string())
    };
    Blocker {
        state: w.state.clone(),
        label,
        tone,
    }
}

/// Whether `item` is held for the owner: its state, a request that also
/// carries a hold, or a hold that an earlier rule outranked.
pub fn is_held(item: &Item) -> bool {
    let w = &item.work;
    w.state == "held_for_owner"
        || w.flags.iter().any(|f| f == "also_held_for_owner")
        || w.also.iter().any(|s| s == "held_for_owner")
}

/// Who `item` waits on, with "you" for `login`.
pub fn waits_text(item: &Item, login: &str) -> String {
    let mine = waits_on_login(item, login);
    let mut parts: Vec<&str> = Vec::new();
    if mine {
        parts.push("you");
    }
    parts.extend(
        item.work
            .waits_on
            .iter()
            .filter(|r| !(mine && **r == Role::Author))
            .map(|r| role_label(*r)),
    );
    if !parts.is_empty() {
        return parts.join(", ");
    }
    if item.work.state == NOT_READ_YET {
        "nobody yet".into()
    } else {
        "nobody".into()
    }
}

/// One row of My PRs. `needs_me`: its blocker waits on you, or a
/// maintainer request waits for your reply.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MineRow {
    pub item: Item,
    pub blocker: Blocker,
    pub reply: Option<Reply>,
    pub held: bool,
    pub needs_me: bool,
}

/// `login`'s open pull requests in `repos` (any letter case), in the My PRs
/// tab's default order: by blocker (on you, CI, someone else, approved or
/// ready, not read yet, each in rule order), then by repo and number.
pub fn mine_rows(view: &PeopleView, repos: &[String], login: &str) -> Vec<MineRow> {
    let me = login.to_ascii_lowercase();
    let mut rows: Vec<MineRow> = view
        .items
        .iter()
        .filter(|i| i.author.as_deref() == Some(me.as_str()))
        .filter(|i| repos.iter().any(|r| r.eq_ignore_ascii_case(&i.repo)))
        .map(|item| {
            let blocker = blocker_of(item, login);
            let reply = item.reply.clone();
            MineRow {
                held: is_held(item),
                needs_me: blocker.tone == Tone::Me || reply.is_some(),
                item: item.clone(),
                blocker,
                reply,
            }
        })
        .collect();
    let key = |r: &MineRow| {
        (
            r.blocker.tone,
            state_order(&r.item.work.state),
            r.item.repo.to_ascii_lowercase(),
            r.item.number,
        )
    };
    rows.sort_by_key(key);
    rows
}

/// One person's figures in the People tab's Work view, for one window and
/// one repo or every repo (`PersonFigures`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PersonFigures {
    pub key: String,
    pub login: String,
    pub name: Option<String>,
    pub open: u64,
    pub open_guessed: u64,
    pub on_them: u64,
    pub on_them_guessed: u64,
    /// Their own open items that wait on someone else, by who.
    pub on_others: BTreeMap<Role, u64>,
    pub on_others_total: u64,
    pub held: u64,
    pub held_guessed: u64,
    pub merged: u64,
    pub landed: u64,
    pub closed: u64,
    pub merge_median_secs: Option<i64>,
    pub landed_rate: Option<f64>,
    pub their_response_secs: Option<i64>,
    pub unanswered: u64,
    pub response_to_them_secs: Option<i64>,
    pub awaiting_first: u64,
    pub reviews: u64,
    #[serde(skip)]
    given: Option<Given>,
}

/// The entry of `rec` for `repo` in any letter case, or for every repo when
/// `repo` is empty.
fn by_repo<'a, T>(rec: Option<&'a BTreeMap<String, T>>, repo: &str) -> Option<&'a T> {
    let rec = rec?;
    if repo.is_empty() {
        return rec.get(ALL_REPOS);
    }
    rec.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(repo))
        .map(|(_, v)| v)
}

/// Who else the author `key`'s own item waits on: every role but the
/// author's, unless the item waits only on them.
fn other_roles(item: &Item, key: &str) -> Vec<Role> {
    let w = &item.work;
    if !w.waits_for.is_empty() && w.waits_for.iter().all(|l| l == key) {
        return Vec::new();
    }
    w.waits_on
        .iter()
        .copied()
        .filter(|r| *r != Role::Author)
        .collect()
}

/// `p`'s figures in `window` for `repo` (empty for every repo).
pub fn person_figures(view: &PeopleView, p: &PersonRow, window: &str, repo: &str) -> PersonFigures {
    let items: Vec<&Item> = view
        .items
        .iter()
        .filter(|i| repo.is_empty() || i.repo.eq_ignore_ascii_case(repo))
        .collect();
    let own: Vec<&Item> = items
        .iter()
        .copied()
        .filter(|i| i.author.as_deref() == Some(p.key.as_str()))
        .collect();
    let on_them: Vec<&Item> = items
        .iter()
        .copied()
        .filter(|i| i.work.waits_for.contains(&p.key))
        .collect();
    let mut on_others: BTreeMap<Role, u64> = BTreeMap::new();
    let mut on_others_total = 0;
    for i in &own {
        let roles = other_roles(i, &p.key);
        if !roles.is_empty() {
            on_others_total += 1;
        }
        for r in roles {
            *on_others.entry(r).or_default() += 1;
        }
    }
    let held = |i: &&Item| {
        i.work.state == "held_for_owner" || i.work.flags.iter().any(|f| f == "also_held_for_owner")
    };
    let held_items: Vec<&Item> = own.iter().copied().filter(held).collect();
    let flow: Option<&Flow> = by_repo(p.flow.get(window), repo);
    let given: Option<&Given> = by_repo(p.given.get(window), repo);
    let response: Option<&Response> = by_repo(Some(&p.response), repo);
    let merged = flow.map_or(0, |f| f.merged);
    let landed = flow.map_or(0, |f| f.landed_elsewhere);
    let guessed = |list: &[&Item]| list.iter().filter(|i| i.work.guess).count() as u64;
    PersonFigures {
        key: p.key.clone(),
        login: p.login.clone(),
        name: p.name.clone(),
        open: own.len() as u64,
        open_guessed: guessed(&own),
        on_them: on_them.len() as u64,
        on_them_guessed: guessed(&on_them),
        on_others,
        on_others_total,
        held: held_items.len() as u64,
        held_guessed: held_items
            .iter()
            .filter(|i| i.work.guess || i.work.hold.is_some())
            .count() as u64,
        merged,
        landed,
        closed: flow.map_or(0, |f| f.closed.values().sum()),
        merge_median_secs: flow.and_then(|f| f.median_merge_secs),
        landed_rate: flow
            .filter(|f| f.closed_total > 0)
            .map(|f| (merged + landed) as f64 / f.closed_total as f64),
        their_response_secs: response.and_then(|r| r.their_median_secs),
        unanswered: response.map_or(0, |r| r.unanswered),
        response_to_them_secs: response.and_then(|r| r.m_median_secs),
        awaiting_first: response.map_or(0, |r| r.awaiting_first),
        reviews: given.map_or(0, |g| g.reviews + g.comments),
        given: given.cloned(),
    }
}

/// Whether a person has anything to show for the window and repo.
fn has_figures(f: &PersonFigures) -> bool {
    let (merges, closes) = f.given.as_ref().map_or((0, 0), |g| (g.merges, g.closes));
    f.open
        + f.on_them
        + f.merged
        + f.landed
        + f.closed
        + f.reviews
        + merges
        + closes
        + f.unanswered
        + f.awaiting_first
        > 0
}

/// The people with anything in `window` for `repo` (empty for every repo),
/// in the Work view's default order: most open pull requests waiting on
/// them first, then by login.
pub fn people_rows(view: &PeopleView, window: &str, repo: &str) -> Vec<PersonFigures> {
    let mut rows: Vec<PersonFigures> = view
        .people
        .iter()
        .map(|p| person_figures(view, p, window, repo))
        .filter(has_figures)
        .collect();
    rows.sort_by(|a, b| {
        b.on_them.cmp(&a.on_them).then_with(|| {
            a.login
                .to_ascii_lowercase()
                .cmp(&b.login.to_ascii_lowercase())
        })
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::super::states::{Hold, WorkState};
    use super::*;

    fn item(repo: &str, number: u64, author: &str, state: &str, waits_for: &[&str]) -> Item {
        Item {
            repo: repo.into(),
            number,
            title: format!("Change {number}"),
            url: format!("https://github.com/{repo}/pull/{number}"),
            author: Some(author.into()),
            work: WorkState {
                state: state.into(),
                waits_for: waits_for.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The `key: 'label'` entries of the object `name` in model.ts, in their
    /// order, with `[NOT_READ_YET]` read as its value.
    fn ts_labels(ts: &str, name: &str) -> Vec<(String, String)> {
        let head = format!("export const {name}: Readonly<Record<string, string>> = {{\n");
        let start = ts.find(&head).map(|i| i + head.len());
        let block = start.and_then(|s| ts[s..].split("\n};").next());
        let Some(block) = block else {
            panic!("model.ts has no {name}");
        };
        block
            .lines()
            .map(|line| {
                let (key, label) = line
                    .trim()
                    .split_once(": '")
                    .unwrap_or_else(|| panic!("{name}: {line}"));
                let label = label
                    .strip_suffix("',")
                    .unwrap_or_else(|| panic!("{name}: {line}"));
                let key = if key == "[NOT_READ_YET]" {
                    NOT_READ_YET
                } else {
                    key
                };
                (key.to_string(), label.to_string())
            })
            .collect()
    }

    #[test]
    fn the_labels_match_the_ui() {
        let ts = include_str!("../../../../ui/src/model.ts");
        for (name, rust) in [
            ("STATE_LABELS", STATE_LABELS),
            ("BLOCKER_LABELS", BLOCKER_LABELS),
        ] {
            let rust: Vec<(String, String)> = rust
                .iter()
                .map(|(k, l)| (k.to_string(), l.to_string()))
                .collect();
            assert_eq!(
                ts_labels(ts, name),
                rust,
                "{name} in model.ts and rows.rs, in order"
            );
        }
    }

    #[test]
    fn blockers_take_their_tone_from_the_state_and_the_login() {
        let mut failing = item(
            "o/app",
            1,
            "ann-example",
            "ci_failing_from_pr",
            &["ann-example"],
        );
        failing.work.waits_on = vec![Role::Author];
        let b = blocker_of(&failing, "Ann-Example");
        assert_eq!((b.label.as_str(), b.tone), ("Failing checks", Tone::Me));
        assert_eq!(blocker_of(&failing, "bo-example").tone, Tone::Them);
        let running = item("o/app", 2, "ann-example", "ci_running", &[]);
        assert_eq!(blocker_of(&running, "ann-example").tone, Tone::Ci);
        let ready = item(
            "o/app",
            3,
            "ann-example",
            "maintainer_engaged_ready_to_merge",
            &[],
        );
        assert_eq!(
            (
                blocker_of(&ready, "ann-example").label.as_str(),
                blocker_of(&ready, "ann-example").tone
            ),
            ("Ready", Tone::Ready)
        );
        let unread = item("o/app", 4, "ann-example", NOT_READ_YET, &[]);
        let b = blocker_of(&unread, "ann-example");
        assert_eq!((b.label.as_str(), b.tone), ("Not read yet", Tone::Unread));
        let mut depends = item("o/app", 5, "ann-example", "depends_on_other_pr", &[]);
        depends.work.blocked_by = vec![7, 9];
        assert_eq!(
            blocker_of(&depends, "ann-example").label,
            "Depends on #7, #9"
        );
        let draft = item("o/app", 6, "ann-example", "draft", &["ann-example"]);
        assert_eq!(blocker_of(&draft, "ann-example").label, "Draft");
        assert_eq!(waits_text(&unread, "ann-example"), "nobody yet");
        assert_eq!(waits_text(&running, "ann-example"), "nobody");
    }

    #[test]
    fn my_rows_list_one_logins_items_in_the_configured_repos_by_blocker() {
        let mut held = item("o/app", 4, "ann-example", "held_for_owner", &["boss-one"]);
        held.work.waits_on = vec![Role::Owner];
        held.work.hold = Some(Hold::default());
        let mut failing = item(
            "o/App",
            9,
            "ann-example",
            "ci_failing_from_pr",
            &["ann-example"],
        );
        failing.work.waits_on = vec![Role::Author];
        let mut asked = item(
            "o/lib",
            2,
            "ann-example",
            "awaiting_maintainer",
            &["maint-one"],
        );
        asked.work.waits_on = vec![Role::Maintainer];
        asked.reply = Some(Reply {
            by: "maint-one".into(),
            subtype: "rebase".into(),
            ..Default::default()
        });
        let view = PeopleView {
            items: vec![
                held,
                failing,
                asked,
                item("o/app", 5, "bo-example", "draft", &["bo-example"]),
                item("o/other", 6, "ann-example", "draft", &["ann-example"]),
                item("o/app", 1, "ann-example", NOT_READ_YET, &[]),
            ],
            ..Default::default()
        };
        let repos = ["o/app".to_string(), "O/Lib".to_string()];
        let rows = mine_rows(&view, &repos, "Ann-Example");
        let got: Vec<(u64, Tone, bool, bool)> = rows
            .iter()
            .map(|r| (r.item.number, r.blocker.tone, r.held, r.needs_me))
            .collect();
        assert_eq!(
            got,
            [
                (9, Tone::Me, false, true),
                (4, Tone::Them, true, false),
                (2, Tone::Them, false, true),
                (1, Tone::Unread, false, false),
            ]
        );
        assert_eq!(waits_text(&rows[1].item, "ann-example"), "the owner");
        assert_eq!(waits_text(&rows[0].item, "ann-example"), "you");
    }

    #[test]
    fn person_figures_count_open_work_and_read_the_window_and_repo() {
        let mut own = item(
            "o/app",
            1,
            "ann-example",
            "changes_requested_by_maintainer",
            &["ann-example", "boss-one"],
        );
        own.work.waits_on = vec![Role::Author, Role::Owner];
        own.work.guess = true;
        let mut theirs = item(
            "o/lib",
            2,
            "bo-example",
            "awaiting_maintainer",
            &["ann-example"],
        );
        theirs.work.waits_on = vec![Role::Maintainer];
        let flow = Flow {
            merged: 3,
            landed_elsewhere: 1,
            closed: [("closed_declined".to_string(), 2)].into(),
            closed_total: 6,
            median_merge_secs: Some(3600),
            ..Default::default()
        };
        let ann = PersonRow {
            key: "ann-example".into(),
            login: "Ann-Example".into(),
            flow: [("7d".to_string(), [(ALL_REPOS.to_string(), flow)].into())].into(),
            given: [(
                "7d".to_string(),
                [(
                    "o/lib".to_string(),
                    Given {
                        reviews: 2,
                        comments: 1,
                        ..Default::default()
                    },
                )]
                .into(),
            )]
            .into(),
            ..Default::default()
        };
        let idle = PersonRow {
            key: "cy-example".into(),
            login: "cy-example".into(),
            ..Default::default()
        };
        let view = PeopleView {
            items: vec![own, theirs],
            people: vec![idle, ann.clone()],
            ..Default::default()
        };
        let f = person_figures(&view, &ann, "7d", "");
        assert_eq!(
            (f.open, f.open_guessed, f.on_them, f.on_others_total),
            (1, 1, 2, 1)
        );
        assert_eq!(f.on_others.get(&Role::Owner), Some(&1));
        assert_eq!((f.merged, f.landed, f.closed), (3, 1, 2));
        assert_eq!(f.landed_rate, Some(4.0 / 6.0));
        assert_eq!(
            f.reviews, 0,
            "the reviews were in o/lib, not across every repo"
        );
        let lib = person_figures(&view, &ann, "7d", "O/LIB");
        assert_eq!(
            (lib.open, lib.on_them, lib.reviews, lib.merged),
            (0, 1, 3, 0)
        );
        let rows = people_rows(&view, "7d", "");
        assert_eq!(
            rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["ann-example"],
            "a person with nothing in the window is left out"
        );
    }
}
