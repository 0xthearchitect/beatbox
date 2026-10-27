/**
 * UI text. Edit locales/{en,pt_PT}/<domain>.json;
 * the merged locales/{en,pt_PT}.json come over the bridge at boot, not as a
 * build import, so switching language is a reload, not a rebuild.
 */

import { call } from '../bridge/client';
import type { ReasonView } from '../bridge/types';
import { OFFLINE_STRINGS } from './fallback';
import { plainSaid, plainValues } from './plain';
import english from '../../../locales/en.json';
import type { Key } from './keys';
export type { Key, KeySuffix } from './keys';

/** Validate names received from the engine or other open-ended sources. */
export function isKey(key: string): key is Key {
  return Object.hasOwn(english, key);
}

/** Only for open-ended runtime names; static UI text must use t(). */
export function runtimeText(key: string, vars?: Record<string, string | number>): string {
  return isKey(key) ? t(key, vars) : key;
}

let strings: Record<string, string> = {};
let language = 'en';
let available: string[] = ['en'];

export async function loadLocale(): Promise<void> {
  try {
    const locale = await call('system.locale');
    strings = (locale.strings ?? {}) as Record<string, string>;
    language = locale.language;
    available = locale.available;
  } catch (error) {
    // Just the "bridge down" strings; see ./fallback.ts for why.
    strings = { ...OFFLINE_STRINGS };
    throw error;
  }
}

export const activeLanguage = () => language;
export const availableLanguages = () => available;

/** Hero and Account are always English, whatever the app language. Same catalogue. */
export function entryText(key: Key, vars?: Record<string, string | number>): string {
  const template = (english as Record<string, string>)[key] ?? key;
  return vars ? template.replace(/\{(\w+)\}/g, (whole, name: string) =>
    name in vars ? String(vars[name]) : whole) : template;
}

export function entryReason(reason: ReasonView | null | undefined, fallback = ''): string {
  const key = `beatbox.${reason?.code ?? ''}`;
  if (!reason?.code) return fallback;
  return isKey(key) ? entryText(key, (reason.values ?? {}) as Record<string, string>) : key;
}

/**
 * Look up one key. Placeholders are `{named}`.
 *
 * Development falls back to English for incomplete locale payloads.
 * Production returns a missing key as itself; locales:check guarantees parity
 * before a build. Open-ended engine names are validated by runtimeText().
 */
export function t(key: Key, vars?: Record<string, string | number>): string {
  const template = strings[key] ?? (import.meta.env.DEV ? english[key] : undefined);
  if (template === undefined) {
    if (import.meta.env.DEV) console.warn(`[beatbox] no string for ${key}`);
    return key;
  }
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (whole, name: string) =>
    name in vars ? String(vars[name]) : whole,
  );
}

/**
 * Turn an engine reason into a sentence. The engine (crates/reasons) only
 * sends a name plus values so it stays translator-free; this is the one
 * place that writes them out.
 */
export function say(reason: ReasonView | null | undefined, fallback = ''): string {
  if (!reason || !reason.code) return fallback;
  const key = `beatbox.${reason.code}`;
  // Unknown reason (newer engine): say something generic, never the raw key.
  if (!isKey(key) || (!import.meta.env.DEV && strings[key] === undefined)) return fallback || t('beatbox.say.unknown');
  const values = { ...((reason.values ?? {}) as Record<string, unknown>) };
  // Nested reasons (a `problems` list) get said one by one.
  if (typeof values.problems === 'string' && values.problems.startsWith('[')) {
    try {
      const inner = JSON.parse(values.problems) as ReasonView[];
      return t(key, { problems: inner.map((each) => `• ${say(each)}`).join('\n') });
    } catch {
      // Not JSON after all; use it as is.
    }
  }
  return t(key, plainValues(values, (each) => strings[each]));
}

/** Reason values made readable, for the page's own sentences (see ./plain.ts). */
export const plainly = (values: Record<string, unknown>): Record<string, string> =>
  plainValues(values, (key) => strings[key]);

/** Program output cut to its telling line, plain words when known. */
export const plainDetail = (raw: string): string => plainSaid(raw, (key) => strings[key]);

/**
 * The error to show a person. Always use this, never `error.message`
 * (that's English on purpose; see BridgeError).
 */
export function fault(error: unknown): string {
  const failure = error as { reason?: ReasonView | null; message?: string; code?: string } | null;
  if (failure && failure.reason) return say(failure.reason, failure.message ?? '');
  // No reason: the code says what kind of failure, the English message is detail.
  const message = failure && failure.message ? failure.message : String(error);
  const word = (key: string) => strings[key];
  const known = failure?.code ? `beatbox.api.${failure.code}` : '';
  if (isKey(known) && (strings[known] !== undefined || import.meta.env.DEV)) return t(known, { detail: plainSaid(message, word) });
  return plainSaid(message, word);
}
