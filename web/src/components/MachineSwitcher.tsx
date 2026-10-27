import { useEffect, useState, type ReactNode } from 'react';
import { Check, ChevronDown, ChevronsUpDown, Plus } from 'lucide-react';
import {
  DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/animate-ui/components/radix/dropdown-menu';
import { call, on } from '../bridge/client';
import type { SandboxSummary, SandboxView } from '../bridge/types';
import type { Kind } from '../chosen';
import { t, runtimeText } from '../i18n';
import { navigate } from '../router';
import { Cube } from './Cube';

/**
 * Pick which machine (or network, server) the pages are about.
 *
 * animate-ui's team switcher at the top of the rail, with the same menu on
 * the breadcrumb name, so you can switch from either place.
 */

function Dot({ lifecycle }: { lifecycle: string }) {
  const running = lifecycle === 'running';
  return (
    <span className={`size-1.5 shrink-0 rounded-full ${running ? 'bg-[var(--bb-color-mint)]' : 'bg-muted-foreground'}`} />
  );
}

/** Cube on a tile: mint while running, slate otherwise. */
function Tile({ lifecycle, size }: { lifecycle: string; size: 'sm' | 'md' }) {
  const running = lifecycle === 'running';
  return (
    <span className={`flex shrink-0 items-center justify-center rounded-lg ${size === 'md' ? 'size-8' : 'size-7'} ${running ? 'bg-[var(--bb-color-mint-wash)]' : 'bg-accent'}`}>
      <Cube size={size === 'md' ? 18 : 16} tone={running ? 'mint' : 'slate'} />
    </span>
  );
}

/** Per kind: its words in the switcher and where "New" goes. */
const WORDS = {
  host: {
    heading: 'beatbox.switcher.machines', none: 'beatbox.switcher.none', make: 'beatbox.switcher.make_one',
    new: 'beatbox.machine.new', change: 'beatbox.switcher.change', to: 'host/machine',
  },
  network: {
    heading: 'beatbox.switcher.networks', none: 'beatbox.switcher.no_networks', make: 'beatbox.switcher.make_network',
    new: 'beatbox.network.new', change: 'beatbox.switcher.change_network', to: 'network/overview',
  },
  server: {
    heading: 'beatbox.switcher.servers', none: 'beatbox.switcher.no_servers', make: 'beatbox.switcher.make_server',
    new: 'beatbox.server.new', change: 'beatbox.switcher.change_server', to: 'server/services',
  },
} as const;

function Menu({ kind, list, chosen, onChoose, children, side }: {
  kind: Kind;
  list: SandboxSummary[];
  chosen: string;
  onChoose: (id: string) => void;
  children: ReactNode;
  side: 'right' | 'bottom';
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>{children}</DropdownMenuTrigger>
      <DropdownMenuContent side={side} align="start" sideOffset={side === 'right' ? 8 : 6}
                           className="w-72 rounded-lg">
        <DropdownMenuLabel className="text-muted-foreground text-xs font-normal">
          {t(WORDS[kind].heading)}
        </DropdownMenuLabel>
        {list.map((row) => (
          <DropdownMenuItem key={row.id} className="gap-2.5 p-2" onSelect={() => onChoose(row.id)}>
            <Tile lifecycle={row.lifecycle} size="sm" />
            <span className="flex min-w-0 flex-1 flex-col">
              <span className="truncate text-[13.5px] font-medium">{row.name}</span>
              <span className="text-muted-foreground text-xs">{runtimeText(`beatbox.plain.state.${row.lifecycle}`)}</span>
            </span>
            {row.id === chosen ? <Check className="text-foreground" /> : null}
          </DropdownMenuItem>
        ))}
        {list.length ? <DropdownMenuSeparator /> : null}
        <DropdownMenuItem className="gap-2.5 p-2" onSelect={() => navigate(WORDS[kind].to, { new: '1' })}>
          <span className="flex size-7 items-center justify-center rounded-lg border border-dashed">
            <Plus className="size-3.5" />
          </span>
          <span className="text-muted-foreground text-[13.5px]">{t(WORDS[kind].new)}</span>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** Shared by the switcher and the crumb. */
export interface Chosen {
  kind: Kind;
  list: SandboxSummary[] | null;
  machine: SandboxView | null;
  choose: (id: string) => void;
}

/**
 * What a running server waits on: its machine or network, if that's off.
 * The server still says "running" (services come back with the machine),
 * but a mint dot would lie that it's working.
 */
function useWaiting(kind: Kind, machine: SandboxView | null): 'host' | 'network' | null {
  const [off, setOff] = useState<'host' | 'network' | null>(null);
  const uses = (machine?.config.uses ?? {}) as Record<string, unknown>;
  const running = machine?.state.lifecycle === 'running';
  useEffect(() => {
    if (kind !== 'server' || !running) { setOff(null); return; }
    let live = true;
    const look = () => {
      void call('sandbox.list').then((all) => {
        if (!live) return;
        const stopped = (id: unknown) => all.some((row) => row.id === id && row.lifecycle !== 'running');
        setOff(stopped(uses.host) ? 'host' : stopped(uses.network) ? 'network' : null);
      }, () => undefined);
    };
    look();
    const stop = on('sandbox', (event) => { if (event.name.startsWith('sandbox.') && event.name !== 'sandbox.action') look(); });
    return () => { live = false; stop(); };
  }, [kind, running, uses.host, uses.network]);
  return off;
}

/** The card at the top of the rail. */
export function MachineSwitcher({ kind, list, machine, choose }: Chosen) {
  const waiting = useWaiting(kind, machine);
  if (list === null) return null;
  const words = WORDS[kind];
  const lifecycle = waiting ? 'stopped' : machine?.state.lifecycle ?? 'stopped';
  const resources = (machine?.config.resources ?? {}) as Record<string, unknown>;
  const nodes = (machine?.state.facts as { nodes?: unknown[] } | undefined)?.nodes;
  const services = (machine?.state.facts as { services?: unknown[] } | undefined)?.services;
  const sized = !machine ? ''
    : kind === 'network' ? t('beatbox.switcher.nodes', { count: Array.isArray(nodes) ? nodes.length : 0 })
    : kind === 'server' ? t('beatbox.switcher.services', { count: Array.isArray(services) ? services.length : 0 })
    : t('beatbox.switcher.sized', {
      cpu: String(resources.cpu ?? '?'),
      memory: String(Math.round(Number(resources.memory ?? 0) / 1024 * 10) / 10),
    });
  return (
    <Menu kind={kind} list={list} chosen={machine?.id ?? ''} onChoose={choose} side="right">
      <button type="button"
              aria-label={machine ? t(words.change, { name: machine.name }) : t(words.new)}
              className="bg-secondary hover:bg-sidebar-accent data-[state=open]:bg-sidebar-accent mt-0.5 mb-1.5 flex w-full items-center gap-2.5 rounded-lg border p-2 text-left transition-colors">
        {machine ? <Tile lifecycle={lifecycle} size="md" /> : (
          <span className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-dashed">
            <Plus className="size-4" />
          </span>
        )}
        <span className="flex min-w-0 flex-1 flex-col">
          <span className="truncate text-[13.5px] font-semibold">
            {machine ? machine.name : t(words.none)}
          </span>
          <span className="text-muted-foreground flex items-center gap-1.5 text-xs">
            {machine ? (
              <>
                <Dot lifecycle={lifecycle} />
                <span className="truncate" title={sized}>{waiting ? t(`beatbox.switcher.waiting_${waiting}`) : runtimeText(`beatbox.plain.state.${lifecycle}`)}{sized ? ` · ${sized}` : ''}</span>
              </>
            ) : t(words.make)}
          </span>
        </span>
        <ChevronsUpDown className="text-muted-foreground size-4 shrink-0" />
      </button>
    </Menu>
  );
}

/** The machine's name in the breadcrumb, with the same menu for when the rail is hidden. */
export function MachineCrumb({ kind, list, machine, choose, menu = true }: Chosen & { menu?: boolean }) {
  if (!list || !machine) return null;
  // Rail open: its switcher already does this, so just the name.
  if (!menu) {
    return (
      <span className="text-foreground flex items-center gap-1.5 text-[13.5px]">
        <Dot lifecycle={machine.state.lifecycle} />
        {machine.name}
      </span>
    );
  }
  return (
    <Menu kind={kind} list={list} chosen={machine.id} onChoose={choose} side="bottom">
      <button type="button" aria-label={t(WORDS[kind].change, { name: machine.name })}
              className="bg-secondary hover:bg-accent data-[state=open]:bg-accent text-foreground flex items-center gap-1.5 rounded-md border border-[var(--bb-color-line)] py-0.5 pr-1.5 pl-2 text-[13.5px] transition-colors">
        <Dot lifecycle={machine.state.lifecycle} />
        {machine.name}
        <ChevronDown className="text-muted-foreground size-3.5" />
      </button>
    </Menu>
  );
}
