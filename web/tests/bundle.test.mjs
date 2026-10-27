// web/dist is committed so building BeatBox needs no Node.
// CI checks it's fresh (build + git diff); this checks the cheap half
// everywhere: it exists, it's wired for the window, and it never fetches.

import assert from 'node:assert/strict';
import { existsSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { describe, test } from 'node:test';

import { DIST, REPO, WEB, all, text } from './support.mjs';

const INDEX = join(DIST, 'index.html');
const ASSETS = join(DIST, 'assets');

// Labs can be offline; the UI must never depend on these.
const FORBIDDEN_HOSTS = ['fonts.googleapis.com', 'fonts.gstatic.com', 'cdnjs.cloudflare.com', 'unpkg.com', 'jsdelivr'];

describe('the bundle', { skip: !existsSync(INDEX) && 'web/dist is not built; run npm run build in web' }, () => {
  const html = existsSync(INDEX) ? text(INDEX) : '';

  // Guest output reaches the page, so "never fetch" must be enforced by CSP.
  test('the page carries a policy', () => {
    const policy = html.match(/<meta http-equiv="Content-Security-Policy" content="([^"]+)"/);
    assert.ok(policy, 'the built page has no content security policy');
    const directives = Object.fromEntries(
      policy[1]
        .split(';')
        .map((part) => part.trim())
        .filter(Boolean)
        .map((part) => {
          const space = part.indexOf(' ');
          return space < 0 ? [part, ''] : [part.slice(0, space), part.slice(space + 1)];
        }),
    );
    assert.equal(directives['default-src'], "'none'");
    // Window IPC and loopback VNC only. No remote hosts, no blanket ws:.
    assert.equal(directives['connect-src'], 'ipc: http://ipc.localhost ws://127.0.0.1:*',
      "the page may reach only its own IPC and a local machine display");
    assert.equal(directives['font-src'], "'self'");
    assert.equal(directives['script-src'], "'self'", 'nothing may run but the bundle');
    assert.ok(!policy[1].includes('unsafe-eval'));
  });

  // frame-ancestors is ignored in a <meta> and Chromium warns on every load.
  test('the policy does not carry a directive a meta tag cannot', () => {
    assert.ok(!html.includes('frame-ancestors'));
  });

  test('the window can find it', () => {
    const config = JSON.parse(text(join(REPO, 'shell', 'tauri.conf.json')));
    assert.equal(resolve(REPO, 'shell', config.build.frontendDist), resolve(DIST));
  });

  test('asset urls are relative', () => {
    for (const url of all(/(?:src|href)="([^"]+)"/g, html)) {
      assert.ok(!url.startsWith('/'), `${url} is root-relative`);
    }
  });

  test('every referenced asset is present', () => {
    for (const url of all(/(?:src|href)="\.\/([^"]+)"/g, html)) {
      assert.ok(existsSync(join(DIST, url)), `index.html points at a missing ${url}`);
    }
  });

  // Hashed names would make every rebuild an unreadable diff.
  test('filenames carry no content hash', () => {
    for (const name of readdirSync(ASSETS)) {
      assert.doesNotMatch(name, /-[0-9a-zA-Z_]{8}\.(js|css)$/, `${name} looks hashed`);
    }
  });

  test('fonts are bundled, not fetched', () => {
    const css = text(join(ASSETS, 'index.css'));
    assert.ok(css.includes('@font-face'));
    for (const raw of all(/url\(([^)]+)\)/g, css)) {
      const url = raw.replace(/^["']|["']$/g, '');
      assert.ok(!url.startsWith('http'), `${url} would be fetched at runtime`);
    }
    assert.ok(readdirSync(ASSETS).some((name) => name.endsWith('.woff2')), 'no font files were emitted');
  });

  test('nothing in the bundle reaches the network', () => {
    for (const name of readdirSync(ASSETS).filter((name) => name.startsWith('index.'))) {
      const body = text(join(ASSETS, name));
      for (const host of FORBIDDEN_HOSTS) {
        assert.ok(!body.includes(host), `${name} mentions ${host}`);
      }
    }
  });

  test('the bundle is not a stub', () => {
    const javascript = text(join(ASSETS, 'index.js'));
    assert.ok(javascript.length > 50_000, 'the bundle is too small to contain the app');
    assert.ok(javascript.includes('__TAURI_INTERNALS__'), "the window's transport is missing");
    // Old Qt/Python transports must not come back.
    for (const gone of ['qwebchannel', '127.0.0.1:8765']) {
      assert.ok(!javascript.includes(gone), `the bundle still carries ${gone}`);
    }
  });
});

describe('the source', () => {
  test('node_modules is ignored and dist is not', () => {
    const ignore = text(join(WEB, '.gitignore'));
    // No trailing slash: worktrees symlink node_modules, and 'node_modules/' misses a symlink.
    assert.match(ignore, /^node_modules$/m);
    assert.doesNotMatch(ignore, /^dist\/?$/m, 'dist must stay committed, so running from source needs no Node');
  });

  test('the build is pinned', () => {
    assert.ok(existsSync(join(WEB, 'package-lock.json')),
      'package-lock.json must be committed so CI builds what we build');
  });
});
