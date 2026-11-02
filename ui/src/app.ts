// Craft Status UI: the window's chrome and tabs. Downloads and Fix first render the
// snapshot pushed by the Rust status poller; the PR tab is pr-app.ts. main.ts picks
// the Tauri API, or a browser stand-in, and calls `start`.

import {
  ALL_TIME, COLUMNS, RECENT, WINDOWS, MANUAL, windowById, value, orderedEntries, nextSort, dropIndex,
  freshness, ago, formatRate, totals, urgentAcross, tokenStatus, isRefreshing, isThirdParty,
  REPO_KINDS, isRepoHidden, toggleKind, manualOrder, reorderVisible,
  NOT_READ_YET, PEOPLE_COLUMNS, ROLE_LABELS, roleHeading, peopleRows, personFigures, personRepos, nextPeopleSort, repoItems,
  stateBreakdown, stateLabel, stateTip, countText, fromTextTip, durationText, waitsOn, heldCount, cachedText, recordsAge,
  RANK_MODES, RANK_COLUMNS, TYPES, PRODUCT, rankRows, nextRankSort, rankSortFor, linesText, guessTip, batchText,
  BATCH_TIP, UNVERIFIED_TIP, DIRECT_MISSING_TIP, RELEASE_TIP, TIME_NOTE, workLead, repoFigures, prRefs,
  searchWords, matchesSearch, personSearchFields, personMatches, matchText, peopleGroups, groupTotals, GROUP_TOTALS_TIP,
  rankRowsOf, rankTotal, RANK_TOTALS_TIP,
} from './model.ts';
import type {
  Column, Meta, RepoEntry, Snapshot, Sort,
  DetailRow, Flow, Given, Item, MainChecks, PeopleView, PersonDetail, PersonFigures, PersonResponse, PersonRow, Rank, RankColumn, RankMode, RankRow,
  RepoFigures, RepoFlow, RepoPeople,
  StateTime, WorkState,
} from './model.ts';
import { startPrs } from './pr-app.ts';
import type { Route } from './pr-app.ts';
import { $, DAY, classes, el, load, localTime, save } from './dom.ts';
import type { Api } from './dom.ts';

export type { Api } from './dom.ts';

type View = 'repos' | 'prs' | 'urgent' | 'people';

/** The view a tab button, a shortcut or the saved view names; the Downloads table for anything else. */
const asView = (v: string | null | undefined): View => (v === 'urgent' || v === 'prs' || v === 'people' ? v : 'repos');

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
  /** The People tab's view from the host; null until the records are read. */
  people: PeopleView | null;
  peopleWindow: string;
  /** The People tab's repo filter; '' for every repo. */
  peopleRepo: string;
  peopleSort: Sort;
  /** The people whose caret is open. */
  peopleOpen: Set<string>;
  /** The People tab's sub-view: open work by state, or the rankings. */
  peopleMode: 'work' | 'rankings';
  rankMode: RankMode;
  rankSort: Sort;
  /** The people whose caret is open in the rankings. */
  rankOpen: Set<string>;
  /** Directory rows opened to their modules, as `person|directory`. */
  dirOpen: Set<string>;
  /** The repos whose caret is open in the Repos table. */
  repoOpen: Set<string>;
  /** The People tab's search, as typed; not kept across launches. */
  peopleQuery: string;
  /** The People tab shows one section per repo. */
  peopleGroup: boolean;
  /** The repo sections collapsed in the grouped People tab. */
  groupClosed: Set<string>;
}

// ───────────────────────────── formatting ─────────────────────────────

const clock = (t: number): string => new Date(t).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
const num = (n: number | string | null | undefined): string => Number(n ?? 0).toLocaleString('en-US');
const gh = (repo: string, path = ''): string => `https://github.com/${repo}${path}`;
const shortName = (repo: string): string => repo.split('/').pop() ?? repo;

// ───────────────────────────── people text ─────────────────────────────
// The lines, cells and hints of the People tab and the Repos table's carets
// that need none of the UI's state.

/** How long an item has been in its state: "3h in state", or for an item not read yet when GitHub last saw it change. */
function stateAge(i: Item, now: number): string {
  if (!i.work.since) return 'time in state unknown';
  const a = ago(i.work.since, now);
  if (i.work.state !== NOT_READ_YET) return `${a} in state`;
  return a === 'now' ? 'updated just now' : `updated ${a} ago`;
}

/** The time a state's items have spent in it, for a person's breakdown. */
function stateAgeNote(s: StateTime): string {
  if (s.state === NOT_READ_YET) return 'their work state comes when the app reads them';
  if (s.medianSecs === null) return 'time in this state unknown';
  return `median ${durationText(s.medianSecs)}, longest ${durationText(s.maxSecs)} in this state`;
}

/** Gives the focus back to the element under `scope` whose `data-focus` key is `focused`. */
function refocus(scope: string, focused: string | undefined): void {
  if (focused) [...document.querySelectorAll<HTMLElement>(`${scope} [data-focus]`)].find((n) => n.dataset.focus === focused)?.focus();
}

/** A section of a detail: a heading and what follows it. */
const detailSection = (title: string, ...kids: (Node | string | null | false)[]): HTMLElement => el('div', {}, el('h4', {}, title), ...kids);
/** The muted line a detail section shows when it has nothing. */
const noneLine = (text: string): HTMLElement => el('p', { class: 'muted small' }, text);
/** A line of a detail as a small paragraph. */
const smallLine = (text: string): HTMLElement => el('p', { class: 'small' }, text);
/** `lines` as small paragraphs, or `none` as a muted line when there are none. */
const smallLines = (lines: readonly string[], none: string): HTMLElement[] => (lines.length ? lines.map(smallLine) : [noneLine(none)]);
/** `lines` as small paragraphs, or `none` as one when there are none. */
const smallLinesOr = (lines: readonly string[], none: string): HTMLElement[] => (lines.length ? lines : [none]).map(smallLine);

/** A repo's open pull requests that wait on one role: the count, its share and the count in each state. */
function roleLine(g: RepoFigures['byRole'][number]): string {
  const states = g.states.map((s) => `${stateLabel(s.state)} ${s.count}`).join(', ');
  return `${roleHeading(g.role)}: ${num(g.count)} (${Math.round(g.share * 100)}%). ${states}.`;
}

/** `rec` as "label count" pairs with the counts formatted; the label defaults to the key with spaces for underscores. */
const repoParts = (rec: Record<string, number>, label: (k: string) => string = (k) => k.replaceAll('_', ' ')): string =>
  Object.entries(rec).map(([k, n]) => `${label(k)} ${num(n)}`).join(', ');

/** The checks on a repo's main: their rollup and the failing ones, or that they have not been read. */
function mainChecksLine(main: MainChecks | null): string {
  if (!main) return 'The checks on main have not been read yet.';
  const rollup = main.rollup ? main.rollup.toLowerCase() : 'none reported';
  const failing = main.failing.length ? `, failing ${main.failing.join(', ')}` : '';
  return `Checks on main: ${rollup}${failing}.`;
}

/** Where a repo's pull requests went in the window: merges and by whom, closes, the median times, and merge methods and mergers. */
function repoFlowLines(fl: RepoFlow): string[] {
  const by = fl.merged ? ` (${repoParts(fl.merged_by)})` : '';
  return [
    `Merged ${num(fl.merged)}${by}, landed elsewhere ${num(fl.landed_elsewhere)}.`,
    Object.keys(fl.closed).length ? `Closed without merging: ${repoParts(fl.closed, stateLabel)}.` : '',
    `Median ${durationText(fl.median_merge_secs) || 'unknown'} to merge, ${durationText(fl.median_response_secs) || 'unknown'} to the first maintainer response.`,
    fl.merged ? `Merge method: ${repoParts(fl.methods, (k) => k)}. Mergers: ${repoParts(fl.mergers, (k) => k)}.` : '',
  ].filter(Boolean);
}

/** A person's responses: how they answer maintainer requests and how maintainers first respond to them. */
function responseLines(r: PersonResponse, now: number): string[] {
  const their = r.their_answered
    ? `They answered ${num(r.their_answered)} maintainer requests in a median of ${durationText(r.their_median_secs)}.`
    : 'They have no answered maintainer requests.';
  const first = r.m_answered
    ? `Maintainers first responded to ${num(r.m_answered)} of their pull requests in a median of ${durationText(r.m_median_secs)}.`
    : 'No maintainer has responded to their pull requests yet.';
  return [
    their,
    r.unanswered ? `${num(r.unanswered)} requests are unanswered; the oldest was made ${ago(r.oldest_unanswered, now)} ago.` : '',
    first,
    r.awaiting_first ? `${num(r.awaiting_first)} await a first response; the oldest has waited ${ago(r.oldest_awaiting, now)}.` : '',
  ].filter(Boolean);
}

/** The reviews a person gave, with a maintainer's requests, holds, merges and closes. */
function givenLines(g: Given, maintainer: boolean): string[] {
  return [
    `${num(g.comments)} comments and ${num(g.reviews)} reviews on ${num(g.prs)} pull requests: ${num(g.approved)} approvals, ${num(g.changes_requested)} change requests.`,
    maintainer ? `${num(g.requests)} requests, ${num(g.holds)} holds, ${num(g.merges)} merges and ${num(g.closes)} closes.` : '',
  ].filter(Boolean);
}

/** The name of a detail table row: a module under its directory, or a row with an empty caret (`blank`) that lines it up with the directories that open. */
function detailName(name: string, sub: boolean, blank: boolean): HTMLElement {
  return el('span', { class: sub ? 'module' : '' }, blank ? el('span', { class: 'tw', 'aria-hidden': 'true' }) : null, name);
}

/** A count, muted when it is zero. */
const plainCount = (n: number): HTMLElement => el('span', { class: n ? '' : 'zero' }, num(n));

/** A ranking's lines cell, flagged with a note when stacked or cut-short pull requests make the count incomplete. */
function rankLines(r: Rank): HTMLElement {
  const notes = [
    r.stacked ? `${r.stacked} merged pull requests credit no lines: every commit was counted in an earlier one (stacked).` : '',
    r.truncated ? `${r.truncated} merged pull requests have a commit, file or review list the fetch cut short. A commit list not read in full credits every line to the author; a file list not read in full counts its unread lines as product lines.` : '',
  ].filter(Boolean).join('\n');
  return el('span', { class: notes ? 'flagged' : '', title: notes || null }, linesText(r.added, r.removed));
}

/** A ranking cell for a `type:` column, with a ? when a fallback rule set some of the count, or for any other numeric column. */
function rankOther(key: string, r: Rank): HTMLElement {
  if (key.startsWith('type:')) {
    const t = key.slice(5);
    const n = r.types[t] ?? 0;
    const g = r.guessed[t] ?? 0;
    return el('span', {}, plainCount(n), g ? el('span', { class: 'q', title: guessTip(t, n, g) }, '?') : null);
  }
  const v = (r as unknown as Record<string, unknown>)[key];
  return plainCount(typeof v === 'number' ? v : 0);
}

/** `rec` as "label count" pairs. */
const countParts = (rec: Record<string, number>, label: (k: string) => string): string => Object.entries(rec).map(([k, n]) => `${label(k)} ${n}`).join(', ');

/** Where a person's pull requests went: merged and by whom, landed elsewhere, closed, and the time to merge. */
function personFlowLines(fl: Flow): string[] {
  const by = fl.merged ? ` (${countParts(fl.merged_by, (k) => k.replaceAll('_', ' '))})` : '';
  return [
    `Merged ${num(fl.merged)}${by}, landed elsewhere ${num(fl.landed_elsewhere)}.`,
    Object.keys(fl.closed).length ? `Closed without merging: ${countParts(fl.closed, stateLabel)}.` : '',
    fl.median_merge_secs === null ? '' : `Median ${durationText(fl.median_merge_secs)} to merge, ${durationText(fl.median_ready_secs)} from ready.`,
  ].filter(Boolean);
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
    people: null,
    peopleWindow: windowById(load('peopleWindow', '7d')).id,
    peopleRepo: '',
    peopleSort: load<Sort>('peopleSort', { key: 'on_them', dir: 'desc' }),
    peopleOpen: new Set(),
    peopleMode: load<string>('peopleMode', 'work') === 'rankings' ? 'rankings' : 'work',
    rankMode: RANK_MODES.find((m) => m.id === load<string>('rankMode', 'merged'))?.id ?? 'merged',
    rankSort: load<Sort>('rankSort', { key: 'merged', dir: 'desc' }),
    rankOpen: new Set(),
    dirOpen: new Set(),
    repoOpen: new Set(),
    peopleQuery: '',
    peopleGroup: load('peopleGroup', false),
    groupClosed: new Set(load<string[]>('groupClosed', [])),
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

  /** A "3P" tag for repos outside our own GitHub org; null for ours. */
  const thirdPartyTag = (repo: string): HTMLElement | null =>
    isThirdParty(repo) ? el('span', { class: 'third-party', title: `Third-party app (${repo.split('/')[0]})` }, '3P') : null;

  /** Cell content for a column. */
  function cell(entry: RepoEntry, col: Column): Node {
    const v = value(entry, col.key, state.window);
    const s = entry.stats;
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
    if (col.key === 'category') return v ? el('span', { class: 'category' }, String(v)) : el('span', { class: 'muted' }, '—');
    if (!s) return entry.inaccessible ? document.createTextNode('') : el('span', { class: 'muted' }, '—');
    switch (col.key) {
      case 'urgency': {
        const a = Math.min(1, Number(v) / 350) * 0.45;
        return el('span', { class: 'urgency', style: `background: rgb(var(--hot-rgb) / ${a.toFixed(2)})` }, String(Math.round(Number(v))));
      }
      case 'downloads_total':
        if (v === null) return el('span', { class: 'muted', title: 'Only known for All time: choose it in the dropdown above' }, '—');
        return document.createTextNode(num(v));
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
      case 'latest_build_rate':
        if (v === null) return el('span', { class: 'muted' }, '—');
        return el('span', {
          class: v ? '' : 'zero',
          title: `${num(s.latest_build_downloads)} downloads of ${s.latest_build}, out ${ago(s.latest_build_at, Date.parse(String(entry.fetched_at)))} when counted`,
        }, formatRate(Number(v)));
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

  /** The repos the toggles beside the tabs leave showing. */
  const shownRepos = (snap: Snapshot): RepoEntry[] =>
    snap.repos.filter((r) => !isRepoHidden(r.repo, r.category, state.hiddenKinds));

  function renderTable(snap: Snapshot): void {
    renderHead();
    const shown = shownRepos(snap);
    const rows = orderedEntries(shown, state.sort, state.order, state.window);
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
      const why = snap.repos.length ? 'Every repo is hidden by the toggles beside the tabs.' : 'No repositories configured — Edit config.';
      $('tbody').replaceChildren(el('tr', {}, el('td', { colspan: COLUMNS.length, class: 'empty' }, why)));
    }
    const t = totals(shown, state.window);
    const noTotal = (c: Column): boolean => c.kind !== 'num' || (c.allTimeOnly === true && state.window !== ALL_TIME);
    const total = (c: Column): string => (noTotal(c) ? '' : num(Math.round(t[c.key] ?? 0)));
    $('tfoot').replaceChildren(el('tr', {}, COLUMNS.map((c, i) =>
      el('td', { class: groupStart(i) }, i === 0 ? `${shown.length} repos` : total(c)))));
  }

  // ───────────────────────────── row dragging ─────────────────────────────
  // Pointer events rather than HTML5 drag and drop, which the webview's own
  // file-drop handling can swallow. A press turns into a drag once it moves a
  // few pixels; until then it is an ordinary click. Dropping saves the order on
  // screen and switches the table to it; clicking a column sorts again.

  let drag: { row: HTMLTableRowElement; startY: number; moved: boolean } | null = null;
  let justDragged = false;

  function startRowDrag(e: PointerEvent): void {
    if (e.button !== 0 || (e.target as Element).closest('.link, .row-refresh, .caret')) return;
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
    const dragged = [...$('tbody').children].map((r) => (r as HTMLElement).dataset.repo ?? '').filter(Boolean);
    state.order = reorderVisible(manualOrder(state.snap?.repos ?? [], state.order).map((r) => r.repo), dragged);
    state.sort = { key: MANUAL };
    save('order', state.order);
    save('sort', state.sort);
    render();
  }

  function cancelRowDrag(): void {
    if (finishRowDrag()) render();
  }

  function renderUrgent(snap: Snapshot): void {
    const shown = shownRepos(snap);
    if (!shown.some((r) => r.repo === state.urgentRepo)) state.urgentRepo = '';
    const sel = $<HTMLSelectElement>('urgent-repo');
    sel.replaceChildren(el('option', { value: '' }, 'All repositories'),
      ...shown.map((r) => el('option', { value: r.repo }, shortName(r.repo))));
    sel.value = state.urgentRepo;
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

  // ───────────────────────────── people ─────────────────────────────
  // The People tab: one row per person, and a caret that opens a row per
  // repo and the person's detail; or the rankings. The data is the view
  // `get_people` and `craft:people` send. The tab is drawn again for a new
  // view, the user's own changes, a changed repo list and when it is shown,
  // and keeps the focused caret. The clock and a snapshot update only the
  // records' age.

  /** What the People tab and a repo's caret say before `get_people` has records; the browser preview never has any. */
  const NO_RECORDS = 'No pull request records yet. The app reads them after each repo’s fetch; the browser preview has none.';

  /** The repos the People picker lists, so a poll leaves an open dropdown alone. */
  let peoplePickerRepos: string | null = null;
  /** Set when the People tab has to be drawn again at the next render. */
  let peopleRedraw = true;

  /** Draws the People tab again, and the open carets of the Repos table. */
  function redrawPeople(): void {
    peopleRedraw = true;
    repoDetailRedraw = true;
    render();
  }

  /** A state as a chip: dotted when it was read from comment text, with a ? at low confidence. A click opens the matched comment. */
  function stateChip(w: WorkState): HTMLElement {
    const url = w.evidence?.url;
    return el('span', {
      class: classes('wstate', w.guess && 'guess', w.low && 'low', w.state === NOT_READ_YET && 'unread'),
      title: stateTip(w),
      dataset: url ? { url } : {},
      onclick: url ? (e: Event) => openUrl(url, e) : null,
    }, stateLabel(w.state), w.low ? el('span', { class: 'q', 'aria-label': 'low confidence' }, '?') : null);
  }

  /** One state's chip in a person's breakdown, with the part read from comment text. */
  function stateGroupChip(s: StateTime): HTMLElement {
    const tip = s.state === NOT_READ_YET ? stateTip(s.items[0]!.work) : [
      `${stateLabel(s.state)}: ${countText(s.items.length, s.guessed)}`,
      s.guessed ? `${fromTextTip(s.items.length, s.guessed)}; hover an item’s state for its rule and sentence.` : 'Read from GitHub fields.',
      s.low ? `${s.low} at low confidence.` : '',
    ].filter(Boolean).join('\n');
    return el('span', { class: classes('wstate', s.guessed > 0 && 'guess', s.state === NOT_READ_YET && 'unread'), title: tip },
      stateLabel(s.state), s.low ? el('span', { class: 'q', 'aria-label': 'low confidence' }, '?') : null);
  }

  /** A count cell: dotted, with the part read from comment text, when there is one. */
  function countCell(n: number, guessed: number): HTMLElement {
    const plain = n ? '' : 'zero';
    return el('span', {
      class: guessed ? 'guess-count' : plain,
      title: guessed ? fromTextTip(n, guessed) : null,
    }, countText(n, guessed));
  }

  function itemLine(i: Item, now: number, withAuthor = false): HTMLElement {
    return el('div', {},
      link(`#${i.number}`, i.url), ' ', i.title, ' ',
      el('span', { class: 'muted' }, [withAuthor && i.author, shortName(i.repo), stateAge(i, now)].filter(Boolean).join(', ')),
      ' ', stateChip(i.work));
  }

  /** A person's first cell: the caret that opens their repos, or in repo section `group` their detail there, their login and their name. */
  function personCell(p: PersonRow, open: boolean, group: string): HTMLTableCellElement {
    const openKey = group ? `${group}|${p.key}` : p.key;
    const what = group ? `${p.login}’s detail in ${shortName(group)}` : `${p.login} by repo`;
    return el('td', { class: 'person' },
      el('button', {
        class: 'caret',
        'aria-expanded': String(open),
        'aria-label': `${open ? 'Hide' : 'Show'} ${what}`,
        dataset: { focus: `caret:${openKey}` },
        onclick: () => togglePerson(openKey),
      }, open ? '▾' : '▸'),
      link(p.login, `https://github.com/${p.login}`, 'who'),
      p.name ? el('span', { class: 'name', title: p.name }, p.name) : null);
  }

  /** A person's row, or with `repo` one of the per-repo rows under it. `group` is the repo section a grouped row is in. */
  function personRow(f: PersonFigures, p: PersonRow, open: boolean, repo = '', group = ''): HTMLTableRowElement {
    const entry = state.snap?.repos.find((e) => e.repo === repo);
    const first = repo
      ? el('td', { class: 'person' }, appIcon(entry ?? { repo }), ' ', shortName(repo))
      : personCell(p, open, group);
    const others = Object.entries(f.onOthers).map(([role, n]) => `${ROLE_LABELS[role as keyof typeof ROLE_LABELS]} ${n}`).join(', ');
    const plain = (n: number) => el('span', { class: n ? '' : 'zero' }, num(n));
    const cells: Node[] = [
      countCell(f.open, f.openGuessed),
      countCell(f.onThem, f.onThemGuessed),
      el('span', { class: f.onOthersTotal ? '' : 'zero', title: others ? `Waiting on ${others}` : null }, num(f.onOthersTotal)),
      countCell(f.held, f.heldGuessed),
      plain(f.merged),
      plain(f.landed),
      plain(f.closed),
      el('span', {}, durationText(f.mergeMedian)),
      el('span', {}, f.landedRate === null ? '' : `${Math.round(f.landedRate * 100)}%`),
      el('span', { title: f.unanswered ? `${f.unanswered} requests unanswered` : null }, durationText(f.theirResponse), f.unanswered ? el('span', { class: 'old' }, ` · ${f.unanswered} open`) : null),
      el('span', { title: f.awaitingFirst ? `${f.awaitingFirst} awaiting a first response` : null }, durationText(f.responseToThem), f.awaitingFirst ? el('span', { class: 'old' }, ` · ${f.awaitingFirst} waiting`) : null),
      plain(f.reviews),
    ];
    return el('tr', { class: repo ? 'sub' : 'person-row', dataset: { key: p.key, repo, group } }, first, ...cells.map((c) => el('td', {}, c)));
  }

  /** The detail under an open person: open work by state with time in state, work waiting on them, holds, responses, reviews given and flow, in `repo` ('' for every repo). */
  function personDetail(v: PeopleView, p: PersonRow, f: PersonFigures, now: number, repo: string): HTMLElement {
    const items = repoItems(v, repo);
    const own = items.filter((i) => i.author === p.key);
    const theirs = items.filter((i) => i.author !== p.key && waitsOn(i, p.key));
    const holds = own.filter((i) => i.work.hold);
    const parked = own.filter((i) => i.work.state === 'author_declared_hold');
    const responses = f.response ? responseLines(f.response, now) : [];
    const maintainer = p.maintainer_of.length > 0;
    const given = f.given ? givenLines(f.given, maintainer) : [];
    if (maintainer) {
      const answeredQueue = items.filter((i) => i.work.state === 'author_responded_awaiting_maintainer' && waitsOn(i, p.key)).length;
      given.push(`${num(answeredQueue)} authors have answered their requests and wait on them.`);
    }
    const flow = f.flow ? personFlowLines(f.flow) : [];
    const windowLabel = windowById(state.peopleWindow).label.toLowerCase();
    const where = repo ? `Where their work went in ${shortName(repo)}` : 'Where their work went';
    return el('div', { class: 'person-detail' },
      detailSection('Open work by state', ...stateGroups(own, now)),
      detailSection('Waiting on them', theirs.length ? el('div', {}, theirs.map((i) => itemLine(i, now, true))) : noneLine('Nothing else waits on them.')),
      detailSection('Held for the owner',
        holds.length ? el('div', {}, holds.map((i) => holdLine(i, now))) : noneLine('None of their pull requests is held.'),
        parked.length ? el('div', {}, el('div', { class: 'muted small' }, 'Parked by them pending a decision'), parked.map((i) => itemLine(i, now))) : null),
      detailSection('Responses', ...smallLines(responses, 'No pull requests.')),
      detailSection(`Reviews given, ${windowLabel}`, ...smallLines(given, 'None in this window.')),
      detailSection(`Flow, ${windowLabel}`, ...smallLines(flow, 'Nothing merged or closed in this window.')),
      el('div', { class: 'wide' }, el('h4', {}, `${where}, ${windowLabel}`), personTables(p.key, repo)));
  }

  /** A person's open work grouped by who it waits on and by state, with time in state and the oldest items. */
  function stateGroups(own: readonly Item[], now: number): HTMLElement[] {
    const groups = stateBreakdown(own, now).map((g) => el('div', {},
      el('div', { class: 'muted small' }, roleHeading(g.role)),
      el('ul', {}, g.states.map((s) => el('li', {},
        el('div', { class: 'state-line' }, stateGroupChip(s), el('span', {}, countText(s.items.length, s.guessed)),
          el('span', { class: 'muted' }, stateAgeNote(s))),
        el('div', { class: 'oldest' }, s.oldest.map((i) => itemLine(i, now))))))));
    return groups.length ? groups : [noneLine('No open pull requests.')];
  }

  /** A held item: its link, title, how long and by whom it is held, and its state. */
  function holdLine(i: Item, now: number): HTMLElement {
    const h = i.work.hold!;
    const bits = [`held ${ago(h.since, now)}`, h.by && `by ${h.by}`, h.category, h.reaffirmed && `held again ${ago(h.reaffirmed, now)} ago`];
    return el('div', {}, link(`#${i.number}`, i.url), ' ', i.title, ' ', el('span', { class: 'muted' }, bits.filter(Boolean).join(', ')), ' ', stateChip(i.work));
  }

  function togglePerson(key: string): void {
    if (state.peopleOpen.has(key)) state.peopleOpen.delete(key); else state.peopleOpen.add(key);
    redrawPeople();
  }

  // The carets' tables come from the host when a caret opens:
  // `person_detail(login, window, repo)` and `repo_detail(repo, window)`.
  // Answers are kept by person or repo and window. After a new People view,
  // a kept answer is still shown while the host is asked again, and the new
  // answer replaces it.

  /** A kept answer: `gen` is the People view it was asked under, `read` the last view a render read it in. */
  type Kept<T> = { value: T | null; gen: number; read: number };
  const personDetails = new Map<string, Kept<PersonDetail>>();
  const repoDetails = new Map<string, Kept<RepoPeople>>();
  const asking = new Set<string>();
  /** Counts People views: an answer asked for before the latest one is dropped. */
  let generation = 0;

  /** The kept answer to `cmd` for `key`, asking the host again when it is from an earlier People view; undefined while the first is asked. */
  function ask<T>(cache: Map<string, Kept<T>>, cmd: string, key: string, args: Record<string, unknown>): T | null | undefined {
    const kept = cache.get(key);
    if (kept) kept.read = generation;
    if (kept?.gen === generation) return kept.value;
    const asked = generation;
    const tag = `${asked}|${cmd}|${key}`;
    if (!asking.has(tag)) {
      asking.add(tag);
      const keep = (d: T | null) => { if (asked === generation) cache.set(key, { value: d, gen: asked, read: asked }); };
      api.invoke<T | null>(cmd, args)
        .then((d) => keep(d ?? null))
        // A failed request keeps the earlier answer when there is one.
        .catch((e: unknown) => { console.warn(e); keep(cache.get(key)?.value ?? null); })
        .finally(() => { asking.delete(tag); redrawPeople(); });
    }
    return kept?.value;
  }

  /** Starts a new People view: answers no render read in the last one are dropped, and the rest are asked again when next read. */
  function newPeopleView(): void {
    for (const cache of [personDetails, repoDetails] as Map<string, Kept<unknown>>[]) {
      for (const [key, kept] of cache) if (kept.read < generation) cache.delete(key);
    }
    generation += 1;
  }

  /** `key`'s tables in `window`, in `repo` alone when it is not ''. */
  const personDetailOf = (key: string, window: string, repo: string) =>
    ask<PersonDetail>(personDetails, 'person_detail', `${key}|${window}|${repo}`, { login: key, window, ...(repo ? { repo } : {}) });
  const repoDetailOf = (repo: string, window: string) =>
    ask<RepoPeople>(repoDetails, 'repo_detail', `${repo}|${window}`, { repo, window });

  /** A share of the person's own total as a bar and a percentage. */
  function shareBar(share: number): HTMLElement {
    const pct = Math.round(share * 100);
    return el('span', { class: 'share', title: `${pct}% of their own total` },
      el('span', { class: 'share-bar' }, el('span', { class: 'share-fill', style: `width: ${pct}%` })), el('span', { class: 'share-pct' }, `${pct}%`));
  }

  /** The caret and name of a directory row whose modules under src/ open under it; `openKey` is `dirOf|directory`. */
  function dirToggle(openKey: string, name: string): HTMLElement {
    const open = state.dirOpen.has(openKey);
    return el('button', {
      class: 'dir-toggle', 'aria-expanded': String(open), dataset: { focus: `dir:${openKey}` },
      title: `${open ? 'Hide' : 'Show'} the modules under ${name}/src/`,
      onclick: () => {
        if (open) state.dirOpen.delete(openKey);
        else state.dirOpen.add(openKey);
        redrawPeople();
      },
    }, el('span', { class: 'tw', 'aria-hidden': 'true' }, open ? '▾' : '▸'), name);
  }

  /** One of a person's detail tables. `repoCols` adds merges, reviews and review commits; `label` names a row. */
  function detailTable(title: string, list: readonly DetailRow[], opts: { repoCols?: boolean; label?: (k: string) => string; type?: boolean; dirOf?: string } = {}): HTMLElement {
    // [header, column class]; style.css gives each class a fixed width.
    const cols: [string, string][] = [['', 'name'], ['Merged', 'n'], ['Lines', 'lines'], ['Share', 'share'],
      ...(opts.repoCols ? [['Merges', 'n'], ['Reviews', 'n'], ['Review commits', 'wide-n']] as [string, string][] : [])];
    const body = el('tbody');
    const dirs = opts.dirOf !== undefined;
    const line = (r: DetailRow, sub: boolean) => {
      const name = opts.label?.(r.key) ?? r.key;
      // A directory with modules under src/ opens them; the others get an
      // empty caret of the same width.
      const first = dirs && !sub && r.children.length ? dirToggle(`${opts.dirOf}|${r.key}`, name) : detailName(name, sub, dirs && !sub);
      const guess = opts.type && r.guessed ? el('span', { class: 'q', title: `${guessTip(r.key, r.merged, r.guessed)}\n${r.rules.join('\n')}` }, '?') : null;
      return el('tr', { class: sub ? 'sub' : '' },
        el('td', { title: [name, ...(opts.type ? [] : r.rules)].join('\n') }, first, guess),
        el('td', {}, num(r.merged)),
        el('td', {}, linesText(r.added, r.removed)),
        el('td', { class: 'share-td' }, shareBar(r.share)),
        ...(opts.repoCols ? [el('td', {}, num(r.merges)), el('td', {}, num(r.reviews)), el('td', {}, num(r.review_commits))] : []));
    };
    for (const r of list) {
      body.append(line(r, false));
      if (dirs && state.dirOpen.has(`${opts.dirOf}|${r.key}`)) for (const c of r.children) body.append(line(c, true));
    }
    if (!list.length) body.append(el('tr', {}, el('td', { colspan: cols.length, class: 'muted' }, 'Nothing in this window.')));
    return el('div', { class: classes('work-table', opts.repoCols && 'wide') }, el('h4', {}, title),
      el('table', { class: classes('figures', 'fixed', opts.repoCols && 'repo-cols') },
        el('colgroup', {}, cols.map(([, c]) => el('col', { class: `c-${c}` }))),
        el('thead', {}, el('tr', {}, cols.map(([h]) => el('th', {}, h)))), body));
  }

  /** Where a person's work went in the People window: by repository, area, type and directory; in `repo` alone when it is not ''. */
  function personTables(key: string, repo = ''): HTMLElement {
    const d = personDetailOf(key, state.peopleWindow, repo);
    if (d === undefined) return el('p', { class: 'muted small' }, 'Loading where their work went…');
    if (d === null) return el('p', { class: 'muted small' }, 'Not available until the pull request records are read.');
    const windowLabel = windowById(state.peopleWindow).label.toLowerCase();
    return el('div', { class: 'work-tables' },
      el('p', { class: 'muted small' }, `${workLead(d.merged, d.added, d.removed, windowLabel)} ${TIME_NOTE}`),
      detailTable('By repository', d.by_repo, { repoCols: true, label: shortName }),
      // The three narrow tables share a grid with no full-width row.
      el('div', { class: 'work-small' },
        detailTable('By area', d.by_area),
        detailTable('By type', d.by_type, { type: true }),
        detailTable('By directory', d.by_dir, { dirOf: repo ? `${repo}|${d.key}` : d.key })));
  }

  /** An unverified marker for a count from a batch-branch repository. */
  const unverified = (on: boolean) => (on ? el('span', { class: 'unverified', title: UNVERIFIED_TIP }, '*') : null);

  /** The person cell of a ranking row: a caret that opens where their work went when `focusPrefix` is set, their login and name. `openKey` names the caret in `state.rankOpen`. */
  function rankPerson(row: RankRow, open: boolean, focusPrefix: string, openKey: string): HTMLElement {
    let caret: HTMLElement | null = null;
    if (focusPrefix) {
      caret = el('button', {
        class: 'caret', 'aria-expanded': String(open), 'aria-label': `${open ? 'Hide' : 'Show'} where ${row.login}’s work went`,
        dataset: { focus: `${focusPrefix}:${openKey}` },
        onclick: () => {
          if (state.rankOpen.has(openKey)) state.rankOpen.delete(openKey);
          else state.rankOpen.add(openKey);
          redrawPeople();
        },
      }, open ? '▾' : '▸');
    }
    return el('span', { class: 'person' }, caret,
      link(row.login, `https://github.com/${row.login}`, 'who'),
      row.name ? el('span', { class: 'name', title: row.name }, row.name) : null);
  }

  /** The cell of ranking column `c` for a person with figures `r`. `openKey` names the caret in `state.rankOpen`. */
  function rankCell(c: RankColumn, row: RankRow, r: Rank, open: boolean, focusPrefix: string, openKey = row.key): Node {
    switch (c.key) {
      case 'person': return rankPerson(row, open, focusPrefix, openKey);
      case 'merged': return el('span', {}, plainCount(r.merged), unverified(r.unverified && r.merged > 0));
      case 'landed': return el('span', {}, plainCount(r.landed), unverified(r.unverified && r.landed > 0));
      case 'lines': return rankLines(r);
      case 'generated': return el('span', { class: r.gen_added + r.gen_removed ? '' : 'zero' }, linesText(r.gen_added, r.gen_removed));
      case 'review_lines': return el('span', { class: r.review_added + r.review_removed ? '' : 'zero' }, linesText(r.review_added, r.review_removed));
      case 'direct': return r.direct === null ? el('span', { class: 'muted', title: DIRECT_MISSING_TIP }, '—') : plainCount(r.direct);
      case 'merges': return el('span', {}, plainCount(r.merges), unverified(r.unverified && r.merges > 0),
        r.batch_merges ? el('span', { class: 'batch', title: BATCH_TIP }, batchText(r)) : null);
      case PRODUCT: return plainCount((r.types.feature ?? 0) + (r.types.fix ?? 0));
      default: return rankOther(c.key, r);
    }
  }

  /** The repos `key` has figures in for the People window, which the search reads. */
  function searchRepos(v: PeopleView, key: string, repos: readonly string[]): string[] {
    const p = v.people.find((x) => x.key === key);
    return p ? personRepos(v, p, state.peopleWindow, repos) : [];
  }

  function toggleGroup(repo: string): void {
    if (state.groupClosed.has(repo)) state.groupClosed.delete(repo); else state.groupClosed.add(repo);
    save('groupClosed', [...state.groupClosed]);
    redrawPeople();
  }

  /** A repo section's header row: a caret that collapses it, the repo, how many people it lists, and `cells` for the other columns. */
  function sectionRow(repo: string, count: number | null, closed: boolean, cells: string[], title: string): HTMLTableRowElement {
    const entry = state.snap?.repos.find((e) => e.repo === repo);
    const noun = count === 1 ? 'person' : 'people';
    const counted = count === null ? 'loading…' : `${num(count)} ${noun}`;
    const first = el('td', {},
      el('button', {
        class: 'caret', 'aria-expanded': String(!closed), 'aria-label': `${closed ? 'Show' : 'Hide'} the people of ${shortName(repo)}`,
        dataset: { focus: `group:${repo}` }, onclick: () => toggleGroup(repo),
      }, closed ? '▸' : '▾'),
      el('span', { class: 'group-name' }, appIcon(entry ?? { repo }), shortName(repo)),
      el('span', { class: 'group-count' }, counted));
    return el('tr', { class: 'repo-group', dataset: { group: repo }, title }, first, ...cells.map((c) => el('td', {}, c)));
  }

  /** The row under a table when nobody is listed. */
  const noneRow = (span: number, text: string) => el('tr', {}, el('td', { colspan: span, class: 'empty' }, text));
  const noMatch = () => `Nobody matches “${state.peopleQuery.trim()}”.`;

  /** The Rankings view: one row per person in the selected ranking and window, sortable, with a caret that opens where their work went.
   * Grouped, it shows one section per repo with each person's figures in that repo. Returns how many people it shows and lists in all,
   * and whether a repo's people are still being asked for. */
  function renderRankings(v: PeopleView, repos: readonly string[]): [number, number, boolean] {
    const mode = state.rankMode;
    const sort = rankSortFor(state.rankSort, mode);
    const cols = RANK_COLUMNS[mode];
    const words = searchWords(state.peopleQuery);
    const release = state.peopleWindow === 'release';
    const head = el('tr', { class: 'cols' }, cols.map((c) => {
      const sorted = sort.key === c.key;
      return el('th', {
        title: release && c.key !== 'person' ? `${c.title}\n${RELEASE_TIP}` : c.title,
        class: classes(sorted && 'sorted', sorted && sort.dir === 'asc' && 'asc'),
        onclick: () => { state.rankSort = nextRankSort(sort, c.key); save('rankSort', state.rankSort); redrawPeople(); },
      }, c.label);
    }));
    const body = el('tbody');
    const addRow = ({ row, rank }: { row: RankRow; rank: Rank }, group: string) => {
      const openKey = group ? `${group}|${row.key}` : row.key;
      const open = state.rankOpen.has(openKey);
      body.append(el('tr', { class: 'rank-row', dataset: { key: row.key, group } }, cols.map((c) => el('td', {}, rankCell(c, row, rank, open, 'rank-caret', openKey)))));
      if (open) body.append(el('tr', { class: 'detail', dataset: { key: row.key, group } }, el('td', { colspan: cols.length }, personTables(row.key, group))));
    };
    const draw: RankDraw = { v, repos, words, mode, sort, body, addRow, shown: new Set(), listed: new Set(), pending: false };
    if (state.peopleGroup) groupedRankRows(draw);
    else flatRankRows(draw);
    if (!body.childElementCount) body.append(noneRow(cols.length, rankEmptyText(words, draw.pending)));
    const notes: Node[] = [];
    const co = Object.entries(v.co_unlinked?.[state.peopleWindow] ?? {});
    if (co.length) {
      const names = co.map(([n, c]) => `${n} (${c})`).join(', ');
      notes.push(el('p', { class: 'muted small' }, `Co-authors without a known account: ${names}.`));
    }
    if (v.unlinked?.length) notes.push(el('p', { class: 'muted small', id: 'rank-unlinked' }, `Not linked to a GitHub account: ${v.unlinked.join(', ')}.`));
    $('people-main').replaceChildren(el('table', { class: 'people-table', id: 'rank-table' }, el('thead', {}, head), body), ...notes);
    return [draw.shown.size, draw.listed.size, draw.pending];
  }

  /** What drawing the Rankings rows reads, and what it counts: who it shows and lists, and whether a repo's people are still being asked for. */
  interface RankDraw {
    v: PeopleView;
    repos: readonly string[];
    words: readonly string[];
    mode: RankMode;
    sort: Sort;
    body: HTMLElement;
    addRow: (r: { row: RankRow; rank: Rank }, group: string) => void;
    shown: Set<string>;
    listed: Set<string>;
    pending: boolean;
  }

  /** The grouped Rankings rows: one section per repo with each person's figures in that repo. */
  function groupedRankRows(d: RankDraw): void {
    const cols = RANK_COLUMNS[d.mode];
    for (const repo of d.repos) {
      const rp = repoDetailOf(repo, state.peopleWindow);
      if (rp === undefined) {
        d.pending = true;
        d.body.append(sectionRow(repo, null, state.groupClosed.has(repo), cols.slice(1).map(() => ''), 'Loading this repo’s people'));
      } else {
        rankSection(d, repo, rp ? rankRowsOf(rp.people, rp.window, d.mode, d.sort) : []);
      }
    }
  }

  /** One repo's section of the grouped Rankings: its header with the totals and, while it is open, the rows of the people in `all` that the search matches. */
  function rankSection(d: RankDraw, repo: string, all: readonly { row: RankRow; rank: Rank }[]): void {
    for (const { row } of all) d.listed.add(row.key);
    const rows = all.filter(({ row }) => matchesSearch(d.words, personSearchFields(d.v, row, [repo])));
    if (!rows.length) return;
    for (const { row } of rows) d.shown.add(row.key);
    const closed = state.groupClosed.has(repo);
    const cols = RANK_COLUMNS[d.mode];
    d.body.append(sectionRow(repo, rows.length, closed, cols.slice(1).map((c) => rankTotal(rows, c.key)), RANK_TOTALS_TIP));
    if (!closed) for (const r of rows) d.addRow(r, repo);
  }

  /** The Rankings rows without groups. */
  function flatRankRows(d: RankDraw): void {
    const all = rankRows(d.v, state.peopleWindow, d.mode, d.sort);
    for (const { row } of all) d.listed.add(row.key);
    // A repo matches the people its section lists when grouped: the ones
    // with figures for this ranking in its `repo_detail`, which is read the
    // first time a person's names leave a word unmatched.
    let members: Map<string, Set<string>> | null = null;
    const rankRepos = (key: string): string[] => {
      members ??= new Map(d.repos.map((repo) => {
        const rp = repoDetailOf(repo, state.peopleWindow);
        if (rp === undefined) d.pending = true;
        return [repo, new Set(rp ? rankRowsOf(rp.people, rp.window, d.mode, d.sort).map(({ row }) => row.key) : [])];
      }));
      return d.repos.filter((repo) => members!.get(repo)!.has(key));
    };
    const rows = all.filter(({ row }) => personMatches(d.v, row, d.words, () => rankRepos(row.key)));
    for (const r of rows) { d.shown.add(r.row.key); d.addRow(r, ''); }
  }

  /** The line under an empty Rankings table. */
  function rankEmptyText(words: readonly string[], pending: boolean): string {
    if (!words.length) return 'Nobody has anything in this ranking in this window.';
    return pending ? 'Looking up each repo’s people…' : noMatch();
  }

  /** Fills the People tab's window and repo pickers; returns the configured repos. */
  function peoplePickers(snap: Snapshot): string[] {
    const win = $<HTMLSelectElement>('people-window');
    if (!win.options.length) win.append(...WINDOWS.map((w) => el('option', { value: w.id }, w.label)));
    if (win.value !== state.peopleWindow) win.value = state.peopleWindow;
    const repos = snap.repos.map((r) => r.repo);
    const sel = $<HTMLSelectElement>('people-repo');
    if (repos.join('\n') !== peoplePickerRepos) {
      sel.replaceChildren(el('option', { value: '' }, 'All repositories'), ...repos.map((r) => el('option', { value: r }, shortName(r))));
      peoplePickerRepos = repos.join('\n');
      // A filter on a repo that left the list goes back to every repo; a
      // repo listed again in other letter case keeps it.
      state.peopleRepo = repos.find((r) => r.toLowerCase() === state.peopleRepo.toLowerCase()) ?? '';
    }
    if (sel.value !== state.peopleRepo) sel.value = state.peopleRepo;
    return repos;
  }

  /** Sets the People tab's sub-view buttons, shows the pickers `mode` uses and hides the others, and sets the hint. */
  function peopleModeControls(mode: State['peopleMode']): void {
    const ranking = mode === 'rankings';
    document.querySelectorAll<HTMLElement>('#people-view .seg').forEach((b) => b.setAttribute('aria-selected', String(b.dataset.pmode === mode)));
    const rankSel = $<HTMLSelectElement>('people-rank');
    if (!rankSel.options.length) rankSel.append(...RANK_MODES.map((m) => el('option', { value: m.id }, m.label)));
    if (rankSel.value !== state.rankMode) rankSel.value = state.rankMode;
    const typeSel = $<HTMLSelectElement>('people-type');
    if (!typeSel.options.length) {
      // The Person header sorts the table too, so the picker lists it.
      typeSel.append(el('option', { value: 'person' }, 'Sort by person'), el('option', { value: PRODUCT }, 'Sort by product work'),
        ...TYPES.map((t) => el('option', { value: `type:${t}` }, `Sort by ${t}`)));
    }
    const typeKey = rankSortFor(state.rankSort, 'type').key;
    if (typeSel.value !== typeKey) typeSel.value = typeKey;
    $('people-repo').hidden = ranking;
    rankSel.hidden = !ranking;
    typeSel.hidden = !ranking || state.rankMode !== 'type';
    const hint = $('people-hint');
    hint.textContent = ranking
      ? '? marks a type set by a fallback rule, * an unverified count. Hover a header for what it counts.'
      : 'Dotted states were read from comment text, not from GitHub fields; ? marks low confidence. Hover a state for its rule and sentence.';
    hint.title = hint.textContent;
    $<HTMLInputElement>('people-group').checked = state.peopleGroup;
    const search = $<HTMLInputElement>('people-search');
    if (search.value !== state.peopleQuery) search.value = state.peopleQuery;
  }

  function renderPeople(snap: Snapshot): void {
    const repos = peoplePickers(snap);
    peopleModeControls(state.peopleMode);
    const words = searchWords(state.peopleQuery);
    const matches = $('people-matches');
    const v = state.people;
    const cached = $('people-cached');
    cached.textContent = cachedText(v);
    cached.title = cached.textContent;
    const focused = (document.activeElement as HTMLElement | null)?.dataset?.focus;
    if (!v) {
      matches.textContent = '';
      $('people-main').replaceChildren(el('p', { class: 'empty' }, NO_RECORDS));
      return;
    }
    if (state.peopleMode === 'rankings') {
      const [shown, listed, pending] = renderRankings(v, repos);
      // A repo still being asked for can add matches, so the count waits for it.
      matches.textContent = pending ? '' : matchText(shown, listed, words);
      refocus('#people-main', focused);
    } else {
      matches.textContent = renderPeopleTable(v, repos, words);
      refocus('#people-table', focused);
    }
  }

  /** The People tab's work table: one row per person in the window and repo, sortable, with a caret that opens their repos and detail.
   * Grouped, it shows one section per repo. Returns how many people match `words`, as the line by the search shows it. */
  function renderPeopleTable(v: PeopleView, repos: readonly string[], words: readonly string[]): string {
    const now = Date.now();
    const head = el('tr', { class: 'cols' }, PEOPLE_COLUMNS.map((c) => {
      const sorted = state.peopleSort.key === c.key;
      return el('th', {
        title: c.title,
        class: classes(sorted && 'sorted', sorted && state.peopleSort.dir === 'asc' && 'asc'),
        onclick: () => { state.peopleSort = nextPeopleSort(state.peopleSort, c.key); save('peopleSort', state.peopleSort); redrawPeople(); },
      }, c.label);
    }));
    const body = el('tbody');
    const text = state.peopleGroup ? groupedPeopleRows(v, repos, words, body, now) : flatPeopleRows(v, repos, words, body, now);
    if (!body.childElementCount) body.append(noneRow(PEOPLE_COLUMNS.length, words.length ? noMatch() : 'Nobody has open work or activity here in this window.'));
    $('people-main').replaceChildren(el('table', { class: 'people-table', id: 'people-table' }, el('thead', {}, head), body));
    return text;
  }

  /** The grouped People rows in `body`: one section per repo with each person's figures in that repo. Returns the match line. */
  function groupedPeopleRows(v: PeopleView, repos: readonly string[], words: readonly string[], body: HTMLElement, now: number): string {
    const shownRepos = state.peopleRepo ? [state.peopleRepo] : repos;
    const groups = peopleGroups(v, state.peopleWindow, shownRepos, state.peopleSort,
      (p, repo) => matchesSearch(words, personSearchFields(v, p, [repo])));
    const listed = words.length ? new Set(peopleGroups(v, state.peopleWindow, shownRepos, state.peopleSort).flatMap((g) => g.rows.map((f) => f.key))) : null;
    const shown = new Set(groups.flatMap((g) => g.rows.map((f) => f.key)));
    for (const g of groups) {
      const closed = state.groupClosed.has(g.repo);
      const totals = groupTotals(g.rows);
      body.append(sectionRow(g.repo, g.rows.length, closed, PEOPLE_COLUMNS.slice(1).map((c) => {
        const n = totals[c.key];
        return n === undefined ? '' : num(n);
      }), GROUP_TOTALS_TIP));
      if (closed) continue;
      for (const f of g.rows) {
        const p = v.people.find((x) => x.key === f.key)!;
        const open = state.peopleOpen.has(`${g.repo}|${f.key}`);
        body.append(personRow(f, p, open, '', g.repo));
        if (open) body.append(el('tr', { class: 'detail', dataset: { key: p.key, group: g.repo } }, el('td', { colspan: PEOPLE_COLUMNS.length }, personDetail(v, p, f, now, g.repo))));
      }
    }
    return matchText(shown.size, (listed ?? shown).size, words);
  }

  /** The People rows in `body` without groups, each open person followed by their repos and detail. Returns the match line. */
  function flatPeopleRows(v: PeopleView, repos: readonly string[], words: readonly string[], body: HTMLElement, now: number): string {
    const shownRepos = state.peopleRepo ? [state.peopleRepo] : repos;
    const all = peopleRows(v, state.peopleWindow, state.peopleRepo, state.peopleSort);
    const rows = all.filter((f) => personMatches(v, f, words, () => searchRepos(v, f.key, shownRepos)));
    for (const f of rows) {
      const p = v.people.find((x) => x.key === f.key)!;
      const open = state.peopleOpen.has(f.key);
      body.append(personRow(f, p, open));
      if (!open) continue;
      for (const repo of personRepos(v, p, state.peopleWindow, state.peopleRepo ? [] : repos)) {
        body.append(personRow(personFigures(v, p, state.peopleWindow, repo), p, false, repo));
      }
      body.append(el('tr', { class: 'detail', dataset: { key: p.key } }, el('td', { colspan: PEOPLE_COLUMNS.length }, personDetail(v, p, f, now, state.peopleRepo))));
    }
    return matchText(rows.length, all.length, words);
  }

  /** The People tab's record age: "Pull requests as of 10:05 (3h ago)", in
   * the alert color once older than the snapshot's stale limit or while a
   * repo was never checked. Every render updates it, the clock's included. */
  function renderPeopleAge(snap: Snapshot): void {
    const node = $('people-age');
    const age = recordsAge(state.people, snap.stale_after_secs);
    if (!age) { node.textContent = ''; node.className = 'hint'; node.title = ''; return; }
    const parts: string[] = [];
    if (age.oldest !== null) {
      const a = ago(new Date(age.oldest).toISOString());
      const since = a === 'now' ? 'now' : `${a} ago`;
      parts.push(`Pull requests as of ${clock(age.oldest)} (${since})`);
    }
    if (age.unchecked.length) parts.push(`not checked yet: ${age.unchecked.map(shortName).join(', ')}`);
    node.textContent = parts.join(' · ');
    node.className = classes('hint', age.stale && 'stale');
    node.title = [
      `Oldest pull request records: ${age.oldest !== null ? localTime(new Date(age.oldest).toISOString()) : '—'}`,
      `Stale after ${Math.round(snap.stale_after_secs / 60)} min without an update`,
    ].join('\n');
  }

  /** Shows the People tab and draws it when it was hidden or something it shows changed. */
  function renderPeopleView(snap: Snapshot): void {
    const pane = $('people-view');
    const shown = pane.hidden;
    pane.hidden = false;
    renderPeopleAge(snap);
    if (shown || peopleRedraw || snap.repos.map((r) => r.repo).join('\n') !== peoplePickerRepos) {
      peopleRedraw = false;
      renderPeople(snap);
    }
  }

  /** The People tab's badge: the open pull requests held for the owner. */
  function renderPeopleBadge(): void {
    const held = heldCount(state.people);
    $('people-count').textContent = held ? String(held) : '';
    $('people-count').title = `${held} open pull requests held for the owner`;
  }

  // ───────────────────────────── repo carets ─────────────────────────────
  // The caret on a row of the Repos table opens a detail row under it with
  // the repo's people in the table's window and its open work. The rows come
  // from renderTable; this adds the carets to them and puts each open
  // repo's detail row under its row. A detail row is kept between renders
  // and drawn again only for a new People view, an answer from the host or
  // the user's own changes, so a snapshot or the clock leaves its tooltips
  // and selection alone.

  /** The detail row under each repo whose caret is open. */
  const repoDetailRows = new Map<string, HTMLTableRowElement>();
  /** Set when the open detail rows have to be drawn again at the next render. */
  let repoDetailRedraw = false;
  /** The window the detail rows were drawn for. */
  let repoDetailWindow = '';

  function toggleRepo(repo: string): void {
    if (state.repoOpen.has(repo)) state.repoOpen.delete(repo); else state.repoOpen.add(repo);
    render();
    [...document.querySelectorAll<HTMLElement>('#tbody .caret')].find((b) => b.dataset.focus === `repo-caret:${repo}`)?.focus();
  }

  /** Adds the caret to the name cell of a Repos row when it has none, and sets it for `open`. */
  function repoCaret(tr: HTMLTableRowElement, repo: string, open: boolean): void {
    const name = tr.querySelector<HTMLElement>('.repo');
    if (!name) return;
    let caret = name.querySelector<HTMLButtonElement>(':scope > .caret');
    if (!caret) {
      caret = el('button', {
        class: 'caret',
        dataset: { focus: `repo-caret:${repo}` },
        onclick: (e: Event) => { e.stopPropagation(); toggleRepo(repo); },
      });
      name.prepend(caret);
    }
    caret.textContent = open ? '▾' : '▸';
    caret.setAttribute('aria-expanded', String(open));
    caret.setAttribute('aria-label', `${open ? 'Hide' : 'Show'} the people and open work of ${shortName(repo)}`);
  }

  /** Puts the detail row of an open repo under its row, drawing it when it is new or `redraw` is set. */
  function placeRepoDetail(tr: HTMLTableRowElement, repo: string, redraw: boolean): void {
    let detail = repoDetailRows.get(repo);
    const fresh = !detail;
    if (!detail) {
      detail = el('tr', { class: 'repo-detail', dataset: { detail: repo } }, el('td', { colspan: COLUMNS.length }));
      repoDetailRows.set(repo, detail);
    }
    if (fresh || redraw) detail.cells[0]!.replaceChildren(repoDetail(repo));
    if (tr.nextElementSibling !== detail) tr.after(detail);
  }

  /** Adds the carets to the rows renderTable drew and puts the open detail rows under them; `focused` is the `data-focus` key that had the focus before. */
  function renderRepoCarets(focused?: string): void {
    const tbody = $('tbody');
    const rows = [...tbody.querySelectorAll<HTMLTableRowElement>(':scope > tr[data-repo]')];
    const shown = new Set(rows.map((tr) => tr.dataset.repo!));
    for (const [repo, tr] of repoDetailRows) {
      if (!state.repoOpen.has(repo) || !shown.has(repo)) { tr.remove(); repoDetailRows.delete(repo); }
    }
    const redraw = repoDetailRedraw || repoDetailWindow !== state.window;
    repoDetailRedraw = false;
    repoDetailWindow = state.window;
    for (const tr of rows) {
      const repo = tr.dataset.repo!;
      const open = state.repoOpen.has(repo);
      repoCaret(tr, repo, open);
      if (open) placeRepoDetail(tr, repo, redraw);
    }
    const active = (document.activeElement as HTMLElement | null)?.dataset?.focus;
    if (focused?.startsWith('repo-caret:') && active !== focused) {
      [...tbody.querySelectorAll<HTMLElement>('.caret')].find((b) => b.dataset.focus === focused)?.focus();
    }
  }

  /** The detail under a repo's row: its people in the table's window and its open work by state, queues, health and flow. */
  function repoDetail(repo: string): HTMLElement {
    const v = state.people;
    if (!v) return el('p', { class: 'muted small' }, NO_RECORDS);
    const now = Date.now();
    const f = repoFigures(v, repo, state.window, now);
    const windowLabel = windowById(state.window).label.toLowerCase();
    const p = smallLine;
    const line = repoItemLine;
    const conflicting = f.holdsConflicting ? `, ${num(f.holdsConflicting)} of them also conflict` : '';
    const competing = f.competing.map(([a, b]) => `#${a} and #${b}`).join(', ');
    return el('div', { class: 'person-detail repo-figures' },
      el('div', { class: 'wide' }, el('h4', {}, `People, ${windowLabel}`), repoPeopleTable(repo)),
      detailSection(`Open pull requests: ${num(f.open)}`, ...smallLinesOr(f.byRole.map(roleLine), 'No open pull requests.')),
      detailSection('Waiting on a maintainer',
        f.oldestOnMaintainer ? el('div', {}, el('div', { class: 'muted small' }, 'Oldest'), line(f.oldestOnMaintainer, ago(f.oldestOnMaintainer.work.since ?? f.oldestOnMaintainer.created_at, now))) : p('Nothing waits on a maintainer.'),
        p(f.firstQueue ? `${num(f.firstQueue)} await a first response, median ${durationText(f.firstQueueMedianSecs)}.` : 'Nothing awaits a first response.')),
      detailSection('Owner decisions',
        p(`${num(f.holds.length)} held for the owner${conflicting}.`),
        ...f.holds.map((i) => line(i, [`held ${ago(i.work.hold!.since, now)}`, i.work.hold!.by && `by ${i.work.hold!.by}`, i.work.hold!.category].filter(Boolean).join(', '))),
        f.parked.length ? el('div', {}, el('div', { class: 'muted small' }, 'Holds the authors placed themselves'), f.parked.map((i) => line(i))) : null),
      detailSection('Maintainer queues', p(`Workflow approvals ${num(f.approvals)} (one click each), authors who answered ${num(f.answered)}, community approved ${num(f.communityApproved)}, ready to merge ${num(f.ready)}.`)),
      detailSection('Author queues',
        p(Object.keys(f.requests).length ? `Maintainer requests: ${repoParts(f.requests)}.` : 'No maintainer requests wait on an author.'),
        p(`Community requests ${num(f.communityRequests)}, conflicts the author owes ${num(f.conflicts)}, CI failing from the pull request ${num(f.ciFromPr)}, authors who stepped back ${num(f.steppedBack)}.`)),
      detailSection('main',
        p(mainChecksLine(f.main)),
        p(`${num(f.inherited)} open pull requests fail because of main.`)),
      detailSection(`Flow, ${windowLabel}`, ...smallLinesOr(f.flow ? repoFlowLines(f.flow) : [], 'Nothing merged or closed in this window.')),
      detailSection('Staleness', ...smallLinesOr(f.staleness.map((s) => `${roleHeading(s.role)}: ${num(s.aging)} aging, ${num(s.stale)} stale.`), 'Nothing is aging or stale.')),
      detailSection('Dependencies and collisions',
        ...(f.blocked.length ? f.blocked.map((i) => line(i, `waits for ${prRefs(i.work.blocked_by)}`)) : [p('No pull request waits for another.')]),
        f.competing.length ? p(`Competing: ${competing}.`) : null));
  }

  /** The people table of a repo's detail: one row per person in the Repos table's window, or a line while it loads or when it is empty. */
  function repoPeopleTable(repo: string): HTMLElement {
    const rp = repoDetailOf(repo, state.window);
    if (rp === undefined) return smallLine('Loading its people…');
    if (!rp?.people.length) return smallLine('Nobody merged, landed, reviewed or committed here in this window.');
    const peopleCols: RankColumn[] = [
      ...RANK_COLUMNS.merged.filter((c) => ['person', 'merged', 'landed', 'lines', 'commits'].includes(c.key)),
      ...RANK_COLUMNS.maintainer.filter((c) => ['merges', 'reviews', 'review_commits'].includes(c.key)),
      RANK_COLUMNS.type.find((c) => c.key === PRODUCT)!,
    ];
    return el('table', { class: 'figures' },
      el('thead', {}, el('tr', {}, peopleCols.map((c) => el('th', { title: c.title }, c.label)))),
      el('tbody', {}, rp.people.map((row) => el('tr', {}, peopleCols.map((c) => el('td', {}, rankCell(c, row, row.windows[rp.window]!, false, '')))))));
  }

  /** An item in a repo's detail: its link, title, author and `extra`, and its state. */
  function repoItemLine(i: Item, extra = ''): HTMLElement {
    return el('div', {}, link(`#${i.number}`, i.url), ' ', i.title, ' ',
      el('span', { class: 'muted' }, [i.author, extra].filter(Boolean).join(', ')), ' ', stateChip(i.work));
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

  function render(): void {
    // Rebuilding the table mid-drag would drop the row out of the user's hand;
    // the drop renders anyway.
    const snap = state.snap;
    if (!snap || drag?.moved) return;
    renderStatus(snap);
    if (state.view === 'repos') {
      // Redrawing the table removes the focused caret, so its focus key is read first.
      const focused = (document.activeElement as HTMLElement | null)?.dataset?.focus;
      renderTable(snap);
      renderRepoCarets(focused);
    }
    else if (state.view === 'people') renderPeopleView(snap);
    else if (state.view === 'urgent') renderUrgent(snap);
  }

  /** What's on screen for the current tab, whether or not there's data yet. */
  function renderTabs(): void {
    const onPrs = state.view === 'prs';
    document.querySelectorAll<HTMLElement>('[data-view]').forEach((b) => b.setAttribute('aria-selected', String(b.dataset.view === state.view)));
    $('repos-view').hidden = state.view !== 'repos';
    $('urgent-view').hidden = state.view !== 'urgent';
    if (state.view !== 'people') $('people-view').hidden = true;
    $('prs-view').hidden = !onPrs;
    for (const id of ['pr-nav', 'crumbs', 'pr-pill', 'pr-foot']) $(id).hidden = !onPrs;
    for (const id of ['fresh-pill', 'refreshed', 'status-foot']) $(id).hidden = onPrs;
    prs.setActive(onPrs);
  }

  function setView(v: View): void {
    // Choosing the PR tab again goes back to its top, as a second Esc would.
    if (v === 'prs' && state.view === 'prs') prs.home();
    state.view = v;
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
  $('people-window').addEventListener('change', (e) => {
    state.peopleWindow = (e.target as HTMLSelectElement).value;
    save('peopleWindow', state.peopleWindow);
    redrawPeople();
  });
  $('people-repo').addEventListener('change', (e) => { state.peopleRepo = (e.target as HTMLSelectElement).value; redrawPeople(); });
  document.querySelectorAll<HTMLElement>('#people-view .seg').forEach((b) => b.addEventListener('click', () => {
    state.peopleMode = b.dataset.pmode === 'rankings' ? 'rankings' : 'work';
    save('peopleMode', state.peopleMode);
    redrawPeople();
  }));
  $('people-rank').addEventListener('change', (e) => {
    const id = (e.target as HTMLSelectElement).value;
    state.rankMode = RANK_MODES.find((m) => m.id === id)?.id ?? 'merged';
    save('rankMode', state.rankMode);
    redrawPeople();
  });
  $('people-type').addEventListener('change', (e) => {
    const key = (e.target as HTMLSelectElement).value;
    state.rankSort = { key, dir: key === 'person' ? 'asc' : 'desc' };
    save('rankSort', state.rankSort);
    redrawPeople();
  });
  $('people-group').addEventListener('change', (e) => {
    state.peopleGroup = (e.target as HTMLInputElement).checked;
    save('peopleGroup', state.peopleGroup);
    redrawPeople();
  });
  $('people-search').addEventListener('input', (e) => {
    state.peopleQuery = (e.target as HTMLInputElement).value;
    $('people-main').scrollTop = 0;
    redrawPeople();
  });
  // In the People tab, Esc clears the search and then leaves its box before
  // the shortcut handler below hides the window; ⌘F moves to the box. This
  // listener is added first, so it sees each key first.
  document.addEventListener('keydown', (e) => {
    if (state.view !== 'people') return;
    const search = $<HTMLInputElement>('people-search');
    if (e.key === 'Escape' && (state.peopleQuery || document.activeElement === search)) {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (state.peopleQuery) {
        state.peopleQuery = '';
        search.value = '';
        redrawPeople();
      } else search.blur();
    } else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'f') {
      e.preventDefault();
      search.focus();
      search.select();
    }
  });
  api.listen<PeopleView>('craft:people', (v) => {
    state.people = v;
    // The carets' tables come from the same records: open ones ask again.
    newPeopleView();
    renderPeopleBadge();
    redrawPeople();
  });
  api.invoke<PeopleView | null>('get_people').then((v) => {
    // A view the event sent first is as new; the browser build answers null.
    if (!v || state.people) return;
    state.people = v;
    renderPeopleBadge();
    redrawPeople();
  }).catch((e: unknown) => console.warn(e));
  document.addEventListener('keydown', (e) => {
    const mod = e.metaKey || e.ctrlKey;
    if (state.view === 'prs' && prs.key(e)) e.preventDefault();
    else if (e.key === 'Escape') void api.invoke('hide_window');
    else if (mod && e.key.toLowerCase() === 'r') { e.preventDefault(); refresh(); }
    else if (mod && e.key === '1') { e.preventDefault(); setView('repos'); }
    else if (mod && e.key === '2') { e.preventDefault(); setView('prs'); }
    else if (mod && e.key === '3') { e.preventDefault(); setView('urgent'); }
    else if (mod && e.key === '4') { e.preventDefault(); setView('people'); }
  });

  api.listen<Snapshot>('craft:snapshot', (s) => { state.snap = s; render(); });
  api.listen<Meta>('craft:meta', applyMeta);
  // Relative times and staleness change with the clock alone.
  const clockTimer = setInterval(render, 15_000);

  renderTabs();
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

  return () => {
    clearInterval(clockTimer);
    prs.stop();
  };
}
