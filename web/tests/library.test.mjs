// src/library.ts: blanks become exactly one shell word on both shells, and
// ping / Test-Connection output parses.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { transform } from 'rolldown/utils';

// esbuild strips the types, so no extra test deps.
const source = readFileSync(new URL('../src/library.ts', import.meta.url), 'utf8');
const { code } = await transform('library.ts', source);
const library = await import(`data:text/javascript,${encodeURIComponent(code)}`);

test('a value is one word on sh, quotes and all', () => {
  const blanks = { name: { kind: 'text' } };
  assert.deepEqual(library.fill('getent hosts {name}', blanks, { name: "a'; rm -rf / #" }, false),
    { line: `getent hosts 'a'\\''; rm -rf / #'` });
});

test('and on PowerShell', () => {
  const blanks = { name: { kind: 'text' } };
  assert.deepEqual(library.fill('Resolve-DnsName {name}', blanks, { name: "it's" }, true),
    { line: "Resolve-DnsName 'it''s'" });
});

test('a home-relative path still reaches home on sh', () => {
  const blanks = { path: { kind: 'path', default: '~' } };
  assert.deepEqual(library.fill('ls {path}', blanks, {}, false), { line: 'ls "$HOME"' });
  assert.deepEqual(library.fill('ls {path}', blanks, { path: '~/My Files' }, false),
    { line: `ls "$HOME"'/My Files'` });
});

test('a number has to be one, and an empty blank is named', () => {
  const blanks = { port: { kind: 'number' }, path: { kind: 'path' } };
  assert.deepEqual(library.fill('serve {port}', blanks, { port: '80; reboot' }, false), { missing: 'port' });
  assert.deepEqual(library.fill('serve {port}', blanks, { port: '8080' }, false), { line: 'serve 8080' });
  assert.deepEqual(library.fill('cat {path}', blanks, {}, false), { missing: 'path' });
});

test('the blanks of both lines, each once', () => {
  assert.deepEqual(library.blanksIn('cd {folder} && serve {port}', 'Set-Location {folder}'), ['folder', 'port']);
});

test('the answers to a ping on Linux, one lost', () => {
  const said = [
    'PING 10.50.20.10 (10.50.20.10) 56(84) bytes of data.',
    '64 bytes from 10.50.20.10: icmp_seq=1 ttl=128 time=71.2 ms',
    '64 bytes from 10.50.20.10: icmp_seq=3 ttl=128 time=64.9 ms',
    '64 bytes from 10.50.20.10: icmp_seq=4 ttl=128 time=77 ms',
    '',
    '--- 10.50.20.10 ping statistics ---',
    '4 packets transmitted, 3 received, 25% packet loss, time 3004ms',
    'rtt min/avg/max/mdev = 64.9/71.0/77.0/4.9 ms',
  ].join('\n');
  assert.deepEqual(library.readEchoes(said), [
    { from: '10.50.20.10', ms: 71.2 }, { from: '10.50.20.10', ms: 64.9 }, { from: '10.50.20.10', ms: 77 },
  ]);
});

test('the answers to Test-Connection on Windows PowerShell', () => {
  const said = [
    '',
    'Source        Destination     IPV4Address      IPV6Address                              Bytes    Time(ms) ',
    '------        -----------     -----------      -----------                              -----    -------- ',
    'PC1           10.50.20.11     10.50.20.11                                               32       68       ',
    'PC1           10.50.20.11     10.50.20.11                                               32       77       ',
  ].join('\r\n');
  assert.deepEqual(library.readEchoes(said), [{ from: '10.50.20.11', ms: 68 }, { from: '10.50.20.11', ms: 77 }]);
  assert.deepEqual(library.readEchoes('Test-Connection : Testing connection to computer failed'), []);
});
