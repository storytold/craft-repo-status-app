import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  PR_COLUMNS, REPO_COLUMNS, STATE_LABEL, STATUS_HELP, ago, claudeLine, compact, filterCounts, matchesQuery, nextSort, sorted, relatedTitle, statePill, totals, visibleRows,
} from './pr-model.ts';
import type { ClaudeStatus, PrRow, RepoRow, Status } from './pr-model.ts';

export const pr = (o: Partial<PrRow> = {}): PrRow => ({
  repo: 'o/a', number: 1, title: 'Change', url: 'https://github.com/o/a/pull/1', author: 'ann', avatar: '', tier: 'regular',
  landed_total: 0, approved_by: [], approved: false, approval_stale: false, conflicted: false, is_draft: false, additions: 1, deletions: 1, changed_files: 1, labels: [], mergeable: 'MERGEABLE',
  comments_total: 0, reactions: 0, state: 'promising', ease: 'easy', quality: 80, high_quality: true, inspected: true,
  ci: 'success', rabbit_flags: 0, rabbit_nits: 0, security_flags: 0, security_nits: 0, sensitive: 0, interesting: false, summary: '',
  analysed: false, related: [], focus: 100, ...o,
});

const repoRow = (o: Partial<RepoRow> = {}): RepoRow => ({
  repo: 'o/a', open: 5, listed: true, inspected: 1, analysed: 0, approved: 0, landable: 0, promising: 0, reviewable: 1, ci_not_run: 0, checking: 0, not_inspected: 0, high_quality: 0, easy: 1, medium: 0, hard: 0,
  needs_work: 0, conflicted: 0, draft: 0, conflicts: 0, ci_failing: 0, rabbit_flagged: 0, rabbit_nits: 0, security: 0, security_nits: 0,
  core: 0, frequent: 0, new: 0, bots: 0, interesting: 0, groups: 0, grouped_prs: 0, inaccessible: false, ...o,
});

test('sorting reverses on a second click, starts text columns ascending and numbers descending', () => {
  const title = PR_COLUMNS.find((c) => c.key === 'title')!;
  const quality = PR_COLUMNS.find((c) => c.key === 'quality')!;
  assert.deepEqual(nextSort({ key: 'quality', dir: 'desc' }, title), { key: 'title', dir: 'asc' });
  assert.deepEqual(nextSort({ key: 'title', dir: 'asc' }, quality), { key: 'quality', dir: 'desc' });
  assert.deepEqual(nextSort({ key: 'quality', dir: 'desc' }, quality), { key: 'quality', dir: 'asc' });
});

test('rows with no value sort last in both directions', () => {
  const rows = [repoRow({ repo: 'o/none', listed: false }), repoRow({ repo: 'o/low', open: 1 }), repoRow({ repo: 'o/high', open: 9 })];
  const names = (dir: 'asc' | 'desc') => sorted(rows, { key: 'open', dir }, REPO_COLUMNS).map((r) => r.repo);
  assert.deepEqual(names('desc'), ['o/high', 'o/low', 'o/none']);
  assert.deepEqual(names('asc'), ['o/low', 'o/high', 'o/none']);
});

test('uninspected PRs have no CodeRabbit or security value to sort by', () => {
  const rows = [pr({ number: 1, inspected: false }), pr({ number: 2, rabbit_flags: 2 }), pr({ number: 3, rabbit_nits: 5 })];
  assert.deepEqual(sorted(rows, { key: 'rabbit', dir: 'desc' }, PR_COLUMNS).map((r) => r.number), [2, 3, 1], 'flags outrank any number of nits');
});

test('ease sorts easiest first', () => {
  const rows = [pr({ number: 1, ease: 'hard' }), pr({ number: 2, ease: 'easy' }), pr({ number: 3, ease: 'medium' })];
  assert.deepEqual(sorted(rows, { key: 'ease', dir: 'desc' }, PR_COLUMNS).map((r) => r.number), [2, 3, 1]);
});

test('an unlisted repo shows the Downloads tab\'s count', () => {
  const open = REPO_COLUMNS.find((c) => c.key === 'open')!;
  assert.equal(open.value(repoRow({ listed: false, open: 0, open_hint: 42 })), 42);
  assert.equal(REPO_COLUMNS.find((c) => c.key === 'reviewable')!.value(repoRow({ listed: false })), null);
});

test('a repo still being listed counts what the Downloads tab says it will have', () => {
  const open = REPO_COLUMNS.find((c) => c.key === 'open')!;
  assert.equal(open.value(repoRow({ partial: true, open: 50, open_hint: 1193 })), 1193);
  assert.equal(open.value(repoRow({ partial: true, open: 50 })), 50);
});

test('sorting by approver puts unapproved PRs last, either way', () => {
  const rows = [pr({ number: 1 }), pr({ number: 2, approved_by: ['Zed'] }), pr({ number: 3, approved_by: ['ann', 'bo'] })];
  const by = (dir: 'asc' | 'desc') => sorted(rows, { key: 'approved_by', dir }, PR_COLUMNS).map((r) => r.number);
  assert.deepEqual(by('asc'), [3, 2, 1]);
  assert.deepEqual(by('desc'), [2, 3, 1]);
});

test('the status column sorts best first and reverses', () => {
  const rows = [pr({ number: 1, state: 'draft', focus: 40 }), pr({ number: 2, state: 'landable', focus: 8100 }), pr({ number: 3, state: 'conflicted', focus: 2300 })];
  assert.deepEqual(sorted(rows, { key: 'focus', dir: 'desc' }, PR_COLUMNS).map((r) => r.number), [2, 3, 1]);
  assert.deepEqual(sorted(rows, { key: 'focus', dir: 'asc' }, PR_COLUMNS).map((r) => r.number), [1, 3, 2]);
});

test('related PRs say how sure anyone is', () => {
  const ref = (relation: string, source: string) => ({ number: 7, relation, source, group: 'Tabs', note: 'Why' });
  assert.equal(relatedTitle(ref('duplicate', 'claude')), '#7 · Duplicate (Claude) · Tabs · Why');
  assert.match(relatedTitle(ref('files', 'local')), /Possibly related: same files/);
  assert.match(relatedTitle(ref('issue', 'local')), /· Same issue ·/);
});

test('every status has a truth-table row, and the rows are all different', () => {
  assert.deepEqual(STATUS_HELP.map((h) => h.state).sort(), Object.keys(STATE_LABEL).sort());
  assert.equal(new Set(STATUS_HELP.map((h) => h.when.join())).size, STATUS_HELP.length);
});

test('filters and search', () => {
  const rows = [
    pr({ number: 1, state: 'landable', approved: true }),
    pr({ number: 2, state: 'needs_work', rabbit_flags: 1, ease: 'hard' }),
    pr({ number: 3, state: 'draft', is_draft: true, ease: 'easy', title: 'WIP' }),
    pr({ number: 4, sensitive: 1, tier: 'new', author: 'bo', labels: ['ci'] }),
  ];
  const ids = (f: string, q = '', drafts = false) => visibleRows(rows, f, q, drafts).map((r) => r.number);
  assert.deepEqual(ids('all'), [1, 2, 4], 'drafts hidden by default');
  assert.deepEqual(ids('all', '', true), [1, 2, 3, 4]);
  assert.deepEqual(ids('draft'), [3], 'the drafts filter shows them regardless');
  assert.deepEqual(ids('landable'), [1]);
  assert.deepEqual(ids('approved'), [1]);
  assert.deepEqual(ids('rabbit'), [2]);
  assert.deepEqual(ids('security'), [4]);
  assert.deepEqual(ids('easy', '', true), [1, 4], 'drafts have no ease');
  assert.deepEqual(ids('new'), [4]);
  assert.deepEqual(ids('all', 'bo ci'), [4], 'every word must match');
  assert.deepEqual(ids('all', '#2'), [2]);
  assert.equal(filterCounts(rows, false).needs_work, 1);
  assert.equal(matchesQuery(pr({ summary: 'Adds a selection brush' }), 'brush'), true);
});

test('totals add what is known and leave out dates', () => {
  const t = totals([repoRow({ open: 3, reviewable: 1 }), repoRow({ open: 4, reviewable: 2 }), repoRow({ listed: false, open_hint: 10 })]);
  assert.equal(t.open, 17);
  assert.equal(t.reviewable, 3);
  assert.equal('oldest_at' in t, false);
});

test('ages and numbers read tersely', () => {
  const now = Date.parse('2026-10-31T12:00:00Z');
  assert.equal(ago(null, now), '—');
  assert.equal(ago('2026-10-31T11:59:40Z', now), 'now');
  assert.equal(ago('2026-10-31T11:30:00Z', now), '30m');
  assert.equal(ago('2026-10-31T06:00:00Z', now), '6h');
  assert.equal(ago('2026-10-20T12:00:00Z', now), '11d');
  assert.equal(ago('2026-05-20T12:00:00Z', now), '5mo');
  assert.equal(compact(950), '950');
  assert.equal(compact(1000), '1k');
  assert.equal(compact(1250), '1.3k');
  assert.equal(compact(18965), '19k');
});

const claude: ClaudeStatus = { enabled: true, available: true, model: 'm', spent_today: 1.5, max_usd_per_day: 5, runs_last_hour: 1, max_runs_per_hour: 12 };
const status: Status = { token_rejected: false, points_last_hour: 0, max_points_per_hour: 2500, backlog: 0, histories_pending: 0, poll_secs: 1800, stale_after_secs: 3600, claude };

test('the pill says what the app is doing, worst news first', () => {
  const now = Date.parse('2026-10-31T12:00:00Z');
  const fresh = repoRow({ list_at: '2026-10-31T11:50:00Z' });
  const old = repoRow({ list_at: '2026-10-31T09:00:00Z' });
  assert.equal(statePill(status, [fresh], now).text, 'FRESH');
  assert.equal(statePill(status, [old], now).text, 'STALE');
  assert.equal(statePill({ ...status, fetching: 'Listing x' }, [old], now).text, 'FETCHING');
  assert.equal(statePill({ ...status, paused_until: '2026-10-31T12:30:00Z', fetching: 'x' }, [fresh], now).text, 'PAUSED');
  assert.equal(statePill({ ...status, paused_until: '2026-10-31T11:00:00Z' }, [fresh], now).text, 'FRESH');
});

test('the Claude line reports spending and problems', () => {
  assert.equal(claudeLine(claude).text, 'Claude $1.50 of $5.00 today');
  assert.equal(claudeLine({ ...claude, enabled: false }).text, 'Claude off');
  assert.equal(claudeLine({ ...claude, available: false }).bad, true);
  const failed = claudeLine({ ...claude, last_error: 'Not logged in' });
  assert.equal(failed.bad, true);
  assert.equal(failed.title, 'Not logged in');
  assert.match(claudeLine({ ...claude, running: '12 PRs in o/a' }).text, /12 PRs in o\/a/);
});
