// Craft Status UI: the window's chrome and tabs. Downloads and Fix first render the
// snapshot pushed by the Rust status poller; the PR tab is pr-app.ts. main.ts picks
// the Tauri API, or a browser stand-in, and calls `start`.

import {
  ALL_TIME, COLUMNS, RECENT, WINDOWS, MANUAL, windowById, value, orderedEntries, nextSort, dropIndex,
  freshness, ago, totals, urgentAcross, tokenStatus, isRefreshing, isThirdParty, formatNum, displayValue, changedCells,
  REPO_KINDS, isRepoHidden, toggleKind, manualOrder, reorderVisible,
} from './model.ts';
import type { Column, Meta, RepoEntry, RepoStats, Snapshot, Sort, Value } from './model.ts';
import { startPrs } from './pr-app.ts';
import type { Route } from './pr-app.ts';
import { $, DAY, classes, el, load, localTime, save } from './dom.ts';
import type { Api } from './dom.ts';

export type { Api } from './dom.ts';

type View = 'repos' | 'prs' | 'urgent';

const asView = (v: string | null): View => (v === 'urgent' || v === 'prs' ? v : 'repos');

/** What triggered a render: a snapshot from the host, the clock, or the user. */
type Reason = 'data' | 'clock' | 'user';

interface State {
  snap: Snapshot | null;
  meta: Meta;
  view: View;
  sort: Sort;
  order: string[];
  window: string;
  urgentRepo: string;
  criticalOnly: boolean;
  /** The ids of the REPO_KINDS toggled off. */
  hiddenKinds: string[];
}

// ───────────────────────────── formatting ─────────────────────────────

const clock = (t: number): string => new Date(t).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
const num = formatNum;
const gh = (repo: string, path = ''): string => `https://github.com/${repo}${path}`;
const shortName = (repo: string): string => repo.split('/').pop() ?? repo;
const shortSha = (sha: string | null) => sha?.slice(0, 7);
const isElement = (n: Node): n is Element => n.nodeType === n.ELEMENT_NODE;

function setTitle(n: Element, title: string | null): void {
  if (title === null) n.removeAttribute('title'); else n.setAttribute('title', title);
}

/** Puts `node` in `td` unless `td` already holds an equal node. When the two
 * differ only in the outer element's `title`, the node in `td` stays and
 * takes the new title. */
function setCell(td: HTMLTableCellElement, node: Node): void {
  const old = td.childNodes.length === 1 ? td.firstChild! : null;
  if (old && isElement(old) && isElement(node)) {
    const title = node.getAttribute('title');
    setTitle(node, old.getAttribute('title'));
    if (old.isEqualNode(node)) { setTitle(old, title); return; }
    setTitle(node, title);
  } else if (old?.isEqualNode(node)) return;
  td.replaceChildren(node);
}

/** The Repo cell's tooltip: the last attempt's error, no access, or when the data was fetched. */
function repoTip(entry: RepoEntry): string {
  if (entry.error) return `Last attempt failed: ${entry.error}`;
  if (entry.inaccessible) return 'Not accessible (private, or not found)';
  return entry.fetched_at ? `Fetched ${localTime(entry.fetched_at)}` : 'Not fetched yet';
}

/** A date column's cell; `oldest_open_pr_at` is marked `old` past 14 days. */
function dateCell(key: string, v: Value, text: string, now: number): HTMLElement {
  if (!v) return el('span', { class: 'muted' }, text);
  const old = key === 'oldest_open_pr_at' && now - Date.parse(String(v)) > 14 * DAY;
  return el('span', { title: localTime(String(v)), class: old ? 'old' : '' }, text);
}

/** A footer cell's text: the repo count under the first column, totals under count columns (all-time-only ones in the All time window). */
function footText(c: Column, i: number, repos: number, t: Record<string, number>, window: string): string {
  if (i === 0) return `${repos} repos`;
  if (c.kind !== 'num' || (c.allTimeOnly === true && window !== ALL_TIME)) return '';
  return num(Math.round(t[c.key] ?? 0));
}

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

/**
 * Starts the UI in the current document. Call once; returns a function that stops
 * its clocks. `prRoute` opens the PR tab at that view.
 */
export function start(api: Api, prRoute?: Route): () => void {
  const state: State = {
    snap: null,
    meta: {},
    view: prRoute ? 'prs' : asView(new URLSearchParams(location.search).get('view') ?? load<string>('view', 'repos')),
    sort: load<Sort>('sort', { key: 'urgency', dir: 'desc' }),
    order: load<string[]>('order', []),
    window: windowById(load('window', '')).id,
    urgentRepo: '',
    criticalOnly: load('criticalOnly', false),
    hiddenKinds: load<string[]>('hiddenKinds', []),
  };

  function openUrl(url: string, ev?: Event): void {
    ev?.stopPropagation();
    api.invoke('open_url', { url }).catch((e: unknown) => console.warn(e));
  }

  const link = (text: string, url: string, cls = ''): HTMLElement =>
    el('span', { class: `link ${cls}`, dataset: { url }, onclick: (e: Event) => openUrl(url, e) }, text);

  /** The repo's app icon; repos without one (libraries) get a lettered tile, unfetched ones a blank. */
  function appIcon(entry: Pick<RepoEntry, 'repo'> & Partial<RepoEntry>): HTMLElement {
    if (entry.icon) return el('img', { class: 'app-icon', src: entry.icon, alt: '' });
    if (!entry.stats) return el('span', { class: 'app-icon pending', 'aria-hidden': 'true' });
    return el('span', { class: 'app-icon none', 'aria-hidden': 'true' }, shortName(entry.repo).charAt(0).toUpperCase());
  }

  /** A "3P" tag for repos outside our own GitHub org; null for ours. */
  const thirdPartyTag = (repo: string): HTMLElement | null =>
    isThirdParty(repo) ? el('span', { class: 'third-party', title: `Third-party app (${repo.split('/')[0]})` }, '3P') : null;

  /** Cell content for a column; its text is `displayValue`'s, which the highlight compares. */
  function cell(entry: RepoEntry, col: Column, now: number): Node {
    const v = value(entry, col.key, state.window);
    const text = displayValue(entry, col.key, state.window, now);
    const s = entry.stats;
    if (col.key === 'name') {
      return el('span', { class: 'repo', title: repoTip(entry) }, el('span', { class: 'dot' }), appIcon(entry), link(text, gh(entry.repo)), thirdPartyTag(entry.repo));
    }
    if (col.key === 'category') return el('span', { class: v ? 'category' : 'muted' }, text);
    if (!s) return entry.inaccessible ? document.createTextNode(text) : el('span', { class: 'muted' }, text);
    switch (col.key) {
      case 'urgency': {
        const a = Math.min(1, Number(v) / 350) * 0.45;
        return el('span', { class: 'urgency', style: `background: rgb(var(--hot-rgb) / ${a.toFixed(2)})` }, text);
      }
      case 'downloads_total':
        if (v === null) return el('span', { class: 'muted', title: 'Only known for All time: choose it in the dropdown above' }, text);
        return document.createTextNode(text);
      case 'critical_issues': return el('span', { class: v ? 'crit' : 'zero' }, text);
      case 'open_prs': return link(text, gh(entry.repo, '/pulls'));
      case 'open_issues': return link(text, gh(entry.repo, '/issues'));
      case 'recent_commits': case 'recent_prs': case 'recent_merged': case 'recent_issues': case 'recent_people':
        return recentCell(s, v, text, now);
      case 'commits_since_build': return sinceBuildCell(entry.repo, s, v, text);
      case 'latest_build_rate':
        if (v === null) return el('span', { class: 'muted' }, text);
        return el('span', {
          class: v ? '' : 'zero',
          title: `${num(s.latest_build_downloads)} downloads of ${s.latest_build}, out ${ago(s.latest_build_at, Date.parse(String(entry.fetched_at)))} when counted`,
        }, text);
      case 'latest_build': return latestBuildCell(entry.repo, s, v, text);
    }
    if (col.kind === 'date') return dateCell(col.key, v, text, now);
    return document.createTextNode(text);
  }

  /** A recent-activity cell; in the release window its tooltip names the release the count starts at. */
  function recentCell(s: RepoStats, v: Value, text: string, now: number): HTMLElement {
    if (v === null) {
      const why = state.window === 'release' && !s.latest_build ? 'No release yet' : 'Not fetched yet for this window';
      return el('span', { class: 'muted', title: why }, text);
    }
    let tip: string | null = null;
    if (state.window === 'release') tip = `Since ${s.latest_build}, published ${localTime(s.latest_build_at)} (${ago(s.latest_build_at, now)} ago)`;
    return el('span', { class: v ? 'active' : 'zero', title: tip }, text);
  }

  /** The commits-since-release cell: the count links to the compare view, and the tooltip says how it was counted. */
  function sinceBuildCell(repo: string, s: RepoStats, v: Value, text: string): HTMLElement {
    const lag = s.since_build;
    if (!lag) return el('span', { class: 'muted' }, text);
    const counted = lag.from_sha ? `, counted from ${shortSha(lag.from_sha)}` : '';
    const tip = {
      exact: `${num(v)} commits on main since ${s.latest_build} (${shortSha(lag.from_sha)})`,
      branched: `${s.latest_build}’s commit isn’t on main: ~${num(v)} commits on main since the two split`,
      by_date: `${s.latest_build}’s commit shares no history with main: ~${num(v)} commits on main are newer than it${counted}`,
    }[lag.basis];
    const from = lag.basis === 'by_date' ? lag.from_sha : s.latest_build;
    if (!from) return el('span', { title: tip }, text);
    return el('span', { title: tip }, link(text, gh(repo, `/compare/${encodeURIComponent(from)}...HEAD`), v ? '' : 'zero'));
  }

  /** The latest release cell: the tag is a link, and a pre-release adds a PRE badge after it, matching `text`. */
  function latestBuildCell(repo: string, s: RepoStats, v: Value, text: string): HTMLElement {
    if (!v) return el('span', { class: 'muted' }, text);
    return el('span', {}, link(String(v), gh(repo, `/releases/tag/${encodeURIComponent(v)}`)),
      s.latest_build_prerelease ? el('span', { class: 'pre' }, 'PRE') : null);
  }

  /** The Fetched cell: the data's age and the row's refresh button. The cell
   * is built once per row. Later renders change only the age text, its
   * tooltip and `muted` class, and the button's spin and tooltip. */
  function updateFetched(td: HTMLTableCellElement, entry: RepoEntry, now: number): void {
    let box = td.firstElementChild;
    if (!box?.classList.contains('fetched')) {
      box = el('span', { class: 'fetched' }, el('span'), rowRefresh(entry.repo));
      td.replaceChildren(box);
    }
    const age = box.firstElementChild as HTMLElement;
    const text = displayValue(entry, 'fetched', state.window, now);
    if (age.textContent !== text) age.textContent = text;
    age.title = entry.fetched_at ? `Fetched ${localTime(entry.fetched_at)}` : 'Not fetched yet';
    age.classList.toggle('muted', !entry.fetched_at);
    const button = box.lastElementChild as HTMLButtonElement;
    const busy = state.snap ? isRefreshing(state.snap, entry.repo) : false;
    button.classList.toggle('spinning', busy);
    button.title = busy ? 'Refreshing this repo…' : `Refresh only ${shortName(entry.repo)}`;
  }

  /** The tiny button that refetches just this row; `updateFetched` sets its spin and tooltip. */
  function rowRefresh(repo: string): HTMLButtonElement {
    return el('button', {
      class: 'row-refresh',
      'aria-label': `Refresh ${shortName(repo)}`,
      onclick: (e: Event) => {
        e.stopPropagation();
        (e.currentTarget as HTMLElement).classList.add('spinning');
        api.invoke('refresh_repo', { repo }).catch((err: unknown) => console.warn(err));
      },
    }, refreshIcon());
  }

  // ───────────────────────────── rendering ─────────────────────────────

  /** The header rows, built once and updated in place: rebuilding them would
   * destroy the window dropdown, closing it mid-choice whenever a poll or the
   * clock re-renders. */
  let groupRow: HTMLTableRowElement | null = null;
  let colsRow: HTMLTableRowElement | null = null;

  function renderHead(): void {
    if (!groupRow) {
      const row = el('tr', { class: 'groups' });
      let prev: string | null = null;
      for (const c of COLUMNS) {
        if (c.group === prev) { (row.lastChild as HTMLTableCellElement).colSpan += 1; continue; }
        prev = c.group;
        row.append(el('th', { class: c.group ? 'group-start' : '' }, c.group === RECENT ? windowPicker() : c.group));
      }
      colsRow = el('tr', { class: 'cols' }, COLUMNS.map((c) => el('th', {
        onclick: () => {
          state.sort = nextSort(state.sort, c.key);
          save('sort', state.sort);
          render();
        },
      }, c.label)));
      $('thead').replaceChildren(row, colsRow);
      groupRow = row;
    }
    const picker = groupRow.querySelector<HTMLSelectElement>('.window-picker')!;
    if (picker.value !== state.window) picker.value = state.window;
    const cols = colsRow!;
    COLUMNS.forEach((c, i) => {
      const sorted = state.sort.key === c.key;
      const th = cols.cells[i]!;
      th.title = c.title?.replace('{span}', windowById(state.window).span) ?? `Sort by ${c.label}`;
      th.className = classes(sorted && 'sorted', sorted && state.sort.dir === 'asc' && 'asc', groupStart(i));
    });
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

  /** The repos the toggles beside the tabs leave showing. */
  const shownRepos = (snap: Snapshot): RepoEntry[] =>
    snap.repos.filter((r) => !isRepoHidden(r.repo, r.category, state.hiddenKinds));

  // Rows are kept per repo, and their cells are updated in place. While the
  // pointer is over the rows, a snapshot or the clock leaves their order
  // alone, and the order is applied when the pointer leaves. Sorting, window
  // changes and drops apply it at once.

  const rows = new Map<string, HTMLTableRowElement>();
  let emptyRow: HTMLTableRowElement | null = null;
  let footRow: HTMLTableRowElement | null = null;
  let hovering = false;
  let pendingOrder = false;

  function newRow(repo: string): HTMLTableRowElement {
    const tr: HTMLTableRowElement = el('tr', {
      dataset: { repo },
      title: 'Click for this repo’s most urgent issues · drag to reorder',
      onpointerdown: startRowDrag,
      onclick: () => {
        if (justDragged) return;
        state.urgentRepo = tr.dataset.repo ?? '';
        setView('urgent');
      },
    }, COLUMNS.map((_, i) => el('td', { class: groupStart(i) })));
    return tr;
  }

  function updateRow(tr: HTMLTableRowElement, entry: RepoEntry, snap: Snapshot, now: number): void {
    const stale = !entry.fetched_at || now - Date.parse(entry.fetched_at) > snap.stale_after_secs * 1000;
    tr.className = classes(stale && 'stale', entry.error && 'error', snap.fetching === entry.repo && 'fetching');
    COLUMNS.forEach((c, i) => {
      if (c.key === 'fetched') updateFetched(tr.cells[i]!, entry, now);
      else setCell(tr.cells[i]!, cell(entry, c, now));
    });
  }

  /** How long a changed cell stays highlighted. It matches `td.changed`'s animation in style.css. */
  const FLASH_MS = 2500;
  const flashTimers = new WeakMap<HTMLTableCellElement, ReturnType<typeof setTimeout>>();
  const flashStarts = new WeakMap<HTMLTableCellElement, number>();

  /** Highlights a cell for FLASH_MS, restarting the highlight if it is already on. */
  function flash(td: HTMLTableCellElement): void {
    clearTimeout(flashTimers.get(td));
    td.classList.remove('changed');
    td.style.animationDelay = '';
    td.getBoundingClientRect(); // Forcing a reflow makes the animation start again.
    td.classList.add('changed');
    flashStarts.set(td, Date.now());
    flashTimers.set(td, setTimeout(() => {
      td.classList.remove('changed');
      td.style.animationDelay = '';
      flashTimers.delete(td);
    }, FLASH_MS));
  }

  /** The browser starts a cell's animation over when its row moves or the
   * table is shown again. A negative delay continues each highlight in
   * `cells` from where its timer is. Call it only for cells whose animation
   * restarted: on a running animation the delay moves it forward. */
  function resumeFlashes(cells: Iterable<HTMLTableCellElement>): void {
    const now = Date.now();
    for (const td of cells) {
      const start = flashStarts.get(td);
      if (start !== undefined && td.classList.contains('changed')) td.style.animationDelay = `${start - now}ms`;
    }
  }

  /** Highlights the cells whose shown text differs between `prev`'s entries and the snapshot just drawn. */
  function flashChanges(prev: ReadonlyMap<string, RepoEntry>, snap: Snapshot): void {
    // A hidden table shows nothing, and mid-drag render() leaves the cells as they were.
    if (state.view !== 'repos' || drag?.moved) return;
    const now = Date.now();
    for (const entry of snap.repos) {
      const tr = rows.get(entry.repo);
      if (!tr) continue;
      for (const key of changedCells(prev.get(entry.repo), entry, state.window, now)) {
        flash(tr.cells[COLUMNS.findIndex((c) => c.key === key)]!);
      }
    }
  }

  /** Puts `want` in order at the top of `tbody`, moving only rows that are out of place, and removes the rest. */
  function placeRows(tbody: HTMLElement, want: readonly HTMLTableRowElement[]): void {
    let at = tbody.firstElementChild;
    for (const tr of want) {
      if (tr === at) { at = at.nextElementSibling; continue; }
      if (at) at.before(tr);
      else tbody.append(tr);
      resumeFlashes(tr.cells);
    }
    while (at) { const next = at.nextElementSibling; at.remove(); at = next; }
  }

  function renderTable(snap: Snapshot, reason: Reason): void {
    renderHead();
    const tbody = $('tbody');
    const shown = shownRepos(snap);
    const entries = orderedEntries(shown, state.sort, state.order, state.window);
    const now = Date.now();
    const configured = new Set(entries.map((e) => e.repo));
    for (const [repo, tr] of rows) if (!configured.has(repo)) { tr.remove(); rows.delete(repo); }
    const want = entries.map((e) => {
      let tr = rows.get(e.repo);
      if (!tr) { tr = newRow(e.repo); rows.set(e.repo, tr); }
      updateRow(tr, e, snap, now);
      return tr;
    });
    if (!want.length) {
      const why = snap.repos.length ? 'Every repo is hidden by the toggles beside the tabs.' : 'No repositories configured — Edit config.';
      emptyRow ??= el('tr', {}, el('td', { colspan: COLUMNS.length, class: 'empty' }));
      emptyRow.cells[0]!.textContent = why;
      tbody.replaceChildren(emptyRow);
      pendingOrder = false;
    } else if (hovering && reason !== 'user') {
      emptyRow?.remove();
      for (const tr of want) if (tr.parentNode !== tbody) tbody.append(tr);
      pendingOrder = want.some((tr, i) => tbody.children[i] !== tr);
    } else {
      placeRows(tbody, want);
      pendingOrder = false;
    }
    const t = totals(shown, state.window);
    const foot = footRow ??= el('tr', {}, COLUMNS.map((_, i) => el('td', { class: groupStart(i) })));
    if (foot.parentNode !== $('tfoot')) $('tfoot').replaceChildren(foot);
    COLUMNS.forEach((c, i) => {
      foot.cells[i]!.textContent = footText(c, i, shown.length, t, state.window);
    });
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
    if (drag.row.nextSibling !== next) { tbody.insertBefore(drag.row, next); resumeFlashes(drag.row.cells); }
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
    const dragged = [...$('tbody').children].map((r) => (r as HTMLElement).dataset.repo ?? '');
    state.order = reorderVisible(manualOrder(state.snap?.repos ?? [], state.order).map((r) => r.repo), dragged);
    state.sort = { key: MANUAL };
    save('order', state.order);
    save('sort', state.sort);
    render();
  }

  function cancelRowDrag(): void {
    if (finishRowDrag()) render();
  }

  /** The repos the urgent view's picker lists. The picker is rebuilt only when they change. */
  let pickerRepos: string | null = null;

  function renderUrgent(snap: Snapshot): void {
    const shown = shownRepos(snap);
    if (!shown.some((r) => r.repo === state.urgentRepo)) state.urgentRepo = '';
    const sel = $<HTMLSelectElement>('urgent-repo');
    const repos = shown.map((r) => r.repo).join('\n');
    if (repos !== pickerRepos) {
      sel.replaceChildren(el('option', { value: '' }, 'All repositories'),
        ...shown.map((r) => el('option', { value: r.repo }, shortName(r.repo))));
      pickerRepos = repos;
    }
    if (sel.value !== state.urgentRepo) sel.value = state.urgentRepo;
    $<HTMLInputElement>('critical-only').checked = state.criticalOnly;
    const items = urgentAcross(shown, { repo: state.urgentRepo, criticalOnly: state.criticalOnly });
    const SEVERE = /crash|panic|freeze|hang|data loss|corruption|won't|blank|security/;
    $('urgent-list').replaceChildren(...items.map((u) => {
      const a = Math.min(1, u.score / 90) * 0.5;
      return el('li', { class: `urgent-item${u.critical ? ' critical' : ''}`, onclick: () => openUrl(u.url), title: u.url },
        el('span', { class: 'score', style: `background: rgb(var(--hot-rgb) / ${a.toFixed(2)})` }, String(u.score)),
        el('span', { class: 'u-repo' }, appIcon(snap.repos.find((r) => r.repo === u.repo) ?? { repo: u.repo }), shortName(u.repo), thirdPartyTag(u.repo)),
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
    $('refresh').classList.toggle('spinning', !!snap.refreshing && state.view !== 'prs');

    const failing = snap.repos.filter((r) => r.error);
    // A snapshot-wide problem (a broken config, a rate limit) matters on every tab.
    const failures = failing.map((r) => `${shortName(r.repo)} (${r.error})`).join('; ');
    const noun = failing.length > 1 ? 'repos' : 'repo';
    const msgs = [
      snap.error,
      state.view !== 'prs' && failing.length ? `${failing.length} ${noun} failed last poll: ${failures}` : null,
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
    const shown = shownRepos(snap);
    const critical = shown.reduce((n, r) => n + (r.stats?.critical_issues ?? 0), 0);
    $('urgent-count').textContent = critical ? String(critical) : '';
    $('urgent-count').title = `${critical} critical open issues`;
    $('repo-count').textContent = String(shown.length);
  }

  function render(reason: Reason = 'user'): void {
    // Reordering the table mid-drag would move rows under the user's hand;
    // the drop renders anyway.
    const snap = state.snap;
    if (!snap || drag?.moved) return;
    renderStatus(snap);
    if (state.view === 'repos') renderTable(snap, reason);
    else if (state.view === 'urgent') renderUrgent(snap);
  }

  /** What's on screen for the current tab, whether or not there's data yet. */
  function renderTabs(): void {
    const onPrs = state.view === 'prs';
    document.querySelectorAll<HTMLElement>('[data-view]').forEach((b) => b.setAttribute('aria-selected', String(b.dataset.view === state.view)));
    const table = $('repos-view');
    const shown = table.hidden && state.view === 'repos';
    table.hidden = state.view !== 'repos';
    if (shown) resumeFlashes(table.querySelectorAll<HTMLTableCellElement>('td.changed'));
    $('urgent-view').hidden = state.view !== 'urgent';
    $('prs-view').hidden = !onPrs;
    for (const id of ['pr-nav', 'crumbs', 'pr-pill', 'pr-foot']) $(id).hidden = !onPrs;
    for (const id of ['fresh-pill', 'refreshed', 'status-foot']) $(id).hidden = onPrs;
    prs.setActive(onPrs);
  }

  function setView(v: View): void {
    // Choosing the PR tab again goes back to its top, as a second Esc would.
    if (v === 'prs' && state.view === 'prs') prs.home();
    state.view = v;
    // A hidden table gets no pointerleave.
    if (v !== 'repos') hovering = false;
    save('view', v);
    renderTabs();
    render();
  }

  function applyMeta(m: Meta | null): void {
    state.meta = m ?? {};
    document.documentElement.style.setProperty('--opacity', String(state.meta.opacity ?? 0.96));
    $('pin').setAttribute('aria-pressed', String(!!state.meta.always_on_top));
    const mac = navigator.platform.toLowerCase().includes('mac');
    $('shortcut').textContent = (state.meta.shortcut ?? '').replace('CmdOrCtrl', mac ? '⌘' : 'Ctrl').replace('Shift', '⇧').replaceAll('+', '');
    prs.setHighQuality(state.meta.high_quality);
  }

  // ───────────────────────────── wiring ─────────────────────────────

  const prs = startPrs(api, prRoute);
  prs.setHiddenKinds(state.hiddenKinds);

  const kindToggles = REPO_KINDS.map((k) => el('button', {
    class: 'kind',
    'data-kind': k.id,
    onclick: () => {
      state.hiddenKinds = toggleKind(state.hiddenKinds, k.id);
      save('hiddenKinds', state.hiddenKinds);
      renderKinds();
      prs.setHiddenKinds(state.hiddenKinds);
      render();
    },
  }, k.label));
  $('repo-kinds').replaceChildren(...kindToggles);

  function renderKinds(): void {
    REPO_KINDS.forEach((k, i) => {
      const on = !state.hiddenKinds.includes(k.id);
      kindToggles[i]!.setAttribute('aria-pressed', String(on));
      kindToggles[i]!.title = `${on ? 'Hide' : 'Show'} ${k.label.toLowerCase()} on every tab`;
    });
  }
  renderKinds();

  document.querySelectorAll<HTMLElement>('[data-view]').forEach((b) => b.addEventListener('click', () => {
    const view = asView(b.dataset.view ?? null);
    if (view === 'urgent' && state.view !== 'urgent') state.urgentRepo = '';
    setView(view);
  }));
  $('urgent-repo').addEventListener('change', (e) => { state.urgentRepo = (e.target as HTMLSelectElement).value; render(); });
  $('critical-only').addEventListener('change', (e) => {
    state.criticalOnly = (e.target as HTMLInputElement).checked;
    save('criticalOnly', state.criticalOnly);
    render();
  });
  const refresh = (): void => {
    if (state.view === 'prs') prs.refresh();
    else void api.invoke('refresh_now');
  };
  $('refresh').addEventListener('click', refresh);
  $('pin').addEventListener('click', () => api.invoke('set_always_on_top', { on: !state.meta.always_on_top }));
  $('hide').addEventListener('click', () => api.invoke('hide_window'));
  $('edit-config').addEventListener('click', () => api.invoke('open_config'));
  $('resize').addEventListener('mousedown', (e) => { e.preventDefault(); api.startResize(); });
  document.addEventListener('keydown', (e) => {
    const mod = e.metaKey || e.ctrlKey;
    if (state.view === 'prs' && prs.key(e)) e.preventDefault();
    else if (e.key === 'Escape') void api.invoke('hide_window');
    else if (mod && e.key.toLowerCase() === 'r') { e.preventDefault(); refresh(); }
    else if (mod && e.key === '1') { e.preventDefault(); setView('repos'); }
    else if (mod && e.key === '2') { e.preventDefault(); setView('prs'); }
    else if (mod && e.key === '3') { e.preventDefault(); setView('urgent'); }
  });

  // pointerenter and pointerleave fire once for the whole tbody.
  $('tbody').addEventListener('pointerenter', () => { hovering = true; });
  /** Ends a hover and applies the order a snapshot held back. */
  function endHover(): void {
    hovering = false;
    if (pendingOrder) render();
  }
  $('tbody').addEventListener('pointerleave', endHover);
  // With overlay scrollbars, WebKit tracks the pointer only in the focused
  // window, so a pointer that leaves an unfocused window sends no pointerleave.
  window.addEventListener('blur', endHover);
  api.listen<boolean>('craft:focus', (focused) => { if (!focused) endHover(); });
  document.addEventListener('visibilitychange', () => { if (document.hidden) endHover(); });

  // Only a snapshot event highlights cells. The first render (from
  // get_snapshot), the clock, sorting, a window change and a drop do not.
  api.listen<Snapshot>('craft:snapshot', (s) => {
    const prev = new Map((state.snap?.repos ?? []).map((e) => [e.repo, e] as const));
    state.snap = s;
    render('data');
    flashChanges(prev, s);
  });
  api.listen<Meta>('craft:meta', applyMeta);
  // Relative times and staleness change with the clock alone.
  const clockTimer = setInterval(() => render('clock'), 15_000);

  renderTabs();
  (async () => {
    try {
      applyMeta(await api.invoke<Meta>('get_meta'));
      const initial = await api.invoke<Snapshot>('get_snapshot');
      // A craft:snapshot event that arrived first is kept: the host emits one
      // after every change, so anything newer in this reply follows as an event.
      if (!state.snap) {
        state.snap = initial;
        render('data');
      }
    } catch (e) {
      console.error(e);
    } finally {
      // Not requestAnimationFrame: WebKit never fires it while the window is hidden.
      api.invoke('window_ready');
    }
  })();

  return () => {
    clearInterval(clockTimer);
    prs.stop();
  };
}
