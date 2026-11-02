// Repo rules that a reviewer could miss. See CLAUDE.md.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

const root = new URL('../../', import.meta.url);

test('no JavaScript is committed: the UI is TypeScript', () => {
  const js = execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard', '*.js', '*.mjs', '*.cjs', '*.jsx'], { cwd: root, encoding: 'utf8' })
    .split('\n').filter(Boolean);
  assert.deepEqual(js, [], 'write these in TypeScript under ui/src/ instead');
});

test('index.html runs no inline script, only the compiled TypeScript', () => {
  const html = readFileSync(new URL('ui/index.html', root), 'utf8');
  const scripts = [...html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/g)];
  assert.ok(scripts.length > 0);
  for (const [tag, attrs, body] of scripts) {
    assert.match(attrs!, /\bsrc="build\/[\w-]+\.js"/, `${tag} must load a file built from ui/src/`);
    assert.equal(body!.trim(), '', `${tag} must not have inline code`);
  }
  assert.doesNotMatch(html, /\son[a-z]+=/i, 'no inline event handlers');
  assert.doesNotMatch(html, /javascript:/i, 'no javascript: URLs');
});

test('PR text can only reach the DOM as text', () => {
  for (const file of ['ui/src/pr-app.ts', 'ui/src/app.ts', 'ui/src/dom.ts']) {
    const code = readFileSync(new URL(file, root), 'utf8');
    assert.doesNotMatch(code, /innerHTML|outerHTML|insertAdjacentHTML|document\.write/, file);
  }
});
