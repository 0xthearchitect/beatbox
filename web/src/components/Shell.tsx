import { useState, type ReactNode } from 'react';
import { ChevronRight } from 'lucide-react';
import {
  Collapsible, CollapsibleContent, CollapsibleTrigger,
} from '@/components/animate-ui/primitives/radix/collapsible';
import {
  Sidebar, SidebarContent, SidebarFooter, SidebarGroup, SidebarGroupLabel, SidebarHeader, SidebarInset,
  SidebarMenu, SidebarMenuButton, SidebarMenuItem, SidebarMenuSub, SidebarMenuSubButton,
  SidebarMenuSubItem, SidebarProvider, SidebarRail, SidebarTrigger,
} from '@/components/animate-ui/components/radix/sidebar';
import {
  Breadcrumb, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbPage, BreadcrumbSeparator,
} from '@/components/ui/breadcrumb';
import { Separator } from '@/components/ui/separator';
import { useIsMobile } from '@/hooks/use-mobile';
import { useChosen, type Kind } from '../chosen';
import { href } from '../router';
import { NAV, SANDBOXES, type RouteDef } from '../routes';
import { t } from '../i18n';
import { useBuilderTools } from '../builder';
import { Logo } from './Logo';
import { Icon } from './Icon';
import { MachineCrumb, MachineSwitcher } from './MachineSwitcher';

/** Rail collapsed or not, kept across launches. */
const RAIL_KEY = 'beatbox.rail.collapsed';

function remembered(): boolean {
  try {
    return window.localStorage.getItem(RAIL_KEY) !== '1';
  } catch {
    return true;
  }
}

function remember(open: boolean) {
  try {
    window.localStorage.setItem(RAIL_KEY, open ? '0' : '1');
  } catch {
    // Can't save a preference? Not worth bothering anyone about.
  }
}

/**
 * The rail and the page beside it.
 *
 * Sandboxes first, each kind nesting its pages (animate-ui's collapsible
 * sections), then product pages, then builder pages. Collapses to icons via
 * the header button, its edge or Ctrl+B; on a narrow window it's a sheet.
 */
export function Shell({ route, children }: { route: RouteDef; children: ReactNode }) {
  const [open, setOpen] = useState(remembered);
  // Narrow window: the rail is a sheet, so the crumb's menu is how you switch.
  const isMobile = useIsMobile();
  const builderTools = useBuilderTools();
  // Read once here for the switcher, the crumb and the pen label.
  const hosts = useChosen('host');
  const networks = useChosen('network');
  const servers = useChosen('server');
  const kinded = (key: string) => (key === 'network' ? 'network' : key === 'server' ? 'server' : 'host') as Kind;
  const chosenOf = (key: string) => ({ host: hosts, network: networks, server: servers })[kinded(key)];
  const penIn = Boolean((hosts.machine?.state.facts as Record<string, unknown> | undefined)?.pen);
  const kind = SANDBOXES.find((sandbox) => sandbox.options.some((option) => option.path === route.path));
  const option = kind?.options.find((each) => each.path === route.path);
  const chosen = chosenOf(kind?.key ?? 'host');
  const nav = NAV.find((entry) => entry.key === route.nav);
  // Breadcrumb parent: a sandbox links back, a group is just a heading.
  const parent = kind
    ? { label: t(`beatbox.nav.${kind.key}`), to: kind.options[0]!.path }
    : nav && nav.section !== 'foot' ? { label: t(`beatbox.section.${nav.section}`), to: null } : null;
  const page = option ? t(`beatbox.nav.${option.key}`) : nav ? t(`beatbox.nav.${nav.key}`) : '';

  const navItems = (section: string) => NAV.filter((entry) => entry.section === section).map((entry) => (
    <SidebarMenuItem key={entry.key}>
      <SidebarMenuButton asChild tooltip={t(`beatbox.nav.${entry.key}`)}>
        <a href={href(entry.path)} style={{ textDecoration: 'none' }}
           aria-current={entry.key === route.nav ? 'page' : undefined}>
          <Icon name={entry.icon} size="sm" />
          <span>{t(`beatbox.nav.${entry.key}`)}</span>
        </a>
      </SidebarMenuButton>
    </SidebarMenuItem>
  ));

  return (
    <SidebarProvider className="h-full min-h-0" open={open}
                     onOpenChange={(next) => { setOpen(next); remember(next); }}>
      <Sidebar collapsible="icon">
        <SidebarHeader>
          <SidebarMenu>
            <SidebarMenuItem>
              <SidebarMenuButton asChild size="lg" tooltip="BeatBox">
                <a href={href('hero')} style={{ textDecoration: 'none' }}>
                  <span className="flex size-8 shrink-0 items-center justify-center">
                    <Logo size={22} />
                  </span>
                  <b className="text-sidebar-accent-foreground text-sm font-semibold tracking-[0.16em]">BEATBOX</b>
                </a>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
        </SidebarHeader>

        <SidebarContent>
          <SidebarGroup>
            <SidebarGroupLabel>{t('beatbox.nav.sandbox')}</SidebarGroupLabel>
            <SidebarMenu>
              {SANDBOXES.map((sandbox) => (
                <Collapsible key={sandbox.key} asChild defaultOpen className="group/collapsible">
                  <SidebarMenuItem>
                    {/* Collapsed to icons the sub-pages are hidden, so the icon
                        jumps straight to the first one. */}
                    {open ? (
                      <CollapsibleTrigger asChild>
                        <SidebarMenuButton tooltip={t(`beatbox.nav.${sandbox.key}`)}>
                          <Icon name={sandbox.icon} size="sm" />
                          <span>{t(`beatbox.nav.${sandbox.key}`)}</span>
                          <ChevronRight className="ml-auto transition-transform duration-300 group-data-[state=open]/collapsible:rotate-90" />
                        </SidebarMenuButton>
                      </CollapsibleTrigger>
                    ) : (
                      <SidebarMenuButton asChild tooltip={t(`beatbox.nav.${sandbox.key}`)}>
                        <a href={href(sandbox.options[0]!.path)} style={{ textDecoration: 'none' }}>
                          <Icon name={sandbox.icon} size="sm" />
                          <span>{t(`beatbox.nav.${sandbox.key}`)}</span>
                        </a>
                      </SidebarMenuButton>
                    )}
                    <CollapsibleContent>
                      <SidebarMenuSub>
                        <SidebarMenuSubItem>
                          <MachineSwitcher kind={kinded(sandbox.key)} {...chosenOf(sandbox.key)} />
                        </SidebarMenuSubItem>
                        {/* None of this kind yet: hide the sub-pages, they'd all be
                            the same "make one" form. Check `=== 0`, not falsy:
                            while loading (null) they stay, so they don't blink. */}
                        {chosenOf(sandbox.key).list?.length === 0 ? null : sandbox.options.map((each) => (
                          <SidebarMenuSubItem key={each.key}>
                            <SidebarMenuSubButton asChild>
                              <a href={href(each.path)} style={{ textDecoration: 'none' }}
                                 aria-current={each === option ? 'page' : undefined}>
                                <span>{t(`beatbox.nav.${each.key}`)}</span>
                                {each.key === 'pen' && penIn ? (
                                  <span className="text-muted-foreground ml-auto text-xs">{t('beatbox.switcher.pen_in')}</span>
                                ) : null}
                              </a>
                            </SidebarMenuSubButton>
                          </SidebarMenuSubItem>
                        ))}
                      </SidebarMenuSub>
                    </CollapsibleContent>
                  </SidebarMenuItem>
                </Collapsible>
              ))}
            </SidebarMenu>
          </SidebarGroup>


          {/* Builder pages, only when Settings turns them on (../builder.ts).
              Their routes work either way. */}
          {builderTools ? (
            <SidebarGroup>
              <SidebarGroupLabel>{t('beatbox.section.build')}</SidebarGroupLabel>
              <SidebarMenu>{navItems('build')}</SidebarMenu>
            </SidebarGroup>
          ) : null}
        </SidebarContent>
        <SidebarFooter className="border-sidebar-border border-t">
          <SidebarMenu>{navItems('foot')}</SidebarMenu>
        </SidebarFooter>
        <SidebarRail />
      </Sidebar>

      <SidebarInset className="min-h-0 min-w-0">
        {/* Same header as the animate-ui demo, shrinking with the rail. */}
        <header className="flex h-16 shrink-0 items-center gap-2 transition-[width,height] ease-linear group-has-[[data-collapsible=icon]]/sidebar-wrapper:h-12">
          <div className="flex items-center gap-2 px-4">
            <SidebarTrigger className="-ml-1" aria-label={t(open ? 'beatbox.shell.collapse' : 'beatbox.shell.expand')} />
            <Separator orientation="vertical" className="mr-2 data-[orientation=vertical]:h-4" />
            <Breadcrumb>
              <BreadcrumbList>
                {parent ? (
                  <>
                    <BreadcrumbItem className="hidden md:block">
                      {parent.to
                        ? <BreadcrumbLink href={href(parent.to)}>{parent.label}</BreadcrumbLink>
                        : parent.label}
                    </BreadcrumbItem>
                    <BreadcrumbSeparator className="hidden md:block" />
                  </>
                ) : null}
                {kind && chosen.machine ? (
                  <>
                    <BreadcrumbItem>
                      <MachineCrumb kind={kinded(kind.key)} {...chosen} menu={!open || isMobile} />
                    </BreadcrumbItem>
                    <BreadcrumbSeparator />
                  </>
                ) : null}
                <BreadcrumbItem>
                  <BreadcrumbPage>{page}</BreadcrumbPage>
                </BreadcrumbItem>
              </BreadcrumbList>
            </Breadcrumb>
          </div>
        </header>
        <div className="bb-main">{children}</div>
      </SidebarInset>
    </SidebarProvider>
  );
}
