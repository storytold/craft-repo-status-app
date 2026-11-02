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

/** A row's own refresh is queued or under way, so its button spins. */
export function isRefreshing(snap: Pick<Snapshot, 'fetching' | 'requested'>, repo: string): boolean {
  return snap.fetching === repo || (snap.requested ?? []).includes(repo);
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

// ───────────────────────────── people tab ─────────────────────────────
// The People tab's data: mirrors `PeopleView` and its parts in
// crates/craft-core/src/people/view.rs, and `WorkState` in states.rs. It
// comes from `get_people` and the `craft:people` event, never from the
// snapshot.

/** A count with thousands separators: 1,234. */
const formatCount = (n: number): string => n.toLocaleString('en-US');

/** Who a pull request waits on: `Role` in states.rs. */
export type Role = 'author' | 'maintainer' | 'owner' | 'ci' | 'other_pr';

/** The sentence a text rule matched. */
export interface Evidence {
  quote: string;
  author: string | null;
  at: Time | null;
  url: string;
  phrase: string;
}

export interface Hold {
  since: Time | null;
  by: string | null;
  /** design, policy, brand, packaging, library, dependency or owner decision. */
  category: string;
  reaffirmed: Time | null;
  owners: string[];
}

export interface WorkState {
  state: string;
  subtype: string | null;
  waits_on: Role[];
  /** Canonical logins the state waits on. */
  waits_for: string[];
  since: Time | null;
  /** Read from comment text, not from a GitHub field. */
  guess: boolean;
  low: boolean;
  rule: string;
  why: string;
  evidence: Evidence | null;
  also: string[];
  flags: string[];
  hold: Hold | null;
  blocked_by: number[];
  competing: number[];
}

/** One open pull request. */
export interface Item {
  repo: string;
  number: number;
  title: string;
  url: string;
  /** The author's canonical login. */
  author: string | null;
  created_at: Time | null;
  work: WorkState;
  /** The latest maintainer request the author has not answered; null when none waits. */
  reply?: Reply | null;
}

/** A maintainer request that waits for the author's answer: `Reply` in view.rs. */
export interface Reply {
  at: Time | null;
  by: string;
  /** What it asks for, such as rebase or split. */
  subtype: string;
  url: string;
}

export interface Flow {
  merged: number;
  merged_by: Record<string, number>;
  landed_elsewhere: number;
  /** Closed without merging, by state; folded parents and accidental closes left out. */
  closed: Record<string, number>;
  closed_total: number;
  median_merge_secs: number | null;
  median_ready_secs: number | null;
}

export interface Given {
  comments: number;
  reviews: number;
  approved: number;
  changes_requested: number;
  prs: number;
  requests: number;
  holds: number;
  merges: number;
  closes: number;
}

export interface PersonResponse {
  their_median_secs: number | null;
  their_answered: number;
  unanswered: number;
  oldest_unanswered: Time | null;
  m_median_secs: number | null;
  m_answered: number;
  awaiting_first: number;
  oldest_awaiting: Time | null;
}

export interface PersonRow {
  key: string;
  login: string;
  name: string | null;
  on_roster: boolean;
  maintainer_of: string[];
  owner_of: string[];
  /** By window id, then by repo or ALL_REPOS. */
  flow: Record<string, Record<string, Flow>>;
  given: Record<string, Record<string, Given>>;
  /** By repo or ALL_REPOS. */
  response: Record<string, PersonResponse>;
}

export interface PeopleView {
  generated_at: Time | null;
  items: Item[];
  people: PersonRow[];
  records: number;
  fetched: number;
  rankings: RankRow[];
  /** By window, trailer names that match no account, with their commits. */
  co_unlinked: Record<string, Record<string, number>>;
  /** Roster rows without a GitHub login, by name. */
  unlinked: string[];
  load: Load;
  /** Each repo's flow, by repo and window. */
  repos: Record<string, Record<string, RepoFlow>>;
  /** The checks on each repo's default branch. */
  main: Record<string, MainChecks>;
  /** When each repo's records were last brought up to date; null for a repo never checked. */
  checked: Record<string, Time | null>;
  /** The login of the user the GitHub token belongs to; null until the app has read it. */
  viewer?: string | null;
  /** `people.my_login` from the config, which My PRs lists in place of `viewer`. */
  my_login?: string | null;
}

/** How far the first load of merged and closed pull requests has come: `Load` in view.rs. */
export interface Load {
  merged: number;
  /** null until every repo's walk has read a page. */
  total: number | null;
  /** Pull requests closed without merging. */
  closed: number;
  /** null until every repo's closed walk has read a page. */
  closed_total: number | null;
  done: boolean;
}

/** `RepoFlow` in view.rs. */
export interface RepoFlow {
  merged: number;
  merged_by: Record<string, number>;
  landed_elsewhere: number;
  closed: Record<string, number>;
  median_merge_secs: number | null;
  median_response_secs: number | null;
  /** 'merge commit', 'squash or rebase' or 'unknown'. */
  methods: Record<string, number>;
  mergers: Record<string, number>;
}

/** `MainChecks` in record.rs. */
export interface MainChecks { oid: string; rollup: string | null; failing: string[] }

// The rankings: mirrors crates/craft-core/src/people/rank.rs.

/** One person's figures in one window: `Rank` in rank.rs. */
export interface Rank {
  merged: number;
  landed: number;
  added: number;
  removed: number;
  gen_added: number;
  gen_removed: number;
  files: number;
  commits: number;
  co_authored: number;
  /** null when the history the app keeps does not cover the window. */
  direct: number | null;
  active_days: number;
  merges: number;
  batch_merges: number;
  reviews: number;
  approved: number;
  changes_requested: number;
  commented: number;
  reviewed_prs: number;
  review_commits: number;
  review_added: number;
  review_removed: number;
  types: Record<string, number>;
  guessed: Record<string, number>;
  stacked: number;
  truncated: number;
  unverified: boolean;
}

export interface RankRow {
  key: string;
  login: string;
  name: string | null;
  on_roster: boolean;
  maintainer: boolean;
  /** By window id. */
  windows: Record<string, Rank>;
}

/** One row of a person's detail table: `DetailRow` in rank.rs. */
export interface DetailRow {
  key: string;
  merged: number;
  added: number;
  removed: number;
  /** Their share of their own total, 0 to 1. */
  share: number;
  merges: number;
  reviews: number;
  review_commits: number;
  guessed: number;
  rules: string[];
  children: DetailRow[];
}

/** `person_detail`'s answer: `PersonDetail` in rank.rs. */
export interface PersonDetail {
  key: string;
  window: string;
  added: number;
  removed: number;
  merged: number;
  by_repo: DetailRow[];
  by_area: DetailRow[];
  by_type: DetailRow[];
  by_dir: DetailRow[];
}

/** `repo_detail`'s answer: `RepoPeople` in rank.rs. */
export interface RepoPeople {
  repo: string;
  window: string;
  people: RankRow[];
  batch: boolean;
}

/** The key of the figures that cover every repo: `ALL_REPOS` in view.rs. */
export const ALL_REPOS = '*';

/** The state of a pull request whose timeline has not been read yet: `NOT_READ_YET` in states.rs. */
export const NOT_READ_YET = 'not_read_yet';

/** The states in rule order, with their labels. */
export const STATE_LABELS: Readonly<Record<string, string>> = {
  author_stepped_back: 'Author stepped back',
  held_for_owner: 'Held for owner',
  held_for_maintainer_task: 'Maintainer task',
  declined_in_comment_still_open: 'Declined, still open',
  superseded_still_open: 'Superseded, still open',
  author_declared_hold: 'Author parked it',
  depends_on_other_pr: 'Depends on another PR',
  draft: 'Draft',
  author_marked_not_ready: 'Not ready',
  competing_prs_maintainer_choice: 'Competing PRs',
  redirected_to_shared_library: 'Move to craft-libs',
  changes_requested_by_maintainer: 'Changes requested',
  community_changes_requested: 'Community asked for changes',
  merge_conflict: 'Merge conflict',
  ci_awaiting_workflow_approval: 'Workflow approval',
  ci_failing_from_pr: 'CI failing',
  ci_failing_inherited_from_main: 'CI failing on main',
  ci_running: 'CI running',
  author_responded_awaiting_maintainer: 'Author answered',
  community_approved_awaiting_maintainer: 'Community approved',
  maintainer_engaged_ready_to_merge: 'Ready to merge',
  awaiting_first_maintainer_response: 'Awaiting first response',
  awaiting_maintainer: 'Awaiting maintainer',
  [NOT_READ_YET]: 'Not read yet',
  merged: 'Merged',
  closed_landed_elsewhere: 'Landed elsewhere',
  closed_duplicate: 'Duplicate',
  closed_folded_split_or_moved: 'Folded or split',
  closed_superseded: 'Superseded',
  closed_declined: 'Declined',
  closed_withdrawn_or_abandoned: 'Withdrawn',
};

export const FLAG_LABELS: Readonly<Record<string, string>> = {
  aging: 'aging', stale: 'stale', no_ci_signal: 'no CI signal', mergeability_unknown: 'mergeability unknown',
  maintainer_authored: 'maintainer authored', assigned: 'assigned', conflict: 'conflict',
  also_held_for_owner: 'also held for owner', community_approved: 'community approved',
  community_gate_request: 'community gate request', stale_approval: 'approval of an older head',
  no_m_response_over_life: 'no maintainer response', disputed_after_close: 'disputed after close',
};

/** Who an item waits on, in the order the tab lists them; `unread` holds the items not read yet. */
export type RoleGroup = Role | 'nobody' | 'unread';
export const ROLES: readonly RoleGroup[] = ['author', 'maintainer', 'owner', 'ci', 'other_pr', 'nobody', 'unread'];
export const ROLE_LABELS: Readonly<Record<RoleGroup, string>> = {
  author: 'the author', maintainer: 'a maintainer', owner: 'the owner', ci: 'CI', other_pr: 'another PR', nobody: 'nobody',
  unread: 'nobody yet',
};

/** The heading of one group of items: "Waiting on the author", or "Not read yet" for the items whose timeline has not been read. */
export const roleHeading = (role: RoleGroup): string => (role === 'unread' ? 'Not read yet' : `Waiting on ${ROLE_LABELS[role]}`);

export const stateLabel = (id: string): string => STATE_LABELS[id] ?? id.replaceAll('_', ' ');
export const flagLabel = (id: string): string => FLAG_LABELS[id] ?? id.replaceAll('_', ' ');

/** A count with the part read from comment text: "38 (6 from comments)", or "38" when GitHub fields gave every state. */
export function countText(n: number, fromText: number): string {
  return fromText ? `${n} (${fromText} from comments)` : String(n);
}

/** The hover text of a count with states read from comment text. */
export const fromTextTip = (n: number, fromText: number): string =>
  `${fromText} of ${n} ${n === 1 ? 'state was' : 'states were'} read from a comment’s text, not from a GitHub field`;

/** A duration: 45m, 5h, 2.5d under ten days, 12d after; '' for none. */
export function durationText(secs: number | null | undefined): string {
  if (secs === null || secs === undefined) return '';
  const s = Math.max(0, secs);
  if (s < 3600) return `${Math.max(1, Math.round(s / 60))}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  const d = s / 86400;
  return d < 10 ? `${d.toFixed(1)}d` : `${Math.round(d)}d`;
}

const shortDate = (iso: Time | null): string => (iso ? iso.slice(0, 10) : '');

/** The hover text of a state: the rule, the matched sentence with its author and date, whether it was read from comment text, and what else matched. */
export function stateTip(w: WorkState): string {
  if (w.state === NOT_READ_YET) {
    return 'Not read yet: the app has found this pull request but has not read its comments, reviews and events, so it has no work state yet.';
  }
  const lines = [stateLabel(w.state) + subtypeText(w.subtype), `Rule ${w.rule}`];
  if (w.evidence) lines.push(evidenceLine(w.evidence));
  if (w.guess) lines.push(`Read from a comment’s text, not from a GitHub field: ${w.why || 'matched a phrase list'}.`);
  else if (w.why) lines.push(`${w.why.charAt(0).toUpperCase()}${w.why.slice(1)}.`);
  if (w.low) lines.push('Low confidence.');
  if (w.hold) lines.push(holdText(w.hold));
  if (w.blocked_by.length) lines.push(`Waits for ${prRefs(w.blocked_by)}.`);
  if (w.competing.length) lines.push(`Competes with ${prRefs(w.competing)}.`);
  if (w.also.length) lines.push(`Also matched: ${w.also.map(stateLabel).join(', ')}.`);
  if (w.flags.length) lines.push(`Flags: ${w.flags.map(flagLabel).join(', ')}.`);
  if (w.evidence?.url) lines.push('Click to open the comment.');
  return lines.join('\n');
}

/** Pull request numbers as "#1, #2". */
export const prRefs = (numbers: readonly number[]): string => numbers.map((n) => `#${n}`).join(', ');

/** A subtype in parentheses with spaces for underscores, after a state's label; '' for none. */
const subtypeText = (subtype: string | null | undefined): string => (subtype ? ` (${subtype.replaceAll('_', ' ')})` : '');

/** The sentence a state was read from, with its author and date. */
function evidenceLine(e: Evidence): string {
  const by = [e.author, shortDate(e.at)].filter(Boolean).join(', ');
  return by ? `“${e.quote}” (${by})` : `“${e.quote}”`;
}

/** A hold: since when, by whom, why and when it was held again. */
function holdText(h: Hold): string {
  const by = h.by ? ` by ${h.by}` : '';
  const again = h.reaffirmed ? `, held again ${shortDate(h.reaffirmed)}` : '';
  return `Held since ${shortDate(h.since)}${by}, reason: ${h.category}${again}.`;
}

/** Who an item waits on first, for grouping; `unread` when its timeline has not been read. */
export const firstRole = (w: WorkState): RoleGroup => (w.state === NOT_READ_YET ? 'unread' : w.waits_on[0] ?? 'nobody');

/** Whether `item` waits on the person `key`: their own item that waits on the author, an item where they made the last maintainer request or comment, or a hold they own. */
export const waitsOn = (item: Item, key: string): boolean => item.work.waits_for.includes(key);

const held = (i: Item): boolean => i.work.state === 'held_for_owner' || i.work.flags.includes('also_held_for_owner');

/** The open pull requests held for the owner, which the tab's badge counts. */
export const heldCount = (view: PeopleView | null | undefined): number => view?.items.filter((i) => i.work.hold).length ?? 0;

/** One person's figures for the table, for one window and one repo ('' for every repo). */
export interface PersonFigures {
  key: string;
  login: string;
  name: string | null;
  open: number;
  openGuessed: number;
  onThem: number;
  onThemGuessed: number;
  /** Their own open items that wait on someone else, by who. */
  onOthers: Partial<Record<Role | 'nobody', number>>;
  onOthersTotal: number;
  held: number;
  heldGuessed: number;
  merged: number;
  landed: number;
  closed: number;
  mergeMedian: number | null;
  landedRate: number | null;
  theirResponse: number | null;
  unanswered: number;
  responseToThem: number | null;
  awaitingFirst: number;
  reviews: number;
  given: Given | null;
  flow: Flow | null;
  response: PersonResponse | null;
}

/** The open items of `repo`, or of every repo for ''. */
export const repoItems = (view: PeopleView, repo: string): Item[] =>
  (repo ? view.items.filter((i) => i.repo.toLowerCase() === repo.toLowerCase()) : view.items);

/** Who else the author `key`'s own item waits on: every role but the author's, unless the item waits only on them. A request that also carries a hold waits on them and on the owner. */
function otherRoles(i: Item, key: string): Role[] {
  const w = i.work;
  if (w.waits_for.length > 0 && w.waits_for.every((login) => login === key)) return [];
  return w.waits_on.filter((r) => r !== 'author');
}

/** The entry of `rec` for `repo` in any letter case, or for every repo when `repo` is ''; null when there is none. */
function byRepo<T>(rec: Record<string, T> | undefined, repo: string): T | null {
  if (!rec) return null;
  if (!repo) return rec[ALL_REPOS] ?? null;
  const key = Object.keys(rec).find((k) => k.toLowerCase() === repo.toLowerCase());
  return key === undefined ? null : rec[key]!;
}

/** `p`'s figures in `window` for `repo` ('' for every repo). */
export function personFigures(view: PeopleView, p: PersonRow, window: string, repo: string): PersonFigures {
  const items = repoItems(view, repo);
  const own = items.filter((i) => i.author === p.key);
  const onThem = items.filter((i) => waitsOn(i, p.key));
  const others = own.filter((i) => otherRoles(i, p.key).length > 0);
  const onOthers: Partial<Record<Role | 'nobody', number>> = {};
  for (const i of others) for (const r of otherRoles(i, p.key)) onOthers[r] = (onOthers[r] ?? 0) + 1;
  const heldItems = own.filter(held);
  const flow = byRepo(p.flow[window], repo);
  const given = byRepo(p.given[window], repo);
  const response = byRepo(p.response, repo);
  const merged = flow?.merged ?? 0;
  const landed = flow?.landed_elsewhere ?? 0;
  return {
    key: p.key,
    login: p.login,
    name: p.name,
    open: own.length,
    openGuessed: own.filter((i) => i.work.guess).length,
    onThem: onThem.length,
    onThemGuessed: onThem.filter((i) => i.work.guess).length,
    onOthers,
    onOthersTotal: others.length,
    held: heldItems.length,
    heldGuessed: heldItems.filter((i) => i.work.guess || i.work.hold !== null).length,
    merged,
    landed,
    closed: Object.values(flow?.closed ?? {}).reduce((a, b) => a + b, 0),
    mergeMedian: flow?.median_merge_secs ?? null,
    landedRate: flow?.closed_total ? (merged + landed) / flow.closed_total : null,
    theirResponse: response?.their_median_secs ?? null,
    unanswered: response?.unanswered ?? 0,
    responseToThem: response?.m_median_secs ?? null,
    awaitingFirst: response?.awaiting_first ?? 0,
    reviews: (given?.reviews ?? 0) + (given?.comments ?? 0),
    given,
    flow,
    response,
  };
}

/** Whether a person has anything to show for the window and repo. */
const hasFigures = (f: PersonFigures): boolean =>
  f.open + f.onThem + f.merged + f.landed + f.closed + f.reviews + (f.given?.merges ?? 0) + (f.given?.closes ?? 0)
    + f.unanswered + f.awaitingFirst > 0;

export interface PeopleColumn { key: string; label: string; title: string; text?: boolean }

export const PEOPLE_COLUMNS: readonly PeopleColumn[] = [
  { key: 'person', label: 'Person', title: 'GitHub login, and the roster name', text: true },
  { key: 'open', label: 'Open', title: 'Their open pull requests; the part in parentheses has states read from comment text, not from GitHub fields' },
  { key: 'on_them', label: 'On them', title: 'Open pull requests waiting on them: their own that wait on the author, ones where they made the last maintainer request or comment, holds they own' },
  { key: 'on_others', label: 'On others', title: 'Their open pull requests waiting on someone else' },
  { key: 'held', label: 'Held', title: 'Their open pull requests held for the owner' },
  { key: 'merged', label: 'Merged', title: 'Merged in the window' },
  { key: 'landed', label: 'Landed elsewhere', title: 'Closed in the window because another pull request carried the work' },
  { key: 'closed', label: 'Closed', title: 'Closed without merging in the window, folded parents and accidental closes left out' },
  { key: 'merge_median', label: 'To merge', title: 'Median time from opening to merge, for pull requests merged in the window' },
  { key: 'landed_rate', label: 'Landed', title: 'Merged plus landed elsewhere, divided by everything closed in the window' },
  { key: 'their_response', label: 'They answer', title: 'Median time from a maintainer request to their next push or comment (a push for rebase and split requests), over all time' },
  { key: 'response_to_them', label: 'First response', title: 'Median time from ready to the first maintainer comment, review or merge on their pull requests, over all time' },
  { key: 'reviews', label: 'Reviews given', title: 'Comments and reviews on other people’s pull requests in the window' },
];

/** A column's value for sorting; null sorts last. */
export type SortValue = number | string | null;

/** The value of column `key` in `f`, for sorting; null sorts last. */
export function personValue(f: PersonFigures, key: string): SortValue {
  switch (key) {
    case 'person': return f.login.toLowerCase();
    case 'open': return f.open;
    case 'on_them': return f.onThem;
    case 'on_others': return f.onOthersTotal;
    case 'held': return f.held;
    case 'merged': return f.merged;
    case 'landed': return f.landed;
    case 'closed': return f.closed;
    case 'merge_median': return f.mergeMedian;
    case 'landed_rate': return f.landedRate;
    case 'their_response': return f.theirResponse;
    case 'response_to_them': return f.responseToThem;
    case 'reviews': return f.reviews;
    default: return null;
  }
}

/** The people with anything in `window` for `repo`, sorted by `sort` (missing values last, ties by login). */
export function peopleRows(view: PeopleView, window: string, repo: string, sort: Sort): PersonFigures[] {
  const rows = view.people.map((p) => personFigures(view, p, window, repo)).filter(hasFigures);
  const sign = sort.dir === 'asc' ? 1 : -1;
  return rows.sort((a, b) => {
    const x = personValue(a, sort.key);
    const y = personValue(b, sort.key);
    if (x !== y) {
      if (x === null) return 1;
      if (y === null) return -1;
      return (x < y ? -1 : 1) * sign;
    }
    return a.login.toLowerCase() < b.login.toLowerCase() ? -1 : 1;
  });
}

/** The next sort for a click on people column `key`: text columns start ascending, the rest descending. */
export function nextPeopleSort(sort: Sort, key: string): Sort {
  if (sort.key === key) return { key, dir: sort.dir === 'asc' ? 'desc' : 'asc' };
  return { key, dir: PEOPLE_COLUMNS.find((c) => c.key === key)?.text ? 'asc' : 'desc' };
}

/** One state's items, with their ages. */
export interface StateTime {
  state: string;
  items: Item[];
  guessed: number;
  low: number;
  medianSecs: number | null;
  maxSecs: number | null;
  /** The five oldest, oldest first. */
  oldest: Item[];
}

/** How long `i` has been in its state; null when the state's start is unknown. */
const ageSecs = (i: Item, now: number): number | null => (i.work.since ? Math.max(0, (now - Date.parse(i.work.since)) / 1000) : null);

const STATE_ORDER = new Map(Object.keys(STATE_LABELS).map((s, i) => [s, i]));

/** The median of `ascending`, a list sorted from smallest to largest; null when it is empty. */
function medianOf(ascending: readonly number[]): number | null {
  if (!ascending.length) return null;
  const mid = ascending.length >> 1;
  return ascending.length % 2 ? ascending[mid]! : (ascending[mid - 1]! + ascending[mid]!) / 2;
}

/** `items` grouped by who they wait on first, then by state (most items first), with the median and longest time in state and the five oldest. Items whose state has no known start count in the state but not in its ages, and list after the rest. */
export function stateBreakdown(items: readonly Item[], now: number): { role: RoleGroup; states: StateTime[] }[] {
  const out: { role: RoleGroup; states: StateTime[] }[] = [];
  for (const role of ROLES) {
    const mine = items.filter((i) => firstRole(i.work) === role);
    if (!mine.length) continue;
    const byState = new Map<string, Item[]>();
    for (const i of mine) byState.set(i.work.state, [...(byState.get(i.work.state) ?? []), i]);
    const states = [...byState].map(([state, list]): StateTime => {
      const known = list.filter((i) => ageSecs(i, now) !== null);
      const sorted = [...known].sort((a, b) => ageSecs(b, now)! - ageSecs(a, now)!);
      const ages = sorted.map((i) => ageSecs(i, now)!).reverse();
      return {
        state,
        items: list,
        guessed: list.filter((i) => i.work.guess).length,
        low: list.filter((i) => i.work.low).length,
        medianSecs: medianOf(ages),
        maxSecs: ages.at(-1) ?? null,
        oldest: [...sorted, ...list.filter((i) => ageSecs(i, now) === null)].slice(0, 5),
      };
    });
    states.sort((a, b) => b.items.length - a.items.length || (STATE_ORDER.get(a.state) ?? 99) - (STATE_ORDER.get(b.state) ?? 99));
    out.push({ role, states });
  }
  return out;
}

/** The repos a person has figures in for `window`, in `order`. */
export function personRepos(view: PeopleView, p: PersonRow, window: string, order: readonly string[]): string[] {
  return order.filter((repo) => hasFigures(personFigures(view, p, window, repo)));
}

/** The tab's line about its records: "120 of 340 timelines read · 1,240 of 9,747 merged and 30 of 120 closed pull requests loaded"; '' once everything is read. */
export function cachedText(view: PeopleView | null | undefined): string {
  if (!view) return '';
  return [
    view.fetched < view.records ? `${formatCount(view.fetched)} of ${formatCount(view.records)} timelines read` : '',
    loadText(view.load),
  ].filter(Boolean).join(' · ');
}

// ───────────────────────────── rankings ─────────────────────────────

/** The three rankings the selector offers. */
export type RankMode = 'merged' | 'maintainer' | 'type';
export const RANK_MODES: readonly { id: RankMode; label: string }[] = [
  { id: 'merged', label: 'Merged' }, { id: 'maintainer', label: 'Maintainer' }, { id: 'type', label: 'Type' },
];

/** The types, in the order the classifier lists them: `TYPES` in kind.rs. */
export const TYPES: readonly string[] = ['feature', 'fix', 'docs', 'tests', 'ci', 'packaging', 'dependency', 'i18n', 'refactor', 'perf', 'release'];
/** The column that adds features and fixes together. */
export const PRODUCT = 'product';

export interface RankColumn { key: string; label: string; title: string; text?: boolean }

const PERSON_COLUMN: RankColumn = { key: 'person', label: 'Person', title: 'GitHub login, and the roster name', text: true };

export const RANK_COLUMNS: Readonly<Record<RankMode, readonly RankColumn[]>> = {
  merged: [
    PERSON_COLUMN,
    { key: 'merged', label: 'Merged', title: 'Their pull requests merged in the window' },
    { key: 'landed', label: 'Landed elsewhere', title: 'Their pull requests closed in the window because another pull request carried the work' },
    { key: 'lines', label: 'Lines', title: 'Product lines added and removed in merged pull requests, split by commit author. Cargo.lock, translation catalogs and contributors.json are counted in the next column.' },
    { key: 'generated', label: 'Catalogs, generated', title: 'Lines in Cargo.lock, translation catalogs and contributors/contributors.json' },
    { key: 'files', label: 'Files', title: 'Changed files over their merged pull requests' },
    { key: 'commits', label: 'Commits', title: 'Commits they authored in merged pull requests, each counted once per repository' },
    { key: 'co_authored', label: 'Co-authored', title: 'Commits that name them in a Co-authored-by trailer. AI co-authors are left out.' },
    { key: 'direct', label: 'Direct to main', title: 'Commits on main that belong to no pull request. The app keeps the history of main for the 7-day and release windows only, and counts these once every merged pull request is loaded.' },
    { key: 'active_days', label: 'Active days', title: 'UTC days on which they authored a commit, merged something or submitted a review' },
  ],
  maintainer: [
    PERSON_COLUMN,
    { key: 'merges', label: 'Merges', title: 'Pull requests they landed: the merge commit’s committer when someone pushed a local merge, otherwise GitHub’s merged-by. Counted by merge time.' },
    { key: 'reviews', label: 'Reviews', title: 'Reviews they submitted on other people’s pull requests' },
    { key: 'approved', label: 'Approved', title: 'Reviews that approved' },
    { key: 'changes_requested', label: 'Changes requested', title: 'Reviews that requested changes' },
    { key: 'commented', label: 'Commented', title: 'Reviews that only commented' },
    { key: 'reviewed_prs', label: 'PRs reviewed', title: 'Distinct pull requests they reviewed' },
    { key: 'review_commits', label: 'Review commits', title: 'Commits they authored in someone else’s pull request, counted when it merged' },
    { key: 'review_lines', label: 'Review lines', title: 'Product lines of those commits' },
  ],
  type: [
    PERSON_COLUMN,
    { key: PRODUCT, label: 'Product work', title: 'Features plus fixes: missing features often arrive as bug reports' },
    ...TYPES.map((t): RankColumn => ({ key: `type:${t}`, label: t, title: `Their merged pull requests of type ${t}` })),
  ],
};

/** The value of ranking column `key` for a person with figures `r`, for sorting; null sorts last. */
export function rankValue(row: RankRow, r: Rank, key: string): SortValue {
  switch (key) {
    case 'person': return row.login.toLowerCase();
    case 'lines': return r.added + r.removed;
    case 'generated': return r.gen_added + r.gen_removed;
    case 'review_lines': return r.review_added + r.review_removed;
    case 'direct': return r.direct;
    case PRODUCT: return (r.types.feature ?? 0) + (r.types.fix ?? 0);
    default: {
      if (key.startsWith('type:')) return r.types[key.slice(5)] ?? 0;
      const v = (r as unknown as Record<string, unknown>)[key];
      return typeof v === 'number' ? v : null;
    }
  }
}

/** Whether `r` has anything the ranking `mode` shows. */
function ranked(r: Rank, mode: RankMode): boolean {
  if (mode === 'maintainer') return r.merges + r.reviews + r.review_commits > 0;
  if (mode === 'type') return Object.values(r.types).some((n) => n > 0);
  return r.merged + r.landed + r.added + r.removed + r.gen_added + r.gen_removed + r.commits + r.co_authored + (r.direct ?? 0) + r.active_days > 0;
}

/** The people with figures for ranking `mode` in `window`, sorted by `sort` (missing values last, ties by login). */
export function rankRows(view: PeopleView, window: string, mode: RankMode, sort: Sort): { row: RankRow; rank: Rank }[] {
  const rows = (view.rankings ?? []).flatMap((row) => {
    const rank = row.windows[window];
    return rank && ranked(rank, mode) ? [{ row, rank }] : [];
  });
  const sign = sort.dir === 'asc' ? 1 : -1;
  return rows.sort((a, b) => {
    const x = rankValue(a.row, a.rank, sort.key);
    const y = rankValue(b.row, b.rank, sort.key);
    if (x !== y) {
      if (x === null) return 1;
      if (y === null) return -1;
      return (x < y ? -1 : 1) * sign;
    }
    return a.row.login.toLowerCase() < b.row.login.toLowerCase() ? -1 : 1;
  });
}

/** The next sort for a click on ranking column `key`: text columns start ascending, the rest descending. */
export function nextRankSort(sort: Sort, key: string): Sort {
  if (sort.key === key) return { key, dir: sort.dir === 'asc' ? 'desc' : 'asc' };
  return { key, dir: key === 'person' ? 'asc' : 'desc' };
}

/** A sort that ranking `mode` has a column for; otherwise its default. */
export function rankSortFor(sort: Sort, mode: RankMode): Sort {
  if (RANK_COLUMNS[mode].some((c) => c.key === sort.key)) return sort;
  if (mode === 'merged') return { key: 'merged', dir: 'desc' };
  return { key: mode === 'maintainer' ? 'merges' : PRODUCT, dir: 'desc' };
}

/** The header while merged and closed pull requests load: "1,240 of 9,747 merged and 30 of 120 closed pull requests loaded"; '' once loaded. */
export function loadText(load: Load | null | undefined): string {
  if (!load || load.done) return '';
  const part = (n: number, total: number | null | undefined, what: string) =>
    total === null || total === undefined ? `${formatCount(n)} ${what}` : `${formatCount(n)} of ${formatCount(total)} ${what}`;
  return `${part(load.merged, load.total, 'merged')} and ${part(load.closed ?? 0, load.closed_total, 'closed')} pull requests loaded`;
}

/** How old the People tab's records are: the oldest time a repo's records
 * were brought up to date, the repos never checked, and whether the records
 * are stale (a repo never checked, or the oldest check is older than
 * `staleAfterSecs`). Null before any view, or with no repos. */
export function recordsAge(
  view: Pick<PeopleView, 'checked'> | null | undefined,
  staleAfterSecs: number,
  now: number = Date.now(),
): { oldest: number | null; unchecked: string[]; stale: boolean } | null {
  const entries = Object.entries(view?.checked ?? {});
  if (!entries.length) return null;
  const unchecked = entries.filter(([, at]) => !at).map(([repo]) => repo);
  const times = entries.flatMap(([, at]) => (at ? [Date.parse(at)] : []));
  const oldest = times.length ? Math.min(...times) : null;
  const stale = unchecked.length > 0 || (oldest !== null && now - oldest > staleAfterSecs * 1000);
  return { oldest, unchecked, stale };
}

/** Lines added and removed: "+1,805 −4". */
export const linesText = (added: number, removed: number): string => `+${formatCount(added)} −${formatCount(removed)}`;

/** The hover text of a type count whose type a fallback rule decided for some of it. */
export function guessTip(type: string, n: number, guessed: number): string {
  return `${guessed} of ${n} ${type} pull requests got their type from a fallback rule, or from a rule not checked outside the pull requests it was tuned on. Open the caret for the rules.`;
}

/** The batch badge: "14 in batch pushes"; '' without batch merges. */
export const batchText = (r: Rank): string => (r.batch_merges ? `${formatCount(r.batch_merges)} in batch pushes` : '');
export const BATCH_TIP = 'One push can land a whole batch, so these merges share a time. The merge count measures pull requests landed, not effort.';
export const UNVERIFIED_TIP = 'Includes a repository that lands work through a batch branch, where landed pull requests may close without merging. Nobody has checked what GitHub records there, so this count is unverified.';
export const DIRECT_MISSING_TIP = 'Not counted: the app keeps the history of main for the 7-day and release windows only, and counts direct commits once every merged pull request is loaded.';
export const RELEASE_TIP = 'Since the latest build: each repository starts at its own latest build, and a person’s figures add up those spans.';
export const TIME_NOTE = 'GitHub records no hours, so merged pull requests and lines stand in for time.';

/** The first line of a person's detail tables: "1 merged pull request and +31 −2 product lines, last week." */
export const workLead = (merged: number, added: number, removed: number, windowLabel: string): string =>
  `${formatCount(merged)} merged pull ${merged === 1 ? 'request' : 'requests'} and ${linesText(added, removed)} product lines, ${windowLabel}.`;

/** The work-state figures of one repo for the caret on its row. */
export interface RepoFigures {
  open: number;
  /** Open pull requests by who they wait on first, with their share and states. */
  byRole: { role: RoleGroup; count: number; share: number; states: { state: string; count: number }[] }[];
  oldestOnMaintainer: Item | null;
  firstQueue: number;
  firstQueueMedianSecs: number | null;
  /** Held for the owner, longest held first, and how many of those also conflict. */
  holds: Item[];
  holdsConflicting: number;
  /** Holds the authors placed on their own pull requests. */
  parked: Item[];
  approvals: number;
  answered: number;
  communityApproved: number;
  ready: number;
  /** Maintainer requests waiting on the author, by subtype. */
  requests: Record<string, number>;
  communityRequests: number;
  conflicts: number;
  ciFromPr: number;
  steppedBack: number;
  inherited: number;
  /** Aging and stale counts by who the item waits on. */
  staleness: { role: RoleGroup; aging: number; stale: number }[];
  blocked: Item[];
  competing: [number, number][];
  flow: RepoFlow | null;
  main: MainChecks | null;
}

const since = (iso: Time | null | undefined, now: number): number | null => (iso ? Math.max(0, (now - Date.parse(iso)) / 1000) : null);

/** `repo`'s figures for window `window` at `now`. */
export function repoFigures(view: PeopleView, repo: string, window: string, now: number): RepoFigures {
  const items = repoItems(view, repo);
  const count = (pred: (i: Item) => boolean) => items.filter(pred).length;
  const byRole = ROLES.flatMap((role) => {
    const mine = items.filter((i) => firstRole(i.work) === role);
    if (!mine.length) return [];
    const states = new Map<string, number>();
    for (const i of mine) states.set(i.work.state, (states.get(i.work.state) ?? 0) + 1);
    return [{
      role, count: mine.length, share: mine.length / items.length,
      states: [...states].map(([state, n]) => ({ state, count: n })).sort((a, b) => b.count - a.count || (STATE_ORDER.get(a.state) ?? 99) - (STATE_ORDER.get(b.state) ?? 99)),
    }];
  });
  const age = (i: Item) => since(i.work.since ?? i.created_at, now) ?? -1;
  const onMaintainer = items.filter((i) => i.work.waits_on.includes('maintainer')).sort((a, b) => age(b) - age(a));
  const queue = items.filter((i) => i.work.state === 'awaiting_first_maintainer_response');
  const queueAges = queue.map((i) => since(i.work.since, now)).filter((a): a is number => a !== null).sort((a, b) => a - b);
  const holdAge = (i: Item) => since(i.work.hold?.since, now) ?? -1;
  const holds = items.filter((i) => i.work.hold).sort((a, b) => holdAge(b) - holdAge(a));
  const conflicting = (i: Item) => i.work.state === 'merge_conflict' || i.work.flags.includes('conflict') || i.work.also.includes('merge_conflict');
  const requests: Record<string, number> = {};
  for (const i of items.filter((x) => x.work.state === 'changes_requested_by_maintainer')) {
    const k = i.work.subtype ?? 'fixes';
    requests[k] = (requests[k] ?? 0) + 1;
  }
  const staleness = ROLES.flatMap((role) => {
    const mine = items.filter((i) => firstRole(i.work) === role);
    const aging = mine.filter((i) => i.work.flags.includes('aging')).length;
    const stale = mine.filter((i) => i.work.flags.includes('stale')).length;
    return aging + stale ? [{ role, aging, stale }] : [];
  });
  const pairs = new Map<string, [number, number]>();
  for (const i of items) {
    for (const n of i.work.competing) {
      const p: [number, number] = i.number < n ? [i.number, n] : [n, i.number];
      pairs.set(p.join(','), p);
    }
  }
  const key = (rec: Record<string, unknown> | undefined) => Object.keys(rec ?? {}).find((k) => k.toLowerCase() === repo.toLowerCase());
  const flowKey = key(view.repos);
  const mainKey = key(view.main);
  return {
    open: items.length,
    byRole,
    oldestOnMaintainer: onMaintainer[0] ?? null,
    firstQueue: queue.length,
    firstQueueMedianSecs: medianOf(queueAges),
    holds,
    holdsConflicting: holds.filter(conflicting).length,
    parked: items.filter((i) => i.work.state === 'author_declared_hold'),
    approvals: count((i) => i.work.state === 'ci_awaiting_workflow_approval'),
    answered: count((i) => i.work.state === 'author_responded_awaiting_maintainer'),
    communityApproved: count((i) => i.work.state === 'community_approved_awaiting_maintainer' || i.work.flags.includes('community_approved')),
    ready: count((i) => i.work.state === 'maintainer_engaged_ready_to_merge'),
    requests,
    communityRequests: count((i) => i.work.state === 'community_changes_requested'),
    conflicts: count((i) => i.work.state === 'merge_conflict'),
    ciFromPr: count((i) => i.work.state === 'ci_failing_from_pr'),
    steppedBack: count((i) => i.work.state === 'author_stepped_back'),
    inherited: count((i) => i.work.state === 'ci_failing_inherited_from_main'),
    staleness,
    blocked: items.filter((i) => i.work.blocked_by.length > 0),
    competing: [...pairs.values()].sort((a, b) => a[0] - b[0] || a[1] - b[1]),
    flow: flowKey === undefined ? null : view.repos[flowKey]![window] ?? null,
    main: mainKey === undefined ? null : view.main[mainKey]!,
  };
}

// ───────────────────────────── my prs ─────────────────────────────
// The My PRs tab lists the People view's open items whose author is one
// login, in the configured repos, with a blocker from each item's work state.

/** The login My PRs lists: `people.my_login` from the config, otherwise the token's user; null before either is known. */
export function myLogin(view: Pick<PeopleView, 'my_login' | 'viewer'> | null | undefined): string | null {
  return view?.my_login || view?.viewer || null;
}

/** Who a blocker waits on: you, CI, someone else, nobody because it is approved or ready to merge, or not known because the timeline has not been read. */
export type Tone = 'me' | 'ci' | 'them' | 'ready' | 'unread';

export interface Blocker { state: string; label: string; tone: Tone }

/** Short blocker names for the work states; the other states keep their People tab label. */
export const BLOCKER_LABELS: Readonly<Record<string, string>> = {
  merge_conflict: 'Conflicting',
  ci_failing_from_pr: 'Failing checks',
  ci_failing_inherited_from_main: 'Failing on main',
  ci_awaiting_workflow_approval: 'Waiting for approval',
  ci_running: 'Checks running',
  held_for_owner: 'Held',
  maintainer_engaged_ready_to_merge: 'Ready',
  community_approved_awaiting_maintainer: 'Approved',
  changes_requested_by_maintainer: 'Changes requested',
  community_changes_requested: 'Review findings',
  author_marked_not_ready: 'Not ready',
  awaiting_first_maintainer_response: 'Awaiting review',
  author_responded_awaiting_maintainer: 'Answered',
  author_declared_hold: 'Parked',
  competing_prs_maintainer_choice: 'Competing',
};

const READY_STATES = new Set(['maintainer_engaged_ready_to_merge', 'community_approved_awaiting_maintainer']);
const TONE_ORDER: Readonly<Record<Tone, number>> = { me: 0, ci: 1, them: 2, ready: 3, unread: 4 };

/** Whether `item`'s work state waits on `login`. */
const waitsOnLogin = (item: Item, login: string): boolean => item.work.waits_for.includes(login.toLowerCase());

/** `item`'s blocker for `login`: the label of its work state, and whether that state is approved or ready to merge, waits on CI, on them, on someone else, or has not been read yet. */
export function blockerOf(item: Item, login: string): Blocker {
  const w = item.work;
  const label = w.state === 'depends_on_other_pr' && w.blocked_by.length
    ? `Depends on ${prRefs(w.blocked_by)}`
    : BLOCKER_LABELS[w.state] ?? stateLabel(w.state);
  return { state: w.state, label, tone: toneOf(item, login) };
}

/** Whether `item` is approved or ready to merge, waits on CI, on `login`, on someone else, or has not been read yet. */
function toneOf(item: Item, login: string): Tone {
  const w = item.work;
  if (w.state === NOT_READ_YET) return 'unread';
  if (READY_STATES.has(w.state)) return 'ready';
  if (w.state === 'ci_running') return 'ci';
  return waitsOnLogin(item, login) ? 'me' : 'them';
}

/** Whether `item` is held for the owner: its state, a request that also carries a hold, or a hold that an earlier rule outranked. */
export const isHeld = (i: Item): boolean => held(i) || i.work.also.includes('held_for_owner');

/** Who `item` waits on, with "you" for `login`. */
export function waitsText(item: Item, login: string): string {
  const mine = waitsOnLogin(item, login);
  const others = item.work.waits_on.filter((r) => !(mine && r === 'author')).map((r) => ROLE_LABELS[r]);
  const parts = mine ? ['you', ...others] : others;
  return parts.length ? parts.join(', ') : ROLE_LABELS[firstRole(item.work)];
}

/** One row of My PRs. `needsMe`: its blocker waits on you, or a maintainer request waits for your reply. */
export interface MineRow { item: Item; blocker: Blocker; reply: Reply | null; held: boolean; needsMe: boolean }

export interface MineColumn { key: string; label: string; title: string; text?: boolean; asc?: boolean }

export const MINE_COLUMNS: readonly MineColumn[] = [
  { key: 'repo', label: 'Repo', title: 'Repository', text: true },
  { key: 'number', label: '#', title: 'Pull request number' },
  { key: 'title', label: 'Title', title: 'Pull request title', text: true },
  { key: 'blocker', label: 'Blocker', title: 'What it waits for, from its work state: on you first, then CI, then other people, then approved or ready to merge, then not read yet', asc: true },
  { key: 'reply', label: 'Reply', title: 'A maintainer request you have not answered; the oldest sorts first' },
  { key: 'held', label: 'Held', title: 'Held for the owner’s decision; the longest held sorts first' },
  { key: 'waits', label: 'Waits on', title: 'Who its work state waits on', text: true },
  { key: 'in_state', label: 'In state', title: 'How long it has been in its work state; for a pull request not read yet, since GitHub last saw it change' },
  { key: 'age', label: 'Opened', title: 'How long ago it was opened' },
];

/** The value of My PRs column `key` for `row`, for sorting; null sorts last. */
export function mineValue(row: MineRow, key: string, login: string, now: number): SortValue {
  const i = row.item;
  switch (key) {
    case 'repo': return (i.repo.split('/').pop() ?? i.repo).toLowerCase();
    case 'number': return i.number;
    case 'title': return i.title.toLowerCase();
    case 'blocker': return TONE_ORDER[row.blocker.tone] * 100 + (STATE_ORDER.get(i.work.state) ?? 99);
    case 'reply': return row.reply ? since(row.reply.at, now) ?? 0 : null;
    case 'held': return row.held ? since(i.work.hold?.since, now) ?? 0 : null;
    case 'waits': return waitsText(i, login);
    case 'in_state': return ageSecs(i, now);
    case 'age': return since(i.created_at, now);
    default: return null;
  }
}

/** `login`'s open pull requests in `repos` (any letter case), sorted by `sort` (missing values last, ties by repo and number); with `needsMeOnly`, only those that need them. */
export function mineRows(view: PeopleView, repos: readonly string[], login: string, sort: Sort, now: number, needsMeOnly = false): MineRow[] {
  const me = login.toLowerCase();
  const listed = new Set(repos.map((r) => r.toLowerCase()));
  const rows = view.items
    .filter((i) => i.author === me && listed.has(i.repo.toLowerCase()))
    .map((item): MineRow => {
      const blocker = blockerOf(item, login);
      const reply = item.reply ?? null;
      return { item, blocker, reply, held: isHeld(item), needsMe: blocker.tone === 'me' || reply !== null };
    })
    .filter((r) => !needsMeOnly || r.needsMe);
  const sign = sort.dir === 'asc' ? 1 : -1;
  const tie = (a: MineRow, b: MineRow) => a.item.repo.toLowerCase().localeCompare(b.item.repo.toLowerCase()) || a.item.number - b.item.number;
  return rows.sort((a, b) => {
    const x = mineValue(a, sort.key, login, now);
    const y = mineValue(b, sort.key, login, now);
    if (x !== y) {
      if (x === null) return 1;
      if (y === null) return -1;
      return (x < y ? -1 : 1) * sign;
    }
    return tie(a, b);
  });
}

/** The next sort for a click on My PRs column `key`: text columns and Blocker start ascending, the rest descending. */
export function nextMineSort(sort: Sort, key: string): Sort {
  if (sort.key === key) return { key, dir: sort.dir === 'asc' ? 'desc' : 'asc' };
  const col = MINE_COLUMNS.find((c) => c.key === key);
  return { key, dir: col?.text || col?.asc ? 'asc' : 'desc' };
}

/** "5h ago", or "just now". */
const agoText = (iso: Time, now: number): string => {
  const a = ago(iso, now);
  return a === 'now' ? 'just now' : `${a} ago`;
};

/** The hover text of a Reply chip: who asked, when and for what. */
export function replyTip(r: Reply, now: number = Date.now()): string {
  const when = r.at ? agoText(r.at, now) : 'at an unknown time';
  return `${r.by} asked ${when}${subtypeText(r.subtype)}, and you have not answered since.\nClick to open the request.`;
}

/** The hover text of a Held chip: for how long, by whom and why, or that an earlier rule decided the state. */
export function heldTip(i: Item, now: number = Date.now()): string {
  const h = i.work.hold;
  if (!h) return 'Held for the owner. An earlier rule decided the state, so the hold’s details are not shown. Hover the blocker for the rules that matched.';
  const bits = [h.since ? `Held for ${ago(h.since, now)}` : 'Held', h.by && `by ${h.by}`, h.category && `reason: ${h.category}`,
    h.reaffirmed && `held again ${agoText(h.reaffirmed, now)}`, h.owners.length ? `waits on ${h.owners.join(', ')}` : ''];
  return bits.filter(Boolean).join(', ') + '.';
}
