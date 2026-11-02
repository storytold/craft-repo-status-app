// Pure helpers: columns, sorting, formatting, staleness. No DOM, so
// `node --test` can exercise them (see model.test.ts).

// ───────────────────────────── data ─────────────────────────────
// What the Rust poller sends: mirrors crates/craft-core/src/model.rs.
// Timestamps are RFC 3339 strings.

export type Time = string;

export interface Activity {
  commits: number;
  prs_opened: number;
  prs_merged: number;
  issues_opened: number;
  people: number;
}

export interface BuildLag {
  commits: number;
  basis: 'exact' | 'branched' | 'by_date';
  from_sha: string | null;
}

export interface UrgentIssue {
  number: number;
  title: string;
  url: string;
  created_at: Time | null;
  updated_at: Time | null;
  comments: number;
  reactions: number;
  labels: string[];
  score: number;
  critical: boolean;
  reasons: string[];
}

export interface RepoStats {
  open_prs: number;
  open_issues: number;
  last_commit_at: Time | null;
  last_issue_at: Time | null;
  oldest_open_pr_at: Time | null;
  newest_pr_at: Time | null;
  recent: Record<string, Activity>;
  latest_build: string | null;
  latest_build_at: Time | null;
  latest_build_prerelease: boolean;
  latest_build_sha: string | null;
  since_build: BuildLag | null;
  latest_build_downloads: number;
  downloads_total: number;
  urgency: number;
  critical_issues: number;
  urgent: UrgentIssue[];
  icon_oid: string | null;
}

export type RepoCategory = 'app' | 'library' | 'assets' | 'ci' | 'tooling' | 'other';

/** The Type column's words; a repo that isn't in the app's own list has none. */
export const CATEGORY_LABEL: Readonly<Record<RepoCategory, string | null>> = {
  app: 'App',
  library: 'Library',
  assets: 'Assets',
  ci: 'CI',
  tooling: 'Tooling',
  other: null,
};

export interface RepoEntry {
  repo: string;
  category?: RepoCategory;
  stats: RepoStats | null;
  fetched_at: Time | null;
  last_attempt_at?: Time | null;
  error?: string | null;
  inaccessible?: boolean;
  icon?: string | null;
  icon_oid?: string | null;
}

export interface Snapshot {
  repos: RepoEntry[];
  last_full_refresh_at: Time | null;
  refreshing: boolean;
  next_refresh_at: Time | null;
  poll_secs: number;
  stale_after_secs: number;
  rate_limit_remaining: number | null;
  error: string | null;
  /** The token the last poll used; null (or absent, from an older build) before the first poll. */
  token?: TokenStatus | null;
  /** The repo being fetched right now (absent from older builds). */
  fetching?: string | null;
  /** Repos refreshed by hand from their row, waiting their turn. */
  requested?: string[];
}

export interface TokenStatus {
  /** `$GITHUB_TOKEN`, `config`, `gh CLI`…; null when no token was found. */
  source: string | null;
  /** GitHub answered 401. */
  rejected: boolean;
}

/** Window settings and facts from `get_meta` in crates/craft-status-app/src/lib.rs. */
export interface Meta {
  config_path?: string;
  cache_path?: string;
  opacity?: number;
  always_on_top?: boolean;
  platform?: string;
  version?: string;
  shortcut?: string;
  frequent_over?: number;
  high_quality?: number;
}

// ───────────────────────────── settings ─────────────────────────────
// ~/.craft_status_config.toml as the Settings view edits it: mirrors `Config`
// in crates/craft-core/src/config.rs, and its limits mirror that file's
// constants and `Config::problems`.

export interface Config {
  repos: string[];
  poll_minutes: number;
  stale_minutes: number;
  github_token: string;
  window: { width: number; height: number; always_on_top: boolean; start_hidden: boolean; opacity: number };
  tray: { hide_dock_icon: boolean; visible_on_all_workspaces: boolean; close_to_tray: boolean };
  shortcuts: { toggle_window: string };
}

/** `get_settings`'s answer: the config with its token left out, and whether the file has one. */
export interface Settings {
  config: Config;
  token_set: boolean;
  path: string;
}

export type SettingKind = 'number' | 'bool' | 'text' | 'secret';

/** One field of the Settings view. The view draws `SETTING_FIELDS` in order, a heading before each new section. */
export interface SettingField {
  /** The field's place in `Config`, dotted: `window.opacity`. */
  key: string;
  section: string;
  label: string;
  kind: SettingKind;
  min?: number;
  max?: number;
  step?: number;
  help?: string;
  /** The app applies it only when it is next launched. */
  relaunch?: boolean;
}

/** The Settings view's fields besides the repo list, which it draws above them. */
export const SETTING_FIELDS: readonly SettingField[] = [
  { key: 'poll_minutes', section: 'Polling', label: 'Fetch each repo every (minutes)', kind: 'number', min: 1, max: 1440, step: 1, help: 'How old a repo’s data gets before it’s refetched. Repos are fetched one at a time, 20 s apart.' },
  { key: 'stale_minutes', section: 'Polling', label: 'Stale after (minutes)', kind: 'number', min: 1, max: 10080, step: 1, help: 'Older data is flagged STALE. It must be at least the fetch interval.' },
  { key: 'github_token', section: 'GitHub', label: 'Token', kind: 'secret', help: 'Used only if $CRAFT_STATUS_GITHUB_TOKEN, $GITHUB_TOKEN and $GH_TOKEN are all unset. With no token saved, the app uses `gh auth token`.' },
  { key: 'window.width', section: 'Window', label: 'Width', kind: 'number', min: 520, max: 10000, step: 1, relaunch: true, help: 'Used when no window position is remembered.' },
  { key: 'window.height', section: 'Window', label: 'Height', kind: 'number', min: 240, max: 10000, step: 1, relaunch: true },
  { key: 'window.opacity', section: 'Window', label: 'Opacity', kind: 'number', min: 0.2, max: 1, step: 0.01 },
  { key: 'window.always_on_top', section: 'Window', label: 'Always on top', kind: 'bool' },
  { key: 'window.start_hidden', section: 'Window', label: 'Start hidden', kind: 'bool', relaunch: true },
  { key: 'tray.hide_dock_icon', section: 'Tray', label: 'No Dock icon (macOS)', kind: 'bool', relaunch: true },
  { key: 'tray.visible_on_all_workspaces', section: 'Tray', label: 'Show the window on every desktop', kind: 'bool' },
  { key: 'tray.close_to_tray', section: 'Tray', label: 'Closing the window hides it', kind: 'bool' },
  { key: 'shortcuts.toggle_window', section: 'Shortcut', label: 'Show or hide the window', kind: 'text', help: 'Keys joined by +, such as CmdOrCtrl+Shift+G. Leave it empty to turn the shortcut off.' },
];

/** The value at a dotted `key` of `cfg`. */
export function getSetting(cfg: object, key: string): unknown {
  return key.split('.').reduce<unknown>((o, k) => (o && typeof o === 'object' ? (o as Record<string, unknown>)[k] : undefined), cfg);
}

/** A copy of `cfg` with the value at a dotted `key` replaced; `cfg` is left alone. */
export function withSetting<T extends object>(cfg: T, key: string, v: unknown): T {
  const [head, ...rest] = key.split('.');
  const o = cfg as Record<string, unknown>;
  if (!rest.length) return { ...cfg, [head!]: v };
  const inner = o[head!];
  const nested = inner && typeof inner === 'object' ? inner : {};
  return { ...cfg, [head!]: withSetting(nested, rest.join('.'), v) };
}

/** `https://github.com/o/r(.git)(/…)` or `o/r` → `o/r`, or null; as `normalize_repo` in config.rs. */
export function normalizeRepo(s: string): string | null {
  const t = s.trim().replace(/^(?:https?:\/\/)?github\.com\//, '');
  const parts = t.split('/').filter(Boolean);
  if (parts.length < 2) return null;
  const owner = parts[0]!;
  const name = parts[1]!.replace(/(\.git)+$/, '');
  const ok = (p: string) => /^[A-Za-z0-9._-]+$/.test(p);
  return ok(owner) && ok(name) ? `${owner}/${name}` : null;
}

/** Why `input` cannot be added to `repos`, or null when it can. */
export function repoProblem(input: string, repos: readonly string[]): string | null {
  const n = normalizeRepo(input);
  if (!n) return 'Use owner/name or a GitHub URL.';
  const lower = n.toLowerCase();
  return repos.some((r) => normalizeRepo(r)?.toLowerCase() === lower) ? `${n} is already listed.` : null;
}

/** What keeps `cfg` from being saved, by field key (`repos` for the list); empty when it can be. */
export function settingsProblems(cfg: Config): Record<string, string> {
  const out: Record<string, string> = {};
  const seen: string[] = [];
  for (const r of cfg.repos) {
    const n = normalizeRepo(r);
    if (!n) { out.repos ??= `${r} is not owner/name or a GitHub URL.`; continue; }
    if (seen.includes(n.toLowerCase())) out.repos ??= `${n} is listed twice.`;
    seen.push(n.toLowerCase());
  }
  for (const f of SETTING_FIELDS) {
    if (f.kind !== 'number') continue;
    const v = getSetting(cfg, f.key);
    if (typeof v !== 'number' || !Number.isFinite(v)) out[f.key] = 'Enter a number.';
    else if (v < (f.min ?? -Infinity) || v > (f.max ?? Infinity)) out[f.key] = `Enter a number from ${f.min} to ${f.max}.`;
  }
  if (!out.stale_minutes && !out.poll_minutes && cfg.stale_minutes < cfg.poll_minutes) {
    out.stale_minutes = 'Enter at least the fetch interval.';
  }
  const accel = cfg.shortcuts.toggle_window.trim();
  if (accel !== '' && accel.split('+').some((k) => !k.trim())) out['shortcuts.toggle_window'] = 'Join the keys with single + signs, such as CmdOrCtrl+Shift+G.';
  return out;
}

/** The labels of the relaunch-only fields whose values differ between two configs. */
export function relaunchChanges(before: Config, after: Config): string[] {
  return SETTING_FIELDS.filter((f) => f.relaunch && getSetting(before, f.key) !== getSetting(after, f.key)).map((f) => f.label);
}

/** The Settings view's line about the token the last fetch used; empty before the first fetch. */
export function tokenSourceNote(token: TokenStatus | null | undefined): string {
  if (!token) return '';
  if (!token.source) return 'The last fetch found no token.';
  let used = token.source;
  if (token.source === 'config') used = 'the token saved here';
  else if (token.source === 'gh CLI') used = '`gh auth token`';
  const note = `The last fetch used ${used}${token.rejected ? ', and GitHub rejected it' : ''}.`;
  return token.source.startsWith('$') ? `${note} A token saved here is not used while ${token.source} is set.` : note;
}

// ───────────────────────────── windows ─────────────────────────────

export interface ActivityWindow {
  id: string;
  label: string;
  span: string;
}

/**
 * The activity windows the dropdown offers, shortest first, then since each
 * repo's latest release (so a different span per repo; repos without a release
 * have no data), then all time. The ids match `WINDOWS`, `SINCE_RELEASE` and
 * `ALL_TIME` in crates/craft-core/src/model.rs; every poll fetches all of them.
 */
export const WINDOWS: readonly ActivityWindow[] = [
  { id: '10m', label: 'Last 10 min', span: 'in the last 10 minutes' },
  { id: '30m', label: 'Last 30 min', span: 'in the last 30 minutes' },
  { id: '1h', label: 'Last hour', span: 'in the last hour' },
  { id: '4h', label: 'Last 4 h', span: 'in the last 4 hours' },
  { id: '12h', label: 'Last 12 h', span: 'in the last 12 hours' },
  { id: '1d', label: 'Last day', span: 'in the last day' },
  { id: '7d', label: 'Last week', span: 'in the last week' },
  { id: 'release', label: 'Since last release', span: 'since each repo’s latest release' },
  { id: 'all', label: 'All time', span: 'over all time' },
];
export const ALL_TIME = 'all';
export const DEFAULT_WINDOW = '4h';
export const windowById = (id: string): ActivityWindow =>
  WINDOWS.find((w) => w.id === id) ?? windowById(DEFAULT_WINDOW);

// ───────────────────────────── columns ─────────────────────────────

/**
 * `age` is a timestamp shown and sorted by how long ago it was: ascending puts
 * the newest first. `rate` is a number per second: it sorts like `num`, whatever
 * unit it displays in, but has no footer total.
 */
export type Kind = 'text' | 'num' | 'rate' | 'date' | 'age' | 'version';
export type Value = string | number | null;

export interface Column {
  key: string;
  label: string;
  kind: Kind;
  group: string;
  title?: string;
  /** Sorts ascending when first clicked, though not text. */
  asc?: boolean;
  /** The field of the selected window's `Activity` this column shows. */
  recent?: keyof Activity;
  /** In the activity group, but only has a value when that dropdown says All time. */
  allTimeOnly?: boolean;
  get?: (e: RepoEntry) => Value;
}

/**
 * Table columns, in display order. `get` reads from a RepoEntry; numbers and
 * dates sort descending first (biggest / newest on top), text ascending.
 * Columns with `recent` read that field of the selected window's activity;
 * their group is `RECENT`, which the table draws as the window dropdown, and
 * `{span}` in their titles names the window.
 */
export const RECENT = 'recent';

/**
 * Downloads per second of the latest build, from its release to the fetch that
 * counted them (not to now, which would sag the rate between polls). `null`
 * without a build, or when the clocks leave it no time to measure.
 */
export function buildRate(entry: RepoEntry): number | null {
  const s = entry.stats;
  if (!s?.latest_build_at || !entry.fetched_at || typeof s.latest_build_downloads !== 'number') return null;
  const secs = (Date.parse(entry.fetched_at) - Date.parse(s.latest_build_at)) / 1000;
  return secs > 0 ? s.latest_build_downloads / secs : null;
}

export const COLUMNS: readonly Column[] = [
  { key: 'name', label: 'Repo', kind: 'text', group: '', get: (e) => e.repo.split('/').pop() ?? e.repo },
  { key: 'fetched', label: 'Fetched', kind: 'age', group: '', title: 'Time since this repo’s data was last fetched in full (failed polls keep the older time)', get: (e) => e.fetched_at ?? null },
  { key: 'category', label: 'Type', kind: 'text', group: '', title: 'What the repo is: app, library, assets, CI or tooling', get: (e) => CATEGORY_LABEL[e.category ?? 'other'] },
  { key: 'latest_build', label: 'Build', kind: 'version', group: 'Downloads', title: 'Most recent published release' },
  { key: 'latest_build_at', label: 'Built', kind: 'date', group: 'Downloads', title: 'When the most recent release was published' },
  { key: 'latest_build_rate', label: 'Build DL Rate', kind: 'rate', group: 'Downloads', title: 'Build DLs divided by how long the most recent release had been out when they were counted', get: buildRate },
  { key: 'latest_build_downloads', label: 'Build DLs', kind: 'num', group: 'Downloads', title: 'Downloads of the most recent release, all OSes and package types (checksums and update deltas excluded)' },
  { key: 'commits_since_build', label: 'Unreleased', kind: 'num', group: 'Downloads', title: 'Commits on main since the most recent release (~ means estimated: the release’s commit isn’t on main)', get: (e) => e.stats?.since_build?.commits ?? null },
  { key: 'urgency', label: 'Urgency', kind: 'num', group: 'Fix first', title: 'Sum of the top five open-issue urgency scores' },
  { key: 'critical_issues', label: 'Critical', kind: 'num', group: 'Fix first', title: 'Open issues that mention a crash, hang, freeze, data loss, launch failure or security problem' },
  { key: 'open_prs', label: 'PRs', kind: 'num', group: 'Open', title: 'Open pull requests' },
  { key: 'open_issues', label: 'Issues', kind: 'num', group: 'Open', title: 'Open issues' },
  { key: 'oldest_open_pr_at', label: 'Oldest PR', kind: 'date', group: 'Open', title: 'Oldest pull request still open', asc: true },
  { key: 'last_commit_at', label: 'Last commit', kind: 'date', group: 'Latest', title: 'Last commit on main' },
  { key: 'last_issue_at', label: 'Last issue', kind: 'date', group: 'Latest', title: 'Newest issue (any state)' },
  { key: 'newest_pr_at', label: 'Newest PR', kind: 'date', group: 'Latest', title: 'Newest pull request (any state)' },
  { key: 'recent_commits', recent: 'commits', label: 'Commits', kind: 'num', group: RECENT, title: 'Commits to main {span}' },
  { key: 'recent_prs', recent: 'prs_opened', label: 'PRs', kind: 'num', group: RECENT, title: 'Pull requests opened {span}' },
  { key: 'recent_merged', recent: 'prs_merged', label: 'Merged', kind: 'num', group: RECENT, title: 'Pull requests merged {span}' },
  { key: 'recent_issues', recent: 'issues_opened', label: 'Issues', kind: 'num', group: RECENT, title: 'Issues opened {span}' },
  { key: 'recent_people', recent: 'people', label: 'People', kind: 'num', group: RECENT, title: 'People who committed to main {span} (all time: GitHub’s contributor count)' },
  { key: 'downloads_total', label: 'Total DLs', kind: 'num', group: RECENT, allTimeOnly: true, title: 'Downloads across every published release (only known for All time)' },
];

export const COLUMN: Readonly<Record<string, Column>> = Object.fromEntries(COLUMNS.map((c) => [c.key, c]));

/**
 * The value a column shows for an entry; `null` when there is no data
 * (including a cache written before that window existed).
 */
export function value(entry: RepoEntry, key: string, window: string = DEFAULT_WINDOW): Value {
  const col = COLUMN[key];
  if (col?.allTimeOnly && window !== ALL_TIME) return null;
  if (col?.get) return col.get(entry);
  if (col?.recent) return entry.stats?.recent?.[window]?.[col.recent] ?? null;
  const v = entry.stats ? (entry.stats as unknown as Record<string, unknown>)[key] : null;
  return typeof v === 'string' || typeof v === 'number' ? v : null;
}

// ───────────────────────────── sorting ─────────────────────────────

export type Dir = 'asc' | 'desc';

/** The table's order: by a column, or `MANUAL` (the rows as last dragged). */
export type Sort = { key: string; dir?: Dir };

/** `v0.10.2-rc.1` → [0, 10, 2, -1, 1] so pre-releases sort below the release. */
export function versionKey(tag: string): number[] {
  const m = String(tag).replace(/^v/i, '').match(/^(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-([A-Za-z]*)\.?(\d+)?)?/);
  if (!m) return [-1];
  return [+m[1]!, +(m[2] ?? 0), +(m[3] ?? 0), m[4] !== undefined ? -1 : 0, +(m[5] ?? 0)];
}

function cmpArrays(a: number[], b: number[]): number {
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const d = (a[i] ?? 0) - (b[i] ?? 0);
    if (d) return d;
  }
  return 0;
}

function cmpValues(kind: Kind, a: string | number, b: string | number): number {
  switch (kind) {
    case 'text': return String(a).localeCompare(String(b), undefined, { sensitivity: 'base', numeric: true });
    case 'date': return Date.parse(String(a)) - Date.parse(String(b));
    case 'age': return Date.parse(String(b)) - Date.parse(String(a));
    case 'version': return cmpArrays(versionKey(String(a)), versionKey(String(b)));
    default: return Number(a) - Number(b);
  }
}

const byName = (a: RepoEntry, b: RepoEntry): number => cmpValues('text', value(a, 'name') ?? '', value(b, 'name') ?? '');

/** The direction a column sorts in when first clicked. */
export function defaultDir(key: string): Dir {
  const col = COLUMN[key];
  return col?.kind === 'text' || col?.asc ? 'asc' : 'desc';
}

/** Sorted copy. Missing values always sink to the bottom; ties fall back to name. */
export function sortEntries(entries: readonly RepoEntry[], key: string, dir: Dir | undefined, window: string = DEFAULT_WINDOW): RepoEntry[] {
  const col = COLUMN[key] ?? COLUMN.urgency!;
  const sign = dir === 'asc' ? 1 : -1;
  return [...entries].sort((a, b) => {
    const va = value(a, col.key, window);
    const vb = value(b, col.key, window);
    if (va === null && vb === null) return byName(a, b);
    if (va === null) return 1;
    if (vb === null) return -1;
    return sign * cmpValues(col.kind, va, vb) || byName(a, b);
  });
}

/** The `sort.key` of a hand-arranged table: rows follow a saved list of repos. */
export const MANUAL = 'manual';

/** Entries in the order of `order` (repo names); repos missing from it keep config order, after the rest. */
export function manualOrder(entries: readonly RepoEntry[], order: readonly string[]): RepoEntry[] {
  const rank = new Map(order.map((repo, i) => [repo, i]));
  const key = (e: RepoEntry, i: number) => rank.get(e.repo) ?? order.length + i;
  return entries.map((e, i) => [key(e, i), e] as const).sort((a, b) => a[0] - b[0]).map(([, e]) => e);
}

/** The table's rows for a sort, manual or by column. */
export function orderedEntries(entries: readonly RepoEntry[], sort: Sort, order: readonly string[], window: string = DEFAULT_WINDOW): RepoEntry[] {
  return sort.key === MANUAL ? manualOrder(entries, order) : sortEntries(entries, sort.key, sort.dir, window);
}

/** The sort after clicking a column header: flip it if already sorted by it, else its default direction. */
export function nextSort(sort: Sort, key: string): Sort {
  return sort.key === key
    ? { key, dir: sort.dir === 'asc' ? 'desc' : 'asc' }
    : { key, dir: defaultDir(key) };
}

/**
 * Where a dragged row goes: the index, among the other rows, of the first row
 * whose vertical midpoint is below the pointer (`mids.length` for the end).
 */
export function dropIndex(mids: readonly number[], y: number): number {
  const i = mids.findIndex((m) => y < m);
  return i === -1 ? mids.length : i;
}

// ───────────────────────────── ownership ─────────────────────────────

/** GitHub owners whose repos are our own apps; anything else is third party. */
export const FIRST_PARTY_OWNERS: readonly string[] = ['storytold'];

/** `owner/name` belongs to someone outside FIRST_PARTY_OWNERS. */
export function isThirdParty(repo: string): boolean {
  const owner = repo.split('/')[0]?.toLowerCase() ?? '';
  return !FIRST_PARTY_OWNERS.includes(owner);
}

// ───────────────────────────── which repos show ─────────────────────────────

/** A kind of repo the toggles beside the tabs can hide. */
export interface RepoKind {
  id: string;
  label: string;
  includes(repo: string, category: RepoCategory | undefined): boolean;
}

const ofCategory = (category: RepoCategory) => (_: string, c: RepoCategory | undefined): boolean => c === category;

export const REPO_KINDS: readonly RepoKind[] = [
  { id: 'third_party', label: '3rd party', includes: (repo) => isThirdParty(repo) },
  { id: 'app', label: 'Apps', includes: ofCategory('app') },
  { id: 'library', label: 'Libraries', includes: ofCategory('library') },
  { id: 'assets', label: 'Assets', includes: ofCategory('assets') },
  { id: 'ci', label: 'CI', includes: ofCategory('ci') },
  { id: 'tooling', label: 'Tooling', includes: ofCategory('tooling') },
];

/** Whether any kind this repo is has been hidden: a third-party app goes with either toggle. */
export function isRepoHidden(repo: string, category: RepoCategory | undefined, hidden: readonly string[]): boolean {
  return REPO_KINDS.some((k) => hidden.includes(k.id) && k.includes(repo, category));
}

/** `hidden` with `id` flipped. */
export function toggleKind(hidden: readonly string[], id: string): string[] {
  return hidden.includes(id) ? hidden.filter((k) => k !== id) : [...hidden, id];
}

/**
 * The manual order after dragging among the visible rows: hidden repos keep their
 * places in `all` (the previous order), and the visible ones fill the rest as dragged.
 */
export function reorderVisible(all: readonly string[], visible: readonly string[]): string[] {
  const shown = new Set(visible);
  const next = visible[Symbol.iterator]();
  return all.map((repo) => (shown.has(repo) ? next.next().value ?? repo : repo));
}

// ───────────────────────────── freshness ─────────────────────────────

export function isStale(entry: RepoEntry, staleAfterSecs: number, now: number = Date.now()): boolean {
  if (!entry.fetched_at) return true;
  return now - Date.parse(entry.fetched_at) > staleAfterSecs * 1000;
}

/** `{ oldest, stale }` for the whole snapshot: the age of the oldest data on screen. */
export function freshness(snap: Pick<Snapshot, 'repos' | 'stale_after_secs'>, now: number = Date.now()): { oldest: number | null; stale: boolean } {
  const times = snap.repos.map((r) => (r.fetched_at ? Date.parse(r.fetched_at) : null));
  const oldest = times.length && times.every((t) => t !== null) ? Math.min(...(times as number[])) : null;
  const stale = !snap.repos.length || snap.repos.some((r) => isStale(r, snap.stale_after_secs, now));
  return { oldest, stale };
}

/** A row's own refresh is queued or under way, so its button spins. Names
 * match in any letter case, as in `Snapshot::entry`. */
export function isRefreshing(snap: Pick<Snapshot, 'fetching' | 'requested'>, repo: string): boolean {
  const same = (r: string | null | undefined) => r?.toLowerCase() === repo.toLowerCase();
  return same(snap.fetching) || (snap.requested ?? []).some(same);
}

/** The footer's account of the GitHub token: `bad` when polling can't work. */
export function tokenStatus(token: TokenStatus | null | undefined): { text: string; title: string; bad: boolean } {
  if (!token) return { text: '', title: '', bad: false };
  if (!token.source) {
    return {
      text: 'No GitHub token',
      title: "GitHub's GraphQL API needs a token: set CRAFT_STATUS_GITHUB_TOKEN (launchctl setenv), github_token in the config, or run `gh auth login`",
      bad: true,
    };
  }
  if (token.rejected) {
    return { text: `Token rejected (${token.source})`, title: `GitHub answered 401 to the token from ${token.source}: replace it or unset it`, bad: true };
  }
  return { text: `Token: ${token.source}`, title: `Polling GitHub with the token from ${token.source}`, bad: false };
}

// ───────────────────────────── formatting ─────────────────────────────

/** "now", "45s", "12m", "3h", "5d", "2y" — compact for table cells. */
export function ago(iso: Time | null | undefined, now: number = Date.now()): string {
  if (!iso) return '';
  const s = Math.max(0, (now - Date.parse(iso)) / 1000);
  if (s < 10) return 'now';
  if (s < 60) return `${Math.floor(s)}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  if (s < 86400 * 365) return `${Math.floor(s / 86400)}d`;
  return `${Math.floor(s / (86400 * 365))}y`;
}

export function compact(n: number | null | undefined): string {
  if (n === null || n === undefined) return '';
  if (n < 10000) return n.toLocaleString('en-US');
  if (n < 1e6) return `${(n / 1000).toFixed(n < 1e5 ? 1 : 0)}k`;
  return `${(n / 1e6).toFixed(1)}M`;
}

/** A per-second rate in the largest unit that keeps it at 1 or more: "1.7/s", "5.6/m", "3.2/h", "0.4/d". */
export function formatRate(perSec: number): string {
  if (perSec >= 1) return `${perSec.toFixed(1)}/s`;
  if (perSec * 60 >= 1) return `${(perSec * 60).toFixed(1)}/m`;
  if (perSec * 3600 >= 1) return `${(perSec * 3600).toFixed(1)}/h`;
  return `${(perSec * 86400).toFixed(1)}/d`;
}

/** Column totals for the footer row (counts only; rates, dates and versions blank). */
export function totals(entries: readonly RepoEntry[], window: string = DEFAULT_WINDOW): Record<string, number> {
  const out: Record<string, number> = {};
  for (const c of COLUMNS) {
    if (c.kind !== 'num') continue;
    out[c.key] = entries.reduce((sum, e) => sum + Number(value(e, c.key, window) ?? 0), 0);
  }
  return out;
}

export type RepoUrgentIssue = UrgentIssue & { repo: string };

/** Every repo's urgent issues merged, best first. */
export function urgentAcross(
  entries: readonly RepoEntry[],
  { repo = '', criticalOnly = false, limit = 60 }: { repo?: string; criticalOnly?: boolean; limit?: number } = {},
): RepoUrgentIssue[] {
  const all: RepoUrgentIssue[] = [];
  for (const e of entries) {
    if (repo && e.repo !== repo) continue;
    for (const u of e.stats?.urgent ?? []) {
      if (criticalOnly && !u.critical) continue;
      all.push({ ...u, repo: e.repo });
    }
  }
  all.sort((a, b) => b.score - a.score || Date.parse(b.created_at ?? '0') - Date.parse(a.created_at ?? '0'));
  return all.slice(0, limit);
}
