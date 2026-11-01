# Craft Repo Status App

## Rules

- **No raw JavaScript, ever.** All frontend code is TypeScript in `ui/src/`.
  Never add `.js`, `.mjs`, `.cjs` or `.jsx` files, inline `<script>` code,
  inline `on…=` handlers or `javascript:` URLs, and never commit `ui/build/`
  (tsc's output). Scripts and tooling go in Rust, shell or Makefile targets,
  not JS. `ui/src/repo.test.ts` enforces this in `make test`.
- New UI code gets tests: pure logic in `ui/src/model.ts` with
  `model.test.ts`, DOM behavior in `app.test.ts` (happy-dom, fake host API).

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
- `make preview`: the UI in a browser against `ui/dev-snapshot.json`

## Shared with Craft PR Review

`../craft-pr-review-tool` depends on `craft-core` and `craft-github` by path, and
the two apps share `~/Library/Caches/craft-github-cache/`. Changes to
`craft_core::shared`, `Config`, `GitHub::sync_ledger` or `resolve_token` affect
both: build and test that repo too. Hold the lease (`SharedCache::lease`) around
anything that talks to GitHub or updates a ledger.
