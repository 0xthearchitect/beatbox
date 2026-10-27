import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { transformSync } from 'rolldown/utils';

const read = (path) => readFileSync(new URL(path, import.meta.url), 'utf8');
const moduleUrl = (source) => `data:text/javascript;base64,${Buffer.from(source).toString('base64')}`;
const plainUrl = moduleUrl(transformSync('plain.ts', read('../src/i18n/plain.ts')).code);

async function translator(dev) {
  const source = transformSync('index.ts', read('../src/i18n/index.ts')).code
    .replace(/^import .* from "\.\.\/bridge\/client";/m,
      'const call = async () => ({strings: {"beatbox.nav.settings": "Definições"}, language: "pt_PT", available: ["en", "pt_PT"]});')
    .replace(/^import .* from "\.\/fallback";/m, 'const OFFLINE_STRINGS = {};')
    .replace('from "./plain"', `from ${JSON.stringify(plainUrl)}`)
    .replace(/^import english from .*;/m, `const english = ${read('../../locales/en.json')};`)
    .replaceAll('import.meta.env.DEV', String(dev));
  const i18n = await import(moduleUrl(source));
  await i18n.loadLocale();
  return i18n;
}

test('development falls back to English and interpolates values', async () => {
  const i18n = await translator(true);
  assert.equal(i18n.t('beatbox.nav.settings'), 'Definições');
  assert.equal(i18n.t('beatbox.build.target_exists', { path: '/disk' }).includes('/disk'), true);
  assert.equal(i18n.t('beatbox.build.target_exists', { path: '/disk' }).includes('{path}'), false);
  assert.equal(i18n.say({ code: 'build.target_exists', values: { path: '/disk' } }).includes('/disk'), true);
  assert.equal(i18n.runtimeText('beatbox.vocab.cap.future.extension'), 'beatbox.vocab.cap.future.extension');
  assert.equal(i18n.isKey('beatbox.nav.sandbxo'), false);
});

test('production exposes missing keys while entry copy and unknown reasons retain their behavior', async () => {
  const i18n = await translator(false);
  assert.equal(i18n.t('beatbox.build.target_exists'), 'beatbox.build.target_exists');
  assert.equal(i18n.entryText('beatbox.nav.settings'), 'Settings');
  assert.equal(i18n.entryReason({ code: 'future.reason' }), 'beatbox.future.reason');
  assert.equal(i18n.say({ code: 'future.reason' }, 'fallback'), 'fallback');
});
