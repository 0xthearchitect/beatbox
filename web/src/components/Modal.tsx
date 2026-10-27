import { useEffect, useRef, type ReactNode } from 'react';
import { Icon } from './Icon';
import { CloseButton } from './primitives';
import { t } from '../i18n';

/**
 * Our own modal: scrim, panel, title, a way out. Native dialogs looked like
 * another app. No drag, resize or stacking; we only ever show one.
 *
 * `dismissable={false}` for questions that block something (like closing the
 * window): Escape or a click outside must not count as an answer.
 */
export function Modal({ title, icon, children, footer, width, onClose, dismissable = true }: {
  title: string;
  icon?: Parameters<typeof Icon>[0]['name'];
  children: ReactNode;
  footer?: ReactNode;
  /** px. Default fits a form; choosers want more. */
  width?: number;
  onClose: () => void;
  dismissable?: boolean;
}) {
  const panel = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // Move focus in, or typing goes to the page behind the scrim. Focus the
    // panel, not the first focusable: that's the close X, and a focus ring
    // on it looked like a bug. `data-autofocus` overrides.
    const wanted = panel.current?.querySelector<HTMLElement>('[data-autofocus]');
    (wanted ?? panel.current)?.focus();
  }, []);

  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if (event.key === 'Escape' && dismissable) {
        event.stopPropagation();
        onClose();
        return;
      }
      if (event.key !== 'Tab') return;
      // Trap Tab, or it reaches the sidebar behind and Enter navigates away.
      const stops = panel.current?.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
      if (!stops?.length) return;
      const first = stops[0]!;
      const last = stops[stops.length - 1]!;
      if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      } else if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      }
    }
    document.addEventListener('keydown', onKey, true);
    return () => document.removeEventListener('keydown', onKey, true);
  }, [onClose, dismissable]);

  return (
    <div
      className="bb-scrim"
      onMouseDown={(event) => { if (dismissable && event.target === event.currentTarget) onClose(); }}
    >
      <div
        className="bb-modal"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        style={width ? { width, maxWidth: '100%' } : undefined}
        ref={panel}
        tabIndex={-1}
      >
        <header className="bb-modal-head">
          {icon ? <Icon name={icon} size="sm" /> : null}
          <b>{title}</b>
          {dismissable ? <CloseButton title={t('beatbox.modal.close')} onClick={onClose} /> : null}
        </header>
        <div className="bb-modal-body">{children}</div>
        {footer ? <footer className="bb-modal-foot">{footer}</footer> : null}
      </div>
    </div>
  );
}
