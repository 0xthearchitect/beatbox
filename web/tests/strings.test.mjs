// Literal keys are checked by TypeScript (Key union) and scripts/locales.mjs.
// This covers what neither sees: engine reasons and keys built at runtime.

import assert from 'node:assert/strict';
import { join } from 'node:path';
import { describe, test } from 'node:test';

import { LANGUAGES, REPO, all, between, catalogue, read, text, walk } from './support.mjs';

const catalogues = Object.fromEntries(LANGUAGES.map((language) => [language, catalogue(language)]));

// assert.ok, not deepEqual: a failing deepEqual prints the whole catalogue.
function assertKey(key, where) {
  for (const [language, strings] of Object.entries(catalogues)) {
    assert.ok(key in strings, `${where} asks for ${key}, missing from ${language}.json`);
  }
}

/**
 * Every `reason!("code", key = value, ...)` in the crates, as [code, keys, file].
 * Misses helper-built codes; crates/api/tests/strings.rs covers those at
 * runtime. Skips comment lines and everything after #[cfg(test)].
 */
function rustReasons() {
  const found = [];
  const files = walk(join(REPO, 'crates'), (name) => name.endsWith('.rs'))
    .filter((path) => path.split(/[\\/]/).includes('src'));
  for (const file of files) {
    const source = text(file)
      .split('#[cfg(test)]')[0]
      .split('\n')
      .filter((line) => !line.trimStart().startsWith('//'))
      .join('\n');
    for (const match of source.matchAll(/\breason!\(/g)) {
      const start = match.index + match[0].length;
      let depth = 1;
      let index = start;
      let quoted = false;
      while (index < source.length && depth) {
        const char = source[index];
        if (quoted) {
          if (char === '\\') index += 1;
          else if (char === '"') quoted = false;
        } else if (char === '"') quoted = true;
        else if ('([{'.includes(char)) depth += 1;
        else if (')]}'.includes(char)) depth -= 1;
        index += 1;
      }
      const body = source.slice(start, index - 1);
      const parts = [];
      let current = '';
      depth = 0;
      quoted = false;
      for (const char of body) {
        if (char === '"' && !current.endsWith('\\')) quoted = !quoted;
        if (!quoted && '([{'.includes(char)) depth += 1;
        if (!quoted && ')]}'.includes(char)) depth -= 1;
        if (char === ',' && depth === 0 && !quoted) {
          parts.push(current);
          current = '';
        } else current += char;
      }
      parts.push(current);
      const literal = parts[0].match(/^\s*"([a-z0-9_.]+)"\s*$/);
      if (!literal) continue;
      const keys = new Set(parts.slice(1).map((part) => part.match(/^\s*(\w+)\s*=/)?.[1]).filter(Boolean));
      found.push([literal[1], keys, file.slice(REPO.length + 1)]);
    }
  }
  return found;
}

describe('runtime families and engine reasons', () => {
  // Exact count on purpose: if routes vanish, someone has to edit this line.
  test('every route carries a title and a blurb', () => {
    const routes = all(/id: '([\w.]+)'/g, read('routes.ts'));
    assert.ok(routes.length >= 6, 'the route table shrank unexpectedly');
    for (const route of routes) {
      assertKey(`beatbox.screen.${route}.title`, 'routes.ts');
      assertKey(`beatbox.screen.${route}.blurb`, 'routes.ts');
    }
  });

  test('every nav entry and tab is named', () => {
    const source = read('routes.ts');
    for (const key of all(/key: '(\w+)'/g, source)) assertKey(`beatbox.nav.${key}`, 'routes.ts');
    for (const tab of all(/tab: '(\w+)'/g, source)) assertKey(`beatbox.tabs.${tab}`, 'routes.ts');
  });

  // Computed keys (t(`beatbox.account.${pick.id}`)) the literal scanner can't see.
  test('every way in names itself and says what it is', () => {
    const picks = all(/\{ id: '(\w+)', live: (?:true|false) \}/g, read('screens/Account.tsx'));
    assert.equal(picks.length, 3, 'the account screen no longer offers three ways in');
    for (const pick of picks) {
      assertKey(`beatbox.account.${pick}`, 'Account.tsx');
      assertKey(`beatbox.account.${pick}.what`, 'Account.tsx');
    }
  });

  // Keys built from the page's own tables (ARGUMENTS, FACTS).
  test('every word the engine screen computes exists', () => {
    const screen = read('screens/Engine.tsx');
    const table = between(screen, 'const ARGUMENTS', '/** Facts worth');
    const dotted = new Set(all(/'([\w.]+\.[\w.]+)'/g, table));
    const argumentsAsked = new Set(all(/'(\w+)'/g, table).filter((word) => !dotted.has(word)));
    assert.ok(argumentsAsked.has('command'), 'the argument scanner stopped finding them');
    for (const argument of [...argumentsAsked].sort()) assertKey(`beatbox.argument.${argument}`, 'Engine.tsx');
    assertKey('beatbox.argument.json', 'Engine.tsx');

    const facts = screen.match(/const FACTS = \[(.*?)\]/s)[1];
    for (const fact of all(/'(\w+)'/g, facts)) assertKey(`beatbox.fact.${fact}`, 'Engine.tsx');
  });

  // Server screen keys, all computed from its own tables.
  test('every word the server screen computes exists', () => {
    const screen = read('screens/Server.tsx');
    const known = all(/'([\w-]+)'/g, screen.match(/const KNOWN = \[(.*?)\]/)[1]);
    assert.ok(known.includes('dns'), 'the definition scanner stopped finding them');
    for (const id of known) {
      assertKey(`beatbox.srv.kind.${id}`, 'Server.tsx');
      assertKey(`beatbox.srv.about.${id}`, 'Server.tsx');
    }
    for (const key of all(/'(\w+)'/g, screen.match(/const KEYS = \[(.*?)\]/)[1])) assertKey(`beatbox.srv.key.${key}`, 'Server.tsx');
    for (const mode of all(/'(\w+)'/g, screen.match(/const WEB_MODES = \[(.*?)\]/)[1])) assertKey(`beatbox.srv.web.mode.${mode}`, 'Server.tsx');
    for (const cap of all(/'([\w.]+)'/g, screen.match(/const CAPS = \[(.*?)\]/)[1])) assertKey(`beatbox.srv.cap.${cap}`, 'Server.tsx');
    for (const mood of all(/'(\w+)'/g, screen.match(/type Mood = (.*?);/)[1])) {
      assertKey(`beatbox.srv.mood.${mood}`, 'Server.tsx');
      if (mood !== 'ok') assertKey(`beatbox.srv.foot.${mood}`, 'Server.tsx');
    }
    for (const source of ['builtin', 'library', 'inline']) assertKey(`beatbox.srv.source.${source}`, 'Server.tsx');
    for (const id of all(/'([\w-]+)'/g, screen.match(/const offered = \[(.*?)\]/)[1])) {
      assertKey(`beatbox.srv.start.${id}`, 'Server.tsx');
      assertKey(`beatbox.srv.start.${id}.what`, 'Server.tsx');
    }
    for (const word of all(/'(beatbox\.srv\.do\.\w+)'/g, screen)) assertKey(word, 'Server.tsx');
    // The server's latest events, said by name.
    const said = all(/'(\w+)'/g, screen.match(/const known = \[(.*?)\]/)[1]);
    assert.ok(said.includes('failed'), 'the event scanner stopped finding them');
    for (const event of [...said, 'answers', 'silent']) assertKey(`beatbox.srv.lately.${event}`, 'Server.tsx');
  });

  test('every word the other server pages compute exists', () => {
    const screen = read('screens/ServerMore.tsx');
    const pages = all(/<Frame page="(\w+)"/g, screen);
    // Try is a panel inside each service now, not a page.
    assert.deepEqual(pages.sort(), ['library', 'settings'], 'the page scanner stopped finding them');
    for (const page of pages) assertKey(`beatbox.srv.${page}.lede`, 'ServerMore.tsx');
    for (const check of ['tcp', 'http', 'none']) assertKey(`beatbox.srv.lib.health.${check}`, 'ServerMore.tsx');
    for (const filter of all(/'(\w+)'/g, screen.match(/const FILTERS = \[(.*?)\]/)[1])) assertKey(`beatbox.srv.lib.filter.${filter}`, 'ServerMore.tsx');
    for (const kind of all(/'(\w+)'/g, screen.match(/const FIELD_KINDS = \[(.*?)\]/)[1])) assertKey(`beatbox.srv.lib.field.${kind}`, 'ServerMore.tsx');
    for (const each of all(/'(\w+)'/g, screen.match(/const ON_EXIT = \[(.*?)\]/)[1])) {
      assertKey(`beatbox.srv.set.on_exit.${each}`, 'ServerMore.tsx');
      assertKey(`beatbox.srv.set.on_exit.${each}.what`, 'ServerMore.tsx');
    }
    for (const each of all(/'(\w+)'/g, screen.match(/const ON_EXIT = \[(.*?)\]/)[1])) {
      assertKey(`beatbox.srv.set.on_exit.${each}`, 'ServerMore.tsx');
      assertKey(`beatbox.srv.set.on_exit.${each}.what`, 'ServerMore.tsx');
    }
    for (const field of all(/'(\w+)'/g, screen.match(/const FIELDS = \[(.*?)\]/)[1])) assertKey(`beatbox.srv.field.${field}`, 'ServerMore.tsx');
    for (const said of ['ok', 'no']) assertKey(`beatbox.srv.try.said.${said}`, 'ServerMore.tsx');
    for (const kind of ['machine', 'network']) {
      assertKey(`beatbox.srv.set.${kind}`, 'ServerMore.tsx');
      assertKey(`beatbox.srv.set.turn_on_${kind}`, 'ServerMore.tsx');
    }
    for (const act of ['reset', 'destroy']) {
      for (const end of ['', '_what', '_do']) assertKey(`beatbox.srv.set.${act}${end}`, 'ServerMore.tsx');
    }
  });

  test('the sandbox screen can say all of it in words', () => {
    const screen = read('screens/Sandbox.tsx');
    // Every transition needs an icon, or its button renders blank.
    const acts = between(screen, 'const ACTS', '};');
    const pictured = new Set(all(/(\w+): '/g, acts));
    // crates/api/tests/strings.rs checks these against the engine.
    const transitions = all(/'(\w+)'/g, read('bridge/types.ts').match(/export const TRANSITIONS = \[(.*?)\]/)[1]);
    assert.equal(transitions.length, 6, 'the transition scanner stopped finding them');
    for (const transition of transitions) {
      assert.ok(pictured.has(transition), `no icon for the ${transition} button`);
    }

    const grave = screen.match(/const GRAVE = \[(.*?)\]/)[1];
    for (const name of all(/'(\w+)'/g, grave)) {
      assertKey(`beatbox.plain.sure.${name}`, 'Sandbox.tsx');
      assertKey(`beatbox.plain.sure.${name}.what`, 'Sandbox.tsx');
    }

    const table = between(screen, 'const WHY', '/** One refusal');
    const words = new Set(all(/: '(\w+)',/g, table));
    assert.ok(words.size >= 15, 'the refusal scanner stopped finding them');
    for (const word of [...words, 'unknown'].sort()) assertKey(`beatbox.plain.why.${word}`, 'Sandbox.tsx');
  });

  // Real bug: "Make it" and "It's installed" shared a slot, so a double-click
  // ended a Windows install twenty seconds in.
  test('the install dialog never puts ending it where starting it was', () => {
    const plain = read('screens/Sandbox.tsx');
    const footer = between(plain, '{busy ? (', '<div className="bb-sbx-form">');
    assert.match(footer, /disks\.finish[^}]*\}\}>\s*\{([^}]*)\}/, 'the finish button moved; check this rule still holds');
    const opening = footer.slice(0, footer.indexOf('disks.finish'));
    assert.ok(!opening.includes('autofocus'), 'the button that ends an install is autofocused');
    assert.ok(!opening.includes('variant="primary"'), 'the button that ends an install is the loud one');
  });

  // say()/fault() print engine-speak; the Sandbox screen uses its WHY table instead.
  test('the sandbox screen never writes out the engine\'s own sentence', () => {
    const plain = read('screens/Sandbox.tsx');
    const body = plain.slice(plain.indexOf('const WHY'));
    for (const leak of ['say(', 'fault(']) {
      assert.ok(!body.includes(leak), `the plain screen writes an engine sentence with ${leak}`);
    }
    assert.ok(!plain.includes('import { fault'));
    assert.ok(read('screens/Engine.tsx').includes('say('), 'the engine screen should still say them');
  });

  // Otherwise someone sees a raw <machine.no_image>. Read from source: most
  // reasons need a broken setup to trigger.
  test('every reason the engine can give exists in both languages', () => {
    const reasons = rustReasons();
    assert.ok(new Set(reasons.map(([code]) => code)).size >= 15, 'the Rust scanner stopped finding reasons');
    for (const [code, , where] of reasons) assertKey(`beatbox.${code}`, where);
  });

  // A placeholder with no value renders literally as {path}.
  test('every value a reason carries is used by its sentence', () => {
    for (const [code, given, where] of rustReasons()) {
      for (const [language, strings] of Object.entries(catalogues)) {
        const sentence = strings[`beatbox.${code}`];
        if (sentence === undefined) continue; // the test above says so
        const wanted = all(/\{(\w+)\}/g, sentence).filter((name) => !given.has(name));
        assert.deepEqual(wanted, [], `${code} (${where}) in ${language}.json asks for values nobody passes`);
      }
    }
  });

  test('every token family rendered on foundations is translated', () => {
    const tokens = read('tokens/index.ts');
    // Only the colour object; radius and space look the same.
    const block = between(tokens, 'export const color = {', 'export type ColorToken');
    const colours = all(/^ {2}(\w+): '[^']+',/gm, block);
    assert.ok(colours.includes('violet'));
    assert.ok(!colours.includes('sm'));
    for (const token of colours) assertKey(`beatbox.token.${token}`, 'tokens/index.ts');
    for (const tone of all(/^ {2}(\w+): \{ top:/gm, tokens)) assertKey(`beatbox.cube.${tone}`, 'tokens/index.ts');
    for (const step of all(/^ {2}(\w+): \{\n {4}family:/gm, tokens)) {
      assertKey(`beatbox.type.${step}.sample`, 'tokens/index.ts');
    }
    for (const verdict of all(/^ {2}([A-Z]+): '\w+',/gm, tokens)) {
      assertKey(`beatbox.verdict.${verdict.toLowerCase()}`, 'tokens/index.ts');
    }
  });
});

// Every route must be reachable. The hero once had path '' and main.tsx
// rewrote it away, so it never showed.
describe('the route table', () => {
  const routes = () => {
    const table = between(read('routes.ts'), 'export const ROUTES', 'export const NAV');
    return [...table.matchAll(/id: '([\w.]+)', path: '([\w/]*)'/g)].map((m) => [m[1], m[2]]);
  };

  test('every route has a path', () => {
    for (const [identifier, path] of routes()) assert.ok(path, `route ${identifier} has no path and cannot be opened`);
  });

  test('no two routes share a path', () => {
    const paths = routes().map(([, path]) => path);
    const duplicates = [...new Set(paths.filter((path, index) => paths.indexOf(path) !== index))].sort();
    assert.deepEqual(duplicates, [], 'these paths resolve to whichever route is declared first');
  });

  test('the default route exists', () => {
    const fallback = read('routes.ts').match(/DEFAULT_ROUTE = '([\w/]+)'/)[1];
    assert.ok(routes().some(([, path]) => path === fallback));
  });
});

// Strings for removed screens rot the locale files.
test('no screen strings without a route', () => {
  const routes = new Set(all(/id: '([\w.]+)'/g, read('routes.ts')));
  const orphans = Object.keys(catalogues.en)
    .filter((key) => key.startsWith('beatbox.screen.'))
    .filter((key) => !routes.has(key.slice(0, key.lastIndexOf('.')).slice('beatbox.screen.'.length)))
    .sort();
  assert.deepEqual(orphans, [], 'these describe screens that no longer exist');
});

describe('the two languages', () => {
  // Some matches are fine ("Terminal", commands); this catches wholesale copies.
  test('no portuguese string was left as its english twin', () => {
    const prefixes = ['beatbox.screen.', 'beatbox.foundations.', 'beatbox.token.'];
    const considered = Object.keys(catalogues.en).filter((key) => prefixes.some((prefix) => key.startsWith(prefix)));
    const identical = considered.filter((key) => catalogues.en[key] === catalogues.pt_PT[key]);
    assert.ok(identical.length < Math.floor(considered.length / 4),
      `${identical.length} of ${considered.length} strings are identical in both languages`);
  });
});
