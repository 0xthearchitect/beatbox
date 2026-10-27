import { createRoot } from 'react-dom/client';
import { App } from './App';
import { call } from './bridge/client';
import { needsSetup } from './screens/Setup';
import { loadLocale, t } from './i18n';
import { injectTokens } from './tokens/inject';
import './styles/fonts.css';
import './styles/app.css';

injectTokens();

const host = document.getElementById('root');
if (!host) throw new Error('index.html has no #root');
const root = createRoot(host);

declare global {
  interface Window {
    __beatboxDisplay?: { id: string; name: string; url: string; password: string };
  }
}

/** Fresh window opens on the machines, or on Setup if this computer is missing something. A reload keeps its hash. */
async function landing(): Promise<void> {
  if (window.location.hash) return;
  let path = '/host/machine';
  try {
    if (needsSetup(await call('setup.status'))) path = '/setup';
  } catch {
    // No daemon: the machines screen reports that, not us.
  }
  window.location.hash = path;
}

/**
 * Load text before paint, but never leave a blank window if the bridge is gone.
 * A failed load leaves t() returning raw keys. Ugly on purpose: see i18n/fallback.ts.
 */
const display = window.__beatboxDisplay;
if (display) {
  Promise.all([
    loadLocale().catch((error) => console.error('[beatbox] locale unavailable', error)),
    import('./screens/MachineDisplay'),
  ]).then(([, { MachineDisplay }]) => root.render(<MachineDisplay {...display} />))
    .catch((error) => {
      console.error('[beatbox] machine display unavailable', error);
      root.render(
        <main style={{ padding: 24, color: '#f3f0ff', background: '#080810', minHeight: '100%' }}>
          {t('beatbox.display.unavailable', { detail: String(error) })}
        </main>,
      );
    });
} else {
  Promise.all([
    loadLocale().catch((error) => console.error('[beatbox] locale unavailable', error)),
    landing(),
  ]).finally(() => root.render(<App />));
}
