import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  sortEntries, manualOrder, orderedEntries, nextSort, dropIndex, MANUAL, versionKey, freshness, isStale, ago, compact, formatRate, buildRate, tokenStatus,
  totals, urgentAcross, isRefreshing, isThirdParty, isRepoHidden, toggleKind, reorderVisible, defaultDir, value, windowById, WINDOWS, COLUMNS, COLUMN,
  mainCiFailing, mainCiText, mainCiTip, mainCiUrl,
} from './model.ts';
import type { Activity, MainCi, MainRun, MainWorkflow, RepoEntry, RepoStats, UrgentIssue } from './model.ts';

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

test('the build download rate is downloads over the time out when they were counted', () => {
  const fetched = '2026-10-08T12:00:00Z';
  const built = (hoursAgo: number) => new Date(Date.parse(fetched) - hoursAgo * 3600e3).toISOString();
  const out = (latest_build_downloads: number, hoursAgo: number) => entry('x', { latest_build_downloads, latest_build_at: built(hoursAgo) }, fetched);
  assert.equal(buildRate(out(3600, 1)), 1);
  assert.equal(value(out(7200, 1), 'latest_build_rate'), 2);
  assert.equal(buildRate(out(0, 1)), 0, 'out, but no downloads yet');
  assert.equal(buildRate(entry('x', { latest_build_downloads: 0, latest_build_at: null }, fetched)), null, 'no build');
  assert.equal(buildRate(entry('x', null, fetched)), null, 'no stats');
  assert.equal(buildRate(entry('x', { latest_build_downloads: 5, latest_build_at: built(1) }, null)), null, 'never fetched');
  assert.equal(buildRate(out(5, 0)), null, 'no time out yet');
  assert.equal(buildRate(out(5, -1)), null, 'published after the fetch, by clock skew');
});

test('build download rates sort by speed across units, missing last', () => {
  const fetched = '2026-10-08T12:00:00Z';
  const dayAgo = '2026-10-07T12:00:00Z';
  const out = (repo: string, latest_build_downloads: number) => entry(repo, { latest_build_downloads, latest_build_at: dayAgo }, fetched);
  const e = [
    out('m', 8640), // 6.0/m
    out('s', 172800), // 2.0/s
    entry('none', { latest_build_downloads: 0, latest_build_at: null }, fetched),
    out('h', 120), // 5.0/h
    out('M', 86400 * 59 / 60), // 59.0/m, numerically above 2.0/s
    out('d', 3), // 3.0/d
    out('z', 0), // 0.0/d
  ];
  const order = (list: RepoEntry[]) => list.map((x) => x.repo.slice(2)).join(' ');
  assert.equal(defaultDir('latest_build_rate'), 'desc', 'fastest first on the first click');
  assert.equal(order(sortEntries(e, 'latest_build_rate', 'desc')), 's M m h d z none');
  assert.equal(order(sortEntries(e, 'latest_build_rate', 'asc')), 'z d h m M s none');
  assert.deepEqual(e.map((x) => { const r = buildRate(x); return r === null ? null : formatRate(r); }),
    ['6.0/m', '2.0/s', null, '5.0/h', '59.0/m', '3.0/d', '0.0/d']);
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
  assert.equal(formatRate(12.34), '12.3/s');
  assert.equal(formatRate(1), '1.0/s');
  assert.equal(formatRate(0.5), '30.0/m');
  assert.equal(formatRate(1 / 60), '1.0/m');
  assert.equal(formatRate(1 / 120), '30.0/h');
  assert.equal(formatRate(1 / 3600), '1.0/h');
  assert.equal(formatRate(1 / 7200), '12.0/d');
  assert.equal(formatRate(0), '0.0/d');
});

test('totals and urgent merge', () => {
  assert.equal(totals(rows).urgency, 110);
  assert.equal(totals(rows).latest_build_rate, undefined, 'rates have no total');
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

const mrun = (id: number, conclusion: string, r: Partial<MainRun> = {}): MainRun => ({
  run_id: 100 + id, conclusion, event: 'push', head_sha: `abcdef${id}0`, title: `Commit ${id}`, at: '2026-10-08T10:00:00Z',
  url: `https://github.com/o/a/actions/runs/${100 + id}`, ...r,
});
const wf = (id: number, ...runs: MainRun[]): MainWorkflow => ({ workflow_id: id, name: `W${id}`, path: `.github/workflows/w${id}.yml`, runs });
const ci = (workflows: MainWorkflow[]): MainCi => ({ branch: 'main', workflows, checked_at: '2026-10-08T12:00:00Z' });

test('the Main CI column counts the workflows whose latest result failed, sorts them first and adds them up', () => {
  assert.equal(mainCiFailing(null), null);
  assert.equal(mainCiFailing(ci([])), null, 'no result is not a pass');
  assert.equal(mainCiText(undefined), '—');
  assert.equal(mainCiText(ci([wf(1, mrun(1, 'success')), wf(2, mrun(2, 'neutral'))])), '✓ 2');
  const red = ci([wf(1, mrun(1, 'success')), wf(2, mrun(2, 'timed_out')), wf(3, mrun(3, 'startup_failure', { event: 'schedule' }))]);
  assert.equal(mainCiText(red), '✗ 2 of 3');
  assert.equal(mainCiText(ci([wf(1, mrun(1, 'action_required')), wf(2, mrun(2, 'stale'))])), '✗ 2 of 2', 'runs that did not pass');
  assert.equal(mainCiUrl('o/a', red), 'https://github.com/o/a/actions/runs/102');
  assert.equal(mainCiUrl('o/a', { ...ci([wf(1, mrun(1, 'success'))]), branch: 'trunk' }), 'https://github.com/o/a/actions?query=branch%3Atrunk');
  const withCi = (name: string, c: MainCi | null): RepoEntry => ({ ...entry(name, { urgency: 1 }), main_ci: c });
  const list = [withCi('green', ci([wf(1, mrun(1, 'success'))])), withCi('none', null), withCi('red', red), withCi('empty', ci([])), entry('old', { urgency: 1 })];
  assert.equal(defaultDir('main_ci'), 'desc');
  assert.deepEqual(sortEntries(list, 'main_ci', 'desc').map((e) => e.repo), ['o/red', 'o/green', 'o/empty', 'o/none', 'o/old'],
    'most failures first; no result, or a snapshot from before the column, last');
  assert.equal(value(list[2]!, 'main_ci'), 2);
  assert.equal(totals(list).main_ci, 2);
  assert.equal(COLUMNS.at(-1)!.key, 'main_ci', 'at the right end of the table');
});

test('the Main CI hover lists each workflow, failures first, and each run of one that ran twice on a commit', () => {
  const at = (iso: string) => `at ${iso.slice(11, 16)}`;
  const dependabot: MainWorkflow = {
    workflow_id: 9, name: 'Dependabot Updates', path: 'dynamic/dependabot/dependabot-updates', runs: [
      mrun(9, 'success', { event: 'dynamic', head_sha: '0d34a58bee', title: 'github_actions in /. - Update #2', at: '2026-10-08T10:32:00Z' }),
      mrun(8, 'failure', { event: 'dynamic', head_sha: '0d34a58bee', title: 'cargo in /. - Update #1', at: '2026-10-08T10:39:00Z' }),
    ],
  };
  const both = ci([wf(1, mrun(1, 'success')), dependabot]);
  assert.equal(mainCiText(both), '✗ 1 of 2', 'one failed run fails its workflow');
  assert.deepEqual(mainCiTip(both, at).split('\n'), [
    'Latest result of each workflow on main; canceled and skipped runs are left out.',
    '✗ Dependabot Updates: 1 of 2 runs failed (dynamic; 0d34a58)',
    '    ✓ success: github_actions in /. - Update #2 (at 10:32)',
    '    ✗ failure: cargo in /. - Update #1 (at 10:39)',
    '✓ W1: success (push; at 10:00; abcdef1)',
    'Checked at 12:00.',
  ]);
  assert.equal(mainCiUrl('o/a', both), 'https://github.com/o/a/actions/runs/108', 'the failed run');
  assert.equal(mainCiTip(ci([wf(2, mrun(2, 'timed_out', { at: null }))]), at).split('\n')[1], '✗ W2: timed out (push; time unknown; abcdef2)');
  assert.match(mainCiTip(ci([]), at), /No completed workflow run on main in the last 30 days/);
  assert.match(mainCiTip(null, at), /not been read yet/);
  const error = 'GitHub HTTP 403: forbidden';
  assert.equal(mainCiTip(null, at, error), `The last read of the workflow runs failed: ${error}`);
  assert.match(mainCiTip(ci([]), at, error), /last 30 days.*\nThe last read of the workflow runs failed: GitHub HTTP 403/s);
  assert.equal(mainCiTip(both, at, error).split('\n').at(-1), `The last read of the workflow runs failed: ${error}`, 'after the held result');
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

test('the Type column sorts by its words, repos without a type last', () => {
  const e = [
    { ...entry('a', {}), category: 'tooling' as const },
    { ...entry('b', {}), category: 'other' as const },
    { ...entry('c', {}), category: 'app' as const },
    entry('d', {}),
  ];
  assert.deepEqual(sortEntries(e, 'category', 'asc').map((x) => x.repo), ['o/c', 'o/a', 'o/b', 'o/d']);
  assert.equal(value(e[2]!, 'category'), 'App');
});

test('repos outside our org are third party', () => {
  assert.equal(isThirdParty('storytold/photocraft'), false);
  assert.equal(isThirdParty('Storytold/photocraft'), false, 'owners ignore case');
  assert.equal(isThirdParty('tangency-labs/solidcraft'), true);
});

test('the toggles hide a repo by any kind it is', () => {
  assert.equal(isRepoHidden('storytold/fonts', 'assets', []), false);
  assert.equal(isRepoHidden('storytold/fonts', 'assets', ['assets']), true);
  assert.equal(isRepoHidden('storytold/fonts', 'assets', ['ci']), false);
  assert.equal(isRepoHidden('tangency-labs/solidcraft', 'app', ['third_party']), true, 'third party');
  assert.equal(isRepoHidden('tangency-labs/solidcraft', 'app', ['app']), true, 'and an app');
  assert.equal(isRepoHidden('someone/mine', 'other', ['third_party']), true, 'not ours, so third party');
  assert.equal(isRepoHidden('someone/mine', 'other', ['app', 'library', 'assets', 'ci', 'tooling']), false);
  assert.equal(isRepoHidden('storytold/mine', 'other', ['app', 'library', 'assets', 'ci', 'tooling', 'third_party']), false, 'no toggle covers it');
});

test('toggling a kind flips it', () => {
  assert.deepEqual(toggleKind([], 'ci'), ['ci']);
  assert.deepEqual(toggleKind(['ci', 'app'], 'ci'), ['app']);
});

test('dragging among visible rows leaves hidden repos in their places', () => {
  assert.deepEqual(reorderVisible(['a', 'H1', 'b', 'c', 'H2'], ['c', 'a', 'b']), ['c', 'H1', 'a', 'b', 'H2']);
  assert.deepEqual(reorderVisible(['a', 'b'], ['b', 'a']), ['b', 'a']);
});
