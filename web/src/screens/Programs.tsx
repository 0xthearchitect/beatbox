import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { Button, Card, CubeWire, Icon, Info, Modal } from '../components';
import { call } from '../bridge/client';
import type { ReasonView } from '../bridge/types';
import { runtimeText, say, t } from '../i18n';
import { navigate } from '../router';
import { useChosenMachine } from '../chosen';

/**
 * Programs BeatBox can add to a machine, from the Host's recipes.
 *
 * Adding is driven by this page: each `programs.adding` does the next step
 * that needs the guest (copy one file, start the install, try it), so the
 * dialog stays open until it is done or stopped.
 */

export interface Program {
  id: string; name: string; group: string; what: string; own: boolean; command: string;
  how: 'apt' | 'file'; package: string | null; source: string; kept_off: boolean; path: string;
  on: boolean; mine: boolean; version: string; files: number; bytes: number;
  cannot: 'arch' | 'system' | 'lists' | 'absent' | null;
}

export interface Listing {
  windows: boolean; system: string; arch: string; free_kb: number; busy: boolean;
  apt: boolean; reach: boolean; lists: boolean; adding: boolean; programs: Program[];
}

interface Adding {
  phase: 'none' | 'download' | 'copy' | 'install' | 'try' | 'done' | 'failed';
  /** `lists` and `indexes` while the machine's package lists are refreshed first. */
  how: 'file' | 'debs' | 'apt' | 'lists' | 'indexes'; name: string; files: number; fetched: number;
  bytes: number; got: number; copied: number; said: string; lines: string[]; failed: ReasonView | null;
  refreshed: boolean; list_bytes: number;
}

const GROUPS = ['email', 'databases', 'websites', 'tools'] as const;

async function hostAct(id: string, capability: string, args: Record<string, unknown> = {}) {
  const outcome = await call('sandbox.act', { id, capability, arguments: args });
  if (!outcome.ok) throw new Error(say(outcome.reason));
  return (outcome.data ?? {}) as Record<string, unknown>;
}

/** Bytes as a person reads a download: 10.7 MB, 167 KB. */
export const bytesText = (bytes: number) => {
  const mb = bytes / 1024 / 1024;
  if (mb >= 1024) return `${(mb / 1024).toFixed(1).replace(/\.0$/, '')} GB`;
  if (mb >= 1) return `${mb.toFixed(1).replace(/\.0$/, '')} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
};

const whatOf = (program: Program) => (program.own ? program.what || t('beatbox.programs.yours')
  : runtimeText(`beatbox.programs.what.${program.id}`));

/** The recipes alone, without asking the machine: which missing program has one. */
export function useRecipes(hostId: string) {
  const [recipes, setRecipes] = useState<Program[]>([]);
  useEffect(() => {
    if (!hostId) return;
    void hostAct(hostId, 'programs.list', { quick: true })
      .then((said) => setRecipes((said.programs as Program[] | undefined) ?? []), () => setRecipes([]));
  }, [hostId]);
  return recipes;
}

/** Services on Servers that use this machine, by the command they need. */
function useNeeds(hostId: string) {
  const [needs, setNeeds] = useState<Map<string, string[]>>(new Map());
  useEffect(() => {
    if (!hostId) return;
    let live = true;
    void (async () => {
      const found = new Map<string, string[]>();
      const servers = (await call('sandbox.list')).filter((row) => row.type === 'server');
      for (const row of servers) {
        const view = await call('sandbox.refresh', { id: row.id });
        const uses = (view.config.uses ?? {}) as Record<string, unknown>;
        if (uses.host !== hostId) continue;
        const [listed, offered] = await Promise.all([hostAct(row.id, 'service.list'), hostAct(row.id, 'service.catalog')]);
        const catalog = (offered.definitions as { id: string; requires?: string[] }[] | undefined) ?? [];
        for (const service of (listed.services as { service: string; definition: string }[] | undefined) ?? []) {
          for (const command of catalog.find((each) => each.id === service.definition)?.requires ?? []) {
            found.set(command, [...(found.get(command) ?? []), service.service]);
          }
        }
      }
      if (live) setNeeds(found);
    })().catch(() => undefined);
    return () => { live = false; };
  }, [hostId]);
  return needs;
}

export function Programs() {
  const { list, machine, reload } = useChosenMachine();
  const id = machine?.id ?? '';
  const running = machine?.state.lifecycle === 'running';
  const can = machine?.capabilities.includes('programs.list') ?? false;
  const [listing, setListing] = useState<Listing | null>(null);
  const [told, setTold] = useState('');
  const [group, setGroup] = useState<string>('all');
  const [find, setFind] = useState('');
  const [adding, setAdding] = useState<{ program: Program | null; follow: boolean } | null>(null);
  const [removing, setRemoving] = useState<Program | null>(null);
  const [owning, setOwning] = useState(false);
  const [busy, setBusy] = useState(false);
  const needs = useNeeds(id);
  const asked = useRef(0);

  const load = useCallback(async () => {
    if (!id || !running) return;
    // A read takes seconds; an older one that lands late must not undo a newer one.
    const mine = ++asked.current;
    try {
      const said = await hostAct(id, 'programs.list') as unknown as Listing;
      if (mine !== asked.current) return;
      setListing(said);
      setTold('');
      if (said.adding) setAdding((now) => now ?? { program: null, follow: true });
    } catch (error) {
      if (mine === asked.current) setTold((error as Error).message);
    }
  }, [id, running]);

  // Reading the machine takes seconds, so the page shows what just changed
  // at once and the read that follows confirms it.
  const changed = (programs: (now: Program[]) => Program[]) => {
    setListing((now) => now && { ...now, programs: programs(now.programs) });
    void load();
  };
  const set = (programId: string | undefined, change: Partial<Program>) =>
    changed((now) => now.map((p) => (p.id === programId ? { ...p, ...change } : p)));
  useEffect(() => { setListing(null); void load(); }, [load]);

  async function turnOn() {
    setBusy(true);
    try { await call('sandbox.transition', { id, transition: 'start' }); } catch { /* the page shows the state */ }
    await reload();
    setBusy(false);
  }

  async function forget(program: Program) {
    try {
      await hostAct(id, 'programs.forget', { program: program.id });
      changed((now) => now.filter((p) => p.id !== program.id));
    } catch (error) { setTold((error as Error).message); }
  }

  const needers = (program: Program) => needs.get(program.command) ?? [];
  // What a service is waiting for comes first.
  const programs = [...listing?.programs ?? []].sort((a, b) => Number(needers(b).length > 0) - Number(needers(a).length > 0));
  const on = programs.filter((program) => program.on);
  const shown = programs.filter((program) => !program.on
    && (group === 'all' || program.group === group)
    && (!find.trim() || `${program.name} ${whatOf(program)}`.toLowerCase().includes(find.trim().toLowerCase())));
  const groups = [...GROUPS, ...(programs.some((program) => program.own) ? ['yours' as const] : [])];

  const head = (
    <div className="bb-pagehead">
      <div>
        <div className="bb-pagehead-title">
          <h1 className="bb-t-title" style={{ margin: 0 }}>{t('beatbox.screen.host.programs.title')}</h1>
          <Info text={machine ? t('beatbox.programs.about', { name: machine.name }) : t('beatbox.screen.host.programs.blurb')} />
          {listing && !listing.windows ? (
            <span className="bb-prog-line">
              {t('beatbox.programs.line', { system: listing.system, free: bytesText(listing.free_kb * 1024) })}
              {listing.reach ? null : <> · <span className="bb-amber">{t('beatbox.programs.offline')}</span></>}
            </span>
          ) : null}
        </div>
      </div>
    </div>
  );

  let body: ReactElement | null = null;
  if (list && !list.length) {
    body = (
      <div className="bb-pending" style={{ minHeight: 280 }}>
        <CubeWire size={80} tone="violet" opacity={0.42} />
        <p>{t('beatbox.host_pages.no_machines')}</p>
        <Button variant="primary" trailingIcon="arrow" onClick={() => navigate('host/machine')}>{t('beatbox.host_pages.to_machine')}</Button>
      </div>
    );
  } else if (machine && !can) {
    body = <Card><p className="bb-host-say">{t('beatbox.commands.no_agent')}</p></Card>;
  } else if (machine && !running) {
    body = (
      <div className="bb-pending" style={{ minHeight: 280 }}>
        <CubeWire size={80} tone="violet" opacity={0.42} />
        <p>{t('beatbox.programs.off', { name: machine.name })}</p>
        <Button variant="primary" icon="play" disabled={busy} onClick={() => void turnOn()}>{t('beatbox.srv.helpers.turn_on')}</Button>
      </div>
    );
  } else if (machine && listing?.windows) {
    body = (
      <div className="bb-pending" style={{ minHeight: 280 }}>
        <CubeWire size={80} tone="violet" opacity={0.42} />
        <p>{t('beatbox.programs.windows', { name: machine.name })}</p>
        <Button trailingIcon="arrow" onClick={() => navigate('host/commands')}>{t('beatbox.srv.prep.open_commands')}</Button>
      </div>
    );
  } else if (machine && !listing) {
    body = <p className="bb-host-say">{told || t('beatbox.programs.looking', { name: machine.name })}</p>;
  } else if (machine && listing) {
    body = (
      <div className="bb-prog-page">
        {told ? <p className="bb-host-say bb-rose">{told}</p> : null}
        {on.length ? (
          <Card title={<>{t('beatbox.programs.on_it')}<span className="bb-faint bb-prog-count">{on.length}</span></>}>
            <div className="bb-prog-have">
              {on.map((program) => (
                <div key={program.id} className="bb-prog-chip">
                  <b>{program.name}</b>
                  <span className="bb-faint bb-mono">
                    {[program.version, t(program.mine ? 'beatbox.programs.added' : 'beatbox.programs.came')].filter(Boolean).join(' · ')}
                  </span>
                  {program.mine ? (
                    <button type="button" className="bb-prog-remove" onClick={() => setRemoving(program)}>{t('beatbox.programs.remove')}</button>
                  ) : null}
                </div>
              ))}
            </div>
          </Card>
        ) : null}
        <Card title={<>{t('beatbox.programs.can_add')}<span className="bb-faint bb-prog-count">{programs.length - on.length}</span></>}>
          <div className="bb-prog-tools">
            <div className="bb-why-seg" role="group" aria-label={t('beatbox.programs.can_add')}>
              {['all', ...groups].map((each) => (
                <button key={each} type="button" className={group === each ? 'is-on' : ''} aria-pressed={group === each}
                        onClick={() => setGroup(each)}>{runtimeText(`beatbox.programs.filter.${each}`)}</button>
              ))}
            </div>
            <label className="bb-prog-find">
              <Icon name="search" size="sm" />
              <input type="text" value={find} placeholder={t('beatbox.programs.find')} aria-label={t('beatbox.programs.find')}
                     onChange={(event) => setFind(event.target.value)} />
            </label>
          </div>
          {shown.length ? (
            <div className="bb-prog-list">
              {shown.map((program) => {
                const who = needers(program);
                return (
                  <div key={program.id} className={`bb-prog-row${who.length && !program.cannot ? ' is-needed' : ''}`}>
                    <span className="bb-prog-name">
                      <span><b>{program.name}</b> {program.version ? <span className="bb-faint bb-mono">{program.version}</span> : null}</span>
                      <span className="bb-faint">
                        {whatOf(program)}
                        {who.length ? <> · <span className="bb-amber">{t('beatbox.programs.needs', { services: who.join(', ') })}</span></> : null}
                      </span>
                    </span>
                    {program.cannot ? <span className="bb-faint">{runtimeText(`beatbox.programs.cannot.${program.cannot}`)}</span>
                      : <span className="bb-faint">{program.bytes ? bytesText(program.bytes) : ''}</span>}
                    {program.own ? <Button size="sm" onClick={() => void forget(program)}>{t('beatbox.programs.forget')}</Button> : null}
                    <Button size="sm" variant={who.length ? 'primary' : undefined} disabled={Boolean(program.cannot) || listing.busy}
                            onClick={() => setAdding({ program, follow: false })}>{t('beatbox.programs.add')}</Button>
                  </div>
                );
              })}
            </div>
          ) : <p className="bb-host-say">{t('beatbox.programs.nothing')}</p>}
          <div className="bb-prog-foot">
            <button type="button" className="bb-why-link" onClick={() => setOwning(true)}>{t('beatbox.programs.add_own')}</button>
          </div>
        </Card>
      </div>
    );
  }

  return (
    <>
      {head}
      {body}
      {adding && machine ? (
        <AddProgram hostId={id} machine={machine.name} program={adding.program} follow={adding.follow}
                    onAdded={(programId) => set(programId, { on: true, mine: true })}
                    onClose={() => setAdding(null)} />
      ) : null}
      {removing && machine ? (
        <RemoveProgram hostId={id} program={removing} needers={needers(removing)}
                       onRemoved={() => set(removing.id, { on: false, mine: false })}
                       onClose={() => setRemoving(null)} />
      ) : null}
      {owning && machine ? <OwnProgram hostId={id}
                                            onSaved={(row) => changed((now) => [...now.filter((p) => p.id !== row.id), row])}
                                            onClose={() => setOwning(false)} /> : null}
    </>
  );
}

/**
 * Adding one program: what it takes, then each step as it happens. With
 * `follow`, an add already under way, picked up where it is.
 */
export function AddProgram({ hostId, machine, program, follow = false, then, onAdded, onClose }: {
  hostId: string; machine: string; program: Program | null; follow?: boolean;
  /** What becomes possible after, and its button. */
  then?: { words: string; action: string; run: () => Promise<void> };
  /** Once it is on, while the dialog still shows how it went. */
  onAdded?: (program: string | undefined) => void;
  onClose: () => void;
}) {
  const [listing, setListing] = useState<Listing | null>(null);
  const [progress, setProgress] = useState<Adding | null>(null);
  const [told, setTold] = useState('');
  const [busy, setBusy] = useState(false);
  const reading = useRef(false);

  const row = listing?.programs.find((each) => each.id === program?.id) ?? program;
  const working = progress !== null && !['done', 'failed', 'none'].includes(progress.phase);
  // From a ref: the caller passes a new function each render, and the effect must run once.
  const added = useRef(onAdded);
  added.current = onAdded;
  const done = progress?.phase === 'done';
  const rowId = row?.id;
  useEffect(() => { if (done) added.current?.(rowId); }, [done, rowId]);

  useEffect(() => {
    if (follow) return;
    void hostAct(hostId, 'programs.list').then((said) => setListing(said as unknown as Listing), (error: Error) => setTold(error.message));
  }, [hostId, follow]);

  const poll = useCallback(async () => {
    if (reading.current) return;
    reading.current = true;
    try {
      setProgress(await hostAct(hostId, 'programs.adding') as unknown as Adding);
    } catch (error) {
      setTold((error as Error).message);
    } finally {
      reading.current = false;
    }
  }, [hostId]);
  useEffect(() => { if (follow) void poll(); }, [follow, poll]);
  useEffect(() => {
    if (!working) return undefined;
    const timer = window.setInterval(() => void poll(), 1000);
    return () => window.clearInterval(timer);
  }, [working, poll]);

  async function add(refresh = false) {
    if (!row) return;
    setBusy(true);
    setTold('');
    try {
      setProgress(await hostAct(hostId, 'programs.add', { program: row.id, refresh }) as unknown as Adding);
    } catch (error) {
      setTold((error as Error).message);
    }
    setBusy(false);
  }

  async function stop() {
    setBusy(true);
    try { await hostAct(hostId, 'programs.stop'); } catch { /* it ends either way */ }
    setBusy(false);
    onClose();
  }

  async function finish() {
    if (!then) return;
    setBusy(true);
    try { await then.run(); } catch { /* the page behind shows how it went */ }
    setBusy(false);
    onClose();
  }

  const name = progress?.name || row?.name || '';
  const system = listing?.system.split(' ').slice(0, 1).join(' ') || 'Ubuntu';
  const close = <Button key="close" onClick={onClose}>{t('beatbox.programs.dialog.close')}</Button>;
  let title = t('beatbox.programs.dialog.title', { name });
  let body: ReactElement;

  if (progress && progress.phase !== 'none') {
    const p = progress;
    const fetchedAll = p.fetched >= p.files && p.files > 0;
    const order = ['download', 'copy', 'install', 'try', 'done'];
    const at = order.indexOf(p.phase);
    // Each step: its words, its count, and whether it is done or now.
    const steps: { words: string; note?: string; done: boolean; now: boolean }[] = [];
    // While the lists come in, the program's own steps wait.
    const listing = p.how === 'lists' || p.how === 'indexes';
    if (p.refreshed) {
      steps.push({ words: t('beatbox.programs.step.list', { machine }),
        note: bytesText(p.list_bytes + (listing && p.phase === 'download' ? p.got : 0)), done: !listing, now: listing });
    }
    if (p.how === 'apt') {
      steps.push({ words: t('beatbox.programs.step.from_system', { system }), done: at > 2, now: at <= 2 });
    } else {
      if (p.how === 'debs' || listing) {
        steps.push({ words: t('beatbox.programs.step.asked', { machine }), note: listing ? undefined : String(p.files), done: !listing, now: false });
      }
      steps.push({ words: t('beatbox.programs.step.downloaded'), note: listing ? undefined : `${bytesText(p.got)} / ${bytesText(p.bytes)}`,
        done: !listing && fetchedAll, now: !listing && p.phase === 'download' });
      steps.push({ words: t(p.files > 1 || listing ? 'beatbox.programs.step.real_many' : 'beatbox.programs.step.real_one'),
        note: !listing && fetchedAll ? t('beatbox.programs.dialog.fingerprint') : undefined, done: !listing && fetchedAll, now: false });
      steps.push({ words: t(p.how === 'file' ? 'beatbox.programs.step.copying' : 'beatbox.programs.step.installing', { machine }),
        note: !listing && p.files > 1 && p.phase === 'copy' ? t('beatbox.programs.of', { done: p.copied, all: p.files }) : undefined,
        done: !listing && at > 2, now: !listing && (at === 1 || at === 2) });
    }
    steps.push({ words: t('beatbox.programs.step.runs'), note: p.said || undefined, done: p.phase === 'done', now: p.phase === 'try' });
    const list = (
      <ol className="bb-prep-steps">
        {steps.map((step) => (
          <li key={step.words} className={step.done ? 'is-done' : step.now && p.phase !== 'failed' ? 'is-now' : ''}>
            <span className="bb-prep-mark" aria-hidden="true">{step.done ? <Icon name="check" size="sm" /> : <i />}</span>
            <span className="bb-prog-step">{step.words}</span>
            {step.note ? <span className="bb-faint bb-prog-note">{step.note}</span> : null}
          </li>
        ))}
      </ol>
    );
    if (p.phase === 'done') {
      title = t('beatbox.programs.dialog.done', { name, machine });
      body = (
        <>
          {list}
          <div className="bb-prep-acts">
            {close}
            {then ? <Button variant="primary" icon="play" disabled={busy} onClick={() => void finish()}>{then.action}</Button> : null}
          </div>
        </>
      );
    } else if (p.phase === 'failed') {
      // Files gone from the system's mirrors: its lists are older than they are.
      const stale = p.failed?.code === 'host.files_gone' && p.how === 'debs';
      title = t('beatbox.programs.dialog.failed', { name });
      body = (
        <>
          <div className="bb-prep-box is-amber"><b>{p.failed ? say(p.failed) : t('beatbox.host.program_install_failed', { program: name })}</b></div>
          {p.lines.length || p.said ? (
            <details className="bb-srv-more">
              <summary>{t('beatbox.programs.dialog.log')}</summary>
              <pre className="bb-prep-log bb-mono">{(p.lines.length ? p.lines : [p.said]).join('\n')}</pre>
            </details>
          ) : null}
          <div className="bb-prep-acts">
            {close}
            {row && stale
              ? <Button variant="primary" icon="refresh" disabled={busy} onClick={() => void add(true)}>{t('beatbox.programs.dialog.refresh')}</Button>
              : row ? <Button variant="primary" icon="refresh" disabled={busy} onClick={() => void add()}>{t('beatbox.programs.dialog.again')}</Button> : null}
          </div>
        </>
      );
    } else {
      title = t('beatbox.programs.dialog.adding', { name });
      body = (
        <>
          {list}
          <div className="bb-prep-acts">
            {p.phase === 'download' || p.phase === 'copy'
              ? <Button disabled={busy} onClick={() => void stop()}>{t('beatbox.programs.dialog.stop')}</Button> : null}
          </div>
        </>
      );
    }
  } else if (!row || !listing) {
    body = <p className="bb-prep-quiet">{told || t('beatbox.programs.looking', { name: machine })}</p>;
  } else {
    const itself = row.how === 'apt' && listing.reach;
    const size = row.files > 1 ? t('beatbox.programs.dialog.size_files', { size: bytesText(row.bytes), count: row.files }) : bytesText(row.bytes);
    const fact = (label: string, value: ReactElement | string) => (
      <div className="bb-prep-fact"><span className="bb-faint">{label}</span><span>{value}</span></div>
    );
    body = (
      <>
        <div>
          {fact(t('beatbox.programs.dialog.program'), <><b>{row.name} {row.version}</b>{row.bytes ? <span className="bb-faint"> · {size}</span> : null}</>)}
          {fact(t('beatbox.programs.dialog.on'), <>{machine} <span className="bb-faint">· {t('beatbox.programs.line', { system: listing.system, free: bytesText(listing.free_kb * 1024) })}</span></>)}
          {fact(t('beatbox.programs.dialog.from'), row.how === 'file' ? t('beatbox.programs.dialog.from_file', { name: row.name })
            : t('beatbox.programs.dialog.from_system', { system }))}
          {fact(t('beatbox.programs.dialog.through'), itself ? t('beatbox.programs.dialog.itself', { machine })
            : <>{t('beatbox.programs.dialog.this_computer')}{listing.reach ? null : <span className="bb-faint"> · {t('beatbox.programs.dialog.no_internet', { machine })}</span>}</>)}
          {then || row.kept_off ? fact(t('beatbox.programs.dialog.then'), then?.words ?? t('beatbox.programs.dialog.stays_off')) : null}
        </div>
        <details className="bb-srv-more">
          <summary>{t('beatbox.programs.dialog.more')}</summary>
          <pre className="bb-prep-log bb-mono">{(row.how === 'file'
            ? [t('beatbox.programs.dialog.more_from', { source: row.source }), t('beatbox.programs.dialog.more_checked'), t('beatbox.programs.dialog.more_goes', { path: row.path })]
            : [t('beatbox.programs.dialog.more_package', { package: row.package ?? '' }), t('beatbox.programs.dialog.more_checked')]).join('\n')}</pre>
        </details>
        <div className="bb-prep-acts">
          <Button onClick={onClose}>{t('beatbox.files.cancel')}</Button>
          {row.cannot === 'lists' && !listing.reach ? (
            <Button variant="primary" icon="refresh" disabled={busy} onClick={() => void add(true)}>{t('beatbox.programs.dialog.refresh')}</Button>
          ) : (
            <Button variant="primary" icon="box" disabled={busy || Boolean(row.cannot)} onClick={() => void add()}>
              {t('beatbox.programs.add_named', { name: row.name })}
            </Button>
          )}
        </div>
      </>
    );
  }

  return (
    <Modal title={title} icon="puzzle" width={560} onClose={working ? () => undefined : onClose} dismissable={!working}
           info={t('beatbox.programs.about', { name: machine })}>
      <div className="bb-prep">
        {body}
        {told && (progress || listing) ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
      </div>
    </Modal>
  );
}

function RemoveProgram({ hostId, program, needers, onRemoved, onClose }: {
  hostId: string; program: Program; needers: string[]; onRemoved: () => void; onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  async function remove() {
    setBusy(true);
    try {
      await hostAct(hostId, 'programs.remove', { program: program.id });
      onRemoved();
      onClose();
    } catch (error) {
      setTold((error as Error).message);
      setBusy(false);
    }
  }
  return (
    <Modal title={t('beatbox.programs.remove_title', { name: program.name })} icon="trash" width={460} onClose={onClose}>
      <div className="bb-prep">
        <div className="bb-prep-box is-grey">
          {needers.length ? <b>{t('beatbox.programs.remove_needed', { services: needers.join(', ') })}</b> : null}
          <span>{t('beatbox.programs.added')}</span>
        </div>
        {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
        <div className="bb-prep-acts">
          <Button onClick={onClose}>{t('beatbox.files.cancel')}</Button>
          <Button variant="danger" icon="trash" disabled={busy} onClick={() => void remove()}>
            {t(busy ? 'beatbox.programs.removing' : 'beatbox.programs.remove')}
          </Button>
        </div>
      </div>
    </Modal>
  );
}

const slug = (name: string) => name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 40);

function OwnProgram({ hostId, onSaved, onClose }: { hostId: string; onSaved: (row: Program) => void; onClose: () => void }) {
  const [from, setFrom] = useState<'apt' | 'file'>('apt');
  const [form, setForm] = useState({ name: '', package: '', url: '', sha: '', urlArm: '', shaArm: '', command: '', try: '' });
  const [told, setTold] = useState('');
  const [busy, setBusy] = useState(false);
  const set = (key: keyof typeof form) => (event: { target: { value: string } }) => setForm({ ...form, [key]: event.target.value });
  const command = form.command.trim() || form.package.trim();
  const words = (form.try.trim() || `${command} --version`).split(/\s+/);
  const archive = (url: string) => /\.(tar\.gz|tgz)$/i.test(url.trim());

  const recipe = useMemo(() => {
    const base = { id: slug(form.name), name: form.name.trim(), command, try: words };
    if (from === 'apt') return { ...base, apt: form.package.trim() };
    const build = (url: string, sha: string) => ({ url: url.trim(), sha256: sha.trim().toLowerCase() });
    return {
      ...base,
      file: {
        version: '',
        ...(archive(form.url) ? { inside: command } : {}),
        x86_64: build(form.url, form.sha),
        ...(form.urlArm.trim() ? { aarch64: build(form.urlArm, form.shaArm) } : {}),
      },
    };
  }, [form, from, command, words]);

  const ready = recipe.id && recipe.name && command && (from === 'apt' ? form.package.trim() : form.url.trim() && form.sha.trim());

  async function save() {
    setBusy(true);
    setTold('');
    try {
      await hostAct(hostId, 'programs.save', { recipe });
      // Its size and version come with the next read of the machine.
      onSaved({
        id: recipe.id, name: recipe.name, group: 'yours', what: '', own: true, command,
        how: from, package: from === 'apt' ? form.package.trim() : null, source: '', kept_off: false, path: '',
        on: false, mine: false, version: '', files: 0, bytes: 0, cannot: null,
      });
      onClose();
    } catch (error) {
      setTold((error as Error).message);
      setBusy(false);
    }
  }

  const field = (key: keyof typeof form, label: string, mono = false, placeholder = '') => (
    <label className="bb-prog-field">
      <span>{label}</span>
      <input className={`bb-input${mono ? ' bb-mono' : ''}`} value={form[key]} placeholder={placeholder} onChange={set(key)} />
    </label>
  );
  return (
    <Modal title={t('beatbox.programs.own.title')} icon="puzzle" width={560} onClose={onClose} info={t('beatbox.programs.own.info')}>
      <div className="bb-prep">
        {field('name', t('beatbox.programs.own.name'))}
        <div className="bb-prog-field">
          <span>{t('beatbox.programs.own.from')}</span>
          <div className="bb-why-seg" role="group" aria-label={t('beatbox.programs.own.from')}>
            {(['apt', 'file'] as const).map((each) => (
              <button key={each} type="button" className={from === each ? 'is-on' : ''} aria-pressed={from === each} onClick={() => setFrom(each)}>
                {t(each === 'apt' ? 'beatbox.programs.own.from_system' : 'beatbox.programs.own.from_file')}
              </button>
            ))}
          </div>
        </div>
        {from === 'apt' ? field('package', t('beatbox.programs.own.package'), true) : (
          <>
            {field('url', t('beatbox.programs.own.url'), true, 'https://')}
            {field('sha', t('beatbox.programs.own.sha'), true)}
            {field('urlArm', `${t('beatbox.programs.own.url_arm')} · ${t('beatbox.programs.own.if_any')}`, true, 'https://')}
            {form.urlArm.trim() ? field('shaArm', t('beatbox.programs.own.sha'), true) : null}
          </>
        )}
        <div className="bb-prog-two">
          {field('command', t('beatbox.programs.own.command'), true, form.package.trim())}
          {field('try', t('beatbox.programs.own.try'), true, command ? `${command} --version` : '')}
        </div>
        {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
        <div className="bb-prep-acts">
          <Button onClick={onClose}>{t('beatbox.files.cancel')}</Button>
          <Button variant="primary" disabled={busy || !ready} onClick={() => void save()}>{t('beatbox.programs.own.save')}</Button>
        </div>
      </div>
    </Modal>
  );
}
