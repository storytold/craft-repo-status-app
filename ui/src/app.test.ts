// The UI end to end in happy-dom: index.html's markup, the real `start`, and a
// fake host API standing in for Tauri.

import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { Window } from 'happy-dom';
import { start } from './app.ts';
import type { Api } from './app.ts';
import { COLUMNS } from './model.ts';
import type { Meta, RepoEntry, RepoStats, Snapshot, UrgentIssue } from './model.ts';

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

/** A fresh document with the UI started in it. */
async function setup({ snap = snapshot(), storage = {} as Record<string, unknown>, meta = {} as Meta } = {}): Promise<Harness> {
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
    view: () => (doc.getElementById('repos-view')!.hidden ? 'urgent' : 'repos'),
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

test('removing the selected repo resets the urgent filter to all repositories', async () => {
  const h = await setup();
  h.row('alpha').click();
  h.emit('craft:snapshot', snapshot([repo('beta', { urgent: [issue(3, 50, false)] })]));
  assert.equal(h.doc.querySelector<HTMLSelectElement>('#urgent-repo')!.value, '');
  assert.deepEqual([...h.doc.querySelectorAll('.urgent-item .num')].map((n) => n.textContent), ['#3']);
  h.emit('craft:snapshot', snapshot());
  assert.equal(h.doc.querySelector<HTMLSelectElement>('#urgent-repo')!.value, '', 'the removed filter stays cleared');
  assert.equal(h.doc.querySelectorAll('.urgent-item').length, 3);
});

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
  key('2', true);
  assert.equal(h.view(), 'urgent');
  key('1', true);
  assert.equal(h.view(), 'repos');
  h.doc.getElementById('pin')!.click();
  h.doc.getElementById('edit-config')!.click();
  h.doc.getElementById('resize')!.dispatchEvent(new h.win.MouseEvent('mousedown') as unknown as Event);
  const cmds = h.calls.map((c) => c.cmd).filter((c) => !['get_meta', 'get_snapshot', 'window_ready'].includes(c));
  assert.deepEqual(cmds, ['refresh_now', 'hide_window', 'set_always_on_top', 'open_config', 'startResize']);
  assert.deepEqual(h.calls.find((c) => c.cmd === 'set_always_on_top')?.args, { on: false });
  assert.equal(h.doc.getElementById('pin')!.getAttribute('aria-pressed'), 'true');
});
