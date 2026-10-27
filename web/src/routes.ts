/**
 * Every destination in BeatBox.
 *
 * `phase` = which phase delivers it; unbuilt screens show a Placeholder, not a blank.
 *
 * Two audiences, one rail: the product (sandboxes in plain words) and
 * `build` (Engine with raw names and events, Foundations with the design
 * system) for whoever builds BeatBox. Build stays hidden unless Settings
 * turns it on.
 */

import type { IconName } from './components';
import type { Key, KeySuffix } from './i18n';

type ScreenId = Key extends infer K ? K extends `beatbox.screen.${infer Id}.title` ? Id : never : never;

export type NavKey = 'settings' | 'engine' | 'foundations';

/** Rail spot below the sandboxes: `foot` = pinned at the bottom, `build` = its own group. */
export type Section = 'foot' | 'build';
export type Chrome = 'bare' | 'shell';

export interface RouteDef {
  readonly id: ScreenId;
  readonly path: string;
  readonly nav: NavKey | null;
  readonly chrome: Chrome;
  readonly phase: 1 | 2 | 3 | 4;
}

export const ROUTES: readonly RouteDef[] = [
  { id: 'hero', path: 'hero', nav: null, chrome: 'bare', phase: 1 },
  { id: 'account', path: 'account', nav: null, chrome: 'bare', phase: 1 },
  { id: 'setup', path: 'setup', nav: null, chrome: 'bare', phase: 1 },
  { id: 'host.machine', path: 'host/machine', nav: null, chrome: 'shell', phase: 1 },
  { id: 'host.commands', path: 'host/commands', nav: null, chrome: 'shell', phase: 1 },
  { id: 'host.watchers', path: 'host/watchers', nav: null, chrome: 'shell', phase: 1 },
  { id: 'host.pen', path: 'host/pen', nav: null, chrome: 'shell', phase: 1 },
  { id: 'network.overview', path: 'network/overview', nav: null, chrome: 'shell', phase: 3 },
  { id: 'network.router', path: 'network/router', nav: null, chrome: 'shell', phase: 3 },
  { id: 'network.names', path: 'network/names', nav: null, chrome: 'shell', phase: 3 },
  { id: 'network.firewall', path: 'network/firewall', nav: null, chrome: 'shell', phase: 3 },
  { id: 'network.traffic', path: 'network/traffic', nav: null, chrome: 'shell', phase: 3 },
  { id: 'network.flows', path: 'network/flows', nav: null, chrome: 'shell', phase: 3 },
  { id: 'server.services', path: 'server/services', nav: null, chrome: 'shell', phase: 4 },
  { id: 'server.try', path: 'server/try', nav: null, chrome: 'shell', phase: 4 },
  { id: 'server.library', path: 'server/library', nav: null, chrome: 'shell', phase: 4 },
  { id: 'server.settings', path: 'server/settings', nav: null, chrome: 'shell', phase: 4 },
  { id: 'settings', path: 'settings', nav: 'settings', chrome: 'shell', phase: 2 },
  { id: 'engine', path: 'engine', nav: 'engine', chrome: 'shell', phase: 1 },
  { id: 'foundations', path: 'foundations', nav: 'foundations', chrome: 'shell', phase: 1 },
];

export const NAV: readonly { key: NavKey; icon: IconName; path: string; section: Section }[] = [
  { key: 'settings', icon: 'settings', path: 'settings', section: 'foot' },
  { key: 'engine', icon: 'code', path: 'engine', section: 'build' },
  { key: 'foundations', icon: 'box', path: 'foundations', section: 'build' },
];

/** Where an empty hash lands. Not the hero: main.tsx rewrites '' on boot, so it'd never show. */
export const DEFAULT_ROUTE = 'host/machine';

export const routeFor = (path: string): RouteDef =>
  ROUTES.find((route) => route.path === path) ??
  ROUTES.find((route) => route.path === DEFAULT_ROUTE)!;

/** A sandbox kind on the rail and its pages. New kinds get added here, one route per page. */
export interface SandboxOption {
  readonly key: KeySuffix<'beatbox.nav.'>;
  readonly icon: IconName;
  readonly path: string;
}

export interface SandboxKind {
  readonly key: KeySuffix<'beatbox.nav.'>;
  readonly icon: IconName;
  readonly options: readonly SandboxOption[];
}

export const SANDBOXES: readonly SandboxKind[] = [
  {
    key: 'host',
    icon: 'cpu',
    options: [
      { key: 'machine', icon: 'box', path: 'host/machine' },
      { key: 'commands', icon: 'terminal', path: 'host/commands' },
      { key: 'watchers', icon: 'eye', path: 'host/watchers' },
      { key: 'pen', icon: 'usb', path: 'host/pen' },
    ],
  },
  {
    key: 'network',
    icon: 'network',
    options: [
      { key: 'overview', icon: 'network', path: 'network/overview' },
      { key: 'router', icon: 'router', path: 'network/router' },
      { key: 'names', icon: 'tag', path: 'network/names' },
      { key: 'firewall', icon: 'shield', path: 'network/firewall' },
      { key: 'flows', icon: 'logs', path: 'network/flows' },
      { key: 'traffic', icon: 'pulse', path: 'network/traffic' },
    ],
  },
  {
    key: 'server',
    icon: 'server',
    options: [
      { key: 'services', icon: 'server', path: 'server/services' },
      { key: 'library', icon: 'book', path: 'server/library' },
      { key: 'server_configuration', icon: 'settings', path: 'server/settings' },
    ],
  },
];
