import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  sortEntries, manualOrder, orderedEntries, nextSort, dropIndex, MANUAL, versionKey, freshness, isStale, ago, compact, tokenStatus,
  totals, urgentAcross, isRefreshing, defaultDir, value, windowById, WINDOWS, COLUMNS, COLUMN,
} from './model.ts';
import type { Activity, RepoEntry, RepoStats, UrgentIssue } from './model.ts';

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
  assert.deepEqual(versionKey('v0.10.0-rc.2'), [0, 10, 0, -1, 'rc', '2']);
  assert.deepEqual(versionKey('V2'), [2, 0, 0, 0]);
  assert.deepEqual(versionKey('1.2.3-beta'), [1, 2, 3, -1, 'beta']);
  assert.deepEqual(versionKey('nightly'), [-1]);
});

test('build versions compare every prerelease identifier before their release', () => {
  const tags = ['v1.0.0-1', 'v1.0.0-9007199254740992', 'v1.0.0-9007199254740993',
    'v1.0.0-alpha', 'v1.0.0-alpha.1', 'v1.0.0-alpha.beta',
    'v1.0.0-beta', 'v1.0.0-beta.2', 'v1.0.0-beta.11', 'v1.0.0-rc.1', 'v1.0.0'];
  const builds = tags.map((latest_build, i) => entry(String(tags.length - i), { latest_build }));
  /** Read build tags back in the requested table order. */
  const sortedTags = (dir: 'asc' | 'desc') => sortEntries(builds, 'latest_build', dir).map((r) => r.stats!.latest_build);
  assert.deepEqual(sortedTags('asc'), tags);
  assert.deepEqual(sortedTags('desc'), [...tags].reverse());
});

test('build metadata does not change version precedence, and short tags still work', () => {
  const builds = [entry('b', { latest_build: 'V2+build.1' }), entry('a', { latest_build: '2.0.0+build.9' }),
    entry('c', { latest_build: 'v2.0-rc.1+build.99' }), entry('d', { latest_build: 'nightly' }), entry('e', null)];
  assert.equal(names(sortEntries(builds, 'latest_build', 'desc')), 'abcde');
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

test('columns: unique keys, every recent column titled with its span', () => {
  assert.equal(Object.keys(COLUMN).length, COLUMNS.length);
  for (const c of COLUMNS.filter((c) => c.recent)) assert.match(c.title ?? '', /\{span\}/, c.key);
  assert.equal(COLUMNS[0]!.key, 'name', 'the sticky first column is the repo');
});
