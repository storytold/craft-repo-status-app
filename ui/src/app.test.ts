// The UI end to end in happy-dom: index.html's markup, the real `start`, and a
// fake host API standing in for Tauri.

import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { Window } from 'happy-dom';
import { start } from './app.ts';
import type { Api } from './app.ts';
import { COLUMNS, RECENT } from './model.ts';
import type { Meta, RepoEntry, RepoStats, Snapshot, UrgentIssue } from './model.ts';
import { BATCH_TIP, NOT_READ_YET, PRODUCT, RANK_COLUMNS, UNVERIFIED_TIP, rankRows } from './model.ts';
import type { DetailRow, Flow, Item, PeopleView, PersonDetail, PersonRow, Rank, RankRow, RepoPeople, WorkState } from './model.ts';

const HTML = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
const BODY = HTML.match(/<body>([\s\S]*)<\/body>/)![1]!.replace(/<script[\s\S]*?<\/script>/g, '');

const NOW = new Date().toISOString();
const ROW_HEIGHT = 20;

const issue = (number: number, score: number, critical: boolean): UrgentIssue => ({
  number, score, critical, title: `Issue ${number}`, url: `https://github.com/o/x/issues/${number}`,
  created_at: NOW, updated_at: NOW, comments: 0, reactions: 0, labels: [], reasons: critical ? ['crash'] : [],
});

const stats = (s: Partial<RepoStats>): RepoStats => ({
  open_prs: 1, open_issues: 2, last_commit_at: NOW, last_issue_at: NOW, oldest_open_pr_at: NOW, newest_pr_at: NOW,
  recent: {}, latest_build: 'v1.0.0', latest_build_at: NOW, latest_build_prerelease: false, latest_build_sha: null,
  since_build: null, latest_build_downloads: 0, downloads_total: 0, urgency: 0, critical_issues: 0, urgent: [], icon_oid: null,
  ...s,
});

const repo = (name: string, s: Partial<RepoStats> | null, extra: Partial<RepoEntry> = {}): RepoEntry =>
  ({ repo: `o/${name}`, stats: s && stats(s), fetched_at: NOW, ...extra });

const snapshot = (repos: RepoEntry[] = [
  repo('alpha', { urgency: 100, critical_issues: 1, urgent: [issue(1, 80, true), issue(2, 20, false)] }),
  repo('beta', { urgency: 300, urgent: [issue(3, 50, false)] }),
  repo('gamma', { urgency: 200 }),
]): Snapshot => ({
  repos, last_full_refresh_at: NOW, refreshing: false, next_refresh_at: NOW, poll_secs: 300,
  stale_after_secs: 900, rate_limit_remaining: 4000, error: null,
});

interface Harness {
  win: Window;
  doc: Document;
  calls: { cmd: string; args?: Record<string, unknown> }[];
  emit<T>(event: string, payload: T): void;
  rows(): string[];
  row(name: string): HTMLTableRowElement;
  view(): string;
}

let teardown: (() => Promise<void>) | null = null;
afterEach(async () => { await teardown?.(); teardown = null; });

const tick = () => new Promise((r) => setTimeout(r, 0));

/** Answers a host command for one test; `undefined` falls back to the defaults below. */
type Host = (cmd: string, args?: Record<string, unknown>) => unknown;

/** A fresh document with the UI started in it. */
async function setup({ snap = snapshot(), storage = {} as Record<string, unknown>, meta = {} as Meta, host = (() => undefined) as Host } = {}): Promise<Harness> {
  const win = new Window({ url: 'http://localhost/index.html' });
  win.document.body.innerHTML = BODY;
  for (const [k, v] of Object.entries(storage)) win.localStorage.setItem(k, JSON.stringify(v));
  const globals = { window: win, document: win.document, localStorage: win.localStorage, location: win.location, navigator: win.navigator };
  const saved = Object.fromEntries(Object.keys(globals).map((k) => [k, Object.getOwnPropertyDescriptor(globalThis, k)]));
  for (const [k, v] of Object.entries(globals)) Object.defineProperty(globalThis, k, { value: v, configurable: true, writable: true });

  // Layout happy-dom doesn't do: rows stacked by DOM position, a tall table.
  (win.HTMLTableRowElement.prototype as unknown as HTMLTableRowElement).getBoundingClientRect = function (this: HTMLTableRowElement) {
    const i = [...(this.parentElement?.children ?? [])].indexOf(this);
    return { top: i * ROW_HEIGHT, bottom: (i + 1) * ROW_HEIGHT, height: ROW_HEIGHT, left: 0, right: 0, width: 0, x: 0, y: i * ROW_HEIGHT } as DOMRect;
  };
  const doc = win.document as unknown as Document;
  doc.getElementById('repos-view')!.getBoundingClientRect = () => ({ top: -1000, bottom: 10000 }) as DOMRect;

  const calls: Harness['calls'] = [];
  const listeners = new Map<string, (p: unknown) => void>();
  const api: Api = {
    async invoke<T>(cmd: string, args?: Record<string, unknown>) {
      calls.push({ cmd, args });
      const answer = await host(cmd, args);
      if (answer !== undefined) return answer as T;
      if (cmd === 'get_snapshot') return snap as T;
      if (cmd === 'get_meta') return meta as T;
      return null as T;
    },
    listen: (ev, fn) => { listeners.set(ev, fn as (p: unknown) => void); },
    startResize: () => { calls.push({ cmd: 'startResize' }); },
  };
  const stop = start(api);
  await tick();

  teardown = async () => {
    stop();
    await win.happyDOM.close();
    for (const [k, d] of Object.entries(saved)) {
      if (d) Object.defineProperty(globalThis, k, d); else delete (globalThis as Record<string, unknown>)[k];
    }
  };

  const tbody = () => doc.getElementById('tbody')!;
  return {
    win, doc, calls,
    emit: (event, payload) => listeners.get(event)?.(payload),
    rows: () => [...tbody().querySelectorAll('tr')].map((r) => r.dataset.repo?.slice(2) ?? ''),
    row: (name) => tbody().querySelector<HTMLTableRowElement>(`tr[data-repo="o/${name}"]`)!,
    view: () => (['repos', 'prs', 'urgent'].find((v) => !doc.getElementById(`${v}-view`)!.hidden) ?? ''),
  };
}

const sortedHeader = (h: Harness) => h.doc.querySelector('thead tr.cols th.sorted')?.textContent ?? null;
const header = (h: Harness, label: string) =>
  [...h.doc.querySelectorAll<HTMLElement>('thead tr.cols th')].find((th) => th.textContent === label)!;
const stored = (h: Harness, k: string): unknown => JSON.parse(h.win.localStorage.getItem(k) ?? 'null');

function pointer(h: Harness, target: EventTarget, type: string, clientY: number): void {
  target.dispatchEvent(new h.win.PointerEvent(type, { bubbles: true, button: 0, clientY }) as unknown as Event);
}

/** Presses on `name`'s row and moves the pointer to `toY` in small steps; `drop` releases it. */
function dragRow(h: Harness, name: string, toY: number, { drop = true } = {}): void {
  const row = h.row(name);
  const y0 = row.getBoundingClientRect().top + ROW_HEIGHT / 2;
  pointer(h, row.cells[3]!, 'pointerdown', y0);
  const step = toY > y0 ? 4 : -4;
  for (let y = y0; step > 0 ? y < toY : y > toY; y += step) pointer(h, h.win as unknown as EventTarget, 'pointermove', y);
  pointer(h, h.win as unknown as EventTarget, 'pointermove', toY);
  if (drop) {
    pointer(h, h.win as unknown as EventTarget, 'pointerup', toY);
    row.click();
  }
}

// ───────────────────────────── table ─────────────────────────────

test('renders a row per repo, sorted by urgency by default', async () => {
  const h = await setup();
  assert.deepEqual(h.rows(), ['beta', 'gamma', 'alpha']);
  assert.equal(sortedHeader(h), 'Urgency');
  assert.equal(h.doc.querySelector('tfoot td')!.textContent, '3 repos');
  assert.equal(h.doc.getElementById('repo-count')!.textContent, '3');
  assert.equal(h.doc.getElementById('urgent-count')!.textContent, '1');
  assert.ok(h.calls.some((c) => c.cmd === 'window_ready'));
});

test('clicking a header sorts by it, then flips it, and remembers', async () => {
  const h = await setup();
  header(h, 'Repo').click();
  assert.deepEqual(h.rows(), ['alpha', 'beta', 'gamma']);
  header(h, 'Repo').click();
  assert.deepEqual(h.rows(), ['gamma', 'beta', 'alpha']);
  assert.ok(h.doc.querySelector('th.sorted:not(.asc)'));
  assert.deepEqual(stored(h, 'sort'), { key: 'name', dir: 'desc' });
});

test('dragging a row reorders the table and switches it to manual order', async () => {
  const h = await setup();
  dragRow(h, 'beta', 2.5 * ROW_HEIGHT);
  assert.deepEqual(h.rows(), ['gamma', 'alpha', 'beta']);
  assert.equal(sortedHeader(h), null, 'no column is sorted');
  assert.deepEqual(stored(h, 'sort'), { key: 'manual' });
  assert.deepEqual(stored(h, 'order'), ['o/gamma', 'o/alpha', 'o/beta']);
  assert.equal(h.view(), 'repos', 'the click ending a drag does not open the urgent view');
  assert.ok(!h.doc.body.classList.contains('reordering'));
});

test('dragging upward and to the top works', async () => {
  const h = await setup();
  dragRow(h, 'alpha', 0);
  assert.deepEqual(h.rows(), ['alpha', 'beta', 'gamma']);
});

test('sorting after a drag goes back to the sorted view', async () => {
  const h = await setup();
  dragRow(h, 'beta', 2.5 * ROW_HEIGHT);
  header(h, 'Urgency').click();
  assert.deepEqual(h.rows(), ['beta', 'gamma', 'alpha']);
  assert.equal(sortedHeader(h), 'Urgency');
  assert.deepEqual(stored(h, 'order'), ['o/gamma', 'o/alpha', 'o/beta'], 'the manual order is kept');
});

test('a saved manual order is restored, with new repos last', async () => {
  const h = await setup({ storage: { sort: { key: 'manual' }, order: ['o/gamma', 'o/alpha'] } });
  assert.deepEqual(h.rows(), ['gamma', 'alpha', 'beta']);
  assert.equal(sortedHeader(h), null);
});

test('a poll arriving mid-drag waits for the drop', async () => {
  const h = await setup();
  dragRow(h, 'beta', 1.6 * ROW_HEIGHT, { drop: false });
  assert.deepEqual(h.rows(), ['gamma', 'beta', 'alpha']);
  h.emit('craft:snapshot', snapshot());
  assert.ok(h.row('beta').classList.contains('dragging'), 'the dragged row is still in hand');
  assert.deepEqual(h.rows(), ['gamma', 'beta', 'alpha']);
  pointer(h, h.win as unknown as EventTarget, 'pointerup', 1.6 * ROW_HEIGHT);
  assert.deepEqual(h.rows(), ['gamma', 'beta', 'alpha']);
  assert.ok(!h.doc.querySelector('tr.dragging'));
});

test('a cancelled drag restores the previous order', async () => {
  const h = await setup();
  dragRow(h, 'beta', 2.5 * ROW_HEIGHT, { drop: false });
  pointer(h, h.win as unknown as EventTarget, 'pointercancel', 0);
  assert.deepEqual(h.rows(), ['beta', 'gamma', 'alpha']);
  assert.equal(stored(h, 'order'), null);
});

test('the Type column names what each repo is, even before its first fetch', async () => {
  const h = await setup({ snap: snapshot([
    repo('lib', {}, { repo: 'storytold/lib', category: 'library' }),
    repo('new', null, { repo: 'storytold/new', category: 'ci', fetched_at: null }),
    repo('mine', {}, { repo: 'someone/mine', category: 'other' }),
  ]) });
  const i = COLUMNS.findIndex((c) => c.key === 'category');
  const type = (r: string) => h.doc.querySelector<HTMLTableRowElement>(`tr[data-repo="${r}"]`)!.cells[i]!.textContent;
  assert.equal(type('storytold/lib'), 'Library');
  assert.equal(type('storytold/new'), 'CI');
  assert.equal(type('someone/mine'), '—');
});

test('third-party repos are tagged 3P in the table and the urgent list', async () => {
  const h = await setup({ snap: snapshot([
    repo('ours', { urgency: 10, urgent: [issue(1, 10, false)] }, { repo: 'storytold/ours' }),
    repo('theirs', { urgency: 20, urgent: [issue(2, 20, false)] }, { repo: 'tangency-labs/theirs' }),
  ]) });
  const tag = (repo: string) => h.doc.querySelector(`tr[data-repo="${repo}"] .third-party`);
  assert.equal(tag('storytold/ours'), null);
  assert.equal(tag('tangency-labs/theirs')?.textContent, '3P');
  assert.equal(tag('tangency-labs/theirs')?.getAttribute('title'), 'Third-party app (tangency-labs)');
  h.doc.querySelector<HTMLElement>('[data-view="urgent"]')!.click();
  assert.equal(h.doc.querySelectorAll('.urgent-item .third-party').length, 1);
});

test('clicking a row, or barely moving it, opens its urgent issues', async () => {
  const h = await setup();
  const row = h.row('alpha');
  const y = row.getBoundingClientRect().top + 5;
  pointer(h, row, 'pointerdown', y);
  pointer(h, h.win as unknown as EventTarget, 'pointermove', y + 3);
  pointer(h, h.win as unknown as EventTarget, 'pointerup', y + 3);
  row.click();
  assert.equal(h.view(), 'urgent');
  assert.equal(h.doc.querySelector<HTMLSelectElement>('#urgent-repo')!.value, 'o/alpha');
  assert.deepEqual([...h.doc.querySelectorAll('.urgent-item .num')].map((n) => n.textContent), ['#1', '#2']);
  assert.equal(stored(h, 'order'), null, 'a click is not a reorder');
});

test('links in a row open on GitHub without dragging or switching views', async () => {
  const h = await setup();
  const link = h.row('gamma').querySelector<HTMLElement>('.repo .link')!;
  pointer(h, link, 'pointerdown', 25);
  pointer(h, h.win as unknown as EventTarget, 'pointermove', 60);
  link.click();
  assert.deepEqual(h.rows(), ['beta', 'gamma', 'alpha']);
  assert.equal(h.view(), 'repos');
  assert.deepEqual(h.calls.filter((c) => c.cmd === 'open_url').map((c) => c.args?.url), ['https://github.com/o/gamma']);
});

test('the Fetched column shows each repo’s data age and sorts by it, freshest first ascending', async () => {
  const ago = (s: number) => new Date(Date.now() - s * 1000).toISOString();
  const h = await setup({ snap: snapshot([
    repo('alpha', {}, { fetched_at: ago(56 * 60) }),
    repo('beta', {}, { fetched_at: ago(55 * 60) }),
    repo('gamma', null, { fetched_at: null }),
    repo('delta', null, { fetched_at: ago(2 * 3600), error: 'boom' }),
  ]) });
  const fetched = (name: string) => h.row(name).cells[1]!.textContent;
  assert.equal(h.doc.querySelectorAll('thead tr.cols th')[1]!.textContent, 'Fetched');
  assert.equal(fetched('beta'), '55m');
  assert.equal(fetched('delta'), '2h', 'a failed poll still shows when the data was fetched');
  assert.equal(fetched('gamma'), '—');
  header(h, 'Fetched').click();
  assert.deepEqual(h.rows(), ['delta', 'alpha', 'beta', 'gamma'], 'oldest data first, never-fetched last');
  header(h, 'Fetched').click();
  assert.deepEqual(h.rows(), ['beta', 'alpha', 'delta', 'gamma']);
});

test('the Build DL Rate column sits left of Build DLs and sorts across units', async () => {
  const fetched = '2026-10-08T12:00:00Z';
  const dayAgo = '2026-10-07T12:00:00Z';
  const out = (latest_build_downloads: number): Partial<RepoStats> => ({ latest_build_downloads, latest_build_at: dayAgo });
  const h = await setup({ snap: snapshot([
    repo('alpha', out(8640), { fetched_at: fetched }),
    repo('beta', out(172800), { fetched_at: fetched }),
    repo('gamma', { latest_build: null, latest_build_at: null }, { fetched_at: fetched }),
    repo('delta', out(0), { fetched_at: fetched }),
    repo('pending', null, { fetched_at: null }),
  ]) });
  const i = COLUMNS.findIndex((c) => c.key === 'latest_build_rate');
  const heads = [...h.doc.querySelectorAll('thead tr.cols th')].map((th) => th.textContent);
  assert.equal(heads[i], 'Build DL Rate');
  assert.equal(heads[i + 1], 'Build DLs');
  const rate = (name: string) => h.row(name).cells[i]!;
  assert.equal(rate('alpha').textContent, '6.0/m');
  assert.equal(rate('beta').textContent, '2.0/s');
  assert.equal(rate('gamma').textContent, '—');
  assert.equal(rate('delta').textContent, '0.0/d');
  assert.ok(rate('delta').querySelector('.zero'));
  assert.equal(rate('pending').textContent, '—');
  assert.equal(rate('alpha').querySelector('span')!.title, '8,640 downloads of v1.0.0, out 1d when counted');
  assert.equal(h.doc.querySelector('tfoot tr')!.children[i]!.textContent, '', 'no total');
  header(h, 'Build DL Rate').click();
  assert.deepEqual(h.rows(), ['beta', 'alpha', 'delta', 'gamma', 'pending']);
  header(h, 'Build DL Rate').click();
  assert.deepEqual(h.rows(), ['delta', 'alpha', 'beta', 'gamma', 'pending']);
});

test('a row’s refresh button refetches only that repo, and spins while it waits', async () => {
  const h = await setup();
  const button = () => h.row('gamma').querySelector<HTMLButtonElement>('.row-refresh')!;
  pointer(h, button(), 'pointerdown', 25);
  pointer(h, h.win as unknown as EventTarget, 'pointermove', 60);
  button().click();
  assert.equal(h.view(), 'repos', 'not a click on the row');
  assert.deepEqual(h.rows(), ['beta', 'gamma', 'alpha'], 'not a drag');
  assert.deepEqual(h.calls.filter((c) => c.cmd.startsWith('refresh')), [{ cmd: 'refresh_repo', args: { repo: 'o/gamma' } }]);
  assert.ok(button().classList.contains('spinning'), 'spins at once, before the poller answers');

  h.emit('craft:snapshot', { ...snapshot(), fetching: 'o/gamma', requested: ['o/alpha'] });
  assert.ok(button().classList.contains('spinning'));
  assert.ok(h.row('alpha').querySelector('.row-refresh.spinning'), 'queued counts too');
  assert.ok(!h.row('beta').querySelector('.row-refresh.spinning'));
  h.emit('craft:snapshot', snapshot());
  assert.ok(!h.doc.querySelector('.row-refresh.spinning'), 'done');
});

test('the window picker switches the activity columns and remembers', async () => {
  const h = await setup({ snap: snapshot([repo('alpha', { recent: { '7d': { commits: 7, prs_opened: 0, prs_merged: 0, issues_opened: 0, people: 0 } } })]) });
  const commits = () => h.row('alpha').cells[COLUMNS.findIndex((c) => c.key === 'recent_commits')]!.textContent;
  assert.equal(commits(), '—', 'the default 4 h window has no data');
  const picker = h.doc.querySelector<HTMLSelectElement>('.window-picker')!;
  picker.value = '7d';
  picker.dispatchEvent(new h.win.Event('change') as unknown as Event);
  assert.equal(commits(), '7');
  assert.equal(stored(h, 'window'), '7d');
});

test('an empty config says so', async () => {
  const h = await setup({ snap: snapshot([]) });
  assert.match(h.doc.getElementById('tbody')!.textContent ?? '', /No repositories configured/);
});

// ───────────────────────────── status ─────────────────────────────

test('status pill, failures banner and budget', async () => {
  const h = await setup({ snap: snapshot([repo('alpha', {}), repo('beta', null, { error: 'boom', fetched_at: null })]) });
  assert.equal(h.doc.getElementById('fresh-pill')!.textContent, 'STALE');
  const banner = h.doc.getElementById('banner')!;
  assert.equal(banner.hidden, false);
  assert.match(banner.textContent ?? '', /1 repo failed last poll: beta \(boom\)/);
  assert.equal(h.doc.getElementById('budget')!.textContent, 'GitHub budget 4,000');
  assert.ok(h.row('beta').classList.contains('error'));

  assert.equal(h.doc.getElementById('token')!.textContent, '', 'nothing to say before the first poll');

  const token = h.doc.getElementById('token')!;
  h.emit('craft:snapshot', { ...snapshot(), token: { source: '$GITHUB_TOKEN', rejected: false } });
  assert.equal(token.textContent, 'Token: $GITHUB_TOKEN');
  assert.ok(!token.classList.contains('bad'));
  h.emit('craft:snapshot', { ...snapshot(), token: { source: 'config', rejected: true }, error: 'GitHub rejected the token from config (401)' });
  assert.equal(token.textContent, 'Token rejected (config)');
  assert.ok(token.classList.contains('bad'));
  assert.match(banner.textContent ?? '', /rejected the token from config/);
  h.emit('craft:snapshot', { ...snapshot(), token: { source: null, rejected: false } });
  assert.equal(token.textContent, 'No GitHub token');
  assert.ok(token.classList.contains('bad'));

  h.emit('craft:snapshot', { ...snapshot(), refreshing: true });
  assert.equal(h.doc.getElementById('fresh-pill')!.textContent, 'REFRESHING');
  assert.equal(banner.hidden, true);
  h.emit('craft:snapshot', snapshot());
  assert.equal(h.doc.getElementById('fresh-pill')!.textContent, 'FRESH');
});

// ───────────────────────────── urgent view ─────────────────────────────

test('urgent view lists every repo’s issues, filters to critical, and opens them', async () => {
  const h = await setup();
  h.doc.querySelector<HTMLElement>('[data-view="urgent"]')!.click();
  const nums = () => [...h.doc.querySelectorAll('.urgent-item .num')].map((n) => n.textContent);
  assert.deepEqual(nums(), ['#1', '#3', '#2']);
  const only = h.doc.querySelector<HTMLInputElement>('#critical-only')!;
  only.checked = true;
  only.dispatchEvent(new h.win.Event('change') as unknown as Event);
  assert.deepEqual(nums(), ['#1']);
  assert.equal(stored(h, 'criticalOnly'), true);
  h.doc.querySelector<HTMLElement>('.urgent-item')!.click();
  assert.equal(h.calls.at(-1)?.args?.url, 'https://github.com/o/x/issues/1');
});

// ───────────────────────────── host ─────────────────────────────

test('keyboard shortcuts and buttons call the host', async () => {
  const h = await setup({ meta: { always_on_top: true, shortcut: 'CmdOrCtrl+Shift+G' } });
  const key = (key: string, mod = false) =>
    h.doc.dispatchEvent(new h.win.KeyboardEvent('keydown', { key, metaKey: mod, bubbles: true }) as unknown as Event);
  key('r', true);
  key('Escape');
  key('3', true);
  assert.equal(h.view(), 'urgent');
  key('1', true);
  assert.equal(h.view(), 'repos');
  h.doc.getElementById('pin')!.click();
  h.doc.getElementById('edit-config')!.click();
  h.doc.getElementById('resize')!.dispatchEvent(new h.win.MouseEvent('mousedown') as unknown as Event);
  const cmds = h.calls.map((c) => c.cmd).filter((c) => !['get_meta', 'get_snapshot', 'get_people', 'get_pr_overview', 'window_ready'].includes(c));
  assert.deepEqual(cmds, ['refresh_now', 'hide_window', 'set_always_on_top', 'open_config', 'startResize']);
  assert.deepEqual(h.calls.find((c) => c.cmd === 'set_always_on_top')?.args, { on: false });
  assert.equal(h.doc.getElementById('pin')!.getAttribute('aria-pressed'), 'true');
});

// ───────────────────────────── people tab ─────────────────────────────

const keydown = (h: Harness, key: string, mod = false) =>
  h.doc.dispatchEvent(new h.win.KeyboardEvent('keydown', { key, metaKey: mod, bubbles: true }) as unknown as Event);
const hoursBack = (h: number): string => new Date(Date.now() - h * 3600e3).toISOString();
const ws = (w: Partial<WorkState>): WorkState => ({
  state: 'awaiting_first_maintainer_response', subtype: null, waits_on: ['maintainer'], waits_for: [], since: hoursBack(1), guess: false,
  low: false, rule: '63: no maintainer comment, review, push or close yet', why: '', evidence: null, also: [], flags: [], hold: null,
  blocked_by: [], competing: [], ...w,
});
const prItem = (name: string, number: number, author: string, w: Partial<WorkState>): Item => ({
  repo: `o/${name}`, number, title: `Change ${number}`, url: `https://github.com/o/${name}/pull/${number}`, author, created_at: hoursBack(100), work: ws(w),
});
const pflow = (f: Partial<Flow>): Flow => ({
  merged: 0, merged_by: {}, landed_elsewhere: 0, closed: {}, closed_total: 0, median_merge_secs: null, median_ready_secs: null, ...f,
});
const prow = (key: string, p: Partial<PersonRow> = {}): PersonRow => ({
  key, login: key, name: null, on_roster: false, maintainer_of: [], owner_of: [], flow: {}, given: {}, response: {}, ...p,
});
const COMMENT_URL = 'https://github.com/o/alpha/pull/1#issuecomment-11';
const rank = (r: Partial<Rank>): Rank => ({
  merged: 0, landed: 0, added: 0, removed: 0, gen_added: 0, gen_removed: 0, files: 0, commits: 0, co_authored: 0, direct: null,
  active_days: 0, merges: 0, batch_merges: 0, reviews: 0, approved: 0, changes_requested: 0, commented: 0, reviewed_prs: 0,
  review_commits: 0, review_added: 0, review_removed: 0, types: {}, guessed: {}, stacked: 0, truncated: 0, unverified: false, ...r,
});
const rankRow = (key: string, windows: Record<string, Rank>, r: Partial<RankRow> = {}): RankRow => ({
  key, login: key, name: null, on_roster: false, maintainer: false, windows, ...r,
});
const people = (): PeopleView => ({
  generated_at: NOW, records: 12, fetched: 10,
  rankings: [
    rankRow('ann-example', {
      '7d': rank({ merged: 3, added: 500, removed: 20, gen_added: 40, commits: 4, active_days: 3, types: { feature: 2, docs: 1 }, guessed: { docs: 1 } }),
      '1d': rank({ merged: 1, added: 100, commits: 1, active_days: 1, types: { feature: 1 } }),
    }, { login: 'Ann-Example', name: 'Ann', on_roster: true }),
    rankRow('maint-one', {
      '7d': rank({ merges: 14, batch_merges: 14, reviews: 5, approved: 3, commented: 2, reviewed_prs: 4, review_commits: 2, review_added: 207, unverified: true, direct: 2, active_days: 2 }),
    }, { maintainer: true }),
    rankRow('bo-example', { '7d': rank({ merged: 1, landed: 1, added: 900, removed: 5, commits: 2, types: { fix: 1 }, active_days: 1 }) }),
  ],
  co_unlinked: { '7d': { 'Cy Unlinked': 2 } },
  unlinked: ['Roster Only'],
  load: { merged: 9747, total: 9747, closed: 120, closed_total: 120, done: true },
  checked: { 'o/alpha': NOW },
  repos: {
    'o/alpha': {
      '4h': {
        merged: 2, merged_by: { direct: 1, batch: 1 }, landed_elsewhere: 1, closed: { closed_withdrawn_or_abandoned: 1 },
        median_merge_secs: 7200, median_response_secs: 3600, methods: { 'merge commit': 2 }, mergers: { 'maint-one': 2 },
      },
    },
  },
  main: { 'o/alpha': { oid: 'head', rollup: 'FAILURE', failing: ['test'] } },
  items: [
    prItem('alpha', 1, 'ann-example', {
      state: 'held_for_owner', waits_on: ['owner'], waits_for: ['boss-one'], since: hoursBack(96), guess: true,
      rule: '10: a maintainer’s hold phrase, not lifted since', why: "matched the phrase 'owner's call' in maint-one's comment",
      evidence: { quote: 'It is the owner’s call.', author: 'maint-one', at: hoursBack(96), url: COMMENT_URL, phrase: "owner's call" },
      hold: { since: hoursBack(96), by: 'maint-one', category: 'brand', reaffirmed: null, owners: ['boss-one'] },
    }),
    prItem('alpha', 2, 'ann-example', { state: 'changes_requested_by_maintainer', waits_on: ['author'], waits_for: ['ann-example'], guess: true }),
    prItem('beta', 3, 'ann-example', { state: 'draft', waits_on: ['author'], waits_for: ['ann-example'], rule: '20: isDraft' }),
    prItem('beta', 5, 'bo-example', { state: 'author_stepped_back', waits_for: ['maint-one'], guess: true, low: true }),
  ],
  people: [
    prow('ann-example', {
      login: 'Ann-Example', name: 'Ann',
      flow: {
        '7d': { '*': pflow({ merged: 2, merged_by: { direct: 2 }, closed_total: 2 }), 'o/alpha': pflow({ merged: 2, closed_total: 2 }) },
        '1d': { '*': pflow({ merged: 1, closed_total: 1 }) },
      },
    }),
    prow('maint-one', { maintainer_of: ['o/alpha', 'o/beta'] }),
    prow('bo-example'),
    prow('boss-one', { owner_of: ['o/alpha'] }),
  ],
});

/** A harness whose host answers get_people with `view`, opened on the People tab. */
async function peopleSetup(view: PeopleView | null = people()): Promise<Harness> {
  const h = await setup({ storage: { peopleWindow: '7d' }, host: (cmd) => (cmd === 'get_people' ? view : undefined) });
  keydown(h, '4', true);
  return h;
}
const personRows = (h: Harness) => [...h.doc.querySelectorAll<HTMLElement>('#people-table tr.person-row')].map((r) => r.dataset.key);
const personCell = (h: Harness, key: string, col: number) =>
  h.doc.querySelector<HTMLTableRowElement>(`#people-table tr.person-row[data-key="${key}"]`)!.cells[col]!;
const caret = (h: Harness, key: string) => h.doc.querySelector<HTMLButtonElement>(`#people-table tr.person-row[data-key="${key}"] .caret`)!;
const peopleShown = (h: Harness) => !h.doc.getElementById('people-view')!.hidden;

test('Cmd+4 and the tab open People, which says it has no records until a view arrives', async () => {
  const h = await peopleSetup(null);
  assert.ok(peopleShown(h));
  assert.equal(h.doc.getElementById('repos-view')!.hidden, true);
  assert.equal(h.doc.querySelector('.view[data-view="people"]')!.getAttribute('aria-selected'), 'true');
  assert.match(h.doc.getElementById('people-main')!.textContent!, /No pull request records yet/);
  h.emit('craft:people', people());
  assert.deepEqual(personRows(h), ['ann-example', 'boss-one', 'maint-one', 'bo-example']);
  assert.equal(h.doc.getElementById('people-cached')!.textContent, '10 of 12 timelines read');
  assert.equal(h.doc.getElementById('people-count')!.textContent, '1', 'one pull request held for the owner');
  keydown(h, '1', true);
  assert.ok(!peopleShown(h));
  (h.doc.querySelector('.view[data-view="people"]') as HTMLElement).click();
  assert.ok(peopleShown(h));
  assert.equal(stored(h, 'view'), 'people');
});

test('the People view from get_people shows even when a snapshot drew the tab while it loaded', async () => {
  let release: (v: PeopleView) => void = () => {};
  const pending = new Promise<PeopleView>((resolve) => { release = resolve; });
  const h = await setup({ storage: { view: 'people', peopleWindow: '7d' }, host: (cmd) => (cmd === 'get_people' ? pending : undefined) });
  h.emit('craft:snapshot', snapshot());
  assert.match(h.doc.getElementById('people-main')!.textContent!, /No pull request records yet/);
  release(people());
  await tick();
  assert.deepEqual(personRows(h), ['ann-example', 'boss-one', 'maint-one', 'bo-example']);
  assert.equal(h.doc.getElementById('people-count')!.textContent, '1');
});

test('each person row counts open work, what waits on them and their flow, with the part read from comments dotted', async () => {
  const h = await peopleSetup();
  const open = personCell(h, 'ann-example', 1);
  assert.equal(open.textContent, '3 (2 from comments)');
  assert.ok(open.querySelector('.guess-count'), 'a count with states read from comments is dotted');
  assert.equal(open.querySelector<HTMLElement>('.guess-count')!.title, '2 of 3 states were read from a comment’s text, not from a GitHub field');
  assert.equal(personCell(h, 'ann-example', 2).textContent, '2 (1 from comments)');
  assert.equal(personCell(h, 'ann-example', 3).querySelector('span')!.title, 'Waiting on the owner 1');
  assert.equal(personCell(h, 'ann-example', 5).textContent, '2');
  assert.equal(personCell(h, 'ann-example', 9).textContent, '100%');
  assert.equal(personCell(h, 'boss-one', 2).textContent, '1 (1 from comments)');
  assert.equal(personCell(h, 'ann-example', 0).querySelector('.name')!.textContent, 'Ann', 'the roster name');
  assert.equal(personCell(h, 'boss-one', 0).querySelector('.name'), null, 'a login off the roster shows alone');
  // The window and repo pickers change the figures.
  const win = h.doc.getElementById('people-window') as HTMLSelectElement;
  win.value = '1d';
  win.dispatchEvent(new h.win.Event('change') as unknown as Event);
  assert.equal(personCell(h, 'ann-example', 5).textContent, '1');
  assert.equal(stored(h, 'peopleWindow'), '1d');
  const picker = h.doc.getElementById('people-repo') as HTMLSelectElement;
  picker.value = 'o/beta';
  picker.dispatchEvent(new h.win.Event('change') as unknown as Event);
  assert.equal(personCell(h, 'ann-example', 1).textContent, '1');
  assert.deepEqual(personRows(h), ['ann-example', 'maint-one', 'bo-example']);
  // A header click sorts.
  [...h.doc.querySelectorAll<HTMLElement>('#people-table th')].find((th) => th.textContent === 'Person')!.click();
  assert.deepEqual(personRows(h), ['ann-example', 'bo-example', 'maint-one']);
  assert.deepEqual(stored(h, 'peopleSort'), { key: 'person', dir: 'asc' });
  // A repo listed again in other letter case keeps the filter.
  h.emit('craft:snapshot', snapshot([repo('alpha', {}), repo('Beta', {}, { repo: 'o/Beta' }), repo('gamma', {})]));
  assert.equal(picker.value, 'o/Beta');
  assert.equal(personCell(h, 'ann-example', 1).textContent, '1');
  // When the filtered repo leaves the list, the filter goes back to every repo.
  h.emit('craft:snapshot', snapshot([repo('alpha', {}), repo('gamma', {})]));
  assert.equal(picker.value, '');
  assert.equal(personCell(h, 'ann-example', 1).textContent, '3 (2 from comments)');
});

test('a snapshot or the clock leaves the People table alone; a new view redraws it', async () => {
  const h = await peopleSetup();
  const row = () => h.doc.querySelector('#people-table tr.person-row[data-key="ann-example"]');
  const before = row();
  h.emit('craft:snapshot', snapshot());
  assert.ok(row() === before, 'the same node after a snapshot');
  h.emit('craft:people', people());
  assert.ok(row() !== before, 'a new People view draws it again');
});

test('the caret opens a row per repo and the person’s detail, and keeps its focus through a new view', async () => {
  const h = await peopleSetup();
  const c = caret(h, 'ann-example');
  assert.equal(c.getAttribute('aria-expanded'), 'false');
  c.click();
  const opened = caret(h, 'ann-example');
  assert.equal(opened.getAttribute('aria-expanded'), 'true');
  const subs = [...h.doc.querySelectorAll<HTMLElement>('#people-table tr.sub')];
  assert.deepEqual(subs.map((r) => r.dataset.repo), ['o/alpha', 'o/beta']);
  assert.equal((subs[0] as HTMLTableRowElement).cells[1]!.textContent, '2 (2 from comments)');
  assert.equal((subs[1] as HTMLTableRowElement).cells[1]!.textContent, '1');
  const detail = h.doc.querySelector('#people-table tr.detail[data-key="ann-example"]')!;
  assert.deepEqual([...detail.querySelectorAll('h4')].map((n) => n.textContent),
    ['Open work by state', 'Waiting on them', 'Held for the owner', 'Responses', 'Reviews given, last week', 'Flow, last week', 'Where their work went, last week']);
  assert.match(detail.textContent!, /Waiting on the author/);
  assert.match(detail.textContent!, /held 4d, by maint-one, brand/);
  opened.focus();
  h.emit('craft:people', people());
  assert.equal((h.doc.activeElement as HTMLElement).dataset.focus, 'caret:ann-example', 'a new view keeps the caret focused');
  assert.ok(h.doc.querySelector('#people-table tr.detail[data-key="ann-example"]'), 'and the person open');
  caret(h, 'ann-example').click();
  assert.equal(h.doc.querySelectorAll('#people-table tr.sub').length, 0);
});

test('states read from comment text are dotted, with their rule and sentence on hover, and a ? at low confidence', async () => {
  const h = await peopleSetup();
  caret(h, 'ann-example').click();
  const chips = [...h.doc.querySelectorAll<HTMLElement>('#people-table tr.detail .oldest .wstate')];
  const held = chips.find((c) => c.textContent === 'Held for owner')!;
  assert.ok(held.classList.contains('guess'));
  assert.match(held.title, /Rule 10: a maintainer’s hold phrase/);
  assert.match(held.title, /“It is the owner’s call\.” \(maint-one, /);
  assert.match(held.title, /Read from a comment’s text, not from a GitHub field: matched the phrase 'owner's call'/);
  held.click();
  assert.deepEqual(h.calls.at(-1), { cmd: 'open_url', args: { url: COMMENT_URL } });
  const draft = chips.find((c) => c.textContent === 'Draft')!;
  assert.ok(!draft.classList.contains('guess'), 'a field state shows plainly');
  caret(h, 'maint-one').click();
  const theirs = h.doc.querySelector('#people-table tr.detail[data-key="maint-one"]')!;
  const stepped = [...theirs.querySelectorAll<HTMLElement>('.wstate')].find((c) => c.textContent?.startsWith('Author stepped back'))!;
  assert.equal(stepped.querySelector('.q')?.textContent, '?');
  assert.match(stepped.title, /Low confidence\./);
});

test('a pull request not read yet shows as Not read yet, not as a rule’s state', async () => {
  const view = people();
  view.items.push(prItem('alpha', 9, 'bo-example', { state: NOT_READ_YET, waits_on: [], waits_for: [], since: hoursBack(6), rule: 'the timeline has not been read yet' }));
  const h = await peopleSetup(view);
  assert.equal(personCell(h, 'bo-example', 1).textContent, '2 (1 from comments)', 'it counts as their open work, not as one read from comments');
  assert.equal(personCell(h, 'maint-one', 2).textContent, '1 (1 from comments)', 'it waits on no maintainer');
  caret(h, 'bo-example').click();
  const detail = h.doc.querySelector('#people-table tr.detail[data-key="bo-example"]')!;
  const heads = [...detail.querySelectorAll('.muted.small')].map((n) => n.textContent);
  assert.ok(heads.includes('Not read yet'), heads.join(' | '));
  const chip = [...detail.querySelectorAll<HTMLElement>('.wstate')].find((c) => c.textContent === 'Not read yet')!;
  assert.ok(chip.classList.contains('unread') && !chip.classList.contains('guess'));
  assert.match(chip.title, /^Not read yet: the app has found this pull request but has not read its comments/);
  assert.match(detail.textContent!, /updated 6h ago/);
  assert.doesNotMatch(detail.textContent!, /Awaiting first response/);
});

// ───────────────────────────── rankings ─────────────────────────────

const detailRow = (key: string, d: Partial<DetailRow> = {}): DetailRow => ({
  key, merged: 0, added: 0, removed: 0, share: 0, merges: 0, reviews: 0, review_commits: 0, guessed: 0, rules: [], children: [], ...d,
});
const annDetail = (window: string): PersonDetail => ({
  key: 'ann-example', window, added: 500, removed: 20, merged: 3,
  by_repo: [detailRow('o/alpha', { merged: 3, added: 500, removed: 20, share: 1, reviews: 2 })],
  by_area: [detailRow('pen', { merged: 2, added: 400, share: 0.8, rules: ['the title’s Area: prefix'] })],
  by_type: [detailRow('docs', { merged: 1, added: 20, share: 0.04, guessed: 1, rules: ['R2: the area "Docs" names a kind of work'] })],
  by_dir: [detailRow('crates/pen', { merged: 2, added: 450, share: 0.9, children: [detailRow('tools', { merged: 1, added: 300, share: 0.6 })] })],
});

/** A harness on the People tab's Rankings view; the host answers the carets' requests. */
async function rankSetup(view: PeopleView = people()): Promise<Harness> {
  const h = await setup({
    storage: { peopleWindow: '7d', peopleMode: 'rankings' },
    host: (cmd, args) => (cmd === 'get_people' ? view : cmd === 'person_detail' ? annDetail(String(args?.window)) : undefined),
  });
  keydown(h, '4', true);
  return h;
}
const rankKeys = (h: Harness) => [...h.doc.querySelectorAll<HTMLElement>('#rank-table tr.rank-row')].map((r) => r.dataset.key);
const rankCell = (h: Harness, key: string, label: string) => {
  const ths = [...h.doc.querySelectorAll<HTMLElement>('#rank-table th')];
  const i = ths.findIndex((th) => th.textContent === label);
  return h.doc.querySelector<HTMLTableRowElement>(`#rank-table tr.rank-row[data-key="${key}"]`)!.cells[i]!;
};
const pick = (h: Harness, id: string, value: string) => {
  const s = h.doc.getElementById(id) as HTMLSelectElement;
  s.value = value;
  s.dispatchEvent(new h.win.Event('change') as unknown as Event);
};
const hostCalls = (h: Harness) => h.calls.filter((c) => !['get_meta', 'get_snapshot', 'get_people', 'window_ready', 'startResize'].includes(c.cmd));

test('the Rankings view sorts by each column of each ranking', async () => {
  const h = await rankSetup();
  assert.equal(h.doc.querySelector('#people-view .seg[data-pmode="rankings"]')!.getAttribute('aria-selected'), 'true');
  assert.equal((h.doc.getElementById('people-repo') as HTMLSelectElement).hidden, true);
  assert.deepEqual(rankKeys(h), ['ann-example', 'bo-example', 'maint-one'], 'most merged first; maint-one has direct commits');
  assert.equal(rankCell(h, 'ann-example', 'Lines').textContent, '+500 −20');
  assert.equal(rankCell(h, 'bo-example', 'Direct to main').textContent, '—');
  assert.match(rankCell(h, 'bo-example', 'Direct to main').querySelector('span')!.title, /7-day and release windows/);
  assert.equal(rankCell(h, 'maint-one', 'Direct to main').textContent, '2');
  for (const mode of ['merged', 'maintainer', 'type'] as const) {
    pick(h, 'people-rank', mode);
    assert.equal(stored(h, 'rankMode'), mode);
    for (const col of RANK_COLUMNS[mode]) {
      [...h.doc.querySelectorAll<HTMLElement>('#rank-table th')].find((th) => th.textContent === col.label)!.click();
      const sort = stored(h, 'rankSort') as { key: string; dir: 'asc' | 'desc' };
      assert.equal(sort.key, col.key);
      assert.equal(h.doc.querySelector('#rank-table th.sorted')!.textContent, col.label);
      assert.deepEqual(rankKeys(h), rankRows(people(), '7d', mode, sort).map((r) => r.row.key), `${mode} by ${col.key}`);
    }
  }
  // The type picker sorts the Type ranking, and lists a sort by person.
  const typeSel = h.doc.getElementById('people-type') as HTMLSelectElement;
  assert.equal(typeSel.hidden, false);
  pick(h, 'people-type', 'type:fix');
  assert.deepEqual(rankKeys(h), ['bo-example', 'ann-example']);
  assert.equal(h.doc.querySelector('#rank-table th.sorted')!.textContent, 'fix');
  [...h.doc.querySelectorAll<HTMLElement>('#rank-table th')].find((th) => th.textContent === 'Person')!.click();
  assert.equal(typeSel.value, 'person');
  assert.ok(typeSel.selectedIndex >= 0, 'an option is selected');
  pick(h, 'people-type', PRODUCT);
  pick(h, 'people-type', 'person');
  assert.deepEqual(stored(h, 'rankSort'), { key: 'person', dir: 'asc' });
});

test('the People tab shows how old its records are, in the alert color once stale', async () => {
  const h = await rankSetup({ ...people(), checked: { 'o/alpha': new Date().toISOString() } });
  const age = h.doc.getElementById('people-age')!;
  assert.match(age.textContent!, /^Pull requests as of .+ \(now\)$/);
  assert.equal(age.classList.contains('stale'), false);
  const old = new Date(Date.now() - 3 * 86400e3).toISOString();
  h.emit('craft:people', { ...people(), checked: { 'o/alpha': old, 'o/beta': null } });
  assert.match(age.textContent!, /^Pull requests as of .+ \(3d ago\) · not checked yet: beta$/);
  assert.equal(age.classList.contains('stale'), true);
  assert.match(age.title, /Stale after 15 min/);
});

test('changing the window redraws the rankings without a request', async () => {
  const h = await rankSetup();
  const before = hostCalls(h).length;
  pick(h, 'people-window', '1d');
  assert.deepEqual(rankKeys(h), ['ann-example']);
  assert.equal(rankCell(h, 'ann-example', 'Merged').textContent, '1');
  assert.equal(hostCalls(h).length, before, 'no fetch command');
  // Since the latest build, each header says how the spans add up.
  pick(h, 'people-window', 'release');
  const merged = [...h.doc.querySelectorAll<HTMLElement>('#rank-table th')].find((th) => th.textContent === 'Merged')!;
  assert.match(merged.title, /each repository starts at its own latest build/);
  (h.doc.querySelector('#people-view .seg[data-pmode="work"]') as HTMLElement).click();
  assert.ok(h.doc.getElementById('people-table'));
  assert.equal(stored(h, 'peopleMode'), 'work');
});

test('the ? marker, the batch badge and the unverified mark show their hover text', async () => {
  const h = await rankSetup();
  pick(h, 'people-rank', 'type');
  const docs = rankCell(h, 'ann-example', 'docs');
  assert.equal(docs.querySelector('.q')!.textContent, '?');
  assert.match(docs.querySelector<HTMLElement>('.q')!.title, /^1 of 1 docs pull requests got their type from a fallback rule/);
  assert.equal(rankCell(h, 'ann-example', 'feature').querySelector('.q'), null, 'no fallback, no marker');
  assert.equal(rankCell(h, 'ann-example', 'Product work').textContent, '2');
  pick(h, 'people-rank', 'maintainer');
  const merges = rankCell(h, 'maint-one', 'Merges');
  assert.equal(merges.querySelector('.batch')!.textContent, '14 in batch pushes');
  assert.equal(merges.querySelector<HTMLElement>('.batch')!.title, BATCH_TIP);
  assert.equal(merges.querySelector<HTMLElement>('.unverified')!.title, UNVERIFIED_TIP);
  assert.equal(rankCell(h, 'maint-one', 'Review lines').textContent, '+207 −0');
  // Unlinked roster rows and co-authors are listed under the table.
  assert.match(h.doc.getElementById('rank-unlinked')!.textContent!, /Not linked to a GitHub account: Roster Only\./);
  assert.match(h.doc.getElementById('people-main')!.textContent!, /Co-authors without a known account: Cy Unlinked \(2\)\./);
});

test('the partial-load header shows until every merged and closed pull request is loaded', async () => {
  const loading = { ...people(), load: { merged: 1240, total: 9747, closed: 30, closed_total: 120, done: false } };
  const h = await rankSetup(loading);
  assert.equal(h.doc.getElementById('people-cached')!.textContent,
    '10 of 12 timelines read · 1,240 of 9,747 merged and 30 of 120 closed pull requests loaded');
  h.emit('craft:people', { ...people(), fetched: 12 });
  assert.equal(h.doc.getElementById('people-cached')!.textContent, '');
});

test('opening a caret asks for that person’s tables in the current window', async () => {
  const h = await rankSetup();
  const caretOf = (key: string) => h.doc.querySelector<HTMLButtonElement>(`#rank-table tr.rank-row[data-key="${key}"] .caret`)!;
  caretOf('ann-example').click();
  assert.deepEqual(hostCalls(h).at(-1), { cmd: 'person_detail', args: { login: 'ann-example', window: '7d' } });
  await tick();
  const detail = () => h.doc.querySelector('#rank-table tr.detail[data-key="ann-example"]')!;
  assert.deepEqual([...detail().querySelectorAll('h4')].map((n) => n.textContent), ['By repository', 'By area', 'By type', 'By directory']);
  assert.match(detail().textContent!, /3 merged pull requests and \+500 −20 product lines, last week\. GitHub records no hours/);
  const typeQ = detail().querySelector<HTMLElement>('.q')!;
  assert.match(typeQ.title, /R2: the area "Docs" names a kind of work/, 'the ? shows the rule that fired');
  // A directory opens its modules.
  assert.equal(detail().textContent!.includes('tools'), false);
  detail().querySelector<HTMLButtonElement>('.dir-toggle')!.click();
  assert.ok(detail().querySelector('tr.sub .module'));
  assert.match(detail().textContent!, /tools/);
  // A second click closes it.
  detail().querySelector<HTMLButtonElement>('.dir-toggle')!.click();
  assert.equal(detail().textContent!.includes('tools'), false);
  // The window picker asks again with the new window; the same answer is not asked twice.
  pick(h, 'people-window', '1d');
  assert.deepEqual(hostCalls(h).at(-1), { cmd: 'person_detail', args: { login: 'ann-example', window: '1d' } });
  const n = hostCalls(h).length;
  pick(h, 'people-window', '7d');
  assert.equal(hostCalls(h).length, n, 'the 7d answer is kept');
  // A new People view asks again for the open caret, and shows the kept answer meanwhile.
  h.emit('craft:people', people());
  assert.deepEqual(hostCalls(h).at(-1), { cmd: 'person_detail', args: { login: 'ann-example', window: '7d' } });
  assert.match(detail().textContent!, /^3 merged pull requests/);
  // A second click on the caret closes the detail row.
  caretOf('ann-example').click();
  assert.ok(!h.doc.querySelector('#rank-table tr.detail[data-key="ann-example"]'), 'the detail row is gone');
  assert.equal(caretOf('ann-example').getAttribute('aria-expanded'), 'false');
});

test('an answer asked for before a new People view is dropped, and the caret asks again', async () => {
  const answers: ((d: PersonDetail) => void)[] = [];
  const h = await setup({
    storage: { peopleWindow: '7d', peopleMode: 'rankings' },
    host: (cmd) => (cmd === 'get_people' ? people() : cmd === 'person_detail' ? new Promise<PersonDetail>((r) => answers.push(r)) : undefined),
  });
  keydown(h, '4', true);
  h.doc.querySelector<HTMLButtonElement>('#rank-table tr.rank-row[data-key="ann-example"] .caret')!.click();
  assert.equal(answers.length, 1);
  h.emit('craft:people', people());
  assert.equal(answers.length, 2, 'the open caret asks again for the new view');
  answers[0]!({ ...annDetail('7d'), merged: 99 });
  await tick();
  const detail = () => h.doc.querySelector('#rank-table tr.detail[data-key="ann-example"]')!.textContent!;
  assert.match(detail(), /Loading where their work went/, 'the old answer is dropped');
  answers[1]!(annDetail('7d'));
  await tick();
  assert.match(detail(), /^3 merged pull requests/);
});

test('the work view’s caret shows where the person’s work went, in the picked repo alone when there is one', async () => {
  const h = await setup({
    storage: { peopleWindow: '7d' },
    host: (cmd, args) => (cmd === 'get_people' ? people() : cmd === 'person_detail' ? annDetail(String(args?.window)) : undefined),
  });
  keydown(h, '4', true);
  caret(h, 'ann-example').click();
  assert.deepEqual(hostCalls(h).at(-1), { cmd: 'person_detail', args: { login: 'ann-example', window: '7d' } });
  await tick();
  const detail = () => h.doc.querySelector('#people-table tr.detail[data-key="ann-example"]')!;
  assert.ok([...detail().querySelectorAll('h4')].some((n) => n.textContent === 'By directory'));
  pick(h, 'people-repo', 'o/alpha');
  assert.deepEqual(hostCalls(h).at(-1), { cmd: 'person_detail', args: { login: 'ann-example', window: '7d', repo: 'o/alpha' } });
  assert.ok([...detail().querySelectorAll('h4')].some((n) => n.textContent === 'Where their work went in alpha, last week'));
});

// ───────────────────────────── repo carets ─────────────────────────────

test('the caret on a Repos row shows its people and open work without opening its issues', async () => {
  const repoPeople: RepoPeople = {
    repo: 'o/alpha', window: '4h', batch: true,
    people: [rankRow('ann-example', { '4h': rank({ merged: 2, added: 30, removed: 1, merges: 0, types: { fix: 2 } }) }, { login: 'Ann-Example' }),
      rankRow('maint-one', { '4h': rank({ merges: 2, reviews: 1, unverified: true }) })],
  };
  const h = await setup({ host: (cmd) => (cmd === 'get_people' ? people() : cmd === 'repo_detail' ? repoPeople : undefined) });
  const repoCaret = () => h.row('alpha').querySelector<HTMLButtonElement>('.caret')!;
  assert.equal(repoCaret().getAttribute('aria-expanded'), 'false');
  repoCaret().click();
  assert.equal(h.view(), 'repos', 'the row’s own click is not taken');
  assert.deepEqual(hostCalls(h).at(-1), { cmd: 'repo_detail', args: { repo: 'o/alpha', window: '4h' } });
  assert.equal(repoCaret().getAttribute('aria-expanded'), 'true');
  assert.equal((h.doc.activeElement as HTMLElement).dataset.focus, 'repo-caret:o/alpha');
  await tick();
  const detail = h.doc.querySelector<HTMLTableRowElement>('#tbody tr.repo-detail[data-detail="o/alpha"]')!;
  assert.equal(detail.previousElementSibling, h.row('alpha'), 'right under its row');
  const text = detail.textContent!;
  assert.match(text, /Ann-Example/);
  assert.ok(detail.querySelector('.unverified'), 'a batch-branch repo marks its merges');
  assert.match(text, /Open pull requests: 2/);
  assert.match(text, /Waiting on the owner: 1 \(50%\)\. Held for owner 1\./);
  assert.match(text, /1 held for the owner/);
  assert.match(text, /Maintainer requests: fixes 1\./);
  assert.match(text, /Checks on main: failure, failing test\./);
  assert.match(text, /Merged 2 \(direct 1, batch 1\), landed elsewhere 1\./);
  assert.match(text, /Merge method: merge commit 2\. Mergers: maint-one 2\./);
  // A drag that starts on the caret is no drag.
  pointer(h, repoCaret(), 'pointerdown', 10);
  pointer(h, h.doc, 'pointermove', 80);
  pointer(h, h.doc, 'pointerup', 80);
  assert.equal(stored(h, 'sort'), null, 'no manual order saved');
  repoCaret().click();
  assert.equal(h.doc.querySelector('#tbody tr.repo-detail'), null);
});

test('an open repo detail keeps its nodes through snapshots and the clock, and a new People view redraws it', async () => {
  const h = await setup({ host: (cmd) => (cmd === 'get_people' ? people() : undefined) });
  await tick();
  h.row('alpha').querySelector<HTMLButtonElement>('.caret')!.click();
  await tick();
  const chip = () => h.doc.querySelector('#tbody tr.repo-detail .wstate');
  const before = chip();
  assert.ok(before, 'the detail shows a state chip');
  // A repo starting and finishing its fetch sends two snapshots.
  h.emit('craft:snapshot', { ...snapshot(), fetching: 'o/alpha' });
  h.emit('craft:snapshot', snapshot());
  assert.equal(h.doc.querySelector('#tbody tr.repo-detail')!.previousElementSibling, h.row('alpha'), 'still under its row');
  // Compared with ===: a failed assert.equal would print two DOM trees.
  assert.ok(chip() === before, 'the hovered chip is the same node');
  h.emit('craft:people', people());
  assert.ok(chip() !== before, 'a new People view redraws the detail');
});

test('a repo caret keeps the keyboard focus when its row is drawn again', async () => {
  const h = await setup({ host: (cmd) => (cmd === 'get_people' ? people() : undefined) });
  const repoCaret = () => h.row('alpha').querySelector<HTMLButtonElement>('.caret')!;
  repoCaret().focus();
  const old = repoCaret();
  assert.ok(h.doc.activeElement === old);
  const later = new Date(Date.parse(NOW) + 5 * 60e3).toISOString();
  h.emit('craft:snapshot', snapshot([repo('alpha', { urgency: 100 }, { fetched_at: later }), repo('beta', {}), repo('gamma', {})]));
  assert.equal((h.doc.activeElement as HTMLElement | null)?.dataset?.focus, 'repo-caret:o/alpha', 'the caret has the focus');
});

// ───────────────────────────── tabs ─────────────────────────────

test('the tabs are Downloads, PRs, Fix first and People, in that order, opening on Downloads', async () => {
  const h = await setup();
  assert.deepEqual([...h.doc.querySelectorAll<HTMLElement>('[data-view]')].map((b) => b.dataset.view), ['repos', 'prs', 'urgent', 'people']);
  assert.match(h.doc.querySelector('[data-view="repos"]')!.textContent!, /Downloads/);
  assert.equal(h.view(), 'repos');
  assert.equal(h.doc.querySelector('[data-view="repos"]')!.getAttribute('aria-selected'), 'true');
});

test('Downloads leads with the build, then its download columns', async () => {
  const keys = COLUMNS.map((c) => c.key);
  const first = keys.indexOf('latest_build');
  assert.equal(first, keys.indexOf('category') + 1, 'right after the repo\'s name and type');
  assert.deepEqual(keys.slice(first, first + 4), ['latest_build', 'latest_build_at', 'latest_build_rate', 'latest_build_downloads']);
  const h = await setup();
  const groups = [...h.doc.querySelectorAll('thead tr.groups th')].map((th) => th.textContent);
  assert.equal(groups.includes('Downloads'), true);
});

test('third-party repos stay on Downloads and Fix first, tagged', async () => {
  const third = { repo: 'tangency-labs/solidcraft', stats: stats({ urgent: [issue(9, 70, false)] }), fetched_at: NOW };
  const h = await setup({ snap: snapshot([repo('alpha', {}), third]) });
  const row = h.doc.querySelector('tr[data-repo="tangency-labs/solidcraft"]')!;
  assert.equal(row.querySelector('.third-party')!.textContent, '3P');
  h.doc.querySelector<HTMLElement>('[data-view="urgent"]')!.click();
  assert.match(h.doc.getElementById('urgent-list')!.textContent!, /Issue 9/);
});

test('keys move between the tabs and refresh follows the tab', async () => {
  const h = await setup();
  const key = (k: string) => h.doc.dispatchEvent(new h.win.KeyboardEvent('keydown', { key: k, metaKey: true, bubbles: true }) as unknown as Event);
  key('2');
  assert.equal(h.view(), 'prs');
  key('r');
  assert.equal(h.calls.at(-1)!.cmd, 'refresh_prs');
  key('3');
  assert.equal(h.view(), 'urgent');
  key('r');
  assert.equal(h.calls.at(-1)!.cmd, 'refresh_now');
  key('1');
  assert.equal(h.view(), 'repos');
});

test('a config problem shows on the PR tab as well', async () => {
  const h = await setup({ snap: { ...snapshot(), error: 'Config error: bad toml' }, storage: { view: 'prs' } });
  assert.equal(h.view(), 'prs');
  assert.match(h.doc.getElementById('banner')!.textContent!, /Config error/);
});

// ───────────────────────────── repo toggles ─────────────────────────────

test('the toggles sit beside the tabs, hide repos on Downloads and Fix first, and are remembered', async () => {
  const fonts = repo('fonts', { urgency: 1, urgent: [issue(7, 30, false)], critical_issues: 2 }, { category: 'assets' });
  const third = { repo: 'tangency-labs/solidcraft', category: 'app' as const, stats: stats({ urgent: [issue(9, 70, false)] }), fetched_at: NOW };
  const alpha = { ...repo('alpha', { open_prs: 3 }, { category: 'app' }), repo: 'storytold/alpha' };
  const h = await setup({ snap: snapshot([alpha, { ...fonts, repo: 'storytold/fonts' }, third]) });
  const toggle = (id: string) => h.doc.querySelector<HTMLElement>(`[data-kind="${id}"]`)!;
  assert.equal(h.doc.getElementById('repo-kinds')!.previousElementSibling!.getAttribute('role'), 'tablist');
  assert.equal(toggle('assets').getAttribute('aria-pressed'), 'true');
  assert.equal(h.rows().length, 3);

  toggle('assets').click();
  assert.equal(toggle('assets').getAttribute('aria-pressed'), 'false');
  assert.equal(h.rows().includes('orytold/fonts'), false);
  assert.match(h.doc.getElementById('tfoot')!.textContent!, /2 repos/);
  assert.equal(h.doc.getElementById('repo-count')!.textContent, '2');
  assert.deepEqual(JSON.parse(h.win.localStorage.getItem('hiddenKinds')!), ['assets']);

  toggle('third_party').click();
  assert.equal(h.rows().length, 1, 'only the first party app is left');
  h.doc.querySelector<HTMLElement>('[data-view="urgent"]')!.click();
  assert.doesNotMatch(h.doc.getElementById('urgent-list')!.textContent!, /Issue (7|9)/);
  assert.equal(h.doc.getElementById('urgent-count')!.textContent, '');
  toggle('assets').click();
  assert.match(h.doc.getElementById('urgent-list')!.textContent!, /Issue 7/);
});

test('hiding everything says why, and saved toggles apply from the start', async () => {
  const h = await setup({ storage: { hiddenKinds: ['third_party'] } });
  assert.equal(h.doc.querySelector('[data-kind="third_party"]')!.getAttribute('aria-pressed'), 'false');
  assert.equal(h.doc.querySelector('tr[data-repo]'), null);
  assert.match(h.doc.getElementById('tbody')!.textContent!, /hidden by the toggles/);
});

test('Total DLs sits with the activity columns and only counts under All time', async () => {
  const i = COLUMNS.findIndex((c) => c.key === 'downloads_total');
  assert.equal(COLUMNS[i]!.group, RECENT);
  assert.equal(COLUMNS[i - 1]!.group, RECENT, 'beside the others, so the dropdown spans it');
  const h = await setup({ snap: snapshot([repo('alpha', { downloads_total: 1234 })]), storage: { window: '4h' } });
  const cell = () => h.row('alpha').cells[i]!.textContent;
  const foot = () => (h.doc.getElementById('tfoot')!.querySelector('tr') as HTMLTableRowElement).cells[i]!.textContent;
  assert.equal(cell(), '—');
  assert.equal(foot(), '');
  const picker = h.doc.querySelector<HTMLSelectElement>('.window-picker')!;
  picker.value = 'all';
  picker.dispatchEvent(new h.win.Event('change') as unknown as Event);
  assert.equal(cell(), '1,234');
  assert.equal(foot(), '1,234');
});
