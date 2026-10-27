import type { KeySuffix } from '../i18n';
import { useCallback, useEffect, useState } from 'react';
import {
  Button, Card, Cube, CubeWire, FilePicker, Icon, IconButton, Info, Modal, Pill, RenameTitle, Select, Switch, toneInk,
} from '../components';
import { Happened, SaveNow, SavePoints, Share, clock, gigs, usePicture } from './MachineParts';
import type { IconName } from '../components';
import { call, on } from '../bridge/client';
import type {
  BeatDropSourceView, BeatDropTargetView, DiskInspectView, DiskView, Job, ReasonView, SandboxStateView, SandboxSummary, SandboxTypeView, SandboxView,
} from '../bridge/types';
import { plainDetail, plainly, t, runtimeText } from '../i18n';
import { TONES, remember, rememberMachine, rememberedMachine, useChosenId } from '../chosen';
import { href, navigate, useRouteParams } from '../router';
import { cableOf, plug, unplug } from '../cable';

/**
 * A machine of your own that you can throw away. The product; Engine.tsx is
 * the debugger.
 *
 * No identifiers, paths, hypervisor words or capability names here. What a
 * machine can't do right now is hidden or explained in one sentence.
 *
 * The engine still decides everything (`view.transitions`,
 * `view.capabilities`, its refusals); this page only picks the words. So no
 * `say()` or `fault()` here, they print engine-speak. Refusals go through `why()`.
 */

/** ms between polls of a running machine. */
const WATCH = 4000;

/** Remembering it is how we open it: the rail follows. */
const rememberNetwork = (id: string) => remember('network', id);

/** Icon per lifecycle button. Resume looks like start on purpose. */
const ACTS: Record<string, IconName> = {
  start: 'play', resume: 'play', stop: 'stop', pause: 'freeze', reset: 'refresh', destroy: 'trash',
};

/**
 * Engine refusals reworded for a person, not a developer. Missing codes fall
 * back to `beatbox.plain.why.unknown` ("didn't work, details are here").
 */
const WHY: Record<string, KeySuffix<'beatbox.plain.why.'>> = {
  'host.no_agent': 'no_agent',
  'host.no_disk': 'no_disk',
  'host.guest_went': 'went_away',
  'host.guest_shut_down': 'shut_down',
  'host.state_saved': 'state_saved',
  'host.guest_killed': 'out_of_memory',
  'host.not_enough_memory': 'not_enough_memory',
  'host.guest_lost': 'guest_lost',
  'engine.sandbox_lost': 'went_away',
  'engine.not_running': 'not_running',
  'engine.cannot_act': 'gone',
  'engine.unsupported': 'cannot',
  'engine.bad_transition': 'not_now',
  'engine.no_such_snapshot': 'no_save_point',
  'engine.no_such_file': 'no_file',
  'engine.missing_argument': 'blank',
  'engine.over_budget': 'no_room',
  'engine.unknown_type': 'unknown_kind',
  'engine.provider_failed': 'broke',
  'machine.no_such_image': 'no_disk_there',
  'machine.no_qemu': 'no_qemu',
  'machine.too_little_memory': 'too_little_memory',
  'machine.too_few_cpus': 'too_few_cpus',
  'machine.hostile_with_a_route_out': 'hostile_with_a_way_out',
  'machine.snapshot_failed': 'save_failed',
  'machine.snapshot_restore_failed': 'restore_failed',
  'machine.snapshot_delete_failed': 'delete_failed',
  'machine.snapshot_setup_differs': 'setup_differs',
  'host.stopped_after_failed_restore': 'stopped_after_failed_restore',
  'machine.bad_snapshot_name': 'bad_save_name',
  'machine.no_channel': 'not_answering',
  'machine.channel_gone': 'not_answering',
  'machine.refused_command': 'refused',
  'host.program_refused': 'program_refused',
  'engine.reconfigure_not_now': 'reconfigure_not_now',
  'host.fixed_setting': 'fixed_setting',
  'machine.command_outlived_us': 'still_going',
  'build.no_qemu_img': 'no_qemu',
  'build.no_display': 'no_display',
  'disk.in_use': 'disk_in_use',
  'disk.unknown': 'no_such_disk',
  'build.overlay_failed': 'no_disk_made',
  'build.unreadable_disk': 'unreadable_disk',
  'build.target_exists': 'disk_in_the_way',
  'tools.download_failed': 'tools_download_failed',
  'tools.wrong_file': 'tools_wrong_file',
  'tools.not_here': 'tools_not_here',
  'disks.not_there': 'disk_not_there',
  'disks.unreadable': 'not_a_disk',
  'disks.virtualbox_difference': 'virtualbox_difference',
  'disks.ova_compressed': 'ova_compressed',
  'disks.ova_without_disk': 'ova_without_disk',
  'disks.unpack_failed': 'unpack_failed',
  'host.no_uefi': 'no_uefi',
  'host.no_uefi_vars': 'no_uefi_vars',
  'machine.no_uefi': 'uefi_gone',
  'host.ova_is_an_archive': 'ova_is_an_archive',
  'disks.nothing_installed': 'nothing_installed',
  'disks.install_unfinished': 'install_unfinished',
  'beatdrop.copy_failed': 'beatdrop_copy_failed',
};

/** One refusal in plain words. Values (paths etc.) pass through untouched. */
function why(reason: ReasonView | null | undefined): string {
  const word = reason && reason.code ? WHY[reason.code] : undefined;
  if (!word) return t('beatbox.plain.why.unknown');
  const values = plainly((reason?.values ?? {}) as Record<string, unknown>);
  if (word === 'setup_differs') {
    const setting = String(values.setting ?? '');
    return t('beatbox.plain.why.setup_differs', {
      name: values.name ?? '', setting: settingWord(setting),
      was: setupWord(setting, String(values.was ?? '')), now: setupWord(setting, String(values.now ?? '')),
    });
  }
  return t(`beatbox.plain.why.${word}`, values);
}

/** Hardware setting, in everyday words. */
function settingWord(setting: string): string {
  switch (setting) {
    case 'network': return t('beatbox.plain.setting.network');
    case 'processors': return t('beatbox.plain.setting.processors');
    case 'memory': return t('beatbox.plain.setting.memory');
    case 'agent': return t('beatbox.plain.setting.agent');
    case 'firmware': return t('beatbox.plain.setting.firmware');
    case 'pen': return t('beatbox.plain.setting.pen');
    default: return setting;
  }
}

/** A hardware setting's value, in words. */
function setupWord(setting: string, value: string): string {
  if (setting === 'network') {
    switch (value) {
      case 'none': return t('beatbox.plain.card.none');
      case 'isolated': return t('beatbox.plain.card.isolated');
      case 'nat': return t('beatbox.plain.card.nat');
      case 'attached': return t('beatbox.plain.card.attached');
      default: return value;
    }
  }
  if (setting === 'memory') return `${value} MB`;
  if (setting === 'firmware') return value.toUpperCase();
  if (setting === 'agent') return value === 'true' ? t('beatbox.plain.on') : t('beatbox.plain.off');
  if (setting === 'pen') return value === 'true' ? t('beatbox.plain.pen.true') : t('beatbox.plain.pen.false');
  return value;
}

/**
 * The line next to the state: the known reason it got there (shut down from
 * inside, killed for memory), else what the state means. "It stopped" is no
 * answer when your computer ran out of memory.
 */
function settledBecause(state: SandboxStateView): string {
  const code = state.reason?.code;
  if (state.lifecycle !== 'running' && code && WHY[code]) return why(state.reason);
  return runtimeText(`beatbox.plain.is.${state.lifecycle}`);
}

/** Same, for a call that threw instead of answering. */
function trouble(error: unknown): string {
  return why((error as { reason?: ReasonView | null } | null)?.reason ?? null);
}

function beatdropTrouble(error: unknown): string {
  const code = (error as { code?: string } | null)?.code;
  const words: Record<string, KeySuffix<'beatbox.beatdrop.error.'>> = {
    'beatdrop-platform': 'platform',
    'beatdrop-inspect': 'inspect_failed',
    'beatdrop-source': 'source_missing',
    'beatdrop-mounted': 'mounted',
    'beatdrop-dependent': 'dependent_error',
    'beatdrop-size': 'size_unknown',
    'beatdrop-partial': 'partial',
    'beatdrop-target': 'target_missing',
    'beatdrop-leave-out': 'leave_out_bad',
    'disk-exists': 'exists',
    'bad-name': 'bad_name',
    busy: 'busy',
  };
  return code && words[code] ? t(`beatbox.beatdrop.error.${words[code]}`) : plainDetail((error as Error).message);
}

/** Can't be undone, so they ask first: reset drops every change, destroy drops the machine. */
const GRAVE = ['reset', 'destroy'];

const bag = (value: unknown): Record<string, unknown> =>
  (value && typeof value === 'object' ? value as Record<string, unknown> : {});

/** "3.2": real file size, not the size the guest sees. */
const gigabytes = (bytes: number) => (bytes / 1e9).toFixed(1);

/* --- making one ----------------------------------------------------------- */

function BeatDrop({ onMade, onClose }: { onMade: (path: string) => void; onClose: () => void }) {
  const [sources, setSources] = useState<BeatDropSourceView[]>([]);
  const [source, setSource] = useState('');
  const [targets, setTargets] = useState<BeatDropTargetView[]>([]);
  const [target, setTarget] = useState('');
  // Two steps: what to copy, then where it goes.
  const [step, setStep] = useState<1 | 2>(1);
  // Folders to skip in a live copy, with sizes once measured.
  const [leaveOut, setLeaveOut] = useState<{ path: string; bytes: number | null }[]>([]);
  const [picking, setPicking] = useState(false);
  // Home's subfolders, largest first: usually what's worth skipping.
  const [homes, setHomes] = useState<DiskView[] | null>(null);
  const [name, setName] = useState('my-linux');
  const [job, setJob] = useState<Job | null>(null);
  const [starting, setStarting] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let live = true;
    void call('beatdrop.inspect')
      .then((view) => {
        if (!live) return;
        setSources(view.sources);
        // Default to the system disk, not whatever lsblk listed first.
        const system = view.sources.find((entry) => entry.mountpoints.includes('/'));
        setSource((system ?? view.sources[0])?.path ?? '');
        setTargets(view.targets);
      })
      .catch((error: unknown) => { if (live) setFailure(beatdropTrouble(error)); })
      .finally(() => { if (live) setLoading(false); });
    void call('jobs.current')
      .then((current) => {
        if (live && current?.kind === 'beatdrop' && current.state === 'running') {
          setJob((seen) => seen?.id === current.id && seen.state !== 'running' ? seen : current);
        }
      })
      .catch(() => undefined);
    return () => { live = false; };
  }, []);

  useEffect(() => on('job', (fresh) => {
    if (fresh.kind !== 'beatdrop') return;
    setJob(fresh);
    if (fresh.state === 'done') onMade(fresh.message);
    if (fresh.state === 'failed') setFailure(fresh.reason ? why(fresh.reason) : plainDetail(fresh.message));
  }), [onMade]);

  const chosen = sources.find((entry) => entry.path === source);
  const inUse = Boolean(chosen?.mountpoints.length);
  // System disk copies live; other disks should be unmounted first.
  const system = Boolean(chosen?.mountpoints.includes('/'));
  const live = inUse && !chosen?.problem && !chosen?.dependent;
  const listHomes = system && live;
  const measuring = leaveOut.some((entry) => entry.bytes === null);
  const saved = leaveOut.reduce((sum, entry) => sum + (entry.bytes ?? 0), 0);
  const needed = Math.max(0, (chosen?.needed ?? 0) - saved);
  const spot = targets.find((entry) => entry.path === target);
  const whatReady = Boolean(chosen && !chosen.dependent && !chosen.problem && !measuring);
  const whereReady = Boolean(name.trim() && (!inUse || (spot && spot.free >= needed)));

  useEffect(() => {
    if (!listHomes || homes) return;
    void call('beatdrop.folders', { path: '' }).then(setHomes).catch(() => setHomes([]));
  }, [listHomes, homes]);

  // Pick the first destination with room.
  useEffect(() => {
    if (step !== 2 || spot) return;
    const roomy = targets.find((entry) => entry.disk !== source && entry.free >= needed)
      ?? targets.find((entry) => entry.free >= needed);
    if (roomy) setTarget(roomy.path);
  }, [step, spot, targets, source, needed]);

  async function start() {
    if (starting) return;
    setStarting(true);
    setFailure(null);
    try {
      const started = await call('beatdrop.start', {
        source, name, target: inUse ? target : null, leaveOut: inUse ? leaveOut.map((entry) => entry.path) : [],
      });
      setJob((current) => current?.id === started.id && current.state !== 'running' ? current : started);
    } catch (error) {
      setFailure(beatdropTrouble(error));
    } finally {
      setStarting(false);
    }
  }

  function leave(path: string) {
    setPicking(false);
    if (leaveOut.some((entry) => entry.path === path)) return;
    setLeaveOut((now) => [...now, { path, bytes: null }]);
    void call('beatdrop.measure', { path })
      .then((bytes) => setLeaveOut((now) => now.map((entry) => entry.path === path ? { path, bytes } : entry)))
      .catch((error: unknown) => {
        setLeaveOut((now) => now.filter((entry) => entry.path !== path));
        setFailure(beatdropTrouble(error));
      });
  }

  function toggle(path: string, bytes: number | null) {
    setLeaveOut((now) => (now.some((other) => other.path === path)
      ? now.filter((other) => other.path !== path)
      : [...now, { path, bytes }]));
  }

  if (picking) {
    return (
      <FilePicker folders title={t('beatbox.beatdrop.leave_out_pick')} suffixes="" filterLabel=""
                  start="" onPick={leave} onClose={() => setPicking(false)} />
    );
  }

  const busy = starting || job?.state === 'running';
  const percent = Number.parseInt(job?.message ?? '', 10) || 0;
  // Hand-picked skips beyond home's subfolders.
  const extra = leaveOut.filter((entry) => !homes?.some((home) => home.path === entry.path));
  const rows = [
    ...(homes ?? []).map((entry) => ({ path: entry.path, label: entry.name, bytes: entry.bytes as number | null })),
    ...extra.map((entry) => ({ path: entry.path, label: entry.path, bytes: entry.bytes })),
  ];
  const place = (entry: BeatDropTargetView) =>
    entry.path.startsWith('/run/media/') ? entry.path.split('/').pop() ?? entry.path : entry.path;

  const footer = busy ? (
    <>
      <span style={{ flex: 1 }} />
      {job?.state === 'running' ? (
        <Button onClick={() => { void call('beatdrop.cancel').catch(() => undefined); }}>
          {t('beatbox.beatdrop.cancel')}
        </Button>
      ) : null}
      <Button onClick={onClose}>{t('beatbox.beatdrop.leave')}</Button>
    </>
  ) : step === 1 ? (
    <>
      <span style={{ flex: 1 }} />
      <Button onClick={onClose}>{t('beatbox.closing.cancel')}</Button>
      <Button variant="primary" disabled={!whatReady || loading}
              onClick={() => { setFailure(null); setStep(2); }}>
        {t('beatbox.beatdrop.next')}
      </Button>
    </>
  ) : (
    <>
      <span style={{ flex: 1 }} />
      <Button onClick={() => { setFailure(null); setStep(1); }}>{t('beatbox.beatdrop.back')}</Button>
      <Button variant="primary" disabled={!whereReady} onClick={() => void start()}>
        {t('beatbox.beatdrop.start')}
      </Button>
    </>
  );

  return (
    <Modal title={t('beatbox.beatdrop.title')} icon="save" width={560} onClose={onClose} footer={footer}>
      <div className="bb-sbx-form bb-beatdrop">
        {busy ? (
          <div className="bb-beatdrop-progress">
            <div className="bb-beatdrop-progress-head">
              <b>{t('beatbox.beatdrop.copying')}</b>
              <span>{percent}%</span>
            </div>
            <div className="bb-beatdrop-meter"><i style={{ width: `${percent}%` }} /></div>
            <small className="bb-faint">{t('beatbox.beatdrop.copying_hint')}</small>
          </div>
        ) : step === 1 ? (
          <>
            <small className="bb-beatdrop-step">{t('beatbox.beatdrop.step_what')}</small>
            <div className="bb-beatdrop-disks" role="radiogroup" aria-label={t('beatbox.beatdrop.source')}>
              {sources.map((entry) => (
                <button key={entry.path} type="button" role="radio" aria-checked={source === entry.path}
                        className={`bb-beatdrop-disk${source === entry.path ? ' on' : ''}`}
                        onClick={() => { setSource(entry.path); setFailure(null); }}>
                  <Icon name="box" size="sm" />
                  <span>
                    <b>{entry.name}</b>
                    <small>
                      {gigabytes(entry.bytes)} GB · {t(entry.mountpoints.includes('/') ? 'beatbox.beatdrop.kind_system'
                        : entry.mountpoints.length ? 'beatbox.beatdrop.kind_mounted' : 'beatbox.beatdrop.kind_idle')}
                    </small>
                  </span>
                  {entry.problem || entry.dependent ? (
                    <Pill tone="amber">
                      {t(entry.problem ? 'beatbox.beatdrop.unmount' : 'beatbox.beatdrop.dependent')}
                    </Pill>
                  ) : null}
                </button>
              ))}
            </div>
            {chosen?.dependent ? (
              <small className="bb-amber">{t('beatbox.beatdrop.selected_dependent')}</small>
            ) : chosen?.problem ? (
              <small className="bb-amber">{t('beatbox.beatdrop.cannot_live', { why: plainDetail(String(chosen.problem ?? '')) })}</small>
            ) : null}
            {!loading && !sources.length ? <small className="bb-faint">{t('beatbox.beatdrop.none')}</small> : null}
            {live ? (
              <div className="bb-field">
                <div className="bb-beatdrop-label">
                  <span>{t('beatbox.beatdrop.leave_out')}</span>
                  {leaveOut.length ? (
                    <small>{t('beatbox.beatdrop.left_out', { count: leaveOut.length, size: gigabytes(saved) })}</small>
                  ) : null}
                </div>
                <div className="bb-beatdrop-folders">
                  {listHomes && !homes ? (
                    <small className="bb-beatdrop-folders-wait">{t('beatbox.beatdrop.measuring_home')}</small>
                  ) : null}
                  {rows.map((row) => (
                    <label key={row.path}>
                      <input type="checkbox" checked={leaveOut.some((entry) => entry.path === row.path)}
                             onChange={() => toggle(row.path, row.bytes)} />
                      <b title={row.path}>{row.label}</b>
                      <small>{row.bytes === null ? t('beatbox.beatdrop.measuring') : `${gigabytes(row.bytes)} GB`}</small>
                    </label>
                  ))}
                  <button type="button" className="bb-beatdrop-more" onClick={() => setPicking(true)}>
                    <Icon name="plus" size="sm" /> {t('beatbox.beatdrop.leave_out_add')}
                  </button>
                </div>
                <small className="bb-faint">{t('beatbox.beatdrop.leave_out_what')}</small>
              </div>
            ) : null}
          </>
        ) : (
          <>
            <small className="bb-beatdrop-step">{t('beatbox.beatdrop.step_where')}</small>
            {inUse ? (
              targets.length ? (
                <div className="bb-beatdrop-disks" role="radiogroup" aria-label={t('beatbox.beatdrop.target')}>
                  {targets.map((entry) => {
                    const small = entry.free < needed;
                    return (
                      <button key={entry.path} type="button" role="radio" aria-checked={target === entry.path}
                              disabled={small}
                              className={`bb-beatdrop-disk${target === entry.path ? ' on' : ''}`}
                              onClick={() => { setTarget(entry.path); setFailure(null); }}>
                        <Icon name="save" size="sm" />
                        <span>
                          <b>{place(entry)}</b>
                          <small>
                            {place(entry) === entry.path ? entry.disk : entry.path}
                            {' · '}{t('beatbox.beatdrop.free', { size: gigabytes(entry.free) })}
                          </small>
                        </span>
                        {small ? <Pill tone="amber">{t('beatbox.beatdrop.too_small')}</Pill> : null}
                      </button>
                    );
                  })}
                </div>
              ) : <small className="bb-amber">{t('beatbox.beatdrop.no_target')}</small>
            ) : null}
            <label className="bb-field">
              <span>{t('beatbox.beatdrop.name')}</span>
              <input className="bb-input" value={name} onChange={(event) => setName(event.target.value)} />
            </label>
            {inUse && spot ? (
              <div className="bb-beatdrop-space">
                <div className="bb-beatdrop-meter">
                  <i style={{ width: `${Math.min(100, (needed / Math.max(1, spot.free)) * 100)}%` }} />
                </div>
                <small className="bb-faint">
                  {t('beatbox.beatdrop.space', { need: gigabytes(needed), free: gigabytes(spot.free) })}
                </small>
              </div>
            ) : null}
            <small className="bb-faint">{t('beatbox.beatdrop.private')}</small>
          </>
        )}
        {failure ? <p className="bb-sbx-no">{failure}</p> : null}
      </div>
    </Modal>
  );
}

/** Icons for the two start options: an empty cube, or this computer going into one. */
function StartGlyph({ clone }: { clone: boolean }) {
  // Hexagon plus the three front edges, centred on (cx, cy), radius r.
  const cube = (cx: number, cy: number, r: number) => {
    const w = r * 0.866;
    return (
      <g>
        <polygon points={`${cx},${cy - r} ${cx + w},${cy - r / 2} ${cx + w},${cy + r / 2} ${cx},${cy + r} ${cx - w},${cy + r / 2} ${cx - w},${cy - r / 2}`} />
        <path d={`M${cx},${cy} L${cx - w},${cy - r / 2} M${cx},${cy} L${cx + w},${cy - r / 2} M${cx},${cy} L${cx},${cy + r}`} strokeWidth="1.1" />
      </g>
    );
  };
  return (
    <svg className="bb-start-glyph" viewBox="0 0 120 64" aria-hidden="true">
      {clone ? (
        <>
          <rect x="6" y="12" width="40" height="28" rx="3" />
          <path d="M20 48 H32 M26 40 V48" />
          <path d="M54 29 H72 M66 23 L72 29 L66 35" />
          {cube(96, 29, 19)}
        </>
      ) : cube(60, 32, 25)}
    </svg>
  );
}

/** Unattended installs by job id, so we know whether there's a window to show. */
const UNWATCHED = new Map<string, boolean>();

/** Install progress on the machines screen. The job makes and starts the machine; we open it after. */
function InstallUnderWay({ onMade }: { onMade: (id: string) => void }) {
  const [job, setJob] = useState<Job | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  // Pick up an install already running; events only cover what comes next.
  useEffect(() => {
    let live = true;
    void call('jobs.current')
      .then((current) => {
        if (live && current?.kind === 'disk' && current.state === 'running') setJob(current);
      })
      .catch(() => undefined);
    return () => { live = false; };
  }, []);

  useEffect(() => on('job', (fresh) => {
    if (fresh.kind !== 'disk') return;
    setJob(fresh.state === 'running' ? fresh : null);
    setFailure(fresh.state === 'failed' ? (fresh.reason ? why(fresh.reason) : plainDetail(fresh.message)) : null);
    if (fresh.state === 'done') onMade(fresh.message);
  }), [onMade]);

  if (!job && !failure) return null;
  const unwatched = job ? UNWATCHED.get(job.id) : undefined;
  return (
    <div className={`bb-waiting bb-install-status${failure ? ' bb-install-status--failed' : ''}`} role="status">
      {failure ? <Icon name="alert" size="sm" /> : <span className="bb-waiting-dot" />}
      <div>
        <b>{t(failure ? 'beatbox.machine.install_failed' : 'beatbox.machine.installing')}</b>
        {failure ? <small>{failure}</small> : (
          <>
            {unwatched === undefined ? null : (
              <small>{t(unwatched ? 'beatbox.machine.installing_windows' : 'beatbox.machine.installing_disc')}</small>
            )}
            <small>{t('beatbox.machine.installing_what')}</small>
          </>
        )}
      </div>
      {failure ? (
        <IconButton name="close" title={t('beatbox.closing.cancel')} onClick={() => setFailure(null)} />
      ) : (
        <Button size="sm" onClick={() => { void call('disks.finish').catch(() => undefined); }}>
          {t('beatbox.machine.install_stop')}
        </Button>
      )}
    </div>
  );
}

function NewMachine({ types, onMade, onClose }: {
  types: SandboxTypeView[];
  onMade: (made: SandboxView) => void;
  onClose: () => void;
}) {
  const [kind, setKind] = useState(types[0]?.name ?? 'host');
  const [name, setName] = useState('');
  const [disk, setDisk] = useState('');
  const [cpu, setCpu] = useState('2');
  // 2 GB: enough for Windows 10 and most Linux desktops. Start with the
  // least that works.
  const [memory, setMemory] = useState('2');
  const [free, setFree] = useState(0);
  const [linux, setLinux] = useState(false);
  const [network, setNetwork] = useState('none');
  const [agent, setAgent] = useState(true);
  const [browsing, setBrowsing] = useState(false);
  const [dropping, setDropping] = useState(false);
  // ISO: install to a new disk and boot that, or (off) boot it as a live disc.
  const [installs, setInstalls] = useState(true);
  const [size, setSize] = useState('60');
  // Step one: from scratch or a clone of this computer.
  const [started, setStarted] = useState(false);
  const [disks, setDisks] = useState<DiskView[]>([]);
  // Disk removal is confirmed inline; a modal on a modal loses people.
  const [removing, setRemoving] = useState('');
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  // Inspect the chosen disk, debounced.
  const [seen, setSeen] = useState<DiskInspectView | null>(null);
  const [unpacking, setUnpacking] = useState<Job | null>(null);

  useEffect(() => {
    const path = disk.trim();
    setSeen(null);
    if (!path) return;
    let live = true;
    const later = window.setTimeout(() => {
      void call('disks.inspect', { path })
        .then((view) => { if (live) setSeen(view); })
        .catch(() => undefined);
    }, 350);
    return () => { live = false; window.clearTimeout(later); };
  }, [disk]);

  useEffect(() => on('job', (fresh) => {
    if (fresh.kind !== 'unpack') return;
    setUnpacking(fresh.state === 'running' ? fresh : null);
    if (fresh.state === 'done') {
      setDisk(fresh.message);
      void call('disks.list').then(setDisks).catch(() => undefined);
    }
    if (fresh.state === 'failed') setFailure(fresh.reason ? why(fresh.reason) : plainDetail(fresh.message));
  }), []);

  /** File name -> disk name: letters, digits, space . _ - */
  const diskName = (text: string, suffix: RegExp) => {
    const stem = (text.split(/[\\/]/).pop() ?? 'disk').replace(suffix, '');
    return stem.replace(/[^A-Za-z0-9 ._-]/g, '-').replace(/^[^A-Za-z0-9]+/, '').slice(0, 64) || 'disk';
  };

  async function unpack() {
    setFailure(null);
    try {
      setUnpacking(await call('disks.unpack', { source: disk.trim(), name: diskName(disk, /\.ova$/i) }));
    } catch (error) {
      setFailure(trouble(error));
    }
  }

  const readDisks = useCallback(() => {
    void call('disks.list').then(setDisks).catch(() => undefined);
  }, []);
  useEffect(readDisks, [readDisks]);
  // Load defaults from Settings > New machines.
  useEffect(() => {
    void call('system.preferences').then((prefs) => {
      setCpu(String(prefs.newCpus));
      setMemory(String(Math.round(prefs.newMemoryMb / 1024 * 10) / 10));
      setNetwork(prefs.newReach);
      setAgent(prefs.newAgent);
    }).catch(() => undefined);
  }, []);
  useEffect(() => {
    void call('system.probe').then((probe) => {
      setFree(probe.memoryFreeMb);
      setLinux(probe.platform.startsWith('Linux'));
    }).catch(() => undefined);
  }, []);
  const memoryGb = (mib: number) => String(Math.round(mib / 1024 * 10) / 10);
  const tooMuch = free > 0 && (Number(memory) || 0) * 1024 + 512 > free;

  async function remove(name: string) {
    setRemoving('');
    try {
      setDisks(await call('disks.forget', { name }));
    } catch (error) {
      setFailure(trouble(error));
    }
  }

  const disc = Boolean(seen?.installer);
  const installing = disc && installs;

  async function make() {
    setBusy(true);
    setFailure(null);
    const machine = {
      type: kind,
      name,
      cpu: Number(cpu) || 0,
      // GB on screen, MiB on the wire.
      memory: Math.round((Number(memory) || 0) * 1024),
      // Always with a display here. Headless is the engine default, for automation.
      settings: { disk, network, agent, display: true },
      uses: {},
    };
    try {
      if (installing) {
        // Disk named after the machine and reusable later. The job makes the
        // machine when the disk is ready; InstallUnderWay takes it from here.
        const job = await call('disks.build', {
          source: disk.trim(),
          name: diskName(name.trim() || disk, /\.iso$/i),
          sizeGb: Number(size) || 0,
          network: false,
          unattended: true,
          machine,
        });
        UNWATCHED.set(job.id, seen?.installer === 'windows');
        onClose();
        return;
      }
      onMade(await call('sandbox.create', machine));
    } catch (error) {
      setFailure(trouble(error));
      setBusy(false);
    }
  }

  if (dropping) {
    return (
      <BeatDrop
        onClose={() => { setDropping(false); readDisks(); }}
        onMade={(path) => { setDisk(path); setDropping(false); setStarted(true); readDisks(); }}
      />
    );
  }

  if (browsing) {
    return (
      <FilePicker
        title={t('beatbox.machine.pick_disk')}
        suffixes=".qcow2 .img .raw .iso .vmdk .vdi .vhd .vhdx .ova"
        filterLabel={t('beatbox.machine.disks_only')}
        start={disk}
        onPick={(path) => { setDisk(path); setBrowsing(false); }}
        onClose={() => setBrowsing(false)}
      />
    );
  }

  if (!started) {
    return (
      // No footer: picking a card is the step; X or Esc leaves.
      <Modal title={t('beatbox.machine.new')} icon="sandbox" width={560} onClose={onClose}>
        <div className="bb-sbx-form">
        <div className="bb-start-choices">
          <button type="button" className="bb-start-choice" onClick={() => setStarted(true)}>
            <StartGlyph clone={false} />
            <b>{t('beatbox.machine.start_host')}</b>
            <small>{t('beatbox.machine.start_host_what')}</small>
          </button>
          <button type="button" className="bb-start-choice" disabled={!linux} onClick={() => setDropping(true)}>
            <StartGlyph clone />
            <b>{t('beatbox.beatdrop.button')}</b>
            <small>{t(linux ? 'beatbox.machine.start_clone_what' : 'beatbox.machine.start_clone_linux')}</small>
          </button>
        </div>
        </div>
      </Modal>
    );
  }

  return (
    <Modal title={t('beatbox.machine.new')} icon="sandbox" width={560} onClose={onClose}
           footer={
             <>
               <span style={{ flex: 1 }} />
               {/* "Cancel", not "Go back": it closes the dialog. */}
               <Button onClick={onClose}>{t('beatbox.files.cancel')}</Button>
               <Button variant="primary" autofocus onClick={make}
                       disabled={busy || !disk || seen?.format === 'ova' || Boolean(seen?.problem)
                         || seen?.bootable === false}>
                 {busy ? t('beatbox.machine.making') : t('beatbox.machine.make')}
               </Button>
             </>
           }>
      <div className="bb-sbx-form">
        {types.length > 1 ? (
          <label className="bb-field">
            <span>{t('beatbox.machine.what_kind')}</span>
            <div className="bb-sbx-choices">
              {types.map((entry) => (
                <button key={entry.name} type="button"
                        className={`bb-choice${kind === entry.name ? ' on' : ''}`}
                        onClick={() => setKind(entry.name)}>
                  {/* Not `entry.name`: that's the identifier, Engine screen only. */}
                  <b>{runtimeText(`beatbox.kind.${entry.name}`)}</b>
                  <small>{runtimeText(`beatbox.plain.boundary.${entry.isolation}`)}</small>
                </button>
              ))}
            </div>
          </label>
        ) : null}
        <label className="bb-field">
          <span>{t('beatbox.machine.what_disk')} <Info text={t('beatbox.machine.disk_hint')} /></span>
          {disks.length ? (
            <div className="bb-disks">
              {disks.map((made) => (removing === made.name ? (
                <div key={made.path} className="bb-disk bb-disk--asking">
                  <b>{t('beatbox.machine.remove_sure', { name: made.name })}</b>
                  <Button size="sm" variant="danger" onClick={() => void remove(made.name)}>
                    {t('beatbox.machine.remove_yes')}
                  </Button>
                  <Button size="sm" onClick={() => setRemoving('')}>
                    {t('beatbox.machine.remove_no')}
                  </Button>
                </div>
              ) : (
                <div key={made.path} className={`bb-disk${disk === made.path ? ' on' : ''}`}>
                  <button type="button" className="bb-disk-pick" onClick={() => setDisk(made.path)}>
                    <Icon name="box" size="sm" />
                    <b>{made.name}</b>
                    {/* "Empty" beats "0.0 GB" for a fresh disk. */}
                    <small>
                      {made.bytes < EMPTY_BYTES ? t('beatbox.machine.empty_disk')
                        : t('beatbox.machine.on_disk', { size: gigabytes(made.bytes) })}
                    </small>
                  </button>
                  <IconButton name="trash" title={t('beatbox.machine.remove_disk')}
                              onClick={() => { setRemoving(made.name); setFailure(null); }} />
                </div>
              )))}
            </div>
          ) : null}
          <div className="bb-shelf" style={{ gap: 8, marginTop: disks.length ? 8 : 0 }}>
            <input className="bb-input" value={disk} onChange={(e) => setDisk(e.target.value)}
                   placeholder={t('beatbox.machine.disk_placeholder')} />
            <Button size="sm" icon="folder" onClick={() => setBrowsing(true)}>
              {t('beatbox.machine.browse')}
            </Button>
          </div>
          {seen?.problem ? (
            <small className="bb-rose">{why(seen.problem)}</small>
          ) : seen?.format === 'ova' ? (
            <div className="bb-disk-note">
              <small>{t('beatbox.machine.ova', { size: gigabytes(seen.bytes) })}</small>
              <Button size="sm" icon="save" disabled={unpacking !== null} onClick={() => void unpack()}>
                {unpacking ? t('beatbox.machine.unpacking', { done: unpacking.message || '0%' })
                  : t('beatbox.machine.unpack')}
              </Button>
            </div>
          ) : (
            <>
              {seen && FOREIGN[seen.format] ? (
                <small className="bb-faint">{t('beatbox.machine.foreign', { maker: FOREIGN[seen.format]! })}</small>
              ) : null}
              {seen?.virtualboxSnapshots ? (
                <small className="bb-amber">{t('beatbox.machine.virtualbox_snapshots')}</small>
              ) : null}
              {seen && !seen.bootable ? (
                <small className="bb-rose">{t('beatbox.machine.not_bootable')}</small>
              ) : null}
              {seen?.firmware === 'uefi' ? (
                <small className="bb-faint">{t('beatbox.machine.uefi_found')}</small>
              ) : null}
              {disc ? (
                <div className="bb-sbx-toggle" style={{ marginTop: 8 }}>
                  <Switch id="bb-installs" checked={installs} onChange={setInstalls}
                          label={t('beatbox.machine.disc_install')} />
                  <label htmlFor="bb-installs">
                    <b>{t('beatbox.machine.disc_install')} <Info text={t(!installs ? 'beatbox.machine.disc_live'
                        : seen?.installer === 'windows' ? 'beatbox.machine.disc_install_windows'
                          : 'beatbox.machine.disc_install_disc')} /></b>
                  </label>
                </div>
              ) : null}
            </>
          )}
        </label>
        <label className="bb-field">
          <span>{t('beatbox.machine.call_it')}</span>
          <input className="bb-input" value={name} onChange={(e) => setName(e.target.value)}
                 placeholder={t('beatbox.machine.call_it_placeholder')} />
        </label>
        {installing ? (
          <label className="bb-field">
            <span>{t('beatbox.machine.disc_size')}</span>
            <input className="bb-input" value={size} inputMode="numeric"
                   onChange={(e) => setSize(e.target.value)} />
            <small className="bb-faint">{t('beatbox.install.size_hint')}</small>
          </label>
        ) : null}
        <div className="bb-sbx-pair">
          <label className="bb-field">
            <span>{t('beatbox.machine.processors')}</span>
            <input className="bb-input" value={cpu} inputMode="decimal"
                   onChange={(e) => setCpu(e.target.value)} />
          </label>
          <label className="bb-field">
            <span>{t('beatbox.machine.memory')}</span>
            <input className="bb-input" value={memory} inputMode="decimal"
                   onChange={(e) => setMemory(e.target.value)} />
            {free > 0 ? (
              <small className={tooMuch ? 'bb-rose' : 'bb-faint'}>
                {t(tooMuch ? 'beatbox.machine.memory_too_much' : 'beatbox.machine.memory_free',
                   { free: memoryGb(free) })}
              </small>
            ) : null}
          </label>
        </div>
        <label className="bb-field">
          <span>{t('beatbox.machine.reach')}</span>
          <div className="bb-sbx-choices">
            {['none', 'isolated', 'nat'].map((mode) => (
              <button key={mode} type="button" className={`bb-choice${network === mode ? ' on' : ''}`}
                      onClick={() => setNetwork(mode)}>
                <b>{runtimeText(`beatbox.reach.${mode}`)}</b>
                <small>{runtimeText(`beatbox.reach.${mode}.what`)}</small>
              </button>
            ))}
          </div>
        </label>
        {/* Extra gap: flush against the network cards it looked like one of them. */}
        <div className="bb-sbx-toggle" style={{ marginTop: 8 }}>
          <Switch id="bb-agent" checked={agent} onChange={setAgent}
                  label={t('beatbox.machine.reach_inside')} />
          <label htmlFor="bb-agent">
            <b>{t('beatbox.machine.reach_inside')} <Info text={t('beatbox.machine.reach_inside_hint')} /></b>
          </label>
        </div>
        {failure ? <p className="bb-rose" style={{ fontSize: 12.5 }}>{failure}</p> : null}
      </div>
    </Modal>
  );
}

/* --- how a machine is made ------------------------------------------------ */

/**
 * New machine's questions, for an existing machine. Editable only while off
 * (the engine refuses otherwise), so we lock the fields instead of letting
 * people edit and get refused. The base disk is shown, never changeable.
 */
function MachineSettings({ machine, onSaved }: {
  machine: SandboxView;
  onSaved: (view: SandboxView) => void;
}) {
  const settings = bag(machine.config.settings);
  const resources = bag(machine.config.resources);
  // On a network sandbox, `network` holds the cable itself (cable.ts).
  const cable = cableOf(machine);
  const saved = {
    cpu: String(resources.cpu ?? 2),
    memory: String(Math.round(Number(resources.memory ?? 2048) / 1024 * 10) / 10),
    network: cable ? 'network' : String(settings.network ?? 'none'),
    net: cable?.sandbox ?? '',
    agent: settings.agent !== false,
    capture: settings.capture === true,
    firmware: ['bios', 'uefi'].includes(String(settings.firmware)) ? String(settings.firmware) : 'auto',
  };
  const [cpu, setCpu] = useState(saved.cpu);
  const [memory, setMemory] = useState(saved.memory);
  const [network, setNetwork] = useState(saved.network);
  const [net, setNet] = useState(saved.net);
  const [networks, setNetworks] = useState<SandboxSummary[]>([]);
  const [agent, setAgent] = useState(saved.agent);
  const [capture, setCapture] = useState(saved.capture);
  const [firmware, setFirmware] = useState(saved.firmware);
  // What "automatic" picked from the disk, shown on that option.
  const detected = String(bag(machine.state.facts).firmware ?? '');
  const [free, setFree] = useState(0);
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState<{ ok: boolean; text: string } | null>(null);

  const off = machine.state.lifecycle === 'created' || machine.state.lifecycle === 'stopped';
  useEffect(() => {
    if (!off) return;
    void call('system.probe').then((probe) => setFree(probe.memoryFreeMb)).catch(() => undefined);
  }, [off]);
  // Even while running: it still shows which network it's on.
  useEffect(() => {
    void call('sandbox.list')
      .then((rows) => setNetworks(rows.filter((row) => row.type === 'network')))
      .catch(() => undefined);
  }, [machine.id]);

  const changed = cpu !== saved.cpu || memory !== saved.memory
    || network !== saved.network || (network === 'network' && net !== saved.net) || agent !== saved.agent || capture !== saved.capture
    || firmware !== saved.firmware;
  const tooMuch = free > 0 && (Number(memory) || 0) * 1024 + 512 > free;

  async function save() {
    setBusy(true);
    setTold(null);
    try {
      // Cables touch two sandboxes (cable.ts): change last, and only if changed.
      const plain = network !== 'network' && !cable;
      let view = await call('sandbox.reconfigure', {
        id: machine.id,
        cpu: Number(cpu) || 0,
        memory: Math.round((Number(memory) || 0) * 1024),
        // No NIC, no capture: QEMU refuses that combination.
        settings: { ...(plain ? { network } : {}), agent, capture: capture && network !== 'none', firmware },
      });
      if (network === 'network') {
        if (net && net !== saved.net) view = await plug(view, net);
      } else if (cable) {
        view = await unplug(view, network);
      }
      onSaved(view);
      setTold({ ok: true, text: t('beatbox.machine.settings_saved') });
    } catch (error) {
      setTold({ ok: false, text: trouble(error) });
    }
    setBusy(false);
  }

  return (
    <div style={{ padding: 18 }}>
      <p className="bb-faint" style={{ fontSize: 11.5, margin: '0 0 12px' }}>
        {off ? t('beatbox.machine.settings_what') : t('beatbox.machine.settings_locked')}
      </p>
      <fieldset className="bb-sbx-settings" disabled={!off || busy}>
        <div className="bb-sbx-pair">
          <label className="bb-field">
            <span>{t('beatbox.machine.processors')}</span>
            <input className="bb-input" value={cpu} inputMode="decimal"
                   onChange={(e) => setCpu(e.target.value)} />
          </label>
          <label className="bb-field">
            <span>{t('beatbox.machine.memory')}</span>
            <input className="bb-input" value={memory} inputMode="decimal"
                   onChange={(e) => setMemory(e.target.value)} />
            {off && free > 0 ? (
              <small className={tooMuch ? 'bb-rose' : 'bb-faint'}>
                {t(tooMuch ? 'beatbox.machine.memory_too_much' : 'beatbox.machine.memory_free',
                   { free: String(Math.round(free / 1024 * 10) / 10) })}
              </small>
            ) : null}
          </label>
        </div>
        <label className="bb-field">
          <span>{t('beatbox.machine.reach')}</span>
          <div className="bb-sbx-choices bb-sbx-choices--4">
            {['none', 'isolated', 'nat', 'network'].map((mode) => (
              <button key={mode} type="button" className={`bb-choice${network === mode ? ' on' : ''}`}
                      onClick={() => {
                        setNetwork(mode);
                        if (mode === 'network' && !net && networks[0]) setNet(networks[0].id);
                      }}>
                <b>{runtimeText(`beatbox.reach.${mode}`)}</b>
                <small>{runtimeText(`beatbox.reach.${mode}.what`)}</small>
              </button>
            ))}
          </div>
        </label>
        {network === 'network' ? (
          networks.length ? (
            <label className="bb-field" style={{ marginTop: 8 }}>
              <span>{t('beatbox.machine.which_network')}</span>
              <Select label={t('beatbox.machine.which_network')} value={net} onChange={setNet}
                      options={networks.map((row) => ({ value: row.id, label: row.name }))} />
              {cable && net === saved.net ? (
                <small className="bb-faint">{t('beatbox.machine.cabled_as', { address: cable.address })}</small>
              ) : null}
            </label>
          ) : (
            <p className="bb-faint" style={{ fontSize: 12, margin: '8px 0 0' }}>
              {t('beatbox.machine.no_networks')}{' '}
              <a href={href('network/overview', { new: '1' })}>{t('beatbox.network.new')}</a>
            </p>
          )
        ) : null}
        <div className="bb-sbx-toggle" style={{ marginTop: 8 }}>
          <Switch id={`bb-agent-${machine.id}`} checked={agent} onChange={setAgent}
                  label={t('beatbox.machine.reach_inside')} />
          <label htmlFor={`bb-agent-${machine.id}`}>
            <b>{t('beatbox.machine.reach_inside')} <Info text={t('beatbox.machine.reach_inside_hint')} /></b>
          </label>
        </div>
        <details className="bb-flow-more" style={{ marginTop: 8 }} open={saved.firmware !== 'auto'}>
        <summary>{t('beatbox.machine.advanced')}</summary>
        <label className="bb-field">
          <span>{t('beatbox.machine.firmware')}</span>
          <div className="bb-sbx-choices">
            {['auto', 'bios', 'uefi'].map((mode) => (
              <button key={mode} type="button" className={`bb-choice${firmware === mode ? ' on' : ''}`}
                      onClick={() => setFirmware(mode)}>
                <b>{runtimeText(`beatbox.machine.firmware.${mode}`)}</b>
                <small>
                  {mode === 'auto' && detected
                    ? t('beatbox.machine.firmware.auto_now', { firmware: runtimeText(`beatbox.machine.firmware.${detected}`) })
                    : runtimeText(`beatbox.machine.firmware.${mode}.what`)}
                </small>
              </button>
            ))}
          </div>
        </label>
        </details>
        {TRAFFIC_ON_SCREEN ? (
        <div className="bb-sbx-toggle" style={{ marginTop: 8, ...(network === 'none' ? { opacity: 0.5, pointerEvents: 'none' } : {}) }}>
          <Switch id={`bb-capture-${machine.id}`} checked={capture && network !== 'none'} onChange={setCapture}
                  label={t('beatbox.machine.capture')} />
          <label htmlFor={`bb-capture-${machine.id}`}>
            <b>{t('beatbox.machine.capture')} <Info text={t('beatbox.machine.capture_hint')} /></b>
            {network === 'none' ? <small className="bb-faint">{t('beatbox.machine.capture_needs_card')}</small> : null}
          </label>
        </div>
        ) : null}
      </fieldset>
      <div className="bb-sbx-settings-foot">
        <span className="bb-faint" role="status" style={{ fontSize: 12 }}>{told?.ok ? told.text : ''}</span>
        <Button size="sm" variant="primary" icon="check"
                disabled={!off || busy || !changed || (network === 'network' && !net)}
                onClick={() => { void save(); }}>
          {t('beatbox.machine.settings_save')}
        </Button>
      </div>
      {told && !told.ok ? <p className="bb-sbx-no" style={{ fontSize: 12 }}>{told.text}</p> : null}
    </div>
  );
}

/* --- what it tried to say -------------------------------------------------- */

/** Traffic recording works end to end but is hidden for now. Flip to show the card and switch. */
const TRAFFIC_ON_SCREEN = false;

/** Re-reads and parses a capture file each time, so slower than WATCH. */
const TRAFFIC_EVERY = 10_000;
/** Past this many destinations, just count the rest. */
const TRAFFIC_ROWS = 12;

/** Well-known ports by name. */
const SERVICES: Record<number, string> = {
  53: 'dns', 80: 'web', 8080: 'web', 443: 'secure_web', 22: 'ssh', 21: 'files',
  25: 'mail', 465: 'mail', 587: 'mail', 993: 'mail', 123: 'time', 3389: 'remote_desktop',
  445: 'windows_files',
};

function kilobytes(bytes: number): string {
  return bytes >= 1024 * 1024
    ? `${Math.round(bytes / 1024 / 1024 * 10) / 10} MB`
    : `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

/**
 * Who the machine tried to talk to, for people who've never read a capture.
 * From `network.traffic`: names looked up and outside places it tried,
 * named by port. Local chatter is one line. When isolated, these are
 * attempts that never got out, and the card says so.
 */
function Traffic({ machine }: { machine: SandboxView }) {
  const [seen, setSeen] = useState<Record<string, unknown> | null>(null);
  const running = machine.state.lifecycle === 'running';
  const network = String(bag(machine.config.settings).network ?? 'none');

  const look = useCallback(async () => {
    try {
      const outcome = await call('sandbox.act', { id: machine.id, capability: 'network.traffic', arguments: {} });
      if (outcome.ok) setSeen(bag(outcome.data));
    } catch { /* keep what we had */ }
  }, [machine.id]);

  useEffect(() => {
    void look();
    if (!running) return;
    const beat = window.setInterval(() => void look(), TRAFFIC_EVERY);
    return () => window.clearInterval(beat);
  }, [look, running, machine.config]);

  const outside = Array.isArray(seen?.outside) ? seen.outside.map(bag) : [];
  const names = Array.isArray(seen?.names) ? seen.names.map(String) : [];
  const local = Number(seen?.local ?? 0);
  const file = String(seen?.file ?? '');
  const size = Number(seen?.size ?? 0);

  return (
    <Card title={<><Icon name="network" size="sm" />{t('beatbox.traffic.title')}</>}
          note={file && size ? kilobytes(size) : undefined} info={t('beatbox.traffic.what')}>
      {seen === null ? null
        : seen.recording !== true ? <p className="bb-traffic-say">{t('beatbox.traffic.off')}</p>
        : !file ? <p className="bb-traffic-say">{t('beatbox.traffic.waiting')}</p>
        : !outside.length && !names.length ? <p className="bb-traffic-say">{t('beatbox.traffic.quiet')}</p>
        : (
          <>
            {names.length ? (
              <>
                <p className="bb-traffic-head">{t('beatbox.traffic.names')}</p>
                <div className="bb-traffic-names">
                  {names.map((name) => <span key={name}>{name}</span>)}
                </div>
              </>
            ) : null}
            {outside.length ? (
              <>
                <p className="bb-traffic-head">{t('beatbox.traffic.tried')}</p>
                {outside.slice(0, TRAFFIC_ROWS).map((row) => {
                  const port = Number(row.port ?? 0);
                  const service = SERVICES[port];
                  return (
                    <div key={`${row.protocol}${row.address}${port}`} className="bb-traffic-row">
                      <span>{String(row.address)}</span>
                      <small>
                        {service ? runtimeText(`beatbox.traffic.service.${service}`)
                          : port ? t('beatbox.traffic.port', { port }) : String(row.protocol)}
                      </small>
                      <small>{t('beatbox.traffic.packets', { count: Number(row.packets ?? 0) })}</small>
                    </div>
                  );
                })}
                {outside.length > TRAFFIC_ROWS ? (
                  <small className="bb-faint">{t('beatbox.traffic.more', { count: outside.length - TRAFFIC_ROWS })}</small>
                ) : null}
              </>
            ) : null}
            {network === 'isolated' ? (
              <p className="bb-traffic-say">{t('beatbox.traffic.contained')}</p>
            ) : network === 'nat' ? (
              <p className="bb-traffic-say bb-traffic-say--open">{t('beatbox.traffic.open')}</p>
            ) : null}
          </>
        )}
      {seen?.recording === true && file && local > 0 ? (
        <small className="bb-faint" style={{ display: 'block', marginTop: 8 }}>
          {t('beatbox.traffic.local', { count: local })}
        </small>
      ) : null}
      {file ? (
        <small className="bb-faint" style={{ display: 'block', marginTop: 8 }}>
          {t('beatbox.traffic.file')} <span className="bb-mono" style={{ userSelect: 'text' }}>{file}</span>
        </small>
      ) : null}
    </Card>
  );
}

/** A fresh qcow2 is ~200 KB; under 1 MB counts as empty. */
const EMPTY_BYTES = 1024 * 1024;

/** Which app a foreign disk format comes from. */
const FOREIGN: Record<string, string> = { vdi: 'VirtualBox', vmdk: 'VMware / VirtualBox', vpc: 'Hyper-V / Virtual PC', vhdx: 'Hyper-V' };

/* --- the screen ----------------------------------------------------------- */

/** Transitions that get a "done" line in the action bar. */
const TOLD = ['start', 'stop', 'pause', 'resume', 'reset'];

export function Sandbox() {
  const [types, setTypes] = useState<SandboxTypeView[]>([]);
  const [list, setList] = useState<SandboxSummary[]>([]);
  const [chosen, setChosen] = useState('');
  const [machine, setMachine] = useState<SandboxView | null>(null);
  const [making, setMaking] = useState(false);
  // The rail's "New machine" lands here with new=1 to open the dialog.
  const params = useRouteParams();
  useEffect(() => {
    if (params.get('new') !== '1') return;
    setMaking(true);
    navigate('host/machine');
  }, [params]);
  const [asking, setAsking] = useState<string>('');
  const [editing, setEditing] = useState(false);
  // Last result, shown in the action bar where people are looking. Errors
  // shown elsewhere got missed.
  const [told, setTold] = useState<{ ok: boolean; text: string; at: string } | null>(null);
  const note = (ok: boolean, text: string) => setTold({ ok, text, at: clock(new Date()) });
  const [totalMb, setTotalMb] = useState(0);
  useEffect(() => {
    void call('system.probe').then((probe) => setTotalMb(probe.memoryTotalMb)).catch(() => undefined);
  }, []);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  const [networks, setNetworks] = useState<SandboxSummary[]>([]);

  const reload = useCallback(async (id: string, observe = false) => {
    const all = await call('sandbox.list');
    setNetworks(all.filter((row) => row.type === 'network'));
    const rows = all.filter((row) => row.type === 'host');
    setList(rows);
    const want = id || rememberedMachine();
    const pick = rows.some((row) => row.id === want) ? want : (rows[0]?.id ?? '');
    setChosen(pick);
    if (pick) rememberMachine(pick);
    if (!pick) { setMachine(null); return; }
    try {
      setMachine(observe ? await call('sandbox.refresh', { id: pick }) : await call('sandbox.view', { id: pick }));
    } catch {
      setMachine(await call('sandbox.view', { id: pick }));
    }
  }, []);
  // Install finished: open the new machine.
  const onInstalled = useCallback((id: string) => { void reload(id); }, [reload]);

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const kinds = await call('sandbox.types');
        if (!live) return;
        // Networks and servers have their own pages and forms.
        setTypes(kinds.filter((kind) => kind.name !== 'network' && kind.name !== 'server'));
        await reload('', true);
      } catch (error) {
        if (live) setFailure(trouble(error));
      }
    })();
    return () => { live = false; };
  }, [reload]);

  useEffect(() => {
    if (machine?.state.lifecycle !== 'running') return;
    const id = machine.id;
    const beat = window.setInterval(() => {
      void call('sandbox.refresh', { id })
        .then((fresh) => setMachine((was) => (was?.id === fresh.id ? fresh : was)))
        .catch(() => undefined);
    }, WATCH);
    return () => window.clearInterval(beat);
  }, [machine?.id, machine?.state.lifecycle]);

  // Machines change on their own (shut down from inside, lost), so redraw on events.
  useEffect(() => on('sandbox', (event) => {
    if (event.name.startsWith('sandbox.')) {
      setChosen((current) => { void reload(current); return current; });
    }
  }), [reload]);

  // The rail's switcher picks the machine; we just follow.
  const wanted = useChosenId();
  useEffect(() => {
    if (wanted && wanted !== chosen && list.some((row) => row.id === wanted)) void choose(wanted);
  }, [wanted, list]);

  async function choose(id: string) {
    setChosen(id);
    rememberMachine(id);
    setTold(null);
    try {
      setMachine(await call('sandbox.refresh', { id }));
    } catch (error) {
      setFailure(trouble(error));
    }
  }

  async function move(transition: string) {
    if (!machine) return;
    setAsking('');
    setFailure(null);
    setBusy(true);
    try {
      const fresh = await call('sandbox.transition', { id: machine.id, transition });
      setMachine(fresh);
      if (TOLD.includes(transition)) note(true, runtimeText(`beatbox.machine.told.${transition}`));
      if (transition === 'start') await openDisplay(fresh);
    } catch (error) {
      note(false, trouble(error));
    }
    setBusy(false);
    await reload(machine.id);
  }

  async function openDisplay(view: SandboxView) {
    const display = bag(view.state.facts).display;
    const access = bag(display);
    const url = String(access.url ?? '');
    const password = String(access.password ?? '');
    if (!url || !password) return;
    await call('host.display', { id: view.id, name: view.name, url, password });
  }

  /** Run a capability; `done` is the success message. */
  async function ask(capability: string, args: Record<string, unknown>, done: string) {
    if (!machine) return;
    setBusy(true);
    try {
      const outcome = await call('sandbox.act', { id: machine.id, capability, arguments: args });
      note(outcome.ok, outcome.ok ? done : why(outcome.reason));
    } catch (error) {
      note(false, trouble(error));
    }
    setBusy(false);
    try {
      setMachine(await call('sandbox.refresh', { id: machine.id }));
    } catch { /* keep what we had */ }
  }

  const facts = bag(machine?.state.facts);
  const settings = bag(machine?.config.settings);
  const resources = bag(machine?.config.resources);
  const can = (name: string) => machine?.capabilities.includes(name) ?? false;
  const may = (name: string) => machine?.transitions.includes(name) ?? false;
  const running = machine?.state.lifecycle === 'running';
  const paused = machine?.state.lifecycle === 'paused';
  // Settings change only while it is off (lifecycle.reconfigure).
  const off = machine?.state.lifecycle === 'created' || machine?.state.lifecycle === 'stopped';
  const viewable = Boolean(bag(facts.display).url);
  const picture = usePicture(machine?.id ?? '', can('display.picture'), machine?.state.lifecycle ?? '');
  const memoryMb = Number(resources.memory ?? 0);
  const cpus = Number(resources.cpu ?? 0);
  const diskName = String(facts.disk ?? '').split(/[\\/]/).pop() ?? '';
  const snapshots = Array.isArray(facts.snapshots) ? facts.snapshots as string[] : [];
  const family = String(facts.family ?? bag(settings.os).family ?? '');
  const cable = cableOf(machine);
  const diskMissing = facts.disk_available === false;
  async function checkDisk() {
    if (!machine) return;
    setBusy(true);
    setFailure(null);
    try {
      await reload(machine.id, true);
    } catch (error) {
      setFailure(trouble(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <InstallUnderWay onMade={onInstalled} />
      {failure && !diskMissing ? <p className="bb-rose" role="alert" style={{ fontSize: 12.5 }}>{failure}</p> : null}

      {!list.length ? (
        <div className="bb-pending" style={{ minHeight: 320 }}>
          <CubeWire size={92} tone="violet" opacity={0.42} />
          <p>{t('beatbox.machine.none')}</p>
          <Button variant="primary" trailingIcon="arrow" disabled={!types.length}
                  onClick={() => setMaking(true)}>
            {t('beatbox.machine.new')}
          </Button>
        </div>
      ) : null}

      {machine ? (
        <>
        <div className="bb-mach-head">
          <Cube size={36} tone="violet" />
          <div className="bb-mach-name">
            <div className="bb-pagehead-title">
              <h1 className="bb-t-title bb-net-title" style={{ margin: 0 }}>
                <RenameTitle name={machine.name}
                             onRename={async (name) => { await call('sandbox.rename', { id: machine.id, name }); await reload(machine.id); }} />
              </h1>
              <Info text={runtimeText(`beatbox.plain.boundary.${machine.isolation}`)} />
            </div>
          </div>
          <span className="bb-mach-state" role="status"
                style={{ color: toneInk(diskMissing ? 'amber' : TONES[machine.state.lifecycle] ?? 'neutral') }}>
            <i />
            {diskMissing ? t('beatbox.machine.disk_missing.badge') : runtimeText(`beatbox.plain.state.${machine.state.lifecycle}`)}
            {running && !diskMissing ? <span className="bb-faint">{t('beatbox.machine.since', { time: clock(new Date(machine.state.since)) })}</span> : null}
          </span>
        </div>

        {diskMissing ? (
          <div className="bb-disk-warning" role="status">
            <Icon name="usb" size="md" />
            <div className="bb-disk-warning-copy">
              <strong>{t('beatbox.machine.disk_missing.title')}</strong>
              <p>{t('beatbox.machine.disk_missing.help')}</p>
              <details>
                <summary>{t('beatbox.machine.disk_missing.details')}</summary>
                <p>{t('beatbox.machine.disk_missing.location')}</p>
                <code>{String(facts.disk ?? settings.disk ?? '')}</code>
              </details>
              {failure ? <p role="alert">{failure}</p> : null}
            </div>
            <Button size="sm" icon="refresh" disabled={busy} onClick={() => void checkDisk()}>
              {t('beatbox.machine.disk_missing.check')}
            </Button>
          </div>
        ) : null}
        {running && facts.saved_state_lost === true ? (
          <p className="bb-amber" role="status" style={{ fontSize: 12.5, margin: '0 0 12px' }}>{t('beatbox.machine.saved_state_lost')}</p>
        ) : null}
        {facts.bootable === false ? <p className="bb-amber" style={{ fontSize: 12.5, margin: '0 0 12px' }}>{t('beatbox.machine.cannot_start')}</p> : null}

        <div className="bb-mach-body">
          <div className="bb-mach-main">
            {/* Click opens/raises the display window. Save points run along the bottom. */}
            <div className={`bb-mach-stage bb-mach-stage--${running ? 'on' : paused ? 'held' : 'off'}`}>
              {/* Screenshot: refreshed while running, last one when off. */}
              {picture && running && viewable ? (
                <button type="button" className="bb-mach-screen bb-mach-screen--shot" disabled={busy}
                        onClick={() => void openDisplay(machine)}>
                  <img src={picture.src} alt={t('beatbox.machine.stage.picture', { name: machine.name })} />
                  <span className="bb-mach-caption"><Icon name="monitor" size="xs" />{t('beatbox.machine.open_display')}</span>
                </button>
              ) : (
              <div className={`bb-mach-screen${picture ? ' bb-mach-screen--shot bb-mach-screen--left' : ''}`}>
                {picture ? <img src={picture.src} alt={t('beatbox.machine.stage.picture', { name: machine.name })} /> : null}
                <div className="bb-mach-say">
                <Icon name={paused ? 'freeze' : 'monitor'} size="lg" />
                {running ? (
                  <>
                    <p>{t(viewable ? 'beatbox.machine.stage.running' : 'beatbox.machine.stage.headless', { name: machine.name })}</p>
                    {viewable ? (
                      <Button disabled={busy} onClick={() => void openDisplay(machine)}>{t('beatbox.machine.open_display')}</Button>
                    ) : null}
                  </>
                ) : paused ? (
                  <p>{t('beatbox.machine.stage.paused')}</p>
                ) : (
                  <p>{diskMissing ? t('beatbox.machine.stage.off') : facts.saved_state === true
                    ? t('beatbox.plain.why.state_saved') : settledBecause(machine.state)}</p>
                )}
                {picture && !running ? (
                  <p className="bb-faint">{t('beatbox.machine.stage.as_it_was', { time: clock(picture.taken) })}</p>
                ) : null}
                </div>
              </div>
              )}
              {can('snapshot.create') ? (
                <SavePoints names={snapshots} running={running} busy={busy}
                            onBack={(name) => void ask('snapshot.restore', { name }, t('beatbox.machine.told.restored', { name }))}
                            onRemove={(name) => void ask('snapshot.delete', { name }, t('beatbox.machine.told.removed', { name }))} />
              ) : null}
            </div>

            <div className="bb-mach-acts">
              {['start', 'stop', 'pause', 'resume'].filter(may).map((name) => (
                <button key={name} type="button" disabled={busy}
                        className={`bb-btn bb-btn--${name === 'start' || name === 'stop' ? 'primary' : 'ghost'} bb-mach-act--${name}`}
                        onClick={() => (name === 'stop' && can('snapshot.save_state') ? setAsking('stop') : void move(name))}>
                  <Icon name={ACTS[name] ?? 'arrow'} size="sm" />
                  {runtimeText(`beatbox.plain.do.${name}`)}
                </button>
              ))}
              {can('snapshot.create') && running ? (
                <SaveNow busy={busy}
                         onSave={(name) => void ask('snapshot.create', { name }, t('beatbox.machine.told.saved', { name }))} />
              ) : null}
              <span className="bb-spacer" />
              {told ? (
                <span className={`bb-mach-told${told.ok ? '' : ' bb-mach-told--no'}`} role="status">
                  <time className="bb-mono">{told.at}</time><i />{told.text}
                </span>
              ) : null}
              {['reset', 'destroy'].filter(may).map((name) => {
                const word = runtimeText(`beatbox.plain.do.${name}`);
                return (
                  <button key={name} type="button" className={`bb-act bb-act--${name === 'destroy' ? 'danger' : 'plain'}`}
                          title={word} aria-label={word} disabled={busy}
                          onClick={() => (GRAVE.includes(name) ? setAsking(name) : void move(name))}>
                    <Icon name={ACTS[name] ?? 'arrow'} size="sm" />
                  </button>
                );
              })}
            </div>
          </div>

          <aside className="bb-mach-facts" aria-labelledby="bb-mach-this">
            <div className="bb-mach-facts-head">
              <h2 id="bb-mach-this">{t('beatbox.machine.this')}</h2>
              {off && can('lifecycle.reconfigure') ? (
                <button type="button" className="bb-mach-change" onClick={() => setEditing(true)}>{t('beatbox.machine.change')}</button>
              ) : null}
            </div>
            <dl>
              {family === 'windows' || family === 'linux' ? (
                <div><dt>{t('beatbox.machine.row.system')}</dt><dd>{t(`beatbox.machine.family.${family}`)}</dd></div>
              ) : null}
              <div>
                <dt>{t('beatbox.machine.row.memory')}</dt>
                <dd>
                  <Share part={memoryMb} whole={totalMb}
                         label={t('beatbox.machine.row.memory_of', { gb: gigs(memoryMb), total: gigs(totalMb) })} />
                  {t('beatbox.machine.row.gb', { gb: gigs(memoryMb) })}
                </dd>
              </div>
              <div>
                <dt>{t('beatbox.machine.row.processors')}</dt>
                <dd>
                  <Share part={cpus} whole={navigator.hardwareConcurrency || 0}
                         label={t('beatbox.machine.row.processors_of', { count: cpus, total: navigator.hardwareConcurrency || 0 })} />
                  {String(resources.cpu ?? '?')}
                </dd>
              </div>
              <div>
                <dt>{t('beatbox.machine.row.reaches')}</dt>
                <dd>{cable ? (
                  <a href={href('network/overview')} onClick={() => rememberNetwork(cable.sandbox)}>
                    <Icon name="network" size="xs" />
                    {networks.find((n) => n.id === cable.sandbox)?.name ?? '?'}
                    <span className="bb-mono bb-faint">{cable.address}</span>
                  </a>
                ) : runtimeText(`beatbox.machine.fact.reach.${String(settings.network ?? 'none')}`)}</dd>
              </div>
              {facts.disk ? (
                <div>
                  <dt>{t('beatbox.machine.row.disk')}</dt>
                  <dd>{facts.overlay ? t('beatbox.machine.row.kept_apart', { name: diskName }) : diskName}</dd>
                </div>
              ) : null}
              {facts.firmware === 'bios' || facts.firmware === 'uefi' ? (
                <div><dt>{t('beatbox.machine.row.firmware')}</dt><dd>{runtimeText(`beatbox.machine.firmware.${facts.firmware}`)}</dd></div>
              ) : null}
              {facts.accelerated === true || facts.accelerated === false ? (
                <div>
                  <dt>{t('beatbox.machine.row.speed')}</dt>
                  <dd className={facts.accelerated ? '' : 'bb-amber'}>
                    <i className={`bb-mach-dot${facts.accelerated ? ' bb-mach-dot--go' : ''}`} />
                    {t(facts.accelerated ? 'beatbox.machine.row.accelerated' : 'beatbox.machine.row.slow')}
                  </dd>
                </div>
              ) : null}
            </dl>
            <Happened id={machine.id} />
            {off ? null : <p className="bb-faint bb-mach-facts-foot">{t('beatbox.machine.settings_locked')}</p>}
          </aside>
        </div>

        {TRAFFIC_ON_SCREEN && can('network.traffic') ? <Traffic key={machine.id} machine={machine} /> : null}

        {editing ? (
          <Modal title={t('beatbox.machine.settings')} icon="settings" width={620} onClose={() => setEditing(false)}>
            <MachineSettings key={machine.id} machine={machine}
                             onSaved={(view) => { setMachine(view); setEditing(false); note(true, t('beatbox.machine.settings_saved')); }} />
          </Modal>
        ) : null}
        </>
      ) : null}

      {making ? (
        <NewMachine types={types} onClose={() => setMaking(false)}
                    onMade={(made) => { setMaking(false); void reload(made.id); }} />
      ) : null}

      {asking === 'stop' ? (
        <Modal title={t('beatbox.machine.stop_how')} icon="stop" width={560} onClose={() => setAsking('')}
               footer={<>
                 <span style={{ flex: 1 }} />
                 <Button onClick={() => setAsking('')}>{t('beatbox.closing.cancel')}</Button>
               </>}>
          <div className="bb-sbx-form">
            <div className="bb-start-choices">
              <button type="button" className="bb-start-choice" autoFocus
                      onClick={() => { setAsking(''); void ask('snapshot.save_state', {}, t('beatbox.machine.told.state_saved')); }}>
                <Icon name="save" size="md" />
                <b>{t('beatbox.machine.save_state')}</b>
                <small>{t('beatbox.machine.save_state_what')}</small>
              </button>
              <button type="button" className="bb-start-choice"
                      onClick={() => { setAsking(''); void move('stop'); }}>
                <Icon name="stop" size="md" />
                <b>{t('beatbox.machine.stop_fully')}</b>
                <small>{t('beatbox.machine.stop_fully_what')}</small>
              </button>
            </div>
          </div>
        </Modal>
      ) : asking ? (
        <Modal title={runtimeText(`beatbox.plain.sure.${asking}`)} icon="alert" onClose={() => setAsking('')}
               footer={
                 <>
                   <span style={{ flex: 1 }} />
                   <Button onClick={() => setAsking('')}>{t('beatbox.closing.cancel')}</Button>
                   <Button variant="danger" autofocus onClick={() => void move(asking)}>
                     {runtimeText(`beatbox.plain.do.${asking}`)}
                   </Button>
                 </>
               }>
          <p className="bb-closing" style={{ margin: 0 }}>
            {runtimeText(`beatbox.plain.sure.${asking}.what`, { name: machine?.name ?? '' })}
          </p>
        </Modal>
      ) : null}
    </>
  );
}
