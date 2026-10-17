// The browser host (web.ts) against a fake snapshot loader.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Window } from 'happy-dom';
import { webApi } from './web.ts';
import type { Snapshot } from './model.ts';

const snap: Snapshot = {
  repos: [], last_full_refresh_at: null, refreshing: false, next_refresh_at: null,
  poll_secs: 900, stale_after_secs: 3600, rate_limit_remaining: null, error: null,
};

test('marks the page as web and serves the loaded snapshot', async () => {
  const win = new Window();
  const api = webApi(win as unknown as globalThis.Window, async () => snap);
  assert.ok(win.document.documentElement.classList.contains('web'));
  assert.equal((await api.invoke<Snapshot>('get_snapshot')).poll_secs, 900);
  assert.equal((await api.invoke<{ shortcut: string }>('get_meta')).shortcut, '');
  await win.happyDOM.close();
});

test('refresh reloads the page, which fetches the newest snapshot', async () => {
  const win = new Window();
  let reloads = 0;
  const fake = { document: win.document, location: { reload: () => { reloads++; } } };
  await webApi(fake as unknown as globalThis.Window, async () => snap).invoke('refresh_now');
  assert.equal(reloads, 1);
  await win.happyDOM.close();
});
