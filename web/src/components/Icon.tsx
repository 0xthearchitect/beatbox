import type { ReactNode } from 'react';

/** The icon set. Stroked, 24×24, one visual weight — no filled shapes. */

const PATHS = {
  sandbox: 'M12 2.8l9 4.9-9 4.9-9-4.9zM3 12.8l9 4.9 9-4.9',
  // Sliders, not a cog: our cog read as a sun.
  settings: 'M3.8 8h4.4M12.2 8H20.2M3.8 16h7.6M15.6 16h4.6',
  user: 'M4.8 20a7.4 7.4 0 0 1 14.4 0',
  arrow: 'M4.5 12h14M12.5 6l6 6-6 6',
  back: 'M19.5 12h-14M11.5 6l-6 6 6 6',
  check: 'M5.5 12.4l4.3 4.3L18.5 8',
  clock: 'M12 7.2V12l3.1 2',
  trash: 'M4 6.6h16M9.2 6.6V3.8h5.6v2.8M6.6 6.6l1 13.6h8.8l1-13.6',
  copy: 'M9.4 9.4h9.2v9.2H9.4zM5.4 14.6V5.4h9.2',
  paste: 'M9 4.6H6.6v15.8h10.8V4.6H15M9 4.6V3.4h6v1.2M9 4.6h6M9.6 11h4.8M9.6 14.6h4.8',
  broom: 'M5 19.4l5.4-5.4M9.2 9.2l5.6 5.6M14.2 4.2l5.6 5.6-4 4-5.6-5.6z',
  save: 'M4.8 3.4h11.4l3.8 3.8v13.4H4.8zM8.4 3.4v5.8h7.2V3.4M8.4 20.6v-5.6h7.2v5.6',
  // A machine's display, opened in its own window.
  monitor: 'M8.5 20.6h7M12 16.8v3.8',
  star: 'm12 3.3 2.72 5.5 6.08.89-4.4 4.28 1.04 6.04L12 17.15l-5.44 2.86 1.04-6.04-4.4-4.28 6.08-.89z',
  shield: 'M12 2.6 20 6v6.1c0 4.9-3.4 8.4-8 9.4-4.6-1-8-4.5-8-9.4V6zM9 12l2.2 2.2L15.2 10',
  bulb: 'M9.3 17.6h5.4M10 20.6h4M12 3.2a5.6 5.6 0 0 1 3.4 10.1c-.6.5-.9 1.1-.9 1.8H9.5c0-.7-.3-1.3-.9-1.8A5.6 5.6 0 0 1 12 3.2',
  terminal: 'M6.5 9.5l3 2.5-3 2.5M12.5 15.5h5',
  network: 'M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3M12 12V8',
  cpu: 'M9 1.9v2.3M15 1.9v2.3M9 19.8v2.3M15 19.8v2.3M1.9 9h2.3M1.9 15h2.3M19.8 9h2.3M19.8 15h2.3',
  logs: 'M8.5 6h11M8.5 12h11M8.5 18h11M4.5 6h.01M4.5 12h.01M4.5 18h.01',
  box: 'M12 2.6l8.4 4.9v9.8L12 22.2l-8.4-4.9V7.5zM3.6 7.5l8.4 4.9 8.4-4.9M12 12.4v9.8',
  lock: 'M8 10.2V7.2a4 4 0 0 1 8 0v3',
  plus: 'M12 5.2v13.6M5.2 12h13.6',
  minus: 'M5.2 12h13.6',
  alert: 'M12 3.4 2.8 20.2h18.4zM12 9.8v4.1M12 17.2h.01',
  eye: 'M2.2 12s3.9-6.4 9.8-6.4S21.8 12 21.8 12s-3.9 6.4-9.8 6.4S2.2 12 2.2 12',
  code: 'M8.8 8l-4.6 4 4.6 4M15.2 8l4.6 4-4.6 4',
  play: 'M8.2 5.4 18.6 12 8.2 18.6z',
  // Arc left open on purpose: a full circle reads as "loading".
  refresh: 'M20.2 12a8.2 8.2 0 1 1-2.4-5.8M20.4 4.4v5.4h-5.4',
  // Snowflake for freeze: a pause sign didn't say "kept exactly as it was".
  freeze: 'M12 2.8V21.2M14.3 3.6 12 5.8 9.7 3.6M14.3 20.4 12 18.2 9.7 20.4M4 7.4l16 9.2M5.9 5.8l.7 3.1-3 .9M20.4 14.2l-3 .9.7 3.1M4 16.6l16-9.2M3.6 14.2l3 .9-.7 3.1M18.1 5.8l-.7 3.1 3 .9',
  stop: '',
  search: 'M15.6 15.6l4.6 4.6',
  pulse: 'M2.6 12.5h4L9 7l3.4 10 2.4-4.5h6',
  close: 'M6.6 6.6l10.8 10.8M17.4 6.6 6.6 17.4',
  up: 'M12 19.4V5.2M5.8 11.4 12 5.2l6.2 6.2',
  file: 'M13.4 3.4H6.8a1.4 1.4 0 0 0-1.4 1.4v14.4a1.4 1.4 0 0 0 1.4 1.4h10.4a1.4 1.4 0 0 0 1.4-1.4V8.4zM13.4 3.4v5h5',
  folder: 'M3.4 19.2V6.2a1.4 1.4 0 0 1 1.4-1.4h4.1l2.3 2.7h8.4a1.4 1.4 0 0 1 1.4 1.4v10.3a1.4 1.4 0 0 1-1.4 1.4H4.8a1.4 1.4 0 0 1-1.4-1.4z',
  bridge: 'M3 16.5V13a9 9 0 0 1 18 0v3.5M3 16.5h18M8 16.5v-4M16 16.5v-4M12 16.5v-6',
  // The router a network goes out through.
  router: 'M7.2 16.6h.01M10.6 16.6h.01M14 16.6h3M8 13 6.2 6.6M16 13l1.8-6.4',
  // The panel with its rail, not an arrow: an arrow would need flipping with the state.
  rail: 'M9.4 4.2v15.6',
  usb: 'M8.6 2.8h6.8v5.4H8.6zM10.8 5.2h.01M13.2 5.2h.01M6.8 8.2h10.4v9.6a3.4 3.4 0 0 1-3.4 3.4h-3.6a3.4 3.4 0 0 1-3.4-3.4z',
  // Kinds of step, and a line that forks.
  bolt: 'M13.4 2.6 5.4 13.4h6l-1 8 8-10.8h-6z',
  listen: 'M12 12h.01M8.6 8.6a4.8 4.8 0 0 0 0 6.8M15.4 8.6a4.8 4.8 0 0 1 0 6.8M5.2 5.2a9.6 9.6 0 0 0 0 13.6M18.8 5.2a9.6 9.6 0 0 1 0 13.6',
  diamond: 'M12 2.8l9.2 9.2-9.2 9.2L2.8 12z',
  flag: 'M5.4 21.2V3.6h11.4l-2.2 4.4 2.2 4.4H5.4',
  pencil: 'M4.6 19.4l.9-4.1L16.4 4.4a1.9 1.9 0 0 1 2.7 0l.5.5a1.9 1.9 0 0 1 0 2.7L8.7 18.5zM14.2 6.6l3.2 3.2',
  branch: 'M6 3.4v12M18 8.6a9.4 9.4 0 0 1-9.4 9.4',
  // Placeholder until whoever drew the network icons draws this one.
  server: 'M7 7h.01M7 17h.01',
  // Service protocol icons on the Server page.
  globe: 'M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18',
  book: 'M5 4h12a2 2 0 0 1 2 2v14H7a2 2 0 0 1-2-2zM5 18a2 2 0 0 1 2-2h12M9 8h6M9 11h4',
  database: 'M4 6c0-4 16-4 16 0s-16 4-16 0v12c0 4 16 4 16 0V6M4 12c0 4 16 4 16 0',
  mail: 'M3 5h18v14H3zM3 5l9 7 9-7',
  puzzle: 'M10 3h4v3a2 2 0 1 0 4 0V3h3v7h-3a2 2 0 1 0 0 4h3v7h-7v-3a2 2 0 1 0-4 0v3H3v-7h3a2 2 0 1 0 0-4H3V3z',
  // Firewall icons: machine group, reply, service name.
  group: 'M3.5 19c.6-3 2.8-4.8 5.5-4.8s4.9 1.8 5.5 4.8M16.5 14.2c2.2 0 3.8 1.5 4.2 4',
  reply: 'M9 7 4 12l5 5M4 12h10a6 6 0 0 1 6 6',
  tag: 'M3.5 12.5V4.5h8l9 9-8 8zM8 8h.01',
  info: 'M12 11v5.4M12 7.8h.01',
} as const;

/** Circles and rects the path strings can't do. */
const EXTRAS: Partial<Record<IconName, ReactNode>> = {
  router: <rect x="3.2" y="13" width="17.6" height="7.2" rx="1.8" />,
  clock: <circle cx="12" cy="12" r="8.6" />,
  info: <circle cx="12" cy="12" r="8.6" />,
  user: <circle cx="12" cy="8.4" r="3.6" />,
  terminal: <rect x="2.6" y="4" width="18.8" height="16" rx="2.6" />,
  network: (
    <>
      <rect x="9" y="2.4" width="6" height="5.6" rx="1.4" />
      <rect x="2.4" y="16" width="6" height="5.6" rx="1.4" />
      <rect x="15.6" y="16" width="6" height="5.6" rx="1.4" />
    </>
  ),
  cpu: (
    <>
      <rect x="4.2" y="4.2" width="15.6" height="15.6" rx="3" />
      <rect x="8.2" y="8.2" width="7.6" height="7.6" rx="1.4" />
    </>
  ),
  branch: (
    <>
      <circle cx="18" cy="6" r="2.6" />
      <circle cx="6" cy="18" r="2.6" />
    </>
  ),
  lock: <rect x="4.4" y="10.2" width="15.2" height="10.4" rx="2.6" />,
  rail: <rect x="3" y="4.2" width="18" height="15.6" rx="2.6" />,
  eye: <circle cx="12" cy="12" r="2.7" />,
  stop: <rect x="5.6" y="5.6" width="12.8" height="12.8" rx="2.4" />,
  monitor: <rect x="3" y="3.4" width="18" height="13.4" rx="2.2" />,
  search: <circle cx="10.8" cy="10.8" r="6.4" />,
  server: (
    <>
      <rect x="3" y="4" width="18" height="6" rx="1.5" />
      <rect x="3" y="14" width="18" height="6" rx="1.5" />
    </>
  ),
  globe: <circle cx="12" cy="12" r="9" />,
  group: (
    <>
      <circle cx="9" cy="8.5" r="3" />
      <circle cx="16.5" cy="9.5" r="2.4" />
    </>
  ),
  settings: (
    <>
      <circle cx="10.2" cy="8" r="2" />
      <circle cx="13.6" cy="16" r="2" />
    </>
  ),
};

export type IconName = keyof typeof PATHS;
export type IconSize = 'xs' | 'sm' | 'md' | 'lg';

const SIZES: Record<IconSize, number> = { xs: 13, sm: 15, md: 18, lg: 22 };

export function Icon({ name, size = 'md' }: { name: IconName; size?: IconSize }) {
  const px = SIZES[size];
  return (
    <svg
      width={px}
      height={px}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={size === 'xs' ? 1.9 : 1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flex: 'none' }}
      aria-hidden="true"
    >
      {EXTRAS[name]}
      {PATHS[name] ? <path d={PATHS[name]} /> : null}
    </svg>
  );
}
