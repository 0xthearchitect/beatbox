import { useState } from 'react';
import logo from '../assets/reinvicta.svg';
import { t } from '../i18n';

/**
 * Who made this, in the welcome window's corner.
 *
 * animate-ui's avatar group, rebuilt without Tailwind. Faces are bundled
 * files: our CSP (`img-src 'self' data:` in vite.config.ts) blocks remote images.
 *
 * `import.meta.glob`, not static imports: a member with no picture yet gets
 * initials instead of a broken build.
 */

const MEMBERS = [
  { handle: 'TheArchitect', initials: 'TA' },
  { handle: 'ZeroDot0ne', initials: 'Z0' },
  { handle: 'DevilBoy', initials: 'DB' },
  { handle: '0xAdamastor', initials: '0x' },
] as const;

/** What's in the folder, keyed by lower-cased basename. */
const FACES: Record<string, string> = Object.fromEntries(
  Object.entries(
    import.meta.glob('../assets/team/*.{png,jpg,jpeg,webp}', {
      eager: true, query: '?url', import: 'default',
    }) as Record<string, string>,
  ).map(([path, url]) => [
    path.split('/').pop()!.replace(/\.\w+$/, '').replace(/^avatar-/, '').toLowerCase(),
    url,
  ]),
);

export function TeamCorner() {
  const [over, setOver] = useState<string | null>(null);

  return (
    <div className="bb-corner">
      <div
        className="bb-team"
        role="group"
        aria-label={t('beatbox.hero.team')}
        onMouseLeave={() => setOver(null)}
      >
        {MEMBERS.map(({ handle, initials }) => {
          const face = FACES[handle.toLowerCase()];
          return (
            <span
              key={handle}
              className={`bb-face${over === handle ? ' on' : ''}`}
              onMouseEnter={() => setOver(handle)}
            >
              {face
                ? <img src={face} alt={handle} draggable={false} />
                : <b aria-hidden="true">{initials}</b>}
              {/* aria-hidden: the img alt and the group label already say the name. */}
              <span className="bb-face-name" aria-hidden="true">{handle}</span>
            </span>
          );
        })}
      </div>
      <img className="bb-corner-mark" src={logo} alt="Reinvicta" draggable={false} />
    </div>
  );
}
