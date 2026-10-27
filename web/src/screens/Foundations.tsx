import { useEffect, useState, type ReactNode } from 'react';
import {
  Button, Card, ChipRow, CloseButton, Counter, Cube, CubeWire, Icon, IconButton, KeyValue, Logo, Meter, Pill, PortInput,
} from '../components';
import type { Tone } from '../components';
import { Select, type SelectGroup, type SelectOption } from '../components/Select';
import { call, transportName } from '../bridge/client';
import type { Availability, SystemProbe } from '../bridge/types';
import { fault, say, t, runtimeText } from '../i18n';
import {
  color, cubeTone, typeScale, verdictTone,
  type ColorToken, type CubeTone, type Verdict,
} from '../tokens';

/**
 * The design system, drawn from src/tokens/index.ts at runtime. No second
 * copy to drift: if this screen looks right, the tokens are right.
 *
 * The bridge check lives here, not in Settings: it's for builders proving
 * the stack joins up.
 */

const GROUND: ColorToken[] = ['void', 'surface', 'raised', 'hover', 'sunken', 'line', 'lineStrong', 'terminal'];
const INK: ColorToken[] = ['text', 'textMuted', 'textFaint'];
const ACCENT: ColorToken[] = ['violet', 'violetLift', 'violetWash', 'mint', 'amber', 'rose', 'sky'];

function Swatch({ token }: { token: ColorToken }) {
  return (
    <div className="bb-swatch">
      <i style={{ background: color[token] }} />
      <div style={{ minWidth: 0 }}>
        <b>{token}</b>
        <code>{color[token]}</code>
        <small>{t(`beatbox.token.${token}`)}</small>
      </div>
    </div>
  );
}

function SectionHead({ title, note }: { title: string; note?: string }) {
  return (
    <div className="bb-sectionhd">
      <h3 className="bb-t-section" style={{ margin: 0 }}>{title}</h3>
      {note ? <span className="bb-faint" style={{ fontSize: 12.5 }}>{note}</span> : null}
    </div>
  );
}

function Probe({ label, value }: { label: string; value: Availability }) {
  return (
    <div style={{ display: 'flex', gap: 12, alignItems: 'flex-start', padding: '9px 0', borderBottom: '1px solid var(--bb-color-line)' }}>
      <span style={{ width: 96, flex: 'none', fontSize: 12.5, fontWeight: 500 }}>{label}</span>
      <Pill tone={value.ready ? 'mint' : 'neutral'}>{value.ready ? t('beatbox.probe.ready') : t('beatbox.probe.missing')}</Pill>
      <span className="bb-faint" style={{ fontSize: 11.8, flex: 1, minWidth: 0, lineHeight: 1.5 }}>{say(value.reason)}</span>
    </div>
  );
}

function BridgeSection() {
  const [transport, setTransport] = useState('…');
  const [probe, setProbe] = useState<SystemProbe | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    (async () => {
      try {
        const [name, system] = await Promise.all([transportName(), call('system.probe')]);
        if (!live) return;
        setTransport(name);
        setProbe(system);
      } catch (error) {
        if (!live) return;
        setFailure(`${(error as { code?: string }).code ?? 'error'} — ${fault(error)}`);
      }
    })();
    return () => { live = false; };
  }, []);

  if (failure) {
    return (
      <Card tone="rose" title={<><Icon name="alert" size="sm" />{t('beatbox.bridge.title')}</>}>
        <p className="bb-prose" style={{ margin: '10px 0 0', fontSize: 12.5 }}>{t('beatbox.bridge.down')}</p>
        <code className="bb-mono" style={{ display: 'block', marginTop: 8, fontSize: 11.5, color: 'var(--bb-color-rose)' }}>{failure}</code>
      </Card>
    );
  }

  return (
    <Card title={<><Icon name="bridge" size="sm" />{t('beatbox.bridge.title')}</>} note={transport}>
      {probe ? (
        <>
          <KeyValue label={t('beatbox.bridge.platform')}>{probe.platform}</KeyValue>
          <div style={{ marginTop: 10 }}>
            <Probe label="KVM" value={probe.kvm} />
            <Probe label="QEMU" value={probe.qemu} />
            <Probe label="Frida" value={probe.frida} />
            <Probe label={t('beatbox.bridge.terminal')} value={probe.terminal} />
          </div>
        </>
      ) : (
        <p className="bb-faint" style={{ margin: 0, fontSize: 12.5 }}>{t('beatbox.bridge.asking')}</p>
      )}
    </Card>
  );
}


/**
 * Motion, live: click a chip and it splashes, bump the number and it rolls.
 *
 * The rule: only animate a change someone just caused or is watching. A
 * thing that moves on its own claims to be an event.
 */
function MotionSection() {
  const [state, setState] = useState('created');
  const [count, setCount] = useState(4);
  const states = ['created', 'running', 'stopped'] as const;
  return (
    <>
      <SectionHead title={t('beatbox.foundations.motion')} />
      <Card>
        <ChipRow current={state} style={{ margin: '4px 0 14px' }}>
          {states.map((name) => (
            <button key={name} type="button" className="bb-chip"
                    aria-current={state === name} onClick={() => setState(name)}>
              {t(`beatbox.lifecycle.${name}`)}
            </button>
          ))}
        </ChipRow>
        <div style={{ display: 'flex', alignItems: 'center', gap: 16, flexWrap: 'wrap' }}>
          <b className="bb-mono" style={{ fontFamily: 'var(--bb-font-display)', fontSize: 26, fontWeight: 600 }}>
            <Counter value={count} />
          </b>
          <Button variant="ghost" size="sm" onClick={() => setCount((was) => was + 1 + Math.floor(Math.random() * 5))}>
            {t('beatbox.demo.bump')}
          </Button>
        </div>
      </Card>
      <p className="bb-prose" style={{ marginTop: 12, fontSize: 12 }}>{t('beatbox.foundations.motion_note')}</p>
    </>
  );
}

/** components/Select.tsx on real questions with made-up answers. */
function MenuSection() {
  const ops: SelectOption[] = [
    { value: '==', glyph: '=', label: t('beatbox.foundations.menu.op.eq'), hint: '==' },
    { value: '!=', glyph: '≠', label: t('beatbox.foundations.menu.op.ne'), hint: '!=' },
    { value: '<', glyph: '<', label: t('beatbox.foundations.menu.op.lt'), hint: '<' },
    { value: '<=', glyph: '≤', label: t('beatbox.foundations.menu.op.le'), hint: '<=' },
    { value: '>', glyph: '>', label: t('beatbox.foundations.menu.op.gt'), hint: '>' },
    { value: '>=', glyph: '≥', label: t('beatbox.foundations.menu.op.ge'), hint: '>=' },
  ];
  const branch: SelectOption[] = [
    { value: '-', label: t('beatbox.foundations.menu.ends_failed') },
    { value: 'probe_block', hint: t('beatbox.foundations.menu.act_on', { name: 'web' }) },
    { value: 'not_cabled', hint: t('beatbox.foundations.menu.kind.end') },
    { value: 'probe_failed', hint: t('beatbox.foundations.menu.kind.end') },
  ];
  const sandboxes: SelectGroup[] = [
    { label: t('beatbox.nav.host'), options: [
      { value: 'pc1', hint: 'Windows · running' },
      { value: 'web', hint: 'Ubuntu · running' },
    ] },
    { label: t('beatbox.nav.network'), options: [{ value: 'office', hint: '10.50.20.0/24' }] },
  ];
  const ports: SelectOption[] = ['22', '80', '443', '3389'].map((value) => ({ value }));
  const [op, setOp] = useState('!=');
  const [then, setThen] = useState('not_cabled');
  const [on, setOn] = useState('web');
  const [port, setPort] = useState('3389');
  const [stale, setStale] = useState('wks-finanace');
  const [empty, setEmpty] = useState('');
  const field = (label: string, control: ReactNode) => (
    <label className="bb-field bb-wf-field"><span>{label}</span>{control}</label>
  );
  return (
    <>
      <SectionHead title={t('beatbox.foundations.menu')} />
      <div className="bb-menu-demo">
        {field(t('beatbox.foundations.menu.check.op'),
          <Select label={t('beatbox.foundations.menu.check.op')} value={op} onChange={setOp} options={ops} />)}
        {field(t('beatbox.foundations.menu.branch'),
          <Select label={t('beatbox.foundations.menu.branch')} value={then} onChange={setThen} options={branch} />)}
        {field(t('beatbox.foundations.menu.act.on'),
          <Select label={t('beatbox.foundations.menu.act.on')} value={on} onChange={setOn} options={sandboxes} />)}
        {field(t('beatbox.foundations.menu.tiny'),
          <Select tiny label={t('beatbox.foundations.menu.tiny')} value={port} onChange={setPort} options={ports} />)}
        {field(t('beatbox.foundations.menu.stale'),
          <Select label={t('beatbox.foundations.menu.stale')} value={stale} onChange={setStale}
                      options={[{ value: 'wks-finance' }, { value: 'fileserver' }]} />)}
        {field(t('beatbox.foundations.menu.empty'),
          <Select label={t('beatbox.foundations.menu.empty')} value={empty} onChange={setEmpty}
                      placeholder={t('beatbox.foundations.menu.empty_placeholder')} options={ports} />)}
        {field(t('beatbox.foundations.menu.disabled'),
          <Select disabled label={t('beatbox.foundations.menu.disabled')} value="22" onChange={() => undefined} options={ports} />)}
      </div>
      <p className="bb-prose" style={{ marginTop: 12, fontSize: 12 }}>{t('beatbox.foundations.menu.note')}</p>
    </>
  );
}

function NumberSection() {
  const [port, setPort] = useState('3610');
  const [bad, setBad] = useState('70000');
  return (
    <>
      <SectionHead title={t('beatbox.foundations.number')} />
      <div className="bb-menu-demo">
        <div className="bb-field bb-wf-field"><span>{t('beatbox.foundations.number.ok')}</span>
          <PortInput id="fd-port" label={t('beatbox.foundations.number.ok')} value={port} onChange={setPort} /></div>
        <div className="bb-field bb-wf-field"><span>{t('beatbox.foundations.number.bad')}</span>
          <PortInput id="fd-port-bad" label={t('beatbox.foundations.number.bad')} value={bad} onChange={setBad} /></div>
      </div>
      <p className="bb-prose" style={{ marginTop: 12, fontSize: 12 }}>{t('beatbox.foundations.number.note')}</p>
    </>
  );
}

export function Foundations() {
  return (
    <>
      <h2 className="bb-t-title" style={{ margin: 0 }}>{t('beatbox.screen.foundations.title')}</h2>
      <p className="bb-lede">{t('beatbox.foundations.lede')}</p>

      <SectionHead title={t('beatbox.foundations.bridge')} />
      <BridgeSection />

      <SectionHead title={t('beatbox.foundations.ground')} />
      <div className="bb-grid">{GROUND.map((token) => <Swatch key={token} token={token} />)}</div>

      <SectionHead title={t('beatbox.foundations.ink')} />
      <div className="bb-grid">{INK.map((token) => <Swatch key={token} token={token} />)}</div>

      <SectionHead title={t('beatbox.foundations.accent')} note={t('beatbox.foundations.accent_note')} />
      <div className="bb-grid">{ACCENT.map((token) => <Swatch key={token} token={token} />)}</div>

      <SectionHead title={t('beatbox.foundations.verdict')} />
      <Card>
        {(Object.keys(verdictTone) as Verdict[]).map((verdict) => (
          <div key={verdict} className="bb-kv">
            <span className="bb-mono" style={{ color: color[verdictTone[verdict]], fontWeight: 500 }}>{verdict}</span>
            <b style={{ fontWeight: 400, fontSize: 12 }} className="bb-faint">{runtimeText(`beatbox.verdict.${verdict.toLowerCase()}`)}</b>
          </div>
        ))}
      </Card>
      <p className="bb-prose" style={{ marginTop: 12, fontSize: 12 }}>{t('beatbox.foundations.verdict_note')}</p>

      <SectionHead title={t('beatbox.foundations.type')} />
      <div className="bb-scale">
        {Object.entries(typeScale).map(([name, step]) => (
          <div key={name}>
            <i>{`${name} · ${step.size} · ${step.weight}`}</i>
            <span className={`bb-t-${name}`}>{runtimeText(`beatbox.type.${name}.sample`)}</span>
          </div>
        ))}
      </div>
      <p className="bb-prose" style={{ marginTop: 12, fontSize: 12 }}>{t('beatbox.foundations.type_note')}</p>

      <SectionHead title={t('beatbox.foundations.shape')} />
      <div className="bb-shelf">
        <figure>
          <Logo size={78} />
          <figcaption>{t('beatbox.foundations.logo')}</figcaption>
        </figure>
        {(Object.keys(cubeTone) as CubeTone[]).map((tone) => (
          <figure key={tone}>
            <Cube size={78} tone={tone} glow />
            <figcaption>{t(`beatbox.cube.${tone}`)}</figcaption>
          </figure>
        ))}
        <figure>
          <CubeWire size={78} tone="violet" opacity={0.55} />
          <figcaption>{t('beatbox.foundations.wire')}</figcaption>
        </figure>
        <p className="bb-prose" style={{ flex: 1, minWidth: 230, margin: 0, fontSize: 12 }}>
          {t('beatbox.foundations.shape_note')}
        </p>
      </div>

      <MotionSection />

      <MenuSection />

      <NumberSection />

      <SectionHead title={t('beatbox.foundations.close')} note={t('beatbox.foundations.close_note')} />
      <div className="bb-close-demo">
        <figure>
          <header className="bb-modal-head">
            <Icon name="pencil" size="sm" />
            <b>{t('beatbox.foundations.close_sample')}</b>
            <IconButton name="close" title={t('beatbox.modal.close')} />
          </header>
          <figcaption>{t('beatbox.foundations.close_before')}</figcaption>
        </figure>
        <figure>
          <header className="bb-modal-head">
            <Icon name="pencil" size="sm" />
            <b>{t('beatbox.foundations.close_sample')}</b>
            <CloseButton title={t('beatbox.modal.close')} />
          </header>
          <figcaption>{t('beatbox.foundations.close_after')}</figcaption>
        </figure>
      </div>

      <SectionHead title={t('beatbox.foundations.components')} />
      <div className="bb-shelf" style={{ gap: 10 }}>
        <Button variant="primary" trailingIcon="arrow">{t('beatbox.demo.enter')}</Button>
        <Button variant="ghost">{t('beatbox.demo.snapshot')}</Button>
        <Button variant="danger" icon="stop">{t('beatbox.demo.end')}</Button>
        <IconButton name="arrow" title={t('beatbox.demo.enter')} />
        {(['mint', 'violet', 'amber', 'rose', 'sky', 'neutral'] as Tone[]).map((tone) => (
          <Pill key={tone} tone={tone}>{tone}</Pill>
        ))}
        <span className="bb-mono bb-muted">18:42</span>
        <div style={{ width: 140 }}><Meter value={62} max={100} /></div>
      </div>
    </>
  );
}
