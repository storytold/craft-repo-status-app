//! Who did what across the repositories: the people (one per GitHub login,
//! with bot and AI accounts flagged), each repository's
//! maintainers and owners, the team roster read at runtime, the per pull
//! request records ([`record`]), the timestamped events derived from them
//! ([`ledger`]), each pull request's work state ([`states`]), type and area
//! ([`kind`]), who wrote its lines and the rankings ([`rank`]), each
//! contributor's track record ([`track`]), and what the People tab shows
//! ([`view`]).

pub mod kind;
pub mod ledger;
pub mod rank;
pub mod record;
pub mod roster;
pub mod states;
pub mod track;
pub mod view;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::model::Time;
use record::{CoAuthor, MergeCommit, PrCache, PrRecord, PrState};
use roster::RosterRow;

/// AI and automation accounts that GitHub lists as users. They are flagged
/// as bots, with every account whose type is `Bot`.
pub const AI_ACCOUNTS: &[&str] = &[
    "claude",
    "codex",
    "copilot",
    "bobtherobot",
    "copilot-pull-request-reviewer",
    "github-actions",
    "ticket-craft",
];

/// How far back the merges that make someone a maintainer reach: the last
/// [`MERGERS_DAYS`] days or the last [`MERGERS_COUNT`] merges, whichever
/// holds more merges.
pub const MERGERS_DAYS: i64 = 14;
pub const MERGERS_COUNT: usize = 100;

/// The key a login is counted under: trimmed and lowercase.
pub fn canonical(login: &str) -> String {
    login.trim().to_ascii_lowercase()
}

/// Whether `login` is a bot or AI account: GitHub's type for it is `Bot`,
/// it ends in `[bot]`, or it is in [`AI_ACCOUNTS`].
pub fn is_bot(login: &str, typename: Option<&str>) -> bool {
    typename == Some("Bot")
        || login.ends_with("[bot]")
        || AI_ACCOUNTS
            .iter()
            .any(|a| a.eq_ignore_ascii_case(login.trim()))
}

/// Whether `s` can be a GitHub login: 1 to 39 letters, digits and hyphens,
/// not starting or ending with a hyphen.
pub fn valid_login(s: &str) -> bool {
    (1..=39).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// The `Co-authored-by: Name <email>` trailers in a commit message, with
/// the login a `users.noreply.github.com` address names. AI accounts are
/// left out: by login, by full name, by an `anthropic.com`, `claude.com` or
/// `openai.com` address, or by a first name in [`AI_ACCOUNTS`] with no
/// address or a `noreply` one. A person whose first name is Claude, Codex or
/// Copilot and whose address is their own stays.
pub fn parse_co_authors(message: &str) -> Vec<CoAuthor> {
    let mut out: Vec<CoAuthor> = Vec::new();
    for line in message.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("co-authored-by") {
            continue;
        }
        let value = value.trim();
        let (name, email) = match (value.find('<'), value.rfind('>')) {
            (Some(l), Some(r)) if l < r => (value[..l].trim(), value[l + 1..r].trim()),
            _ => (value, ""),
        };
        let email = email.to_ascii_lowercase();
        let login = noreply_login(&email);
        let (local, domain) = email.rsplit_once('@').unwrap_or((&email, ""));
        let first = name.split_whitespace().next().unwrap_or_default();
        let unowned =
            email.is_empty() || local.starts_with("noreply") || local.starts_with("no-reply");
        let ai = login.as_deref().is_some_and(|l| is_bot(l, None))
            || is_bot(name, None)
            || (unowned && is_bot(first, None))
            || matches!(domain, "anthropic.com" | "claude.com" | "openai.com");
        if ai || (name.is_empty() && email.is_empty()) {
            continue;
        }
        let a = CoAuthor {
            name: name.to_string(),
            email,
            login,
        };
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// The commits a commit reverts: the oids in its message's `This reverts
/// commit <oid>` lines, lowercase, when its headline starts with `Revert "`
/// as `git revert` writes it. A squash commit that only quotes such a line
/// reverts nothing.
pub fn parse_reverts(headline: &str, message: &str) -> Vec<String> {
    if !headline.starts_with("Revert \"") {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for line in message.lines() {
        let Some(rest) = line.trim().strip_prefix("This reverts commit ") else {
            continue;
        };
        let oid: String = rest
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect::<String>()
            .to_ascii_lowercase();
        if oid.len() >= 7 && !out.contains(&oid) {
            out.push(oid);
        }
    }
    out
}

/// The login in `<id>+<login>@users.noreply.github.com` or
/// `<login>@users.noreply.github.com`.
fn noreply_login(email: &str) -> Option<String> {
    let local = email.strip_suffix("@users.noreply.github.com")?;
    let login = local.rsplit('+').next()?;
    valid_login(login).then(|| login.to_string())
}

/// Whether a commit in a pull request is a merge of another branch into
/// it, which carries that branch's lines: its headline starts with "Merge
/// branch", "Merge remote-tracking branch", "Merge pull request" or "Merge
/// commit", or it adds more lines than the whole pull request.
pub fn is_merge_commit(headline: &str, additions: u64, pr_additions: u64) -> bool {
    let merge = ["branch", "remote-tracking branch", "pull request", "commit"]
        .iter()
        .any(|w| headline.starts_with(&format!("Merge {w}")));
    merge || additions > pr_additions
}

/// Who landed a merged pull request: the merge commit's committer when that
/// is a person with a login and the commit is a real merge (two parents, or
/// a "Merge pull request" headline: a merge made locally and pushed),
/// otherwise `mergedBy`, as a [`canonical`] login either way. A pull request
/// landed by pushing its own commits has the head commit as its merge
/// commit, committed by the author, so its committer is not the merger.
pub fn merger(merge_commit: Option<&MergeCommit>, merged_by: Option<&str>) -> Option<String> {
    let committer = merge_commit
        .filter(|m| m.committer_name != "GitHub")
        .filter(|m| m.parents >= 2 || m.headline.starts_with("Merge pull request"))
        .and_then(|m| m.committer_login.as_deref());
    committer.or(merged_by).map(canonical)
}

/// One repository's maintainers, as canonical logins: everyone with the
/// `MEMBER` or `OWNER` association on a pull request, comment or review in
/// `records`; the `mergedBy` accounts and the mergers (see [`merger`]) of
/// the last [`MERGERS_DAYS`] days or the last [`MERGERS_COUNT`] merges,
/// whichever holds more; whoever closed a pull request they did not author;
/// and `extra`. Bot and AI accounts are left out.
pub fn maintainers<'a>(
    records: impl IntoIterator<Item = &'a PrRecord>,
    extra: &[String],
    now: Time,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut add = |login: &str, bot: bool| {
        if !bot && !is_bot(login, None) {
            out.insert(canonical(login));
        }
    };
    let member = |a: Option<&str>| matches!(a, Some("MEMBER" | "OWNER"));
    let mut merges: Vec<(Time, &str, Option<String>)> = Vec::new();
    for r in records {
        if let Some(author) = &r.author {
            if member(Some(&r.author_association)) {
                add(author, r.author_bot);
            }
        }
        for t in &r.timeline {
            let Some(actor) = t.actor.as_deref() else {
                continue;
            };
            if member(t.association.as_deref()) {
                add(actor, t.actor_bot);
            }
            let other = r.author.as_deref().map(canonical) != Some(canonical(actor));
            if t.kind == "ClosedEvent" && other {
                add(actor, t.actor_bot);
            }
        }
        if let (Some(at), Some(by), PrState::Merged) = (r.merged_at, &r.merged_by, r.state) {
            let landed = r
                .contribution
                .merger
                .clone()
                .or_else(|| merger(r.contribution.merge_commit.as_ref(), Some(by)));
            merges.push((at, by, landed));
        }
    }
    merges.sort_by_key(|m| std::cmp::Reverse(m.0));
    let recent = merges
        .iter()
        .filter(|(at, ..)| now - *at <= chrono::Duration::days(MERGERS_DAYS))
        .count();
    for (_, by, landed) in merges.iter().take(recent.max(MERGERS_COUNT)) {
        add(by, false);
        if let Some(l) = landed {
            add(l, false);
        }
    }
    for login in extra {
        add(login, false);
    }
    out
}

/// One person, keyed by canonical login in a [`Directory`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Person {
    /// The login as GitHub spells it.
    pub login: String,
    /// A bot or AI account (see [`is_bot`]); left out of rankings.
    pub bot: bool,
    pub on_roster: bool,
    pub name: Option<String>,
    pub discord: Option<String>,
    pub timezone: Option<String>,
    pub focus: Vec<String>,
    /// Repositories in whose maintainer set the person is.
    pub maintainer_of: Vec<String>,
    /// Repositories the person owns (see `owner_logins` in the config).
    pub owner_of: Vec<String>,
}

/// Everyone the records and the roster name. People who are not on the
/// roster are listed under their login; roster rows without a GitHub login
/// are kept in `unlinked`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Directory {
    pub people: BTreeMap<String, Person>,
    pub unlinked: Vec<RosterRow>,
}

/// One repository's part of a [`Directory`].
pub struct RepoPeople<'a> {
    pub repo: &'a str,
    pub cache: &'a PrCache,
    pub owners: &'a [String],
    pub maintainers: &'a BTreeSet<String>,
}

/// Builds the [`Directory`] from each repository's records, owners and
/// maintainers, and the roster.
pub fn directory(repos: &[RepoPeople], roster: &[RosterRow]) -> Directory {
    let mut dir = Directory::default();
    for rp in repos {
        for r in rp.cache.records.values() {
            if let Some(a) = &r.author {
                dir.see(a, r.author_bot);
            }
            for login in [&r.merged_by, &r.contribution.merger].into_iter().flatten() {
                dir.see(login, false);
            }
            for t in &r.timeline {
                if let Some(a) = &t.actor {
                    dir.see(a, t.actor_bot);
                }
            }
            for c in &r.contribution.commits {
                if let Some(a) = &c.author {
                    dir.see(a, false);
                }
                for co in &c.co_authors {
                    if let Some(l) = &co.login {
                        dir.see(l, false);
                    }
                }
            }
        }
        for m in rp.maintainers {
            dir.see(m, false).maintainer_of.push(rp.repo.to_string());
        }
        for o in rp.owners {
            dir.see(o, false).owner_of.push(rp.repo.to_string());
        }
    }
    for row in roster {
        let Some(login) = row.github.as_deref() else {
            dir.unlinked.push(row.clone());
            continue;
        };
        let p = dir.see(login, false);
        p.on_roster = true;
        p.name = row.name.clone();
        p.discord = row.discord.clone();
        p.timezone = row.timezone.clone();
        p.focus = row.focus.clone();
    }
    for p in dir.people.values_mut() {
        p.maintainer_of.dedup();
        p.owner_of.dedup();
    }
    dir
}

impl Directory {
    /// The person `login` counts as, added when new.
    fn see(&mut self, login: &str, bot: bool) -> &mut Person {
        let p = self
            .people
            .entry(canonical(login))
            .or_insert_with(|| Person {
                login: login.trim().to_string(),
                ..Default::default()
            });
        p.bot |= bot || is_bot(login, None);
        p
    }
}

#[cfg(test)]
mod tests {
    use super::record::{Contribution, PrCommit, TimelineItem};
    use super::*;

    fn t(s: &str) -> Time {
        s.parse().unwrap()
    }

    #[test]
    fn logins_match_case_insensitively() {
        assert_eq!(canonical(" Ann-Example "), "ann-example");
        assert!(is_bot("claude", None) && is_bot("Copilot", None));
        assert!(is_bot("dependabot[bot]", None));
        assert!(is_bot("helper", Some("Bot")));
        assert!(!is_bot("ann-example", Some("User")));
        assert!(valid_login("a-b-c") && valid_login("x1"));
        assert!(!valid_login("-x") && !valid_login("x y") && !valid_login(""));
        assert!(!valid_login(&"x".repeat(40)));
    }

    #[test]
    fn co_author_trailers_give_logins_and_skip_ai_accounts() {
        let msg = "Fix the thing\n\nBody.\n\n\
                   Co-authored-by: Ann Example <12345+ann-example@users.noreply.github.com>\n\
                   co-authored-by: Bo Sample <bo@example.com>\n\
                   Co-Authored-By: Claude Opus <noreply@anthropic.com>\n\
                   Co-authored-by: Helper Model <helper@openai.com>\n\
                   Co-authored-by: Copilot <175728472+Copilot@users.noreply.github.com>\n\
                   Co-authored-by: Cy <cy@users.noreply.github.com>\n\
                   Co-authored-by: Bo Sample <bo@example.com>\n\
                   Co-authored-by: Claude Martin <claude.martin@example.fr>\n\
                   Co-authored-by: Codex Lee <9+codex-lee@users.noreply.github.com>\n\
                   Co-authored-by: Claude Code <noreply@claude.com>\n\
                   Co-authored-by: Codex Agent <noreply@example.com>\n\
                   Co-authored-by: Copilot Helper\n";
        let co = parse_co_authors(msg);
        assert_eq!(
            co,
            vec![
                CoAuthor {
                    name: "Ann Example".into(),
                    email: "12345+ann-example@users.noreply.github.com".into(),
                    login: Some("ann-example".into()),
                },
                CoAuthor {
                    name: "Bo Sample".into(),
                    email: "bo@example.com".into(),
                    login: None,
                },
                CoAuthor {
                    name: "Cy".into(),
                    email: "cy@users.noreply.github.com".into(),
                    login: Some("cy".into()),
                },
                // People whose first name is an AI account's, at their own address.
                CoAuthor {
                    name: "Claude Martin".into(),
                    email: "claude.martin@example.fr".into(),
                    login: None,
                },
                CoAuthor {
                    name: "Codex Lee".into(),
                    email: "9+codex-lee@users.noreply.github.com".into(),
                    login: Some("codex-lee".into()),
                },
            ]
        );
        assert!(parse_co_authors("Co-authored-by:\n").is_empty());
    }

    #[test]
    fn a_revert_commit_names_the_commits_it_reverts() {
        let msg = "Revert \"Pen: smoother curves\"\n\nThis reverts commit ABCDEF1234567890.\n\
                   This reverts commit abcdef1234567890.\nThis reverts commit 12ab.\n";
        assert_eq!(
            parse_reverts("Revert \"Pen: smoother curves\"", msg),
            ["abcdef1234567890"]
        );
        // A squash commit that quotes the line is not a revert.
        assert!(parse_reverts("Fix the pen (#12)", msg).is_empty());
    }

    #[test]
    fn merge_commits_are_found_by_headline_or_size() {
        assert!(is_merge_commit("Merge branch 'main' into x", 3, 100));
        assert!(is_merge_commit(
            "Merge remote-tracking branch 'o/main'",
            3,
            100
        ));
        assert!(is_merge_commit("Tidy", 500, 100));
        assert!(!is_merge_commit("Merge sort for layers", 30, 100));
    }

    #[test]
    fn the_merger_is_the_local_committer_or_merged_by() {
        let mc = |name: &str, login: Option<&str>| MergeCommit {
            committer_name: name.into(),
            committer_login: login.map(str::to_string),
            parents: 2,
            ..Default::default()
        };
        // A merge or squash on the web, with or without an account on the committer.
        assert_eq!(
            merger(Some(&mc("GitHub", None)), Some("Maint-Two")).as_deref(),
            Some("maint-two")
        );
        let web = mc("GitHub", Some("web-flow"));
        assert_eq!(
            merger(Some(&web), Some("maint-two")).as_deref(),
            Some("maint-two")
        );
        // A local merge pushed by another account, committed by its author.
        let local = mc("Maint One", Some("maint-one"));
        assert_eq!(
            merger(Some(&local), Some("push-account")).as_deref(),
            Some("maint-one")
        );
        // The same merge with no account on the committer.
        let unlinked = mc("Maint One", None);
        assert_eq!(
            merger(Some(&unlinked), Some("push-account")).as_deref(),
            Some("push-account")
        );
        // Landed by pushing the author's own commits: the merge commit is the
        // head, with one parent, committed by the author.
        let pushed = MergeCommit {
            headline: "Fix the mixer".into(),
            parents: 1,
            ..mc("Grove Example", Some("grove-example"))
        };
        assert_eq!(
            merger(Some(&pushed), Some("maint-one")).as_deref(),
            Some("maint-one")
        );
        // A local merge whose parents were not read still counts by its headline.
        let titled = MergeCommit {
            headline: "Merge pull request #7 from ann/fix".into(),
            parents: 0,
            ..mc("Lands Locally", Some("lander"))
        };
        assert_eq!(
            merger(Some(&titled), Some("pusher")).as_deref(),
            Some("lander")
        );
        assert_eq!(merger(None, None), None);
    }

    fn merged(number: u64, at: &str, by: &str) -> PrRecord {
        PrRecord {
            number,
            state: PrState::Merged,
            author: Some(format!("author{number}")),
            author_association: "CONTRIBUTOR".into(),
            merged_at: Some(t(at)),
            merged_by: Some(by.into()),
            ..Default::default()
        }
    }

    #[test]
    fn maintainers_come_from_membership_merges_closes_and_the_override() {
        let now = t("2026-10-17T00:00:00Z");
        let mut member = merged(1, "2026-10-16T00:00:00Z", "maint-two");
        member.timeline.push(TimelineItem {
            kind: "IssueComment".into(),
            actor: Some("Rev-Member".into()),
            association: Some("MEMBER".into()),
            ..Default::default()
        });
        member.timeline.push(TimelineItem {
            kind: "IssueComment".into(),
            actor: Some("ticket-craft".into()),
            association: Some("MEMBER".into()),
            ..Default::default()
        });
        let mut closed = PrRecord {
            number: 2,
            state: PrState::Closed,
            author: Some("author2".into()),
            ..Default::default()
        };
        closed.timeline.push(TimelineItem {
            kind: "ClosedEvent".into(),
            actor: Some("closer-x".into()),
            ..Default::default()
        });
        // An author closing their own pull request is not a maintainer.
        closed.timeline.push(TimelineItem {
            kind: "ClosedEvent".into(),
            actor: Some("AUTHOR2".into()),
            ..Default::default()
        });
        let old = merged(3, "2026-09-01T00:00:00Z", "Maint-One");
        // Merged locally by one person and pushed by another account.
        let mut local = merged(4, "2026-10-15T00:00:00Z", "pusher");
        local.contribution.merge_commit = Some(MergeCommit {
            committer_name: "Lands Locally".into(),
            committer_login: Some("lander".into()),
            parents: 2,
            ..Default::default()
        });
        // Landed by pushing the author's commits: the author is no maintainer.
        let mut pushed = merged(5, "2026-10-15T00:00:00Z", "pusher");
        pushed.contribution.merge_commit = Some(MergeCommit {
            headline: "Fix the mixer".into(),
            parents: 1,
            committer_name: "Author Five".into(),
            committer_login: Some("author5".into()),
            ..Default::default()
        });
        let records = [member, closed, old, local, pushed];
        let m = maintainers(&records, &["extra".into()], now);
        let want: BTreeSet<String> = [
            "maint-two",
            "rev-member",
            "closer-x",
            "maint-one",
            "extra",
            "pusher",
            "lander",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(m, want);
    }

    #[test]
    fn mergers_reach_back_the_longer_of_fourteen_days_and_a_hundred_merges() {
        let now = t("2026-10-17T00:00:00Z");
        // 120 merges in the last week by one person, one older merge by another.
        let mut records: Vec<PrRecord> = (0..120)
            .map(|i| merged(i, "2026-10-15T00:00:00Z", "busy"))
            .collect();
        records.push(merged(500, "2026-09-01T00:00:00Z", "older"));
        let m = maintainers(&records, &[], now);
        assert!(m.contains("busy") && !m.contains("older"), "{m:?}");
        // With few merges, the last 100 reach back further than 14 days.
        let few = [
            merged(1, "2026-10-15T00:00:00Z", "busy"),
            merged(2, "2026-08-01T00:00:00Z", "older"),
        ];
        assert!(maintainers(&few, &[], now).contains("older"));
    }

    #[test]
    fn the_directory_joins_letter_cases_flags_bots_and_lists_unlinked_roster_rows() {
        let mut cache = PrCache::default();
        let mut r = merged(1, "2026-10-16T00:00:00Z", "Maint-One");
        r.author = Some("Ann-Example".into());
        r.contribution = Contribution {
            merger: Some("maint-one".into()),
            commits: vec![PrCommit {
                author: Some("github-actions".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        r.timeline.push(TimelineItem {
            kind: "PullRequestReview".into(),
            actor: Some("helper".into()),
            actor_bot: true,
            ..Default::default()
        });
        // A deleted account has no login.
        let ghost = PrRecord {
            number: 2,
            author: None,
            ..Default::default()
        };
        cache.records.insert(1, r);
        cache.records.insert(2, ghost);
        let owners = vec!["maint-one".to_string(), "maint-two".to_string()];
        let maint: BTreeSet<String> = ["maint-one".to_string()].into();
        let roster = vec![
            RosterRow {
                github: Some("ann-example".into()),
                name: Some("Ann".into()),
                focus: vec!["App One".into()],
                ..Default::default()
            },
            RosterRow {
                github: Some("newcomer".into()),
                ..Default::default()
            },
            RosterRow {
                name: Some("No Login".into()),
                ..Default::default()
            },
        ];
        let d = directory(
            &[RepoPeople {
                repo: "o/app",
                cache: &cache,
                owners: &owners,
                maintainers: &maint,
            }],
            &roster,
        );
        let ann = &d.people["ann-example"];
        assert_eq!(ann.login, "Ann-Example");
        assert!(ann.on_roster && !ann.bot);
        assert_eq!(ann.name.as_deref(), Some("Ann"));
        let m1 = &d.people["maint-one"];
        assert_eq!(m1.login, "Maint-One", "the first spelling seen");
        assert_eq!(m1.maintainer_of, ["o/app"]);
        assert_eq!(m1.owner_of, ["o/app"]);
        assert!(!m1.on_roster, "people off the roster are still listed");
        assert!(d.people["github-actions"].bot && d.people["helper"].bot);
        assert!(d.people["newcomer"].on_roster);
        assert_eq!(d.people["maint-two"].owner_of, ["o/app"]);
        assert_eq!(d.unlinked.len(), 1);
        assert_eq!(d.unlinked[0].name.as_deref(), Some("No Login"));
    }
}
