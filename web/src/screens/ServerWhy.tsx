import { useCallback, useEffect, useRef, useState } from 'react';
import { Button, Icon, Info, Modal, Select } from '../components';
import { call, on } from '../bridge/client';
import { remember } from '../chosen';
import { fault, runtimeText, say, t } from '../i18n';
import { navigate } from '../router';
import { act, bag, rows, text, useOthers, type Server, type Service } from './Server';
import { fromLight, useSources, type Source } from './ServerMore';
import { clock } from './MachineParts';

/**
 * Why a service does not answer. "Seen" is what the Server measured just
 * now; "Maybe" is its best guess and always says so. Nothing here changes
 * anything until a person presses a button.
 */

interface Fact { fact: string; ok: boolean; port?: number; code?: number | null; missing?: string[]; service?: string; state?: string; problem?: unknown }
interface Maybe { kind: string; port?: number; ports?: number[]; other?: string; definition?: string; word?: string | null; line?: string | null; problem?: unknown }
interface Why { at: number; facts: Fact[]; maybe: Maybe | null }
interface Entry { at: number; name: string; service: string; to: string; again: boolean; role: string; sandbox: string; state: string; attempt: number; first: boolean }

/** now, 2 min, 3 h. */
export function ago(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  return s < 5 ? t('beatbox.net.ago.now') : s < 60 ? t('beatbox.net.ago.s', { n: s })
    : s < 3600 ? t('beatbox.net.ago.min', { n: Math.round(s / 60) }) : t('beatbox.net.ago.h', { n: Math.round(s / 3600) });
}

/** The Server's diary, oldest first, kept in sync with its events. */
export function useDiary(srv: Server) {
  const [entries, setEntries] = useState<Entry[]>([]);
  const load = useCallback(async () => {
    if (!srv.id) return;
    try {
      const said = await act(srv.id, 'server.diary');
      setEntries(rows(said.entries).map((row) => ({
        at: Number(row.at), name: text(row.name), service: text(row.service), to: text(row.to), again: row.again === true,
        role: text(row.role), sandbox: text(row.sandbox), state: text(row.state), attempt: Number(row.attempt ?? 1), first: row.first === true,
      })));
    } catch { setEntries([]); }
  }, [srv.id]);
  useEffect(() => { void load(); }, [load]);
  useEffect(() => on('sandbox', (event) => {
    if (event.sandboxId === srv.id && event.name.startsWith('service.')) void load();
  }), [srv.id, load]);
  return entries;
}

/** One diary line in words, or nothing for one a person need not read. */
export function told(entry: Entry, names: Map<string, string>): { tone: string; text: string; who: string } | null {
  const service = entry.service;
  if (entry.name === 'beatbox.started') return { tone: 'violet', text: t('beatbox.srv.why.order.restarted'), who: 'beatbox' };
  if (entry.name === 'peer') {
    const name = names.get(entry.sandbox) ?? t(entry.role === 'host' ? 'beatbox.srv.why.who.host' : 'beatbox.srv.why.who.network');
    const on = entry.state === 'running';
    const off = entry.state === 'stopped' || entry.state === 'created';
    if (!on && !off) return null;
    const key = entry.first ? (on ? 'beatbox.srv.why.order.is_on' : 'beatbox.srv.why.order.is_off') : on ? 'beatbox.srv.why.order.on' : 'beatbox.srv.why.order.off';
    return { tone: on ? 'mint' : 'neutral', text: t(key, { name }), who: entry.role };
  }
  if (entry.name === 'service.health.changed') {
    if (entry.to === 'unknown') return null;
    const answers = entry.to === 'healthy';
    return { tone: answers ? 'mint' : 'amber', who: 'server',
      text: t(answers ? 'beatbox.srv.lately.answers' : entry.again ? 'beatbox.srv.why.order.still' : 'beatbox.srv.lately.silent', { service }) };
  }
  if (entry.name === 'service.restarting') return { tone: 'amber', who: 'server', text: t('beatbox.srv.lately.restarting', { service, attempt: entry.attempt }) };
  const known = ['started', 'stopped', 'failed', 'created', 'removed'].find((each) => entry.name === `service.${each}`);
  if (!known) return null;
  return { tone: known === 'failed' ? 'rose' : known === 'started' ? 'mint' : 'neutral', who: 'server', text: runtimeText(`beatbox.srv.lately.${known}`, { service }) };
}

/** Sandbox ids to names, for the diary's machine and network lines. */
export function useNames() {
  const hosts = useOthers('host');
  const networks = useOthers('network');
  return new Map([...hosts, ...networks].map((each) => [each.id, each.name]));
}

function useWhy(srv: Server, service: Service) {
  const [why, setWhy] = useState<Why | null>(null);
  const [failed, setFailed] = useState('');
  const [busy, setBusy] = useState(false);
  const load = useCallback(async () => {
    setBusy(true);
    try {
      const said = await act(srv.id, 'server.why', { service: service.name });
      setWhy({ at: Number(said.checked_at), facts: rows(said.facts) as unknown as Fact[], maybe: said.maybe ? bag(said.maybe) as unknown as Maybe : null });
      setFailed('');
    } catch (error) {
      setWhy(null);
      setFailed(fault(error));
    }
    setBusy(false);
  }, [srv.id, service.name]);
  useEffect(() => { void load(); }, [load]);
  return { why, failed, busy, load };
}

/** "database is stopped", "email has a problem: …". */
function otherWords(fact: Fact): string {
  const service = text(fact.service);
  if (fact.state === 'failed') {
    return fact.problem ? t('beatbox.srv.why.fact.other_problem', { service, problem: say(fact.problem as never, text(bag(fact.problem).code)) })
      : t('beatbox.srv.why.fact.other_failed', { service });
  }
  return t(fact.state === 'created' ? 'beatbox.srv.why.fact.other_new' : 'beatbox.srv.why.fact.other_stopped', { service });
}

function factWords(fact: Fact, service: string, machine: string, network: string): string {
  switch (fact.fact) {
    case 'machine': return t(fact.ok ? 'beatbox.srv.why.fact.machine_on' : 'beatbox.srv.why.fact.machine_off', { machine });
    case 'running': return t(fact.ok ? 'beatbox.srv.why.fact.running' : 'beatbox.srv.why.fact.not_running', { service });
    case 'listens': return t(fact.ok ? 'beatbox.srv.why.fact.listens' : 'beatbox.srv.why.fact.deaf', { port: fact.port ?? '' });
    case 'check': {
      if (fact.ok) return t('beatbox.srv.why.fact.check_ok');
      const missing = (fact.missing ?? []).map((word) => t('beatbox.srv.why.fact.no', { word })).join(', ');
      const said = fact.code ? t('beatbox.srv.why.fact.check_code', { code: fact.code }) : t('beatbox.srv.why.fact.check_failed');
      return missing ? `${said} · ${missing}` : said;
    }
    case 'other': return otherWords(fact);
    case 'network': return t(fact.ok ? 'beatbox.srv.why.fact.network_on' : 'beatbox.srv.why.fact.network_off', { network });
    default: return '';
  }
}

/** The guess, in a headline and a line, and what can be done about it. */
function maybeWords(maybe: Maybe, why: Why, service: string, machine: string, server: string) {
  const other = why.facts.find((fact) => fact.fact === 'other' && fact.service === maybe.other);
  switch (maybe.kind) {
    case 'machine_off':
      return { seen: true, tone: 'grey', head: t('beatbox.srv.why.maybe.machine_off', { machine }), line: t('beatbox.srv.why.maybe.machine_off_why', { server }) };
    case 'stopped_itself':
      return { seen: true, tone: 'rose', head: t('beatbox.srv.why.maybe.stopped', { service }),
        line: maybe.line ? t('beatbox.srv.why.maybe.last_line', { line: maybe.line })
          : maybe.problem ? say(maybe.problem as never, text(bag(maybe.problem).code)) : '' };
    case 'elsewhere':
      return { seen: true, tone: 'amber', head: t('beatbox.srv.why.maybe.deaf', { port: maybe.port ?? '' }),
        line: t('beatbox.srv.why.maybe.elsewhere', { service, ports: (maybe.ports ?? []).join(', ') }) };
    case 'not_listening':
      return { seen: false, tone: 'amber', head: t('beatbox.srv.why.maybe.deaf', { port: maybe.port ?? '' }), line: t('beatbox.srv.why.maybe.deaf_why') };
    case 'needs': {
      const known = ['postgresql', 'smtp', 'dns'].includes(text(maybe.definition)) ? text(maybe.definition) : 'other';
      const state = other ? otherWords(other) : '';
      return { seen: false, tone: 'amber', head: runtimeText(`beatbox.srv.why.maybe.needs.${known}`, { other: text(maybe.other) }),
        line: maybe.word ? t('beatbox.srv.why.maybe.said_no', { state, service, word: maybe.word }) : t('beatbox.srv.why.maybe.needs_first', { state, service }) };
    }
    default: {
      const code = why.facts.find((fact) => fact.fact === 'check')?.code;
      return { seen: false, tone: 'amber', head: t('beatbox.srv.why.maybe.check'),
        line: code ? t('beatbox.srv.why.fact.check_code', { code }) : t('beatbox.srv.why.maybe.check_why') };
    }
  }
}

/** The whole answer as plain text, to paste somewhere. No paths, no secrets: only what is on screen. */
function summary(why: Why, service: Service, kind: string, machine: string, server: string, network: string, lines: string[]): string {
  const out = [`${service.name} (${kind}) · ${server} · ${machine}`, t('beatbox.srv.why.checked', { time: clock(new Date(why.at)), ago: ago(Date.now() - why.at) }), ''];
  if (why.maybe) {
    const said = maybeWords(why.maybe, why, service.name, machine, server);
    out.push(`${t(said.seen ? 'beatbox.srv.why.seen' : 'beatbox.srv.why.maybe')}: ${said.head}. ${said.line}`, '');
  }
  out.push(`${t('beatbox.srv.why.seen')}:`);
  for (const fact of why.facts) out.push(`  ${fact.ok ? 'ok' : 'no'}  ${factWords(fact, service.name, machine, network)}`);
  if (lines.length) out.push('', `${t('beatbox.srv.live_log')}:`, ...fold(lines).slice(-5).map((each) => `  ${each.line}${each.times > 1 ? ` ×${each.times}` : ''}`));
  return out.join('\n');
}

/** Why it does not answer: in the service panel while it doesn't. */
export function Why({ srv, service, kind, machine, lines, onOpen, onEdit }: {
  srv: Server; service: Service; kind: string; machine: string; lines: string[]; onOpen: (service: string) => void; onEdit: () => void;
}) {
  const { why, failed, busy, load } = useWhy(srv, service);
  const { networkName } = useSources(srv);
  const names = useNames();
  const diary = useDiary(srv);
  const [checking, setChecking] = useState(false);
  const [reading, setReading] = useState(false);
  const [copied, setCopied] = useState(false);
  const [doing, setDoing] = useState('');
  const server = srv.server?.name ?? '';
  // Ticks the "1 min ago".
  const [, tick] = useState(0);
  useEffect(() => { const timer = window.setInterval(() => tick((n) => n + 1), 30000); return () => window.clearInterval(timer); }, []);

  async function turnOn() {
    setDoing('');
    try {
      await call('sandbox.transition', { id: text(bag(srv.server?.config.uses).host), transition: 'start' });
      await load();
    } catch (error) { setDoing(fault(error)); }
  }

  async function start(other: string) {
    setDoing('');
    try {
      await act(srv.id, 'service.start', { service: other });
      await srv.load();
      await load();
    } catch (error) { setDoing(fault(error)); }
  }

  const others = new Set(why?.facts.filter((fact) => fact.fact === 'other').map((fact) => text(fact.service)));
  const mine = diary.filter((entry) => entry.service === service.name || others.has(entry.service) || entry.name === 'peer' || entry.name === 'beatbox.started')
    .map((entry) => ({ entry, said: told(entry, names) }))
    .filter((each) => each.said)
    .slice(-8);

  const said = why?.maybe ? maybeWords(why.maybe, why, service.name, machine, server) : null;
  return (
    <div className="bb-why">
      <div className="bb-why-head">
        <b>{t('beatbox.srv.why.title')}</b><Info text={t('beatbox.srv.why.info')} />
        <span className="bb-why-gap" />
        {why ? <small className="bb-faint">{t('beatbox.srv.why.checked', { time: clock(new Date(why.at)), ago: ago(Date.now() - why.at) })}</small> : null}
        <Button size="sm" disabled={busy} onClick={() => setChecking(true)}>{t('beatbox.srv.why.again')}</Button>
        {why ? (
          <Button size="sm" onClick={() => {
            void navigator.clipboard?.writeText(summary(why, service, kind, machine, server, networkName, lines));
            setCopied(true); window.setTimeout(() => setCopied(false), 1500);
          }}>{t(copied ? 'beatbox.srv.why.copied' : 'beatbox.srv.why.copy')}</Button>
        ) : null}
      </div>

      {!why && failed ? (
        <div className="bb-why-box is-grey">
          <b>{service.healthCheckedAt ? t('beatbox.srv.why.stale', { ago: ago(Date.now() - service.healthCheckedAt) }) : t('beatbox.srv.why.unknown')}</b>
          <span className="bb-faint">{service.healthCheckedAt ? t(service.health === 'healthy' ? 'beatbox.srv.why.stale_ok' : 'beatbox.srv.why.stale_no')
            : t('beatbox.srv.why.unknown_why')}</span>
        </div>
      ) : null}

      {said && why?.maybe ? (
        <div className={`bb-why-box is-${said.tone}`}>
          <span className={`bb-why-tag ${said.seen ? 'is-seen' : 'is-maybe'}`}>{t(said.seen ? 'beatbox.srv.why.seen' : 'beatbox.srv.why.maybe')}</span>
          <b>{said.head}</b>
          {said.line ? <span>{said.line}</span> : null}
          {why.maybe.kind === 'needs' && why.maybe.other ? (
            <span><Button size="sm" variant="primary" onClick={() => void start(text(why.maybe?.other))}>{t('beatbox.srv.why.put_to_work', { other: text(why.maybe.other) })}</Button></span>
          ) : why.maybe.kind === 'machine_off' ? (
            <span><Button size="sm" variant="primary" onClick={() => void turnOn()}>{t('beatbox.srv.why.turn_on', { network: machine })}</Button></span>
          ) : why.maybe.kind === 'stopped_itself' ? (
            <span className="bb-why-acts is-start">
              <Button size="sm" onClick={() => setReading(true)}>{t('beatbox.srv.why.log')}</Button>
              <Button size="sm" variant="primary" onClick={() => void start(service.name)}>{t('beatbox.srv.do.start')}</Button>
            </span>
          ) : why.maybe.kind === 'elsewhere' ? (
            <span><Button size="sm" variant="primary" onClick={onEdit}>{t('beatbox.srv.why.edit')}</Button></span>
          ) : why.maybe.kind === 'check_failed' ? (
            <span><Button size="sm" onClick={() => setReading(true)}>{t('beatbox.srv.why.log')}</Button></span>
          ) : null}
          {doing ? <span className="bb-sbx-no">{doing}</span> : null}
        </div>
      ) : null}

      {why ? (
        <div className="bb-why-facts">
          <small className="bb-faint">{t('beatbox.srv.why.seen')}</small>
          {why.facts.map((fact, index) => (
            <div key={index} className={`bb-why-fact${fact.fact === 'network' && !fact.ok ? ' is-quiet' : ''}`}>
              <span className={fact.ok ? 'bb-srv-good' : fact.fact === 'network' ? 'bb-faint' : 'bb-srv-bad'} aria-hidden="true">{fact.ok ? '✓' : fact.fact === 'network' ? '–' : '✕'}</span>
              <span className="bb-why-grow">{factWords(fact, service.name, machine, networkName)}</span>
              {fact.fact === 'other' && fact.service ? (
                <button type="button" className="bb-why-link" onClick={() => onOpen(text(fact.service))}>{t('beatbox.srv.why.open')}</button>
              ) : null}
            </div>
          ))}
        </div>
      ) : null}

      {mine.length ? (
        <div className="bb-why-facts">
          <small className="bb-faint">{t('beatbox.srv.why.order.title')}</small>
          {mine.map(({ entry, said: words }, index) => (
            <div key={index} className="bb-why-event">
              <span className="bb-mono bb-faint">{clock(new Date(entry.at))}</span>
              <i className={`is-${words!.tone}`} aria-hidden="true" />
              <span className="bb-why-grow">{words!.text}</span>
              <small className="bb-faint">{runtimeText(`beatbox.srv.why.who.${words!.who}`)}</small>
            </div>
          ))}
        </div>
      ) : null}

      {reading ? <WholeLog srv={srv} service={service} onClose={() => setReading(false)} /> : null}
      {checking ? <CheckAgain srv={srv} service={service} machine={machine} onClose={() => { setChecking(false); void load(); }} /> : null}
    </div>
  );
}

type Step = { key: string; words: string; state: 'ok' | 'no' | 'wait'; note?: string };
type Ending = { tone: string; tag: 'seen' | 'maybe' | null; head: string; line: string; flows?: boolean; rule?: boolean; network?: boolean; start?: string };

/** Check it again from one place, step by step; the first step that fails ends it. */
function CheckAgain({ srv, service, machine, onClose }: { srv: Server; service: Service; machine: string; onClose: () => void }) {
  const { sources, network, networkState, networkName } = useSources(srv);
  const [from, setFrom] = useState('itself');
  const [steps, setSteps] = useState<Step[]>([]);
  const [ending, setEnding] = useState<Ending | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const source: Source | undefined = sources.find((each) => each.value === from);
  // What a check reads, as of now. Every render rebuilds these, so the check
  // must not depend on them: it would run again on each step it shows.
  const now = useRef({ source, network, networkState, networkName, services: srv.services });
  now.current = { source, network, networkState, networkName, services: srv.services };

  const run = useCallback(async () => {
    const { source, network, networkState, networkName, services } = now.current;
    setBusy(true); setError(''); setEnding(null); setSteps([]);
    const shown: Step[] = [];
    const show = (step: Step) => { shown.push(step); setSteps([...shown]); };
    const wait = (keys: [string, string][]) => { for (const [key, words] of keys) shown.push({ key, words, state: 'wait', note: t('beatbox.srv.why.step.not_tried') }); setSteps([...shown]); };
    try {
      const said = await act(srv.id, 'server.why', { service: service.name });
      const facts = rows(said.facts) as unknown as Fact[];
      // Its address as of now: it gets one when its network comes up, after the page last looked.
      const listed = rows((await act(srv.id, 'service.list')).services).find((row) => text(row.service) === service.name);
      const ends = rows(listed?.endpoints).map((row) => ({ protocol: row.protocol == null ? null : text(row.protocol), transport: text(row.transport), address: text(row.address), port: Number(row.port) }));
      const end = ends.find((each) => each.protocol === 'http') ?? ends.find((each) => each.transport === 'tcp');
      const fact = (name: string) => facts.find((each) => each.fact === name);
      const later: [string, string][] = [];
      const name = srv.domain ? `${service.name}.${srv.domain}` : '';
      const address = end ? `${end.address}:${end.port}` : service.name;
      if (source) {
        later.push(['network', t('beatbox.srv.why.step.network', { from: source.name })]);
        if (source.light && name) later.push(['name', t('beatbox.srv.why.step.name', { from: source.name, name })]);
        later.push(['through', t('beatbox.srv.why.step.through', { from: source.name, address })]);
      }
      later.push(['answers', t('beatbox.srv.why.step.answers', { service: service.name })]);
      const inside: [string, string, Fact | undefined][] = [
        ['machine', t('beatbox.srv.why.fact.machine_on', { machine }), fact('machine')],
        ['running', t('beatbox.srv.why.fact.running', { service: service.name }), fact('running')],
        ...facts.filter((each) => each.fact === 'listens').map((each): [string, string, Fact] => ['listens', t('beatbox.srv.why.fact.listens', { port: each.port ?? '' }), each]),
      ];
      for (const [key, words, seen] of inside) {
        if (!seen) continue;
        show({ key, words, state: seen.ok ? 'ok' : 'no', note: key === 'listens' ? t('beatbox.srv.why.step.inside') : undefined });
        if (!seen.ok) { wait(later); return; }
      }
      const check = fact('check');
      if (!source) {
        const ok = check ? check.ok : true;
        show({ key: 'answers', words: later.at(-1)![1], state: ok ? 'ok' : 'no', note: check?.code ? String(check.code) : undefined });
        setEnding(ok ? { tone: 'ok', tag: null, head: t('beatbox.srv.why.end.answers', { service: service.name, from: machine }), line: '' }
          : { tone: 'amber', tag: null, head: t('beatbox.srv.why.end.way_fine'), line: t('beatbox.srv.why.end.way_fine_why', { service: service.name, code: check?.code ?? '—' }) });
        return;
      }
      const rest = later.slice(1);
      // Another machine reaches it only through the Server's network.
      if (network && networkState !== 'running') {
        show({ key: 'network', words: later[0]![1], state: 'no', note: t('beatbox.srv.why.fact.network_off', { network: networkName }) });
        wait(rest);
        setEnding({ tone: 'grey', tag: 'seen', head: t('beatbox.srv.why.fact.network_off', { network: networkName }), line: '', network: true });
        return;
      }
      show({ key: 'network', words: later[0]![1], state: 'ok' });
      if (!end) {
        show({ key: 'through', words: rest.at(-2)?.[1] ?? '', state: 'no', note: t('beatbox.srv.why.step.no_address') });
        wait(rest.slice(-1));
        setEnding({ tone: 'grey', tag: 'seen', head: t('beatbox.srv.why.end.no_address', { service: service.name }), line: '' });
        return;
      }
      let next = 0;
      if (source.light && name) {
        const found = await fromLight(network, source.name, 'dns', end, name);
        show({ key: 'name', words: rest[0]![1], state: found.ok ? 'ok' : 'no', note: found.answer || undefined });
        next = 1;
        if (!found.ok) {
          wait(rest.slice(1));
          const names = services?.find((each) => each.definition === 'dns' && each.state !== 'running');
          setEnding({ tone: 'amber', tag: 'seen', head: t('beatbox.srv.why.end.no_name', { from: source.name, name }), start: names?.name,
            line: t(names ? 'beatbox.srv.why.end.no_name_off' : 'beatbox.srv.why.end.no_name_why', { address: end.address, names: names?.name ?? '' }) });
          return;
        }
      }
      const tcp = source.light ? await fromLight(network, source.name, 'tcp', end, '')
        : await act(source.id, 'network.probe', { kind: 'tcp', address: end.address, port: end.port });
      const flows = network ? rows(bag(await act(network, 'network.flows')).flows) : [];
      const flow = flows
        .filter((each) => each.destination === end.address && Number(each.port) === end.port && (!source.light || text(each.source_node) === source.name))
        .sort((a, b) => Number(a.quiet_ms ?? 0) - Number(b.quiet_ms ?? 0))[0];
      if (tcp.ok !== true) {
        show({ key: 'through', words: rest[next]![1], state: 'no', note: flow ? undefined : t('beatbox.srv.why.step.no_reply', { s: Math.round(Number(tcp.ms ?? 0) / 1000) }) });
        wait(rest.slice(next + 1));
        if (flow && flow.action === 'deny') {
          setEnding({ tone: 'rose', tag: 'seen', head: t('beatbox.srv.why.end.blocked'), flows: true, rule: true,
            line: flow.rule == null ? t('beatbox.srv.why.end.blocked_default', { count: Number(flow.packets ?? 1), ago: ago(Number(flow.quiet_ms ?? 0)) })
              : t('beatbox.srv.why.end.blocked_why', { rule: text(flow.rule), count: Number(flow.packets ?? 1), ago: ago(Number(flow.quiet_ms ?? 0)) }) });
        } else {
          setEnding({ tone: 'grey', tag: 'maybe', head: t('beatbox.srv.why.end.unknown'), line: t(flow ? 'beatbox.srv.why.end.unknown_let' : 'beatbox.srv.why.end.unknown_why'), flows: Boolean(network) });
        }
        return;
      }
      show({ key: 'through', words: rest[next]![1], state: 'ok', note: t('beatbox.srv.why.step.ms', { ms: Number(tcp.ms ?? 0) }) });
      if (end.protocol !== 'http') {
        setEnding({ tone: 'ok', tag: null, head: t('beatbox.srv.why.end.answers', { service: service.name, from: source.name }), line: '' });
        return;
      }
      const page = source.light ? await fromLight(network, source.name, 'http', end, '')
        : await act(source.id, 'network.probe', { kind: 'http', address: end.address, port: end.port });
      const code = text(page.said).split(' ')[0] ?? '';
      show({ key: 'answers', words: page.ok ? rest.at(-1)![1] : t('beatbox.srv.why.step.answers_badly', { service: service.name }),
        state: page.ok === true ? 'ok' : 'no', note: code || undefined });
      setEnding(page.ok === true ? { tone: 'ok', tag: null, head: t('beatbox.srv.why.end.answers', { service: service.name, from: source.name }), line: '' }
        : { tone: 'amber', tag: null, head: t('beatbox.srv.why.end.way_fine'), line: t('beatbox.srv.why.end.way_fine_why', { service: service.name, code: code || '—' }) });
    } catch (failure) {
      setError(fault(failure));
    } finally {
      setBusy(false);
    }
  }, [srv.id, srv.domain, service.name, machine]);
  // On open, and again when where it starts from or its network changes
  // (Turn on ends there).
  useEffect(() => { void run(); }, [run, from, networkState]);

  async function turnOn() {
    setBusy(true);
    try { await call('sandbox.transition', { id: network, transition: 'start' }); } catch (failure) { setError(fault(failure)); setBusy(false); }
  }

  const options = [{ value: 'itself', name: t('beatbox.srv.why.itself', { machine }) }, ...sources.map((each) => ({ value: each.value, name: each.name }))];
  return (
    <Modal title={t('beatbox.srv.why.check_title', { service: service.name })} info={t('beatbox.srv.why.check_info')} icon="refresh" width={600} onClose={onClose}>
      <div className="bb-why-check">
        {options.length > 3 ? (
          <div className="bb-why-from">
            <small className="bb-faint">{t('beatbox.srv.why.from')}</small>
            <Select label={t('beatbox.srv.why.from')} value={from} disabled={busy} onChange={setFrom} style={{ flex: 'none', width: 260 }}
                    options={options.map((option) => ({ value: option.value, label: option.name }))} />
          </div>
        ) : options.length > 1 ? (
          <div>
            <small className="bb-faint">{t('beatbox.srv.why.from')}</small>
            <div className="bb-why-seg" role="radiogroup" aria-label={t('beatbox.srv.why.from')}>
              {options.map((option) => (
                <button key={option.value} type="button" role="radio" aria-checked={from === option.value} disabled={busy}
                        className={from === option.value ? 'is-on' : undefined} onClick={() => setFrom(option.value)}>{option.name}</button>
              ))}
            </div>
          </div>
        ) : null}
        <div className="bb-why-facts">
          {steps.map((step, index) => (
            <div key={index} className={`bb-why-fact${step.state === 'wait' ? ' is-quiet' : ''}`}>
              <span className={step.state === 'ok' ? 'bb-srv-good' : step.state === 'no' ? 'bb-srv-bad' : 'bb-faint'} aria-hidden="true">
                {step.state === 'ok' ? '✓' : step.state === 'no' ? '✕' : '○'}
              </span>
              <span className="bb-why-grow">{step.words}</span>
              {step.note ? <small className={step.state === 'no' ? 'bb-srv-bad' : 'bb-faint'}>{step.note}</small> : null}
            </div>
          ))}
          {busy ? <div className="bb-why-fact is-quiet"><span className="bb-why-spin" aria-hidden="true" /><span>{t('beatbox.srv.why.looking')}</span></div> : null}
        </div>
        {ending ? (
          <div className={`bb-why-box is-${ending.tone}`}>
            {ending.tag ? <span className={`bb-why-tag is-${ending.tag}`}>{runtimeText(`beatbox.srv.why.${ending.tag}`)}</span> : null}
            <b>{ending.tone === 'ok' ? <Icon name="check" size="sm" /> : null}{ending.head}</b>
            {ending.line ? <span>{ending.line}</span> : null}
          </div>
        ) : null}
        {error ? <p className="bb-sbx-no" style={{ margin: 0 }}>{error}</p> : null}
        <div className="bb-why-acts">
          {ending?.flows ? <Button onClick={() => { remember('network', network); navigate('network/flows'); }}>{t('beatbox.srv.why.see_flows')}</Button> : null}
          {ending?.rule ? <Button onClick={() => { remember('network', network); navigate('network/firewall'); }}>{t('beatbox.srv.why.open_rule')}</Button> : null}
          {ending?.start ? <Button variant="primary" disabled={busy} onClick={() => void act(srv.id, 'service.start', { service: ending.start }).then(() => { void srv.load(); void run(); }, (failure) => setError(fault(failure)))}>
            {t('beatbox.srv.why.put_to_work', { other: ending.start })}</Button> : null}
          {ending?.network ? <Button variant="primary" disabled={busy} onClick={() => void turnOn()}>{t('beatbox.srv.why.turn_on', { network: networkName })}</Button> : null}
          <Button variant={ending?.network || ending?.start ? undefined : 'primary'} disabled={busy} onClick={() => void run()}>{t('beatbox.srv.why.again')}</Button>
          <Button onClick={onClose}>{t('beatbox.srv.why.close')}</Button>
        </div>
      </div>
    </Modal>
  );
}

/** Its whole log, as much as the guest keeps of it, with repeats folded. */
function WholeLog({ srv, service, onClose }: { srv: Server; service: Service; onClose: () => void }) {
  const [lines, setLines] = useState<string[] | null>(null);
  useEffect(() => {
    void act(srv.id, 'service.logs', { service: service.name, lines: 400 })
      .then((said) => setLines(Array.isArray(said.lines) ? said.lines.map(String) : []), () => setLines([]));
  }, [srv.id, service.name]);
  return (
    <Modal title={t('beatbox.srv.why.log_title', { service: service.name })} icon="logs" width={760} onClose={onClose}>
      <div className="bb-srv-modal">
      {lines === null ? <div className="bb-why-fact is-quiet"><span className="bb-why-spin" aria-hidden="true" /><span>{t('beatbox.srv.why.looking')}</span></div>
        : lines.length ? <LogLines lines={lines} className="bb-srv-log bb-why-log" />
        : <div className="bb-srv-log bb-srv-log--empty">{t('beatbox.srv.side.no_log')}</div>}
      </div>
    </Modal>
  );
}

/** A log with repeats folded and warnings marked; `last` keeps only the end. */
export function LogLines({ lines, last, className = 'bb-srv-log' }: { lines: string[]; last?: number; className?: string }) {
  const folded = fold(lines);
  return (
    <pre className={className}>{(last ? folded.slice(-last) : folded).map((each, index) => (
      <span key={index} className={each.marked ? 'is-marked' : undefined}>{each.line}{each.times > 1 ? <b className="bb-srv-times"> ×{each.times}</b> : null}{'\n'}</span>
    ))}</pre>
  );
}

/** Log lines with repeats folded (the same line but for its time) and the warnings marked. */
export function fold(lines: string[]): { line: string; times: number; marked: boolean }[] {
  const stamp = /^\s*[\d:.,T\-+ ]{8,}\s*/;
  const out: { line: string; times: number; marked: boolean; key: string }[] = [];
  for (const line of lines) {
    const key = line.replace(stamp, '');
    const last = out.at(-1);
    if (last && last.key === key) { last.times += 1; last.line = line; continue; }
    out.push({ line, times: 1, key, marked: /\b(warn(ing)?|error|fatal|refused|unavailable|failed|denied|traceback)\b/i.test(key) });
  }
  return out;
}

