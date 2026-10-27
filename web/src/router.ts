/**
 * Hash routing, by hand.
 *
 * The webview opens the bundle from disk, so there's no server to rewrite
 * paths and history routing breaks on reload. Not worth a library for forty lines.
 */

import { useSyncExternalStore } from 'react';

const listeners = new Set<() => void>();

function announce() {
  for (const listener of listeners) listener();
}

if (typeof window !== 'undefined') {
  window.addEventListener('hashchange', announce);
}

const raw = () => window.location.hash.replace(/^#\/?/, '');

/** `#/sandbox/analyse?id=x` -> `sandbox/analyse`; an empty hash is the root route. */
export const currentPath = (): string => raw().split('?')[0] ?? '';

/** `#/sandbox/analyse?id=x` -> `id=x`. A string so useSyncExternalStore can compare it. */
export const currentQuery = (): string => raw().split('?')[1] ?? '';

export function href(path: string, params?: Record<string, string>): string {
  const query = params ? new URLSearchParams(params).toString() : '';
  return query ? `#/${path}?${query}` : `#/${path}`;
}

export function navigate(path: string, params?: Record<string, string>): void {
  const target = href(path, params);
  if (window.location.hash === target.slice(1) || window.location.hash === target) return;
  window.location.hash = target.replace(/^#/, '');
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

export function useRoutePath(): string {
  return useSyncExternalStore(subscribe, currentPath, () => '');
}

/** The query of the current route. Re-renders when only the query changes. */
export function useRouteParams(): URLSearchParams {
  return new URLSearchParams(useSyncExternalStore(subscribe, currentQuery, () => ''));
}
