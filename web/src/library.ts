import type { CommandView } from './bridge/types';

/**
 * Built-in commands, and how one with blanks becomes a shell line.
 *
 * One line per OS with blanks in braces (`ping -c 4 {node}`), run via
 * `process.shell`: `sh -c` on Linux, PowerShell on Windows. Saved ones live
 * in the daemon (`commands.*`).
 */

export type BlankKind = 'text' | 'number' | 'path' | 'node';
export interface Blank { kind: BlankKind; default?: string }

export interface Command {
  id: string;
  name: string;
  group: string;
  linux: string;
  windows: string;
  blanks: Record<string, Blank>;
  timeout: number;
  /** `processes` runs `process.list` instead, shown as a table. */
  special?: 'processes';
  /** Saved by a person, not built in. */
  kept?: boolean;
  machine?: string;
}

const built = (id: string, group: string, name: string, linux: string, windows: string,
  blanks: Record<string, Blank> = {}): Command => ({ id, group, name, linux, windows, blanks, timeout: 30 });

export const BUILT_IN: readonly Command[] = [
  { ...built('processes', 'look', 'What is running', 'ps', 'tasklist'), special: 'processes' },
  built('who', 'look', 'Who is signed in', 'who', 'query user'),
  built('system', 'look', 'What system it is', 'uname -a; cat /etc/os-release',
    'Get-CimInstance Win32_OperatingSystem | Format-List Caption,Version,CSName'),
  built('uptime', 'look', 'How long it has been up', 'uptime',
    '(Get-Date) - (Get-CimInstance Win32_OperatingSystem).LastBootUpTime'),
  built('disk', 'look', 'Disk space', 'df -h', 'Get-PSDrive -PSProvider FileSystem'),
  built('addresses', 'network', 'Its addresses', 'ip -brief addr',
    'Get-NetIPAddress | Format-Table InterfaceAlias,IPAddress,PrefixLength'),
  built('ports', 'network', 'Ports it listens on', 'ss -tln',
    'Get-NetTCPConnection -State Listen | Format-Table LocalAddress,LocalPort,OwningProcess'),
  built('routes', 'network', 'Where it sends traffic', 'ip route', 'Get-NetRoute | Format-Table DestinationPrefix,NextHop'),
  built('reach', 'network', 'Reach another machine', 'ping -c 4 {node}', 'Test-Connection -Count 4 {node}',
    { node: { kind: 'node' } }),
  built('resolve', 'network', 'Look a name up', 'getent hosts {name}', 'Resolve-DnsName {name}',
    { name: { kind: 'text', default: 'gateway' } }),
  built('list', 'files', 'What is in a folder', 'ls -la {path}', 'Get-ChildItem -Force {path}',
    { path: { kind: 'path', default: '~' } }),
  built('read', 'files', 'Read a file', 'cat {path}', 'Get-Content {path}',
    { path: { kind: 'path', default: '' } }),
  built('downloads', 'files', 'Newest downloads', 'ls -lt ~/Downloads | head -20',
    'Get-ChildItem $HOME\\Downloads | Sort-Object LastWriteTime -Descending | Select-Object -First 20'),
];

export const GROUPS = ['look', 'network', 'files', 'yours'] as const;

/** A saved command in the built-in shape. */
export function fromKept(view: CommandView): Command {
  const blanks = (view.blanks && typeof view.blanks === 'object' ? view.blanks : {}) as Record<string, Blank>;
  return {
    id: view.id, name: view.name, group: 'yours', linux: view.linux, windows: view.windows,
    blanks, timeout: view.timeout, kept: true, machine: view.machine,
  };
}

/** Blank names in order, no repeats. */
export function blanksIn(...lines: string[]): string[] {
  const names: string[] = [];
  for (const line of lines) {
    for (const found of line.matchAll(/\{([a-z_][a-z0-9_]*)\}/gi)) {
      if (!names.includes(found[1]!)) names.push(found[1]!);
    }
  }
  return names;
}

/** '' when the command has no line for this OS. */
export const lineFor = (command: Command, windows: boolean) => (windows ? command.windows : command.linux);

/**
 * Quote a value so the shell sees exactly one word. Single quotes; a leading
 * `~` in a Linux path becomes "$HOME" so it still expands. null = bad number.
 */
export function quote(value: string, kind: BlankKind, windows: boolean): string | null {
  if (kind === 'number') return /^-?\d+(\.\d+)?$/.test(value.trim()) ? value.trim() : null;
  if (windows) return `'${value.replace(/'/g, "''")}'`;
  const quoted = (text: string) => `'${text.replace(/'/g, `'\\''`)}'`;
  if (kind === 'path' && (value === '~' || value.startsWith('~/'))) {
    return value === '~' ? '"$HOME"' : `"$HOME"${quoted(value.slice(1))}`;
  }
  return quoted(value);
}

/** Filled line, or the name of the first blank that can't be filled. */
export function fill(line: string, blanks: Record<string, Blank>, values: Record<string, string>, windows: boolean):
  { line: string } | { missing: string } {
  let missing = '';
  const filled = line.replace(/\{([a-z_][a-z0-9_]*)\}/gi, (whole, name: string) => {
    const value = values[name] ?? blanks[name]?.default ?? '';
    const said = value === '' ? null : quote(value, blanks[name]?.kind ?? 'text', windows);
    if (said === null && !missing) missing = name;
    return said ?? whole;
  });
  return missing ? { missing } : { line: filled };
}

export interface Echo { from: string; ms: number }

/**
 * Replies out of "Reach another machine": Linux `ping`
 * (`64 bytes from 10.0.0.2: icmp_seq=1 ttl=64 time=0.41 ms`) or PowerShell
 * 5.1 `Test-Connection` (rows ending in address, bytes, time). Lost pings
 * print nothing, so missing lines = no answer.
 */
export function readEchoes(output: string): Echo[] {
  const echoes: Echo[] = [];
  for (const line of output.split('\n')) {
    const linux = line.match(/from ([\d.]+).*time[=<]([\d.]+) ?ms/);
    if (linux) { echoes.push({ from: linux[1]!, ms: Number(linux[2]) }); continue; }
    const address = line.match(/\b(\d{1,3}(?:\.\d{1,3}){3})\b/);
    const last = line.trim().split(/\s+/);
    if (address && last.length >= 4 && /^\d+$/.test(last[last.length - 1]!) && /^\d+$/.test(last[last.length - 2]!)) {
      echoes.push({ from: address[1]!, ms: Number(last[last.length - 1]) });
    }
  }
  return echoes;
}
