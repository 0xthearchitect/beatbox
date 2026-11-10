import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { Button, Card, FilePicker, Icon, Info, Modal, Pill, RenameTitle, Select, Switch, type IconName } from '../components';
import { call, on } from '../bridge/client';
import type { SandboxView } from '../bridge/types';
import { cableOf, plug, unplug } from '../cable';
import { TONES, remember, useChosen } from '../chosen';
import { activeLanguage, fault, say, t, runtimeText } from '../i18n';
import { service, tell, type RawFlow, type Service, type Talk } from '../flows';
import {
  EDITOR, OUT_THERE, PRESETS, SERVICES, decide, editorPicture, feel, hits, modeOf, presetOf, reach,
  fits, servedBy, suggestRange, type RouterInfo, vlanOf, vlansIn, type Cable, type RouterNet, type Here, type PortMode, type Rule, type Topo,
  type Dhcp, type DnsRecord, askName, cutBy,
} from '../network';
import { href, navigate, useRouteParams } from '../router';
import { labInEnglish } from '../network';
import labDmz from '../labs/dmz.json';
import labHome from '../labs/home.json';
import labSites from '../labs/sites.json';
import labSwitches from '../labs/switches.json';
import labVlans from '../labs/vlans.json';
import {
  DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuTrigger,
} from '@/components/animate-ui/components/radix/dropdown-menu';

/**
 * The network sandbox's pages.
 *
 * Every change is an engine action through `sandbox.act`; refusals are the
 * engine's own. Firewall/cable maths live in ../network.ts, flow reading in
 * ../flows.ts.
 */

/** ms between polls of a running network. */
const WATCH = 3000;
/** Names shown per machine before folding. */
const NAMES_SHOWN = 3;

const bag = (value: unknown): Record<string, unknown> =>
  (value && typeof value === 'object' ? value as Record<string, unknown> : {});
const rows = (value: unknown): Record<string, unknown>[] => (Array.isArray(value) ? value.map(bag) : []);

const speaking = () => activeLanguage().replace('_', '-');
const count = (n: number) => n.toLocaleString(speaking());
const hm = (at: Date) => at.toLocaleTimeString(speaking(), { hour: '2-digit', minute: '2-digit', hour12: false });
/** "ms ago" from the network -> a clock time. */
const ago = (quiet: number) => (Number.isFinite(quiet) ? hm(new Date(Date.now() - quiet)) : '');

/** A light machine (crates/network/src/light.rs): a fake PC or server, network stack only. */
interface Light { kind: 'pc' | 'server'; ping: boolean; web: boolean; echo: boolean; dhcp: boolean; gateway: string | null; dns: string | null }
interface Node {
  node: string; address: string; mac: string; at?: string; light?: Light | null; addresses6?: string[];
  /** DHCP reservation, and when the lease was last given. */
  reserved?: boolean; lease_ms?: number | null;
}
interface Lookup { node: string; name: string; count: number; answer: string | null; rule?: string | null }
/** Name rule plus how often it fired. */
interface NameRuleInfo { id: string; action: 'hide' | 'point' | 'cut'; name: string; source: string; to: string | null; uses: number }

/** Run one network capability; throws the engine's reason. */
async function act(id: string, capability: string, args: Record<string, unknown> = {}) {
  const outcome = await call('sandbox.act', { id, capability, arguments: args });
  if (!outcome.ok) throw { reason: outcome.reason, message: capability };
  return bag(outcome.data);
}

/** The chosen network, re-observed every WATCH ms while running. */
function useNetwork() {
  const chosen = useChosen('network');
  const servers = useChosen('server');
  // Resolve labels at render time; node ids remain stable for flow matching.
  const label = (node: string) => servers.list?.find((server) => server.id === node)?.name ?? endWord(node);
  const network = chosen.machine;
  const running = network?.state.lifecycle === 'running';
  const { reload } = chosen;
  useEffect(() => {
    if (!running) return;
    const beat = window.setInterval(() => void reload(), WATCH);
    return () => window.clearInterval(beat);
  }, [running, reload]);
  const facts = bag(network?.state.facts);
  const nodes = rows(facts.nodes) as unknown as Node[];
  const firewall = bag(facts.firewall);
  const num = (value: unknown) => (value == null ? null : Number(value));
  const rules: Rule[] = rows(firewall.rules).map((rule) => ({
    id: String(rule.id), action: String(rule.action), source: String(rule.source), destination: String(rule.destination),
    protocol: String(rule.protocol ?? 'any'), port: num(rule.port), port_last: num(rule.port_last),
    icmp_type: num(rule.icmp_type), ethertype: num(rule.ethertype),
    state: String(rule.state ?? 'any'), service: rule.service == null ? null : String(rule.service),
    limit: rule.limit && typeof rule.limit === 'object' ? (() => {
      const l = bag(rule.limit);
      return { count: Number(l.count), per: String(l.per), each: l.each !== false, over: String(l.over) };
    })() : null,
    family: num(rule.family),
  }));
  const groups = Object.fromEntries(Object.entries(bag(firewall.groups)).map(([name, who]) =>
    [name, Array.isArray(who) ? who.map(String) : []])) as Record<string, string[]>;
  const services = Object.fromEntries(Object.entries(bag(firewall.services)).map(([name, raw]) => {
    const what = bag(raw);
    return [name, { protocol: String(what.protocol ?? 'tcp'), port: num(what.port), port_last: num(what.port_last) }];
  })) as Record<string, { protocol: string; port: number | null; port_last: number | null }>;
  const internet = facts.internet === true;
  vlanNames = Object.fromEntries(Object.entries(bag(facts.vlan_names)).map(([vlan, name]) => [Number(vlan), String(name)]));
  const interfaces: RouterNet[] = rows(facts.interfaces).map((row) => ({
    vlan: Number(row.vlan), cidr: String(row.cidr), gateway: String(row.gateway),
  }));
  const routers: RouterInfo[] = rows(facts.routers).map((row) => ({
    name: String(row.name), main: row.main === true,
    networks: rows(row.networks).map((n) => ({
      vlan: Number(n.vlan), cidr: String(n.cidr), address: String(n.address),
      prefix6: n.prefix6 == null ? undefined : String(n.prefix6), address6: n.address6 == null ? undefined : String(n.address6),
      ipv6: n.ipv6 === true,
      dhcp: n.dhcp ? (() => {
        const d = bag(n.dhcp);
        return { on: d.on !== false, first: Number(d.first ?? 10), last: num(d.last), lease: Number(d.lease ?? 600), dns: d.dns == null ? null : String(d.dns) };
      })() : undefined,
      pool: Array.isArray(n.pool) ? [String(n.pool[0]), String(n.pool[1])] as [string, string] : undefined,
    })),
    routes: rows(row.routes).map((r) => ({ to: String(r.to), via: String(r.via) })),
    hidden: Array.isArray(row.hidden) ? row.hidden.map(Number) : [],
    forwards: rows(row.forwards).map((f) => ({
      id: String(f.id), protocol: String(f.protocol), port: Number(f.port), to: String(f.to), to_port: Number(f.to_port),
      uses: Number(f.uses ?? 0), quiet_ms: f.quiet_ms == null ? null : Number(f.quiet_ms),
    })),
  }));
  const here: Here = {
    nodes: nodes.map(({ node, address }) => ({ node, address })), gateway: String(facts.gateway ?? ''),
    cidr: String(facts.cidr ?? ''), internet, interfaces, groups,
    vlanNets: routers.flatMap((r) => r.networks.map(({ vlan, cidr }) => ({ vlan, cidr }))),
  };
  const names: NameRuleInfo[] = rows(firewall.names).map((n) => ({
    id: String(n.id), action: String(n.action) as NameRuleInfo['action'], name: String(n.name), source: String(n.source ?? 'any'),
    to: n.to == null ? null : String(n.to), uses: Number(n.uses ?? 0),
  }));
  const records = rows(facts.records) as unknown as DnsRecord[];
  const dnsFrom = Object.fromEntries(Object.entries(bag(facts.dns_from)).map(([name, from]) => [name, String(from)]));
  const domain = String(facts.domain ?? 'local');
  /** The Server whose machine node `id` is, if it is one. */
  const serverOf = (id: string) => servers.list?.find((server) => server.id === id);
  const serverIds = (servers.list ?? []).map((server) => server.id).join(' ');
  useEffect(() => {
    let live = true;
    void Promise.all(serverIds.split(' ').filter(Boolean).map((id) => call('sandbox.view', { id }))).then((views) => {
      if (!live) return;
      serverPorts = Object.fromEntries(views.flatMap((view) => rows(bag(view.state.facts).endpoints).map((endpoint) =>
        [`${String(endpoint.transport ?? 'tcp')} ${String(endpoint.address)}:${String(endpoint.port)}`, t('beatbox.net.flows.server_service', { service: String(endpoint.service), server: view.name })])));
    }).catch(() => undefined);
    return () => { live = false; };
  }, [serverIds, network?.id]);
  return {
    ...chosen, network, running, facts, nodes, rules, names, here, label, internet, interfaces, routers, groups, services,
    records, dnsFrom, domain, serverOf,
    defaultAllow: String(firewall.default ?? 'allow') === 'allow',
    answers: firewall.answers !== false,
    refresh: reload,
  };
}

type Net = ReturnType<typeof useNetwork>;

/** Flows, polled while running. */
function useFlows(id: string, running: boolean) {
  const [flows, setFlows] = useState<RawFlow[] | null>(null);
  const [lookups, setLookups] = useState<Lookup[]>([]);
  const look = useCallback(async () => {
    if (!id) return;
    const answer = await act(id, 'network.flows');
    setFlows(rows(answer.flows) as unknown as RawFlow[]);
    setLookups(rows(answer.lookups).map((row) => ({
      node: String(row.node), name: String(row.name), count: Number(row.count ?? 0),
      answer: row.answer ? String(row.answer) : null, rule: row.rule ? String(row.rule) : null,
    })));
  }, [id]);
  useEffect(() => {
    setFlows(null);
    void look().catch(() => undefined);
    if (!running) return;
    const beat = window.setInterval(() => void look().catch(() => undefined), WATCH);
    return () => window.clearInterval(beat);
  }, [look, running]);
  return { flows, lookups };
}

/** All machines in full, so we can read their cables. */
function useMachines(stamp: unknown) {
  const [machines, setMachines] = useState<SandboxView[]>([]);
  const load = useCallback(async () => {
    const hosts = (await call('sandbox.list')).filter((row) => row.type === 'host');
    setMachines(await Promise.all(hosts.map((row) => call('sandbox.view', { id: row.id }))));
  }, []);
  useEffect(() => { void load().catch(() => undefined); }, [load, stamp]);
  useEffect(() => on('sandbox', (event) => {
    if (event.name.startsWith('sandbox.')) void load().catch(() => undefined);
  }), [load]);
  return { machines, reload: () => load().catch(() => undefined) };
}

const off = (machine: SandboxView) => ['created', 'stopped'].includes(machine.state.lifecycle);

const rangeBlock = (net: Net, blockedBy: string[]) => net.running ? t('beatbox.net.range.blocked_network')
  : blockedBy.length ? t('beatbox.net.range.blocked_machines', { names: blockedBy.join(', ') }) : '';

/* ── words ────────────────────────────────────────────────────────────── */

const named = (what: Service) =>
  (what.key === 'port' ? `${what.protocol}/${what.port}` : runtimeText(`beatbox.net.service.${what.key}`));

/** "remote shell (SSH)" -> "SSH". */
const short = (what: Service) => named(what).match(/\(([^)]+)\)$/)?.[1] ?? named(what);

/** A rule end in words: a machine, "anything", "the gateway", "anything on 10.0.0.0/24". */
function endWord(spec: string): string {
  if (spec === 'any' || spec === '*' || spec === '') return t('beatbox.net.end.any');
  if (spec === 'gateway') return t('beatbox.net.end.gateway');
  if (spec === 'internet') return t('beatbox.net.end.internet');
  if (spec.startsWith('vlan:')) {
    const vlan = Number(spec.slice(5));
    return `VLAN ${vlan}${vlanNames[vlan] ? ` · ${vlanNames[vlan]}` : vlan === 1 ? ` · ${t('beatbox.net.router.design.principal')}` : ''}`;
  }
  if (spec.startsWith('group:')) return spec.slice(6);
  if (spec.includes('/')) return t('beatbox.net.end.cidr', { cidr: spec });
  return spec;
}

/** Service name plus the protocol/ports behind it. */
function ruleWhat(rule: Rule): { words: string; code: string } {
  if (rule.ethertype != null) return { words: t('beatbox.net.rule.not_ip'), code: `0x${rule.ethertype.toString(16).padStart(4, '0')}` };
  if (rule.protocol === 'icmp') {
    return rule.icmp_type == null || rule.icmp_type === 8 ? { words: named(service('icmp', 0)), code: rule.icmp_type == null ? '' : 'icmp 8' }
      : { words: '', code: t('beatbox.net.rule.icmp_code', { type: rule.icmp_type }) };
  }
  if (rule.protocol === 'any') {
    return rule.port === null ? { words: t('beatbox.net.rule.anything'), code: '' } : { words: '', code: t('beatbox.net.rule.port_only', { port: rule.port }) };
  }
  const code = portsText(rule.protocol, rule.port, rule.port_last);
  if (rule.service) return { words: rule.service, code };
  if (!['tcp', 'udp'].includes(rule.protocol)) {
    return { words: rule.protocol.toUpperCase(), code: /^\d+$/.test(rule.protocol) ? t('beatbox.net.rule.protocol_n', { n: rule.protocol }) : '' };
  }
  if (rule.port === null) return { words: t('beatbox.net.rule.any_port', { protocol: rule.protocol }), code: '' };
  const what = service(rule.protocol, rule.port);
  return { words: what.key === 'port' || (rule.port_last ?? rule.port) !== rule.port ? '' : short(what), code };
}

const verb = (action: string) => runtimeText(`beatbox.net.verb.${action === 'allow' || action === 'reject' ? action : 'deny'}`);

function Mark({ ok }: { ok: boolean }) {
  return <span className={`bb-net-mark ${ok ? 'bb-net-mark--ok' : 'bb-net-mark--no'}`}><Icon name={ok ? 'check' : 'close'} size="xs" /></span>;
}

/** Which rule decided, as a link to the firewall. */
function ByRule({ talk }: { talk: Talk }) {
  if (talk.rule?.startsWith('n')) return <a href={href('network/firewall')}>{t('beatbox.net.names.rule_n', { n: talk.rule.slice(1) })}</a>;
  if (!talk.rule) return <>{t(talk.allowed ? 'beatbox.net.flows.by_default_allow' : 'beatbox.net.flows.by_default_deny')}</>;
  return <a href={href('network/firewall')}>{t('beatbox.net.flows.by_rule', { id: talk.rule.replace(/^r/, '') })}</a>;
}

function talkSaid(talk: Talk): string {
  return !talk.allowed ? t('beatbox.net.flows.tried', { count: count(talk.sent) })
    : talk.back ? t('beatbox.net.flows.sent_back', { sent: count(talk.sent), back: count(talk.back) })
    : t('beatbox.net.flows.no_answer', { sent: count(talk.sent) });
}

function TalkRow({ talk, label }: { talk: Talk; label: (node: string) => string }) {
  return (
    <div className="bb-net-talk">
      <Mark ok={talk.allowed} />
      <span className="bb-net-talkwhat">
        <span><b>{label(talk.from)} → {label(talk.to)}</b> <span className="bb-muted">· {namedTo(talk)}</span>
          {talk.service.key !== 'port' && talk.service.key !== 'ping' && talk.service.key !== 'multicast'
            ? <code className="bb-mono bb-faint"> {talk.service.protocol} {talk.service.port}</code> : null}</span>
        <small>{talkSaid(talk)} · <ByRule talk={talk} /></small>
      </span>
      <small className="bb-mono bb-faint">{ago(talk.quiet)}</small>
    </div>
  );
}

/* ── the frame every page shares ──────────────────────────────────────── */

function Page({ page, children, net, title, lede, info, actions }: {
  page: 'overview' | 'router' | 'names' | 'firewall' | 'traffic' | 'flows';
  children: ReactNode;
  net: Net;
  title?: ReactNode;
  /** One line of facts under the title. */
  lede?: ReactNode;
  /** Text for the ⓘ by the title. */
  info: string;
  actions?: ReactNode;
}) {
  const making = useRouteParams().get('new') === '1';
  return (
    <>
      <div className="bb-pagehead">
        <div>
          <div className="bb-pagehead-title">
            <h1 className="bb-t-title bb-net-title" style={{ margin: 0 }}>{title ?? t(`beatbox.screen.network.${page}.title`)}</h1>
            <Info text={net.network ? info : t('beatbox.net.blurb')} />
          </div>
          {net.network && lede ? <p className="bb-lede">{lede}</p> : null}
        </div>
        {net.network && !making ? actions : null}
      </div>
      {making || (net.list && !net.list.length) ? <NewNetwork />
        : net.network ? children : null}
    </>
  );
}

/** Starting points: empty (router only), a bundled lab, or a file. */
const LABS = { switches: labSwitches, home: labHome, vlans: labVlans, sites: labSites, dmz: labDmz } as Record<string, NetFile>;
const STARTS = ['empty', ...Object.keys(LABS)];

/** Labs ship with Portuguese names: a page in English gets them in English. */
function labIn(lab: NetFile): NetFile {
  return activeLanguage().startsWith('pt') ? lab : labInEnglish(lab);
}

/** A network as `network.export` writes it (crates/network/src/sandbox.rs). */
interface NetFile {
  format: string; version: number; cidr: string;
  firewall: { default: string; rules: unknown[] };
  nodes: { node: string; light?: unknown }[];
  topology: { devices: { kind: string }[] };
  routers: { forwards: unknown[] }[];
}

/** "1 router, 3 switches, 6 light machines, 9 rules and 1 forward". Routers counted beyond the main one. */
function fileMakes(file: NetFile): string {
  const devices = file.topology?.devices ?? [];
  const parts = ([
    // Everyone has the main router; count only extras.
    ['routers', devices.filter((d) => d.kind === 'router').length - 1],
    ['switches', devices.filter((d) => d.kind === 'switch').length],
    ['lights', (file.nodes ?? []).filter((n) => n.light).length],
    ['rules', file.firewall?.rules?.length ?? 0],
    ['forwards', (file.routers ?? []).reduce((sum, r) => sum + (r.forwards?.length ?? 0), 0)],
  ] as const).filter(([, n]) => n > 0)
    .map(([what, n]) => runtimeText(`beatbox.net.lab.n.${what}${n === 1 ? '_one' : ''}`, { count: n }));
  return new Intl.ListFormat(speaking(), { type: 'conjunction' }).format(parts);
}

/** Thumbnail of each starting point, drawn like the Overview. */
function LabSketch({ kind }: { kind: string }) {
  const colour = (n: number) => `var(${VLAN_COLOURS[n]})`;
  const [v1, v2, v3] = [colour(0), colour(1), colour(2)];
  const wires: ReactNode[] = [];
  const tiles: ReactNode[] = [];
  const tile = (x: number, y: number, icon: IconName, tone?: string, w = 34, h = 26, empty = false) => tiles.push(
    <span key={tiles.length} className={`bb-lab-tile${empty ? ' bb-lab-tile--empty' : ''}`}
          style={{ left: x - w / 2, top: y - h / 2, width: w, height: h, ...(tone ? { color: tone, borderColor: tone } : {}) }}>
      <Icon name={icon} size="sm" /></span>);
  const wire = (points: [number, number][], tone?: string, dashed = false) => wires.push(
    <path key={wires.length} d={points.map(([x, y], i) => `${i ? 'L' : 'M'}${x} ${y}`).join(' ')}
          style={tone ? { stroke: tone } : undefined} strokeDasharray={dashed ? '3 3' : undefined} />);
  const label = (x: number, y: number, text: string, tone?: string) => tiles.push(
    <span key={tiles.length} className="bb-lab-label" style={{ left: x, top: y, color: tone }}>{text}</span>);
  const zone = (x: number, y: number, w: number, h: number, text: string, tone: string) => {
    tiles.unshift(<span key={`z${x}`} className="bb-lab-zone" style={{ left: x, top: y, width: w, height: h, borderColor: tone }} />);
    label(x + 6, y - 7, text, tone);
  };
  const drop = (x: number, from: number, bus: number, to: number, dx: number, tone?: string) =>
    wire([[x, from], [x, bus], [x + dx, bus], [x + dx, to]], tone);
  const router = 'var(--bb-color-violet)';
  const outside = 'var(--bb-color-mint)';
  // Router plus dashed placeholders.
  if (kind === 'empty') {
    // Dashed cables stop at the edges of the empty places, not under them.
    wire([[60, 20], [83, 20]], outside, true);
    for (const dx of [-58, 0, 58]) wire([[100, 33], [100, 56], [100 + dx, 56], [100 + dx, 74]], undefined, true);
    tile(46, 20, 'globe', undefined, 28, 20, true); tile(100, 20, 'router', router);
    for (const dx of [-58, 0, 58]) tile(100 + dx, 86, 'plus', undefined, 34, 24, true);
  }
  if (kind === 'switches') {
    wire([[55, 22], [145, 22]]);
    for (const x of [55, 145]) {
      drop(x, 35, 48, 56, -22, v1); drop(x, 35, 48, 56, 22, v2);
      tile(x, 22, 'network'); tile(x - 22, 66, 'terminal', v1, 28, 22); tile(x + 22, 66, 'terminal', v2, 28, 22);
    }
    label(4, 84, 'VLAN 10', v1); label(150, 84, 'VLAN 20', v2);
  }
  if (kind === 'home') {
    wire([[46, 20], [100, 20]], outside); wire([[100, 20], [100, 54]]);
    for (const dx of [-58, 0, 58]) drop(100, 67, 74, 80, dx);
    tile(46, 20, 'globe', outside, 28, 20); tile(100, 20, 'router', router); tile(100, 54, 'network');
    tile(42, 90, 'terminal', undefined, 28, 20); tile(100, 90, 'terminal', undefined, 28, 20); tile(158, 90, 'server', undefined, 28, 20);
  }
  if (kind === 'vlans') {
    wire([[100, 12], [100, 50]]);
    ([[-62, v1], [0, v2], [62, v3]] as const).forEach(([dx, tone]) => drop(100, 63, 72, 80, dx, tone));
    tile(100, 12, 'router', router); tile(100, 50, 'network'); label(108, 25, 'trunk');
    tile(38, 91, 'terminal', v1, 28, 22); tile(100, 91, 'terminal', v2, 28, 22); tile(162, 91, 'server', v3, 28, 22);
  }
  if (kind === 'sites') {
    zone(4, 6, 90, 90, t('beatbox.net.lab.sketch.hq'), 'var(--bb-color-text-faint)');
    zone(106, 6, 90, 90, t('beatbox.net.lab.sketch.branch'), router);
    wire([[49, 30], [151, 30]], v1);
    for (const x of [49, 151]) {
      drop(x, 43, 56, 64, -22); drop(x, 43, 56, 64, 22);
      tile(x, 30, 'router', router); tile(x - 22, 75, 'terminal', undefined, 28, 20); tile(x + 22, 75, 'server', undefined, 28, 20);
    }
  }
  if (kind === 'dmz') {
    const dmz = 'var(--bb-color-amber)';
    zone(4, 50, 90, 46, 'DMZ', dmz); zone(106, 50, 90, 46, t('beatbox.net.lab.sketch.inside'), outside);
    wire([[46, 14], [100, 14]], outside);
    wire([[100, 27], [100, 36], [49, 36], [49, 50]], dmz); wire([[100, 36], [151, 36], [151, 50]], outside);
    tile(46, 14, 'globe', outside, 28, 20); tile(100, 14, 'router', router);
    tile(32, 75, 'server', dmz, 28, 20); tile(66, 75, 'server', dmz, 28, 20);
    tile(134, 75, 'terminal', undefined, 28, 20); tile(168, 75, 'terminal', undefined, 28, 20);
  }
  return (
    <span className="bb-lab-sketch" aria-hidden="true">
      <svg className="bb-lab-wires" viewBox="0 0 200 104">{wires}</svg>
      {tiles}
    </span>
  );
}

function NewNetwork() {
  const [name, setName] = useState('lab');
  const [cidr, setCidr] = useState('10.10.10.0/24');
  const [start, setStart] = useState('empty');
  const [file, setFile] = useState<{ name: string; state: NetFile } | null>(null);
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  const [picking, setPicking] = useState(false);
  const state = start === 'file' ? file?.state : LABS[start] && labIn(LABS[start]);

  function choose(each: string) {
    setStart(each);
    // Labs and files bring a range; prefill it, still editable.
    if (LABS[each]) setCidr(LABS[each].cidr);
  }

  /** Read the picked file through the window. */
  async function read(path: string) {
    setTold('');
    setPicking(false);
    let text: string;
    try {
      text = await call('files.read', { path });
    } catch (error) {
      setTold(fault(error));
      return;
    }
    try {
      const parsed = JSON.parse(text) as NetFile;
      if (parsed?.format !== 'beatbox-network') throw new Error();
      setFile({ name: path.split(/[\\/]/).pop() ?? path, state: parsed });
      setStart('file');
      setCidr(parsed.cidr);
    } catch {
      setTold(t('beatbox.network.bad_file'));
    }
  }

  async function make() {
    setBusy(true);
    setTold('');
    const asked = cidr.trim();
    try {
      const made = await call('sandbox.create', {
        type: 'network', name: name.trim(), cpu: 0, memory: 0,
        settings: { cidr: state ? state.cidr : asked, firewall: { default: state?.firewall?.default ?? 'allow' } },
        uses: {},
      });
      // Build fully before selecting it, so the page never sees it half made.
      // If the import is refused, no network is left behind.
      if (state) {
        try {
          await act(made.id, 'network.import', { state });
          // New range: renumber like the Router page does (addresses, names, rules).
          if (asked !== state.cidr) {
            const plan = await act(made.id, 'network.renumber_plan', { cidr: asked }) as unknown as Renumbering;
            await call('sandbox.reconfigure', { id: made.id, cpu: 0, memory: 0, settings: plan.settings });
          }
        } catch (error) {
          await call('sandbox.transition', { id: made.id, transition: 'destroy' }).catch(() => undefined);
          throw error;
        }
      }
      remember('network', made.id);
      navigate('network/overview');
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }

  const expects = file ? file.state.nodes.filter((n) => !n.light).map((n) => n.node) : [];
  return (
    <Card title={<><Icon name="network" size="sm" />{t('beatbox.net.new.title')}</>} info={t('beatbox.net.new.what')}>
      <div className="bb-sbx-pair bb-net-first">
        <label className="bb-field">
          <span>{t('beatbox.net.new.name')}</span>
          <input className="bb-input" value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="bb-field">
          <span>{t('beatbox.net.new.cidr')} <Info text={t(state ? 'beatbox.net.new.cidr_from_lab' : 'beatbox.net.new.cidr_hint')} /></span>
          <input className="bb-input bb-mono" value={cidr} onChange={(e) => setCidr(e.target.value)} />
        </label>
      </div>
      <div className="bb-field" style={{ marginTop: 24 }}>
        <span>{t('beatbox.net.new.start')}</span>
        <div className="bb-lab-grid">
          {STARTS.map((each) => (
            <button key={each} type="button" className={`bb-lab${start === each ? ' on' : ''}`} onClick={() => choose(each)}>
              <LabSketch kind={each} />
              <b>{runtimeText(`beatbox.net.lab.${each}`)}</b>
              <small>{runtimeText(`beatbox.net.lab.${each}.what`)}</small>
            </button>
          ))}
        </div>
        {picking ? <FilePicker title={t('beatbox.net.lab.import')} suffixes=".bbn .beatbox-network.json" filterLabel={t('beatbox.net.lab.import.filter')}
                               start="" onPick={(path) => void read(path)} onClose={() => setPicking(false)} /> : null}
        {file && start === 'file' ? (
          <div className="bb-lab-file on">
            <Icon name="file" size="sm" />
            <span>
              <b>{file.name}</b>
              <small>{t('beatbox.net.lab.makes', { parts: fileMakes(file.state) })}</small>
              {expects.length ? <small className="bb-lab-expects"><Icon name="alert" size="sm" />
                {t(expects.length === 1 ? 'beatbox.net.lab.expects_one' : 'beatbox.net.lab.expects', { names: expects.join(', ') })}</small> : null}
            </span>
            <Button size="sm" variant="ghost" onClick={() => { setFile(null); setStart('empty'); }}>{t('beatbox.net.lab.cancel')}</Button>
          </div>
        ) : (
          <button type="button" className="bb-lab-file" onClick={() => setPicking(true)}>
            <Icon name="file" size="sm" />
            <b>{t('beatbox.net.lab.import')}</b>
          </button>
        )}
      </div>
      <div className="bb-sbx-settings-foot">
        <span />
        <Button variant="primary" icon="plus" disabled={busy || !name.trim()} onClick={() => void make()}>
          {t('beatbox.net.new.make')}
        </Button>
      </div>
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </Card>
  );
}

/* ── Overview ─────────────────────────────────────────────────────────── */

const ACTS = { start: 'play', stop: 'stop', reset: 'refresh', destroy: 'trash' } as const;

/** Start, stop, reset, delete. The last two confirm first. */
function Moves({ net, cabled }: { net: Net; cabled: SandboxView[] }) {
  const network = net.network!;
  const [asking, setAsking] = useState('');
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');

  async function move(transition: string) {
    setAsking('');
    setTold('');
    if (transition === 'destroy') {
      const on = cabled.filter((machine) => !off(machine));
      if (on.length) {
        setTold(t('beatbox.net.destroy_running', { names: on.map((m) => m.name).join(', ') }));
        return;
      }
    }
    setBusy(true);
    try {
      if (transition === 'destroy') {
        // Unplug its machines, or they hold a cable to a dead switch.
        for (const machine of cabled) await unplug(machine);
      }
      await call('sandbox.transition', { id: network.id, transition });
      await net.reload();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }

  return (
    <div className="bb-net-moves">
      <div className="bb-net-movebtns">
        {(['start', 'stop', 'reset', 'destroy'] as const).filter((name) => network.transitions.includes(name)).map((name) => {
          const word = t(`beatbox.net.do.${name}`);
          return name === 'start' || name === 'stop' ? (
            <Button key={name} icon={ACTS[name]} disabled={busy} variant={name === 'start' ? 'primary' : 'ghost'} onClick={() => void move(name)}>{word}</Button>
          ) : (
            <button key={name} type="button" className={`bb-act bb-act--${name === 'destroy' ? 'danger' : 'plain'}`}
                    title={word} aria-label={word} disabled={busy} onClick={() => setAsking(name)}>
              <Icon name={ACTS[name]} size="sm" />
            </button>
          );
        })}
      </div>
      {/* Its own modal: inline, the text pushed the header buttons around.
          No autofocus, so a stray Enter never deletes a network. */}
      {asking ? (
        <Modal title={runtimeText(`beatbox.net.sure.${asking}`, { name: network.name })} icon={ACTS[asking as 'reset' | 'destroy']}
               width={440} onClose={() => setAsking('')}
               footer={<>
                 <span style={{ flex: 1 }} />
                 <Button onClick={() => setAsking('')}>{t('beatbox.net.keep')}</Button>
                 <Button variant="danger" onClick={() => void move(asking)}>{runtimeText(`beatbox.net.do.${asking}`)}</Button>
               </>}>
          <p className="bb-closing" style={{ margin: 0 }}>{runtimeText(`beatbox.net.sure.${asking}.what`)}</p>
        </Modal>
      ) : null}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12, margin: 0 }}>{told}</p> : null}
    </div>
  );
}

/** Switch counters as one bar and one line. */
function Counted({ counters, since }: { counters: Record<string, unknown>; since: string }) {
  const n = (key: string) => Number(counters[key] ?? 0);
  const nowhere = n('refused') + n('unroutable');
  const parts = [
    { key: 'delivered', value: n('delivered'), tone: 'mint' },
    { key: 'blocked', value: n('blocked'), tone: 'rose' },
    { key: 'lost', value: n('lost'), tone: 'amber' },
    { key: 'nowhere', value: nowhere, tone: 'faint' },
  ];
  return (
    <div className="bb-net-strip">
      <div className="bb-net-bar" aria-hidden="true">
        {parts.filter((part) => part.value).map((part) => (
          <span key={part.key} className={`bb-net-bar--${part.tone}`} style={{ flexGrow: part.value }} />
        ))}
      </div>
      <div className="bb-net-legend">
        <span><b>{count(n('received'))}</b> {t('beatbox.net.count.since', { time: since })}</span>
        {parts.map((part) => (
          <span key={part.key}><i className={`bb-net-dot bb-net-dot--${part.tone}`} />{runtimeText(`beatbox.net.count.${part.key}_n`, { count: count(part.value) })}</span>
        ))}
      </div>
      {/* Broadcasts are delivered once per machine, so deliveries can exceed packets. */}
      {n('delivered') > n('received') ? (
        <p className="bb-faint" style={{ margin: 0, fontSize: 12 }}>{t('beatbox.net.count.fanout')}</p>
      ) : null}
    </div>
  );
}

/** Element width, live. */
function useWidth(fallback: number) {
  const [width, setWidth] = useState(fallback);
  const [element, setElement] = useState<HTMLElement | null>(null);
  useEffect(() => {
    if (!element) return;
    const watch = new ResizeObserver(([entry]) => setWidth(entry!.contentRect.width));
    watch.observe(element);
    return () => watch.disconnect();
  }, [element]);
  return [width, setElement] as const;
}

/** The editor's current selection. */
type Selection = { kind: 'router' | 'switch' | 'machine'; name: string } | null;

/** Topology from the network's facts. */
function topoOf(net: Net): Topo {
  const raw = bag(net.facts.topology);
  const devices = rows(raw.devices).map((d) => ({ name: String(d.name), kind: d.kind === 'switch' ? 'switch' as const : 'router' as const }));
  const cables = (Array.isArray(raw.cables) ? raw.cables : []).map((c) => [String((c as unknown[])[0]), String((c as unknown[])[1])] as [string, string]);
  const ports: Record<string, PortMode> = {};
  for (const [port, mode] of Object.entries(bag(raw.ports))) {
    const m = bag(mode);
    ports[port] = m.mode === 'trunk'
      ? { mode: 'trunk', native: Number(m.native ?? 1), allowed: (Array.isArray(m.allowed) ? m.allowed : []).map(Number) }
      : { mode: 'access', vlan: Number(m.vlan ?? 1) };
  }
  return { devices: devices.length ? devices : [{ name: 'gw', kind: 'router' }], cables, ports };
}

/** VLAN colours (VLAN 1 has none). Always shown with the number, never colour alone. */
const VLAN_COLOURS = ['--bb-color-violet', '--bb-color-mint', '--bb-color-amber', '--bb-color-sky', '--bb-color-rose'];
/** VLANs on screen in order, so neighbours never share a colour. Set from the facts. */
let paletteOrder: number[] = [];
/** VLAN names on screen, set with `paletteOrder`. */
let vlanNames: Record<number, string> = {};

/** `protocol address:port` -> "portal · Server loja", so Flows can name Server services (issue #52). */
let serverPorts: Record<string, string> = {};

/** A talk's service, using the Server's name when one runs it. */
const namedTo = (talk: Talk) => serverPorts[`${talk.service.protocol} ${talk.destination}:${talk.service.port}`] ?? named(talk.service);
const vlanColour = (vlan: number) => {
  const at = paletteOrder.indexOf(vlan);
  return vlan === 1 || at < 0 ? undefined : `var(${VLAN_COLOURS[at % VLAN_COLOURS.length]})`;
};

function VlanChip({ vlan, main = false }: { vlan: number; main?: boolean }) {
  const colour = vlanColour(vlan);
  return <span className="bb-net-vlan" style={colour ? { color: colour, borderColor: colour } : undefined}>VLAN {vlan}{vlanNames[vlan] ? ` · ${vlanNames[vlan]}` : main ? ` · ${t('beatbox.net.router.design.principal')}` : ''}</span>;
}

/** Rename a VLAN in place. Empty clears it. */
function VlanName({ net, vlan }: { net: Net; vlan: number }) {
  const name = vlanNames[vlan] ?? '';
  const [text, setText] = useState(name);
  const [told, setTold] = useState('');
  useEffect(() => setText(name), [name]);
  async function save() {
    if (text.trim() === name) return;
    setTold('');
    try {
      await act(net.network!.id, 'vlan.name', { vlan, name: text.trim() });
      await net.refresh();
    } catch (error) {
      setTold(fault(error));
      setText(name);
    }
  }
  const colour = vlanColour(vlan);
  return (
    <span className="bb-net-vlankey">
      <i style={{ background: colour ?? 'var(--bb-color-text-faint)' }} />
      <b className="bb-mono">{vlan}</b>
      <input className="bb-net-vlaninput" value={text} placeholder={t('beatbox.net.vlans.name_ph')} maxLength={24}
             aria-label={t('beatbox.net.vlans.name_of', { vlan })} onChange={(e) => setText(e.target.value)}
             onBlur={() => void save()} onKeyDown={(e) => { if (e.key === 'Enter') (e.target as HTMLInputElement).blur(); }} />
      {told ? <small className="bb-sbx-no">{told}</small> : null}
    </span>
  );
}

/** "VLAN 10", "trunk 10 · 20". */
function modeWords(mode: PortMode): string {
  if (mode.mode === 'access') return t('beatbox.net.port.access', { vlan: mode.vlan });
  return mode.allowed.length ? t('beatbox.net.port.trunk', { vlans: mode.allowed.join(' · ') }) : t('beatbox.net.port.trunk_all');
}

/** Who owns a node that isn't one of our VMs: a Server, or attached by hand. */
const ownerOf = (net: Net, machines: SandboxView[], node: string) =>
  machines.find((m) => cableOf(m)?.sandbox === net.network?.id && cableOf(m)?.node === node);

/** Remove the selection. Switches and extra routers hand their machines to the main router; VMs unplug only when off. */
async function removeSelected(net: Net, machines: SandboxView[], selected: NonNullable<Selection>) {
  const id = net.network!.id;
  if (selected.kind === 'machine') {
    const machine = ownerOf(net, machines, selected.name);
    if (machine && !off(machine)) throw new Error(t('beatbox.net.nodes.stop_first', { name: machine.name }));
    await (machine ? unplug(machine) : act(id, 'network.detach', { node: selected.name }));
    return;
  }
  if (net.routers.find((r) => r.name === selected.name)?.main) return;
  await act(id, 'device.remove', { device: selected.name });
}

/** Cable label: its mode unless plain VLAN 1, and its impairments if any. */
function cableWords(net: Net, topo: Topo, port: string): string {
  const mode = modeOf(topo, port);
  const cable = cableTrafficOf(net, port);
  return [
    mode.mode === 'access' && mode.vlan === 1 ? '' : modeWords(mode),
    presetOf(cable) === 'perfect' ? '' : cableLine(cable),
  ].filter(Boolean).join(' · ');
}

/** Port chips on a switch head; past this the last one says "+N". */
const PORTS_SHOWN = 6;

/** Draws `editorPicture`'s layout. Click selects; the selected device gets its Remove. */
function EditorPicture({ net, machines, reload, selected, onSelect }: {
  net: Net; machines: SandboxView[]; reload: () => Promise<unknown>; selected: Selection;
  onSelect: (s: Selection) => void;
}) {
  const network = net.network!;
  const topo = topoOf(net);
  const plugged = net.nodes.map((node) => ({ node: node.node, at: node.at ?? '' }));
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  const [unfolded, setUnfolded] = useState<Set<string>>(new Set());
  const others = machines.filter((machine) => cableOf(machine)?.sandbox !== network.id);
  const [room, measure] = useWidth(800);
  const laid = editorPicture(topo, plugged, Math.max(EDITOR.column.w + 2 * EDITOR.zone.pad, room), unfolded);
  const isOn = (kind: string, name: string) => selected?.kind === kind && selected.name === name;

  async function run(work: () => Promise<unknown>) {
    setBusy(true);
    setTold('');
    try {
      await work();
      await Promise.all([net.reload(), reload()]);
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  const line = ([x1, y1, x2, y2]: [number, number, number, number], key: string, port: string | null, extra = '') => {
    const mode = port ? modeOf(topo, port) : null;
    const colour = mode?.mode === 'access' && mode.vlan !== 1 ? vlanColour(mode.vlan) : undefined;
    const thick = 2;
    const style = x1 === x2
      ? { left: x1 - thick / 2, top: Math.min(y1, y2), width: thick, height: Math.abs(y2 - y1) }
      : { left: Math.min(x1, x2), top: y1 - thick / 2, width: Math.abs(x2 - x1), height: thick };
    return <i key={key} className={`bb-net-cord${extra}`} style={{ ...style, ...(colour ? { background: colour } : {}) }} />;
  };

  return (
    <div className="bb-net-editorpic" ref={measure}>
      <div className="bb-net-stage" style={{ width: laid.width, height: laid.height }}>
        {laid.zones.length > 1 ? laid.zones.map((zone) => (
          <div key={`zone-${zone.y}`} className="bb-net-zonebox" style={{ left: zone.x, top: zone.y, width: zone.w, height: zone.h }} />
        )) : null}
        {line(laid.uplink, 'uplink', null, net.internet ? ' bb-net-cord--on' : ' bb-net-cord--free')}
        {laid.links.map((link, i) => line(link.line, `link-${i}`, link.port))}
        {laid.zones.flatMap((zone, z) => zone.lines.map((l, i) => line(l.line, `l-${z}-${i}`, l.port)))}
        {laid.zones.flatMap((zone) => zone.columns.map((column) => (
          <div key={`col-${column.device}`} className={`bb-net-col${!column.direct && isOn('switch', column.device) ? ' on' : ''}`}
               style={{ left: column.x, top: column.y, width: column.w, height: column.h }} />
        )))}
        {laid.zones.flatMap((zone, z) => zone.lines.filter((l) => l.port).map((l, i) => {
          const words = cableWords(net, topo, l.port!);
          return words ? <span key={`w-${z}-${i}`} className="bb-net-cablelabel" style={{ left: l.line[0] + 8, top: l.line[1] + 2 }}>{words}</span> : null;
        }))}
        {laid.links.map((link, i) => (link.port ? (
          <span key={`lw-${i}`} className="bb-net-cablelabel" style={{ left: link.line[0] + 12, top: (link.line[1] + link.line[3]) / 2 - 8 }}>
            {[t('beatbox.net.topo.cable_to', { end: link.port }), cableWords(net, topo, link.port)].filter(Boolean).join(' · ')}</span>
        ) : null))}
        <a className={`bb-net-outside${net.internet ? ' on' : ''}`} href={href('network/router')}
           style={{ left: laid.internet.x, top: laid.internet.y, width: laid.internet.w, height: laid.internet.h }}>
          <Icon name="globe" size="sm" />
          <span><b>{t('beatbox.net.internet.title')}</b><small>{t(net.internet ? 'beatbox.net.internet.on_short' : 'beatbox.net.internet.none_short')}</small></span>
        </a>
        {laid.zones.map((zone) => {
          if (!zone.routerBox) {
            return <span key={`island-${zone.y}`} className="bb-net-zonelabel" style={{ left: zone.x + EDITOR.zone.pad, top: zone.y + EDITOR.zone.pad }}>{t('beatbox.net.topo.no_link')}</span>;
          }
          const box = zone.routerBox;
          const name = zone.router!;
          const networks = net.routers.find((r) => r.name === name)?.networks ?? [];
          const more = networks.length - 1;
          const on = isOn('router', name);
          return (
            <Fragment key={`router-${name}`}>
              <button type="button" className={`bb-net-dev${on ? ' on' : ''}`} style={{ left: box.x, top: box.y, width: box.w, height: box.h }}
                      onClick={() => onSelect(on ? null : { kind: 'router', name })}>
                <Icon name="router" size="sm" />
                <span><b>{t('beatbox.net.router.title')} · {name}</b>
                  <code className="bb-mono">{networks[0]?.address ?? t('beatbox.net.topo.no_address')}{more > 0
                    ? ` · ${t(more === 1 ? 'beatbox.net.topo.more_one' : 'beatbox.net.topo.more', { count: more })}` : ''}</code></span>
              </button>
            </Fragment>
          );
        })}
        {laid.zones.flatMap((zone) => zone.columns.map((column) => {
          const head = column.head;
          const on = !column.direct && isOn('switch', column.device);
          const ports = [
            ...net.nodes.filter((n) => n.at?.startsWith(`${column.device}:`)).map((n) => n.at!),
            ...topo.cables.flatMap(([a, b]) => [a, b]).filter((end) => end.startsWith(`${column.device}:`)),
          ].sort((a, b) => Number(a.split(':')[1]) - Number(b.split(':')[1]));
          return (
            <Fragment key={`c-${column.device}`}>
              {column.direct ? (
                <button type="button" className="bb-net-colhead bb-net-colhead--direct" style={{ left: head.x, top: head.y, width: head.w, height: head.h }}
                        onClick={() => onSelect({ kind: 'router', name: column.device })}>
                  <span><b>{t('beatbox.net.topo.direct', { router: column.device })}</b></span>
                </button>
              ) : (
                <button type="button" className={`bb-net-colhead${on ? ' on' : ''}`} style={{ left: head.x, top: head.y, width: head.w, height: head.h }}
                        onClick={() => onSelect(on ? null : { kind: 'switch', name: column.device })}>
                  <Icon name="network" size="sm" />
                  <span><b>{t('beatbox.net.switch.title')} · {column.device}</b>
                    <span className="bb-net-ports" aria-hidden="true">
                      {ports.slice(0, ports.length > PORTS_SHOWN ? PORTS_SHOWN - 1 : PORTS_SHOWN).map((port) => {
                        const mode = modeOf(topo, port);
                        const colour = mode.mode === 'access' ? vlanColour(mode.vlan) : undefined;
                        return <i key={port} className={mode.mode === 'trunk' ? 'trunk' : ''} style={colour ? { background: colour, borderColor: colour } : undefined}>
                          {mode.mode === 'trunk' ? 'T' : mode.vlan}</i>;
                      })}
                      {ports.length > PORTS_SHOWN ? <i className="more">+{ports.length - PORTS_SHOWN + 1}</i> : null}
                    </span></span>
                </button>
              )}
              {column.machines.map((row) => {
                const node = net.nodes.find((n) => n.node === row.node)!;
                const machine = ownerOf(net, machines, node.node);
                const where = servedBy(topo, net.routers, node.at ?? '');
                const vlan = vlanOf(modeOf(topo, node.at ?? ''));
                const picked = isOn('machine', node.node);
                return (
                  <button key={`m-${node.node}`} type="button" className={`bb-net-row${picked ? ' on' : ''}`}
                          style={{ left: row.x, top: row.y, width: row.w, height: row.h }}
                          onClick={() => onSelect(picked ? null : { kind: 'machine', name: node.node })}>
                    <Icon name={machine ? 'box' : node.light ? (node.light.kind === 'server' ? 'server' : 'terminal') : 'server'} size="sm" />
                    <span>
                      <b>{machine ? <i className={`bb-net-dot ${machine.state.lifecycle === 'running' ? 'bb-net-dot--mint' : 'bb-net-dot--faint'}`} /> : null}
                        {machine?.name ?? net.label(node.node)}
                        {machine ? <em className="bb-net-kind">{t('beatbox.net.topo.vm')}</em>
                          : node.light ? <em className="bb-net-kind">{t('beatbox.net.topo.light')}</em> : null}</b>
                      {/* "No router at all" is said once per zone, not per machine. */}
                      {where && !where.served ? <small className="bb-net-warn">{t('beatbox.net.topo.no_network', { vlan: where.vlan })}</small>
                        : <small><code className="bb-mono">{node.address}</code>{vlan !== 1 ? <span className="bb-net-vdot" style={{ color: vlanColour(vlan) }}>■ {vlan}</span> : null}</small>}
                    </span>
                  </button>
                );
              })}
              {column.more ? (
                <button type="button" className="bb-net-more" style={{ left: column.more.x, top: column.more.y, width: column.more.w, height: column.more.h }}
                        onClick={() => setUnfolded(new Set([...unfolded, column.device]))}>
                  {t('beatbox.net.topo.more_machines', { count: column.more.count })}</button>
              ) : null}
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button type="button" className="bb-net-plug" disabled={busy || !others.length}
                          style={{ left: column.add.x, top: column.add.y, width: column.add.w, height: column.add.h }}
                          title={others.length ? t('beatbox.net.topo.plug_into', { device: column.device }) : t('beatbox.net.palette.no_vms')}>
                    <Icon name="plus" size="xs" />{t('beatbox.net.topo.plug_here')}
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="center" sideOffset={6} className="w-72 rounded-lg">
                  <DropdownMenuLabel className="text-muted-foreground text-xs font-normal">{t('beatbox.net.topo.plug_into', { device: column.device })}</DropdownMenuLabel>
                  {others.map((machine) => (
                    <DropdownMenuItem key={machine.id} className="gap-2.5 p-2" disabled={!off(machine)}
                                      onSelect={() => void run(() => plug(machine, network.id, column.device))}>
                      <span className="bg-accent flex size-7 shrink-0 items-center justify-center rounded-lg"><Icon name="box" size="sm" /></span>
                      <span className="flex min-w-0 flex-1 flex-col">
                        <span className="truncate text-[13.5px] font-medium">{machine.name}</span>
                        <span className="text-muted-foreground text-xs">{!off(machine) ? t('beatbox.net.nodes.stop_first_short') : runtimeText(`beatbox.plain.state.${machine.state.lifecycle}`)}</span>
                      </span>
                    </DropdownMenuItem>
                  ))}
                </DropdownMenuContent>
              </DropdownMenu>
            </Fragment>
          );
        }))}
      </div>
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </div>
  );
}

/** A cable's own impairments. Machines by name, device links by either end. */
function cableTrafficOf(net: Net, cable: string): Cable {
  const row = rows(net.facts.cable_traffic).find((c) => c.node === cable || c.a === cable || c.b === cable);
  return {
    latency_ms: Number(row?.latency_ms ?? 0), jitter_ms: Number(row?.jitter_ms ?? 0),
    loss_percent: Number(row?.loss_percent ?? 0), bandwidth_kbps: Number(row?.bandwidth_kbps ?? 0),
  };
}

/** Frames carried by a cable since start. */
function framesOf(net: Net, cable: string): string {
  const row = rows(net.facts.cable_frames).find((c) => c.node === cable || c.a === cable || c.b === cable);
  if (!row) return t('beatbox.net.cable.no_frames');
  return row.node !== undefined
    ? t('beatbox.net.cable.frames_machine', { sent: count(Number(row.sent ?? 0)), received: count(Number(row.received ?? 0)) })
    : t('beatbox.net.cable.frames', { n: count(Number(row.frames ?? 0)) });
}

/** One cable's impairments on top of the network's: a preset or custom numbers. */
function CableEditor({ net, cable }: { net: Net; cable: string }) {
  const now = cableTrafficOf(net, cable);
  const preset = presetOf(now);
  const [own, setOwn] = useState(false);
  const [draft, setDraft] = useState<Cable>(now);
  const [told, setTold] = useState('');
  const [busy, setBusy] = useState(false);
  async function apply(next: Cable) {
    setBusy(true);
    setTold('');
    try {
      await act(net.network!.id, 'cable.configure', { cable, ...next });
      await net.refresh();
      setOwn(false);
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  const field = (key: keyof Cable, label: string) => (
    <label className="bb-net-portfield"><span>{label}</span>
      <input className="bb-input bb-mono" inputMode="decimal" value={String(draft[key])}
             onChange={(e) => setDraft({ ...draft, [key]: Number(e.target.value) || 0 })} /></label>
  );
  return (
    <div>
      <div className="bb-net-inline" style={{ marginTop: 0 }}>
        <Select label={t('beatbox.net.cable.its')} value={own || preset === null ? 'own' : preset} disabled={busy}
                options={[...PRESETS.map(({ key }) => ({ value: key, label: runtimeText(`beatbox.net.preset.${key}`) })),
                  { value: 'own', label: t('beatbox.net.preset.own') }]}
                onChange={(value) => {
                  if (value === 'own') { setDraft(now); setOwn(true); return; }
                  void apply(PRESETS.find((p) => p.key === value)!.cable);
                }} />
        <small className="bb-faint">{preset === 'perfect' ? t('beatbox.net.cable.adds_nothing') : cableLine(now)}</small>
      </div>
      {own ? (
        <div className="bb-net-porteditor" style={{ marginTop: 8 }}>
          {field('latency_ms', t('beatbox.net.cable.ms'))}
          {field('jitter_ms', t('beatbox.net.cable.jitter'))}
          {field('loss_percent', t('beatbox.net.cable.loss'))}
          {field('bandwidth_kbps', t('beatbox.net.cable.kbps'))}
          <Button size="sm" variant="primary" disabled={busy} onClick={() => void apply(draft)}>{t('beatbox.net.port.apply')}</Button>
        </div>
      ) : null}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </div>
  );
}

/** Edit a machine's address in place, within its network. */
function AddressEdit({ net, node, address }: { net: Net; node: string; address: string }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(address);
  const [told, setTold] = useState('');
  const [busy, setBusy] = useState(false);
  async function save() {
    setBusy(true);
    setTold('');
    try {
      await act(net.network!.id, 'node.address', { node, address: text.trim() });
      await net.refresh();
      setEditing(false);
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  if (!editing) {
    return (
      <span className="bb-net-inline" style={{ marginTop: 0 }}>
        <code className="bb-mono">{address}</code>
        <button type="button" className="bb-net-link" style={{ marginTop: 0 }} onClick={() => { setText(address); setEditing(true); }}>
          {t('beatbox.net.topo.change')}</button>
      </span>
    );
  }
  return (
    <span>
      <span className="bb-net-inline" style={{ marginTop: 0 }}>
        <input className="bb-input bb-mono" style={{ width: 140, height: 32 }} value={text} autoFocus
               onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') void save(); }} />
        <Button size="sm" variant="primary" disabled={busy} onClick={() => void save()}>{t('beatbox.net.port.apply')}</Button>
        <button type="button" className="bb-net-link" style={{ marginTop: 0 }} onClick={() => setEditing(false)}>{t('beatbox.net.range.cancel')}</button>
      </span>
      {told ? <small className="bb-sbx-no" style={{ display: 'block', marginTop: 4 }}>{told}</small> : null}
    </span>
  );
}

/** Port mode: access VLAN, or trunk with native + allowed VLANs. */
function PortEditor({ net, port }: { net: Net; port: string }) {
  const topo = topoOf(net);
  const now = modeOf(topo, port);
  const [mode, setMode] = useState(now.mode);
  const [vlan, setVlan] = useState(String(now.mode === 'access' ? now.vlan : 1));
  const [native, setNative] = useState(String(now.mode === 'trunk' ? now.native : 1));
  const [allowed, setAllowed] = useState(now.mode === 'trunk' ? now.allowed.join(', ') : '');
  const [told, setTold] = useState('');
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    setMode(now.mode);
    setVlan(String(now.mode === 'access' ? now.vlan : 1));
    setNative(String(now.mode === 'trunk' ? now.native : 1));
    setAllowed(now.mode === 'trunk' ? now.allowed.join(', ') : '');
  }, [port, JSON.stringify(now)]);
  async function apply() {
    setBusy(true);
    setTold('');
    try {
      const list = allowed.split(/[\s,]+/).filter(Boolean).map(Number);
      await act(net.network!.id, 'port.configure', mode === 'access'
        ? { port, mode, vlan: Number(vlan) } : { port, mode, native: Number(native), allowed: list });
      await net.refresh();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  return (
    <div className="bb-net-porteditor">
      <span className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.net.port.mode')}>
        {(['access', 'trunk'] as const).map((each) => (
          <button key={each} type="button" role="radio" aria-checked={mode === each} className={mode === each ? 'on' : ''}
                  onClick={() => setMode(each)}>{t(`beatbox.net.port.${each}_mode`)}</button>
        ))}
      </span>
      <i className="bb-net-break" />
      {mode === 'access' ? (
        <label className="bb-net-portfield"><span>VLAN</span>
          <input className="bb-input bb-mono" inputMode="numeric" value={vlan} onChange={(e) => setVlan(e.target.value)} /></label>
      ) : (
        <>
          <label className="bb-net-portfield"><span>{t('beatbox.net.port.native')}</span>
            <input className="bb-input bb-mono" inputMode="numeric" value={native} onChange={(e) => setNative(e.target.value)} /></label>
          <label className="bb-net-portfield bb-net-portfield--wide"><span>{t('beatbox.net.port.allowed')}</span>
            <input className="bb-input bb-mono" placeholder={t('beatbox.net.port.allowed_all')} value={allowed} onChange={(e) => setAllowed(e.target.value)} /></label>
        </>
      )}
      <Button size="sm" variant="primary" disabled={busy} onClick={() => void apply()}>{t('beatbox.net.port.apply')}</Button>
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12, margin: 0, flexBasis: '100%' }}>{told}</p> : null}
    </div>
  );
}

/** Side panel for the selection: actions first, then details. */
function Inspector({ net, machines, reload, selected, onSelect, onRemove }: {
  net: Net; machines: SandboxView[]; reload: () => Promise<unknown>; selected: NonNullable<Selection>;
  onSelect: (s: Selection) => void; onRemove: (s: NonNullable<Selection>) => void;
}) {
  const network = net.network!;
  const topo = topoOf(net);
  const [told, setTold] = useState('');
  const [busy, setBusy] = useState(false);
  const [connectTo, setConnectTo] = useState('');
  const [open, setOpen] = useState<string | null>(null);
  const [tab, setTab] = useState<'ports' | 'networks' | 'routes'>('ports');
  async function run(work: () => Promise<unknown>, after?: () => void) {
    setBusy(true);
    setTold('');
    try {
      await work();
      await Promise.all([net.refresh(), reload()]);
      after?.();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  const close = (
    <button type="button" className="bb-net-x" aria-label={t('beatbox.net.topo.close')} onClick={() => onSelect(null)}><Icon name="close" size="xs" /></button>
  );
  const danger = (label: string, disabled = false) => (
    <Button size="sm" disabled={busy || disabled} onClick={() => onRemove(selected)}>
      <span className="bb-net-danger"><Icon name="trash" size="xs" />{label}</span></Button>
  );

  if (selected.kind === 'machine' && net.nodes.find((n) => n.node === selected.name)?.light) {
    return <LightInspector net={net} name={selected.name} close={close} onRemove={() => onRemove(selected)} onRenamed={(next) => onSelect({ kind: 'machine', name: next })} />;
  }

  if (selected.kind === 'machine') {
    const node = net.nodes.find((n) => n.node === selected.name);
    if (!node?.at) return null;
    const where = servedBy(topo, net.routers, node.at);
    const devices = topo.devices.map((d) => d.name);
    const machine = ownerOf(net, machines, node.node);
    const running = !!machine && !off(machine);
    return (
      <Card title={<><Icon name={machine ? 'box' : 'server'} size="sm" />{machine?.name ?? net.label(node.node)}</>} note={close}>
        <div className="bb-net-actions bb-net-first">
          {danger(t('beatbox.net.topo.unplug_machine'), running)}
          {running ? <small className="bb-faint">{t('beatbox.net.nodes.stop_first', { name: machine!.name })}</small> : null}
        </div>
        <div className="bb-net-facts" style={{ marginTop: 16 }}>
          <span>{t('beatbox.net.topo.address')}</span><AddressEdit net={net} node={node.node} address={node.address} />
          <span>{t('beatbox.net.topo.plugged_into')}</span><code className="bb-mono">{node.at}</code>
        </div>
        {where === null ? <p className="bb-net-note">{t('beatbox.net.topo.no_router_why')}</p> : null}
        {where && !where.served ? (() => {
          const { vlan, router } = where;
          const cidr = suggestRange(String(net.facts.cidr ?? ''), vlan, allRanges(net));
          return (
            <>
              <p className="bb-net-note">{t('beatbox.net.topo.no_network_why', { vlan })}</p>
              <div style={{ marginTop: 8 }}>
                <Button size="sm" variant="primary" disabled={busy || !cidr}
                        onClick={() => void run(() => giveNetwork(net, router, vlan, cidr))}>
                  {t('beatbox.net.topo.give_network', { vlan, cidr })}</Button>
              </div>
            </>
          );
        })() : null}
        <div className="bb-wf-label" style={{ marginTop: 16 }}>{t('beatbox.net.topo.its_port')}</div>
        <PortEditor net={net} port={node.at} />
        <div className="bb-wf-label" style={{ marginTop: 16 }}>{t('beatbox.net.cable.its')}</div>
        <CableEditor net={net} cable={node.node} />
        <p className="bb-faint" style={{ fontSize: 12, margin: '8px 0 0' }}>{framesOf(net, node.node)}</p>
        <CaptureControl net={net} cable={node.node} />
        <div className="bb-wf-label" style={{ marginTop: 16 }}>{t('beatbox.net.topo.move')}</div>
        <div className="bb-net-inline">
          <Select label={t('beatbox.net.topo.move')} value={node.at.split(':')[0]!} options={devices.map((value) => ({ value }))}
                  onChange={(device) => void run(() => act(network.id, 'node.place', { node: node.node, at: device }))} />
        </div>
        {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
      </Card>
    );
  }

  const name = selected.name;
  const isMain = !!net.routers.find((r) => r.name === name)?.main;
  const main = net.routers.find((r) => r.main)?.name ?? 'gw';
  const mine = (end: string) => end.startsWith(`${name}:`);
  const ports = [
    ...net.nodes.filter((n) => n.at && mine(n.at)).map((n) => ({ port: n.at!, what: net.label(n.node), cable: false })),
    ...topo.cables.flatMap(([a, b]) => (mine(a) ? [{ port: a, what: b, cable: true }] : mine(b) ? [{ port: b, what: a, cable: true }] : [])),
  ].sort((a, b) => Number(a.port.split(':')[1]) - Number(b.port.split(':')[1]));
  const unconnected = topo.devices.map((d) => d.name).filter((other) => other !== name
    && !topo.cables.some(([a, b]) => (mine(a) && b.startsWith(`${other}:`)) || (mine(b) && a.startsWith(`${other}:`))));
  const machinesHere = ports.filter((p) => !p.cable).length;
  const shortMode = (port: string) => {
    const mode = modeOf(topo, port);
    if (mode.mode === 'trunk') return <span className="bb-net-trunkword">{t('beatbox.net.topo.trunk_short')}</span>;
    return mode.vlan === 1 ? <code className="bb-mono bb-faint">1</code> : <VlanChip vlan={mode.vlan} />;
  };
  const portsTable = (
    <>
      {ports.length ? (
        <div className="bb-net-porttable">
          {ports.map((p) => {
            const isOpen = open === p.port;
            return (
              <Fragment key={p.port}>
                <button type="button" className={`bb-net-portline${isOpen ? ' on' : ''}`} onClick={() => setOpen(isOpen ? null : p.port)}>
                  <code className="bb-mono">{p.port.split(':')[1]}</code>
                  <span>{p.cable ? t('beatbox.net.topo.cable_to', { end: p.what }) : p.what}</span>
                  {shortMode(p.port)}
                </button>
                {isOpen ? (
                  <div className="bb-net-portopen">
                    <PortEditor net={net} port={p.port} />
                    {p.cable ? (
                      <>
                        <div className="bb-wf-label" style={{ marginTop: 12 }}>{t('beatbox.net.cable.title')}</div>
                        <CableEditor net={net} cable={p.port} />
                        <p className="bb-faint" style={{ fontSize: 12, margin: '8px 0 0' }}>{framesOf(net, p.port)}</p>
                        <CaptureControl net={net} cable={p.port} />
                        <button type="button" className="bb-net-link" disabled={busy}
                                onClick={() => void run(() => act(network.id, 'link.remove', { at: p.port }))}>{t('beatbox.net.topo.unplug_cable')}</button>
                      </>
                    ) : null}
                  </div>
                ) : null}
              </Fragment>
            );
          })}
        </div>
      ) : <p className="bb-net-say">{t('beatbox.net.topo.no_ports')}</p>}
      {unconnected.length ? (
        <>
          <div className="bb-wf-label" style={{ marginTop: 16 }}>{t('beatbox.net.topo.connect')}</div>
          <div className="bb-net-inline">
            <Select label={t('beatbox.net.topo.connect')} value={connectTo || unconnected[0]!} options={unconnected.map((value) => ({ value }))} onChange={setConnectTo} />
            <Button size="sm" disabled={busy} onClick={() => void run(() => act(network.id, 'link.add', { a: name, b: connectTo || unconnected[0]! }))}>
              {t('beatbox.net.topo.connect_go')}</Button>
          </div>
        </>
      ) : null}
    </>
  );
  return (
    <Card title={<><Icon name={selected.kind === 'router' ? 'router' : 'network'} size="sm" />
      <RenameTitle name={name} onRename={async (next) => { await act(network.id, 'device.rename', { device: name, name: next }); await net.refresh(); onSelect({ kind: selected.kind, name: next }); }} /></>} note={close}>
      <div className="bb-net-actions bb-net-first">
        {isMain
          ? <a className="bb-net-link" style={{ marginTop: 0 }} href={href('network/router')}>{t('beatbox.net.router.open')} →</a>
          : danger(t('beatbox.net.topo.remove'))}
        <small className="bb-faint">{isMain ? t(machinesHere === 1 ? 'beatbox.net.vlans.machines_one' : 'beatbox.net.vlans.machines', { count: machinesHere })
          : t('beatbox.net.topo.remove_note', { router: main })}</small>
      </div>
      {selected.kind === 'router' ? (
        <span className="bb-net-seg bb-net-tabs" role="tablist">
          {(['ports', 'networks', 'routes'] as const).map((each) => (
            <button key={each} type="button" role="tab" aria-selected={tab === each} className={tab === each ? 'on' : ''}
                    onClick={() => setTab(each)}>{t(`beatbox.net.topo.tab.${each}`)}</button>
          ))}
        </span>
      ) : null}
      {selected.kind !== 'router' || tab === 'ports' ? portsTable : null}
      {selected.kind === 'router' && tab === 'networks' ? <RouterNetworks net={net} router={name}
        blockedBy={machines.filter((machine) => cableOf(machine)?.sandbox === network.id && !off(machine)).map((machine) => machine.name)} /> : null}
      {selected.kind === 'router' && tab === 'routes' ? <Routes net={net} router={name} /> : null}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </Card>
  );
}

/** One light-terminal line, worded. */
function lightLine(line: Record<string, unknown>): string {
  const code = String(line.code ?? '');
  const v = Object.fromEntries(Object.entries(line).map(([k, x]) => [k, x === null || x === undefined ? '—' : String(x)]));
  switch (code) {
    case 'ipconfig': return `${t('beatbox.light.ipconfig', v)} · ${t(line.by === 'dhcp' ? 'beatbox.light.by.dhcp' : 'beatbox.light.by.fixed')}${line.address6 ? ` · ${t('beatbox.light.ipconfig6', { address: String(line.address6) })}` : ''}`;
    case 'curl.answer': return line.title ? t('beatbox.light.curl.answer', v) : t('beatbox.light.curl.untitled', v);
    case 'tcp.open': return line.said ? t('beatbox.light.tcp.said', v) : t('beatbox.light.tcp.open', v);
    case 'why.firewall': return line.rule ? t('beatbox.light.why.firewall_rule', v) : t('beatbox.light.why.firewall_default');
    case 'why.name': return t('beatbox.light.why.name', { name: v.name!, rule: v.rule!.replace(/^n/, '') });
    case 'why.limit': return t('beatbox.light.why.limit', { ...v, rule: v.rule!.replace(/^r/, ''), per: runtimeText(`beatbox.net.limit.per.${v.per}`) });
    default: return runtimeText(`beatbox.light.${code}`, v);
  }
}

/** A light machine's mini terminal. */
function LightTerminal({ net, name, onClose }: { net: Net; name: string; onClose: () => void }) {
  const [lines, setLines] = useState<{ said: string; kind: 'in' | 'out' | 'why' | 'wait'; way?: boolean }[]>([{ said: t('beatbox.light.help'), kind: 'out' }]);
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => end.current?.scrollIntoView({ block: 'end' }), [lines]);
  async function send() {
    const line = text.trim();
    if (!line || busy) return;
    setText('');
    setBusy(true);
    setLines((now) => [...now, { said: `${name}> ${line}`, kind: 'in' }]);
    // Fire and poll: the network stays responsive meanwhile.
    setLines((now) => [...now, { said: t('beatbox.light.running'), kind: 'wait' }]);
    const settle = (more: { said: string; kind: 'in' | 'out' | 'why' | 'wait'; way?: boolean }[]) =>
      setLines((now) => [...now.filter((l) => l.kind !== 'wait'), ...more]);
    try {
      const id = net.network!.id;
      const { job } = await act(id, 'light.start', { node: name, line });
      for (;;) {
        await new Promise((go) => window.setTimeout(go, 250));
        const done = await act(id, 'light.result', { job });
        if (done.done !== true) continue;
        settle(rows(done.lines).map((l) => ({
          said: lightLine(l), kind: String(l.code).startsWith('why.') ? 'why' as const : 'out' as const,
          way: l.code === 'why.firewall' || l.code === 'why.limit',
        })));
        break;
      }
    } catch (error) {
      settle([{ said: fault(error), kind: 'why' }]);
    }
    setBusy(false);
  }
  return (
    <Modal title={t('beatbox.light.terminal', { name })} icon="terminal" width={760} onClose={onClose}>
      <div className="bb-net-term" role="log" aria-live="polite">
        {lines.map((line, i) => (
          <div key={i} className={`bb-net-termline bb-net-termline--${line.kind}`}>{line.said}
            {line.way ? <> <a className="bb-net-termway" href={href('network/flows', { from: name })} onClick={onClose}>{t('beatbox.net.path.see')} →</a></> : null}
          </div>
        ))}
        <div ref={end} />
      </div>
      <form className="bb-net-terminput" onSubmit={(event) => { event.preventDefault(); void send(); }}>
        <span className="bb-mono">{name}&gt;</span>
        <input className="bb-mono" data-autofocus value={text} disabled={busy} placeholder="ping gateway"
               aria-label={t('beatbox.light.terminal', { name })} onChange={(event) => setText(event.target.value)} />
      </form>
    </Modal>
  );
}

/** Inspector for a light machine: kind, services, address, terminal. */
function LightInspector({ net, name, close, onRemove, onRenamed }: { net: Net; name: string; close: ReactNode; onRemove: () => void; onRenamed: (name: string) => void }) {
  const network = net.network!;
  const node = net.nodes.find((n) => n.node === name)!;
  const light = node.light!;
  const topo = topoOf(net);
  const [terminal, setTerminal] = useState(false);
  const [gateway, setGateway] = useState(light.gateway ?? '');
  const [dns, setDns] = useState(light.dns ?? '');
  const [told, setTold] = useState('');
  const [busy, setBusy] = useState(false);
  async function set(change: Record<string, unknown>) {
    setBusy(true);
    setTold('');
    try {
      await act(network.id, 'light.configure', { node: name, ...change });
      await net.refresh();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  const toggle = (key: 'ping' | 'web' | 'echo') => (
    <div className="bb-sbx-toggle" key={key}>
      <Switch id={`bb-light-${key}`} checked={light[key]} label={t(`beatbox.light.serve.${key}`)} onChange={(on) => { if (!busy) void set({ [key]: on }); }} />
      <label htmlFor={`bb-light-${key}`}><b>{t(`beatbox.light.serve.${key}`)}</b></label>
    </div>
  );
  return (
    <Card info={t(light.kind === 'server' ? 'beatbox.light.what_server' : 'beatbox.light.what_pc')} title={<><Icon name={light.kind === 'server' ? 'server' : 'terminal'} size="sm" />
      <RenameTitle name={name} onRename={async (next) => { await act(network.id, 'node.rename', { node: name, name: next }); await net.refresh(); onRenamed(next); }} /></>} note={close}>
      <div className="bb-net-actions bb-net-first">
        <Button size="sm" variant="primary" icon="terminal" disabled={!net.running} onClick={() => setTerminal(true)}>{t('beatbox.light.open')}</Button>
        <Button size="sm" onClick={onRemove}><span className="bb-net-danger"><Icon name="trash" size="xs" />{t('beatbox.net.topo.remove')}</span></Button>
      </div>
      {net.running ? null : <p className="bb-net-say">{t('beatbox.light.start_network')}</p>}
      <div className="bb-wf-label" style={{ marginTop: 16 }}>{t('beatbox.light.address')}</div>
      <span className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.light.address')}>
        {([true, false] as const).map((dhcp) => (
          <button key={String(dhcp)} type="button" role="radio" aria-checked={light.dhcp === dhcp} className={light.dhcp === dhcp ? 'on' : ''}
                  disabled={busy} onClick={() => void set({ dhcp })}>{t(dhcp ? 'beatbox.light.by.dhcp' : 'beatbox.light.by.fixed')}</button>
        ))}
      </span>
      <div className="bb-net-facts" style={{ marginTop: 12 }}>
        <span>{t('beatbox.net.topo.address')}</span>
        {light.dhcp ? <code className="bb-mono">{node.address}</code> : <AddressEdit net={net} node={name} address={node.address} />}
        <span>{t('beatbox.net.topo.plugged_into')}</span><code className="bb-mono">{node.at}</code>
      </div>
      {!light.dhcp ? (
        <div className="bb-net-porteditor" style={{ marginTop: 8 }}>
          <label className="bb-net-portfield bb-net-portfield--wide"><span>{t('beatbox.light.gateway')}</span>
            <input className="bb-input bb-mono" value={gateway} placeholder={t('beatbox.light.router_default')} onChange={(e) => setGateway(e.target.value)} /></label>
          <label className="bb-net-portfield bb-net-portfield--wide"><span>DNS</span>
            <input className="bb-input bb-mono" value={dns} placeholder={t('beatbox.light.router_default')} onChange={(e) => setDns(e.target.value)} /></label>
          <Button size="sm" variant="primary" disabled={busy} onClick={() => void set({ gateway, dns })}>{t('beatbox.net.port.apply')}</Button>
        </div>
      ) : null}
      <div className="bb-wf-label" style={{ marginTop: 16 }}>{t('beatbox.light.answers')}</div>
      <div className="bb-net-toggles">{(['ping', 'web', 'echo'] as const).map(toggle)}</div>
      <div className="bb-wf-label" style={{ marginTop: 16 }}>{t('beatbox.net.topo.its_port')}</div>
      <PortEditor net={net} port={node.at!} />
      <div className="bb-wf-label" style={{ marginTop: 16 }}>{t('beatbox.net.topo.move')}</div>
      <div className="bb-net-inline">
        <Select label={t('beatbox.net.topo.move')} value={node.at!.split(':')[0]!} options={topo.devices.map((d) => ({ value: d.name }))}
                onChange={(device) => void (async () => { await act(network.id, 'node.place', { node: name, at: device }); await net.refresh(); })()} />
      </div>
      <CaptureControl net={net} cable={name} />
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
      {terminal ? <LightTerminal net={net} name={name} onClose={() => setTerminal(false)} /> : null}
    </Card>
  );
}

/**
 * Add devices. New ones cable to the selection, else the main router. A new
 * router gets its own link and a default route back.
 */
function Palette({ net, machines, reload, selected, onSelect }: {
  net: Net; machines: SandboxView[]; reload: () => Promise<unknown>; selected: Selection; onSelect: (s: Selection) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  const topo = topoOf(net);
  const id = net.network!.id;
  const main = net.routers.find((r) => r.main)?.name ?? topo.devices.find((d) => d.kind === 'router')?.name ?? 'gw';
  const target = !selected ? main
    : selected.kind === 'machine' ? (net.nodes.find((n) => n.node === selected.name)?.at ?? main).split(':')[0]!
    : selected.name;
  const targetRouter = net.routers.some((r) => r.name === target) ? target : main;
  const others = machines.filter((machine) => cableOf(machine)?.sandbox !== id);
  async function run(work: () => Promise<unknown>) {
    setBusy(true);
    setTold('');
    try {
      await work();
      await Promise.all([net.refresh(), reload()]);
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  async function add(kind: 'switch' | 'router') {
    const prefix = kind === 'switch' ? 'sw' : 'r';
    let n = kind === 'switch' ? 1 : 2;
    while (topo.devices.some((d) => d.name === `${prefix}${n}`)) n += 1;
    const name = `${prefix}${n}`;
    await act(id, 'device.add', { kind, name });
    const link = await act(id, 'link.add', { a: kind === 'router' ? targetRouter : target, b: name });
    if (kind === 'router') {
      // Own VLAN and small network to the parent router, default route via it.
      const used = new Set([...vlansIn(topo), ...net.routers.flatMap((r) => r.networks.map((x) => x.vlan))]);
      let vlan = 100;
      while (used.has(vlan)) vlan += 1;
      const cidr = suggestRange(String(net.facts.cidr ?? ''), vlan, allRanges(net));
      const [first, second] = hostsOf(cidr);
      for (const port of [String(link.a), String(link.b)]) await act(id, 'port.configure', { port, mode: 'access', vlan });
      await act(id, 'interface.configure', { router: targetRouter, vlan, cidr, address: first });
      await act(id, 'interface.configure', { router: name, vlan, cidr, address: second });
      await act(id, 'route.add', { router: name, to: '0.0.0.0/0', via: first });
    }
    onSelect({ kind, name });
  }
  async function addLight(kind: 'pc' | 'server') {
    const prefix = kind === 'pc' ? 'pc' : 'srv';
    let n = 1;
    const taken = (name: string) => net.nodes.some((x) => x.node === name) || topo.devices.some((d) => d.name === name);
    while (taken(`${prefix}${n}`)) n += 1;
    const name = `${prefix}${n}`;
    await act(id, 'light.add', { node: name, kind, at: target });
    onSelect({ kind: 'machine', name });
  }
  const item = (icon: IconName, title: string, what: string, onClick: (() => void) | null, soon = false) => (
    <button type="button" className="bb-net-paletteitem" disabled={busy || !onClick} onClick={onClick ?? undefined}>
      <Icon name={icon} size="sm" />
      <span><b>{title}{soon ? <em className="bb-net-soon">{t('beatbox.net.palette.soon')}</em> : null}</b><small>{what}</small></span>
    </button>
  );
  const vlans = vlansIn(topo);
  return (
    <aside className="bb-net-palette" aria-label={t('beatbox.net.palette.add')}>
      <div className="bb-wf-label">{t('beatbox.net.palette.add')}</div>
      {item('router', t('beatbox.net.router.title'), t('beatbox.net.palette.router_what'), () => void run(() => add('router')))}
      {item('network', t('beatbox.net.switch.title'), t('beatbox.net.palette.switch_what'), () => void run(() => add('switch')))}
      {item('terminal', t('beatbox.net.palette.light'), t('beatbox.net.palette.light_what'), () => void run(() => addLight('pc')))}
      {item('server', t('beatbox.net.palette.server'), t('beatbox.net.palette.server_what'), () => void run(() => addLight('server')))}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button type="button" className="bb-net-paletteitem" disabled={busy || !others.length}>
            <Icon name="box" size="sm" />
            <span><b>{t('beatbox.net.palette.vm')}</b><small>{others.length ? t('beatbox.net.palette.vm_what') : t('beatbox.net.palette.no_vms')}</small></span>
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" sideOffset={6} className="w-72 rounded-lg">
          <DropdownMenuLabel className="text-muted-foreground text-xs font-normal">{t('beatbox.net.topo.plug_into', { device: target })}</DropdownMenuLabel>
          {others.map((machine) => (
            <DropdownMenuItem key={machine.id} className="gap-2.5 p-2" disabled={!off(machine)}
                              onSelect={() => void run(() => plug(machine, id, target))}>
              <span className="bg-accent flex size-7 shrink-0 items-center justify-center rounded-lg"><Icon name="box" size="sm" /></span>
              <span className="flex min-w-0 flex-1 flex-col">
                <span className="truncate text-[13.5px] font-medium">{machine.name}</span>
                <span className="text-muted-foreground text-xs">{!off(machine) ? t('beatbox.net.nodes.stop_first_short') : runtimeText(`beatbox.plain.state.${machine.state.lifecycle}`)}</span>
              </span>
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
      <p className="bb-net-say">{t('beatbox.net.palette.target', { device: target })}</p>
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12, margin: 0 }}>{told}</p> : null}
      {vlans.length ? (
        <>
          <div className="bb-wf-label" style={{ marginTop: 8 }}>{t('beatbox.net.palette.vlans')}</div>
          {vlans.map((vlan) => <VlanName key={vlan} net={net} vlan={vlan} />)}
        </>
      ) : null}
    </aside>
  );
}

/** Every range any router has a network in. */
function allRanges(net: Net): string[] {
  return net.routers.flatMap((r) => r.networks.map((n) => n.cidr));
}

/** 10.50.100.0/24 -> 10.50.100.1 and .2. */
function hostsOf(cidr: string): [string, string] {
  const [a, b, c, d] = cidr.split('/')[0]!.split('.').map(Number);
  return [`${a}.${b}.${c}.${d! + 1}`, `${a}.${b}.${c}.${d! + 2}`];
}

/** Give `router` a network in `vlan`. For a non-main router, also add the main router's route to it. */
async function giveNetwork(net: Net, router: string, vlan: number, cidr: string, address?: string): Promise<Record<string, unknown>> {
  const id = net.network!.id;
  const done = await act(id, 'interface.configure', { router, vlan, cidr, ...(address ? { address } : {}) });
  const main = net.routers.find((r) => r.main);
  const mine = net.routers.find((r) => r.name === router);
  if (main && mine && !mine.main) {
    const shared = mine.networks.find((n) => main.networks.some((m) => m.cidr === n.cidr));
    if (shared) await act(id, 'route.add', { router: main.name, to: cidr, via: shared.address });
  }
  return done;
}

/** A router's networks by VLAN. All removable except the main range. */
function RouterNetworks({ net, router, table = false, blockedBy = [] }: { net: Net; router: string; table?: boolean; blockedBy?: string[] }) {
  const network = net.network!;
  const info = net.routers.find((r) => r.name === router);
  const networks = info?.networks ?? [];
  const main = String(net.facts.cidr ?? '');
  const waiting = [1, ...vlansIn(topoOf(net))].filter((v) => !networks.some((n) => n.vlan === v));
  const nextVlan = waiting[0] ?? Array.from({ length: 4093 }, (_, i) => i + 2).find((v) => !networks.some((n) => n.vlan === v));
  const suggestion = nextVlan ? suggestRange(main, nextVlan, allRanges(net)) : '';
  const [adding, setAdding] = useState(!table);
  const [changingRange, setChangingRange] = useState(false);
  const [vlan, setVlan] = useState(String(waiting[0] ?? ''));
  const [cidr, setCidr] = useState('');
  const [told, setTold] = useState('');
  const [moved, setMoved] = useState<{ node: string; from: string; to: string }[]>([]);
  const [busy, setBusy] = useState(false);
  const number = Number(vlan);
  const offer = Number.isInteger(number) && number >= 1 && number < 4095 ? suggestRange(main, number, allRanges(net)) : '';
  const machinesIn = (side: RouterInfo['networks'][number]) => net.nodes.filter((node) => {
    if (!fits(side.cidr, node.address, { nodes: [], gateway: '' })) return false;
    if (!node.at) return true;
    const serving = servedBy(topoOf(net), net.routers, node.at);
    return serving?.served && serving.router === router && serving.vlan === side.vlan;
  }).length;
  const block = rangeBlock(net, blockedBy);
  async function run(work: () => Promise<Record<string, unknown>>) {
    setBusy(true);
    setTold('');
    setMoved([]);
    try {
      const done = await work();
      setMoved(rows(done.moved) as unknown as { node: string; from: string; to: string }[]);
      await net.refresh();
      if (table) setAdding(false);
      setVlan('');
      setCidr('');
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  return (
    <>
      {table ? (
        <div className="bb-router-table-scroll">
          <table className="bb-router-table bb-router-networks">
            <thead><tr><th scope="col">VLAN</th><th scope="col">{t('beatbox.net.vlans.range')}</th>
              <th scope="col">{t('beatbox.net.router.design.address')}</th><th scope="col">IPv6</th><th scope="col">{t('beatbox.net.router.design.machines')}</th>
              <th scope="col">{t('beatbox.net.nat.shows')}</th>
              <th scope="col"><span className="sr-only">{t('beatbox.net.router.design.actions')}</span></th></tr></thead>
            <tbody>{networks.map((n) => {
              const isMain = !!info?.main && n.vlan === 1;
              const count = machinesIn(n);
              return <tr key={n.vlan}>
                <td><VlanChip vlan={n.vlan} main={isMain} /></td>
                <td><code className="bb-mono">{n.cidr}</code>{n.ipv6 && n.prefix6 ? <code className="bb-mono bb-ipv6">{n.prefix6}</code> : null}</td>
                <td className="bb-muted"><code className="bb-mono">{n.address}</code>{n.ipv6 && n.address6 ? <code className="bb-mono bb-ipv6">{n.address6}</code> : null}</td>
                <td><Ipv6Toggle net={net} router={router} side={n} /></td>
                <td className="bb-muted">{t(count === 1 ? 'beatbox.net.vlans.machines_one' : 'beatbox.net.vlans.machines', { count })}</td>
                <td><HideToggle net={net} router={router} side={n} /></td>
                <td>{isMain ? <button type="button" className="bb-router-row-action" disabled={!!block} title={block || undefined}
                  onClick={() => setChangingRange(true)}>{t('beatbox.net.range.change')}</button>
                  : <button type="button" className="bb-router-row-action bb-net-danger" disabled={busy}
                    aria-label={t('beatbox.net.router.design.remove_network', { vlan: n.vlan, router })}
                    onClick={() => void run(() => act(network.id, 'interface.remove', { router, vlan: n.vlan }))}>{t('beatbox.net.vlans.remove')}</button>}</td>
              </tr>;
            })}</tbody>
          </table>
        </div>
      ) : (
      <div className="bb-net-vlanlist">
        {networks.map((n) => {
          const isMain = info?.main && n.vlan === 1;
          const count = machinesIn(n);
          return (
            <div key={n.vlan} className="bb-net-vlanrow">
              <VlanChip vlan={n.vlan} /><code className="bb-mono">{n.cidr}</code>
              <span className="bb-faint">{isMain ? t('beatbox.net.vlans.main') : t('beatbox.net.vlans.gateway', { gateway: n.address })} · {t(count === 1 ? 'beatbox.net.vlans.machines_one' : 'beatbox.net.vlans.machines', { count })}</span>
              {isMain ? <button type="button" className="bb-net-link" style={{ marginTop: 0 }} disabled={!!block} title={block || undefined}
                onClick={() => setChangingRange(true)}>{t('beatbox.net.range.change')}</button> : (
                <button type="button" className="bb-net-link" style={{ marginTop: 0 }} disabled={busy}
                        onClick={() => void run(() => act(network.id, 'interface.remove', { router, vlan: n.vlan }))}>{t('beatbox.net.vlans.remove')}</button>
              )}
            </div>
          );
        })}
      </div>
      )}
      {table && !adding ? <button type="button" className="bb-router-add" disabled={busy} onClick={() => {
        setVlan(String(nextVlan ?? '')); setAdding(true);
      }}><Icon name="plus" size="sm" />{t('beatbox.net.router.design.add_network')}
        {suggestion ? <span> · {t('beatbox.net.router.design.suggestion', { cidr: suggestion })}</span> : null}</button> : null}
      {adding ? <form className="bb-net-vlanadd" onSubmit={(event) => { event.preventDefault(); if (!busy && vlan.trim() && (cidr.trim() || offer)) void run(() => giveNetwork(net, router, number, cidr.trim() || offer)); }}>
        <label className="bb-net-portfield"><span>VLAN</span>
          <input className="bb-input bb-mono" disabled={busy} inputMode="numeric" value={vlan} placeholder="10" onChange={(e) => setVlan(e.target.value)} /></label>
        <label className="bb-net-portfield bb-net-portfield--wide"><span>{t('beatbox.net.vlans.range')}</span>
          <input className="bb-input bb-mono" disabled={busy} value={cidr} placeholder={offer} onChange={(e) => setCidr(e.target.value)} /></label>
        <Button size="sm" variant="primary" type="submit" disabled={busy || !vlan.trim() || !(cidr.trim() || offer)}>
          {t('beatbox.net.vlans.add')}</Button>
        {table ? <Button size="sm" disabled={busy} onClick={() => { setAdding(false); setTold(''); }}>{t('beatbox.files.cancel')}</Button> : null}
      </form> : null}
      {changingRange ? <Modal title={t('beatbox.net.range.change')} icon="refresh" onClose={() => setChangingRange(false)}>
        <div className="bb-router-range-modal"><Range net={net} blockedBy={blockedBy} initialOpen onClose={() => setChangingRange(false)} /></div>
      </Modal> : null}
      {moved.length ? (
        <div className="bb-net-say" style={{ marginTop: 12 }}>
          {moved.map((m) => <p key={m.node} style={{ margin: '0 0 4px' }}>{t('beatbox.net.vlans.moved', { node: net.label(m.node), from: m.from, to: m.to })}</p>)}
        </div>
      ) : null}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </>
  );
}

/** NAT switch for one network, or why not (the link to another router can't). */
function HideToggle({ net, router, side }: { net: Net; router: string; side: RouterInfo['networks'][number] }) {
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  const hidden = !!net.routers.find((r) => r.name === router)?.hidden?.includes(side.vlan);
  const other = net.routers.find((r) => r.name !== router && r.networks.some((n) => n.cidr === side.cidr));
  if (other) {
    return <span className="bb-nat-cell"><b>{t('beatbox.net.nat.as_is')}</b><small>{t('beatbox.net.nat.shared', { other: other.name })}</small></span>;
  }
  async function turn(on: boolean) {
    setBusy(true);
    setTold('');
    try {
      await act(net.network!.id, 'nat.hide', { router, vlan: side.vlan, on });
      await net.refresh();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  return (
    <span className="bb-nat-cell bb-nat-cell--switch" title={t(hidden ? 'beatbox.net.nat.hidden_what' : 'beatbox.net.nat.shown_what', { router })}>
      <Switch checked={hidden} label={t('beatbox.net.nat.hidden')} onChange={(on) => { if (!busy) void turn(on); }} />
      <span><b>{t(hidden ? 'beatbox.net.nat.hidden' : 'beatbox.net.nat.shown')}</b>
        {told ? <small className="bb-sbx-no">{told}</small> : null}</span>
    </span>
  );
}

/** IPv6 on/off. On, the router advertises its /64 and machines pick their own address (SLAAC). */
function Ipv6Toggle({ net, router, side }: { net: Net; router: string; side: RouterInfo['networks'][number] }) {
  const { busy, told, run } = useFirewallAct(net);
  return (
    <span className="bb-ipv6-cell">
      <Switch checked={!!side.ipv6} label={t('beatbox.net.ipv6.on', { vlan: side.vlan })}
              onChange={(on) => { if (!busy) void run('ipv6.set', { router, vlan: side.vlan, on }); }} />
      {told ? <small className="bb-sbx-no">{told}</small> : null}
    </span>
  );
}

/** Port forwards. */
function RouterForwards({ net, router }: { net: Net; router: string }) {
  const { busy, told, run } = useFirewallAct(net);
  const forwards = net.routers.find((r) => r.name === router)?.forwards ?? [];
  const [protocol, setProtocol] = useState('tcp');
  const [port, setPort] = useState('');
  const [to, setTo] = useState(net.nodes[0]?.node ?? '');
  const [toPort, setToPort] = useState('');
  async function add() {
    if (await run('nat.add', { router, protocol, port: Number(port), to, to_port: Number(toPort) })) {
      setPort('');
      setToPort('');
    }
  }
  return (
    <>
      {forwards.length ? (
        <div className="bb-nat-list bb-net-first">
          {forwards.map((f) => (
            <div key={f.id} className="bb-nat-row">
              <span><Icon name="router" size="xs" /><b>{router}</b> {t('beatbox.net.nat.port', { port: f.port })} <code className="bb-mono bb-faint">{f.protocol}</code></span>
              <span><span className="bb-faint">→</span> <Icon name={machineIcon(net, f.to)} size="xs" /><b>{net.label(f.to)}</b> {t('beatbox.net.nat.port', { port: f.to_port })}</span>
              <small className={f.uses ? '' : 'bb-faint'}>{f.uses ? t('beatbox.net.nat.uses', { count: f.uses, when: since(f.quiet_ms ?? 0) }) : t('beatbox.net.nat.unused')}</small>
              <button type="button" className="bb-net-unplug" disabled={busy} aria-label={t('beatbox.net.nat.remove', { port: f.port })}
                      onClick={() => void run('nat.remove', { router, id: f.id })}><Icon name="close" size="xs" /></button>
            </div>
          ))}
        </div>
      ) : null}
      <form className="bb-nat-add" onSubmit={(event) => { event.preventDefault(); if (!busy && port && to && toPort) void add(); }}>
        <span>{t('beatbox.net.nat.when')}</span>
        <Select label={t('beatbox.net.rule.protocol')} value={protocol} onChange={setProtocol} tiny options={['tcp', 'udp'].map((value) => ({ value }))} />
        <span>{t('beatbox.net.nat.arrives')}</span>
        <input className="bb-input bb-mono bb-nat-port" inputMode="numeric" value={port} placeholder="8080"
               aria-label={t('beatbox.net.nat.router_port')} onChange={(e) => setPort(e.target.value)} />
        <span>{t('beatbox.net.nat.send_to')}</span>
        <Select label={t('beatbox.net.nat.machine')} value={to} onChange={setTo} options={net.nodes.map((n) => ({ value: n.node, label: net.label(n.node) }))} />
        <input className="bb-input bb-mono bb-nat-port" inputMode="numeric" value={toPort} placeholder="80"
               aria-label={t('beatbox.net.nat.machine_port')} onChange={(e) => setToPort(e.target.value)} />
        <Button size="sm" variant="primary" type="submit" disabled={busy || !port || !to || !toPort}>{t('beatbox.net.nat.add')}</Button>
      </form>
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </>
  );
}

/** Routes for traffic outside the router's own networks. */
function Routes({ net, router, table = false }: { net: Net; router: string; table?: boolean }) {
  const network = net.network!;
  const routes = net.routers.find((r) => r.name === router)?.routes ?? [];
  const info = net.routers.find((r) => r.name === router);
  const [adding, setAdding] = useState(!table);
  const [to, setTo] = useState('');
  const [via, setVia] = useState('');
  const [told, setTold] = useState('');
  const [busy, setBusy] = useState(false);
  async function run(work: () => Promise<unknown>, clear = false) {
    setBusy(true);
    setTold('');
    try {
      await work();
      await net.refresh();
      if (clear) { setTo(''); setVia(''); if (table) setAdding(false); }
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  return (
    <>
      {table ? <div className="bb-router-table-scroll"><table className="bb-router-table bb-router-routes">
        <thead><tr><th scope="col">{t('beatbox.net.router.design.destination')}</th><th scope="col">{t('beatbox.net.router.design.via')}</th>
          <th scope="col">{t('beatbox.net.router.design.origin')}</th><th scope="col"><span className="sr-only">{t('beatbox.net.router.design.actions')}</span></th></tr></thead>
        <tbody>
          {info?.networks.map((side) => <tr key={`connected-${side.vlan}`} className="bb-muted">
            <td><code className="bb-mono">{side.cidr}</code></td><td><code className="bb-mono">{router}</code></td>
            <td>{t('beatbox.net.router.design.connected')}</td><td /></tr>)}
          {routes.map((route) => {
            const next = net.routers.find((r) => r.networks.some((n) => n.address === route.via));
            return <tr key={route.to}>
              <td><code className="bb-mono">{route.to === '0.0.0.0/0' ? t('beatbox.net.routes.everything') : route.to}</code></td>
              <td className="bb-muted"><code className="bb-mono">{route.via}{next ? ` · ${next.name}` : ''}</code></td>
              <td>{t('beatbox.net.router.design.static')}</td>
              <td><button type="button" className="bb-router-row-action bb-net-danger" disabled={busy}
                aria-label={t('beatbox.net.router.design.remove_route', { to: route.to, router })}
                onClick={() => void run(() => act(network.id, 'route.remove', { router, to: route.to }))}>{t('beatbox.net.vlans.remove')}</button></td>
            </tr>;
          })}
          {info?.main && net.internet && !routes.some((r) => r.to === '0.0.0.0/0') ? <tr className="bb-muted">
            <td><code className="bb-mono">{t('beatbox.net.routes.everything')}</code></td><td>{t('beatbox.net.router.design.internet')}</td>
            <td>{t('beatbox.net.router.design.default')}</td><td /></tr> : null}
        </tbody>
      </table></div> : (<>
      {routes.length ? (
        <div className="bb-net-vlanlist">
          {routes.map((r) => (
            <div key={r.to} className="bb-net-routerow">
              <code className="bb-mono">{r.to === '0.0.0.0/0' ? t('beatbox.net.routes.everything') : r.to}</code>
              <span className="bb-faint">{t('beatbox.net.routes.through', { via: r.via })}</span>
              <button type="button" className="bb-net-link" style={{ marginTop: 0 }} disabled={busy}
                      onClick={() => void run(() => act(network.id, 'route.remove', { router, to: r.to }))}>{t('beatbox.net.vlans.remove')}</button>
            </div>
          ))}
        </div>
      ) : <p className="bb-net-say">{t('beatbox.net.routes.none')}</p>}
      </>)}
      {table && !adding ? <button type="button" className="bb-router-add" disabled={busy} onClick={() => setAdding(true)}><Icon name="plus" size="sm" />{t('beatbox.net.router.design.add_route')}</button> : null}
      {adding ? <form className="bb-net-vlanadd" onSubmit={(event) => { event.preventDefault(); if (!busy && to.trim() && via.trim()) void run(() => act(network.id, 'route.add', { router, to: to.trim(), via: via.trim() }), true); }}>
        <label className="bb-net-portfield bb-net-portfield--wide"><span>{t('beatbox.net.routes.to')}</span>
          <input className="bb-input bb-mono" disabled={busy} value={to} placeholder="10.20.0.0/24" onChange={(e) => setTo(e.target.value)} /></label>
        <label className="bb-net-portfield bb-net-portfield--wide"><span>{t('beatbox.net.routes.via')}</span>
          <input className="bb-input bb-mono" disabled={busy} value={via} placeholder="10.99.0.2" onChange={(e) => setVia(e.target.value)} /></label>
        <Button size="sm" variant="primary" type="submit" disabled={busy || !to.trim() || !via.trim()}>{t('beatbox.net.routes.add')}</Button>
        {table ? <Button size="sm" disabled={busy} onClick={() => { setAdding(false); setTold(''); }}>{t('beatbox.files.cancel')}</Button> : null}
      </form> : null}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </>
  );
}

/** An IPv4 address as a number. */
const ipNumber = (address: string) => address.split('.').reduce((sum, part) => sum * 256 + Number(part), 0);

/** Record types we let people add. */
const DNS_TYPES = ['A', 'AAAA', 'CNAME', 'MX', 'TXT'] as const;
type DnsType = typeof DNS_TYPES[number];

/** DNS: machine names (folded), Server names (edited on the Server), and custom records. */
function Dns({ net }: { net: Net }) {
  const network = net.network!;
  const domain = net.domain;
  const [kind, setKind] = useState<DnsType>('A');
  const [name, setName] = useState('');
  const [value, setValue] = useState('');
  const [priority, setPriority] = useState('10');
  const [domainText, setDomainText] = useState(domain);
  const [told, setTold] = useState('');
  const [busy, setBusy] = useState(false);
  useEffect(() => setDomainText(domain), [domain]);

  async function run(work: () => Promise<unknown>) {
    setBusy(true);
    setTold('');
    try {
      await work();
      await net.refresh();
      setBusy(false);
      return true;
    } catch (error) {
      setTold(fault(error));
      setBusy(false);
      return false;
    }
  }

  /** `www` -> `www.<domain>`; dotted names stay as typed. */
  const full = (text: string) => {
    const name = text.trim().toLowerCase().replace(/\.$/, '');
    return name.includes('.') || name === domain ? name : `${name}.${domain}`;
  };
  const add = () => run(() => kind === 'A'
    ? act(network.id, 'dns.add', { name: full(name), address: value.trim() })
    : act(network.id, 'dns.record', { name: full(name), type: kind, value: value.trim(), ...(kind === 'MX' ? { priority: Number(priority) || 10 } : {}) }),
  ).then((ok) => { if (ok) { setName(''); setValue(''); } });

  type Row = { key: string; name: string; type: string; value: string; from: 'machine' | 'server' | 'mine'; who?: string; remove?: () => Promise<unknown> };
  const machines: Row[] = net.nodes.filter((node) => !net.serverOf(node.node)).map((node) => ({
    key: `m:${node.node}`, name: `${node.node}.${domain}`, type: 'A', value: node.address, from: 'machine', who: node.node,
  }));
  const rowsShown: Row[] = [
    ...Object.entries(bag(net.facts.dns)).map(([entry, address]): Row => {
      const from = net.dnsFrom[entry];
      return {
        key: `a:${entry}`, name: entry, type: 'A', value: String(address), from: from ? 'server' : 'mine', who: from,
        remove: from ? undefined : () => act(network.id, 'dns.remove', { name: entry }),
      };
    }),
    ...net.records.map((record): Row => ({
      key: `r:${record.name}:${record.type}:${record.value}`, name: record.name, type: record.type,
      value: record.type === 'MX' ? `${record.priority ?? 10} ${record.value}` : record.type === 'TXT' ? `"${record.value}"` : record.value,
      from: 'mine', remove: () => act(network.id, 'dns.record_remove', { name: record.name, type: record.type, value: record.value }),
    })),
  ];
  const pill = (row: Row) => row.from === 'machine' ? <span className="bb-net-vlan">{t('beatbox.net.dns.from_machine', { name: net.label(row.who!) })}</span>
    : row.from === 'server' ? <span className="bb-net-vlan bb-dns-server"><Icon name="lock" size="xs" />{t('beatbox.net.dns.from_server', { name: net.label(row.who!) })}</span>
    : <span className="bb-net-vlan">{t('beatbox.net.dns.from_mine')}</span>;
  const table = (shown: Row[]) => (
    <div className="bb-router-table-scroll">
      <table className="bb-router-table bb-dns-table">
        <thead><tr><th scope="col">{t('beatbox.net.dns.name')}</th><th scope="col">{t('beatbox.net.dns.type')}</th>
          <th scope="col">{t('beatbox.net.dns.answer')}</th><th scope="col">{t('beatbox.net.dns.from')}</th>
          <th scope="col"><span className="sr-only">{t('beatbox.net.router.design.actions')}</span></th></tr></thead>
        <tbody>{shown.map((row) => (
          <tr key={row.key}>
            <td><code className="bb-mono">{row.name}</code></td>
            <td className="bb-muted"><code className="bb-mono">{row.type}</code></td>
            <td><code className="bb-mono">{row.value}</code></td>
            <td>{pill(row)}</td>
            <td>{row.from === 'server' ? <button type="button" className="bb-router-row-action"
              onClick={() => { remember('server', row.who!); navigate('server/services'); }}>{t('beatbox.net.dns.open_server')}</button>
              : row.remove ? <button type="button" className="bb-router-row-action bb-net-danger" disabled={busy}
                aria-label={t('beatbox.net.dns.remove', { name: row.name, type: row.type })}
                onClick={() => void run(row.remove!)}>{t('beatbox.net.dns.remove_short')}</button> : null}</td>
          </tr>
        ))}</tbody>
      </table>
    </div>
  );

  return (
    <Card title={<><Icon name="globe" size="sm" />{t('beatbox.net.dns.title')}</>}
          info={t(net.internet ? 'beatbox.net.dns.unknown_internet' : 'beatbox.net.dns.unknown_none')}
          note={<form className="bb-dns-domainform" onSubmit={(event) => {
            event.preventDefault();
            if (!busy && domainText.trim() && domainText.trim() !== domain) void run(() => act(network.id, 'dns.domain', { domain: domainText.trim() }));
          }}>
            <span>{t('beatbox.net.dns.domain')}</span>
            <input className="bb-input bb-mono bb-dns-domain" disabled={busy} value={domainText} aria-label={t('beatbox.net.dns.domain')}
                   onChange={(e) => setDomainText(e.target.value)} />
            {domainText.trim() !== domain ? <Button size="sm" type="submit" disabled={busy || !domainText.trim()}>{t('beatbox.net.dns.domain_set')}</Button> : null}
          </form>}>
      {rowsShown.length ? table(rowsShown) : <p className="bb-net-say">{t('beatbox.net.dns.none')}</p>}
      <form className="bb-nat-add bb-dns-add" onSubmit={(event) => { event.preventDefault(); if (!busy && name.trim() && value.trim()) void add(); }}>
        <Select label={t('beatbox.net.dns.type')} value={kind} tiny options={DNS_TYPES.map((value) => ({ value }))} onChange={(next) => setKind(next as DnsType)} />
        <input className="bb-input bb-mono bb-dns-field" disabled={busy} value={name} aria-label={t('beatbox.net.dns.name')}
               placeholder={kind === 'MX' || kind === 'TXT' ? domain : `www.${domain}`} onChange={(e) => setName(e.target.value)} />
        <span>{t(`beatbox.net.dns.says.${kind.toLowerCase() as Lowercase<DnsType>}`)}</span>
        {kind === 'MX' ? <input className="bb-input bb-mono bb-nat-port" disabled={busy} inputMode="numeric" value={priority}
                                aria-label={t('beatbox.net.dns.priority')} title={t('beatbox.net.dns.priority')} onChange={(e) => setPriority(e.target.value)} /> : null}
        <input className="bb-input bb-mono bb-dns-field" disabled={busy} value={value} aria-label={t('beatbox.net.dns.answer')} onChange={(e) => setValue(e.target.value)}
               placeholder={{ A: '10.0.0.20', AAAA: 'fd00::20', CNAME: `intranet.${domain}`, MX: `mail.${domain}`, TXT: 'v=spf1 mx -all' }[kind]} />
        <Button size="sm" variant="primary" type="submit" disabled={busy || !name.trim() || !value.trim()}>{t('beatbox.net.dns.add')}</Button>
      </form>
      {machines.length ? <details className="bb-fold">
        <summary>
          <Icon name="arrow" size="xs" />
          <span><b>{t('beatbox.net.dns.machines')}</b> <span className="bb-muted">· {machines.length}</span>
            <small className="bb-mono">{machines.slice(0, 4).map((row) => row.name).join(', ')}{machines.length > 4 ? '…' : ''}</small></span>
          <span className="bb-fold-see">{t('beatbox.net.fold.see')}</span><span className="bb-fold-hide">{t('beatbox.net.fold.hide')}</span>
        </summary>
        {table(machines)}
      </details> : null}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </Card>
  );
}

/** Lease length choices, in seconds. */
const LEASES = [600, 1800, 3600, 28800, 86400];
const leaseWords = (seconds: number) => seconds % 3600 === 0 ? t('beatbox.net.dhcp.hours', { count: seconds / 3600 })
  : t('beatbox.net.dhcp.minutes', { count: Math.round(seconds / 60) });

/** 10.20.0.10 in 10.20.0.0/24 -> ".10". */
const tail = (address: string, cidr: string) => {
  const mine = address.split('.');
  const base = cidr.split('/')[0]!.split('.');
  const same = mine.findIndex((part, i) => part !== base[i]);
  return same <= 0 ? address : `.${mine.slice(same).join('.')}`;
};

const DHCP_DEFAULT: Dhcp = { on: true, first: 10, last: null, lease: 600, dns: null };

/** DHCP per router network, plus leases (folded): reserve one or release it. */
function DhcpCard({ net }: { net: Net }) {
  const network = net.network!;
  const sides = net.routers.flatMap((router) => router.networks.map((side) => ({ router, side })));
  const many = net.routers.length > 1;
  const [editing, setEditing] = useState('');
  const [first, setFirst] = useState('');
  const [last, setLast] = useState('');
  const [lease, setLease] = useState('600');
  const [dns, setDns] = useState('');
  const [told, setTold] = useState('');
  const [busy, setBusy] = useState(false);

  async function run(work: () => Promise<unknown>) {
    setBusy(true);
    setTold('');
    try {
      await work();
      await net.refresh();
      setBusy(false);
      return true;
    } catch (error) {
      setTold(fault(error));
      setBusy(false);
      return false;
    }
  }
  const keyOf = (router: string, vlan: number) => `${router}:${vlan}`;
  const configure = (router: string, vlan: number, settings: Record<string, unknown>) =>
    run(() => act(network.id, 'dhcp.configure', { router, vlan, ...settings }));
  function open(router: string, side: RouterInfo['networks'][number]) {
    setEditing(keyOf(router, side.vlan));
    setFirst(side.pool?.[0] ?? '');
    setLast(side.pool?.[1] ?? '');
    setLease(String(side.dhcp?.lease ?? 600));
    setDns(side.dhcp?.dns ?? '');
  }
  const open_ = sides.find(({ router, side }) => keyOf(router.name, side.vlan) === editing);
  const sideOf = (node: Node) => {
    const serving = node.at ? servedBy(topoOf(net), net.routers, node.at) : null;
    return sides.find(({ router, side }) => fits(side.cidr, node.address, { nodes: [], gateway: '' })
      && (!serving || (serving.served && serving.router === router.name && serving.vlan === side.vlan)))?.side;
  };
  const held = net.nodes.map((node) => ({ node, side: sideOf(node) })).filter((each) => each.side);
  const reserved = held.filter(({ node }) => node.reserved && !net.serverOf(node.node)).length;
  const servers = held.filter(({ node }) => net.serverOf(node.node)).length;
  // Engines before #52 don't report DHCP; say so instead of empty columns.
  const old = sides.length > 0 && sides.every(({ side }) => !side.dhcp);

  return (
    <Card title={<><Icon name="tag" size="sm" />{t('beatbox.net.dhcp.title')}</>}
          info={sides.every(({ side }) => !side.dhcp?.dns) ? t('beatbox.net.dhcp.all_router') : undefined}>
      {old ? <p className="bb-net-say">{t('beatbox.net.dhcp.old_engine')}</p> : <>
      <div className="bb-router-table-scroll">
        <table className="bb-router-table bb-dhcp-table">
          <thead><tr><th scope="col">{t('beatbox.net.dhcp.network')}</th><th scope="col">{t('beatbox.net.dhcp.pool')}</th>
            <th scope="col">{t('beatbox.net.dhcp.lease')}</th><th scope="col">{t('beatbox.net.dhcp.on')}</th>
            <th scope="col"><span className="sr-only">{t('beatbox.net.router.design.actions')}</span></th></tr></thead>
          <tbody>{sides.map(({ router, side: n }) => {
            const dhcp = n.dhcp ?? DHCP_DEFAULT;
            return <tr key={keyOf(router.name, n.vlan)}>
              <td>{many ? <span className="bb-muted bb-dhcp-router">{router.name}</span> : null}<VlanChip vlan={n.vlan} main={router.main && n.vlan === 1} /></td>
              {dhcp.on ? <>
                <td><code className="bb-mono">{n.pool ? `${tail(n.pool[0], n.cidr)} – ${tail(n.pool[1], n.cidr)}` : ''}</code>
                  {dhcp.dns ? <small className="bb-muted bb-dhcp-dns">{t('beatbox.net.dhcp.dns_short', { dns: dhcp.dns })}</small> : null}</td>
                <td>{leaseWords(dhcp.lease)}</td>
              </> : <td colSpan={2} className="bb-faint">{t('beatbox.net.dhcp.off')}</td>}
              <td><Switch checked={dhcp.on} label={t('beatbox.net.dhcp.on_vlan', { vlan: n.vlan })}
                onChange={(on) => { if (!busy) void configure(router.name, n.vlan, { on }); }} /></td>
              <td>{dhcp.on ? <button type="button" className="bb-router-row-action" disabled={busy} onClick={() => open(router.name, n)}>{t('beatbox.net.dhcp.change')}</button> : null}</td>
            </tr>;
          })}</tbody>
        </table>
      </div>
      {open_ ? <form className="bb-nat-add bb-dhcp-edit" onSubmit={(event) => {
        event.preventDefault();
        if (busy) return;
        const base = ipNumber(open_.side.cidr.split('/')[0]!);
        void configure(open_.router.name, open_.side.vlan, {
          first: ipNumber(first.trim()) - base, last: ipNumber(last.trim()) - base, lease: Number(lease), dns: dns.trim(),
        }).then((ok) => { if (ok) setEditing(''); });
      }}>
        <VlanChip vlan={open_.side.vlan} main={open_.router.main && open_.side.vlan === 1} />
        <span>{t('beatbox.net.dhcp.gives')}</span>
        <input className="bb-input bb-mono bb-dhcp-address" disabled={busy} value={first} aria-label={t('beatbox.net.dhcp.first')} onChange={(e) => setFirst(e.target.value)} />
        <span>{t('beatbox.net.dhcp.to')}</span>
        <input className="bb-input bb-mono bb-dhcp-address" disabled={busy} value={last} aria-label={t('beatbox.net.dhcp.last')} onChange={(e) => setLast(e.target.value)} />
        <span>{t('beatbox.net.dhcp.for')}</span>
        <Select label={t('beatbox.net.dhcp.lease')} value={lease} onChange={setLease} tiny
          options={[...new Set([...LEASES, Number(lease)])].sort((a, b) => a - b).map((value) => ({ value: String(value), label: leaseWords(value) }))} />
        <span>{t('beatbox.net.dhcp.dns_is')}</span>
        <input className="bb-input bb-mono bb-dhcp-address" disabled={busy} value={dns} placeholder={t('beatbox.net.dhcp.the_router')}
               aria-label={t('beatbox.net.dhcp.dns')} onChange={(e) => setDns(e.target.value)} />
        <Button size="sm" variant="primary" type="submit" disabled={busy || !first.trim() || !last.trim()}>{t('beatbox.net.dhcp.save')}</Button>
        <Button size="sm" disabled={busy} onClick={() => { setEditing(''); setTold(''); }}>{t('beatbox.files.cancel')}</Button>
      </form> : null}
      {held.length ? <details className="bb-fold">
        <summary>
          <Icon name="arrow" size="xs" />
          <span><b>{t('beatbox.net.dhcp.held')}</b> <span className="bb-muted">· {[
            t(held.length === 1 ? 'beatbox.net.dhcp.n_machines_one' : 'beatbox.net.dhcp.n_machines', { count: held.length }),
            reserved ? t(reserved === 1 ? 'beatbox.net.dhcp.n_reserved_one' : 'beatbox.net.dhcp.n_reserved', { count: reserved }) : '',
            servers ? t('beatbox.net.dhcp.n_server', { count: servers }) : '',
          ].filter(Boolean).join(' · ')}</span></span>
          <span className="bb-fold-see">{t('beatbox.net.fold.see')}</span><span className="bb-fold-hide">{t('beatbox.net.fold.hide')}</span>
        </summary>
        <div className="bb-router-table-scroll">
          <table className="bb-router-table">
            <thead><tr><th scope="col">{t('beatbox.net.dhcp.machine')}</th><th scope="col">MAC</th>
              <th scope="col">{t('beatbox.net.dhcp.address')}</th><th scope="col">{t('beatbox.net.dhcp.until')}</th>
              <th scope="col"><span className="sr-only">{t('beatbox.net.router.design.actions')}</span></th></tr></thead>
            <tbody>{held.map(({ node, side: n }) => {
              const dhcp = n!.dhcp ?? DHCP_DEFAULT;
              const server = net.serverOf(node.node);
              // A Server's machine gets a fixed address when cabled; it never asks DHCP.
              const written = !dhcp.on || node.light?.dhcp === false || !!server;
              const until = written ? t('beatbox.net.dhcp.written')
                : node.lease_ms ? t('beatbox.net.dhcp.until_at', { until: hm(new Date(node.lease_ms + dhcp.lease * 1000)), renews: hm(new Date(node.lease_ms + dhcp.lease * 500)) })
                : t('beatbox.net.dhcp.not_yet');
              return <tr key={node.node}>
                <td>{net.label(node.node)}{node.light ? null : <span className="bb-net-vlan bb-dhcp-kind">{t(server ? 'beatbox.net.dhcp.server' : 'beatbox.net.dhcp.vm')}</span>}</td>
                <td className="bb-muted"><code className="bb-mono">{node.mac}</code></td>
                <td><code className="bb-mono">{node.address}</code></td>
                <td className="bb-muted">{until}</td>
                <td>{server ? <span className="bb-net-vlan bb-dns-server">{t('beatbox.net.dhcp.reserved_server')}</span> : written ? null
                  : <span className="bb-dhcp-act">
                    {node.reserved ? <span className="bb-net-vlan">{t('beatbox.net.dhcp.reserved')}</span> : null}
                    <button type="button" className="bb-router-row-action" disabled={busy}
                      onClick={() => void run(() => act(network.id, 'node.reserve', { node: node.node, on: !node.reserved }))}>
                      {t(node.reserved ? 'beatbox.net.dhcp.release' : 'beatbox.net.dhcp.reserve')}</button>
                  </span>}</td>
              </tr>;
            })}</tbody>
          </table>
        </div>
      </details> : <p className="bb-net-say">{t('beatbox.net.dhcp.nobody')}</p>}
      </>}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </Card>
  );
}

/** The Names page: DHCP and DNS side by side (issue #52). */
export function NetworkNames() {
  const net = useNetwork();
  // Same colour order as the Router page.
  paletteOrder = [...new Set(net.routers.flatMap((r) => r.networks.map((n) => n.vlan)).concat(vlansIn(topoOf(net))))].filter((v) => v !== 1).sort((a, b) => a - b);
  return (
    <Page page="names" net={net} info={t('beatbox.net.names.about', { name: net.network?.name ?? '' })}>
      {net.network ? <div className="bb-addr-grid">
        <DhcpCard key={`dhcp:${net.network.id}`} net={net} />
        <Dns key={`dns:${net.network.id}`} net={net} />
      </div> : null}
    </Page>
  );
}

const cableOfFacts = (facts: Record<string, unknown>): Cable => {
  const now = bag(facts.traffic);
  return {
    latency_ms: Number(now.latency_ms ?? 0), jitter_ms: Number(now.jitter_ms ?? 0),
    loss_percent: Number(now.loss_percent ?? 0), bandwidth_kbps: Number(now.bandwidth_kbps ?? 0),
  };
};

/** "35 ms · ±8 ms · 1 in 200 lost · 20 Mbit/s", or "a perfect cable". */
function cableLine(cable: Cable): string {
  if (presetOf(cable) === 'perfect') return t('beatbox.net.cable.perfect');
  const felt = feel(cable);
  return [
    t('beatbox.net.cable.delay', { ms: cable.latency_ms }),
    cable.jitter_ms ? t('beatbox.net.cable.wobble', { ms: cable.jitter_ms }) : '',
    felt.lossOneIn ? t('beatbox.net.cable.lost', { n: count(felt.lossOneIn) }) : '',
    cable.bandwidth_kbps ? width(cable.bandwidth_kbps) : '',
  ].filter(Boolean).join(' · ');
}

const width = (kbps: number) => (kbps <= 0 ? t('beatbox.net.traffic.no_limit')
  : kbps >= 1000000 ? t('beatbox.net.traffic.gbit', { n: count(kbps / 1000000) })
  : kbps >= 1000 ? t('beatbox.net.traffic.mbit', { n: count(kbps / 1000) })
  : t('beatbox.net.traffic.kbit', { n: count(kbps) }));

/** One-line summary: counts and whether it reaches the internet. */
function summaryOf(net: Net): string {
  const topo = topoOf(net);
  const count = (kind: string) => topo.devices.filter((d) => d.kind === kind).length;
  const routers = count('router');
  const switches = count('switch');
  const machines = net.nodes.length;
  return [
    t(routers === 1 ? 'beatbox.net.summary.routers_one' : 'beatbox.net.summary.routers', { count: routers }),
    switches ? t(switches === 1 ? 'beatbox.net.summary.switches_one' : 'beatbox.net.summary.switches', { count: switches }) : '',
    t(machines === 1 ? 'beatbox.net.vlans.machines_one' : 'beatbox.net.vlans.machines', { count: machines }),
    t(net.internet ? 'beatbox.net.summary.internet_on' : 'beatbox.net.summary.internet_off'),
  ].filter(Boolean).join(' · ');
}

export function NetworkOverview() {
  const net = useNetwork();
  const { machines, reload } = useMachines(net.network?.id);
  const { flows, lookups } = useFlows(net.network?.id ?? '', net.running);
  const network = net.network;
  const cabled = machines.filter((machine) => cableOf(machine)?.sandbox === network?.id);
  const blockedBy = cabled.filter((machine) => !off(machine)).map((machine) => machine.name);
  const story = tell(flows ?? []);
  const outside = lookups.filter((lookup) => !lookup.answer && !lookup.rule).length;
  const since = network ? hm(new Date(network.state.since)) : '';
  const [selected, setSelected] = useState<Selection>(null);
  const [told, setTold] = useState('');
  paletteOrder = vlansIn(topoOf(net));
  useEffect(() => setSelected(null), [network?.id]);
  async function remove(what: NonNullable<Selection>) {
    setTold('');
    try {
      await removeSelected(net, machines, what);
      await Promise.all([net.refresh(), reload()]);
      setSelected(null);
    } catch (error) {
      setTold(fault(error));
    }
  }
  // Delete removes the selection, Esc clears it. Ignored while typing.
  useEffect(() => {
    const keys = (event: KeyboardEvent) => {
      const at = event.target as HTMLElement | null;
      if (at && (at.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(at.tagName))) return;
      if (event.key === 'Escape') setSelected(null);
      if (event.key === 'Delete' && selected) void remove(selected);
    };
    window.addEventListener('keydown', keys);
    return () => window.removeEventListener('keydown', keys);
  });

  return (
    <Page page="overview" net={net} info={t('beatbox.net.overview.about', { name: network?.name ?? '' })}
          title={network ? (
            <RenameTitle name={network.name}
                         onRename={async (name) => { await call('sandbox.rename', { id: network.id, name }); await net.reload(); }}>
              <Pill tone={TONES[network.state.lifecycle] ?? 'neutral'}>
                {net.running ? t('beatbox.net.running_since', { time: since }) : runtimeText(`beatbox.plain.state.${network.state.lifecycle}`)}</Pill>
            </RenameTitle>) : undefined}
          lede={network ? <>{t('beatbox.net.overview.lede', { cidr: String(net.facts.cidr ?? ''), parts: summaryOf(net) })}
            {network.state.reason ? <span className="bb-faint"> {say(network.state.reason)}</span> : null}</> : undefined}
          actions={network ? <div className="bb-net-page-actions">
            <RangeAction key={network.id} net={net} blockedBy={blockedBy} />
            <Moves net={net} cabled={cabled} />
          </div> : undefined}>
      {network ? (
        <>
          <Counted counters={bag(net.facts.counters)} since={since} />
          <div className="bb-net-editor">
            <Palette net={net} machines={machines} reload={reload} selected={selected} onSelect={setSelected} />
            <Card title={<><Icon name="network" size="sm" />{t('beatbox.net.picture.title')}</>}>
              <EditorPicture net={net} machines={machines} reload={reload} selected={selected} onSelect={setSelected} />
              {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
              <div className="bb-net-talks">
                <div className="bb-wf-label">{t('beatbox.net.flows.between')}</div>
                {story.talks.length ? story.talks.map((talk, i) => <TalkRow key={i} talk={talk} label={net.label} />)
                  : <p className="bb-net-say">{t(flows === null && !net.running ? 'beatbox.net.flows.off' : 'beatbox.net.story.quiet')}</p>}
              </div>
            </Card>
            <aside className="bb-net-side">
              {selected ? <Inspector key={`${selected.kind}-${selected.name}`} net={net} machines={machines} reload={reload} selected={selected}
                                     onSelect={setSelected} onRemove={(what) => void remove(what)} /> : null}
              <Card title={<><Icon name="pulse" size="sm" />{t('beatbox.net.lately')}</>}>
                {outside ? <p className="bb-net-line">{t('beatbox.net.story.outside', { count: count(outside) })}</p> : null}
                {story.noisePackets ? <p className="bb-net-line">{t(story.noise.every((heard) => !heard.allowed)
                  ? 'beatbox.net.story.noise_blocked' : 'beatbox.net.story.noise', { count: count(story.noisePackets) })}</p> : null}
                {!outside && !story.noisePackets ? <p className="bb-net-say">{t('beatbox.net.lately.none')}</p> : null}
                <a className="bb-net-link" href={href('network/flows')}>{t('beatbox.net.lately.all')} →</a>
              </Card>
              <a className="bb-net-cablelink" href={href('network/router')}>
                <span><span className="bb-wf-label">{t('beatbox.net.router.title')}</span>
                  <span>{t(Object.keys(bag(net.facts.dns)).length + net.records.length === 1 ? 'beatbox.net.router.line_one' : 'beatbox.net.router.line', {
                    gateway: String(net.facts.gateway ?? ''), names: Object.keys(bag(net.facts.dns)).length + net.records.length,
                    policy: t(net.defaultAllow ? 'beatbox.net.policy.allow' : 'beatbox.net.policy.deny'),
                  })}</span></span>
                <Icon name="arrow" size="sm" />
              </a>
              <a className="bb-net-cablelink" href={href('network/traffic')}>
                <span><span className="bb-wf-label">{t('beatbox.net.cable.title')}</span><span>{cableLine(cableOfFacts(net.facts))}</span></span>
                <Icon name="arrow" size="sm" />
              </a>
              <Keep key={network.id} net={net} />
            </aside>
          </div>
        </>
      ) : null}
    </Page>
  );
}

interface Point { id: string; name: string; at_ms: number }

/** Save the network: to a file, as a copy, or as a save point. */
function Keep({ net }: { net: Net }) {
  const network = net.network!;
  const can = network.capabilities.includes('point.save');
  const [points, setPoints] = useState<Point[]>([]);
  const [naming, setNaming] = useState<'' | 'file' | 'copy' | 'point'>('');
  const [text, setText] = useState('');
  const [folder, setFolder] = useState('');
  const [picking, setPicking] = useState(false);
  const [asking, setAsking] = useState('');
  const [busy, setBusy] = useState(false);
  const [said, setSaid] = useState('');
  const [told, setTold] = useState('');
  const list = useCallback(async () => {
    const answer = await act(network.id, 'point.list');
    setPoints(rows(answer.points).map((p) => ({ id: String(p.id), name: String(p.name ?? ''), at_ms: Number(p.at_ms) })));
  }, [network.id]);
  useEffect(() => { if (can) void list().catch(() => undefined); }, [can, list]);
  // Default to Downloads.
  useEffect(() => {
    call('files.places').then((places) => {
      const place = places.find((p) => p.key === 'downloads') ?? places.find((p) => p.key === 'home');
      if (place) setFolder((now) => now || place.path);
    }).catch(() => undefined);
  }, []);

  async function run(work: () => Promise<unknown>) {
    setBusy(true);
    setTold('');
    setSaid('');
    try {
      await work();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  const write = () => run(async () => {
    const done = await act(network.id, 'network.export', { file: true, folder, name: text.trim() });
    setNaming('');
    setSaid(t('beatbox.net.keep.exported', { path: String(done.path) }));
  });
  const copy = () => run(async () => {
    const state = bag((await act(network.id, 'network.export')).state);
    const made = await call('sandbox.create', {
      type: 'network', name: text.trim(), cpu: 0, memory: 0,
      settings: { cidr: String(state.cidr), firewall: { default: String(bag(state.firewall).default ?? 'allow') } }, uses: {},
    });
    await act(made.id, 'network.import', { state });
    setNaming('');
    remember('network', made.id);
  });
  const mark = () => run(async () => {
    await act(network.id, 'point.save', { name: text.trim() });
    setNaming('');
    await list();
  });
  const back = (id: string) => run(async () => {
    setAsking('');
    await act(network.id, 'point.restore', { id });
    await net.reload();
  });
  const drop = (id: string) => run(async () => {
    await act(network.id, 'point.remove', { id });
    await list();
  });
  const when = (ms: number) => new Date(ms).toLocaleString(speaking(), { dateStyle: 'short', timeStyle: 'short', hour12: false });
  const ask = (what: 'file' | 'copy' | 'point') => {
    setNaming(naming === what ? '' : what);
    setText(what === 'file' ? network.name : what === 'copy' ? `${network.name}-2` : '');
  };
  const row = (icon: IconName, what: string, onClick: () => void) => (
    <button type="button" className="bb-keep-row" disabled={busy} onClick={onClick}>
      <Icon name={icon} size="sm" />
      <span><b>{runtimeText(`beatbox.net.keep.${what}`)}</b></span>
    </button>
  );
  const form = (go: () => void, word: string, placeholder: string, need: boolean, extra?: ReactNode) => (
    <form className="bb-keep-form" onSubmit={(e) => { e.preventDefault(); go(); }}>
      <div className="bb-keep-name">
        <input className="bb-input" autoFocus value={text} placeholder={placeholder} onChange={(e) => setText(e.target.value)} />
        <Button size="sm" variant="primary" disabled={busy || (need && !text.trim())} onClick={go}>{word}</Button>
      </div>
      {extra}
    </form>
  );

  if (!can) {
    return (
      <Card title={<><Icon name="save" size="sm" />{t('beatbox.net.keep.title')}</>}>
        <p className="bb-net-say">{t('beatbox.net.keep.old_daemon')}</p>
      </Card>
    );
  }
  return (
    <Card title={<><Icon name="save" size="sm" />{t('beatbox.net.keep.title')}</>}>
      {picking ? <FilePicker folders title={t('beatbox.net.keep.where')} suffixes="" filterLabel="" start={folder}
                             onPick={(chosen) => { setFolder(chosen); setPicking(false); }}
                             onClose={() => setPicking(false)} /> : null}
      {row('file', 'file', () => ask('file'))}
      {naming === 'file' ? form(() => void write(), t('beatbox.net.keep.file_do'), network.name, false, (
        <button type="button" className="bb-keep-folder" title={folder} onClick={() => setPicking(true)}>
          <Icon name="folder" size="sm" />
          <span className="bb-mono">{folder}</span>
          <b>{t('beatbox.net.keep.change_folder')}</b>
        </button>
      )) : null}
      {row('copy', 'copy', () => ask('copy'))}
      {naming === 'copy' ? form(() => void copy(), t('beatbox.net.keep.copy_do'), '', true) : null}
      {row('flag', 'point', () => ask('point'))}
      {naming === 'point' ? form(() => void mark(), t('beatbox.net.keep.point_do'), t('beatbox.net.keep.point_name'), false) : null}
      {said ? <p className="bb-net-say bb-keep-said">{said}</p> : null}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
      {points.length ? (
        <div className="bb-keep-points">
          <div className="bb-wf-label">{t('beatbox.net.keep.points')}</div>
          {points.map((point) => (
            <div key={point.id} className="bb-keep-point">
              <span><b>{point.name || when(point.at_ms)}</b>{point.name ? <small>{when(point.at_ms)}</small> : null}</span>
              {asking === point.id ? (
                <Button size="sm" variant="primary" disabled={busy} onClick={() => void back(point.id)}>{t('beatbox.net.keep.back_sure')}</Button>
              ) : (
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => setAsking(point.id)}>{t('beatbox.net.keep.back')}</Button>
              )}
              <Button size="sm" variant="ghost" icon="trash" title={t('beatbox.net.keep.forget')}
                      disabled={busy} onClick={() => void drop(point.id)} />
            </div>
          ))}
          <p className="bb-net-say">{t('beatbox.net.keep.vms_stay')}</p>
        </div>
      ) : null}
    </Card>
  );
}

/** Any router with IPv6 on? */
const speaks6 = (net: Net) => net.routers.some((router) => router.networks.some((side) => side.ipv6));

/** What happens when no rule matches: Allow or Deny, applied at once. */
function DefaultPolicy({ net, compact = false }: { net: Net; compact?: boolean }) {
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  async function choose(mode: string) {
    if (!net.network || net.defaultAllow === (mode === 'allow')) return;
    setBusy(true);
    setTold('');
    try {
      await act(net.network.id, 'firewall.default', { policy: mode });
      await net.refresh();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  return (
    <>
      <span className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.net.firewall.default')}>
        {['allow', 'deny'].map((mode) => (
          <button key={mode} type="button" role="radio" aria-checked={net.defaultAllow === (mode === 'allow')}
                  className={net.defaultAllow === (mode === 'allow') ? 'on' : ''} disabled={busy}
                  onClick={() => void choose(mode)}>
            {runtimeText(`beatbox.net.policy.${mode}`)}
          </button>
        ))}
      </span>
      {compact ? null : <span className="bb-muted">{t(net.defaultAllow ? 'beatbox.net.policy.allow.what' : 'beatbox.net.policy.deny.what')}</span>}
      {told ? <span className="bb-sbx-no" style={{ fontSize: 12, margin: 0 }}>{told}</span> : null}
    </>
  );
}

/* ── Router ───────────────────────────────────────────────────────────── */

/** The parts of `network.renumber_plan` we use. */
interface Renumbering {
  from: string; to: string; gateway: string; settings: Record<string, unknown>;
  nodes: { what: string; from: string; to: string }[];
  names: { what: string; from: string; to: string }[];
  rules: { id: string; end: string; from: string; to: string }[];
  kept: string[];
}

/**
 * Move the network to a new range: fetch the plan as you type, show what
 * moves, apply exactly that. Only with everything off, or running machines
 * keep their old addresses.
 */
function Range({ net, blockedBy, initialOpen = false, onClose }: { net: Net; blockedBy: string[]; initialOpen?: boolean; onClose?: () => void }) {
  const network = net.network!;
  const [open, setOpen] = useState(initialOpen);
  const [text, setText] = useState('');
  const [plan, setPlan] = useState<Renumbering | null>(null);
  const [problem, setProblem] = useState('');
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);

  useEffect(() => {
    if (!open) return;
    const cidr = text.trim();
    setPlan(null);
    setProblem('');
    if (!cidr) return;
    let live = true;
    const timer = window.setTimeout(() => {
      act(network.id, 'network.renumber_plan', { cidr }).then(
        (answer) => { if (live) setPlan(answer as unknown as Renumbering); },
        (error) => { if (live) setProblem(fault(error)); },
      );
    }, 250);
    return () => { live = false; window.clearTimeout(timer); };
  }, [open, text, network.id]);

  async function apply() {
    if (!plan || busy || block) return;
    setBusy(true);
    setProblem('');
    try {
      await call('sandbox.reconfigure', { id: network.id, cpu: 0, memory: 0, settings: plan.settings });
      await net.reload();
      setOpen(false);
      setText('');
      setPlan(null);
      setDone(true);
      onClose?.();
    } catch (error) {
      setProblem(fault(error));
    }
    setBusy(false);
  }

  const block = rangeBlock(net, blockedBy);
  if (!open) {
    return (
      <div className="bb-net-first">
        <Button icon="refresh" disabled={!!block} onClick={() => { setDone(false); setOpen(true); }}>{t('beatbox.net.range.change')}</Button>
        {block ? <small className="bb-faint" style={{ marginLeft: 12 }}>{block}</small> : null}
        {done ? <p className="bb-net-say" style={{ marginTop: 12 }}>{t('beatbox.net.range.after')}</p> : null}
      </div>
    );
  }
  return (
    <div className="bb-net-first bb-net-range">
      <label className="bb-field">
        <span>{t('beatbox.net.range.new')} <Info text={`${t('beatbox.net.range.hint')} ${t('beatbox.net.range.unchanged')}`} /></span>
        <input className="bb-input bb-mono" disabled={busy} value={text} placeholder="10.60.0.0/24" autoFocus onChange={(e) => setText(e.target.value)} />
      </label>
      {problem ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{problem}</p> : null}
      {plan ? (
        <div className="bb-net-plan">
          <div className="bb-wf-label">{t('beatbox.net.range.preview', { from: plan.from, to: plan.to })}</div>
          <div className="bb-net-facts">
            <span>{t('beatbox.net.router.title')}</span><code className="bb-mono">{plan.gateway}</code>
            {plan.nodes.map((moved) => (
              <Fragment key={moved.what}><span>{net.label(moved.what)}</span><code className="bb-mono">{moved.from} → {moved.to}</code></Fragment>
            ))}
            {plan.names.map((moved) => (
              <Fragment key={moved.what}><span>{moved.what}</span><code className="bb-mono">{moved.from} → {moved.to}</code></Fragment>
            ))}
            {plan.rules.map((moved, i) => (
              <Fragment key={`${moved.id}-${moved.end}-${i}`}>
                <span>{t('beatbox.net.range.rule', { n: net.rules.findIndex((rule) => rule.id === moved.id) + 1 || '·' })} · {runtimeText(`beatbox.net.range.${moved.end}`)}</span>
                <code className="bb-mono">{moved.from} → {moved.to}</code>
              </Fragment>
            ))}
          </div>
          {plan.kept.length ? <p className="bb-net-note">{t('beatbox.net.range.kept', { list: plan.kept.join(', ') })}</p> : null}
        </div>
      ) : null}
      <div className="bb-net-movebtns" style={{ marginTop: 14 }}>
        <Button disabled={busy} onClick={() => { setOpen(false); setText(''); onClose?.(); }}>{t('beatbox.net.range.cancel')}</Button>
        <Button variant="primary" icon="check" disabled={!plan || busy || !!block} onClick={() => void apply()}>{t('beatbox.net.range.apply')}</Button>
      </div>
      {block ? <p className="bb-net-say" style={{ marginTop: 12 }}>{block}</p> : null}
    </div>
  );
}

/** Renumber button outside the router inspector. */
function RangeAction({ net, blockedBy }: { net: Net; blockedBy: string[] }) {
  const [open, setOpen] = useState(false);
  const [told, setTold] = useState('');
  const block = rangeBlock(net, blockedBy);
  // Never disabled: pressing it says why below, where keyboards and screen
  // readers reach it. A disabled button's tooltip can't be focused.
  return <div className="bb-net-moves">
    <Button icon="refresh" onClick={() => (block ? setTold(block) : setOpen(true))}>{t('beatbox.net.range.change')}</Button>
    {told && told === block ? <p className="bb-sbx-no" role="status" style={{ fontSize: 12, margin: 0 }}>{block}</p> : null}
    {open ? <Modal title={t('beatbox.net.range.change')} icon="refresh" onClose={() => setOpen(false)}>
      <div className="bb-router-range-modal"><Range net={net} blockedBy={blockedBy} initialOpen onClose={() => setOpen(false)} /></div>
    </Modal> : null}
  </div>;
}

/**
 * The Router page: gateway, DHCP, DNS, firewall, internet. Same switch
 * thread underneath; we just show it as two devices.
 */
export function NetworkRouter() {
  const net = useNetwork();
  const { machines } = useMachines(net.network?.id);
  const [chosenRouter, setChosenRouter] = useState<{ network: string; router: string } | null>(null);
  const main = net.routers.find((r) => r.main);
  const selected = chosenRouter && chosenRouter.network === net.network?.id ? chosenRouter.router : '';
  const router = net.routers.find((r) => r.name === selected) ?? main ?? net.routers[0];
  const blockedBy = machines.filter((machine) => cableOf(machine)?.sandbox === net.network?.id && !off(machine)).map((machine) => machine.name);
  paletteOrder = [...new Set(net.routers.flatMap((r) => r.networks.map((n) => n.vlan)).concat(vlansIn(topoOf(net))))].filter((v) => v !== 1).sort((a, b) => a - b);
  return (
    <Page page="router" net={net} title={<span className="bb-router-heading"><Icon name="router" />{t('beatbox.net.router.title')}</span>}
          info={t('beatbox.net.router.design.about')}
          actions={router?.main ? <RangeAction key={net.network?.id} net={net} blockedBy={blockedBy} /> : undefined}>
      {net.network ? <div className="bb-router-page">
        {router ? <>
          <div className="bb-router-picker">
            <div className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.net.router.title')} onKeyDown={(event) => {
              if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
              event.preventDefault();
              const current = net.routers.findIndex((r) => r.name === router.name);
              const next = event.key === 'Home' ? 0 : event.key === 'End' ? net.routers.length - 1
                : (current + (event.key === 'ArrowRight' ? 1 : -1) + net.routers.length) % net.routers.length;
              setChosenRouter({ network: net.network!.id, router: net.routers[next]!.name });
              event.currentTarget.querySelectorAll('button')[next]?.focus();
            }}>
              {net.routers.map((r) => <button key={r.name} type="button" role="radio" aria-checked={r.name === router.name} tabIndex={r.name === router.name ? 0 : -1}
                className={r.name === router.name ? 'on' : ''} onClick={() => setChosenRouter({ network: net.network!.id, router: r.name })}>
                {r.name}{r.main ? ` · ${t('beatbox.net.router.design.principal')}` : ''}</button>)}
            </div>
            {router.main ? <Internet net={net} />
              : <p className="bb-router-internet">{t('beatbox.net.router.design.main_internet', { router: main?.name ?? '' })}</p>}
          </div>
          <div className="bb-router-grid">
            <div className="bb-net-stack">
              <Card title={<><Icon name="network" size="sm" />{t('beatbox.net.vlans.title')}</>}
                    info={t('beatbox.net.router.design.networks_what', { firewall: t('beatbox.net.router.design.the_firewall') })}>
                <div className="bb-net-first">
                  <RouterNetworks key={`${net.network.id}:${router.name}`} net={net} router={router.name} table blockedBy={blockedBy} />
                </div>
              </Card>
              <Card title={<><Icon name="branch" size="sm" />{t('beatbox.net.routes.title')}</>} info={t('beatbox.net.routes.what')}>
                <div className="bb-net-first">
                  <Routes key={`${net.network.id}:${router.name}`} net={net} router={router.name} table />
                </div>
              </Card>
              <Card title={<><Icon name="branch" size="sm" />{t('beatbox.net.nat.forwards')}</>}
                    info={`${t('beatbox.net.nat.forwards_what')} ${t('beatbox.net.nat.inside_only')}`}
                    note={<span className="bb-faint">{t('beatbox.net.nat.forwards_note')}</span>}>
                <RouterForwards key={`${net.network.id}:${router.name}`} net={net} router={router.name} />
              </Card>
            </div>
          </div>
        </> : <p className="bb-net-say">{t('beatbox.net.router.design.no_router')}</p>}
      </div> : null}
    </Page>
  );
}

/**
 * Internet access, off by default. Traffic leaves through this computer's
 * connection as BeatBox; only replies come back in.
 */
function Internet({ net }: { net: Net }) {
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  async function turn(on: boolean) {
    if (!net.network) return;
    setBusy(true);
    setTold('');
    try {
      await act(net.network.id, 'router.internet', { on });
      await net.refresh();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  return (
    <div className="bb-router-internet">
      <div className="bb-sbx-toggle">
        <Switch id="bb-net-internet" checked={net.internet} label={t('beatbox.net.internet.switch')}
                onChange={(on) => { if (!busy) void turn(on); }} />
        <label htmlFor="bb-net-internet">
          <b>{t('beatbox.net.internet.switch')} <Info text={t('beatbox.net.router.design.internet_what')} /></b>
        </label>
      </div>
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12, margin: 0 }}>{told}</p> : null}
    </div>
  );
}

/* ── Firewall ─────────────────────────────────────────────────────────── */

/** Protocols under "something else" (plus a raw number and non-IP). */
const PROTOCOLS = ['tcp', 'udp', 'icmp', 'gre', 'esp', 'ah', 'ospf', 'vrrp', 'sctp'] as const;

/** Service choices: built-in, custom, any, or "something else". */
function serviceOptions(net: Net, withAny: boolean) {
  const mine = Object.entries(net.services);
  return [
    ...SERVICES.map((what) => ({
      value: what.key, label: named(service(what.protocol, what.port)),
      hint: what.protocol === 'icmp' ? undefined : `${what.protocol} ${what.port}`,
    })),
    ...mine.map(([name, what]) => ({ value: `mine:${name}`, label: name, hint: portsText(what.protocol, what.port, what.port_last) })),
    ...(withAny ? [{ value: 'any', label: t('beatbox.net.rule.anything') }] : []),
    { value: 'other', label: t('beatbox.net.rule.other') },
  ];
}

/** `tcp 22`, `tcp 8000–8100`, or the protocol alone. */
const portsText = (protocol: string, port: number | null, last?: number | null) =>
  port === null ? protocol : last != null && last !== port ? `${protocol} ${port}–${last}` : `${protocol} ${port}`;

/** Parse "8000-8100", "22" or ""; `undefined` if invalid. */
function readPorts(text: string): { port: number | null; port_last: number | null } | undefined {
  if (!text.trim()) return { port: null, port_last: null };
  const match = /^\s*(\d+)\s*(?:[-–]\s*(\d+))?\s*$/.exec(text);
  if (!match) return undefined;
  const port = Number(match[1]);
  return { port, port_last: match[2] ? Number(match[2]) : port };
}

interface Other { protocol: string; value: string }

/** A picked service as `firewall.add` fields. */
function picked(net: Net, key: string, other: Other): Record<string, unknown> {
  const known = SERVICES.find((what) => what.key === key);
  if (known) return known.protocol === 'icmp' ? { protocol: 'icmp', icmp_type: 8 } : { protocol: known.protocol, port: known.port };
  const mine = key.startsWith('mine:') ? net.services[key.slice(5)] : undefined;
  if (mine) return { protocol: mine.protocol, port: mine.port, port_last: mine.port_last, service: key.slice(5) };
  if (key === 'any') return { protocol: 'any' };
  const value = other.value.trim();
  if (other.protocol === 'ethertype') return { ethertype: value ? Number(value.startsWith('0x') ? value : `0x${value}`) : NaN };
  if (other.protocol === 'number') return { protocol: value };
  if (other.protocol === 'icmp') return value ? { protocol: 'icmp', icmp_type: Number(value) } : { protocol: 'icmp' };
  const ports = readPorts(value);
  return { protocol: other.protocol, port: ports ? ports.port : NaN, port_last: ports?.port_last ?? null };
}

/** The test packet for "Would it get through?". */
function attemptOf(fields: Record<string, unknown>) {
  const protocol = String(fields.protocol ?? 'any');
  return { protocol: protocol === 'any' ? 'tcp' : protocol, port: Number(fields.port ?? 0), icmpType: fields.icmp_type == null ? 8 : Number(fields.icmp_type) };
}

function ServicePick({ net, value, other, onValue, onOther, withAny }: {
  net: Net; value: string; other: Other; withAny: boolean;
  onValue: (v: string) => void; onOther?: (v: Other) => void;
}) {
  return (
    <>
      <Select label={t('beatbox.net.rule.service')} value={value} onChange={onValue} options={serviceOptions(net, withAny)} />
      {value === 'other' && onOther ? <OtherFields other={other} onOther={onOther} /> : null}
    </>
  );
}

/** Fields for "something else": protocol by name/number, or a non-IP frame. */
function OtherFields({ other, onOther }: { other: Other; onOther: (v: Other) => void }) {
  const hint = other.protocol === 'icmp' ? t('beatbox.net.rule.icmp_type')
    : other.protocol === 'number' ? t('beatbox.net.rule.number')
    : other.protocol === 'ethertype' ? '0x88cc'
    : ['tcp', 'udp', 'sctp'].includes(other.protocol) ? t('beatbox.net.rule.ports_ph') : '';
  return (
    <span className="bb-fw-other">
      <Select label={t('beatbox.net.rule.protocol')} value={other.protocol} onChange={(protocol) => onOther({ protocol, value: '' })} tiny
              options={[...PROTOCOLS.map((value) => ({ value })),
                { value: 'number', label: t('beatbox.net.rule.by_number') },
                { value: 'ethertype', label: t('beatbox.net.rule.not_ip') }]} />
      {hint ? <input className="bb-input bb-mono bb-fw-otherin" value={other.value} placeholder={hint} aria-label={hint}
                     onChange={(event) => onOther({ ...other, value: event.target.value })} /> : null}
    </span>
  );
}

/** A rule end as a chip. */
function End({ net, spec }: { net: Net; spec: string }) {
  if (spec.startsWith('vlan:')) return <VlanChip vlan={Number(spec.slice(5))} main={spec === 'vlan:1'} />;
  if (spec.startsWith('group:')) {
    const name = spec.slice(6);
    return <span className="bb-fw-end"><Icon name="group" size="xs" />{name}<span className="bb-faint"> · {net.groups[name]?.length ?? 0}</span></span>;
  }
  if (spec === 'any' || spec === '*' || spec === '') return <span className="bb-fw-end bb-muted">{t('beatbox.net.end.any')}</span>;
  if (spec === 'internet') return <span className="bb-fw-end"><Icon name="globe" size="xs" />{t('beatbox.net.end.internet')}</span>;
  if (spec === 'gateway') return <span className="bb-fw-end"><Icon name="router" size="xs" />{t('beatbox.net.end.gateway')}</span>;
  const node = net.nodes.find((n) => n.node === spec);
  if (node) {
    return <span className="bb-fw-end"><Icon name={node.light ? (node.light.kind === 'server' ? 'server' : 'terminal') : 'box'} size="xs" />{net.label(spec)}</span>;
  }
  return <span className="bb-fw-end bb-mono">{spec}</span>;
}

/** Choices for a rule end: VLANs, groups, machines, router, internet, any. */
function endOptions(net: Net) {
  const vlans = [...new Set(net.here.vlanNets?.map((n) => n.vlan) ?? [])].sort((a, b) => a - b);
  const groups = Object.keys(net.groups);
  return [
    ...(vlans.length ? [{ label: t('beatbox.net.end.vlans'), options: vlans.map((vlan) => ({ value: `vlan:${vlan}`, label: endWord(`vlan:${vlan}`) })) }] : []),
    ...(groups.length ? [{ label: t('beatbox.net.end.groups'), options: groups.map((name) => ({ value: `group:${name}`, label: name, hint: String(net.groups[name]!.length) })) }] : []),
    ...(net.nodes.length ? [{ label: t('beatbox.net.end.machines'), options: net.nodes.map((n) => ({ value: n.node, label: net.label(n.node) })) }] : []),
    { label: t('beatbox.net.end.others'), options: [
      { value: 'gateway', label: t('beatbox.net.end.gateway') },
      { value: 'internet', label: t('beatbox.net.end.internet') },
      { value: 'any', label: t('beatbox.net.end.any') },
    ] },
  ];
}

/** Firewall tabs: rules, name rules, building blocks. */
const FIREWALL_TABS = [
  { key: 'rules', label: 'beatbox.net.firewall.rules' },
  { key: 'names', label: 'beatbox.net.names.title' },
  { key: 'blocks', label: 'beatbox.net.firewall.blocks' },
] as const;
type FirewallTab = (typeof FIREWALL_TABS)[number]['key'];

export function NetworkFirewall() {
  const net = useNetwork();
  const { flows } = useFlows(net.network?.id ?? '', net.running);
  const [tab, setTab] = useState<FirewallTab>('rules');
  const counts: Partial<Record<FirewallTab, number>> = { rules: net.rules.length, names: net.names.length };
  return (
    <Page page="firewall" net={net} info={t('beatbox.net.firewall.lede')}>
      {net.network ? (
        <>
          <div className="bb-net-seg bb-fw-tabs" role="tablist" aria-label={t('beatbox.screen.network.firewall.title')} onKeyDown={(event) => {
            if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
            event.preventDefault();
            const at = FIREWALL_TABS.findIndex((each) => each.key === tab);
            const next = event.key === 'Home' ? 0 : event.key === 'End' ? FIREWALL_TABS.length - 1
              : (at + (event.key === 'ArrowRight' ? 1 : -1) + FIREWALL_TABS.length) % FIREWALL_TABS.length;
            setTab(FIREWALL_TABS[next]!.key);
            event.currentTarget.querySelectorAll('button')[next]?.focus();
          }}>
            {FIREWALL_TABS.map((each) => (
              <button key={each.key} type="button" role="tab" id={`bb-fw-tab-${each.key}`} aria-controls="bb-fw-pane"
                      aria-selected={tab === each.key} tabIndex={tab === each.key ? 0 : -1}
                      className={tab === each.key ? 'on' : ''} onClick={() => setTab(each.key)}>
                {t(each.label)}{counts[each.key] !== undefined ? <small>{counts[each.key]}</small> : null}
              </button>
            ))}
          </div>
          <div id="bb-fw-pane" role="tabpanel" aria-labelledby={`bb-fw-tab-${tab}`}>
            {tab === 'rules' ? (
              <div className="bb-net-grid bb-router-grid bb-fw-grid">
                <div className="bb-net-stack">
                  <Rules net={net} flows={flows}><NewRule key={net.network.id} net={net} /></Rules>
                </div>
                <aside className="bb-net-side"><Tester key="by-machine" net={net} flows={flows} /></aside>
              </div>
            ) : tab === 'names' ? (
              <div className="bb-net-grid bb-router-grid bb-fw-grid">
                <div className="bb-net-stack"><NameRules key={`names-${net.network.id}`} net={net} /></div>
                <aside className="bb-net-side"><Tester key="by-name" net={net} flows={flows} start="name" /></aside>
              </div>
            ) : (
              <div className="bb-fw-blocks"><Services net={net} /><Groups net={net} /></div>
            )}
          </div>
        </>
      ) : null}
    </Page>
  );
}

/** Firewall action + refresh, with the error kept for display. */
function useFirewallAct(net: Net) {
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  async function run(capability: string, args: Record<string, unknown>): Promise<boolean> {
    if (!net.network) return false;
    setBusy(true);
    setTold('');
    let done = true;
    try {
      await act(net.network.id, capability, args);
      await net.refresh();
    } catch (error) {
      setTold(fault(error));
      done = false;
    }
    setBusy(false);
    return done;
  }
  return { busy, told, run };
}

/** "≤ 20/min". */
const paceShort = (limit: NonNullable<Rule['limit']>) =>
  `≤ ${limit.count}/${runtimeText(`beatbox.net.limit.short.${limit.per}`)}`;

/** Connections a limited rule let through vs cut. */
function tally(flows: RawFlow[] | null, rule: string): { passed: number; cut: number } {
  const mine = (flows ?? []).filter((flow) => flow.rule === rule && !flow.answer);
  return { passed: mine.filter((flow) => flow.action === 'allow').length, cut: mine.filter((flow) => flow.limited).length };
}

/** Rules top to bottom, between the two fixed ones: replies first, default last. */
function Rules({ net, flows, children }: { net: Net; flows: RawFlow[] | null; children?: ReactNode }) {
  const { busy, told, run } = useFirewallAct(net);
  const decided = hits(flows ?? []);
  const reaches = reach(net.rules, net.here);
  return (
    <Card title={<><Icon name="shield" size="sm" />{t('beatbox.net.firewall.rules')}</>} info={t('beatbox.net.firewall.ends')}>
      <div className="bb-fw-table bb-net-first" role="table" aria-label={t('beatbox.net.firewall.rules')}>
        <div className="bb-fw-row bb-fw-head" role="row">
          <span>#</span><span>{t('beatbox.net.rule.action')}</span><span>{t('beatbox.net.rule.source')}</span>
          <span>{t('beatbox.net.rule.destination')}</span><span>{t('beatbox.net.rule.what')}</span><span>{t('beatbox.net.firewall.use')}</span><span />
        </div>
        <div className="bb-fw-row bb-fw-pinned" role="row">
          <span className="bb-faint"><Icon name="reply" size="xs" /></span>
          <span className="bb-fw-verb bb-fw-verb--allow">{verb('allow')}</span>
          <span className="bb-fw-wide">{t('beatbox.net.firewall.answers')} <Info text={t('beatbox.net.firewall.answers_what')} /></span>
          <span className="bb-fw-tools bb-fw-tools--on">
            <Switch checked={net.answers} label={t('beatbox.net.firewall.answers')}
                    onChange={(on) => { if (!busy) void run('firewall.answers', { on }); }} />
          </span>
        </div>
        {speaks6(net) ? (
          <div className="bb-fw-row bb-fw-pinned" role="row">
            <span className="bb-faint"><Icon name="search" size="xs" /></span>
            <span className="bb-fw-verb bb-fw-verb--allow">{verb('allow')}</span>
            <span className="bb-fw-wide">{t('beatbox.net.ipv6.discovery')} <Info text={t('beatbox.net.ipv6.discovery_what')} /></span>
            <span className="bb-fw-tools bb-fw-tools--on"><span className="bb-fw-badge">{t('beatbox.net.ipv6.always')}</span></span>
          </div>
        ) : null}
        {net.rules.map((rule, index) => {
          const state = reaches[rule.id];
          const hit = decided[rule.id];
          const what = ruleWhat(rule);
          return (
            <Fragment key={rule.id}>
              <div className={`bb-fw-row${state === 'shadowed' ? ' bb-fw-row--shadowed' : ''}`} role="row">
                <span className="bb-mono bb-faint">{index + 1}</span>
                <span className="bb-fw-act">
                  <span className={`bb-fw-verb bb-fw-verb--${rule.action}`}>{verb(rule.action)}</span>
                  {rule.state === 'new' ? <span className="bb-fw-badge bb-fw-badge--amber">{t('beatbox.net.rule.only_new')}</span> : null}
                  {rule.limit ? <span className="bb-fw-badge bb-fw-badge--amber">{paceShort(rule.limit)}</span> : null}
                  {rule.family ? <span className="bb-fw-badge">IPv{rule.family}</span> : null}
                </span>
                <End net={net} spec={rule.source} />
                <End net={net} spec={rule.destination} />
                <span className="bb-fw-what">{what.words ? <span>{what.words}</span> : null}{what.code ? <code className="bb-mono bb-faint">{what.code}</code> : null}</span>
                <small className={state === 'shadowed' ? 'bb-amber' : hit ? '' : 'bb-faint'}
                       title={hit ? t('beatbox.net.firewall.hits', { count: count(hit.packets), time: ago(hit.quiet) }) : undefined}>
                  {state === 'shadowed' ? t('beatbox.net.firewall.never')
                    : state === 'idle' ? t('beatbox.net.firewall.idle')
                    : rule.limit && hit ? <>{t('beatbox.net.limit.passed', { count: tally(flows, rule.id).passed })}<br /><span className="bb-rose">{t('beatbox.net.limit.cut', { count: tally(flows, rule.id).cut })}</span></>
                    : hit ? t('beatbox.net.firewall.packets', { count: count(hit.packets) })
                    : t('beatbox.net.firewall.unused')}
                </small>
                <span className="bb-fw-tools">
                  <button type="button" disabled={busy || index === 0} aria-label={t('beatbox.net.firewall.up_n', { n: index + 1 })}
                          onClick={() => void run('firewall.move', { id: rule.id, index: index - 1 })}><Icon name="up" size="xs" /></button>
                  <button type="button" className="bb-fw-down" disabled={busy || index === net.rules.length - 1} aria-label={t('beatbox.net.firewall.down_n', { n: index + 1 })}
                          onClick={() => void run('firewall.move', { id: rule.id, index: index + 1 })}><Icon name="up" size="xs" /></button>
                  <button type="button" disabled={busy} aria-label={t('beatbox.net.firewall.remove_n', { n: index + 1 })}
                          onClick={() => void run('firewall.remove', { id: rule.id })}><Icon name="close" size="xs" /></button>
                </span>
              </div>
              {state === 'shadowed' ? <p className="bb-fw-note">{t('beatbox.net.firewall.shadowed')}</p> : null}
            </Fragment>
          );
        })}
        <div className="bb-fw-row bb-fw-pinned" role="row">
          <span className="bb-faint bb-mono">…</span>
          <span>{t('beatbox.net.firewall.the_rest')}</span>
          <span className="bb-faint bb-fw-mid">{t('beatbox.net.firewall.the_rest_what')}</span>
          <span className="bb-fw-policy"><DefaultPolicy net={net} compact /></span>
        </div>
      </div>
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
      {children}
    </Card>
  );
}

/** New rule as a sentence; rarer options one click deeper. */
function NewRule({ net }: { net: Net }) {
  const { busy, told, run } = useFirewallAct(net);
  const names = net.nodes.map((node) => node.node);
  const [action, setAction] = useState('allow');
  const [source, setSource] = useState(names[0] ?? 'any');
  const [destination, setDestination] = useState(names[1] ?? 'any');
  const [what, setWhat] = useState('ssh');
  const [other, setOther] = useState<Other>({ protocol: 'tcp', value: '' });
  const [more, setMore] = useState(false);
  const [onlyNew, setOnlyNew] = useState(false);
  const [paced, setPaced] = useState(false);
  const [pace, setPace] = useState({ count: '20', per: 'minute', each: 'each', over: 'reject' });
  const [family, setFamily] = useState(0);
  // Machines load after the first render; reset the sentence when they do.
  const known = names.join('|');
  useEffect(() => {
    if (source === 'any' && names[0]) setSource(names[0]);
    if (destination === 'any' && names[1]) setDestination(names[1]);
  }, [known]);
  const choose = (key: string) => {
    setWhat(key);
    if (key === 'other') setMore(true);
  };
  const add = () => run('firewall.add', {
    action, source, destination, ...picked(net, what, other),
    ...(onlyNew ? { state: 'new' } : {}),
    ...(paced && action === 'allow' ? { limit: { count: Number(pace.count), per: pace.per, each: pace.each === 'each', over: pace.over } } : {}),
    ...(family ? { family } : {}),
  });
  return (
    <div className="bb-fw-compose">
      <div className="bb-fw-compose-head">
        <h3><Icon name="plus" size="sm" />{t('beatbox.net.firewall.add_title')}</h3>
        <span className="bb-faint">{t('beatbox.net.firewall.goes_as', { n: net.rules.length + 1 })}</span>
      </div>
      <div className="bb-net-builder">
        <Select label={t('beatbox.net.rule.action')} value={action} onChange={setAction}
                options={['allow', 'deny', 'reject'].map((v) => ({ value: v, label: verb(v) }))} />
        <Info text={t('beatbox.net.rule.deny_reject')} />
        <span>{t('beatbox.net.rule.from_word')}</span>
        <Select label={t('beatbox.net.rule.source')} value={source} onChange={setSource} options={endOptions(net)} />
        <span>{t('beatbox.net.rule.to_word')}</span>
        <Select label={t('beatbox.net.rule.destination')} value={destination} onChange={setDestination} options={endOptions(net)} />
        <span>{t('beatbox.net.rule.by')}</span>
        <ServicePick net={net} value={what} other={other} withAny onValue={choose} />
      </div>
      {speaks6(net) ? (
        <div className="bb-fw-family">
          <span>{t('beatbox.net.ipv6.in')}</span>
          <span className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.net.ipv6.in')}>
            {[0, 4, 6].map((f) => (
              <button key={f} type="button" role="radio" aria-checked={family === f} className={family === f ? 'on' : ''}
                      onClick={() => setFamily(f)}>{f ? t('beatbox.net.ipv6.only', { family: f }) : t('beatbox.net.ipv6.both')}</button>
            ))}
          </span>
        </div>
      ) : null}
      <button type="button" className="bb-net-link bb-fw-more" aria-expanded={more} onClick={() => setMore(!more)}>
        {t('beatbox.net.rule.more')} <Icon name={more ? 'up' : 'arrow'} size="xs" />
      </button>
      {more ? (
        <div className="bb-fw-moreblock">
          <div>
            <div className="bb-wf-label">{t('beatbox.net.rule.protocol_ports')} <Info text={t('beatbox.net.rule.other_what')} /></div>
            {what === 'other' ? <OtherFields other={other} onOther={setOther} />
              : <p className="bb-net-say">{t('beatbox.net.rule.pick_other')}</p>}
          </div>
          <div>
            <div className="bb-wf-label">{t('beatbox.net.rule.connections')} <Info text={t('beatbox.net.rule.only_new_what')} /></div>
            <span className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.net.rule.connections')}>
              {[false, true].map((only) => (
                <button key={String(only)} type="button" role="radio" aria-checked={onlyNew === only}
                        className={onlyNew === only ? 'on' : ''} onClick={() => setOnlyNew(only)}>
                  {t(only ? 'beatbox.net.rule.only_new_long' : 'beatbox.net.rule.all')}
                </button>
              ))}
            </span>
          </div>
          <div className="bb-fw-pace">
            <div className="bb-wf-label">{t('beatbox.net.limit.title')} <Info text={t('beatbox.net.limit.what')} /></div>
            {action === 'allow' ? (
              <>
                <div className="bb-fw-pacehead">
                  <Switch id="bb-fw-pace" checked={paced} label={t('beatbox.net.limit.on')} onChange={setPaced} />
                  <label htmlFor="bb-fw-pace"><b>{t('beatbox.net.limit.on')}</b></label>
                </div>
                {paced ? (
                  <>
                    <div className="bb-fw-pacesay">
                      <span>{t('beatbox.net.limit.at_most')}</span>
                      <input className="bb-input bb-mono bb-fw-pacecount" inputMode="numeric" value={pace.count} aria-label={t('beatbox.net.limit.count')}
                             onChange={(event) => setPace({ ...pace, count: event.target.value })} />
                      <span>{t('beatbox.net.limit.per_word')}</span>
                      <Select label={t('beatbox.net.limit.per_word')} value={pace.per} onChange={(per) => setPace({ ...pace, per })} tiny
                              options={['second', 'minute', 'hour'].map((value) => ({ value, label: runtimeText(`beatbox.net.limit.per.${value}`) }))} />
                      <span>{t('beatbox.net.limit.from')}</span>
                      <Select label={t('beatbox.net.limit.from')} value={pace.each} onChange={(each) => setPace({ ...pace, each })}
                              options={[{ value: 'each', label: t('beatbox.net.limit.each') }, { value: 'all', label: t('beatbox.net.limit.all') }]} />
                    </div>
                    <div className="bb-fw-pacesay">
                      <span>{t('beatbox.net.limit.past_it')}</span>
                      <span className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.net.limit.past_it')}>
                        {['deny', 'reject'].map((over) => (
                          <button key={over} type="button" role="radio" aria-checked={pace.over === over} className={pace.over === over ? 'on' : ''}
                                  onClick={() => setPace({ ...pace, over })}>{verb(over)}</button>
                        ))}
                      </span>
                    </div>
                  </>
                ) : null}
              </>
            ) : <p className="bb-net-say">{t('beatbox.net.limit.only_allow')}</p>}
          </div>
        </div>
      ) : null}
      <div className="bb-sbx-settings-foot">
        <span />
        <Button variant="primary" icon="plus" disabled={busy} onClick={() => void add()}>{t('beatbox.net.firewall.add')}</Button>
      </div>
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </div>
  );
}

/** Name rules list plus a sentence to add one. */
function NameRules({ net }: { net: Net }) {
  const { busy, told, run } = useFirewallAct(net);
  const machines = net.nodes.map((node) => node.node);
  const [action, setAction] = useState<NameRuleInfo['action']>('hide');
  const [name, setName] = useState('');
  const [source, setSource] = useState('any');
  const [to, setTo] = useState(machines[0] ?? '');
  const tone = (a: string) => (a === 'point' ? 'bb-violet' : 'bb-rose');
  const add = async () => {
    await run('firewall.name', { action, name: name.trim(), source, ...(action === 'point' ? { to } : {}) });
    setName('');
  };
  return (
    <Card title={<><Icon name="globe" size="sm" />{t('beatbox.net.names.title')}</>}
          note={<span className="bb-faint">{t('beatbox.net.names.first')}</span>}>
      {net.names.length ? (
        <div className="bb-names-table" role="table" aria-label={t('beatbox.net.names.title')}>
          <div className="bb-names-row bb-fw-head" role="row">
            <span>#</span><span>{t('beatbox.net.names.what')}</span><span>{t('beatbox.net.names.name')}</span>
            <span>{t('beatbox.net.names.who')}</span><span>{t('beatbox.net.firewall.use')}</span><span />
          </div>
          {net.names.map((rule) => (
            <div key={rule.id} className="bb-names-row" role="row">
              <span className="bb-mono bb-faint">{rule.id.slice(1)}</span>
              <span className={`bb-fw-verb ${tone(rule.action)}`}>{t(`beatbox.net.names.action.${rule.action}`)}</span>
              <span className="bb-names-name"><code className="bb-mono">{rule.name}</code>
                {rule.to ? <><span className="bb-faint">→</span><span>{net.label(rule.to)}</span></> : null}</span>
              <span className="bb-names-who"><End net={net} spec={rule.source} /></span>
              <small className={rule.uses ? '' : 'bb-faint'}>{rule.uses
                ? t(rule.action === 'cut' ? 'beatbox.net.names.cuts' : rule.uses === 1 ? 'beatbox.net.names.asks_one' : 'beatbox.net.names.asks', { count: rule.uses })
                : t('beatbox.net.firewall.unused')}</small>
              <span className="bb-fw-tools">
                <button type="button" disabled={busy} aria-label={t('beatbox.net.names.remove_n', { n: rule.id.slice(1) })}
                        onClick={() => void run('firewall.name_remove', { id: rule.id })}><Icon name="close" size="xs" /></button>
              </span>
            </div>
          ))}
        </div>
      ) : null}
      <div className="bb-net-builder bb-names-builder bb-net-first">
        <Select label={t('beatbox.net.names.what')} value={action} onChange={(v) => setAction(v as NameRuleInfo['action'])}
                options={(['hide', 'point', 'cut'] as const).map((v) => ({ value: v, label: t(`beatbox.net.names.action.${v}`) }))} />
        <Info text={`${t('beatbox.net.names.what_each')} ${t('beatbox.net.names.star')} ${t('beatbox.net.names.before')}`} />
        <span>{t('beatbox.net.names.the_name')}</span>
        <input className="bb-input bb-mono bb-names-input" value={name} placeholder="*.example.com" aria-label={t('beatbox.net.names.name')}
               onChange={(event) => setName(event.target.value)} onKeyDown={(event) => { if (event.key === 'Enter' && name.trim() && !busy) void add(); }} />
        {action === 'point' ? <>
          <span>→</span>
          <Select label={t('beatbox.net.names.to')} value={to} onChange={setTo} options={machines.map((m) => ({ value: m, label: net.label(m) }))} />
        </> : null}
        <span>{t('beatbox.net.names.for')}</span>
        <Select label={t('beatbox.net.names.who')} value={source} onChange={setSource} options={endOptions(net)} />
      </div>
      <div className="bb-sbx-settings-foot">
        <span />
        <Button variant="primary" icon="plus" disabled={busy || !name.trim()} onClick={() => void add()}>{t('beatbox.net.firewall.add')}</Button>
      </div>
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </Card>
  );
}

/** Built-in and custom services. */
function Services({ net }: { net: Net }) {
  const { busy, told, run } = useFirewallAct(net);
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState('');
  const [protocol, setProtocol] = useState('tcp');
  const [ports, setPorts] = useState('');
  const mine = Object.entries(net.services);
  async function add() {
    const read = readPorts(ports);
    if (await run('firewall.service', { name: name.trim(), protocol, port: read ? read.port : NaN, port_last: read?.port_last ?? null })) {
      setAdding(false);
      setName('');
      setPorts('');
    }
  }
  return (
    <Card title={<><Icon name="tag" size="sm" />{t('beatbox.net.services.title')}</>} info={t('beatbox.net.services.what')}>
      <div className="bb-wf-label bb-net-first">{t('beatbox.net.services.built_in')}</div>
      <div className="bb-fw-list">
        {SERVICES.map((what) => (
          <div key={what.key} className="bb-fw-item bb-muted">
            <span>{short(service(what.protocol, what.port))}</span>
            <code className="bb-mono bb-faint">{what.protocol === 'icmp' ? 'icmp 8' : `${what.protocol} ${what.port}`}</code>
          </div>
        ))}
      </div>
      <div className="bb-wf-label">{t('beatbox.net.services.yours')}</div>
      <div className="bb-fw-list">
        {mine.length ? mine.map(([key, what]) => (
          <div key={key} className="bb-fw-item">
            <b>{key}</b>
            <code className="bb-mono bb-muted">{portsText(what.protocol, what.port, what.port_last)}</code>
            <button type="button" className="bb-net-unplug" disabled={busy} aria-label={t('beatbox.net.services.remove', { name: key })}
                    onClick={() => void run('firewall.service', { name: key })}><Icon name="close" size="xs" /></button>
          </div>
        )) : <p className="bb-net-say">{t('beatbox.net.services.none')}</p>}
      </div>
      {adding ? (
        <form className="bb-net-vlanadd" onSubmit={(event) => { event.preventDefault(); if (!busy && name.trim()) void add(); }}>
          <input className="bb-input bb-fw-name" value={name} maxLength={24} placeholder={t('beatbox.net.services.name_ph')}
                 aria-label={t('beatbox.net.services.name_ph')} onChange={(event) => setName(event.target.value)} />
          <Select label={t('beatbox.net.rule.protocol')} value={protocol} onChange={setProtocol} tiny options={['tcp', 'udp', 'sctp'].map((value) => ({ value }))} />
          <input className="bb-input bb-mono bb-fw-otherin" value={ports} placeholder={t('beatbox.net.rule.ports_ph')}
                 aria-label={t('beatbox.net.rule.ports_ph')} onChange={(event) => setPorts(event.target.value)} />
          <Button onClick={() => setAdding(false)}>{t('beatbox.net.groups.cancel')}</Button>
          <Button variant="primary" disabled={busy || !name.trim()} type="submit">{t('beatbox.net.services.add')}</Button>
        </form>
      ) : <button type="button" className="bb-fw-dashed" onClick={() => setAdding(true)}><Icon name="plus" size="sm" />{t('beatbox.net.services.new')}</button>}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </Card>
  );
}

/** Named machine groups, so one rule can cover many. */
function Groups({ net }: { net: Net }) {
  const { busy, told, run } = useFirewallAct(net);
  const [editing, setEditing] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [members, setMembers] = useState<string[]>([]);
  const open = (group: string) => {
    setEditing(group);
    setName(group);
    setMembers(net.groups[group] ?? []);
  };
  async function save() {
    if (await run('firewall.group', { name: name.trim(), members })) setEditing(null);
  }
  return (
    <Card title={<><Icon name="group" size="sm" />{t('beatbox.net.groups.title')}</>} info={t('beatbox.net.groups.what')}>
      <div className="bb-fw-list bb-net-first">
        {Object.entries(net.groups).map(([group, who]) => (
          <div key={group} className="bb-fw-item">
            <button type="button" className="bb-fw-groupname" onClick={() => open(group)}>{group}</button>
            <code className="bb-mono bb-muted">{who.map(net.label).join(', ') || t('beatbox.net.groups.empty')}</code>
            <button type="button" className="bb-net-unplug" disabled={busy} aria-label={t('beatbox.net.groups.remove', { name: group })}
                    onClick={() => void run('firewall.group', { name: group, members: [] })}><Icon name="close" size="xs" /></button>
          </div>
        ))}
      </div>
      {editing !== null ? (
        <form className="bb-fw-groupform" onSubmit={(event) => { event.preventDefault(); if (!busy && name.trim() && members.length) void save(); }}>
          <input className="bb-input" value={name} maxLength={24} disabled={editing !== ''} placeholder={t('beatbox.net.groups.name_ph')}
                 aria-label={t('beatbox.net.groups.name_ph')} onChange={(event) => setName(event.target.value)} />
          <div className="bb-fw-members">
            {net.nodes.map((node) => (
              <label key={node.node} className="bb-fw-check">
                <input type="checkbox" checked={members.includes(node.node)}
                       onChange={(event) => setMembers(event.target.checked ? [...members, node.node] : members.filter((m) => m !== node.node))} />
                <span>{net.label(node.node)}</span>
              </label>
            ))}
          </div>
          <div className="bb-fw-formfoot">
            <Button onClick={() => setEditing(null)}>{t('beatbox.net.groups.cancel')}</Button>
            <Button variant="primary" type="submit" disabled={busy || !name.trim() || !members.length}>{t('beatbox.net.groups.save')}</Button>
          </div>
        </form>
      ) : <button type="button" className="bb-fw-dashed" onClick={() => open('')}><Icon name="plus" size="sm" />{t('beatbox.net.groups.new')}</button>}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </Card>
  );
}

/** Yes or No, and why. */
function Said({ ok, why }: { ok: boolean; why: ReactNode }) {
  return (
    <div className={`bb-net-verdict ${ok ? 'bb-net-verdict--ok' : 'bb-net-verdict--no'}`}>
      <Mark ok={ok} />
      <span><b>{ok ? t('beatbox.net.test.yes') : t('beatbox.net.test.no')}</b><small>{why}</small></span>
    </div>
  );
}

/** Which rule (or the default) decided, as a sentence. */
function byRules(net: Net, verdict: ReturnType<typeof decide>): string {
  return verdict.rule
    ? t(verdict.allowed ? 'beatbox.net.test.by_allow' : verdict.rejected ? 'beatbox.net.test.by_reject' : 'beatbox.net.test.by_deny', { n: net.rules.findIndex((r) => r.id === verdict.rule) + 1 })
    : t(verdict.allowed ? 'beatbox.net.test.default_allow' : 'beatbox.net.test.default_deny');
}

/** Which rules matched and which didn't, in order. */
function Trace({ net, trace }: { net: Net; trace: { from: number; to: number; fits: boolean; first: boolean }[] }) {
  if (!net.rules.length) return null;
  return (
    <div>
      <div className="bb-wf-label">{t('beatbox.net.test.how')}</div>
      {trace.map((step) => (
        <div key={step.from} className={`bb-net-step${step.first ? ' bb-net-step--first' : step.fits ? ' bb-net-step--late' : ''}`}>
          <span className="bb-mono">{step.from === step.to ? step.from + 1 : `${step.from + 1}–${step.to + 1}`}</span>
          <span>{step.fits ? `${verb(net.rules[step.from]!.action)} · ${endWord(net.rules[step.from]!.source)} → ${endWord(net.rules[step.from]!.destination)}`
            : t(step.from === step.to ? 'beatbox.net.test.misses_one' : 'beatbox.net.test.misses')}</span>
          <small>{step.first ? t('beatbox.net.test.decides') : step.fits ? t('beatbox.net.test.late') : ''}</small>
        </div>
      ))}
    </div>
  );
}

/** "Would it get through?": test one connection against the rules. */
function Tester({ net, flows, start = 'machine' }: { net: Net; flows: RawFlow[] | null; start?: 'machine' | 'name' }) {
  const names = net.nodes.map((node) => node.node);
  const [from, setFrom] = useState(names[1] ?? names[0] ?? '');
  const [to, setTo] = useState(names[0] ?? '');
  const [what, setWhat] = useState(start === 'name' ? 'web_secure' : 'remote_desktop');
  // To a machine, or to a name (resolved by the router first, like a real machine would).
  const [mode, setMode] = useState(start);
  const [nameText, setNameText] = useState('');
  const known = names.join('|');
  useEffect(() => {
    if (!from && names[0]) setFrom(names[1] ?? names[0]);
    if (!to && names[0]) setTo(names[0]);
  }, [known]);
  const source = net.nodes.find((node) => node.node === from);
  const chosen = attemptOf(picked(net, what, { protocol: 'tcp', value: '' }));
  const named = mode === 'name' ? nameText.trim() : '';
  const dns = Object.fromEntries(Object.entries(bag(net.facts.dns)).map(([name, address]) => [name, String(address)]));
  const told = named && source ? askName(named, source.address, net.names, { dns, records: net.records, domain: net.domain }, net.here) : null;
  const toldAddress = !told ? null : told.kind === 'found' ? told.address : told.kind === 'sent' ? told.address
    : told.kind === 'outside' ? OUT_THERE : null;
  const destination = mode === 'name' ? (toldAddress ? { node: '', address: toldAddress } : undefined)
    : to === 'internet' && net.internet ? { node: 'internet', address: OUT_THERE } : net.nodes.find((node) => node.node === to);
  const attempt = source && destination && source !== destination
    ? { source: source.address, destination: destination.address, ...chosen } : null;
  const verdict = attempt ? decide(net.rules, attempt, net.defaultAllow, net.here) : null;
  // Name cuts only work where the name is on the wire: TLS SNI, HTTP Host.
  const cut = named && source ? cutBy(named, source.address, net.names, net.here) : null;
  const saysName = what === 'web' || what === 'web_secure';
  const ruleName = (id: string) => t('beatbox.net.names.rule_n', { n: id.slice(1) });
  const suggestions = [...new Set([
    ...net.names.map((rule) => rule.name.replace(/^\*\./, '')), ...Object.keys(dns), ...net.records.map((record) => record.name),
    ...names.map((node) => `${node}.${net.domain}`),
  ])];

  // Non-matching rules fold into runs.
  const trace: { from: number; to: number; fits: boolean; first: boolean }[] = [];
  let decidedAt = -1;
  verdict?.fitting.forEach((fits, i) => {
    const first = fits && decidedAt < 0;
    if (first) decidedAt = i;
    const last = trace[trace.length - 1];
    if (!fits && last && !last.fits) last.to = i;
    else trace.push({ from: i, to: i, fits, first });
  });

  const real = (mode === 'name' ? [] : flows ?? []).filter((flow) => flow.source_node === from && flow.destination_node === to
    && flow.protocol === chosen.protocol && (chosen.protocol === 'icmp' || flow.port === chosen.port));
  const realPackets = real.reduce((sum, flow) => sum + flow.packets, 0);
  const realAllowed = real.some((flow) => flow.action === 'allow');

  return (
    <Card title={<><Icon name="search" size="sm" />{t('beatbox.net.test.title')}</>}>
      {names.length < 2 ? <p className="bb-net-say">{t('beatbox.net.test.two')}</p> : (
        <div className="bb-net-test">
          <label className="bb-net-testrow"><span>{t('beatbox.net.rule.source')}</span>
            <Select label={t('beatbox.net.rule.source')} value={from} onChange={setFrom} options={names.map((value) => ({ value, label: net.label(value) }))} /></label>
          <div className="bb-net-testrow"><span>{t('beatbox.net.rule.destination')}</span>
            <div className="bb-net-testto">
              <span className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.net.rule.destination')}>
                {(['machine', 'name'] as const).map((each) => (
                  <button key={each} type="button" role="radio" aria-checked={mode === each} className={mode === each ? 'on' : ''}
                          onClick={() => setMode(each)}>{t(each === 'machine' ? 'beatbox.net.test.to_machine' : 'beatbox.net.test.to_name')}</button>
                ))}
              </span>
              {mode === 'machine' ? (
                <Select label={t('beatbox.net.rule.destination')} value={to} onChange={setTo}
                        options={[...names.map((value) => ({ value, label: net.label(value) })),
                          ...(net.internet ? [{ value: 'internet', label: t('beatbox.net.end.internet') }] : [])]} />
              ) : (
                <>
                  <input className="bb-input bb-mono" list="bb-net-test-names" value={nameText} placeholder="www.example.com" spellCheck={false}
                         aria-label={t('beatbox.net.test.name')} onChange={(event) => setNameText(event.target.value)} />
                  <datalist id="bb-net-test-names">{suggestions.map((name) => <option key={name} value={name} />)}</datalist>
                </>
              )}
            </div></div>
          <div className="bb-net-testrow"><span>{t('beatbox.net.rule.by_short')}</span>
            <div className="bb-net-builder"><ServicePick net={net} value={what} other={{ protocol: 'tcp', value: '' }} withAny={false} onValue={setWhat} /></div></div>
          {mode === 'name' ? (
            !told ? <p className="bb-net-say">{t('beatbox.net.test.type_name')}</p> : (
              <>
                <div className="bb-wf-label">{t('beatbox.net.test.ask')}</div>
                <p className="bb-net-told">{told.kind === 'hidden' ? t('beatbox.net.test.hidden', { rule: ruleName(told.rule) })
                  : told.kind === 'sent' ? (told.address ? t('beatbox.net.test.sent', { to: net.label(told.to), address: told.address, rule: ruleName(told.rule) })
                    : t('beatbox.net.test.sent_nowhere', { to: told.to, rule: ruleName(told.rule) }))
                  : told.kind === 'found' ? (told.by === 'machine' ? t('beatbox.net.test.found_machine', { machine: net.label(told.machine ?? ''), address: told.address })
                    : t(told.by === 'gateway' ? 'beatbox.net.test.found_gateway' : 'beatbox.net.test.found_record', { address: told.address }))
                  : t(told.kind === 'outside' ? 'beatbox.net.test.outside' : 'beatbox.net.test.unknown')}</p>
                {!toldAddress ? <Said ok={false} why={t('beatbox.net.test.no_address')} /> : (
                  <>
                    <div className="bb-wf-label">{t('beatbox.net.test.connect')}</div>
                    {cut && saysName ? <Said ok={false} why={t('beatbox.net.test.cut', { rule: ruleName(cut) })} /> : verdict ? (
                      <>
                        <Said ok={verdict.allowed} why={byRules(net, verdict)} />
                        {cut ? <p className="bb-net-say">{t('beatbox.net.test.cut_unsaid', { rule: ruleName(cut) })}</p> : null}
                        <Trace net={net} trace={trace} />
                      </>
                    ) : <p className="bb-net-say">{t('beatbox.net.test.same_machine')}</p>}
                  </>
                )}
              </>
            )
          ) : verdict ? (
            <>
              <Said ok={verdict.allowed} why={byRules(net, verdict)} />
              <Trace net={net} trace={trace} />
              <p className="bb-net-say">{realPackets
                ? t(realAllowed ? 'beatbox.net.test.real_through' : 'beatbox.net.test.real_stopped', { count: count(realPackets) })
                : t('beatbox.net.test.real_none')}</p>
            </>
          ) : <p className="bb-net-say">{t('beatbox.net.test.pick')}</p>}
        </div>
      )}
    </Card>
  );
}

/* ── Flows ────────────────────────────────────────────────────────────── */

/** Talks shown before "show more". */
const TALKS_SHOWN = 6;
/** Stable key, so the selection survives a refresh. */
const talkKey = (talk: Talk) => [talk.from, talk.to, talk.service.protocol, talk.service.port, talk.allowed, talk.rule].join('|');

/** Translated sentence with `{name}` slots filled by React nodes. */
function rich(template: string, parts: Record<string, ReactNode>): ReactNode {
  return template.split(/\{(\w+)\}/g).map((piece, i) => (i % 2 ? <Fragment key={i}>{parts[piece] ?? piece}</Fragment> : piece));
}

/** Services with their own "did" sentence ("asked for a page"). */
const DID = new Set(['ssh', 'web', 'web_secure', 'ping', 'file_sharing', 'remote_desktop', 'dns', 'ftp', 'mail', 'database', 'vnc', 'time', 'telnet']);
function didWhat(talk: Talk): string {
  return DID.has(talk.service.key) ? runtimeText(`beatbox.net.did.${talk.service.key}`)
    : t('beatbox.net.did.other', { what: `${talk.service.protocol} ${talk.service.port}` });
}

/**
 * How a connection ended. Reached the machine: `went`, `said_no` (reset),
 * `silent`. Stopped: `blocked` (dropped), `refused` (rejected), `closed`
 * (hidden network), `cut` (name rule).
 */
type Ending = 'went' | 'said_no' | 'silent' | 'blocked' | 'refused' | 'closed' | 'cut';
const ending = (talk: Talk): Ending =>
  (talk.path?.nat?.kind === 'closed' ? 'closed'
    : !talk.allowed ? (talk.rule?.startsWith('n') ? 'cut' : talk.path?.verdict === 'reject' ? 'refused' : 'blocked')
    : talk.answered ? 'went' : talk.reset ? 'said_no' : 'silent');
/** `closed` reads as blocked. */
const endKey = (end: Ending) => (end === 'closed' ? 'blocked' : end);
/** Reached the machine, whatever it answered. */
const reached = (end: Ending) => end === 'went' || end === 'said_no' || end === 'silent';

/** now, 10 s, 2 min, 1 h. */
function since(quiet: number): string {
  const s = Math.round((Number.isFinite(quiet) ? quiet : 0) / 1000);
  return s < 5 ? t('beatbox.net.ago.now') : s < 60 ? t('beatbox.net.ago.s', { n: s })
    : s < 3600 ? t('beatbox.net.ago.min', { n: Math.round(s / 60) }) : t('beatbox.net.ago.h', { n: Math.round(s / 3600) });
}

export function NetworkFlows() {
  const net = useNetwork();
  const { flows, lookups } = useFlows(net.network?.id ?? '', net.running);
  const params = useRouteParams();
  const story = tell(flows ?? []);
  const names = net.nodes.map((node) => node.node);
  const outside = lookups.filter((lookup) => !lookup.answer && !lookup.rule).length;
  const asks = lookups.reduce((sum, lookup) => sum + lookup.count, 0);
  const byNode = [...new Set(lookups.map((lookup) => lookup.node))].map((node) => {
    const mine = lookups.filter((lookup) => lookup.node === node).sort((a, b) => b.count - a.count);
    return { node, mine, total: mine.reduce((sum, lookup) => sum + lookup.count, 0) };
  }).sort((a, b) => b.total - a.total);
  const top = [...lookups].sort((a, b) => b.count - a.count)[0];
  // Newest first (from a light machine's "why", its latest stopped one).
  // A limited rule's connections between the same pair and service fold
  // into one row: passed vs cut.
  const pace = new Map<string, Pace>();
  const talks: Talk[] = [];
  for (const talk of [...story.talks].sort((a, b) => a.quiet - b.quiet)) {
    const rule = net.rules.find((r) => r.id === talk.rule);
    if (!rule?.limit) {
      talks.push(talk);
      continue;
    }
    const key = [talk.from, talk.to, talk.service.protocol, talk.service.port, talk.rule].join('|');
    const tally = pace.get(key);
    if (!tally) talks.push(talk);
    const now = tally ?? { passed: 0, cut: 0, limit: rule.limit, rule: rule.id };
    if (talk.allowed) now.passed += talk.count;
    else now.cut += talk.count;
    pace.set(key, now);
  }
  const paceOf = (talk: Talk) => pace.get([talk.from, talk.to, talk.service.protocol, talk.service.port, talk.rule].join('|'));
  const [picked, setPicked] = useState('');
  const [noise, setNoise] = useState(false);
  const [allTalks, setAllTalks] = useState(false);
  const from = params.get('from');
  const chosen = talks.find((talk) => talkKey(talk) === picked)
    ?? (from ? talks.find((talk) => talk.from === from && !talk.allowed) : undefined)
    ?? talks[0];

  return (
    <Page page="flows" net={net} info={t('beatbox.net.flows.lede')}>
      {net.network ? (
        flows === null ? <p className="bb-net-say">{t(net.running ? 'beatbox.net.flows.asking' : 'beatbox.net.flows.off')}</p> : (
          <>
            <div className="bb-net-grid bb-router-grid bb-flow-grid">
              <Card title={<><Icon name="logs" size="sm" />{t('beatbox.net.flows.talks')}</>}>
                <div className="bb-flow-list bb-net-first">
                  {talks.length ? talks.filter((talk, i) => allTalks || i < TALKS_SHOWN || talk === chosen).map((talk) => {
                    const end = ending(talk);
                    const tone = end === 'went' ? 'mint' : reached(end) ? 'amber' : 'rose';
                    return (
                      <button key={talkKey(talk)} type="button" aria-pressed={talk === chosen}
                              className={`bb-flow-talk${talk === chosen ? ' on' : ''}`} onClick={() => setPicked(talkKey(talk))}>
                        <i className={`bb-net-dot bb-net-dot--${tone}`} />
                        <span className="bb-flow-did"><b>{net.label(talk.from)}</b> <span className="bb-muted">{didWhat(talk)}</span></span>
                        <span className="bb-flow-to">{rich(t('beatbox.net.flows.to_whom'), { to: <span>{net.label(talk.to)}</span> })}</span>
                        <small>{paceOf(talk)?.cut ? <><span className="bb-mint">{t('beatbox.net.limit.passed', { count: paceOf(talk)!.passed })}</span> · <span className="bb-rose">{t('beatbox.net.limit.cut', { count: paceOf(talk)!.cut })}</span></>
                          : <span className={`bb-${tone}`}>{t(`beatbox.net.flows.end.${end === 'closed' ? 'blocked' : end}`)}</span>} <span className="bb-faint">· {since(talk.quiet)}</span></small>
                      </button>
                    );
                  }) : <p className="bb-net-say">{t('beatbox.net.story.quiet')}</p>}
                </div>
                {talks.length > TALKS_SHOWN ? (
                  <button type="button" className="bb-net-link bb-flow-more" aria-expanded={allTalks} onClick={() => setAllTalks(!allTalks)}>
                    {allTalks ? t('beatbox.net.flows.fewer') : t('beatbox.net.flows.more', { count: talks.length - TALKS_SHOWN })}
                  </button>
                ) : null}
                {story.noisePackets ? (
                  <div className="bb-flow-noise">
                    <div className="bb-flow-noisehead">
                      <i className="bb-net-dot bb-net-dot--faint" />
                      <span>{t('beatbox.net.flows.noise_title')} <Info text={t('beatbox.net.flows.noise_what')} /> <span className="bb-faint">· {t('beatbox.net.flows.packets', { count: count(story.noisePackets) })}</span></span>
                      <button type="button" className="bb-net-link" aria-expanded={noise} onClick={() => setNoise(!noise)}>
                        {t(noise ? 'beatbox.net.flows.noise_hide' : 'beatbox.net.flows.noise_show')}</button>
                    </div>
                    {noise ? story.noise.map((heard, i) => (
                      <p key={i} className="bb-flow-noiseline">
                        <b>{net.label(heard.from)}</b> <span className="bb-muted">· {short(heard.service)} · {count(heard.packets)}</span>
                      </p>
                    )) : null}
                  </div>
                ) : null}
              </Card>
              <div className="bb-net-stack">
                {chosen ? <TalkWay key={talkKey(chosen)} net={net} talk={chosen} pace={paceOf(chosen)} /> : null}
                <Captures net={net} />
              </div>
            </div>
            <div className="bb-net-grid">
              <div className="bb-net-stack">
                <Card title={<><Icon name="network" size="sm" />{t('beatbox.net.flows.between')}</>}>
                  {names.length ? <Between net={net} names={names} story={story} onWay={(talk) => { setPicked(talkKey(talk)); window.scrollTo?.({ top: 0 }); }} />
                    : <p className="bb-net-say">{t('beatbox.net.nodes.none')}</p>}
                </Card>
              </div>
              <Card title={<><Icon name="search" size="sm" />{t('beatbox.net.lookups.title')}</>} info={outside ? t('beatbox.net.lookups.what_short') : undefined}
                    note={t('beatbox.net.lookups.counted', { names: count(lookups.length), asks: count(asks) })}>
                {top && asks && top.count / asks >= 0.5 ? (
                  <p className="bb-net-insight">{t('beatbox.net.lookups.most', {
                    percent: Math.round((top.count / asks) * 100), node: net.label(top.node), name: top.name,
                  })}</p>
                ) : null}
                {byNode.length ? byNode.map(({ node, mine }) => (
                  <div key={node} className="bb-net-asker">
                    <div className="bb-net-askerhead"><b>{net.label(node)}</b><small>{t('beatbox.net.lookups.n_names', { count: mine.length })}</small></div>
                    {mine.slice(0, NAMES_SHOWN).map((lookup) => <Ask key={lookup.name} lookup={lookup} most={mine[0]!.count} />)}
                    {mine.length > NAMES_SHOWN ? (
                      <details className="bb-net-askmore">
                        <summary>{t('beatbox.net.lookups.more', { count: mine.length - NAMES_SHOWN })}</summary>
                        {mine.slice(NAMES_SHOWN).map((lookup) => <Ask key={lookup.name} lookup={lookup} most={mine[0]!.count} />)}
                      </details>
                    ) : null}
                  </div>
                )) : <p className="bb-net-say">{t('beatbox.net.lookups.none')}</p>}
              </Card>
            </div>
          </>
        )
      ) : null}
    </Page>
  );
}

/** Light machine kind icon, or the VM box. */
function machineIcon(net: Net, name: string): IconName {
  const node = net.nodes.find((n) => n.node === name);
  return node?.light ? (node.light.kind === 'server' ? 'server' : 'terminal') : 'box';
}

/** One stop on a connection's path. */
function Stop({ icon, name, note, tone }: { icon: IconName; name: string; note?: string; tone?: 'stop' | 'ghost' | 'nat' | 'quiet' }) {
  return (
    <div className={`bb-trip-stop${tone ? ` bb-trip-stop--${tone}` : ''}`}>
      <span className="bb-trip-dot"><Icon name={icon} size="md" /></span>
      <b>{name}</b>
      {note ? <small>{note}</small> : null}
    </div>
  );
}

/** A limited rule's connections between two machines: passed vs cut. */
interface Pace { passed: number; cut: number; limit: NonNullable<Rule['limit']>; rule: string }

/** One connection: how it ended, its path as stops, technical detail folded. */
function TalkWay({ net, talk, pace }: { net: Net; talk: Talk; pace?: Pace }) {
  const path = talk.path;
  const { busy, told, run } = useFirewallAct(net);
  const [details, setDetails] = useState(false);
  const end = ending(talk);
  const ruleN = path?.rule ? path.rule.replace(/^r/, '') : '';
  const rule = net.rules.find((r) => r.id === path?.rule);
  const byName = net.names.find((n) => n.id === path?.rule);
  const from = <b>{net.label(talk.from)}</b>;
  const to = <b>{net.label(talk.to)}</b>;
  const thing = t(talk.service.key === 'ping' ? 'beatbox.net.way.ping' : 'beatbox.net.way.request');
  const ms = path?.reached && path.ms >= 1 ? Math.round(path.ms) : 0;
  const headline = reached(end)
    ? rich(t(ms ? 'beatbox.net.way.reached_ms' : 'beatbox.net.way.reached'), { thing, from, to, ms: <span className="bb-mint">{t('beatbox.net.path.ms', { ms })}</span> })
    : rich(t('beatbox.net.way.stopped'), { thing, from, to, how: <span className="bb-rose">{t(end === 'cut' ? 'beatbox.net.way.cut' : end === 'refused' ? 'beatbox.net.way.refused' : 'beatbox.net.way.blocked')}</span> });
  const closedAt = end === 'closed' ? path?.nat?.router : undefined;
  const nat = path?.nat ?? null;
  const subline = end === 'said_no' ? t('beatbox.net.way.said_no_what', { to: net.label(talk.to), port: talk.service.port })
    : end === 'silent' ? t('beatbox.net.way.silent_what', { to: net.label(talk.to) })
    : !path ? t('beatbox.net.way.no_router')
    : closedAt ? t('beatbox.net.way.closed_what', { router: closedAt, to: net.label(talk.to) })
    : nat?.kind === 'hide' && end === 'went' ? t('beatbox.net.way.hidden_what', { from: net.label(talk.from), to: net.label(talk.to), router: nat.router })
    : nat?.kind === 'forward' && end === 'went' ? t('beatbox.net.way.forward_what', { router: nat.router, port: nat.from.split(':')[1] ?? '', to: net.label(talk.to), to_port: nat.to.split(':')[1] ?? '' })
    : talk.source.includes(':') && end === 'went' ? t('beatbox.net.ipv6.no_nat')
    : end === 'cut' && byName ? t('beatbox.net.way.cut_what', { name: byName.name, to: net.label(talk.to) })
    : end === 'refused' ? t('beatbox.net.way.refused_what', { to: net.label(talk.to) })
    : end === 'blocked' ? t('beatbox.net.way.blocked_what', { to: net.label(talk.to) })
    : path.reached === 'internet' ? t('beatbox.net.path.internet') : '';
  // Routers crossed, in order, once each. Switches are detail.
  const routers = [...new Set((path?.hops ?? []).filter((hop) => hop.kind === 'router').map((hop) => hop.device))];
  const words: ReactNode[] = [];
  if (path) {
    const routerOut = (i: number) => path.hops[i + 1]?.vlan;
    path.hops.forEach((hop, i) => {
      const out = routerOut(i);
      words.push(hop.kind === 'switch'
        ? t('beatbox.net.path.switch', { device: hop.device, port: hop.port, vlan: hop.vlan })
        : out !== undefined && out !== hop.vlan
          ? `${t('beatbox.net.path.routed', { device: hop.device, vlan: hop.vlan, out })}${hop.ttl !== null ? ` TTL ${hop.ttl} → ${hop.ttl - 1}.` : ''}`
          : t('beatbox.net.path.router', { device: hop.device, vlan: hop.vlan }));
    });
  }
  return (
    <Card>
      {pace?.cut ? <>
        <h2 className="bb-trip-head">{rich(t('beatbox.net.limit.tried'), { from, what: short(talk.service), to, n: pace.passed + pace.cut })}</h2>
        <p className="bb-trip-sub">{rich(t('beatbox.net.limit.result'), {
          passed: <b className="bb-mint">{t('beatbox.net.limit.passed', { count: pace.passed })}</b>,
          cut: <b className="bb-rose">{t('beatbox.net.limit.were_cut', { count: pace.cut })}</b>,
          n: pace.rule.replace(/^r/, ''),
        })}</p>
        <div className="bb-trip-ticks" aria-hidden="true">
          {Array.from({ length: Math.min(pace.passed + pace.cut, 100) }, (_, i) => (
            <i key={i} className={i < pace.passed ? 'went' : 'cut'} />
          ))}
        </div>
        <div className="bb-trip-pace">
          <span>{rich(t('beatbox.net.way.rule'), { n: pace.rule.replace(/^r/, ''), action: <span className="bb-mint">{verb('allow')}</span> })} · {t('beatbox.net.limit.up_to', {
            count: pace.limit.count, per: runtimeText(`beatbox.net.limit.per.${pace.limit.per}`) })}</span>
          <small>{t(pace.limit.each ? 'beatbox.net.limit.each_what' : 'beatbox.net.limit.all_what')}</small>
        </div>
      </> : <>
        <h2 className="bb-trip-head">{headline}</h2>
        {subline ? <p className="bb-trip-sub">{subline}</p> : null}
      </>}
      {path ? (
        <div className={`bb-trip bb-trip--${end}`}>
          <Stop icon={machineIcon(net, talk.from)} name={net.label(talk.from)} />
          {routers.map((router) => (
            <Fragment key={router}>
              <span className="bb-trip-line" />
              <Stop icon="router" name={router}
                    note={t(router === closedAt ? 'beatbox.net.way.stopped_here' : nat?.router === router && nat.kind !== 'closed' ? 'beatbox.net.way.changes_here' : 'beatbox.net.way.the_router')}
                    tone={router === closedAt ? 'stop' : nat?.router === router && nat.kind !== 'closed' ? 'nat' : undefined} />
            </Fragment>
          ))}
          {end === 'blocked' || end === 'refused' || end === 'cut' ? (
            <>
              <span className="bb-trip-line" />
              <Stop icon="shield" name={t('beatbox.net.path.firewall')} note={t('beatbox.net.way.stopped_here')} tone="stop" />
            </>
          ) : null}
          <span className={`bb-trip-line${reached(end) ? '' : ' bb-trip-line--broken'}`}>
            {ms ? <small>{t('beatbox.net.path.ms', { ms })}</small> : null}
          </span>
          {path.reached === 'internet' ? <Stop icon="globe" name={t('beatbox.net.end.internet')} />
            : <Stop icon={machineIcon(net, talk.to)} name={net.label(talk.to)}
                    note={end === 'went' ? undefined : reached(end) ? t(`beatbox.net.way.${end}`) : t('beatbox.net.path.never')}
                    tone={end === 'went' ? undefined : reached(end) ? 'quiet' : 'ghost'} />}
        </div>
      ) : null}
      {talk.source.includes(':') ? (
        <div className="bb-trip-nat">
          <span className="bb-trip-natchip">IPv6</span><code className="bb-mono">{talk.source}</code><span>→</span><code className="bb-mono">{talk.destination}</code>
        </div>
      ) : null}
      {nat && nat.kind !== 'closed' ? (
        <div className="bb-trip-nat">
          {nat.kind === 'hide' ? <><code className="bb-mono">{nat.from.split(':')[0]}</code><span>→</span><code className="bb-mono bb-trip-natto">{nat.to.split(':')[0]}</code><small>{t('beatbox.net.way.at_router', { router: nat.router })}</small></>
            : <><span className="bb-trip-natchip">{t('beatbox.net.way.forward')}</span><small>{nat.router} · {t('beatbox.net.nat.port', { port: nat.from.split(':')[1] ?? '' })} → {net.label(talk.to)}:{nat.to.split(':')[1]}</small></>}
        </div>
      ) : null}
      {(end === 'blocked' || end === 'refused' || end === 'cut') && !pace?.cut ? (
        <div className="bb-trip-rule">
          {rule ? <><span>{rich(t('beatbox.net.way.rule'), { n: ruleN, action: <span className="bb-rose">{verb(rule.action)}</span> })}</span>
            <small>{ruleWhat(rule).words || ruleWhat(rule).code} · {endWord(rule.source)} → {endWord(rule.destination)}</small></>
            : byName ? <><span>{t('beatbox.net.names.rule_n', { n: byName.id.slice(1) })} · <span className="bb-rose">{t(`beatbox.net.names.action.${byName.action}`)}</span></span>
              <small><code className="bb-mono">{byName.name}</code> · {endWord(byName.source)}</small></>
            : <><span>{t('beatbox.net.way.by_default')}</span><small>{t('beatbox.net.way.by_default_what')}</small></>}
        </div>
      ) : null}
      <div className="bb-trip-actions">
        {rule ? <a className="bb-btn bb-btn--ghost bb-btn--sm" href={href('network/firewall')}>{t('beatbox.net.path.open_rule', { n: ruleN })}</a> : null}
        <Button size="sm" icon="pulse" disabled={busy}
                onClick={() => void run('capture.start', {
                  node: talk.from, source: talk.source, destination: talk.destination,
                  protocol: talk.service.protocol, port: talk.service.protocol === 'icmp' ? 0 : talk.service.port,
                })}>{t('beatbox.net.path.capture')}</Button>
        {path ? <button type="button" className="bb-net-link bb-trip-more" aria-expanded={details} onClick={() => setDetails(!details)}>
          {t('beatbox.net.way.details')} <Icon name={details ? 'up' : 'arrow'} size="xs" /></button> : null}
      </div>
      {details && path ? (
        <div className="bb-trip-details">
          <p className="bb-net-say">{talk.source} → {talk.destination} · {talk.service.protocol}{talk.service.protocol === 'icmp' ? '' : ` ${talk.service.port}`}</p>
          <ol className="bb-path-list">{words.map((said, i) => <li key={i}>{said}</li>)}</ol>
        </div>
      ) : null}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </Card>
  );
}

/** One `facts.captures` row. */
interface CaptureRow {
  id: string; node: string | null; a: string | null; b: string | null; talk: boolean;
  bytes: number; frames: number; started_ms: number; ended_ms: number | null; full: boolean; live: boolean;
}

const capturesOf = (net: Net) => rows(net.facts.captures) as unknown as CaptureRow[];
const megabytes = (bytes: number) => (bytes < 1024 * 1024 ? `${Math.max(1, Math.round(bytes / 1024))} kB` : `${(bytes / 1024 / 1024).toFixed(1)} MB`);
const lasted = (capture: CaptureRow) => {
  const s = Math.max(0, Math.round(((capture.ended_ms ?? Date.now()) - capture.started_ms) / 1000));
  return `${String(Math.floor(s / 60)).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}`;
};
const CAPTURE_MAX = 16 * 1024 * 1024;

/** Saved captures; one click saves a .pcap. */
function Captures({ net }: { net: Net }) {
  const { busy, told, run } = useFirewallAct(net);
  const [saved, setSaved] = useState('');
  const captures = capturesOf(net).slice().reverse();
  if (!captures.length) return null;
  async function save(id: string) {
    setSaved('');
    try {
      const done = await act(net.network!.id, 'capture.save', { id });
      setSaved(t('beatbox.net.capture.saved', { path: String(done.path) }));
    } catch (error) {
      setSaved(fault(error));
    }
  }
  return (
    <Card title={<><Icon name="pulse" size="sm" />{t('beatbox.net.capture.title')}<span className="bb-faint"> · {captures.length}</span></>}>
      <div className="bb-flow-captures bb-net-first">
        {captures.map((c) => (
          <div key={c.id} className="bb-flow-capture">
            <span>
              <b>{c.node ? net.label(c.node) : `${c.a} ↔ ${c.b}`}</b>
              <small>
                {c.live ? <span className="bb-rose"><i className="bb-net-dot bb-net-dot--rose" />{t('beatbox.net.capture.recording')} · </span> : null}
                {c.talk ? `${t('beatbox.net.capture.one_talk')} · ` : ''}{lasted(c)} · <span className={c.full ? 'bb-amber' : ''}>{megabytes(c.bytes)}{c.full ? ` · ${t('beatbox.net.capture.full')}` : ''}</span>
              </small>
            </span>
            {c.live ? <Button size="sm" disabled={busy} onClick={() => void run('capture.stop', { id: c.id })}>{t('beatbox.net.capture.stop')}</Button>
              : <Button size="sm" icon="save" onClick={() => void save(c.id)}>{t('beatbox.net.capture.save')}</Button>}
            <button type="button" className="bb-net-unplug" disabled={busy} aria-label={t('beatbox.net.capture.remove')}
                    onClick={() => void run('capture.remove', { id: c.id })}><Icon name="close" size="xs" /></button>
          </div>
        ))}
      </div>
      {saved ? <p className="bb-net-say bb-flow-saved">{saved}</p> : null}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </Card>
  );
}

/** Capture one cable from the editor: a machine's (by name) or a port's. */
function CaptureControl({ net, cable }: { net: Net; cable: string }) {
  const { busy, told, run } = useFirewallAct(net);
  const [keep, setKeep] = useState('all');
  const [saved, setSaved] = useState('');
  const mine = capturesOf(net).filter((c) => c.node === cable || c.a === cable || c.b === cable);
  const live = mine.find((c) => c.live);
  const isNode = net.nodes.some((n) => n.node === cable);
  async function stopAndSave(id: string) {
    if (!(await run('capture.stop', { id }))) return;
    try {
      const done = await act(net.network!.id, 'capture.save', { id });
      setSaved(t('beatbox.net.capture.saved', { path: String(done.path) }));
    } catch (error) {
      setSaved(fault(error));
    }
  }
  return (
    <div className="bb-capture">
      <div className="bb-capture-head"><Icon name="pulse" size="sm" /><b>{t('beatbox.net.capture.this')}</b>
        <Info text={t(isNode ? 'beatbox.net.capture.what_node' : 'beatbox.net.capture.what_cable')} /></div>
      {live ? (
        <>
          <p className="bb-capture-live"><i className="bb-net-dot bb-net-dot--rose" />{t('beatbox.net.capture.recording')}
            <span className="bb-muted"> · {lasted(live)} · {t('beatbox.net.capture.frames', { count: count(live.frames) })}</span></p>
          <div className="bb-capture-bar"><i style={{ width: `${Math.min(100, (live.bytes / CAPTURE_MAX) * 100)}%` }} /></div>
          <p className="bb-net-say">{t('beatbox.net.capture.size', { now: megabytes(live.bytes), most: '16 MB' })}</p>
          <div className="bb-capture-buttons">
            <Button size="sm" variant="primary" disabled={busy} onClick={() => void stopAndSave(live.id)}>{t('beatbox.net.capture.stop_save')}</Button>
            <Button size="sm" disabled={busy} onClick={() => void run('capture.remove', { id: live.id })}>{t('beatbox.net.capture.discard')}</Button>
          </div>
        </>
      ) : (
        <>
          <span className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.net.capture.keep')}>
            {(['all', 'ip', 'quiet'] as const).map((k) => (
              <button key={k} type="button" role="radio" aria-checked={keep === k} className={keep === k ? 'on' : ''}
                      onClick={() => setKeep(k)}>{runtimeText(`beatbox.net.capture.keep.${k}`)}</button>
            ))}
          </span>
          <div className="bb-capture-buttons">
            <Button size="sm" variant="primary" icon="pulse" disabled={busy}
                    onClick={() => void run('capture.start', { ...(isNode ? { node: cable } : { cable }), keep })}>{t('beatbox.net.capture.start')}</Button>
            <small className="bb-faint">{t('beatbox.net.capture.limit')}</small>
          </div>
        </>
      )}
      {saved ? <p className="bb-net-say">{saved}</p> : null}
      {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
    </div>
  );
}

/** Past this many machines the matrix groups by network. */
const MATRIX_MAX = 12;

/**
 * Who talked to whom. Only talkers get rows (the rest in one line); a cell is
 * a dot and a word. Click for a sentence, double-click for details. Past
 * MATRIX_MAX, networks first; a cell opens its two networks' machines.
 */
function Between({ net, names, story, onWay }: { net: Net; names: string[]; story: ReturnType<typeof tell>; onWay: (talk: Talk) => void }) {
  const [all, setAll] = useState(false);
  const [pair, setPair] = useState<[number, number] | null>(null);
  const [picked, setPicked] = useState<{ from: string; to: string } | null>(null);
  const [open, setOpen] = useState(false);
  const topo = topoOf(net);
  const vlan = (node: string) => {
    const at = net.nodes.find((n) => n.node === node)?.at;
    return at ? vlanOf(modeOf(topo, at)) : 1;
  };
  const talked = new Set(story.talks.flatMap((talk) => [talk.from, talk.to]));
  const active = names.filter((name) => talked.has(name));
  const grouped = !all && !pair && active.length > MATRIX_MAX;
  const shown = all ? names : pair ? active.filter((name) => pair.includes(vlan(name))) : active;
  const between = (from: string, to: string) => story.talks.filter((talk) => talk.from === from && talk.to === to);
  const talks = picked ? between(picked.from, picked.to) : [];
  const lead = talks[0];
  const vlans = [...new Set(active.map(vlan))].sort((a, b) => a - b);
  const quiet = names.length - active.length;
  const tone = (list: Talk[]) => (list.every((talk) => talk.allowed) ? 'ok' : list.every((talk) => !talk.allowed) ? 'no' : 'mixed');

  return (
    <>
      <div className="bb-between-bar">
        {pair ? <button type="button" className="bb-net-link" onClick={() => { setPair(null); setPicked(null); }}>{t('beatbox.net.between.back')}</button> : <span />}
        <span className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.net.flows.between')}>
          {[false, true].map((every) => (
            <button key={String(every)} type="button" role="radio" aria-checked={all === every} className={all === every ? 'on' : ''} onClick={() => setAll(every)}>
              {every ? t('beatbox.net.between.all', { count: names.length }) : t('beatbox.net.between.talked')}</button>
          ))}
        </span>
      </div>
      {!shown.length && !grouped ? <p className="bb-net-say">{t('beatbox.net.between.nobody')}</p> : grouped ? (
        <div className="bb-between-grid" style={{ gridTemplateColumns: `140px repeat(${vlans.length}, minmax(96px, 1fr))` }}>
          <span />
          {vlans.map((v) => <span key={v} className="bb-between-head" style={{ color: vlanColour(v) }}>VLAN {v}</span>)}
          {vlans.map((from) => (
            <Fragment key={from}>
              <span className="bb-between-side" style={{ color: vlanColour(from) }}>{vlanNames[from] ?? `VLAN ${from}`}</span>
              {vlans.map((to) => {
                const list = story.talks.filter((talk) => vlan(talk.from) === from && vlan(talk.to) === to);
                if (!list.length) return <span key={to} className="bb-between-cell bb-between-cell--none">{from === to ? '—' : ''}</span>;
                const blocked = list.filter((talk) => !talk.allowed).length;
                return (
                  <button key={to} type="button" className={`bb-between-cell bb-between-cell--${tone(list)}`} onClick={() => setPair([from, to])}>
                    {t('beatbox.net.between.conversations', { count: list.length })}
                    {blocked ? <small className="bb-rose">{t('beatbox.net.between.blocked', { count: blocked })}</small> : null}
                  </button>
                );
              })}
            </Fragment>
          ))}
        </div>
      ) : (
        <div className="bb-between-grid" style={{ gridTemplateColumns: `140px repeat(${shown.length}, 64px)` }}>
          <span />
          {shown.map((name) => <span key={name} className="bb-between-head" title={net.label(name)}>{net.label(name)}</span>)}
          {shown.map((from) => (
            <Fragment key={from}>
              <span className="bb-between-side"><span className="bb-faint">{t('beatbox.net.between.of')}</span> {net.label(from)}</span>
              {shown.map((to) => {
                if (to === from) return <span key={to} className="bb-between-cell bb-between-cell--none">—</span>;
                const list = between(from, to);
                if (!list.length) return <span key={to} className="bb-between-cell bb-between-cell--none" />;
                const on = picked?.from === from && picked.to === to;
                return (
                  <button key={to} type="button" aria-pressed={on} className={`bb-between-cell bb-between-cell--${tone(list)}${on ? ' on' : ''}`}
                          title={list.map(namedTo).join(', ')}
                          onClick={() => setPicked(on ? null : { from, to })} onDoubleClick={() => { setPicked({ from, to }); setOpen(true); }}>
                    <i className={`bb-net-dot bb-net-dot--${tone(list) === 'ok' ? 'mint' : tone(list) === 'no' ? 'rose' : 'amber'}`} />
                    <span>{short(list[0]!.service)}{list.length > 1 ? ` +${list.length - 1}` : ''}</span>
                  </button>
                );
              })}
            </Fragment>
          ))}
        </div>
      )}
      {quiet > 0 && !all && !grouped ? (
        <p className="bb-between-quiet"><i className="bb-net-dot bb-net-dot--faint" />{t(quiet === 1 ? 'beatbox.net.between.quiet_one' : 'beatbox.net.between.quiet', { count: quiet })}</p>
      ) : null}
      {lead && picked ? (
        <div className="bb-between-pick">
          <p><b>{net.label(picked.from)}</b> <span className="bb-muted">{didWhat(lead)}</span> {rich(t('beatbox.net.flows.to_whom'), { to: <b>{net.label(picked.to)}</b> })}.</p>
          <p className="bb-net-say">{talkSaid(lead)} · <ByRule talk={lead} />{talks.length > 1 ? ` · ${t('beatbox.net.between.and_more', { count: talks.length - 1 })}` : ''}</p>
          <div className="bb-net-actions">
            <Button size="sm" onClick={() => onWay(lead)}>{t('beatbox.net.between.way')}</Button>
            <button type="button" className="bb-net-link" style={{ marginTop: 0 }} onClick={() => setOpen(true)}>{t('beatbox.net.between.details')}</button>
          </div>
        </div>
      ) : null}
      {open && picked ? (
        <Modal title={`${net.label(picked.from)} → ${net.label(picked.to)}`} icon="logs" width={560} onClose={() => setOpen(false)}>
          <div className="bb-between-detail">
            {talks.map((talk, i) => (
              <dl key={i}>
                <dt>{t('beatbox.net.between.d_what')}</dt><dd>{namedTo(talk)} <code className="bb-mono bb-faint">{talk.service.protocol} {talk.service.port}</code></dd>
                <dt>{t('beatbox.net.between.d_ended')}</dt><dd className={talk.allowed ? 'bb-mint' : 'bb-rose'}>{t(`beatbox.net.flows.end.${endKey(ending(talk))}`)}</dd>
                <dt>{t('beatbox.net.between.d_packets')}</dt><dd>{talkSaid(talk)}</dd>
                <dt>{t('beatbox.net.between.d_rule')}</dt><dd><ByRule talk={talk} /></dd>
                <dt>{t('beatbox.net.between.d_count')}</dt><dd>{count(talk.count)} · {since(talk.quiet)}</dd>
                <dt>{t('beatbox.net.between.d_addresses')}</dt><dd><code className="bb-mono">{talk.source} → {talk.destination}</code></dd>
                {talk.path ? <><dt>{t('beatbox.net.between.d_way')}</dt><dd>{[...new Set(talk.path.hops.map((hop) => hop.device))].join(' → ')}</dd></> : null}
              </dl>
            ))}
            <div className="bb-net-actions"><Button size="sm" onClick={() => { setOpen(false); if (lead) onWay(lead); }}>{t('beatbox.net.between.way')}</Button></div>
          </div>
        </Modal>
      ) : null}
    </>
  );
}

function Ask({ lookup, most }: { lookup: Lookup; most: number }) {
  const n = lookup.rule?.replace(/^n/, '');
  return (
    <div className="bb-net-ask">
      <code className="bb-mono">{lookup.name}</code>
      {n ? <span className={lookup.answer ? 'bb-violet' : 'bb-rose'}>
        {lookup.answer ? `→ ${lookup.answer} · ${t('beatbox.net.names.rule_n', { n })}` : t('beatbox.net.names.hidden_by', { n })}</span>
        : <span className="bb-mono bb-muted">{lookup.answer ? `→ ${lookup.answer}` : count(lookup.count)}</span>}
      <span className="bb-net-askbar"><i style={{ width: `${Math.max(1, (lookup.count / most) * 100)}%` }} /></span>
    </div>
  );
}

/* ── Traffic ──────────────────────────────────────────────────────────── */

/** Bandwidth steps in kbps; 0 = unlimited. */
const WIDTHS = [1000, 2000, 5000, 10000, 20000, 50000, 100000, 1000000, 10000000, 0];
const widthStep = (kbps: number) => {
  if (kbps <= 0) return WIDTHS.length - 1;
  let best = 0;
  WIDTHS.forEach((step, i) => { if (step > 0 && Math.abs(step - kbps) < Math.abs(WIDTHS[best]! - kbps)) best = i; });
  return best;
};

export function NetworkTraffic() {
  const net = useNetwork();
  const now = cableOfFacts(net.facts);
  const [draft, setDraft] = useState<Cable | null>(null);
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState<{ ok: boolean; text: string } | null>(null);
  const shown = draft ?? now;
  const preset = presetOf(shown);
  const felt = feel(shown);
  const counters = bag(net.facts.counters);

  useEffect(() => setDraft(null), [net.network?.id]);

  async function apply() {
    if (!net.network || !draft) return;
    setBusy(true);
    setTold(null);
    try {
      await act(net.network.id, 'traffic.configure', { ...draft });
      setDraft(null);
      await net.refresh();
      setTold({ ok: true, text: t('beatbox.net.traffic.applied') });
    } catch (error) {
      setTold({ ok: false, text: fault(error) });
    }
    setBusy(false);
  }

  const set = (key: keyof Cable, value: number) => setDraft({ ...shown, [key]: value });
  const knob = (key: keyof Cable, value: number, max: number, step: number, said: string, onValue: (v: number) => void) => (
    <div key={key} className="bb-net-knob">
      <span><b>{t(`beatbox.net.traffic.${key}`)}</b> <Info text={t(`beatbox.net.traffic.${key}.what`)} /></span>
      <input type="range" min={0} max={max} step={step} value={value} aria-label={t(`beatbox.net.traffic.${key}`)}
             onChange={(e) => onValue(Number(e.target.value))} />
      <span className="bb-mono bb-net-knobvalue">{said}</span>
    </div>
  );

  return (
    <Page page="traffic" net={net} info={t('beatbox.net.traffic.lede')}>
      {net.network ? (
        <>
          <div role="radiogroup" aria-label={t('beatbox.net.traffic.start_from')} className="bb-net-presets">
            {PRESETS.map(({ key, cable }) => (
              <button key={key} type="button" role="radio" aria-checked={preset === key}
                      className={`bb-net-preset${preset === key ? ' on' : ''}`} onClick={() => setDraft(cable)}>
                <b>{runtimeText(`beatbox.net.preset.${key}`)}</b><small>{cableLine(cable)}</small>
              </button>
            ))}
            <span className={`bb-net-preset bb-net-preset--own${preset === null ? ' on' : ''}`} aria-hidden="true">
              <b>{t('beatbox.net.preset.own')}</b><small>{t('beatbox.net.preset.own_what')}</small>
            </span>
          </div>
          <div className="bb-net-grid">
            <Card title={<><Icon name="pulse" size="sm" />{t('beatbox.net.cable.title')}</>}>
              {knob('latency_ms', shown.latency_ms, 500, 1, t('beatbox.net.cable.delay', { ms: shown.latency_ms }), (v) => set('latency_ms', v))}
              {knob('jitter_ms', shown.jitter_ms, 200, 1, t('beatbox.net.cable.wobble', { ms: shown.jitter_ms }), (v) => set('jitter_ms', v))}
              {knob('loss_percent', shown.loss_percent, 50, 0.1, `${shown.loss_percent} %`, (v) => set('loss_percent', v))}
              {knob('bandwidth_kbps', widthStep(shown.bandwidth_kbps), WIDTHS.length - 1, 1, width(shown.bandwidth_kbps),
                (v) => set('bandwidth_kbps', WIDTHS[v]!))}
              <div className="bb-sbx-settings-foot">
                <span className="bb-faint" style={{ fontSize: 12 }}>{draft ? t('beatbox.net.traffic.pending') : t('beatbox.net.traffic.unchanged')}</span>
                <span className="bb-net-movebtns">
                  {draft ? <Button onClick={() => setDraft(null)}>{t('beatbox.net.traffic.undo')}</Button> : null}
                  <Button variant="primary" icon="check" disabled={busy || !draft} onClick={() => void apply()}>{t('beatbox.net.traffic.apply')}</Button>
                </span>
              </div>
              {told ? <p className={told.ok ? 'bb-faint' : 'bb-sbx-no'} style={{ fontSize: 12 }}>{told.text}</p> : null}
            </Card>
            <Card title={<><Icon name="bulb" size="sm" />{t('beatbox.net.feel.title')}</>}>
              <p className="bb-net-line">{felt.pingMs
                ? t(felt.giveMs ? 'beatbox.net.feel.ping_give' : 'beatbox.net.feel.ping', { ms: felt.pingMs, give: felt.giveMs })
                : t('beatbox.net.feel.ping_none')}</p>
              <p className="bb-net-line">{felt.fileSeconds !== null
                ? t('beatbox.net.feel.file', { seconds: count(felt.fileSeconds) }) : t('beatbox.net.feel.file_free')}</p>
              <p className="bb-net-line">{felt.lossOneIn !== null
                ? t('beatbox.net.feel.loss', { n: count(felt.lossOneIn) }) : t('beatbox.net.feel.loss_none')}</p>
              <p className="bb-net-say bb-net-foot">{t('beatbox.net.feel.so_far', {
                lost: count(Number(counters.lost ?? 0)), received: count(Number(counters.received ?? 0)),
              })}</p>
            </Card>
          </div>
        </>
      ) : null}
    </Page>
  );
}
