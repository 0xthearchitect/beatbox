/**
 * Design tokens, the one source.
 *
 * No colour, radius or font literals anywhere else in src/. Components read
 * the CSS variables cssVariables() emits from here.
 */

/**
 * Backgrounds and text. Neutral on purpose: colour only comes from the
 * accent and the three states. Bluish neutrals made everything look navy.
 * Blacks lean a hair cool so they don't read brown; hairlines are white at
 * low alpha so they match on every surface.
 */
export const color = {
  void: '#050506',
  surface: '#0A0A0C',
  raised: '#101013',
  hover: '#17171B',
  sunken: '#242429',

  line: 'rgba(255, 255, 255, 0.07)',
  lineStrong: 'rgba(255, 255, 255, 0.15)',

  text: '#F2F2F5',
  textMuted: '#9C9CA6',
  // Keeps 4.5:1 contrast on every background up to hover. Don't go lighter.
  textFaint: '#7F7F87',

  /** Primary buttons: white, not violet. Violet on black shouts; it's kept for focus and selection. */
  action: '#F2F2F5',
  actionLift: '#FFFFFF',
  actionInk: '#08080A',

  /** The one accent. Marks and selection — never a fill, never state. */
  violet: '#6E5BFF',
  violetLift: '#8F7CFF',
  violetWash: 'rgba(110, 91, 255, 0.18)',

  /** State colours, kept apart from the accent on purpose. */
  mint: '#3FD9A4',
  mintWash: 'rgba(63, 217, 164, 0.15)',
  amber: '#F2B84B',
  amberWash: 'rgba(242, 184, 75, 0.15)',
  rose: '#F4566E',
  roseWash: 'rgba(244, 86, 110, 0.15)',
  sky: '#4EA8F5',
  skyWash: 'rgba(78, 168, 245, 0.15)',

  /** Darker than any panel, so the terminal reads as a hole. */
  terminal: '#000000',
} as const;

export type ColorToken = keyof typeof color;

/** Crisp corners: rounder read as a consumer app. */
export const radius = {
  sm: '6px',
  md: '10px',
  lg: '14px',
  xl: '18px',
  pill: '999px',
} as const;

/** 4px steps. Anything in between is a mistake. */
export const space = {
  '1': '4px',
  '2': '8px',
  '3': '12px',
  '4': '16px',
  '5': '22px',
  '6': '30px',
  '7': '44px',
} as const;

export const font = {
  display: "'Sora', system-ui, -apple-system, sans-serif",
  body: "'IBM Plex Sans', system-ui, -apple-system, sans-serif",
  mono: "'IBM Plex Mono', ui-monospace, 'Cascadia Mono', monospace",
} as const;

/** Sora for headings, Plex Sans for reading, Plex Mono for machine output (IPs, ports, paths). */
export type TypeStep = {
  readonly family: keyof typeof font;
  readonly size: string;
  readonly weight: number;
  readonly leading: number;
  readonly tracking: string;
  readonly use: string;
};

export const typeScale = {
  hero: {
    family: 'display', size: '52px', weight: 700, leading: 1.08, tracking: '-0.03em',
    use: 'the hero, once per product',
  },
  title: {
    family: 'display', size: '24px', weight: 600, leading: 1.2, tracking: '-0.02em',
    use: 'the one thing a screen is about',
  },
  section: {
    family: 'display', size: '15.5px', weight: 600, leading: 1.3, tracking: '0',
    use: 'a group of things below it',
  },
  body: {
    family: 'body', size: '13px', weight: 400, leading: 1.5, tracking: '0',
    use: 'anything written for a person to read',
  },
  label: {
    family: 'body', size: '11px', weight: 600, leading: 1.4, tracking: '0.13em',
    use: 'uppercase labels over a group',
  },
  data: {
    family: 'mono', size: '12.3px', weight: 400, leading: 1.6, tracking: '0',
    use: 'anything a machine produced',
  },
} as const satisfies Record<string, TypeStep>;

export type TypeToken = keyof typeof typeScale;

/** Cube tints (components/Cube.tsx). The logo uses the violet one. */
export const cubeTone = {
  violet: { top: '#9C8BFF', left: '#5B4AD6', right: '#38299B' },
  mint: { top: '#5FEBBA', left: '#2FB489', right: '#1A6E55' },
  sky: { top: '#7CC6FF', left: '#3E86D6', right: '#25538F' },
  /** Grey = no state. A blue one looked like a fourth state. */
  slate: { top: '#8B8B95', left: '#4B4B55', right: '#2C2C34' },
} as const;

export type CubeTone = keyof typeof cubeTone;

/** The network trace's five answers. */
export type Verdict = 'ALLOWED' | 'BLOCKED' | 'FILTERED' | 'UNREACHABLE' | 'INSUFFICIENT';

/**
 * ALLOWED is rose on purpose: in a sandbox, "reachable" is the finding.
 * INSUFFICIENT gets no tint: it's the engine refusing to guess, not a verdict.
 * Record<Verdict> makes a new verdict without a tone a compile error.
 */
export const verdictTone: Record<Verdict, ColorToken> = {
  ALLOWED: 'rose',
  FILTERED: 'amber',
  BLOCKED: 'mint',
  UNREACHABLE: 'mint',
  INSUFFICIENT: 'textFaint',
};

const kebab = (name: string) => name.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`);

/** Every token as a CSS custom property. */
export function cssVariables(): string {
  const groups: Record<string, Record<string, string>> = {
    color,
    radius,
    space,
    font,
  };
  const lines: string[] = [];
  for (const [group, tokens] of Object.entries(groups)) {
    for (const [name, value] of Object.entries(tokens)) {
      lines.push(`--bb-${group}-${kebab(name)}:${value};`);
    }
  }
  for (const [name, step] of Object.entries(typeScale)) {
    const key = kebab(name);
    lines.push(`--bb-type-${key}-family:var(--bb-font-${kebab(step.family)});`);
    lines.push(`--bb-type-${key}-size:${step.size};`);
    lines.push(`--bb-type-${key}-weight:${step.weight};`);
    lines.push(`--bb-type-${key}-leading:${step.leading};`);
    lines.push(`--bb-type-${key}-tracking:${step.tracking};`);
  }
  return `:root{${lines.join('')}}`;
}

/** One class per type step, generated so a new step only means editing `type`. */
export function typeClasses(): string {
  return Object.keys(typeScale)
    .map((name) => {
      const key = kebab(name);
      return (
        `.bb-t-${key}{` +
        `font-family:var(--bb-type-${key}-family);` +
        `font-size:var(--bb-type-${key}-size);` +
        `font-weight:var(--bb-type-${key}-weight);` +
        `line-height:var(--bb-type-${key}-leading);` +
        `letter-spacing:var(--bb-type-${key}-tracking);}`
      );
    })
    .join('');
}

/** For computed styles in TSX. */
export const cssVar = (group: 'color' | 'radius' | 'space' | 'font', name: string) =>
  `var(--bb-${group}-${kebab(name)})`;
