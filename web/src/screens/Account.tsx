import { useState } from 'react';
import type { KeyboardEvent } from 'react';
import { Button, Cube, CubeWire, Wordmark } from '../components';
import { entryText as t } from '../i18n';
import { navigate, useRouteParams } from '../router';
import { DEFAULT_ROUTE, routeFor } from '../routes';

/**
 * After the hero: who's at the keyboard? Three cubes, one works today.
 *
 * Guest is the whole product right now: everything stays in this install's
 * folders, nothing counted or capped.
 *
 * Sign in / sign up are drawn but do nothing, on purpose. There's no server
 * yet, but a screen with one option isn't a choice. They're wireframe cubes
 * ("modelled, not running") with `beatbox.account.soon` so it doesn't read as
 * a bug.
 */

export type PickId = 'guest' | 'signin' | 'signup';

interface Pick {
  readonly id: PickId;
  /** Clickable? Only guest, for now. */
  readonly live: boolean;
}

/** Wiring accounts up later? Flip `live`; everything else here reads only that. */
export const PICKS: readonly Pick[] = [
  { id: 'guest', live: true },
  { id: 'signin', live: false },
  { id: 'signup', live: false },
];

export function Account() {
  const params = useRouteParams();
  // Start on guest: with nothing lit the description panel would be empty.
  const [picked, setPicked] = useState<PickId>('guest');

  // `next` comes from the hash, so anyone can type it. routeFor() falls back
  // to the default for junk, so the worst case is the home screen.
  const next = routeFor(params.get('next') ?? DEFAULT_ROUTE).path;

  const enter = (pick: Pick) => {
    if (pick.live) navigate(next);
  };

  // Arrow keys move focus; the highlight follows focus. Tab still works.
  const steer = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
    if (!step) return;
    const buttons = Array.from(event.currentTarget.querySelectorAll('button'));
    const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (at < 0) return;
    event.preventDefault();
    buttons[(at + step + buttons.length) % buttons.length]?.focus();
  };

  return (
    <div className="bb-account">
      {/* No chrome and no history here, so without this the hero is one-shot. */}
      <span className="bb-account-back">
        <Button size="sm" icon="back" onClick={() => navigate('hero')}>
          {t('beatbox.account.back')}
        </Button>
      </span>

      <div className="bb-account-copy">
        <Wordmark large />
        <h1 className="bb-t-title bb-account-title">{t('beatbox.screen.account.title')}</h1>
        <p className="bb-account-lede">{t('beatbox.screen.account.blurb')}</p>
      </div>

      {/* Buttons, not a radiogroup: nothing is submitted. `aria-disabled`,
          not `disabled`, so the dead ones stay tabbable and readable. */}
      <div className="bb-ways" onKeyDown={steer}>
        {PICKS.map((pick) => (
          <button
            key={pick.id}
            type="button"
            className={`bb-way${picked === pick.id ? ' on' : ''}${pick.live ? '' : ' bb-way--soon'}`}
            aria-describedby={`bb-way-what-${pick.id}`}
            aria-disabled={pick.live ? undefined : true}
            onMouseEnter={() => setPicked(pick.id)}
            onFocus={() => setPicked(pick.id)}
            onClick={() => enter(pick)}
          >
            <span className="bb-way-cube">
              <span className="bb-way-spin">
                {pick.live
                  ? <Cube fill tone="violet" />
                  : <CubeWire fill tone="violet" opacity={0.55} />}
              </span>
            </span>
            <b className="bb-way-name">{t(`beatbox.account.${pick.id}`)}</b>
            {pick.live ? null : <span className="bb-way-soon">{t('beatbox.account.soon')}</span>}
          </button>
        ))}
      </div>

      {/* All three stacked in one grid cell, only the chosen one painted, so
          the box fits the longest and nothing jumps. Hidden ones stay in the
          a11y tree: each cube's `aria-describedby` points at its own. */}
      <div className="bb-way-what">
        {PICKS.map((pick) => (
          <p key={pick.id} id={`bb-way-what-${pick.id}`} className={picked === pick.id ? 'on' : undefined}>
            {t(`beatbox.account.${pick.id}.what`)}
          </p>
        ))}
      </div>
    </div>
  );
}
