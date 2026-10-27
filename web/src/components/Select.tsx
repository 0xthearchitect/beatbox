import type { CSSProperties } from 'react';
import { Check, ChevronsUpDown } from 'lucide-react';
import { Select as S } from 'radix-ui';

/**
 * Pick from a known set, so nobody can typo a machine name into a broken network.
 *
 * - `options` flat or grouped (a rule can pick a machine, segment, role or kind).
 * - `value` always shows, even if it's not an option any more, and gets
 *   marked. Otherwise a stale value shows blank and saves as ''.
 * - Matching ignores case, like the engine does. Exact matching flagged half
 *   of a fine policy as broken.
 *
 * Radix Select, not native: GTK draws native lists and CSS can't reach them.
 * Radix rejects '' as a value, so empty goes through a sentinel. Trigger keeps
 * `bb-select` so the old layout CSS still applies. Raw values (names, ports)
 * render mono; labelled ones in the reading face with a mono hint.
 */

export interface SelectOption {
  value: string;
  /** Defaults to the value (fine for names and ports). */
  label?: string;
  /** Short sign before the label, e.g. ≠. */
  glyph?: string;
  /** Muted word after the label, e.g. a sandbox's kind. */
  hint?: string;
}

export interface SelectGroup {
  label: string;
  options: SelectOption[];
}

const EMPTY = '\u0000';
const inward = (value: string) => (value === '' ? EMPTY : value);
const outward = (value: string) => (value === EMPTY ? '' : value);

const flatten = (options: readonly (SelectOption | SelectGroup)[]): SelectOption[] =>
  options.flatMap((entry) => ('options' in entry ? entry.options : [entry]));

function Item({ option }: { option: SelectOption }) {
  return (
    <S.Item value={inward(option.value)} className={`bb-menu-item${option.label ? ' bb-menu-item--words' : ''}`}>
      {option.glyph ? <span className="bb-menu-glyph">{option.glyph}</span> : null}
      <span className="bb-menu-text"><S.ItemText>{option.label ?? option.value}</S.ItemText></span>
      {option.hint ? <span className="bb-menu-hint">{option.hint}</span> : null}
      <S.ItemIndicator className="bb-menu-check"><Check size={14} /></S.ItemIndicator>
    </S.Item>
  );
}

export function Select({
  value, options, onChange, label, tiny, disabled, placeholder, style,
}: {
  value: string;
  options: readonly (SelectOption | SelectGroup)[];
  onChange: (next: string) => void;
  /** Accessible name; these usually have no visible label. */
  label: string;
  tiny?: boolean;
  disabled?: boolean;
  placeholder?: string;
  style?: CSSProperties;
}) {
  const same = (left: string, right: string) => left.toLowerCase() === right.toLowerCase();
  const match = flatten(options).find((option) => same(option.value, value));
  const stale = value !== '' && match === undefined;
  return (
    <S.Root value={match ? inward(match.value) : stale ? value : undefined}
            onValueChange={(next) => onChange(outward(next))} disabled={disabled}>
      <S.Trigger className={`bb-select${tiny ? ' bb-select--tiny' : ''}`} aria-label={label} style={style}>
        {match?.glyph ? <span className="bb-menu-glyph">{match.glyph}</span> : null}
        <span className={`bb-select-value${match || stale ? '' : ' bb-select-value--empty'}${match?.label ? ' bb-select-value--words' : ''}`}>
          <S.Value placeholder={placeholder ?? '—'}>{match ? match.label ?? match.value : value}</S.Value>
        </span>
        {stale ? <span className="bb-select-stale">⚠</span> : null}
        <S.Icon className="bb-select-chevron"><ChevronsUpDown size={14} /></S.Icon>
      </S.Trigger>
      <S.Portal>
        <S.Content className="bb-menu-content" position="popper" sideOffset={6} align="start" collisionPadding={8}>
          <S.Viewport className="bb-menu-viewport">
            {stale ? (
              <S.Group>
                <Item option={{ value, hint: '⚠' }} />
                <S.Separator className="bb-menu-sep" />
              </S.Group>
            ) : null}
            {options.map((entry, index) =>
              'options' in entry ? (
                <S.Group key={`g${index}`}>
                  {index > 0 ? <S.Separator className="bb-menu-sep" /> : null}
                  <S.Label className="bb-menu-label">{entry.label}</S.Label>
                  {entry.options.map((option) => <Item key={option.value} option={option} />)}
                </S.Group>
              ) : <Item key={entry.value} option={entry} />,
            )}
          </S.Viewport>
        </S.Content>
      </S.Portal>
    </S.Root>
  );
}
