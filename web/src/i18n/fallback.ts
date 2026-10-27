/**
 * The only strings not in the locale files: the "bridge is down" message.
 * The bridge is where translations come from, so this one can't be translated.
 * Used only when loadLocale() failed.
 *
 * Keep it tiny. Everything else shows its raw key in that state on purpose;
 * a UI in fluent English with no bridge would hide the failure.
 */
export const OFFLINE_STRINGS: Record<string, string> = {
  'beatbox.bridge.title': 'The bridge',
  'beatbox.bridge.down':
    'No answer from the BeatBox window, so no text could be loaded either — that is why the rest of this page is showing key names. This page only runs inside that window.',
  'beatbox.bridge.asking': 'Asking the host…',
  'beatbox.screen.foundations.title': 'Foundations',
};
