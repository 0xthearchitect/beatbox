import { useRef, useState, type ReactNode } from 'react';
import { Icon } from './Icon';
import { fault, t } from '../i18n';

/**
 * A page title you rename in place. Goes inside the `<h1>`; children follow
 * the name (state pills). Enter or blur keeps, Escape drops.
 *
 * `onRename` throws when it can't; the error shows by the field and the field
 * stays open, so a refused rename never looks like it worked.
 */
export function RenameTitle({ name, placeholder, onRename, children }: {
  name: string;
  /** Shown when the name is empty. */
  placeholder?: string;
  onRename: (next: string) => Promise<unknown>;
  children?: ReactNode;
}) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState(name);
  const [failure, setFailure] = useState<string | null>(null);
  // Enter causes a blur; without this the rename fires twice.
  const settled = useRef(false);

  function begin() {
    settled.current = false;
    setValue(name);
    setFailure(null);
    setEditing(true);
  }

  async function keep() {
    if (settled.current) return;
    settled.current = true;
    const next = value.trim();
    if (next === name) { setEditing(false); return; }
    try {
      await onRename(next);
      setEditing(false);
    } catch (error) {
      settled.current = false;
      setFailure(fault(error));
    }
  }

  function drop() {
    settled.current = true;
    setEditing(false);
  }

  if (!editing) {
    return (
      <>
        {name || placeholder}
        <button type="button" className="bb-rename-btn" title={t('beatbox.rename.button')}
                aria-label={t('beatbox.rename.button')} onClick={begin}>
          <Icon name="pencil" size="sm" />
        </button>
        {children}
      </>
    );
  }
  return (
    <>
      <input className="bb-input bb-rename-input" value={value} autoFocus aria-label={t('beatbox.rename.field')}
             onChange={(event) => setValue(event.target.value)}
             onFocus={(event) => event.target.select()}
             onBlur={() => void keep()}
             onKeyDown={(event) => {
               if (event.key === 'Enter') void keep();
               if (event.key === 'Escape') drop();
             }} />
      {failure ? <small className="bb-rose bb-rename-say">{failure}</small> : null}
      {children}
    </>
  );
}
