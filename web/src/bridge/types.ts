/**
 * Wire shapes, mirroring crates/contract/contract.json by hand.
 *
 * Keep it plain: one `export interface` per type, one field per line.
 * web/tests/contract.test.mjs parses this file with a regex and fails when
 * it drifts from the contract. Blank lines and `//` comments are fine.
 *
 * camelCase here, snake_case in the contract; the conversion is mechanical.
 */

/**
 * A refusal by name (crates/reasons). `code` is a locale key minus the
 * `beatbox.` prefix; render it with say() from ../i18n.
 */
export interface ReasonView {
  code: string;
  values: unknown;
}

export interface ApiError {
  code: string;
  message: string;
  reason: ReasonView | null;
}

export interface Availability {
  ready: boolean;
  reason: ReasonView;
}

export interface SystemProbe {
  platform: string;
  user: string;
  kvm: Availability;
  qemu: Availability;
  frida: Availability;
  terminal: Availability;
  memoryFreeMb: number;
  memoryTotalMb: number;
}

export interface Locale {
  language: string;
  available: string[];
  strings: unknown;
}

export interface Preferences {
  onClose: string;
  builderTools: boolean;
  newMemoryMb: number;
  newCpus: number;
  newReach: string;
  newAgent: boolean;
}

export interface AboutView {
  version: string;
  engineBuild: string;
  enginePid: number;
  folder: string;
  log: string;
  logBytes: number;
}

export interface StoredMachine {
  id: string;
  name: string;
  type: string;
  lifecycle: string;
  bytes: number;
}

export interface StoredDisk {
  name: string;
  bytes: number;
  madeInto: string[];
}

export interface StorageView {
  folder: string;
  machines: StoredMachine[];
  disks: StoredDisk[];
  toolsBytes: number;
  leftoverBytes: number;
}

/** A shortcut folder. `key` is a locale key, not the name on disk. */
export interface Place {
  key: string;
  icon: string;
  path: string;
}

/** One step of the path, spelt as on disk. */
export interface Crumb {
  name: string;
  path: string;
}

/** One row of a folder. Zeroes mean "could not be read", not "empty". */
export interface FileEntry {
  name: string;
  path: string;
  folder: boolean;
  size: number;
  modified: number;
  hidden: boolean;
}

/** One folder listing. `beyond` and `filtered` count the rows left out, so the chooser can say so. */
export interface FolderView {
  path: string;
  parent: string;
  crumbs: Crumb[];
  entries: FileEntry[];
  beyond: number;
  filtered: number;
}

/** What's still running when the window closes. Counts only; the page writes the words. */
export interface ClosingView {
  sandboxes: number;
  installing: boolean;
}

export interface Job {
  id: string;
  kind: string;
  state: string;
  message: string;
  reason: ReasonView | null;
}

export const JOB_RUNNING = 'running';
export const JOB_DONE = 'done';
export const JOB_FAILED = 'failed';

/** A saved command. Empty `machine` = every machine. */
export interface CommandView {
  id: string;
  name: string;
  group: string;
  machine: string;
  linux: string;
  windows: string;
  blanks: unknown;
  timeout: number;
}

/** A watcher and its last answer. */
export interface WatcherView {
  id: string;
  sandboxId: string;
  name: string;
  line: string;
  every: number;
  tell: string;
  text: string;
  paused: boolean;
  state: string;
  runs: number;
  lastAt: string;
  lastExit: number | null;
  lastOutput: string;
  lastReason: ReasonView | null;
  changedAt: string;
}

export interface DiskView {
  name: string;
  path: string;
  bytes: number;
}

export interface BeatDropSourceView {
  path: string;
  name: string;
  bytes: number;
  mountpoints: string[];
  dependent: boolean;
  needed: number;
  problem: string;
}

export interface BeatDropTargetView {
  path: string;
  disk: string;
  free: number;
}

export interface BeatDropInspectView {
  sources: BeatDropSourceView[];
  targets: BeatDropTargetView[];
}

export interface GuestToolsView {
  ready: boolean;
  version: string;
  bytes: number;
}

export interface SetupStepView {
  key: string;
  ready: boolean;
  optional: boolean;
  fixable: boolean;
  reason: ReasonView;
}

export interface SetupView {
  steps: SetupStepView[];
}

export interface DiskInspectView {
  format: string;
  bytes: number;
  firmware: string;
  bootable: boolean;
  virtualboxSnapshots: boolean;
  inside: string;
  installer: string;
  problem: ReasonView | null;
}

/* --- the sandbox runtime ------------------------------------------------- */

/** `kind`: text, number, bool, list, object or any. */
export interface ActionFieldView {
  name: string;
  kind: string;
  required: boolean;
}

/** What one capability takes and gives back. */
export interface ActionSpecView {
  capability: string;
  inputs: ActionFieldView[];
  outputs: ActionFieldView[];
}

export interface SandboxTypeView {
  name: string;
  version: string;
  isolation: string;
  capabilities: string[];
  actions: ActionSpecView[];
}

export interface SandboxSummary {
  id: string;
  type: string;
  name: string;
  lifecycle: string;
  since: string;
}

export interface SandboxConfigView {
  type: string;
  name: string;
  resources: unknown;
  isolation: string;
  settings: unknown;
  /** Role -> sandbox id, e.g. a server's host and network. */
  uses: unknown;
}

export interface SandboxStateView {
  lifecycle: string;
  since: string;
  reason: ReasonView | null;
  resources: unknown;
  facts: unknown;
}

/** `transitions` = what the runtime accepts right now. Grey buttons out from this, never a local lifecycle table. */
export interface SandboxView {
  id: string;
  type: string;
  name: string;
  version: string;
  isolation: string;
  capabilities: string[];
  transitions: string[];
  config: SandboxConfigView;
  state: SandboxStateView;
}

export interface ActionOutcomeView {
  capability: string;
  ok: boolean;
  data: unknown;
  reason: ReasonView | null;
}

export interface SandboxEventView {
  name: string;
  sandboxId: string;
  at: string;
  sequence: number;
  data: unknown;
}

/** Must match crates/engine/src/runtime/lifecycle.rs. */
export const TRANSITIONS = ['start', 'stop', 'pause', 'resume', 'reset', 'destroy'] as const;
export type Transition = (typeof TRANSITIONS)[number];

/** Only `destroyed` is final. */
export const LIFECYCLES = ['created', 'running', 'paused', 'stopped', 'failed', 'destroyed'] as const;
export type Lifecycle = (typeof LIFECYCLES)[number];

type NoPayload = Record<string, never>;

/**
 * Every bridge action with its payload and response; this is what types call().
 * contract.test.mjs checks the keys against the contract.
 */
export interface Api {
  'system.probe': { payload: NoPayload; response: SystemProbe };
  'system.locale': { payload: NoPayload; response: Locale };
  'system.set_language': { payload: { language: string }; response: Locale };
  'system.preferences': { payload: NoPayload; response: Preferences };
  'system.set_preferences': { payload: { onClose: string; builderTools: boolean; newMemoryMb: number; newCpus: number; newReach: string; newAgent: boolean }; response: Preferences };
  'system.about': { payload: NoPayload; response: AboutView };
  'host.reveal': { payload: { what: string }; response: boolean };
  'storage.usage': { payload: NoPayload; response: StorageView };
  'storage.sweep': { payload: NoPayload; response: StorageView };
  'jobs.current': { payload: NoPayload; response: Job | null };
  'disks.list': { payload: NoPayload; response: DiskView[] };
  'beatdrop.inspect': { payload: NoPayload; response: BeatDropInspectView };
  'beatdrop.start': { payload: { source: string; name: string; target: string | null; leaveOut: string[] }; response: Job };
  'beatdrop.folders': { payload: { path: string }; response: DiskView[] };
  'beatdrop.measure': { payload: { path: string }; response: number };
  'beatdrop.cancel': { payload: NoPayload; response: boolean };
  'disks.build': { payload: { source: string; name: string; sizeGb: number; network: boolean; unattended: boolean; machine: unknown }; response: Job };
  'disks.finish': { payload: NoPayload; response: Job };
  'disks.forget': { payload: { name: string }; response: DiskView[] };
  'commands.list': { payload: NoPayload; response: CommandView[] };
  'commands.save': { payload: { command: unknown }; response: CommandView[] };
  'commands.forget': { payload: { id: string }; response: CommandView[] };
  'watchers.list': { payload: NoPayload; response: WatcherView[] };
  'watchers.save': { payload: { watcher: unknown }; response: WatcherView[] };
  'watchers.forget': { payload: { id: string }; response: WatcherView[] };
  'tools.status': { payload: NoPayload; response: GuestToolsView };
  'tools.fetch': { payload: NoPayload; response: Job };
  'tools.insert': { payload: { sandboxId: string }; response: ActionOutcomeView };
  'disks.inspect': { payload: { path: string }; response: DiskInspectView };
  'disks.unpack': { payload: { source: string; name: string }; response: Job };
  'setup.status': { payload: NoPayload; response: SetupView };
  'setup.fix': { payload: { step: string }; response: Job };
  'sandbox.types': { payload: NoPayload; response: SandboxTypeView[] };
  'sandbox.list': { payload: NoPayload; response: SandboxSummary[] };
  'sandbox.view': { payload: { id: string }; response: SandboxView };
  'sandbox.refresh': { payload: { id: string }; response: SandboxView };
  'sandbox.create': { payload: { type: string; name: string; cpu: number; memory: number; settings: unknown; uses: unknown }; response: SandboxView };
  'sandbox.rename': { payload: { id: string; name: string }; response: SandboxView };
  'sandbox.reconfigure': { payload: { id: string; cpu: number; memory: number; settings: unknown }; response: SandboxView };
  'sandbox.use': { payload: { id: string; uses: unknown }; response: SandboxView };
  'sandbox.transition': { payload: { id: string; transition: string }; response: SandboxView };
  'sandbox.act': { payload: { id: string; capability: string; arguments: unknown }; response: ActionOutcomeView };
  'sandbox.events': { payload: { since: number }; response: SandboxEventView[] };
  // Answered by the window (shell/), not the daemon. Same envelope.
  'host.closing': { payload: { choice: string }; response: boolean };
  'host.display': { payload: { id: string; name: string; url: string; password: string }; response: boolean };
  'files.places': { payload: NoPayload; response: Place[] };
  'files.list': { payload: { path: string; suffixes: string }; response: FolderView };
  'files.read': { payload: { path: string }; response: string };
}

/** Pushed events. Checked against the contract too, or a new one would be silently ignored. */
export interface Events {
  'job': Job;
  'locale': Locale;
  'closing': ClosingView;
  'sandbox': SandboxEventView;
}

export type EventName = keyof Events;

export type Action = keyof Api;
