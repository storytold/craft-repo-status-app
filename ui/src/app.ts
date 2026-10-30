// Craft Status UI: renders the snapshot pushed by the Rust poller. main.ts
// picks the Tauri API, or a browser stand-in, and calls `start`.

import {
  PARITY_FIGURES, figurePct, formatPct, parityTip, parityUrl,
  AUDIENCES, BASIS_BADGE, GAP_KINDS, NO_GAP_FILTER, PARITY_COLUMNS, PARITY_WARN_DAYS, allGaps, areaStrip, areasByDrag,
  audienceFigure, betaUnknownWhy, blocksLabel, cellValue, changedAt, checklistSummary, daysSince, dimScore, dimsByDrag, docUrl,
  figureSrc, gapKinds, gapTotal, hasParity, heatMeans, heatOf, hoursText, kindSums, nextGapSort, parityBadge,
  parityNextSort, parityOrder, partText, reviewAgeDays, scoreText, sourceUrl, stageOf, sumText, whyMissing,
  COLUMNS, RECENT, WINDOWS, MANUAL, windowById, value, orderedEntries, nextSort, dropIndex,
  freshness, ago, totals, urgentAcross, tokenStatus, isRefreshing, isThirdParty,
} from './model.ts';
import type {
  Column, Meta, RepoEntry, Snapshot, Sort, ParityField,
  Area, Audience, Canonical, Cell, Checklist, Counts, Dim, DocFile, Figure, Gap, GapFilter, Hours, Link, Parity, Src, StageStep,
} from './model.ts';

/** What the UI needs from the host: Tauri commands and events, window chrome. */
export interface Api {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  listen<T>(event: string, fn: (payload: T) => void): void;
  startResize(): void;
}

type View = 'repos' | 'urgent' | 'parity';

interface State {
  snap: Snapshot | null;
  meta: Meta;
  view: View;
  sort: Sort;
  order: string[];
  window: string;
  urgentRepo: string;
  criticalOnly: boolean;
  parityView: ParityView;
  paritySort: Sort;
  audience: Audience;
  gapFilter: GapFilter;
  /** The All gaps sort; null for the default order. */
  gapSort: Sort | null;
  /** The app selected in the Parity tab, and whether its detail pane is open. */
  paritySel: string | null;
  detailOpen: boolean;
  detailBetaOnly: boolean;
}

/** The views a tab button, a shortcut or the saved view can open. */
const TABS: readonly View[] = ['repos', 'urgent', 'parity'];
const tabOf = (v: string | null | undefined): View => TABS.find((t) => t === v) ?? 'repos';

/** The Parity tab's two sub-views. */
type ParityView = 'apps' | 'gaps';

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

/** The App cell's tooltip for a repo with parity numbers, or one not read or not accessible. */
function appReadTip(e: RepoEntry, p: Parity | null | undefined): string {
  if (e.inaccessible) return 'Not accessible';
  return p ? `Read at ${p.commit?.slice(0, 7) ?? 'HEAD'}` : 'Not read yet';
}

/** One line of the Stage chip's tooltip: the next stage, its points and its hours. */
function stepText(st: StageStep): string {
  const points = st.points === null ? '' : `, ${st.points} points`;
  const hours = st.hours ? `, ${hoursText(st.hours)}` : '';
  return `next: ${st.stage}${points}${hours}`;
}

/** A parity document's dates in the Freshness strip; without a date line, why, and its last commit time. */
function fileDates(f: DocFile): string {
  if (f.why) {
    const when = f.commit_date ? `: ${localTime(f.commit_date)}` : '';
    return `${f.why}${when}`;
  }
  return [f.reviewed && `reviewed ${f.reviewed}`, f.updated && `updated ${f.updated}`, f.generated && `generated ${f.generated}`].filter(Boolean).join(' · ');
}

/** A dimension's tooltip: its name, the one table it is in, and whether it is part of the row above. */
function dimTip(d: Dim): string {
  const lines = [d.name];
  if (d.only_in) lines.push(`Only in the ${d.only_in === 'w' ? 'weight' : 'By-dimension'} table`);
  if (d.child) lines.push('Part of the row above; left out of the sums');
  return lines.join('\n');
}

/** A checklist tier's tooltip: its rows by status. */
function tierTip(x: Counts): string {
  const parts = [`${x.done} done`];
  if (x.ahead) parts.push(`${x.ahead} ahead`);
  parts.push(`${x.partial} partial`, `${x.missing} missing`, `${x.excluded} out of scope`);
  if (x.unrecognized) parts.push(`${x.unrecognized} unrecognized`);
  if (x.not_measured) parts.push(`${x.not_measured} not measured`);
  return parts.join(', ');
}

/** The heading of the detail pane's areas table, saying when it is sorted by hours and why. */
function areasHeading(byHours: boolean, why: string | null): string {
  if (!byHours) return 'Areas';
  return why ? `Areas (sorted by hours: ${why})` : 'Areas (sorted by hours)';
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

/** Starts the UI in the current document. Call once; returns a function that stops its clock. */
export function start(api: Api): () => void {
  const state: State = {
    snap: null,
    meta: {},
    view: tabOf(new URLSearchParams(location.search).get('view') ?? load<string>('view', 'repos')),
    sort: load<Sort>('sort', { key: 'urgency', dir: 'desc' }),
    order: load<string[]>('order', []),
    window: windowById(load('window', '')).id,
    urgentRepo: '',
    criticalOnly: load('criticalOnly', false),
    parityView: load<string>('parityView', 'apps') === 'gaps' ? 'gaps' : 'apps',
    paritySort: load<Sort>('paritySort', { key: 'ready', dir: 'desc' }),
    audience: AUDIENCES.find((a) => a.id === load<string>('audience', 'full'))?.id ?? 'full',
    gapFilter: { ...NO_GAP_FILTER, ...load<Partial<GapFilter>>('gapFilter', {}) },
    gapSort: load<Sort | null>('gapSort', null),
    paritySel: null,
    detailOpen: false,
    detailBetaOnly: false,
  };

  /** A Breadth or Ready cell shows the percentage, or a dash whose tooltip
   * gives the reason, with an amber dot when the row exists but did not
   * parse. A click opens docs/target-app-parity.md on GitHub, or the
   * figure's row in ROADMAP.md when the repo has no such document. */
  function parityCell(entry: RepoEntry, field: ParityField): HTMLElement {
    const p = entry.parity;
    const pct = figurePct(p?.[field]);
    const text = pct === null ? '—' : formatPct(pct);
    const title = parityTip(p, field);
    if (!p) return el('span', { class: 'muted', title }, text);
    const f = p[field];
    const url = parityUrl(entry.repo, p, f);
    const cls = f.state === 'ok' ? '' : 'muted';
    const shown = url ? link(text, url, cls) : el('span', { class: 'muted' }, text);
    return el('span', { class: 'parity', title }, f.state === 'unreadable' && el('span', { class: 'warn-dot', 'aria-hidden': 'true' }), shown);
  }

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

  /** A "3P" tag for repos outside our own GitHub org; null for ours. */
  const thirdPartyTag = (repo: string): HTMLElement | null =>
    isThirdParty(repo) ? el('span', { class: 'third-party', title: `Third-party app (${repo.split('/')[0]})` }, '3P') : null;

  /** Cell content for a column. */
  function cell(entry: RepoEntry, col: Column): Node {
    const v = value(entry, col.key, state.window);
    const s = entry.stats;
    const parity = PARITY_FIGURES[col.key];
    if (parity && !entry.inaccessible) return parityCell(entry, parity);
    if (col.key === 'name') {
      const tip = entry.error
        ? `Last attempt failed: ${entry.error}`
        : entry.inaccessible ? 'Not accessible (private, or not found)'
        : entry.fetched_at ? `Fetched ${localTime(entry.fetched_at)}` : 'Not fetched yet';
      return el('span', { class: 'repo', title: tip }, el('span', { class: 'dot' }), appIcon(entry), link(String(v), gh(entry.repo)), thirdPartyTag(entry.repo));
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

  // ───────────────────────────── parity ─────────────────────────────
  // The Parity tab: the Apps table or the All gaps list, and a detail pane
  // for the selected app. Every render draws them again; the header rows,
  // the filters and the pane's open sections, focus and scroll stay.

  /** A link to `src` at the commit `p` was read at, or plain text without a source. */
  function srcLink(repo: string, p: Parity, src: Src | null | undefined, text: string, cls = ''): HTMLElement {
    if (!src) return el('span', { class: cls }, text);
    return link(text, sourceUrl(repo, p.commit, src), cls);
  }

  /** A Doc cell: the linked document at the section it names, or the link's text for one outside the repo. */
  function docCell(repo: string, p: Parity, doc: Link | null): Node | string {
    const url = doc && docUrl(repo, p.commit, doc);
    return url ? link(doc!.text, url) : doc?.text ?? '';
  }

  /** A dash whose tooltip says why the value is missing, with an amber dot when it exists but did not parse. */
  function dash(c: Cell<unknown> | Figure | null | undefined): HTMLElement {
    return el('span', { class: 'muted', title: whyMissing(c) },
      c?.state === 'unreadable' && el('span', { class: 'warn-dot', 'aria-hidden': 'true' }), '—');
  }

  /** A headline percentage, linked to its line, with its basis and source in the tooltip. */
  function figureCell(repo: string, p: Parity, f: Figure): HTMLElement {
    if (f.state !== 'ok') return dash(f);
    const tip = [`${f.label}: ${f.text}`, BASIS_BADGE[f.basis], f.kind, f.from ? `Source: ${f.from}` : ''].filter(Boolean).join('\n');
    const node = srcLink(repo, p, figureSrc(f), formatPct(f.pct));
    node.title = tip;
    return node;
  }

  function hoursCell(repo: string, p: Parity, c: Cell<Hours>): HTMLElement {
    if (c.state !== 'ok') return dash(c);
    const node = srcLink(repo, p, c.src, hoursText(c.v));
    node.title = [c.v.raw, c.v.basis ? `Basis: ${c.v.basis}` : '', c.from ? `Source: ${c.from}` : ''].filter(Boolean).join('\n');
    return node;
  }

  /** The bar of the Ready column: the value, its range as a whisker, and a tick at Breadth. */
  function readyBar(e: RepoEntry, p: Parity): HTMLElement {
    const f = audienceFigure(p, state.audience);
    if (f.state !== 'ok') return dash(f);
    const clamp = (v: number) => Math.max(0, Math.min(100, v));
    const breadth = figurePct(p.breadth);
    const bar = el('span', { class: 'ready-bar', 'aria-hidden': 'true' },
      el('span', { class: 'fill', style: `width: ${clamp(f.pct)}%` }),
      f.low !== null && f.high !== null && el('span', { class: 'range', style: `left: ${clamp(f.low)}%; width: ${clamp(f.high) - clamp(f.low)}%` }),
      breadth !== null && el('span', { class: 'tick', style: `left: ${clamp(breadth)}%` }));
    const text = figureCell(e.repo, p, f);
    if (f.low !== null && f.high !== null) text.title += `\nRange ${f.low}–${f.high}%`;
    if (breadth !== null) text.title += `\nBreadth ${formatPct(breadth)} (the tick)`;
    return el('span', { class: 'ready' }, bar, text);
  }

  /** A heat cell: a single-hue fill that scales with the value, and the value printed. */
  function heatCell(e: RepoEntry, p: Parity, col: Canonical): HTMLElement {
    const h = heatOf(p, col);
    if (!h) return el('span', { class: 'muted', title: 'No dimension row for this column' }, '—');
    const a = h.value === null ? 0 : (h.value / 100) * 0.55;
    return el('span', {
      class: classes('heat', h.text === 'ahead' && 'ahead'),
      style: `background: rgb(var(--heat-rgb) / ${a.toFixed(2)})`,
      title: h.rows.join('\n'),
    }, h.src ? link(h.text, sourceUrl(e.repo, p.commit, h.src)) : h.text);
  }

  /** The area strip: segment widths by weight, filled to ready. */
  function stripCell(p: Parity): HTMLElement {
    const segs = areaStrip(p);
    if (!segs.length) return el('span', { class: 'muted', title: whyMissing(p.areas_total) || 'No feature-area table' }, '—');
    return el('span', { class: 'strip', title: p.area_weights_why ?? '' }, segs.map((s) =>
      el('span', { class: 'seg', style: `width: ${s.width.toFixed(2)}%`, title: s.tip },
        el('span', { class: 'seg-fill', style: `height: ${s.fill === null ? 0 : Math.max(0, Math.min(100, s.fill))}%` }))));
  }

  /** The days after which a review date is marked. */
  const warnDays = (snap: Snapshot): number => snap.parity_warn_days || PARITY_WARN_DAYS;

  const PARITY_GROUP: Readonly<Record<string, string>> = {
    ready: 'complete', breadth: 'complete', to_beta: 'complete', to_full: 'complete',
    top_drag: 'Held back by', beta_gaps: 'Held back by', areas: 'Areas', checklist: 'Checklist', reviewed: 'Fresh', changed: 'Fresh',
  };
  const parityGroup = (key: string): string => PARITY_GROUP[key] ?? (key.startsWith('heat_') ? 'Dimensions' : '');
  const parityGroupStart = (i: number): boolean => i > 0 && parityGroup(PARITY_COLUMNS[i - 1]!.key) !== parityGroup(PARITY_COLUMNS[i]!.key);

  /** The App column's cell: the app's icon and name, titled with why it has no numbers or the commit read. */
  function appNameCell(e: RepoEntry): HTMLElement {
    const p = e.parity;
    const tip = p?.na ?? appReadTip(e, p);
    return el('span', { class: 'repo', title: tip }, appIcon(e), shortName(e.repo), p?.na ? el('span', { class: 'na' }, 'n/a') : null);
  }

  /** The Stage column's chip, titled with the next steps and where the stage was read. */
  function stageCell(e: RepoEntry, p: Parity): HTMLElement {
    const s = stageOf(p);
    if (!s) return dash(p.stage);
    const steps = cellValue(p.stage)?.steps.map(stepText) ?? [];
    const from = p.stage.state === 'ok' && p.stage.from ? [`From ${p.stage.from}`] : [];
    const chip = srcLink(e.repo, p, p.stage.state === 'ok' ? p.stage.src : null, s, classes('stage', s.replace(' ', '-')));
    chip.title = [...steps, ...from].join('\n');
    return chip;
  }

  /** The Top drag column's cell: the dimension that loses the most weighted points. */
  function topDragCell(repo: string, p: Parity): HTMLElement {
    const d = p.top_drag;
    if (d.state !== 'ok') return dash(d);
    const name = d.v.name.split(' (')[0]!;
    return el('span', { title: `${d.v.name}: ${d.v.points} weighted points lost` }, srcLink(repo, p, d.src, `${name} −${d.v.points.toFixed(1)}`));
  }

  /** The Beta gaps column's cell: the open gaps that block beta and their hours. */
  function betaGapsCell(p: Parity): HTMLElement {
    const t = p.gap_totals;
    if (t.beta_unknown) return el('span', { class: 'muted', title: betaUnknownWhy(p) }, '—');
    const b = t.beta;
    const part = t.partial ? `\n+ ${partText(t.partial)}` : '';
    return el('span', { title: `Open gaps that block beta${part}` }, b ? `${b.count} · ${sumText(b)}` : '0');
  }

  /** The Checklist column's cell. */
  function checklistCell(p: Parity): HTMLElement {
    const c = checklistSummary(p);
    return el('span', { class: c.text === '—' ? 'muted' : '', title: c.title }, c.text);
  }

  /** The Reviewed column's cell: the age of the oldest Last reviewed date, marked when it is old. */
  function reviewedCell(p: Parity, snap: Snapshot, now: number): HTMLElement {
    const age = reviewAgeDays(p, now);
    const dates = p.files.filter((f) => ['ROADMAP.md', 'docs/target-app-parity.md', 'docs/gaps.md'].includes(f.path))
      .map((f) => `${f.path}: reviewed ${f.reviewed ?? '—'}, updated ${f.updated ?? '—'}`);
    if (age === null) return el('span', { class: 'muted', title: dates.join('\n') || 'No Last reviewed date' }, '—');
    return el('span', { class: age > warnDays(snap) ? 'old' : '', title: [`Oldest Last reviewed: ${p.reviewed}`, ...dates].join('\n') }, `${age}d`);
  }

  /** The Changed column's cell: the time since the last commit to a parity document. */
  function changedCell(p: Parity, now: number): HTMLElement {
    const t = changedAt(p);
    return el('span', { class: t ? '' : 'muted', title: t ? `Last commit to a parity document: ${localTime(t)}` : 'Last commits not read' }, t ? ago(t, now) : '—');
  }

  /** An Apps row's cell for column `key`. */
  function appCell(e: RepoEntry, key: string, snap: Snapshot, now: number): Node {
    const p = e.parity;
    if (key === 'app') return appNameCell(e);
    if (!p || e.inaccessible) return el('span', { class: 'muted', title: e.inaccessible ? 'Not accessible' : 'Not read yet' }, '—');
    if (key === 'stage') return stageCell(e, p);
    if (p.na) return el('span', { class: 'muted', title: p.na }, '');
    switch (key) {
      case 'ready': return readyBar(e, p);
      case 'breadth': return figureCell(e.repo, p, p.breadth);
      case 'to_beta': return hoursCell(e.repo, p, p.to_beta);
      case 'to_full': return hoursCell(e.repo, p, p.to_full);
      case 'top_drag': return topDragCell(e.repo, p);
      case 'beta_gaps': return betaGapsCell(p);
      case 'areas': return stripCell(p);
      case 'checklist': return checklistCell(p);
      case 'reviewed': return reviewedCell(p, snap, now);
      case 'changed': return changedCell(p, now);
    }
    if (key.startsWith('heat_')) return heatCell(e, p, key.slice(5) as Canonical);
    return document.createTextNode('');
  }

  /** The Apps table and its header rows, built once and updated in place, as
   * the Repos table's group row is, so a render does not close the audience
   * dropdown. */
  let parityTable: HTMLTableElement | null = null;

  function parityApps(): HTMLTableElement {
    if (!parityTable) {
      const groups = el('tr', { class: 'groups' });
      let prev: string | null = null;
      for (const c of PARITY_COLUMNS) {
        const g = parityGroup(c.key);
        if (g === prev) { (groups.lastChild as HTMLTableCellElement).colSpan += 1; continue; }
        prev = g;
        groups.append(el('th', { class: g ? 'group-start' : '' }, g === 'complete' ? audiencePicker() : g));
      }
      const cols = el('tr', { class: 'cols' }, PARITY_COLUMNS.map((c) => el('th', {
        title: c.title,
        onclick: () => { state.paritySort = parityNextSort(state.paritySort, c.key); save('paritySort', state.paritySort); render(); },
      }, c.label)));
      parityTable = el('table', { id: 'parity-table', class: 'parity-table' }, el('thead', {}, groups, cols), el('tbody'), el('tfoot'));
    }
    const picker = parityTable.querySelector<HTMLSelectElement>('.window-picker')!;
    if (picker.value !== state.audience) picker.value = state.audience;
    const cols = parityTable.querySelector<HTMLTableRowElement>('thead tr.cols')!;
    PARITY_COLUMNS.forEach((c, i) => {
      const sorted = state.paritySort.key === c.key;
      cols.cells[i]!.className = classes(sorted && 'sorted', sorted && state.paritySort.dir === 'asc' && 'asc', parityGroupStart(i) && 'group-start');
    });
    return parityTable;
  }

  function renderParityApps(snap: Snapshot): void {
    const now = Date.now();
    const rows = parityOrder(snap.repos, state.paritySort, state.audience);
    if (!state.paritySel || !rows.some((e) => e.repo === state.paritySel)) state.paritySel = rows.find(hasParity)?.repo ?? null;
    const body = el('tbody', { id: 'parity-rows' }, rows.map((e) => el('tr', {
      dataset: { repo: e.repo },
      class: classes(!hasParity(e) && 'na-row', e.repo === state.paritySel && 'selected'),
      'aria-selected': String(e.repo === state.paritySel),
      onclick: () => selectApp(e.repo),
    }, PARITY_COLUMNS.map((c, i) => el('td', { class: classes(parityGroupStart(i) && 'group-start', c.key.startsWith('heat_') && 'heat-td') }, appCell(e, c.key, snap, now))))));
    const means = heatMeans(snap.repos);
    const foot = el('tr', {}, PARITY_COLUMNS.map((c, i) => {
      let text = '';
      let title = '';
      if (i === 0) text = `${snap.repos.filter(hasParity).length} of ${snap.repos.length} apps`;
      const m = c.key.startsWith('heat_') ? means[c.key.slice(5)] : undefined;
      if (m && m.mean !== null) { text = `${formatPct(m.mean)} (n=${m.n})`; title = `Mean across ${m.n} apps`; }
      return el('td', { class: parityGroupStart(i) ? 'group-start' : '', title }, text);
    }));
    if (!rows.length) body.append(el('tr', {}, el('td', { colspan: PARITY_COLUMNS.length, class: 'empty' }, 'No repositories configured.')));
    const table = parityApps();
    table.tBodies[0]!.replaceWith(body);
    table.tFoot!.replaceChildren(foot);
    // Only the body and footer change: the header, and the dropdown in it, stay.
    if (table.parentNode !== $('parity-main')) $('parity-main').replaceChildren(table);
    $('parity-filters').replaceChildren();
  }

  function audiencePicker(): HTMLSelectElement {
    const sel = el('select', {
      class: 'window-picker',
      'aria-label': 'Audience for the Ready column',
      title: 'Whose readiness the Ready column shows',
      onchange: (ev: Event) => { state.audience = (ev.target as HTMLSelectElement).value as Audience; save('audience', state.audience); render(); },
    }, AUDIENCES.map((a) => el('option', { value: a.id }, a.label)));
    sel.value = state.audience;
    return sel;
  }

  /** Selects `repo` in the Apps table and opens its detail pane. */
  function selectApp(repo: string): void {
    state.paritySel = repo;
    state.detailOpen = true;
    render();
  }

  /** The All gaps filters, built once so typing in them keeps the focus. */
  let gapControls: HTMLElement | null = null;

  function gapFilters(snap: Snapshot): HTMLElement {
    const setFilter = (patch: Partial<GapFilter>) => { state.gapFilter = { ...state.gapFilter, ...patch }; save('gapFilter', state.gapFilter); render(); };
    gapControls ??= el('span', { class: 'gap-filters' },
      el('select', { id: 'gap-repo', 'aria-label': 'App', onchange: (ev: Event) => setFilter({ repo: (ev.target as HTMLSelectElement).value }) }),
      el('select', { id: 'gap-kind', 'aria-label': 'Kind', onchange: (ev: Event) => setFilter({ kind: (ev.target as HTMLSelectElement).value }) },
        el('option', { value: '' }, 'All kinds'), [...GAP_KINDS, 'other'].map((k) => el('option', { value: k }, k))),
      el('label', { class: 'toggle' }, el('input', { type: 'checkbox', id: 'gap-beta', onchange: (ev: Event) => setFilter({ betaOnly: (ev.target as HTMLInputElement).checked }) }), 'Blocks beta'),
      el('label', { class: 'toggle' }, el('input', { type: 'checkbox', id: 'gap-closed', onchange: (ev: Event) => setFilter({ hideClosed: (ev.target as HTMLInputElement).checked }) }), 'Hide closed'),
      el('label', { class: 'toggle' }, 'Min hours', el('input', { type: 'number', id: 'gap-hours', min: 0, step: 10, 'aria-label': 'Minimum hours',
        oninput: (ev: Event) => setFilter({ minHours: Number((ev.target as HTMLInputElement).value) || 0 }) })),
      el('input', { type: 'search', id: 'gap-text', placeholder: 'Filter text', 'aria-label': 'Filter text', spellcheck: 'false',
        oninput: (ev: Event) => setFilter({ text: (ev.target as HTMLInputElement).value }) }));
    // A filter for a repo no longer configured is cleared; one whose report
    // is not read yet keeps its option.
    if (state.gapFilter.repo && !snap.repos.some((e) => e.repo === state.gapFilter.repo)) {
      state.gapFilter = { ...state.gapFilter, repo: '' };
      save('gapFilter', state.gapFilter);
    }
    const f = state.gapFilter;
    const repoSel = gapControls.querySelector<HTMLSelectElement>('#gap-repo')!;
    const repos = snap.repos.filter((e) => hasParity(e) || e.repo === f.repo).map((e) => e.repo);
    if (repoSel.dataset.repos !== repos.join('\n')) {
      repoSel.replaceChildren(el('option', { value: '' }, 'All apps'), ...repos.map((r) => el('option', { value: r }, shortName(r))));
      repoSel.dataset.repos = repos.join('\n');
    }
    if (repoSel.value !== f.repo) repoSel.value = f.repo;
    const kindSel = gapControls.querySelector<HTMLSelectElement>('#gap-kind')!;
    if (kindSel.value !== f.kind) kindSel.value = f.kind;
    gapControls.querySelector<HTMLInputElement>('#gap-beta')!.checked = f.betaOnly;
    gapControls.querySelector<HTMLInputElement>('#gap-closed')!.checked = f.hideClosed;
    const hours = gapControls.querySelector<HTMLInputElement>('#gap-hours')!;
    if (document.activeElement !== hours) hours.value = f.minHours ? String(f.minHours) : '';
    const text = gapControls.querySelector<HTMLInputElement>('#gap-text')!;
    if (document.activeElement !== text) text.value = f.text;
    return gapControls;
  }

  const GAP_COLUMNS: readonly { key: string; label: string }[] = [
    { key: 'app', label: 'App' }, { key: 'rank', label: 'Rank' }, { key: 'id', label: 'Id' }, { key: 'gap', label: 'Gap' },
    { key: 'blocks', label: 'Blocks' }, { key: 'kind', label: 'Kind' }, { key: 'hours', label: 'Hours' }, { key: 'doc', label: 'Doc' },
    { key: 'reviewed', label: 'Reviewed' },
  ];

  /** The flag of a gap's Blocks cell. */
  function flagCell(g: Gap): HTMLElement {
    const b = blocksLabel(g);
    return el('td', { title: b.title }, b.text ? el('span', { class: classes('flag', b.text.toLowerCase().replace(' ', '-')) }, b.text) : '');
  }

  /** A gap's Hours cell: struck through when the hours are counted inside another gap. */
  function gapHoursCell(g: Gap): HTMLElement {
    const h = cellValue(g.hours);
    return el('td', { title: h ? h.raw : whyMissing(g.hours) },
      g.counted_in ? el('s', { title: `Counted in ${g.counted_in}` }, hoursText(h)) : hoursText(h));
  }

  function renderAllGaps(snap: Snapshot): void {
    const filters = gapFilters(snap);
    if (filters.parentNode !== $('parity-filters')) $('parity-filters').replaceChildren(filters);
    const gaps = allGaps(snap.repos, state.gapFilter, state.gapSort);
    const sums = kindSums(allGaps(snap.repos, { ...state.gapFilter, kind: '' }));
    const summary = el('div', { class: 'kind-sums', id: 'kind-sums' },
      el('span', { class: 'kind-total' }, `${gaps.length} gaps · ${sumText(gapTotal(gaps))}`),
      sums.map((s) => el('button', {
        class: classes('kind-chip', state.gapFilter.kind === s.kind && 'on'),
        title: `${s.count} gaps in ${s.apps} apps, ${sumText(s)}; click to filter`,
        onclick: () => { state.gapFilter = { ...state.gapFilter, kind: state.gapFilter.kind === s.kind ? '' : s.kind }; save('gapFilter', state.gapFilter); render(); },
      }, `${s.kind}: ${s.count} · ${sumText(s)}`)));
    const head = el('tr', { class: 'cols' }, GAP_COLUMNS.map((c) => {
      const sorted = state.gapSort?.key === c.key;
      return el('th', {
        class: classes(sorted && 'sorted', sorted && state.gapSort?.dir === 'asc' && 'asc'),
        title: c.key === 'blocks' ? 'Default order: alpha blockers, beta blockers, partial blockers, the rest' : `Sort by ${c.label}`,
        onclick: () => { state.gapSort = nextGapSort(state.gapSort, c.key); save('gapSort', state.gapSort); render(); },
      }, c.label);
    }));
    const now = Date.now();
    const body = el('tbody', {}, gaps.map((g) => {
      const p = snap.repos.find((e) => e.repo === g.repo)!.parity!;
      const age = daysSince(g.reviewed, now);
      return el('tr', { class: classes(g.closed && 'closed'), dataset: { repo: g.repo } },
        el('td', {}, shortName(g.repo)),
        el('td', {}, g.rank ? String(g.rank) : el('span', { class: 'muted', title: g.group ?? '' }, '·')),
        el('td', {}, g.id),
        el('td', { class: 'gap-title', title: g.detail ?? '' }, srcLink(g.repo, p, g.detail_src ?? g.src, g.title),
          g.paired_with.length ? el('span', { class: 'muted' }, ` (also ${g.paired_with.join(', ')})`) : null),
        flagCell(g),
        el('td', { class: 'muted' }, gapKinds(g).join(', ')),
        gapHoursCell(g),
        el('td', {}, docCell(g.repo, p, g.doc)),
        el('td', { class: age !== null && age > warnDays(snap) ? 'old' : 'muted' }, age === null ? '—' : `${age}d`));
    }));
    if (!gaps.length) body.append(el('tr', {}, el('td', { colspan: GAP_COLUMNS.length, class: 'empty' }, 'No gaps match.')));
    $('parity-main').replaceChildren(summary, el('table', { class: 'gaps-table', id: 'gaps-table' }, el('thead', {}, head), body));
  }

  /** One section of the detail pane. */
  const section = (title: string, ...kids: (Node | null | false | undefined)[]): HTMLElement =>
    el('section', { class: 'detail-section' }, el('h3', {}, title), ...kids);

  /**
   * Fills the detail pane with `kids` for `repo`. Every render draws the pane
   * again, so for the same repo the sections the user expanded stay open, the
   * element with the keyboard focus gets it back, and the scroll position
   * stays. Elements are matched by `data-key`; a summary by its section's key.
   */
  function replaceDetail(pane: HTMLElement, repo: string, ...kids: (Node | null | false | undefined)[]): void {
    const same = pane.dataset.repo === repo;
    const open = new Set(same ? [...pane.querySelectorAll<HTMLElement>('details[open][data-key]')].map((d) => d.dataset.key) : []);
    const focused = document.activeElement as HTMLElement | null;
    const active = focused && pane.contains(focused) ? focused : null;
    const focusKey = active?.tagName === 'SUMMARY' ? `${active.closest<HTMLElement>('details')?.dataset.key ?? ''} summary` : active?.dataset.key;
    const top = pane.scrollTop;
    pane.replaceChildren(...kids.filter((k): k is Node => !!k));
    pane.dataset.repo = repo;
    if (!same) return;
    pane.querySelectorAll<HTMLElement>('details[data-key]').forEach((d) => { if (open.has(d.dataset.key)) d.setAttribute('open', ''); });
    if (focusKey) {
      const summary = focusKey.endsWith(' summary');
      const key = summary ? focusKey.slice(0, -' summary'.length) : focusKey;
      const owner = [...pane.querySelectorAll<HTMLElement>('[data-key]')].find((n) => n.dataset.key === key);
      const target = summary ? owner?.querySelector<HTMLElement>(':scope > summary') : owner;
      target?.focus({ preventScroll: true });
    }
    pane.scrollTop = top;
  }

  function renderDetail(snap: Snapshot): void {
    const pane = $('parity-detail');
    const e = snap.repos.find((r) => r.repo === state.paritySel);
    pane.hidden = !state.detailOpen || !e || state.parityView !== 'apps';
    $('parity-view').classList.toggle('with-detail', !pane.hidden);
    if (pane.hidden || !e) return;
    const p = e.parity;
    const close = el('button', { class: 'icon-button small detail-close', title: 'Close (Esc)', 'aria-label': 'Close the detail pane',
      dataset: { key: 'close' }, onclick: () => { state.detailOpen = false; render(); } }, '✕');
    if (!p || p.na || e.inaccessible) {
      replaceDetail(pane, e.repo, el('div', { class: 'detail-head' }, appIcon(e), el('h2', {}, shortName(e.repo)), close),
        el('p', { class: 'muted' }, e.inaccessible ? 'Not accessible (private, or not found).' : p?.na ?? 'Not read yet.'));
      return;
    }
    const repo = e.repo;
    const stage = stageOf(p);
    const target = p.files.find((f) => f.target)?.target;
    const head = el('div', { class: 'detail-head' }, appIcon(e), el('h2', {}, shortName(repo)),
      stage && el('span', { class: classes('stage', stage.replace(' ', '-')) }, stage),
      target && el('span', { class: 'muted target' }, `vs ${target}`), close);
    const { dims, byHours } = dimsByDrag(p.dims);
    const ar = areasByDrag(p.areas);
    const areas = ar.areas.length ? areaTable(repo, p, ar.areas) : null;
    const checks = p.checklists.map((c) => checklistBlock(repo, p, c));

    replaceDetail(pane, repo, head, detailNumbers(repo, p),
      section('Freshness', el('div', { class: 'fresh-strip' }, p.files.map((f) => freshChip(repo, p, f)))),
      section(byHours ? 'Holding it back (sorted by hours: no weights in source)' : 'Holding it back', dimTable(repo, p, dims),
        areas && el('h4', {}, areasHeading(ar.byHours, p.area_weights_why)), areas,
        areas && el('p', { class: 'muted small' }, 'The Missing chips are read from the area’s gaps text; the text itself is under each row.')),
      gapSection(repo, p),
      section('Checklists', ...(checks.length ? checks : [el('p', { class: 'muted' }, 'No machine checklist in this repo.')]), checklistDocs(repo, p)),
      section('Parser notes', parserNotes(repo, p)));
  }

  /** The detail pane's headline numbers: the readiness figures and the hours to each stage. */
  function detailNumbers(repo: string, p: Parity): HTMLElement {
    const number = (label: string, f: Figure) => el('div', { class: 'number' }, el('span', { class: 'label' }, label), figureCell(repo, p, f),
      f.state === 'ok' && f.low !== null && f.high !== null && el('span', { class: 'range-text' }, `${f.low}–${f.high}%`),
      f.state === 'ok' && BASIS_BADGE[f.basis] && el('span', { class: `basis ${f.basis}` }, BASIS_BADGE[f.basis]));
    const hoursNumber = (label: string, c: Cell<Hours>) =>
      el('div', { class: 'number' }, el('span', { class: 'label' }, label), hoursCell(repo, p, c));
    return el('div', { class: 'numbers' },
      number('Breadth', p.breadth), number('Ready', p.ready), number('Mainstream', p.mainstream), number('Essentials', p.essentials),
      hoursNumber('To alpha', p.to_alpha), hoursNumber('To beta', p.to_beta), hoursNumber('To full', p.to_full));
  }

  /** One parity document's chip in the Freshness strip. */
  function freshChip(repo: string, p: Parity, f: DocFile): HTMLElement {
    const name = f.path.split('/').pop() ?? f.path;
    const dates = fileDates(f);
    const tip = [f.path, f.date_lines > 1 ? `${f.date_lines} date lines` : '', f.commit_date ? `Last commit ${localTime(f.commit_date)}` : 'Last commit not read']
      .filter(Boolean).join('\n');
    return el('span', { class: classes('fresh-chip', !!f.why && 'nodate'), title: tip },
      srcLink(repo, p, { path: f.path, line: f.line ?? 0 }, name), el('span', { class: 'muted' }, ` ${dates || '—'}`));
  }

  /** The dimensions table, or why there is none. */
  function dimTable(repo: string, p: Parity, dims: readonly Dim[]): HTMLElement {
    if (!dims.length) return el('p', { class: 'muted' }, whyMissing(p.dims_total));
    return el('table', { class: 'detail-table' },
      el('thead', {}, el('tr', {}, ['Dimension', 'Weight', 'Ready', 'Lost', 'Hours', 'Doc'].map((t) => el('th', {}, t)))),
      el('tbody', {}, dims.map((d) => el('tr', { class: d.child ? 'child muted' : '' },
        el('td', { title: dimTip(d) }, d.child ? '↳ ' : '', srcLink(repo, p, d.src, d.name.split(' (')[0]!)),
        el('td', { title: d.weight_src ?? '' }, d.weight === null ? el('span', { class: 'muted', title: 'Not weighted' }, '—') : String(d.weight)),
        el('td', {}, d.fills === 'breadth' ? el('span', { title: 'Breadth row' }, `${scoreText(dimScore(d))} breadth`) : scoreText(d.ready)),
        el('td', {}, d.shortfall === null ? '' : `−${d.shortfall.toFixed(1)}`),
        el('td', { title: d.hours.state === 'ok' ? d.hours.v.raw : whyMissing(d.hours) }, hoursText(cellValue(d.hours))),
        el('td', {}, docCell(repo, p, d.doc))))));
  }

  /** The feature areas table, each row with its missing features and the gaps text under them. */
  function areaTable(repo: string, p: Parity, areas: readonly Area[]): HTMLElement {
    return el('table', { class: 'detail-table areas' },
      el('thead', {}, el('tr', {}, ['Area', 'Weight', 'Breadth', 'Ready', 'Hours', 'Missing'].map((t) => el('th', {}, t)))),
      el('tbody', {}, areas.map((a) => el('tr', { class: a.unweighted ? 'muted' : '' },
        el('td', { title: a.full_name }, srcLink(repo, p, a.src, a.name), a.conflict && el('span', { class: 'warn', title: a.conflict }, ' ⚠'),
          a.unweighted && el('span', { class: 'na' }, 'unweighted')),
        el('td', { title: a.weight_src ?? p.area_weights_why ?? '' }, a.weight === null ? '—' : String(a.weight)),
        el('td', {}, scoreText(a.breadth)),
        el('td', { title: a.ready_from ? `From ${a.ready_from}` : '' }, scoreText(a.ready)),
        el('td', {}, hoursText(cellValue(a.hours))),
        el('td', {}, el('details', { dataset: { key: `area/${a.key}` } }, el('summary', { title: a.gaps_header ?? '' },
          a.missing.map((c) => el('span', { class: 'chip', title: c.hover ?? '' }, c.text)),
          a.more ? el('span', { class: 'chip more' }, `+${a.more} more`) : null,
          !a.missing.length && el('span', { class: 'muted' }, a.raw_gaps ? 'show text' : '—')),
          el('p', { class: 'raw' }, a.raw_gaps || '(empty)')))))));
  }

  /** The sums under the ranked gaps, or why the beta blockers are unknown; null when the report has no gaps. */
  function gapTotalsLine(p: Parity): HTMLElement | null {
    if (!p.gaps.length) return null;
    const t = p.gap_totals;
    if (t.beta_unknown) return el('p', { class: 'gap-totals muted' }, `${betaUnknownWhy(p)}.`);
    return el('p', { class: 'gap-totals' },
      t.beta ? `Σ beta gaps ${sumText(t.beta)} (${t.beta.count})` : 'No beta gaps',
      t.partial ? ` + ${partText(t.partial)}` : '',
      t.stated.state === 'ok' ? `; docs/gaps.md says ${hoursText(t.stated.v)}` : '',
      p.to_beta.state === 'ok' ? `; ROADMAP says ${hoursText(p.to_beta.v)} to beta` : '');
  }

  /** The Ranked gaps section: the ranked table, the sums, and a table for each group of unranked gaps. */
  function gapSection(repo: string, p: Parity): HTMLElement {
    const gaps = p.gaps.filter((g) => !state.detailBetaOnly || g.beta === 'yes');
    const ranked = gaps.filter((g) => g.rank > 0);
    const unranked = gaps.filter((g) => g.rank === 0);
    const gapRow = (g: Gap) => el('tr', { class: g.closed ? 'closed' : '' },
      el('td', {}, g.rank ? String(g.rank) : ''), el('td', {}, g.id),
      el('td', { title: g.detail ?? '' }, srcLink(repo, p, g.detail_src ?? g.src, g.title)),
      flagCell(g),
      el('td', { class: 'muted' }, gapKinds(g).join(', ')),
      gapHoursCell(g),
      el('td', {}, docCell(repo, p, g.doc)));
    const gapHead = el('thead', {}, el('tr', {}, ['#', 'Id', 'Gap', 'Blocks', 'Kind', 'Hours', 'Doc'].map((t) => el('th', {}, t))));
    const groups = [...new Set(unranked.map((g) => g.group ?? ''))];
    const rankedTable = ranked.length ? el('table', { class: 'detail-table gaps' }, gapHead, el('tbody', {}, ranked.map(gapRow))) : null;
    const empty = !gaps.length && el('p', { class: 'muted' }, p.gaps.length ? 'None match.' : betaUnknownWhy(p));
    return section('Ranked gaps',
      el('label', { class: 'toggle' }, el('input', { type: 'checkbox', id: 'detail-beta', dataset: { key: 'beta-only' }, checked: state.detailBetaOnly ? 'checked' : null,
        onchange: (ev: Event) => { state.detailBetaOnly = (ev.target as HTMLInputElement).checked; render(); } }), 'Beta blockers only'),
      rankedTable,
      empty,
      gapTotalsLine(p),
      ...groups.map((grp) => el('div', { class: 'unranked' }, el('h4', {}, grp || 'Unranked'),
        el('table', { class: 'detail-table gaps' }, el('tbody', {}, unranked.filter((g) => (g.group ?? '') === grp).map(gapRow))))));
  }

  /** One machine checklist: its total and missing rows by group, or its tiers or groups and the rows not done. */
  function checklistBlock(repo: string, p: Parity, c: Checklist): HTMLElement {
    const title = srcLink(repo, p, { path: c.path, line: 0 }, c.path);
    if (c.kind === 'presence') {
      const tot = c.total.state === 'ok' ? `${c.total.v.done} of ${c.total.v.total}` : whyMissing(c.total);
      const byGroup = new Map<string, typeof c.missing>();
      for (const m of c.missing) byGroup.set(m.group, [...(byGroup.get(m.group) ?? []), m]);
      return el('div', { class: 'checklist' }, el('h4', {}, title, ` · ${tot}`),
        [...byGroup].map(([g, rows]) => el('details', { dataset: { key: `presence/${c.path}/${g}` } }, el('summary', {}, `${g || 'Missing'} (${rows.length})`),
          el('ul', {}, rows.map((r) => el('li', {}, srcLink(repo, p, { path: c.path, line: r.line }, r.text)))))),
        c.more ? el('p', { class: 'muted' }, `+${c.more} more`) : null);
    }
    const tiers = c.kind === 'tiered' ? c.tiers : c.groups;
    const more = c.more ? `, +${c.more} more` : '';
    return el('div', { class: 'checklist' }, el('h4', {}, title, c.overall !== null ? ` · ${formatPct(c.overall)}` : ''),
      tiers.map((x) => {
        const n = x.done + x.ahead + x.partial + x.missing + x.excluded || 1;
        return el('div', { class: 'tier', title: tierTip(x) },
          el('span', { class: 'tier-name' }, x.name),
          el('span', { class: 'stack' }, (['done', 'ahead', 'partial', 'missing', 'excluded'] as const).map((k) =>
            el('span', { class: `part ${k}`, style: `width: ${((x[k] / n) * 100).toFixed(1)}%` }))),
          el('span', { class: 'tier-pct' }, x.pct === null ? '—' : formatPct(x.pct)));
      }),
      c.rows.length ? el('details', { dataset: { key: `rows/${c.path}` } }, el('summary', {}, `Partial and missing rows (${c.rows.length}${more})`),
        el('ul', {}, c.rows.map((r) => el('li', {}, el('span', { class: `row-status ${r.status.replace(' ', '-')}` }, r.status), ' ',
          el('span', { class: 'muted' }, r.group), ' ', srcLink(repo, p, { path: c.path, line: r.line }, r.text),
          r.notes && el('span', { class: 'muted' }, ` · ${r.notes}`))))) : null);
  }

  /** The hand-written parity documents, each with its score, counts, dates and rows; null when there are none. */
  function checklistDocs(repo: string, p: Parity): HTMLElement | null {
    if (!p.docs.length) return null;
    return el('div', { class: 'checklist-docs' }, el('h4', {}, 'Checklist docs'),
      el('ul', {}, p.docs.map((d) => {
        const f = p.files.find((x) => x.path === d.path);
        const counts = d.counts ? `${d.counts.done} done · ${d.counts.partial} partial · ${d.counts.missing} missing` : '';
        const rows = [...d.rows, ...d.pct_rows];
        return el('li', {}, srcLink(repo, p, { path: d.path, line: 0 }, d.path.split('/').pop() ?? d.path),
          d.score.state === 'ok' ? ` · ${formatPct(d.score.v.value)}` : '',
          counts && el('span', {}, ` · ${counts}`), d.pct_rows.length ? el('span', {}, ` · ${d.pct_rows.length} rows with a %`) : null,
          d.why && el('span', { class: 'muted' }, ` · ${d.why}`),
          f && el('span', { class: 'muted' }, ` · reviewed ${f.reviewed ?? '—'}, updated ${f.updated ?? '—'}`),
          rows.length ? el('details', { dataset: { key: `docrows/${d.path}` } }, el('summary', {}, 'Rows'),
            el('ul', {}, rows.map((r) => el('li', {}, el('span', { class: 'row-status' }, r.status), ' ',
              srcLink(repo, p, { path: d.path, line: r.line }, r.text))))) : null);
      })));
  }

  /** The parser's notes about the documents, or "None.". */
  function parserNotes(repo: string, p: Parity): HTMLElement {
    if (!p.notes.length) return el('p', { class: 'muted' }, 'None.');
    return el('ul', { class: 'notes' }, p.notes.map((n) => el('li', { class: n.level },
      n.src ? srcLink(repo, p, n.src, n.text) : n.text, n.src ? el('span', { class: 'muted' }, ` ${n.src.path}:${n.src.line}`) : null)));
  }

  function renderParity(snap: Snapshot): void {
    document.querySelectorAll<HTMLElement>('#parity-view .seg').forEach((b) => b.setAttribute('aria-selected', String(b.dataset.pview === state.parityView)));
    if (state.parityView === 'apps') renderParityApps(snap);
    else renderAllGaps(snap);
    renderDetail(snap);
  }

  /** Moves the Apps selection by `step` rows. */
  function moveSelection(step: number): void {
    const snap = state.snap;
    if (!snap) return;
    const rows = parityOrder(snap.repos, state.paritySort, state.audience).map((e) => e.repo);
    if (!rows.length) return;
    const i = state.paritySel ? rows.indexOf(state.paritySel) : -1;
    state.paritySel = rows[Math.max(0, Math.min(rows.length - 1, i + step))]!;
    render();
    document.querySelector('#parity-rows tr.selected')?.scrollIntoView?.({ block: 'nearest' });
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
    const blockers = parityBadge(snap.repos);
    $('parity-count').textContent = blockers ? String(blockers) : '';
    $('parity-count').title = `${blockers} open gaps that block alpha or beta`;
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
    $('parity-view').hidden = state.view !== 'parity';
    if (state.view === 'repos') renderTable(snap);
    else if (state.view === 'urgent') renderUrgent(snap);
    else renderParity(snap);
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
    const view = tabOf(b.dataset.view);
    if (view === 'urgent' && state.view !== 'urgent') state.urgentRepo = '';
    setView(view);
  }));
  document.querySelectorAll<HTMLElement>('#parity-view .seg').forEach((b) => b.addEventListener('click', () => {
    state.parityView = b.dataset.pview === 'gaps' ? 'gaps' : 'apps';
    save('parityView', state.parityView);
    render();
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
  // The Apps view's keys, ahead of the window's: the arrows move the
  // selection, Enter opens its detail pane and Esc closes the pane before it
  // hides the window. Enter on a focused button, link or summary activates
  // that instead.
  document.addEventListener('keydown', (e) => {
    if (state.view !== 'parity' || state.parityView !== 'apps' || e.metaKey || e.ctrlKey) return;
    const target = e.target as Element | null;
    if (e.key === 'Escape' && state.detailOpen) { e.stopImmediatePropagation(); state.detailOpen = false; render(); }
    else if (target?.closest?.('input, select, textarea')) return;
    else if (e.key === 'ArrowDown') { e.preventDefault(); moveSelection(1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); moveSelection(-1); }
    else if (e.key === 'Enter' && state.paritySel && !target?.closest?.('button, a, summary')) { e.preventDefault(); state.detailOpen = true; render(); }
  });
  document.addEventListener('keydown', (e) => {
    const mod = e.metaKey || e.ctrlKey;
    if (e.key === 'Escape') api.invoke('hide_window');
    else if (mod && e.key.toLowerCase() === 'r') { e.preventDefault(); api.invoke('refresh_now'); }
    else if (mod && e.key === '1') setView('repos');
    else if (mod && e.key === '2') setView('urgent');
    else if (mod && e.key === '3') setView('parity');
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
