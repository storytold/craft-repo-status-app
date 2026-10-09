// Craft Status UI: renders the snapshot pushed by the Rust poller.
// Outside Tauri (plain browser) it loads ./dev-snapshot.json, written by
// `make snapshot`, so the UI can be iterated on without the app.

import { COLUMNS, RECENT, WINDOWS, windowById, value, sortEntries, defaultDir, freshness, ago, totals, urgentAcross } from './model.js';

const $ = (id) => document.getElementById(id);
const TAURI = window.__TAURI__;

const api = TAURI
  ? {
      invoke: (cmd, args) => TAURI.core.invoke(cmd, args),
      listen: (ev, fn) => TAURI.event.listen(ev, (e) => fn(e.payload)),
      startDrag: () => TAURI.window.getCurrentWindow().startDragging(),
      startResize: () => TAURI.window.getCurrentWindow().startResizeDragging('SouthEast'),
    }
  : {
      async invoke(cmd, args) {
        if (cmd === 'get_snapshot') return (await fetch('dev-snapshot.json')).json();
        if (cmd === 'get_meta') return { opacity: 0.96, always_on_top: false, shortcut: 'CmdOrCtrl+Shift+G' };
        if (cmd === 'open_url') window.open(args.url, '_blank');
        return null;
      },
      listen() {},
      startDrag() {},
      startResize() {},
    };

const load = (k, d) => {
  try { return JSON.parse(localStorage.getItem(k)) ?? d; } catch { return d; }
};
const save = (k, v) => {
  try { localStorage.setItem(k, JSON.stringify(v)); } catch { /* private mode */ }
};

const state = {
  snap: null,
  meta: {},
  view: new URLSearchParams(location.search).get('view') ?? load('view', 'repos'),
  sort: load('sort', { key: 'urgency', dir: 'desc' }),
  window: windowById(load('window', '')).id,
  urgentRepo: '',
  criticalOnly: load('criticalOnly', false),
};

// ───────────────────────────── formatting ─────────────────────────────

const el = (tag, props = {}, ...kids) => {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (v === undefined || v === null || v === false) continue;
    if (k === 'class') n.className = v;
    else if (k === 'dataset') Object.assign(n.dataset, v);
    else if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
    else n.setAttribute(k, v);
  }
  for (const kid of kids.flat()) if (kid !== null && kid !== undefined && kid !== false) n.append(kid);
  return n;
};

const localTime = (iso) => (iso ? new Date(iso).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }) : '');
const clock = (t) => new Date(t).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
const num = (n) => (n ?? 0).toLocaleString('en-US');
const gh = (repo, path = '') => `https://github.com/${repo}${path}`;
const DAY = 86400e3;

function openUrl(url, ev) {
  ev?.stopPropagation();
  api.invoke('open_url', { url }).catch((e) => console.warn(e));
}

const link = (text, url, cls = '') => el('span', { class: `link ${cls}`, onclick: (e) => openUrl(url, e) }, text);

/** The repo's app icon; repos without one (libraries) get a lettered tile, unfetched ones a blank. */
function appIcon(entry) {
  if (entry.icon) return el('img', { class: 'app-icon', src: entry.icon, alt: '' });
  if (!entry.stats) return el('span', { class: 'app-icon pending', 'aria-hidden': 'true' });
  return el('span', { class: 'app-icon none', 'aria-hidden': 'true' }, entry.repo.split('/').pop().charAt(0).toUpperCase());
}

/** Cell content for a column. */
function cell(entry, col) {
  const v = value(entry, col.key, state.window);
  const s = entry.stats;
  if (col.key === 'name') {
    const tip = entry.error
      ? `Last attempt failed: ${entry.error}`
      : entry.fetched_at ? `Fetched ${localTime(entry.fetched_at)}` : 'Not fetched yet';
    return el('span', { class: 'repo', title: tip }, el('span', { class: 'dot' }), appIcon(entry), link(v, gh(entry.repo)));
  }
  if (!s) return el('span', { class: 'muted' }, '—');
  switch (col.key) {
    case 'urgency': {
      const a = Math.min(1, v / 350) * 0.45;
      return el('span', { class: 'urgency', style: `background: rgb(var(--hot-rgb) / ${a.toFixed(2)})` }, Math.round(v));
    }
    case 'critical_issues': return el('span', { class: v ? 'crit' : 'zero' }, num(v));
    case 'open_prs': return link(num(v), gh(entry.repo, '/pulls'));
    case 'open_issues': return link(num(v), gh(entry.repo, '/issues'));
    case 'recent_commits': case 'recent_prs': case 'recent_merged': case 'recent_issues': case 'recent_people':
      if (v === null) return el('span', { class: 'muted', title: 'Not fetched yet for this window' }, '—');
      return el('span', { class: v ? 'active' : 'zero' }, num(v));
    case 'commits_since_build': {
      const lag = s.since_build;
      if (!lag) return el('span', { class: 'muted' }, '—');
      const short = (sha) => sha?.slice(0, 7);
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
      return el('span', {}, link(v, gh(entry.repo, `/releases/tag/${encodeURIComponent(v)}`)),
        s.latest_build_prerelease ? el('span', { class: 'pre' }, 'PRE') : null);
  }
  if (col.kind === 'date') {
    if (!v) return el('span', { class: 'muted' }, '—');
    const old = col.key === 'oldest_open_pr_at' && Date.now() - Date.parse(v) > 14 * DAY;
    return el('span', { title: localTime(v), class: old ? 'old' : '' }, ago(v));
  }
  return document.createTextNode(num(v));
}

// ───────────────────────────── rendering ─────────────────────────────

function renderHead() {
  const groups = el('tr', { class: 'groups' });
  let prev = null;
  for (const c of COLUMNS) {
    if (c.group === prev) { groups.lastChild.colSpan += 1; continue; }
    prev = c.group;
    groups.append(el('th', { class: c.group ? 'group-start' : '' }, c.group === RECENT ? windowPicker() : c.group));
  }
  const cols = el('tr', { class: 'cols' });
  COLUMNS.forEach((c, i) => {
    const sorted = state.sort.key === c.key;
    const start = i > 0 && COLUMNS[i - 1].group !== c.group;
    cols.append(el('th', {
      title: c.title?.replace('{span}', windowById(state.window).span) ?? `Sort by ${c.label}`,
      class: [sorted && 'sorted', sorted && state.sort.dir === 'asc' && 'asc', start && 'group-start'].filter(Boolean).join(' '),
      onclick: () => {
        state.sort = state.sort.key === c.key
          ? { key: c.key, dir: state.sort.dir === 'asc' ? 'desc' : 'asc' }
          : { key: c.key, dir: defaultDir(c.key) };
        save('sort', state.sort);
        render();
      },
    }, c.label));
  });
  $('thead').replaceChildren(groups, cols);
}

/** The dropdown that heads the recent-activity columns. */
function windowPicker() {
  const sel = el('select', {
    class: 'window-picker',
    'aria-label': 'Time window for recent activity',
    title: 'Time window for the activity columns',
    onchange: (e) => { state.window = e.target.value; save('window', state.window); render(); },
  }, WINDOWS.map((w) => el('option', { value: w.id }, w.label)));
  sel.value = state.window;
  return sel;
}

function groupStart(i) {
  return i > 0 && COLUMNS[i - 1].group !== COLUMNS[i].group ? 'group-start' : '';
}

function renderTable() {
  renderHead();
  const snap = state.snap;
  const rows = sortEntries(snap.repos, state.sort.key, state.sort.dir, state.window);
  const now = Date.now();
  $('tbody').replaceChildren(...rows.map((e) => {
    const stale = !e.fetched_at || now - Date.parse(e.fetched_at) > snap.stale_after_secs * 1000;
    return el('tr', {
      class: [stale && 'stale', e.error && 'error'].filter(Boolean).join(' '),
      title: 'Click for this repo’s most urgent issues',
      onclick: () => { state.urgentRepo = e.repo; setView('urgent'); },
    }, COLUMNS.map((c, i) => el('td', { class: groupStart(i) }, cell(e, c))));
  }));
  if (!rows.length) {
    $('tbody').replaceChildren(el('tr', {}, el('td', { colspan: COLUMNS.length, class: 'empty' }, 'No repositories configured — Edit config.')));
  }
  const t = totals(snap.repos, state.window);
  $('tfoot').replaceChildren(el('tr', {}, COLUMNS.map((c, i) =>
    el('td', { class: groupStart(i) }, i === 0 ? `${snap.repos.length} repos` : c.kind === 'num' ? num(Math.round(t[c.key])) : ''))));
}

function renderUrgent() {
  const snap = state.snap;
  const sel = $('urgent-repo');
  sel.replaceChildren(el('option', { value: '' }, 'All repositories'),
    ...snap.repos.map((r) => el('option', { value: r.repo }, r.repo.split('/').pop())));
  sel.value = state.urgentRepo;
  $('critical-only').checked = state.criticalOnly;
  const items = urgentAcross(snap.repos, { repo: state.urgentRepo, criticalOnly: state.criticalOnly });
  const SEVERE = /crash|panic|freeze|hang|data loss|corruption|won't|blank|security/;
  $('urgent-list').replaceChildren(...items.map((u) => {
    const a = Math.min(1, u.score / 90) * 0.5;
    return el('li', { class: `urgent-item${u.critical ? ' critical' : ''}`, onclick: () => openUrl(u.url), title: u.url },
      el('span', { class: 'score', style: `background: rgb(var(--hot-rgb) / ${a.toFixed(2)})` }, u.score),
      el('span', { class: 'u-repo' }, appIcon(snap.repos.find((r) => r.repo === u.repo) ?? { repo: u.repo }), u.repo.split('/').pop()),
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

function renderStatus() {
  const snap = state.snap;
  const { oldest, stale } = freshness(snap);
  const pill = $('fresh-pill');
  if (snap.refreshing) { pill.className = 'pill refreshing'; pill.textContent = 'REFRESHING'; }
  else if (stale) { pill.className = 'pill stale'; pill.textContent = 'STALE'; }
  else { pill.className = 'pill fresh'; pill.textContent = 'FRESH'; }
  const last = snap.last_full_refresh_at ? Date.parse(snap.last_full_refresh_at) : null;
  let text;
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
  const msgs = [snap.error, failing.length ? `${failing.length} repo${failing.length > 1 ? 's' : ''} failed last poll: ${failing.map((r) => `${r.repo.split('/').pop()} (${r.error})`).join('; ')}` : null].filter(Boolean);
  $('banner').hidden = !msgs.length;
  $('banner').textContent = msgs.join(' — ');

  $('next').textContent = snap.refreshing ? 'Polling GitHub…'
    : snap.next_refresh_at ? `Next poll ${clock(Date.parse(snap.next_refresh_at))} · every ${Math.round(snap.poll_secs / 60)} min` : '';
  $('budget').textContent = snap.rate_limit_remaining != null ? `GitHub budget ${num(snap.rate_limit_remaining)}` : '';
  const critical = snap.repos.reduce((n, r) => n + (r.stats?.critical_issues ?? 0), 0);
  $('urgent-count').textContent = critical ? String(critical) : '';
  $('urgent-count').title = `${critical} critical open issues`;
  $('repo-count').textContent = String(snap.repos.length);
}

function render() {
  if (!state.snap) return;
  renderStatus();
  document.querySelectorAll('.view').forEach((b) => b.setAttribute('aria-selected', String(b.dataset.view === state.view)));
  $('repos-view').hidden = state.view !== 'repos';
  $('urgent-view').hidden = state.view !== 'urgent';
  if (state.view === 'repos') renderTable(); else renderUrgent();
}

function setView(v) {
  state.view = v;
  save('view', v);
  render();
}

function applyMeta(m) {
  state.meta = m ?? {};
  document.documentElement.style.setProperty('--opacity', String(state.meta.opacity ?? 0.96));
  $('pin').setAttribute('aria-pressed', String(!!state.meta.always_on_top));
  const mac = navigator.platform.toLowerCase().includes('mac');
  $('shortcut').textContent = (state.meta.shortcut ?? '').replace('CmdOrCtrl', mac ? '⌘' : 'Ctrl').replace('Shift', '⇧').replaceAll('+', '');
}

// ───────────────────────────── wiring ─────────────────────────────

document.querySelectorAll('.view').forEach((b) => b.addEventListener('click', () => {
  if (b.dataset.view === 'urgent' && state.view !== 'urgent') state.urgentRepo = '';
  setView(b.dataset.view);
}));
$('urgent-repo').addEventListener('change', (e) => { state.urgentRepo = e.target.value; render(); });
$('critical-only').addEventListener('change', (e) => { state.criticalOnly = e.target.checked; save('criticalOnly', state.criticalOnly); render(); });
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

api.listen('craft:snapshot', (s) => { state.snap = s; render(); });
api.listen('craft:meta', applyMeta);
// Relative times and staleness change with the clock alone.
setInterval(render, 15_000);

(async () => {
  try {
    applyMeta(await api.invoke('get_meta'));
    state.snap = await api.invoke('get_snapshot');
    render();
  } catch (e) {
    console.error(e);
  } finally {
    // Not requestAnimationFrame: WebKit never fires it while the window is hidden.
    api.invoke('window_ready');
  }
})();
