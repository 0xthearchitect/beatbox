import { t } from '../i18n';
import { Icon } from './Icon';

export const validPort = (value: string) => value !== '' && Number.isInteger(Number(value))
  && Number(value) >= 1 && Number(value) <= 65535;

/** Number field with our own step buttons. Ports by default. */
export function PortInput({ id, label, value, onChange, defaultPort = 1, min = 1, max = 65535 }: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  defaultPort?: number;
  min?: number;
  max?: number;
}) {
  const invalid = value !== '' && !(Number.isInteger(Number(value)) && Number(value) >= min && Number(value) <= max);
  const number = Number(value);
  function step(amount: number) {
    const start = value !== '' && Number.isFinite(number) ? number : defaultPort - amount;
    onChange(String(Math.min(max, Math.max(min, Math.trunc(start) + amount))));
  }
  return (
    <span className="bb-port-input" data-invalid={invalid || undefined}>
      <input id={id} className="bb-input bb-mono" type="number" inputMode="numeric"
        min={min} max={max} step={1} required value={value}
        aria-invalid={invalid || undefined} aria-describedby={invalid ? `${id}-range` : undefined}
        onChange={(event) => onChange(event.target.value)} />
      <span className="bb-port-steps">
        <button type="button" aria-label={t('beatbox.srv.port.decrease', { field: label })}
          title={t('beatbox.srv.port.decrease', { field: label })} disabled={value !== '' && number <= min}
          onClick={() => step(-1)}><Icon name="minus" size="sm" /></button>
        <button type="button" aria-label={t('beatbox.srv.port.increase', { field: label })}
          title={t('beatbox.srv.port.increase', { field: label })} disabled={value !== '' && number >= max}
          onClick={() => step(1)}><Icon name="plus" size="sm" /></button>
      </span>
    </span>
  );
}
