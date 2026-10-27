// src/flows.ts against real flows from the office network
// pc1 pinged and SSHed into web, got refused RDP,
// and both OSes chattered on the segment.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { transform } from 'rolldown/utils';

const source = readFileSync(new URL('../src/flows.ts', import.meta.url), 'utf8');
const { code } = await transform('flows.ts', source);
const flows = await import(`data:text/javascript,${encodeURIComponent(code)}`);

const A = '10.50.20.10';
const B = '10.50.20.20';
const flow = (source, source_node, destination, destination_node, protocol, port, source_port, action, rule, answer, packets) =>
  ({ source, source_node, destination, destination_node, protocol, port, source_port, action, rule, answer, packets });

const seen = [
  flow(B, 'web', '224.0.0.251', null, 'udp', 5353, 5353, 'deny', null, false, 24),
  flow(A, 'pc1', B, 'web', 'udp', 137, 137, 'deny', null, false, 12),
  flow(A, 'pc1', '10.50.20.255', null, 'udp', 137, 137, 'deny', null, false, 12),
  flow(B, 'web', A, 'pc1', 'icmp', 1, 1, 'allow', null, true, 6),
  flow(A, 'pc1', B, 'web', 'icmp', 1, 1, 'allow', 'r5', false, 6),
  flow(B, 'web', A, 'pc1', 'tcp', 50750, 22, 'allow', null, true, 5),
  flow(A, 'pc1', B, 'web', 'tcp', 22, 50750, 'allow', 'r2', false, 5),
  flow(A, 'pc1', B, 'web', 'tcp', 3389, 50751, 'deny', null, false, 5),
  flow(A, 'pc1', '224.0.0.252', null, 'udp', 5355, 61000, 'deny', null, false, 2),
  flow(A, 'pc1', '224.0.0.252', null, 'udp', 5355, 61001, 'deny', null, false, 2),
];

test('a request and its answer are one talk, named by what it was for', () => {
  const { talks } = flows.tell(seen);
  const ssh = talks.find((talk) => talk.service.key === 'ssh');
  assert.deepEqual({ from: ssh.from, to: ssh.to, allowed: ssh.allowed, rule: ssh.rule, sent: ssh.sent, back: ssh.back },
    { from: 'pc1', to: 'web', allowed: true, rule: 'r2', sent: 5, back: 5 });
  const ping = talks.find((talk) => talk.service.key === 'ping');
  assert.equal(ping.from, 'pc1');
  assert.equal(ping.back, 6);
  // Everything web sent was a reply, so it starts no talks.
  assert.ok(!talks.some((talk) => talk.from === 'web'));
});

test('what was refused says so, and by the default when no rule decided', () => {
  const rdp = flows.tell(seen).talks.find((talk) => talk.service.key === 'remote_desktop');
  assert.equal(rdp.allowed, false);
  assert.equal(rdp.rule, null);
  assert.equal(rdp.back, 0);
});

test('announcements to nobody in particular are noise, grouped', () => {
  const { noise, noisePackets } = flows.tell(seen);
  assert.equal(noisePackets, 24 + 12 + 2 + 2);
  const llmnr = noise.filter((heard) => heard.service.key === 'llmnr');
  assert.equal(llmnr.length, 1, 'two source ports, one thing heard');
  assert.equal(llmnr[0].packets, 4);
});

test('a port nobody named keeps its number', () => {
  assert.deepEqual(flows.service('tcp', 8443), { key: 'port', protocol: 'tcp', port: 8443 });
  assert.equal(flows.service('udp', 5353).key, 'mdns');
});

test('a talk knows whether the machine answered, said no, or kept quiet', () => {
  const asked = (port, more) => ({ ...flow(A, 'pc1', B, 'web', 'tcp', port, 50800 + port, 'allow', null, false, 2), ...more });
  const { talks } = flows.tell([
    asked(22, { replied: true, reset: true }),
    asked(80, { replied: false, reset: false }),
    asked(443, { replied: true, reset: false }),
    asked(21, {}),
  ]);
  const of = (port) => talks.find((talk) => talk.service.port === port);
  assert.deepEqual([of(22).answered, of(22).reset], [false, true]);
  assert.deepEqual([of(80).answered, of(80).reset], [false, false]);
  assert.deepEqual([of(443).answered, of(443).reset], [true, false]);
  // Older networks don't report it: counts as answered.
  assert.equal(of(21).answered, true);
});
