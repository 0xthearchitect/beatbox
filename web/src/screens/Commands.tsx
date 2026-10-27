import { useCallback, useEffect, useState } from 'react';
import { Button, Card, ChipRow, CubeWire, Icon, Info, Modal, Select } from '../components';
import { call } from '../bridge/client';
import type { SandboxView } from '../bridge/types';
import { cableOf } from '../cable';
import { useChosenMachine } from '../chosen';
import { fault, say, t, runtimeText } from '../i18n';
import {
  BUILT_IN, GROUPS, blanksIn, fill, fromKept, lineFor, readEchoes, type Blank, type BlankKind, type Command,
} from '../library';
import { navigate } from '../router';

/**
 * Run things in a machine without touching its screen.
 *
 * A library of commands (built-in or saved, for all machines or one) and a
 * console: pick, fill the blanks, run through the guest agent
 * (`process.shell`), read the answer. Nothing gets typed into the guest's
 * window or installed on it.
 */

const bag = (value: unknown): Record<string, unknown> =>
  (value && typeof value === 'object' ? value as Record<string, unknown> : {});

interface Row { pid: number; user: string; command: string }

interface Run {
  at: string;
  name: string;
  line: string;
  exit: number | null;
  stdout: string;
  stderr: string;
  seconds: number;
  trouble: string;
  rows?: Row[];
  /** Ping results: how many answered, and who was asked. */
  echo?: string;
}

/** Run history per machine. Lives as long as the window, not saved. */
const RAN = new Map<string, Run[]>();
const TIMEOUTS = [10, 30, 60, 120, 300];

/** Windows guest? Asks the guest's own report first, then the disk's description. */
export function isWindows(machine: SandboxView | null): boolean {
  const facts = bag(machine?.state.facts);
  return facts.family === 'windows' || bag(bag(machine?.config.settings).os).family === 'windows';
}

/** The other machines on this one's network, for a `node` blank. */
export function useNodes(machine: SandboxView | null) {
  const cable = cableOf(machine);
  const [nodes, setNodes] = useState<{ value: string; label: string; hint: string }[]>([]);
  useEffect(() => {
    if (!cable?.sandbox) { setNodes([]); return; }
    void call('sandbox.view', { id: cable.sandbox })
      .then((network) => setNodes((Array.isArray(bag(network.state.facts).nodes) ? bag(network.state.facts).nodes as unknown[] : [])
        .map(bag)
        .filter((node) => node.address !== cable.address)
        .map((node) => ({ value: String(node.address), label: String(node.node), hint: String(node.address) }))))
      .catch(() => setNodes([]));
  }, [cable?.sandbox, cable?.address]);
  return nodes;
}

export function Commands() {
  const { list, machine } = useChosenMachine();
  const id = machine?.id ?? '';
  const running = machine?.state.lifecycle === 'running';
  const can = machine?.capabilities.includes('process.shell') ?? false;
  const windows = isWindows(machine);
  const nodes = useNodes(machine);

  const [kept, setKept] = useState<Command[]>([]);
  const [group, setGroup] = useState('all');
  const [search, setSearch] = useState('');
  const [pick, setPick] = useState<Command>(BUILT_IN.find((c) => c.id === 'system')!);
  const [values, setValues] = useState<Record<string, string>>({});
  const [timeout, setTimeoutSeconds] = useState(30);
  const [typed, setTyped] = useState('');
  const [busy, setBusy] = useState(false);
  const [shown, setShown] = useState<Run | null>(null);
  const [, redraw] = useState(0);
  const [editing, setEditing] = useState<Command | null>(null);
  const [ending, setEnding] = useState<number | null>(null);
  const [told, setTold] = useState('');

  const loadKept = useCallback(async () => {
    const all = await call('commands.list');
    setKept(all.filter((c) => !c.machine || c.machine === id).map(fromKept));
  }, [id]);
  useEffect(() => { void loadKept().catch(() => setKept([])); }, [loadKept]);
  useEffect(() => { setShown(RAN.get(id)?.[0] ?? null); }, [id]);

  const every = [...BUILT_IN, ...kept].filter((c) => lineFor(c, windows) || c.special);
  const needle = search.trim().toLowerCase();
  const offered = every.filter((c) => (group === 'all' || c.group === group)
    && (!needle || c.name.toLowerCase().includes(needle) || lineFor(c, windows).toLowerCase().includes(needle)));

  function remember(run: Run) {
    RAN.set(id, [run, ...(RAN.get(id) ?? [])].slice(0, 50));
    setShown(run);
    redraw((n) => n + 1);
  }

  async function run(name: string, line: string, echo?: string) {
    setBusy(true);
    const started = performance.now();
    const at = new Date().toLocaleTimeString([], { hour12: false });
    const done = (part: Partial<Run>) => remember({
      at, name, line, exit: null, stdout: '', stderr: '', trouble: '', echo,
      seconds: Math.round((performance.now() - started) / 100) / 10, ...part,
    });
    try {
      const outcome = await call('sandbox.act', { id, capability: 'process.shell', arguments: { line, timeout } });
      const data = bag(outcome.data);
      if (outcome.ok) {
        done({ exit: Number(data.exit_code ?? 0), stdout: String(data.stdout ?? ''), stderr: String(data.stderr ?? '') });
      } else {
        done({ trouble: say(outcome.reason) });
      }
    } catch (error) {
      done({ trouble: fault(error) });
    }
    setBusy(false);
  }

  async function processes() {
    setBusy(true);
    const started = performance.now();
    try {
      const outcome = await call('sandbox.act', { id, capability: 'process.list', arguments: {} });
      const rows = (Array.isArray(bag(outcome.data).processes) ? bag(outcome.data).processes as unknown[] : [])
        .map(bag).map((row) => ({ pid: Number(row.pid), user: String(row.user ?? ''), command: String(row.command ?? '') }));
      remember({
        at: new Date().toLocaleTimeString([], { hour12: false }), name: t('beatbox.commands.built.processes'), line: windows ? 'tasklist' : 'ps',
        exit: outcome.ok ? 0 : null, stdout: '', stderr: '', trouble: outcome.ok ? '' : say(outcome.reason),
        seconds: Math.round((performance.now() - started) / 100) / 10, rows,
      });
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }

  async function end(row: Row) {
    setEnding(null);
    try {
      const outcome = await call('sandbox.act', { id, capability: 'process.kill', arguments: { pid: row.pid } });
      setTold(outcome.ok ? t('beatbox.processes.ended', { name: row.command }) : say(outcome.reason));
    } catch (error) {
      setTold(fault(error));
    }
    void processes();
  }

  function runPicked() {
    if (pick.special === 'processes') { void processes(); return; }
    const filled = fill(lineFor(pick, windows), pick.blanks, values, windows);
    if ('missing' in filled) return;
    const target = pick.id === 'reach' ? values.node ?? '' : '';
    const echo = target ? nodes.find((node) => node.value === target)?.label ?? target : undefined;
    void run(pick.kept ? pick.name : runtimeText(`beatbox.commands.built.${pick.id}`), filled.line, echo);
  }

  const line = lineFor(pick, windows);
  const blanks = blanksIn(line);
  // Last run, if it belongs to the picked command or a hand-typed line.
  const pickName = pick.kept ? pick.name : runtimeText(`beatbox.commands.built.${pick.id}`);
  const shownNow = shown;
  const filled = pick.special ? null : fill(line, pick.blanks, values, windows);
  const missing = filled && 'missing' in filled ? filled.missing : '';

  return (
    <>
      <div className="bb-pagehead">
        <div>
          <div className="bb-pagehead-title">
            <h1 className="bb-t-title" style={{ margin: 0 }}>{t('beatbox.screen.host.commands.title')}</h1>
            <Info text={machine ? t('beatbox.commands.about', { name: machine.name }) : t('beatbox.screen.host.commands.blurb')} />
          </div>
        </div>
      </div>

      {list && !list.length ? (
        <div className="bb-pending" style={{ minHeight: 280 }}>
          <CubeWire size={80} tone="violet" opacity={0.42} />
          <p>{t('beatbox.host_pages.no_machines')}</p>
          <Button variant="primary" trailingIcon="arrow" onClick={() => navigate('host/machine')}>{t('beatbox.host_pages.to_machine')}</Button>
        </div>
      ) : machine && !can ? (
        <Card><p className="bb-host-say">{t('beatbox.commands.no_agent')}</p></Card>
      ) : machine && !running ? (
        <div className="bb-pending" style={{ minHeight: 280 }}>
          <CubeWire size={80} tone="violet" opacity={0.42} />
          <p>{t('beatbox.commands.off', { name: machine.name })}</p>
          <Button trailingIcon="arrow" onClick={() => navigate('host/machine')}>{t('beatbox.host_pages.to_machine')}</Button>
        </div>
      ) : machine ? (
        <div className="bb-cmd">
          <Card>
            <input className="bb-input" type="search" value={search} placeholder={t('beatbox.commands.search')}
                   aria-label={t('beatbox.commands.search')} onChange={(e) => setSearch(e.target.value)} style={{ width: '100%', marginBottom: 10 }} />
            <ChipRow current={group} style={{ marginBottom: 8 }}>
              {['all', ...GROUPS].map((key) => (
                <button key={key} type="button" className="bb-chip" aria-current={group === key} onClick={() => setGroup(key)}>
                  {runtimeText(`beatbox.commands.group.${key}`)}
                </button>
              ))}
            </ChipRow>
            <div className="bb-cmd-list">
              {GROUPS.map((key) => {
                const mine = offered.filter((command) => command.group === key);
                if (!mine.length) return null;
                return (
                  <div key={key} className="bb-cmd-group">
                    {group === 'all' ? <div className="bb-wf-label">{t(`beatbox.commands.group.${key}`)}</div> : null}
                    {mine.map((command) => (
                      <button key={command.id} type="button" className="bb-cmd-item" aria-current={command.id === pick.id}
                              onClick={() => { setPick(command); setValues({}); setTimeoutSeconds(command.timeout); }}>
                        {command.kept ? command.name : runtimeText(`beatbox.commands.built.${command.id}`)}
                      </button>
                    ))}
                  </div>
                );
              })}
              {!offered.length ? <p className="bb-host-say">{t('beatbox.commands.none_match')}</p> : null}
            </div>
            <button type="button" className="bb-cmd-own"
                    onClick={() => setEditing({ id: '', name: '', group: 'yours', linux: '', windows: '', blanks: {}, timeout: 30, kept: true, machine: '' })}>
              <Icon name="plus" size="xs" />{t('beatbox.commands.new')}
            </button>
          </Card>

          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--bb-space-3)', minWidth: 0 }}>
            <Card>
              <div className="bb-cmd-head">
                <h2 className="bb-cmd-title">{pickName}</h2>
                <p className="bb-lede" style={{ margin: 0 }}>{pick.kept ? t('beatbox.commands.yours') : runtimeText(`beatbox.commands.what.${pick.id}`)}</p>
              </div>
              <div className="bb-cmd-sentence">
                {blanks.map((name) => {
                  const blank: Blank = pick.blanks[name] ?? { kind: 'text' };
                  const value = values[name] ?? blank.default ?? '';
                  const set = (next: string) => setValues((was) => ({ ...was, [name]: next }));
                  return (
                    <label key={name} className="bb-cmd-slot">
                      <span>{pick.kept ? name : runtimeText(`beatbox.commands.blank.${pick.id}`)}</span>
                      {blank.kind === 'node' && nodes.length ? (
                        <Select label={name} value={value} options={nodes} onChange={set} placeholder={t('beatbox.commands.kind.node')} />
                      ) : (
                        <input className="bb-input bb-mono" value={value} onChange={(e) => set(e.target.value)}
                               placeholder={t(`beatbox.commands.kind.${blank.kind}`)}
                               inputMode={blank.kind === 'number' ? 'numeric' : undefined} />
                      )}
                    </label>
                  );
                })}
                {pick.special ? null : (
                  <label className="bb-cmd-slot">
                    <span>{t('beatbox.commands.give_up')}</span>
                    <Select label={t('beatbox.commands.stop_after')} value={String(timeout)} tiny
                            options={TIMEOUTS.map((s) => ({ value: String(s), label: `${s} s` }))}
                            onChange={(v) => setTimeoutSeconds(Number(v))} />
                  </label>
                )}
                <span style={{ flex: 1 }} />
                <Button variant="primary" icon="play" disabled={busy || Boolean(missing)} onClick={runPicked}>
                  {t(busy ? 'beatbox.commands.running' : 'beatbox.commands.run')}
                </Button>
              </div>
              {missing ? <p className="bb-faint" style={{ fontSize: 12, margin: '8px 0 0' }}>{pick.kept ? t('beatbox.commands.fill', { name: missing }) : t('beatbox.commands.fill_it')}</p> : null}
              <code className="bb-cmd-line bb-mono">{pick.special ? line : filled && 'line' in filled ? filled.line : line}</code>
              <div className="bb-cmd-sub">
                <small className="bb-faint">{t(windows ? 'beatbox.commands.shell.windows' : 'beatbox.commands.shell.linux')}</small>
                {pick.kept ? <button type="button" className="bb-linkbtn" onClick={() => setEditing(pick)}>{t('beatbox.commands.edit')}</button> : null}
              </div>
            </Card>

            <Card>
              {told ? <p className="bb-host-say" style={{ margin: '0 0 8px' }}>{told}</p> : null}
              {shownNow ? <Answer run={shownNow} machine={machine} ending={ending} onEnding={setEnding} onEnd={(row) => void end(row)} />
                : <p className="bb-host-say">{t('beatbox.commands.nothing_yet')}</p>}
              <form className="bb-cmd-typed" onSubmit={(e) => { e.preventDefault(); if (typed.trim()) void run(typed.trim(), typed.trim()); }}>
                <span className="bb-cmd-prompt" aria-hidden="true">$</span>
                <input value={typed} onChange={(e) => setTyped(e.target.value)} aria-label={t('beatbox.commands.typed', { name: machine.name })}
                       placeholder={t('beatbox.commands.typed', { name: machine.name })} disabled={busy} />
                <Button size="sm" disabled={!typed.trim()}
                        onClick={() => setEditing({ id: '', name: '', group: 'yours', linux: windows ? '' : typed.trim(), windows: windows ? typed.trim() : '', blanks: {}, timeout: 30, kept: true, machine: '' })}>
                  {t('beatbox.commands.keep')}
                </Button>
              </form>
            </Card>

            {RAN.get(id)?.length ? (
              <div className="bb-cmd-earlier">
                <span className="bb-wf-label">{t('beatbox.commands.earlier')}</span>
                {(RAN.get(id) ?? []).slice(0, 10).map((ran, index) => (
                  <button key={index} type="button" className={`bb-cmd-ran${ran === shown ? ' on' : ''}`} onClick={() => setShown(ran)}>
                    <span className={ran.exit === 0 ? 'bb-mint' : 'bb-rose'}><Icon name={ran.exit === 0 ? 'check' : 'close'} size="xs" /></span>
                    {ran.name}
                    <span className="bb-mono bb-faint">{ran.at}</span>
                  </button>
                ))}
              </div>
            ) : null}
          </div>
        </div>
      ) : null}

      {editing && machine ? (
        <CommandEditor command={editing} machine={machine} onClose={() => setEditing(null)}
                       onSaved={(saved) => { setEditing(null); void loadKept(); if (saved) setPick(saved); }} />
      ) : null}
    </>
  );
}

/** A run's answer: status, a table when we can parse one, and the raw text. */
function Answer({ run, machine, ending, onEnding, onEnd }: {
  run: Run; machine: SandboxView; ending: number | null; onEnding: (pid: number | null) => void; onEnd: (row: Row) => void;
}) {
  const echoes = run.echo && !run.trouble ? readEchoes(run.stdout) : null;
  const ok = !run.trouble && run.exit === 0 && (!echoes || echoes.length > 0);
  const mean = echoes?.length ? Math.round(echoes.reduce((sum, echo) => sum + echo.ms, 0) / echoes.length) : 0;
  const title = run.trouble ? t('beatbox.commands.answer.refused')
    : echoes ? (echoes.length ? t('beatbox.commands.answer.echoes', { name: run.echo!, count: echoes.length, sent: 4 })
      : t('beatbox.commands.answer.silent', { name: run.echo! }))
    : run.rows ? t('beatbox.commands.answer.rows', { count: run.rows.length })
    : run.exit === 0 ? t('beatbox.commands.answer.done') : t('beatbox.commands.answer.failed', { code: run.exit ?? '?' });
  const words = run.stdout || run.stderr || run.trouble;
  return (
    <div className="bb-cmd-answer">
      <div className="bb-cmd-answerhead">
        <span className={`bb-net-mark ${ok ? 'bb-net-mark--ok' : 'bb-net-mark--no'}`}><Icon name={ok ? 'check' : 'close'} size="sm" /></span>
        <span>
          <b>{title}</b>
          <small>{echoes?.length ? t('beatbox.commands.answer.mean', { ms: mean }) : run.name}</small>
        </span>
        <small className="bb-mono bb-faint">{[run.at, `${run.seconds} s`, run.exit !== null ? t('beatbox.commands.exit', { code: run.exit }) : ''].filter(Boolean).join(' · ')}</small>
      </div>
      {echoes?.length ? (
        <div className="bb-cmd-grid bb-cmd-grid--echo">
          <span className="bb-wf-label">{t('beatbox.commands.answer.try')}</span><span className="bb-wf-label">{t('beatbox.commands.answer.from')}</span><span className="bb-wf-label">{t('beatbox.commands.answer.took')}</span>
          {echoes.map((echo, i) => (
            <span key={i} className="bb-cmd-gridrow"><span className="bb-mono bb-faint">{i + 1}</span><span className="bb-mono">{echo.from}</span><span className="bb-mono">{echo.ms} ms</span></span>
          ))}
        </div>
      ) : null}
      {run.rows ? (
        <div className="bb-cmd-table">
          {run.rows.map((row) => (ending === row.pid ? (
            <div key={row.pid} className="bb-proc-row bb-proc-row--asking">
              <b>{t('beatbox.processes.end_sure', { name: row.command })}</b>
              <Button size="sm" variant="danger" onClick={() => onEnd(row)}>{t('beatbox.processes.end')}</Button>
              <Button size="sm" onClick={() => onEnding(null)}>{t('beatbox.processes.keep')}</Button>
            </div>
          ) : (
            <div key={row.pid} className="bb-proc-row">
              <span className="bb-proc-name">{row.command}</span>
              <small>{row.user}</small>
              <small className="bb-mono">{row.pid}</small>
              {row.pid > 1 && machine.capabilities.includes('process.kill') ? (
                <Button size="sm" onClick={() => onEnding(row.pid)}>{t('beatbox.processes.end')}</Button>
              ) : null}
            </div>
          )))}
        </div>
      ) : null}
      {!run.rows && words ? (
        echoes?.length ? (
          <details className="bb-cmd-words"><summary>{t('beatbox.commands.answer.words')}</summary><Words run={run} /></details>
        ) : <Words run={run} />
      ) : null}
    </div>
  );
}

function Words({ run }: { run: Run }) {
  return (
    <pre className="bb-cmd-out"><span className="bb-cmd-prompt">$ </span>{run.line}{'\n'}{run.stdout}{run.stderr ? <span className="bb-rose">{run.stderr}</span> : null}{run.trouble ? <span className="bb-rose">{run.trouble}</span> : null}</pre>
  );
}

const KINDS: BlankKind[] = ['text', 'number', 'path', 'node'];

/** New command, or edit/delete a saved one. */
function CommandEditor({ command, machine, onClose, onSaved }: {
  command: Command;
  machine: SandboxView;
  onClose: () => void;
  onSaved: (saved: Command | null) => void;
}) {
  const [name, setName] = useState(command.name);
  const [linux, setLinux] = useState(command.linux);
  const [windows, setWindows] = useState(command.windows);
  const [blanks, setBlanks] = useState<Record<string, Blank>>(command.blanks);
  const [only, setOnly] = useState(Boolean(command.machine));
  const [told, setTold] = useState('');
  const found = blanksIn(linux, windows);
  const blank = (key: string): Blank => blanks[key] ?? { kind: 'text' };

  async function save() {
    setTold('');
    try {
      const all = await call('commands.save', {
        command: {
          id: command.id, name, linux, windows, timeout: command.timeout, machine: only ? machine.id : '',
          blanks: Object.fromEntries(found.map((key) => [key, blank(key)])),
        },
      });
      const saved = all.find((c) => c.name === name.trim() && (command.id ? c.id === command.id : true));
      onSaved(saved ? fromKept(saved) : null);
    } catch (error) {
      setTold(fault(error));
    }
  }

  async function forget() {
    try {
      await call('commands.forget', { id: command.id });
      onSaved(null);
    } catch (error) {
      setTold(fault(error));
    }
  }

  return (
    <Modal title={t(command.id ? 'beatbox.commands.edit_title' : 'beatbox.commands.new_title')} icon="terminal" width={720} onClose={onClose}
           footer={<>
             {command.id ? <Button variant="danger" onClick={() => void forget()}>{t('beatbox.commands.forget')}</Button> : null}
             <span style={{ flex: 1 }} />
             <Button onClick={onClose}>{t('beatbox.commands.cancel')}</Button>
             <Button variant="primary" icon="check" disabled={!name.trim() || !(linux.trim() || windows.trim())}
                     onClick={() => void save()}>{t('beatbox.commands.save')}</Button>
           </>}>
      <div className="bb-sbx-form">
        <label className="bb-field">
          <span>{t('beatbox.commands.name')}</span>
          <input className="bb-input" value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="bb-field">
          <span>{t('beatbox.commands.linux')} <Info text={t('beatbox.commands.blanks_what')} /></span>
          <textarea className="bb-input bb-mono" rows={2} value={linux} onChange={(e) => setLinux(e.target.value)} />
        </label>
        <label className="bb-field">
          <span>{t('beatbox.commands.windows')}</span>
          <textarea className="bb-input bb-mono" rows={2} value={windows} onChange={(e) => setWindows(e.target.value)} />
        </label>
        {found.length ? (
          <div className="bb-field">
            <span>{t('beatbox.commands.blanks')}</span>
            {found.map((key) => (
              <div key={key} className="bb-cmd-slot">
                <code className="bb-mono">{`{${key}}`}</code>
                <Select label={t('beatbox.commands.kind')} value={blank(key).kind}
                        options={KINDS.map((kind) => ({ value: kind, label: t(`beatbox.commands.kind.${kind}`) }))}
                        onChange={(kind) => setBlanks((was) => ({ ...was, [key]: { ...blank(key), kind: kind as BlankKind } }))} />
                <input className="bb-input bb-mono" value={blank(key).default ?? ''} placeholder={t('beatbox.commands.default')}
                       aria-label={t('beatbox.commands.default')}
                       onChange={(e) => setBlanks((was) => ({ ...was, [key]: { ...blank(key), default: e.target.value } }))} />
              </div>
            ))}
          </div>
        ) : null}
        <div className="bb-field">
          <span>{t('beatbox.commands.for')}</span>
          <div className="bb-sbx-choices" style={{ gridTemplateColumns: 'repeat(2, 1fr)' }}>
            <button type="button" className={`bb-choice${!only ? ' on' : ''}`} onClick={() => setOnly(false)}>
              <b>{t('beatbox.commands.for.every')}</b><small>{t('beatbox.commands.for.every.what')}</small>
            </button>
            <button type="button" className={`bb-choice${only ? ' on' : ''}`} onClick={() => setOnly(true)}>
              <b>{t('beatbox.commands.for.only', { name: machine.name })}</b><small>{t('beatbox.commands.for.only.what')}</small>
            </button>
          </div>
        </div>
        {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
      </div>
    </Modal>
  );
}
