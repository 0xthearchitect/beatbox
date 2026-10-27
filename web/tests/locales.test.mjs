import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { test } from 'node:test';
import { loadCatalogues, migrate, parseCatalogue, parityProblems, run, usage } from '../scripts/locales.mjs';

test('duplicate JSON keys are detected regardless of layout or escaped spelling', () => {
  assert.throws(() => parseCatalogue('{"beatbox.x.a":"a", "beatbox.x.\\u0061":"b"}', 'test'), /duplicate key/);
  assert.throws(() => parseCatalogue('{"beatbox.x.a":42}', 'test'), /string value/);
});

test('translation validation detects missing, extra and mismatched placeholders', () => {
  const problems = parityProblems({ en: { 'beatbox.x.a': '{count} {name}', 'beatbox.x.b': 'B' },
    pt_PT: { 'beatbox.x.a': '{count} {wrong}', 'beatbox.x.c': 'C' } });
  assert.equal(problems.length, 3);
  assert.ok(problems.some((problem) => problem.includes('placeholders differ')));
  assert.ok(problems.some((problem) => problem.includes('missing beatbox.x.b')));
  assert.ok(problems.some((problem) => problem.includes('extra beatbox.x.c')));
  assert.deepEqual(parityProblems({ en: { a: '{name} {count} {name}' }, pt_PT: { a: '{count} {name}' } }), []);
});

test('migration preserves every key and value and refuses to overwrite domains', () => {
  const root = mkdtempSync(join(tmpdir(), 'beatbox-locales-'));
  try {
    mkdirSync(join(root, 'locales'));
    writeFileSync(join(root, 'locales/domains.json'), JSON.stringify({ sandbox: ['plain'], entry: ['hero'] }));
    const en = { 'beatbox.plain.a': 'a {name}', 'beatbox.screen.hero.title': 'Hello' };
    const pt_PT = { 'beatbox.plain.a': 'á {name}', 'beatbox.screen.hero.title': 'Olá' };
    for (const [language, catalogue] of Object.entries({ en, pt_PT })) {
      writeFileSync(join(root, 'locales', `${language}.json`), JSON.stringify(catalogue));
    }
    migrate(root);
    assert.deepEqual(loadCatalogues(root), { en, pt_PT });
    assert.throws(() => migrate(root), /already exists/);
    writeFileSync(join(root, 'locales/en/entry.json'), JSON.stringify({ 'beatbox.plain.a': 'misplaced' }));
    assert.throws(() => loadCatalogues(root), /wrong domain|duplicate key across domains/);
    assert.deepEqual(JSON.parse(readFileSync(join(root, 'locales/en.json'), 'utf8')), en);
  } finally { rmSync(root, { recursive: true }); }
});

test('usage parses TypeScript literals and computed families without counting comments', () => {
  const root = mkdtempSync(join(tmpdir(), 'beatbox-usage-'));
  try {
    for (const folder of ['web/src', 'crates', 'shell/src']) mkdirSync(join(root, folder), { recursive: true });
    writeFileSync(join(root, 'web/src/screen.tsx'), `
      // t('beatbox.x.unused')
      const state: string = 'running';
      const label = <p>{t("beatbox.x.literal")}{runtimeText(\`beatbox.state.\${state}\`)}</p>;
    `);
    const result = usage({ 'beatbox.x.literal': 'A', 'beatbox.state.running': 'B', 'beatbox.x.unused': 'C' }, root);
    assert.deepEqual(result, { unused: ['beatbox.x.unused'], problems: [] });
    writeFileSync(join(root, 'web/src/bad.ts'), `t("beatbox.x.typo"); runtimeText(\`beatbox.typo.\${state}\`);`);
    assert.equal(usage({ 'beatbox.x.literal': 'A', 'beatbox.state.running': 'B' }, root).problems.length, 2);
  } finally { rmSync(root, { recursive: true }); }
});


test('the CI gate fails on new unused keys and stale generated artifacts', () => {
  const root = mkdtempSync(join(tmpdir(), 'beatbox-gate-'));
  try {
    for (const folder of ['locales/en', 'locales/pt_PT', 'web/src/i18n', 'crates', 'shell/src']) {
      mkdirSync(join(root, folder), { recursive: true });
    }
    writeFileSync(join(root, 'locales/domains.json'), JSON.stringify({ common: ['x'] }));
    writeFileSync(join(root, 'locales/usage-exceptions.json'), '{}');
    for (const language of ['en', 'pt_PT']) {
      writeFileSync(join(root, 'locales', language, 'common.json'), JSON.stringify({ 'beatbox.x.a': 'A' }));
    }
    writeFileSync(join(root, 'web/src/page.ts'), `t('beatbox.x.a');`);
    run(['--write'], root);
    run([], root);
    writeFileSync(join(root, 'web/src/i18n/keys.d.ts'), '// stale');
    assert.throws(() => run([], root), /stale generated file/);
    run(['--write'], root);
    for (const language of ['en', 'pt_PT']) {
      writeFileSync(join(root, 'locales', language, 'common.json'), JSON.stringify({ 'beatbox.x.a': 'A', 'beatbox.x.unused': 'B' }));
    }
    assert.throws(() => run(['--write'], root), /unused: beatbox.x.unused/);
  } finally { rmSync(root, { recursive: true }); }
});
