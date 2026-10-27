// A className with no CSS rule fails silently (bb-col vs bb-column shipped
// once). Pulls every .name from selector lines; would be fooled only by a
// class name inside a content: string.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { SRC, all, relativeToSrc, stylesheet, text, walk } from './support.mjs';

// Generated at runtime by typeClasses(), invisible to the scanner.
const GENERATED_PREFIXES = ['bb-t-'];

function definedClasses() {
  const found = new Set();
  for (const line of stylesheet().split('\n')) {
    if (!line.includes('{')) continue;
    for (const name of all(/\.([\w-]+)/g, line.split('{')[0])) found.add(name);
  }
  return new Set([...found].filter((name) => name.startsWith('bb-')));
}

function usedClasses() {
  const usage = new Map();
  for (const path of walk(SRC, (name) => name.endsWith('.tsx'))) {
    for (const match of text(path).matchAll(/className=(?:"([^"]*)"|\{`([^`]*)`\})/g)) {
      for (const token of (match[1] ?? match[2]).split(/\s+/)) {
        if (!/^bb-[\w-]+$/.test(token) || GENERATED_PREFIXES.some((prefix) => token.startsWith(prefix))) continue;
        if (!usage.has(token)) usage.set(token, new Set());
        usage.get(token).add(relativeToSrc(path));
      }
    }
  }
  return usage;
}

// Literal bb-* only; computed ones like `bb-tick--${state}` can't be resolved statically.
test('every literal class used in a screen is defined', () => {
  const defined = definedClasses();
  const missing = [...usedClasses()]
    .filter(([name]) => !defined.has(name))
    .map(([name, paths]) => `  ${name}  (${[...paths].sort().join(', ')})`)
    .sort();
  assert.deepEqual(missing, [], `used but never styled:\n${missing.join('\n')}`);
});

// Canary: if global.css moves, the test above would pass by reading nothing.
test('the scanner still finds the stylesheet', () => {
  assert.ok(definedClasses().size > 100);
});

test('the scanner still finds screen usage', () => {
  assert.ok(usedClasses().size > 50);
});
