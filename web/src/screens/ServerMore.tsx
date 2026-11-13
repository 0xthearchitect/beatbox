import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { Button, Icon, Info, Pill, PortInput, Select, Switch } from '../components';
import { call } from '../bridge/client';
import type { ReasonView } from '../bridge/types';
import { fault, say, t, runtimeText } from '../i18n';
import { navigate, useRouteParams } from '../router';
import {
  NewServer, aboutOf, act, argvText, bag, capWord, iconOf, kindOf, reach, readyFor, rows, startOrder, text, useApart, useCheck,
  useOthers, useServer, type Check, type Definition, type Endpoint, type Server, type Service,
} from './Server';

/** The Server's other pages: Library and Configuration, plus Try from and service capabilities. */

/** Shared page frame. With no server yet, it shows the "make one" screen instead. */
function Frame({ page, children, action }: { page: 'library' | 'settings' | 'try'; children: (srv: Server) => ReactNode; action?: ReactNode }) {
  const srv = useServer();
  const making = useRouteParams().get('new') === '1';
  if (making || (srv.list && !srv.list.length)) return <NewServer />;
  if (!srv.server) return null;
  return (
    <>
      <div className="bb-srv-pagehead">
        <div>
          <div className="bb-pagehead-title">
            <h1 className="bb-t-title bb-srv-title">{t(`beatbox.screen.server.${page}.title`)}</h1>
            <Info text={t(`beatbox.srv.${page}.lede`)} />
          </div>
        </div>
        {action}
      </div>
      {srv.told ? <p className="bb-sbx-no">{srv.told}</p> : null}
      {children(srv)}
    </>
  );
}

/* ── Experimentar ─────────────────────────────────────────────────────── */

/** Try moved into the service panel. Old links land on the overview. */
export function ServerTry() {
  useEffect(() => { navigate('server/services'); }, []);
  return null;
}

/** Which probe fits this endpoint. null = nothing we can try. */
function probeKind(end: Endpoint | undefined): 'http' | 'dns' | 'tcp' | 'postgresql' | 'smtp' | null {
  if (!end) return null;
  if (end.protocol === 'http') return 'http';
  if (end.protocol === 'dns') return 'dns';
  if (end.protocol === 'postgresql') return 'postgresql';
  if (end.protocol === 'smtp') return 'smtp';
  return end.transport === 'tcp' ? 'tcp' : null;
}

interface Tried {
  ok: boolean;
  ms: number;
  said: string;
  answer: string;
  from: string;
  /** From a light PC we only learn the port opens; it can't log in. */
  light: boolean;
  verdict: { allowed: boolean; rule: string | null } | null;
}

/** Where a try can start from: a running VM, or a light PC on the server's network. */
interface Source { value: string; name: string; light: boolean; id: string }

/**
 * The same probe, run from a light PC's terminal (crates/network/src/light.rs):
 * curl for a page, nslookup for a name, nc for a port, and the SMTP greeting
 * for email.
 */
async function fromLight(network: string, node: string, kind: string, end: Endpoint, name: string) {
  const line = kind === 'http' ? `curl http://${end.address}:${end.port}/`
    : kind === 'dns' ? `nslookup ${name}` : `nc ${end.address} ${end.port}`;
  const started = Date.now();
  const lines = rows(bag(await act(network, 'light.run', { node, line })).lines);
  const ms = Date.now() - started;
  const found = (code: string) => lines.find((each) => each.code === code);
  if (kind === 'http') {
    const page = found('curl.answer');
    const status = text(page?.status).split(' ').slice(1).join(' ');
    return { ok: /^[23]\d\d/.test(status), ms, said: status, answer: '' };
  }
  if (kind === 'dns') {
    const named = lines.find((each) => each.code === 'dns.answer' && !text(each.address).includes(':')) ?? found('dns.answer');
    return { ok: Boolean(named), ms, said: '', answer: text(named?.address) };
  }
  const open = found('tcp.open');
  const said = text(open?.said);
  return { ok: kind === 'smtp' ? said.startsWith('220 ') : Boolean(open), ms: open ? Number(open.ms ?? ms) : ms, said, answer: '' };
}

function databaseSaid(code: string): string {
  switch (code) {
    case 'authentication_failed': return t('beatbox.srv.try.auth_failed');
    case 'database_missing': return t('beatbox.srv.try.database_missing');
    case 'connection_failed': return t('beatbox.srv.try.connection_failed');
    case 'tls_error': case 'tls_required': return t('beatbox.srv.try.tls_failed');
    case 'unsupported_authentication': return t('beatbox.srv.try.auth_unsupported');
    default: return t('beatbox.srv.try.query_failed');
  }
}

/** Try the service from another machine, like a real user would. */
export function FromElsewhere({ srv, service }: { srv: Server; service: Service }) {
  const hosts = useOthers('host');
  const uses = bag(srv.server?.config.uses);
  const network = text(uses.network);
  // Light PCs live as long as their network. Re-list when it changes state.
  const [lights, setLights] = useState<string[]>([]);
  const networkState = useOthers('network').find((each) => each.id === network)?.lifecycle;
  useEffect(() => {
    setLights([]);
    if (!network || networkState !== 'running') return;
    void call('sandbox.refresh', { id: network })
      .then((view) => setLights(rows(bag(view.state.facts).nodes).filter((node) => node.light).map((node) => text(node.node))),
        () => undefined);
  }, [network, networkState]);
  const machines: Source[] = [
    ...hosts.filter((host) => host.lifecycle === 'running' && host.id !== uses.host)
      .map((host) => ({ value: `host:${host.id}`, name: host.name, light: false, id: host.id })),
    ...lights.map((node) => ({ value: `light:${node}`, name: node, light: true, id: node })),
  ];
  const [from, setFrom] = useState('');
  const [name, setName] = useState('intranet.lab');
  const [busy, setBusy] = useState(false);
  const [tried, setTried] = useState<Tried | null>(null);
  const [told, setTold] = useState('');
  const [endpoint, setEndpoint] = useState('0');
  const [username, setUsername] = useState('postgres');
  const [database, setDatabase] = useState('postgres');
  const [password, setPassword] = useState('');
  const end = service.endpoints[Number(endpoint)];
  const kind = probeKind(end);
  const picked = machines.find((machine) => machine.value === from) ?? machines[0];

  async function run() {
    if (!picked || !end || !kind) return;
    setBusy(true);
    setTold('');
    try {
      const answer = picked.light ? await fromLight(network, picked.name, kind, end, name.trim())
        : await act(picked.id, 'network.probe', {
          kind, address: end.address, port: end.port, ...(kind === 'dns' ? { name: name.trim() } : {}),
          ...(kind === 'postgresql' ? { username, database, password } : {}),
        });
      // If there's a network, find the flow it logged to show allowed/blocked and by which rule.
      let verdict: Tried['verdict'] = null;
      if (uses.network) {
        const flows = rows(bag(await act(text(uses.network), 'network.flows')).flows);
        const flow = flows
          .filter((each) => each.destination === end.address && Number(each.port) === end.port
            && text(each.source_node) === picked.name)
          .sort((a, b) => Number(a.quiet_ms ?? 0) - Number(b.quiet_ms ?? 0))[0];
        if (flow) verdict = { allowed: flow.action === 'allow', rule: flow.rule == null ? null : text(flow.rule) };
      }
      setTried({
        ok: answer.ok === true, ms: Number(answer.ms ?? 0), said: text(answer.said), answer: text(answer.answer),
        from: picked.name, light: picked.light, verdict,
      });
    } catch (error) {
      setTold(fault(error));
      setTried(null);
    }
    setBusy(false);
  }

  const headline = !tried ? ''
    : !tried.ok && kind === 'http' && /^\d{3}/.test(tried.said) ? t('beatbox.srv.try.http_error', { from: tried.from })
    : !tried.ok ? t('beatbox.srv.try.failed', { from: tried.from })
    : kind === 'http' ? t('beatbox.srv.try.http_ok', { from: tried.from })
    : kind === 'dns' ? t('beatbox.srv.try.dns_ok', { from: tried.from })
    : kind === 'postgresql' && tried.light ? t('beatbox.srv.try.tcp_ok', { from: tried.from })
    : kind === 'postgresql' ? t('beatbox.srv.try.database_ok', { from: tried.from })
    : kind === 'smtp' ? t('beatbox.srv.try.smtp_ok', { from: tried.from })
    : t('beatbox.srv.try.tcp_ok', { from: tried.from });
  const detail = !tried ? ''
    : kind === 'http' && tried.said ? t('beatbox.srv.try.http_said', { said: tried.said, ms: tried.ms })
    : kind === 'dns' && tried.answer ? t('beatbox.srv.try.dns_said', { name: name.trim(), answer: tried.answer, ms: tried.ms })
    : kind === 'postgresql' && tried.light ? t(tried.ok ? 'beatbox.srv.try.light_port' : 'beatbox.srv.try.connection_failed', { ms: tried.ms })
    : kind === 'smtp' && tried.light && tried.said ? t('beatbox.srv.try.http_said', { said: tried.said, ms: tried.ms })
    : kind === 'postgresql' && !tried.ok ? databaseSaid(tried.said)
    : t(`beatbox.srv.try.said.${tried.ok ? 'ok' : 'no'}`, { ms: tried.ms });

  // How a try works and what a light PC can't do: in the ⓘ, not on screen.
  const how = [t('beatbox.srv.try.how'), picked?.light ? t(kind === 'dns' ? 'beatbox.srv.try.light_dns'
    : kind === 'postgresql' ? 'beatbox.srv.try.light_login' : 'beatbox.srv.try.light_what') : ''].filter(Boolean).join(' ');
  return (
    <div className="bb-srv-sidepart bb-srv-try">
      <small>{t('beatbox.srv.try.from')} <Info text={how} /></small>
      {!kind ? <span className="bb-faint">{t('beatbox.srv.try.cannot')}</span>
        : !machines.length ? (
          <div className="bb-srv-row">
            <span className="bb-faint">{t('beatbox.srv.try.no_machine')}</span>
            <Button size="sm" icon="cpu" onClick={() => navigate('host/machine')}>{t('beatbox.srv.try.go_machines')}</Button>
          </div>
        ) : (
        <>
          <div className="bb-srv-tryrow">
            {service.endpoints.length > 1 ? (
              <Select label={t('beatbox.srv.try.endpoint')} value={endpoint}
                onChange={(value) => { setEndpoint(value); setTried(null); }}
                options={service.endpoints.map((each, index) => ({ value: String(index), label: reach(service, each),
                  hint: each.protocol === 'smtp' ? t('beatbox.srv.try.smtp_endpoint')
                    : each.protocol === 'http' && service.endpoints.some((end) => end.protocol === 'smtp') ? t('beatbox.srv.try.inbox_endpoint')
                    : each.protocol ?? each.transport,
                }))} />
            ) : null}
            {kind === 'dns' ? (
              <input className="bb-input bb-mono" aria-label={t('beatbox.srv.try.ask_name')} placeholder={t('beatbox.srv.try.ask_name')}
                     value={name} onChange={(e) => setName(e.target.value)} />
            ) : null}
            <Select label={t('beatbox.srv.try.from')} value={picked?.value ?? ''}
                    onChange={(value) => { setFrom(value); setTried(null); }}
                    options={machines.map((machine) => ({ value: machine.value, label: machine.name,
                      hint: t(machine.light ? 'beatbox.srv.try.light' : 'beatbox.srv.try.virtual') }))} />
            <Button disabled={busy || (kind === 'dns' && !name.trim())} onClick={() => void run()}>
              {busy ? t('beatbox.srv.try.trying') : t(tried ? 'beatbox.srv.try.again' : 'beatbox.srv.try.run')}
            </Button>
          </div>
          {kind === 'postgresql' && !picked?.light ? (
            <div className="bb-srv-trylogin">
              <input className="bb-input" aria-label={t('beatbox.srv.try.username')} placeholder={t('beatbox.srv.try.username')} value={username} onChange={(event) => { setUsername(event.target.value); setTried(null); }} />
              <input className="bb-input" aria-label={t('beatbox.srv.try.database')} placeholder={t('beatbox.srv.try.database')} value={database} onChange={(event) => { setDatabase(event.target.value); setTried(null); }} />
              <input type="password" className="bb-input" autoComplete="off" aria-label={t('beatbox.srv.try.password')} placeholder={t('beatbox.srv.try.password')} value={password} onChange={(event) => { setPassword(event.target.value); setTried(null); }} />
            </div>
          ) : null}
          {tried ? (
            <div className={`bb-srv-result ${tried.ok ? 'bb-srv-result--ok' : 'bb-srv-result--bad'}`}>
              <b className={tried.ok ? 'bb-srv-good' : 'bb-srv-bad'}>
                <Icon name={tried.ok ? 'check' : 'alert'} size="sm" />{headline}
              </b>
              <span className="bb-lede">{detail}</span>
              <div className="bb-srv-path">
                <span>{tried.from}</span><Icon name="arrow" size="xs" />
                {tried.verdict ? (
                  <span className={tried.verdict.allowed ? 'bb-srv-good' : 'bb-srv-bad'}>
                    {t(tried.verdict.allowed ? 'beatbox.srv.try.let' : 'beatbox.srv.try.blocked',
                      { rule: tried.verdict.rule ?? t('beatbox.srv.try.default_rule') })}
                  </span>
                ) : <span>{t('beatbox.srv.try.no_verdict')}</span>}
                <Icon name="arrow" size="xs" />
                <span>{hosts.find((host) => host.id === uses.host)?.name && kind !== 'dns' ? hosts.find((host) => host.id === uses.host)?.name : srv.server?.name} · {end?.address}:{end?.port}</span>
              </div>
            </div>
          ) : null}
          {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
        </>
      )}
    </div>
  );
}

/** A small form per capability the service has. */
export function Asks({ srv, service, definition }: { srv: Server; service: Service; definition: Definition | undefined }) {
  const [capabilities, setCapabilities] = useState<{ capability: string; inputs: { name: string; kind: string; required?: boolean }[] }[] | null>(null);
  useEffect(() => {
    void act(srv.id, 'service.describe', { service: service.name })
      .then((said) => setCapabilities(rows(said.capabilities).map((each) => ({
        capability: text(each.capability),
        inputs: rows(each.inputs).map((input) => ({ name: text(input.name), kind: text(input.kind), required: input.required !== false })),
      }))), () => setCapabilities([]));
  }, [srv.id, service.name]);
  return (
    <section className="bb-srv-box">
      <h2>{t('beatbox.srv.try.can')}</h2>
      {capabilities === null ? null : capabilities.length ? capabilities.map((each) => (
        <Ask key={each.capability} srv={srv} service={service} capability={each.capability} inputs={each.inputs} />
      )) : <p className="bb-lede" style={{ margin: 0 }}>{t('beatbox.srv.try.nothing', { kind: kindOf(definition, service.definition) })}</p>}
    </section>
  );
}

/** Engine field names that get a translated label. */
const FIELDS = ['name', 'address', 'words', 'search'];
const fieldWord = (field: string) => (FIELDS.includes(field) ? runtimeText(`beatbox.srv.field.${field}`) : field);

function Ask({ srv, service, capability, inputs }: {
  srv: Server; service: Service; capability: string; inputs: { name: string; kind: string; required?: boolean }[];
}) {
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [said, setSaid] = useState<string | null>(null);
  const [told, setTold] = useState('');
  async function run() {
    setBusy(true);
    setTold('');
    try {
      const args = Object.fromEntries(inputs.filter((input) => (values[input.name] ?? '') !== '')
        .map((input) => [input.name, input.kind === 'number' ? Number(values[input.name]) : values[input.name]]));
      const answer = await act(srv.id, 'service.call', { service: service.name, capability, arguments: args });
      const shown = Object.entries(answer).filter(([key]) => key !== 'service' && key !== 'capability');
      const stdout = answer.stdout;
      const stderr = text(answer.stderr).trim();
      const failed = typeof answer.exit_code === 'number' && answer.exit_code !== 0;
      setTold(failed ? t('beatbox.srv.action_failed', { code: Number(answer.exit_code) }) : '');
      setSaid(typeof stdout === 'string' ? [stdout.trim(), stderr].filter(Boolean).join('\n')
        : shown.map(([key, value]) => `${fieldWord(key)}: ${text(value) || '—'}`).join('\n'));
    } catch (error) {
      setTold(fault(error));
      setSaid(null);
    }
    setBusy(false);
  }
  return (
    <div className="bb-srv-ask">
      <b>{capWord(capability)}</b>
      <div className="bb-srv-row">
        {inputs.map((input) => (
          input.name === 'sql' ? <textarea key={input.name} className="bb-input bb-mono" rows={4} aria-label="SQL" placeholder="SQL" value={values[input.name] ?? ''}
            onChange={(event) => setValues({ ...values, [input.name]: event.target.value })} /> : <input key={input.name} className="bb-input" aria-label={fieldWord(input.name)} placeholder={fieldWord(input.name)}
                 type={input.kind === 'number' ? 'number' : 'text'} value={values[input.name] ?? ''}
                 onChange={(e) => setValues({ ...values, [input.name]: e.target.value })} />
        ))}
        <Button disabled={busy || inputs.some((input) => input.required && !(values[input.name] ?? '').trim())}
                onClick={() => void run()}>{t('beatbox.srv.try.send')}</Button>
      </div>
      {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
      {said !== null ? <pre className="bb-srv-log" style={{ maxHeight: 160 }}>{said || '—'}</pre> : null}
    </div>
  );
}

/* ── Biblioteca ───────────────────────────────────────────────────────── */

export function ServerLibrary() {
  const [making, setMaking] = useState(false);
  return (
    <Frame page="library" action={<Button variant="primary" icon="plus" onClick={() => setMaking(true)}>{t('beatbox.srv.lib.make')}</Button>}>
      {(srv) => <Library srv={srv} making={making} onClose={() => setMaking(false)} />}
    </Frame>
  );
}

type Ready = 'ready' | 'missing' | 'unknown';

/** Can this kind run here? Based on the last check, so it can be stale. */
function readiness(definition: Definition, check: Check | null): { state: Ready; missing: string[] } {
  if (definition.runtime === 'internal') {
    if (!check) return { state: 'unknown', missing: [] };
    return { state: check.network?.state === 'running' ? 'ready' : 'missing', missing: [] };
  }
  if (!check?.host || check.host.state !== 'running') return { state: 'unknown', missing: [] };
  const missing = (definition.requires ?? []).filter((program) => check.host!.programs[program] === false);
  return { state: missing.length ? 'missing' : 'ready', missing };
}

const FILTERS = ['all', 'ready', 'missing', 'yours'] as const;

function Library({ srv, making, onClose }: { srv: Server; making: boolean; onClose: () => void }) {
  const { check, recheck } = useCheck(srv);
  const hosts = useOthers('host');
  const [refused, setRefused] = useState<{ file: string; why: string }[]>([]);
  const [saved, setSaved] = useState('');
  const [filter, setFilter] = useState<(typeof FILTERS)[number]>('all');
  const loadRefused = useCallback(async () => {
    const catalog = await act(srv.id, 'service.catalog');
    setRefused(rows(catalog.refused).map((row) => ({
      file: text(row.file).split(/[\\/]/).pop() ?? '',
      why: say(bag(row.reason) as unknown as ReasonView, text(bag(row.reason).code)),
    })));
  }, [srv.id]);
  useEffect(() => { void loadRefused().catch(() => undefined); }, [loadRefused, srv.catalog]);
  const machine = hosts.find((row) => row.id === bag(srv.server?.config.uses).host)?.name ?? '';
  const counts = {
    all: srv.catalog.length,
    ready: srv.catalog.filter((each) => readiness(each, check).state === 'ready').length,
    missing: srv.catalog.filter((each) => readiness(each, check).state === 'missing').length,
    yours: srv.catalog.filter((each) => each.source === 'library').length,
  };
  const shown = (list: Definition[]) => list.filter((each) => filter === 'all'
    || (filter === 'yours' ? each.source === 'library' : readiness(each, check).state === filter));
  const builtin = shown(srv.catalog.filter((each) => each.source === 'builtin'));
  const yours = shown(srv.catalog.filter((each) => each.source === 'library'));
  const use = (definition: Definition) => navigate(`server/services?add=${encodeURIComponent(definition.id)}`);
  return (
    <div className="bb-srv-libbody">
      <div className="bb-srv-stackpage" style={{ flex: 1, minWidth: 0 }}>
        {saved ? <p className="bb-srv-saved"><Icon name="check" size="sm" />{t('beatbox.srv.lib.saved', { file: saved })}</p> : null}
        <div className="bb-srv-filters">
          <div className="bb-srv-chips" role="tablist" aria-label={t('beatbox.srv.lib.show')}>
            {FILTERS.map((each) => (
              <button key={each} type="button" role="tab" aria-selected={filter === each}
                      className={`bb-srv-chipbtn${filter === each ? ' on' : ''}`} onClick={() => setFilter(each)}>
                {runtimeText(`beatbox.srv.lib.filter.${each}`)} · {counts[each]}
              </button>
            ))}
          </div>
          <span className="bb-faint">{check?.host?.state === 'running'
            ? t('beatbox.srv.lib.checked_on', { machine }) : machine ? t('beatbox.srv.lib.machine_off', { machine }) : ''}</span>
          {check?.host?.state === 'running' ? <Button size="sm" icon="refresh" onClick={() => void recheck()}>{t('beatbox.srv.helpers.check_again')}</Button> : null}
        </div>
        {builtin.length ? (
          <section className="bb-srv-group">
            <h2>{t('beatbox.srv.lib.builtin')}</h2>
            <div className="bb-srv-kinds">{builtin.map((each) => <Kind key={each.id} srv={srv} definition={each} check={check} machine={machine} onUse={() => use(each)} />)}</div>
          </section>
        ) : null}
        {filter === 'all' || filter === 'yours' || yours.length ? (
          <section className="bb-srv-group">
            <h2>{t('beatbox.srv.lib.yours')}</h2>
            {yours.length ? <div className="bb-srv-kinds">{yours.map((each) => <Kind key={each.id} srv={srv} definition={each} check={check} machine={machine} onUse={() => use(each)} />)}</div>
              : <p className="bb-lede" style={{ margin: 0 }}>{t('beatbox.srv.lib.yours_none')}</p>}
          </section>
        ) : null}
        {refused.length && (filter === 'all' || filter === 'yours') ? (
          <section className="bb-srv-group">
            <h2>{t('beatbox.srv.lib.refused')}</h2>
            {refused.map((each) => (
              <div key={each.file} className="bb-srv-refused">
                <Icon name="alert" size="sm" />
                <span><b className="bb-mono">{each.file}</b><span>{each.why}</span></span>
              </div>
            ))}
          </section>
        ) : null}
      </div>
      {making ? <Make srv={srv} onClose={onClose}
                      onSaved={(file, id, andUse) => { setSaved(file); onClose(); void srv.load().then(() => { if (andUse) navigate(`server/services?add=${encodeURIComponent(id)}`); }); }} /> : null}
    </div>
  );
}

function Kind({ srv, definition, check, machine, onUse }: {
  srv: Server; definition: Definition; check: Check | null; machine: string; onUse: () => void;
}) {
  const ready = readiness(definition, check);
  const internal = definition.runtime === 'internal';
  const port = definition.interfaces[0];
  const where = t(internal ? 'beatbox.srv.runs.inside' : 'beatbox.srv.runs.machine');
  const portWord = port ? t('beatbox.srv.lib.port', { port: text(bag(definition.configuration?.port).default ?? port.port) }) : '';
  const using = (srv.services ?? []).filter((service) => service.definition === definition.id).map((service) => service.name);
  const programs = check?.host?.programs ?? {};
  const can = readyFor(srv)(definition);
  return (
    <article className="bb-srv-kind">
      <div className="bb-srv-kindtop">
        <span className="bb-srv-icon"><Icon name={iconOf(definition)} size="lg" /></span>
        <span><b>{kindOf(definition, definition.id)}</b><small>{where}{portWord ? ` · ${portWord}` : ''}</small></span>
      </div>
      <p className="bb-srv-kindabout" title={aboutOf(definition, definition.id)}>{aboutOf(definition, definition.id)}</p>
      {definition.source !== 'builtin' && definition.runs?.command ? (
        <>
          <small className="bb-faint">{t('beatbox.srv.lib.as_root')}</small>
          <code className="bb-srv-kindcode">{argvText(definition.runs.command)}</code>
        </>
      ) : null}
      <ul className="bb-srv-needchips" aria-label={t('beatbox.srv.reqs.title')}>
        {internal ? <li className={ready.state === 'ready' ? 'ok' : ready.state === 'missing' ? 'no' : ''}>{t('beatbox.srv.lib.needs_network')}</li>
          : (definition.requires ?? []).map((program) => (
            <li key={program} className={programs[program] === true ? 'ok' : programs[program] === false ? 'no' : ''}>{program}</li>
          ))}
      </ul>
      {ready.state === 'missing' && !internal ? <small className="bb-amber">{t('beatbox.srv.lib.install', { programs: ready.missing.join(', '), machine })}</small> : null}
      <div className="bb-srv-kindfoot">
        <small>{using.length ? t('beatbox.srv.lib.in_use', { names: using.join(', ') }) : t('beatbox.srv.lib.not_used')}</small>
        <Button size="sm" disabled={!can} title={can ? undefined : t(internal ? 'beatbox.srv.needs_network' : 'beatbox.srv.needs_machine')}
                onClick={onUse}>{t('beatbox.srv.lib.use')}</Button>
      </div>
    </article>
  );
}

/** Typed name -> id: lowercase, digits and `-`, starting with a letter. */
const slug = (name: string) => {
  const base = name.normalize('NFD').replace(/[̀-ͯ]/g, '').toLowerCase()
    .replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 40);
  return /^[a-z]/.test(base) ? base : `s-${base}`.slice(0, 40);
};
/** Field name -> config key: lowercase, digits and `_`. */
const keyOf = (name: string) => {
  const base = name.normalize('NFD').replace(/[̀-ͯ]/g, '').toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '');
  return /^[a-z]/.test(base) ? base : `f_${base}`;
};

const FIELD_KINDS = ['text', 'number', 'secret', 'enum'] as const;
interface FieldRow { key: string; kind: (typeof FIELD_KINDS)[number]; value: string }

function Make({ srv, onClose, onSaved }: { srv: Server; onClose: () => void; onSaved: (file: string, id: string, andUse: boolean) => void }) {
  const hosts = useOthers('host');
  const machine = hosts.find((row) => row.id === bag(srv.server?.config.uses).host)?.name ?? '';
  const [name, setName] = useState('');
  const [about, setAbout] = useState('');
  const [program, setProgram] = useState('');
  const [port, setPort] = useState('3610');
  const [transport, setTransport] = useState<'tcp' | 'udp'>('tcp');
  const [health, setHealth] = useState<'tcp' | 'http' | 'none'>('tcp');
  const [fields, setFields] = useState<FieldRow[]>([]);
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  const [found, setFound] = useState<Record<string, boolean> | null>(null);
  const checks = transport === 'udp' ? ['none'] as const : ['tcp', 'http', 'none'] as const;
  const check = (checks as readonly string[]).includes(health) ? health : 'none';
  const words = program.trim().split(/\s+/).filter(Boolean).map((word) => word.replace(/\{porta\}/g, '{port}'));
  const configuration: Record<string, unknown> = { port: { kind: 'number', default: Number(port) || 0 } };
  for (const field of fields) {
    const key = keyOf(field.key || field.kind);
    if (key === 'port') continue;
    configuration[key] = field.kind === 'secret' ? { kind: 'secret' }
      : field.kind === 'enum' ? { kind: 'enum', values: field.value.split(',').map((each) => each.trim()).filter(Boolean), default: field.value.split(',')[0]?.trim() || null }
      : field.kind === 'number' ? { kind: 'number', default: field.value === '' ? null : Number(field.value) }
      : { kind: 'text', default: field.value === '' ? null : field.value };
  }
  const definition = {
    id: slug(name || 'servico'),
    version: '1',
    name: name.trim(),
    description: about.trim(),
    author: '',
    runtime: { type: 'process', command: words },
    ...(words[0] ? { requires: [words[0]] } : {}),
    configuration,
    interfaces: [{ transport, port: '{port}' }],
    ...(check === 'tcp' ? { health: { type: 'tcp', port: '{port}' } }
      : check === 'http' ? { health: { type: 'http', port: '{port}', path: '/' } } : {}),
  };

  // Check as you type that the program (and any absolute path in the
  // command) exists on the machine. We only look, never run it.
  const asked = [...new Set([words[0], ...words.slice(1).filter((word) => word.startsWith('/'))].filter(Boolean))] as string[];
  const askedKey = asked.join('\n');
  useEffect(() => {
    setFound(null);
    if (!asked.length || srv.server?.state.lifecycle !== 'running') return;
    const timer = window.setTimeout(() => {
      void act(srv.id, 'server.check', { programs: asked })
        .then((said) => { const host = bag(said.host); setFound(host.state === 'running' ? bag(host.programs) as Record<string, boolean> : null); }, () => setFound(null));
    }, 600);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [askedKey, srv.id]);

  async function save(andUse: boolean) {
    setBusy(true);
    setTold('');
    try {
      const said = await act(srv.id, 'service.define', { definition });
      onSaved(text(said.file).split(/[\\/]/).pop() ?? '', definition.id, andUse);
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  const ready = Boolean(name.trim() && words.length && Number(port) > 0);

  return (
    <aside className="bb-srv-make" aria-label={t('beatbox.srv.lib.make_title')}>
      <div className="bb-srv-makehead">
        <h2>{t('beatbox.srv.lib.make_title')}</h2>
        <button type="button" className="bb-srv-copy" aria-label={t('beatbox.srv.lib.close')} onClick={onClose}><Icon name="close" size="sm" /></button>
      </div>
      <fieldset className="bb-srv-makepart">
        <legend>{t('beatbox.srv.lib.part.what')}</legend>
        <label className="bb-field"><span>{t('beatbox.srv.lib.name')}</span>
          <input className="bb-input" value={name} placeholder={t('beatbox.srv.lib.name_example')} onChange={(e) => setName(e.target.value)} /></label>
        <label className="bb-field"><span>{t('beatbox.srv.lib.about')}</span>
          <input className="bb-input" value={about} onChange={(e) => setAbout(e.target.value)} /></label>
      </fieldset>
      <fieldset className="bb-srv-makepart">
        <legend>{t('beatbox.srv.lib.part.runs')}</legend>
        <label className="bb-field"><span>{t('beatbox.srv.lib.program')} <Info text={t('beatbox.srv.lib.program_hint')} /></span>
          <input className="bb-input bb-mono" value={program} placeholder="python3 /srv/eco.py {porta}" onChange={(e) => setProgram(e.target.value)} /></label>
        {asked.length ? (
          <ul className="bb-srv-needchips" aria-label={t('beatbox.srv.reqs.title')}>
            {asked.map((each) => (
              <li key={each} className={found?.[each] === true ? 'ok' : found?.[each] === false ? 'no' : ''}>{each}</li>
            ))}
          </ul>
        ) : null}
        {found && asked.some((each) => found[each] === false) ? <small className="bb-amber">{t('beatbox.srv.lib.not_on_machine', { machine })}</small> : null}
      </fieldset>
      <fieldset className="bb-srv-makepart">
        <legend>{t('beatbox.srv.lib.part.where')}</legend>
        <div className="bb-srv-row">
          <label className="bb-field" style={{ width: 150 }}><span>{t('beatbox.srv.key.port')}</span>
            <PortInput id="lib-port" label={t('beatbox.srv.key.port')} value={port} onChange={setPort} /></label>
          <div className="bb-field" style={{ flex: 1 }}><span>{t('beatbox.srv.lib.speaks')}</span>
            <div className="bb-srv-tabs" role="radiogroup" aria-label={t('beatbox.srv.lib.speaks')} style={{ marginTop: 0 }}>
              {(['tcp', 'udp'] as const).map((each) => (
                <button key={each} type="button" role="radio" aria-checked={transport === each}
                        className={`bb-srv-tab${transport === each ? ' on' : ''}`} onClick={() => setTransport(each)}>{each.toUpperCase()}</button>
              ))}
            </div></div>
        </div>
        <fieldset className="bb-srv-radios">
          <legend>{t('beatbox.srv.lib.health')}</legend>
          {checks.map((each) => (
            <label key={each} className={`bb-srv-radio${check === each ? ' on' : ''}`}>
              <input type="radio" name="bb-srv-health" checked={check === each} onChange={() => setHealth(each)} />
              <span><b>{t(`beatbox.srv.lib.health.${each}`)}</b></span>
            </label>
          ))}
        </fieldset>
      </fieldset>
      <fieldset className="bb-srv-makepart">
        <legend>{t('beatbox.srv.lib.part.asks')}</legend>
        <div className="bb-srv-fieldrow bb-srv-fieldrow--fixed"><span className="bb-mono">port</span><small>{t('beatbox.srv.lib.field.port')}</small></div>
        {fields.map((field, index) => (
          <div key={index} className="bb-srv-fieldrow">
            <input className="bb-input bb-mono" aria-label={t('beatbox.srv.lib.field.name')} placeholder={t('beatbox.srv.lib.field.name')} value={field.key}
                   onChange={(e) => setFields(fields.map((each, at) => (at === index ? { ...each, key: e.target.value } : each)))} />
            <Select label={t('beatbox.srv.lib.field.kind')} value={field.kind}
                    onChange={(value) => setFields(fields.map((each, at) => (at === index ? { ...each, kind: value as FieldRow['kind'] } : each)))}
                    options={FIELD_KINDS.map((kind) => ({ value: kind, label: runtimeText(`beatbox.srv.lib.field.${kind}`) }))} />
            {field.kind === 'secret' ? <small className="bb-faint">{t('beatbox.srv.lib.field.secret_hint')}</small>
              : <input className="bb-input" aria-label={t('beatbox.srv.lib.field.value')} value={field.value}
                       placeholder={t(field.kind === 'enum' ? 'beatbox.srv.lib.field.values' : 'beatbox.srv.lib.field.value')}
                       onChange={(e) => setFields(fields.map((each, at) => (at === index ? { ...each, value: e.target.value } : each)))} />}
            <button type="button" className="bb-srv-copy" aria-label={t('beatbox.srv.lib.field.remove', { name: field.key || field.kind })}
                    onClick={() => setFields(fields.filter((_, at) => at !== index))}><Icon name="close" size="sm" /></button>
          </div>
        ))}
        <span className="bb-pagehead-title">
          <Button size="sm" icon="plus" onClick={() => setFields([...fields, { key: '', kind: 'text', value: '' }])}>{t('beatbox.srv.lib.field.add')}</Button>
          <Info text={t('beatbox.srv.lib.field.how')} />
        </span>
      </fieldset>
      <div className="bb-srv-preview">
        <small className="bb-faint">{t('beatbox.srv.lib.preview')}</small>
        <b>{definition.id}</b>
        <span className="bb-mono">{machine || '—'}:{Number(port) || '—'} · {transport}</span>
        <details className="bb-srv-more">
          <summary>{t('beatbox.srv.lib.file')}</summary>
          <pre className="bb-srv-log">{JSON.stringify(definition, null, 2)}</pre>
        </details>
      </div>
      {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
      <Button variant="primary" wide disabled={busy || !ready} onClick={() => void save(true)}>
        {t('beatbox.srv.lib.save_use', { server: srv.server?.name ?? '' })}</Button>
      <Button wide disabled={busy || !ready} onClick={() => void save(false)}>{t('beatbox.srv.lib.save')}</Button>
    </aside>
  );
}

/* ── Definições ───────────────────────────────────────────────────────── */

export function ServerSettings() {
  return <Frame page="settings">{(srv) => <Settings srv={srv} />}</Frame>;
}

const PERIODS = [5, 10, 30, 60];
/** What to do when a service exits by itself. */
const ON_EXIT = ['leave', 'retry', 'always'] as const;

/** Mirrors what `server.settings` returns. */
interface Own {
  start_period_s: number; restore: boolean; on_exit: string; library: string; libraryShown: string;
  domain: string; certificate: boolean; secrets: { name: string; usedBy: string[] }[];
}

function Settings({ srv }: { srv: Server }) {
  const server = srv.server!;
  const hosts = useOthers('host');
  const networks = useOthers('network');
  const uses = bag(server.config.uses);
  const apart = useApart(text(uses.host), text(uses.network));
  const [own, setOwn] = useState<Own | null>(null);
  const [asking, setAsking] = useState('');
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  const [copied, setCopied] = useState(false);
  const running = server.state.lifecycle === 'running';

  const change = useCallback(async (args: Record<string, unknown> = {}) => {
    try {
      const said = await act(srv.id, 'server.settings', args);
      setOwn({
        start_period_s: Number(said.start_period_s), restore: said.restore === true, on_exit: text(said.on_exit) || 'leave', library: text(said.library), libraryShown: text(said.library_shown),
        domain: text(said.domain), certificate: said.certificate === true,
        secrets: rows(said.secrets).map((each) => ({ name: text(each.name), usedBy: Array.isArray(each.used_by) ? each.used_by.map(String) : [] })),
      });
      setTold('');
      // The overview shows addresses under the domain, so reload it.
      if ('domain' in args) await srv.load();
    } catch (error) {
      setTold(fault(error));
    }
  }, [srv]);
  useEffect(() => { void change(); }, [change]);

  async function move(id: string, transition: string) {
    setBusy(true);
    setTold('');
    setAsking('');
    try {
      await call('sandbox.transition', { id, transition });
      if (transition === 'destroy') navigate('server/services');
      await srv.reload();
      await srv.load();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }

  /** Move the server to another machine or network. Only while it's off. */
  async function point(role: 'host' | 'network', target: string) {
    const next: Record<string, string> = {};
    for (const each of ['host', 'network'] as const) {
      const id = each === role ? target : text(uses[each]);
      if (id) next[each] = id;
    }
    if (!Object.keys(next).length) {
      setTold(t('beatbox.srv.set.need_one'));
      return;
    }
    setBusy(true);
    setTold('');
    try {
      await call('sandbox.use', { id: server.id, uses: next });
      await srv.reload();
      await srv.load();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }

  const place = (role: 'host' | 'network') => {
    const kind = role === 'host' ? 'machine' : 'network';
    const list = role === 'host' ? hosts : networks;
    const current = list.find((row) => row.id === uses[role]);
    return (
      <div className="bb-srv-place">
        <div className="bb-srv-placehead">
          <small><Icon name={role === 'host' ? 'cpu' : 'network'} size="sm" />{t(`beatbox.srv.set.${kind}`)}</small>
          {current ? <Pill tone={current.lifecycle === 'running' ? 'mint'
            : current.lifecycle === 'failed' ? 'rose' : 'neutral'}>
            {runtimeText(`beatbox.plain.state.${current.lifecycle}`)}
          </Pill> : null}
        </div>
        {running ? (
          <>
            {/* The lists load after the page opens: a server that points somewhere isn't "None" meanwhile. */}
            <b>{current?.name ?? (uses[role] ? '' : t('beatbox.srv.new.none'))}</b>
          </>
        ) : (
          <div className="bb-srv-selectrow">
            <Select label={t(`beatbox.srv.set.${kind}`)} value={text(uses[role])} onChange={(value) => void point(role, value)}
                    disabled={busy}
                    options={[
                      ...list.map((row) => ({ value: row.id, label: row.name, hint: runtimeText(`beatbox.plain.state.${row.lifecycle}`) })),
                      { value: '', label: t('beatbox.srv.new.none') },
                    ]} />
          </div>
        )}
        {current && current.lifecycle !== 'running' ? (
          <Button size="sm" icon="play" disabled={busy} onClick={() => void move(current.id, 'start')}>
            {t(`beatbox.srv.set.turn_on_${kind}`)}</Button>
        ) : null}
      </div>
    );
  };

  return (
    <div className="bb-srv-setgrid">
      <div className="bb-srv-stackpage">
        <section className="bb-srv-box">
          <h2>{t('beatbox.srv.set.where')} {running ? null : <Info text={t('beatbox.srv.set.change_hint')} />}</h2>
          <div className="bb-srv-two">{place('host')}{place('network')}</div>
          {apart ? <p className="bb-amber" role="note" style={{ margin: 0, fontSize: 13 }}>
            {t('beatbox.srv.new.apart', {
              machine: hosts.find((row) => row.id === text(uses.host))?.name ?? '',
              network: networks.find((row) => row.id === text(uses.network))?.name ?? '',
            })}
          </p> : null}
          {running ? <p className="bb-lede" style={{ margin: 0 }}>{t('beatbox.srv.set.running_lock')}</p> : null}
          {running ? (
            <span className="bb-srv-solo">
              <Button size="sm" icon="stop" disabled={busy} onClick={() => void move(server.id, 'stop')}>
                {t('beatbox.srv.set.off_do')}</Button>
            </span>
          ) : null}
        </section>
        <section className="bb-srv-box">
          <h2>{t('beatbox.srv.set.when_on')}</h2>
          {own ? (
            <>
              <div className="bb-sbx-toggle">
                <Switch id="bb-srv-restore" checked={own.restore} onChange={(next) => void change({ restore: next })}
                        label={t('beatbox.srv.set.restore')} />
                <label htmlFor="bb-srv-restore">
                  <b>{t('beatbox.srv.set.restore')} <Info text={t('beatbox.srv.set.restore_what')} /></b>
                </label>
              </div>
              <div className="bb-field">
                <span>{t('beatbox.srv.set.period')} <Info text={t('beatbox.srv.set.period_what')} /></span>
                <div className="bb-srv-tabs bb-srv-tabs--fit" role="radiogroup" aria-label={t('beatbox.srv.set.period')}>
                  {PERIODS.map((seconds) => (
                    <button key={seconds} type="button" role="radio" aria-checked={own.start_period_s === seconds}
                            className={`bb-srv-tab${own.start_period_s === seconds ? ' on' : ''}`} style={{ padding: '0 16px' }}
                            onClick={() => void change({ start_period_s: seconds })}>
                      {seconds < 60 ? `${seconds} s` : `${seconds / 60} min`}
                    </button>
                  ))}
                </div>
              </div>
              <div className="bb-field">
                <span>{t('beatbox.srv.set.order')} <Info text={t((srv.services ?? []).some((service) => service.after.length) ? 'beatbox.srv.set.order_what' : 'beatbox.srv.set.order_none')} /></span>
                {(srv.services ?? []).some((service) => service.after.length) ? (
                  <ol className="bb-srv-order">
                    {startOrder(srv.services ?? []).map((name, index) => (
                      <li key={name}>
                        {index ? <span className="bb-srv-orderarrow" aria-hidden="true">→</span> : null}
                        <span className="bb-srv-orderstep"><span className="bb-mono">{index + 1}</span>{name}</span>
                      </li>
                    ))}
                  </ol>
                ) : null}
              </div>
              <div className="bb-field">
                <span>{t('beatbox.srv.set.on_exit')} <Info text={runtimeText(`beatbox.srv.set.on_exit.${own.on_exit}.what`)} /></span>
                <div className="bb-srv-tabs bb-srv-tabs--fit" role="radiogroup" aria-label={t('beatbox.srv.set.on_exit')}>
                  {ON_EXIT.map((each) => (
                    <button key={each} type="button" role="radio" aria-checked={own.on_exit === each}
                            className={`bb-srv-tab${own.on_exit === each ? ' on' : ''}`} style={{ padding: '0 16px' }}
                            onClick={() => void change({ on_exit: each })}>
                      {runtimeText(`beatbox.srv.set.on_exit.${each}`)}
                    </button>
                  ))}
                </div>
              </div>
            </>
          ) : null}
        </section>
        {own ? <Shares srv={srv} own={own} change={change} /> : null}
        <section className="bb-srv-box">
          <h2>{t('beatbox.srv.set.library')} <Info text={t('beatbox.srv.set.library_what')} /></h2>
          <div className="bb-srv-row">
            <span className="bb-input bb-mono bb-srv-path-field">{own?.libraryShown || own?.library || '—'}</span>
            <button type="button" className="bb-srv-copy" aria-label={t('beatbox.srv.set.copy_path')} title={t('beatbox.srv.set.copy_path')}
                    disabled={!own?.library}
                    onClick={() => { void navigator.clipboard?.writeText(own?.library ?? ''); setCopied(true); window.setTimeout(() => setCopied(false), 1500); }}>
              <Icon name={copied ? 'check' : 'copy'} size="sm" />
            </button>
          </div>
        </section>
        {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
      </div>
      <aside className="bb-srv-stackpage">
        <section className="bb-srv-box bb-srv-box--danger">
          <h2>{t('beatbox.srv.set.actions')}</h2>
          <div className="bb-srv-act">
            <b>{t(running ? 'beatbox.srv.set.off' : 'beatbox.srv.set.on')} <Info text={t(running ? 'beatbox.srv.set.off_what' : 'beatbox.srv.set.on_what')} /></b>
            <Button size="sm" icon={running ? 'stop' : 'play'} disabled={busy}
                    onClick={() => void move(server.id, running ? 'stop' : 'start')}>
              {t(running ? 'beatbox.srv.set.off_do' : 'beatbox.srv.set.on_do')}</Button>
          </div>
          {(['reset', 'destroy'] as const).map((each) => (
            <div key={each} className="bb-srv-act">
              <b>{t(`beatbox.srv.set.${each}`)}</b>
              {/* Only explain what gets deleted once they ask to delete it. */}
              {asking === each ? <small>{t(`beatbox.srv.set.${each}_what`)}</small> : null}
              {asking === each ? (
                <span className="bb-srv-row">
                  <Button size="sm" variant="danger" disabled={busy} onClick={() => void move(server.id, each)}>{t(`beatbox.srv.set.${each}_do`)}</Button>
                  <Button size="sm" onClick={() => setAsking('')}>{t('beatbox.srv.keep')}</Button>
                </span>
              ) : (
                <Button size="sm" variant={each === 'destroy' ? 'danger' : 'ghost'} disabled={busy || !server.transitions.includes(each)}
                        onClick={() => setAsking(each)}>{t(`beatbox.srv.set.${each}_do`)}</Button>
              )}
            </div>
          ))}
        </section>
      </aside>
    </div>
  );
}

/** "a, b and c" in the page's language. */
const listed = (names: string[]) => (names.length < 2 ? names.join('')
  : t('beatbox.srv.shared.and', { first: names.slice(0, -1).join(', '), last: names.at(-1) ?? '' }));

/**
 * Stuff the services share: a domain to name them, one certificate for
 * HTTPS, and secrets you set once here and services point at by name.
 */
function Shares({ srv, own, change }: { srv: Server; own: Own; change: (args?: Record<string, unknown>) => Promise<void> }) {
  const [domain, setDomain] = useState(own.domain);
  useEffect(() => setDomain(own.domain), [own.domain]);
  // Which secret is being edited. '' = a new one.
  const [editing, setEditing] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  const services = (srv.services ?? []).map((service) => service.name);
  const typed = domain.trim().toLowerCase().replace(/\.$/, '');

  async function secret(capability: 'server.secret.set' | 'server.secret.remove', args: Record<string, unknown>) {
    setBusy(true);
    setTold('');
    try {
      await act(srv.id, capability, args);
      setEditing(null);
      setName('');
      setValue('');
      await change();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }
  const form = (fixed: string | null) => (
    <form className="bb-srv-secretform" onSubmit={(e) => { e.preventDefault(); void secret('server.secret.set', { name: fixed ?? name.trim(), value }); }}>
      {fixed === null ? (
        <label className="bb-field" htmlFor="bb-srv-secretname">
          <span>{t('beatbox.srv.shared.name')}</span>
          <input id="bb-srv-secretname" className="bb-input bb-mono" placeholder="db_password" value={name}
                 onChange={(e) => setName(e.target.value)} autoFocus />
        </label>
      ) : null}
      <label className="bb-field" htmlFor="bb-srv-secretvalue">
        <span>{t(fixed === null ? 'beatbox.srv.shared.value' : 'beatbox.srv.shared.new_value')}</span>
        <input id="bb-srv-secretvalue" className="bb-input" type="password" value={value} autoComplete="new-password"
               onChange={(e) => setValue(e.target.value)} autoFocus={fixed !== null} />
      </label>
      <span className="bb-srv-row">
        <Button size="sm" variant="primary" type="submit" disabled={busy || !value || (fixed === null && !name.trim())}>{t('beatbox.srv.shared.save')}</Button>
        <Button size="sm" disabled={busy} onClick={() => { setEditing(null); setValue(''); setName(''); }}>{t('beatbox.files.cancel')}</Button>
      </span>
    </form>
  );

  return (
    <section className="bb-srv-box">
      <h2>{t('beatbox.srv.shared.title')}</h2>
      <div className="bb-srv-two">
        <div>
          <label className="bb-field" htmlFor="bb-srv-domain">
            <span>{t('beatbox.srv.shared.domain')}</span>
            <input id="bb-srv-domain" className="bb-input bb-mono" placeholder="srv.lab" value={domain}
                   onChange={(e) => setDomain(e.target.value)}
                   onKeyDown={(e) => { if (e.key === 'Enter' && typed !== own.domain) void change({ domain: typed }); }} />
          </label>
          {typed !== own.domain ? (
            <span className="bb-srv-row bb-srv-domainsave">
              <Button size="sm" variant="primary" onClick={() => void change({ domain: typed })}>{t('beatbox.srv.shared.save')}</Button>
              <Button size="sm" onClick={() => setDomain(own.domain)}>{t('beatbox.files.cancel')}</Button>
            </span>
          ) : null}
        </div>
        <div className="bb-field">
          <span>{t(typed ? 'beatbox.srv.shared.names' : 'beatbox.srv.shared.no_domain')}
            {typed ? null : <> <Info text={t('beatbox.srv.shared.no_domain_what')} /></>}</span>
          {typed && services.length ? (
            <div className="bb-srv-names bb-mono">{services.map((each) => <span key={each}>{each}.{typed}</span>)}</div>
          ) : typed ? <small className="bb-faint">{t('beatbox.srv.shared.names_none')}</small> : null}
        </div>
      </div>

      <div className="bb-srv-sharepart">
        {own.domain ? (
          <div className="bb-sbx-toggle">
            <Switch id="bb-srv-certificate" checked={own.certificate} onChange={(next) => void change({ certificate: next })}
                    label={t('beatbox.srv.shared.certificate')} />
            <label htmlFor="bb-srv-certificate">
              <b>{t('beatbox.srv.shared.certificate')} <Info text={t('beatbox.srv.shared.certificate_what', { domain: own.domain })} /></b>
            </label>
          </div>
        ) : (
          <div className="bb-field">
            <span>{t('beatbox.srv.shared.certificate')}</span>
            <small className="bb-faint">{t('beatbox.srv.shared.certificate_needs')}</small>
          </div>
        )}
      </div>

      <div className="bb-srv-sharepart">
        <b className="bb-srv-sharehead">{t('beatbox.srv.shared.secrets')} <Info text={t('beatbox.srv.shared.secrets_what')} /></b>
        {own.secrets.map((each) => (
          <div key={each.name} className="bb-srv-secret">
            <div className="bb-srv-secretrow">
              <span className="bb-mono">{each.name}</span>
              <span className="bb-mono bb-faint" aria-label={t('beatbox.srv.shared.hidden')}>••••</span>
              <span className="bb-faint">{each.usedBy.length ? t('beatbox.srv.shared.used_by', { names: listed(each.usedBy) }) : t('beatbox.srv.shared.unused')}</span>
              <span className="bb-srv-secretacts">
                {!each.usedBy.length ? <Button size="sm" variant="ghost" icon="trash" disabled={busy}
                  onClick={() => void secret('server.secret.remove', { name: each.name })}>{t('beatbox.srv.shared.remove')}</Button> : null}
                <Button size="sm" disabled={busy || editing !== null} onClick={() => { setEditing(each.name); setValue(''); }}>{t('beatbox.srv.shared.change')}</Button>
              </span>
            </div>
            {editing === each.name ? form(each.name) : null}
          </div>
        ))}
        {editing === '' ? form(null) : (
          <button type="button" className="bb-srv-secretadd" disabled={editing !== null} onClick={() => { setEditing(''); setValue(''); setName(''); }}>
            <Icon name="plus" size="sm" />{t('beatbox.srv.shared.add')}
          </button>
        )}
      </div>
      {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
    </section>
  );
}
