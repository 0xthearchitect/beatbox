import { useEffect, useId, useRef } from 'react';
import type { CSSProperties, ReactNode, RefObject } from 'react';

/**
 * Chip rows where the selected pill slides over and splashes.
 *
 * From React Bits' GooeyNav (MIT, reactbits.dev), changed:
 * - Doesn't own the selection: it follows the `aria-current` chip, so loads
 *   and navigation animate too, and deleting this leaves the rows working.
 * - Gel via SVG alpha threshold (`feColorMatrix`), not CSS blur+contrast,
 *   which thresholds luminance and needs a black backdrop.
 * - No text clone; `.bb-chip[aria-current]` styles the label.
 * - White droplets behind the white pill, so they never touch the label.
 * - The pill (`.bb-chip-glide`) is our own element so it can slide. As an
 *   attribute-selector background it could only jump.
 */

// Tuned on real chips. The demo's 15 drops at 90px for 0.66s looked like a
// lava lamp here: blobs spilling out of the row.
const COUNT = 10;
/** Start and end distance (px). The gel happens near the end. */
const SPREAD: readonly [number, number] = [28, 4];
const SPIN = 60;
const LIFE = 400;
const VARIANCE = 150;

/** CSS vars so a row can override them. */
const TINTS = ['var(--bb-burst-a)', 'var(--bb-burst-b)'] as const;

function noise(n = 1): number {
  return n / 2 - Math.random() * n;
}

/** A point on the circle with wobble, so bursts don't repeat. */
function around(distance: number, index: number, total: number): [number, number] {
  const angle = ((360 + noise(8)) / total) * index * (Math.PI / 180);
  return [distance * Math.cos(angle), distance * Math.sin(angle)];
}

export function Burst({ hostRef, current }: {
  hostRef: RefObject<HTMLElement | null>;
  /** Anything that changes with the current chip. Only used as a trigger. */
  current: string;
}) {
  const layer = useRef<HTMLSpanElement>(null);
  const glide = useRef<HTMLSpanElement>(null);
  const timers = useRef<number[]>([]);
  // Filter id per row: with a shared id, url() resolves to whichever mounted
  // first and breaks when it unmounts. Colons stripped for url().
  const gel = `bb-goo-${useId().replace(/:/g, '')}`;
  /** null until the first placement, so opening a screen doesn't burst. */
  const placed = useRef<string | null>(null);

  useEffect(() => {
    const host = hostRef.current;
    const element = layer.current;
    if (!host || !element) return;

    const chip = host.querySelector<HTMLElement>('.bb-chip[aria-current="true"]');
    if (!chip) {
      // Nothing selected, nothing drawn.
      element.style.opacity = '0';
      if (glide.current) glide.current.style.opacity = '0';
      placed.current = null;
      return;
    }

    const place = () => {
      const box = chip.getBoundingClientRect();
      const frame = host.getBoundingClientRect();
      element.style.opacity = '1';
      element.style.left = `${box.x - frame.x}px`;
      element.style.top = `${box.y - frame.y}px`;
      element.style.width = `${box.width}px`;
      element.style.height = `${box.height}px`;

      const pill = glide.current;
      if (pill) {
        // No transition on first placement, or the pill slides in from the top-left corner.
        pill.style.transition = placed.current === null ? 'none' : '';
        pill.style.opacity = '1';
        pill.style.transform = `translate(${box.x - frame.x}px, ${box.y - frame.y}px)`;
        pill.style.width = `${box.width}px`;
        pill.style.height = `${box.height}px`;
      }
    };
    place();

    // Rows wrap on narrow windows; watch the box instead of duplicating breakpoints.
    const observer = new ResizeObserver(place);
    observer.observe(host);

    const arrived = placed.current !== null && placed.current !== current;
    placed.current = current;
    if (arrived && !window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      burst(element, timers);
    }

    return () => observer.disconnect();
  }, [current, hostRef]);

  // Clear timers on unmount, or a late droplet touches a removed node.
  useEffect(() => () => {
    timers.current.forEach(window.clearTimeout);
    timers.current = [];
  }, []);

  return (
    <>
      <span className="bb-chip-glide" ref={glide} aria-hidden="true" />
      <span className="bb-burst" ref={layer} aria-hidden="true" style={{ filter: `url(#${gel})` }} />
      {/* Big filter region: the default 120% clips droplets thrown outside the chip. */}
      <svg className="bb-burst-defs" aria-hidden="true" focusable="false">
        <filter id={gel} x="-200%" y="-200%" width="500%" height="500%">
          <feGaussianBlur in="SourceGraphic" stdDeviation="2.4" result="blur" />
          {/* Alpha threshold a * 11 - 4. Any steeper and a fresh 3px droplet
              falls below zero and vanishes until it's nearly home. */}
          <feColorMatrix
            in="blur"
            type="matrix"
            values="1 0 0 0 0  0 1 0 0 0  0 0 1 0 0  0 0 0 11 -4"
          />
        </filter>
      </svg>
    </>
  );
}

function burst(element: HTMLElement, timers: { current: number[] }): void {
  for (let index = 0; index < COUNT; index += 1) {
    const life = LIFE * 2 + noise(VARIANCE * 2);
    const from = around(SPREAD[0], COUNT - index, COUNT);
    const to = around(SPREAD[1] + noise(6), COUNT - index, COUNT);
    const turn = noise(SPIN / 10);

    const particle = document.createElement('span');
    particle.className = 'bb-burst-particle';
    particle.style.setProperty('--from-x', `${from[0]}px`);
    particle.style.setProperty('--from-y', `${from[1]}px`);
    particle.style.setProperty('--to-x', `${to[0]}px`);
    particle.style.setProperty('--to-y', `${to[1]}px`);
    particle.style.setProperty('--life', `${life}ms`);
    particle.style.setProperty('--scale', `${1 + noise(0.2)}`);
    particle.style.setProperty('--turn', `${(turn > 0 ? turn + SPIN / 20 : turn - SPIN / 20) * 10}deg`);

    const point = document.createElement('span');
    point.className = 'bb-burst-point';
    point.style.setProperty('--tint', index % 2 ? TINTS[1] : TINTS[0]);

    particle.appendChild(point);
    element.appendChild(particle);
    timers.current.push(window.setTimeout(() => particle.remove(), life));
  }
}

/** A `.bb-chip` row with the burst. Use it for every chip row, so they all behave the same. */
export function ChipRow({ current, style, children }: {
  current: string;
  style?: CSSProperties;
  children: ReactNode;
}) {
  const row = useRef<HTMLDivElement>(null);
  return (
    <div className="bb-filters" style={style} ref={row}>
      {children}
      <Burst hostRef={row} current={current} />
    </div>
  );
}
