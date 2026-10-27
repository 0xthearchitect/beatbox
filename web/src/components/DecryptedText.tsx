import { useEffect, useMemo, useRef, useState } from 'react';

/**
 * Text that starts as noise and resolves into itself.
 *
 * From React Bits' DecryptedText (MIT, reactbits.dev), changed:
 * - No framer-motion: the original's `motion.span` never animated anything.
 * - One reveal order, computed once. The original had two that disagreed on `center`.
 * - Honours reduced motion; scrambled text is unreadable while it runs.
 * - Screen readers get the real text. The original hid even that.
 *
 * For a one-off claim on arrival, never for labels or values.
 */

/** Hex and terminal punctuation, so the noise looks machine-made. */
const NOISE = 'ABCDEF0123456789<>[]{}/\\|*#%&+=-_$?!';

export function DecryptedText({
  text,
  speed = 42,
  maxIterations = 12,
  sequential = true,
  revealDirection = 'start',
  useOriginalCharsOnly = false,
  characters = NOISE,
  animateOn = 'view',
  className = '',
  encryptedClassName = '',
  parentClassName = '',
  onDone,
}: {
  text: string;
  /** ms between iterations. */
  speed?: number;
  /** How many scrambles before a non-sequential reveal gives up and lands. */
  maxIterations?: number;
  /** Reveal one character at a time, in order, rather than all at once. */
  sequential?: boolean;
  revealDirection?: 'start' | 'end' | 'center';
  /** Scramble using only the letters already in the text. */
  useOriginalCharsOnly?: boolean;
  characters?: string;
  /** `view` runs once when it scrolls in; `hover` runs whenever pointed at. */
  animateOn?: 'view' | 'hover';
  className?: string;
  encryptedClassName?: string;
  parentClassName?: string;
  /** Fires once the text is legible, also when reduced motion skipped the animation. */
  onDone?: () => void;
}) {
  const [display, setDisplay] = useState(text);
  /** Revealed indices. `null` = all of them (the usual state). */
  const [revealed, setRevealed] = useState<Set<number> | null>(null);
  const [running, setRunning] = useState(false);
  const host = useRef<HTMLSpanElement>(null);
  const seen = useRef(false);

  const alphabet = useMemo(
    () => (useOriginalCharsOnly
      ? [...new Set(text.split(''))].filter((char) => char.trim())
      : characters.split('')),
    [useOriginalCharsOnly, text, characters],
  );

  const order = useMemo(() => revealOrder(text.length, revealDirection), [text.length, revealDirection]);

  // New text (usually a language switch) starts legible; it only scrambles if asked.
  useEffect(() => {
    setDisplay(text);
    setRevealed(null);
    setRunning(false);
    seen.current = false;
  }, [text]);

  useEffect(() => {
    if (!running) return undefined;

    const land = () => {
      setDisplay(text);
      setRevealed(null);
      setRunning(false);
      onDone?.();
    };

    // Checked here so turning on reduced motion mid-reveal applies immediately.
    if (calm()) { land(); return undefined; }

    // Local, not state: an updater that calls setState is impure and React may run it twice.
    let out = new Set<number>();
    let tick = 0;
    const timer = window.setInterval(() => {
      tick += 1;
      if (sequential) out = new Set(out).add(order[out.size]!);
      const finished = sequential ? out.size >= text.length : tick >= maxIterations;
      if (finished) {
        window.clearInterval(timer);
        land();
        return;
      }
      setRevealed(out);
      setDisplay(scramble(text, out, alphabet));
    }, speed);
    return () => window.clearInterval(timer);
  }, [running, text, speed, sequential, maxIterations, order, alphabet, onDone]);

  const start = () => {
    if (running) return;
    const nothing = new Set<number>();
    setRevealed(nothing);
    setDisplay(scramble(text, nothing, alphabet));
    setRunning(true);
  };

  useEffect(() => {
    if (animateOn !== 'view') return undefined;
    const element = host.current;
    if (!element) return undefined;
    const watcher = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting || seen.current) continue;
        seen.current = true;
        // Same as `start()`, inlined: depending on `start` re-observes every frame.
        setRunning(true);
      }
    }, { threshold: 0.1 });
    watcher.observe(element);
    return () => watcher.disconnect();
  }, [animateOn, text]);

  return (
    <span
      className={parentClassName}
      ref={host}
      style={{ display: 'inline-block', whiteSpace: 'pre-wrap' }}
      onMouseEnter={animateOn === 'hover' ? start : undefined}
    >
      {/* What screen readers hear; the noise below is aria-hidden. */}
      <span className="bb-sr">{text}</span>
      <span aria-hidden="true">
        {/* Plain text once landed: per-char spans break kerning, and the line
            jumped sideways when TypeSwap took over. */}
        {revealed
          ? display.split('').map((char, index) => (
              <span key={index} className={revealed.has(index) ? className : encryptedClassName}>
                {char}
              </span>
            ))
          : <span className={className}>{display}</span>}
      </span>
    </span>
  );
}

const calm = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches;

/** Order in which characters resolve. */
function revealOrder(length: number, direction: 'start' | 'end' | 'center'): number[] {
  const indices = [...Array(length).keys()];
  if (direction === 'start') return indices;
  if (direction === 'end') return indices.reverse();
  const middle = Math.floor(length / 2);
  // Middle outwards, alternating sides: 0 -> middle, 1 -> middle-1, 2 -> middle+1.
  return indices
    .map((index) => ({ index, distance: Math.abs(index - middle) * 2 + (index < middle ? 1 : 0) }))
    .sort((a, b) => a.distance - b.distance)
    .map((entry) => entry.index);
}

/** Revealed chars as themselves, noise for the rest. */
function scramble(text: string, revealed: Set<number>, alphabet: string[]): string {
  return text
    .split('')
    .map((char, index) => {
      // Spaces stay put, or it reads as text being replaced, not resolving.
      if (char === ' ' || revealed.has(index)) return char;
      return alphabet[Math.floor(Math.random() * alphabet.length)] ?? char;
    })
    .join('');
}
