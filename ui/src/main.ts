// Entry point. Inside Tauri the UI talks to the Rust side; in a plain browser
// it loads ./dev-snapshot.json, written by `make snapshot`, so the UI can be
// iterated on without the app.

import { start } from './app.ts';
import type { Api } from './app.ts';

const TAURI = window.__TAURI__;

const api: Api = TAURI
  ? {
      invoke: (cmd, args) => TAURI.core.invoke(cmd, args),
      listen: (ev, fn) => { void TAURI.event.listen(ev, (e) => fn(e.payload as never)); },
      startResize: () => { void TAURI.window.getCurrentWindow().startResizeDragging('SouthEast'); },
    }
  : {
      async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
        if (cmd === 'get_snapshot') return (await fetch('dev-snapshot.json', { cache: 'no-cache' })).json() as Promise<T>;
        if (cmd === 'get_meta') return { opacity: 0.96, always_on_top: false, shortcut: 'CmdOrCtrl+Shift+G' } as T;
        if (cmd === 'open_url') window.open(String(args?.url), '_blank');
        if (cmd === 'refresh_now') window.location.reload();
        return null as T;
      },
      listen() {},
      startResize() {},
    };

start(api);
