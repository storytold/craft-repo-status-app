//! Finds PRs that step on each other, without Claude: the same issue, the same
//! files or nearly the same title. Claude's groups, when it has run, take
//! precedence; these work from the first listing and cost nothing.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::{ClaudeGroup, Pr};

/// A file touched by more PRs than this says nothing about any two of them.
const HOT: usize = 25;
/// Stops weak links from chaining a whole repo into one group.
const MAX_GROUP: usize = 12;
const MIN_SHARED_FILES: usize = 3;
const MIN_FILE_JACCARD: f64 = 0.5;
const MIN_TITLE_WORDS: usize = 4;
const MIN_TITLE_JACCARD: f64 = 0.7;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Group {
    pub id: String,
    pub title: String,
    /// duplicate, overlap, stack, theme, issue, files or title.
    pub relation: String,
    pub note: String,
    pub numbers: Vec<u64>,
    /// claude or local.
    pub source: String,
}

struct Links {
    parent: HashMap<u64, u64>,
    size: HashMap<u64, usize>,
}

impl Links {
    fn new(numbers: impl Iterator<Item = u64>) -> Links {
        let numbers: Vec<u64> = numbers.collect();
        Links {
            parent: numbers.iter().map(|n| (*n, *n)).collect(),
            size: numbers.iter().map(|n| (*n, 1)).collect(),
        }
    }

    fn find(&mut self, n: u64) -> u64 {
        let p = self.parent[&n];
        if p == n {
            return n;
        }
        let root = self.find(p);
        self.parent.insert(n, root);
        root
    }

    /// Links `a` and `b` unless that makes too big a group.
    fn join(&mut self, a: u64, b: u64) -> bool {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return false;
        }
        let total = self.size[&a] + self.size[&b];
        if total > MAX_GROUP {
            return false;
        }
        self.parent.insert(b, a);
        self.size.insert(a, total);
        true
    }
}

/// Issue numbers named as `#123` in a title.
fn issues_in_title(title: &str) -> Vec<u64> {
    let mut out = vec![];
    for part in title.split('#').skip(1) {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u64>() {
            out.push(n);
        }
    }
    out
}

fn title_words(title: &str) -> HashSet<String> {
    const SKIP: &[&str] = &[
        "the", "and", "for", "fix", "feat", "with", "from", "that", "this", "when", "into", "add",
        "adds", "chore", "docs", "refactor", "use",
    ];
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !SKIP.contains(w))
        .map(str::to_string)
        .collect()
}

fn is_common_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "Cargo.lock"
            | "Cargo.toml"
            | "package.json"
            | "package-lock.json"
            | "CHANGELOG.md"
            | "README.md"
            | "ATTRIBUTION.md"
            | "NOTICE"
            | "lib.rs"
            | "mod.rs"
            | "main.rs"
            | "tests.rs"
    )
}

fn jaccard(shared: usize, a: usize, b: usize) -> f64 {
    shared as f64 / (a + b - shared).max(1) as f64
}

/// The pairs that share at least one rare key, with how many they share.
fn pairs_sharing<'a>(
    index: impl Iterator<Item = &'a Vec<u64>>,
    cap: usize,
) -> BTreeMap<(u64, u64), usize> {
    let mut pairs = BTreeMap::new();
    for members in index.filter(|m| m.len() >= 2 && m.len() <= cap) {
        for (i, a) in members.iter().enumerate() {
            for b in &members[i + 1..] {
                *pairs.entry((*a.min(b), *a.max(b))).or_insert(0) += 1;
            }
        }
    }
    pairs
}

/// Links PRs naming the same issue; returns which issue each linked PR came in by.
fn link_by_issue(prs: &[&Pr], links: &mut Links) -> HashMap<u64, u64> {
    let mut by_issue: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for p in prs {
        let mut issues = issues_in_title(&p.title);
        if let Some(d) = &p.detail {
            issues.extend(d.linked_issues.iter().map(|i| i.number));
        }
        issues.sort_unstable();
        issues.dedup();
        for i in issues {
            by_issue.entry(i).or_default().push(p.number);
        }
    }
    let mut issue_of: HashMap<u64, u64> = HashMap::new();
    for (issue, members) in by_issue.iter().filter(|(_, m)| m.len() >= 2) {
        for pair in members.windows(2) {
            if links.join(pair[0], pair[1]) {
                issue_of.entry(pair[0]).or_insert(*issue);
                issue_of.entry(pair[1]).or_insert(*issue);
            }
        }
    }
    issue_of
}

/// Links PRs changing enough of the same files, noting why.
fn link_by_files(prs: &[&Pr], links: &mut Links, why: &mut HashMap<(u64, u64), String>) {
    let files_of: HashMap<u64, HashSet<&str>> = prs
        .iter()
        .filter_map(|p| {
            let d = p.detail.as_ref()?;
            Some((
                p.number,
                d.files
                    .iter()
                    .map(|f| f.path.as_str())
                    .filter(|f| !is_common_file(f))
                    .collect(),
            ))
        })
        .collect();
    let mut by_file: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for (n, files) in &files_of {
        for f in files {
            by_file.entry(f).or_default().push(*n);
        }
    }
    for members in by_file.values_mut() {
        members.sort_unstable();
    }
    for ((a, b), shared) in pairs_sharing(by_file.values(), HOT) {
        let (fa, fb) = (&files_of[&a], &files_of[&b]);
        // One file in common means nothing unless it's all either PR changes.
        let strong = shared >= MIN_SHARED_FILES
            || (shared >= 2 && jaccard(shared, fa.len(), fb.len()) >= MIN_FILE_JACCARD)
            || (shared == 1 && fa.len() == 1 && fb.len() == 1);
        if strong && links.join(a, b) {
            let mut common: Vec<&&str> = fa.intersection(fb).collect();
            common.sort();
            let file = common.first().map_or("", |f| **f);
            why.insert((a, b), format!("{shared} shared file(s), such as {file}"));
        }
    }
}

/// Links PRs with nearly the same title, noting why.
fn link_by_title(prs: &[&Pr], links: &mut Links, why: &mut HashMap<(u64, u64), String>) {
    let words: HashMap<u64, HashSet<String>> = prs
        .iter()
        .map(|p| (p.number, title_words(&p.title)))
        .filter(|(_, w)| w.len() >= MIN_TITLE_WORDS)
        .collect();
    let mut by_word: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for (n, ws) in &words {
        for w in ws {
            by_word.entry(w).or_default().push(*n);
        }
    }
    for members in by_word.values_mut() {
        members.sort_unstable();
    }
    for ((a, b), shared) in pairs_sharing(by_word.values(), 40) {
        if jaccard(shared, words[&a].len(), words[&b].len()) >= MIN_TITLE_JACCARD
            && links.join(a, b)
        {
            why.insert((a, b), "Nearly the same title".to_string());
        }
    }
}

/// Groups `prs` by shared issues, then files, then near-identical titles.
pub fn local_groups(prs: &[&Pr]) -> Vec<Group> {
    let mut links = Links::new(prs.iter().map(|p| p.number));
    let mut why: HashMap<(u64, u64), String> = HashMap::new();
    let issue_of = link_by_issue(prs, &mut links);
    link_by_files(prs, &mut links, &mut why);
    link_by_title(prs, &mut links, &mut why);

    let mut clusters: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    let numbers: Vec<u64> = prs.iter().map(|p| p.number).collect();
    for n in numbers {
        let root = links.find(n);
        clusters.entry(root).or_default().push(n);
    }
    let mut out: Vec<Group> = clusters
        .into_values()
        .filter(|c| c.len() >= 2)
        .enumerate()
        .map(|(i, mut numbers)| {
            numbers.sort_unstable();
            let issue = numbers.iter().find_map(|n| issue_of.get(n));
            let note = match issue {
                Some(issue) => format!("Each names or closes #{issue}"),
                None => why
                    .iter()
                    .filter(|((a, b), _)| numbers.contains(a) && numbers.contains(b))
                    .min_by_key(|(k, _)| **k)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default(),
            };
            let (relation, title) = match issue {
                Some(issue) => ("issue", format!("Same issue: #{issue}")),
                None if note.starts_with("Nearly") => ("title", "Similar titles".to_string()),
                None => ("files", "Touch the same files".to_string()),
            };
            Group {
                id: format!("l{i}"),
                title,
                relation: relation.to_string(),
                note,
                numbers,
                source: "local".to_string(),
            }
        })
        .collect();
    out.sort_by_key(|g| std::cmp::Reverse(g.numbers.len()));
    out
}

/// Claude's groups, then the local groups it didn't already cover. Numbers no
/// longer open are dropped, as are groups left with fewer than two PRs.
pub fn merged_groups(claude: &[ClaudeGroup], local: Vec<Group>, open: &HashSet<u64>) -> Vec<Group> {
    let mut out: Vec<Group> = claude
        .iter()
        .enumerate()
        .filter_map(|(i, g)| {
            let mut numbers: Vec<u64> = g
                .numbers
                .iter()
                .copied()
                .filter(|n| open.contains(n))
                .collect();
            numbers.sort_unstable();
            numbers.dedup();
            (numbers.len() >= 2).then(|| Group {
                id: format!("c{i}"),
                title: g.title.clone(),
                relation: g.relation.clone(),
                note: g.note.clone(),
                numbers,
                source: "claude".to_string(),
            })
        })
        .collect();
    let covered: Vec<HashSet<u64>> = out
        .iter()
        .map(|g| g.numbers.iter().copied().collect())
        .collect();
    out.extend(local.into_iter().filter(|g| {
        !covered
            .iter()
            .any(|c| g.numbers.iter().all(|n| c.contains(n)))
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileChange, LinkedIssue, PrDetail};

    fn pr(number: u64, title: &str, files: &[&str]) -> Pr {
        Pr {
            number,
            title: title.into(),
            detail: (!files.is_empty()).then(|| PrDetail {
                files: files
                    .iter()
                    .map(|p| FileChange {
                        path: p.to_string(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn groups(prs: &[Pr]) -> Vec<Group> {
        local_groups(&prs.iter().collect::<Vec<_>>())
    }

    #[test]
    fn same_issue_in_title_or_links() {
        let mut linked = pr(3, "Totally different words", &[]);
        linked.detail = Some(PrDetail {
            linked_issues: vec![LinkedIssue {
                number: 10467,
                title: String::new(),
            }],
            ..Default::default()
        });
        let g = groups(&[
            pr(1, "Right-click layer picker grows (#10467)", &[]),
            pr(2, "Fix the picker for #10467 again", &[]),
            linked,
            pr(4, "Unrelated (#999)", &[]),
        ]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].numbers, [1, 2, 3]);
        assert_eq!(g[0].relation, "issue");
        assert_eq!(g[0].title, "Same issue: #10467");
    }

    #[test]
    fn shared_files_group_prs() {
        let g = groups(&[
            pr(1, "One", &["a/x.rs", "a/y.rs"]),
            pr(2, "Two", &["a/x.rs", "a/y.rs", "a/z.rs"]),
            pr(3, "Three", &["b/q.rs"]),
        ]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].numbers, [1, 2]);
        assert_eq!(g[0].relation, "files");
        assert!(g[0].note.contains("a/x.rs"), "{}", g[0].note);
    }

    #[test]
    fn one_shared_file_only_counts_if_its_all_they_change() {
        assert!(groups(&[
            pr(1, "One", &["a/x.rs", "a/own1.rs"]),
            pr(2, "Two", &["a/x.rs", "a/own2.rs"])
        ])
        .is_empty());
        assert_eq!(
            groups(&[pr(1, "One", &["a/x.rs"]), pr(2, "Two", &["a/x.rs"])]).len(),
            1
        );
    }

    #[test]
    fn an_issue_group_says_so_whatever_else_linked_it() {
        let g = groups(&[
            pr(1, "Fix the picker for the layers panel now (#7)", &[]),
            pr(2, "Fix the picker for the layers panel now (#7)", &[]),
        ]);
        assert_eq!(g[0].note, "Each names or closes #7");
    }

    #[test]
    fn manifests_and_hot_files_dont_link_prs() {
        let g = groups(&[
            pr(1, "One", &["Cargo.lock", "Cargo.toml", "x/src/lib.rs"]),
            pr(2, "Two", &["Cargo.lock", "Cargo.toml", "x/src/lib.rs"]),
        ]);
        assert!(g.is_empty(), "{g:?}");
        let many: Vec<Pr> = (0..30)
            .map(|n| {
                pr(
                    n,
                    &format!("T{n}"),
                    &["src/ui/app.rs", &format!("own{n}.rs")],
                )
            })
            .collect();
        assert!(groups(&many).is_empty());
    }

    #[test]
    fn near_identical_titles() {
        let g = groups(&[
            pr(
                1,
                "Linux: return memory of closed documents to the system",
                &[],
            ),
            pr(
                2,
                "Return memory of closed documents to the system on Linux",
                &[],
            ),
            pr(3, "Reuse large buffers so freed memory stays fast", &[]),
        ]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].numbers, [1, 2]);
        assert_eq!(g[0].relation, "title");
    }

    #[test]
    fn groups_cant_snowball() {
        let many: Vec<Pr> = (0..40).map(|n| pr(n, "Tweak (#7)", &[])).collect();
        let g = groups(&many);
        assert!(g.iter().all(|g| g.numbers.len() <= MAX_GROUP));
    }

    #[test]
    fn claude_groups_come_first_and_absorb_local_ones() {
        let open: HashSet<u64> = [1, 2, 3, 4, 5].into_iter().collect();
        let claude = [
            ClaudeGroup {
                title: "Panels".into(),
                relation: "overlap".into(),
                note: "n".into(),
                numbers: vec![1, 2, 3, 99],
            },
            ClaudeGroup {
                title: "Gone".into(),
                relation: "stack".into(),
                note: "".into(),
                numbers: vec![4, 98],
            },
        ];
        let local = vec![
            Group {
                id: "l0".into(),
                numbers: vec![1, 2],
                source: "local".into(),
                ..Default::default()
            },
            Group {
                id: "l1".into(),
                numbers: vec![4, 5],
                source: "local".into(),
                ..Default::default()
            },
        ];
        let g = merged_groups(&claude, local, &open);
        assert_eq!(g.len(), 2);
        assert_eq!(
            (g[0].source.as_str(), g[0].numbers.clone()),
            ("claude", vec![1, 2, 3])
        );
        assert_eq!(g[1].id, "l1");
    }
}
