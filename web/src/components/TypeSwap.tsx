import { useEffect, useRef, useState } from 'react';

/**
 * One word backspaces away and another types in. Once.
 *
 * From React Bits' TextType (MIT, reactbits.dev), trimmed down:
 * - No GSAP: the blinking cursor is a CSS @keyframes.
 * - One swap, not a looping carousel; that's all the hero needs.
 * - Cursor blinks while waiting, goes solid while typing, leaves when done.
 *
 * The width isn't reserved on purpose: the rest of the line shifting is what
 * retyping looks like, and a reserved box leaves a gap forever.
 */
export function TypeSwap({
  from,
  to,
  startAfter = 1350,
  deletingSpeed = 125,
  typingSpeed = 175,
  className = '',
  onDone,
}: {
  /** Shown on mount, then deleted char by char. */
  from: string;
  to: string;
  /**
   * ms between DecryptedText finishing (when this mounts) and the cursor
   * starting to delete. Reading time: people must read "Break the box."
   * before it corrects itself.
   */
  startAfter?: number;
  deletingSpeed?: number;
  typingSpeed?: number;
  className?: string;
  onDone?: () => void;
}) {
  const [shown, setShown] = useState(from);
  const [phase, setPhase] = useState<Phase>('waiting');
  const finished = useRef(false);

  // New words (language switch): start over.
  useEffect(() => {
    setShown(from);
    setPhase('waiting');
    finished.current = false;
  }, [from, to]);

  useEffect(() => {
    if (phase === 'done') {
      if (!finished.current) {
        finished.current = true;
        onDone?.();
      }
      return undefined;
    }

    // Checked every step, so turning on reduced motion applies right away.
    if (calm()) {
      setShown(to);
      setPhase('done');
      return undefined;
    }

    const wait = phase === 'waiting' ? startAfter
      : phase === 'deleting' ? deletingSpeed
      : typingSpeed;

    const timer = window.setTimeout(() => {
      if (phase === 'waiting') {
        setPhase('deleting');
      } else if (phase === 'deleting') {
        // Delete the whole word, not down to the shared "Be": that reads as
        // fixing a typo, not changing its mind.
        if (shown) setShown(shown.slice(0, -1));
        else setPhase('typing');
      } else if (shown.length < to.length) {
        setShown(to.slice(0, shown.length + 1));
      } else {
        setPhase('done');
      }
    }, wait);
    return () => window.clearTimeout(timer);
  }, [phase, shown, to, startAfter, deletingSpeed, typingSpeed, onDone]);

  return (
    <span className={className} style={{ whiteSpace: 'pre' }}>
      {/* Screen readers get the final word only. */}
      <span className="bb-sr">{to}</span>
      <span aria-hidden="true">
        {shown}
        {phase === 'done' ? null : (
          <i className={`bb-caret${phase === 'waiting' ? ' bb-caret--idle' : ''}`} />
        )}
      </span>
    </span>
  );
}

type Phase = 'waiting' | 'deleting' | 'typing' | 'done';

const calm = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches;
