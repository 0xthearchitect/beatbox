import { cubeTone, type CubeTone } from '../tokens';

/**
 * BeatBox's logo: the cube taken apart, in the violet tints. Same drawing as
 * shell/icons/BeatBox_logo.svg (the app icon), keep them in sync.
 * Only for the brand; machines use Cube, which changes colour. This never does.
 */

const RIGHT = 'M693 376L541.5 638L390 376L541.5 114L693 376ZM541.5 535.323L633.695 376L541.568 216.677L449.372 376L541.5 535.323Z';
const TOP = 'M70.5017 375.309L372.501 375.309L523.501 113.77L221.501 113.769L70.5017 375.309ZM159.266 324.061L342.947 324.119L434.77 165.076L251.09 165.018L159.266 324.061Z';
const BOTTOM = 'M372.501 375.309L70.5017 375.309L221.5 636.849L523.499 636.849L372.501 375.309ZM159.265 426.558L342.945 426.499L434.769 585.542L251.088 585.601L159.265 426.558Z';
/** Square, centred on the drawing (x 70-693, y 114-638). */
const VIEW_BOX = '-8 -14 780 780';

export function Logo({ size, fill = false }: {
  size?: number;
  /** Fill the parent instead of taking a pixel size, so CSS can scale it. */
  fill?: boolean;
}) {
  const face = cubeTone.violet;
  const box = fill ? { width: '100%', height: '100%' } : { width: size, height: size, flex: 'none' as const };
  return (
    <svg viewBox={VIEW_BOX} style={box} aria-hidden="true">
      <path d={RIGHT} fillRule="evenodd" fill={face.right} />
      <path d={TOP} fillRule="evenodd" fill={face.top} />
      <path d={BOTTOM} fillRule="evenodd" fill={face.left} />
    </svg>
  );
}

/** The silhouette in one stroke, gaps closed: hull plus the three inner edges. */
const OUTLINE = 'M221.5 114L532.5 114L693 376L532.5 637.5L221.5 637.5L70.5 375.3Z'
  + 'M70.5 375.3L372.5 375.3L532.5 114M372.5 375.3L532.5 637.5';

/** Wireframe logo for the hero's satellites. Uses OUTLINE, not the real
 *  paths: those draw double lines where the faces stand apart. */
export function LogoWire({ size, tone = 'violet', opacity = 0.5, fill = false }: {
  size?: number; tone?: CubeTone; opacity?: number; fill?: boolean;
}) {
  const stroke = cubeTone[tone].top;
  const box = fill
    ? { width: '100%', height: '100%', opacity }
    : { width: size, height: size, flex: 'none' as const, opacity };
  return (
    <svg viewBox={VIEW_BOX} style={box} aria-hidden="true">
      <path d={OUTLINE} fill="none" stroke={stroke} strokeWidth="10" strokeLinejoin="round" />
    </svg>
  );
}
