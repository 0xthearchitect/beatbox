/**
 * Whether the rail shows the builder pages (Engine, Foundations).
 *
 * The window keeps it (shell/src/preferences.rs) and hands it over at start
 * as `__beatboxBuilder`. Settings flips it here too so the rail updates
 * without a reload.
 */

import { useSyncExternalStore } from 'react';

let shown = Boolean((window as { __beatboxBuilder?: boolean }).__beatboxBuilder);
const listeners = new Set<() => void>();

export function showBuilderTools(next: boolean): void {
  shown = next;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

export const useBuilderTools = (): boolean => useSyncExternalStore(subscribe, () => shown, () => false);
