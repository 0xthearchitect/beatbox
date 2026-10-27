import { useState } from 'react';
import { Button, Card, CubeWire, FilePicker, Icon, Info } from '../components';
import { call } from '../bridge/client';
import type { ReasonView } from '../bridge/types';
import { say, t } from '../i18n';
import { navigate } from '../router';
import { useChosenMachine } from '../chosen';

/**
 * Files into a machine the way people already know: plug in a pen.
 *
 * A local folder shows up as a USB drive (`media.pen`, QEMU's `vvfat`,
 * read-only at the block layer). The guest can read it but never write,
 * change or delete, which is why it's safe even for an offline sample box.
 * One pen at a time, 500 MB max, and never into a machine marked dangerous.
 */

const bag = (value: unknown): Record<string, unknown> =>
  (value && typeof value === 'object' ? value as Record<string, unknown> : {});

export function Pen() {
  const { list, machine, reload } = useChosenMachine();
  const [picking, setPicking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState<{ ok: boolean; text: string } | null>(null);

  const id = machine?.id ?? '';
  const running = machine?.state.lifecycle === 'running';
  const can = machine?.capabilities.includes('media.pen') ?? false;
  const pen = String(bag(machine?.state.facts).pen ?? '');

  async function act(capability: string, args: Record<string, unknown>, done: string) {
    setBusy(true);
    setTold(null);
    try {
      const outcome = await call('sandbox.act', { id, capability, arguments: args });
      setTold(outcome.ok ? { ok: true, text: done } : { ok: false, text: say(outcome.reason) });
    } catch (error) {
      setTold({ ok: false, text: say((error as { reason?: ReasonView | null }).reason) });
    }
    await reload();
    setBusy(false);
  }

  if (picking) {
    return (
      <FilePicker folders title={t('beatbox.pen.pick')} suffixes="" filterLabel="" start={pen}
                  onPick={(folder) => {
                    setPicking(false);
                    void act('media.pen', { folder }, t('beatbox.pen.plugged'));
                  }}
                  onClose={() => setPicking(false)} />
    );
  }

  return (
    <>
      <div className="bb-pagehead">
        <div>
          <div className="bb-pagehead-title">
            <h1 className="bb-t-title" style={{ margin: 0 }}>{t('beatbox.screen.host.pen.title')}</h1>
            <Info text={machine ? t('beatbox.pen.about', { name: machine.name }) : t('beatbox.screen.host.pen.blurb')} />
          </div>
        </div>
      </div>


      {list && !list.length ? (
        <div className="bb-pending" style={{ minHeight: 280 }}>
          <CubeWire size={80} tone="violet" opacity={0.42} />
          <p>{t('beatbox.host_pages.no_machines')}</p>
          <Button variant="primary" trailingIcon="arrow" onClick={() => navigate('host/machine')}>
            {t('beatbox.host_pages.to_machine')}
          </Button>
        </div>
      ) : machine && !can ? (
        <Card>
          <p className="bb-host-say">{t('beatbox.pen.not_this_one')}</p>
        </Card>
      ) : machine && !running ? (
        <div className="bb-pending" style={{ minHeight: 280 }}>
          <CubeWire size={80} tone="violet" opacity={0.42} />
          <p>{t('beatbox.pen.off', { name: machine.name })}</p>
          <Button trailingIcon="arrow" onClick={() => navigate('host/machine')}>
            {t('beatbox.host_pages.to_machine')}
          </Button>
        </div>
      ) : machine ? (
        <Card title={<><Icon name="usb" size="sm" />{t('beatbox.pen.title')}</>}
              info={pen ? t('beatbox.pen.safe') : `${t('beatbox.pen.how')} ${t('beatbox.pen.safe')}`}>
          {pen ? (
            <>
              <p className="bb-host-say">{t('beatbox.pen.in')} <Info text={t('beatbox.pen.inside')} /></p>
              <p className="bb-pen-folder bb-mono">{pen}</p>
              <div className="bb-pen-actions">
                <Button disabled={busy} icon="folder" onClick={() => setPicking(true)}>{t('beatbox.pen.other')}</Button>
                <Button disabled={busy} variant="danger" onClick={() => void act('media.eject', {}, t('beatbox.pen.ejected'))}>
                  {t('beatbox.pen.eject')}
                </Button>
              </div>
            </>
          ) : (
            <>
              <div className="bb-pen-actions">
                <Button variant="primary" icon="folder" disabled={busy} onClick={() => setPicking(true)}>
                  {t('beatbox.pen.choose')}
                </Button>
              </div>
            </>
          )}
          {told ? <p className={told.ok ? 'bb-host-say' : 'bb-host-say bb-rose'} style={{ marginTop: 10 }}>{told.text}</p> : null}
        </Card>
      ) : null}
    </>
  );
}
