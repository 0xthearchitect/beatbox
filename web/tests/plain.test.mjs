// src/i18n/plain.ts: program output cut to its telling line, in plain words when known.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { transform } from 'rolldown/utils';

const source = readFileSync(new URL('../src/i18n/plain.ts', import.meta.url), 'utf8');
const { code } = await transform('plain.ts', source);
const plain = await import(`data:text/javascript,${encodeURIComponent(code)}`);

const pt = JSON.parse(readFileSync(new URL('../../locales/pt_PT.json', import.meta.url), 'utf8'));
const word = (key) => pt[key];

test('a traceback is its last line, without the class name', () => {
  const said = `Traceback (most recent call last):
  File "/opt/portal-suporte/app.py", line 316, in <module>
    main()
  File "/opt/portal-suporte/app.py", line 87, in run
    raise RuntimeError("database operation failed")
RuntimeError: database operation failed`;
  assert.equal(plain.telling(said), 'database operation failed');
});

test('log decoration goes: timestamps, [tags], nginx pids', () => {
  assert.equal(plain.telling('2026-10-05 16:17:19,767 ERROR could not bind'), 'could not bind');
  assert.equal(plain.telling('[db] got terminated signal, shutting down'), 'got terminated signal, shutting down');
  assert.equal(plain.telling('[emerg] 1234#1234: unknown directive "foo" in /etc/x.conf:3'),
    'unknown directive "foo" in /etc/x.conf:3');
});

test('the telling line is the one that names the failure, not the last', () => {
  assert.equal(plain.telling('starting\nError: no such table\nbye'), 'no such table');
});

test('what every program says is said in plain words, keeping the path', () => {
  assert.equal(plain.plainSaid('Errno 111 Connection refused', word), 'nada estava à escuta do outro lado');
  assert.equal(plain.plainSaid("FileNotFoundError: [Errno 2] No such file or directory: '/opt/x/app.py'", word),
    'o ficheiro ou a pasta não existe (/opt/x/app.py)');
  assert.equal(plain.plainSaid('bind: Address already in use', word), 'a porta já está ocupada por outro programa');
  assert.equal(plain.plainSaid('', word), 'não deu nenhuma razão');
});

test('a long line is cut', () => {
  assert.ok(plain.telling('x'.repeat(400)).length <= 160);
});

test("BeatBox's own names become the interface's words", () => {
  const values = plain.plainValues(
    { capability: 'process.execute', state: 'stopped', transition: 'start', type: 'host', detail: 'panic: boom', name: 'pc1' },
    word,
  );
  assert.deepEqual(values, {
    capability: 'Correr um programa', state: 'Desligada', transition: 'Arrancar', type: 'máquina', detail: 'boom', name: 'pc1',
  });
});

test('every plain word the module can ask for exists in both languages', () => {
  const en = JSON.parse(readFileSync(new URL('../../locales/en.json', import.meta.url), 'utf8'));
  for (const key of source.matchAll(/\/[a-z]*, '([a-z_]+)'\]/g)) {
    for (const strings of [en, pt]) assert.ok(`beatbox.say.detail.${key[1]}` in strings, key[1]);
  }
  let seen = 0;
  for (const key of source.matchAll(/\/[a-z]*, '([a-z_]+)'\]/g)) seen += 1;
  assert.ok(seen >= 10, 'the known phrases were found');
  for (const strings of [en, pt]) assert.ok('beatbox.say.detail.none' in strings && 'beatbox.say.unknown' in strings);
});
