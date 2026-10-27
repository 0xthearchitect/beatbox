import { useId } from 'react';

/**
 * A switch whose dot goes through a liquid drop instead of sliding.
 * Based on Aaron Iker's Liquid switch, with changes:
 *
 * - No GSAP: one CSS transition with overshoot easing; the gooey filter
 *   turns the two dots crossing into a merge.
 * - "On" is white, not violet: the accent is never a fill or a state (tokens/index.ts).
 * - Dots are `border-radius: 50%` divs, not SVG circles: a 16px SVG circle
 *   renders as a visible octagon here (Burst.tsx does the same). The <svg>
 *   only holds the filter.
 */
export function Switch({ checked, onChange, id, label }: {
  checked: boolean;
  onChange: (next: boolean) => void;
  id?: string;
  /** For screen readers; the visible label sits beside it. */
  label?: string;
}) {
  const gel = `bb-switch-goo-${useId().replace(/:/g, '')}`;
  return (
    <span className="bb-switch">
      <input
        type="checkbox"
        id={id}
        checked={checked}
        aria-label={label}
        onChange={(event) => onChange(event.target.checked)}
      />
      <span className="bb-switch-art" style={{ filter: `url(#${gel})` }}>
        <span className="bb-switch-off" />
        <span className="bb-switch-on" />
      </span>
      <svg className="bb-burst-defs" aria-hidden="true" focusable="false">
        {/* Softer blur than Burst's: at 38x24 that one smears the switch into a blob. */}
        <filter id={gel} x="-40%" y="-60%" width="180%" height="220%" colorInterpolationFilters="sRGB">
          <feGaussianBlur in="SourceGraphic" stdDeviation="1.6" result="blur" />
          <feColorMatrix in="blur" type="matrix" values="1 0 0 0 0  0 1 0 0 0  0 0 1 0 0  0 0 0 20 -7" />
        </filter>
      </svg>
    </span>
  );
}
