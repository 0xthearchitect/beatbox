import type { KeySuffix } from './keys';
import type { t, entryText, isKey } from './index';

/** Compile-only contracts; this module is not imported by the application. */
export function checkKeys(translate: typeof t, entry: typeof entryText, known: typeof isKey,
  state: KeySuffix<'beatbox.lifecycle.'>, incoming: string) {
  translate('beatbox.nav.settings');
  translate(`beatbox.lifecycle.${state}`);
  // @ts-expect-error misspelled literal keys must fail compilation
  translate('beatbox.nav.sandbxo');
  // @ts-expect-error entry copy uses the same key contract
  entry('beatbox.account.typo');
  // @ts-expect-error unrestricted engine strings require runtime validation
  translate(incoming);
  if (known(incoming)) translate(incoming);
}
