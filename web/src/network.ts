/**
 * Firewall, cable, topology and names, page side.
 *
 * `decide` mirrors `Switch::verdict` (crates/network/src/switch.rs) so the
 * page can answer "would this get through?" without asking the network.
 * Keep them in step. Pure, so web/tests/network.test.mjs can run it
 * against a real lab's rules.
 */

export interface Rule {
  id: string;
  /** `allow`, `deny` or `reject`. */
  action: string;
  source: string;
  destination: string;
  protocol: string;
  port: number | null;
  /** The last port of a range; the same as `port` for one. */
  port_last?: number | null;
  icmp_type?: number | null;
  /** Non-IP frame type; such a rule never matches anything tried here. */
  ethertype?: number | null;
  /** `new`: only first packets, never replies. */
  state?: string;
  /** The service it was written with. */
  service?: string | null;
  /** At most `count` new connections per `per` (each machine, or all together); past that, `over`. */
  limit?: { count: number; per: string; each: boolean; over: string } | null;
  /** Only IPv4 (4) or only IPv6 (6); both when missing. */
  family?: number | null;
}

/** A machine and the node name rules refer to it by. */
export interface Place { node: string; address: string }

/** What rules are matched against: who's on the network, its range (`internet` = outside it), and whether the router goes out. */
export interface Here {
  nodes: Place[]; gateway: string; cidr?: string; internet?: boolean;
  /** The router's other networks, one per VLAN (`Net::interfaces`). */
  interfaces?: RouterNet[];
  /** Every router's networks, by VLAN: what a `vlan:N` end is. */
  vlanNets?: { vlan: number; cidr: string }[];
  /** Named groups of machines: what a `group:name` end is. */
  groups?: Record<string, string[]>;
}

/** One of the router's networks besides the main one. */
export interface RouterNet { vlan: number; cidr: string; gateway: string }

/** Stand-in address for "the internet" when testing rules. */
export const OUT_THERE = '1.1.1.1';

/** A connection's first packet. `icmpType` 8 = ping. */
export interface Attempt { source: string; destination: string; protocol: string; port: number; icmpType?: number }

const toNumber = (address: string) => {
  const parts = address.split('.').map(Number);
  if (parts.length !== 4 || parts.some((part) => !Number.isInteger(part) || part < 0 || part > 255)) return null;
  return ((parts[0]! << 24) >>> 0) + (parts[1]! << 16) + (parts[2]! << 8) + parts[3]!;
};

/** Does `spec` (address, CIDR, node, `gateway`, `any`) match? Mirrors the switch's `matches`. */
export function fits(spec: string, address: string, here: Here): boolean {
  if (spec === '' || spec === 'any' || spec === '*') return true;
  if (spec === 'gateway') return address === here.gateway || !!here.interfaces?.some((i) => i.gateway === address);
  if (spec === 'internet') return outside(address, here);
  if (spec.startsWith('vlan:')) {
    const vlan = Number(spec.slice(5));
    return !!here.vlanNets?.some((n) => n.vlan === vlan && fits(n.cidr, address, here));
  }
  if (spec.startsWith('group:')) {
    return !!here.groups?.[spec.slice(6)]?.some((member) => here.nodes.some((p) => p.node === member && p.address === address));
  }
  const node = here.nodes.find((place) => place.node === spec);
  if (node) return node.address === address;
  const [base, bits] = spec.split('/');
  const network = toNumber(base ?? '');
  const at = toNumber(address);
  const prefix = bits === undefined ? 32 : Number(bits);
  if (network === null || at === null || !Number.isInteger(prefix) || prefix < 0 || prefix > 32) return false;
  const mask = prefix === 0 ? 0 : (~0 << (32 - prefix)) >>> 0;
  return ((network & mask) >>> 0) === ((at & mask) >>> 0);
}

/** Internet = not ours, and not loopback, 0/8, link-local, multicast or broadcast (`Baseline::outside`). */
export function outside(address: string, here: Here): boolean {
  const at = toNumber(address);
  // Every router's networks are inside, not only the main one's (`Net::inside`).
  const mine = [here.cidr, ...(here.interfaces ?? []).map((i) => i.cidr), ...(here.vlanNets ?? []).map((n) => n.cidr)];
  if (!here.cidr || at === null || mine.some((cidr) => fits(cidr!, address, { nodes: [], gateway: '' }))) return false;
  const first = at >>> 24;
  return first !== 127 && first !== 0 && first < 224 && at >>> 16 !== ((169 << 8) | 254);
}

/** Whether two ranges share an address (`switch::overlap`). */
export function overlaps(a: string, b: string): boolean {
  const none = { nodes: [], gateway: '' };
  return fits(a, a.split('/')[0]!, none) && (fits(a, b.split('/')[0]!, none) || fits(b, a.split('/')[0]!, none));
}

/** Suggest a range for a VLAN: 10.50.20.0/24 + VLAN 10 -> 10.50.10.0/24, else the next free /24. */
export function suggestRange(main: string, vlan: number, taken: readonly string[]): string {
  const [a, b] = main.split('.');
  for (let step = 0; step < 256; step++) {
    const candidate = `${a}.${b}.${(vlan + step) % 256}.0/24`;
    if (![main, ...taken].some((other) => overlaps(candidate, other))) return candidate;
  }
  return '';
}

/** Does `rule` match `attempt` (IPv4 unless `family` is 6)? Port is ignored for ICMP. Mirrors `Net::verdict`. */
export function applies(rule: Rule, attempt: Attempt, here: Here, family = 4): boolean {
  const last = rule.port_last ?? rule.port;
  return rule.ethertype == null
    && (rule.family == null || rule.family === family)
    && fits(rule.source, attempt.source, here)
    && fits(rule.destination, attempt.destination, here)
    && (rule.protocol === 'any' || rule.protocol === attempt.protocol)
    && (rule.icmp_type == null || rule.icmp_type === attempt.icmpType)
    && (attempt.protocol === 'icmp' || rule.port === null || (attempt.port >= rule.port && attempt.port <= last!));
}

export interface Verdict {
  allowed: boolean;
  /** Refused with a reply, not silently dropped. */
  rejected?: boolean;
  /** `null` = default policy. */
  rule: string | null;
  /** Per rule, whether it matched. First match wins. */
  fitting: boolean[];
}

export function decide(rules: readonly Rule[], attempt: Attempt, defaultAllow: boolean, here: Here): Verdict {
  const fitting = rules.map((rule) => applies(rule, attempt, here));
  const first = fitting.indexOf(true);
  return first < 0
    ? { allowed: defaultAllow, rule: null, fitting }
    : { allowed: rules[first]!.action === 'allow', rejected: rules[first]!.action === 'reject', rule: rules[first]!.id, fitting };
}

/**
 * Per rule, given today's machines: `decides` (wins for some connection),
 * `shadowed` (matches, but an earlier rule always wins) or `idle` (matches nothing here).
 */
export function reach(rules: readonly Rule[], here: Here): Record<string, 'decides' | 'shadowed' | 'idle'> {
  const out: Record<string, 'decides' | 'shadowed' | 'idle'> = {};
  rules.forEach((rule, index) => {
    // "Any port" rules get tested on a port no rule names, so a single-port
    // rule above doesn't count as shadowing them.
    const port = rule.port ?? -1;
    const protocols = rule.protocol === 'any' ? ['tcp', 'udp', 'icmp'] : [rule.protocol];
    let fitsAny = false;
    let decidesAny = false;
    // If the router goes out, the internet is one more destination.
    const places = here.internet && here.cidr ? [...here.nodes, { node: 'internet', address: OUT_THERE }] : here.nodes;
    for (const from of here.nodes) {
      for (const to of places) {
        if (from === to) continue;
        for (const protocol of protocols) {
          const attempt = { source: from.address, destination: to.address, protocol, port, icmpType: rule.icmp_type ?? 8 };
          // IPv6-only rules get tested as IPv6.
          if (!applies(rule, attempt, here, rule.family ?? 4)) continue;
          fitsAny = true;
          if (!rules.slice(0, index).some((earlier) => applies(earlier, attempt, here, rule.family ?? 4))) decidesAny = true;
        }
      }
    }
    out[rule.id] = decidesAny ? 'decides' : fitsAny ? 'shadowed' : 'idle';
  });
  return out;
}

/** The bits of a `network.flows` row the firewall page needs. */
export interface Counted { rule?: string | null; packets: number; quiet_ms?: number }

/** Packets per rule and time since its last one. */
export function hits(flows: readonly Counted[]): Record<string, { packets: number; quiet: number }> {
  const out: Record<string, { packets: number; quiet: number }> = {};
  for (const flow of flows) {
    if (!flow.rule) continue;
    const was = out[flow.rule] ?? { packets: 0, quiet: Infinity };
    out[flow.rule] = { packets: was.packets + flow.packets, quiet: Math.min(was.quiet, flow.quiet_ms ?? Infinity) };
  }
  return out;
}

/** Services offered before "something else". */
export const SERVICES = [
  { key: 'ssh', protocol: 'tcp', port: 22 },
  { key: 'web', protocol: 'tcp', port: 80 },
  { key: 'web_secure', protocol: 'tcp', port: 443 },
  { key: 'remote_desktop', protocol: 'tcp', port: 3389 },
  { key: 'file_sharing', protocol: 'tcp', port: 445 },
  { key: 'dns', protocol: 'udp', port: 53 },
  { key: 'ping', protocol: 'icmp', port: 0 },
] as const;

/* ── the cable ─────────────────────────────────────────────────────────── */

export interface Cable { latency_ms: number; jitter_ms: number; loss_percent: number; bandwidth_kbps: number }

/** Cable presets. `bandwidth_kbps: 0` = unlimited. */
export const PRESETS: { key: string; cable: Cable }[] = [
  { key: 'perfect', cable: { latency_ms: 0, jitter_ms: 0, loss_percent: 0, bandwidth_kbps: 0 } },
  { key: 'office', cable: { latency_ms: 1, jitter_ms: 0, loss_percent: 0, bandwidth_kbps: 1000000 } },
  { key: 'branch', cable: { latency_ms: 35, jitter_ms: 8, loss_percent: 0.5, bandwidth_kbps: 20000 } },
  { key: 'mobile', cable: { latency_ms: 60, jitter_ms: 15, loss_percent: 1, bandwidth_kbps: 10000 } },
  { key: 'wifi', cable: { latency_ms: 120, jitter_ms: 40, loss_percent: 5, bandwidth_kbps: 5000 } },
];

/** Which preset this cable is; `null` = custom. */
export function presetOf(cable: Cable): string | null {
  const same = (a: Cable, b: Cable) => (Object.keys(a) as (keyof Cable)[]).every((k) => Math.abs(a[k] - b[k]) < 1e-9);
  return PRESETS.find((preset) => same(preset.cable, cable))?.key ?? null;
}

/** The cable in human terms: ping, delay, a 100 MB download, loss. */
export function feel(cable: Cable): { pingMs: number; giveMs: number; fileSeconds: number | null; lossOneIn: number | null } {
  return {
    pingMs: Math.round(2 * cable.latency_ms),
    giveMs: Math.round(2 * cable.jitter_ms),
    // 100 MB is 800 000 kbit.
    fileSeconds: cable.bandwidth_kbps > 0 ? Math.round(800000 / cable.bandwidth_kbps) : null,
    lossOneIn: cable.loss_percent > 0 ? Math.round(100 / cable.loss_percent) : null,
  };
}

/* ── the topology: devices, ports, cables, VLANs ───────────────────────── */

/** What a port carries (`crates/network/src/topology.rs`, `Mode`). */
export type PortMode = { mode: 'access'; vlan: number } | { mode: 'trunk'; native: number; allowed: number[] };

export interface Topo {
  devices: { name: string; kind: 'router' | 'switch' }[];
  /** Pairs of `name:port`. */
  cables: [string, string][];
  /** Ports not in access VLAN 1, by `name:port`. */
  ports: Record<string, PortMode>;
}

/** A machine and the `device:port` it's plugged into. */
export interface Plugged { node: string; at: string }

export const DEFAULT_MODE: PortMode = { mode: 'access', vlan: 1 };

export const modeOf = (topo: Topo, port: string): PortMode => topo.ports[port] ?? DEFAULT_MODE;

/** Access VLAN, or the trunk's native one. */
export const vlanOf = (mode: PortMode): number => (mode.mode === 'access' ? mode.vlan : mode.native);

const carries = (mode: PortMode, vlan: number) => (mode.mode === 'access' ? mode.vlan === vlan
  : mode.native === vlan || mode.allowed.length === 0 || mode.allowed.includes(vlan));

const split = (port: string): [string, number] => {
  const at = port.lastIndexOf(':');
  return [port.slice(0, at), Number(port.slice(at + 1))];
};

const across = (topo: Topo, port: string): string | null => {
  for (const [a, b] of topo.cables) {
    if (a === port) return b;
    if (b === port) return a;
  }
  return null;
};

/** `device`'s cabled ports and what's at the other end. */
const cabled = (topo: Topo, device: string): [string, string][] => topo.cables.flatMap(([a, b]) =>
  split(a)[0] === device ? [[a, b] as [string, string]] : split(b)[0] === device ? [[b, a] as [string, string]] : []);

/** Which port of `from` leads toward `to`, following the cables. */
function toward(topo: Topo, from: string, to: string, seen = new Set<string>()): string | null {
  seen.add(from);
  for (const [mine, theirs] of cabled(topo, from)) {
    const next = split(theirs)[0];
    if (seen.has(next)) continue;
    if (next === to || toward(topo, next, to, seen)) return mine;
  }
  return null;
}

/**
 * The VLAN a machine's frames reach the router in, walking cable by cable
 * like the switch engine: untagged lands in the far port's access/native
 * VLAN, tagged needs a trunk allowing it. `null` = can't reach it, so no
 * address and no gateway.
 */
export function routerVlan(topo: Topo, at: string, toRouter?: string): number | null {
  const router = toRouter ?? topo.devices.find((d) => d.kind === 'router')?.name;
  if (!router) return null;
  let vlan = vlanOf(modeOf(topo, at));
  let device = split(at)[0];
  for (let hops = 0; device !== router; hops += 1) {
    if (hops > topo.devices.length) return null;
    const out = toward(topo, device, router);
    const into = out && across(topo, out);
    if (!out || !into) return null;
    const leaving = modeOf(topo, out);
    if (!carries(leaving, vlan)) return null;
    const tagged = !(leaving.mode === 'access' || leaving.native === vlan);
    const entering = modeOf(topo, into);
    if (tagged) {
      if (entering.mode === 'access' || !carries(entering, vlan)) return null;
    } else {
      vlan = vlanOf(entering);
    }
    device = split(into)[0];
  }
  return vlan;
}

/** DHCP for one network (`dhcp.configure`). */
export interface Dhcp { on: boolean; first: number; last: number | null; lease: number; dns: string | null }

/** Extra DNS records (`dns.record`). */
export interface DnsRecord { name: string; type: 'AAAA' | 'CNAME' | 'MX' | 'TXT'; value: string; priority?: number | null }

/* ── Names, as the router answers and cuts them (switch.rs) ─────────────── */

/** A name rule: hide the name, point it at a machine, or cut connections using it. */
export interface NameRule { id: string; action: 'hide' | 'point' | 'cut'; name: string; source: string; to: string | null }

/** The router's DNS: names to addresses, other records, and its domain. */
export interface Names { dns: Record<string, string>; records: readonly DnsRecord[]; domain: string }

const bare = (name: string) => name.trim().replace(/\.+$/, '').toLowerCase();

/** Exact match, or `*.x` for x and everything under it. Mirrors `name_fits`. */
export function nameFits(pattern: string, name: string): boolean {
  const n = bare(name);
  const p = pattern.toLowerCase();
  return p.startsWith('*.') ? n === p.slice(2) || n.endsWith(`.${p.slice(2)}`) : n === p;
}

/** `nas` and `nas.lab` are the same name. Mirrors `forms`. */
function forms(name: string, domain: string): string[] {
  const n = bare(name);
  const suffix = `.${domain}`;
  return [n, n.endsWith(suffix) ? n.slice(0, -suffix.length) : `${n}${suffix}`];
}

export interface Found { address: string; by: 'record' | 'gateway' | 'machine'; machine?: string }

/** Resolve like the router: follow CNAMEs to a record, the router, or a machine by first label. Mirrors `Net::resolve`. */
export function resolveName(name: string, names: Names, here: Here): Found | null {
  let at = bare(name);
  for (let i = 0; i < 8; i++) {
    const owners = forms(at, names.domain);
    const cname = names.records.find((record) => record.type === 'CNAME' && owners.includes(bare(record.name)));
    if (!cname) break;
    at = bare(cname.value);
  }
  for (const form of forms(at, names.domain)) {
    const address = names.dns[form];
    if (address) return { address, by: 'record' };
  }
  const host = at.split('.')[0] ?? '';
  if (host === 'gateway') return { address: here.gateway, by: 'gateway' };
  const node = here.nodes.find((place) => place.node === host);
  return node ? { address: node.address, by: 'machine', machine: node.node } : null;
}

export type Asked =
  | { kind: 'hidden'; rule: string }
  | { kind: 'sent'; rule: string; to: string; address: string | null }
  | ({ kind: 'found' } & Found)
  | { kind: 'outside' }
  | { kind: 'unknown' };

/**
 * What `source` gets when it asks for `name`: first matching hide/point rule,
 * else the router's answer, else the internet's if the router goes out.
 */
export function askName(name: string, source: string, rules: readonly NameRule[], names: Names, here: Here): Asked {
  const ruled = rules.find((rule) => rule.action !== 'cut' && nameFits(rule.name, name) && fits(rule.source, source, here));
  if (ruled?.action === 'hide') return { kind: 'hidden', rule: ruled.id };
  if (ruled) {
    const to = ruled.to ?? '';
    const address = resolveName(to, names, here)?.address ?? (toNumber(to) !== null ? to : null);
    return { kind: 'sent', rule: ruled.id, to, address };
  }
  const found = resolveName(name, names, here);
  if (found) return { kind: 'found', ...found };
  return here.internet ? { kind: 'outside' } : { kind: 'unknown' };
}

/** The rule that cuts `source`'s connection to `name` (TLS SNI or HTTP Host), if any. */
export function cutBy(name: string, source: string, rules: readonly NameRule[], here: Here): string | null {
  return rules.find((rule) => rule.action === 'cut' && nameFits(rule.name, name) && fits(rule.source, source, here))?.id ?? null;
}

/** A router: its networks per VLAN and its routes. */
export interface RouterInfo {
  name: string; main: boolean;
  networks: {
    vlan: number; cidr: string; address: string;
    /** IPv6 /64, the router's address in it, and whether IPv6 is on. */
    prefix6?: string; address6?: string; ipv6?: boolean;
    /** DHCP settings and the resulting first/last address. */
    dhcp?: Dhcp; pool?: [string, string];
  }[];
  routes: { to: string; via: string }[];
  /** VLANs behind NAT. */
  hidden?: number[];
  /** Port forwards to machines behind it. */
  forwards?: { id: string; protocol: string; port: number; to: string; to_port: number; uses?: number; quiet_ms?: number | null }[];
}

/**
 * Which router gives the machine at `at` its address (`Net::serving`): the
 * first one, main first, reached in a VLAN it has a network for. Otherwise
 * the first reached at all with `served: false` (that VLAN needs a network
 * there), or `null`.
 */
export function servedBy(topo: Topo, routers: readonly RouterInfo[], at: string):
  { router: string; vlan: number; served: boolean } | null {
  let first: { router: string; vlan: number; served: boolean } | null = null;
  for (const router of routers) {
    const vlan = routerVlan(topo, at, router.name);
    if (vlan === null) continue;
    if (router.networks.some((n) => n.vlan === vlan)) return { router: router.name, vlan, served: true };
    first ??= { router: router.name, vlan, served: false };
  }
  return first;
}

/** Non-default VLANs in order; each gets a colour. */
export function vlansIn(topo: Topo): number[] {
  const seen = new Set<number>();
  for (const mode of Object.values(topo.ports)) {
    if (mode.mode === 'access') seen.add(mode.vlan);
    else { seen.add(mode.native); mode.allowed.forEach((v) => seen.add(v)); }
  }
  seen.delete(1);
  return [...seen].sort((a, b) => a - b);
}

/** One box in the editor's picture, in px. */
export interface Box { x: number; y: number; w: number; h: number }

/** A switch and its machines, or a router's own ports (`direct`). */
export interface Column extends Box {
  device: string;
  direct: boolean;
  head: Box;
  machines: (Box & { node: string })[];
  /** The "+N more" row when folded. */
  more: (Box & { count: number }) | null;
  add: Box;
}

/** A router and everything under it up to the next router. `router: null` = devices cabled to no router. */
export interface Zone extends Box {
  router: string | null;
  routerBox: Box | null;
  columns: Column[];
  /** `[x1, y1, x2, y2]` and the port each cable is. */
  lines: { line: [number, number, number, number]; port: string | null }[];
}

export interface EditorPicture {
  internet: Box;
  /** From the internet down to the main router. */
  uplink: [number, number, number, number];
  zones: Zone[];
  /** Lines between zones, with the router port each one uses. */
  links: { line: [number, number, number, number]; port: string | null }[];
  width: number;
  height: number;
}

/** Editor sizes in px, 8px grid. */
export const EDITOR = {
  column: { w: 208, gap: 16 }, head: 52, row: { h: 52, step: 60 }, more: 40, add: 40, pad: 8,
  zone: { pad: 16, gap: 56, label: 40 }, router: { w: 240, h: 60 }, internet: { w: 168, h: 40 },
  /** More machines than this and the column folds (shows fold-1 plus "+N more"). */
  fold: 6,
};

/**
 * Lay out the editor from the topology (pure). One zone per router, stacked;
 * in a zone, one column per switch plus one for the router's own ports.
 * Columns wrap, so it grows down, never sideways.
 */
export function editorPicture(topo: Topo, nodes: Plugged[], width: number, unfolded: ReadonlySet<string> = new Set()): EditorPicture {
  const E = EDITOR;
  const kind = (name: string) => topo.devices.find((d) => d.name === name)?.kind;
  const main = topo.devices.find((d) => d.kind === 'router')?.name ?? null;
  // A device's zone = nearest router on its way to the main one.
  const zoneOf = new Map<string, string>();
  const order: string[] = [];
  const parentPort = new Map<string, string>();
  // Devices with no router: one zone per connected piece, so unconnected
  // switches never look connected.
  const islands = new Set<string>();
  const walk = (root: string, island: boolean) => {
    const todo: [string, string][] = [[root, root]];
    zoneOf.set(root, root);
    if (island) islands.add(root);
    while (todo.length) {
      const [device, zone] = todo.shift()!;
      order.push(device);
      for (const [mine, theirs] of cabled(topo, device)) {
        const next = split(theirs)[0];
        if (zoneOf.has(next)) continue;
        const nextZone = !island && kind(next) === 'router' ? next : zone;
        zoneOf.set(next, nextZone);
        parentPort.set(next, mine);
        todo.push([next, nextZone]);
      }
    }
  };
  if (main) walk(main, false);
  for (const device of topo.devices) if (!zoneOf.has(device.name)) walk(device.name, true);
  const zoneNames: string[] = [];
  for (const device of order) {
    const zone = zoneOf.get(device)!;
    if (!zoneNames.includes(zone)) zoneNames.push(zone);
  }
  const plugged = (device: string) => nodes.filter((n) => split(n.at)[0] === device).sort((a, b) => split(a.at)[1] - split(b.at)[1]);
  const inner = Math.max(E.column.w, width - 2 * E.zone.pad);
  const perRow = Math.max(1, Math.floor((inner + E.column.gap) / (E.column.w + E.column.gap)));
  const internet = { x: width / 2 - E.internet.w / 2, y: 0, w: E.internet.w, h: E.internet.h };
  const zones: Zone[] = [];
  const links: EditorPicture['links'] = [];
  let y = E.internet.h + 24;
  for (const zoneName of zoneNames) {
    const router = islands.has(zoneName) ? null : zoneName;
    const devices = [
      ...(router ? [{ device: router, direct: true }] : []),
      ...order.filter((d) => zoneOf.get(d) === zoneName && kind(d) === 'switch').map((device) => ({ device, direct: false })),
    ];
    // Nothing cabled to this router: nothing to draw.
    if (!router && !devices.length) continue;
    const top = y;
    const lines: Zone['lines'] = [];
    let cursor = top + E.zone.pad;
    let routerBox: Box | null = null;
    if (router) {
      routerBox = { x: width / 2 - E.router.w / 2, y: cursor, w: E.router.w, h: E.router.h };
      cursor += E.router.h + 20;
    } else {
      cursor += E.zone.label;
    }
    const columns: Column[] = [];
    const rows = Math.ceil(devices.length / perRow);
    let previousBus = 0;
    // Rows centred on the router; extra rows hang off a spine left of the first.
    const spine = width / 2 - (Math.min(perRow, devices.length) * (E.column.w + E.column.gap) - E.column.gap) / 2 - 12;
    for (let r = 0; r < rows; r++) {
      const these = devices.slice(r * perRow, (r + 1) * perRow);
      const rowWidth = these.length * (E.column.w + E.column.gap) - E.column.gap;
      const left = width / 2 - rowWidth / 2;
      const bus = cursor;
      // Lone switch: no cable, no room for one.
      const cabledIn = router !== null || devices.length > 1;
      const colTop = bus + (cabledIn ? 20 : 0);
      let rowBottom = colTop;
      these.forEach(({ device, direct }, i) => {
        const x = left + i * (E.column.w + E.column.gap);
        const all = plugged(device);
        const folded = all.length > E.fold && !unfolded.has(device);
        const shown = folded ? all.slice(0, E.fold - 1) : all;
        const head = { x: x + E.pad, y: colTop + E.pad, w: E.column.w - 2 * E.pad, h: E.head };
        let rowY = head.y + E.head + E.pad;
        const machines = shown.map((n) => {
          const box = { node: n.node, x: head.x, y: rowY, w: head.w, h: E.row.h };
          rowY += E.row.step;
          return box;
        });
        let more: Column['more'] = null;
        if (folded) {
          more = { x: head.x, y: rowY, w: head.w, h: E.more, count: all.length - shown.length };
          rowY += E.more + E.pad;
        }
        const add = { x: head.x, y: rowY, w: head.w, h: E.add };
        const h = rowY + E.add + E.pad - colTop;
        columns.push({ device, direct, x, y: colTop, w: E.column.w, h, head, machines, more, add });
        rowBottom = Math.max(rowBottom, colTop + h);
        const centre = x + E.column.w / 2;
        if (cabledIn) lines.push({ line: [centre, bus, centre, colTop], port: direct ? null : parentPort.get(device) ?? null });
      });
      const first = left + E.column.w / 2;
      const last = left + (these.length - 1) * (E.column.w + E.column.gap) + E.column.w / 2;
      if (rows > 1) lines.push({ line: [spine, bus, last, bus], port: null });
      else if (these.length > 1) lines.push({ line: [first, bus, last, bus], port: null });
      if (r > 0) lines.push({ line: [spine, previousBus, spine, bus], port: null });
      previousBus = bus;
      cursor = rowBottom + 40;
    }
    if (routerBox && columns.length) lines.push({ line: [width / 2, routerBox.y + routerBox.h, width / 2, routerBox.y + routerBox.h + 20], port: null });
    const bottom = (columns.length ? cursor - 40 : (routerBox ? routerBox.y + routerBox.h : cursor)) + E.zone.pad;
    zones.push({ router, x: 0, y: top, w: width, h: bottom - top, routerBox, columns, lines });
    if (router && router !== main && zones.length > 1) {
      const above = zones[zones.length - 2]!;
      links.push({ line: [width / 2, above.y + above.h, width / 2, top + E.zone.pad], port: parentPort.get(zoneName) ?? null });
    }
    y = bottom + E.zone.gap;
  }
  const mainZone = zones[0];
  const uplink: [number, number, number, number] = [width / 2, internet.y + internet.h, width / 2, mainZone?.routerBox?.y ?? internet.h + 24];
  return { internet, uplink, zones, links, width, height: y - E.zone.gap };
}

/** Example address on `cidr` (the .10). */
export function sampleAddress(cidr: string): string {
  const [base, prefix] = cidr.split('/');
  const parts = (base ?? '').split('.').map(Number);
  if (parts.length !== 4 || parts.some((part) => !Number.isInteger(part) || part < 0 || part > 255) || !(Number(prefix) <= 28)) return '';
  const size = 2 ** (32 - Number(prefix));
  const start = parts.reduce((sum, part) => sum * 256 + part, 0);
  const at = start - (start % size) + 10;
  return [24, 16, 8, 0].map((shift) => Math.floor(at / 2 ** shift) % 256).join('.');
}
