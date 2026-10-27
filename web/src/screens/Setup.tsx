import { useCallback, useEffect, useRef, useState } from 'react';
import { Button, Card, Icon, Info, Pill, Wordmark } from '../components';
import type { IconName } from '../components';
import { call, on } from '../bridge/client';
import type { Job, SetupStepView, SetupView } from '../bridge/types';
import { fault, plainDetail, say, t, runtimeText } from '../i18n';
import { navigate, useRouteParams } from '../router';

/**
 * What BeatBox needs from this computer before the first machine: QEMU, a
 * hypervisor this user can use, and (optional) Windows guest tools. The
 * daemon checks them and says which it can fix (crates/api/src/setup.rs).
 *
 * Opens on start when something's missing: always for QEMU (nothing runs
 * without it), for acceleration only until skipped. Machines still run
 * without it, just slowly, and a nag on every start trains people to click
 * through.
 */

const ICONS: Record<string, IconName> = { qemu: 'cpu', acceleration: 'pulse', guest_tools: 'box' };
/** Job kinds a fix can start: `setup.fix` itself, then the guest tools download. */
const FIXES: Record<string, string> = { setup: '', tools: 'guest_tools' };
const SKIPPED = 'bb-setup-skipped';

function skipped(): boolean {
  try {
    return window.localStorage.getItem(SKIPPED) === '1';
  } catch {
    return false;
  }
}

/** Open here instead of on the machines? */
export function needsSetup(view: SetupView): boolean {
  return view.steps.some((step) => !step.ready && !step.optional
    && (step.key === 'qemu' || (step.fixable && !skipped())));
}

/**
 * `setup?preview=1`: a fake bare Debian, to see the wizard on a machine that
 * has everything. Buttons pretend; the daemon is never called.
 */
const PREVIEW: SetupStepView[] = [
  { key: 'qemu', ready: false, optional: false, fixable: true, reason: { code: 'setup.qemu.missing',
    values: { command: 'apt-get update && apt-get install -y qemu-system-x86 qemu-utils qemu-system-gui ovmf' } } },
  { key: 'acceleration', ready: false, optional: false, fixable: true,
    reason: { code: 'setup.acceleration.kvm_group', values: { user: '' } } },
  { key: 'guest_tools', ready: false, optional: true, fixable: true,
    reason: { code: 'setup.guest_tools.missing', values: { megabytes: '31' } } },
];
/** Each preview step after its fake fix. */
const PREVIEW_FIXED: Record<string, SetupStepView> = {
  qemu: { key: 'qemu', ready: true, optional: false, fixable: false, reason: { code: 'setup.qemu.ready', values: {} } },
  acceleration: { key: 'acceleration', ready: false, optional: false, fixable: false,
    reason: { code: 'setup.acceleration.relogin', values: {} } },
  guest_tools: { key: 'guest_tools', ready: true, optional: true, fixable: false,
    reason: { code: 'setup.guest_tools.ready', values: { version: '0.1.302' } } },
};

/** Steps and fixes, shared by the wizard and Settings > This computer. */
export function useSetup(preview = false) {
  // Preview has to invent the account name; use a localised one.
  const [view, setView] = useState<SetupView | null>(preview ? { steps: PREVIEW.map((step) =>
    step.key === 'acceleration'
      ? { ...step, reason: { ...step.reason, values: { user: t('beatbox.setup.preview_user') } } }
      : step) } : null);
  const [working, setWorking] = useState<string | null>(null);
  const [notes, setNotes] = useState<Record<string, { ok: boolean; text: string }>>({});
  const [failure, setFailure] = useState<string | null>(null);
  // The job's end event doesn't say which step it was for, so remember.
  const fixing = useRef<string | null>(null);

  const load = useCallback(async () => {
    if (preview) return;
    try {
      setView(await call('setup.status'));
    } catch (error) {
      setFailure(fault(error));
    }
  }, [preview]);

  useEffect(() => {
    if (preview) return;
    void load();
    // Pick up a fix already running, e.g. a password prompt left open while
    // the person wandered off and came back.
    void call('jobs.current')
      .then((current) => {
        if (current && current.state === 'running' && current.kind in FIXES) {
          fixing.current = FIXES[current.kind] || current.message;
          setWorking(fixing.current);
        }
      })
      .catch(() => undefined);
  }, [load]);

  useEffect(() => on('job', (fresh: Job) => {
    const key = fixing.current;
    if (!(fresh.kind in FIXES) || fresh.state === 'running' || !key) return;
    // Windows reports the platform off until a restart, so the step can't tell; we must.
    const restart = fresh.kind === 'setup' && fresh.message === 'restart';
    const note = fresh.state === 'done'
      ? { ok: true, text: restart ? t('beatbox.setup.after.restart') : '' }
      : { ok: false, text: fresh.reason ? say(fresh.reason) : plainDetail(fresh.message) };
    setNotes((was) => ({ ...was, [key]: note }));
    fixing.current = null;
    setWorking(null);
    void load();
  }), [load]);

  async function fix(key: string) {
    if (preview) {
      setWorking(key);
      window.setTimeout(() => {
        setView((was) => was && { steps: was.steps.map((step) => PREVIEW_FIXED[step.key] && step.key === key
          ? PREVIEW_FIXED[step.key]! : step) });
        setWorking(null);
      }, 2000);
      return;
    }
    setFailure(null);
    setNotes((was) => ({ ...was, [key]: { ok: true, text: '' } }));
    fixing.current = key;
    setWorking(key);
    try {
      await call('setup.fix', { step: key });
    } catch (error) {
      fixing.current = null;
      setWorking(null);
      setNotes((was) => ({ ...was, [key]: { ok: false, text: fault(error) } }));
    }
  }

  return { view, working, notes, failure, load, fix };
}

export function Setup() {
  const preview = useRouteParams().get('preview') === '1';
  const { view, working, notes, failure, load, fix } = useSetup(preview);
  const needed = view?.steps.filter((step) => !step.optional) ?? [];
  const ready = view !== null && needed.every((step) => step.ready);

  function leave() {
    if (preview) {
      navigate('settings');
      return;
    }
    if (!ready) {
      try {
        window.localStorage.setItem(SKIPPED, '1');
      } catch {
        // Couldn't save the skip: it asks again next time. Safe way to fail.
      }
    }
    navigate('host/machine');
  }

  return (
    <div className="bb-setup">
      <div>
        <Wordmark />
        <div className="bb-pagehead-title" style={{ margin: 'var(--bb-space-4) 0 0' }}>
          <h1 className="bb-t-title" style={{ margin: 0 }}>{t('beatbox.screen.setup.title')}</h1>
          <Info text={t('beatbox.screen.setup.blurb')} />
        </div>
        {preview ? <p className="bb-amber" style={{ fontSize: 12.5, margin: '10px 0 0' }}>{t('beatbox.setup.preview_note')}</p> : null}
      </div>

      <Card>
        {view ? view.steps.map((step, index) => {
          const note = notes[step.key];
          return (
            <div key={step.key} className="bb-objective">
              <span className="bb-rowmark">
                <Icon name={step.ready ? 'check' : ICONS[step.key] ?? 'box'} size="sm" />
              </span>
              <div style={{ flex: 1, minWidth: 0 }}>
                <b>{index + 1}. {runtimeText(`beatbox.setup.step.${step.key}`)}</b>
                <p>{say(step.reason)}</p>
                {working === step.key ? <p className="bb-amber">{t('beatbox.setup.working')}</p> : null}
                {note?.text ? <p className={note.ok ? 'bb-mint' : 'bb-rose'}>{note.text}</p> : null}
              </div>
              <Pill tone={step.ready ? 'mint' : step.optional ? 'neutral' : 'amber'}>
                {step.ready ? t('beatbox.probe.ready')
                  : step.optional ? t('beatbox.setup.optional') : t('beatbox.probe.missing')}
              </Pill>
              {step.fixable ? (
                <Button size="sm" variant={step.optional ? 'ghost' : 'primary'} disabled={working !== null}
                        onClick={() => void fix(step.key)}>
                  {runtimeText(`beatbox.setup.fix.${step.key}`)}
                </Button>
              ) : null}
            </div>
          );
        }) : (
          <p className="bb-faint" style={{ fontSize: 12.5, margin: 0 }}>{t('beatbox.run_stage.reading')}</p>
        )}
      </Card>

      {failure ? <p className="bb-rose" style={{ fontSize: 12.5, margin: 0 }}>{failure}</p> : null}

      <div className="bb-setup-foot">
        <Button size="sm" icon="refresh" disabled={working !== null} onClick={() => void load()}>
          {t('beatbox.setup.check_again')}
        </Button>
        <span />
        <Button variant={ready ? 'primary' : 'ghost'} trailingIcon="arrow" onClick={leave}>
          {ready ? t('beatbox.setup.continue') : t('beatbox.setup.skip')}
        </Button>
      </div>
    </div>
  );
}
