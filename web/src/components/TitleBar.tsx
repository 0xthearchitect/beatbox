import { useEffect, useRef, useState, type MouseEvent } from 'react';
import { t } from '../i18n';

interface Frame {
  invoke(command: 'frame', args: { what: string }): Promise<boolean>;
}

/** The window's own commands (shell/src/main.rs); absent in a plain browser. */
const frame = (window as unknown as { __TAURI_INTERNALS__?: Frame }).__TAURI_INTERNALS__;

const ask = (what: string) => frame!.invoke('frame', { what });

const WINDOWS = navigator.userAgent.includes('Windows');

/** 10px glyphs, same weight as the bar's text. */
const GLYPHS = {
  monitor: 'M2.5 2.5h11v8h-11zM5.5 13.5h5M8 10.5v3',
  minimize: 'M1 5.5h8',
  maximize: 'M1.5 1.5h7v7h-7z',
  restore: 'M1.5 3.5h5v5h-5zM3.5 3.5v-2h5v5h-2',
  close: 'M1.5 1.5l7 7M8.5 1.5l-7 7',
} as const;

function Glyph({ name }: { name: keyof typeof GLYPHS }) {
  const box = name === 'monitor' ? 16 : 10;
  return (
    <svg width={10} height={10} viewBox={`0 0 ${box} ${box}`} fill="none" stroke="currentColor"
         strokeWidth={1} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d={GLYPHS[name]} />
    </svg>
  );
}

/** Dot in 12 units, drawn 14px inside a 20px button: easy to hit, small gaps. */
const MARKS = {
  minimize: 'M3.5 6h5',
  maximize: 'M6 3.5v5M3.5 6h5',
  close: 'M4 4l4 4M8 4l-4 4',
} as const;

function Mark({ name }: { name: keyof typeof MARKS }) {
  return (
    <svg width={14} height={14} viewBox="0 0 12 12" fill="none" stroke="currentColor"
         strokeWidth={1.2} strokeLinecap="round" aria-hidden="true">
      <circle cx={6} cy={6} r={6} />
      <path d={MARKS[name]} />
    </svg>
  );
}

/**
 * Our own title bar, so it looks the same on every desktop. Controls on the
 * right: Windows' buttons on Windows, coloured dots elsewhere, close outermost.
 * Close goes through CloseRequested so Closing.tsx can ask first.
 * In a plain browser or tests there's no window to control.
 */
export function TitleBar() {
  const [maximized, setMaximized] = useState(true);
  // Tracked by hand, not :hover: after a window drag WebKitGTK leaves :hover
  // stuck on whatever was under the pointer.
  const dots = useRef<HTMLDivElement>(null);
  const [near, setNear] = useState(false);

  useEffect(() => {
    if (!frame) return;
    // The desktop can maximize us (shortcut, drag to top) and a resize is the
    // only hint we get.
    const follow = () => void ask('state').then(setMaximized).catch(() => undefined);
    const move = (event: PointerEvent) =>
      setNear(Boolean(dots.current?.contains(event.target as Node)));
    const away = () => setNear(false);
    follow();
    window.addEventListener('resize', follow);
    window.addEventListener('pointermove', move);
    window.addEventListener('blur', away);
    document.documentElement.addEventListener('pointerleave', away);
    return () => {
      window.removeEventListener('resize', follow);
      window.removeEventListener('pointermove', move);
      window.removeEventListener('blur', away);
      document.documentElement.removeEventListener('pointerleave', away);
    };
  }, []);

  if (!frame) return null;

  const act = (what: string) => {
    setNear(false);
    void ask(what).then(setMaximized).catch(() => undefined);
  };
  // On the bar itself (not a button): double-click maximizes, press drags.
  const press = (event: MouseEvent) => {
    if (event.button !== 0 || (event.target as Element).closest('button')) return;
    act(event.detail === 2 ? 'maximize' : 'drag');
  };
  const resize = maximized ? 'beatbox.window.restore' : 'beatbox.window.maximize';

  return (
    <header className={WINDOWS ? 'bb-titlebar' : 'bb-titlebar bb-titlebar--dots'} onMouseDown={press}>
      <span className="bb-titlebar-local"><Glyph name="monitor" />{t('beatbox.window.local')}</span>
      <span>{t('beatbox.window.title')}</span>
      {WINDOWS ? (
        <div className="bb-titlebar-controls">
          <button type="button" aria-label={t('beatbox.window.minimize')} title={t('beatbox.window.minimize')}
                  onClick={() => act('minimize')}><Glyph name="minimize" /></button>
          <button type="button" aria-label={t(resize)} title={t(resize)}
                  onClick={() => act('maximize')}><Glyph name={maximized ? 'restore' : 'maximize'} /></button>
          <button type="button" className="bb-titlebar-close" aria-label={t('beatbox.window.close')}
                  title={t('beatbox.window.close')} onClick={() => act('close')}><Glyph name="close" /></button>
        </div>
      ) : (
        <div ref={dots} className="bb-titlebar-dots" data-near={near || undefined}>
          <button type="button" className="bb-titlebar-minimize" aria-label={t('beatbox.window.minimize')}
                  title={t('beatbox.window.minimize')} onClick={() => act('minimize')}><Mark name="minimize" /></button>
          <button type="button" className="bb-titlebar-maximize" aria-label={t(resize)} title={t(resize)}
                  onClick={() => act('maximize')}><Mark name="maximize" /></button>
          <button type="button" className="bb-titlebar-close" aria-label={t('beatbox.window.close')}
                  title={t('beatbox.window.close')} onClick={() => act('close')}><Mark name="close" /></button>
        </div>
      )}
    </header>
  );
}
