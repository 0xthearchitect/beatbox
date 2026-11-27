import { Fragment, useCallback, useEffect, useState, type ReactElement } from 'react';
import { Button, Icon, Modal } from '../components';
import { call } from '../bridge/client';
import { remember } from '../chosen';
import { fault, t } from '../i18n';
import { navigate } from '../router';
import { clock } from './MachineParts';
import { act, bag, rows, text, useOthers, type Server } from './Server';

/**
 * Getting a service's programs onto its machine. Looking only reads; the
 * install runs once a person presses Install, detached in the guest, and
 * this follows it. Installing by hand is always there too.
 */

interface Plan {
  off?: boolean;
  system: string;
  apt: boolean;
  busy: boolean;
  reach: boolean;
  free_kb?: number;
  room_kb?: number;
  get_kb?: number;
  have: string[];
  install: { program: string; package: string }[];
  yourself: string[];
  absent: string[];
}

interface Progress { state: string; installed: Record<string, string>; lines: string[] }

type View = 'look' | 'working' | 'done' | 'failed' | 'manual';

/** kB -> "2.1 MB". */
const size = (kb: number) => (kb >= 1024 * 1024 ? `${(kb / 1024 / 1024).toFixed(1)} GB`
  : kb >= 1024 ? `${(kb / 1024).toFixed(1)} MB` : `${kb} kB`);
const list = (items: string[]) => [...new Set(items)].join(', ');
/** apt says it is working on the files once it unpacks the first. */
const unpacking = (lines: string[]) => lines.some((line) => /^(Unpacking|Setting up|Selecting) /.test(line));

export function Prepare({ srv, service, manual = false, onClose, onChecked }: {
  srv: Server; service: string; manual?: boolean; onClose: () => void; onChecked: () => Promise<void>;
}) {
  const hosts = useOthers('host');
  const hostId = text(bag(srv.server?.config.uses).host);
  const machine = hosts.find((row) => row.id === hostId)?.name ?? '';
  const [view, setView] = useState<View>(manual ? 'manual' : 'look');
  const [plan, setPlan] = useState<Plan | null>(null);
  const [progress, setProgress] = useState<Progress | null>(null);
  const [checkedAt, setCheckedAt] = useState<Date | null>(null);
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  const [copied, setCopied] = useState(false);

  const look = useCallback(async () => {
    setTold('');
    try {
      const said = await act(srv.id, 'server.prepare', { service });
      const next: Plan = {
        off: said.off === true, system: text(said.system), apt: said.apt === true, busy: said.busy === true, reach: said.reach === true,
        free_kb: typeof said.free_kb === 'number' ? said.free_kb : undefined,
        room_kb: typeof said.room_kb === 'number' ? said.room_kb : undefined,
        get_kb: typeof said.get_kb === 'number' ? said.get_kb : undefined,
        have: (said.have as string[] | undefined) ?? [],
        install: rows(said.install).map((row) => ({ program: text(row.program), package: text(row.package) })),
        yourself: (said.yourself as string[] | undefined) ?? [],
        absent: (said.absent as string[] | undefined) ?? [],
      };
      setPlan(next);
      setCheckedAt(new Date());
      return next;
    } catch (error) {
      setTold(fault(error));
      return null;
    }
  }, [srv.id, service]);

  const follow = useCallback(async () => {
    try {
      const said = await act(srv.id, 'server.preparing');
      const now: Progress = { state: text(said.state), installed: bag(said.installed) as Record<string, string>, lines: (said.lines as string[] | undefined) ?? [] };
      setProgress(now);
      return now;
    } catch { return null; }
  }, [srv.id]);

  // An install already under way, from this page or another: follow it.
  useEffect(() => {
    void (async () => {
      const now = await follow();
      if (now?.state === 'running') setView('working');
      await look();
    })();
  }, [follow, look]);

  useEffect(() => {
    if (view !== 'working') return undefined;
    const timer = window.setInterval(() => {
      void follow().then(async (now) => {
        if (!now || now.state === 'running') return;
        await look();
        await onChecked();
        setView(now.state === 'done' ? 'done' : 'failed');
      });
    }, 1500);
    return () => window.clearInterval(timer);
  }, [view, follow, look, onChecked]);

  async function install() {
    setBusy(true);
    setTold('');
    try {
      await act(srv.id, 'server.prepare', { service, install: true });
      setProgress({ state: 'running', installed: {}, lines: [] });
      setView('working');
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }

  async function turnOn() {
    setBusy(true);
    try {
      await call('sandbox.transition', { id: hostId, transition: 'start' });
      await look();
      await onChecked();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }

  async function putToWork() {
    setBusy(true);
    try {
      await act(srv.id, 'service.start', { service });
      await srv.load();
      onClose();
    } catch (error) {
      setTold(fault(error));
    }
    setBusy(false);
  }

  async function checkAgain() {
    setBusy(true);
    await look();
    await onChecked();
    setBusy(false);
  }

  const openCommands = () => { remember('host', hostId); navigate('host/commands'); };
  const openMachine = () => { remember('host', hostId); navigate('host/machine'); };

  const packages = list(plan?.install.map((each) => each.package) ?? []);
  const missing = [...(plan?.install.map((each) => each.program) ?? []), ...(plan?.yourself ?? [])];
  const close = <Button onClick={onClose}>{t('beatbox.files.cancel')}</Button>;
  const myself = <Button onClick={() => setView('manual')}>{t('beatbox.srv.prep.myself')}</Button>;

  /** A no: what, why, and the buttons that get past it. */
  const no = (tone: 'amber' | 'grey', title: string, why: string, ...actions: ReactElement[]) => (
    <>
      <div className={`bb-prep-box is-${tone}`}><b>{title}</b><span>{why}</span></div>
      <div className="bb-prep-acts">{actions.map((each, index) => <Fragment key={index}>{each}</Fragment>)}</div>
    </>
  );

  let body: ReactElement;
  if (view === 'manual') {
    const all = Boolean(plan && !plan.off && plan.install.length + plan.yourself.length === 0);
    // Only what Debian names: a program with no package has no line to copy.
    const line = plan?.apt && plan.install.length ? `sudo apt install -y ${packages}` : '';
    const shown = missing.length ? list(missing) : list(plan?.have ?? []);
    body = (
      <>
        {line ? (
          <div>
            <p className="bb-prep-label">{t('beatbox.srv.prep.manual_where', { machine })}</p>
            <div className="bb-prep-line">
              <code className="bb-mono">{line}</code>
              <button type="button" className="bb-srv-copy" aria-label={t('beatbox.srv.prep.copy')} title={t('beatbox.srv.prep.copy')}
                      onClick={() => { void navigator.clipboard?.writeText(line); setCopied(true); window.setTimeout(() => setCopied(false), 1500); }}>
                <Icon name={copied ? 'check' : 'copy'} size="sm" />
              </button>
            </div>
          </div>
        ) : <p className="bb-prep-quiet">{t('beatbox.srv.prep.manual_own', { programs: list(missing), machine })}</p>}
        {plan && !plan.off ? (
          <div className={`bb-prep-status${all ? ' is-ok' : ''}`}>
            <Icon name={all ? 'check' : 'close'} size="sm" />
            <span>{t(all ? 'beatbox.srv.prep.there' : 'beatbox.srv.prep.not_yet', { programs: shown })}</span>
            {checkedAt ? <small>{t('beatbox.srv.helpers.checked', { time: clock(checkedAt) })}</small> : null}
          </div>
        ) : null}
        <div className="bb-prep-acts">
          {all ? (
            <Button variant="primary" icon="play" disabled={busy} onClick={() => void putToWork()}>{t('beatbox.srv.do.start')}</Button>
          ) : (
            <>
              <Button icon="terminal" onClick={openCommands}>{t('beatbox.srv.prep.open_commands')}</Button>
              <Button variant="primary" icon="refresh" disabled={busy} onClick={() => void checkAgain()}>{t('beatbox.srv.helpers.check_again')}</Button>
            </>
          )}
        </div>
      </>
    );
  } else if (view === 'working' || view === 'failed') {
    const lines = progress?.lines ?? [];
    const step = view === 'failed' ? -1 : unpacking(lines) ? 1 : 0;
    const steps = [t('beatbox.srv.prep.step.download', { packages }), t('beatbox.srv.prep.step.install'), t('beatbox.srv.prep.step.check')];
    const why = [...lines].reverse().find((line) => line.startsWith('E:')) ?? t('beatbox.srv.prep.failed_why');
    body = (
      <>
        {view === 'failed' ? (
          <div className="bb-prep-box is-amber"><b>{t('beatbox.srv.prep.failed', { packages })}</b><span>{why.replace(/^E:\s*/, '')}</span></div>
        ) : (
          <>
            <ol className="bb-prep-steps">
              {steps.map((words, index) => (
                <li key={words} className={index < step ? 'is-done' : index === step ? 'is-now' : ''}>
                  <span className="bb-prep-mark" aria-hidden="true">{index < step ? <Icon name="check" size="sm" /> : <i />}</span>{words}
                </li>
              ))}
            </ol>
            <div className="bb-prep-bar"><i style={{ width: `${((step + 1) / (steps.length + 1)) * 100}%` }} /></div>
          </>
        )}
        <details className="bb-srv-more" open={view === 'failed'}>
          <summary>{t('beatbox.srv.prep.log')}</summary>
          <pre className="bb-prep-log bb-mono">{lines.join('\n')}</pre>
        </details>
        <div className="bb-prep-acts">
          {view === 'failed' ? (
            <>
              {close}
              <Button variant="primary" icon="refresh" disabled={busy} onClick={() => { setView('look'); void look(); }}>{t('beatbox.srv.do.retry')}</Button>
            </>
          ) : (
            <>
              <small className="bb-faint">{t('beatbox.srv.prep.carries_on')}</small>
              <Button onClick={onClose}>{t('beatbox.srv.lib.close')}</Button>
            </>
          )}
        </div>
      </>
    );
  } else if (view === 'done') {
    const installed = Object.entries(progress?.installed ?? {});
    const still = plan ? [...plan.install.map((each) => each.program), ...plan.yourself] : [];
    body = (
      <>
        {installed.map(([name, version]) => (
          <div key={name} className="bb-prep-box is-ok bb-prep-row">
            <Icon name="check" size="sm" /><b>{t('beatbox.srv.prep.installed', { package: name })}</b><span className="bb-faint">{version}</span>
          </div>
        ))}
        <div className={`bb-prep-status${still.length ? '' : ' is-ok'}`}>
          <Icon name={still.length ? 'close' : 'check'} size="sm" />
          <span>{still.length ? t('beatbox.srv.prep.still', { programs: list(still) }) : t('beatbox.srv.prep.ready', { service })}</span>
        </div>
        <div className="bb-prep-acts">
          <Button onClick={onClose}>{t('beatbox.srv.lib.close')}</Button>
          {still.length ? myself
            : <Button variant="primary" icon="play" disabled={busy} onClick={() => void putToWork()}>{t('beatbox.srv.do.start')}</Button>}
        </div>
      </>
    );
  } else if (!plan) {
    body = <p className="bb-prep-quiet">{told || t('beatbox.srv.prep.looking', { machine })}</p>;
  } else if (plan.off) {
    body = no('grey', t('beatbox.srv.prep.off', { machine }), t('beatbox.srv.prep.off_why'),
      close, <Button key="on" variant="primary" icon="play" disabled={busy} onClick={() => void turnOn()}>{t('beatbox.srv.helpers.turn_on')}</Button>);
  } else if (!plan.apt) {
    body = no('grey', t('beatbox.srv.prep.no_system'),
      t('beatbox.srv.prep.no_system_why', { machine, system: plan.system || '?', programs: list(missing) }), close, myself);
  } else if (!missing.length) {
    body = no('grey', t('beatbox.srv.prep.nothing'), t('beatbox.srv.prep.nothing_why', { service, machine }),
      <Button key="ok" variant="primary" onClick={onClose}>{t('beatbox.srv.lib.close')}</Button>);
  } else if (!plan.install.length) {
    body = no('grey', t('beatbox.srv.prep.yourself', { programs: list(plan.yourself) }), t('beatbox.srv.prep.yourself_why'), close, myself);
  } else if (plan.absent.length) {
    body = no('grey', t('beatbox.srv.prep.absent', { system: plan.system, packages: list(plan.absent) }), t('beatbox.srv.prep.yourself_why'), close, myself);
  } else if (!plan.reach) {
    body = (
      <>
        <Facts plan={plan} />
        {no('amber', t('beatbox.srv.prep.no_reach', { packages }), t('beatbox.srv.prep.no_reach_why'),
          myself, <Button key="open" variant="primary" icon="arrow" onClick={openMachine}>{t('beatbox.srv.helpers.open_machine')}</Button>)}
      </>
    );
  } else if (plan.room_kb !== undefined && plan.free_kb !== undefined && plan.room_kb + ROOM_SPARE_KB > plan.free_kb) {
    body = no('amber', t('beatbox.srv.prep.no_room'),
      t('beatbox.srv.prep.no_room_why', { need: size(plan.room_kb + ROOM_SPARE_KB), machine, free: size(plan.free_kb) }),
      close, <Button key="open" variant="primary" icon="arrow" onClick={openMachine}>{t('beatbox.srv.helpers.open_machine')}</Button>);
  } else {
    const byPackage = [...new Map(plan.install.map((each) => [each.package, each])).values()];
    body = (
      <>
        <Facts plan={plan} />
        <div>
          <p className="bb-prep-label">{t('beatbox.srv.prep.will')}</p>
          {byPackage.map((each) => (
            <div key={each.package} className="bb-prep-fact">
              <span><b>{each.package}</b> <span className="bb-faint">{t('beatbox.srv.prep.for', { service })}</span></span>
              {byPackage.length === 1 && plan.get_kb ? <span className="bb-faint">{t('beatbox.srv.prep.about_size', { size: size(plan.get_kb) })}</span> : null}
            </div>
          ))}
          {byPackage.length > 1 && plan.get_kb ? (
            <div className="bb-prep-fact"><span className="bb-faint">{t('beatbox.srv.prep.download')}</span><span className="bb-faint">{t('beatbox.srv.prep.about_size', { size: size(plan.get_kb) })}</span></div>
          ) : null}
          {plan.have.map((program) => (
            <div key={program} className="bb-prep-fact">
              <span className="bb-faint">{program}</span>
              <span className="bb-faint bb-prep-had"><Icon name="check" size="sm" />{t('beatbox.srv.prep.already')}</span>
            </div>
          ))}
        </div>
        <details className="bb-srv-more">
          <summary>{t('beatbox.srv.prep.runs')}</summary>
          <pre className="bb-prep-log bb-mono">{`apt-get update\napt-get install --no-install-recommends ${packages.replace(/, /g, ' ')}`}</pre>
        </details>
        <div className="bb-prep-acts">
          {close}
          <Button variant="primary" icon="box" disabled={busy} onClick={() => void install()}>{t('beatbox.srv.prep.install', { packages })}</Button>
        </div>
      </>
    );
  }

  const title = view === 'manual' ? t('beatbox.srv.prep.manual_title', { programs: list(missing.length ? missing : plan?.have ?? []) })
    : t('beatbox.srv.prep.title', { machine });
  return (
    <Modal title={title} icon="cpu" width={560} onClose={onClose}
           info={view === 'manual' ? undefined : t('beatbox.srv.prep.about', { machine })}>
      <div className="bb-prep">
        {body}
        {told && plan ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
      </div>
    </Modal>
  );
}

/** Room left over after an install, so the machine is not left full. */
const ROOM_SPARE_KB = 100 * 1024;

function Facts({ plan }: { plan: Plan }) {
  return (
    <div>
      <p className="bb-prep-label">{t('beatbox.srv.prep.has')}</p>
      <div className="bb-prep-fact"><span className="bb-faint">{t('beatbox.srv.prep.system')}</span><span>{plan.system}</span></div>
      {plan.free_kb !== undefined ? (
        <div className="bb-prep-fact"><span className="bb-faint">{t('beatbox.srv.prep.free')}</span><span>{size(plan.free_kb)}</span></div>
      ) : null}
      <div className="bb-prep-fact">
        <span className="bb-faint">{t('beatbox.srv.prep.source')}</span>
        <span className={plan.reach ? '' : 'bb-amber'}><i className={`bb-prep-dot${plan.reach ? ' is-ok' : ''}`} />{t(plan.reach ? 'beatbox.srv.prep.source_yes' : 'beatbox.srv.prep.source_no')}</span>
      </div>
    </div>
  );
}
