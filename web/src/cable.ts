import { call } from './bridge/client';
import type { SandboxView } from './bridge/types';

/**
 * A machine's cable to a network.
 *
 * The engine doesn't wire sandboxes together, the caller does. Plugging in
 * = ask the network for a cable (`network.attach`), then set it as the
 * machine's `network`. We add `sandbox` so the machine knows which network;
 * the host only reads `switch`, `port` and `mac`.
 */
export interface Cable {
  sandbox: string;
  node: string;
  address: string;
  mac: string;
}

const bag = (value: unknown): Record<string, unknown> =>
  (value && typeof value === 'object' ? value as Record<string, unknown> : {});

export function cableOf(machine: SandboxView | null | undefined): Cable | null {
  const network = bag(machine?.config.settings).network;
  if (!network || typeof network !== 'object') return null;
  const cable = bag(network);
  return {
    sandbox: String(cable.sandbox ?? ''),
    node: String(cable.node ?? ''),
    address: String(cable.address ?? ''),
    mac: String(cable.mac ?? ''),
  };
}

/** Machine name as a DNS label: "Windows 10" -> `windows-10`. */
export function nodeName(name: string): string {
  const slug = name.toLowerCase().replace(/[^a-z0-9_-]+/g, '-').replace(/^[^a-z]+/, '').replace(/-+$/, '');
  const word = slug && !['gateway', 'any'].includes(slug) ? slug : `host-${slug || 'x'}`;
  return word.slice(0, 32);
}

/** reconfigure wants CPUs and memory every time, even unchanged. */
function sized(machine: SandboxView) {
  const resources = bag(machine.config.resources);
  return { cpu: Number(resources.cpu ?? 0), memory: Number(resources.memory ?? 0) };
}

/**
 * Plug a stopped `machine` into `networkId`, unplugging it first.
 * `at` is a device (`sw1`) or port (`sw1:3`); default is the router's first free port.
 */
export async function plug(machine: SandboxView, networkId: string, at?: string): Promise<SandboxView> {
  const was = cableOf(machine);
  if (was?.sandbox === networkId) return machine;
  if (was) await release(was);
  // `network.attach` hands back the existing cable for a known name, so pick
  // a free one: two QEMUs on one cable fight over one socket.
  const network = await call('sandbox.refresh', { id: networkId });
  const taken = new Set((Array.isArray(bag(network.state.facts).nodes) ? bag(network.state.facts).nodes as unknown[] : [])
    .map((node) => String(bag(node).node)));
  const base = nodeName(machine.name);
  let node = base;
  for (let n = 2; taken.has(node); n += 1) node = `${base.slice(0, 28)}-${n}`;
  const outcome = await call('sandbox.act', {
    id: networkId, capability: 'network.attach', arguments: at ? { node, at } : { node },
  });
  if (!outcome.ok) throw { reason: outcome.reason, message: outcome.capability };
  return call('sandbox.reconfigure', {
    id: machine.id, ...sized(machine),
    settings: { network: { ...bag(outcome.data), sandbox: networkId } },
  });
}

/** Unplug a stopped `machine`, leaving it on `mode`. */
export async function unplug(machine: SandboxView, mode = 'none'): Promise<SandboxView> {
  const was = cableOf(machine);
  if (was) await release(was);
  return call('sandbox.reconfigure', { id: machine.id, ...sized(machine), settings: { network: mode } });
}

/** Return the cable. Network already gone? Nothing to return. */
async function release(cable: Cable) {
  if (!cable.sandbox) return;
  await call('sandbox.act', {
    id: cable.sandbox, capability: 'network.detach', arguments: { node: cable.node },
  }).catch(() => undefined);
}
