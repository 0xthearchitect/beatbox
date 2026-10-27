// Shared helpers. These suites only read source text, no browser or
// compiler, on purpose: heavy checks get switched off.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

export const WEB = join(dirname(fileURLToPath(import.meta.url)), '..');
export const REPO = join(WEB, '..');
export const SRC = join(WEB, 'src');
export const DIST = join(WEB, 'dist');
export const LOCALES = join(REPO, 'locales');
export const LANGUAGES = ['en', 'pt_PT'];

export function text(path) {
  return readFileSync(path, 'utf-8');
}

/** A file under src/. */
export function read(name) {
  return text(join(SRC, name));
}

export const stylesheet = () => read('styles/global.css');

/** Every file under `folder` whose name `keep` accepts, sorted. */
export function walk(folder, keep) {
  const found = [];
  for (const entry of readdirSync(folder)) {
    const path = join(folder, entry);
    if (statSync(path).isDirectory()) found.push(...walk(path, keep));
    else if (keep(entry)) found.push(path);
  }
  return found.sort();
}

/** Every .ts and .tsx under src/. */
export function sources() {
  return walk(SRC, (name) => name.endsWith('.ts') || name.endsWith('.tsx'));
}

export function catalogue(language) {
  return JSON.parse(text(join(LOCALES, `${language}.json`)));
}

export function relativeToSrc(path) {
  return relative(SRC, path).split('\\').join('/');
}

/** Every match of a global regex, as its first group (or the whole match). */
export function all(pattern, source) {
  return [...source.matchAll(pattern)].map((match) => match[1] ?? match[0]);
}

/** `source` from the first `start` to the next `end`. */
export function between(source, start, end) {
  const from = source.indexOf(start);
  if (from < 0) throw new Error(`${start} is gone`);
  const to = source.indexOf(end, from);
  if (to < 0) throw new Error(`nothing ends ${start} with ${end}`);
  return source.slice(from, to);
}
