import { cubeTone, type CubeTone } from '../tokens';

/**
 * A machine: one isometric cube in four tints, wireframe = "modelled, not running".
 *
 * One hexagon at every size, so the topology node, card art and empty states
 * are the same drawing, not near-copies. Not the logo; that never changes colour.
 */

const HEX = '0,-45 38.97,-22.5 38.97,22.5 0,45 -38.97,22.5 -38.97,-22.5';
const TOP = '0,-45 38.97,-22.5 0,0 -38.97,-22.5';
const LEFT = '-38.97,-22.5 0,0 0,45 -38.97,22.5';
const RIGHT = '38.97,-22.5 0,0 0,45 38.97,22.5';
const VIEW_BOX = '-56 -60 112 120';

export function Cube({ size, tone = 'violet', glow = false, fill = false }: {
  size?: number; tone?: CubeTone; glow?: boolean;
  /** Fill the parent instead of taking a pixel size, so CSS can scale it. */
  fill?: boolean;
}) {
  const face = cubeTone[tone];
  const box = fill ? { width: '100%', height: '100%' } : { width: size, height: size, flex: 'none' as const };
  return (
    <svg viewBox={VIEW_BOX} style={box} aria-hidden="true">
      {glow ? <circle cx="0" cy="0" r="54" fill={face.left} opacity="0.14" /> : null}
      <polygon points={TOP} fill={face.top} />
      <polygon points={LEFT} fill={face.left} />
      <polygon points={RIGHT} fill={face.right} />
      <polygon points={HEX} fill="none" stroke={face.top} strokeOpacity="0.5" strokeWidth="1.5" />
    </svg>
  );
}

export function CubeWire({ size, tone = 'violet', opacity = 0.5, fill = false }: {
  size?: number; tone?: CubeTone; opacity?: number; fill?: boolean;
}) {
  const face = cubeTone[tone];
  const box = fill
    ? { width: '100%', height: '100%', opacity }
    : { width: size, height: size, flex: 'none' as const, opacity };
  return (
    <svg viewBox={VIEW_BOX} style={box} aria-hidden="true">
      <polygon points={HEX} fill="none" stroke={face.top} strokeWidth="1.6" />
      {/* Three inner edges, not four: the fourth (centre to top) is the hidden
          back edge and turns it into a cube seen from inside. */}
      <path d="M0,0 38.97,-22.5M0,0 -38.97,-22.5M0,0v45" fill="none" stroke={face.top} strokeWidth="1.2" />
    </svg>
  );
}
