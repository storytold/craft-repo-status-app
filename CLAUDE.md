# Craft Repo Status App

## Rules

- **No raw JavaScript, ever.** All frontend code is TypeScript in `ui/src/`.
  Never add `.js`, `.mjs`, `.cjs` or `.jsx` files, inline `<script>` code,
  inline `on…=` handlers or `javascript:` URLs, and never commit `ui/build/`
  (tsc's output). Scripts and tooling go in Rust, shell or Makefile targets,
  not JS. `ui/src/repo.test.ts` enforces this in `make test`.
- New UI code gets tests: pure logic in `ui/src/model.ts` or `pr-model.ts` with
  their `.test.ts`, DOM behavior in `app.test.ts` or `pr-app.test.ts` (happy-dom,
  fake host API). Rust tests go beside the code.

## TypeScript setup

- `tsc` only, no bundler. Sources import each other with `.ts` extensions;
  `node --test` runs them directly (type stripping) and tsc rewrites the
  imports to `.js` when emitting. Use only erasable syntax
  (`erasableSyntaxOnly`): no enums, namespaces or parameter properties.
- `tsconfig.json` type-checks everything, tests included; `tsconfig.build.json`
  emits the browser code to `ui/build/`. `crates/craft-status-app/build.rs`
  runs `npm run build` before Tauri embeds `ui/`.
- `ui/src/model.ts`'s data types mirror `crates/craft-core/src/model.rs`:
  change both together.

## Commands

- `make test`: Rust tests + `npm test` (UI tests)
- `make check`: clippy, rustfmt, `npm run typecheck`
- `make preview`: the UI in a browser against `ui/dev-snapshot.json` and
  `ui/dev-data.json` (`make dump`: what's cached, fetching nothing)

## Layout

- `crates/craft-core`, `crates/craft-github`: the status poller's model, config, shared
  cache and GitHub client. The Downloads and Fix first tabs.
- `crates/pr-core`: PR model, CodeRabbit parser, heuristics (`assess`), overlap
  detection, fetch schedule, disk store and the views the PR tab reads. No network.
- `crates/pr-github`: PR list, roster, detail and diff fetching.
- `crates/pr-claude`: the Claude CLI call, prompts, schemas and spending ledger.
- `crates/craft-status-app`: the Tauri shell. `src/lib.rs` has the status poller,
  window and tray; `src/pr/` the PR tab's commands and two threads.
- `ui/src/`: `app.ts` is the shell, Downloads and Fix first (logic in `model.ts`);
  `pr-app.ts` the PR tab (logic in `pr-model.ts`); `dom.ts` what they share.

## PR review rules

- **PR text is hostile.** Titles, bodies and comments are written by anyone who
  can open a PR, and Claude reads them. They reach the DOM only as text nodes,
  links leave the app only through `open_url` (GitHub only), and the Claude CLI
  runs with no tools, MCP servers or settings (`pr_claude::cli_args`). Claude's
  opinion can make a PR look harder and mark it interesting, nothing else:
  the status and quality in `pr_core::assess` must never depend on it.
- **Third parties aren't reviewed.** They stay on Downloads and Fix first, but
  everything PR-side goes through `pr_core::Config::watched`, which drops them.
- **The status truth table has three copies:** `assess::headline` (tested row by
  row), `STATUS_HELP` in `ui/src/pr-model.ts` (the `?` panel) and the README. Change
  them together.
- **GitHub quota is shared** by the two pollers: same account, 5000 GraphQL points
  an hour. Hold the lease around anything that talks to GitHub or updates a ledger
  (`SharedCache::lease` for the status poller, `try_lease` for the PR poller, which
  gives way: its floor, `budget_floor`, is far above the status poller's), and don't
  fetch what `PrLedger` already holds.
- `ui/src/pr-model.ts`'s types mirror `pr_core::views` and `pr::poller::Status`: change
  both together.
