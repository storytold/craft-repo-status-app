// Pure helpers: columns, sorting, formatting, staleness. No DOM, so
// `node --test ui/` can exercise them (see model.test.mjs).

/** @typedef {'text'|'num'|'date'|'version'} Kind */

/**
 * Table columns, in display order. `get` reads from a RepoEntry; numbers and
 * dates sort descending first (biggest / newest on top), text ascending.
 */
export const COLUMNS = [
  { key: 'name', label: 'Repo', kind: 'text', group: '', get: (e) => e.repo.split('/').pop() },
  { key: 'urgency', label: 'Urgency', kind: 'num', group: 'Fix first', title: 'Sum of the top five open-issue urgency scores' },
  { key: 'critical_issues', label: 'Critical', kind: 'num', group: 'Fix first', title: 'Open issues that mention a crash, hang, freeze, data loss, launch failure or security problem' },
  { key: 'open_prs', label: 'PRs', kind: 'num', group: 'Open', title: 'Open pull requests' },
  { key: 'open_issues', label: 'Issues', kind: 'num', group: 'Open', title: 'Open issues' },
  { key: 'oldest_open_pr_at', label: 'Oldest PR', kind: 'date', group: 'Open', title: 'Oldest pull request still open', asc: true },
  { key: 'last_commit_at', label: 'Last commit', kind: 'date', group: 'Latest', title: 'Last commit on main' },
  { key: 'last_issue_at', label: 'Last issue', kind: 'date', group: 'Latest', title: 'Newest issue (any state)' },
  { key: 'newest_pr_at', label: 'Newest PR', kind: 'date', group: 'Latest', title: 'Newest pull request (any state)' },
  { key: 'commits_4h', label: 'Commits', kind: 'num', group: 'Last 4 h', title: 'Commits to main in the last 4 hours' },
  { key: 'prs_4h', label: 'PRs', kind: 'num', group: 'Last 4 h', title: 'Pull requests opened in the last 4 hours' },
  { key: 'issues_4h', label: 'Issues', kind: 'num', group: 'Last 4 h', title: 'Issues opened in the last 4 hours' },
  { key: 'commits_total', label: 'Commits', kind: 'num', group: 'All time', title: 'Commits on main, all time' },
  { key: 'contributors', label: 'People', kind: 'num', group: 'All time', title: 'Contributors' },
  { key: 'issues_total', label: 'Issues', kind: 'num', group: 'All time', title: 'Issues, all time, open + closed' },
  { key: 'prs_total', label: 'PRs', kind: 'num', group: 'All time', title: 'Pull requests, all time, open + closed + merged' },
  { key: 'latest_build', label: 'Build', kind: 'version', group: 'Builds', title: 'Most recent published release' },
  { key: 'latest_build_at', label: 'Built', kind: 'date', group: 'Builds', title: 'When the most recent release was published' },
  { key: 'latest_build_downloads', label: 'Build DLs', kind: 'num', group: 'Builds', title: 'Downloads of the most recent release, all OSes and package types (checksums and update deltas excluded)' },
  { key: 'downloads_total', label: 'Total DLs', kind: 'num', group: 'Builds', title: 'Downloads across every published release' },
];

export const COLUMN = Object.fromEntries(COLUMNS.map((c) => [c.key, c]));

/** The value a column shows for an entry; `null` when there is no data. */
export function value(entry, key) {
  const col = COLUMN[key];
  if (col?.get) return col.get(entry);
  const v = entry.stats ? entry.stats[key] : null;
  return v === undefined ? null : v;
}

/** `v0.10.2-rc.1` → [0, 10, 2, -1, 1] so pre-releases sort below the release. */
export function versionKey(tag) {
  const m = String(tag).replace(/^v/i, '').match(/^(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-([A-Za-z]*)\.?(\d+)?)?/);
  if (!m) return [-1];
  return [+m[1], +(m[2] ?? 0), +(m[3] ?? 0), m[4] !== undefined ? -1 : 0, +(m[5] ?? 0)];
}

function cmpArrays(a, b) {
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const d = (a[i] ?? 0) - (b[i] ?? 0);
    if (d) return d;
  }
  return 0;
}

function cmpValues(kind, a, b) {
  switch (kind) {
    case 'text': return String(a).localeCompare(String(b), undefined, { sensitivity: 'base', numeric: true });
    case 'date': return Date.parse(a) - Date.parse(b);
    case 'version': return cmpArrays(versionKey(a), versionKey(b));
    default: return a - b;
  }
}

/** The direction a column sorts in when first clicked. */
export function defaultDir(key) {
  const col = COLUMN[key];
  return col.kind === 'text' || col.asc ? 'asc' : 'desc';
}

/** Sorted copy. Missing values always sink to the bottom; ties fall back to name. */
export function sortEntries(entries, key, dir) {
  const col = COLUMN[key] ?? COLUMN.urgency;
  const sign = dir === 'asc' ? 1 : -1;
  return [...entries].sort((a, b) => {
    const va = value(a, col.key);
    const vb = value(b, col.key);
    if (va === null && vb === null) return cmpValues('text', value(a, 'name'), value(b, 'name'));
    if (va === null) return 1;
    if (vb === null) return -1;
    return sign * cmpValues(col.kind, va, vb) || cmpValues('text', value(a, 'name'), value(b, 'name'));
  });
}

export function isStale(entry, staleAfterSecs, now = Date.now()) {
  if (!entry.fetched_at) return true;
  return now - Date.parse(entry.fetched_at) > staleAfterSecs * 1000;
}

/** `{ oldest, stale }` for the whole snapshot: the age of the oldest data on screen. */
export function freshness(snap, now = Date.now()) {
  const times = snap.repos.map((r) => (r.fetched_at ? Date.parse(r.fetched_at) : null));
  const oldest = times.length && times.every((t) => t !== null) ? Math.min(...times) : null;
  const stale = !snap.repos.length || snap.repos.some((r) => isStale(r, snap.stale_after_secs, now));
  return { oldest, stale };
}

/** "now", "45s", "12m", "3h", "5d", "2y" — compact for table cells. */
export function ago(iso, now = Date.now()) {
  if (!iso) return '';
  const s = Math.max(0, (now - Date.parse(iso)) / 1000);
  if (s < 10) return 'now';
  if (s < 60) return `${Math.floor(s)}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  if (s < 86400 * 365) return `${Math.floor(s / 86400)}d`;
  return `${Math.floor(s / (86400 * 365))}y`;
}

export function compact(n) {
  if (n === null || n === undefined) return '';
  if (n < 10000) return n.toLocaleString('en-US');
  if (n < 1e6) return `${(n / 1000).toFixed(n < 1e5 ? 1 : 0)}k`;
  return `${(n / 1e6).toFixed(1)}M`;
}

/** Column totals for the footer row (counts only; dates and versions blank). */
export function totals(entries) {
  const out = {};
  for (const c of COLUMNS) {
    if (c.kind !== 'num') continue;
    out[c.key] = entries.reduce((sum, e) => sum + (value(e, c.key) ?? 0), 0);
  }
  return out;
}

/** Every repo's urgent issues merged, best first. */
export function urgentAcross(entries, { repo = '', criticalOnly = false, limit = 60 } = {}) {
  const all = [];
  for (const e of entries) {
    if (repo && e.repo !== repo) continue;
    for (const u of e.stats?.urgent ?? []) {
      if (criticalOnly && !u.critical) continue;
      all.push({ ...u, repo: e.repo });
    }
  }
  all.sort((a, b) => b.score - a.score || Date.parse(b.created_at ?? 0) - Date.parse(a.created_at ?? 0));
  return all.slice(0, limit);
}
