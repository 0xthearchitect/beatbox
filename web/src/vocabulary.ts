/**
 * Plain words for capabilities (`dns.resolve`) and events (`packet.blocked`).
 *
 * The engine names stay what we store and compare; these are for people,
 * with the name shown beside as a hint.
 *
 * Unknown names show as themselves. A new sandbox type still works without
 * words; they're a courtesy, never a gate.
 *
 * Literal t() calls in switches so web/tests/strings.test.mjs sees every key.
 */

import { t } from './i18n';

export interface Worded {
  label: string;
  group: string;
}

const namespace = (name: string) => name.split('.')[0] ?? name;

function groupWord(ns: string): string {
  switch (ns) {
    case 'lifecycle': return t('beatbox.vocab.group.lifecycle');
    case 'sandbox': return t('beatbox.vocab.group.lifecycle');
    case 'process': return t('beatbox.vocab.group.process');
    case 'filesystem': return t('beatbox.vocab.group.files');
    case 'file': return t('beatbox.vocab.group.files');
    case 'snapshot': return t('beatbox.vocab.group.snapshot');
    case 'media': return t('beatbox.vocab.group.media');
    case 'network': return t('beatbox.vocab.group.network');
    case 'connection': return t('beatbox.vocab.group.connections');
    case 'packet': return t('beatbox.vocab.group.connections');
    case 'dns': return t('beatbox.vocab.group.dns');
    case 'firewall': return t('beatbox.vocab.group.firewall');
    case 'traffic': return t('beatbox.vocab.group.traffic');
    case 'guest': return t('beatbox.vocab.group.guest');
    case 'watcher': return t('beatbox.vocab.group.watcher');
    default: return ns;
  }
}

function capabilityLabel(capability: string): string {
  switch (capability) {
    case 'lifecycle.start': return t('beatbox.vocab.cap.lifecycle.start');
    case 'lifecycle.stop': return t('beatbox.vocab.cap.lifecycle.stop');
    case 'lifecycle.pause': return t('beatbox.vocab.cap.lifecycle.pause');
    case 'lifecycle.resume': return t('beatbox.vocab.cap.lifecycle.resume');
    case 'lifecycle.reset': return t('beatbox.vocab.cap.lifecycle.reset');
    case 'process.execute': return t('beatbox.vocab.cap.process.execute');
    case 'process.shell': return t('beatbox.vocab.cap.process.shell');
    case 'process.list': return t('beatbox.vocab.cap.process.list');
    case 'process.kill': return t('beatbox.vocab.cap.process.kill');
    case 'filesystem.read': return t('beatbox.vocab.cap.filesystem.read');
    case 'filesystem.write': return t('beatbox.vocab.cap.filesystem.write');
    case 'filesystem.list': return t('beatbox.vocab.cap.filesystem.list');
    case 'network.interfaces': return t('beatbox.vocab.cap.network.interfaces');
    case 'network.traffic': return t('beatbox.vocab.cap.network.traffic');
    case 'network.attach': return t('beatbox.vocab.cap.network.attach');
    case 'network.detach': return t('beatbox.vocab.cap.network.detach');
    case 'network.flows': return t('beatbox.vocab.cap.network.flows');
    case 'snapshot.create': return t('beatbox.vocab.cap.snapshot.create');
    case 'snapshot.restore': return t('beatbox.vocab.cap.snapshot.restore');
    case 'snapshot.delete': return t('beatbox.vocab.cap.snapshot.delete');
    case 'snapshot.list': return t('beatbox.vocab.cap.snapshot.list');
    case 'snapshot.save_state': return t('beatbox.vocab.cap.snapshot.save_state');
    case 'media.insert': return t('beatbox.vocab.cap.media.insert');
    case 'media.pen': return t('beatbox.vocab.cap.media.pen');
    case 'media.eject': return t('beatbox.vocab.cap.media.eject');
    case 'display.picture': return t('beatbox.vocab.cap.display.picture');
    case 'firewall.add': return t('beatbox.vocab.cap.firewall.add');
    case 'firewall.remove': return t('beatbox.vocab.cap.firewall.remove');
    case 'firewall.default': return t('beatbox.vocab.cap.firewall.default');
    case 'firewall.move': return t('beatbox.vocab.cap.firewall.move');
    case 'firewall.answers': return t('beatbox.vocab.cap.firewall.answers');
    case 'firewall.group': return t('beatbox.vocab.cap.firewall.group');
    case 'firewall.service': return t('beatbox.vocab.cap.firewall.service');
    case 'nat.hide': return t('beatbox.vocab.cap.nat.hide');
    case 'nat.add': return t('beatbox.vocab.cap.nat.add');
    case 'nat.remove': return t('beatbox.vocab.cap.nat.remove');
    case 'capture.start': return t('beatbox.vocab.cap.capture.start');
    case 'capture.stop': return t('beatbox.vocab.cap.capture.stop');
    case 'capture.remove': return t('beatbox.vocab.cap.capture.remove');
    case 'capture.save': return t('beatbox.vocab.cap.capture.save');
    case 'router.internet': return t('beatbox.vocab.cap.router.internet');
    case 'network.renumber_plan': return t('beatbox.vocab.cap.network.renumber_plan');
    case 'device.add': return t('beatbox.vocab.cap.device.add');
    case 'device.remove': return t('beatbox.vocab.cap.device.remove');
    case 'link.add': return t('beatbox.vocab.cap.link.add');
    case 'link.remove': return t('beatbox.vocab.cap.link.remove');
    case 'port.configure': return t('beatbox.vocab.cap.port.configure');
    case 'interface.configure': return t('beatbox.vocab.cap.interface.configure');
    case 'interface.remove': return t('beatbox.vocab.cap.interface.remove');
    case 'cable.configure': return t('beatbox.vocab.cap.cable.configure');
    case 'vlan.name': return t('beatbox.vocab.cap.vlan.name');
    case 'light.add': return t('beatbox.vocab.cap.light.add');
    case 'light.configure': return t('beatbox.vocab.cap.light.configure');
    case 'device.rename': return t('beatbox.vocab.cap.device.rename');
    case 'node.rename': return t('beatbox.vocab.cap.node.rename');
    case 'light.run': return t('beatbox.vocab.cap.light.run');
    case 'light.start': return t('beatbox.vocab.cap.light.start');
    case 'light.result': return t('beatbox.vocab.cap.light.result');
    case 'route.add': return t('beatbox.vocab.cap.route.add');
    case 'route.remove': return t('beatbox.vocab.cap.route.remove');
    case 'node.address': return t('beatbox.vocab.cap.node.address');
    case 'node.place': return t('beatbox.vocab.cap.node.place');
    case 'dns.add': return t('beatbox.vocab.cap.dns.add');
    case 'dns.remove': return t('beatbox.vocab.cap.dns.remove');
    case 'dns.resolve': return t('beatbox.vocab.cap.dns.resolve');
    case 'traffic.configure': return t('beatbox.vocab.cap.traffic.configure');
    case 'traffic.observe': return t('beatbox.vocab.cap.traffic.observe');
    default: return capability;
  }
}

function eventLabel(event: string): string {
  switch (event) {
    case '*': return t('beatbox.vocab.event.any');
    case 'connection.created': return t('beatbox.vocab.event.connection.created');
    case 'packet.blocked': return t('beatbox.vocab.event.packet.blocked');
    case 'dns.query': return t('beatbox.vocab.event.dns.query');
    case 'network.attached': return t('beatbox.vocab.event.network.attached');
    case 'network.detached': return t('beatbox.vocab.event.network.detached');
    case 'traffic.configured': return t('beatbox.vocab.event.traffic.configured');
    case 'process.created': return t('beatbox.vocab.event.process.created');
    case 'process.exited': return t('beatbox.vocab.event.process.exited');
    case 'process.killed': return t('beatbox.vocab.event.process.killed');
    case 'file.written': return t('beatbox.vocab.event.file.written');
    case 'guest.agent_ready': return t('beatbox.vocab.event.guest.agent_ready');
    case 'guest.rtc_change': return t('beatbox.vocab.event.guest.rtc_change');
    case 'media.inserted': return t('beatbox.vocab.event.media.inserted');
    case 'media.ejected': return t('beatbox.vocab.event.media.ejected');
    case 'snapshot.created': return t('beatbox.vocab.event.snapshot.created');
    case 'snapshot.restored': return t('beatbox.vocab.event.snapshot.restored');
    case 'snapshot.deleted': return t('beatbox.vocab.event.snapshot.deleted');
    case 'snapshot.state_saved': return t('beatbox.vocab.event.snapshot.state_saved');
    case 'watcher.changed': return t('beatbox.vocab.event.watcher.changed');
    case 'sandbox.started': return t('beatbox.vocab.event.sandbox.started');
    case 'sandbox.stopped': return t('beatbox.vocab.event.sandbox.stopped');
    case 'sandbox.paused': return t('beatbox.vocab.event.sandbox.paused');
    case 'sandbox.resumed': return t('beatbox.vocab.event.sandbox.resumed');
    case 'sandbox.reset': return t('beatbox.vocab.event.sandbox.reset');
    case 'sandbox.failed': return t('beatbox.vocab.event.sandbox.failed');
    case 'sandbox.destroyed': return t('beatbox.vocab.event.sandbox.destroyed');
    case 'sandbox.reconfigured': return t('beatbox.vocab.event.sandbox.reconfigured');
    default: return event;
  }
}

export const capabilityWords = (capability: string): Worded =>
  ({ label: capabilityLabel(capability), group: groupWord(namespace(capability)) });

export const eventWords = (event: string): Worded =>
  ({ label: eventLabel(event), group: event === '*' ? '' : groupWord(namespace(event)) });

/**
 * Events per sandbox kind, most-looked-for first, lifecycle last.
 * Copied from the `emit`s in crates/network and crates/host, plus `watcher.changed`.
 */
const SAID: Record<string, readonly string[]> = {
  network: ['connection.created', 'packet.blocked', 'dns.query', 'network.attached', 'network.detached', 'traffic.configured'],
  host: ['process.created', 'process.exited', 'process.killed', 'file.written', 'guest.agent_ready', 'guest.rtc_change',
    'watcher.changed', 'media.inserted', 'media.ejected', 'snapshot.created', 'snapshot.restored', 'snapshot.deleted',
    'snapshot.state_saved'],
};
const LIFE = ['sandbox.started', 'sandbox.stopped', 'sandbox.paused', 'sandbox.resumed', 'sandbox.reset',
  'sandbox.failed', 'sandbox.destroyed', 'sandbox.reconfigured'];

/** Events for `type`; all of them when the type is unknown. */
export function eventsOf(type: string | null): string[] {
  const own = type && SAID[type] ? SAID[type]! : Object.values(SAID).flat();
  return [...own, ...LIFE];
}

/** Capabilities grouped for a Select, worded, with the raw name as hint. */
export function grouped<T>(names: readonly string[], words: (name: string) => Worded,
  option: (name: string, worded: Worded) => T): { label: string; options: T[] }[] {
  const groups = new Map<string, T[]>();
  for (const name of names) {
    const worded = words(name);
    const list = groups.get(worded.group) ?? [];
    list.push(option(name, worded));
    groups.set(worded.group, list);
  }
  return [...groups].map(([label, options]) => ({ label, options }));
}

/** Per event, the fields a `where` can test. FLOW is shared by the traffic events. */
const FLOW = ['source_node', 'destination_node', 'port', 'protocol', 'action', 'rule', 'source', 'destination'];
const FIELDS: Record<string, readonly string[]> = {
  'connection.created': FLOW,
  'packet.blocked': FLOW,
  'dns.query': ['node', 'name', 'answer'],
  'network.attached': ['node', 'address', 'mac'],
  'network.detached': ['node'],
  'process.created': ['command', 'arguments'],
  'process.exited': ['command', 'exit_code', 'pid'],
  'process.killed': ['command', 'pid'],
  'watcher.changed': ['name', 'because', 'exit_code', 'output'],
  'file.written': ['path'],
};

/** Paths a `where` can test for `event`. `*` means any event, plus `event` itself. */
export function fieldsOf(event: string): string[] {
  const own = event === '*' ? [...new Set(Object.values(FIELDS).flat())] : [...(FIELDS[event] ?? [])];
  return [...own.map((field) => `data.${field}`), 'name'];
}

export function fieldWord(path: string): string {
  switch (path) {
    case 'name': return t('beatbox.vocab.field.event');
    case 'data.source_node': return t('beatbox.vocab.field.source_node');
    case 'data.destination_node': return t('beatbox.vocab.field.destination_node');
    case 'data.port': return t('beatbox.vocab.field.port');
    case 'data.protocol': return t('beatbox.vocab.field.protocol');
    case 'data.action': return t('beatbox.vocab.field.action');
    case 'data.rule': return t('beatbox.vocab.field.rule');
    case 'data.source': return t('beatbox.vocab.field.source');
    case 'data.destination': return t('beatbox.vocab.field.destination');
    case 'data.node': return t('beatbox.vocab.field.node');
    case 'data.name': return t('beatbox.vocab.field.name');
    case 'data.answer': return t('beatbox.vocab.field.answer');
    case 'data.address': return t('beatbox.vocab.field.address');
    case 'data.mac': return t('beatbox.vocab.field.mac');
    case 'data.command': return t('beatbox.vocab.field.command');
    case 'data.arguments': return t('beatbox.vocab.field.arguments');
    case 'data.exit_code': return t('beatbox.vocab.field.exit_code');
    case 'data.pid': return t('beatbox.vocab.field.pid');
    case 'data.because': return t('beatbox.vocab.field.because');
    case 'data.output': return t('beatbox.vocab.field.output');
    case 'data.path': return t('beatbox.vocab.field.path');
    case 'data.stdout': return t('beatbox.vocab.field.stdout');
    case 'data.stderr': return t('beatbox.vocab.field.stderr');
    default: return path;
  }
}
