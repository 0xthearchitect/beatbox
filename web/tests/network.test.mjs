// src/network.ts against office's real rules: rule 1
// blocks web's RDP to pc1; rule 6 allows the whole /24 to that
// port, after it.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { transform } from 'rolldown/utils';

const source = readFileSync(new URL('../src/network.ts', import.meta.url), 'utf8');
const { code } = await transform('network.ts', source);
const net = await import(`data:text/javascript,${encodeURIComponent(code)}`);

const here = {
  gateway: '10.50.20.1',
  nodes: [{ node: 'pc1', address: '10.50.20.10' }, { node: 'web', address: '10.50.20.11' }],
};
const rule = (id, action, source, destination, protocol, port) => ({ id, action, source, destination, protocol, port });
const rules = [
  rule('r1', 'deny', 'web', 'pc1', 'tcp', 3389),
  rule('r2', 'allow', 'pc1', 'web', 'tcp', 22),
  rule('r3', 'allow', 'pc1', 'web', 'tcp', 80),
  rule('r4', 'allow', 'pc1', 'web', 'tcp', 443),
  rule('r5', 'allow', 'pc1', 'web', 'icmp', null),
  rule('r6', 'allow', '10.50.20.0/24', 'pc1', 'tcp', 3389),
];

test('the first rule that fits decides, as the switch does', () => {
  const rdp = { source: '10.50.20.11', destination: '10.50.20.10', protocol: 'tcp', port: 3389 };
  const verdict = net.decide(rules, rdp, false, here);
  assert.equal(verdict.allowed, false);
  assert.equal(verdict.rule, 'r1');
  assert.deepEqual(verdict.fitting, [true, false, false, false, false, true]);

  const ping = { source: '10.50.20.10', destination: '10.50.20.11', protocol: 'icmp', port: 0 };
  assert.equal(net.decide(rules, ping, false, here).rule, 'r5');

  const other = { source: '10.50.20.10', destination: '10.50.20.11', protocol: 'udp', port: 53 };
  assert.deepEqual(net.decide(rules, other, false, here), { allowed: false, rule: null, fitting: rules.map(() => false) });
});

test('an end is a node, an address, a CIDR, the gateway or anything', () => {
  assert.ok(net.fits('any', '8.8.8.8', here));
  assert.ok(net.fits('gateway', '10.50.20.1', here));
  assert.ok(net.fits('pc1', '10.50.20.10', here));
  assert.ok(!net.fits('pc1', '10.50.20.11', here));
  assert.ok(net.fits('10.50.20.0/24', '10.50.20.200', here));
  assert.ok(!net.fits('10.50.20.0/25', '10.50.20.200', here));
  assert.ok(net.fits('10.50.20.11', '10.50.20.11', here));
  assert.ok(!net.fits('nobody', '10.50.20.11', here));
});

test('a rule an earlier one always beats is told apart from one that decides', () => {
  const reach = net.reach(rules, here);
  assert.deepEqual(reach, { r1: 'decides', r2: 'decides', r3: 'decides', r4: 'decides', r5: 'decides', r6: 'shadowed' });
  // A rule for any port is not beaten by a rule for one port.
  const wide = [rule('a', 'deny', 'pc1', 'web', 'tcp', 22), rule('b', 'allow', 'pc1', 'web', 'tcp', null)];
  assert.equal(net.reach(wide, here).b, 'decides');
  assert.equal(net.reach([rule('c', 'allow', '192.168.0.0/16', 'any', 'any', null)], here).c, 'idle');
});

test('what each rule decided, from the flows', () => {
  const flows = [
    { rule: 'r1', packets: 3, quiet_ms: 9888921 },
    { rule: 'r1', packets: 3, quiet_ms: 8901156 },
    { rule: 'r2', packets: 5, quiet_ms: 8897611 },
    { rule: null, packets: 26, quiet_ms: 10 },
  ];
  assert.deepEqual(net.hits(flows), { r1: { packets: 6, quiet: 8901156 }, r2: { packets: 5, quiet: 8897611 } });
});

test('the cable: which preset it is, and what it feels like', () => {
  const branch = { latency_ms: 35, jitter_ms: 8, loss_percent: 0.5, bandwidth_kbps: 20000 };
  assert.equal(net.presetOf(branch), 'branch');
  assert.equal(net.presetOf({ ...branch, latency_ms: 36 }), null);
  assert.deepEqual(net.feel(branch), { pingMs: 70, giveMs: 16, fileSeconds: 40, lossOneIn: 200 });
  assert.deepEqual(net.feel({ latency_ms: 0, jitter_ms: 0, loss_percent: 0, bandwidth_kbps: 0 }),
    { pingMs: 0, giveMs: 0, fileSeconds: null, lossOneIn: null });
});

test('"internet" is everything outside the network, and never this computer', () => {
  const out = { ...here, cidr: '10.50.20.0/24', internet: true };
  assert.ok(net.fits('internet', '93.184.216.34', out));
  assert.ok(net.fits('internet', net.OUT_THERE, out));
  for (const inside of ['10.50.20.10', '10.50.20.1', '127.0.0.1', '169.254.1.1', '224.0.0.251', '255.255.255.255', '0.1.2.3']) {
    assert.ok(!net.fits('internet', inside, out), inside);
  }
  // Without the network's range, nothing is known to be outside.
  assert.ok(!net.fits('internet', '93.184.216.34', here));
});

test('a rule about the internet decides only when the router leads there', () => {
  const rules = [{ id: 'r1', action: 'deny', source: 'pc1', destination: 'internet', protocol: 'any', port: null }];
  const off = { ...here, cidr: '10.50.20.0/24', internet: false };
  assert.equal(net.reach(rules, off).r1, 'idle');
  assert.equal(net.reach(rules, { ...off, internet: true }).r1, 'decides');
  const attempt = { source: '10.50.20.10', destination: net.OUT_THERE, protocol: 'udp', port: 123 };
  assert.deepEqual(net.decide(rules, attempt, true, { ...off, internet: true }).rule, 'r1');
});

const topo = (ports = {}) => ({
  devices: [{ name: 'gw', kind: 'router' }, { name: 'sw1', kind: 'switch' }, { name: 'sw2', kind: 'switch' }],
  cables: [['gw:100', 'sw1:0'], ['sw1:7', 'sw2:0']],
  ports,
});

test('a machine reaches the router only along ports that carry its VLAN', () => {
  // Everything in VLAN 1: reached, in VLAN 1.
  assert.equal(net.routerVlan(topo(), 'sw2:1'), 1);
  // VLAN 10 at the edge, VLAN 1 on the way up: no way there.
  assert.equal(net.routerVlan(topo({ 'sw2:1': { mode: 'access', vlan: 10 } }), 'sw2:1'), null);
  // A trunk carrying 10 between the switches, and the router's side in 10: reached.
  const trunk = { mode: 'trunk', native: 1, allowed: [10] };
  const ok = topo({ 'sw2:1': { mode: 'access', vlan: 10 }, 'sw1:7': trunk, 'sw2:0': trunk,
    'sw1:0': { mode: 'access', vlan: 10 }, 'gw:100': { mode: 'access', vlan: 10 } });
  assert.equal(net.routerVlan(ok, 'sw2:1'), 10);
  // Two access ends that disagree carry it into the other VLAN, as the engine does.
  const leak = topo({ 'sw1:1': { mode: 'access', vlan: 10 }, 'sw1:0': { mode: 'access', vlan: 10 } });
  assert.equal(net.routerVlan(leak, 'sw1:1'), 1);
});

test('a VLAN is offered a range of its own next to the main one', () => {
  assert.equal(net.suggestRange('10.50.20.0/24', 10, []), '10.50.10.0/24');
  // The VLAN's own number is the main range: the next one up.
  assert.equal(net.suggestRange('10.50.20.0/24', 20, []), '10.50.21.0/24');
  assert.equal(net.suggestRange('10.50.20.0/24', 30, ['10.50.30.0/24']), '10.50.31.0/24');
  assert.ok(net.overlaps('10.50.0.0/16', '10.50.30.0/24'));
  assert.ok(!net.overlaps('10.50.20.0/24', '10.50.30.0/24'));
});

test('the router\'s other networks are not the internet, and their gateways are the gateway', () => {
  const here = { nodes: [], gateway: '10.50.20.1', cidr: '10.50.20.0/24', interfaces: [{ vlan: 10, cidr: '10.50.10.0/24', gateway: '10.50.10.1' }] };
  assert.ok(!net.outside('10.50.10.7', here));
  assert.ok(net.outside('1.1.1.1', here));
  assert.ok(net.fits('gateway', '10.50.10.1', here));
});

test('a machine is served by the first router that has a network where it arrives', () => {
  const t = {
    devices: [{ name: 'gw', kind: 'router' }, { name: 'r2', kind: 'router' }],
    cables: [['gw:0', 'r2:0']],
    ports: { 'gw:0': { mode: 'access', vlan: 99 }, 'r2:0': { mode: 'access', vlan: 99 } },
  };
  const routers = [
    { name: 'gw', main: true, networks: [{ vlan: 1, cidr: '10.0.0.0/24', address: '10.0.0.1' }], routes: [] },
    { name: 'r2', main: false, networks: [{ vlan: 99, cidr: '10.99.0.0/24', address: '10.99.0.2' }], routes: [] },
  ];
  // On r2 in VLAN 1: gw is not reached through the VLAN 99 link, r2 has no VLAN 1 network.
  assert.deepEqual(net.servedBy(t, routers, 'r2:5'), { router: 'r2', vlan: 1, served: false });
  routers[1].networks.push({ vlan: 1, cidr: '10.20.0.0/24', address: '10.20.0.1' });
  assert.deepEqual(net.servedBy(t, routers, 'r2:5'), { router: 'r2', vlan: 1, served: true });
  assert.deepEqual(net.servedBy(t, routers, 'gw:3'), { router: 'gw', vlan: 1, served: true });
});

const overlapping = (boxes) => {
  for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) {
    const a = boxes[i], b = boxes[j];
    if (!(a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y)) return [a, b];
  }
  return null;
};

test('the editor puts each router in a zone, its switches in columns and their machines under them', () => {
  const t = {
    devices: [{ name: 'gw', kind: 'router' }, { name: 'sw1', kind: 'switch' }, { name: 'sw2', kind: 'switch' }, { name: 'r2', kind: 'router' }, { name: 'sw9', kind: 'switch' }],
    cables: [['gw:1', 'sw1:0'], ['gw:2', 'sw2:0'], ['gw:3', 'r2:0']],
    ports: {},
  };
  const nodes = [{ node: 'a', at: 'gw:0' }, { node: 'b', at: 'sw1:1' }, { node: 'c', at: 'sw1:2' }, { node: 'd', at: 'r2:4' }, { node: 'e', at: 'sw9:1' }];
  const p = net.editorPicture(t, nodes, 1000);
  assert.deepEqual(p.zones.map((z) => z.router), ['gw', 'r2', null], 'main first, then r2, then what no cable joins');
  assert.deepEqual(p.zones[0].columns.map((c) => c.device), ['gw', 'sw1', 'sw2']);
  assert.deepEqual(p.zones[0].columns[1].machines.map((m) => m.node), ['b', 'c']);
  assert.deepEqual(p.zones[1].columns[0].machines.map((m) => m.node), ['d']);
  assert.deepEqual(p.zones[2].columns.map((c) => c.device), ['sw9']);
  // Zones stack down; nothing overlaps; everything inside the width.
  for (let i = 1; i < p.zones.length; i++) assert.ok(p.zones[i].y >= p.zones[i - 1].y + p.zones[i - 1].h);
  const boxes = p.zones.flatMap((z) => [...(z.routerBox ? [z.routerBox] : []), ...z.columns]);
  assert.equal(overlapping(boxes), null);
  assert.ok(boxes.every((b) => b.x >= 0 && b.x + b.w <= 1000));
  assert.equal(p.links.length, 1, 'r2 hangs from the zone above');
});

test('switches no cable joins to a router still show the cable between them', () => {
  const t = {
    devices: [{ name: 'gw', kind: 'router' }, { name: 'sw1', kind: 'switch' }, { name: 'sw2', kind: 'switch' }],
    cables: [['sw1:1', 'sw2:1']],
    ports: {},
  };
  const island = net.editorPicture(t, [], 1000).zones.find((z) => z.router === null);
  assert.deepEqual(island.columns.map((c) => c.device), ['sw1', 'sw2']);
  assert.ok(island.lines.some((l) => l.port === 'sw1:1'), 'the sw1-sw2 cable carries its port, so its words are drawn');
});

test('switches with no cable between them, nor to a router, are each a zone of their own', () => {
  const t = {
    devices: [{ name: 'gw', kind: 'router' }, { name: 'sw1', kind: 'switch' }, { name: 'sw2', kind: 'switch' }, { name: 'r9', kind: 'router' }],
    cables: [],
    ports: {},
  };
  const p = net.editorPicture(t, [], 1000);
  assert.deepEqual(p.zones.map((z) => [z.router, z.columns.map((c) => c.device)]), [['gw', ['gw']], [null, ['sw1']], [null, ['sw2']]],
    'one per switch; a router cabled to nothing has nothing to draw');
  assert.deepEqual(p.zones[1].lines, [], 'a switch on its own has no cable');
  assert.equal(p.links.length, 0);
});

test('columns that do not fit go on to the next row, and a long one folds', () => {
  const devices = [{ name: 'gw', kind: 'router' }, ...[1, 2, 3, 4, 5, 6].map((n) => ({ name: `sw${n}`, kind: 'switch' }))];
  const t = { devices, cables: devices.slice(1).map((d, i) => [`gw:${i + 1}`, `${d.name}:0`]), ports: {} };
  const nodes = [...Array(9)].map((_, i) => ({ node: `m${i}`, at: `sw1:${i + 1}` }));
  const p = net.editorPicture(t, nodes, 760);
  const columns = p.zones[0].columns;
  assert.equal(columns.length, 7);
  assert.ok(new Set(columns.map((c) => c.y)).size > 1, 'more than one row');
  assert.ok(columns.every((c) => c.x >= 0 && c.x + c.w <= 760), 'inside the width');
  assert.equal(overlapping(columns), null);
  const sw1 = columns.find((c) => c.device === 'sw1');
  assert.equal(sw1.machines.length, net.EDITOR.fold - 1);
  assert.equal(sw1.more.count, 9 - (net.EDITOR.fold - 1));
  const open = net.editorPicture(t, nodes, 760, new Set(['sw1'])).zones[0].columns.find((c) => c.device === 'sw1');
  assert.equal(open.machines.length, 9);
  assert.equal(open.more, null);
});

test('step 3 ends and matches: a VLAN, a group, a port range, a ping, reject and a frame that is not IP', () => {
  const lab = {
    gateway: '10.70.0.1', cidr: '10.70.0.0/24',
    nodes: [{ node: 'pc', address: '10.70.20.10' }, { node: 'srv', address: '10.70.0.11' }],
    vlanNets: [{ vlan: 1, cidr: '10.70.0.0/24' }, { vlan: 20, cidr: '10.70.20.0/24' }],
    groups: { Servers: ['srv'] },
  };
  const panel = { id: 'r1', action: 'reject', source: 'vlan:20', destination: 'group:Servers', protocol: 'tcp', port: 8000, port_last: 8100 };
  const tried = (port) => net.decide([panel], { source: '10.70.20.10', destination: '10.70.0.11', protocol: 'tcp', port }, true, lab);
  assert.deepEqual([tried(8080).allowed, tried(8080).rejected, tried(9000).allowed], [false, true, true]);
  // From the other VLAN, the rule does not fit.
  assert.equal(net.decide([panel], { source: '10.70.0.11', destination: '10.70.0.11', protocol: 'tcp', port: 8080 }, true, lab).rule, null);
  const ping = { id: 'r2', action: 'deny', source: 'any', destination: 'any', protocol: 'icmp', port: null, icmp_type: 8 };
  assert.equal(net.decide([ping], { source: '10.70.20.10', destination: '10.70.0.11', protocol: 'icmp', port: 0, icmpType: 8 }, true, lab).allowed, false);
  assert.equal(net.decide([ping], { source: '10.70.20.10', destination: '10.70.0.11', protocol: 'icmp', port: 0, icmpType: 0 }, true, lab).allowed, true);
  const lldp = { id: 'r3', action: 'deny', source: 'any', destination: 'any', protocol: 'any', port: null, ethertype: 0x88cc };
  assert.equal(net.decide([lldp], { source: '10.70.20.10', destination: '10.70.0.11', protocol: 'tcp', port: 22 }, true, lab).rule, null);
});

test("another router's network is not the internet", () => {
  const lab = { gateway: '10.70.0.1', cidr: '10.70.0.0/24', nodes: [], vlanNets: [{ vlan: 1, cidr: '10.70.1.0/24' }] };
  assert.equal(net.outside('10.70.1.5', lab), false);
  assert.equal(net.outside('1.1.1.1', lab), true);
});

test('a rule for one IP version is not tried on the other', () => {
  const rdp = { source: '10.50.20.11', destination: '10.50.20.10', protocol: 'tcp', port: 3389 };
  const only6 = [{ ...rules[0], family: 6 }, rules[5]];
  // The page's "would it get through?" tries IPv4: the IPv6 rule does not decide it.
  assert.equal(net.decide(only6, rdp, false, here).rule, 'r6');
  assert.equal(net.decide([{ ...rules[0], family: 4 }], rdp, true, here).rule, 'r1');
  // Yet it is not idle: tried as the IPv6 it is for, it decides.
  assert.equal(net.reach(only6, here).r1, 'decides');
});

// Name answers and cuts, mirroring switch.rs (name_fits, resolve, cut by name).
test('a name is hidden, sent, found or asked outside, as the router would answer it', () => {
  const lab = {
    gateway: '10.20.0.1', cidr: '10.20.0.0/24',
    nodes: [{ node: 'sales1', address: '10.20.20.10' }, { node: 'intranet', address: '10.20.30.5' }, { node: 'mgmt1', address: '10.20.10.10' }],
    vlanNets: [{ vlan: 20, cidr: '10.20.20.0/24' }, { vlan: 10, cidr: '10.20.10.0/24' }],
  };
  const names = { dns: { 'nas.lab': '10.20.30.9' }, records: [{ name: 'files.lab', type: 'CNAME', value: 'nas.lab' }], domain: 'lab' };
  const rules = [
    { id: 'n1', action: 'hide', name: '*.facebook.com', source: 'vlan:20', to: null },
    { id: 'n2', action: 'point', name: 'intranet.local', source: 'any', to: 'intranet' },
    { id: 'n3', action: 'cut', name: '*.example.com', source: 'any', to: null },
  ];
  const sales = '10.20.20.10';
  const mgmt = '10.20.10.10';

  assert.deepEqual(net.askName('www.facebook.com', sales, rules, names, lab), { kind: 'hidden', rule: 'n1' });
  assert.deepEqual(net.askName('facebook.com.', sales, rules, names, lab), { kind: 'hidden', rule: 'n1' }, '*. covers the name itself, and the dot at the end is the same name');
  assert.equal(net.askName('www.facebook.com', mgmt, rules, names, lab).kind, 'unknown', 'a hide for VLAN 20 does not hide it for VLAN 10');
  assert.deepEqual(net.askName('intranet.local', sales, rules, names, lab), { kind: 'sent', rule: 'n2', to: 'intranet', address: '10.20.30.5' });
  assert.deepEqual(net.askName('nas', sales, rules, names, lab), { kind: 'found', address: '10.20.30.9', by: 'record' }, 'nas and nas.lab are one name');
  assert.deepEqual(net.askName('files.lab', sales, rules, names, lab), { kind: 'found', address: '10.20.30.9', by: 'record' }, 'a CNAME is followed');
  assert.deepEqual(net.askName('mgmt1.lab', sales, rules, names, lab), { kind: 'found', address: '10.20.10.10', by: 'machine', machine: 'mgmt1' });
  assert.deepEqual(net.askName('gateway', sales, rules, names, lab), { kind: 'found', address: '10.20.0.1', by: 'gateway' });
  assert.equal(net.askName('games.example.com', sales, rules, names, lab).kind, 'unknown', 'a cut rule does not answer the name');
  assert.equal(net.askName('games.example.com', sales, rules, names, { ...lab, internet: true }).kind, 'outside');

  assert.equal(net.cutBy('games.example.com', sales, rules, lab), 'n3');
  assert.equal(net.cutBy('www.facebook.com', sales, rules, lab), null, 'a hide is not a cut');
});

test('an example address is the tenth of the network, and none for what is no IPv4 network', () => {
  assert.equal(net.sampleAddress('10.50.20.0/24'), '10.50.20.10');
  assert.equal(net.sampleAddress('192.168.1.248/28'), '192.168.1.250', 'inside the network, from its start');
  assert.equal(net.sampleAddress('172.16.0.0/12'), '172.16.0.10');
  assert.equal(net.sampleAddress('10.0.0.0/30'), '');
  assert.equal(net.sampleAddress(''), '');
  assert.equal(net.sampleAddress('fd00::/64'), '');
});
