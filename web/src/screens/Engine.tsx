import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Button, Card, ChipRow, Cube, CubeWire, FilePicker, Icon, KeyValue, Modal, Pill, Select, Switch,
} from '../components';
import type { Tone } from '../components';
import { call, on } from '../bridge/client';
import type { SandboxEventView, SandboxSummary, SandboxTypeView, SandboxView } from '../bridge/types';
import { TRANSITIONS } from '../bridge/types';
import { fault, say, t, runtimeText } from '../i18n';

/**
 * The runtime, raw: a debugger for whoever builds BeatBox, not the product
 * (that's Sandbox.tsx, in plain words).
 *
 * Engine names everywhere: transitions, capabilities, facts, ids, events in
 * bus order. Transitions come from `view.transitions` and actions from
 * `view.capabilities`, never a local copy that could drift. The only things
 * decided here: which facts get a line, and ARGUMENTS.
 */

/** Lifecycle colours: trouble should look like trouble. */
const TONES: Record<string, Tone> = {
  created: 'sky', running: 'mint', paused: 'amber',
  stopped: 'neutral', failed: 'rose', destroyed: 'neutral',
};

/**
 * Argument names per capability, so we can draw fields. Delete this once the
 * engine declares arguments itself. Missing ones get a JSON box: clunky, never wrong.
 */
const ARGUMENTS: Record<string, readonly string[]> = {
  'process.execute': ['command', 'arguments'],
  'process.shell': ['line', 'timeout'],
  'process.list': [],
  'filesystem.read': ['path'],
  'filesystem.write': ['path', 'content'],
  'filesystem.list': [],
  'network.interfaces': [],
  'snapshot.create': ['name'],
  'snapshot.restore': ['name'],
  'snapshot.delete': ['name'],
  'snapshot.save_state': [],
  'snapshot.list': [],
};

/** Facts worth their own line, in reading order. */
const FACTS = ['status', 'accelerated', 'agent', 'addresses', 'processes', 'snapshots', 'disk', 'overlay'] as const;

/**
 * Read one sandbox, observing it or not. If `sandbox.refresh` fails (provider
 * gone) fall back to what we know; an empty panel is useless right when it dies.
 */
async function read(id: string, observe: boolean) {
  if (!observe) return call('sandbox.view', { id });
  try {
    return await call('sandbox.refresh', { id });
  } catch {
    return call('sandbox.view', { id });
  }
}

const bag = (value: unknown): Record<string, unknown> =>
  (value && typeof value === 'object' ? value as Record<string, unknown> : {});

function show(value: unknown): string {
  if (value === null || value === undefined) return '—';
  if (Array.isArray(value)) return value.length ? value.map(show).join(', ') : '—';
  if (typeof value === 'object') return JSON.stringify(value);
  if (typeof value === 'boolean') return value ? t('beatbox.yes') : t('beatbox.no');
  return String(value);
}

function amounts(value: unknown): string {
  const entries = Object.entries(bag(value));
  if (!entries.length) return '—';
  return entries
    .map(([kind, amount]) => runtimeText(`beatbox.resource.${kind}`, { amount: show(amount) }))
    .join(' · ');
}

/* --- making one ----------------------------------------------------------- */

function NewSandbox({ types, onMade, onClose }: {
  types: SandboxTypeView[];
  onMade: (made: SandboxView) => void;
  onClose: () => void;
}) {
  const [type, setType] = useState(types[0]?.name ?? '');
  const [name, setName] = useState('');
  const [cpu, setCpu] = useState('2');
  const [memory, setMemory] = useState('2048');
  const [disk, setDisk] = useState('');
  const [network, setNetwork] = useState('none');
  const [agent, setAgent] = useState(true);
  const [browsing, setBrowsing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  async function make() {
    setBusy(true);
    setFailure(null);
    try {
      onMade(await call('sandbox.create', {
        type,
        name,
        cpu: Number(cpu) || 0,
        memory: Number(memory) || 0,
        settings: { disk, network, agent },
        uses: {},
      }));
    } catch (error) {
      setFailure(fault(error));
      setBusy(false);
    }
  }

  if (browsing) {
    return (
      <FilePicker
        title={t('beatbox.sandbox.form.pick_disk')}
        suffixes=".qcow2 .img .raw .iso .vmdk .vdi"
        filterLabel={t('beatbox.sandbox.form.disks_only')}
        start={disk}
        onPick={(path) => { setDisk(path); setBrowsing(false); }}
        onClose={() => setBrowsing(false)}
      />
    );
  }

  return (
    <Modal title={t('beatbox.sandbox.new')} icon="sandbox" width={560} onClose={onClose}
           footer={
             <>
               <span style={{ flex: 1 }} />
               <Button onClick={onClose}>{t('beatbox.closing.cancel')}</Button>
               <Button variant="primary" autofocus disabled={busy || !type || !disk} onClick={make}>
                 {busy ? t('beatbox.sandbox.making') : t('beatbox.sandbox.make')}
               </Button>
             </>
           }>
      <div className="bb-sbx-form">
        <label className="bb-field">
          <span>{t('beatbox.sandbox.form.type')}</span>
          <Select label={t('beatbox.sandbox.form.type')} value={type} onChange={setType}
                  options={types.map((entry) => ({
                    value: entry.name,
                    label: t('beatbox.sandbox.form.type_at', { type: entry.name, boundary: entry.isolation }),
                  }))} />
        </label>
        <label className="bb-field">
          <span>{t('beatbox.sandbox.form.name')}</span>
          <input className="bb-input" value={name} onChange={(e) => setName(e.target.value)}
                 placeholder={type} />
        </label>
        <div className="bb-sbx-pair">
          <label className="bb-field">
            <span>{t('beatbox.sandbox.form.cpu')}</span>
            <input className="bb-input" value={cpu} inputMode="decimal"
                   onChange={(e) => setCpu(e.target.value)} />
          </label>
          <label className="bb-field">
            <span>{t('beatbox.sandbox.form.memory')}</span>
            <input className="bb-input" value={memory} inputMode="numeric"
                   onChange={(e) => setMemory(e.target.value)} />
          </label>
        </div>
        <label className="bb-field">
          <span>{t('beatbox.sandbox.form.disk')}</span>
          <div className="bb-shelf" style={{ gap: 8 }}>
            <input className="bb-input" value={disk} onChange={(e) => setDisk(e.target.value)}
                   placeholder={t('beatbox.sandbox.form.disk_placeholder')} />
            <Button size="sm" icon="folder" onClick={() => setBrowsing(true)}>
              {t('beatbox.sandbox.form.browse')}
            </Button>
          </div>
          <small className="bb-faint">{t('beatbox.sandbox.form.disk_hint')}</small>
        </label>
        <label className="bb-field">
          <span>{t('beatbox.sandbox.form.network')}</span>
          <ChipRow current={network} style={{ margin: 0 }}>
            {['none', 'isolated', 'nat'].map((mode) => (
              <button key={mode} type="button" className="bb-chip" aria-current={network === mode}
                      onClick={() => setNetwork(mode)}>
                {runtimeText(`beatbox.network.${mode}`)}
              </button>
            ))}
          </ChipRow>
          <small className="bb-faint">{runtimeText(`beatbox.network.${network}.what`)}</small>
        </label>
        {/* Switch's `label` is only for screen readers; we draw the visible one. */}
        <div className="bb-sbx-toggle">
          <Switch id="bb-agent" checked={agent} onChange={setAgent}
                  label={t('beatbox.sandbox.form.agent')} />
          <label htmlFor="bb-agent">
            <b>{t('beatbox.sandbox.form.agent')}</b>
            <small className="bb-faint">{t('beatbox.sandbox.form.agent_hint')}</small>
          </label>
        </div>
        {failure ? <p className="bb-rose" style={{ fontSize: 12.5 }}>{failure}</p> : null}
      </div>
    </Modal>
  );
}

/* --- asking one to do something ------------------------------------------- */

function Doing({ sandbox, onDone }: { sandbox: SandboxView; onDone: () => void }) {
  const [capability, setCapability] = useState(sandbox.capabilities[0] ?? '');
  const [values, setValues] = useState<Record<string, string>>({});
  const [outcome, setOutcome] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const fields = ARGUMENTS[capability] ?? ['arguments'];
  const known = capability in ARGUMENTS;
  const running = sandbox.state.lifecycle === 'running';

  async function run() {
    setBusy(true);
    try {
      const args: Record<string, unknown> = {};
      for (const field of fields) {
        const raw = (values[field] ?? '').trim();
        if (!raw) continue;
        if (!known) {
          try { Object.assign(args, JSON.parse(raw)); } catch { /* not JSON: sent as typed below */ }
        } else if (field === 'arguments') {
          args[field] = raw.split(/\s+/);
        } else {
          args[field] = raw;
        }
      }
      const answer = await call('sandbox.act', { id: sandbox.id, capability, arguments: args });
      setOutcome({
        ok: answer.ok,
        text: answer.ok ? JSON.stringify(answer.data, null, 1) : say(answer.reason),
      });
    } catch (error) {
      setOutcome({ ok: false, text: fault(error) });
    }
    setBusy(false);
    onDone();
  }

  if (!sandbox.capabilities.length) {
    return <p className="bb-faint" style={{ fontSize: 12.5 }}>{t('beatbox.sandbox.can_nothing')}</p>;
  }

  return (
    <>
      <div className="bb-sbx-ask">
        <Select label={t('beatbox.sandbox.capability')} value={capability}
                onChange={(next) => { setCapability(next); setOutcome(null); }}
                options={sandbox.capabilities.map((name) => ({ value: name, label: name }))} />
        {fields.map((field) => (
          <input key={field} className="bb-input" value={values[field] ?? ''}
                 placeholder={known ? runtimeText(`beatbox.argument.${field}`) : t('beatbox.argument.json')}
                 onChange={(e) => setValues({ ...values, [field]: e.target.value })} />
        ))}
        <Button variant="primary" size="sm" disabled={busy || !running} onClick={run}
                title={running ? undefined : t('beatbox.sandbox.not_running')}>
          {t('beatbox.sandbox.run')}
        </Button>
      </div>
      {!running ? (
        <small className="bb-faint">{t('beatbox.sandbox.not_running')}</small>
      ) : null}
      {outcome ? (
        <pre className={`bb-sbx-out bb-mono${outcome.ok ? '' : ' bb-rose'}`}>{outcome.text}</pre>
      ) : null}
    </>
  );
}

/* --- the screen ----------------------------------------------------------- */

export function Engine() {
  const [types, setTypes] = useState<SandboxTypeView[]>([]);
  const [list, setList] = useState<SandboxSummary[]>([]);
  const [chosen, setChosen] = useState<string>('');
  const [sandbox, setSandbox] = useState<SandboxView | null>(null);
  const [log, setLog] = useState<SandboxEventView[]>([]);
  const [making, setMaking] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  /**
   * Re-read the list and the open sandbox. `observe` asks the guest itself.
   * Events just re-read; a person's click observes, or a fresh screen shows stale facts.
   */
  const reload = useCallback(async (id: string, observe = false) => {
    const rows = await call('sandbox.list');
    setList(rows);
    const pick = rows.some((row) => row.id === id) ? id : (rows[0]?.id ?? '');
    setChosen(pick);
    setSandbox(pick ? await read(pick, observe) : null);
  }, []);

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const [kinds, backlog] = await Promise.all([
          call('sandbox.types'),
          call('sandbox.events', { since: 0 }),
        ]);
        if (!live) return;
        setTypes(kinds);
        setLog(backlog.slice(-200).reverse());
        await reload('', true);
      } catch (error) {
        if (live) setFailure(fault(error));
      }
    })();
    return () => { live = false; };
  }, [reload]);

  // Pushed, not polled. Lifecycle events mean the list is stale: re-read, don't guess.
  useEffect(() => on('sandbox', (event) => {
    setLog((was) => [event, ...was].slice(0, 200));
    if (event.name.startsWith('sandbox.')) {
      setChosen((current) => { void reload(current); return current; });
    }
  }), [reload]);

  async function choose(id: string) {
    setChosen(id);
    try {
      setSandbox(await read(id, true));
    } catch (error) {
      setFailure(fault(error));
    }
  }

  async function move(transition: string) {
    if (!sandbox) return;
    setFailure(null);
    try {
      setSandbox(await call('sandbox.transition', { id: sandbox.id, transition }));
    } catch (error) {
      setFailure(fault(error));
    }
    await reload(sandbox.id);
  }

  async function look() {
    if (!sandbox) return;
    try {
      setSandbox(await call('sandbox.refresh', { id: sandbox.id }));
    } catch (error) {
      setFailure(fault(error));
    }
  }

  const facts = useMemo(() => bag(sandbox?.state.facts), [sandbox]);

  return (
    <>
      <div className="bb-pagehead">
        <div>
          <h1 className="bb-t-title" style={{ margin: 0 }}>{t('beatbox.screen.engine.title')}</h1>
          <p className="bb-lede">{t('beatbox.screen.engine.blurb')}</p>
        </div>
        <Button variant="primary" icon="plus" disabled={!types.length} onClick={() => setMaking(true)}>
          {t('beatbox.sandbox.new')}
        </Button>
      </div>

      {failure ? <p className="bb-rose" style={{ fontSize: 12.5 }}>{failure}</p> : null}

      {!list.length ? (
        <div className="bb-pending" style={{ minHeight: 300 }}>
          <CubeWire size={84} tone="violet" opacity={0.42} />
          <p>{t('beatbox.sandbox.none')}</p>
          <Button variant="primary" trailingIcon="arrow" disabled={!types.length}
                  onClick={() => setMaking(true)}>
            {t('beatbox.sandbox.new')}
          </Button>
        </div>
      ) : (
        <div className="bb-g2">
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--bb-space-3)' }}>
            {sandbox ? (
              <>
                <Card title={<><Cube size={18} tone="violet" />{sandbox.name}</>}
                      note={sandbox.id}>
                  <div className="bb-shelf" style={{ gap: 8, marginBottom: 'var(--bb-space-3)' }}>
                    <Pill tone={TONES[sandbox.state.lifecycle] ?? 'neutral'}>
                      {runtimeText(`beatbox.lifecycle.${sandbox.state.lifecycle}`)}
                    </Pill>
                    <span style={{ flex: 1 }} />
                    {TRANSITIONS.map((name) => {
                      const can = sandbox.transitions.includes(name);
                      // Primary only when enabled: a disabled primary still
                      // looked like a live "Start" on a running guest.
                      const variant = name === 'destroy' ? 'danger'
                        : can && (name === 'start' || name === 'resume') ? 'primary'
                        : 'ghost';
                      return (
                        <Button key={name} size="sm" variant={variant} disabled={!can}
                                onClick={() => move(name)}>
                          {t(`beatbox.transition.${name}`)}
                        </Button>
                      );
                    })}
                  </div>
                  {sandbox.state.reason ? (
                    <p className="bb-rose" style={{ fontSize: 12.5 }}>{say(sandbox.state.reason)}</p>
                  ) : null}
                  <KeyValue label={t('beatbox.sandbox.type')}>
                    {sandbox.type} · {runtimeText(`beatbox.isolation.${sandbox.isolation}`)}
                  </KeyValue>
                  <KeyValue label={t('beatbox.sandbox.asked')}>{amounts(sandbox.config.resources)}</KeyValue>
                  <KeyValue label={t('beatbox.sandbox.using')}>{amounts(sandbox.state.resources)}</KeyValue>
                  {FACTS.filter((key) => key in facts).map((key) => (
                    <KeyValue key={key} label={t(`beatbox.fact.${key}`)}>
                      <span className="bb-mono" style={{ fontSize: 11.5 }}>{show(facts[key])}</span>
                    </KeyValue>
                  ))}
                  <div className="bb-shelf" style={{ marginTop: 'var(--bb-space-3)' }}>
                    <Button size="sm" icon="eye" onClick={look}>{t('beatbox.sandbox.look')}</Button>
                    <small className="bb-faint">{t('beatbox.sandbox.look_hint')}</small>
                  </div>
                </Card>

                <Card title={<><Icon name="play" size="sm" />{t('beatbox.sandbox.can')}</>}
                      note={`${sandbox.capabilities.length}`}>
                  <Doing sandbox={sandbox} onDone={look} />
                </Card>
              </>
            ) : (
              <Card>{t('beatbox.sandbox.pick_one')}</Card>
            )}
          </div>

          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--bb-space-3)' }}>
            <Card title={<><Icon name="sandbox" size="sm" />{t('beatbox.sandbox.list')}</>}
                  note={`${list.length}`}>
              {list.map((row) => (
                <button key={row.id} type="button" className={`bb-sbx-row${row.id === chosen ? ' on' : ''}`}
                        onClick={() => void choose(row.id)}>
                  <Cube size={16} tone={row.lifecycle === 'running' ? 'mint' : 'slate'} />
                  <span className="bb-sbx-name">{row.name}</span>
                  <Pill tone={TONES[row.lifecycle] ?? 'neutral'} dot={false}>
                    {runtimeText(`beatbox.lifecycle.${row.lifecycle}`)}
                  </Pill>
                </button>
              ))}
            </Card>

            <Card title={<><Icon name="pulse" size="sm" />{t('beatbox.sandbox.log')}</>}
                  note={log.length ? `${log[0]!.sequence}` : undefined}>
              {log.length ? (
                <div className="bb-sbx-log">
                  {log.map((event) => (
                    <div key={event.sequence}>
                      <span className="bb-mono bb-faint">{event.sequence}</span>
                      <b className="bb-mono">{event.name}</b>
                      <span className="bb-faint">{show(event.data)}</span>
                    </div>
                  ))}
                </div>
              ) : (
                <p className="bb-faint" style={{ fontSize: 12.5, margin: 0 }}>
                  {t('beatbox.sandbox.no_events')}
                </p>
              )}
            </Card>
          </div>
        </div>
      )}

      {making ? (
        <NewSandbox types={types} onClose={() => setMaking(false)}
                    onMade={(made) => { setMaking(false); void reload(made.id); }} />
      ) : null}
    </>
  );
}
