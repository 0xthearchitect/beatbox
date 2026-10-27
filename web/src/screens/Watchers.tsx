import { useCallback, useEffect, useState } from 'react';
import { Button, Card, CubeWire, Icon, Info, Pill, Select, type Tone } from '../components';
import { call, on } from '../bridge/client';
import type { SandboxEventView, WatcherView } from '../bridge/types';
import { useChosenMachine } from '../chosen';
import { fault, say, t, runtimeText } from '../i18n';
import { BUILT_IN, blanksIn, fill, fromKept, lineFor, type Command } from '../library';
import { navigate } from '../router';
import { isWindows, useNodes } from './Commands';

/**
 * Watchers: a command run over and over in a machine, reported when the answer
 * changes, fails or shows some text.
 *
 * The daemon runs them (`watchers.*`), so they keep going with the window
 * closed; news goes out as `watcher.changed`. Machines that are off are skipped.
 */

/** How often the page refreshes the list (ms). The daemon's own schedule is separate. */
const LOOK = 3000;
const EVERY = [5, 10, 30, 60, 300];
const TELLS = ['change', 'fails', 'contains'] as const;
const TONE: Record<string, Tone> = {
  watching: 'mint', failing: 'rose', waiting: 'neutral', paused: 'neutral', no_agent: 'amber', gone: 'neutral',
};

const tail = (text: string, lines: number) => text.trimEnd().split('\n').slice(-lines).join('\n');

export function Watchers() {
  const { list, machine } = useChosenMachine();
  const id = machine?.id ?? '';
  const windows = isWindows(machine);
  const [watchers, setWatchers] = useState<WatcherView[] | null>(null);
  const [said, setSaid] = useState<SandboxEventView[]>([]);
  const [kept, setKept] = useState<Command[]>([]);
  const [told, setTold] = useState('');
  const [preset, setPreset] = useState<Preset | null>(null);
  const nodes = useNodes(machine);

  const look = useCallback(async () => {
    setWatchers((await call('watchers.list')).filter((w) => w.sandboxId === id));
  }, [id]);

  useEffect(() => {
    if (!id) return;
    void look().catch(() => setWatchers([]));
    void call('commands.list')
      .then((all) => setKept(all.filter((c) => !c.machine || c.machine === id).map(fromKept)))
      .catch(() => setKept([]));
    const beat = window.setInterval(() => void look().catch(() => undefined), LOOK);
    return () => window.clearInterval(beat);
  }, [id, look]);

  useEffect(() => {
    if (!id) return;
    const mine = (event: SandboxEventView) => event.sandboxId === id && event.name === 'watcher.changed';
    void call('sandbox.events', { since: 0 })
      .then((events) => setSaid(events.filter(mine).slice(-30).reverse()))
      .catch(() => undefined);
    return on('sandbox', (event) => {
      if (mine(event)) {
        setSaid((was) => [event, ...was].slice(0, 30));
        void look().catch(() => undefined);
      }
    });
  }, [id, look]);

  async function change(watcher: Record<string, unknown>) {
    setTold('');
    try {
      setWatchers((await call('watchers.save', { watcher })).filter((w) => w.sandboxId === id));
    } catch (error) {
      setTold(fault(error));
    }
  }

  async function forget(watcherId: string) {
    try {
      setWatchers((await call('watchers.forget', { id: watcherId })).filter((w) => w.sandboxId === id));
    } catch (error) {
      setTold(fault(error));
    }
  }

  // No blanks allowed: nobody is there to fill them when a watcher runs.
  const offered = [...BUILT_IN, ...kept].filter((c) => !c.special && lineFor(c, windows) && !blanksIn(lineFor(c, windows)).length);

  // Starter suggestions for an empty list: built-ins, aimed at the first
  // machine on the network.
  const reach = BUILT_IN.find((c) => c.id === 'reach')!;
  const node = nodes[0];
  const reachLine = node ? fill(lineFor(reach, windows), reach.blanks, { node: node.value }, windows) : null;
  const ideas: (Preset & { key: string; title: string })[] = [
    { key: 'ports', title: t('beatbox.watchers.idea.ports'), from: 'ports', every: '10', tell: 'change' },
    { key: 'downloads', title: t('beatbox.watchers.idea.downloads'), from: 'downloads', every: '30', tell: 'change' },
    { key: 'who', title: t('beatbox.watchers.idea.who'), from: 'who', every: '30', tell: 'change' },
    ...(node && reachLine && 'line' in reachLine ? [{
      key: 'reach', title: t('beatbox.watchers.idea.reach', { name: node.label }), from: 'own', line: reachLine.line,
      name: t('beatbox.watchers.idea.reach_what', { name: node.label }), every: '30', tell: 'fails',
    }] : []),
  ];
  const ideaWhat = (idea: Preset) => t('beatbox.watchers.idea.what', {
    seconds: idea.every,
    what: idea.from === 'own' ? idea.name ?? '' : runtimeText(`beatbox.commands.built.${idea.from}`),
    when: runtimeText(`beatbox.watchers.when.${idea.tell}`).toLowerCase(),
  });

  return (
    <>
      <div className="bb-pagehead">
        <div>
          <div className="bb-pagehead-title">
            <h1 className="bb-t-title" style={{ margin: 0 }}>{t('beatbox.screen.host.watchers.title')}</h1>
            <Info text={machine ? t('beatbox.watchers.about', { name: machine.name }) : t('beatbox.screen.host.watchers.blurb')} />
          </div>
        </div>
      </div>

      {list && !list.length ? (
        <div className="bb-pending" style={{ minHeight: 280 }}>
          <CubeWire size={80} tone="violet" opacity={0.42} />
          <p>{t('beatbox.host_pages.no_machines')}</p>
          <Button variant="primary" trailingIcon="arrow" onClick={() => navigate('host/machine')}>{t('beatbox.host_pages.to_machine')}</Button>
        </div>
      ) : machine && !machine.capabilities.includes('process.shell') ? (
        <Card><p className="bb-host-say">{t('beatbox.commands.no_agent')}</p></Card>
      ) : machine ? (
        <div className="bb-net-stack">
          <NewWatcher offered={offered} windows={windows} machine={machine.name} preset={preset}
                      onStart={(watcher) => change({ ...watcher, sandboxId: id })} />
          {told ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told}</p> : null}
          <div className="bb-net-grid">
            <div className="bb-net-stack">
              {machine.state.lifecycle !== 'running' && watchers?.length ? (
                <p className="bb-host-say bb-amber">{t('beatbox.watchers.machine_off', { name: machine.name })}</p>
              ) : null}
              {watchers === null ? null : watchers.length ? watchers.map((watcher) => (
                <Card key={watcher.id}
                      title={<>
                        <Icon name="eye" size="sm" />{watcher.name}
                        <Pill tone={TONE[watcher.state] ?? 'neutral'}>{runtimeText(`beatbox.watchers.state.${watcher.state}`)}</Pill>
                      </>}
                      note={t('beatbox.watchers.every', { seconds: watcher.every })}>
                  {watcher.name !== watcher.line ? <code className="bb-mono bb-faint" style={{ fontSize: 12 }}>{watcher.line}</code> : null}
                  {watcher.lastReason ? <p className="bb-host-say bb-rose" style={{ marginTop: 8 }}>{say(watcher.lastReason)}</p> : null}
                  {watcher.changedAt ? (
                    <p className="bb-watch-news">
                      {t('beatbox.watchers.changed_at', { at: new Date(watcher.changedAt).toLocaleTimeString([], { hour12: false }) })}
                    </p>
                  ) : null}
                  {watcher.lastOutput ? <pre className="bb-cmd-out bb-cmd-out--small">{tail(watcher.lastOutput, 8)}</pre> : null}
                  <div className="bb-watch-foot">
                    <span className="bb-faint">
                      {runtimeText(`beatbox.watchers.tell.${watcher.tell}`, { text: watcher.text })}
                      {watcher.lastAt ? ` · ${t('beatbox.watchers.last', { at: new Date(watcher.lastAt).toLocaleTimeString([], { hour12: false }), runs: watcher.runs })}` : ''}
                    </span>
                    <Button size="sm" onClick={() => void change({ id: watcher.id, paused: !watcher.paused })}>
                      {t(watcher.paused ? 'beatbox.watchers.resume' : 'beatbox.watchers.pause')}
                    </Button>
                    <Button size="sm" variant="danger" onClick={() => void forget(watcher.id)}>{t('beatbox.watchers.forget')}</Button>
                  </div>
                </Card>
              )) : (
                <>
                  <div className="bb-wf-label">{t('beatbox.watchers.start_here')}</div>
                  <div className="bb-watch-ideas">
                    {ideas.map((idea) => (
                      <div key={idea.key} className="bb-watch-idea">
                        <b>{idea.title}</b>
                        <p>{ideaWhat(idea)}</p>
                        <Button size="sm" onClick={() => setPreset({ ...idea, stamp: Date.now() })}>{t('beatbox.watchers.use')}</Button>
                      </div>
                    ))}
                  </div>
                </>
              )}
            </div>

            <Card title={<><Icon name="logs" size="sm" />{t('beatbox.watchers.said')}</>}>
              {said.length ? said.map((event) => {
                const data = (event.data ?? {}) as Record<string, unknown>;
                return (
                  <div key={event.sequence} className="bb-proc-row">
                    <small className="bb-mono">{new Date(event.at).toLocaleTimeString([], { hour12: false })}</small>
                    <Pill tone={data.because === 'failed' ? 'rose' : 'amber'} dot={false}>{runtimeText(`beatbox.watchers.because.${String(data.because)}`)}</Pill>
                    <span className="bb-proc-name">{String(data.name ?? '')}</span>
                  </div>
                );
              }) : <p className="bb-host-say">{t('beatbox.watchers.said_none_on', { name: machine.name })}</p>}
            </Card>
          </div>
        </div>
      ) : null}
    </>
  );
}

/** A way to start a watcher: a built-in by its id, or a line of its own with a name. */
interface Preset { from: string; line?: string; name?: string; every: string; tell: string; stamp?: number }

function NewWatcher({ offered, windows, machine, preset, onStart }: {
  offered: Command[];
  windows: boolean;
  machine: string;
  preset: Preset | null;
  onStart: (watcher: Record<string, unknown>) => Promise<void>;
}) {
  const [from, setFrom] = useState('ports');
  const [line, setLine] = useState('');
  const [named, setNamed] = useState('');
  const [every, setEvery] = useState('10');
  const [tell, setTell] = useState('change');
  const [text, setText] = useState('');
  useEffect(() => {
    if (!preset) return;
    setFrom(preset.from);
    setLine(preset.line ?? '');
    setNamed(preset.name ?? '');
    setEvery(preset.every);
    setTell(preset.tell);
  }, [preset]);
  const picked = offered.find((c) => c.id === from);
  const name = picked ? (picked.kept ? picked.name : runtimeText(`beatbox.commands.built.${picked.id}`)) : named || line.trim();
  const run = picked ? lineFor(picked, windows) : line.trim();

  return (
    <Card>
      <div className="bb-wf-label">{t('beatbox.watchers.new')} <Info text={t('beatbox.watchers.what')} /></div>
      <div className="bb-cmd-sentence bb-watch-sentence">
        <span>{t('beatbox.watchers.s.every')}</span>
        <Select label={t('beatbox.watchers.how_often')} value={every} onChange={setEvery} tiny
                options={EVERY.map((s) => ({ value: String(s), label: `${s} s` }))} />
        <span>{t('beatbox.watchers.s.run')}</span>
        <Select label={t('beatbox.watchers.which')} value={from}
                options={[...offered.map((c) => ({ value: c.id, label: c.kept ? c.name : runtimeText(`beatbox.commands.built.${c.id}`) })),
                  { value: 'own', label: t('beatbox.watchers.own_line') }]}
                onChange={(v) => { setFrom(v); setNamed(''); }} />
        {picked ? null : (
          <input className="bb-input bb-mono bb-watch-own" value={line} onChange={(e) => { setLine(e.target.value); setNamed(''); }}
                 placeholder={windows ? 'Get-ChildItem $HOME\\Downloads' : 'ls ~/Downloads'} aria-label={t('beatbox.watchers.own_line')} />
        )}
        <span>{t('beatbox.watchers.s.tell')}</span>
        <Select label={t('beatbox.watchers.tell_me')} value={tell} onChange={setTell}
                options={TELLS.map((v) => ({ value: v, label: t(`beatbox.watchers.when.${v}`) }))} />
        {tell === 'contains' ? (
          <input className="bb-input bb-mono bb-watch-own" value={text} onChange={(e) => setText(e.target.value)}
                 placeholder={t('beatbox.watchers.text')} aria-label={t('beatbox.watchers.text')} />
        ) : null}
        <span style={{ flex: 1 }} />
        <Button variant="primary" icon="eye" disabled={!run || (tell === 'contains' && !text.trim())}
                onClick={() => void onStart({ name, line: run, every: Number(every), tell, text: text.trim() }).then(() => { setLine(''); setText(''); setNamed(''); })}>
          {t('beatbox.watchers.start')}
        </Button>
      </div>
      {run ? <p className="bb-faint bb-watch-runs">{t('beatbox.watchers.runs_inside', { name: machine })} <code className="bb-mono">{run}</code></p> : null}
    </Card>
  );
}
