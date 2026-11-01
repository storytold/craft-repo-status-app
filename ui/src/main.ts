// Entry point. Inside Tauri the UI talks to the Rust side; in a plain browser it
// loads ./dev-snapshot.json (`make snapshot`) and ./dev-data.json (`make dump`), so
// the UI can be iterated on without the app.

import { start } from './app.ts';
import type { Api } from './app.ts';
import type { Route } from './pr-app.ts';

interface DevData {
  overview: unknown[];
  queue: unknown[];
  repos: Record<string, unknown>;
  prs: Record<string, unknown>;
}

const TAURI = window.__TAURI__;

let dev: Promise<DevData> | null = null;
const devData = (): Promise<DevData> => (dev ??= fetch('dev-data.json')
  .then((r) => r.json() as Promise<DevData>)
  .catch(() => ({ overview: [], queue: [], repos: {}, prs: {} })));

const api: Api = TAURI
  ? {
      invoke: (cmd, args) => TAURI.core.invoke(cmd, args),
      listen: (ev, fn) => { void TAURI.event.listen(ev, (e) => fn(e.payload as never)); },
      startResize: () => { void TAURI.window.getCurrentWindow().startResizeDragging('SouthEast'); },
    }
  : {
      async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
        const answer = (v: unknown): T => v as T;
        if (cmd === 'get_snapshot') return (await fetch('dev-snapshot.json')).json() as Promise<T>;
        if (cmd === 'get_meta') return answer({ opacity: 0.96, always_on_top: false, shortcut: 'CmdOrCtrl+Shift+G', high_quality: 75 });
        if (cmd === 'open_url') window.open(String(args?.url), '_blank');
        if (!['get_pr_overview', 'get_pr_queue', 'get_pr_repo', 'get_pr'].includes(cmd)) return answer(null);
        const d = await devData();
        if (cmd === 'get_pr_overview') {
          return answer({
            repos: d.overview,
            status: {
              points_last_hour: 0, max_points_per_hour: 2500, backlog: 0, histories_pending: 0, poll_secs: 1800,
              stale_after_secs: 0, token_rejected: false,
              claude: { enabled: true, available: true, model: 'preview', spent_today: 0, max_usd_per_day: 5, runs_last_hour: 0, max_runs_per_hour: 12 },
            },
          });
        }
        if (cmd === 'get_pr_queue') return answer(d.queue);
        if (cmd === 'get_pr_repo') return answer(d.repos[String(args?.repo)] ?? null);
        return answer(d.prs[`${String(args?.repo)}#${String(args?.number)}`] ?? null);
      },
      listen() {},
      startResize() {},
    };

/** In a browser, `?repo=o/r&pr=1`, `?repo=o/r` or `?pr-view=queue` opens the PR tab there. */
function prRouteFromUrl(): Route | undefined {
  const q = new URLSearchParams(location.search);
  const repo = q.get('repo');
  const pr = Number(q.get('pr'));
  if (repo && pr) return { view: 'pr', repo, number: pr, from: 'repo' };
  if (repo) return { view: 'repo', repo };
  return q.get('pr-view') === 'queue' ? { view: 'queue' } : undefined;
}

start(api, TAURI ? undefined : prRouteFromUrl());
