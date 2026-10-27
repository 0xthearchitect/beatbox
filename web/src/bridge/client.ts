/**
 * The only way the page talks to the backend.
 *
 * One transport: the BeatBox window (shell/), which handles each action
 * itself or passes it to beatboxd. No dev server; we develop in the real
 * window, so we test what we ship.
 */

import type { Action, Api, ApiError, EventName, Events, ReasonView } from './types';

type Envelope =
  | { ok: true; data: unknown; error: null }
  | { ok: false; data: null; error: ApiError };

interface Transport {
  readonly name: string;
  send(action: string, payload: unknown): Promise<Envelope>;
  /** Start delivering pushed events to `deliver`. Called once, at connect. */
  listen(deliver: (name: string, payload: unknown) => void): void;
}

export class BridgeError extends Error {
  readonly code: string;
  /**
   * Set when the engine refused. It sends a name, not a sentence
   * (crates/reasons), so `message` is that name; show it via fault() in ../i18n.
   */
  readonly reason: ReasonView | null;
  constructor(code: string, message: string, reason: ReasonView | null = null) {
    super(message);
    this.name = 'BridgeError';
    this.code = code;
    this.reason = reason;
  }
}

interface TauriInternals {
  invoke(command: string, args?: Record<string, unknown>): Promise<unknown>;
}
interface TauriWindow {
  __TAURI_INTERNALS__?: TauriInternals;
  /** Set by listen(); the window calls it with every pushed event. */
  __beatboxPushed?: (name: string, payload: unknown) => void;
}

/**
 * `call` returns the envelope; the window pushes events into `__beatboxPushed`.
 * No @tauri-apps/api: we only need invoke, and it's already on the window.
 */
function tauriTransport(internals: TauriInternals): Transport {
  return {
    name: 'tauri',
    send: async (action, payload) =>
      (await internals.invoke('call', { action, payload: payload ?? {} })) as Envelope,
    listen(deliver) {
      (window as unknown as TauriWindow).__beatboxPushed = deliver;
    },
  };
}

let pending: Promise<Transport> | null = null;

function transport(): Promise<Transport> {
  if (!pending) {
    const internals = (window as unknown as TauriWindow).__TAURI_INTERNALS__;
    pending = internals
      ? Promise.resolve(tauriTransport(internals))
      : Promise.reject(new BridgeError('transport', 'this page runs inside the BeatBox window, and it is not in one'));
  }
  return pending;
}

type Handler = (payload: never) => void;
const handlers = new Map<string, Set<Handler>>();
let listening = false;

function deliver(name: string, payload: unknown): void {
  for (const handler of handlers.get(name) ?? []) {
    try {
      (handler as (value: unknown) => void)(payload);
    } catch (error) {
      console.error(`[beatbox] handler for ${name} threw`, error);
    }
  }
}

/**
 * Subscribe to a pushed event; returns the unsubscribe. Typed off Events, so
 * a misspelled name fails to compile instead of silently never firing.
 */
export function on<E extends EventName>(name: E, handler: (payload: Events[E]) => void): () => void {
  const set = handlers.get(name) ?? new Set<Handler>();
  set.add(handler as Handler);
  handlers.set(name, set);
  if (!listening) {
    listening = true;
    // No window, nothing to hear. call() is the one that reports it.
    void transport().then(
      (carrier) => carrier.listen(deliver),
      () => undefined,
    );
  }
  return () => {
    set.delete(handler as Handler);
  };
}

/** For the Foundations screen. */
export async function transportName(): Promise<string> {
  return (await transport()).name;
}

/** Call one action. Rejects with a BridgeError; branch on its code, not its wording. */
export async function call<A extends Action>(
  action: A,
  payload?: Api[A]['payload'],
): Promise<Api[A]['response']> {
  let envelope: Envelope;
  try {
    envelope = await (await transport()).send(action, payload ?? {});
  } catch (error) {
    if (error instanceof BridgeError) throw error;
    throw new BridgeError('transport', error instanceof Error ? error.message : String(error));
  }
  if (!envelope.ok) throw new BridgeError(envelope.error.code, envelope.error.message, envelope.error.reason);
  return envelope.data as Api[A]['response'];
}
