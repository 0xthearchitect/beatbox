import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { Button, Icon, Info, Modal, PortInput, RenameTitle, Select, Switch, validPort, type IconName } from '../components';
import { call, on } from '../bridge/client';
import type { ReasonView, SandboxSummary } from '../bridge/types';
import { remember, useChosen } from '../chosen';
import { fault, say, t, runtimeText } from '../i18n';
import { navigate, useRouteParams } from '../router';
import { Asks, FromElsewhere } from './ServerMore';
import { sampleAddress } from '../network';

/**
 * The Server screen: a card per service, in plain words. Engine names only
 * show up in the details.
 *
 * Health is asked for on purpose, every 15s while the page is visible.
 * Everything else reads what the daemon already knows, so no guest I/O.
 */

export const bag = (value: unknown): Record<string, unknown> =>
  (value && typeof value === 'object' ? value as Record<string, unknown> : {});
export const rows = (value: unknown): Record<string, unknown>[] => (Array.isArray(value) ? value.map(bag) : []);
export const text = (value: unknown) => (value == null ? '' : String(value));

/** Run one of the server's capabilities. Throws the daemon's reason if it fails. */
export async function act(id: string, capability: string, args: Record<string, unknown> = {}) {
  const outcome = await call('sandbox.act', { id, capability, arguments: args });
  if (!outcome.ok) throw { reason: outcome.reason, message: capability };
  return bag(outcome.data);
}

export interface Endpoint { protocol: string | null; transport: string; address: string; port: number }
export interface Service {
  name: string;
  definition: string;
  definitionVersion: string;
  state: string;
  health: string;
  healthCheckedAt: number | null;
  pid: number | null;
  config: Record<string, unknown>;
  endpoints: Endpoint[];
  /** Why the last start failed. Cleared once it starts or is stopped. */
  problem: ReasonView | null;
  /** Should be running. Stopped but wanted means its machine took it down. */
  wanted: boolean;
  /** Services that must be up before this one starts. */
  after: string[];
  /** How often the Server restarted it on its own, recently. */
  restarts: number;
}
export interface Setting { kind: string; default?: unknown; values?: string[] }
export interface Definition {
  id: string;
  version: string;
  name: string;
  description: string;
  source: string;
  runtime: string;
  configuration: Record<string, Setting>;
  interfaces: { transport: string; port: unknown; protocol?: string | null }[];
  /** Programs the machine must have for it to run. */
  requires?: string[];
  /** Every argv it can run and file it writes, placeholders left as they are. */
  runs?: {
    command: string[] | null; directory: string | null; prepare: string[] | null; files: string[];
    health_command: string[] | null; capabilities: { capability: string; run: string[] }[];
  };
}

/** argv the way you'd type it: words with spaces get quoted. */
export const argvText = (argv: string[]) => argv.map((word) => (/[\s"']/.test(word) || !word ? JSON.stringify(word) : word)).join(' ');

/** What a folder definition will do in the machine. Shown before you let it start. */
function Runs({ definition }: { definition: Definition }) {
  const runs = definition.runs;
  if (!runs) return null;
  const rows: [string, string][] = [
    ...(runs.command ? [[t('beatbox.srv.runs.command'), argvText(runs.command)] as [string, string]] : []),
    ...(runs.directory ? [[t('beatbox.srv.runs.directory'), runs.directory] as [string, string]] : []),
    ...(runs.prepare ? [[t('beatbox.srv.runs.prepare'), argvText(runs.prepare)] as [string, string]] : []),
    ...runs.files.map((path) => [t('beatbox.srv.runs.file'), path] as [string, string]),
    ...(runs.health_command ? [[t('beatbox.srv.runs.health'), argvText(runs.health_command)] as [string, string]] : []),
    ...runs.capabilities.map((each) => [each.capability, argvText(each.run)] as [string, string]),
  ];
  return (
    <div>
      <p className="bb-faint" style={{ margin: '0 0 6px' }}>{t('beatbox.srv.runs.trust')}</p>
      <details className="bb-srv-more">
        <summary>{t('beatbox.srv.runs.title')}</summary>
        <pre className="bb-srv-log">{rows.map(([label, value]) => `${label}: ${value}`).join('\n')}</pre>
      </details>
    </div>
  );
}

/** Built-in definitions. Each one needs a name and blurb in the locales (the strings test checks). */
const KNOWN = ['http-static', 'nginx', 'dns', 'postgresql', 'smtp'];
/** Config keys a person can see. Each one needs a label in the locales. */
const KEYS = ['port', 'root', 'page', 'mode', 'command', 'health_path', 'bin_directory', 'initial_password', 'web_port', 'accept_any_login', 'database', 'user', 'upstream', 'https_port'];
const WEB_MODES = ['page', 'folder', 'application'];
const NGINX_MODES = ['folder', 'proxy'];

/** Capabilities with a translated name, so details and Try it say the same thing. */
export const CAPS = ['dns.resolve', 'dns.record.add', 'dns.record.remove', 'database.query', 'nginx.test', 'email.received'];
export const capWord = (capability: string) =>
  runtimeText(CAPS.includes(capability) ? `beatbox.srv.cap.${capability}` : 'beatbox.srv.cap.custom');

export const kindOf = (definition: Definition | undefined, id: string) =>
  (KNOWN.includes(id) ? runtimeText(`beatbox.srv.kind.${id}`) : definition?.name ?? id);
export const aboutOf = (definition: Definition | undefined, id: string) =>
  (KNOWN.includes(id) ? runtimeText(`beatbox.srv.about.${id}`) : definition?.description ?? '');
const keyWord = (key: string) => (KEYS.includes(key) ? runtimeText(`beatbox.srv.key.${key}`) : key);

/** Pick the icon from the protocol, not the name: people name services anything. */
export function iconOf(definition: Definition | undefined): IconName {
  const protocol = text(definition?.interfaces[0]?.protocol);
  if (protocol === 'http' || protocol === 'https') return 'globe';
  if (protocol === 'dns') return 'book';
  if (protocol === 'postgresql') return 'database';
  if (protocol === 'smtp') return 'mail';
  return 'puzzle';
}

/** The address as you'd type it: name.domain if the server has a domain, else the IP. */
export function reach(service: Service, end = service.endpoints[0], domain = ''): string {
  if (!end) return '';
  const host = domain ? `${service.name}.${domain}` : end.address.includes(':') ? `[${end.address}]` : end.address;
  if (end.protocol === 'http' || end.protocol === 'https') return `${end.protocol}://${host}:${end.port}`;
  if (end.protocol === 'dns') return end.address;
  return `${host}:${end.port}`;
}

export type Mood = 'ok' | 'slow' | 'failed' | 'stopped';
export function mood(service: Service): Mood {
  if (service.state === 'running') return service.health === 'unhealthy' ? 'slow' : 'ok';
  return service.state === 'failed' ? 'failed' : 'stopped';
}

export function useServer() {
  const chosen = useChosen('server');
  const server = chosen.machine;
  const id = server?.id ?? '';
  const [services, setServices] = useState<Service[] | null>(null);
  const [catalog, setCatalog] = useState<Definition[]>([]);
  const [domain, setDomain] = useState('');
  const [told, setTold] = useState('');

  // Health results since the page opened, per service. Feeds the card sparks.
  const history = useRef<Map<string, string[]>>(new Map());
  const inFlight = useRef<string | null>(null);
  const currentId = useRef(id);
  currentId.current = id;
  const load = useCallback(async () => {
    if (!id) return;
    if (inFlight.current === id) return;
    inFlight.current = id;
    try {
      const [listed, offered, own] = await Promise.all([act(id, 'service.list', { check_health: true }), act(id, 'service.catalog'),
        act(id, 'server.settings')]);
      if (currentId.current !== id) return;
      for (const row of rows(listed.services)) {
        const name = text(row.service);
        const seen = history.current.get(name) ?? [];
        history.current.set(name, [...seen, row.state === 'running' ? text(row.health) : 'off'].slice(-HISTORY));
      }
      setServices(rows(listed.services).map((row) => ({
        name: text(row.service), definition: text(row.definition), state: text(row.state),
        definitionVersion: text(row.definition_version),
        health: text(row.health), pid: row.pid == null ? null : Number(row.pid), config: bag(row.config),
        healthCheckedAt: row.health_checked_at == null ? null : Number(row.health_checked_at),
        problem: row.problem ? bag(row.problem) as unknown as ReasonView : null,
        wanted: row.wanted === true,
        after: Array.isArray(row.after) ? row.after.map(String) : [],
        restarts: Number(row.restarts ?? 0),
        endpoints: rows(row.endpoints).map((end) => ({
          protocol: end.protocol == null ? null : text(end.protocol), transport: text(end.transport),
          address: text(end.address), port: Number(end.port),
        })),
      })));
      setCatalog(rows(offered.definitions) as unknown as Definition[]);
      setDomain(text(own.domain));
      setTold('');
    } catch (error) {
      if (currentId.current === id) setTold(fault(error));
    } finally {
      if (inFlight.current === id) inFlight.current = null;
    }
  }, [id]);

  useEffect(() => {
    setServices(null);
    history.current = new Map();
    void load();
  }, [load]);
  useEffect(() => on('sandbox', (event) => {
    if (event.sandboxId === id && event.name.startsWith('service.')) void load();
  }), [id, load]);

  useEffect(() => {
    if (server?.state.lifecycle !== 'running') return;
    const refresh = () => { if (!document.hidden) void load(); };
    const timer = window.setInterval(refresh, 15000);
    document.addEventListener('visibilitychange', refresh);
    return () => { window.clearInterval(timer); document.removeEventListener('visibilitychange', refresh); };
  }, [server?.state.lifecycle, load]);
  return { ...chosen, server, id, services, catalog, domain, told, load, history: history.current };
}

/** Bars per spark. */
const HISTORY = 12;

export interface Check {
  host: { id: string; state: string; agent: boolean; programs: Record<string, boolean> } | null;
  network: { id: string; state: string } | null;
  at: number;
}

/**
 * Does the machine and network have what the server needs? Asked on open,
 * when one of them changes state, or when you press ↻. One guest call each
 * time, never on a timer.
 */
export function useCheck(srv: Server) {
  const [check, setCheck] = useState<Check | null>(null);
  const uses = bag(srv.server?.config.uses);
  const load = useCallback(async () => {
    if (!srv.id) { setCheck(null); return; }
    try {
      const said = await act(srv.id, 'server.check');
      setCheck({
        host: said.host ? bag(said.host) as unknown as Check['host'] : null,
        network: said.network ? bag(said.network) as unknown as Check['network'] : null,
        at: Date.now(),
      });
    } catch { setCheck(null); }
  }, [srv.id]);
  useEffect(() => { void load(); }, [load]);
  useEffect(() => on('sandbox', (event) => {
    if ((event.sandboxId === uses.host || event.sandboxId === uses.network)
      && event.name.startsWith('sandbox.') && event.name !== 'sandbox.action') void load();
  }), [load, uses.host, uses.network]);
  return { check, recheck: load };
}

export type Server = ReturnType<typeof useServer>;

/** All sandboxes of one type, to turn the ids in `uses` into names. */
export function useOthers(type: string) {
  const [list, setList] = useState<SandboxSummary[]>([]);
  const load = useCallback(() => {
    void call('sandbox.list').then((all) => setList(all.filter((row) => row.type === type)), () => undefined);
  }, [type]);
  useEffect(load, [load]);
  // Keep in sync when another page turns something on or off.
  useEffect(() => on('sandbox', (event) => {
    if (event.name.startsWith('sandbox.') && event.name !== 'sandbox.action') load();
  }), [load]);
  return list;
}

/* ── the page ─────────────────────────────────────────────────────────── */

export function ServerServices() {
  const srv = useServer();
  const params = useRouteParams();
  const making = params.get('new') === '1';
  const [adding, setAdding] = useState<string | null>(null);
  // The Library links here with ?add=<kind> to open that form.
  const asked = params.get('add');
  useEffect(() => { if (asked) setAdding(asked); }, [asked]);
  const closeAdding = () => { setAdding(null); if (asked) navigate('server/services'); };

  if (making || (srv.list && !srv.list.length)) return <NewServer />;
  if (!srv.server) return null;
  return (
    <div className="bb-srv-ov">
      <Top srv={srv} onAdd={() => setAdding('')} />
      {srv.told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{srv.told}</p> : null}
      <Helpers srv={srv} />
      {srv.services === null ? null
        : srv.services.length ? <Board srv={srv} />
        : <Empty srv={srv} onPick={(definition) => setAdding(definition)} />}
      {adding !== null ? <AddService srv={srv} start={adding} onClose={closeAdding} /> : null}
    </div>
  );
}

function Top({ srv, onAdd }: { srv: Server; onAdd: () => void }) {
  const server = srv.server!;
  const on = server.state.lifecycle === 'running';
  const services = srv.services ?? [];
  const working = services.filter((service) => mood(service) === 'ok').length;
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  async function turn() {
    setBusy(true);
    setTold('');
    try {
      await call('sandbox.transition', { id: server.id, transition: on ? 'stop' : 'start' });
      await srv.reload();
      await srv.load();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  return (
    <header className="bb-srv-top">
      <div className="bb-srv-toptext">
        <div className="bb-srv-topname">
          <h1 className="bb-t-title bb-srv-title">
            <RenameTitle name={server.name}
                         onRename={async (name) => { await call('sandbox.rename', { id: server.id, name }); await srv.reload(); }} />
          </h1>
          <span className={`bb-srv-blkstate is-${on ? 'ok' : 'stopped'}`}>{runtimeText(`beatbox.plain.state.${server.state.lifecycle}`)}</span>
          {services.length || srv.domain ? <span className="bb-srv-topsub">
            {[services.length ? t('beatbox.srv.top.working', { working, count: services.length }) : '',
              srv.domain ? t('beatbox.srv.top.domain', { domain: srv.domain }) : ''].filter(Boolean).join(' · ')}
          </span> : null}
        </div>
        {told ? <small className="bb-sbx-no">{told}</small> : null}
      </div>
      <div className="bb-srv-topacts">
        <Button icon={on ? 'stop' : 'play'} disabled={busy} onClick={() => void turn()}>{t(on ? 'beatbox.srv.top.off' : 'beatbox.srv.top.on')}</Button>
        <Button variant="primary" icon="plus" onClick={onAdd}>{t('beatbox.srv.add')}</Button>
      </div>
    </header>
  );
}

/* ── what it needs ────────────────────────────────────────────────────── */

/**
 * machine — server — network in one line. What's fine hides in the hover;
 * each problem gets its own line with the button that fixes it.
 */
function Helpers({ srv }: { srv: Server }) {
  const { check, recheck } = useCheck(srv);
  const hosts = useOthers('host');
  const networks = useOthers('network');
  const uses = bag(srv.server?.config.uses);
  const apart = useApart(text(uses.host), text(uses.network));
  const [busy, setBusy] = useState('');
  const hostName = hosts.find((row) => row.id === uses.host)?.name ?? '';
  const networkName = networks.find((row) => row.id === uses.network)?.name ?? '';
  const hostState = check?.host?.state ?? hosts.find((row) => row.id === uses.host)?.lifecycle;
  const networkState = check?.network?.state ?? networks.find((row) => row.id === uses.network)?.lifecycle;
  const services = srv.services ?? [];
  // Machines only get our names while a names service is running.
  const internal = services.filter((service) => srv.catalog.find((each) => each.id === service.definition)?.runtime === 'internal');
  const names = internal.some((service) => service.state === 'running');
  // Missing program -> the services that need it.
  const missing = new Map<string, string[]>();
  for (const service of services) {
    const definition = srv.catalog.find((each) => each.id === service.definition);
    for (const program of definition?.requires ?? []) {
      if (check?.host?.programs[program] === false) missing.set(program, [...(missing.get(program) ?? []), service.name]);
    }
  }
  async function turnOn(id: string) {
    setBusy(id);
    try {
      await call('sandbox.transition', { id, transition: 'start' });
      await srv.load();
      await recheck();
    } catch { /* the line shows the state */ }
    setBusy('');
  }
  const stateOf = (state: string | undefined) => runtimeText(`beatbox.plain.state.${state === 'gone' ? 'destroyed' : state ?? 'created'}`);
  const toneOf = (state: string | undefined) => (state === 'running' ? 'ok' : state === 'gone' ? 'failed' : 'stopped');
  const hostOn = hostState === 'running';
  const openMachine = () => { remember('host', text(uses.host)); navigate('host/machine'); };
  const turnOnButton = (id: string) => <Button size="sm" icon="play" disabled={busy !== ''} onClick={() => void turnOn(id)}>{t('beatbox.srv.helpers.turn_on')}</Button>;
  const openButton = <Button size="sm" icon="arrow" onClick={openMachine}>{t('beatbox.srv.helpers.open_machine')}</Button>;
  const issues: { key: string; tone: 'no' | 'warn' | 'off'; words: string; action?: ReactNode }[] = [];
  if (uses.host && hostState && !hostOn && hostState !== 'gone') issues.push({ key: 'host-off', tone: 'off', words: t('beatbox.srv.helpers.machine_off'), action: turnOnButton(text(uses.host)) });
  if (uses.host && uses.network && apart) issues.push({ key: 'cable', tone: 'no', words: t('beatbox.srv.helpers.cable_no', { network: networkName }), action: openButton });
  if (hostOn && check?.host && !check.host.agent) issues.push({ key: 'agent', tone: 'no', words: t('beatbox.srv.helpers.agent_no'), action: openButton });
  if (hostOn && check?.host) for (const [program, who] of missing) {
    issues.push({ key: program, tone: 'warn', words: t('beatbox.srv.helpers.missing', { program, services: who.join(', ') }), action: openButton });
  }
  if (!uses.network) issues.push({ key: 'no-network', tone: 'off', words: t('beatbox.srv.helpers.none_network') });
  // No state yet means a page that just opened, not a network that is off.
  else if (networkState && networkState !== 'running') issues.push({ key: 'network-off', tone: 'off', words: t('beatbox.srv.helpers.network_off'),
    action: networkState !== 'gone' ? turnOnButton(text(uses.network)) : undefined });
  else if (!names && internal.length) issues.push({ key: 'names', tone: 'off', words: t('beatbox.srv.helpers.names_waiting') });
  // The fine stuff goes in the tooltip. Nobody reads a wall of green ticks.
  const hostSaid = [stateOf(hostState), uses.network && !apart ? t('beatbox.srv.helpers.cable_ok', { network: networkName }) : '',
    hostOn && check?.host?.agent ? t('beatbox.srv.helpers.agent_ok') : ''].filter(Boolean).join(' · ');
  const networkSaid = [stateOf(networkState), networkState === 'running'
    ? t(names ? 'beatbox.srv.helpers.names_ok' : 'beatbox.srv.helpers.names_none') : ''].filter(Boolean).join(' · ');
  return (
    <section className="bb-srv-strip" aria-label={t('beatbox.srv.helpers.title')}>
      <div className="bb-srv-striprow">
        <span className="bb-srv-strippart" title={uses.host ? hostSaid : undefined}>
          <Icon name="cpu" size="sm" /><b>{hostName || t('beatbox.srv.helpers.machine')}</b>
          {uses.host ? <span className={`bb-srv-blkstate is-${toneOf(hostState)}`} aria-label={hostSaid} />
            : <small className="bb-faint">{t('beatbox.srv.helpers.none_machine')}</small>}
        </span>
        <span className="bb-srv-helperlink" aria-hidden="true" />
        <span className="bb-srv-strippart"><Icon name="server" size="sm" /><b>{srv.server?.name}</b></span>
        <span className="bb-srv-helperlink" aria-hidden="true" />
        <span className="bb-srv-strippart" title={uses.network ? networkSaid : undefined}>
          <Icon name="network" size="sm" /><b>{networkName || t('beatbox.srv.helpers.network')}</b>
          {uses.network ? <span className={`bb-srv-blkstate is-${toneOf(networkState)}`} aria-label={networkSaid} /> : null}
        </span>
        <span className="bb-srv-stripwhen">
          {check ? <small>{t('beatbox.srv.helpers.checked', { time: new Date(check.at).toLocaleTimeString() })}</small> : null}
          <button type="button" className="bb-srv-copy" aria-label={t('beatbox.srv.helpers.check_again')} title={t('beatbox.srv.helpers.check_again')}
                  onClick={() => void recheck()}><Icon name="refresh" size="sm" /></button>
        </span>
      </div>
      {issues.length ? (
        <ul className="bb-srv-stripissues">
          {issues.map((issue) => (
            <li key={issue.key} className={issue.tone}>
              {issue.tone === 'off' ? null : <Icon name="alert" size="sm" />}<span>{issue.words}</span>{issue.action}
            </li>
          ))}
        </ul>
      ) : null}
    </section>
  );
}

function Spark({ seen }: { seen: string[] }) {
  if (!seen.length) return null;
  return (
    <span className="bb-srv-spark" role="img" aria-label={t('beatbox.srv.spark', { count: seen.length })}>
      {seen.map((each, index) => <i key={index} className={`is-${each}`} />)}
    </span>
  );
}

/** The last few service events, in words. Hidden when there are none. */
function Lately({ srv }: { srv: Server }) {
  const [said, setSaid] = useState<{ at: string; tone: string; text: string }[]>([]);
  const load = useCallback(async () => {
    try {
      const all = await call('sandbox.events', { since: 0 });
      const mine = all.filter((event) => event.sandboxId === srv.id && event.name.startsWith('service.')).slice(-40);
      const words = mine.flatMap((event) => {
        const data = bag(event.data);
        const service = text(data.service);
        const at = new Date(event.at).toLocaleTimeString();
        if (event.name === 'service.health.changed') {
          if (data.to === 'unknown') return [];
          return [{ at, tone: data.to === 'healthy' ? 'mint' : 'amber', text: runtimeText(`beatbox.srv.lately.${data.to === 'healthy' ? 'answers' : 'silent'}`, { service }) }];
        }
        if (event.name === 'service.restarting') {
          return [{ at, tone: 'amber', text: t('beatbox.srv.lately.restarting', { service, attempt: Number(data.attempt ?? 1) }) }];
        }
        const known = ['started', 'stopped', 'failed', 'created', 'removed'].find((each) => event.name === `service.${each}`);
        if (!known) return [];
        return [{ at, tone: known === 'failed' ? 'rose' : known === 'started' ? 'mint' : 'neutral', text: runtimeText(`beatbox.srv.lately.${known}`, { service }) }];
      });
      setSaid(words.slice(-6).reverse());
    } catch { /* nothing to show */ }
  }, [srv.id]);
  useEffect(() => { void load(); }, [load]);
  useEffect(() => on('sandbox', (event) => { if (event.sandboxId === srv.id && event.name.startsWith('service.')) void load(); }), [srv.id, load]);
  if (!said.length) return null;
  return (
    <section className="bb-srv-events" aria-labelledby="bb-srv-lately">
      <h3 id="bb-srv-lately">{t('beatbox.srv.lately.title')}</h3>
      <ol>
        {said.map((each, index) => (
          <li key={index}><span className="bb-mono">{each.at}</span><i className={`is-${each.tone}`} aria-hidden="true" /><span>{each.text}</span></li>
        ))}
      </ol>
    </section>
  );
}

/* ── the services ─────────────────────────────────────────────────────── */

function Board({ srv }: { srv: Server }) {
  const services = srv.services!;
  const [chosen, setChosen] = useState(services[0]!.name);
  const picked = services.find((service) => service.name === chosen) ?? services[0]!;
  return (
    <div className="bb-srv-ovbody">
      <section className="bb-srv-board" aria-labelledby="bb-srv-board">
        <h2 id="bb-srv-board">{t('beatbox.srv.board.title')}</h2>
        <div className="bb-srv-blks">
          {services.map((service) => (
            <Block key={service.name} srv={srv} service={service} on={service.name === picked.name} onPick={() => setChosen(service.name)} />
          ))}
        </div>
      </section>
      <Side srv={srv} service={picked} />
      <Lately srv={srv} />
    </div>
  );
}

function Block({ srv, service, on: picked, onPick }: { srv: Server; service: Service; on: boolean; onPick: () => void }) {
  const definition = srv.catalog.find((each) => each.id === service.definition);
  const how = mood(service);
  const internal = definition?.runtime === 'internal';
  const serverOff = srv.server?.state.lifecycle !== 'running';
  return (
    <button type="button" aria-pressed={picked} className={`bb-srv-blk is-${how}${picked ? ' on' : ''}`} onClick={onPick}>
      <span className="bb-srv-blktop">
        <b>{service.name}</b>
        <span className={`bb-srv-blkstate is-${how}`}>{runtimeText(`beatbox.srv.mood.${how}`)}</span>
      </span>
      <small>{kindOf(definition, service.definition)}</small>
      {service.after.length ? <small className="bb-srv-blkafter">{t('beatbox.srv.after.short', { names: service.after.join(', ') })}</small> : null}
      {how === 'ok' && service.endpoints.length ? <span className="bb-mono">{reach(service, undefined, srv.domain)}</span>
        : how === 'ok' ? <span className="bb-faint">{t('beatbox.srv.foot.no_address')}</span>
        : how === 'stopped' && service.wanted && serverOff ? <span className="bb-faint">{t('beatbox.srv.foot.server_off')}</span>
        : how === 'stopped' && service.wanted ? <span className="bb-faint">{t(internal ? 'beatbox.srv.foot.was_running_network' : 'beatbox.srv.foot.was_running')}</span>
        : <span className="bb-faint">{runtimeText(`beatbox.srv.foot.${how}`)}</span>}
      <Spark seen={srv.history.get(service.name) ?? []} />
    </button>
  );
}

/**
 * The picked service. Left: what happened, address, Try from, More.
 * Right: its log, always, even empty -- otherwise half the panel sits empty.
 */
function Side({ srv, service }: { srv: Server; service: Service }) {
  const [lines, setLines] = useState<string[]>([]);
  const [described, setDescribed] = useState<Record<string, unknown>>({});
  const [busy, setBusy] = useState(false);
  const [asking, setAsking] = useState(false);
  const [told, setTold] = useState('');
  const [copied, setCopied] = useState('');
  const [editing, setEditing] = useState<{ definition: Definition; service: Service } | null>(null);
  const definition = srv.catalog.find((each) => each.id === service.definition);
  const how = mood(service);
  const live = how === 'ok' || how === 'slow';
  const problem = how === 'slow' ? t('beatbox.srv.problem.slow')
    : how !== 'failed' ? ''
    : service.problem ? say(service.problem, service.problem.code) : t('beatbox.srv.problem.failed');
  const { check } = useCheck(srv);

  useEffect(() => { setAsking(false); setTold(''); setEditing(null); setLines([]); }, [service.name]);
  // Log: every 5s while it runs and the page is visible. Once if it failed.
  useEffect(() => {
    if (!live && how !== 'failed') return;
    const read = () => {
      if (document.hidden) return;
      void act(srv.id, 'service.logs', { service: service.name, lines: 8 })
        .then((said) => setLines(Array.isArray(said.lines) ? said.lines.map(String) : []), () => setLines([]));
    };
    read();
    if (!live) return;
    const timer = window.setInterval(read, 5000);
    return () => window.clearInterval(timer);
  }, [how, live, srv.id, service.name, service.state]);
  useEffect(() => {
    void act(srv.id, 'service.describe', { service: service.name }).then(setDescribed, () => setDescribed({}));
  }, [srv.id, service.name]);

  async function run(capability: string, arguments_: Record<string, unknown> = {}) {
    setBusy(true);
    setTold('');
    try {
      await act(srv.id, capability, { service: service.name, ...arguments_ });
      await srv.load();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
    setAsking(false);
  }

  async function edit() {
    setBusy(true);
    setTold('');
    try {
      const current = await act(srv.id, 'service.describe', { service: service.name });
      const saved = bag(current.definition);
      setEditing({
        definition: { ...saved, runtime: text(bag(saved.runtime).type) } as unknown as Definition,
        service: { ...service, config: bag(current.config) },
      });
    } catch (error) {
      setTold(fault(error));
    } finally {
      setBusy(false);
    }
  }

  const primary = how === 'stopped' ? ['service.start', 'beatbox.srv.do.start'] as const
    : how === 'failed' ? ['service.start', 'beatbox.srv.do.retry'] as const
    : ['service.stop', 'beatbox.srv.do.stop'] as const;
  const capabilities = rows(described.capabilities).map((each) => text(each.capability));
  const needs = needsOf(srv, definition, check);
  // Missing needs go in the problem box. The full list is under More.
  const lacking = needs.filter((need) => need.state === 'no');
  const running = service.state === 'running';
  const outdated = definition && service.definitionVersion && definition.version !== service.definitionVersion;
  const logged = live || how === 'failed';

  return (
    <section className="bb-srv-side" aria-labelledby="bb-srv-side">
      <header className="bb-srv-sidehead">
        <div>
          <small>{kindOf(definition, service.definition)}</small>
          <div className="bb-srv-sidename">
            <h2 id="bb-srv-side">{service.name}</h2>
            <span className={`bb-srv-blkstate is-${how}`}>{runtimeText(`beatbox.srv.mood.${how}`)}</span>
          </div>
        </div>
        <div className="bb-srv-sideacts">
          <Button variant={live ? undefined : 'primary'} icon={live ? 'stop' : 'play'} disabled={busy} onClick={() => void run(primary[0]!)}>{t(primary[1])}</Button>
          {live ? <Button icon="refresh" disabled={busy} onClick={() => void run('service.restart')}>{t('beatbox.srv.do.restart')}</Button> : null}
          <Button icon="pencil" disabled={busy || running} title={running ? t('beatbox.srv.edit.stop_first') : undefined}
                  onClick={() => void edit()}>{t('beatbox.srv.do.edit')}</Button>
          {!live ? <Button icon="trash" disabled={busy} onClick={() => { setTold(''); setAsking(true); }}>{t('beatbox.srv.do.remove')}</Button> : null}
        </div>
      </header>

      <div className="bb-srv-sidebody">
        <div className="bb-srv-sidemain">
          {problem || lacking.length ? (
            <div className={`bb-srv-problem is-${how === 'slow' ? 'slow' : 'failed'}`}>
              <b>{t(problem ? 'beatbox.srv.what_happened' : 'beatbox.srv.reqs.title')}</b>
              {problem ? <span>{problem}</span> : null}
              {lacking.length ? (
                <div className="bb-srv-lacking">
                  <ul className="bb-srv-checks">{lacking.map((need) => <li key={need.key} className="no">{need.words}</li>)}</ul>
                  {lacking.some((need) => need.key !== 'net') ? (
                    <Button size="sm" icon="arrow" onClick={() => { remember('host', text(bag(srv.server?.config.uses).host)); navigate('host/machine'); }}>
                      {t('beatbox.srv.helpers.open_machine')}
                    </Button>
                  ) : null}
                </div>
              ) : null}
            </div>
          ) : null}
          {how === 'stopped' && service.wanted ? (
            <p className="bb-faint" style={{ margin: 0 }}>{t(srv.server?.state.lifecycle !== 'running' ? 'beatbox.srv.side.server_off'
              : definition?.runtime === 'internal' ? 'beatbox.srv.was_running_network' : 'beatbox.srv.was_running')}</p>
          ) : null}
          {/* Same text as the problem box? Don't say it twice. */}
          {told && told !== problem ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}

          {live ? [false, true].map((inbox) => {
            const ends = service.endpoints.filter((end) => (end.protocol === 'http' && service.endpoints.some((each) => each.protocol === 'smtp')) === inbox);
            if (!ends.length) return null;
            return (
              <div className="bb-srv-sidepart" key={inbox ? 'inbox' : 'address'}>
                <small>{t(inbox ? 'beatbox.srv.reach.inbox_label' : 'beatbox.srv.reach.address')} <Info text={t(inbox ? 'beatbox.srv.reach.inbox'
                  : ends[0]!.protocol === 'dns' ? 'beatbox.srv.reach.dns'
                  : srv.domain ? 'beatbox.srv.reach.named' : definition?.runtime === 'process' ? 'beatbox.srv.reach.guest'
                  : definition?.interfaces[0]?.protocol === 'dns' ? 'beatbox.srv.reach.dns' : 'beatbox.srv.reach.other')} /></small>
                {ends.map((end) => {
                  const address = reach(service, end, srv.domain);
                  return (
                    <span className="bb-srv-sideaddr" key={`${end.transport}-${address}`}>
                      <b className="bb-mono">{address}</b>
                      <button type="button" className="bb-srv-copy" aria-label={t('beatbox.srv.copy')} title={t('beatbox.srv.copy')}
                              onClick={() => { void navigator.clipboard?.writeText(address); setCopied(address); window.setTimeout(() => setCopied(''), 1500); }}>
                        <Icon name={copied === address ? 'check' : 'copy'} size="sm" />
                      </button>
                    </span>
                  );
                })}
              </div>
            );
          }) : null}

          {live ? <FromElsewhere key={`from-${service.name}`} srv={srv} service={service} /> : null}

          <details className="bb-srv-more">
            <summary>{t('beatbox.srv.side.more')}</summary>
            <div className="bb-srv-stack">
              {live && capabilities.length ? <Asks key={`asks-${service.name}`} srv={srv} service={service} definition={definition} /> : null}
              <Needs needs={needs} />
              <After srv={srv} service={service} />
              {outdated ? (
                <div className="bb-srv-row">
                  <Button size="sm" icon="refresh" disabled={busy || running}
                          onClick={() => void run('service.configure', { config: {}, update_definition: true })}>
                    {t('beatbox.srv.update_definition', { version: definition.version })}
                  </Button>
                  {running ? <small className="bb-faint">{t('beatbox.srv.side.stop_first')}</small> : null}
                </div>
              ) : null}
              <dl className="bb-srv-facts">
                <dt>{t('beatbox.srv.fact.definition')}</dt><dd>{service.definition} · {runtimeText(`beatbox.srv.source.${definition?.source ?? 'inline'}`)}</dd>
                {service.endpoints.map((end) => (
                  <span key={`${end.transport}${end.port}`} style={{ display: 'contents' }}>
                    <dt>{t('beatbox.srv.fact.port')}</dt><dd>{end.transport} {end.port}</dd>
                  </span>
                ))}
                {service.pid ? <><dt>{t('beatbox.srv.fact.runs')}</dt><dd>pid {service.pid}</dd></> : null}
                {definition?.runtime === 'internal' ? <><dt>{t('beatbox.srv.fact.runs')}</dt><dd>{t('beatbox.srv.inside')}</dd></> : null}
                {Object.entries(service.config).map(([key, value]) => (
                  <span key={key} style={{ display: 'contents' }}><dt>{keyWord(key)}</dt>
                    <dd>{sharedName(value) ? t('beatbox.srv.shared.uses', { name: sharedName(value) }) : text(value)}</dd></span>
                ))}
                {capabilities.length ? <><dt>{t('beatbox.srv.fact.can')}</dt><dd>{[...new Set(capabilities.map(capWord))].join(' · ')}</dd></> : null}
                {service.healthCheckedAt ? <><dt>{t('beatbox.srv.side.checked')}</dt><dd>{new Date(service.healthCheckedAt).toLocaleTimeString()}</dd></> : null}
              </dl>
            </div>
          </details>
        </div>

        {logged ? (
          <div className="bb-srv-sidelog">
            <small className={live ? 'is-live' : undefined}>{t(live ? 'beatbox.srv.live_log' : 'beatbox.srv.side.last_log')}</small>
            {lines.length ? <pre className="bb-srv-log">{lines.join('\n')}</pre>
              : <div className="bb-srv-log bb-srv-log--empty">{t('beatbox.srv.side.no_log')}</div>}
          </div>
        ) : null}
      </div>

      {asking ? <Remove srv={srv} service={service} busy={busy} onClose={() => setAsking(false)}
                         onRemove={(deleteData) => void run('service.remove', { delete_data: deleteData })} /> : null}
      {editing ? <Modal title={t('beatbox.srv.edit.title', { name: service.name })} icon="pencil" width={720}
                        onClose={() => setEditing(null)}>
        <div className="bb-srv-modal">
          <Configure srv={srv} definition={editing.definition} service={editing.service}
                     onBack={() => setEditing(null)} onDone={() => setEditing(null)} />
        </div>
      </Modal> : null}
    </section>
  );
}

type Need = { key: string; state: 'ok' | 'no' | 'off'; words: string };

/**
 * What it needs to start. 'off' = couldn't check yet: the machine is off,
 * or the check hasn't answered. Every page asks on its own, so a page that
 * just opened has no answer for a second, and that is not a "no".
 */
function needsOf(srv: Server, definition: Definition | undefined, check: Check | null): Need[] {
  if (!definition || !srv.server) return [];
  const host = check?.host;
  if (definition.runtime === 'internal') {
    return [{ key: 'net', state: !check ? 'off' : check.network?.state === 'running' ? 'ok' : 'no', words: t('beatbox.srv.reqs.network_on') }];
  }
  return [
    { key: 'on', state: !check ? 'off' : host?.state === 'running' ? 'ok' : 'no', words: t('beatbox.srv.reqs.machine_on') },
    { key: 'agent', state: host?.state !== 'running' ? 'off' : host.agent ? 'ok' : 'no', words: t('beatbox.srv.reqs.agent') },
    ...(definition.requires ?? []).map((program) => ({
      key: program,
      state: (host?.programs[program] === undefined ? 'off' : host.programs[program] ? 'ok' : 'no') as Need['state'],
      words: t('beatbox.srv.reqs.program', { program }),
    })),
  ];
}

function Needs({ needs }: { needs: Need[] }) {
  if (!needs.length) return null;
  return (
    <div className="bb-srv-sidepart">
      <small>{t('beatbox.srv.reqs.title')}</small>
      <ul className="bb-srv-checks">
        {needs.map((item) => <li key={item.key} className={item.state}>{item.words}</li>)}
      </ul>
    </div>
  );
}

/** Services that must start first. Edit any time; it counts from the next start. */
function After({ srv, service }: { srv: Server; service: Service }) {
  const others = (srv.services ?? []).filter((each) => each.name !== service.name);
  const [editing, setEditing] = useState(false);
  const [picked, setPicked] = useState<string[]>(service.after);
  const [told, setTold] = useState('');
  useEffect(() => { setEditing(false); setPicked(service.after); setTold(''); }, [service.name, service.after.join(',')]);
  if (!others.length) return null;
  async function save() {
    setTold('');
    try {
      await act(srv.id, 'service.depend', { service: service.name, after: picked });
      await srv.load();
      setEditing(false);
    } catch (error) {
      setTold(fault(error));
    }
  }
  return (
    <div className="bb-srv-sidepart">
      <small>{t('beatbox.srv.after.title')}</small>
      {!editing ? (
        <div className="bb-srv-afterrow">
          {service.after.length ? service.after.map((name) => {
            const other = others.find((each) => each.name === name);
            return <span key={name} className={`bb-srv-afterchip${other && mood(other) === 'ok' ? ' ok' : ''}`}>{name}</span>;
          }) : <span className="bb-faint">{t('beatbox.srv.after.none')}</span>}
          <Button size="sm" icon="pencil" onClick={() => setEditing(true)}>{t('beatbox.srv.after.change')}</Button>
        </div>
      ) : (
        <fieldset className="bb-srv-radios">
          <legend><Info text={t('beatbox.srv.after.how')} /></legend>
          {others.map((other) => (
            <label key={other.name} className={`bb-srv-radio${picked.includes(other.name) ? ' on' : ''}`}>
              <input type="checkbox" checked={picked.includes(other.name)}
                     onChange={(event) => setPicked(event.target.checked ? [...picked, other.name] : picked.filter((name) => name !== other.name))} />
              <span><b>{other.name}</b><small>{kindOf(srv.catalog.find((each) => each.id === other.definition), other.definition)}</small></span>
            </label>
          ))}
          <div className="bb-srv-row">
            <Button size="sm" variant="primary" onClick={() => void save()}>{t('beatbox.srv.after.save')}</Button>
            <Button size="sm" onClick={() => { setEditing(false); setPicked(service.after); setTold(''); }}>{t('beatbox.files.cancel')}</Button>
          </div>
        </fieldset>
      )}
      {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
    </div>
  );
}

/** Dependencies first: the order the Server starts things in. */
export function startOrder(services: Service[]): string[] {
  const done: string[] = [];
  const visit = (name: string, path: string[]) => {
    if (done.includes(name) || path.includes(name)) return;
    const service = services.find((each) => each.name === name);
    for (const need of service?.after ?? []) visit(need, [...path, name]);
    done.push(name);
  };
  for (const service of services) visit(service.name, []);
  return done;
}

/** Internal services need a network; the rest need a machine. */
export function readyFor(srv: Server) {
  const uses = bag(srv.server?.config.uses);
  return (definition: Definition) => Boolean(definition.runtime === 'internal' ? uses.network : uses.host);
}
const whyNot = (definition: Definition) =>
  t(definition.runtime === 'internal' ? 'beatbox.srv.needs_network' : 'beatbox.srv.needs_machine');

/**
 * Remove a service. Its files stay by default, so re-adding the same name
 * picks them back up. If you delete them, you see what goes first.
 */
function Remove({ srv, service, busy, onClose, onRemove }: {
  srv: Server; service: Service; busy: boolean; onClose: () => void; onRemove: (deleteData: boolean) => void;
}) {
  const [data, setData] = useState<string[] | null>(null);
  const [drop, setDrop] = useState(false);
  useEffect(() => {
    void act(srv.id, 'service.describe', { service: service.name })
      .then((said) => setData(Array.isArray(said.data) ? said.data.map(String) : []), () => setData([]));
  }, [srv.id, service.name]);
  return (
    <Modal title={t('beatbox.srv.sure_remove', { name: service.name })} icon="trash" width={480} onClose={onClose}
           footer={<>
             <span style={{ flex: 1 }} />
             <Button onClick={onClose}>{t('beatbox.files.cancel')}</Button>
             <Button variant="danger" disabled={busy || data === null} onClick={() => onRemove(drop)}>
               {t(drop ? 'beatbox.srv.remove.delete_do' : 'beatbox.srv.do.remove')}
             </Button>
           </>}>
      <div className="bb-srv-modal">
      {data?.length ? (
        <fieldset className="bb-srv-radios">
          <legend>{t('beatbox.srv.remove.data')}</legend>
          <label className={`bb-srv-radio${!drop ? ' on' : ''}`}>
            <input type="radio" name="bb-srv-data" checked={!drop} onChange={() => setDrop(false)} />
            <span><b>{t('beatbox.srv.remove.keep')}</b><small>{t('beatbox.srv.remove.keep_what')}</small></span>
          </label>
          <label className={`bb-srv-radio${drop ? ' on' : ''}`}>
            <input type="radio" name="bb-srv-data" checked={drop} onChange={() => setDrop(true)} />
            <span><b>{t('beatbox.srv.remove.delete')}</b><small>{t('beatbox.srv.remove.delete_what')}</small></span>
          </label>
          {drop ? <pre className="bb-srv-log" style={{ margin: 0 }}>{data.join('\n')}</pre> : null}
        </fieldset>
      ) : <p style={{ margin: 0 }}>{t('beatbox.srv.remove.nothing')}</p>}
      </div>
    </Modal>
  );
}

/* ── nothing yet ──────────────────────────────────────────────────────── */

function Empty({ srv, onPick }: { srv: Server; onPick: (definition: string) => void }) {
  const offered = ['http-static', 'dns', 'postgresql', 'smtp'].filter((id) => srv.catalog.some((each) => each.id === id));
  const ready = readyFor(srv);
  // Example address for the names tile, on this server's network.
  const network = text(bag(srv.server?.config.uses).network);
  const [example, setExample] = useState('');
  useEffect(() => {
    setExample('');
    if (!network) return;
    void call('sandbox.refresh', { id: network })
      .then((view) => setExample(sampleAddress(text(bag(view.state.facts).cidr))), () => undefined);
  }, [network]);
  return (
    <div className="bb-srv-empty">
      <h2>{t('beatbox.srv.empty.title')} <Info text={t('beatbox.srv.empty.about')} /></h2>
      <div className="bb-srv-tiles">
        {offered.map((id) => {
          const definition = srv.catalog.find((each) => each.id === id);
          return (
            <button key={id} type="button" className="bb-srv-tile" disabled={!ready(definition!)} onClick={() => onPick(id)}>
              <span className="bb-srv-icon"><Icon name={iconOf(definition)} size="lg" /></span>
              <b>{runtimeText(`beatbox.srv.start.${id}`)}</b>
              <small>{!ready(definition!) ? whyNot(definition!)
                : id === 'dns' ? t(example ? 'beatbox.srv.start.dns.what' : 'beatbox.srv.start.dns.what_plain', { address: example })
                : runtimeText(`beatbox.srv.start.${id}.what`)}</small>
            </button>
          );
        })}
        <button type="button" className="bb-srv-tile bb-srv-tile--more" onClick={() => onPick('')}>
          <span className="bb-srv-icon"><Icon name="sandbox" size="lg" /></span>
          <b>{t('beatbox.srv.start.all')}</b>
        </button>
      </div>
    </div>
  );
}

/* ── adding one ───────────────────────────────────────────────────────── */

/** Default name for a new service, from what it does. */
const suggested = (definition: Definition) =>
  (iconOf(definition) === 'globe' ? t('beatbox.srv.suggest.web')
    : iconOf(definition) === 'book' ? t('beatbox.srv.suggest.names') : definition.id);

function AddService({ srv, start, onClose }: { srv: Server; start: string; onClose: () => void }) {
  const [picked, setPicked] = useState<Definition | null>(srv.catalog.find((each) => each.id === start) ?? null);
  // ?add= can arrive before the catalog. Pick it up once it loads.
  useEffect(() => {
    if (!picked && start) { const found = srv.catalog.find((each) => each.id === start); if (found) setPicked(found); }
  }, [srv.catalog, start, picked]);
  const ready = readyFor(srv);
  return (
    <Modal title={picked ? kindOf(picked, picked.id) : t('beatbox.srv.add')} icon="server" width={720} onClose={onClose}>
      <div className="bb-srv-modal">
      {picked ? <Configure srv={srv} definition={picked} onBack={() => setPicked(null)} onDone={onClose} /> : (
        <>
          <div className="bb-srv-pick">
            {srv.catalog.map((definition) => (
              <button key={definition.id} type="button" className="bb-srv-tile" disabled={!ready(definition)}
                      onClick={() => setPicked(definition)}>
                <span className="bb-srv-icon"><Icon name={iconOf(definition)} size="lg" /></span>
                <b>{kindOf(definition, definition.id)}</b>
                <small className="bb-srv-kindabout" title={aboutOf(definition, definition.id)}>{aboutOf(definition, definition.id)}</small>
                <small className="bb-faint">{!ready(definition) ? whyNot(definition)
                  : definition.source === 'library' ? t('beatbox.srv.source.library')
                  : t(definition.runtime === 'internal' ? 'beatbox.srv.runs.inside' : 'beatbox.srv.runs.machine')}</small>
              </button>
            ))}
          </div>
        </>
      )}
      </div>
    </Modal>
  );
}

/** Config value <-> input value. */
const asField = (value: unknown) => (value == null ? '' : String(value));
/** `{shared: name}` -> name, or '' if it's a plain value. */
const sharedName = (value: unknown) => text(bag(value).shared);
function asValue(setting: Setting, value: string | boolean): unknown {
  if (setting.kind === 'bool') return value === true;
  if (setting.kind === 'number') return value === '' ? null : Number(value);
  return value;
}

function Field({ name, setting, value, onValue, preserveSecret = false, web = false, nginx = false, port = false, email = false }: {
  name: string; setting: Setting; value: string | boolean; onValue: (value: string | boolean) => void;
  preserveSecret?: boolean;
  web?: boolean;
  nginx?: boolean;
  port?: boolean;
  email?: boolean;
}) {
  const id = `bb-srv-${name}`;
  const label = email && name === 'port' ? t('beatbox.srv.key.smtp_port') : keyWord(name);
  if (name === 'https_port' && setting.kind === 'number') {
    // https_port 0 means off, so it's a switch. The port field shows once it's on.
    const on = String(value) !== '' && String(value) !== '0';
    return (
      <div className="bb-field">
        <div className="bb-sbx-toggle">
          <Switch id={id} checked={on} onChange={(next) => onValue(next ? '443' : '0')} label={keyWord(name)} />
          <label htmlFor={id}><b>{keyWord(name)} <Info text={t('beatbox.srv.key.https_port.help')} /></b></label>
        </div>
        {on ? <PortInput id={`${id}-port`} label={t('beatbox.srv.key.https_port.port')} value={String(value)}
                         onChange={onValue} defaultPort={443} /> : null}
      </div>
    );
  }
  if (setting.kind === 'bool') {
    return (
      <div className="bb-sbx-toggle">
        <Switch id={id} checked={value === true} onChange={onValue} label={keyWord(name)} />
        <label htmlFor={id}><b>{keyWord(name)}
          {name === 'accept_any_login' ? <> <Info text={t('beatbox.srv.key.accept_any_login.help')} /></> : null}</b></label>
      </div>
    );
  }
  const help = web && name === 'mode' ? t('beatbox.srv.web.help')
    : name === 'bin_directory' ? t('beatbox.srv.key.bin_directory.help')
    : name === 'database' || name === 'user' ? t(`beatbox.srv.key.${name}.help`)
    : web && name === 'root' ? t('beatbox.srv.web.folder_help')
    : nginx && name === 'upstream' ? t('beatbox.srv.key.upstream.help')
    : web && name === 'command' ? t('beatbox.srv.web.command_help')
    : (web || nginx) && name === 'health_path' ? t('beatbox.srv.web.health_help')
    : email && name === 'port' ? t('beatbox.srv.port.smtp_help')
    : email && name === 'web_port' ? t('beatbox.srv.port.inbox_help') : '';
  return (
    <label className="bb-field" htmlFor={id}>
      <span>{label}{help ? <> <Info text={help} /></> : null}</span>
      {setting.kind === 'enum' ? (
        <Select label={keyWord(name)} value={String(value)} onChange={onValue}
                options={(setting.values ?? []).map((each) => ({ value: each,
                  label: web && name === 'mode' && WEB_MODES.includes(each) ? runtimeText(`beatbox.srv.web.mode.${each}`)
                    : nginx && name === 'mode' && NGINX_MODES.includes(each) ? runtimeText(`beatbox.srv.nginx.mode.${each}`) : each }))} />
      ) : port ? (
        <PortInput id={id} label={label} value={String(value)} onChange={onValue}
          defaultPort={typeof setting.default === 'number' ? setting.default : 1} />
      ) : name === 'page' && setting.kind === 'text' ? (
        <textarea id={id} className="bb-input bb-mono" rows={8} value={String(value)}
                  onChange={(e) => onValue(e.target.value)} />
      ) : (
        <input id={id} className={`bb-input${setting.kind === 'number' ? ' bb-mono' : ''}`}
               type={setting.kind === 'secret' ? 'password' : setting.kind === 'number' ? 'number' : 'text'}
               value={String(value)} onChange={(e) => onValue(e.target.value)} />
      )}
      {setting.kind === 'secret' ? <small className="bb-faint">{t(preserveSecret ? 'beatbox.srv.edit.secret_keep' : 'beatbox.srv.secret_kept')}</small> : null}
      {port && String(value) !== '' && !validPort(String(value)) ? <small id={`${id}-range`} className="bb-srv-bad">{t('beatbox.srv.port.range')}</small> : null}
    </label>
  );
}

function Configure({ srv, definition, service, onBack, onDone }: {
  srv: Server; definition: Definition; onBack: () => void; onDone: () => void;
  service?: Service;
}) {
  const settings = Object.entries(definition.configuration ?? {});
  const portKeys = new Set(definition.interfaces.flatMap((each) => {
    const key = typeof each.port === 'string' ? /^\{([^{}]+)\}$/.exec(each.port)?.[1] : undefined;
    return key && definition.configuration[key]?.kind === 'number' ? [key] : [];
  }));
  const [name, setName] = useState(() => {
    if (service) return service.name;
    const base = suggested(definition);
    const taken = new Set((srv.services ?? []).map((service) => service.name));
    let candidate = base;
    for (let n = 2; taken.has(candidate); n += 1) candidate = `${base}-${n}`;
    return candidate;
  });
  const [values, setValues] = useState<Record<string, string | boolean>>(() => Object.fromEntries(
    settings.map(([key, setting]) => {
      const value = service ? service.config[key] : setting.default;
      return [key, setting.kind === 'bool' ? value === true
        : service && setting.kind === 'secret' ? '' : asField(value)];
    }),
  ));
  // Secret settings that point at a shared secret instead of holding one.
  const [shared, setShared] = useState<Record<string, string>>(() => Object.fromEntries(
    settings.filter(([key, setting]) => setting.kind === 'secret' && service && sharedName(service.config[key]))
      .map(([key]) => [key, sharedName(service!.config[key])]),
  ));
  const [secretNames, setSecretNames] = useState<string[]>([]);
  useEffect(() => {
    if (!settings.some(([, setting]) => setting.kind === 'secret')) return;
    void act(srv.id, 'server.settings').then((said) => setSecretNames(rows(said.secrets).map((each) => text(each.name))), () => undefined);
  }, [srv.id]);
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  const web = definition.id === 'http-static' && Boolean(definition.configuration.mode);
  const nginx = definition.id === 'nginx' && Boolean(definition.configuration.mode);
  const mode = String(values.mode || (web ? 'page' : 'folder'));
  // Hide fields that mean nothing in this mode: a page lives in the
  // service's own folder, a proxy has no folder, an app does its own HTTPS
  // (or sits behind nginx).
  const hidden = web ? [...(mode !== 'page' ? ['page'] : []), ...(mode !== 'application' ? ['command'] : []),
    ...(mode === 'page' ? ['root'] : []), ...(mode === 'application' ? ['https_port'] : [])]
    : nginx ? [mode === 'proxy' ? 'root' : 'upstream'] : [];
  const primary = web ? ['mode', ...(mode === 'page' ? ['page'] : mode === 'folder' ? ['root'] : ['root', 'command']), 'port', 'https_port']
    : nginx ? ['mode', mode === 'proxy' ? 'upstream' : 'root', 'port', 'https_port'] : null;
  const visible = settings.filter(([key]) => !hidden.includes(key));
  const needed = primary ? primary.map((key) => visible.find(([name]) => name === key)!).filter(Boolean)
    : service || definition.id === 'smtp' ? visible : visible.filter(([, setting]) => setting.default === undefined || setting.default === null);
  const rest = primary ? visible.filter(([key]) => !primary.includes(key))
    : service || definition.id === 'smtp' ? [] : visible.filter(([, setting]) => !(setting.default === undefined || setting.default === null));
  const [created, setCreated] = useState(false);
  const [blockedHost, setBlockedHost] = useState<string | null>(null);
  // 0 is a valid https_port: it means off.
  const invalidPort = [...portKeys].some((key) => !(key === 'https_port' && String(values[key]) === '0')
    && !validPort(String(values[key] ?? '')));

  async function make() {
    setBusy(true);
    setTold('');
    setBlockedHost(null);
    const config = Object.fromEntries(settings
      .filter(([key, setting]) => shared[key] || !(service && setting.kind === 'secret' && values[key] === ''))
      .map(([key, setting]) => [key, shared[key] ? { shared: shared[key] } : asValue(setting, values[key] ?? '')]));
    try {
      if (service) {
        await act(srv.id, 'service.configure', { service: service.name, config });
        await srv.load();
        onDone();
        return;
      }
      const hostId = text(bag(srv.server?.config.uses).host);
      if (definition.runtime === 'process' && hostId) {
        const host = await call('sandbox.refresh', { id: hostId });
        if (host.state.lifecycle !== 'running' || bag(host.state.facts).disk_available === false) {
          setBlockedHost(hostId);
          setTold(t(bag(host.state.facts).disk_available === false
            ? 'beatbox.srv.add.host_disk_missing' : 'beatbox.srv.add.host_not_ready', { name: host.name }));
          return;
        }
      }
      if (!created) {
        await act(srv.id, 'service.create', { definition: definition.id, name: name.trim(), config });
        setCreated(true);
      }
      await act(srv.id, 'service.start', { service: name.trim() });
      await srv.load();
      onDone();
    } catch (error) {
      // It was created, just not started. Reload so the list shows it, with the reason.
      await srv.load();
      setTold(fault(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="bb-srv-form">
      <fieldset className="bb-srv-config-fields" disabled={busy || created}>
      <label className="bb-field" htmlFor="bb-srv-name">
        <span>{t('beatbox.srv.name')} <Info text={t(service ? 'beatbox.srv.edit.name_fixed' : 'beatbox.srv.name_hint')} /></span>
        <input id="bb-srv-name" className="bb-input" value={name} disabled={Boolean(service)} onChange={(e) => setName(e.target.value)} />
      </label>
      {!service && definition.source !== 'builtin' ? <Runs definition={definition} /> : null}
      {needed.map(([key, setting]) => (
        <span key={key} className="bb-srv-form">
          {setting.kind === 'secret' && secretNames.length ? (
            <label className="bb-field">
              <span>{t('beatbox.srv.shared.from', { key: keyWord(key) })}</span>
              <Select label={t('beatbox.srv.shared.from', { key: keyWord(key) })} value={shared[key] ?? ''}
                      onChange={(value) => setShared({ ...shared, [key]: value })}
                      options={[{ value: '', label: t('beatbox.srv.shared.here') },
                        ...secretNames.map((each) => ({ value: each, label: t('beatbox.srv.shared.use', { name: each }) }))]} />
            </label>
          ) : null}
          {shared[key] ? null : (
            <Field name={key} setting={setting} value={values[key] ?? ''} preserveSecret={Boolean(service)} web={web} nginx={nginx}
                   port={portKeys.has(key)} email={definition.id === 'smtp'}
                   onValue={(value) => setValues({ ...values, [key]: value })} />
          )}
        </span>
      ))}
      {rest.length ? (
        <details className="bb-srv-more">
          <summary>{t('beatbox.srv.advanced')}</summary>
          <div className="bb-srv-form">
            {rest.map(([key, setting]) => (
              <Field key={key} name={key} setting={setting} value={values[key] ?? ''} web={web} nginx={nginx}
                     port={portKeys.has(key)} email={definition.id === 'smtp'}
                     onValue={(value) => setValues({ ...values, [key]: value })} />
            ))}
          </div>
        </details>
      ) : null}
      </fieldset>
      {told ? <div className="bb-sbx-no bb-srv-form-notice" role="alert">
        <span>{told}</span>
        {blockedHost ? <Button size="sm" icon="arrow" onClick={() => {
          remember('host', blockedHost);
          navigate('host/machine');
        }}>{t('beatbox.srv.add.open_host')}</Button> : null}
      </div> : null}
      <div className="bb-srv-formfoot">
        <Button icon="back" disabled={busy} onClick={onBack}>{t(service ? 'beatbox.files.cancel' : 'beatbox.srv.back')}</Button>
        <Button variant="primary" icon={service ? 'check' : 'play'} disabled={busy || !name.trim() || invalidPort} onClick={() => void make()}>
          {t(service ? 'beatbox.srv.edit.save' : 'beatbox.srv.put_to_work')}
        </Button>
      </div>
    </div>
  );
}

/* ── a new server ─────────────────────────────────────────────────────── */

/**
 * Is the machine cabled to some other network? Then nobody on this one can
 * reach its services. null until we know.
 */
export function useApart(host: string, network: string) {
  const [cabledTo, setCabledTo] = useState<string | null>(null);
  useEffect(() => {
    setCabledTo(null);
    if (!host) return;
    void call('sandbox.refresh', { id: host })
      .then((view) => setCabledTo(text(bag(bag(view.config.settings).network).sandbox)), () => undefined);
  }, [host]);
  return Boolean(host && network && cabledTo !== null && cabledTo !== network);
}

export function NewServer() {
  const hosts = useOthers('host');
  const networks = useOthers('network');
  const [name, setName] = useState('srv');
  const [host, setHost] = useState<string | null>(null);
  const [network, setNetwork] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  const pickedHost = host ?? hosts[0]?.id ?? '';
  // Default to the first network: without one the server can't hand out names.
  const pickedNetwork = network ?? networks[0]?.id ?? '';
  // Needs a machine or a network. With neither, the button says so.
  const needs = !pickedHost && !pickedNetwork;
  const nothingYet = !hosts.length && !networks.length;
  const apart = useApart(pickedHost, pickedNetwork);

  async function make() {
    setBusy(true);
    setTold('');
    try {
      const uses: Record<string, string> = {};
      if (pickedHost) uses.host = pickedHost;
      if (pickedNetwork) uses.network = pickedNetwork;
      const made = await call('sandbox.create', {
        type: 'server', name: name.trim(), cpu: 0, memory: 0, settings: {}, uses,
      });
      await call('sandbox.transition', { id: made.id, transition: 'start' });
      remember('server', made.id);
      navigate('server/services');
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }

  return (
    <div className="bb-srv-new">
      <div className="bb-pagehead-title">
        <h1 className="bb-t-title" style={{ margin: 0 }}>{t('beatbox.srv.new.title')}</h1>
        <Info text={t('beatbox.srv.new.first')} />
      </div>
      <label className="bb-field" htmlFor="bb-srv-newname">
        <span>{t('beatbox.srv.name')}</span>
        <input id="bb-srv-newname" className="bb-input" value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      <fieldset className="bb-srv-radios">
        <legend>{t('beatbox.srv.new.machine')} <Info text={`${t('beatbox.srv.new.none_machine')} ${t('beatbox.srv.new.fixed')}`} /></legend>
        {hosts.map((row) => (
          <label key={row.id} className={`bb-srv-radio${pickedHost === row.id ? ' on' : ''}`}>
            <input type="radio" name="bb-srv-host" checked={pickedHost === row.id} onChange={() => setHost(row.id)} />
            <span><b>{row.name}</b><small>{runtimeText(`beatbox.plain.state.${row.lifecycle}`)}</small></span>
          </label>
        ))}
        <label className={`bb-srv-radio${pickedHost === '' ? ' on' : ''}`}>
          <input type="radio" name="bb-srv-host" checked={pickedHost === ''} onChange={() => setHost('')} />
          <span><b>{t('beatbox.srv.new.none')}</b></span>
        </label>
      </fieldset>
      {networks.length ? (
        <fieldset className="bb-srv-radios">
          <legend>{t('beatbox.srv.new.network')} <Info text={t('beatbox.srv.new.network_why')} /></legend>
          {networks.map((row) => (
            <label key={row.id} className={`bb-srv-radio${pickedNetwork === row.id ? ' on' : ''}`}>
              <input type="radio" name="bb-srv-net" checked={pickedNetwork === row.id} onChange={() => setNetwork(row.id)} />
              <span><b>{row.name}</b></span>
            </label>
          ))}
          <label className={`bb-srv-radio${pickedNetwork === '' ? ' on' : ''}`}>
            <input type="radio" name="bb-srv-net" checked={pickedNetwork === ''} onChange={() => setNetwork('')} />
            <span><b>{t('beatbox.srv.new.none')}</b></span>
          </label>
        </fieldset>
      ) : null}
      {apart ? <p className="bb-amber" role="note" style={{ margin: 0, fontSize: 13 }}>
        {t('beatbox.srv.new.apart', {
          machine: hosts.find((row) => row.id === pickedHost)?.name ?? '',
          network: networks.find((row) => row.id === pickedNetwork)?.name ?? '',
        })}
      </p> : null}
      {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
      <div className="bb-srv-formfoot">
        {needs ? (
          <span className="bb-srv-needs">
            <span className="bb-faint">{t(nothingYet ? 'beatbox.srv.new.needs_network' : 'beatbox.srv.new.needs_pick')}</span>
            {nothingYet ? <span className="bb-srv-row">
              <Button size="sm" icon="arrow" onClick={() => navigate('host/machine')}>{t('beatbox.srv.new.make_machine')}</Button>
              <Button size="sm" icon="arrow" onClick={() => navigate('network/overview')}>{t('beatbox.srv.new.make_network')}</Button>
            </span> : null}
          </span>
        ) : <span />}
        <Button variant="primary" icon="plus" disabled={busy || !name.trim() || needs}
                onClick={() => void make()}>{t('beatbox.srv.new.make')}</Button>
      </div>
    </div>
  );
}
