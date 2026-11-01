// The PR tab: repos → a repo's PRs → one PR, plus a queue of what someone can act on
// across every repo. Third-party repos never reach it: the Rust side leaves them out.
// Reads views from the Rust side and rereads when told something changed. PR text is
// untrusted, so it only ever reaches the DOM as text nodes. app.ts owns the window
// chrome and tells this tab when it's on screen.

import {
  FILTERS, FLAG_HELP, HELP_COLUMNS, PAGE_SIZE, PR_COLUMNS, RELATED_HELP, REPO_COLUMNS, STATE_LABEL, STATUS_HELP, ago, claudeLine, compact, filterCounts, nextSort, shortName,
  relatedIsFlag, relatedLabel, relatedTitle, sorted, statePill, totals, visibleRows,
} from './pr-model.ts';
import type { Column, Finding, Group, Overview, PrFull, PrRow, Reason, RepoRow, RepoView, Sort } from './pr-model.ts';
import { $, DAY, classes, el, load, localTime, save } from './dom.ts';
import type { Api, Child } from './dom.ts';

export type Route =
  | { view: 'apps' }
  | { view: 'queue' }
  | { view: 'repo'; repo: string }
  | { view: 'pr'; repo: string; number: number; from: 'repo' | 'queue' };

/** How the window's shell drives this tab. */
export interface PrTab {
  /** The tab came on screen, or left it. */
  setActive(on: boolean): void;
  /** Back to the table of repos. */
  home(): void;
  /** Refreshes whatever's on screen. */
  refresh(): void;
  /** Handles a key meant for this tab; false leaves it to the shell. */
  key(e: KeyboardEvent): boolean;
  setHighQuality(n: number | undefined): void;
  stop(): void;
}

const num = (n: number | null | undefined): string => (n === null || n === undefined ? '—' : Number(n).toLocaleString('en-US'));
const clock = (iso: string): string => new Date(iso).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });

const TIER_LABEL: Record<string, string> = { core: 'core', frequent: 'frequent', new: 'new', bot: 'bot', regular: '' };

function count(v: number | null | undefined, tone = ''): Node {
  if (v === null || v === undefined) return el('span', { class: 'zero' }, '—');
  if (v === 0) return el('span', { class: 'zero' }, '0');
  return el('span', { class: tone }, num(v));
}

/** Starts the tab in the current document, off screen until `setActive(true)`. */
export function startPrs(api: Api, initial: Route = { view: 'apps' }): PrTab {
  const state = {
    route: initial,
    active: false,
    overview: null as Overview | null,
    highQuality: 75,
    repoSort: load<Sort>('repoSort', { key: 'landable', dir: 'desc' }),
    prSort: load<Sort>('prSort', { key: 'focus', dir: 'desc' }),
    filter: 'all',
    query: '',
    showDrafts: load('showDrafts', false),
    grouped: false,
    limit: PAGE_SIZE,
    repoView: null as RepoView | null,
    queue: null as PrRow[] | null,
    pr: null as PrFull | null,
    prMissing: false,
    /** When Claude was asked about each PR, until a newer answer lands. */
    asked: new Map<string, number>(),
    lastPrJson: '',
    /** The row just jumped to, highlighted for a moment. */
    jumped: null as number | null,
  };

  const main = $('prs-view');

  function openUrl(url: string, ev?: Event): void {
    ev?.stopPropagation();
    api.invoke('open_url', { url }).catch((e: unknown) => console.warn(e));
  }

  const ghLink = (text: string, url: string, cls = ''): HTMLElement =>
    el('span', { class: `link ${cls}`, onclick: (e: Event) => openUrl(url, e) }, text);

  function appIcon(repo: string, icon?: string | null): HTMLElement {
    if (icon) return el('img', { class: 'app-icon', src: icon, alt: '' });
    return el('span', { class: 'app-icon none', 'aria-hidden': 'true' }, shortName(repo).charAt(0).toUpperCase());
  }

  // ───────────────────────────── navigation ─────────────────────────────

  function go(route: Route): void {
    const sameRepo = state.route.view === 'repo' && route.view === 'pr';
    const backToRepo = state.route.view === 'pr' && route.view === 'repo';
    state.route = route;
    if (!sameRepo && !backToRepo) {
      state.filter = 'all';
      state.query = '';
      state.limit = PAGE_SIZE;
      state.grouped = false;
    }
    state.pr = null;
    state.prMissing = false;
    state.lastPrJson = '';
    if (route.view !== 'repo') state.repoView = route.view === 'pr' ? state.repoView : null;
    main.replaceChildren();
    main.scrollTop = 0;
    renderChrome();
    void reload();
  }

  function back(): boolean {
    const r = state.route;
    if (r.view === 'pr') {
      go(r.from === 'queue' ? { view: 'queue' } : { view: 'repo', repo: r.repo });
      return true;
    }
    if (r.view === 'repo' || r.view === 'queue') {
      go({ view: 'apps' });
      return true;
    }
    return false;
  }

  function renderChrome(): void {
    const r = state.route;
    for (const b of document.querySelectorAll<HTMLElement>('[data-pr-view]')) {
      const v = b.dataset.prView;
      const on = v === r.view || (v === 'apps' && r.view === 'repo')
        || (r.view === 'pr' && v === (r.from === 'queue' ? 'queue' : 'apps'));
      b.setAttribute('aria-selected', String(on));
    }
    const crumbs = $('crumbs');
    crumbs.replaceChildren();
    if (r.view === 'repo' || r.view === 'pr') {
      const name = shortName(r.repo);
      const sep = (): HTMLElement => el('span', { class: 'sep' }, '›');
      if (r.view === 'repo') crumbs.append(sep(), el('span', {}, name));
      else {
        crumbs.append(sep(), el('button', { onclick: () => go({ view: 'repo', repo: r.repo }) }, name), sep(), el('span', {}, `#${r.number}`));
      }
    }
  }

  // ───────────────────────────── data ─────────────────────────────

  async function reload(): Promise<void> {
    const route = state.route;
    state.overview = await api.invoke<Overview | null>('get_pr_overview');
    renderStatus();
    // Off screen, only the counts on the tab are kept up to date.
    if (route !== state.route || !state.active || !state.overview) return;
    if (route.view === 'apps') renderApps();
    else if (route.view === 'queue') {
      state.queue = await api.invoke<PrRow[]>('get_pr_queue');
      if (route === state.route && state.active) renderPrPage('queue');
    } else if (route.view === 'repo') {
      state.repoView = await api.invoke<RepoView | null>('get_pr_repo', { repo: route.repo });
      if (route === state.route && state.active) renderPrPage('repo');
    } else {
      const pr = await api.invoke<PrFull | null>('get_pr', { repo: route.repo, number: route.number });
      if (route !== state.route || !state.active) return;
      state.pr = pr;
      state.prMissing = !pr;
      const json = JSON.stringify(pr);
      if (json !== state.lastPrJson || !main.firstChild) {
        state.lastPrJson = json;
        renderPr();
      }
    }
  }

  // Keeps a busy poller from rereading faster than anyone can read. Off screen,
  // all that's rereading is the tab's counts, so much less often.
  let timer: ReturnType<typeof setTimeout> | null = null;
  let jumpTimer: ReturnType<typeof setTimeout> | null = null;
  function changed(): void {
    if (timer) return;
    timer = setTimeout(() => {
      timer = null;
      void reload().catch((e: unknown) => console.warn(e));
    }, state.active ? 1500 : 15000);
  }

  // ───────────────────────────── status ─────────────────────────────

  function renderStatus(): void {
    const o = state.overview;
    if (!o) return;
    const s = o.status;
    const open = o.repos.reduce((sum, r) => sum + (r.listed ? r.open : r.open_hint ?? 0), 0);
    const queued = o.repos.reduce((sum, r) => sum + r.landable + r.promising + r.reviewable + r.ci_not_run, 0);
    $('apps-count').textContent = open ? compact(open) : '';
    $('queue-count').textContent = queued ? compact(queued) : '';
    $('prs-count').textContent = queued ? compact(queued) : '';
    $('prs-count').title = `${num(queued)} PRs someone can act on, of ${num(open)} open`;

    const pill = statePill(s, o.repos);
    const p = $('pr-pill');
    p.className = `pill ${pill.cls}`;
    p.textContent = pill.text;
    p.title = pill.title;

    const repoErrors = o.repos.filter((r) => r.error).map((r) => `${shortName(r.repo)}: ${r.error}`);
    const banner = $('pr-banner');
    const problems = [s.error, ...repoErrors].filter(Boolean);
    banner.hidden = !state.active || problems.length === 0;
    banner.textContent = problems.join(' · ');

    const paused = s.paused_until && Date.parse(s.paused_until) > Date.now();
    $('activity').textContent = paused
      ? `Paused until ${clock(s.paused_until!)}`
      : s.fetching ?? (s.next_at && Date.parse(s.next_at) > Date.now() ? `Next fetch ${clock(s.next_at)}` : 'Idle');
    const hist = s.histories_pending ? ` · counting merges in ${s.histories_pending} repos` : '';
    $('backlog').textContent = s.backlog ? `${num(s.backlog)} PRs left to inspect${hist}` : hist.replace(' · ', '');
    const budget = $('pr-budget');
    budget.textContent = `GitHub ${num(s.points_last_hour)}/${num(s.max_points_per_hour)} pts this hour${s.rate_remaining === null || s.rate_remaining === undefined ? '' : `, ${num(s.rate_remaining)} left`}`;
    budget.title = `Token: ${s.token_source ?? 'none'}. The budget is shared with the Downloads tab's polling.`;
    budget.classList.toggle('bad', s.token_rejected);
    const c = claudeLine(s.claude ?? ({} as Overview['status']['claude']));
    const cl = $('claude');
    cl.textContent = c.text;
    cl.title = c.title;
    cl.classList.toggle('bad', c.bad);
  }

  // ───────────────────────────── tables ─────────────────────────────

  function headers<T>(columns: Column<T>[], sort: Sort, onSort: (s: Sort) => void, left: (key: string) => boolean): HTMLElement[] {
    const rows: HTMLElement[] = [];
    if (columns.some((c) => c.group)) {
      const groups = el('tr', { class: 'groups' });
      let i = 0;
      while (i < columns.length) {
        const g = columns[i]!.group;
        let span = 1;
        while (i + span < columns.length && columns[i + span]!.group === g) span++;
        groups.append(el('th', { colspan: span, class: g ? 'group-start' : '' }, g ?? ''));
        i += span;
      }
      rows.push(groups);
    }
    const cols = el('tr', { class: 'cols' });
    columns.forEach((c, i) => {
      const startsGroup = c.group && columns[i - 1]?.group !== c.group;
      cols.append(el('th', {
        class: classes(left(c.key) && 'left', startsGroup && 'group-start', sort.key === c.key && 'sorted', sort.key === c.key && sort.dir === 'asc' && 'asc'),
        title: c.title,
        'data-key': c.key,
        onclick: () => onSort(nextSort(sort, c)),
      }, c.label));
    });
    rows.push(cols);
    return rows;
  }

  function appCell(r: RepoRow, c: Column<RepoRow>): Node | string {
    const v = c.value(r);
    switch (c.key) {
      case 'name':
        return el('span', { class: 'repo', title: r.error ? `Last attempt failed: ${r.error}` : r.repo },
          el('span', { class: 'dot' }), appIcon(r.repo, r.icon), shortName(r.repo));
      case 'category':
        return v === null ? el('span', { class: 'muted' }, '—') : el('span', { class: 'category' }, String(v));
      case 'open':
        if (r.partial) return el('span', { class: 'muted', title: 'Still listing' }, `${num(r.open)} of ~${num(v as number)}`);
        return r.listed ? count(r.open) : el('span', { class: 'muted', title: 'From the Downloads tab; not listed here yet' }, v === null ? '—' : `~${num(v as number)}`);
      case 'inspected':
        return r.listed ? el('span', { class: r.inspected >= r.open ? '' : 'muted' }, `${num(r.inspected)}`) : count(null);
      case 'landable': case 'approved': case 'easy':
        return count(v as number | null, 'n-good');
      case 'promising':
        return count(v as number | null, 'n-accent');
      case 'reviewable': case 'high_quality': case 'ci_not_run':
        return count(v as number | null);
      case 'needs_work': case 'hard': case 'conflicts': case 'ci_failing': case 'medium': case 'core': case 'frequent': case 'new': case 'groups': case 'interesting':
        return count(v as number | null);
      case 'rabbit_flagged': case 'security':
        return count(v as number | null, 'n-bad');
      case 'rabbit_nits': case 'security_nits':
        return count(v as number | null, 'n-warn');
      case 'oldest_at': {
        const old = r.oldest_at && Date.now() - Date.parse(r.oldest_at) > 14 * DAY;
        return el('span', { class: old ? 'old' : '', title: localTime(r.oldest_at) }, ago(r.oldest_at));
      }
      case 'newest_at': case 'list_at': {
        const iso = c.key === 'newest_at' ? r.newest_at : r.list_at;
        return el('span', { title: localTime(iso) }, ago(iso));
      }
      default:
        return String(v ?? '');
    }
  }

  function renderApps(): void {
    const o = state.overview!;
    if (!o.repos.length) {
      main.replaceChildren(el('div', { class: 'empty' }, 'No first-party repos in the config.'));
      return;
    }
    const rows = sorted(o.repos, state.repoSort, REPO_COLUMNS);
    const scroll = main.scrollTop;
    const body = el('tbody');
    for (const r of rows) {
      const tr = el('tr', {
        class: classes(r.error && 'error', !r.listed && 'unlisted'),
        'data-repo': r.repo,
        onclick: () => go({ view: 'repo', repo: r.repo }),
      });
      REPO_COLUMNS.forEach((c, i) => {
        const startsGroup = c.group && REPO_COLUMNS[i - 1]?.group !== c.group;
        tr.append(el('td', { class: classes(c.text && 'left', startsGroup && 'group-start') }, appCell(r, c)));
      });
      body.append(tr);
    }
    const sums = totals(o.repos);
    const foot = el('tr');
    REPO_COLUMNS.forEach((c, i) => {
      const startsGroup = c.group && REPO_COLUMNS[i - 1]?.group !== c.group;
      const cell = c.key === 'name' ? 'All repos' : c.key in sums ? num(sums[c.key]) : '';
      foot.append(el('td', { class: classes(c.text && 'left', startsGroup && 'group-start') }, cell));
    });
    main.replaceChildren(el('table', { id: 'apps-table' },
      el('thead', {}, headers(REPO_COLUMNS, state.repoSort, (s) => {
        state.repoSort = s;
        save('repoSort', s);
        renderApps();
      }, (k) => k === 'name')),
      body, el('tfoot', {}, foot)));
    main.scrollTop = scroll;
  }

  function stateBadge(state: PrRow['state']): HTMLElement {
    return el('span', { class: `state ${state}` }, STATE_LABEL[state]);
  }

  /** Flags that don't already show in their own column or as the status. */
  function flagChips(r: { state: PrRow['state']; approved: boolean; approval_stale: boolean; conflicted: boolean; approved_by: string[] }, related: PrRow['related']): Node[] {
    const chips: Node[] = [];
    if (r.approved && !r.approval_stale) chips.push(el('span', { class: 'flag approved', title: r.approved_by.length ? `Approved by ${r.approved_by.join(', ')}` : 'Approved' }, 'Approved'));
    if (r.approval_stale) chips.push(el('span', { class: 'flag stale', title: 'The approval is of an older commit' }, 'Stale approval'));
    if (r.conflicted && r.state !== 'conflicted') chips.push(el('span', { class: 'flag conflicted' }, 'Conflicted'));
    const kinds = [...new Set(related.filter((x) => relatedIsFlag(x.relation)).map((x) => x.relation))];
    for (const k of kinds) chips.push(el('span', { class: 'chip group' }, relatedLabel(k)));
    return chips;
  }

  /** Jumps to a PR's row, clearing whatever hides it; opens it if it isn't in this table. */
  function jumpTo(repo: string, number: number, from: 'repo' | 'queue'): void {
    const rows = from === 'queue' ? state.queue ?? [] : state.repoView?.prs ?? [];
    const target = rows.find((x) => x.number === number && x.repo === repo);
    if (!target) {
      go({ view: 'pr', repo, number, from });
      return;
    }
    if (!main.querySelector(`tr[data-number="${number}"]`)) {
      state.filter = 'all';
      state.query = '';
      state.grouped = false;
      if (target.is_draft) state.showDrafts = true;
      const columns = from === 'queue' ? QUEUE_COLUMNS : PR_COLUMNS;
      const order = sorted(visibleRows(rows, 'all', '', state.showDrafts), state.prSort, columns);
      state.limit = Math.max(state.limit, order.findIndex((x) => x.number === number) + 1);
      const search = main.querySelector<HTMLInputElement>('input[type="search"]');
      if (search) search.value = '';
      renderPrPage(from);
    }
    state.jumped = number;
    const tr = main.querySelector<HTMLElement>(`tr[data-number="${number}"]`);
    tr?.classList.add('jumped');
    tr?.scrollIntoView?.({ block: 'center' });
    if (jumpTimer) clearTimeout(jumpTimer);
    jumpTimer = setTimeout(() => {
      state.jumped = null;
      for (const n of main.querySelectorAll('tr.jumped')) n.classList.remove('jumped');
    }, 2000);
  }

  /** Green only if GitHub counts it: approvals from people without write access don't. */
  function approvers(r: PrRow): HTMLElement {
    const names = r.approved_by.join(', ');
    if (!r.approved) return el('span', { class: 'muted', title: 'Approved, but GitHub doesn\'t count it: usually because they don\'t have write access' }, names);
    if (r.approval_stale) return el('span', { class: 'n-warn', title: 'Approved an older commit' }, names);
    return el('span', { class: 'n-good' }, names);
  }

  function relatedCell(r: PrRow, from: 'repo' | 'queue'): Node {
    if (!r.related.length) return el('span', { class: 'zero' }, '—');
    const shown = r.related.slice(0, RELATED_SHOWN);
    const links = shown.map((x) => el('button', {
      class: 'rel',
      'data-rel': x.number,
      title: `${relatedTitle(x)}\nClick to jump to it, ⌘-click to open it`,
      onclick: (e: MouseEvent) => {
        e.stopPropagation();
        if (e.metaKey || e.ctrlKey) go({ view: 'pr', repo: r.repo, number: x.number, from });
        else jumpTo(r.repo, x.number, from);
      },
    }, `#${x.number}`));
    const more = r.related.length - shown.length;
    return el('span', { class: 'rels' }, links, more > 0 ? el('span', { class: 'muted', title: 'Open the PR to see them all' }, `+${more}`) : null);
  }

  function prCell(r: PrRow, c: Column<PrRow>): Node | string {
    switch (c.key) {
      case 'focus':
        return stateBadge(r.state);
      case 'title': {
        const chips: Node[] = [];
        if (r.interesting) chips.push(el('span', { class: 'chip star' }, '★ interesting'));
        chips.push(...flagChips(r, r.related));
        for (const l of r.labels.slice(0, 3)) chips.push(el('span', { class: 'chip' }, l));
        return el('div', {},
          el('span', { class: 'pr-num' }, `#${r.number}`),
          el('span', { class: 'pr-title' }, r.title),
          chips.length ? el('span', { class: 'chips' }, chips) : null,
          r.summary ? el('div', { class: 'summary' }, r.summary) : null);
      }
      case 'author':
        return el('span', {}, r.author, TIER_LABEL[r.tier] ? el('span', { class: `tier ${r.tier}` }, TIER_LABEL[r.tier]) : null);
      case 'landed_total':
        return count(r.landed_total);
      case 'related':
        return relatedCell(r, state.route.view === 'queue' ? 'queue' : 'repo');
      case 'approved_by':
        return r.approved_by.length ? approvers(r) : el('span', { class: 'zero' }, '—');
      case 'ease':
        return el('span', { class: `ease-${r.ease}` }, r.ease);
      case 'quality':
        return r.inspected ? el('span', { class: r.high_quality ? 'n-good' : r.quality < 50 ? 'n-bad' : '' }, r.quality) : el('span', { class: 'muted', title: 'Partial until inspected' }, `~${r.quality}`);
      case 'ci':
        return el('span', { class: r.ci === 'failure' ? 'n-bad' : r.ci === 'success' ? 'n-good' : 'muted' }, r.ci === 'failure' ? 'fails' : r.ci === 'success' ? 'passes' : r.ci);
      case 'rabbit':
        if (!r.inspected) return count(null);
        return el('span', {}, r.rabbit_flags ? el('span', { class: 'n-bad' }, `${r.rabbit_flags} flag${r.rabbit_flags > 1 ? 's' : ''}`) : null,
          r.rabbit_flags && r.rabbit_nits ? ' ' : null,
          r.rabbit_nits ? el('span', { class: 'n-warn' }, `${r.rabbit_nits} nit${r.rabbit_nits > 1 ? 's' : ''}`) : null,
          !r.rabbit_flags && !r.rabbit_nits ? el('span', { class: 'zero' }, 'clean') : null);
      case 'security': {
        if (!r.inspected) return count(null);
        const flags = r.security_flags + r.sensitive;
        if (flags) return el('span', { class: 'n-bad' }, r.sensitive && !r.security_flags ? 'sensitive' : `${flags} flag${flags > 1 ? 's' : ''}`);
        if (r.security_nits) return el('span', { class: 'n-warn' }, `${r.security_nits} nit${r.security_nits > 1 ? 's' : ''}`);
        return el('span', { class: 'zero' }, '—');
      }
      case 'size':
        return el('span', {}, el('span', { class: 'add' }, `+${compact(r.additions)}`), ' ', el('span', { class: 'del' }, `−${compact(r.deletions)}`));
      case 'changed_files':
        return num(r.changed_files);
      case 'updated_at':
        return el('span', { title: localTime(r.updated_at) }, ago(r.updated_at));
      case 'created_at': {
        const old = r.created_at && Date.now() - Date.parse(r.created_at) > 14 * DAY;
        return el('span', { class: old ? 'old' : '', title: localTime(r.created_at) }, ago(r.created_at));
      }
      default:
        return '';
    }
  }

  function prRow(r: PrRow, from: 'repo' | 'queue', columns: Column<PrRow>[]): HTMLElement {
    const tr = el('tr', {
      'data-number': r.number,
      onclick: () => go({ view: 'pr', repo: r.repo, number: r.number, from }),
    });
    for (const c of columns) {
      tr.append(el('td', { class: classes(c.key === 'title' && 'pr-title-cell', (c.text || c.key === 'focus' || c.key === 'related') && 'left') }, prCell(r, c)));
    }
    return tr;
  }

  const RELATED_SHOWN = 6;
  const QUEUE_COLUMNS: Column<PrRow>[] = [
    PR_COLUMNS[0]!,
    { key: 'app', label: 'App', text: true, value: (r) => shortName(r.repo) },
    ...PR_COLUMNS.slice(1),
  ];

  /** The explanation behind the (?) button: what every status and flag means. */
  function helpPanel(): HTMLElement {
    const close = (): void => { closeHelp(); };
    const table = (head: string[], rows: (Child | Child[])[][]): HTMLElement => el('table', { class: 'help-table' },
      el('thead', {}, el('tr', {}, head.map((h) => el('th', { class: 'left' }, h)))),
      el('tbody', {}, rows.map((cells) => el('tr', {}, cells.map((c) => el('td', { class: 'left' }, c))))));
    return el('div', { class: 'help-overlay', id: 'help-panel', role: 'dialog', 'aria-label': 'What the statuses mean', onclick: (e: Event) => { if (e.target === e.currentTarget) close(); } },
      el('div', { class: 'help-box' },
        el('div', { class: 'help-head' }, el('h2', {}, 'What the statuses mean'), el('button', { class: 'btn', id: 'help-close', onclick: close }, 'Close')),
        el('p', {}, 'Each PR has one status, worked out from independent facts. Read the table from the top: the first row that matches wins, and – means that column doesn\'t matter. A problem is any flag marked Yes below. Running means CI or CodeRabbit is still going, or GitHub hasn\'t worked out mergeability.'),
        table(['Status', ...HELP_COLUMNS, 'What it means'], STATUS_HELP.map((h) => [stateBadge(h.state), ...h.when, h.meaning])),
        el('h3', {}, 'Flags'),
        el('p', {}, 'Flags are independent, and a PR shows all that apply. Approved is a flag, not a status: an approved PR with conflicts is Conflicted, with the Approved flag beside it.'),
        table(['Flag', 'What it means', 'Counts as a problem?'], FLAG_HELP.map((f) => [el('span', { class: `flag ${f.cls}` }, f.label), f.meaning, f.problem])),
        el('h3', {}, 'Possibly related'),
        el('p', {}, 'These never change a status: they are judgments or loose matches, and in a duplicate pair one PR is usually the one you want. Links go both ways. Click a number to jump to that row, or ⌘-click to open that PR. Hover for how sure anyone is and why.'),
        table(['Kind', 'Shown beside the title?', 'Found by', 'What it means'], RELATED_HELP.map((h) => [h.label, h.flag ? 'Yes' : 'No', h.source, h.meaning]))));
  }

  function closeHelp(): boolean {
    const open = document.getElementById('help-panel');
    open?.remove();
    return Boolean(open);
  }

  function renderTable(slot: HTMLElement, rows: PrRow[], from: 'repo' | 'queue', groups: Group[]): void {
    const columns = from === 'queue' ? QUEUE_COLUMNS : PR_COLUMNS;
    const cell = (c: Column<PrRow>, r: PrRow): Node | string => (c.key === 'app' ? shortName(r.repo) : prCell(r, c));
    const row = (r: PrRow): HTMLElement => {
      const tr = prRow(r, from, []);
      if (r.number === state.jumped) tr.classList.add('jumped');
      for (const c of columns) tr.append(el('td', { class: classes(c.key === 'title' && 'pr-title-cell', (c.text || c.key === 'focus' || c.key === 'related') && 'left') }, cell(c, r)));
      return tr;
    };
    const visible = sorted(visibleRows(rows, state.filter, state.query, state.showDrafts), state.prSort, columns);
    const head = (): HTMLElement => el('thead', {}, headers(columns, state.prSort, (s) => {
      state.prSort = s;
      save('prSort', s);
      renderTable(slot, rows, from, groups);
    }, (k) => k === 'focus' || k === 'title' || k === 'author' || k === 'app' || k === 'approved_by' || k === 'related'));
    if (!visible.length) {
      slot.replaceChildren(el('div', { class: 'empty' }, rows.length ? 'No PRs match.' : from === 'queue'
        ? 'Nothing to act on yet. PRs show up here once they have been inspected and nothing is in the way.'
        : 'No open PRs, or not listed yet.'));
      return;
    }
    if (state.grouped && groups.length) {
      const byNumber = new Map(visible.map((r) => [r.number, r]));
      const parts: Node[] = [];
      const grouped = groups
        .map((g) => ({ g, members: g.numbers.flatMap((n) => byNumber.get(n) ?? []) }))
        .filter((x) => x.members.length >= 1)
        .sort((a, b) => b.members.length - a.members.length);
      for (const { g, members } of grouped) {
        parts.push(el('div', { class: 'group-head' },
          el('span', { class: 'chip group' }, g.relation), ' ', g.title,
          g.source === 'claude' ? el('span', { class: 'chip star', title: 'Grouped by Claude' }, 'Claude') : null,
          g.note ? el('span', { class: 'note' }, g.note) : null));
        parts.push(el('table', { class: 'pr-table' }, el('tbody', {}, members.map(row))));
      }
      if (!parts.length) parts.push(el('div', { class: 'empty' }, 'No possibly related PRs match.'));
      slot.replaceChildren(...parts);
      return;
    }
    const shown = visible.slice(0, state.limit);
    const parts: Node[] = [el('table', { class: 'pr-table' }, head(), el('tbody', {}, shown.map(row)))];
    if (visible.length > shown.length) {
      parts.push(el('button', {
        class: 'more',
        onclick: () => {
          state.limit += PAGE_SIZE;
          renderTable(slot, rows, from, groups);
        },
      }, `Show more (${num(visible.length - shown.length)} left)`));
    }
    slot.replaceChildren(...parts);
    syncControlsHeight();
  }

  /** The column headers stick just below the filter bar, however tall it has wrapped. */
  function syncControlsHeight(): void {
    const bar = main.querySelector<HTMLElement>('.controls');
    if (bar?.offsetHeight) main.style.setProperty('--controls-height', `${bar.offsetHeight}px`);
  }

  /** Controls stay put across rereads, so typing in the search box keeps its focus. */
  function renderPrPage(from: 'repo' | 'queue'): void {
    const rows = from === 'queue' ? state.queue ?? [] : state.repoView?.prs ?? [];
    const groups = from === 'repo' ? state.repoView?.groups ?? [] : [];
    let bar = main.querySelector<HTMLElement>('.controls');
    let slot = main.querySelector<HTMLElement>('#pr-slot');
    if (!bar || !slot) {
      bar = el('div', { class: 'controls' });
      slot = el('div', { id: 'pr-slot' });
      main.replaceChildren(bar, slot);
    }
    const counts = filterCounts(rows, state.showDrafts);
    const search = bar.querySelector<HTMLInputElement>('input[type="search"]');
    const chips = FILTERS.filter((f) => f.id === 'all' || (counts[f.id] ?? 0) > 0 || f.id === state.filter).map((f) =>
      el('button', {
        class: 'chip-button',
        'aria-pressed': String(state.filter === f.id),
        'data-filter': f.id,
        onclick: () => {
          state.filter = f.id;
          state.limit = PAGE_SIZE;
          renderPrPage(from);
        },
      }, f.label, el('span', { class: 'badge' }, compact(counts[f.id] ?? 0))));
    const extras: Node[] = [];
    if (from === 'repo' && groups.length) {
      const groupBox = el('input', {
        type: 'checkbox',
        id: 'group-toggle',
        onchange: (e: Event) => {
          state.grouped = (e.target as HTMLInputElement).checked;
          renderPrPage(from);
        },
      });
      groupBox.checked = state.grouped;
      extras.push(el('label', { title: 'Show possibly related PRs together' }, groupBox, `Group possibly related (${groups.length})`));
    }
    if (from === 'repo') {
      const draftBox = el('input', {
        type: 'checkbox',
        id: 'draft-toggle',
        onchange: (e: Event) => {
          state.showDrafts = (e.target as HTMLInputElement).checked;
          save('showDrafts', state.showDrafts);
          state.limit = PAGE_SIZE;
          renderPrPage(from);
        },
      });
      draftBox.checked = state.showDrafts;
      extras.push(el('label', {}, draftBox, 'Show drafts'));
    }
    const input = search ?? el('input', {
      type: 'search',
      placeholder: 'Search title, #, author…',
      'aria-label': 'Search PRs',
      oninput: (e: Event) => {
        state.query = (e.target as HTMLInputElement).value;
        state.limit = PAGE_SIZE;
        const live = state.route.view === 'queue' ? 'queue' : 'repo';
        renderPrPage(live);
      },
    });
    const help = el('button', {
      class: 'help-button',
      id: 'help',
      title: 'What the statuses mean',
      'aria-label': 'What the statuses mean',
      onclick: () => {
        if (!closeHelp()) $('app').append(helpPanel());
      },
    }, '?');
    bar.replaceChildren(...chips, ...extras, input, help);
    if (search) search.focus();
    renderTable(slot, rows, from, groups);
  }

  // ───────────────────────────── one PR ─────────────────────────────

  function reasonList(reasons: Reason[], showPoints: boolean): HTMLElement {
    return el('ul', { class: 'reasons' }, reasons.map((r) =>
      el('li', {}, el('span', {}, r.text),
        showPoints && r.points !== 0
          ? el('span', { class: `pts ${r.points > 0 ? 'up' : 'down'}` }, `${r.points > 0 ? '+' : ''}${r.points}`)
          : null)));
  }

  function findingItem(f: Finding): HTMLElement {
    const where = f.path + (f.line ? `:${f.line}` : '');
    return el('li', { class: 'finding' },
      el('div', {},
        el('span', { class: `sev ${f.severity}` }, f.severity),
        f.security ? el('span', { class: 'sev security' }, 'security') : null,
        f.url ? ghLink(f.title || '(untitled)', f.url) : f.title),
      el('div', { class: 'where' }, [where, f.category, f.effort].filter(Boolean).join(' · ')),
      f.snippet ? el('div', { class: 'snippet' }, f.snippet) : null);
  }

  function claudeCard(pr: PrFull): HTMLElement {
    const key = `${pr.repo}#${pr.number}`;
    const c = state.overview?.status.claude;
    const a = pr.analysis;
    const ask = (deep: boolean): void => {
      state.asked.set(key, Date.now());
      void api.invoke('analyze_pr', { repo: pr.repo, number: pr.number, deep });
      renderPr();
    };
    const askedAt = state.asked.get(key);
    // Answered, or it failed or timed out: the CLI gives up within five minutes.
    if (askedAt && ((a?.at && Date.parse(a.at) >= askedAt) || c?.last_error || Date.now() - askedAt > 6 * 60000)) state.asked.delete(key);
    const asked = state.asked.has(key);
    const can = c?.enabled !== false && c?.available !== false;
    const buttons = el('div', { class: 'pr-actions' },
      el('button', { class: 'btn', disabled: !can || asked, 'data-ask': 'quick', onclick: () => ask(false), title: 'Reads the description, files and CodeRabbit findings' }, a ? 'Reassess' : 'Assess now'),
      el('button', { class: 'btn', disabled: !can || asked, 'data-ask': 'deep', onclick: () => ask(true), title: 'Also reads the diff' }, 'Read the diff'),
      asked ? el('span', { class: 'muted' }, 'Asked Claude…') : null);
    const line = (label: string, text: string, cls = ''): Node | null =>
      (text ? el('p', { class: cls }, el('span', { class: 'label' }, label), text) : null);
    const body = a && a.summary
      ? el('div', { class: 'claude-text' },
          el('p', {}, a.summary),
          a.interesting && a.why_interesting ? line('★ Interesting:', a.why_interesting) : null,
          line('Look first at:', a.review_focus),
          line('Risk:', a.risk),
          line('Security:', a.security_note, 'notice'),
          !pr.analysis_current ? el('p', { class: 'notice' }, 'Written before the latest commits.') : null,
          a.diff_truncated ? el('p', { class: 'notice' }, 'The diff was too long to send whole, so this only saw the start of it.') : null)
      : el('p', { class: 'muted' }, can ? 'Not assessed yet. Claude works through the best PRs first, within the limits in the config.' : c?.enabled === false ? 'Claude is turned off in the config.' : 'The Claude CLI wasn\'t found.');
    return el('section', { class: 'card', id: 'claude-card' },
      el('h2', {}, 'Claude', el('span', { title: 'Advisory. It can make a PR look harder, but it can\'t change its status or raise its quality.' },
        a?.at ? `${a.deep ? 'read the diff' : 'metadata only'} · ${ago(a.at)}` : '')),
      body, buttons);
  }

  function renderPr(): void {
    const pr = state.pr;
    const route = state.route;
    if (route.view !== 'pr') return;
    if (!pr) {
      main.replaceChildren(el('div', { class: 'empty' }, state.prMissing ? 'This PR is no longer open.' : 'Loading…'));
      return;
    }
    const scroll = main.scrollTop;
    const a = pr.assessment;
    const d = pr.detail;
    const qualityMeter = el('div', { class: `meter ${a.quality >= (state.highQuality) ? 'high' : a.quality < 50 ? 'low' : ''}` },
      el('span', { style: `width:${a.quality}%` }));

    const verdict = el('section', { class: 'card', id: 'verdict' },
      el('div', { class: 'verdicts' },
        el('div', { class: 'verdict' }, el('span', { class: `big ${a.state}` }, STATE_LABEL[a.state]),
          el('div', { class: 'chips' }, flagChips({ state: a.state, approved: a.approved, approval_stale: a.approval_stale, conflicted: a.conflicted, approved_by: pr.approved_by }, pr.related_refs)),
          el('ul', { class: 'reasons' }, a.state_reasons.map((r) => el('li', {}, r)))),
        el('div', { class: 'verdict' }, el('span', { class: `big ${a.ease}` }, `${a.ease} to land`),
          a.ease_raised_by_claude ? el('div', { class: 'notice' }, 'Claude thinks it\'s harder than the numbers suggest.') : null,
          reasonList(a.ease_reasons, true)),
        el('div', { class: 'verdict' }, el('span', { class: 'big' }, `Quality ${a.quality}${a.inspected ? '' : ' (partial)'}`), qualityMeter,
          reasonList(a.quality_reasons, true))));

    const left: (Node | null)[] = [verdict, claudeCard(pr)];

    if (a.security_reasons.length || a.security_nits) {
      left.push(el('section', { class: 'card', id: 'security' }, el('h2', {}, 'Security'),
        el('ul', { class: 'reasons' }, a.security_reasons.map((r) => el('li', {}, r))),
        a.sensitive_paths.length ? el('ul', { class: 'rows' }, a.sensitive_paths.map((p) => el('li', {}, el('span', { class: 'name' }, p)))) : null));
    }

    if (d) {
      const active = d.findings.filter((f) => !f.resolved && !f.outdated);
      const order = { critical: 0, major: 1, minor: 2, trivial: 3 };
      active.sort((x, y) => order[x.severity] - order[y.severity]);
      const done = d.findings.length - active.length;
      const rabbitState = { reviewed: 'reviewed', in_progress: 'still reviewing', skipped: 'skipped this PR', none: 'hasn\'t reviewed this' }[d.rabbit];
      left.push(el('section', { class: 'card', id: 'findings' },
        el('h2', {}, 'CodeRabbit', el('span', {}, `${rabbitState}${done ? ` · ${done} resolved or outdated` : ''}`)),
        active.length
          ? el('ul', { class: 'findings' }, active.map(findingItem))
          : el('p', { class: 'muted' }, d.rabbit === 'reviewed' ? 'Nothing unresolved.' : 'No findings.'),
        d.threads_total > d.findings.length + d.human_threads_open
          ? el('p', { class: 'muted' }, `Only the first threads were read; there are ${d.threads_total} in all.`) : null));
      if (d.body) left.push(el('section', { class: 'card' }, el('h2', {}, 'Description'), el('div', { class: 'body' }, d.body)));
      if (d.comments.length) {
        left.push(el('section', { class: 'card' }, el('h2', {}, 'Latest comments'),
          d.comments.map((c) => el('div', { class: 'comment' },
            el('span', { class: 'who' }, c.author), ' ', el('span', { class: 'muted' }, ago(c.at)), '\n', c.text))));
      }
    } else {
      left.push(el('section', { class: 'card' }, el('h2', {}, 'CodeRabbit'),
        el('p', { class: 'muted' }, 'This PR hasn\'t been inspected yet, so its findings, files and checks are unknown.')));
    }

    const au = pr.author;
    const kv = (rows: [string, Child][]): HTMLElement =>
      el('dl', { class: 'kv' }, rows.flatMap(([k, v]) => [el('dt', {}, k), el('dd', {}, v)]));
    const right: (Node | null)[] = [
      el('section', { class: 'card', id: 'author' }, el('h2', {}, 'Author'),
        el('div', { class: 'author-card' },
          pr.avatar.startsWith('https://avatars.githubusercontent.com/') ? el('img', { class: 'avatar', src: pr.avatar, alt: '' }) : null,
          el('div', {}, ghLink(au.login, `https://github.com/${encodeURIComponent(au.login)}`),
            TIER_LABEL[au.tier] ? el('span', { class: `tier ${au.tier}` }, TIER_LABEL[au.tier]) : null)),
        kv([
          ['Landed across apps', `${num(au.landed_total)}${au.counts_complete ? '' : ' so far'}`],
          [`Landed in ${shortName(pr.repo)}`, num(au.landed_here)],
          ['Repo access', au.permission ? au.permission.toLowerCase() : 'none'],
          ['In people.toml', au.listed ? 'yes' : 'no'],
          ['GitHub says', pr.association.replaceAll('_', ' ').toLowerCase()],
        ])),
    ];

    if (pr.groups.length) {
      right.push(el('section', { class: 'card', id: 'overlaps' }, el('h2', {}, 'Possibly related'),
        pr.groups.map((g) => el('div', {}, el('span', { class: 'chip group' }, relatedLabel(g.relation)), ' ', g.title,
          g.note ? el('div', { class: 'muted' }, g.note) : null)),
        el('ul', { class: 'rows' }, pr.related.map((r) => {
          const ref = pr.related_refs.find((x) => x.number === r.number);
          return el('li', {},
            el('span', { class: 'name link', 'data-rel': r.number, onclick: () => go({ view: 'pr', repo: r.repo, number: r.number, from: route.from }), title: ref ? relatedTitle(ref) : r.title }, `#${r.number} ${r.title}`),
            el('span', { class: 'fig' }, stateBadge(r.state)));
        }))));
    }

    if (d) {
      const fails = d.checks.filter((c) => /FAIL|ERROR|TIMED|CANCEL|ACTION/.test(c.state));
      const passed = d.checks.filter((c) => /SUCCESS|NEUTRAL/.test(c.state)).length;
      const skipped = d.checks.filter((c) => c.state === 'SKIPPED').length;
      const running = d.checks.length - fails.length - passed - skipped;
      right.push(el('section', { class: 'card', id: 'checks' },
        el('h2', {}, 'Checks', el('span', {}, a.ci)),
        el('p', { class: 'muted' }, `${passed} passed, ${fails.length} failed, ${running} running, ${skipped} skipped`),
        fails.length ? el('ul', { class: 'rows' }, fails.map((c) => el('li', {},
          c.url?.startsWith('https://github.com/') ? ghLink(c.name, c.url, 'name n-bad') : el('span', { class: 'name n-bad' }, c.name)))) : null));
      if (d.reviews.length || pr.review_decision) {
        right.push(el('section', { class: 'card' }, el('h2', {}, 'Reviews', el('span', {}, (pr.review_decision ?? '').replaceAll('_', ' ').toLowerCase())),
          el('ul', { class: 'rows' }, d.reviews.map((r) => el('li', {}, el('span', { class: 'name' }, r.author), el('span', { class: 'fig muted' }, `${r.state.replaceAll('_', ' ').toLowerCase()} ${ago(r.at)}`))))));
      }
      if (d.linked_issues.length) {
        right.push(el('section', { class: 'card' }, el('h2', {}, 'Closes'),
          el('ul', { class: 'rows' }, d.linked_issues.map((i) => el('li', {}, ghLink(`#${i.number} ${i.title}`, `https://github.com/${pr.repo}/issues/${i.number}`, 'name'))))));
      }
      const files = [...d.files].sort((x, y) => y.additions + y.deletions - x.additions - x.deletions);
      right.push(el('section', { class: 'card', id: 'files' },
        el('h2', {}, 'Files', el('span', {}, `${num(pr.changed_files)}${a.tests_touched ? ' · has tests' : ''}${a.deps_changed ? ' · deps changed' : ''}`)),
        el('ul', { class: 'rows' }, files.slice(0, 40).map((f) => el('li', { title: f.path },
          el('span', { class: 'name' }, f.path),
          el('span', { class: 'fig' }, el('span', { class: 'add' }, `+${compact(f.additions)}`), ' ', el('span', { class: 'del' }, `−${compact(f.deletions)}`))))),
        pr.changed_files > 40 ? el('p', { class: 'muted' }, `Largest 40 of ${files.length === pr.changed_files ? pr.changed_files : `the ${files.length} read, of ${pr.changed_files}`}.`) : null));
    }

    main.replaceChildren(el('div', { class: 'pr-page' },
      el('div', { class: 'pr-head' },
        el('h1', {}, el('span', { class: 'pr-num' }, `#${pr.number}`), pr.title),
        el('div', { class: 'pr-meta' },
          el('span', { class: 'repo' }, appIcon(pr.repo, pr.icon), shortName(pr.repo)),
          el('span', { class: 'add' }, `+${num(pr.additions)}`), el('span', { class: 'del' }, `−${num(pr.deletions)}`),
          el('span', {}, `${num(pr.changed_files)} files`),
          el('span', { title: localTime(pr.created_at) }, `opened ${ago(pr.created_at)} ago`),
          el('span', { title: localTime(pr.updated_at) }, `updated ${ago(pr.updated_at)} ago`),
          el('span', {}, `${pr.head_ref} → ${pr.base_ref}${pr.cross_repo ? ' (fork)' : ''}`),
          pr.approved_by.length ? el('span', { class: 'n-good', id: 'approved-by' }, `Approved by ${pr.approved_by.join(', ')}`) : null,
          pr.labels.map((l) => el('span', { class: 'chip' }, l)),
          !pr.detail_current && d ? el('span', { class: 'notice' }, 'Changed since it was inspected') : null)),
      el('div', { class: 'pr-actions' },
        el('button', { class: 'btn primary', onclick: () => openUrl(pr.url) }, 'Open on GitHub'),
        el('button', { class: 'btn', onclick: () => openUrl(`${pr.url}/files`) }, 'Files changed'),
        el('button', { class: 'btn', 'data-action': 'inspect', onclick: () => void api.invoke('refresh_pr', { repo: pr.repo, number: pr.number }) }, 'Inspect now')),
      el('div', { class: 'pr-grid' }, el('div', { class: 'col' }, left), el('div', { class: 'col' }, right))));
    main.scrollTop = scroll;
  }

  // ───────────────────────────── wiring ─────────────────────────────

  for (const b of document.querySelectorAll<HTMLElement>('[data-pr-view]')) {
    b.addEventListener('click', () => go({ view: b.dataset.prView === 'queue' ? 'queue' : 'apps' }));
  }

  const refresh = (): void => {
    const r = state.route;
    if (r.view === 'repo') void api.invoke('refresh_pr_repo', { repo: r.repo });
    else if (r.view === 'pr') void api.invoke('refresh_pr', { repo: r.repo, number: r.number });
    else void api.invoke('refresh_prs');
  };

  const key = (e: KeyboardEvent): boolean => {
    const typing = (e.target as HTMLElement | null)?.tagName === 'INPUT';
    if (e.key === 'Escape') {
      if (closeHelp()) return true;
      if (typing && state.query) {
        state.query = '';
        (e.target as HTMLInputElement).value = '';
        renderPrPage(state.route.view === 'queue' ? 'queue' : 'repo');
        return true;
      }
      return back();
    }
    if (e.key === '/' && !typing) {
      const s = main.querySelector<HTMLInputElement>('input[type="search"]');
      if (s) {
        e.preventDefault();
        s.focus();
        return true;
      }
    }
    return false;
  };

  api.listen('pr:changed', changed);

  renderChrome();
  void reload().catch((e: unknown) => console.warn(e));

  // The ages on screen go stale with the clock alone.
  const clockTimer = setInterval(() => {
    if (state.overview) renderStatus();
  }, 30000);

  return {
    setActive(on) {
      if (on === state.active) return;
      state.active = on;
      if (on) {
        renderChrome();
        void reload().catch((e: unknown) => console.warn(e));
      } else {
        closeHelp();
      }
      renderStatus();
    },
    home: () => go({ view: 'apps' }),
    refresh,
    key,
    setHighQuality(n) {
      if (n === undefined || n === state.highQuality) return;
      state.highQuality = n;
      if (state.active && state.route.view === 'pr') renderPr();
    },
    stop() {
      clearInterval(clockTimer);
      if (timer) clearTimeout(timer);
      if (jumpTimer) clearTimeout(jumpTimer);
    },
  };
}
