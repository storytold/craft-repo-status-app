// Craft Status UI: renders the snapshot pushed by the Rust poller. main.ts
// picks the Tauri API, or a browser stand-in, and calls `start`.

import {
  COLUMNS, RECENT, WINDOWS, MANUAL, windowById, value, orderedEntries, nextSort, dropIndex,
  freshness, ago, totals, urgentAcross, tokenStatus, isRefreshing,
} from './model.ts';
import type { Column, Meta, RepoEntry, Snapshot, Sort } from './model.ts';

/** What the UI needs from the host: Tauri commands and events, window chrome. */
export interface Api {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  listen<T>(event: string, fn: (payload: T) => void): void;
  startResize(): void;
}

type View = 'repos' | 'urgent';

interface State {
  snap: Snapshot | null;
  meta: Meta;
  view: View;
  sort: Sort;
  order: string[];
  window: string;
  urgentRepo: string;
  criticalOnly: boolean;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string): T => {
  const n = document.getElementById(id);
  if (!n) throw new Error(`#${id} is missing from index.html`);
  return n as T;
};

function load<T>(k: string, d: T): T {
  try { return (JSON.parse(localStorage.getItem(k) ?? 'null') as T | null) ?? d; } catch { return d; }
}
function save(k: string, v: unknown): void {
  try { localStorage.setItem(k, JSON.stringify(v)); } catch { /* private mode */ }
}

// ───────────────────────────── formatting ─────────────────────────────

type Child = Node | string | null | undefined | false;
type Props = Record<string, unknown>;

function el<K extends keyof HTMLElementTagNameMap>(tag: K, props: Props = {}, ...kids: (Child | Child[])[]): HTMLElementTagNameMap[K] {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (v === undefined || v === null || v === false) continue;
    if (k === 'class') n.className = String(v);
    else if (k === 'dataset') Object.assign(n.dataset, v);
    else if (k.startsWith('on')) n.addEventListener(k.slice(2), v as EventListener);
    else n.setAttribute(k, String(v));
  }
  for (const kid of kids.flat()) if (kid !== null && kid !== undefined && kid !== false) n.append(kid);
  return n;
}

const localTime = (iso: string | null | undefined): string =>
  (iso ? new Date(iso).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }) : '');
const clock = (t: number): string => new Date(t).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
const num = (n: number | string | null | undefined): string => Number(n ?? 0).toLocaleString('en-US');
const gh = (repo: string, path = ''): string => `https://github.com/${repo}${path}`;
const shortName = (repo: string): string => repo.split('/').pop() ?? repo;
const classes = (...cs: (string | false | null | undefined)[]): string => cs.filter(Boolean).join(' ');
const DAY = 86400e3;

/** The toolbar's refresh arrow, as an element (SVG needs its namespace). */
function refreshIcon(): SVGSVGElement {
  const ns = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(ns, 'svg');
  svg.setAttribute('viewBox', '0 0 16 16');
  svg.setAttribute('aria-hidden', 'true');
  const path = document.createElementNS(ns, 'path');
  path.setAttribute('d', 'M13.5 8a5.5 5.5 0 1 1-1.6-3.9M13.5 2v3h-3');
  svg.append(path);
  return svg;
}

/** Starts the UI in the current document. Call once; returns a function that stops its clock. */
export function start(api: Api): () => void {
  const state: State = {
    snap: null,
    meta: {},
    view: (new URLSearchParams(location.search).get('view') ?? load<string>('view', 'repos')) === 'urgent' ? 'urgent' : 'repos',
    sort: load<Sort>('sort', { key: 'urgency', dir: 'desc' }),
    order: load<string[]>('order', []),
    window: windowById(load('window', '')).id,
    urgentRepo: '',
    criticalOnly: load('criticalOnly', false),
  };

  function openUrl(url: string, ev?: Event): void {
    ev?.stopPropagation();
    api.invoke('open_url', { url }).catch((e: unknown) => console.warn(e));
  }

  const link = (text: string, url: string, cls = ''): HTMLElement =>
    el('span', { class: `link ${cls}`, onclick: (e: Event) => openUrl(url, e) }, text);

  /** The repo's app icon; repos without one (libraries) get a lettered tile, unfetched ones a blank. */
  function appIcon(entry: Pick<RepoEntry, 'repo'> & Partial<RepoEntry>): HTMLElement {
    if (entry.icon) return el('img', { class: 'app-icon', src: entry.icon, alt: '' });
    if (!entry.stats) return el('span', { class: 'app-icon pending', 'aria-hidden': 'true' });
    return el('span', { class: 'app-icon none', 'aria-hidden': 'true' }, shortName(entry.repo).charAt(0).toUpperCase());
  }

  /** Cell content for a column. */
  function cell(entry: RepoEntry, col: Column): Node {
    const v = value(entry, col.key, state.window);
    const s = entry.stats;
    if (col.key === 'name') {
      const tip = entry.error
        ? `Last attempt failed: ${entry.error}`
        : entry.inaccessible ? 'Not accessible (private, or not found)'
        : entry.fetched_at ? `Fetched ${localTime(entry.fetched_at)}` : 'Not fetched yet';
      return el('span', { class: 'repo', title: tip }, el('span', { class: 'dot' }), appIcon(entry), link(String(v), gh(entry.repo)));
    }
    if (col.key === 'fetched') {
      const age = v ? el('span', { title: `Fetched ${localTime(String(v))}` }, ago(String(v))) : el('span', { class: 'muted', title: 'Not fetched yet' }, '—');
      return el('span', { class: 'fetched' }, age, rowRefresh(entry));
    }
    if (!s) return entry.inaccessible ? document.createTextNode('') : el('span', { class: 'muted' }, '—');
    switch (col.key) {
      case 'urgency': {
        const a = Math.min(1, Number(v) / 350) * 0.45;
        return el('span', { class: 'urgency', style: `background: rgb(var(--hot-rgb) / ${a.toFixed(2)})` }, String(Math.round(Number(v))));
      }
      case 'critical_issues': return el('span', { class: v ? 'crit' : 'zero' }, num(v));
      case 'open_prs': return link(num(v), gh(entry.repo, '/pulls'));
      case 'open_issues': return link(num(v), gh(entry.repo, '/issues'));
      case 'recent_commits': case 'recent_prs': case 'recent_merged': case 'recent_issues': case 'recent_people':
        if (v === null) {
          const why = state.window === 'release' && !s.latest_build ? 'No release yet' : 'Not fetched yet for this window';
          return el('span', { class: 'muted', title: why }, '—');
        }
        return el('span', {
          class: v ? 'active' : 'zero',
          title: state.window === 'release' ? `Since ${s.latest_build}, published ${localTime(s.latest_build_at)} (${ago(s.latest_build_at)} ago)` : null,
        }, num(v));
      case 'commits_since_build': {
        const lag = s.since_build;
        if (!lag) return el('span', { class: 'muted' }, '—');
        const short = (sha: string | null) => sha?.slice(0, 7);
        const tip = {
          exact: `${num(v)} commits on main since ${s.latest_build} (${short(lag.from_sha)})`,
          branched: `${s.latest_build}’s commit isn’t on main: ~${num(v)} commits on main since the two split`,
          by_date: `${s.latest_build}’s commit shares no history with main: ~${num(v)} commits on main are newer than it`
            + (lag.from_sha ? `, counted from ${short(lag.from_sha)}` : ''),
        }[lag.basis];
        const from = lag.basis === 'by_date' ? lag.from_sha : s.latest_build;
        const text = `${lag.basis === 'exact' ? '' : '~'}${num(v)}`;
        return el('span', { title: tip }, from ? link(text, gh(entry.repo, `/compare/${encodeURIComponent(from)}...HEAD`), v ? '' : 'zero') : text);
      }
      case 'latest_build':
        if (!v) return el('span', { class: 'muted' }, '—');
        return el('span', {}, link(String(v), gh(entry.repo, `/releases/tag/${encodeURIComponent(v)}`)),
          s.latest_build_prerelease ? el('span', { class: 'pre' }, 'PRE') : null);
    }
    if (col.kind === 'date') {
      if (!v) return el('span', { class: 'muted' }, '—');
      const old = col.key === 'oldest_open_pr_at' && Date.now() - Date.parse(String(v)) > 14 * DAY;
      return el('span', { title: localTime(String(v)), class: old ? 'old' : '' }, ago(String(v)));
    }
    return document.createTextNode(num(v));
  }

  /** The tiny button that refetches just this row. */
  function rowRefresh(entry: RepoEntry): HTMLButtonElement {
    const busy = state.snap ? isRefreshing(state.snap, entry.repo) : false;
    return el('button', {
      class: classes('row-refresh', busy && 'spinning'),
      title: busy ? 'Refreshing this repo…' : `Refresh only ${shortName(entry.repo)}`,
      'aria-label': `Refresh ${shortName(entry.repo)}`,
      onclick: (e: Event) => {
        e.stopPropagation();
        (e.currentTarget as HTMLElement).classList.add('spinning');
        api.invoke('refresh_repo', { repo: entry.repo }).catch((err: unknown) => console.warn(err));
      },
    }, refreshIcon());
  }

  // ───────────────────────────── rendering ─────────────────────────────

  /** The group row, built once: rebuilding it would destroy the window
   * dropdown, closing it mid-choice whenever a poll or the clock re-renders. */
  let groupRow: HTMLTableRowElement | null = null;

  function renderHead(): void {
    if (!groupRow) {
      const row = el('tr', { class: 'groups' });
      let prev: string | null = null;
      for (const c of COLUMNS) {
        if (c.group === prev) { (row.lastChild as HTMLTableCellElement).colSpan += 1; continue; }
        prev = c.group;
        row.append(el('th', { class: c.group ? 'group-start' : '' }, c.group === RECENT ? windowPicker() : c.group));
      }
      $('thead').replaceChildren(row);
      groupRow = row;
    }
    const picker = groupRow.querySelector<HTMLSelectElement>('.window-picker')!;
    if (picker.value !== state.window) picker.value = state.window;
    const cols = el('tr', { class: 'cols' });
    COLUMNS.forEach((c, i) => {
      const sorted = state.sort.key === c.key;
      cols.append(el('th', {
        title: c.title?.replace('{span}', windowById(state.window).span) ?? `Sort by ${c.label}`,
        class: classes(sorted && 'sorted', sorted && state.sort.dir === 'asc' && 'asc', groupStart(i)),
        onclick: () => {
          state.sort = nextSort(state.sort, c.key);
          save('sort', state.sort);
          render();
        },
      }, c.label));
    });
    groupRow.nextSibling?.remove();
    $('thead').append(cols);
  }

  /** The dropdown that heads the recent-activity columns. */
  function windowPicker(): HTMLSelectElement {
    return el('select', {
      class: 'window-picker',
      'aria-label': 'Time window for recent activity',
      title: 'Time window for the activity columns',
      onchange: (e: Event) => { state.window = (e.target as HTMLSelectElement).value; save('window', state.window); render(); },
    }, WINDOWS.map((w) => el('option', { value: w.id }, w.label)));
  }

  function groupStart(i: number): string {
    return i > 0 && COLUMNS[i - 1]!.group !== COLUMNS[i]!.group ? 'group-start' : '';
  }

  function renderTable(snap: Snapshot): void {
    renderHead();
    const rows = orderedEntries(snap.repos, state.sort, state.order, state.window);
    const now = Date.now();
    $('tbody').replaceChildren(...rows.map((e) => {
      const stale = !e.fetched_at || now - Date.parse(e.fetched_at) > snap.stale_after_secs * 1000;
      return el('tr', {
        class: classes(stale && 'stale', e.error && 'error'),
        dataset: { repo: e.repo },
        title: 'Click for this repo’s most urgent issues · drag to reorder',
        onpointerdown: startRowDrag,
        onclick: () => {
          if (justDragged) return;
          state.urgentRepo = e.repo;
          setView('urgent');
        },
      }, COLUMNS.map((c, i) => el('td', { class: groupStart(i) }, cell(e, c))));
    }));
    if (!rows.length) {
      $('tbody').replaceChildren(el('tr', {}, el('td', { colspan: COLUMNS.length, class: 'empty' }, 'No repositories configured — Edit config.')));
    }
    const t = totals(snap.repos, state.window);
    $('tfoot').replaceChildren(el('tr', {}, COLUMNS.map((c, i) =>
      el('td', { class: groupStart(i) }, i === 0 ? `${snap.repos.length} repos` : c.kind === 'num' ? num(Math.round(t[c.key] ?? 0)) : ''))));
  }

  // ───────────────────────────── row dragging ─────────────────────────────
  // Pointer events rather than HTML5 drag and drop, which the webview's own
  // file-drop handling can swallow. A press turns into a drag once it moves a
  // few pixels; until then it is an ordinary click. Dropping saves the order on
  // screen and switches the table to it; clicking a column sorts again.

  let drag: { row: HTMLTableRowElement; startY: number; moved: boolean } | null = null;
  let justDragged = false;

  function startRowDrag(e: PointerEvent): void {
    if (e.button !== 0 || (e.target as Element).closest('.link, .row-refresh')) return;
    drag = { row: e.currentTarget as HTMLTableRowElement, startY: e.clientY, moved: false };
    window.addEventListener('pointermove', moveRowDrag);
    window.addEventListener('pointerup', endRowDrag);
    window.addEventListener('pointercancel', cancelRowDrag);
  }

  function moveRowDrag(e: PointerEvent): void {
    if (!drag) return;
    if (!drag.moved) {
      if (Math.abs(e.clientY - drag.startY) < 5) return;
      drag.moved = true;
      drag.row.classList.add('dragging');
      document.body.classList.add('reordering');
      window.getSelection()?.removeAllRanges();
    }
    const main = $('repos-view');
    const box = main.getBoundingClientRect();
    if (e.clientY < box.top + 50) main.scrollTop -= 12;
    else if (e.clientY > box.bottom - 40) main.scrollTop += 12;

    const tbody = $('tbody');
    const others = [...tbody.children].filter((r) => r !== drag!.row);
    const mids = others.map((r) => { const b = r.getBoundingClientRect(); return b.top + b.height / 2; });
    const next = others[dropIndex(mids, e.clientY)] ?? null;
    if (drag.row.nextSibling !== next) tbody.insertBefore(drag.row, next);
  }

  /** Ends a press; true if it had become a drag. */
  function finishRowDrag(): boolean {
    window.removeEventListener('pointermove', moveRowDrag);
    window.removeEventListener('pointerup', endRowDrag);
    window.removeEventListener('pointercancel', cancelRowDrag);
    document.body.classList.remove('reordering');
    const moved = drag?.moved ?? false;
    drag = null;
    return moved;
  }

  function endRowDrag(): void {
    if (!finishRowDrag()) return;
    // The click that follows this pointerup must not open the urgent view.
    justDragged = true;
    setTimeout(() => { justDragged = false; });
    state.order = [...$('tbody').children].map((r) => (r as HTMLElement).dataset.repo ?? '');
    state.sort = { key: MANUAL };
    save('order', state.order);
    save('sort', state.sort);
    render();
  }

  function cancelRowDrag(): void {
    if (finishRowDrag()) render();
  }

  function renderUrgent(snap: Snapshot): void {
    if (state.urgentRepo && !snap.repos.some((r) => r.repo === state.urgentRepo)) state.urgentRepo = '';
    const sel = $<HTMLSelectElement>('urgent-repo');
    sel.replaceChildren(el('option', { value: '' }, 'All repositories'),
      ...snap.repos.map((r) => el('option', { value: r.repo }, shortName(r.repo))));
    sel.value = state.urgentRepo;
    $<HTMLInputElement>('critical-only').checked = state.criticalOnly;
    const items = urgentAcross(snap.repos, { repo: state.urgentRepo, criticalOnly: state.criticalOnly });
    const SEVERE = /crash|panic|freeze|hang|data loss|corruption|won't|blank|security/;
    $('urgent-list').replaceChildren(...items.map((u) => {
      const a = Math.min(1, u.score / 90) * 0.5;
      return el('li', { class: `urgent-item${u.critical ? ' critical' : ''}`, onclick: () => openUrl(u.url), title: u.url },
        el('span', { class: 'score', style: `background: rgb(var(--hot-rgb) / ${a.toFixed(2)})` }, String(u.score)),
        el('span', { class: 'u-repo' }, appIcon(snap.repos.find((r) => r.repo === u.repo) ?? { repo: u.repo }), shortName(u.repo)),
        el('div', { class: 'u-title' },
          el('span', { class: 'num' }, `#${u.number}`), el('span', { class: 'text' }, u.title),
          el('div', { class: 'reasons' }, u.reasons.map((r) => el('span', { class: `reason${SEVERE.test(r) ? ' sev' : ''}` }, r)),
            u.labels.filter((l) => !u.reasons.includes(`${l} label`)).map((l) => el('span', { class: 'reason' }, l)))),
        el('span', { class: 'u-age', title: `Opened ${localTime(u.created_at)}` }, ago(u.created_at)));
    }));
    if (!items.length) {
      $('urgent-list').replaceChildren(el('li', { class: 'empty' }, snap.repos.some((r) => r.stats) ? 'Nothing urgent. 🎉' : 'Waiting for the first poll…'));
    }
  }

  function renderStatus(snap: Snapshot): void {
    const { oldest, stale } = freshness(snap);
    const pill = $('fresh-pill');
    if (snap.refreshing) { pill.className = 'pill refreshing'; pill.textContent = 'REFRESHING'; }
    else if (stale) { pill.className = 'pill stale'; pill.textContent = 'STALE'; }
    else { pill.className = 'pill fresh'; pill.textContent = 'FRESH'; }
    const last = snap.last_full_refresh_at ? Date.parse(snap.last_full_refresh_at) : null;
    let text: string;
    if (oldest === null) text = last ? `Partial data · last full refresh ${clock(last)}` : 'No data yet';
    else {
      text = `Data as of ${clock(oldest)} (${ago(new Date(oldest).toISOString())} ago)`;
      if (last && Math.abs(last - oldest) > 60e3) text += ` · last full refresh ${clock(last)}`;
    }
    $('refreshed').textContent = text;
    $('refreshed').title = [
      `Oldest data on screen: ${oldest ? localTime(new Date(oldest).toISOString()) : '—'}`,
      `Last full refresh: ${localTime(snap.last_full_refresh_at) || '—'}`,
      `Stale after ${Math.round(snap.stale_after_secs / 60)} min without new data`,
    ].join('\n');
    $('refresh').classList.toggle('spinning', !!snap.refreshing);

    const failing = snap.repos.filter((r) => r.error);
    const msgs = [
      snap.error,
      failing.length ? `${failing.length} repo${failing.length > 1 ? 's' : ''} failed last poll: ${failing.map((r) => `${shortName(r.repo)} (${r.error})`).join('; ')}` : null,
    ].filter(Boolean);
    $('banner').hidden = !msgs.length;
    $('banner').textContent = msgs.join(' — ');

    $('next').textContent = snap.refreshing ? 'Fetching from GitHub…'
      : snap.next_refresh_at ? `Next fetch ${clock(Date.parse(snap.next_refresh_at))} · each repo every ${Math.round(snap.poll_secs / 60)} min` : '';
    $('budget').textContent = snap.rate_limit_remaining != null ? `GitHub budget ${num(snap.rate_limit_remaining)}` : '';
    const token = tokenStatus(snap.token);
    $('token').textContent = token.text;
    $('token').title = token.title;
    $('token').classList.toggle('bad', token.bad);
    const critical = snap.repos.reduce((n, r) => n + (r.stats?.critical_issues ?? 0), 0);
    $('urgent-count').textContent = critical ? String(critical) : '';
    $('urgent-count').title = `${critical} critical open issues`;
    $('repo-count').textContent = String(snap.repos.length);
  }

  function render(): void {
    // Rebuilding the table mid-drag would drop the row out of the user's hand;
    // the drop renders anyway.
    const snap = state.snap;
    if (!snap || drag?.moved) return;
    renderStatus(snap);
    document.querySelectorAll<HTMLElement>('.view').forEach((b) => b.setAttribute('aria-selected', String(b.dataset.view === state.view)));
    $('repos-view').hidden = state.view !== 'repos';
    $('urgent-view').hidden = state.view !== 'urgent';
    if (state.view === 'repos') renderTable(snap); else renderUrgent(snap);
  }

  function setView(v: View): void {
    state.view = v;
    save('view', v);
    render();
  }

  function applyMeta(m: Meta | null): void {
    state.meta = m ?? {};
    document.documentElement.style.setProperty('--opacity', String(state.meta.opacity ?? 0.96));
    $('pin').setAttribute('aria-pressed', String(!!state.meta.always_on_top));
    const mac = navigator.platform.toLowerCase().includes('mac');
    $('shortcut').textContent = (state.meta.shortcut ?? '').replace('CmdOrCtrl', mac ? '⌘' : 'Ctrl').replace('Shift', '⇧').replaceAll('+', '');
  }

  // ───────────────────────────── wiring ─────────────────────────────

  document.querySelectorAll<HTMLElement>('.view').forEach((b) => b.addEventListener('click', () => {
    const view = b.dataset.view === 'urgent' ? 'urgent' : 'repos';
    if (view === 'urgent' && state.view !== 'urgent') state.urgentRepo = '';
    setView(view);
  }));
  $('urgent-repo').addEventListener('change', (e) => { state.urgentRepo = (e.target as HTMLSelectElement).value; render(); });
  $('critical-only').addEventListener('change', (e) => {
    state.criticalOnly = (e.target as HTMLInputElement).checked;
    save('criticalOnly', state.criticalOnly);
    render();
  });
  $('refresh').addEventListener('click', () => api.invoke('refresh_now'));
  $('pin').addEventListener('click', () => api.invoke('set_always_on_top', { on: !state.meta.always_on_top }));
  $('hide').addEventListener('click', () => api.invoke('hide_window'));
  $('edit-config').addEventListener('click', () => api.invoke('open_config'));
  $('resize').addEventListener('mousedown', (e) => { e.preventDefault(); api.startResize(); });
  document.addEventListener('keydown', (e) => {
    const mod = e.metaKey || e.ctrlKey;
    if (e.key === 'Escape') api.invoke('hide_window');
    else if (mod && e.key.toLowerCase() === 'r') { e.preventDefault(); api.invoke('refresh_now'); }
    else if (mod && e.key === '1') setView('repos');
    else if (mod && e.key === '2') setView('urgent');
  });

  api.listen<Snapshot>('craft:snapshot', (s) => { state.snap = s; render(); });
  api.listen<Meta>('craft:meta', applyMeta);
  // Relative times and staleness change with the clock alone.
  const clockTimer = setInterval(render, 15_000);

  (async () => {
    try {
      applyMeta(await api.invoke<Meta>('get_meta'));
      state.snap = await api.invoke<Snapshot>('get_snapshot');
      render();
    } catch (e) {
      console.error(e);
    } finally {
      // Not requestAnimationFrame: WebKit never fires it while the window is hidden.
      api.invoke('window_ready');
    }
  })();

  return () => clearInterval(clockTimer);
}
