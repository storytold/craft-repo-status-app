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
| Activity (dropdown) | commits to `main` · PRs opened · PRs merged · issues opened · people committing to `main` |
| Builds | most recent release tag · its publish date · **Unreleased** (commits on `main` since it) · its downloads · downloads across all releases |

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
- **Fix first** (`⌘2`) ranks open issues across every repo. Each issue shows
  the reasons behind its score. Click a row in the Repos table to filter the
  list to that repo. Click an issue to open it on GitHub.
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
- Two caches in `~/Library/Caches/craft-status/` survive restarts:
  - `raw/<owner>@<repo>.json`: each repo's commit authors and issues from the
    last week (or since the latest build). Each poll fetches only what's new
    since the previous one, and refetches from scratch every six hours to drop
    deleted issues and pick up late-dated commits.
  - `snapshot.json`: what the window shows, saved after each repo. On launch
    the window shows it at once, marked `STALE` if it's old. Repos whose
    data is still fresh aren't refetched at launch.
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

## Configuration: `~/.craft_status_config.toml`

The app creates this file from
[`craft_status_config.template.toml`](craft_status_config.template.toml) the
first time it launches. It re-reads the file before each poll, so edits to the
repo list or the intervals take effect without a restart. PR review's settings are
its `[pr_review]` section, and all of them are optional: a file from before PR review
moved in just gets the defaults. `~/.craft_pr_review_config.toml` is no longer read,
so copy any changes you made there into that section (minus `[window]`, `[tray]` and
`[shortcuts]`, which the main ones cover).

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
