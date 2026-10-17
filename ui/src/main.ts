// Entry point. Inside Tauri the UI talks to the Rust side; in a plain browser
// it loads ./snapshot.json (see web.ts), so the same UI runs on GitHub Pages
// and in `make preview`.

import { start } from './app.ts';
import type { Api } from './app.ts';
import type { Snapshot } from './model.ts';
import { webApi } from './web.ts';

const TAURI = window.__TAURI__;

const api: Api = TAURI
  ? {
      invoke: (cmd, args) => TAURI.core.invoke(cmd, args),
      listen: (ev, fn) => { void TAURI.event.listen(ev, (e) => fn(e.payload as never)); },
      startResize: () => { void TAURI.window.getCurrentWindow().startResizeDragging('SouthEast'); },
    }
  : webApi(window, async () => (await fetch('snapshot.json', { cache: 'no-store' })).json() as Promise<Snapshot>);

start(api);
