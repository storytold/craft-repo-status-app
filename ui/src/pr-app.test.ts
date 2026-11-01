// The PR tab end to end in happy-dom: index.html's markup, the real `start` opened
// on this tab, and a fake host API standing in for Tauri.

import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { Window } from 'happy-dom';
import { start } from './app.ts';
import type { Api } from './app.ts';
import { REPO_COLUMNS } from './pr-model.ts';
import type { Overview, PrFull, PrRow, RelatedRef, RepoRow, RepoView, Status } from './pr-model.ts';

const HTML = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
const BODY = HTML.match(/<body>([\s\S]*)<\/body>/)![1]!.replace(/<script[\s\S]*?<\/script>/g, '');
const NOW = new Date().toISOString();

const row = (o: Partial<RepoRow>): RepoRow => ({
  repo: 'o/alpha', open: 3, listed: true, inspected: 2, analysed: 0, approved: 0, landable: 1, promising: 0, reviewable: 1, ci_not_run: 0, checking: 0, not_inspected: 0, high_quality: 1, easy: 1, medium: 1, hard: 1,
  needs_work: 1, conflicted: 0, draft: 0, conflicts: 0, ci_failing: 0, rabbit_flagged: 1, rabbit_nits: 0, security: 0, security_nits: 0,
  core: 1, frequent: 0, new: 0, bots: 0, interesting: 0, groups: 1, grouped_prs: 2, list_at: NOW, inaccessible: false, ...o,
});

const pr = (o: Partial<PrRow>): PrRow => ({
  repo: 'o/alpha', number: 1, title: 'Change', url: 'https://github.com/o/alpha/pull/1', author: 'ann', avatar: '', tier: 'core',
  landed_total: 40, approved_by: [], approved: false, approval_stale: false, conflicted: false, created_at: NOW, updated_at: NOW, is_draft: false, additions: 10, deletions: 2, changed_files: 1, labels: [],
  mergeable: 'MERGEABLE', comments_total: 0, reactions: 0, state: 'landable', ease: 'easy', quality: 85, high_quality: true,
  inspected: true, ci: 'success', rabbit_flags: 0, rabbit_nits: 0, security_flags: 0, security_nits: 0, sensitive: 0, interesting: false,
  summary: '', analysed: false, related: [], focus: 8000, ...o,
});

const STATUS: Status = {
  token_rejected: false, points_last_hour: 16, max_points_per_hour: 2500, backlog: 12, histories_pending: 0, poll_secs: 1800, stale_after_secs: 5400,
  claude: { enabled: true, available: true, model: 'm', spent_today: 0.5, max_usd_per_day: 5, runs_last_hour: 1, max_runs_per_hour: 12 },
};

const XSS = '<img src=x onerror="window.pwned=1"> Fix';

const FULL: PrFull = {
  repo: 'o/alpha', number: 1, title: XSS, url: 'https://github.com/o/alpha/pull/1',
  author: { login: 'ann', tier: 'core', permission: 'WRITE', listed: true, landed_here: 12, landed_total: 40, counts_complete: true },
  avatar: '', association: 'CONTRIBUTOR', approved_by: ['bflatastic', 'aiongg'], created_at: NOW, updated_at: NOW, is_draft: false, additions: 10, deletions: 2, changed_files: 1,
  head_ref: 'fix', base_ref: 'main', cross_repo: true, labels: [], mergeable: 'MERGEABLE', merge_state: 'CLEAN', comments_total: 0, reactions: 0,
  assessment: {
    state: 'needs_work', state_reasons: ['1 unresolved CodeRabbit flag(s)'], ease: 'medium', ease_score: 3, ease_reasons: [{ text: 'Small change: 12 lines', points: 1 }],
    ease_raised_by_claude: false, quality: 60, quality_reasons: [{ text: 'Baseline', points: 60 }, { text: '1 major finding(s)', points: -10 }], high_quality: false, conflicted: true, approved: true, approval_stale: false, changes_requested: false,
    inspected: true, ci: 'success', rabbit_flags: 1, rabbit_nits: 1, security_flags: 1, security_nits: 0, sensitive_paths: ['.github/workflows/ci.yml'],
    security_reasons: ['1 CodeRabbit security finding(s)'], deps_changed: false, tests_touched: true, interesting: true,
  },
  detail: {
    body: '<script>window.pwned=2</script>Why this change', files: [{ path: 'src/a.rs', additions: 10, deletions: 2, change: 'MODIFIED' }], files_total: 1,
    checks: [{ name: 'test', state: 'SUCCESS' }, { name: 'lint', state: 'FAILURE', url: 'https://github.com/o/alpha/runs/1' }], rabbit: 'reviewed',
    findings: [
      { severity: 'minor', category: 'Style', effort: 'Quick win', title: 'Rename it', snippet: '', path: 'src/a.rs', line: 3, url: '', resolved: false, outdated: false, security: false },
      { severity: 'major', category: 'Security & Privacy', effort: '', title: 'Validate the path', snippet: 'It escapes.', path: 'src/a.rs', line: 9, url: 'https://github.com/o/alpha/pull/1#r1', resolved: false, outdated: false, security: true },
      { severity: 'critical', category: 'X', effort: '', title: 'Already fixed', snippet: '', path: 'src/a.rs', url: '', resolved: true, outdated: false, security: false },
    ],
    threads_total: 3, human_threads_open: 0, reviews: [], comments: [], linked_issues: [],
  },
  detail_current: true,
  analysis: { model: 'm', at: NOW, deep: false, diff_truncated: false, summary: 'Stops a crash.', ease: 'medium', interesting: true, why_interesting: 'Touches core', review_focus: 'The parser', risk: '', security_note: '' },
  analysis_current: true,
  groups: [{ id: 'c0', title: 'Tab fixes', relation: 'overlap', note: 'Land one first', numbers: [1, 2], source: 'claude' }],
  related_refs: [{ number: 2, relation: 'overlap', group: 'Tab fixes', note: 'Land one first', source: 'claude' }],
  related: [pr({ number: 2, title: 'Other tab fix', state: 'checking' })],
};

const rel = (number: number): RelatedRef => ({ number, relation: 'overlap', group: 'Tab fixes', note: 'Land one first', source: 'claude' });

const REPO: RepoView = {
  row: row({}),
  prs: [
    pr({ number: 1, title: XSS, focus: 8100, quality: 85, related: [rel(2)], summary: 'Stops a crash.' }),
    pr({ number: 2, title: 'Blocked one', state: 'needs_work', focus: 1100, quality: 40, high_quality: false, rabbit_flags: 2, ease: 'hard', related: [rel(1)] }),
    pr({ number: 3, title: 'A draft', state: 'draft', is_draft: true, focus: 0, quality: 50 }),
  ],
  groups: [{ id: 'c0', title: 'Tab fixes', relation: 'overlap', note: 'Land one first', numbers: [1, 2], source: 'claude' }],
};

interface Harness {
  win: Window;
  doc: Document;
  calls: { cmd: string; args?: Record<string, unknown> }[];
  emit(event: string): void;
  $(sel: string): HTMLElement | null;
  $$(sel: string): HTMLElement[];
  key(key: string, opts?: Record<string, unknown>): void;
}

let teardown: (() => Promise<void>) | null = null;
afterEach(async () => { await teardown?.(); teardown = null; });

const tick = (ms = 0) => new Promise((r) => setTimeout(r, ms));

async function setup(overview?: Partial<Overview>): Promise<Harness> {
  const win = new Window({ url: 'http://localhost/index.html' });
  const doc = win.document as unknown as Document;
  doc.body.innerHTML = BODY;
  const g = globalThis as Record<string, unknown>;
  const globals = { window: win, document: doc, localStorage: win.localStorage, location: win.location, navigator: win.navigator };
  const saved = Object.fromEntries(Object.keys(globals).map((k) => [k, Object.getOwnPropertyDescriptor(g, k)]));
  for (const [k, v] of Object.entries(globals)) Object.defineProperty(g, k, { value: v, configurable: true, writable: true });
  const calls: Harness['calls'] = [];
  const listeners = new Map<string, () => void>();
  const api: Api = {
    async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
      calls.push({ cmd, args });
      const answers: Record<string, unknown> = {
        get_meta: { shortcut: 'CmdOrCtrl+Shift+Y', high_quality: 75 },
        get_pr_overview: { repos: [row({}), row({ repo: 'o/beta', open: 9, landable: 0, reviewable: 4, listed: true }), row({ repo: 'o/gamma', listed: false, open: 0, open_hint: 77 })], status: STATUS, ...overview },
        get_pr_repo: args?.repo === 'o/alpha' ? REPO : null,
        get_pr_queue: [pr({ repo: 'o/beta', number: 9, title: 'Queued', related: [rel(1)] })],
        get_pr: args?.number === 1 ? FULL : null,
      };
      return answers[cmd] as T;
    },
    listen(event, fn) { listeners.set(event, fn as () => void); },
    startResize() {},
  };
  const stop = start(api, { view: 'apps' });
  await tick();
  await tick();
  teardown = async () => {
    stop();
    await win.happyDOM.close();
    for (const [k, d] of Object.entries(saved)) {
      if (d) Object.defineProperty(g, k, d); else delete g[k];
    }
  };
  return {
    win, doc, calls,
    emit: (e) => listeners.get(e)?.(),
    $: (s) => doc.querySelector(s),
    $$: (s) => [...doc.querySelectorAll<HTMLElement>(s)],
    key: (key, opts = {}) => { doc.dispatchEvent(new win.KeyboardEvent('keydown', { key, bubbles: true, ...opts }) as unknown as Event); },
  };
}

const appRows = (h: Harness) => h.$$('#apps-table tbody tr').map((t) => t.dataset.repo);
const prRows = (h: Harness) => h.$$('.pr-table tbody tr').map((t) => Number(t.dataset.number));

test('the root table says what each repo is', async () => {
  const h = await setup({ repos: [
    row({ repo: 'o/fonts', category: 'assets', listed: true }),
    row({ repo: 'o/new', category: 'ci', listed: false }),
    row({ repo: 'o/mine', category: 'other', listed: true }),
  ] });
  const i = REPO_COLUMNS.findIndex((c) => c.key === 'category');
  const type = (r: string) => (h.$(`tr[data-repo="${r}"]`) as HTMLTableRowElement).cells[i]!.textContent;
  assert.equal(type('o/fonts'), 'Assets');
  assert.equal(type('o/new'), 'CI');
  assert.equal(type('o/mine'), '—');
});

test('the root lists each app, sortable, with totals', async () => {
  const h = await setup();
  assert.deepEqual(appRows(h), ['o/alpha', 'o/beta', 'o/gamma'], 'sorted by landable');
  (h.$('th[data-key="open"]') as HTMLElement).click();
  assert.deepEqual(appRows(h), ['o/gamma', 'o/beta', 'o/alpha']);
  (h.$('th[data-key="open"]') as HTMLElement).click();
  assert.deepEqual(appRows(h), ['o/alpha', 'o/beta', 'o/gamma']);
  assert.match(h.$('#apps-table tbody tr[data-repo="o/gamma"]')!.textContent!, /~77/, 'unlisted: the Downloads tab\'s count');
  assert.match(h.$('#apps-table tfoot')!.textContent!, /All repos/);
  assert.equal(h.$('#apps-count')!.textContent, '89');
  assert.equal(h.$('#queue-count')!.textContent, '8');
  assert.equal(h.$('#prs-count')!.textContent, '8', 'the tab counts what someone can act on');
  assert.equal(h.$('#pr-pill')!.textContent, 'FRESH');
  assert.match(h.$('#claude')!.textContent!, /\$0\.50 of \$5\.00/);
  assert.match(h.$('#backlog')!.textContent!, /12 PRs left to inspect/);
});

test('drilling down: app, then PR, and Esc walks back up', async () => {
  const h = await setup();
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  assert.deepEqual(prRows(h), [1, 2], 'drafts hidden, best first');
  assert.match(h.$('#crumbs')!.textContent!, /alpha/);
  (h.$('tr[data-number="1"]') as HTMLElement).click();
  await tick();
  assert.ok(h.$('#verdict'));
  assert.match(h.$('#crumbs')!.textContent!, /#1/);

  h.key('Escape');
  await tick();
  assert.deepEqual(prRows(h), [1, 2]);
  h.key('Escape');
  await tick();
  assert.equal(appRows(h).length, 3);
  assert.equal(h.calls.some((c) => c.cmd === 'hide_window'), false);
  h.key('Escape');
  assert.equal(h.calls.some((c) => c.cmd === 'hide_window'), true, 'at the root it hides');
});

test('PR tables filter, search, sort and group', async () => {
  const h = await setup();
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  (h.$('[data-filter="rabbit"]') as HTMLElement).click();
  assert.deepEqual(prRows(h), [2]);
  (h.$('[data-filter="all"]') as HTMLElement).click();
  const search = h.$('input[type="search"]') as HTMLInputElement;
  search.value = 'Blocked one';
  search.dispatchEvent(new h.win.Event('input') as unknown as Event);
  assert.deepEqual(prRows(h), [2]);
  search.value = '';
  search.dispatchEvent(new h.win.Event('input') as unknown as Event);
  (h.$('th[data-key="quality"]') as HTMLElement).click();
  assert.deepEqual(prRows(h), [1, 2]);
  (h.$('th[data-key="quality"]') as HTMLElement).click();
  assert.deepEqual(prRows(h), [2, 1]);

  (h.$('#draft-toggle') as HTMLInputElement).click();
  assert.equal(prRows(h).includes(3), true);
  (h.$('#draft-toggle') as HTMLInputElement).click();
  (h.$('#group-toggle') as HTMLInputElement).click();
  assert.match(h.$('.group-head')!.textContent!, /Tab fixes/);
  assert.match(h.$('.group-head')!.textContent!, /Land one first/);
});

test('approval is a flag beside the status, naming who', async () => {
  const h = await setup();
  const row2 = REPO.prs[1]!;
  Object.assign(row2, { approved: true, conflicted: true, approved_by: ['bflatastic', 'aiongg'] });
  try {
    (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
    await tick();
    const row = h.$('tr[data-number="2"]')!;
    assert.equal(row.querySelector('td .state')!.textContent, 'Needs work');
    assert.ok(row.querySelector('.flag.approved'));
    assert.ok(row.querySelector('.flag.conflicted'));
    assert.match(row.textContent!, /bflatastic, aiongg/);
    assert.equal(h.$('tr[data-number="1"] td .state')!.textContent, 'Landable');
  } finally {
    Object.assign(row2, { approved: false, conflicted: false, approved_by: [] });
  }
});

test('related numbers jump to the row, clearing what hides it, and link back', async () => {
  const h = await setup();
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  const toTwo = h.$('tr[data-number="1"] .rel') as HTMLElement;
  assert.equal(toTwo.textContent, '#2');
  toTwo.click();
  assert.ok(h.$('tr[data-number="2"]')!.classList.contains('jumped'));
  assert.ok(h.$('#prs-view .pr-table'), 'stays on the table');

  (h.$('[data-filter="landable"]') as HTMLElement).click();
  assert.deepEqual(prRows(h), [1], 'the target is filtered out');
  (h.$('tr[data-number="1"] .rel') as HTMLElement).click();
  assert.ok(h.$('tr[data-number="2"]')!.classList.contains('jumped'), 'the filter is cleared');
  assert.equal((h.$('tr[data-number="2"] .rel') as HTMLElement).textContent, '#1', 'the link goes both ways');

  (h.$('tr[data-number="2"] .rel') as HTMLElement).dispatchEvent(new h.win.MouseEvent('click', { bubbles: true, metaKey: true }) as unknown as Event);
  await tick();
  assert.ok(h.$('#verdict'), '⌘-click opens it');
});

test('an approval GitHub does not count is shown, but not in green', async () => {
  const h = await setup();
  const row1 = REPO.prs[0]!;
  row1.approved_by = ['outsider'];
  try {
    (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
    await tick();
    const row = h.$('tr[data-number="1"]')!;
    assert.match(row.textContent!, /outsider/);
    assert.equal(row.querySelector('.n-good.approver'), null);
    assert.equal(row.querySelector('.flag.approved'), null);
    assert.equal([...row.querySelectorAll('.muted')].some((n) => n.textContent === 'outsider'), true);
  } finally {
    row1.approved_by = [];
  }
});

test('a related PR outside the queue opens instead', async () => {
  const h = await setup();
  (h.$('[data-pr-view="queue"]') as HTMLElement).click();
  await tick();
  (h.$('.rel') as HTMLElement).click();
  await tick();
  assert.ok(h.$('#verdict'));
});

test('the help button explains every status, and Esc closes it first', async () => {
  const h = await setup();
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  (h.$('#help') as HTMLElement).click();
  const panel = h.$('#help-panel')!;
  for (const word of ['Landable', 'Promising', 'Reviewable', 'CI not run', 'Checking', 'Not inspected', 'Conflicted', 'Needs work', 'Draft', 'Stale approval', 'Possibly related', 'Same issue']) {
    assert.match(panel.textContent!, new RegExp(word));
  }
  h.key('Escape');
  assert.equal(h.$('#help-panel'), null);
  assert.deepEqual(prRows(h), [1, 2], 'still on the table');
  (h.$('#help') as HTMLElement).click();
  (h.$('#help-close') as HTMLElement).click();
  assert.equal(h.$('#help-panel'), null);
});

test('the review queue spans apps and opens PRs', async () => {
  const h = await setup();
  (h.$('[data-pr-view="queue"]') as HTMLElement).click();
  await tick();
  assert.deepEqual(prRows(h), [9]);
  assert.match(h.$('.pr-table tbody')!.textContent!, /beta/);
  assert.equal(h.$('[data-pr-view="queue"]')!.getAttribute('aria-selected'), 'true');
});

test('one PR: verdict, Claude, findings worst first, author, overlaps, checks', async () => {
  const h = await setup();
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  (h.$('tr[data-number="1"]') as HTMLElement).click();
  await tick();
  const text = (s: string) => h.$(s)!.textContent!;
  assert.match(text('#approved-by'), /Approved by bflatastic, aiongg/);
  assert.match(text('#verdict'), /Needs work/);
  assert.match(text('#verdict'), /1 unresolved CodeRabbit flag/);
  assert.match(text('#verdict'), /-10/);
  assert.match(text('#claude-card'), /Stops a crash/);
  assert.match(text('#claude-card'), /Touches core/);
  const titles = h.$$('#findings .finding').map((f) => f.textContent!);
  assert.equal(titles.length, 2, 'resolved findings are counted, not listed');
  assert.match(titles[0]!, /Validate the path/);
  assert.match(titles[1]!, /Rename it/);
  assert.match(text('#findings h2'), /1 resolved or outdated/);
  assert.match(text('#security'), /\.github\/workflows\/ci\.yml/);
  assert.match(text('#author'), /40/);
  assert.match(text('#author'), /write/);
  assert.match(text('#overlaps'), /Other tab fix/);
  assert.match(text('#checks'), /1 passed, 1 failed/);
  assert.match(text('#files'), /has tests/);
});

test('PR text is shown as text, never markup', async () => {
  const h = await setup();
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  assert.equal(h.$('.pr-table img'), null);
  assert.match(h.$('.pr-table')!.textContent!, /<img src=x/);
  (h.$('tr[data-number="1"]') as HTMLElement).click();
  await tick();
  assert.equal(h.$('.pr-head h1 img'), null);
  assert.equal(h.$('.body script'), null);
  assert.match(h.$('.body')!.textContent!, /<script>/);
});

test('links only open through the host, and asking Claude queues it', async () => {
  const h = await setup();
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  (h.$('tr[data-number="1"]') as HTMLElement).click();
  await tick();
  (h.$('.btn.primary') as HTMLElement).click();
  assert.deepEqual(h.calls.at(-1), { cmd: 'open_url', args: { url: 'https://github.com/o/alpha/pull/1' } });
  (h.$('[data-ask="deep"]') as HTMLElement).click();
  assert.ok(h.calls.some((c) => c.cmd === 'analyze_pr' && c.args?.deep === true && c.args?.number === 1));
  assert.equal(h.$('[data-ask="deep"]')!.hasAttribute('disabled'), true, 'no double spending');
});

test('refresh targets what is on screen', async () => {
  const h = await setup();
  (h.$('#refresh') as HTMLElement).click();
  assert.equal(h.calls.at(-1)!.cmd, 'refresh_prs');
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  (h.$('#refresh') as HTMLElement).click();
  assert.deepEqual(h.calls.at(-1), { cmd: 'refresh_pr_repo', args: { repo: 'o/alpha' } });
  (h.$('tr[data-number="1"]') as HTMLElement).click();
  await tick();
  (h.$('#refresh') as HTMLElement).click();
  assert.deepEqual(h.calls.at(-1), { cmd: 'refresh_pr', args: { repo: 'o/alpha', number: 1 } });
});

test('rereading keeps the search box and its text', async () => {
  const h = await setup();
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  const search = h.$('input[type="search"]') as HTMLInputElement;
  search.value = 'Blocked one';
  search.dispatchEvent(new h.win.Event('input') as unknown as Event);
  h.emit('pr:changed');
  await tick(1600);
  assert.equal(h.$('input[type="search"]'), search, 'the same element');
  assert.equal(search.value, 'Blocked one');
  assert.deepEqual(prRows(h), [2]);
});

test('problems reach the banner', async () => {
  const h = await setup({
    repos: [row({ error: 'boom' })],
    status: { ...STATUS, error: 'Rate limited by GitHub; pausing until 12:00', paused_until: new Date(Date.now() + 60000).toISOString() },
  });
  const banner = h.$('#pr-banner')!;
  assert.equal(banner.hidden, false);
  assert.match(banner.textContent!, /Rate limited/);
  assert.match(banner.textContent!, /alpha: boom/);
  assert.equal(h.$('#pr-pill')!.textContent, 'PAUSED');
});

test('a PR that closed meanwhile says so', async () => {
  const h = await setup();
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  (h.$('tr[data-number="2"]') as HTMLElement).click();
  await tick();
  assert.match(h.$('#prs-view')!.textContent!, /no longer open/);
});

test('leaving the tab keeps its place, and choosing it again goes to the top', async () => {
  const h = await setup();
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  (h.$('[data-view="repos"]') as HTMLElement).click();
  assert.equal(h.$('#prs-view')!.hidden, true);
  assert.equal(h.$('#pr-nav')!.hidden, true);
  assert.equal(h.$('#crumbs')!.hidden, true);
  assert.equal(h.$('#pr-foot')!.hidden, true);
  assert.equal(h.$('#status-foot')!.hidden, false);
  (h.$('[data-view="prs"]') as HTMLElement).click();
  await tick();
  assert.deepEqual(prRows(h), [1, 2], 'back at the same repo');
  (h.$('[data-view="prs"]') as HTMLElement).click();
  await tick();
  assert.equal(appRows(h).length, 3);
});

test('off screen, a change rereads only the counts, and slowly', async () => {
  const h = await setup();
  (h.$('[data-view="repos"]') as HTMLElement).click();
  const before = h.calls.length;
  h.emit('pr:changed');
  await tick(1600);
  assert.equal(h.calls.length, before, 'not at the on-screen pace');
  assert.equal(h.calls.some((c) => c.cmd === 'get_pr_repo'), false);
});

test('a problem on the PR side stays on the PR tab', async () => {
  const h = await setup({ status: { ...STATUS, error: 'Rate limited by GitHub' } });
  assert.equal(h.$('#pr-banner')!.hidden, false);
  (h.$('[data-view="repos"]') as HTMLElement).click();
  await tick();
  assert.equal(h.$('#pr-banner')!.hidden, true);
});

test('Esc on the PR tab closes the help panel when leaving the tab too', async () => {
  const h = await setup();
  (h.$('tr[data-repo="o/alpha"]') as HTMLElement).click();
  await tick();
  (h.$('#help') as HTMLElement).click();
  assert.ok(h.$('#help-panel'));
  h.key('1', { metaKey: true });
  assert.equal(h.$('#help-panel'), null);
  assert.equal(h.$('#repos-view')!.hidden, false);
});
