import { useId, type CSSProperties } from 'react';
import { Logo } from '../Logo';
import { impacts, shieldEdges, shieldNodes } from './geometry';

const fraction = (seed: number) => {
  const value = Math.sin(seed * 127.1 + 311.7) * 43758.5453;
  return value - Math.floor(value);
};

/** One red cube, drawn isometric so it reads as the logo's enemy. */
function Threat({ size }: { size: number }) {
  const w = size * 0.87, h = size / 2;
  return (
    <g>
      <path d={`M0 ${-size}L${w} ${-h}L0 0L${-w} ${-h}Z`} fill="#ff97a6" />
      <path d={`M${-w} ${-h}L0 0L0 ${size}L${-w} ${h}Z`} fill="#f4566e" />
      <path d={`M0 0L${w} ${-h}L${w} ${h}L0 ${size}Z`} fill="#b8304a" />
    </g>
  );
}

/** The logo inside a geodesic shield, with red cubes breaking up against it.
 * Thin lines and solid fills only: blur and glow read as generated art.
 * Animation is CSS only, so it still shows without WebGL or with reduced motion. */
export function HeroArtwork() {
  const id = useId().replace(/:/g, '');
  return (
    <div className="bb-hero-art" aria-hidden="true">
      <svg className="bb-hero-back" viewBox="0 0 1000 780" fill="none" focusable="false">
        <g stroke="#8f7cff" strokeWidth="1">
          {shieldEdges.map((e, i) => (
            <path key={i} d={e.d} strokeOpacity={e.z > 0 ? (0.4 + e.z * 0.3).toFixed(2) : '0.14'} />
          ))}
        </g>
        {shieldNodes.filter((n) => n.z > 0).map((n, i) => (
          <circle key={i} cx={n.x} cy={n.y} r="2" fill="#c9bdff" />
        ))}
      </svg>

      <span className="bb-hero-anchor"><Logo fill /></span>

      {/* Threats in front of everything, so the debris lands over the shield's rim. */}
      <svg className="bb-hero-front" viewBox="0 0 1000 780" fill="none" focusable="false">
        {impacts.map((hit, i) => {
          const from = { x: hit.x + hit.dx * 340, y: hit.y + hit.dy * 340 };
          const cube = { x: hit.x + hit.dx * 108, y: hit.y + hit.dy * 108 };
          const trail = `${id}-trail-${i}`;
          const push = { '--bb-push-x': `${-hit.dx * 34}px`, '--bb-push-y': `${-hit.dy * 34}px`, animationDelay: `${-i * 1.3}s` } as CSSProperties;
          return (
            <g key={i}>
              <linearGradient id={trail} gradientUnits="userSpaceOnUse" x1={from.x} y1={from.y} x2={hit.x} y2={hit.y}>
                <stop stopColor="#f4566e" stopOpacity="0" /><stop offset="0.7" stopColor="#f4566e" stopOpacity="0.9" />
                <stop offset="1" stopColor="#ff97a6" />
              </linearGradient>
              <path className="bb-hero-trail" d={`M${from.x} ${from.y}L${hit.x + hit.dx * 18} ${hit.y + hit.dy * 18}`}
                stroke={`url(#${trail})`} strokeWidth="2.4" strokeDasharray="3 7" strokeLinecap="round" />
              {/* Spin inside the push, or the push follows the spin off the trail. */}
              <g transform={`translate(${cube.x.toFixed(1)} ${cube.y.toFixed(1)})`}>
                <g className="bb-hero-threat" style={push}>
                  <g transform={`rotate(${(fraction(i) - 0.5) * 30})`}><Threat size={14} /></g>
                </g>
              </g>
              {/* The cube breaks into violet pixels where it meets the shield. */}
              <g>
                {Array.from({ length: 16 }, (_, j) => {
                  const spread = Math.atan2(hit.dy, hit.dx) + (fraction(i * 31 + j) - 0.5) * 2.6;
                  const distance = 8 + fraction(i * 17 + j + 5) ** 1.4 * 74;
                  const size = 2 + fraction(i * 7 + j + 9) * 5;
                  const style = { '--bb-chip-x': `${Math.cos(spread) * 10}px`, '--bb-chip-y': `${Math.sin(spread) * 10}px`, animationDelay: `${-fraction(j + i) * 4}s` } as CSSProperties;
                  return (
                    <rect key={j} className="bb-hero-chip" style={style} width={size} height={size}
                      x={hit.x + Math.cos(spread) * distance - size / 2} y={hit.y + Math.sin(spread) * distance - size / 2}
                      fill={j % 4 ? '#8f7cff' : '#c9bdff'} opacity={(1 - distance / 96).toFixed(2)} />
                  );
                })}
              </g>
            </g>
          );
        })}
      </svg>
    </div>
  );
}
