/**
 * Make a reason's values readable inside a sentence.
 *
 * Program output (stderr, tracebacks, OS errors) gets cut to the one line
 * that names the failure, minus timestamps and class names; the common ones
 * ("permission denied", "no such file") become plain words, path kept. The
 * full text is still in the logs.
 *
 * Our own names (`process.execute`, `stopped`) become the UI's words for them.
 *
 * No imports: the page passes its lookup in, so tests can load this alone.
 */

/** Values holding raw program output. */
const SAID = new Set(['detail', 'said', 'error', 'why', 'problem', 'stderr']);

/** Values holding one of our names, and the key prefixes to find its word. */
const NAMED: Record<string, string[]> = {
  capability: ['beatbox.vocab.cap.'],
  state: ['beatbox.plain.state.'],
  transition: ['beatbox.transition.'],
  step: ['beatbox.transition.', 'beatbox.say.step.', 'beatbox.vocab.cap.'],
  type: ['beatbox.say.type.'],
  role: ['beatbox.say.type.'],
};

/** Errors every program words the same, and the plain key for each. */
const KNOWN: [RegExp, string][] = [
  [/connection refused|ECONNREFUSED|actively refused/i, 'refused'],
  [/address already in use|EADDRINUSE|only one usage of each socket address/i, 'port_busy'],
  [/permission denied|operation not permitted|EACCES|EPERM|access is denied/i, 'no_permission'],
  [/no space left on device|ENOSPC|not enough space on the disk/i, 'disk_full'],
  [/cannot allocate memory|out of memory|ENOMEM/i, 'no_memory'],
  [/read-only file system|EROFS/i, 'read_only'],
  [/command not found|executable file not found|is not recognized as an internal or external command/i, 'program_missing'],
  [/no such file or directory|ENOENT|cannot find the (?:file|path) specified/i, 'not_found'],
  [/network is unreachable|no route to host|EHOSTUNREACH|ENETUNREACH/i, 'unreachable'],
  [/name or service not known|could not resolve|nodename nor servname|temporary failure in name resolution/i, 'no_name'],
  [/connection reset|broken pipe|ECONNRESET|EPIPE|forcibly closed/i, 'dropped'],
  [/timed out|ETIMEDOUT|deadline exceeded/i, 'timeout'],
  [/device or resource busy|EBUSY|being used by another process/i, 'busy'],
  [/terminated signal|SIGTERM|received signal 15/i, 'stopped'],
];

/** Picks the line that names the failure. */
const TELLING = /error|erro|fatal|emerg|crit|panic|refused|denied|not found|failed|cannot|can't|could not|unable|invalid|no such|missing|timed out/i;

/** A path in the line, quoted or not. */
const PATH = /(?:^|[\s'"(:])((?:\/|[A-Za-z]:\\)[^\s'"():,]+)/;

/** The line of `raw` that says what went wrong, decoration stripped. */
export function telling(raw: string): string {
  const lines = raw
    .replace(/\x1b\[[0-9;]*[A-Za-z]/g, '')
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && !/^(?:Traceback|File ".*", line \d+|at .+:\d+:\d+\)?$|\^+|~+)/.test(line));
  const line = [...lines].reverse().find((each) => TELLING.test(each)) ?? lines.at(-1) ?? '';
  const bare = line
    .replace(/^\d{4}[-/]\d\d[-/]\d\d[T ][\d:.,]+(?:Z|[+-]\d\d:?\d\d)?\s*/, '')
    .replace(/^(?:\[[^\]]{1,40}\]\s*)+/, '')
    .replace(/^\d+#\d+:\s*(?:\*\d+\s*)?/, '')
    .replace(/^(?:panic:\s*)/, '')
    .replace(/^(?:[A-Za-z_][\w.]*)?(?:Error|Exception)(?:\s*\[Errno \d+\])?:\s*/, '')
    .replace(/^\[Errno \d+\]\s*/, '')
    .replace(/^(?:ERROR|FATAL|CRITICAL|WARNING|error|fatal|warning)\s*[:\-]?\s*/, '')
    .trim();
  return bare.length > 160 ? `${bare.slice(0, 157).trimEnd()}…` : bare;
}

/** Plain words for a known error, else its telling line. */
export function plainSaid(raw: string, word: (key: string) => string | undefined): string {
  const line = telling(raw);
  const known = KNOWN.find(([pattern]) => pattern.test(line))?.[1];
  const plain = known ? word(`beatbox.say.detail.${known}`) : undefined;
  if (plain) {
    const path = PATH.exec(line)?.[1];
    return path ? `${plain} (${path})` : plain;
  }
  return line || word('beatbox.say.detail.none') || '';
}

export function plainValues(
  values: Record<string, unknown>,
  word: (key: string) => string | undefined,
): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [name, value] of Object.entries(values)) {
    const text = value == null ? '' : String(value);
    if (SAID.has(name)) {
      out[name] = plainSaid(text, word);
      continue;
    }
    const found = (NAMED[name] ?? []).map((prefix) => word(prefix + text)).find(Boolean);
    out[name] = found ?? text;
  }
  return out;
}
