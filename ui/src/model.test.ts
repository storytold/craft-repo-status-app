import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  sortEntries, manualOrder, orderedEntries, nextSort, dropIndex, MANUAL, versionKey, freshness, isStale, ago, compact, tokenStatus,
  totals, urgentAcross, isRefreshing, defaultDir, value, windowById, WINDOWS, COLUMNS, COLUMN,
} from './model.ts';
import type { Activity, RepoEntry, RepoStats, UrgentIssue } from './model.ts';
import {
  NOT_READ_YET, countText, fromTextTip, durationText, stateLabel, stateTip, firstRole, roleHeading, personFigures, peopleRows,
  nextPeopleSort, personRepos, stateBreakdown, waitsOn, heldCount, cachedText, recordsAge, rankRows, rankValue, nextRankSort,
  rankSortFor, PRODUCT, loadText, linesText, batchText, guessTip, workLead, repoFigures,
} from './model.ts';
import type { Flow, Given, Item, PeopleView, PersonResponse, PersonRow, Rank, RankRow, WorkState } from './model.ts';
import {
  searchWords, matchesSearch, personSearchFields, personMatches, matchText, peopleGroups, groupTotals, rankRowsOf, rankTotal,
  RANK_TOTALS_TIP,
} from './model.ts';

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

test('columns: unique keys, every recent column titled with its span', () => {
  assert.equal(Object.keys(COLUMN).length, COLUMNS.length);
  for (const c of COLUMNS.filter((c) => c.recent)) assert.match(c.title ?? '', /\{span\}/, c.key);
  assert.equal(COLUMNS[0]!.key, 'name', 'the sticky first column is the repo');
});

// ───────────────────────────── people tab ─────────────────────────────

const PNOW = Date.parse('2026-10-17T12:00:00Z');
const hoursAgo = (h: number): string => new Date(PNOW - h * 3600e3).toISOString();

const work = (w: Partial<WorkState>): WorkState => ({
  state: 'awaiting_first_maintainer_response', subtype: null, waits_on: ['maintainer'], waits_for: [], since: null, guess: false,
  low: false, rule: '63: no maintainer comment, review, push or close yet', why: '', evidence: null, also: [], flags: [], hold: null,
  blocked_by: [], competing: [], ...w,
});
const pitem = (repo: string, number: number, author: string, w: Partial<WorkState>): Item => ({
  repo, number, title: `Change ${number}`, url: `https://github.com/${repo}/pull/${number}`, author, created_at: hoursAgo(200), work: work(w),
});
const flow = (f: Partial<Flow>): Flow => ({
  merged: 0, merged_by: {}, landed_elsewhere: 0, closed: {}, closed_total: 0, median_merge_secs: null, median_ready_secs: null, ...f,
});
const given = (g: Partial<Given>): Given => ({
  comments: 0, reviews: 0, approved: 0, changes_requested: 0, prs: 0, requests: 0, holds: 0, merges: 0, closes: 0, ...g,
});
const presp = (r: Partial<PersonResponse>): PersonResponse => ({
  their_median_secs: null, their_answered: 0, unanswered: 0, oldest_unanswered: null, m_median_secs: null, m_answered: 0,
  awaiting_first: 0, oldest_awaiting: null, ...r,
});
const person = (key: string, p: Partial<PersonRow> = {}): PersonRow => ({
  key, login: key, name: null, on_roster: false, maintainer_of: [], owner_of: [], flow: {}, given: {}, response: {}, ...p,
});

const HOLD_WORK: Partial<WorkState> = {
  state: 'held_for_owner', waits_on: ['owner'], waits_for: ['boss-one'], since: hoursAgo(96), guess: true,
  rule: '10: a maintainer’s hold phrase, not lifted since', why: "matched the phrase 'owner's call' in maint-one's comment of 10-13; no later release phrase found",
  evidence: { quote: 'It is the owner’s call.', author: 'maint-one', at: hoursAgo(96), url: 'https://github.com/o/alpha/pull/1#issuecomment-11', phrase: "owner's call" },
  hold: { since: hoursAgo(96), by: 'maint-one', category: 'brand', reaffirmed: hoursAgo(24), owners: ['boss-one'] },
  also: ['awaiting_maintainer'], flags: ['aging'],
};

/** Found by the sweep, with two comments on GitHub, not read yet. */
const UNREAD_WORK: Partial<WorkState> = { state: NOT_READ_YET, waits_on: [], waits_for: [], since: hoursAgo(6), rule: 'the timeline has not been read yet' };

const peopleView = (): PeopleView => ({
  generated_at: hoursAgo(0),
  records: 12,
  fetched: 10,
  rankings: [],
  co_unlinked: {},
  unlinked: [],
  discord: {},
  load: { merged: 0, total: null, closed: 0, closed_total: null, done: true },
  checked: {},
  repos: {},
  main: {},
  items: [
    pitem('o/alpha', 1, 'ann-example', HOLD_WORK),
    pitem('o/alpha', 2, 'ann-example', { state: 'changes_requested_by_maintainer', subtype: 'rebase', waits_on: ['author'], waits_for: ['ann-example'], since: hoursAgo(48), guess: true }),
    pitem('o/beta', 3, 'ann-example', { state: 'draft', waits_on: ['author'], waits_for: ['ann-example'], since: hoursAgo(24), rule: '20: isDraft' }),
    pitem('o/beta', 4, 'bo-example', { state: 'author_responded_awaiting_maintainer', waits_for: ['maint-one'], since: hoursAgo(3) }),
    pitem('o/beta', 5, 'bo-example', { state: 'author_stepped_back', waits_for: ['maint-one'], since: hoursAgo(5), guess: true, low: true }),
  ],
  people: [
    person('ann-example', {
      login: 'Ann-Example', name: 'Ann',
      flow: {
        '7d': {
          '*': flow({ merged: 2, merged_by: { direct: 2 }, closed: { closed_duplicate: 1 }, closed_total: 3, median_merge_secs: 7200, median_ready_secs: 3600 }),
          'o/alpha': flow({ merged: 2, merged_by: { direct: 2 }, closed_total: 2, median_merge_secs: 7200 }),
        },
        '1d': { '*': flow({ merged: 1, merged_by: { direct: 1 }, closed_total: 1 }) },
      },
      response: {
        '*': presp({ their_median_secs: 3600, their_answered: 2, unanswered: 1, oldest_unanswered: hoursAgo(48), m_median_secs: 1800, m_answered: 3 }),
        'o/alpha': presp({ unanswered: 1, oldest_unanswered: hoursAgo(48) }),
      },
    }),
    person('maint-one', {
      maintainer_of: ['o/alpha', 'o/beta'],
      given: { '7d': { '*': given({ comments: 5, reviews: 2, prs: 4, requests: 2, holds: 1, merges: 3 }), 'o/beta': given({ comments: 1 }) } },
    }),
    person('bo-example'),
    person('boss-one', { owner_of: ['o/alpha', 'o/beta'] }),
  ],
});

test('counts show their part read from comments, and durations read in minutes, hours or days', () => {
  assert.equal(countText(38, 6), '38 (6 from comments)');
  assert.equal(countText(5, 0), '5');
  assert.equal(fromTextTip(1, 1), '1 of 1 state was read from a comment’s text, not from a GitHub field');
  assert.equal(fromTextTip(3, 2), '2 of 3 states were read from a comment’s text, not from a GitHub field');
  assert.equal(durationText(null), '');
  assert.equal(durationText(20), '1m');
  assert.equal(durationText(7200), '2h');
  assert.equal(durationText(2.5 * 86400), '2.5d');
  assert.equal(durationText(12.4 * 86400), '12d');
  assert.equal(stateLabel('held_for_owner'), 'Held for owner');
  assert.equal(stateLabel(NOT_READ_YET), 'Not read yet');
  assert.equal(stateLabel('some_new_state'), 'some new state', 'an unknown id still reads');
});

test('a state’s hover text gives the rule, the quoted sentence, whether it came from comment text and what else matched', () => {
  const tip = stateTip(work(HOLD_WORK));
  for (const want of [
    'Held for owner', 'Rule 10: a maintainer’s hold phrase', '“It is the owner’s call.” (maint-one, 2026-10-13)',
    "Read from a comment’s text, not from a GitHub field: matched the phrase 'owner's call'",
    'Held since 2026-10-13 by maint-one, reason: brand, held again 2026-10-16.',
    'Also matched: Awaiting maintainer.', 'Flags: aging.', 'Click to open the comment.',
  ]) assert.ok(tip.includes(want), `${want}\n---\n${tip}`);
  assert.ok(!tip.includes('Low confidence'));
  const low = stateTip(work({ state: 'author_stepped_back', guess: true, low: true, why: 'matched x' }));
  assert.ok(low.includes('Low confidence.') && !low.includes('Click'), low);
  const field = stateTip(work({ state: 'draft', rule: '20: isDraft' }));
  assert.ok(!field.includes('comment’s text'), field);
  const unread = stateTip(work(UNREAD_WORK));
  assert.match(unread, /^Not read yet: the app has found this pull request but has not read its comments/);
  assert.ok(!unread.includes('Rule'), unread);
});

test('a person’s figures count their work, what waits on them and their flow in the window', () => {
  const v = peopleView();
  const ann = personFigures(v, v.people[0]!, '7d', '');
  assert.deepEqual(
    [ann.open, ann.openGuessed, ann.onThem, ann.onThemGuessed, ann.onOthersTotal, ann.held, ann.heldGuessed],
    [3, 2, 2, 1, 1, 1, 1],
  );
  assert.deepEqual(ann.onOthers, { owner: 1 });
  assert.deepEqual([ann.merged, ann.landed, ann.closed, ann.mergeMedian, ann.theirResponse, ann.unanswered], [2, 0, 1, 7200, 3600, 1]);
  assert.equal(ann.landedRate, 2 / 3);
  const beta = personFigures(v, v.people[0]!, '7d', 'O/Beta');
  assert.deepEqual([beta.open, beta.onThem, beta.merged, beta.held, beta.landedRate], [1, 1, 0, 0, null], 'one repo, in any letter case');
  assert.equal(personFigures(v, v.people[0]!, '1d', '').merged, 1);
  const m = personFigures(v, v.people[1]!, '7d', '');
  assert.deepEqual([m.onThem, m.reviews, m.open], [2, 7, 0]);
  assert.equal(personFigures(v, v.people[3]!, '7d', '').onThem, 1, 'the owner owns the hold');
  assert.ok(waitsOn(v.items[0]!, 'boss-one') && !waitsOn(v.items[0]!, 'ann-example'));
  // A request after a hold waits on them and on the owner.
  const both = pitem('o/alpha', 7, 'ann-example', {
    state: 'changes_requested_by_maintainer', waits_on: ['author', 'owner'], waits_for: ['ann-example', 'boss-one'], guess: true,
    flags: ['also_held_for_owner'],
  });
  const fb = personFigures({ ...v, items: [...v.items, both] }, v.people[0]!, '7d', '');
  assert.deepEqual([fb.onThem, fb.onOthersTotal, fb.held], [3, 2, 2]);
  assert.deepEqual(fb.onOthers, { owner: 2 });
  // A maintainer's own draft waits only on them.
  const own = pitem('o/alpha', 8, 'maint-one', { state: 'draft', waits_on: ['maintainer'], waits_for: ['maint-one'] });
  const fm = personFigures({ ...v, items: [own] }, v.people[1]!, '7d', '');
  assert.deepEqual([fm.open, fm.onThem, fm.onOthersTotal], [1, 1, 0]);
  // Each part finds its repo in any letter case, whatever the others hold.
  const cased = person('cy-test', { given: { '7d': { 'O/Beta': given({ comments: 2 }) } }, response: { 'O/BETA': presp({ unanswered: 1 }) } });
  const c = personFigures(v, cased, '7d', 'o/beta');
  assert.deepEqual([c.reviews, c.unanswered, c.flow], [2, 1, null]);
});

test('a pull request not read yet counts as its author’s open work and waits on nobody', () => {
  const v = peopleView();
  v.items.push(pitem('o/alpha', 9, 'bo-example', UNREAD_WORK));
  const bo = personFigures(v, v.people[2]!, '7d', '');
  assert.deepEqual([bo.open, bo.onThem, bo.onOthersTotal, bo.openGuessed], [3, 0, 2, 1]);
  assert.equal(personFigures(v, v.people[1]!, '7d', '').onThem, 2, 'no maintainer is named for it');
  assert.equal(firstRole(work(UNREAD_WORK)), 'unread');
  assert.equal(firstRole(work({ waits_on: [] })), 'nobody');
  assert.equal(roleHeading('unread'), 'Not read yet');
  assert.equal(roleHeading('author'), 'Waiting on the author');
  const groups = stateBreakdown(v.items.filter((i) => i.author === 'bo-example'), PNOW);
  assert.deepEqual(groups.map((g) => g.role), ['maintainer', 'unread'], 'listed after every role');
  assert.deepEqual(groups[1]!.states.map((s) => [s.state, s.items.length]), [[NOT_READ_YET, 1]]);
  const f = repoFigures(v, 'o/alpha', '7d', PNOW);
  assert.deepEqual(f.byRole.map((g) => [g.role, g.count]), [['author', 1], ['owner', 1], ['unread', 1]]);
  assert.equal(f.firstQueue, 0, 'not counted as awaiting a first response');
});

test('people rows keep everyone with something to show, sorted with ties by login', () => {
  const v = peopleView();
  const keys = (rows: { key: string }[]) => rows.map((r) => r.key);
  assert.deepEqual(keys(peopleRows(v, '7d', '', { key: 'on_them', dir: 'desc' })), ['ann-example', 'maint-one', 'boss-one', 'bo-example']);
  assert.deepEqual(keys(peopleRows(v, '7d', '', { key: 'person', dir: 'asc' })), ['ann-example', 'bo-example', 'boss-one', 'maint-one']);
  assert.deepEqual(keys(peopleRows(v, '7d', '', { key: 'merge_median', dir: 'asc' })).slice(0, 1), ['ann-example'], 'missing values sort last');
  assert.deepEqual(keys(peopleRows(v, '7d', 'o/alpha', { key: 'on_them', dir: 'desc' })), ['ann-example', 'boss-one']);
  assert.deepEqual(nextPeopleSort({ key: 'open', dir: 'desc' }, 'person'), { key: 'person', dir: 'asc' });
  assert.deepEqual(nextPeopleSort({ key: 'open', dir: 'desc' }, 'open'), { key: 'open', dir: 'asc' });
  assert.deepEqual(nextPeopleSort({ key: 'person', dir: 'asc' }, 'held'), { key: 'held', dir: 'desc' });
  assert.deepEqual(personRepos(v, v.people[0]!, '7d', ['o/alpha', 'o/beta', 'o/gamma']), ['o/alpha', 'o/beta']);
  assert.deepEqual(personRepos(v, v.people[1]!, '7d', ['o/alpha', 'o/beta']), ['o/beta'], 'work waiting on them and reviews given');
  // Someone whose only activity is closing others' pull requests is listed too.
  v.people.push(person('closer-x', { given: { '7d': { '*': given({ closes: 1 }), 'o/alpha': given({ closes: 1 }) } } }));
  assert.ok(keys(peopleRows(v, '7d', '', { key: 'person', dir: 'asc' })).includes('closer-x'));
  assert.deepEqual(personRepos(v, v.people.at(-1)!, '7d', ['o/alpha', 'o/beta']), ['o/alpha']);
});

test('the state breakdown groups items by who they wait on, with time in state and the five oldest', () => {
  const v = peopleView();
  const ann = v.items.filter((i) => i.author === 'ann-example');
  const groups = stateBreakdown(ann, PNOW);
  assert.deepEqual(groups.map((g) => [g.role, g.states.map((s) => s.state)]), [
    ['author', ['draft', 'changes_requested_by_maintainer']],
    ['owner', ['held_for_owner']],
  ], 'ties follow rule order');
  const many = [1, 2, 3, 4, 5, 6, 7].map((h) => pitem('o/x', h, 'ann-example', { state: 'draft', waits_on: ['author'], since: hoursAgo(h), guess: h % 2 === 0 }));
  const [g] = stateBreakdown(many, PNOW);
  const s = g!.states[0]!;
  assert.deepEqual([s.items.length, s.guessed, s.medianSecs, s.maxSecs], [7, 3, 4 * 3600, 7 * 3600]);
  assert.deepEqual(s.oldest.map((i) => i.number), [7, 6, 5, 4, 3]);
  const even = stateBreakdown(many.slice(0, 4), PNOW)[0]!.states[0]!;
  assert.equal(even.medianSecs, 2.5 * 3600);
  // A state with no known start counts, but not in the ages, and lists last.
  const unknown = pitem('o/x', 9, 'ann-example', { state: 'draft', waits_on: ['author'], since: null });
  const mixed = stateBreakdown([unknown, ...many.slice(0, 2)], PNOW)[0]!.states[0]!;
  assert.deepEqual([mixed.items.length, mixed.medianSecs, mixed.maxSecs], [3, 1.5 * 3600, 2 * 3600]);
  assert.deepEqual(mixed.oldest.map((i) => i.number), [2, 1, 9]);
  const alone = stateBreakdown([unknown], PNOW)[0]!.states[0]!;
  assert.deepEqual([alone.medianSecs, alone.maxSecs], [null, null]);
});

test('the badge counts held pull requests, and the tab says how much of the records it has read', () => {
  const v = peopleView();
  assert.equal(heldCount(v), 1);
  assert.equal(heldCount(null), 0);
  assert.equal(cachedText(v), '10 of 12 timelines read');
  assert.equal(cachedText({ ...v, fetched: 12, load: { merged: 30, total: 120, closed: 0, closed_total: null, done: false } }),
    '30 of 120 merged and 0 closed pull requests loaded');
  assert.equal(cachedText({ ...v, fetched: 12 }), '');
  assert.equal(cachedText(null), '');
});

test('the People tab’s record age is its oldest check, stale past the limit or with a repo never checked', () => {
  const now = Date.parse('2026-10-17T12:00:00Z');
  assert.equal(recordsAge(null, 900, now), null);
  assert.equal(recordsAge({ checked: {} }, 900, now), null);
  const fresh = recordsAge({ checked: { 'o/a': '2026-10-17T11:55:00Z', 'o/b': '2026-10-17T11:50:00Z' } }, 900, now);
  assert.deepEqual(fresh, { oldest: Date.parse('2026-10-17T11:50:00Z'), unchecked: [], stale: false });
  const old = recordsAge({ checked: { 'o/a': '2026-10-17T11:55:00Z', 'o/b': '2026-10-14T09:00:00Z' } }, 900, now);
  assert.equal(old?.stale, true, 'one repo’s records are three days old');
  const never = recordsAge({ checked: { 'o/a': '2026-10-17T11:55:00Z', 'o/b': null } }, 900, now);
  assert.deepEqual(never, { oldest: Date.parse('2026-10-17T11:55:00Z'), unchecked: ['o/b'], stale: true });
});

const rk = (r: Partial<Rank>): Rank => ({
  merged: 0, landed: 0, added: 0, removed: 0, gen_added: 0, gen_removed: 0, files: 0, commits: 0, co_authored: 0, direct: null,
  active_days: 0, merges: 0, batch_merges: 0, reviews: 0, approved: 0, changes_requested: 0, commented: 0, reviewed_prs: 0,
  review_commits: 0, review_added: 0, review_removed: 0, types: {}, guessed: {}, stacked: 0, truncated: 0, unverified: false, ...r,
});
const rrow = (key: string, windows: Record<string, Rank>): RankRow => ({ key, login: key, name: null, on_roster: false, maintainer: false, windows });

test('rankings keep the people each ranking has figures for, sorted with missing values last', () => {
  const v = peopleView();
  v.rankings = [
    rrow('ann', { '7d': rk({ merged: 2, added: 10, removed: 5, direct: 0, types: { feature: 1, fix: 1 } }) }),
    rrow('bo', { '7d': rk({ merged: 1, added: 40, direct: 3, types: { docs: 1 } }) }),
    rrow('cy', { '7d': rk({ merges: 4, reviews: 2 }), '1d': rk({ reviews: 1 }) }),
    rrow('dee', { '7d': rk({ direct: null, active_days: 1 }) }),
  ];
  const keys = (rows: { row: RankRow }[]) => rows.map((r) => r.row.key);
  assert.deepEqual(keys(rankRows(v, '7d', 'merged', { key: 'merged', dir: 'desc' })), ['ann', 'bo', 'dee']);
  assert.deepEqual(keys(rankRows(v, '7d', 'merged', { key: 'lines', dir: 'desc' })), ['bo', 'ann', 'dee']);
  assert.deepEqual(keys(rankRows(v, '7d', 'merged', { key: 'direct', dir: 'asc' })), ['ann', 'bo', 'dee'], 'not counted sorts last');
  assert.deepEqual(keys(rankRows(v, '7d', 'maintainer', { key: 'merges', dir: 'desc' })), ['cy']);
  assert.deepEqual(keys(rankRows(v, '7d', 'type', { key: PRODUCT, dir: 'desc' })), ['ann', 'bo']);
  assert.deepEqual(keys(rankRows(v, '7d', 'type', { key: 'type:docs', dir: 'desc' })), ['bo', 'ann']);
  assert.deepEqual(keys(rankRows(v, '1d', 'maintainer', { key: 'person', dir: 'asc' })), ['cy']);
  assert.equal(rankValue(v.rankings[0]!, v.rankings[0]!.windows['7d']!, PRODUCT), 2);
  assert.deepEqual(nextRankSort({ key: 'merged', dir: 'desc' }, 'merged'), { key: 'merged', dir: 'asc' });
  assert.deepEqual(nextRankSort({ key: 'merged', dir: 'desc' }, 'person'), { key: 'person', dir: 'asc' });
  assert.deepEqual(rankSortFor({ key: 'lines', dir: 'asc' }, 'maintainer'), { key: 'merges', dir: 'desc' });
  assert.deepEqual(rankSortFor({ key: 'lines', dir: 'asc' }, 'merged'), { key: 'lines', dir: 'asc' });
  assert.deepEqual(rankSortFor({ key: 'merges', dir: 'desc' }, 'type'), { key: PRODUCT, dir: 'desc' });
});

test('the load header, line counts, badges and type hover text', () => {
  assert.equal(loadText({ merged: 1240, total: 9747, closed: 30, closed_total: 120, done: false }),
    '1,240 of 9,747 merged and 30 of 120 closed pull requests loaded');
  assert.equal(loadText({ merged: 300, total: null, closed: 0, closed_total: null, done: false }), '300 merged and 0 closed pull requests loaded');
  assert.equal(loadText({ merged: 9747, total: 9747, closed: 4, closed_total: 120, done: false }),
    '9,747 of 9,747 merged and 4 of 120 closed pull requests loaded', 'the merged walk is done, the closed walk is not');
  assert.equal(loadText({ merged: 9747, total: 9747, closed: 120, closed_total: 120, done: true }), '');
  assert.equal(loadText(undefined), '');
  assert.equal(linesText(1805, 4), '+1,805 −4');
  assert.equal(batchText(rk({ batch_merges: 14 })), '14 in batch pushes');
  assert.equal(batchText(rk({})), '');
  assert.match(guessTip('docs', 3, 1), /^1 of 3 docs pull requests got their type from a fallback rule/);
  assert.equal(workLead(1, 31, 2, 'last week'), '1 merged pull request and +31 −2 product lines, last week.');
  assert.equal(workLead(1200, 0, 0, 'last day'), '1,200 merged pull requests and +0 −0 product lines, last day.');
});

test('a repo’s figures group its open work, queues, holds, health and flow', () => {
  const v = peopleView();
  v.items.push(
    pitem('o/alpha', 6, 'cy-example', { state: 'awaiting_first_maintainer_response', waits_on: ['maintainer'], since: hoursAgo(10), flags: ['stale'] }),
    pitem('o/alpha', 7, 'cy-example', { state: 'awaiting_first_maintainer_response', waits_on: ['maintainer'], since: hoursAgo(2), competing: [6] }),
    pitem('o/alpha', 8, 'dee-example', { state: 'merge_conflict', waits_on: ['author'], since: hoursAgo(1), blocked_by: [6] }),
    pitem('O/Alpha', 9, 'dee-example', { state: 'ci_failing_inherited_from_main', waits_on: ['ci'], since: hoursAgo(1), competing: [7] }),
  );
  v.repos = { 'o/alpha': { '7d': { merged: 3, merged_by: { batch: 3 }, landed_elsewhere: 0, closed: {}, median_merge_secs: 60, median_response_secs: null, methods: {}, mergers: {} } } };
  v.main = { 'O/Alpha': { oid: 'h', rollup: 'FAILURE', failing: ['test'] } };
  const f = repoFigures(v, 'o/ALPHA', '7d', PNOW);
  assert.equal(f.open, 6);
  assert.deepEqual(f.byRole.map((g) => [g.role, g.count]), [['author', 2], ['maintainer', 2], ['owner', 1], ['ci', 1]]);
  assert.equal(f.byRole[0]!.share, 2 / 6);
  assert.equal(f.oldestOnMaintainer?.number, 6);
  assert.deepEqual([f.firstQueue, f.firstQueueMedianSecs], [2, 6 * 3600]);
  assert.deepEqual(f.holds.map((i) => i.number), [1]);
  assert.deepEqual(f.requests, { rebase: 1 });
  assert.deepEqual([f.conflicts, f.inherited], [1, 1]);
  assert.deepEqual(f.staleness, [{ role: 'maintainer', aging: 0, stale: 1 }, { role: 'owner', aging: 1, stale: 0 }]);
  assert.deepEqual(f.blocked.map((i) => i.number), [8]);
  assert.deepEqual(f.competing, [[6, 7], [7, 9]]);
  assert.equal(f.flow?.merged, 3);
  assert.deepEqual(f.main?.failing, ['test']);
  assert.equal(repoFigures(v, 'o/beta', '7d', PNOW).flow, null);
});

// ───────────────────────────── search and groups ─────────────────────────────

test('the People search matches every word against the login, the roster and Discord names, and repos', () => {
  const v = { ...peopleView(), discord: { 'ann-example': 'ann_d' } };
  const ann = v.people[0]!;
  const fields = personSearchFields(v, ann, ['o/alpha']);
  const hit = (q: string) => matchesSearch(searchWords(q), fields);
  assert.deepEqual(searchWords('  Ann   ALPHA '), ['ann', 'alpha']);
  assert.ok(hit(''), 'no words match everyone');
  assert.ok(hit('ann-ex'), 'the login, in any case');
  assert.ok(hit('ANN'), 'the roster name');
  assert.ok(hit('ann_d'), 'the Discord name');
  assert.ok(hit('alpha'), 'a repo by short name');
  assert.ok(hit('o/alpha'), 'and by full name');
  assert.ok(hit('ann alpha'), 'every word in some field');
  assert.ok(!hit('ann beta'), 'beta is not one of their repos');
  assert.ok(!hit('bo-ex'));
  assert.ok(!matchesSearch(['ann_d'], personSearchFields(peopleView(), ann, [])), 'a view without Discord names matches none');
  const asked = { n: 0 };
  const repos = () => { asked.n += 1; return ['o/alpha']; };
  assert.ok(personMatches(v, ann, ['ann'], repos));
  assert.equal(asked.n, 0, 'a name match does not list their repos');
  assert.ok(personMatches(v, ann, ['ann', 'alpha'], repos));
  assert.equal(asked.n, 1, 'a word only a repo matches does');
  assert.ok(!personMatches(v, ann, ['beta'], repos));
  assert.equal(matchText(2, 12, ['x']), '2 of 12 people');
  assert.equal(matchText(1, 1, ['x']), '1 of 1 person');
  assert.equal(matchText(1200, 1500, ['x']), '1,200 of 1,500 people');
  assert.equal(matchText(12, 12, []), '', 'no search, no count');
});

test('the grouped Work view lists each repo’s people with that repo’s figures, sorted, searched and totaled', () => {
  const v = peopleView();
  const sort = { key: 'open', dir: 'desc' } as const;
  const groups = peopleGroups(v, '7d', ['o/alpha', 'o/beta', 'o/gamma'], sort);
  assert.deepEqual(groups.map((g) => g.repo), ['o/alpha', 'o/beta'], 'a repo with nobody is left out');
  assert.deepEqual(groups[0]!.rows.map((f) => [f.key, f.open]), [['ann-example', 2], ['boss-one', 0]]);
  assert.deepEqual(groups[1]!.rows.map((f) => [f.key, f.open]), [['bo-example', 2], ['ann-example', 1], ['maint-one', 0]]);
  const byName = peopleGroups(v, '7d', ['o/beta'], { key: 'person', dir: 'asc' });
  assert.deepEqual(byName[0]!.rows.map((f) => f.key), ['ann-example', 'bo-example', 'maint-one']);
  const searched = peopleGroups(v, '7d', ['o/alpha', 'o/beta'], sort, (p) => matchesSearch(['bo-ex'], personSearchFields(v, p, [])));
  assert.deepEqual(searched.map((g) => [g.repo, g.rows.map((f) => f.key)]), [['o/beta', ['bo-example']]]);
  assert.deepEqual(groupTotals(groups[1]!.rows), { open: 3, on_others: 2, held: 0, merged: 0, landed: 0, closed: 0, reviews: 1 });
});

test('grouped rankings sort any list of rows and total the columns that add up', () => {
  const list = [
    rrow('ann', { '7d': rk({ merged: 2, added: 10, removed: 5, types: { feature: 1, fix: 1 } }) }),
    rrow('bo', { '7d': rk({ merged: 1, added: 40, direct: 3, types: { docs: 1 } }) }),
    rrow('cy', { '7d': rk({ merges: 4 }) }),
  ];
  const rows = rankRowsOf(list, '7d', 'merged', { key: 'lines', dir: 'desc' });
  assert.deepEqual(rows.map((r) => r.row.key), ['bo', 'ann']);
  assert.equal(rankTotal(rows, 'merged'), '3');
  assert.equal(rankTotal(rows, 'lines'), '+50 −5');
  assert.equal(rankTotal(rows, 'direct'), '3');
  assert.equal(rankTotal(rows.slice(1), 'direct'), '', 'nobody counted');
  assert.equal(rankTotal(rows, PRODUCT), '2');
  assert.equal(rankTotal(rows, 'type:docs'), '1');
  assert.equal(rankTotal(rows, 'person'), '');
  // Both review pull request #5 on the same day, and one commit names both as co-authors.
  const shared = rk({ reviews: 1, reviewed_prs: 1, active_days: 1, co_authored: 1 });
  const pair = [rrow('ann', { '7d': shared }), rrow('bo', { '7d': shared })];
  const merged = rankRowsOf(pair, '7d', 'merged', { key: 'person', dir: 'asc' });
  const maint = rankRowsOf(pair, '7d', 'maintainer', { key: 'person', dir: 'asc' });
  assert.deepEqual([merged.length, maint.length], [2, 2]);
  assert.equal(rankTotal(merged, 'co_authored'), '');
  assert.equal(rankTotal(merged, 'active_days'), '');
  assert.equal(rankTotal(maint, 'reviewed_prs'), '');
  assert.equal(rankTotal(maint, 'reviews'), '2', 'each review has one author and adds up');
  assert.match(RANK_TOTALS_TIP, /Co-authored, Active days and PRs reviewed do not add up/);
});
