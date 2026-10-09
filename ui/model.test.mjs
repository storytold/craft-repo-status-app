import { test } from 'node:test';
import assert from 'node:assert/strict';
import { sortEntries, versionKey, freshness, ago, compact, totals, urgentAcross, defaultDir } from './model.js';

const entry = (repo, stats, fetched_at = '2026-10-08T12:00:00Z') => ({ repo: `o/${repo}`, stats, fetched_at });

const rows = [
  entry('b', { urgency: 10, latest_build: 'v0.10.0', last_commit_at: '2026-10-08T10:00:00Z' }),
  entry('a', { urgency: 50, latest_build: 'v0.9.0', last_commit_at: '2026-10-08T11:00:00Z' }),
  entry('c', null, null),
  entry('d', { urgency: 50, latest_build: 'v0.10.0-rc.2', last_commit_at: null }),
];

const names = (list) => list.map((e) => e.repo.slice(2)).join('');

test('numbers sort descending with ties by name, missing last', () => {
  assert.equal(names(sortEntries(rows, 'urgency', 'desc')), 'adbc');
  assert.equal(names(sortEntries(rows, 'urgency', 'asc')), 'badc');
});

test('versions sort semantically, prereleases below their release', () => {
  assert.equal(names(sortEntries(rows, 'latest_build', 'desc')), 'bdac');
  assert.deepEqual(versionKey('v0.10.0-rc.2'), [0, 10, 0, -1, 2]);
  assert.deepEqual(versionKey('nightly'), [-1]);
});

test('dates and names', () => {
  assert.equal(names(sortEntries(rows, 'last_commit_at', 'desc')), 'abcd');
  assert.equal(names(sortEntries(rows, 'name', 'asc')), 'abcd');
  assert.equal(defaultDir('name'), 'asc');
  assert.equal(defaultDir('downloads_total'), 'desc');
  assert.equal(defaultDir('oldest_open_pr_at'), 'asc');
});

test('freshness', () => {
  const now = Date.parse('2026-10-08T12:10:00Z');
  const snap = { stale_after_secs: 900, repos: [entry('a', {}), entry('b', {}, '2026-10-08T12:05:00Z')] };
  assert.deepEqual(freshness(snap, now), { oldest: Date.parse('2026-10-08T12:00:00Z'), stale: false });
  assert.equal(freshness(snap, now + 6 * 60_000).stale, true);
  assert.equal(freshness({ ...snap, repos: [...snap.repos, entry('x', null, null)] }, now).oldest, null);
  assert.equal(freshness({ stale_after_secs: 1, repos: [] }, now).stale, true);
});

test('formatting', () => {
  const now = Date.parse('2026-10-08T12:00:00Z');
  assert.equal(ago('2026-10-08T11:58:00Z', now), '2m');
  assert.equal(ago('2026-10-06T12:00:00Z', now), '2d');
  assert.equal(ago(null, now), '');
  assert.equal(compact(279524), '280k');
  assert.equal(compact(57247), '57.2k');
  assert.equal(compact(950), '950');
});

test('totals and urgent merge', () => {
  assert.equal(totals(rows).urgency, 110);
  const e = [
    entry('a', { urgent: [{ number: 1, score: 30, critical: false }, { number: 2, score: 80, critical: true }] }),
    entry('b', { urgent: [{ number: 3, score: 50, critical: true }] }),
  ];
  assert.deepEqual(urgentAcross(e).map((u) => u.number), [2, 3, 1]);
  assert.deepEqual(urgentAcross(e, { criticalOnly: true }).map((u) => u.repo), ['o/a', 'o/b']);
  assert.deepEqual(urgentAcross(e, { repo: 'o/b' }).map((u) => u.number), [3]);
});
