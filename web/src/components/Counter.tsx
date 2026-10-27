import { useEffect, useRef } from 'react';

/**
 * A number that rolls to its new value, odometer style.
 *
 * From React Bits' Counter (MIT, reactbits.dev), changed:
 * - No framer-motion (50 KB for one odometer). One hand-rolled spring for
 *   the whole number, not one per digit.
 * - Writes `translateY` directly; nothing re-renders while it rolls.
 * - Inherits font and colour, so it can replace a `<b>`. Edges fade with a
 *   mask, not gradients that must match the background.
 *
 * Use it for numbers that change while someone watches, not static properties.
 */

// Damping ratio ~0.94: firm, settles without rocking. The spring runs 0 to 1,
// so every counter takes the same half-second whatever the number.
const STIFFNESS = 190;
const DAMPING = 26;
/** Stop within 0.1% of done. */
const SETTLED = 0.001;

/** 1345 -> [1000, 100, 10, 1]. */
function placesFor(value: number): number[] {
  const digits = Math.max(1, Math.floor(Math.abs(value)).toString().length);
  return Array.from({ length: digits }, (_, index) => 10 ** (digits - index - 1));
}

/**
 * Place every wheel `part` of the way from `from` to `to`.
 *
 * Don't "simplify" to `(value / place) % 10`: 1345 would leave the tens at
 * 4.5 and the hundreds at 3.45, stuck between digits forever. Each wheel
 * gets whole-number ends, `floor(from/place)` to `floor(to/place)`, so all
 * land on integers.
 */
function paint(root: HTMLElement, from: number, to: number, part: number): void {
  for (const wheel of root.querySelectorAll<HTMLElement>('.bb-counter-wheel')) {
    const place = Number(wheel.dataset.place);
    const start = Math.floor(from / place);
    const end = Math.floor(to / place);
    // JS % keeps the sign; force it positive.
    const at = (((start + (end - start) * part) % 10) + 10) % 10;
    for (let number = 0; number < wheel.children.length; number += 1) {
      let offset = (10 + number - at) % 10;
      // Shortest way round: 9 -> 0 rolls forward one, not back nine.
      if (offset > 5) offset -= 10;
      (wheel.children[number] as HTMLElement).style.transform = `translateY(${offset * 100}%)`;
    }
  }
}

export function Counter({ value, countIn = false, places }: {
  value: number;
  /** Roll up from zero on first render. */
  countIn?: boolean;
  /** Fixed wheels, so the width doesn't jump as the number grows. */
  places?: number[];
}) {
  const root = useRef<HTMLSpanElement>(null);
  /** Where the wheels really are; React's value is only the target. */
  const shown = useRef(countIn ? 0 : value);
  const wheels = places ?? placesFor(value);

  useEffect(() => {
    const element = root.current;
    if (!element) return;

    const from = shown.current;
    if (from === value || window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      shown.current = value;
      paint(element, value, value, 1);
      return;
    }

    // Spring the progress, not the number: every wheel lands at the same moment.
    let part = 0;
    let velocity = 0;
    let last = performance.now();
    let frame = requestAnimationFrame(function step(now: number) {
      // Cap the step: after a hidden window one huge step would fling the wheels past.
      const seconds = Math.min((now - last) / 1000, 1 / 30);
      last = now;
      velocity += ((1 - part) * STIFFNESS - velocity * DAMPING) * seconds;
      part += velocity * seconds;
      shown.current = from + (value - from) * part;

      if (Math.abs(1 - part) < SETTLED && Math.abs(velocity) < SETTLED) {
        shown.current = value;
        paint(element, value, value, 1);
        return;
      }
      paint(element, from, value, part);
      frame = requestAnimationFrame(step);
    });
    return () => cancelAnimationFrame(frame);
  }, [value]);

  return (
    <span className="bb-counter" ref={root}>
      {wheels.map((place) => (
        <span className="bb-counter-wheel" key={place} data-place={place}>
          {Array.from({ length: 10 }, (_, number) => (
            <span className="bb-counter-number" key={number}>{number}</span>
          ))}
        </span>
      ))}
      {/* For screen readers. */}
      <span className="bb-counter-said">{value}</span>
    </span>
  );
}
