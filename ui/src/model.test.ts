import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  sortEntries, manualOrder, orderedEntries, nextSort, dropIndex, MANUAL, versionKey, freshness, isStale, ago, compact, tokenStatus,
  totals, urgentAcross, isRefreshing, isThirdParty, defaultDir, value, windowById, WINDOWS, COLUMNS, COLUMN,
} from './model.ts';
import type { Activity, RepoEntry, RepoStats, UrgentIssue } from './model.ts';

import { figurePct, formatPct, parityTip, parityUrl } from './model.ts';
import type { Figure, Parity } from './model.ts';
import {
  allGaps, areaStrip, audienceFigure, betaUnknownWhy, blocksLabel, changedAt, checklistSummary, dimsByDrag, docUrl, gapTotal, heatMeans, hoursText, kindSums,
  nextGapSort, parityBadge, parityNextSort, parityOrder, partText, reviewAgeDays, sourceUrl, sumText, whyMissing, NO_GAP_FILTER,
} from './model.ts';
import type { Area, Checklist, Dim, DocFile, Gap, Heat, Hours } from './model.ts';

const entry = (repo: string, stats: Partial<RepoStats> | null, fetched_at: string | null = '2026-10-08T12:00:00Z'): RepoEntry =>
  ({ repo: `o/${repo}`, stats: stats as RepoStats | null, fetched_at });

const issue = (number: number, score: number, critical = false, created_at: string | null = null): UrgentIssue => ({
  number, score, critical, created_at, title: `#${number}`, url: '', updated_at: null, comments: 0, reactions: 0, labels: [], reasons: [],
});

const rows = [
  entry('b', { urgency: 10, latest_build: 'v0.10.0', last_commit_at: '2026-10-08T10:00:00Z' }),
  entry('a', { urgency: 50, latest_build: 'v0.9.0', last_commit_at: '2026-10-08T11:00:00Z' }),
  entry('c', null, null),
  entry('d', { urgency: 50, latest_build: 'v0.10.0-rc.2', last_commit_at: null }),
];

const names = (list: RepoEntry[]): string => list.map((e) => e.repo.slice(2)).join('');

test('numbers sort descending with ties by name, missing last', () => {
  assert.equal(names(sortEntries(rows, 'urgency', 'desc')), 'adbc');
  assert.equal(names(sortEntries(rows, 'urgency', 'asc')), 'badc');
});

test('sorting by an unknown column falls back to urgency', () => {
  assert.equal(names(sortEntries(rows, 'bogus', 'desc')), 'adbc');
});

test('sorting leaves its input alone', () => {
  const before = names(rows);
  sortEntries(rows, 'name', 'asc');
  manualOrder(rows, ['o/d']);
  assert.equal(names(rows), before);
});

test('versions sort semantically, prereleases below their release', () => {
  assert.equal(names(sortEntries(rows, 'latest_build', 'desc')), 'bdac');
  assert.deepEqual(versionKey('v0.10.0-rc.2'), [0, 10, 0, -1, 2]);
  assert.deepEqual(versionKey('V2'), [2, 0, 0, 0, 0]);
  assert.deepEqual(versionKey('1.2.3-beta'), [1, 2, 3, -1, 0]);
  assert.deepEqual(versionKey('nightly'), [-1]);
});

test('commits since the build read through to since_build', () => {
  const lagged = [
    entry('a', { since_build: { commits: 3, basis: 'exact', from_sha: 'x' } }),
    entry('b', { since_build: { commits: 40, basis: 'branched', from_sha: null } }),
    entry('c', { since_build: null }),
    entry('d', null, null),
  ];
  assert.equal(names(sortEntries(lagged, 'commits_since_build', 'desc')), 'bacd');
  assert.equal(value(lagged[2]!, 'commits_since_build'), null);
  assert.equal(totals(lagged).commits_since_build, 43);
});

test('dates and names', () => {
  assert.equal(names(sortEntries(rows, 'last_commit_at', 'desc')), 'abcd');
  assert.equal(names(sortEntries(rows, 'name', 'asc')), 'abcd');
  assert.equal(names(sortEntries(rows, 'name', 'desc')), 'dcba');
  assert.equal(defaultDir('name'), 'asc');
  assert.equal(defaultDir('downloads_total'), 'desc');
  assert.equal(defaultDir('oldest_open_pr_at'), 'asc');
});

test('data age sorts by the exact fetch time, whatever the coarse label', () => {
  const at = [
    entry('a', {}, '2026-10-08T10:04:00Z'),
    entry('b', {}, '2026-10-08T10:05:00Z'),
    entry('c', null, null),
    entry('d', null, '2026-10-08T08:00:00Z'),
  ];
  const now = Date.parse('2026-10-08T12:00:00Z');
  assert.equal(ago(at[0]!.fetched_at, now), ago(at[1]!.fetched_at, now), '1 h 56 m and 1 h 55 m both read 1h');
  assert.equal(names(sortEntries(at, 'fetched', 'asc')), 'badc', 'ascending: youngest first');
  assert.equal(names(sortEntries(at, 'fetched', 'desc')), 'dabc');
  assert.equal(defaultDir('fetched'), 'desc', 'stalest first on the first click');
  assert.equal(COLUMNS[1]!.key, 'fetched');
});

test('names compare numerically and ignore case', () => {
  const e = [entry('app10', {}), entry('App2', {}), entry('app1', {})];
  assert.deepEqual(sortEntries(e, 'name', 'asc').map((x) => x.repo), ['o/app1', 'o/App2', 'o/app10']);
});

test('value is null for missing stats and fields, and for non-scalar fields', () => {
  assert.equal(value(entry('x', null), 'urgency'), null);
  assert.equal(value(entry('x', {}), 'urgency'), null);
  assert.equal(value(entry('x', { urgent: [] }), 'urgent'), null);
  assert.equal(value(entry('x', { urgency: 0 }), 'urgency'), 0);
  assert.equal(value({ repo: 'solo', stats: null, fetched_at: null }, 'name'), 'solo');
});

test('manual order follows the saved list, new repos last in config order', () => {
  assert.equal(names(manualOrder(rows, ['o/d', 'o/b'])), 'dbac');
  assert.equal(names(manualOrder(rows, ['o/gone', 'o/c'])), 'cbad');
  assert.equal(names(manualOrder(rows, [])), 'bacd');
});

test('the table order is manual or by column', () => {
  assert.equal(names(orderedEntries(rows, { key: MANUAL }, ['o/c', 'o/a'])), 'cabd');
  assert.equal(names(orderedEntries(rows, { key: 'urgency', dir: 'desc' }, ['o/c', 'o/a'])), 'adbc');
});

test('clicking a header flips its direction, or starts a new column at its default', () => {
  assert.deepEqual(nextSort({ key: 'urgency', dir: 'desc' }, 'urgency'), { key: 'urgency', dir: 'asc' });
  assert.deepEqual(nextSort({ key: 'urgency', dir: 'asc' }, 'urgency'), { key: 'urgency', dir: 'desc' });
  assert.deepEqual(nextSort({ key: 'urgency', dir: 'desc' }, 'name'), { key: 'name', dir: 'asc' });
  assert.deepEqual(nextSort({ key: MANUAL }, 'urgency'), { key: 'urgency', dir: 'desc' });
  assert.deepEqual(nextSort({ key: MANUAL }, 'oldest_open_pr_at'), { key: 'oldest_open_pr_at', dir: 'asc' });
});

test('a dragged row drops before the first row whose middle is below the pointer', () => {
  const mids = [10, 30, 50];
  assert.equal(dropIndex(mids, 0), 0);
  assert.equal(dropIndex(mids, 10), 1, 'exactly on a midpoint goes after it');
  assert.equal(dropIndex(mids, 29), 1);
  assert.equal(dropIndex(mids, 49), 2);
  assert.equal(dropIndex(mids, 99), 3, 'past the last row');
  assert.equal(dropIndex([], 5), 0);
});

test('freshness', () => {
  const now = Date.parse('2026-10-08T12:10:00Z');
  const snap = { stale_after_secs: 900, repos: [entry('a', {}), entry('b', {}, '2026-10-08T12:05:00Z')] };
  assert.deepEqual(freshness(snap, now), { oldest: Date.parse('2026-10-08T12:00:00Z'), stale: false });
  assert.equal(freshness(snap, now + 6 * 60_000).stale, true);
  assert.equal(freshness({ ...snap, repos: [...snap.repos, entry('x', null, null)] }, now).oldest, null);
  assert.equal(freshness({ stale_after_secs: 1, repos: [] }, now).stale, true);
});

test('a repo is stale once its data is older than the limit, or never fetched', () => {
  const now = Date.parse('2026-10-08T12:10:00Z');
  assert.equal(isStale(entry('a', {}), 600, now), false, 'exactly at the limit');
  assert.equal(isStale(entry('a', {}), 599, now), true);
  assert.equal(isStale(entry('a', null, null), 1e9, now), true);
});

test('formatting', () => {
  const now = Date.parse('2026-10-08T12:00:00Z');
  assert.equal(ago('2026-10-08T11:59:55Z', now), 'now');
  assert.equal(ago('2026-10-08T11:59:15Z', now), '45s');
  assert.equal(ago('2026-10-08T11:58:00Z', now), '2m');
  assert.equal(ago('2026-10-08T09:00:00Z', now), '3h');
  assert.equal(ago('2026-10-06T12:00:00Z', now), '2d');
  assert.equal(ago('2024-10-01T12:00:00Z', now), '2y');
  assert.equal(ago('2026-10-08T12:05:00Z', now), 'now', 'clock skew never goes negative');
  assert.equal(ago(null, now), '');
  assert.equal(compact(279524), '280k');
  assert.equal(compact(57247), '57.2k');
  assert.equal(compact(950), '950');
  assert.equal(compact(2_340_000), '2.3M');
  assert.equal(compact(null), '');
});

test('totals and urgent merge', () => {
  assert.equal(totals(rows).urgency, 110);
  assert.equal(totals(rows).latest_build, undefined, 'versions have no total');
  const e = [
    entry('a', { urgent: [issue(1, 30), issue(2, 80, true)] }),
    entry('b', { urgent: [issue(3, 50, true)] }),
  ];
  assert.deepEqual(urgentAcross(e).map((u) => u.number), [2, 3, 1]);
  assert.deepEqual(urgentAcross(e, { criticalOnly: true }).map((u) => u.repo), ['o/a', 'o/b']);
  assert.deepEqual(urgentAcross(e, { repo: 'o/b' }).map((u) => u.number), [3]);
  assert.deepEqual(urgentAcross(e, { limit: 2 }).map((u) => u.number), [2, 3]);
});

test('urgent ties go to the newer issue', () => {
  const e = [entry('a', { urgent: [issue(1, 40, false, '2026-10-01T00:00:00Z'), issue(2, 40, false, '2026-10-05T00:00:00Z')] })];
  assert.deepEqual(urgentAcross(e).map((u) => u.number), [2, 1]);
});

test('recent-activity columns follow the selected window', () => {
  const act = (commits: number, prs_merged: number, people = 0): Activity => ({ commits, prs_opened: 0, prs_merged, issues_opened: 0, people });
  const e = [
    entry('a', { recent: { '4h': act(5, 1, 2), '7d': act(9, 40), all: act(1234, 300, 12) } }),
    entry('b', { recent: { '4h': act(8, 2), '7d': act(8, 10) } }),
    entry('old', { commits_4h: 99 } as Partial<RepoStats>),
  ];
  const [a, b, old] = e as [RepoEntry, RepoEntry, RepoEntry];
  assert.equal(value(a, 'recent_commits'), 5, 'defaults to 4 h');
  assert.equal(value(a, 'recent_merged', '7d'), 40);
  assert.equal(value(a, 'recent_commits', '10m'), null, 'window not fetched yet');
  assert.equal(value(old, 'recent_commits'), null, 'cache from before windows');
  assert.equal(names(sortEntries(e, 'recent_commits', 'desc', '4h')), 'baold');
  assert.equal(names(sortEntries(e, 'recent_commits', 'desc', '7d')), 'abold');
  assert.equal(totals(e, '7d').recent_merged, 50);
  assert.equal(windowById('bogus').id, '4h');
  assert.equal(value(a, 'recent_people'), 2);
  assert.equal(value(a, 'recent_people', 'all'), 12, 'all time is just another window');
  assert.equal(value(b, 'recent_commits', 'all'), null, 'cache from before all time');
  assert.equal(names(sortEntries(e, 'recent_merged', 'desc', 'all')), 'abold');
  assert.equal(value(a, 'recent_commits', 'release'), null, 'no release');
  assert.deepEqual(WINDOWS.map((w) => w.id), ['10m', '30m', '1h', '4h', '12h', '1d', '7d', 'release', 'all']);
});

test('a row is refreshing while its repo is being fetched or queued', () => {
  assert.equal(isRefreshing({ fetching: 'o/a', requested: ['o/b'] }, 'o/a'), true);
  assert.equal(isRefreshing({ fetching: 'o/a', requested: ['o/b'] }, 'o/b'), true);
  assert.equal(isRefreshing({ fetching: 'o/a', requested: ['o/b'] }, 'o/c'), false);
  assert.equal(isRefreshing({}, 'o/a'), false, 'a snapshot from an older build');
});

test('token status', () => {
  assert.deepEqual(tokenStatus(null), { text: '', title: '', bad: false });
  assert.deepEqual(tokenStatus(undefined), { text: '', title: '', bad: false });
  assert.equal(tokenStatus({ source: '$GITHUB_TOKEN', rejected: false }).text, 'Token: $GITHUB_TOKEN');
  assert.equal(tokenStatus({ source: '$GITHUB_TOKEN', rejected: false }).bad, false);
  assert.deepEqual(
    { ...tokenStatus({ source: 'config', rejected: true }), title: '' },
    { text: 'Token rejected (config)', title: '', bad: true },
  );
  assert.equal(tokenStatus({ source: null, rejected: false }).text, 'No GitHub token');
  assert.equal(tokenStatus({ source: null, rejected: false }).bad, true);
});

// ───────────────────────────── parity ─────────────────────────────

const figure = (pct: number, extra: Partial<Extract<Figure, { state: 'ok' }>> = {}): Figure => ({
  state: 'ok', pct, low: null, high: null, label: 'Feature breadth', text: `~${pct}%`, kind: 'estimated', basis: 'estimated',
  path: 'ROADMAP.md', line: 7, from: null, ...extra,
});

const unread = { state: 'absent', why: 'Not read yet' } as const;

const parity = (p: Partial<Parity> = {}): Parity => ({
  version: 1, commit: 'c0ffee', roadmap_oid: 'r1', parity_doc: true, na: null, breadth: figure(80), ready: figure(50),
  last_reviewed: '2026-10-10', breadth_alts: [], menu_breadth: unread, menu_hint: null, mainstream: unread, essentials: unread,
  to_alpha: unread, to_beta: unread, to_full: unread, stage: unread, files: [], notes: [],
  breadth_links: [], reviewed: null, dims: [], heat: [], dims_total: unread, top_drag: unread, areas: [], areas_total: unread,
  area_alts: [], area_weight_sum: null, area_weights_why: null, gaps: [], checklists: [], docs: [],
  gap_totals: { beta: null, partial: null, alpha: 0, alpha_none: false, stated: unread, ids_stable: false, form: '', beta_unknown: true },
  ...p,
});

test('parity columns sort by percentage with missing numbers last, and have no total', () => {
  const absent: Figure = { state: 'absent', why: 'No ROADMAP.md at HEAD' };
  const list: RepoEntry[] = [
    { ...entry('a', {}), parity: parity({ ready: figure(61) }) },
    { ...entry('b', {}), parity: parity({ breadth: figure(81.1), ready: absent }) },
    { ...entry('c', {}), parity: null },
    { ...entry('d', {}), parity: parity({ ready: figure(70) }) },
  ];
  assert.equal(defaultDir('parity_ready'), 'desc');
  assert.equal(names(sortEntries(list, 'parity_ready', 'desc')), 'dabc');
  assert.equal(names(sortEntries(list, 'parity_ready', 'asc')), 'adbc');
  assert.equal(names(sortEntries(list, 'parity_breadth', 'desc')), 'badc');
  assert.equal(value(list[1]!, 'parity_breadth'), 81.1);
  assert.equal(value(list[1]!, 'parity_ready'), null);
  assert.equal(value(list[2]!, 'parity_ready'), null, 'not read yet');
  assert.equal(figurePct({ state: 'unreadable', why: 'x', raw: 'most', path: 'ROADMAP.md', line: 3 }), null);
  const t = totals(list);
  assert.equal(t.parity_ready, undefined, 'percentages are not summed');
  assert.equal(t.parity_breadth, undefined);
  assert.equal(formatPct(76.96), '77%');
  assert.equal(formatPct(81.1), '81.1%');
});

test('a parity tooltip gives the row, its basis and the review date, or why there is no number', () => {
  const ok = parity({
    ready: figure(75, { label: 'Feature depth (weighted by use)', text: '~75%', kind: 'estimated', from: 'Feature depth row (no Ready for real work row)' }),
    breadth: figure(88, { text: '~88%', kind: 'measured in part: menus 348/375', basis: 'partly' }),
  });
  assert.equal(parityTip(ok, 'breadth'), [
    'Feature breadth: ~88%', 'Partly measured · measured in part: menus 348/375',
    'ROADMAP.md last reviewed 2026-10-10', 'Click to open docs/target-app-parity.md on GitHub',
  ].join('\n'));
  assert.match(parityTip(ok, 'ready'), /^Feature depth \(weighted by use\): ~75%\nEstimated · estimated\nSource: Feature depth row \(no Ready for real work row\)\n/);
  const bad = parity({
    parity_doc: false, last_reviewed: null,
    breadth: { state: 'unreadable', why: 'No percentage in the Value cell of Feature breadth', raw: 'most of it', path: 'ROADMAP.md', line: 9 },
  });
  assert.equal(parityTip(bad, 'breadth'), [
    'No percentage in the Value cell of Feature breadth', 'ROADMAP.md line 9: most of it',
    'ROADMAP.md has no Last reviewed date', 'Click to open ROADMAP.md on GitHub',
  ].join('\n'));
  const why = 'No ROADMAP.md or docs/target-app-parity.md at HEAD';
  const none = parity({ roadmap_oid: null, parity_doc: false, na: why, breadth: { state: 'absent', why } });
  assert.equal(parityTip(none, 'breadth'), why);
  assert.equal(parityTip(null, 'breadth'), 'Not read yet');
});

test('a parity cell links to the parity document at the commit read, else to its ROADMAP.md row', () => {
  const p = parity();
  assert.equal(parityUrl('o/app', p, p.breadth), 'https://github.com/o/app/blob/c0ffee/docs/target-app-parity.md');
  const roadmapOnly = parity({ parity_doc: false });
  assert.equal(parityUrl('o/app', roadmapOnly, roadmapOnly.breadth), 'https://github.com/o/app/blob/c0ffee/ROADMAP.md?plain=1#L7');
  const absent: Figure = { state: 'absent', why: 'x' };
  assert.equal(parityUrl('o/app', roadmapOnly, absent), 'https://github.com/o/app/blob/c0ffee/ROADMAP.md');
  assert.equal(parityUrl('o/app', parity({ parity_doc: false, roadmap_oid: null }), absent), null);
  assert.equal(parityUrl('o/app', parity({ commit: null }), absent), 'https://github.com/o/app/blob/HEAD/docs/target-app-parity.md');
});

// ───────────────────────────── parity tab ─────────────────────────────

const hours = (lo: number, hi: number): Hours => ({ lo, hi, approx: false, basis: null, note: null, raw: `${lo}–${hi} h`, counted_in: null });
const at = (line: number) => ({ path: 'docs/gaps.md', line });

const gap = (id: string, g: Partial<Gap> = {}): Gap => ({
  key: id, id, rank: 0, group: null, title: `Gap ${id}`, alpha: 'no', beta: 'no', blocks_src: null, scope_note: null, priority: null,
  kinds: [], hours: unread, counted_in: null, doc: null, workstream: null, src: at(3), detail_src: null, detail: null, closed: false,
  paired_with: [], issues: [], ...g,
});

const doc = (path: string, d: Partial<DocFile> = {}): DocFile => ({
  path, oid: `${path}-oid`, reviewed: null, updated: null, generated: null, target: null, date_lines: 1, line: 1, why: null,
  commit_date: null, commit: null, ...d,
});

const app = (name: string, p: Partial<Parity> | null, extra: Partial<RepoEntry> = {}): RepoEntry =>
  ({ repo: `o/${name}`, stats: null, fetched_at: null, parity: p && parity(p), ...extra });

test('the Apps rows put repos without parity documents last, then sort with missing values last', () => {
  const rows = [
    app('none', { na: 'No ROADMAP.md', ready: { state: 'absent', why: 'No ROADMAP.md' } }),
    app('low', { ready: figure(30) }),
    app('unread', null),
    app('blank', { ready: unread }),
    app('high', { ready: figure(70), mainstream: figure(90) }),
    app('gone', { ready: figure(99) }, { inaccessible: true }),
  ];
  const order = (key: string, dir: 'asc' | 'desc', audience: 'full' | 'mainstream' = 'full') =>
    parityOrder(rows, { key, dir }, audience).map((e) => e.repo.slice(2));
  assert.deepEqual(order('ready', 'desc'), ['high', 'low', 'blank', 'gone', 'none', 'unread']);
  assert.deepEqual(order('ready', 'asc'), ['low', 'high', 'blank', 'gone', 'none', 'unread'], 'missing values stay last');
  assert.deepEqual(order('ready', 'desc', 'mainstream').slice(0, 3), ['high', 'blank', 'low'], 'only high has a Mainstream figure');
  assert.deepEqual(order('app', 'asc'), ['blank', 'high', 'low', 'gone', 'none', 'unread']);
  assert.deepEqual(parityNextSort({ key: 'ready', dir: 'desc' }, 'ready'), { key: 'ready', dir: 'asc' });
  assert.deepEqual(parityNextSort({ key: 'ready', dir: 'desc' }, 'to_beta'), { key: 'to_beta', dir: 'asc' }, 'fewest hours first');
  assert.deepEqual(parityNextSort({ key: 'ready', dir: 'desc' }, 'breadth'), { key: 'breadth', dir: 'desc' });
});

test('All gaps orders by blocking band, then rank across apps, and filters', () => {
  const rows = [
    app('a', { gaps: [
      gap('A1', { rank: 1, beta: 'yes', hours: { state: 'ok', v: hours(10, 20), src: at(5) }, kinds: ['feature'] }),
      gap('A2', { rank: 2, alpha: 'yes' }),
      gap('A3', { rank: 3, closed: true }),
      gap('A4', { group: 'Later', kinds: ['codec', 'performance'] }),
    ] }),
    app('b', { gaps: [
      gap('B1', { rank: 1, beta: 'yes', hours: { state: 'ok', v: hours(40, 80), src: at(5) }, title: 'Color management' }),
      gap('B2', { rank: 2, beta: { partial: 'the CMYK half' } }),
    ] }),
    app('c', { na: 'No ROADMAP.md or docs/target-app-parity.md at HEAD', gaps: [gap('C1', { alpha: 'yes' })] }),
  ];
  const ids = (list: Gap[]) => list.map((g) => g.id);
  assert.deepEqual(ids(allGaps(rows)), ['A2', 'B1', 'A1', 'B2', 'A4'], 'alpha, beta by rank then hours, part, the rest; closed hidden');
  assert.deepEqual(ids(allGaps(rows, { ...NO_GAP_FILTER, hideClosed: false })), ['A2', 'B1', 'A1', 'B2', 'A3', 'A4']);
  assert.deepEqual(ids(allGaps(rows, { ...NO_GAP_FILTER, betaOnly: true })), ['B1', 'A1']);
  assert.deepEqual(ids(allGaps(rows, { ...NO_GAP_FILTER, repo: 'o/b' })), ['B1', 'B2']);
  assert.deepEqual(ids(allGaps(rows, { ...NO_GAP_FILTER, kind: 'other' })), ['A2', 'B1', 'B2']);
  assert.deepEqual(ids(allGaps(rows, { ...NO_GAP_FILTER, minHours: 30 })), ['B1']);
  assert.deepEqual(ids(allGaps(rows, { ...NO_GAP_FILTER, text: 'color' })), ['B1']);
  assert.deepEqual(ids(allGaps(rows, NO_GAP_FILTER, { key: 'hours', dir: 'desc' })).slice(0, 2), ['B1', 'A1']);
  assert.deepEqual(ids(allGaps(rows, NO_GAP_FILTER, { key: 'hours', dir: 'desc' })), ['B1', 'A1', 'A2', 'A4', 'B2'], 'gaps without hours last');
  assert.deepEqual(ids(allGaps(rows, NO_GAP_FILTER, { key: 'hours', dir: 'asc' })), ['A1', 'B1', 'A2', 'A4', 'B2'], 'gaps without hours last');
  assert.deepEqual(ids(allGaps(rows, NO_GAP_FILTER, { key: 'rank', dir: 'desc' })), ['A2', 'B2', 'A1', 'B1', 'A4'], 'unranked last');
  assert.deepEqual(nextGapSort(null, 'hours'), { key: 'hours', dir: 'desc' });
  assert.deepEqual(nextGapSort({ key: 'hours', dir: 'desc' }, 'hours'), { key: 'hours', dir: 'asc' });
  assert.equal(nextGapSort({ key: 'hours', dir: 'asc' }, 'hours'), null, 'the third click restores the default order');
  assert.deepEqual(nextGapSort(null, 'app'), { key: 'app', dir: 'asc' });
  assert.equal(parityBadge(rows), 3, 'A1, A2 and B1 block; closed, partial and n/a gaps do not count');
});

test('gap sums leave out closed gaps and add no hours twice for a gap counted inside another', () => {
  const ok = (lo: number, hi: number) => ({ state: 'ok' as const, v: hours(lo, hi), src: at(4) });
  const gaps = allGaps([
    app('a', { gaps: [
      gap('G1', { hours: ok(10, 20), kinds: ['feature'] }),
      gap('G2', { hours: ok(5, 5), kinds: ['feature'], counted_in: 'G1' }),
      gap('G3', { kinds: ['feature', 'codec'] }),
      gap('G4', { hours: ok(100, 100), closed: true }),
    ] }),
    app('b', { gaps: [gap('G5', { hours: ok(1, 2), kinds: ['codec'] })] }),
  ]);
  assert.deepEqual(gapTotal(gaps), { count: 3, lo: 11, hi: 22, unknown: 1 });
  assert.deepEqual(kindSums(gaps), [
    { kind: 'feature', count: 3, lo: 10, hi: 20, unknown: 1, apps: 1 },
    { kind: 'codec', count: 2, lo: 1, hi: 2, unknown: 1, apps: 2 },
  ]);
  assert.equal(hoursText(hours(1200, 1500)), '1,200–1,500 h');
  assert.equal(hoursText(hours(3, 3)), '3 h');
  assert.equal(hoursText(null), '—');
  assert.equal(sumText({ count: 3, lo: 10, hi: 20, unknown: 1 }), '10–20 h + 1 unestimated');
  assert.equal(partText({ count: 3, lo: 10, hi: 1200, unknown: 1 }), 'part of 3 gaps (up to 1,200 h, 1 unestimated)');
  assert.equal(partText({ count: 2, lo: 4, hi: 8, unknown: 0 }), 'part of 2 gaps (up to 8 h)');
  assert.equal(partText({ count: 2, lo: 0, hi: 0, unknown: 2 }), 'part of 2 gaps (hours unknown)');
});

test('the audience picker reads the readiness figure of its audience', () => {
  const p = parity({ ready: figure(50), mainstream: figure(70), essentials: figure(90) });
  assert.equal(figurePct(audienceFigure(p, 'full')), 50);
  assert.equal(figurePct(audienceFigure(p, 'mainstream')), 70);
  assert.equal(figurePct(audienceFigure(p, 'essentials')), 90);
});

test('a gap flag says what it blocks', () => {
  assert.deepEqual(blocksLabel(gap('x', { alpha: 'yes' })), { text: 'A', title: 'Blocks alpha' });
  assert.deepEqual(blocksLabel(gap('x', { alpha: { partial: 'import' } })), { text: 'A part', title: 'Blocks alpha in part: import' });
  assert.deepEqual(blocksLabel(gap('x', { beta: 'yes', scope_note: 'Windows only' })), { text: 'B', title: 'Blocks beta (Windows only)' });
  assert.equal(blocksLabel(gap('x', { beta: { partial: 'export' } })).text, 'B part');
  assert.deepEqual(blocksLabel(gap('x', { beta: 'unknown' })), { text: '', title: 'The document does not say whether this blocks beta' });
  assert.deepEqual(blocksLabel(gap('x')), { text: '', title: 'Does not block beta' });
});

test('the checklist cell sums each kind of machine checklist', () => {
  const counts = (name: string, pct: number | null) =>
    ({ name, done: 0, partial: 0, missing: 0, excluded: 0, ahead: 0, not_measured: 0, unrecognized: 0, pct });
  const tiered: Checklist = { kind: 'tiered', path: 'parity/features.toml', tiers: [counts('P0', 98.4), counts('P1', 95), counts('untiered', 10)], overall: 96, rows: [], more: 0 };
  const presence: Checklist = { kind: 'presence', path: 'docs/menus.md', total: { state: 'ok', v: { done: 97, total: 100 }, src: { path: 'docs/menus.md', line: 2 } }, groups: [], missing: [], more: 0 };
  const status: Checklist = { kind: 'status', path: 'docs/scorecard.md', groups: [], overall: 75, rows: [], more: 0 };
  assert.deepEqual(checklistSummary(parity({ checklists: [tiered] })), { text: 'P0 98 · P1 95', title: 'parity/features.toml: 96% overall' });
  assert.equal(checklistSummary(parity({ checklists: [presence] })).text, '97% menus');
  assert.equal(checklistSummary(parity({ checklists: [presence, status] })).text, 'menus 97% · scorecard 75%');
  assert.deepEqual(checklistSummary(parity()), { text: '—', title: 'No machine checklist in this repo' });
});

test('the area strip widens each area by its weight and fills it to its ready', () => {
  const score = (value: number) => ({ state: 'ok' as const, v: { kind: 'pct' as const, value, approx: false, low: null, high: null, basis: 'estimated' as const, raw: `${value}%`, note: null, computed: false }, src: { path: 'ROADMAP.md', line: 9 } });
  const area = (name: string, weight: number | null, ready: number, a: Partial<Area> = {}): Area => ({
    name, full_name: name, key: name.toLowerCase(), weight, weight_src: null, breadth: unread, ready: score(ready), ready_from: null,
    hours: unread, missing: [], more: 0, chips_from: 'none', raw_gaps: '', gaps_header: null, unweighted: false, dup_lines: [],
    conflict: null, kind: '', doc: null, counts: null, src: { path: 'ROADMAP.md', line: 9 }, shortfall: null, ...a,
  });
  const strip = areaStrip(parity({ areas: [area('Text', 1, 40), area('Paths', 3, 80), area('Plugins', 5, 10, { unweighted: true })] }));
  assert.deepEqual(strip.map((s) => [s.name, s.width, s.fill]), [['Paths', 75, 80], ['Text', 25, 40]], 'heaviest first; unweighted left out');
  assert.match(strip[0]!.tip, /Weight 3 of 4/);
  const equal = areaStrip(parity({ areas: [area('Text', null, 40), area('Paths', null, 80)] }));
  assert.deepEqual(equal.map((s) => s.width), [50, 50]);
  assert.match(equal[0]!.tip, /equal widths/);
  const breadthOnly = areaStrip(parity({ areas: [area('Text', 1, 0, { ready: unread, breadth: score(90) })] }));
  assert.equal(breadthOnly[0]!.fill, null, 'breadth does not stand in for ready');
  assert.deepEqual(areaStrip(parity()), []);
});

test('unknown beta blockers say why', () => {
  const gapsDoc = [doc('docs/gaps.md')];
  assert.equal(betaUnknownWhy(parity()), 'No docs/gaps.md at HEAD');
  assert.equal(betaUnknownWhy(parity({ files: gapsDoc })), 'No gaps read from docs/gaps.md');
  assert.equal(betaUnknownWhy(parity({ files: gapsDoc, gaps: [gap('G1', { beta: 'unknown' })] })), 'docs/gaps.md does not say which gaps block beta');
});

test('review age, last change, dimension means and source links', () => {
  const now = Date.parse('2026-10-18T12:00:00Z');
  assert.equal(reviewAgeDays(parity({ reviewed: '2026-10-01' }), now), 17);
  assert.equal(reviewAgeDays(parity(), now), null);
  const files = [doc('ROADMAP.md', { commit_date: '2026-10-17T09:00:00Z' }), doc('docs/gaps.md', { commit_date: '2026-10-18T08:00:00Z' }), doc('docs/ui-parity.md')];
  assert.equal(changedAt(parity({ files })), '2026-10-18T08:00:00Z');
  assert.equal(changedAt(parity({ files: [doc('ROADMAP.md')] })), null, 'last commits not read');
  assert.equal(changedAt(parity({ files: [] })), null, 'no parity documents');
  assert.equal(changedAt(null), null, 'no parity');
  assert.equal(changedAt(parity({ files: [doc('ROADMAP.md', { commit_date: '2026-10-18T08:00:00Z' }), doc('docs/gaps.md', { commit_date: '2026-10-18T08:00:00Z' })] })), '2026-10-18T08:00:00Z', 'equal times');
  const heat = (column: Heat['column'], value: number | null): Heat => ({ column, value, text: value === null ? 'ahead' : `${value}%`, rows: [], src: null });
  const means = heatMeans([
    app('a', { heat: [heat('features', 80), heat('ai', null)] }),
    app('b', { heat: [heat('features', 61)] }),
    app('c', { na: 'none', heat: [heat('features', 0)] }),
  ]);
  assert.deepEqual(means.features, { mean: 70.5, n: 2 }, 'apps without parity documents are left out');
  assert.deepEqual(means.ai, { mean: null, n: 0 }, '"ahead" has no percentage');
  assert.equal(sourceUrl('o/app', 'c1', { path: 'docs/gaps.md', line: 12 }), 'https://github.com/o/app/blob/c1/docs/gaps.md?plain=1#L12');
  assert.equal(sourceUrl('o/app', null, { path: 'docs/a b.md', line: 0 }), 'https://github.com/o/app/blob/HEAD/docs/a%20b.md');
  assert.equal(whyMissing({ state: 'unreadable', why: 'No hours', raw: 'soon', src: at(8) }), 'No hours\ndocs/gaps.md:8: soon');
  assert.equal(whyMissing(null), 'Not read yet');
});

test('the detail pane lists dimensions by points lost, each ↳ row under its own', () => {
  const dim = (name: string, shortfall: number | null, child = false): Dim => ({
    name, group: 'features', weight: null, weight_src: null, ready: unread, breadth: unread, hours: unread, kind: '', doc: null,
    counted: null, child, fills: 'ready', only_in: null, src: { path: 'ROADMAP.md', line: 20 }, shortfall,
  });
  const { dims, byHours } = dimsByDrag([dim('Text', 2), dim('Raw', 6), dim('Cameras', null, true), dim('Lenses', null, true), dim('UI', 4)]);
  assert.deepEqual(dims.map((d) => d.name), ['Raw', 'Cameras', 'Lenses', 'UI', 'Text']);
  assert.equal(byHours, false);
});

test('a document link opens the section it names', () => {
  const link = (url: string, path: string | null) => ({ text: 'doc', url, path });
  assert.equal(docUrl('o/app', 'c1', link('raw-parity.md#colour', 'docs/raw-parity.md')), 'https://github.com/o/app/blob/c1/docs/raw-parity.md#colour');
  assert.equal(docUrl('o/app', 'c1', link('../ROADMAP.md', 'ROADMAP.md')), 'https://github.com/o/app/blob/c1/ROADMAP.md');
  assert.equal(docUrl('o/app', 'c1', link('https://example.com/spec', null)), null);
});

test('columns: unique keys, every recent column titled with its span', () => {
  assert.equal(Object.keys(COLUMN).length, COLUMNS.length);
  for (const c of COLUMNS.filter((c) => c.recent)) assert.match(c.title ?? '', /\{span\}/, c.key);
  assert.equal(COLUMNS[0]!.key, 'name', 'the sticky first column is the repo');
});

test('repos outside our org are third party', () => {
  assert.equal(isThirdParty('storytold/photocraft'), false);
  assert.equal(isThirdParty('Storytold/photocraft'), false, 'owners ignore case');
  assert.equal(isThirdParty('tangency-labs/solidcraft'), true);
});
