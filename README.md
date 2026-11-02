Craft Repo Status App
=====================

Find out the status of the craft apps: a tray dashboard ("Craft Status") that
polls the storytold `*craft` repositories on GitHub, keeps a cache, and shows
downloads, what to fix first, and how close each open PR is to landing, with
CodeRabbit's findings and Claude's summaries. Everything runs locally: no server.

![Craft Status: the repos table](docs/screenshot.png)

- **Rust workspace**
  - `craft-core`: the model, release/download accounting, the urgency
    heuristic, config, cache and window state. No network or UI dependencies,
    and it is unit-tested.
  - `craft-github`: the GitHub GraphQL/REST fetcher, plus a `craft-fetch` CLI
    that prints a table, for debugging.
  - `pr-core`: the PR model, CodeRabbit parser, landing heuristics, overlap
    detection, fetch schedule and PR cache. No network either.
  - `pr-github`: the PR fetcher, plus a `pr-fetch` CLI for debugging.
  - `pr-claude`: the Claude Code CLI call, prompts and spending ledger.
  - `craft-status-app`: the Tauri 2 shell (poll threads, tray, global
    shortcut, window).
- **TypeScript, HTML and CSS** in `ui/`, with no framework or bundler. `tsc`
  compiles `ui/src/` to `ui/build/`, which `index.html` loads; the app's
  `build.rs` runs it, so building the app needs Node.js 24+ and `npm ci`.
  `ui/src/model.ts` and `pr-model.ts` are pure; they and the UI itself (in happy-dom) are tested
  with `node --test`. No JavaScript is committed: see `CLAUDE.md`.

## Tabs

| Tab | Key | What it's for | Third-party repos |
| --- | --- | --- | --- |
| **Downloads** | `⌘1` | Download counts and rates, builds, issues and activity for every repo | shown, tagged `3P` |
| **PRs** | `⌘2` | Reviewing open PRs: grades, CodeRabbit findings, Claude's summaries | left out |
| **Fix first** | `⌘3` | Open issues most worth fixing | shown, tagged `3P` |
| **People** | `⌘4` | Open pull requests by work state, and rankings of who merged, reviewed and shipped what | shown |

The chips beside the tabs show or hide kinds of repo on every tab: **3rd party**, **Apps**,
**Libraries**, **Assets**, **CI** and **Tooling**. A repo hides if any kind it is has been
turned off, so solidcraft goes with either 3rd party or Apps. Hiding only changes what's on
screen, including totals and tab counts: the repo keeps being fetched, and fetch errors still
show. The choice is remembered.

## Downloads

The default list covers the apps and also libraries, assets, CI and tooling repos.
The **Type** column says which is which (`DEFAULT_REPOS` in
`crates/craft-core/src/config.rs`); any repo not named there shows `—`.

Every column sorts. Click a header; click it again to reverse the order. Repos
with no data always sort last. Or drag rows into your own order (say, by
priority): dropping one clears the sort, and the order is remembered until you
sort by a column again.

| Group | Columns |
| --- | --- |
| Fix first | **Urgency** (the sum of the top five open-issue scores) · **Critical** (open issues naming a crash, hang, freeze, data loss, launch failure or security problem) |
| Open | open PRs · open issues · oldest open PR |
| Latest | last commit to `main` · newest issue · newest PR |
| Activity (dropdown) | commits to `main` · PRs opened · PRs merged · issues opened · people committing to `main` · **Total DLs** (downloads across all releases; only under All time) |
| Downloads | the latest release's tag · its publish date · its download rate and downloads · **Unreleased** (commits on `main` since it) |

- **Activity** covers the window picked in that group's header: the last
  10 min, 30 min, hour, 4 h (the default), 12 h, day or week, since the last
  release, or all time. Every fetch gets all nine, so switching is instant;
  the choice is remembered. People are distinct commit authors in the window;
  all time, it is GitHub's contributor count. The numbers are as of the last
  fetch of that repo, so short windows lag by up to `poll_minutes`.
- **Since last release** starts at each repo's own latest build, so the span
  differs per row (hover a number for the release and its date). Its commits
  are the **Unreleased** count; PRs, merges, issues and people count from the
  release's publish date. Repos with no release show `—`.
- **Builds** are GitHub releases. Drafts are ignored. Prereleases count, and
  they carry a `PRE` tag.
- **Unreleased** counts the commits that have landed on `main` since the latest
  build, by comparing its tag with `main`. When the tag's commit isn't on
  `main` (say, a release branch with a version bump), it counts the commits
  since the two split. When they share no history, it counts the `main`
  commits newer than the tag's commit. Both estimates show a `~`. Click the
  number to open the comparison on GitHub.
- **Downloads** add up every OS and package type: dmg, msi, AppImage, deb,
  rpm, flatpak, zips and tarballs. Checksum lists, signatures, `.zsync` delta
  files and updater manifests are left out, because machines fetch them, not
  people.
- **Fix first** (`⌘3`) ranks open issues across every repo. Each issue shows
  the reasons behind its score. Click a row in the Downloads table to filter the
  list to that repo. Click an issue to open it on GitHub. The caret on a
  Downloads row shows the repo's people and open work under the row instead (see
  [The People tab](#the-people-tab)).
- **People** (`⌘4`) shows one row per person with their open pull requests
  by work state, who each one waits on, and their flow and reviews in a
  window, or the rankings. The tab's badge counts the open pull requests held
  for the owner. See [The People tab](#the-people-tab).
- **Freshness**: the header shows when the oldest data on screen was fetched,
  and a pill that reads `FRESH`, `REFRESHING` or **`STALE`**. Data is stale
  when any repo has gone `stale_minutes` (default 15) without a successful
  fetch. A stale row is dimmed and gets an amber dot; a failed row gets a red
  dot, and its error appears in the banner. The tray tooltip also says
  whether the data is stale.

### How issues are scored

The score is deterministic and simple to read (`crates/craft-core/src/urgency.rs`):

| Signal | Points |
| --- | --- |
| Crash, panic, freeze, hang, data loss, corruption, won't open or launch, blank screen, security | +40 |
| Broken, but not severe ("doesn't work", error, regression…) | +15 |
| `bug` label | +15 |
| Filed by someone outside the team | +10 |
| Reactions ×3 and comments ×2, log-scaled | up to +40 |
| Names the latest build's version | +10 |
| Opened in the last 24 h | +8 |
| Opened in the last 72 h | +4 |
| Feature request, question or docs nit | −25 |
| Untouched for 30 days | −5 |

Issues labelled `wontfix`, `duplicate` or `invalid` are dropped. For each
repo, the 100 newest open issues and the 30 most-commented open issues are
scored.

## The People tab

Every open pull request has one work state, decided by the rules in
`crates/craft-core/src/people/states.rs` in order: the author stepped back,
held for the owner, a maintainer's own task, declined in a comment,
superseded, parked by the author, depends on another pull request, draft, not
ready, competing pull requests, move to craft-libs, changes requested by a
maintainer, changes requested by a community reviewer, merge conflict,
workflow approval, CI failing (from the pull request or inherited from
`main`), CI running, author answered, community approved, ready to merge and
awaiting a first response. The first rule that matches gives the state; every
later match is listed on hover. A maintainer request made at or after a hold
outranks the hold and waits on both the author and the owner. In repos listed
in `people.rebase_repos`, a conflict on a contributor's pull request that
maintainers can push to is a flag.

States read from GitHub fields (draft, mergeability, checks) show plainly.
States read from comment text carry a dotted underline. Hovering one shows
the rule, the matched sentence (up to 200 characters) with its author and
date, the phrase that matched, and the other states that matched; clicking it
opens the comment on GitHub. A `?` marks low confidence: the two author
states, a competitor found only through a shared issue, and a close with no
maintainer response. Counts give the part read from comments, as in
`3 (2 from comments)`.

A pull request the app has found but whose comments, reviews and events it
has not read yet shows as **Not read yet**, under its author, with when GitHub
last saw it change. It waits on nobody, and no rule decides its state until
the app reads it. Until every record is read, the bar shows
`N of M timelines read`.

The columns are Open, On them (pull requests waiting on the person: their own
that wait on the author, ones where they made the last maintainer request or
comment, and holds they own), On others, Held, Merged, Landed elsewhere,
Closed (without merging, folded parents and accidental closes left out), To
merge (median), Landed (merged plus landed elsewhere over everything closed),
They answer (their median time to answer a maintainer request; a rebase or
split request is answered only by a push), First response (the median time
to the first maintainer comment, review or merge on their pull requests) and
Reviews given. The window picker sets the window for flow and reviews; the
response times cover all time. The repo picker limits every figure to one
repo. Every column sorts. A name from the team roster (`people.roster`)
shows next to the login.

The search box in the bar (`⌘F`) filters the work view and Rankings as you
type, by GitHub login, roster name, the roster's Discord name, and repo.
Every word has to match. In Rankings, a repo matches the people listed in
that repo's own figures, co-authors included. The bar shows how many people
match, as in `3 of 12 people`, and `Esc` clears the search.

**Group by repo**, next to the Work and Rankings buttons, shows one section
per repo with that repo's people and each person's figures in that repo. A
section's header row gives its people count and the totals of the columns
that add up; its caret collapses the section. Sorting applies inside each
section, and the search keeps a person in a section when the words match them
or the section's repo. The app remembers the choice and the collapsed
sections. In Rankings, the sections come from the same per-repo figures as
the caret on a Repos row, and their header rows leave Co-authored, Active days
and PRs reviewed blank.

The caret on a person opens one row per repo and the person's detail: open
work by state, grouped by who it waits on, with the median and longest time in
state and the five oldest items; the work of others that waits on them; their
pull requests held for the owner, with the hold's age, who placed it, its
reason (design, policy, brand, packaging, library, dependency or owner
decision) and when it was last held again, and the ones they parked
themselves; their response times and unanswered requests; the reviews,
requests, holds, merges and closes they gave; their flow by subtype and
outcome; and where their work went (see below). The bar also shows how old
the records are, in the alert color past `stale_minutes` or while a repo has
never been checked.

**Rankings** (the second button in the bar) lists one row per person for
the window picked, with a selector for three rankings. Every column sorts.

- **Merged**: their merged pull requests; pull requests that landed elsewhere;
  product lines added and removed, split by commit author; lines in
  `Cargo.lock`, translation catalogs and `contributors/contributors.json` in a
  column of their own; changed files; commits they authored in merged pull
  requests; commits whose `Co-authored-by:` trailer names them (AI co-authors
  are left out); commits on `main` that belong to no pull request; and active
  days (UTC days with a commit, a merge or a review). Direct commits come from
  the history of `main` that the app keeps, so they show for the 7-day and
  release windows only, once every merged pull request is loaded, and `—`
  otherwise.
- **Maintainer**: merges, credited to the merge commit's committer when
  someone pushed a local merge and to GitHub's merged-by otherwise; reviews of
  other people's pull requests, split into approved, changes requested and
  commented, with the number of pull requests reviewed; and review commits
  (commits in someone else's pull request) with their lines. A merge count
  with batch merges carries a badge such as `14 in batch pushes`.
- **Type**: their merged pull requests by type (feature, fix, docs, tests, ci,
  packaging, dependency, i18n, refactor, perf, release) and product work
  (feature plus fix). The picker next to the selector sorts by one type. A
  `?` marks counts whose type a fallback rule decided; hovering it says so.

A merged pull request's product lines are split over its non-merge commits by
each commit's share of the commits' lines. A commit that an earlier merged
pull request in the same repo holds counts there only. A commit without a
linked account, and a pull request whose commits past the first 100 are not
read yet, credit their share to the author. The type and area come from
`crates/craft-core/src/people/kind.rs`: the title's Conventional Commits type
or `Area:` prefix, `Fix #N:`, the changed paths, refactor and perf words, the
branch prefix and fix words, in that order. In repos listed in
`people.batch_repos`, merge and landed counts carry an `*` marking them
unverified. Roster rows without a GitHub login and co-author names without an
account are listed under the table. While the first load of merged and closed
pull requests runs, the bar shows how many of each are loaded.

The caret on a person, in Rankings and in the work view, shows where their
work went in the window: by repository (with their merges, reviews and review
commits), by area, by type and by directory (two path segments under
`crates/` and `apps/`, translation catalogs grouped as `translations`). With a
repo picked, and in a repo section of the grouped view, the tables cover that
repo alone. Each row gives their merged pull requests, their product lines and
their share of their own total. Click a directory, or press `Enter` or `Space`
on it, to list its modules under `src/`. Hover an area or a type for the rule
that decided it.

The caret on a row of the Repos table shows that repo's people for the table's
window, with their merges, reviews and review commits, and its open work: open
pull requests by who they wait on and state, the oldest one waiting on a
maintainer and the first-response queue, the pull requests held for the owner,
the maintainer and author queues, the checks on `main`, the flow in the window
(merged by subtype, landed elsewhere, closed by outcome, median time to merge
and to the first maintainer response, merge methods and mergers), aging and
stale counts, and pull requests that wait on or compete with another.

## PRs

The PR tab covers every watched repo except third-party ones: a repo counts as
first party if its owner is in `first_party_owners`, and `exclude_repos` leaves out
more. Solidcraft's PRs aren't fetched, graded or sent to Claude.

**Repos** has one row per repo, and every column sorts: open PRs, how many have been
inspected, landable, promising, reviewable, CI not run, approved, high quality,
easy/medium/hard, needs work, conflicts, failing CI, CodeRabbit flags and nits,
security flags and nits, PRs by core, frequent and new authors, interesting PRs,
possibly related groups and ages. Its **Type** column says which are apps, libraries,
assets, CI or tooling.

**A repo** (click its row) lists its PRs, best first, with sortable columns (including
who approved each one and what it's possibly related to), filters, search and **Group
possibly related**. Drafts are hidden unless asked for.

**A PR** (click its row) has the verdict and the reasons behind it, Claude's
assessment, CodeRabbit findings worst first, security flags, the author's standing,
the PRs it's possibly related to, checks, reviews, files and the description. **Read
the diff** asks Claude for a closer look.

**Review queue** is what someone can act on across every repo: Landable, Promising,
Reviewable and CI not run. The number on the PRs tab counts these.

`Esc` goes back a level and hides the window at the top. `/` searches. Choosing the PRs
tab again goes back to its top; leaving it keeps your place. `⌘R` refreshes what's on
screen.

#### How PRs are graded

All of it is deterministic, and the PR view shows the working
(`crates/pr-core/src/assess.rs`).

A PR has independent **flags** (Approved, Conflicted, CI failed…) and one **status**
worked out from them, from this truth table. The first matching row wins, and `–`
means that column doesn't matter. The `?` beside a PR table explains all of this in
the app. A *problem* is a failed check, requested changes, an unresolved critical or
major CodeRabbit finding, a failed security check, or an open conversation started by
a person. *Running* means CI or CodeRabbit is going, or GitHub hasn't worked out
mergeability.

| Draft | Problem | Conflicted | Inspected | Running | CI | Approved | Quality ≥ 75 | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Y | – | – | – | – | – | – | – | **Draft** |
| N | Y | – | – | – | – | – | – | **Needs work** |
| N | N | Y | – | – | – | – | – | **Conflicted** |
| N | N | N | N | – | – | – | – | **Not inspected** |
| N | N | N | Y | Y | – | – | – | **Checking** |
| N | N | N | Y | N | not run | – | – | **CI not run** |
| N | N | N | Y | N | pass | Y | – | **Landable** |
| N | N | N | Y | N | pass | N | Y | **Promising** |
| N | N | N | Y | N | pass | N | N | **Reviewable** |

- **Approved** means GitHub's review decision is approved, on the latest commit. An
  approval of an older commit shows as a *Stale approval* flag and doesn't make a PR
  Landable. The Approved by column also lists approvals GitHub doesn't count, usually
  from people without write access, in grey.
- **Cancelled and skipped checks** don't count as failures or as CI having run. Until a
  PR is inspected, only GitHub's overall result is known, which counts a cancelled
  run as a failure.
- **The Status column** sorts best first, in the order Landable, Promising, Reviewable,
  CI not run, Checking, Not inspected, Conflicted, Needs work, Draft. Quality, ease and
  the author only break ties within a status.
- **Possibly related** never changes a status. Each PR lists the others it may relate to,
  and links go both ways: click a number to jump to its row, or ⌘-click to open it.
- **Ease** adds up size (not counting lockfiles), files, spread, conflicts, failing
  CI, flags and sensitive paths, less a point for authors who know the codebase.
- **Quality** starts at 60. It moves with CI, conflicts, reviews, description, a
  linked issue, tests, CodeRabbit's findings and the author's record. Until a PR is
  inspected it shows as `~` and can't count as high quality.
- **CodeRabbit**: critical and major findings are *flags*; minor and trivial are
  *nits*. Resolved and outdated threads don't count.
- **Security** means CodeRabbit findings in its `Security & Privacy` category (or
  titled like one), failed checks named like security checks, and `sensitive_paths`
  changed by someone without write access. The last doesn't count against a PR, since the
  author can't do anything about it: it asks the reviewer to look closer.
- **Authors**: *core* has write access or better, or is in the repo's
  `contributors/people.toml`. Contributors may add their own entry there, so set
  `people_toml_is_core = false` for a stricter list. *Frequent* has more than 30 PRs
  merged across the craft apps, counted from merge history. *New* is GitHub's
  first-time contributor.

### Claude

Claude runs through the Claude Code CLI (`claude -p`), using its existing login.
It works through inspected PRs best first, and regroups each repo daily.

- **Spending**: at most `max_runs_per_hour` background calls (12) and
  `max_usd_per_day` ($5, as reported by the CLI). The footer shows the day's total.
  At these limits only part of the backlog gets summarized each day, so change them
  in the config's `[pr_review.claude]`, along with `model` and `enabled`.
- **Safety**: PR text is written by strangers, so Claude gets no tools, MCP servers,
  settings or project files, and its answers are only shown as text. It can mark a PR
  harder or interesting, but can't change its status or raise its quality or ease.

### PR polling

There are too many open PRs to inspect in one go (about 7,900, at 16 points per 25),
so work is split into tiers:

1. **Lists** of every open PR, every 30 min if anything changed, and a full rewalk
   every 4 h, which is the only way to see CI finishing on a PR nobody touched.
2. **Who can write**, daily.
3. **Merge history**, once, for the landed counts (about 160 pages).
4. **Inspection**, best candidates first: findings, files, checks, reviews. A first
   pass over everything takes a couple of hours, and after that only changed PRs
   are refetched. The footer counts what's left.

The PR cache is `~/Library/Caches/craft-pr-review/`. Fetching stops for the budget at
`max_points_per_hour` (2500), and everything pauses below `budget_floor` (1000), well
above the 150 where the Downloads tab's polling pauses itself.

## Polling and cache

- The app fetches one repo at a time, 20 s apart: first any repo with no data
  (or data cached by an older build that lacks a column), then whichever repo
  has the oldest data, once it is `poll_minutes` old (default 30). Within a
  repo it pauses after each GraphQL request as long as it took: GitHub allows
  only 60 s of GraphQL server time per minute, and its secondary rate limits
  punish bursts. Each repo costs about six GraphQL requests and one REST
  request. It never uses search, which GitHub limits far more tightly (to
  about 30 a minute); PR and issue counts per window come from walking the
  recent PRs and issues.
- Each repo's icon is `assets/app-icon/hicolor/64x64/apps/*.png` on its default
  branch. It is downloaded once and again only when the file changes; it's
  kept in the cache. Repos without one (libraries) get a lettered tile.
- If fewer than 150 points remain, fetching pauses for 30 minutes. If GitHub
  rate-limits the app, it pauses for 5 minutes, or until GitHub's retry time
  if that is later, doubling up to an hour while the limit persists. A repo
  whose fetch fails (offline, just woke from sleep) is retried after 10
  minutes; the others carry on.
- Timing follows the wall clock, so a laptop that wakes from sleep polls
  within about 15 s.
- `⌘R` or the ⟳ button makes every repo due at once; they are still fetched
  one at a time, stalest first, and a rate-limit pause still holds.
- The People tab's pull request records are kept up to date by a thread of
  their own. At launch and after each successful repo fetch, it reads the
  changed pull requests of the repos whose newest pull request update moved
  (the fetch reads that time with its other counts), sweeps the open pull
  requests of a share of the repos so that each is swept every 30 minutes,
  reads the records the sweep found (yours first, then the most recently
  updated), and continues the first load of merged and closed pull
  requests. It reads commit lists and the checks on a head commit in pages
  of 100, one request a page. Its requests go one at a
  time and only while no repo is being fetched, so they fill the 20 s gaps.
  Each run sends at most 30 requests for the reading, the first load and long
  commit, check and file lists, and below 1,500 points left it reads only the
  changed lists.
  When GitHub cannot finish a query (a server error, or an error that names
  a timeout), it asks again for a smaller page, or splits the batch in two.
  If GitHub rate-limits one of its requests, fetching
  pauses as after a rate limit of its own. `craft-fetch --people` runs one
  such update and prints what it read; quit the app before running it,
  because both write the same record files. `craft-fetch --states` prints the
  work states of the kept records without a request.
- Two caches in `~/Library/Caches/craft-status/` survive restarts:
  - `raw/<owner>@<repo>.json`: each repo's commit authors and issues from the
    last week (or since the latest build). Each poll fetches only what's new
    since the previous one, and refetches from scratch every six hours to drop
    deleted issues and pick up late-dated commits.
  - `snapshot.json`: what the window shows, saved after each repo. On launch
    the window shows it at once, marked `STALE` if it's old. Repos whose
    data is still fresh aren't refetched at launch.
  - `prs/<owner>@<repo>.json`: each repo's pull request records for the
    People tab, saved after each stage of an update; `roster.json` keeps a
    downloaded roster for an hour. People data stays out of `snapshot.json`.
- A third cache, `~/Library/Caches/craft-github-cache/`, is shared by this
  polling and the PR tab's. Everything in it can be deleted.
  - `prs/<owner>@<repo>.json`: a ledger of each repo's PRs, which is where the
    Downloads tab's PR counts come from. One walk, most recently updated first,
    serves both, and a ledger either of them walked in the last 5 minutes isn't
    walked again. A repo's first ledger is seeded from what its `raw/` file held about
    PRs, so upgrading doesn't refetch the last week.
  - `lease.lock`: a file lock held while talking to GitHub, so the pollers take
    turns and can't overwrite each other's updates. The status poller waits up to
    3 minutes for it, then goes ahead anyway; the PR poller gives way. It releases
    if the app dies.
  - `budget.json`: the last rate-limit reading and any backoff. When GitHub
    rate-limits either poller, both pause.
- The GitHub token comes from the first of `$CRAFT_STATUS_GITHUB_TOKEN`,
  `$GITHUB_TOKEN` and `$GH_TOKEN` that is set, then from `github_token` in the
  config, then from `gh auth token`. The app is started by launchd, not a
  shell, so it sees variables set with `launchctl setenv` (then relaunch it),
  not those exported in your shell profile.

## Tray and window

The app behaves like the Todo app:

- It lives in the menu bar, with no Dock icon.
- Left-click the tray icon to show or hide the window. Right-click it to get
  Refresh Now, Always on Top, Edit Config… and Quit.
- `⌘⇧G` shows or hides the window from anywhere.
- The window is chrome-less and semi-transparent, and it appears on every
  Space. It remembers its position and size in `~/.craft_status_state.toml`.
- `Esc` hides the window (on the PRs tab, once it's back at the top).
- `⌘4` switches to the People tab.
- In the People tab, `⌘F` moves to the search box. There `Esc` clears the
  search when there is one, then leaves the search box, and then hides the
  window.

## Configuration: `~/.craft_status_config.toml`

The app creates this file from
[`craft_status_config.template.toml`](craft_status_config.template.toml) the
first time it launches. It re-reads the file before each poll, so edits to the
repo list or the intervals take effect without a restart. PR review's settings are
its `[pr_review]` section, and all of them are optional: a file from before PR review
moved in just gets the defaults. `~/.craft_pr_review_config.toml` is no longer read,
so copy any changes you made there into that section (minus `[window]`, `[tray]` and
`[shortcuts]`, which the main ones cover).

The `[people]` table sets up the People tab:

- `roster` (default empty): the team roster, as the `https://` URL of a
  sheet's CSV export or a CSV file path. Columns are found by their header:
  `GitHub` for the login, `Discord` with `user` or `handle`, the first other
  column with `name` for the display name, `time zone` or `timezone`, and
  `focus`. The app reads it at runtime; a downloaded copy is kept for an hour.
- `owner_logins` (default none): the owners of every repo that
  `[people.repo_owners]` does not list; a pull request held for the owner
  waits on them. Without owners, a hold waits on a login named after "owner"
  in the hold's sentence.
- `[people.repo_owners]`: owners by repo (`"owner/name" = ["login"]`), in
  place of `owner_logins`. An empty list means no owners.
- `maintainers` (default none): logins counted as maintainers of every repo,
  besides the ones found from membership, merges and closes.
- `rebase_repos`: repos whose maintainers rebase contributors' branches
  themselves.
- `batch_repos`: repos that land work through a batch branch; their merge and
  landed counts are marked unverified.

## Build, install, test

Prerequisites:

- Rust stable
- Node.js 24+, then `npm ci` (the UI's TypeScript compiler and test DOM)
- `cargo install tauri-cli --version "^2"`
- Xcode CLT on macOS
- `gh auth login`, or a token in the config
- Claude Code, for the PR tab's Claude features

```sh
make install    # release .app → /Applications/Craft Status.app, start at login, launch
make uninstall  # quit, remove the app and the login item (keeps config + cache)
make run        # debug binary
make test       # Rust + UI tests
make check      # clippy + rustfmt + tsc type-check
make snapshot   # fetch once, print a table, write ui/dev-snapshot.json
make dump       # write the cached PR views to ui/dev-data.json, fetching nothing
make preview    # serve ui/ in a browser against both (no Tauri needed)
```

To start the app at login, `make install` adds
`~/Library/LaunchAgents/ai.storyteller.craft-status.plist`. Don't run a separate Craft
PR Review alongside it: both would write to the same PR cache and spend the same quota.
