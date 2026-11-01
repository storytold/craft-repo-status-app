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
  /** The commit at the head of the default branch. */
  head_oid: string | null;
  /** Blob ids of the parity documents at the head of the default branch, by path. */
  parity_oids: Record<string, string>;
}

// Parity: mirrors crates/craft-core/src/parity/mod.rs.

/** The basis a number's Kind or How cell states. */
export type Basis = 'measured' | 'partly' | 'estimated' | 'unknown';

/** A file in the repo and a line in it, from 1. */
export interface Src { path: string; line: number }

/** A parsed value: read, missing from the document, or present but not in a form the parser reads. */
export type Cell<T> =
  | { state: 'ok'; v: T; src: Src; from?: string | null }
  | { state: 'absent'; why: string }
  | { state: 'unreadable'; why: string; raw: string; src: Src };

/** A percentage as written. */
export interface Pct {
  value: number;
  approx: boolean;
  low: number | null;
  high: number | null;
  basis: Basis;
  raw: string;
  note: string | null;
  computed: boolean;
}

/** A percentage, or "ahead of" or "beyond" the target app. */
export type Score = ({ kind: 'pct' } & Pct) | { kind: 'ahead'; text: string };

export interface Hours {
  lo: number;
  hi: number;
  approx: boolean;
  basis: string | null;
  note: string | null;
  raw: string;
  counted_in: string | null;
}

export interface Fraction { done: number; total: number }

/** One headline percentage: read, missing from the document, or present but not in a form the parser reads. */
export type Figure =
  | {
    state: 'ok';
    pct: number;
    low: number | null;
    high: number | null;
    /** The row's label and its Value and Kind cells, without markup. */
    label: string;
    text: string;
    kind: string;
    basis: Basis;
    /** The file and its line, from 1. */
    path: string;
    line: number;
    /** Where the number came from when it is not the row it names. */
    from: string | null;
  }
  | { state: 'absent'; why: string }
  | { state: 'unreadable'; why: string; raw: string; path: string; line: number };

export interface StageStep { stage: string; points: number | null; hours: Hours | null; text: string }
export interface Stage { name: string; steps: StageStep[] }

/** One parity document and its dates (`YYYY-MM-DD`). */
export interface DocFile {
  path: string;
  oid: string;
  reviewed: string | null;
  updated: string | null;
  generated: string | null;
  target: string | null;
  date_lines: number;
  line: number | null;
  /** Set when the file has no date line. */
  why: string | null;
  /** The last commit to the path: its date and id. */
  commit_date: Time | null;
  commit: string | null;
}

/** The dimension groups the Apps table has a column for. */
export type Canonical =
  | 'features' | 'ui_ux' | 'file_formats' | 'type_layout' | 'hardware' | 'localization' | 'performance'
  | 'stability' | 'platforms' | 'ecosystem' | 'ai' | 'agent_control' | 'collaboration' | 'standards' | 'other';

/** A link in a document, and the in-repo path it resolves to. */
export interface Link { text: string; url: string; path: string | null }

/** One dimension of "ready for real work". */
export interface Dim {
  name: string;
  group: Canonical;
  weight: number | null;
  weight_src: string | null;
  ready: Cell<Score>;
  breadth: Cell<Score>;
  hours: Cell<Hours>;
  kind: string;
  doc: Link | null;
  counted: string | null;
  /** A `↳` row under the row above; left out of sums. */
  child: boolean;
  fills: 'ready' | 'breadth';
  only_in: 'w' | 'd' | null;
  src: Src;
  /** Weighted points lost: weight share × (100 − ready). */
  shortfall: number | null;
}

/** One heat cell of the Apps table: the dimension rows of one column. */
export interface Heat { column: Canonical; value: number | null; text: string; rows: string[]; src: Src | null }

/** The dimension that loses the most weighted points. */
export interface Drag { name: string; group: Canonical; points: number }
export interface Chip { text: string; hover: string | null }
export interface AreaCounts { features: number | null; shipped: number | null; partial: number | null }

/** One feature area. */
export interface Area {
  name: string;
  full_name: string;
  key: string;
  weight: number | null;
  weight_src: string | null;
  breadth: Cell<Score>;
  ready: Cell<Score>;
  ready_from: string | null;
  hours: Cell<Hours>;
  missing: Chip[];
  more: number;
  chips_from: 'marker' | 'gaps_column' | 'negative_clauses' | 'none';
  /** The gaps cell as written. */
  raw_gaps: string;
  gaps_header: string | null;
  unweighted: boolean;
  dup_lines: number[];
  conflict: string | null;
  kind: string;
  doc: Link | null;
  counts: AreaCounts | null;
  src: Src;
  shortfall: number | null;
}

export interface AltTotal { label: string; score: Cell<Score> }

/** Whether a gap blocks a stage. */
export type Flag = 'yes' | 'no' | 'unknown' | { partial: string };

/** One gap from docs/gaps.md. */
export interface Gap {
  key: string;
  id: string;
  /** Row order in the ranked list, from 1; 0 for unranked gaps. */
  rank: number;
  group: string | null;
  title: string;
  alpha: Flag;
  beta: Flag;
  blocks_src: string | null;
  scope_note: string | null;
  priority: string | null;
  kinds: string[];
  hours: Cell<Hours>;
  counted_in: string | null;
  doc: Link | null;
  workstream: string | null;
  src: Src;
  detail_src: Src | null;
  detail: string | null;
  closed: boolean;
  paired_with: string[];
  issues: number[];
}

export interface HoursSum { count: number; lo: number; hi: number; unknown: number }

/** Sums over the open gaps. */
export interface GapTotals {
  beta: HoursSum | null;
  partial: HoursSum | null;
  alpha: number;
  alpha_none: boolean;
  stated: Cell<Hours>;
  ids_stable: boolean;
  form: string;
  beta_unknown: boolean;
}

/** Status counts for one tier, group or document. */
export interface Counts {
  name: string;
  done: number;
  partial: number;
  missing: number;
  excluded: number;
  ahead: number;
  not_measured: number;
  unrecognized: number;
  pct: number | null;
}

export interface CheckRow { status: string; group: string; text: string; notes: string; line: number }
export interface Group { name: string; done: number; total: number; line: number }

/** A machine checklist. */
export type Checklist =
  | { kind: 'tiered'; path: string; tiers: Counts[]; overall: number | null; rows: CheckRow[]; more: number }
  | { kind: 'presence'; path: string; total: Cell<Fraction>; groups: Group[]; missing: CheckRow[]; more: number }
  | { kind: 'status'; path: string; groups: Counts[]; overall: number | null; rows: CheckRow[]; more: number };

/** A hand-written docs/*-parity.md. */
export interface DocChecklist {
  path: string;
  score: Cell<Pct>;
  counts: Counts | null;
  rows: CheckRow[];
  pct_rows: CheckRow[];
  why: string | null;
}

/** A note the parser adds about a document, such as two sources that disagree. */
export interface Note { level: 'info' | 'warn'; text: string; src: Src | null }

/** What the app shows from a repo's parity documents. */
export interface Parity {
  version: number;
  /** The commit the files were read at. */
  commit: string | null;
  roadmap_oid: string | null;
  /** Whether docs/target-app-parity.md exists at that commit. */
  parity_doc: boolean;
  /** Why the repo has no parity numbers. */
  na: string | null;
  breadth: Figure;
  ready: Figure;
  /** The newest Last reviewed date in ROADMAP.md, YYYY-MM-DD. */
  last_reviewed: string | null;
  breadth_alts: Figure[];
  menu_breadth: Figure;
  menu_hint: Fraction | null;
  /** The in-repo paths the breadth rows' Kind cells link to, the shown row's first. */
  breadth_links: string[];
  mainstream: Figure;
  essentials: Figure;
  to_alpha: Cell<Hours>;
  to_beta: Cell<Hours>;
  to_full: Cell<Hours>;
  stage: Cell<Stage>;
  files: DocFile[];
  /** The oldest Last reviewed among ROADMAP.md, the parity doc and gaps.md. */
  reviewed: string | null;
  dims: Dim[];
  heat: Heat[];
  dims_total: Cell<Score>;
  top_drag: Cell<Drag>;
  areas: Area[];
  areas_total: Cell<Score>;
  area_alts: AltTotal[];
  area_weight_sum: number | null;
  /** Why the areas have no weights. */
  area_weights_why: string | null;
  gaps: Gap[];
  gap_totals: GapTotals;
  checklists: Checklist[];
  docs: DocChecklist[];
  notes: Note[];
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
  /** The parity numbers; null (or absent, from an older build) until read. */
  parity?: Parity | null;
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
  /** The Parity tab marks documents last reviewed more than this many days ago (absent from older builds). */
  parity_warn_days?: number;
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
export const DEFAULT_WINDOW = '4h';
export const windowById = (id: string): ActivityWindow =>
  WINDOWS.find((w) => w.id === id) ?? windowById(DEFAULT_WINDOW);

// ───────────────────────────── columns ─────────────────────────────

/**
 * `age` is a timestamp shown and sorted by how long ago it was: ascending puts
 * the newest first. `pct` sorts as a number but has no footer total. `rate` is a
 * number per second: it sorts like `num`, whatever unit it displays in, but has
 * no footer total.
 */
export type Kind = 'text' | 'num' | 'pct' | 'rate' | 'date' | 'age' | 'version';
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
  { key: 'latest_build_rate', label: 'Build DL Rate', kind: 'rate', group: 'Downloads', title: 'Build DLs divided by how long the most recent release had been out when they were counted', get: buildRate },
  { key: 'latest_build_downloads', label: 'Build DLs', kind: 'num', group: 'Downloads', title: 'Downloads of the most recent release, all OSes and package types (checksums and update deltas excluded)' },
  { key: 'downloads_total', label: 'Total DLs', kind: 'num', group: 'Downloads', title: 'Downloads across every published release' },
  { key: 'latest_build', label: 'Build', kind: 'version', group: 'Downloads', title: 'Most recent published release' },
  { key: 'latest_build_at', label: 'Built', kind: 'date', group: 'Downloads', title: 'When the most recent release was published' },
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
  { key: 'parity_breadth', label: 'Breadth', kind: 'pct', group: 'Parity', title: 'Feature breadth from the Headline numbers of ROADMAP.md (or docs/target-app-parity.md): how much of the target app’s feature set exists', get: (e) => figurePct(e.parity?.breadth) },
  { key: 'parity_ready', label: 'Ready', kind: 'pct', group: 'Parity', title: 'Ready for real work from ROADMAP.md’s Headline numbers; without that row, from its Readiness by audience table, the headline table of docs/target-app-parity.md, or Feature depth', get: (e) => figurePct(e.parity?.ready) },
];

export const COLUMN: Readonly<Record<string, Column>> = Object.fromEntries(COLUMNS.map((c) => [c.key, c]));

/**
 * The value a column shows for an entry; `null` when there is no data
 * (including a cache written before that window existed).
 */
export function value(entry: RepoEntry, key: string, window: string = DEFAULT_WINDOW): Value {
  const col = COLUMN[key];
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

// ───────────────────────────── parity ─────────────────────────────

/** The `Parity` field each parity column shows. */
export type ParityField = 'breadth' | 'ready';
export const PARITY_FIGURES: Readonly<Record<string, ParityField>> = { parity_breadth: 'breadth', parity_ready: 'ready' };

/** A figure's percentage; null when it is missing or unread. */
export function figurePct(f: Figure | null | undefined): number | null {
  return f?.state === 'ok' ? f.pct : null;
}

/** "77%", or "81.1%" when the document gives a decimal. */
export const formatPct = (pct: number): string => `${Math.round(pct * 10) / 10}%`;

const BASIS_TEXT: Readonly<Record<Basis, string>> = {
  measured: 'Measured', partly: 'Partly measured', estimated: 'Estimated', unknown: 'Basis not stated',
};

/**
 * The Last reviewed date of the file figure `f` was read from, for its tooltip.
 * An absent figure names ROADMAP.md, or docs/target-app-parity.md when the repo
 * has no ROADMAP.md; null when it has neither file.
 */
function reviewLine(p: Parity, f: Figure): string | null {
  let src = 'ROADMAP.md';
  if (f.state !== 'absent') src = f.path;
  else if (!p.roadmap_oid && p.parity_doc) src = 'docs/target-app-parity.md';
  if (src !== 'ROADMAP.md') {
    const reviewed = p.files.find((x) => x.path === src)?.reviewed;
    return reviewed ? `${src} last reviewed ${reviewed}` : `${src} has no Last reviewed date`;
  }
  if (!p.roadmap_oid) return null;
  return p.last_reviewed ? `ROADMAP.md last reviewed ${p.last_reviewed}` : 'ROADMAP.md has no Last reviewed date';
}

/**
 * Where a click on a parity cell goes: docs/target-app-parity.md on GitHub at
 * the commit read, or without that document the figure's row in ROADMAP.md,
 * shown as source (`?plain=1`); null when the repo has neither file.
 */
export function parityUrl(repo: string, p: Parity, f: Figure): string | null {
  const at = encodeURIComponent(p.commit ?? 'HEAD');
  if (p.parity_doc) return `https://github.com/${repo}/blob/${at}/docs/target-app-parity.md`;
  if (!p.roadmap_oid) return null;
  if (f.state === 'absent') return `https://github.com/${repo}/blob/${at}/ROADMAP.md`;
  const path = f.path.split('/').map(encodeURIComponent).join('/');
  return `https://github.com/${repo}/blob/${at}/${path}?plain=1#L${f.line}`;
}

/** The tooltip of a parity cell: the row and its basis, or why there is no number, and the review date. */
export function parityTip(p: Parity | null | undefined, field: ParityField): string {
  if (!p) return 'Not read yet';
  const f = p[field];
  const lines: string[] = [];
  if (f.state === 'ok') {
    lines.push(`${f.label}: ${f.text}`, f.kind ? `${BASIS_TEXT[f.basis]} · ${f.kind}` : BASIS_TEXT[f.basis]);
    if (f.from) lines.push(`Source: ${f.from}`);
  } else {
    lines.push(f.why);
    if (f.state === 'unreadable') lines.push(`${f.path} line ${f.line}: ${f.raw}`);
  }
  const review = reviewLine(p, f);
  if (review) lines.push(review);
  if (p.parity_doc) lines.push('Click to open docs/target-app-parity.md on GitHub');
  else if (p.roadmap_oid) lines.push('Click to open ROADMAP.md on GitHub');
  return lines.join('\n');
}

// ───────────────────────────── parity tab ─────────────────────────────

/**
 * A document line on GitHub at `commit`: `…/blob/<commit>/<path>?plain=1#L<line>`.
 * `?plain=1` shows a Markdown file as text, where the line anchor works; with
 * no line (0) the link opens the rendered file.
 */
export function sourceUrl(repo: string, commit: string | null | undefined, src: Src): string {
  const at = encodeURIComponent(commit ?? 'HEAD');
  const path = src.path.split('/').map(encodeURIComponent).join('/');
  const anchor = src.line ? `?plain=1#L${src.line}` : '';
  return `https://github.com/${repo}/blob/${at}/${path}${anchor}`;
}

/** A document link on GitHub at `commit`, at the section it names; null for a link outside the repo. */
export function docUrl(repo: string, commit: string | null | undefined, doc: Link): string | null {
  if (!doc.path) return null;
  const hash = doc.url.indexOf('#');
  return sourceUrl(repo, commit, { path: doc.path, line: 0 }) + (hash >= 0 ? doc.url.slice(hash) : '');
}

/** A cell's value; null when absent or unreadable. */
export function cellValue<T>(c: Cell<T> | null | undefined): T | null {
  return c?.state === 'ok' ? c.v : null;
}

/** A score's percentage; null for "ahead" and missing scores. */
export function scorePct(c: Cell<Score> | null | undefined): number | null {
  const s = cellValue(c);
  return s?.kind === 'pct' ? s.value : null;
}

/** "75%", "ahead" or "—". */
export function scoreText(c: Cell<Score> | null | undefined): string {
  const s = cellValue(c);
  if (!s) return '—';
  return s.kind === 'pct' ? formatPct(s.value) : 'ahead';
}

/** "170–270 h", "3 h" or "—". */
export function hoursText(h: Pick<Hours, 'lo' | 'hi'> | null | undefined): string {
  if (!h) return '—';
  const n = (v: number) => v.toLocaleString('en-US');
  return h.lo === h.hi ? `${n(h.lo)} h` : `${n(h.lo)}–${n(h.hi)} h`;
}

/** The hours of a sum of gaps, saying how many of them have no estimate. */
export function sumText(s: HoursSum): string {
  if (s.unknown >= s.count) return 'hours unknown';
  return s.unknown ? `${hoursText(s)} + ${s.unknown} unestimated` : hoursText(s);
}

/** The gaps that block beta in part, with the most hours they add. */
export function partText(s: HoursSum): string {
  if (s.unknown >= s.count) return `part of ${s.count} gaps (hours unknown)`;
  const unestimated = s.unknown ? `, ${s.unknown} unestimated` : '';
  return `part of ${s.count} gaps (up to ${s.hi.toLocaleString('en-US')} h${unestimated})`;
}

/** Why a value is missing, for a dash's tooltip: the reason, and for an unreadable value its text and line. */
export function whyMissing(c: Cell<unknown> | Figure | null | undefined): string {
  if (!c) return 'Not read yet';
  if (c.state === 'absent') return c.why;
  if (c.state === 'unreadable') {
    const at = 'src' in c ? `${c.src.path}:${c.src.line}` : `${c.path}:${c.line}`;
    return `${c.why}\n${at}: ${c.raw}`;
  }
  return '';
}

/** Where a figure was read, or null when it was not. */
export function figureSrc(f: Figure | null | undefined): Src | null {
  if (!f || f.state === 'absent') return null;
  return { path: f.path, line: f.line };
}

/** The badge text of a basis; empty when the document does not say. */
export const BASIS_BADGE: Readonly<Record<Basis, string>> = { measured: 'Measured', partly: 'Partly measured', estimated: 'Estimated', unknown: '' };

/** The Apps table's dimension columns, in order; agent control, collaboration and standards show under Other. */
export const HEAT_COLUMNS: readonly { key: Canonical; label: string }[] = [
  { key: 'features', label: 'Features' }, { key: 'ui_ux', label: 'UI/UX' }, { key: 'file_formats', label: 'Files' },
  { key: 'type_layout', label: 'Type and layout' }, { key: 'hardware', label: 'Hardware' }, { key: 'localization', label: 'Localization' },
  { key: 'performance', label: 'Performance' }, { key: 'stability', label: 'Stability' }, { key: 'platforms', label: 'Platforms' },
  { key: 'ecosystem', label: 'Ecosystem' }, { key: 'ai', label: 'AI' }, { key: 'other', label: 'Other' },
];

/** The heat cell of column `col`. */
export function heatOf(p: Parity | null | undefined, col: Canonical): Heat | undefined {
  return p?.heat.find((h) => h.column === col);
}

export type Audience = 'full' | 'mainstream' | 'essentials';
export const AUDIENCES: readonly { id: Audience; label: string }[] = [
  { id: 'full', label: 'Full target' }, { id: 'mainstream', label: 'Mainstream' }, { id: 'essentials', label: 'Essentials' },
];

/** The readiness figure for an audience: Ready for the full target. */
export function audienceFigure(p: Parity, a: Audience): Figure {
  if (a === 'full') return p.ready;
  return a === 'mainstream' ? p.mainstream : p.essentials;
}

/** Whether the repo has parity documents (and has been read). */
export const hasParity = (e: RepoEntry): boolean => !!e.parity && !e.parity.na && !e.inaccessible;

/** A gap that blocks alpha or beta outright and is still open. */
export const isBlocker = (g: Gap): boolean => !g.closed && (g.alpha === 'yes' || g.beta === 'yes');

/** The Parity tab's badge: open gaps that block alpha or beta, each counted once, summed across apps. */
export function parityBadge(entries: readonly RepoEntry[]): number {
  return entries.reduce((n, e) => n + (hasParity(e) ? e.parity!.gaps.filter(isBlocker).length : 0), 0);
}

/** How many days old a review date may be before the Parity tab marks it, when the snapshot does not say. */
export const PARITY_WARN_DAYS = 14;

/** Whole days since `date` (`YYYY-MM-DD`); null without one. */
export function daysSince(date: string | null | undefined, now: number = Date.now()): number | null {
  if (!date) return null;
  return Math.floor((now - Date.parse(`${date}T00:00:00Z`)) / 86400e3);
}

/** Whole days since the oldest Last reviewed of ROADMAP.md, the parity doc and gaps.md; null without one. */
export function reviewAgeDays(p: Parity | null | undefined, now: number = Date.now()): number | null {
  return daysSince(p?.reviewed, now);
}

/** When any parity document last changed: the newest last commit to one; null when unknown. */
export function changedAt(p: Parity | null | undefined): string | null {
  const times = (p?.files ?? []).map((f) => f.commit_date).filter((s): s is string => !!s);
  return times.reduce<string | null>((a, b) => (a !== null && Date.parse(a) > Date.parse(b) ? a : b), null);
}

const STAGES = ['pre-alpha', 'early alpha', 'alpha', 'beta', 'stable'];

/** The stage's name, or null. */
export const stageOf = (p: Parity | null | undefined): string | null => cellValue(p?.stage)?.name ?? null;

/** The value a Parity tab column sorts by; null when there is none. */
export type ParityValue = number | string | null;

/** A column of the Apps table: its label, its tooltip, and the value it sorts by. */
export interface ParityColumn {
  key: string;
  label: string;
  title: string;
  /** Sorts ascending when first clicked. */
  asc?: boolean;
  value: (e: RepoEntry, audience: Audience) => ParityValue;
}

const readyFor = (e: RepoEntry, a: Audience): number | null => figurePct(e.parity && audienceFigure(e.parity, a));

export const PARITY_COLUMNS: readonly ParityColumn[] = [
  { key: 'app', label: 'App', title: 'Sort by name', asc: true, value: (e) => e.repo.split('/').pop() ?? e.repo },
  { key: 'stage', label: 'Stage', title: 'The stage from ROADMAP.md’s Stage line', value: (e) => { const s = stageOf(e.parity); return s === null ? null : STAGES.indexOf(s); } },
  { key: 'ready', label: 'Ready', title: 'Ready for real work for the chosen audience, with its range; the tick marks Breadth', value: readyFor },
  { key: 'breadth', label: 'Breadth', title: 'Feature breadth: how much of the target app’s feature set exists', value: (e) => figurePct(e.parity?.breadth) },
  { key: 'to_beta', label: 'To beta', title: 'Hours to beta from ROADMAP.md’s Headline numbers', asc: true, value: (e) => cellValue(e.parity?.to_beta)?.hi ?? null },
  { key: 'to_full', label: 'To full', title: 'Hours to full parity from ROADMAP.md’s Headline numbers', asc: true, value: (e) => cellValue(e.parity?.to_full)?.hi ?? null },
  { key: 'top_drag', label: 'Top drag', title: 'The dimension that loses the most weighted points: weight share × (100 − ready)', value: (e) => cellValue(e.parity?.top_drag)?.points ?? null },
  { key: 'beta_gaps', label: 'Beta gaps', title: 'Open gaps that block beta, and their hours, from docs/gaps.md', value: (e) => {
    const t = e.parity?.gap_totals;
    return !t || t.beta_unknown ? null : t.beta?.count ?? 0;
  } },
  ...HEAT_COLUMNS.map((c): ParityColumn => ({
    key: `heat_${c.key}`, label: c.label, title: `${c.label}: ready for real work on this dimension`,
    value: (e) => {
      const h = heatOf(e.parity, c.key);
      if (!h) return null;
      return h.value ?? (h.text === 'ahead' ? 101 : null);
    },
  })),
  { key: 'areas', label: 'Areas', title: 'Feature areas: width is weight, fill is ready', value: (e) => e.parity?.areas.length || null },
  { key: 'checklist', label: 'Checklist', title: 'The machine checklist: done share per tier, or menu items that exist', value: (e) => checklistOverall(e.parity) },
  { key: 'reviewed', label: 'Reviewed', title: 'Age of the oldest Last reviewed among ROADMAP.md, docs/target-app-parity.md and docs/gaps.md', asc: true, value: (e) => reviewAgeDays(e.parity) },
  { key: 'changed', label: 'Changed', title: 'When a parity document last changed: the newest last commit to one', value: (e) => { const t = changedAt(e.parity); return t ? Date.parse(t) : null; } },
];

export const PARITY_COLUMN: Readonly<Record<string, ParityColumn>> = Object.fromEntries(PARITY_COLUMNS.map((c) => [c.key, c]));

/** The Apps rows in order: repos without parity documents last, then by the sort, missing values last. */
export function parityOrder(entries: readonly RepoEntry[], sort: Sort, audience: Audience): RepoEntry[] {
  const col = PARITY_COLUMN[sort.key] ?? PARITY_COLUMN.ready!;
  const sign = sort.dir === 'asc' ? 1 : -1;
  const name = (e: RepoEntry) => String(PARITY_COLUMN.app!.value(e, audience));
  const byApp = (a: RepoEntry, b: RepoEntry) => name(a).localeCompare(name(b), undefined, { sensitivity: 'base', numeric: true });
  return [...entries].sort((a, b) => {
    const pa = hasParity(a);
    const pb = hasParity(b);
    if (pa !== pb) return pa ? -1 : 1;
    const va = col.value(a, audience);
    const vb = col.value(b, audience);
    if (va === null && vb === null) return byApp(a, b);
    if (va === null) return 1;
    if (vb === null) return -1;
    const d = typeof va === 'string' || typeof vb === 'string'
      ? String(va).localeCompare(String(vb), undefined, { sensitivity: 'base', numeric: true })
      : va - vb;
    return sign * d || byApp(a, b);
  });
}

/** The Apps sort after clicking column `key`: flip it if already sorted by it, else the column's first direction. */
export function parityNextSort(sort: Sort, key: string): Sort {
  if (sort.key === key) return { key, dir: sort.dir === 'asc' ? 'desc' : 'asc' };
  return { key, dir: PARITY_COLUMN[key]?.asc ? 'asc' : 'desc' };
}

/** Each dimension column's mean across the apps with a percentage there, and how many those are. */
export function heatMeans(entries: readonly RepoEntry[]): Record<string, { mean: number | null; n: number }> {
  const out: Record<string, { mean: number | null; n: number }> = {};
  for (const c of HEAT_COLUMNS) {
    const vs = entries.filter(hasParity).map((e) => heatOf(e.parity, c.key)?.value).filter((v): v is number => typeof v === 'number');
    out[c.key] = { mean: vs.length ? Math.round((vs.reduce((a, b) => a + b, 0) / vs.length) * 10) / 10 : null, n: vs.length };
  }
  return out;
}

/** One segment of an app's area strip. */
export interface Segment { name: string; key: string; width: number; fill: number | null; tip: string }

/** The area strip: one segment per area, as wide as its weight share (equal without weights), filled to its ready (empty without one). */
export function areaStrip(p: Parity | null | undefined): Segment[] {
  const areas = (p?.areas ?? []).filter((a) => !a.unweighted);
  if (!areas.length) return [];
  const sum = areas.reduce((s, a) => s + (a.weight ?? 0), 0);
  const weighted = sum > 0 && areas.every((a) => a.weight !== null);
  const sorted = weighted ? [...areas].sort((a, b) => (b.weight ?? 0) - (a.weight ?? 0)) : areas;
  return sorted.map((a) => ({
    name: a.name,
    key: a.key,
    width: weighted ? ((a.weight ?? 0) / sum) * 100 : 100 / areas.length,
    fill: scorePct(a.ready),
    tip: [
      a.name,
      `Ready ${scoreText(a.ready)} · Breadth ${scoreText(a.breadth)} · ${hoursText(cellValue(a.hours))}`,
      weighted ? `Weight ${a.weight} of ${sum}` : 'No weights in the source: equal widths',
    ].join('\n'),
  }));
}

/** The machine checklists' overall done share, for sorting: the first tiered or status one, else the presence one. */
export function checklistOverall(p: Parity | null | undefined): number | null {
  for (const c of p?.checklists ?? []) {
    if (c.kind !== 'presence' && c.overall !== null) return c.overall;
  }
  for (const c of p?.checklists ?? []) {
    if (c.kind === 'presence' && c.total.state === 'ok' && c.total.v.total > 0) return (c.total.v.done / c.total.v.total) * 100;
  }
  return null;
}

/** The Apps table's checklist cell: `P0 98 · P1 95 · P2 50`, `menus 100% · scorecard 75%`, `97% menus`, or a dash. */
export function checklistSummary(p: Parity | null | undefined): { text: string; title: string } {
  const lists = p?.checklists ?? [];
  if (!lists.length) return { text: '—', title: 'No machine checklist in this repo' };
  const parts: string[] = [];
  const title: string[] = [];
  const presence = lists.find((c) => c.kind === 'presence');
  const tiered = lists.find((c) => c.kind === 'tiered');
  const status = lists.find((c) => c.kind === 'status');
  if (tiered?.kind === 'tiered') {
    parts.push(tiered.tiers.filter((t) => t.name !== 'untiered' && t.pct !== null).map((t) => `${t.name} ${Math.round(t.pct!)}`).join(' · '));
    title.push(`${tiered.path}: ${tiered.overall === null ? '—' : formatPct(tiered.overall)} overall`);
  }
  const menus = presence?.kind === 'presence' && presence.total.state === 'ok' && presence.total.v.total > 0
    ? (presence.total.v.done / presence.total.v.total) * 100 : null;
  if (menus !== null && presence?.kind === 'presence' && presence.total.state === 'ok') {
    parts.push(status || tiered ? `menus ${Math.round(menus)}%` : `${Math.round(menus)}% menus`);
    title.push(`${presence.path}: ${presence.total.v.done} of ${presence.total.v.total}`);
  }
  if (status?.kind === 'status' && status.overall !== null) {
    parts.push(`scorecard ${Math.round(status.overall)}%`);
    title.push(`${status.path}: ${formatPct(status.overall)} done`);
  }
  return { text: parts.filter(Boolean).join(' · ') || '—', title: title.join('\n') };
}

/** A gap with the repo it belongs to, for the All gaps view. */
export interface AppGap extends Gap {
  repo: string;
  commit: string | null;
  /** The Last reviewed date of the repo's docs/gaps.md. */
  reviewed: string | null;
}

/** A gap's band in the default order: alpha blockers, beta blockers, partial blockers, the rest. */
export function gapBand(g: Gap): number {
  if (g.alpha === 'yes') return 0;
  if (g.beta === 'yes') return 1;
  if (typeof g.beta === 'object' || typeof g.alpha === 'object') return 2;
  return 3;
}

/** The All gaps filters. */
export interface GapFilter {
  repo: string;
  kind: string;
  betaOnly: boolean;
  hideClosed: boolean;
  minHours: number;
  text: string;
}

export const NO_GAP_FILTER: GapFilter = { repo: '', kind: '', betaOnly: false, hideClosed: true, minHours: 0, text: '' };

/** The canonical gap kinds, as `KINDS` in crates/craft-core/src/parity/value.rs. */
export const GAP_KINDS: readonly string[] = [
  'feature', 'ui/ux', 'file format', 'codec', 'typography', 'layout', 'hardware', 'localization', 'performance',
  'stability', 'platforms', 'ecosystem', 'ai', 'collaboration', 'standards', 'measurement', 'fidelity',
];

/** The kinds a summary groups a gap under: its canonical kinds, or "other". */
export function gapKinds(g: Gap): string[] {
  const known = g.kinds.filter((k) => GAP_KINDS.includes(k));
  return known.length ? known : ['other'];
}

/**
 * Every app's gaps, filtered. The default order is the bands (see `gapBand`)
 * and, within each, rank interleaved across apps: every app's rank 1, then
 * every rank 2, ties largest hours first. A sort by a column replaces it.
 */
export function allGaps(entries: readonly RepoEntry[], f: GapFilter = NO_GAP_FILTER, sort: Sort | null = null): AppGap[] {
  const text = f.text.trim().toLowerCase();
  const out: AppGap[] = [];
  for (const e of entries) {
    if (!hasParity(e) || (f.repo && e.repo !== f.repo)) continue;
    const p = e.parity!;
    const reviewed = p.files.find((x) => x.path === 'docs/gaps.md')?.reviewed ?? null;
    for (const g of p.gaps) {
      if (gapPasses(g, e.repo, f, text)) out.push({ ...g, repo: e.repo, commit: p.commit, reviewed });
    }
  }
  const hi = (g: Gap) => cellValue(g.hours)?.hi ?? -1;
  const rank = (g: Gap) => g.rank || 1e6;
  if (!sort) {
    return out.sort((a, b) => gapBand(a) - gapBand(b) || rank(a) - rank(b) || hi(b) - hi(a) || a.repo.localeCompare(b.repo));
  }
  const sign = sort.dir === 'asc' ? 1 : -1;
  return out.sort((a, b) => compareGapValues(gapSortValue(a, sort.key), gapSortValue(b, sort.key), sign));
}

/** Whether gap `g` of `repo` passes filter `f`; `text` is the filter's text, trimmed and in lower case. */
function gapPasses(g: Gap, repo: string, f: GapFilter, text: string): boolean {
  if (f.hideClosed && g.closed) return false;
  if (f.betaOnly && g.beta !== 'yes') return false;
  if (f.kind && !gapKinds(g).includes(f.kind)) return false;
  if (f.minHours > 0 && (cellValue(g.hours)?.hi ?? 0) < f.minHours) return false;
  return !text || `${g.title} ${g.id} ${repo}`.toLowerCase().includes(text);
}

/** The value the All gaps column `key` sorts a gap by; null when it has none. */
function gapSortValue(g: AppGap, key: string): ParityValue {
  switch (key) {
    case 'app': return g.repo.split('/').pop() ?? g.repo;
    case 'rank': return g.rank || null;
    case 'id': return g.id || null;
    case 'gap': return g.title;
    case 'blocks': return gapBand(g);
    case 'kind': return gapKinds(g)[0] ?? null;
    case 'hours': return cellValue(g.hours)?.hi ?? null;
    case 'doc': return g.doc?.text || null;
    case 'reviewed': return g.reviewed;
    default: return null;
  }
}

/** Compares two All gaps sort values, ascending for `sign` 1 and descending for -1; a null sorts last either way. */
function compareGapValues(va: ParityValue, vb: ParityValue, sign: number): number {
  if (va === null && vb === null) return 0;
  if (va === null) return 1;
  if (vb === null) return -1;
  const d = typeof va === 'number' && typeof vb === 'number' ? va - vb : String(va).localeCompare(String(vb), undefined, { numeric: true });
  return sign * d;
}

/**
 * The All gaps sort after clicking column `key`: its first direction (Hours
 * largest first, the others ascending), then the other direction, then the
 * default order (null).
 */
export function nextGapSort(sort: Sort | null, key: string): Sort | null {
  const first: Dir = key === 'hours' ? 'desc' : 'asc';
  if (sort?.key !== key) return { key, dir: first };
  return sort.dir === first ? { key, dir: first === 'asc' ? 'desc' : 'asc' } : null;
}

/** One kind's chip above the All gaps table; `unknown` counts its gaps without hours. */
export interface KindSum extends HoursSum { kind: string; apps: number }

/** The hours of the open gaps in `gaps`, leaving out a gap counted inside another (`(in N)`). */
export function gapTotal(gaps: readonly Gap[]): HoursSum {
  const s: HoursSum = { count: 0, lo: 0, hi: 0, unknown: 0 };
  for (const g of gaps) {
    if (g.closed || g.counted_in) continue;
    const h = cellValue(g.hours);
    s.count += 1;
    s.lo += h?.lo ?? 0;
    s.hi += h?.hi ?? 0;
    if (!h) s.unknown += 1;
  }
  return s;
}

/** Gaps and hours per kind across apps, most hours first. A gap counted inside another (`(in N)`) adds no hours. */
export function kindSums(gaps: readonly AppGap[]): KindSum[] {
  const by = new Map<string, KindSum & { repos: Set<string> }>();
  for (const g of gaps) {
    if (g.closed) continue;
    const h = g.counted_in ? null : cellValue(g.hours);
    for (const k of gapKinds(g)) {
      const s = by.get(k) ?? { kind: k, count: 0, lo: 0, hi: 0, unknown: 0, apps: 0, repos: new Set<string>() };
      s.count += 1;
      s.lo += h?.lo ?? 0;
      s.hi += h?.hi ?? 0;
      if (!h && !g.counted_in) s.unknown += 1;
      s.repos.add(g.repo);
      s.apps = s.repos.size;
      by.set(k, s);
    }
  }
  return [...by.values()].map(({ repos: _repos, ...s }) => s).sort((a, b) => b.hi - a.hi || b.count - a.count);
}

/** The flag shown for a gap: A (alpha), B (beta), a part of either, or nothing. */
export function blocksLabel(g: Gap): { text: string; title: string } {
  if (g.alpha === 'yes') return { text: 'A', title: 'Blocks alpha' };
  if (typeof g.alpha === 'object') return { text: 'A part', title: `Blocks alpha in part: ${g.alpha.partial}` };
  if (g.beta === 'yes') return { text: 'B', title: g.scope_note ? `Blocks beta (${g.scope_note})` : 'Blocks beta' };
  if (typeof g.beta === 'object') return { text: 'B part', title: `Blocks beta in part: ${g.beta.partial}` };
  if (g.beta === 'unknown') return { text: '', title: 'The document does not say whether this blocks beta' };
  return { text: '', title: g.scope_note ?? 'Does not block beta' };
}

/** Why a report's beta blockers are unknown: no docs/gaps.md, no gap read from it, or it does not say. */
export function betaUnknownWhy(p: Parity): string {
  if (!p.files.some((f) => f.path === 'docs/gaps.md')) return 'No docs/gaps.md at HEAD';
  return p.gaps.length ? 'docs/gaps.md does not say which gaps block beta' : 'No gaps read from docs/gaps.md';
}

/**
 * The score a dimension row shows. A breadth row in a table without a
 * Breadth column holds its score in `ready`.
 */
export function dimScore(d: Dim): Cell<Score> {
  return d.fills === 'breadth' && d.breadth.state === 'ok' ? d.breadth : d.ready;
}

/**
 * The dimensions in the order the detail pane lists them: most points lost
 * first, or with no weights most hours first. A `↳` row follows the row
 * above it in the document.
 */
export function dimsByDrag(dims: readonly Dim[]): { dims: Dim[]; byHours: boolean } {
  const groups: Dim[][] = [];
  for (const d of dims) {
    const last = groups.at(-1);
    if (d.child && last) last.push(d);
    else groups.push([d]);
  }
  const byHours = !dims.some((d) => !d.child && d.shortfall !== null);
  const key = (d: Dim) => (byHours ? cellValue(d.hours)?.hi ?? -1 : d.shortfall ?? -1);
  groups.sort((a, b) => key(b[0]!) - key(a[0]!));
  return { dims: groups.flat(), byHours };
}

/** The areas in the same order. */
export function areasByDrag(areas: readonly Area[]): { areas: Area[]; byHours: boolean } {
  const byHours = !areas.some((a) => a.shortfall !== null);
  const key = (a: Area) => (byHours ? cellValue(a.hours)?.hi ?? -1 : a.shortfall ?? -1);
  return { areas: [...areas].sort((a, b) => key(b) - key(a)), byHours };
}
