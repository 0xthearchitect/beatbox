/**
 * Turn raw `network.flows` into what a person would call traffic.
 *
 * Raw flows are switch-eye: one per direction per port, so a ping is two
 * rows, SSH is port 22 plus 50750, and real conversations drown in OS
 * chatter. We make:
 *
 * - talks: one per request between two machines, reply folded in, named by
 *   service (`SSH`, not `tcp/22`), with the rule that decided;
 * - noise: broadcast/multicast, grouped by sender and kind.
 *
 * Pure, so web/tests/flows.test.mjs can feed it real captures.
 */

export interface RawFlow {
  source: string;
  source_node: string | null;
  destination: string;
  destination_node: string | null;
  protocol: string;
  port: number;
  source_port?: number;
  action: string;
  rule?: string | null;
  answer?: boolean;
  packets: number;
  /** ms since its last packet. */
  quiet_ms?: number;
  /** Route taken, when it crossed a router (`Net::flows`). */
  path?: Path;
  /** Throttled by its rule's limit. */
  limited?: boolean;
  /** Whether a reply came back. See `reset` for a "no". */
  replied?: boolean;
  reset?: boolean;
}

/** One device on the route: entry port, VLAN, delay so far. */
export interface Hop { device: string; kind: 'router' | 'switch'; port: number; vlan: number; ttl: number | null; ms: number }

/** Route, firewall verdict and how far the packet got. */
export interface Path {
  hops: Hop[]; verdict: string | null; rule: string | null; reached: string | null; ms: number;
  /** NAT on the way: `hide` (left as the router), `forward` (port forward in), `closed` (stopped at a hidden network). */
  nat?: { router: string; kind: 'hide' | 'forward' | 'closed'; from: string; to: string } | null;
}

/** A `beatbox.net.service.*` key, or just the port when we don't know it. */
export interface Service { key: string; protocol: string; port: number }

export interface Talk {
  from: string;
  to: string;
  /** Addresses as the request carried them. */
  source: string;
  destination: string;
  /** `null` when it never crossed a router. */
  path: Path | null;
  service: Service;
  allowed: boolean;
  /** `null` = the network's default policy. */
  rule: string | null;
  sent: number;
  back: number;
  /** Requests folded into this talk. */
  count: number;
  /** ms since either side last sent anything. */
  quiet: number;
  /** At least one real reply (a reset doesn't count). */
  answered: boolean;
  reset: boolean;
}

export interface Noise { from: string; service: Service; allowed: boolean; packets: number }

export interface Story { talks: Talk[]; noise: Noise[]; noisePackets: number }

const TCP: Record<number, string> = {
  21: 'ftp', 22: 'ssh', 23: 'telnet', 25: 'mail', 53: 'dns', 80: 'web', 139: 'netbios', 443: 'web_secure',
  445: 'file_sharing', 1433: 'database', 3306: 'database', 3389: 'remote_desktop', 5432: 'database',
  5900: 'vnc', 8080: 'web',
};
const UDP: Record<number, string> = {
  53: 'dns', 67: 'dhcp', 68: 'dhcp', 123: 'time', 137: 'netbios', 138: 'netbios', 1900: 'discovery',
  3702: 'discovery', 5353: 'mdns', 5355: 'llmnr',
};

/** Best guess at the service from protocol and port. */
export function service(protocol: string, port: number): Service {
  const key = protocol === 'icmp' || protocol === 'icmpv6' ? 'ping'
    : protocol === 'igmp' ? 'multicast'
    : protocol === 'tcp' ? TCP[port] ?? 'port'
    : protocol === 'udp' ? UDP[port] ?? 'port'
    : 'port';
  return { key, protocol, port };
}

const between = (flow: RawFlow) => Boolean(flow.source_node && flow.destination_node);

export function tell(flows: readonly RawFlow[]): Story {
  const direct = flows.filter(between);
  const used = new Set<RawFlow>();
  const answerOf = (request: RawFlow) => direct.find((flow) => flow !== request && !used.has(flow)
    && flow.answer === true
    && flow.source === request.destination && flow.destination === request.source
    && flow.protocol === request.protocol
    && flow.port === (request.source_port ?? -1) && flow.source_port === request.port);

  const talks = new Map<string, Talk>();
  // Requests first so each reply finds its request. Orphan replies become their own talk.
  const ordered = [...direct].sort((a, b) => Number(a.answer === true) - Number(b.answer === true));
  for (const flow of ordered) {
    if (used.has(flow)) continue;
    used.add(flow);
    const answer = answerOf(flow);
    if (answer) used.add(answer);
    const what = service(flow.protocol, flow.port);
    const allowed = flow.action === 'allow';
    const rule = flow.rule ?? null;
    const key = [flow.source_node, flow.destination_node, what.key, what.key === 'port' ? `${what.protocol}/${what.port}` : '', allowed, rule].join('|');
    const talk = talks.get(key) ?? {
      from: flow.source_node!, to: flow.destination_node!, source: flow.source, destination: flow.destination,
      path: null, service: what, allowed, rule, sent: 0, back: 0, count: 0, quiet: Infinity,
      answered: false, reset: false,
    };
    talk.count += 1;
    talk.path ??= flow.path ?? null;
    talk.quiet = Math.min(talk.quiet, flow.quiet_ms ?? Infinity, answer?.quiet_ms ?? Infinity);
    talk.sent += flow.packets;
    talk.back += answer?.packets ?? 0;
    // Older networks don't report `replied`; assume yes.
    talk.answered ||= (flow.replied ?? true) && !flow.reset;
    talk.reset ||= flow.reset === true;
    talks.set(key, talk);
  }

  const noise = new Map<string, Noise>();
  let noisePackets = 0;
  for (const flow of flows.filter((f) => !between(f))) {
    const what = service(flow.protocol, flow.port);
    const allowed = flow.action === 'allow';
    const from = flow.source_node ?? flow.source;
    const key = [from, what.key, what.key === 'port' ? `${what.protocol}/${what.port}` : '', allowed].join('|');
    const heard = noise.get(key) ?? { from, service: what, allowed, packets: 0 };
    heard.packets += flow.packets;
    noisePackets += flow.packets;
    noise.set(key, heard);
  }

  return {
    talks: [...talks.values()].sort((a, b) => Number(b.allowed) - Number(a.allowed) || b.sent - a.sent),
    noise: [...noise.values()].sort((a, b) => b.packets - a.packets),
    noisePackets,
  };
}
