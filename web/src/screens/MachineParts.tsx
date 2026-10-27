import { Fragment, useEffect, useRef, useState } from 'react';
import { call, on } from '../bridge/client';
import type { SandboxEventView } from '../bridge/types';
import { Button, Icon, IconButton } from '../components';
import { t, type Key } from '../i18n';

/** Machine page (Sandbox.tsx) bits with their own state: save points and the "Save point" button. */

/** 14:02, 24-hour. */
export const clock = (when: Date) => when.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false });

/** MB to GB, at most two decimals: 4096 -> "4", 768 -> "0.75". */
export const gigs = (mb: number) => String(Math.round((mb / 1024) * 100) / 100);

/** How much of this computer the machine gets, as a short bar. */
export function Share({ part, whole, label }: { part: number; whole: number; label: string }) {
  if (!(whole > 0) || !(part > 0)) return null;
  return (
    <span className="bb-mach-share" title={label}>
      <i style={{ width: `${Math.min(100, (part / whole) * 100)}%` }} />
    </span>
  );
}

/** Save points, oldest first, then "now". Each has a menu: go back or delete (delete asks twice). */
export function SavePoints({ names, running, busy, onBack, onRemove }: {
  names: string[];
  running: boolean;
  busy: boolean;
  onBack: (name: string) => void;
  onRemove: (name: string) => void;
}) {
  const [open, setOpen] = useState('');
  const [removing, setRemoving] = useState(false);
  const holder = useRef<HTMLDivElement>(null);
  const close = () => { setOpen(''); setRemoving(false); };

  useEffect(() => {
    if (!open) return;
    const away = (event: MouseEvent) => {
      if (!holder.current?.contains(event.target as Node)) close();
    };
    const key = (event: KeyboardEvent) => { if (event.key === 'Escape') close(); };
    document.addEventListener('mousedown', away);
    document.addEventListener('keydown', key);
    return () => {
      document.removeEventListener('mousedown', away);
      document.removeEventListener('keydown', key);
    };
  }, [open]);

  return (
    <div className="bb-mach-points">
      <h2>{t('beatbox.machine.save_points')}</h2>
      {names.length ? names.map((name, i) => (
        <Fragment key={name}>
          {i ? <span className="bb-mach-sep" aria-hidden="true">›</span> : null}
          <div className="bb-mach-point" ref={open === name ? holder : undefined}>
            <button type="button" className={`bb-mach-chip${open === name ? ' on' : ''}`}
                    aria-haspopup="menu" aria-expanded={open === name}
                    onClick={() => { if (open === name) close(); else { setOpen(name); setRemoving(false); } }}>
              <i />{name}
            </button>
            {open === name ? (
              <div className="bb-mach-menu" role="menu" aria-label={name}>
                {removing ? (
                  <>
                    <p>{t('beatbox.machine.remove_sure', { name })}</p>
                    <button type="button" role="menuitem" className="bb-mach-item bb-mach-item--danger" disabled={busy}
                            onClick={() => { close(); onRemove(name); }}>
                      {t('beatbox.machine.remove_yes')}
                    </button>
                    <button type="button" role="menuitem" className="bb-mach-item" onClick={() => setRemoving(false)}>
                      {t('beatbox.machine.remove_no')}
                    </button>
                  </>
                ) : (
                  <>
                    <button type="button" role="menuitem" className="bb-mach-item" disabled={busy || !running}
                            onClick={() => { close(); onBack(name); }}>
                      {t('beatbox.machine.points.go_back')}
                    </button>
                    {running ? null : <p>{t('beatbox.machine.points.needs_running')}</p>}
                    <div role="separator" />
                    <button type="button" role="menuitem" className="bb-mach-item bb-mach-item--danger"
                            onClick={() => setRemoving(true)}>
                      {t('beatbox.machine.remove_named', { name })}
                    </button>
                  </>
                )}
              </div>
            ) : null}
          </div>
        </Fragment>
      )) : <span className="bb-mach-none">{t('beatbox.machine.save_none')}</span>}
      {names.length && running ? (
        <>
          <span className="bb-mach-sep" aria-hidden="true">›</span>
          <span className="bb-mach-now"><i />{t('beatbox.machine.points.now')}</span>
        </>
      ) : null}
    </div>
  );
}

/** "Save point" button; asks for the name in place. */
export function SaveNow({ busy, onSave }: { busy: boolean; onSave: (name: string) => void }) {
  const [naming, setNaming] = useState(false);
  const [name, setName] = useState('');
  const done = () => { setNaming(false); setName(''); };
  if (!naming) {
    return (
      <button type="button" className="bb-btn bb-btn--ghost bb-mach-act--save" disabled={busy} onClick={() => setNaming(true)}>
        <Icon name="plus" size="sm" />{t('beatbox.machine.points.save')}
      </button>
    );
  }
  return (
    <form className="bb-mach-naming"
          onSubmit={(event) => { event.preventDefault(); if (name.trim()) { onSave(name.trim()); done(); } }}>
      <input className="bb-input bb-mono" autoFocus value={name} placeholder={t('beatbox.machine.save_placeholder')}
             aria-label={t('beatbox.machine.points.name')} onChange={(event) => setName(event.target.value)}
             onKeyDown={(event) => { if (event.key === 'Escape') done(); }} />
      <Button type="submit" disabled={busy || !name.trim()}>{t('beatbox.machine.save_now')}</Button>
      <IconButton name="close" title={t('beatbox.machine.points.cancel')} onClick={done} />
    </form>
  );
}

/** Events worth showing, in plain words. Anything not listed is hidden. */
const SAID: Record<string, Key> = {
  'sandbox.created': 'beatbox.machine.happened.created',
  'sandbox.started': 'beatbox.machine.told.start',
  'sandbox.stopped': 'beatbox.machine.told.stop',
  'sandbox.paused': 'beatbox.machine.told.pause',
  'sandbox.resumed': 'beatbox.machine.told.resume',
  'sandbox.reset': 'beatbox.machine.told.reset',
  'sandbox.failed': 'beatbox.machine.happened.failed',
  'sandbox.reconfigured': 'beatbox.machine.happened.reconfigured',
  'snapshot.created': 'beatbox.machine.told.saved',
  'snapshot.restored': 'beatbox.machine.told.restored',
  'snapshot.deleted': 'beatbox.machine.told.removed',
  'snapshot.state_saved': 'beatbox.machine.happened.state_saved',
};
const HAPPENED_ROWS = 12;

/** Recent events for this machine, newest first, live. */
export function Happened({ id }: { id: string }) {
  const [said, setSaid] = useState<SandboxEventView[]>([]);
  useEffect(() => {
    const mine = (event: SandboxEventView) => event.sandboxId === id && event.name in SAID;
    let live = true;
    void call('sandbox.events', { since: 0 })
      .then((events) => { if (live) setSaid(events.filter(mine).slice(-HAPPENED_ROWS).reverse()); })
      .catch(() => undefined);
    const off = on('sandbox', (event) => {
      if (mine(event)) setSaid((was) => [event, ...was].slice(0, HAPPENED_ROWS));
    });
    return () => { live = false; off(); };
  }, [id]);

  return (
    <section className="bb-mach-happened" aria-labelledby="bb-mach-happened">
      <h2 id="bb-mach-happened">{t('beatbox.machine.happened.title')}</h2>
      {said.length ? (
        <ol>
          {said.map((event) => (
            <li key={event.sequence}>
              <time className="bb-mono">{clock(new Date(event.at))}</time>
              {t(SAID[event.name]!, { name: String((event.data as { name?: unknown } | null)?.name ?? '') })}
            </li>
          ))}
        </ol>
      ) : <p>{t('beatbox.machine.happened.none')}</p>}
    </section>
  );
}

/** Screenshot refresh (ms). Each one is an action on the bus, so seconds, not frames. */
const PICTURE_EVERY = 10_000;

export interface Picture { src: string; taken: Date }

/** Screenshot of the machine: refreshed while it runs and the page is visible, else the last one. */
export function usePicture(id: string, offered: boolean, lifecycle: string): Picture | null {
  const [picture, setPicture] = useState<Picture | null>(null);
  useEffect(() => setPicture(null), [id]);
  useEffect(() => {
    if (!offered) return;
    let live = true;
    const look = async () => {
      if (document.hidden) return;
      try {
        const outcome = await call('sandbox.act', { id, capability: 'display.picture', arguments: {} });
        const data = outcome.data as { picture?: string; taken?: number } | null;
        if (!live || !outcome.ok) return;
        setPicture(data?.picture
          ? { src: `data:image/png;base64,${data.picture}`, taken: new Date((data.taken ?? 0) * 1000) }
          : null);
      } catch { /* keep the old picture */ }
    };
    void look();
    const beat = lifecycle === 'running' ? window.setInterval(() => void look(), PICTURE_EVERY) : 0;
    return () => { live = false; window.clearInterval(beat); };
  }, [id, offered, lifecycle]);
  return picture;
}
