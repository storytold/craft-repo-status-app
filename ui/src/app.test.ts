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
  head_oid: null, parity_oids: {},
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

type ParityReport = NonNullable<RepoEntry['parity']>;
type ParityGap = ParityReport['gaps'][number];
const noValue = { state: 'absent', why: 'Not read yet' } as const;

/** The parts of a parity report beyond the headline numbers, empty. */
const parityParts = () => ({
  breadth_links: [] as string[], reviewed: null, dims: [], heat: [], dims_total: noValue, top_drag: noValue, areas: [],
  areas_total: noValue, area_alts: [], area_weight_sum: null, area_weights_why: null, gaps: [], checklists: [], docs: [],
  gap_totals: { beta: null, partial: null, alpha: 0, alpha_none: false, stated: noValue, ids_stable: false, form: '', beta_unknown: true },
});

const okFigure = (pct: number, label: string, line = 12): ParityReport['breadth'] => ({
  state: 'ok', pct, low: null, high: null, label, text: `~${pct}%`, kind: 'estimated', basis: 'estimated', path: 'ROADMAP.md', line, from: null,
});

/** A parity report read at commit c1, with Breadth 80% and Ready 55%. */
const report = (p: Partial<ParityReport> = {}): ParityReport => ({
  version: 2, commit: 'c1', roadmap_oid: 'r1', parity_doc: true, na: null, last_reviewed: '2026-10-10',
  breadth: okFigure(80, 'Feature breadth'), ready: okFigure(55, 'Ready for real work', 13), breadth_alts: [], menu_breadth: noValue,
  menu_hint: null, mainstream: noValue, essentials: noValue, to_alpha: noValue, to_beta: noValue, to_full: noValue, stage: noValue,
  files: [], notes: [], ...parityParts(), ...p,
});

const parityGap = (id: string, g: Partial<ParityGap> = {}): ParityGap => ({
  key: id, id, rank: 0, group: null, title: `Gap ${id}`, alpha: 'no', beta: 'no', blocks_src: null, scope_note: null, priority: null,
  kinds: [], hours: noValue, counted_in: null, doc: null, workstream: null, src: { path: 'docs/gaps.md', line: 4 }, detail_src: null,
  detail: null, closed: false, paired_with: [], issues: [], ...g,
});

test('the parity columns show Breadth and Ready, explain dashes, sort, and open the parity document', async () => {
  type Parity = NonNullable<RepoEntry['parity']>;
  type Figure = Parity['breadth'];
  const figure = (pct: number, label: string): Figure => ({
    state: 'ok', pct, low: null, high: null, label, text: `~${pct}%`, kind: 'estimated', basis: 'estimated', path: 'ROADMAP.md', line: 12, from: null,
  });
  const absent = (why: string): Figure => ({ state: 'absent', why });
  const unread = { state: 'absent', why: 'Not read yet' } as const;
  const parity = (p: Partial<Parity>): Parity => ({
    version: 1, commit: 'abc123', roadmap_oid: 'r1', parity_doc: true, na: null, last_reviewed: '2026-10-10',
    breadth: figure(80, 'Feature breadth'), ready: figure(50, 'Ready for real work'), breadth_alts: [], menu_breadth: unread,
    menu_hint: null, mainstream: unread, essentials: unread, to_alpha: unread, to_beta: unread, to_full: unread,
    stage: unread, files: [], notes: [], ...parityParts(), ...p,
  });
  const none = 'No ROADMAP.md or docs/target-app-parity.md at HEAD';
  const h = await setup({ snap: snapshot([
    repo('alpha', { urgency: 100 }, { parity: parity({ ready: figure(61, 'Ready for real work') }) }),
    repo('beta', { urgency: 300 }, { parity: parity({ roadmap_oid: null, parity_doc: false, na: none, breadth: absent(none), ready: absent(none) }) }),
    repo('gamma', { urgency: 200 }, { parity: parity({
      parity_doc: false, breadth: { state: 'unreadable', why: 'No percentage in the Value cell of Feature breadth', raw: 'most', path: 'ROADMAP.md', line: 9 },
    }) }),
    repo('delta', null, { fetched_at: null }),
    repo('gone', null, { inaccessible: true }),
  ]) });
  const col = (label: string) => [...h.doc.querySelectorAll('thead tr.cols th')].findIndex((th) => th.textContent === label);
  const breadth = col('Breadth');
  const ready = col('Ready');
  assert.equal(ready, breadth + 1);
  const groups = [...h.doc.querySelectorAll('thead tr.groups th')].map((th) => th.textContent);
  assert.ok(groups.includes('Parity'));
  const cell = (name: string, i: number) => h.row(name).cells[i]!;
  assert.equal(cell('alpha', breadth).textContent, '80%');
  assert.equal(cell('alpha', ready).textContent, '61%');
  assert.match(cell('alpha', ready).querySelector<HTMLElement>('.parity')!.title, /^Ready for real work: ~61%\nEstimated · estimated\nROADMAP\.md last reviewed 2026-10-10\n/);
  // A repo without the documents shows a plain dash, and its tooltip gives the reason.
  assert.equal(cell('beta', ready).textContent, '—');
  assert.equal(cell('beta', ready).querySelector<HTMLElement>('.parity')!.title, none);
  assert.ok(cell('beta', ready).querySelector('.parity .muted'));
  assert.equal(cell('beta', ready).querySelector('.link'), null);
  // A row that did not parse shows a dash with an amber dot and links to its line.
  assert.ok(cell('gamma', breadth).querySelector('.warn-dot'));
  assert.match(cell('gamma', breadth).querySelector<HTMLElement>('.parity')!.title, /ROADMAP\.md line 9: most/);
  // A repo not read yet shows a dash, and an inaccessible repo shows nothing.
  assert.equal(cell('delta', ready).textContent, '—');
  assert.equal(cell('delta', ready).querySelector<HTMLElement>('.muted')!.title, 'Not read yet');
  assert.equal(cell('gone', ready).textContent, '');
  // Percentages have no total in the footer.
  assert.equal(h.doc.querySelector('tfoot tr')!.querySelectorAll('td')[ready]!.textContent, '');

  header(h, 'Ready').click();
  assert.deepEqual(h.rows(), ['alpha', 'gamma', 'beta', 'delta', 'gone'], 'highest first, missing numbers last');
  cell('alpha', breadth).querySelector<HTMLElement>('.link')!.click();
  cell('gamma', breadth).querySelector<HTMLElement>('.link')!.click();
  assert.deepEqual(h.calls.filter((c) => c.cmd === 'open_url').map((c) => c.args?.url), [
    'https://github.com/o/alpha/blob/abc123/docs/target-app-parity.md',
    'https://github.com/o/gamma/blob/abc123/ROADMAP.md?plain=1#L9',
  ]);
  assert.equal(h.view(), 'repos', 'a click on the number does not open the urgent view');
});

// ───────────────────────────── parity tab ─────────────────────────────

/** Two apps with parity documents (alpha, gamma), one without (beta), and one not accessible (gone). */
function paritySnapshot(): Snapshot {
  const daysAgo = (n: number) => new Date(Date.now() - n * 86400e3).toISOString().slice(0, 10);
  const hours = (lo: number, hi: number) => ({ state: 'ok' as const, v: { lo, hi, approx: false, basis: null, note: null, raw: `${lo}–${hi} h`, counted_in: null }, src: { path: 'docs/gaps.md', line: 6 } });
  const alpha = report({
    reviewed: daysAgo(30),
    stage: { state: 'ok', v: { name: 'alpha', steps: [] }, src: { path: 'ROADMAP.md', line: 3 } },
    heat: [{ column: 'features', value: 70, text: '70%', rows: ['Features: 70%'], src: { path: 'ROADMAP.md', line: 20 } }],
    files: [
      { path: 'ROADMAP.md', oid: 'r1', reviewed: daysAgo(30), updated: daysAgo(30), generated: null, target: 'Some App 7', date_lines: 1, line: 2, why: null, commit_date: '2026-10-17T09:00:00Z', commit: 'c1' },
      { path: 'docs/gaps.md', oid: 'g1', reviewed: daysAgo(2), updated: null, generated: null, target: null, date_lines: 1, line: 2, why: null, commit_date: null, commit: null },
    ],
    gaps: [
      parityGap('G1', { rank: 1, beta: 'yes', title: 'Color management', hours: hours(40, 80), kinds: ['feature'], detail_src: { path: 'docs/gaps.md', line: 30 } }),
      parityGap('G2', { rank: 2, alpha: 'yes', title: 'Crash on save', kinds: ['stability'] }),
      parityGap('G3', { group: 'Later', title: 'Plugin store' }),
    ],
    gap_totals: { beta: { count: 1, lo: 40, hi: 80, unknown: 0 }, partial: null, alpha: 1, alpha_none: false, stated: noValue, ids_stable: true, form: 'table', beta_unknown: false },
    areas: [{
      name: 'Text', full_name: 'Text and type', key: 'text', weight: 2, weight_src: null, breadth: noValue,
      ready: { state: 'ok', v: { kind: 'pct', value: 40, approx: false, low: null, high: null, basis: 'estimated', raw: '40%', note: null, computed: false }, src: { path: 'ROADMAP.md', line: 40 } },
      ready_from: null, hours: noValue, missing: [{ text: 'vertical text', hover: null }], more: 0, chips_from: 'marker',
      raw_gaps: 'Missing: vertical text', gaps_header: 'Gaps', unweighted: false, dup_lines: [], conflict: null, kind: '', doc: null,
      counts: null, src: { path: 'ROADMAP.md', line: 40 }, shortfall: 3,
    }],
    notes: [{ level: 'warn', text: 'two date lines', src: { path: 'ROADMAP.md', line: 2 } }],
  });
  const gamma = report({
    ready: okFigure(70, 'Ready for real work'),
    reviewed: daysAgo(1),
    gaps: [parityGap('H1', { rank: 1, beta: 'yes', title: 'Tables', kinds: ['feature'], hours: hours(10, 20) })],
    gap_totals: { beta: { count: 1, lo: 10, hi: 20, unknown: 0 }, partial: null, alpha: 0, alpha_none: true, stated: noValue, ids_stable: true, form: 'table', beta_unknown: false },
  });
  const none = 'No ROADMAP.md or docs/target-app-parity.md at HEAD';
  return {
    ...snapshot([
      repo('alpha', {}, { parity: alpha }),
      repo('beta', {}, { parity: report({ na: none, roadmap_oid: null, parity_doc: false, breadth: { state: 'absent', why: none }, ready: { state: 'absent', why: none } }) }),
      repo('gamma', {}, { parity: gamma }),
      repo('gone', null, { inaccessible: true }),
    ]),
    parity_warn_days: 14,
  };
}

const parityRows = (h: Harness) => [...h.doc.querySelectorAll<HTMLElement>('#parity-rows tr')].map((r) => r.dataset.repo?.slice(2));
const parityCell = (h: Harness, name: string, label: string) => {
  const i = [...h.doc.querySelectorAll('#parity-table thead tr.cols th')].findIndex((th) => th.textContent === label);
  return h.doc.querySelector<HTMLTableRowElement>(`#parity-rows tr[data-repo="o/${name}"]`)!.cells[i]!;
};
const press = (h: Harness, key: string, mod = false) =>
  h.doc.dispatchEvent(new h.win.KeyboardEvent('keydown', { key, metaKey: mod, bubbles: true }) as unknown as Event);

test('the Parity tab lists each app’s numbers, opens with ⌘3, sorts, and remembers', async () => {
  const h = await setup({ snap: paritySnapshot() });
  assert.equal(h.doc.getElementById('parity-count')!.textContent, '3', 'G1, G2 and H1 block alpha or beta');
  press(h, '3', true);
  assert.equal(h.doc.getElementById('parity-view')!.hidden, false);
  assert.equal(h.doc.getElementById('repos-view')!.hidden, true);
  assert.equal(stored(h, 'view'), 'parity');
  assert.deepEqual(parityRows(h), ['gamma', 'alpha', 'beta', 'gone'], 'by Ready, highest first; apps without numbers last');
  assert.equal(parityCell(h, 'alpha', 'Ready').textContent, '55%');
  assert.ok(parityCell(h, 'alpha', 'Ready').querySelector('.ready-bar .tick'), 'the tick marks Breadth');
  assert.equal(parityCell(h, 'alpha', 'Breadth').textContent, '80%');
  assert.equal(parityCell(h, 'alpha', 'Stage').textContent, 'alpha');
  assert.equal(parityCell(h, 'alpha', 'Beta gaps').textContent, '1 · 40–80 h');
  assert.equal(parityCell(h, 'alpha', 'Features').textContent, '70%');
  assert.equal(parityCell(h, 'alpha', 'Reviewed').textContent, '30d');
  assert.ok(parityCell(h, 'alpha', 'Reviewed').querySelector('.old'), 'older than parity_warn_days');
  assert.ok(!parityCell(h, 'gamma', 'Reviewed').querySelector('.old'));
  assert.match(parityCell(h, 'alpha', 'Changed').querySelector<HTMLElement>('span')!.title, /^Last commit to a parity document: /);
  assert.equal(parityCell(h, 'beta', 'App').querySelector('.na')!.textContent, 'n/a');
  assert.equal(parityCell(h, 'gone', 'Ready').textContent, '—');
  assert.equal(h.doc.querySelector('#parity-table tfoot td')!.textContent, '2 of 4 apps');

  const header = [...h.doc.querySelectorAll<HTMLElement>('#parity-table thead tr.cols th')].find((th) => th.textContent === 'App')!;
  header.click();
  assert.deepEqual(parityRows(h), ['alpha', 'gamma', 'beta', 'gone']);
  assert.deepEqual(stored(h, 'paritySort'), { key: 'app', dir: 'asc' });
  parityCell(h, 'alpha', 'Breadth').querySelector<HTMLElement>('.link')!.click();
  assert.equal(h.calls.at(-1)?.args?.url, 'https://github.com/o/alpha/blob/c1/ROADMAP.md?plain=1#L12');
  assert.equal(h.doc.getElementById('parity-detail')!.hidden, true, 'a click on a number does not open the detail pane');

  // The audience dropdown survives a snapshot.
  const picker = h.doc.querySelector<HTMLSelectElement>('#parity-table .window-picker')!;
  h.emit('craft:snapshot', paritySnapshot());
  assert.equal(h.doc.querySelector('#parity-table .window-picker'), picker);
});

test('a click on an app opens its detail pane; the arrows move it and Esc closes it before hiding the window', async () => {
  const h = await setup({ snap: paritySnapshot(), storage: { view: 'parity', paritySort: { key: 'app', dir: 'asc' } } });
  const pane = h.doc.getElementById('parity-detail')!;
  assert.equal(pane.hidden, true);
  h.doc.querySelector<HTMLElement>('#parity-rows tr[data-repo="o/alpha"]')!.click();
  assert.equal(pane.hidden, false);
  assert.equal(pane.querySelector('h2')!.textContent, 'alpha');
  assert.match(pane.querySelector('.detail-head')!.textContent ?? '', /vs Some App 7/);
  const numbers = [...pane.querySelectorAll('.number')].map((n) => n.textContent);
  assert.ok(numbers.includes('Breadth80%Estimated'), numbers.join(' | '));
  const gapTitles = () => [...pane.querySelectorAll<HTMLTableRowElement>('.detail-table.gaps tbody tr')].map((r) => r.cells[2]!.textContent);
  assert.deepEqual(gapTitles(), ['Color management', 'Crash on save', 'Plugin store']);
  assert.match(pane.querySelector('.gap-totals')!.textContent ?? '', /Σ beta gaps 40–80 h \(1\)/);
  assert.match(pane.querySelector('.notes')!.textContent ?? '', /two date lines/);
  const title = [...pane.querySelectorAll<HTMLElement>('.detail-table.gaps .link')].find((l) => l.textContent === 'Color management')!;
  title.click();
  assert.equal(h.calls.at(-1)?.args?.url, 'https://github.com/o/alpha/blob/c1/docs/gaps.md?plain=1#L30', 'the gap’s own heading');

  const beta = h.doc.querySelector<HTMLInputElement>('#detail-beta')!;
  beta.checked = true;
  beta.dispatchEvent(new h.win.Event('change') as unknown as Event);
  assert.deepEqual(gapTitles(), ['Color management']);

  press(h, 'ArrowDown');
  assert.equal(pane.querySelector('h2')!.textContent, 'gamma');
  press(h, 'ArrowUp');
  press(h, 'ArrowUp');
  assert.equal(pane.querySelector('h2')!.textContent, 'alpha', 'the first row stays selected');

  press(h, 'Escape');
  assert.equal(pane.hidden, true);
  assert.ok(!h.calls.some((c) => c.cmd === 'hide_window'), 'Esc closed the pane only');
  press(h, 'Enter');
  assert.equal(pane.hidden, false, 'Enter opens the selected app');
  press(h, 'Escape');
  press(h, 'Escape');
  assert.ok(h.calls.some((c) => c.cmd === 'hide_window'));
});

test('the detail pane keeps an expanded section through a snapshot, and explains apps without numbers', async () => {
  const h = await setup({ snap: paritySnapshot(), storage: { view: 'parity' } });
  h.doc.querySelector<HTMLElement>('#parity-rows tr[data-repo="o/alpha"]')!.click();
  const pane = h.doc.getElementById('parity-detail')!;
  const area = () => pane.querySelector<HTMLDetailsElement>('details[data-key="area/text"]')!;
  assert.equal(area().querySelector('.chip')!.textContent, 'vertical text');
  area().setAttribute('open', '');
  h.emit('craft:snapshot', paritySnapshot());
  assert.ok(area().hasAttribute('open'), 'a new snapshot draws the pane again with the section open');

  h.doc.querySelector<HTMLElement>('#parity-rows tr[data-repo="o/beta"]')!.click();
  assert.equal(pane.querySelector('p.muted')!.textContent, 'No ROADMAP.md or docs/target-app-parity.md at HEAD');
  h.doc.querySelector<HTMLElement>('#parity-rows tr[data-repo="o/gone"]')!.click();
  assert.equal(pane.querySelector('p.muted')!.textContent, 'Not accessible (private, or not found).');
});

test('the Apps cells and the detail pane title each number with where it was read', async () => {
  type ParityDim = ParityReport['dims'][number];
  const hrs = (lo: number, hi: number) => ({ lo, hi, approx: false, basis: null, note: null, raw: `${lo}–${hi} h`, counted_in: null });
  const src = (line: number) => ({ path: 'ROADMAP.md', line });
  const pct = (value: number) => ({
    state: 'ok' as const, v: { kind: 'pct' as const, value, approx: false, low: null, high: null, basis: 'estimated' as const, raw: `${value}%`, note: null, computed: false }, src: src(30),
  });
  const dim = (name: string, d: Partial<ParityDim> = {}): ParityDim => ({
    name, group: 'features', weight: 2, weight_src: null, ready: pct(50), breadth: noValue, hours: noValue, kind: '', doc: null,
    counted: null, child: false, fills: 'ready', only_in: null, src: src(20), shortfall: null, ...d,
  });
  const alpha = report({
    stage: {
      state: 'ok', v: { name: 'alpha', steps: [{ stage: 'beta', points: 12, hours: hrs(40, 60), text: '' }, { stage: 'full', points: null, hours: null, text: '' }] },
      src: src(3), from: 'Stage row',
    },
    files: [
      { path: 'ROADMAP.md', oid: 'r1', reviewed: '2026-10-01', updated: '2026-10-02', generated: null, target: null, date_lines: 2, line: 2, why: null, commit_date: null, commit: null },
      { path: 'docs/gaps.md', oid: 'g1', reviewed: null, updated: null, generated: null, target: null, date_lines: 0, line: null, why: 'No date line', commit_date: null, commit: null },
    ],
    dims: [dim('Text', { shortfall: 4, only_in: 'w' }), dim('Vertical text', { child: true })],
    areas: [{
      name: 'Text', full_name: 'Text and type', key: 'text', weight: null, weight_src: null, breadth: noValue, ready: pct(40), ready_from: null,
      hours: { state: 'ok', v: hrs(10, 20), src: src(40) }, missing: [], more: 0, chips_from: 'none', raw_gaps: '', gaps_header: null,
      unweighted: false, dup_lines: [], conflict: null, kind: '', doc: null, counts: null, src: src(40), shortfall: null,
    }],
    area_weights_why: 'No Weight column',
    checklists: [{
      kind: 'tiered', path: 'parity/features.toml', overall: 50, more: 2,
      tiers: [{ name: 'P0', done: 5, ahead: 1, partial: 2, missing: 3, excluded: 0, not_measured: 0, unrecognized: 4, pct: 50 }],
      rows: [{ status: 'missing', group: 'P0', text: 'Row', notes: '', line: 4 }],
    }],
    gaps: [parityGap('G1', { rank: 1 })],
    gap_totals: {
      beta: null, partial: { count: 2, lo: 0, hi: 30, unknown: 1 }, alpha: 0, alpha_none: true, stated: { state: 'ok', v: hrs(100, 200), src: src(5) },
      ids_stable: true, form: 'table', beta_unknown: false,
    },
    to_beta: { state: 'ok', v: hrs(300, 400), src: src(6) },
  });
  const none = 'No ROADMAP.md or docs/target-app-parity.md at HEAD';
  const h = await setup({ snap: snapshot([
    repo('alpha', {}, { parity: alpha }),
    repo('beta', {}, { parity: report({ na: none }) }),
    repo('delta', null, { fetched_at: null }),
    repo('gone', null, { inaccessible: true }),
  ]), storage: { view: 'parity' } });
  const appTitle = (name: string) => parityCell(h, name, 'App').querySelector<HTMLElement>('.repo')!.title;
  assert.deepEqual(['alpha', 'beta', 'delta', 'gone'].map(appTitle), ['Read at c1', none, 'Not read yet', 'Not accessible']);
  assert.equal(parityCell(h, 'alpha', 'Stage').querySelector<HTMLElement>('.stage')!.title, 'next: beta, 12 points, 40–60 h\nnext: full\nFrom Stage row');
  const betaGaps = parityCell(h, 'alpha', 'Beta gaps');
  assert.equal(betaGaps.textContent, '0');
  assert.equal(betaGaps.querySelector<HTMLElement>('span')!.title, 'Open gaps that block beta\n+ part of 2 gaps (up to 30 h, 1 unestimated)');

  h.doc.querySelector<HTMLElement>('#parity-rows tr[data-repo="o/alpha"]')!.click();
  const pane = h.doc.getElementById('parity-detail')!;
  const chips = [...pane.querySelectorAll<HTMLElement>('.fresh-chip')];
  assert.deepEqual(chips.map((c) => c.textContent), ['ROADMAP.md reviewed 2026-10-01 · updated 2026-10-02', 'gaps.md No date line']);
  assert.deepEqual(chips.map((c) => c.title), ['ROADMAP.md\n2 date lines\nLast commit not read', 'docs/gaps.md\nLast commit not read']);
  const dimRows = [...pane.querySelector('table.detail-table')!.querySelectorAll<HTMLTableRowElement>('tbody tr')];
  assert.deepEqual(dimRows.map((r) => r.cells[0]!.title), ['Text\nOnly in the weight table', 'Vertical text\nPart of the row above; left out of the sums']);
  assert.ok([...pane.querySelectorAll('h4')].some((x) => x.textContent === 'Areas (sorted by hours: No Weight column)'));
  assert.equal(pane.querySelector<HTMLElement>('.tier')!.title, '5 done, 1 ahead, 2 partial, 3 missing, 0 out of scope, 4 unrecognized');
  assert.equal(pane.querySelector('details[data-key="rows/parity/features.toml"] summary')!.textContent, 'Partial and missing rows (1, +2 more)');
  assert.equal(pane.querySelector('.gap-totals')!.textContent,
    'No beta gaps + part of 2 gaps (up to 30 h, 1 unestimated); docs/gaps.md says 100–200 h; ROADMAP says 300–400 h to beta');
});

test('All gaps lists every app’s open gaps by blocking band, filters them, and sorts by a column', async () => {
  const h = await setup({ snap: paritySnapshot(), storage: { view: 'parity' } });
  h.doc.querySelector<HTMLElement>('#parity-view .seg[data-pview="gaps"]')!.click();
  assert.equal(stored(h, 'parityView'), 'gaps');
  const ids = () => [...h.doc.querySelectorAll('#gaps-table tbody tr')].map((r) => r.querySelectorAll('td')[2]!.textContent);
  assert.deepEqual(ids(), ['G2', 'G1', 'H1', 'G3'], 'the alpha blocker, the beta blockers by rank, then the rest');
  assert.match(h.doc.getElementById('kind-sums')!.textContent ?? '', /^4 gaps · 50–100 h \+ 2 unestimated/);
  const row = (id: string) => [...h.doc.querySelectorAll<HTMLTableRowElement>('#gaps-table tbody tr')].find((r) => r.cells[2]!.textContent === id)!;
  assert.equal(row('G2').cells[4]!.textContent, 'A');
  assert.equal(row('G1').cells[8]!.textContent, '2d', 'the age of docs/gaps.md’s review');

  const chip = [...h.doc.querySelectorAll<HTMLElement>('.kind-chip')].find((c) => c.textContent?.startsWith('feature:'))!;
  chip.click();
  assert.deepEqual(ids(), ['G1', 'H1']);
  assert.equal(stored(h, 'gapFilter') && (stored(h, 'gapFilter') as { kind: string }).kind, 'feature');
  const text = h.doc.querySelector<HTMLInputElement>('#gap-text')!;
  text.value = 'table';
  text.dispatchEvent(new h.win.Event('input') as unknown as Event);
  assert.deepEqual(ids(), ['H1']);
  assert.equal(h.doc.querySelector('#gap-text'), text, 'the filter keeps its box while typing');
  text.value = '';
  text.dispatchEvent(new h.win.Event('input') as unknown as Event);

  const hoursHeader = [...h.doc.querySelectorAll<HTMLElement>('#gaps-table thead th')].find((th) => th.textContent === 'Hours')!;
  hoursHeader.click();
  assert.deepEqual(ids(), ['G1', 'H1'], 'most hours first');
  assert.equal(h.doc.getElementById('parity-detail')!.hidden, true, 'All gaps has no detail pane');
  press(h, 'Escape');
  assert.ok(h.calls.some((c) => c.cmd === 'hide_window'), 'Esc in All gaps hides the window');
});

test('All gaps drops a saved app filter for a repo no longer configured', async () => {
  const h = await setup({ snap: paritySnapshot(), storage: { view: 'parity', parityView: 'gaps', gapFilter: { repo: 'o/removed' } } });
  assert.equal(h.doc.querySelectorAll('#gaps-table tbody tr').length, 4);
  assert.equal(h.doc.querySelector<HTMLSelectElement>('#gap-repo')!.value, '');
  assert.equal((stored(h, 'gapFilter') as { repo: string }).repo, '');
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
