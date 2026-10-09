Craft Repo Status App
=====================

Find out the status of the craft apps: a tray dashboard ("Craft Status") that
polls the storytold `*craft` repositories on GitHub every 5 minutes, keeps a
cache, and tells you what to fix first. Everything runs locally: no server.

- **Rust workspace**
  - `craft-core`: the model, release/download accounting, the urgency
    heuristic, config, cache and window state. No network or UI dependencies,
    and it is unit-tested.
  - `craft-github`: the GitHub GraphQL/REST fetcher, plus a `craft-fetch` CLI
    that prints a table, for debugging.
  - `craft-status-app`: the Tauri 2 shell (poll thread, tray, global
    shortcut, window).
- **Plain HTML/CSS/ES modules** in `ui/`. There is no bundler and no Node at
  build time. `ui/model.js` is pure and is tested with `node --test`.

## What it shows

Every column sorts. Click a header; click it again to reverse the order. Repos
with no data always sort last.

| Group | Columns |
| --- | --- |
| Fix first | **Urgency** (the sum of the top five open-issue scores) · **Critical** (open issues naming a crash, hang, freeze, data loss, launch failure or security problem) |
| Open | open PRs · open issues · oldest open PR |
| Latest | last commit to `main` · newest issue · newest PR |
| Recent (dropdown) | commits to `main` · PRs opened · PRs merged · issues opened |
| All time | commits on `main` · contributors · issues (open + closed) · PRs (open + closed + merged) |
| Builds | most recent release tag · its publish date · its downloads · downloads across all releases |

- **Recent** activity covers the window picked in that group's header: the
  last 10 min, 30 min, hour, 4 h (the default), 12 h, day or week. Every poll
  fetches all seven windows, so switching is instant; the choice is remembered.
  The numbers are as of the last poll, so short windows lag by up to
  `poll_minutes`.
- **Builds** are GitHub releases. Drafts are ignored. Prereleases count, and
  they carry a `PRE` tag.
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

- The app polls every `poll_minutes` (default 5), four repos at a time. Each
  repo costs about three GraphQL requests and one REST request: roughly 50 of
  the 5,000 GraphQL points GitHub allows per hour per cycle.
- If fewer than 150 points remain, polling slows to every 30 minutes. If
  GitHub rate-limits the app, it waits three intervals. If every repo fails
  (offline, or just woke from sleep), it retries after one minute.
- Timing follows the wall clock, so a laptop that wakes from sleep polls
  within about 15 s.
- `⌘R` or the ⟳ button refreshes immediately.
- The last snapshot is cached in `~/Library/Caches/craft-status/snapshot.json`.
  On launch the window shows the cached data at once, marked `STALE` if it's
  old. If the cache is less than one interval old, the app skips the poll it
  would otherwise run at launch.
- The GitHub token comes from `github_token` in the config. If that is empty,
  it comes from `$GITHUB_TOKEN` or `$GH_TOKEN`, and if those are unset, from
  `gh auth token`.

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
- `cargo install tauri-cli --version "^2"`
- Xcode CLT on macOS
- `gh auth login`, or a token in the config

```sh
make install    # release .app → /Applications/Craft Status.app, start at login, launch
make uninstall  # quit, remove the app and the login item (keeps config + cache)
make run        # debug binary
make test       # Rust + UI unit tests
make check      # clippy + rustfmt
make snapshot   # fetch once, print a table, write ui/dev-snapshot.json
make preview    # serve ui/ in a browser against that snapshot (no Tauri needed)
```

To start the app at login, `make install` adds
`~/Library/LaunchAgents/ai.storyteller.craft-status.plist`.
