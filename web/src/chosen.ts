import { useCallback, useEffect, useState, useSyncExternalStore } from 'react';
import type { Tone } from './components';
import { call, on } from './bridge/client';
import type { SandboxSummary, SandboxView } from './bridge/types';

/**
 * Which machine the Host pages (Machine, Commands, Watchers, Pen) are about.
 * Picked in the rail's switcher, followed live by every page, kept across launches.
 */

/** A machine's state as a colour and a plain word. */
export const TONES: Record<string, Tone> = {
  created: 'sky', running: 'mint', paused: 'amber',
  stopped: 'neutral', failed: 'rose', destroyed: 'neutral',
};

/** Each kind remembers its own pick. */
export type Kind = 'host' | 'network' | 'server';

const CHOSEN: Record<Kind, string> = {
  host: 'beatbox.machine.chosen',
  network: 'beatbox.network.chosen',
  server: 'beatbox.server.chosen',
};
const listeners = new Set<() => void>();

export function remembered(kind: Kind): string {
  try {
    return window.localStorage.getItem(CHOSEN[kind]) ?? '';
  } catch {
    return '';
  }
}

/** Pick one, save it and tell every listener. */
export function remember(kind: Kind, id: string) {
  if (id === remembered(kind)) return;
  try {
    window.localStorage.setItem(CHOSEN[kind], id);
  } catch {
    // No storage: next launch just opens on the first machine. Fine.
  }
  for (const listener of listeners) listener();
}

export const rememberedMachine = () => remembered('host');
export const rememberMachine = (id: string) => remember('host', id);

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

export function useChosenId(kind: Kind = 'host'): string {
  return useSyncExternalStore(subscribe, () => remembered(kind), () => '');
}

/** Every sandbox of `kind` plus the chosen one, kept live. */
export function useChosen(kind: Kind) {
  const id = useChosenId(kind);
  const [list, setList] = useState<SandboxSummary[] | null>(null);
  const [machine, setMachine] = useState<SandboxView | null>(null);

  const load = useCallback(async () => {
    const rows = (await call('sandbox.list')).filter((row) => row.type === kind);
    setList(rows);
    const pick = rows.find((row) => row.id === remembered(kind))?.id ?? rows[0]?.id ?? '';
    if (!pick) {
      setMachine(null);
      return;
    }
    remember(kind, pick);
    setMachine(await call('sandbox.refresh', { id: pick }));
  }, [kind]);

  useEffect(() => {
    void load().catch(() => setList([]));
  }, [load, id]);

  // Skip `sandbox.action`: watchers fire one every few seconds and actions
  // never change the machine list or states.
  useEffect(() => on('sandbox', (event) => {
    // Services count too: the rail shows them for servers.
    if ((event.name.startsWith('sandbox.') && event.name !== 'sandbox.action')
        || (kind === 'server' && event.name.startsWith('service.'))) void load().catch(() => undefined);
  }), [load]);

  const reload = useCallback(() => load().catch(() => undefined), [load]);
  return {
    list,
    machine,
    choose: (chosen: string) => remember(kind, chosen),
    reload,
  };
}

export const useChosenMachine = () => useChosen('host');
