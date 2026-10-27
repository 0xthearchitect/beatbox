// src/bridge/types.ts is a hand-written mirror of crates/contract/contract.json.
// This is the only thing stopping it drifting: a field renamed on one side
// just reads as undefined and renders as an empty cell.
// The daemon's half is crates/api/tests/contract.rs.

import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from 'node:test';

import { REPO, read, text } from './support.mjs';

const CONTRACT = JSON.parse(text(join(REPO, 'crates', 'contract', 'contract.json')));
const PRIMITIVES = { str: 'string', int: 'number', float: 'number', bool: 'boolean', any: 'unknown' };

/** snake_case -> camelCase, the same way crates/contract's door does it. */
const camel = (name) => name.replace(/_([a-z0-9])/g, (_, letter) => letter.toUpperCase());

/** Contract type -> the TypeScript we expect. */
function tsType(declared) {
  if (declared.endsWith('?')) return `${tsType(declared.slice(0, -1))} | null`;
  if (declared.endsWith('[]')) return `${tsType(declared.slice(0, -2))}[]`;
  return PRIMITIVES[declared] ?? declared;
}

/** Regex-reads `export interface X { field: type; }` blocks. types.ts stays plain so this works. */
function interfaces() {
  const found = {};
  for (const [, name, body] of read('bridge/types.ts').matchAll(/export interface (\w+)\s*\{(.*?)\n\}/gs)) {
    const entries = {};
    for (const raw of body.split('\n')) {
      const line = raw.trim();
      if (!line || line.startsWith('//') || line.startsWith('/*') || line.startsWith('*')) continue;
      const colon = line.indexOf(':');
      const field = (colon < 0 ? line : line.slice(0, colon)).trim();
      const declared = colon < 0 ? '' : line.slice(colon + 1).trim().replace(/;$/, '').trim();
      entries[field] = declared;
    }
    found[name] = entries;
  }
  return found;
}

const unquote = (key) => key.replace(/^['"]|['"]$/g, '');

test('typescript declares the same actions', () => {
  const declared = Object.keys(interfaces().Api).map(unquote).sort();
  assert.deepEqual(declared, Object.keys(CONTRACT.actions).sort());
});

test('typescript declares the right response for each action', () => {
  for (const [key, shape] of Object.entries(interfaces().Api)) {
    const action = unquote(key);
    const expected = tsType(CONTRACT.actions[action].response);
    assert.ok(shape.includes(`response: ${expected}`), `${action} responds with something else`);
  }
});

// Real bug: the page sent hintId, the backend wanted hint_id. Everything
// else agreed, so it only showed up when someone first pressed that button.
test('typescript declares the right payload for each action', () => {
  for (const [key, shape] of Object.entries(interfaces().Api)) {
    const action = unquote(key);
    const declared = CONTRACT.actions[action].payload;
    if (!declared || Object.keys(declared).length === 0) {
      assert.ok(shape.includes('payload: NoPayload'), `${action} takes nothing`);
      continue;
    }
    for (const [field, kind] of Object.entries(declared)) {
      assert.ok(shape.includes(`${camel(field)}: ${tsType(kind)}`), `${action} should take ${camel(field)}`);
    }
  }
});

test('typescript declares the same events', () => {
  const events = interfaces().Events;
  assert.deepEqual(Object.keys(events).map(unquote).sort(), Object.keys(CONTRACT.events).sort());
  for (const [key, kind] of Object.entries(events)) {
    const name = unquote(key);
    assert.equal(kind, tsType(CONTRACT.events[name].payload), name);
  }
});

test('typescript interfaces match the contract', () => {
  const found = interfaces();
  for (const [name, declared] of Object.entries(CONTRACT.types)) {
    assert.ok(name in found, `types.ts has no ${name}`);
    const expected = Object.fromEntries(Object.entries(declared).map(([field, kind]) => [camel(field), tsType(kind)]));
    assert.deepEqual(found[name], expected, name);
  }
});
