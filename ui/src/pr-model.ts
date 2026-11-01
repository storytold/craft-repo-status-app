// The PR tab's pure logic and data types. The types mirror crates/pr-core/src/views.rs
// and crates/craft-status-app/src/pr/poller.rs: change both together.

import { CATEGORY_LABEL } from './model.ts';
import type { RepoCategory } from './model.ts';

export type PrState = 'draft' | 'needs_work' | 'conflicted' | 'not_inspected' | 'checking' | 'ci_not_run' | 'landable' | 'promising' | 'reviewable';
export type Ease = 'easy' | 'medium' | 'hard';
export type Tier = 'core' | 'frequent' | 'regular' | 'new' | 'bot';
export type Severity = 'critical' | 'major' | 'minor' | 'trivial';

export interface RepoRow {
  repo: string;
  category?: RepoCategory;
  icon?: string | null;
  open: number;
  open_hint?: number | null;
  listed: boolean;
  /** A first list walk is still under way. */
  partial?: boolean;
  inspected: number;
  analysed: number;
  approved: number;
  landable: number;
  promising: number;
  reviewable: number;
  ci_not_run: number;
  checking: number;
  not_inspected: number;
  high_quality: number;
  easy: number;
  medium: number;
  hard: number;
  needs_work: number;
  conflicted: number;
  draft: number;
  conflicts: number;
  ci_failing: number;
  rabbit_flagged: number;
  rabbit_nits: number;
  security: number;
  security_nits: number;
  core: number;
  frequent: number;
  new: number;
  bots: number;
  interesting: number;
  groups: number;
  grouped_prs: number;
  oldest_at?: string | null;
  newest_at?: string | null;
  list_at?: string | null;
  error?: string | null;
  inaccessible: boolean;
}

export interface RelatedRef {
  number: number;
  relation: string;
  group: string;
  note: string;
  source: string;
}

export interface PrRow {
  repo: string;
  number: number;
  title: string;
  url: string;
  author: string;
  avatar: string;
  tier: Tier;
  landed_total: number;
  created_at?: string | null;
  updated_at?: string | null;
  is_draft: boolean;
  additions: number;
  deletions: number;
  changed_files: number;
  labels: string[];
  review_decision?: string | null;
  approved_by: string[];
  approved: boolean;
  approval_stale: boolean;
  conflicted: boolean;
  mergeable: string;
  comments_total: number;
  reactions: number;
  state: PrState;
  ease: Ease;
  quality: number;
  high_quality: boolean;
  inspected: boolean;
  ci: string;
  rabbit_flags: number;
  rabbit_nits: number;
  security_flags: number;
  security_nits: number;
  sensitive: number;
  interesting: boolean;
  summary: string;
  analysed: boolean;
  related: RelatedRef[];
  focus: number;
}

export interface Group {
  id: string;
  title: string;
  relation: string;
  note: string;
  numbers: number[];
  source: string;
}

export interface RepoView {
  row: RepoRow;
  prs: PrRow[];
  groups: Group[];
}

export interface Reason { text: string; points: number }

export interface Assessment {
  state: PrState;
  state_reasons: string[];
  ease: Ease;
  ease_score: number;
  ease_reasons: Reason[];
  ease_raised_by_claude: boolean;
  quality: number;
  quality_reasons: Reason[];
  high_quality: boolean;
  conflicted: boolean;
  approved: boolean;
  approval_stale: boolean;
  changes_requested: boolean;
  inspected: boolean;
  ci: string;
  rabbit_flags: number;
  rabbit_nits: number;
  security_flags: number;
  security_nits: number;
  sensitive_paths: string[];
  security_reasons: string[];
  deps_changed: boolean;
  tests_touched: boolean;
  interesting: boolean;
}

export interface Finding {
  severity: Severity;
  category: string;
  effort: string;
  title: string;
  snippet: string;
  path: string;
  line?: number | null;
  url: string;
  resolved: boolean;
  outdated: boolean;
  security: boolean;
}

export interface PrDetail {
  fetched_at?: string | null;
  body: string;
  files: { path: string; additions: number; deletions: number; change: string }[];
  files_total: number;
  checks: { name: string; state: string; url?: string | null }[];
  rabbit: 'none' | 'in_progress' | 'reviewed' | 'skipped';
  findings: Finding[];
  threads_total: number;
  rabbit_actionable?: number | null;
  human_threads_open: number;
  reviews: { author: string; state: string; at?: string | null }[];
  comments: { author: string; text: string; at?: string | null; url: string }[];
  linked_issues: { number: number; title: string }[];
}

export interface Analysis {
  at?: string | null;
  model: string;
  deep: boolean;
  diff_truncated: boolean;
  summary: string;
  ease?: Ease | null;
  interesting: boolean;
  why_interesting: string;
  review_focus: string;
  risk: string;
  security_note: string;
}

export interface AuthorInfo {
  login: string;
  tier: Tier;
  permission?: string | null;
  listed: boolean;
  landed_here: number;
  landed_total: number;
  counts_complete: boolean;
}

export interface PrFull {
  repo: string;
  icon?: string | null;
  number: number;
  title: string;
  url: string;
  author: AuthorInfo;
  avatar: string;
  association: string;
  created_at?: string | null;
  updated_at?: string | null;
  is_draft: boolean;
  additions: number;
  deletions: number;
  changed_files: number;
  head_ref: string;
  base_ref: string;
  cross_repo: boolean;
  labels: string[];
  review_decision?: string | null;
  approved_by: string[];
  mergeable: string;
  merge_state: string;
  comments_total: number;
  reactions: number;
  assessment: Assessment;
  detail?: PrDetail | null;
  detail_current: boolean;
  analysis?: Analysis | null;
  analysis_current: boolean;
  groups: Group[];
  related_refs: RelatedRef[];
  related: PrRow[];
}

export interface ClaudeStatus {
  enabled: boolean;
  available: boolean;
  model: string;
  running?: string | null;
  spent_today: number;
  max_usd_per_day: number;
  runs_last_hour: number;
  max_runs_per_hour: number;
  last_error?: string | null;
}

export interface Status {
  fetching?: string | null;
  next_at?: string | null;
  paused_until?: string | null;
  error?: string | null;
  token_source?: string | null;
  token_rejected: boolean;
  rate_remaining?: number | null;
  points_last_hour: number;
  max_points_per_hour: number;
  backlog: number;
  histories_pending: number;
  poll_secs: number;
  stale_after_secs: number;
  claude: ClaudeStatus;
}

export interface Overview { repos: RepoRow[]; status: Status }

export interface Sort { key: string; dir: 'asc' | 'desc' }

export const STATE_LABEL: Record<PrState, string> = {
  draft: 'Draft',
  needs_work: 'Needs work',
  conflicted: 'Conflicted',
  not_inspected: 'Not inspected',
  checking: 'Checking',
  ci_not_run: 'CI not run',
  landable: 'Landable',
  promising: 'Promising',
  reviewable: 'Reviewable',
};

/** One row of the truth table: the columns, in HELP_COLUMNS order, then what it means. */
export interface StatusHelp {
  state: PrState;
  when: [string, string, string, string, string, string, string, string];
  meaning: string;
  color: string;
}

export const HELP_COLUMNS = ['Draft', 'Problem', 'Conflicted', 'Inspected', 'Running', 'CI', 'Approved', 'Quality ≥ 75'] as const;

/** The truth table in crates/pr-core/src/assess.rs (`headline`): change both together. */
export const STATUS_HELP: StatusHelp[] = [
  { state: 'draft', when: ['Y', '–', '–', '–', '–', '–', '–', '–'], meaning: 'Marked as a draft on GitHub.', color: 'Grey' },
  { state: 'needs_work', when: ['N', 'Y', '–', '–', '–', '–', '–', '–'], meaning: 'At least one problem below. Any conflicts still show as a flag.', color: 'Red' },
  { state: 'conflicted', when: ['N', 'N', 'Y', '–', '–', '–', '–', '–'], meaning: 'Merge conflicts, and nothing else known to be wrong. If it hasn\'t been inspected, CodeRabbit and conversations are still unknown.', color: 'Orange' },
  { state: 'not_inspected', when: ['N', 'N', 'N', 'N', '–', '–', '–', '–'], meaning: 'Not read yet, or changed since it was. CodeRabbit, files and conversations are unknown.', color: 'Muted' },
  { state: 'checking', when: ['N', 'N', 'N', 'Y', 'Y', '–', '–', '–'], meaning: 'CI or CodeRabbit is running, or GitHub hasn\'t worked out mergeability.', color: 'Amber' },
  { state: 'ci_not_run', when: ['N', 'N', 'N', 'Y', 'N', 'not run', '–', '–'], meaning: 'No CI result to go on. Often a first-time contributor\'s workflows, waiting for a maintainer to approve them.', color: 'Amber' },
  { state: 'landable', when: ['N', 'N', 'N', 'Y', 'N', 'pass', 'Y', '–'], meaning: 'A person approved the latest commit, CI passed, nothing is in the way.', color: 'Solid green' },
  { state: 'promising', when: ['N', 'N', 'N', 'Y', 'N', 'pass', 'N', 'Y'], meaning: 'Passes every automated check and scores well, but no person has approved it. A prediction, not a decision.', color: 'Solid blue' },
  { state: 'reviewable', when: ['N', 'N', 'N', 'Y', 'N', 'pass', 'N', 'N'], meaning: 'Nothing is in the way and no one has approved it, and the score is unremarkable.', color: 'Grey outline' },
];

export interface FlagHelp { cls: string; label: string; meaning: string; problem: string }

export const FLAG_HELP: FlagHelp[] = [
  { cls: 'approved', label: 'Approved', meaning: 'GitHub\'s review decision is approved. The Approved by column says who, and lists approvals GitHub doesn\'t count, usually from people without write access, in grey.', problem: 'No' },
  { cls: 'stale', label: 'Stale approval', meaning: 'The approval is of an older commit, so it doesn\'t make the PR Landable.', problem: 'No' },
  { cls: 'conflicted', label: 'Conflicted', meaning: 'GitHub reports merge conflicts.', problem: 'No, it has its own status' },
  { cls: 'bad', label: 'CI failed', meaning: 'A check failed. Cancelled and skipped runs don\'t count.', problem: 'Yes' },
  { cls: 'bad', label: 'Changes requested', meaning: 'GitHub\'s review decision says so.', problem: 'Yes' },
  { cls: 'bad', label: 'CodeRabbit flags', meaning: 'Unresolved critical or major findings.', problem: 'Yes' },
  { cls: 'bad', label: 'Security', meaning: 'A failed security check, or an unresolved critical or major security finding.', problem: 'Yes' },
  { cls: 'bad', label: 'Open conversation', meaning: 'An unresolved review thread started by a person.', problem: 'Yes' },
  { cls: '', label: 'CodeRabbit nits', meaning: 'Unresolved minor or trivial findings.', problem: 'No, lowers quality only' },
  { cls: 'warn', label: 'Sensitive paths', meaning: 'Someone without write access changed CI or build files.', problem: 'No, a reason to look closer' },
];

/** How two PRs may be related, with the wording it gets. */
export const RELATED_HELP: { relation: string; label: string; flag: boolean; source: string; meaning: string }[] = [
  { relation: 'duplicate', label: 'Duplicate', flag: true, source: 'Claude', meaning: 'Does the same job as another PR. Probably only one should land.' },
  { relation: 'overlap', label: 'Overlaps', flag: true, source: 'Claude', meaning: 'Changes the same behavior, so likely to conflict. Decide on an approach or an order.' },
  { relation: 'stack', label: 'Stacked', flag: true, source: 'Claude', meaning: 'Builds on another PR. Land them in order.' },
  { relation: 'issue', label: 'Same issue', flag: true, source: 'Automatic', meaning: 'Names or closes the same issue. Maybe a duplicate, maybe parts of one fix.' },
  { relation: 'theme', label: 'Related', flag: false, source: 'Claude', meaning: 'Same theme. Handy to review together; implies nothing else.' },
  { relation: 'files', label: 'Same files', flag: false, source: 'Automatic', meaning: 'Changes the same files. Maybe a conflict, maybe nothing.' },
  { relation: 'title', label: 'Similar title', flag: false, source: 'Automatic', meaning: 'Nearly the same title. Maybe a duplicate.' },
];

export const relatedLabel = (relation: string): string => RELATED_HELP.find((h) => h.relation === relation)?.label ?? relation;
export const relatedIsFlag = (relation: string): boolean => RELATED_HELP.find((h) => h.relation === relation)?.flag ?? false;

/** A related PR's hover text: how sure anyone is, and why. */
export function relatedTitle(r: RelatedRef): string {
  const kind = relatedLabel(r.relation);
  const lead = r.source === 'claude' ? `${kind} (Claude)` : r.relation === 'issue' ? kind : `Possibly related: ${kind.toLowerCase()}`;
  return [`#${r.number}`, lead, r.group, r.note].filter(Boolean).join(' · ');
}

export interface Column<T> {
  key: string;
  label: string;
  title?: string;
  group?: string;
  /** Text columns sort ascending first and align left. */
  text?: boolean;
  value(row: T): string | number | null;
}

const time = (iso: string | null | undefined): number | null => (iso ? Date.parse(iso) : null);

export const REPO_COLUMNS: Column<RepoRow>[] = [
  { key: 'name', label: 'Repo', text: true, value: (r) => shortName(r.repo) },
  { key: 'category', label: 'Type', text: true, title: 'What the repo is: app, library, assets, CI or tooling', value: (r) => CATEGORY_LABEL[r.category ?? 'other'] },
  { key: 'open', label: 'Open', group: 'PRs', title: 'Open PRs', value: (r) => (r.listed ? (r.partial ? Math.max(r.open, r.open_hint ?? 0) : r.open) : r.open_hint ?? null) },
  { key: 'inspected', label: 'Inspected', group: 'PRs', title: 'How many have had CodeRabbit findings, files and checks read. Heuristics are partial until then.', value: (r) => (r.listed ? r.inspected : null) },
  { key: 'landable', label: 'Landable', group: 'Landing', title: 'A person approved the latest commit, CI passed, nothing in the way', value: (r) => (r.listed ? r.landable : null) },
  { key: 'promising', label: 'Promising', group: 'Landing', title: 'Passes every automated check and scores well, but no person has approved it', value: (r) => (r.listed ? r.promising : null) },
  { key: 'reviewable', label: 'Reviewable', group: 'Landing', title: 'Nothing is in the way and no one has approved it, with an unremarkable score', value: (r) => (r.listed ? r.reviewable : null) },
  { key: 'ci_not_run', label: 'CI not run', group: 'Landing', title: 'No CI result to go on, often workflows waiting for a maintainer to approve them', value: (r) => (r.listed ? r.ci_not_run : null) },
  { key: 'approved', label: 'Approved', group: 'Landing', title: 'PRs a person approved, whatever else is going on', value: (r) => (r.listed ? r.approved : null) },
  { key: 'easy', label: 'Easy', group: 'Landing', title: 'Seems easy to land (non-drafts)', value: (r) => (r.listed ? r.easy : null) },
  { key: 'medium', label: 'Medium', group: 'Landing', value: (r) => (r.listed ? r.medium : null) },
  { key: 'hard', label: 'Hard', group: 'Landing', title: 'Seems hard to land (non-drafts)', value: (r) => (r.listed ? r.hard : null) },
  { key: 'needs_work', label: 'Needs work', group: 'Needs the author', title: 'A failed check, requested changes, a CodeRabbit flag or an open conversation', value: (r) => (r.listed ? r.needs_work : null) },
  { key: 'conflicts', label: 'Conflicts', group: 'Needs the author', title: 'PRs with merge conflicts, whatever their status', value: (r) => (r.listed ? r.conflicts : null) },
  { key: 'ci_failing', label: 'CI fails', group: 'Needs the author', value: (r) => (r.listed ? r.ci_failing : null) },
  { key: 'rabbit_flagged', label: 'Flags', group: 'CodeRabbit', title: 'PRs with unresolved critical or major findings', value: (r) => (r.listed ? r.rabbit_flagged : null) },
  { key: 'rabbit_nits', label: 'Nits', group: 'CodeRabbit', title: 'PRs with only minor or trivial findings', value: (r) => (r.listed ? r.rabbit_nits : null) },
  { key: 'security', label: 'Flags', group: 'Security', title: 'PRs with a security finding, a failed security check, or sensitive paths changed by someone without write access', value: (r) => (r.listed ? r.security : null) },
  { key: 'security_nits', label: 'Nits', group: 'Security', title: 'PRs with only minor security findings', value: (r) => (r.listed ? r.security_nits : null) },
  { key: 'core', label: 'Core', group: 'Authors', title: 'PRs by people with write access or in contributors/people.toml', value: (r) => (r.listed ? r.core : null) },
  { key: 'frequent', label: 'Frequent', group: 'Authors', value: (r) => (r.listed ? r.frequent : null) },
  { key: 'new', label: 'New', group: 'Authors', title: 'First-time contributors', value: (r) => (r.listed ? r.new : null) },
  { key: 'interesting', label: 'Interesting', group: 'Notable', title: 'Flagged by Claude, or drawing a lot of discussion', value: (r) => (r.listed ? r.interesting : null) },
  { key: 'groups', label: 'Possibly related', group: 'Notable', title: 'Groups of PRs that may be related', value: (r) => (r.listed ? r.groups : null) },
  { key: 'oldest_at', label: 'Oldest', group: 'Age', value: (r) => time(r.oldest_at) },
  { key: 'newest_at', label: 'Newest', group: 'Age', value: (r) => time(r.newest_at) },
  { key: 'list_at', label: 'Fetched', group: 'Age', value: (r) => time(r.list_at) },
];

const EASE_ORDER: Record<Ease, number> = { easy: 2, medium: 1, hard: 0 };
const CI_ORDER: Record<string, number> = { success: 3, none: 2, pending: 1, failure: 0 };

export const PR_COLUMNS: Column<PrRow>[] = [
  { key: 'focus', label: 'Status', title: 'Best first: Landable, Promising, Reviewable, CI not run, Checking, Not inspected, Conflicted, Needs work, Draft', value: (r) => r.focus },
  { key: 'title', label: 'Pull request', text: true, value: (r) => r.title.toLowerCase() },
  { key: 'author', label: 'Author', text: true, value: (r) => r.author.toLowerCase() },
  { key: 'landed_total', label: 'Landed', title: 'PRs this author has landed across the craft apps', value: (r) => r.landed_total },
  { key: 'approved_by', label: 'Approved by', text: true, title: 'Whose latest review is an approval', value: (r) => r.approved_by.join(', ').toLowerCase() || null },
  { key: 'related', label: 'Possibly related', title: 'Other PRs this may be related to. Click a number to jump to it, ⌘-click to open it.', value: (r) => r.related.length || null },
  { key: 'ease', label: 'Ease', title: 'Sorted easiest first', value: (r) => EASE_ORDER[r.ease] },
  { key: 'quality', label: 'Quality', value: (r) => r.quality },
  { key: 'ci', label: 'CI', value: (r) => CI_ORDER[r.ci] ?? 2 },
  { key: 'rabbit', label: 'CodeRabbit', title: 'Unresolved flags (critical, major) and nits (minor, trivial)', value: (r) => (r.inspected ? r.rabbit_flags * 1000 + r.rabbit_nits : null) },
  { key: 'security', label: 'Security', value: (r) => (r.inspected ? (r.security_flags + r.sensitive) * 1000 + r.security_nits : null) },
  { key: 'size', label: 'Size', title: 'Lines added and removed', value: (r) => r.additions + r.deletions },
  { key: 'changed_files', label: 'Files', value: (r) => r.changed_files },
  { key: 'updated_at', label: 'Updated', value: (r) => time(r.updated_at) },
  { key: 'created_at', label: 'Opened', value: (r) => time(r.created_at) },
];

/** A column click: reverse the current sort, or start one in its natural direction. */
export function nextSort<T>(sort: Sort, col: Column<T>): Sort {
  if (sort.key === col.key) return { key: col.key, dir: sort.dir === 'asc' ? 'desc' : 'asc' };
  return { key: col.key, dir: col.text ? 'asc' : 'desc' };
}

/** Sorted copy of `rows`; rows with no value always go last. */
export function sorted<T>(rows: T[], sort: Sort, columns: Column<T>[]): T[] {
  const col = columns.find((c) => c.key === sort.key) ?? columns[0]!;
  const sign = sort.dir === 'asc' ? 1 : -1;
  return [...rows].sort((a, b) => {
    const [x, y] = [col.value(a), col.value(b)];
    if (x === null && y === null) return 0;
    if (x === null) return 1;
    if (y === null) return -1;
    if (typeof x === 'string' || typeof y === 'string') return sign * String(x).localeCompare(String(y));
    return sign * (x - y);
  });
}

export interface Filter { id: string; label: string; test(row: PrRow): boolean }

export const FILTERS: Filter[] = [
  { id: 'all', label: 'All', test: () => true },
  { id: 'landable', label: 'Landable', test: (r) => r.state === 'landable' },
  { id: 'promising', label: 'Promising', test: (r) => r.state === 'promising' },
  { id: 'reviewable', label: 'Reviewable', test: (r) => r.state === 'reviewable' },
  { id: 'ci_not_run', label: 'CI not run', test: (r) => r.state === 'ci_not_run' },
  { id: 'approved', label: 'Approved', test: (r) => r.approved },
  { id: 'needs_work', label: 'Needs work', test: (r) => r.state === 'needs_work' },
  { id: 'conflicted', label: 'Conflicted', test: (r) => r.conflicted },
  { id: 'not_inspected', label: 'Not inspected', test: (r) => r.state === 'not_inspected' },
  { id: 'easy', label: 'Easy', test: (r) => r.ease === 'easy' && r.state !== 'draft' },
  { id: 'hard', label: 'Hard', test: (r) => r.ease === 'hard' && r.state !== 'draft' },
  { id: 'rabbit', label: 'CodeRabbit flags', test: (r) => r.rabbit_flags > 0 },
  { id: 'security', label: 'Security', test: (r) => r.security_flags + r.sensitive + r.security_nits > 0 },
  { id: 'interesting', label: 'Interesting', test: (r) => r.interesting },
  { id: 'related', label: 'Possibly related', test: (r) => r.related.length > 0 },
  { id: 'core', label: 'Core', test: (r) => r.tier === 'core' },
  { id: 'frequent', label: 'Frequent', test: (r) => r.tier === 'frequent' },
  { id: 'new', label: 'New', test: (r) => r.tier === 'new' },
  { id: 'draft', label: 'Drafts', test: (r) => r.is_draft },
];

/** Whether `row` matches every word of `query`, by title, number, author, label or summary. */
export function matchesQuery(row: PrRow, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  const hay = `${row.title} #${row.number} ${row.author} ${row.labels.join(' ')} ${row.summary}`.toLowerCase();
  return q.split(/\s+/).every((w) => hay.includes(w));
}

/** The rows a filter and search leave. Drafts only show under "All" and "Drafts" when asked. */
export function visibleRows(rows: PrRow[], filterId: string, query: string, showDrafts: boolean): PrRow[] {
  const filter = FILTERS.find((f) => f.id === filterId) ?? FILTERS[0]!;
  return rows.filter((r) => filter.test(r) && matchesQuery(r, query) && (showDrafts || !r.is_draft || filterId === 'draft'));
}

export function filterCounts(rows: PrRow[], showDrafts: boolean): Record<string, number> {
  const out: Record<string, number> = {};
  for (const f of FILTERS) out[f.id] = visibleRows(rows, f.id, '', showDrafts).length;
  return out;
}

/** Sums the numeric columns across `rows`; a column no row has a value for stays null. */
export function totals(rows: RepoRow[]): Record<string, number | null> {
  const out: Record<string, number | null> = {};
  for (const c of REPO_COLUMNS) {
    if (c.text || c.group === 'Age') continue;
    let sum: number | null = null;
    for (const r of rows) {
      const v = c.value(r);
      if (typeof v === 'number') sum = (sum ?? 0) + v;
    }
    out[c.key] = sum;
  }
  return out;
}

export const shortName = (repo: string): string => repo.split('/').pop() ?? repo;

/** How long ago `iso` was, tersely: "12m", "3h", "5d". */
export function ago(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return '—';
  const secs = Math.max(0, (now - Date.parse(iso)) / 1000);
  if (secs < 60) return 'now';
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  if (secs < 86400 * 60) return `${Math.floor(secs / 86400)}d`;
  return `${Math.floor(secs / (86400 * 30))}mo`;
}

/** 1234 → "1.2k". */
export function compact(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10000) return `${(n / 1000).toFixed(1).replace(/\.0$/, '')}k`;
  return `${Math.round(n / 1000)}k`;
}

export function statePill(status: Status, repos: RepoRow[], now = Date.now()): { cls: string; text: string; title: string } {
  if (status.paused_until && Date.parse(status.paused_until) > now) {
    return { cls: 'paused', text: 'PAUSED', title: status.error ?? 'Fetching is paused' };
  }
  const stale = status.stale_after_secs > 0 && repos.some((r) => r.listed && r.list_at
    && (now - Date.parse(r.list_at)) / 1000 > status.stale_after_secs);
  if (status.fetching) return { cls: 'working', text: 'FETCHING', title: status.fetching };
  if (stale) return { cls: 'stale', text: 'STALE', title: 'Some repos have gone too long without a successful fetch' };
  return { cls: 'fresh', text: 'FRESH', title: 'Up to date' };
}

export const usd = (n: number): string => `$${n.toFixed(2)}`;

/** The footer's sentence about Claude. */
export function claudeLine(c: ClaudeStatus): { text: string; bad: boolean; title: string } {
  if (!c.enabled) return { text: 'Claude off', bad: false, title: 'claude.enabled is false in the config' };
  if (!c.available) return { text: 'Claude CLI not found', bad: true, title: 'Set claude.binary in the config, or install Claude Code' };
  const spent = `${usd(c.spent_today)} of ${usd(c.max_usd_per_day)} today`;
  if (c.last_error) return { text: `Claude failed · ${spent}`, bad: true, title: c.last_error };
  if (c.running) return { text: `Claude: ${c.running} · ${spent}`, bad: false, title: c.model };
  return { text: `Claude ${spent}`, bad: false, title: `${c.model}, ${c.runs_last_hour} of ${c.max_runs_per_hour} calls this hour` };
}

/** Longest list that's worth rendering at once; more arrive on request. */
export const PAGE_SIZE = 300;
