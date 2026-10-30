Craft Repo Status App
=====================

Find out the status of the craft apps: a tray dashboard ("Craft Status") that
polls the storytold `*craft` repositories on GitHub every 30 minutes, keeps a
cache, and tells you what to fix first. Everything runs locally: no server.

![Craft Status: the Repos table, sorted by urgency](docs/screenshot.png)

- **Rust workspace**
  - `craft-core`: the model, release/download accounting, the urgency
    heuristic, config, cache and window state. No network or UI dependencies,
    and it is unit-tested.
  - `craft-github`: the GitHub GraphQL/REST fetcher, plus a `craft-fetch` CLI
    that prints a table, for debugging.
  - `craft-status-app`: the Tauri 2 shell (poll thread, tray, global
    shortcut, window).
- **TypeScript, HTML and CSS** in `ui/`, with no framework or bundler. `tsc`
  compiles `ui/src/` to `ui/build/`, which `index.html` loads; the app's
  `build.rs` runs it, so building the app needs Node.js 24+ and `npm ci`.
  `ui/src/model.ts` is pure; it and the UI itself (in happy-dom) are tested
  with `node --test`. No JavaScript is committed: see `CLAUDE.md`.

## What it shows

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
| Parity | **Breadth** · **Ready**, from the repo's `ROADMAP.md` |
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
- **Parity** reads the "Headline numbers" table of each repo's `ROADMAP.md`,
  or the headline table of `docs/target-app-parity.md` when `ROADMAP.md` has
  none. Breadth is the percentage in the Feature breadth row (the row marked
  weighted when there are several). Ready is the percentage in the Ready for
  real work row. Without that row, Ready comes from the "Readiness by
  audience" table, then from the headline table of
  `docs/target-app-parity.md`, then from the Feature depth row. Hover a number
  for its row, whether the document calls it measured or estimated, and the
  file's Last reviewed date. Click it to open `docs/target-app-parity.md` on
  GitHub, or the row in `ROADMAP.md` when the repo has no such document. A
  repo without the file, the table or the row shows `—`, and hovering it says
  which is missing. A row whose value has no percentage shows `—` with an
  amber dot.
- **The Parity tab** (`⌘3`) shows each app's parity with its target app, read
  from the same two documents plus `docs/gaps.md`, the machine checklists
  (`docs/parity-checklist.md`, `docs/scorecard.md`, `parity/*.toml` and the
  documents the Feature breadth rows link to) and every `docs/*-parity.md`.
  The **Apps** view has a row per app: its stage; Ready, for the full target,
  mainstream users or essentials (picked in the header), with its range and a
  tick at Breadth; the hours to beta and to full parity; the dimension that
  loses the most weighted points; the open gaps that block beta, with their
  hours; a cell per dimension; a strip of feature areas; the checklist
  progress; the age of the oldest Last reviewed date, amber after
  `parity_review_warn_days` (default 14); and how long ago a parity document
  last changed. Click a row, or press `Enter`, to open a pane with every
  dimension, area, ranked gap, checklist and parser note. `↑` and `↓` move
  between apps, and `Esc` closes the pane. **All gaps** lists every app's open
  gaps, alpha blockers first, then beta blockers by rank, with filters for the
  app, kind, beta blockers, closed gaps, hours and text, and the gaps and hours
  per kind. The headline numbers, dimension cells, gaps, checklist rows and
  parser notes link to the line they were read from, on GitHub at the commit
  read. The tab's badge counts the open gaps that block alpha or beta.
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
- Each repo fetch also reads the blob ids of the repo's parity documents:
  `ROADMAP.md`, `docs/target-app-parity.md`, `docs/gaps.md`,
  `docs/parity-checklist.md`, `docs/scorecard.md`, every `docs/*-parity.md`
  and `parity/*.toml`, and `README.md` when there is no `ROADMAP.md`. The app
  downloads their text, in one more GraphQL request, only when one of them has
  changed since the parity numbers were read, and keeps the numbers in the
  cache. One more request reads the last commit to each document, and
  another reads the documents the Feature breadth rows link to that are not
  in that list. A repo whose Feature breadth rows link to such a document is
  read again after each new commit to its default branch. While fewer than
  1,500 points remain, it waits, and the Parity columns and tab keep their
  previous numbers.
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
  - `raw/<owner>@<repo>.json`: each repo's commit authors, PRs and issues
    from the last week (or since the latest build). Each poll fetches only
    what's new since the previous one, and refetches from scratch every six
    hours to drop deleted issues and pick up late-dated commits.
  - `snapshot.json`: what the window shows, saved after each repo. On launch
    the window shows it at once, marked `STALE` if it's old. Repos whose
    data is still fresh aren't refetched at launch.
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
- `Esc` hides the window.

## Configuration: `~/.craft_status_config.toml`

The app creates this file from
[`craft_status_config.template.toml`](craft_status_config.template.toml) the
first time it launches. It re-reads the file before each poll, so edits to the
repo list or the intervals take effect without a restart.

## Build, install, test

Prerequisites:

- Rust stable
- Node.js 24+, then `npm ci` (the UI's TypeScript compiler and test DOM)
- `cargo install tauri-cli --version "^2"`
- Xcode CLT on macOS
- `gh auth login`, or a token in the config

```sh
make install    # release .app → /Applications/Craft Status.app, start at login, launch
make uninstall  # quit, remove the app and the login item (keeps config + cache)
make run        # debug binary
make test       # Rust + UI tests
make check      # clippy + rustfmt + tsc type-check
make snapshot   # fetch once, print a table, write ui/dev-snapshot.json
make preview    # serve ui/ in a browser against that snapshot (no Tauri needed)
```

To start the app at login, `make install` adds
`~/Library/LaunchAgents/ai.storyteller.craft-status.plist`.
