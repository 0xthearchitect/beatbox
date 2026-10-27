import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import { Button, Card, Icon, Info, Switch } from '../components';
import type { IconName } from '../components/Icon';
import { call } from '../bridge/client';
import type { AboutView, Availability, Preferences, SetupStepView, StorageView, StoredMachine, SystemProbe } from '../bridge/types';
import { fault, activeLanguage, availableLanguages, loadLocale, say, runtimeText, t } from '../i18n';
import type { Key } from '../i18n';
import { href, navigate, useRouteParams } from '../router';
import { showBuilderTools } from '../builder';
import { remember, type Kind } from '../chosen';
import { SANDBOXES } from '../routes';
import { useSetup } from './Setup';

/**
 * Settings. Each section shows its current value beside its name, so the
 * list alone answers most questions. `settings?section=` picks one.
 *
 * - General: window prefs (shell/src/preferences.rs) and language.
 * - This computer: probe + setup steps; engine detail behind "Show details".
 * - New machines: defaults for the new machine form.
 * - Storage: what we keep on disk (`storage.usage`).
 * - About: version and what to attach to a bug report.
 */

const SECTIONS = ['general', 'computer', 'machines', 'storage', 'about'] as const;
type Section = typeof SECTIONS[number];

const LANGUAGE_NAMES: Record<string, string> = { en: 'English', pt_PT: 'Português' };
const CLOSE_CHOICES = ['ask', 'stop', 'leave'] as const;
/** QEMU keeps this much beside a machine's own memory. */
const QEMU_MB = 512;

const gb = (mb: number) => `${(Math.max(0, mb) / 1024).toFixed(1)} GB`;
/** KB under 1 MB, MB under 1 GB, then GB. */
const bytes = (count: number) => count < 1024 ** 2 ? `${Math.round(count / 1024)} KB`
  : count < 1024 ** 3 ? `${Math.round(count / 1024 ** 2)} MB` : `${(count / 1024 ** 3).toFixed(1)} GB`;
/** New machine defaults; "Back to the least" restores these. */
const LEAST = { newMemoryMb: 2048, newCpus: 2, newReach: 'none', newAgent: true };
const REACH = ['none', 'isolated', 'nat'] as const;

type Tone = 'mint' | 'amber' | 'rose';

interface Computer {
  probe: SystemProbe | null;
  steps: SetupStepView[];
  machinesMb: number;
}

/** Full speed, slow, or can't run: the headline of This computer. */
function verdict(probe: SystemProbe): { tone: Tone; key: Key } {
  if (!probe.qemu.ready) return { tone: 'rose', key: 'beatbox.settings.state.none' };
  if (!probe.kvm.ready) return { tone: 'amber', key: 'beatbox.settings.state.slow' };
  return { tone: 'mint', key: 'beatbox.settings.state.full' };
}

export function Settings() {
  const asked = useRouteParams().get('section');
  const section: Section = SECTIONS.find((each) => each === asked) ?? 'general';
  const setup = useSetup();
  const { load } = setup;
  const [probe, setProbe] = useState<SystemProbe | null>(null);
  const [machinesMb, setMachinesMb] = useState(0);
  const [prefs, setPrefs] = useState<Preferences | null>(null);
  const [storage, setStorage] = useState<StorageView | null>(null);
  const [about, setAbout] = useState<AboutView | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  const read = useCallback(async () => {
    void load();
    call('system.probe').then(setProbe).catch(() => undefined);
    // Memory given to running machines, the first part of the bar.
    try {
      const running = (await call('sandbox.list'))
        .filter((row) => row.type === 'host' && row.lifecycle === 'running');
      const views = await Promise.all(running.map((row) => call('sandbox.view', { id: row.id })));
      setMachinesMb(views.reduce((sum, view) =>
        sum + Number((view.config.resources as { memory?: number } | null)?.memory ?? 0), 0));
    } catch {
      setMachinesMb(0);
    }
  }, [load]);

  useEffect(() => { void read(); }, [read]);
  useEffect(() => {
    call('system.preferences').then(setPrefs).catch((error) => setFailure(fault(error)));
    call('system.about').then(setAbout).catch(() => undefined);
  }, []);
  // Re-read on every open: machines keep writing while it's closed.
  useEffect(() => {
    call('storage.usage').then(setStorage).catch(() => undefined);
  }, [section]);

  async function change(next: Preferences) {
    setFailure(null);
    try {
      const saved = await call('system.set_preferences', next);
      setPrefs(saved);
      showBuilderTools(saved.builderTools);
    } catch (error) {
      setFailure(fault(error));
    }
  }

  const computer: Computer = { probe, steps: setup.view?.steps ?? [], machinesMb };
  const state = probe ? verdict(probe) : null;
  const stored = storage ? storageTotal(storage) : null;

  const entry = (id: Section, icon: IconName, name: Key, value: ReactNode) => (
    <a href={id === 'general' ? href('settings') : href('settings', { section: id })}
       aria-current={section === id ? 'page' : undefined}>
      <Icon name={icon} size="sm" />
      {t(name)}
      {value === null ? null : <em>{value}</em>}
    </a>
  );

  return (
    <>
      <div className="bb-pagehead">
        <div>
          <div className="bb-pagehead-title">
            <h2 className="bb-t-title" style={{ margin: 0 }}>{t('beatbox.screen.settings.title')}</h2>
            <Info text={t('beatbox.screen.settings.blurb')} />
          </div>
        </div>
      </div>

      <div className="bb-prefs">
        <nav className="bb-prefs-nav" aria-label={t('beatbox.screen.settings.title')}>
          {entry('general', 'settings', 'beatbox.settings.general', LANGUAGE_NAMES[activeLanguage()] ?? activeLanguage())}
          {entry('computer', 'monitor', 'beatbox.settings.computer', state ? (
            <><i className={`bb-prefs-dot bb-prefs-dot--${state.tone}`} />
              {t(state.tone === 'mint' ? 'beatbox.settings.ready' : 'beatbox.settings.attention')}</>
          ) : null)}
          {entry('machines', 'cpu', 'beatbox.settings.machines', prefs
            ? `${gb(prefs.newMemoryMb).replace('.0 ', ' ')} · ${prefs.newCpus} CPU` : null)}
          {entry('storage', 'database', 'beatbox.settings.storage', stored === null ? null : bytes(stored))}
          {entry('about', 'logs', 'beatbox.settings.about', about ? about.version : null)}
        </nav>

        <div className="bb-prefs-col">
          {section === 'general' ? <General prefs={prefs} change={change} />
            : section === 'computer' ? <ThisComputer computer={computer} setup={setup} recheck={() => void read()} />
            : section === 'machines' ? <NewMachines prefs={prefs} change={change} computer={computer} />
            : section === 'storage' ? <Storage storage={storage} update={setStorage} />
            : <About about={about} probe={probe} />}
          {failure ? <p className="bb-rose" style={{ fontSize: 12.5, margin: 0 }}>{failure}</p> : null}
        </div>
      </div>
    </>
  );
}

/* ── General ─────────────────────────────────────────────────────────── */

function General({ prefs, change }: { prefs: Preferences | null; change: (next: Preferences) => Promise<void> }) {
  const [language, setLanguage] = useState(activeLanguage());
  const [failure, setFailure] = useState<string | null>(null);

  async function choose(next: string) {
    if (next === language) return;
    try {
      await call('system.set_language', { language: next });
      await loadLocale();
      setLanguage(next);
      // Screens cache their text at render; reload is the only clean switch.
      // The hash keeps us on this page.
      window.location.reload();
    } catch (error) {
      setFailure(fault(error));
    }
  }

  return (
    <>
      <Card title={<><Icon name="globe" size="sm" />{t('beatbox.settings.language')}</>}
            info={t('beatbox.settings.language_note')}>
        <span className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.settings.language')}>
          {availableLanguages().map((code) => (
            <button key={code} type="button" role="radio" aria-checked={code === language}
                    className={code === language ? 'on' : ''} onClick={() => void choose(code)}>
              {LANGUAGE_NAMES[code] ?? code}
            </button>
          ))}
        </span>
      </Card>

      <Card title={<><Icon name="close" size="sm" />{t('beatbox.settings.close')}</>}
            info={t('beatbox.settings.close_what')}>
        <div className="bb-prefs-opts" role="radiogroup" aria-label={t('beatbox.settings.close')}>
          {CLOSE_CHOICES.map((choice) => (
            <button key={choice} type="button" role="radio" aria-checked={prefs?.onClose === choice}
                    disabled={!prefs} onClick={() => prefs && void change({ ...prefs, onClose: choice })}>
              <span className="bb-prefs-radio" />
              <b>{runtimeText(`beatbox.settings.close.${choice}`)}</b>
            </button>
          ))}
        </div>
      </Card>

      <Card title={<><Icon name="rail" size="sm" />{t('beatbox.settings.sidebar')}</>}>
        <div className="bb-prefs-toggle">
          <Switch checked={prefs?.builderTools ?? false} label={t('beatbox.settings.builder')}
                  onChange={(next) => prefs && void change({ ...prefs, builderTools: next })} />
          <div>
            <b>{t('beatbox.settings.builder')} <Info text={t('beatbox.settings.builder_what')} /></b>
          </div>
        </div>
      </Card>

      {failure ? <p className="bb-rose" style={{ fontSize: 12.5, margin: 0 }}>{failure}</p> : null}
    </>
  );
}

/* ── This computer ───────────────────────────────────────────────────── */

interface Row {
  icon: IconName;
  name: Key;
  what: Key;
  ready: boolean;
  optional: boolean;
  /** Engine's raw sentence, for "Show details". */
  detail: string;
  /** The setup step that fixes it, if any. */
  step?: SetupStepView;
}

function ThisComputer({ computer, setup, recheck }: {
  computer: Computer;
  setup: ReturnType<typeof useSetup>;
  recheck: () => void;
}) {
  const [details, setDetails] = useState(false);
  const { probe, steps, machinesMb } = computer;
  if (!probe) {
    return <Card><p className="bb-faint" style={{ fontSize: 12.5, margin: 0 }}>{t('beatbox.run_stage.reading')}</p></Card>;
  }

  const step = (key: string) => steps.find((each) => each.key === key);
  const from = (value: Availability) => say(value.reason);
  const tools = step('guest_tools');
  const rows: Row[] = [
    { icon: 'box', name: 'beatbox.settings.can.qemu', what: 'beatbox.settings.can.qemu_what',
      ready: probe.qemu.ready, optional: false, detail: from(probe.qemu), step: step('qemu') },
    { icon: 'bolt', name: 'beatbox.settings.can.kvm', what: 'beatbox.settings.can.kvm_what',
      ready: probe.kvm.ready, optional: false, detail: from(probe.kvm), step: step('acceleration') },
    { icon: 'terminal', name: 'beatbox.settings.can.terminal', what: 'beatbox.settings.can.terminal_what',
      ready: probe.terminal.ready, optional: false, detail: from(probe.terminal) },
    { icon: 'eye', name: 'beatbox.settings.can.frida', what: 'beatbox.settings.can.frida_what',
      ready: probe.frida.ready, optional: true, detail: from(probe.frida) },
  ];
  if (tools) {
    rows.push({ icon: 'monitor', name: 'beatbox.settings.can.tools', what: 'beatbox.settings.can.tools_what',
      ready: tools.ready, optional: true, detail: say(tools.reason), step: tools });
  }
  const state = verdict(probe);
  const lede: Key = !rows.every((row) => row.optional || row.ready) ? 'beatbox.settings.something_missing'
    : rows.every((row) => row.ready) ? 'beatbox.settings.all_here' : 'beatbox.settings.optional_missing';

  // Bar = our machines + other programs + free. "Other" is the rest, floored at 0.
  const total = probe.memoryTotalMb;
  const free = probe.memoryFreeMb;
  const others = Math.max(0, total - free - machinesMb);
  const share = (mb: number) => `${total ? (mb / total) * 100 : 0}%`;

  return (
    <>
      <Card>
        <div className="bb-prefs-sum">
          <div>
            <p className="bb-prefs-verdict"><i className={`bb-prefs-dot bb-prefs-dot--${state.tone}`} />{t(state.key)}</p>
            <p className="bb-prefs-what" style={{ margin: '6px 0 0' }}>{t(lede)}</p>
            <p className="bb-mono bb-faint" style={{ fontSize: 12, margin: '10px 0 0' }}>
              {[probe.platform, probe.user].filter(Boolean).join(' · ')}
            </p>
          </div>
          <Button size="sm" icon="refresh" disabled={setup.working !== null} onClick={recheck}>
            {t('beatbox.setup.check_again')}
          </Button>
        </div>
      </Card>

      {total > 0 ? (
        <Card title={<><Icon name="cpu" size="sm" />{t('beatbox.settings.memory')}</>}>
          <p className="bb-prefs-what">{t('beatbox.settings.memory_can', { amount: gb(free - QEMU_MB) })}</p>
          <div className="bb-prefs-bar" role="img" aria-label={[
            `${t('beatbox.settings.memory.machines')} ${gb(machinesMb)}`,
            `${t('beatbox.settings.memory.others')} ${gb(others)}`,
            `${t('beatbox.settings.memory.free')} ${gb(free)}`,
            t('beatbox.settings.memory.of', { total: gb(total) }),
          ].join(', ')}>
            <span style={{ width: share(machinesMb) }} className="bb-prefs-bar--machines" />
            <span style={{ width: share(others) }} className="bb-prefs-bar--others" />
          </div>
          <div className="bb-prefs-legend">
            <span><i className="bb-prefs-bar--machines" />{t('beatbox.settings.memory.machines')} <b className="bb-mono">{gb(machinesMb)}</b></span>
            <span><i className="bb-prefs-bar--others" />{t('beatbox.settings.memory.others')} <b className="bb-mono">{gb(others)}</b></span>
            <span><i className="bb-prefs-bar--free" />{t('beatbox.settings.memory.free')} <b className="bb-mono">{gb(free)}</b></span>
            <span className="bb-mono" style={{ marginLeft: 'auto' }}>{t('beatbox.settings.memory.of', { total: gb(total) })}</span>
          </div>
        </Card>
      ) : null}

      <Card title={<><Icon name="box" size="sm" />{t('beatbox.settings.can')}</>}
            note={(
              <button type="button" className="bb-prefs-link" aria-expanded={details}
                      onClick={() => setDetails((was) => !was)}>
                {t(details ? 'beatbox.settings.details_hide' : 'beatbox.settings.details_show')}
              </button>
            )}>
        {rows.map((row) => {
          const tone: Tone = row.ready ? 'mint' : row.optional ? 'amber' : 'rose';
          const fixing = row.step?.key;
          const note = fixing ? setup.notes[fixing] : undefined;
          return (
            <div key={row.name} className="bb-prefs-row">
              <span className={`bb-rowmark${row.ready ? '' : ` bb-${tone}`}`}><Icon name={row.icon} size="sm" /></span>
              <div>
                <b>{t(row.name)} <Info text={t(row.what)} /></b>
                {details ? <code>{row.detail}</code> : null}
                {fixing && setup.working === fixing ? <span className="bb-amber">{t('beatbox.setup.working')}</span> : null}
                {note?.text ? <span className={note.ok ? 'bb-mint' : 'bb-rose'}>{note.text}</span> : null}
              </div>
              <span className="bb-prefs-state">
                <i className={`bb-prefs-dot bb-prefs-dot--${tone}`} />
                {t(row.ready ? 'beatbox.settings.ready' : row.optional ? 'beatbox.settings.not_installed' : 'beatbox.settings.missing')}
              </span>
              {!row.ready && fixing && row.step?.fixable ? (
                <Button size="sm" disabled={setup.working !== null} onClick={() => void setup.fix(fixing)}>
                  {runtimeText(`beatbox.setup.fix.${fixing}`)}
                </Button>
              ) : null}
            </div>
          );
        })}
        <div className="bb-prefs-foot">
          <Button size="sm" trailingIcon="arrow" onClick={() => navigate('setup')}>
            {t('beatbox.settings.open_setup')}
          </Button>
        </div>
        {setup.failure ? <p className="bb-rose" style={{ fontSize: 12.5, margin: '8px 0 0' }}>{setup.failure}</p> : null}
      </Card>
    </>
  );
}

/* ── New machines ────────────────────────────────────────────────────── */

function Stepper({ label, value, less, more, onLess, onMore }: {
  label: string;
  value: string;
  less: boolean;
  more: boolean;
  onLess: () => void;
  onMore: () => void;
}) {
  return (
    <span className="bb-prefs-stepper" role="group" aria-label={label}>
      <button type="button" disabled={!less} aria-label={`${label} −`} onClick={onLess}><Icon name="minus" size="sm" /></button>
      <b className="bb-mono">{value}</b>
      <button type="button" disabled={!more} aria-label={`${label} +`} onClick={onMore}><Icon name="plus" size="sm" /></button>
    </span>
  );
}

function NewMachines({ prefs, change, computer }: {
  prefs: Preferences | null;
  change: (next: Preferences) => Promise<void>;
  computer: Computer;
}) {
  const { probe, machinesMb } = computer;
  if (!prefs) {
    return <Card><p className="bb-faint" style={{ fontSize: 12.5, margin: 0 }}>{t('beatbox.run_stage.reading')}</p></Card>;
  }
  const cpus = navigator.hardwareConcurrency || 1;
  const total = probe?.memoryTotalMb ?? 0;
  const free = probe?.memoryFreeMb ?? 0;
  const give = free - QEMU_MB;
  const memory = prefs.newMemoryMb;
  const fits = !probe || memory <= give;
  const others = Math.max(0, total - free - machinesMb);
  const share = (mb: number) => `${total ? (Math.min(mb, total) / total) * 100 : 0}%`;
  const set = (next: Partial<Preferences>) => void change({ ...prefs, ...next });
  const least = Object.entries(LEAST).every(([key, value]) => prefs[key as keyof Preferences] === value);

  return (
    <>
      <Card title={<><Icon name="cpu" size="sm" />{t('beatbox.settings.machines.title')}</>}
            note={least ? undefined : (
              <button type="button" className="bb-prefs-link" onClick={() => set(LEAST)}>
                {t('beatbox.settings.machines.least')}
              </button>
            )}
            info={t('beatbox.settings.machines.what')}>
        <div className="bb-prefs-row">
          <span className="bb-rowmark"><Icon name="database" size="sm" /></span>
          <div>
            <b>{t('beatbox.settings.memory')}</b>
            <span className={fits ? undefined : 'bb-amber'}>
              {fits ? t('beatbox.settings.memory_can', { amount: gb(give) }) : t('beatbox.settings.machines.too_much')}
            </span>
          </div>
          <Stepper label={t('beatbox.settings.memory')} value={gb(memory)}
                   less={memory > 512} more={!total || memory + 512 <= total}
                   onLess={() => set({ newMemoryMb: Math.max(512, memory - 512) })}
                   onMore={() => set({ newMemoryMb: memory + 512 })} />
        </div>
        <div className="bb-prefs-row">
          <span className="bb-rowmark"><Icon name="cpu" size="sm" /></span>
          <div>
            <b>{t('beatbox.settings.machines.cpus')}</b>
            <span>{t('beatbox.settings.machines.cpus_here', { count: cpus })}</span>
          </div>
          <Stepper label={t('beatbox.settings.machines.cpus')} value={String(prefs.newCpus)}
                   less={prefs.newCpus > 1} more={prefs.newCpus < cpus}
                   onLess={() => set({ newCpus: prefs.newCpus - 1 })}
                   onMore={() => set({ newCpus: prefs.newCpus + 1 })} />
        </div>
        <div className="bb-prefs-row">
          <span className="bb-rowmark"><Icon name="network" size="sm" /></span>
          <div>
            <b>{t('beatbox.settings.machines.reach')} <Info text={t('beatbox.settings.machines.reach_what')} /></b>
          </div>
          <span className="bb-net-seg" role="radiogroup" aria-label={t('beatbox.settings.machines.reach')}>
            {REACH.map((reach) => (
              <button key={reach} type="button" role="radio" aria-checked={prefs.newReach === reach}
                      title={runtimeText(`beatbox.reach.${reach}.what`)}
                      className={prefs.newReach === reach ? 'on' : ''} onClick={() => set({ newReach: reach })}>
                {runtimeText(`beatbox.reach.${reach}`)}
              </button>
            ))}
          </span>
        </div>
        <div className="bb-prefs-row">
          <span className="bb-rowmark"><Icon name="terminal" size="sm" /></span>
          <div>
            <b>{t('beatbox.settings.machines.agent')} <Info text={t('beatbox.settings.machines.agent_what')} /></b>
          </div>
          <Switch checked={prefs.newAgent} label={t('beatbox.settings.machines.agent')}
                  onChange={(next) => set({ newAgent: next })} />
        </div>
      </Card>

      {total > 0 ? (
        <Card title={<><Icon name="box" size="sm" />{t('beatbox.settings.machines.now')}</>}>
          <p className="bb-prefs-what" style={fits ? undefined : { color: 'var(--bb-color-amber)' }}>
            {fits ? t('beatbox.settings.machines.fits', { left: gb(free - memory - QEMU_MB) })
              : t('beatbox.settings.machines.would_not', { free: gb(free) })}
          </p>
          <div className="bb-prefs-bar" role="img" aria-label={[
            `${t('beatbox.settings.machines.this_one')} ${gb(memory)}`,
            `${t('beatbox.settings.memory.machines')} ${gb(machinesMb)}`,
            `${t('beatbox.settings.memory.others')} ${gb(others)}`,
            t('beatbox.settings.memory.of', { total: gb(total) }),
          ].join(', ')}>
            <span style={{ width: share(machinesMb) }} className="bb-prefs-bar--machines" />
            <span style={{ width: share(others) }} className="bb-prefs-bar--others" />
            <span style={{ width: share(memory) }} className={fits ? 'bb-prefs-bar--new' : 'bb-prefs-bar--over'} />
          </div>
          <div className="bb-prefs-legend">
            <span><i className={fits ? 'bb-prefs-bar--new' : 'bb-prefs-bar--over'} />{t('beatbox.settings.machines.this_one')} <b className="bb-mono">{gb(memory)}</b></span>
            <span><i className="bb-prefs-bar--machines" />{t('beatbox.settings.memory.machines')} <b className="bb-mono">{gb(machinesMb)}</b></span>
            <span><i className="bb-prefs-bar--others" />{t('beatbox.settings.memory.others')} <b className="bb-mono">{gb(others)}</b></span>
            <span className="bb-mono" style={{ marginLeft: 'auto' }}>{t('beatbox.settings.memory.of', { total: gb(total) })}</span>
          </div>
        </Card>
      ) : null}
    </>
  );
}

/* ── Storage ─────────────────────────────────────────────────────────── */

function storageTotal(view: StorageView): number {
  return view.machines.reduce((sum, row) => sum + row.bytes, 0)
    + view.disks.reduce((sum, row) => sum + row.bytes, 0)
    + view.toolsBytes + view.leftoverBytes;
}

const LIVE = new Set(['running', 'paused']);
const LIFE_TONE: Record<string, Tone | undefined> = { running: 'mint', paused: 'amber', failed: 'rose' };
const KIND_ICON: Record<Kind, IconName> = { host: 'monitor', network: 'network', server: 'server' };
const kindOf = (type: string): Kind => (type === 'network' ? 'network' : type === 'server' ? 'server' : 'host');

/**
 * Delete, asked twice: what and how much, then "for good". The second button
 * is briefly disabled so a double click can't answer both.
 */
function DeleteAsk({ first, busy, onKeep, onDelete }: {
  first: string;
  busy: boolean;
  onKeep: () => void;
  onDelete: () => void;
}) {
  const [sure, setSure] = useState(false);
  const [settling, setSettling] = useState(false);
  useEffect(() => {
    if (!sure) return;
    setSettling(true);
    const later = window.setTimeout(() => setSettling(false), 700);
    return () => window.clearTimeout(later);
  }, [sure]);
  return (
    <div className="bb-prefs-ask" role="alertdialog" aria-live="polite">
      <Icon name="alert" size="sm" />
      <p>
        <b>{sure ? t('beatbox.settings.storage.last_chance') : first}</b>
        {sure ? <span>{t('beatbox.settings.storage.no_undo')}</span> : null}
      </p>
      <Button size="sm" onClick={onKeep}>{t('beatbox.settings.storage.keep')}</Button>
      {sure ? (
        <Button size="sm" variant="danger" icon="trash" disabled={busy || settling} onClick={onDelete}>
          {t('beatbox.settings.storage.delete_for_good')}
        </Button>
      ) : (
        <Button size="sm" variant="danger" onClick={() => setSure(true)}>{t('beatbox.settings.storage.delete')}</Button>
      )}
    </div>
  );
}

function Storage({ storage, update }: { storage: StorageView | null; update: (next: StorageView) => void }) {
  const [asking, setAsking] = useState('');
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  if (!storage) {
    return <Card><p className="bb-faint" style={{ fontSize: 12.5, margin: 0 }}>{t('beatbox.run_stage.reading')}</p></Card>;
  }

  async function act(work: () => Promise<StorageView>) {
    setBusy(true);
    setFailure(null);
    try {
      update(await work());
    } catch (error) {
      setFailure(fault(error));
    } finally {
      setBusy(false);
    }
  }

  const total = storageTotal(storage);
  const machines = storage.machines.reduce((sum, row) => sum + row.bytes, 0);
  const disks = storage.disks.reduce((sum, row) => sum + row.bytes, 0) + storage.toolsBytes;
  const share = (count: number) => `${total ? (count / total) * 100 : 0}%`;
  const parts: [string, string, number][] = [
    ['bb-prefs-bar--machines', t('beatbox.settings.storage.machines'), machines],
    ['bb-prefs-bar--others', t('beatbox.settings.storage.disks'), disks],
  ];
  if (storage.leftoverBytes > 0) parts.push(['bb-prefs-bar--over', t('beatbox.settings.storage.left'), storage.leftoverBytes]);

  return (
    <>
      <Card>
        <div className="bb-prefs-sum">
          <div>
            <p className="bb-prefs-verdict">{bytes(total)} <Info text={t('beatbox.settings.storage.what')} /></p>
          </div>
          <Button size="sm" icon="folder" onClick={() => {
            void call('host.reveal', { what: 'folder' }).catch((error) => setFailure(fault(error)));
          }}>
            {t('beatbox.settings.open_folder')}
          </Button>
        </div>
        <div className="bb-prefs-bar" style={{ marginTop: 16 }} role="img"
             aria-label={parts.map(([, name, count]) => `${name} ${bytes(count)}`).join(', ')}>
          {parts.map(([tone, name, count]) => <span key={name} style={{ width: share(count) }} className={tone} />)}
        </div>
        <div className="bb-prefs-legend">
          {parts.map(([tone, name, count]) => (
            <span key={name}><i className={tone} />{name} <b className="bb-mono">{bytes(count)}</b></span>
          ))}
          <span className="bb-mono" style={{ marginLeft: 'auto' }}>{storage.folder}</span>
        </div>
        {storage.leftoverBytes > 0 ? (
          <div className="bb-prefs-row" style={{ marginTop: 16 }}>
            <span className="bb-rowmark bb-amber"><Icon name="broom" size="sm" /></span>
            <div>
              <b>{t('beatbox.settings.storage.left_title', { amount: bytes(storage.leftoverBytes) })} <Info text={t('beatbox.settings.storage.left_what')} /></b>
            </div>
            <Button size="sm" disabled={busy} onClick={() => void act(() => call('storage.sweep'))}>
              {t('beatbox.settings.storage.clean')}
            </Button>
          </div>
        ) : (
          <p className="bb-faint" style={{ fontSize: 12.5, margin: '16px 0 0' }}>{t('beatbox.settings.storage.tidy')}</p>
        )}
      </Card>

      <Card title={<><Icon name="cpu" size="sm" />{t('beatbox.settings.storage.machines')}</>}
            info={t('beatbox.settings.storage.machines_what')}>
        {storage.machines.length === 0 ? (
          <p className="bb-faint" style={{ fontSize: 12.5, margin: '12px 0 0' }}>{t('beatbox.settings.storage.no_machines')}</p>
        ) : [...storage.machines].sort((a, b) => b.bytes - a.bytes).map((row) => {
          const kind = kindOf(row.type);
          const key = `machine:${row.id}`;
          return (
            <div key={row.id}>
              <div className="bb-prefs-row">
                <span className="bb-rowmark"><Icon name={KIND_ICON[kind]} size="sm" /></span>
                <div>
                  <b>{row.name}</b>
                  <span className="bb-prefs-life">
                    {LIFE_TONE[row.lifecycle] ? <i className={`bb-prefs-dot bb-prefs-dot--${LIFE_TONE[row.lifecycle]}`} /> : null}
                    {runtimeText(`beatbox.lifecycle.${row.lifecycle}`)}
                  </span>
                </div>
                <span className="bb-mono bb-prefs-size">{bytes(row.bytes)}</span>
                <span className="bb-prefs-acts">
                  <button type="button" className="bb-act bb-act--row" title={t('beatbox.settings.open')}
                          aria-label={`${t('beatbox.settings.open')} ${row.name}`} onClick={() => {
                            remember(kind, row.id);
                            navigate(SANDBOXES.find((each) => each.key === kind)?.options[0]?.path ?? 'host/machine');
                          }}>
                    <Icon name="arrow" size="sm" />
                  </button>
                  {/* Running ones can't be deleted here; stop it on its page first. */}
                  <button type="button" className="bb-act bb-act--row bb-act--danger"
                          disabled={busy || LIVE.has(row.lifecycle) || asking === key}
                          title={LIVE.has(row.lifecycle) ? t('beatbox.settings.storage.stop_first') : t('beatbox.settings.storage.delete')}
                          aria-label={`${t('beatbox.settings.storage.delete')} ${row.name}`} onClick={() => setAsking(key)}>
                    <Icon name="trash" size="sm" />
                  </button>
                </span>
              </div>
              {asking === key ? (
                <DeleteAsk first={t('beatbox.settings.storage.delete_ask', { name: row.name, amount: bytes(row.bytes) })}
                           busy={busy} onKeep={() => setAsking('')}
                           onDelete={() => {
                             setAsking('');
                             void act(async () => {
                               await call('sandbox.transition', { id: row.id, transition: 'destroy' });
                               return call('storage.usage');
                             });
                           }} />
              ) : null}
            </div>
          );
        })}
      </Card>

      <Card title={<><Icon name="database" size="sm" />{t('beatbox.settings.storage.disks')}</>}
            info={t('beatbox.settings.storage.disks_what')}>
        {storage.disks.length === 0 ? (
          <p className="bb-faint" style={{ fontSize: 12.5, margin: '12px 0 0' }}>{t('beatbox.settings.storage.no_disks')}</p>
        ) : [...storage.disks].sort((a, b) => b.bytes - a.bytes).map((row) => {
          const key = `disk:${row.name}`;
          // Machines built on it go too: their disks are overlays on this file.
          const made = row.madeInto.map((id) => storage.machines.find((each) => each.id === id))
            .filter((each): each is StoredMachine => Boolean(each));
          const names = made.map((each) => each.name).join(', ');
          const running = made.filter((each) => LIVE.has(each.lifecycle));
          const amount = row.bytes + made.reduce((sum, each) => sum + each.bytes, 0);
          return (
            <div key={row.name}>
              <div className="bb-prefs-row">
                <span className="bb-rowmark"><Icon name="database" size="sm" /></span>
                <div>
                  <b>{row.name}</b>
                  <span>{made.length ? t('beatbox.settings.storage.used_by', { names }) : t('beatbox.settings.storage.unused')}</span>
                </div>
                <span className="bb-mono bb-prefs-size">{bytes(row.bytes)}</span>
                <span className="bb-prefs-acts">
                  <button type="button" className="bb-act bb-act--row bb-act--danger"
                          disabled={busy || running.length > 0 || asking === key}
                          title={running.length
                            ? t('beatbox.settings.storage.stop_these', { names: running.map((each) => each.name).join(', ') })
                            : t('beatbox.settings.storage.delete')}
                          aria-label={`${t('beatbox.settings.storage.delete')} ${row.name}`} onClick={() => setAsking(key)}>
                    <Icon name="trash" size="sm" />
                  </button>
                </span>
              </div>
              {asking === key ? (
                <DeleteAsk first={made.length
                  ? t(made.length === 1 ? 'beatbox.settings.storage.delete_ask_made_one' : 'beatbox.settings.storage.delete_ask_made',
                    { name: row.name, names, amount: bytes(amount) })
                  : t('beatbox.settings.storage.delete_ask', { name: row.name, amount: bytes(row.bytes) })}
                           busy={busy} onKeep={() => setAsking('')}
                           onDelete={() => {
                             setAsking('');
                             void act(async () => {
                               for (const each of made) {
                                 await call('sandbox.transition', { id: each.id, transition: 'destroy' });
                               }
                               await call('disks.forget', { name: row.name });
                               return call('storage.usage');
                             });
                           }} />
              ) : null}
            </div>
          );
        })}
        {storage.toolsBytes > 0 ? (
          <div className="bb-prefs-row">
            <span className="bb-rowmark"><Icon name="monitor" size="sm" /></span>
            <div>
              <b>{t('beatbox.settings.can.tools')} <Info text={t('beatbox.settings.can.tools_what')} /></b>
            </div>
            <span className="bb-mono bb-prefs-size">{bytes(storage.toolsBytes)}</span>
            <span className="bb-prefs-acts" />
          </div>
        ) : null}
      </Card>

      {failure ? <p className="bb-rose" style={{ fontSize: 12.5, margin: 0 }}>{failure}</p> : null}
    </>
  );
}

/* ── About ───────────────────────────────────────────────────────────── */

function About({ about, probe }: { about: AboutView | null; probe: SystemProbe | null }) {
  const [copied, setCopied] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  if (!about) {
    return <Card><p className="bb-faint" style={{ fontSize: 12.5, margin: 0 }}>{t('beatbox.run_stage.reading')}</p></Card>;
  }
  const running = about.enginePid > 0;

  function reveal(what: 'folder' | 'log') {
    setFailure(null);
    void call('host.reveal', { what }).catch((error) => setFailure(fault(error)));
  }

  async function copy() {
    if (!about) return;
    const lines = [
      `BeatBox ${about.version}`,
      `Engine: ${running ? `${about.engineBuild || '?'} (pid ${about.enginePid})` : 'not running'}`,
      probe ? `Platform: ${probe.platform}` : '',
      `Folder: ${about.folder}`,
      `Log: ${about.log} (${bytes(about.logBytes)})`,
    ].filter(Boolean);
    try {
      await navigator.clipboard.writeText(lines.join('\n'));
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch (error) {
      setFailure(fault(error));
    }
  }

  return (
    <>
      <Card>
        <div className="bb-prefs-sum">
          <div>
            <p className="bb-prefs-verdict">BeatBox</p>
            <p className="bb-prefs-what" style={{ margin: '6px 0 0' }}>{t('beatbox.settings.about.version', { version: about.version })}</p>
          </div>
        </div>
      </Card>

      <Card title={<><Icon name="user" size="sm" />{t('beatbox.settings.about.you')}</>}>
        <div className="bb-prefs-row">
          <span className="bb-rowmark"><Icon name="user" size="sm" /></span>
          <div>
            <b>{t('beatbox.account.guest')} <Info text={t('beatbox.settings.about.guest_what')} /></b>
          </div>
          <span className="bb-prefs-state">{t('beatbox.settings.about.signin_later')}</span>
        </div>
      </Card>

      <Card title={<><Icon name="alert" size="sm" />{t('beatbox.settings.about.wrong')}</>}
            info={t('beatbox.settings.about.wrong_what')}>
        <div className="bb-prefs-row">
          <span className={`bb-rowmark${running ? '' : ' bb-amber'}`}><Icon name="server" size="sm" /></span>
          <div>
            <b>{t('beatbox.settings.about.engine')} <Info text={t('beatbox.settings.about.engine_what')} /></b>
            {running ? <code>beatboxd · pid {about.enginePid}{about.engineBuild ? ` · ${about.engineBuild}` : ''}</code> : null}
          </div>
          <span className="bb-prefs-state">
            <i className={`bb-prefs-dot bb-prefs-dot--${running ? 'mint' : 'amber'}`} />
            {t(running ? 'beatbox.lifecycle.running' : 'beatbox.settings.about.engine_off')}
          </span>
        </div>
        <div className="bb-prefs-row">
          <span className="bb-rowmark"><Icon name="logs" size="sm" /></span>
          <div>
            <b>{t('beatbox.settings.about.log')} <Info text={t('beatbox.settings.about.log_what')} /></b>
            <code>{about.log} · {bytes(about.logBytes)}</code>
          </div>
          <Button size="sm" onClick={() => reveal('log')}>{t('beatbox.settings.open')}</Button>
        </div>
        <div className="bb-prefs-row">
          <span className="bb-rowmark"><Icon name="folder" size="sm" /></span>
          <div>
            <b>{t('beatbox.settings.about.folder')} <Info text={t('beatbox.settings.about.folder_what')} /></b>
            <code>{about.folder}</code>
          </div>
          <Button size="sm" onClick={() => reveal('folder')}>{t('beatbox.settings.open')}</Button>
        </div>
        <div className="bb-prefs-foot">
          <Button size="sm" icon={copied ? 'check' : 'copy'} onClick={() => void copy()}>
            {t(copied ? 'beatbox.settings.about.copied' : 'beatbox.settings.about.copy')}
          </Button>
        </div>
        {failure ? <p className="bb-rose" style={{ fontSize: 12.5, margin: '8px 0 0' }}>{failure}</p> : null}
      </Card>
    </>
  );
}
