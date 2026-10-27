import { useEffect, useState } from 'react';
import { Modal } from './Modal';
import { Button } from './primitives';
import { Icon } from './Icon';
import { call, on } from '../bridge/client';
import type { ClosingView } from '../bridge/types';
import { t } from '../i18n';

/**
 * "Something is still running": the window asks, the page draws it.
 *
 * A native dialog would look like a second app, so the window holds the
 * close, this shows, and the choice goes back through `host.closing`.
 *
 * Stop / leave running / go back, not yes/no: a throwaway sandbox costs
 * nothing to stop, but someone may have left one running on purpose.
 */
export function Closing() {
  const [asked, setAsked] = useState<ClosingView | null>(null);

  useEffect(() => on('closing', setAsked), []);

  if (!asked) return null;

  const answer = (choice: 'stop' | 'leave' | 'cancel') => {
    setAsked(null);
    // Result ignored: stop/leave close the window anyway, and a failure just
    // leaves it open, which is where we already are.
    void call('host.closing', { choice }).catch(() => undefined);
  };

  return (
    <Modal title={t('beatbox.closing.title')} icon="alert" onClose={() => answer('cancel')}
           footer={
             <>
               <span style={{ flex: 1 }} />
               <Button onClick={() => answer('cancel')}>{t('beatbox.closing.cancel')}</Button>
               <Button variant="danger" onClick={() => answer('leave')}>{t('beatbox.closing.leave')}</Button>
               <Button variant="primary" autofocus onClick={() => answer('stop')}>
                 {t('beatbox.closing.stop')}
               </Button>
             </>
           }>
      <div className="bb-closing">
        {asked.sandboxes ? (
          <p><Icon name="cpu" size="sm" />{t('beatbox.closing.sandboxes', { count: asked.sandboxes })}</p>
        ) : null}
        {/* Called out on its own: an install may be an hour of someone's evening. */}
        {asked.installing ? (
          <p><Icon name="save" size="sm" />{t('beatbox.closing.installing')}</p>
        ) : null}
      </div>
    </Modal>
  );
}
