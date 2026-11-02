// What every tab needs from the page and the host. No text from GitHub reaches the
// DOM except through `el`, as text nodes.

/** What the UI needs from the host: Tauri commands and events, window chrome. */
export interface Api {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  listen<T>(event: string, fn: (payload: T) => void): void;
  startResize(): void;
}

export const $ = <T extends HTMLElement = HTMLElement>(id: string): T => {
  const n = document.getElementById(id);
  if (!n) throw new Error(`#${id} is missing from index.html`);
  return n as T;
};

export function load<T>(k: string, d: T): T {
  try { return (JSON.parse(localStorage.getItem(k) ?? 'null') as T | null) ?? d; } catch { return d; }
}
export function save(k: string, v: unknown): void {
  try { localStorage.setItem(k, JSON.stringify(v)); } catch { /* private mode */ }
}

export type Child = Node | string | number | null | undefined | false;
type Props = Record<string, unknown>;

export function el<K extends keyof HTMLElementTagNameMap>(tag: K, props: Props = {}, ...kids: (Child | Child[])[]): HTMLElementTagNameMap[K] {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (v === undefined || v === null || v === false) continue;
    if (k === 'class') n.className = String(v);
    else if (k === 'dataset') Object.assign(n.dataset, v);
    else if (k.startsWith('on')) n.addEventListener(k.slice(2), v as EventListener);
    else n.setAttribute(k, String(v));
  }
  for (const kid of kids.flat()) {
    if (kid !== null && kid !== undefined && kid !== false) n.append(typeof kid === 'number' ? String(kid) : kid);
  }
  return n;
}

export const classes = (...cs: (string | false | null | undefined)[]): string => cs.filter(Boolean).join(' ');
export const localTime = (iso: string | null | undefined): string =>
  (iso ? new Date(iso).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }) : '');
export const DAY = 86400e3;
