import { useEffect, useRef, useState } from 'react';
import RFBExport from '@novnc/novnc/lib/rfb.js';
import { plainDetail, t } from '../i18n';

interface MachineDisplayProps {
  id: string;
  name: string;
  url: string;
  password: string;
}

// noVNC 1.5 is CommonJS with `exports.default = RFB`, so the bundler may hand
// us the module object instead of the class.
const RFB = (RFBExport as typeof RFBExport & { default?: typeof RFBExport }).default ?? RFBExport;

/** The machine's screen over VNC, drawn in our own window. */
export function MachineDisplay({ name, url, password }: MachineDisplayProps) {
  const surface = useRef<HTMLDivElement>(null);
  const session = useRef<InstanceType<typeof RFB> | null>(null);
  const [fit, setFit] = useState(false);
  const [status, setStatus] = useState(t('beatbox.display.connecting'));

  useEffect(() => {
    const target = surface.current;
    if (!target) return undefined;
    let rfb: InstanceType<typeof RFB>;
    try {
      rfb = new RFB(target, url, { credentials: { password }, shared: true });
    } catch (error) {
      console.error('[beatbox] VNC viewer could not start', error);
      setStatus(t('beatbox.display.unavailable', { detail: plainDetail(String(error)) }));
      return undefined;
    }
    session.current = rfb;
    // The connection is local: favour text clarity over JPEG bandwidth.
    rfb.qualityLevel = 9;
    rfb.compressionLevel = 2;
    rfb.scaleViewport = false;
    rfb.resizeSession = true;
    rfb.focusOnClick = true;
    rfb.addEventListener('connect', () => {
      setStatus('');
      rfb.focus();
      // Windows guests blank QEMU's monitor while still running. A lone Shift
      // is harmless when awake and wakes the screen when it's not.
      rfb.sendKey(0xffe1, 'ShiftLeft', true);
      rfb.sendKey(0xffe1, 'ShiftLeft', false);
    });
    rfb.addEventListener('credentialsrequired', () => rfb.sendCredentials({ password }));
    rfb.addEventListener('securityfailure', () => setStatus(t('beatbox.display.refused')));
    rfb.addEventListener('disconnect', (event) => {
      const clean = Boolean((event as CustomEvent<{ clean?: boolean }>).detail?.clean);
      setStatus(t(clean ? 'beatbox.display.machine_off' : 'beatbox.display.ended'));
    });
    return () => {
      session.current = null;
      rfb.disconnect();
    };
  }, [url, password]);

  useEffect(() => {
    if (session.current) session.current.scaleViewport = fit;
  }, [fit, url, password]);

  return (
    <main aria-label={`BeatBox — ${name}`} className="bb-display">
      <div className="bb-display-toolbar" role="group" aria-label={t('beatbox.display.size')}>
        <span>{t('beatbox.display.size')}</span>
        <button type="button" aria-pressed={!fit} onClick={() => setFit(false)}>{t('beatbox.display.native')}</button>
        <button type="button" aria-pressed={fit} onClick={() => setFit(true)}>{t('beatbox.display.fit')}</button>
      </div>
      <div ref={surface} className="bb-display-surface" />
      {status ? (
        <div role="status" style={{ position: 'absolute', inset: 0, display: 'grid', placeItems: 'center', color: '#c9c5e8', fontFamily: 'system-ui, sans-serif', pointerEvents: 'none' }}>
          {status}
        </div>
      ) : null}
    </main>
  );
}
