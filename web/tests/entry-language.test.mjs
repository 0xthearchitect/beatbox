import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const source = (path) => readFileSync(new URL(`../src/${path}`, import.meta.url), 'utf8');

test('entry screens use English without changing the application language', () => {
  assert.match(source('screens/Hero.tsx'), /entryText as t/);
  assert.match(source('screens/Account.tsx'), /entryText as t/);
  const i18n = source('i18n/index.ts');
  assert.match(i18n, /import english from '\.\.\/\.\.\/\.\.\/locales\/en\.json'/);
  assert.match(i18n, /export function t\([\s\S]*?const template = strings\[key\]/);
});
