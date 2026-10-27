import { useState, type CSSProperties, type ReactNode } from 'react';
import {
  FloatingPortal, autoUpdate, flip, offset, shift,
  useClick, useDismiss, useFloating, useFocus, useHover, useInteractions, useRole,
} from '@floating-ui/react';
import { cssVar, type ColorToken } from '../tokens';
import { Icon, type IconName, type IconSize } from './Icon';

/** Tone is a role, not a colour: callers say "gap", tokens decide it's amber. No hex here. */
export type Tone = 'violet' | 'mint' | 'amber' | 'rose' | 'sky' | 'neutral';

const TONE_INK: Record<Tone, ColorToken> = {
  violet: 'violetLift',
  mint: 'mint',
  amber: 'amber',
  rose: 'rose',
  sky: 'sky',
  neutral: 'textMuted',
};
const TONE_GROUND: Record<Tone, ColorToken> = {
  violet: 'violetWash',
  mint: 'mintWash',
  amber: 'amberWash',
  rose: 'roseWash',
  sky: 'skyWash',
  neutral: 'hover',
};

export const toneInk = (tone: Tone) => cssVar('color', TONE_INK[tone]);
export const toneGround = (tone: Tone) => cssVar('color', TONE_GROUND[tone]);

export function Button({
  children, variant = 'ghost', size = 'md', icon, trailingIcon, wide, disabled, onClick, title, autofocus, type = 'button',
}: {
  children?: ReactNode;
  variant?: 'primary' | 'ghost' | 'danger';
  size?: 'sm' | 'md';
  icon?: IconName;
  trailingIcon?: IconName;
  wide?: boolean;
  disabled?: boolean;
  onClick?: () => void;
  title?: string;
  type?: 'button' | 'submit' | 'reset';
  /** Focus this inside a Modal. Not native `autoFocus`: Modal's focus pass overrides that. */
  autofocus?: boolean;
}) {
  const classes = ['bb-btn', `bb-btn--${variant}`];
  if (size === 'sm') classes.push('bb-btn--sm');
  if (wide) classes.push('bb-btn--wide');
  return (
    <button type={type} className={classes.join(' ')} disabled={disabled} onClick={onClick} title={title}
            data-autofocus={autofocus ? '' : undefined}>
      {icon ? <Icon name={icon} size={size === 'sm' ? 'xs' : 'sm'} /> : null}
      {children}
      {trailingIcon ? <Icon name={trailingIcon} size={size === 'sm' ? 'xs' : 'sm'} /> : null}
    </button>
  );
}

export function IconButton({ name, onClick, title, size = 'sm' }: {
  name: IconName; onClick?: () => void; title?: string; size?: IconSize;
}) {
  return (
    <button type="button" className="bb-iconbtn" onClick={onClick} title={title} aria-label={title}>
      <Icon name={name} size={size} />
    </button>
  );
}

/** Red X, no background until hover: closing isn't worth the loudest button. */
export function CloseButton({ onClick, title }: { onClick?: () => void; title: string }) {
  return (
    <button type="button" className="bb-close" onClick={onClick} title={title} aria-label={title}>
      <Icon name="close" size="md" />
    </button>
  );
}

export function Pill({ children, tone = 'neutral', dot = true }: { children: ReactNode; tone?: Tone; dot?: boolean }) {
  return (
    <span className="bb-pill" style={{ color: toneInk(tone), background: toneGround(tone) }}>
      {dot ? <span className="bb-dot" /> : null}
      {children}
    </span>
  );
}

/** ⓘ with the explanation. Portalled and kept inside the window so cards don't clip it. */
export function Info({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const { refs, floatingStyles, context } = useFloating({
    open, onOpenChange: setOpen, placement: 'bottom-start',
    middleware: [offset(6), flip(), shift({ padding: 8 })], whileElementsMounted: autoUpdate,
  });
  const { getReferenceProps, getFloatingProps } = useInteractions([
    useHover(context, { move: false }), useFocus(context), useClick(context), useDismiss(context), useRole(context, { role: 'tooltip' }),
  ]);
  return (
    <>
      <button type="button" className="bb-info" aria-label={text} ref={refs.setReference} {...getReferenceProps()}>
        <Icon name="info" size="xs" />
      </button>
      {open ? (
        <FloatingPortal>
          <div className="bb-info-pop" ref={refs.setFloating} style={floatingStyles} {...getFloatingProps()}>{text}</div>
        </FloatingPortal>
      ) : null}
    </>
  );
}

export function Card({ children, tone, title, note, info, style }: {
  children: ReactNode;
  tone?: Tone;
  title?: ReactNode;
  note?: ReactNode;
  info?: string;
  style?: CSSProperties;
}) {
  // Tone the header icon and title too: border tint alone was invisible.
  // `--bb-tone` lets the body reuse it.
  const ink = tone ? toneInk(tone) : undefined;
  return (
    <section
      className={`bb-card${tone ? ' bb-card--toned' : ''}`}
      style={{
        ...(tone ? { borderColor: toneGround(tone), ['--bb-tone' as string]: ink } : null),
        ...style,
      }}
    >
      {title ? (
        <header className="bb-panelhd" style={{ padding: '0 0 10px', margin: '0 0 4px', color: ink }}>
          {/* h2 so screen readers can jump card to card. */}
          <h2 className="bb-panelhd-title">{title}</h2>
          {info ? <Info text={info} /> : null}
          {note ? <span className="bb-note">{note}</span> : null}
        </header>
      ) : null}
      {children}
    </section>
  );
}



/** A bar. White by default; pass a tone only when the colour means something (mint = done). */
export function Meter({ value, max, tone }: { value: number; max: number; tone?: Tone }) {
  const pct = max > 0 ? Math.max(0, Math.min(100, (value / max) * 100)) : 0;
  return (
    <div className="bb-meter" role="progressbar" aria-valuenow={value} aria-valuemin={0} aria-valuemax={max}>
      <i style={{ width: `${pct}%`, background: tone ? toneInk(tone) : 'var(--bb-color-action)' }} />
    </div>
  );
}

export function KeyValue({ label, children }: { label: ReactNode; children: ReactNode }) {
  return (
    <div className="bb-kv">
      <span>{label}</span>
      <b className="bb-mono" style={{ fontWeight: 500 }}>{children}</b>
    </div>
  );
}
