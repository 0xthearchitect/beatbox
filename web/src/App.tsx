import { Closing, Shell } from './components';
import { TitleBar } from './components/TitleBar';
import { useRoutePath } from './router';
import { routeFor } from './routes';
import { Account } from './screens/Account';
import { Foundations } from './screens/Foundations';
import { Hero } from './screens/Hero';
import { Setup } from './screens/Setup';
import { Placeholder } from './screens/Placeholder';
import { Engine } from './screens/Engine';
import { Sandbox } from './screens/Sandbox';
import { Settings } from './screens/Settings';
import { Commands } from './screens/Commands';
import { Watchers } from './screens/Watchers';
import { Pen } from './screens/Pen';
import { NetworkFirewall, NetworkFlows, NetworkNames, NetworkOverview, NetworkRouter, NetworkTraffic } from './screens/Network';
import { ServerServices } from './screens/Server';
import { ServerLibrary, ServerSettings, ServerTry } from './screens/ServerMore';

export function App() {
  const route = routeFor(useRoutePath());

  const screen =
    route.id === 'hero' ? <Hero />
    : route.id === 'account' ? <Account />
    : route.id === 'setup' ? <Setup />
    : route.id === 'foundations' ? <Foundations />
    : route.id === 'host.machine' ? <Sandbox />
    : route.id === 'host.commands' ? <Commands />
    : route.id === 'host.watchers' ? <Watchers />
    : route.id === 'host.pen' ? <Pen />
    : route.id === 'network.overview' ? <NetworkOverview />
    : route.id === 'network.router' ? <NetworkRouter />
    : route.id === 'network.names' ? <NetworkNames />
    : route.id === 'network.firewall' ? <NetworkFirewall />
    : route.id === 'network.traffic' ? <NetworkTraffic />
    : route.id === 'network.flows' ? <NetworkFlows />
    : route.id === 'server.services' ? <ServerServices />
    : route.id === 'server.try' ? <ServerTry />
    : route.id === 'server.library' ? <ServerLibrary />
    : route.id === 'server.settings' ? <ServerSettings />
    : route.id === 'engine' ? <Engine />
    : route.id === 'settings' ? <Settings />
    : <Placeholder route={route} />;

  // <Closing /> answers the title bar's X, not a screen, so it lives outside
  // both chromes and pops up when the window asks.
  if (route.chrome === 'bare') {
    return <><TitleBar /><div className="bb-app"><div className="bb-column">{screen}</div><Closing /></div></>;
  }
  return <><TitleBar /><div className="bb-app"><Shell route={route}>{screen}</Shell><Closing /></div></>;
}
