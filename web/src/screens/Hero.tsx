import { useEffect, useState } from 'react';
import { Button, DecryptedText, Icon, Pill, TeamCorner, TerminalField, TypeSwap, Wordmark } from '../components';
import { HeroArtwork } from '../components/hero/HeroArtwork';
import { BridgeError, call } from '../bridge/client';
import type { SystemProbe } from '../bridge/types';
import { entryReason as say, entryText as t } from '../i18n';
import { color } from '../tokens';
import { navigate } from '../router';

/**
 * The way in: the claim as headline, and under it only bad news. A "ready"
 * badge is noise; a missing hypervisor should show up here, not three
 * clicks later when a machine won't start.
 */

/**
 * Decrypts into "Break the box.", then retypes itself as "Beat the box."
 * That's the joke and the name. Only this line moves; two moving headlines
 * are unreadable.
 *
 * "Beat the box." is the resting state: screenshots, reduced motion and
 * screen readers get it straight away.
 */
function Claim() {
  const from = t('beatbox.hero.accent_from');
  const to = t('beatbox.hero.accent_to');
  const rest = t('beatbox.hero.accent_rest');
  const [resolved, setResolved] = useState(false);

  // Language switch: play it again from the top.
  useEffect(() => setResolved(false), [from, to, rest]);

  // 120ms per char: reads as decrypting, done before anyone reaches for the mouse.
  return resolved ? (
    <>
      <TypeSwap from={from} to={to} />
      {` ${rest}`}
    </>
  ) : (
    <DecryptedText
      text={`${from} ${rest}`}
      animateOn="view"
      speed={160}
      encryptedClassName="bb-scrambled"
      onDone={() => setResolved(true)}
    />
  );
}

export function Hero() {
  const [probe, setProbe] = useState<SystemProbe | null>(null);
  // Why the engine didn't answer; null when it did.
  const [unreached, setUnreached] = useState<string | null>(null);
  // Did the glyph field (WebGL) light up? null = don't know yet. Not a
  // boolean starting at false: that flashed the fallback for a second even
  // where WebGL was about to work.
  const [field, setField] = useState<boolean | null>(null);

  useEffect(() => {
    let live = true;
    // One call: what we report, and proof the host answers at all.
    call('system.probe')
      .then((system) => { if (live) setProbe(system); })
      .catch((error: unknown) => {
        if (!live) return;
        setUnreached(error instanceof BridgeError ? say(error.reason, error.message) : String(error));
      });
    return () => { live = false; };
  }, []);

  // Machines need a hypervisor, so that's the one thing worth warning about.
  const hypervisorReady = probe?.qemu.ready ?? false;

  return (
    // No top bar: this screen is a picture. The wordmark sits in the copy instead.
    <div className={`bb-hero${field === true ? ' bb-hero--field' : ''}${field === false ? ' bb-hero--nofield' : ''}`}>
      <TerminalField tint={color.violet} onLive={setField} />

      <div className="bb-hero-copy">
        <Wordmark large />
        <h1 className="bb-t-hero bb-hero-title">
          {t('beatbox.hero.title')}
          <span><Claim /></span>
        </h1>
        <p className="bb-hero-lede">{t('beatbox.hero.lede')}</p>

        {/* Straight in as guest. Account.tsx waits for real sign-in. */}
        <div className="bb-hero-actions">
          <Button variant="primary" trailingIcon="arrow" onClick={() => navigate('host/machine')}>
            {t('beatbox.hero.cta')}
          </Button>
        </div>

        <div className="bb-hero-state">
          {unreached !== null ? (
            <Pill tone="rose">{t('beatbox.hero.pill.offline')}</Pill>
          ) : (
            <>
              {probe && !hypervisorReady ? (
                <Pill tone="rose">{t('beatbox.hero.pill.qemu_down')}</Pill>
              ) : null}
              <span className="bb-hero-note">
                <Icon name="lock" size="xs" />
                {t('beatbox.hero.pill.isolated')}
              </span>
            </>
          )}
        </div>
        {unreached !== null ? <p className="bb-hero-why">{unreached}</p> : null}
        {/* Just the warning; Setup explains why and offers the fix. */}
        {probe && !hypervisorReady ? (
          <div className="bb-hero-detail">
            <Button size="sm" trailingIcon="arrow" onClick={() => navigate('setup')}>{t('beatbox.hero.setup')}</Button>
          </div>
        ) : null}
      </div>

      <HeroArtwork />

      <TeamCorner />
    </div>
  );
}
