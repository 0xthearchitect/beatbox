import { CubeWire, Pill } from '../components';
import { t } from '../i18n';
import type { RouteDef } from '../routes';

/** A screen we've described but not built. Wireframe cube on purpose: it already means "modelled, not running". */
export function Placeholder({ route }: { route: RouteDef }) {
  return (
    <div className="bb-todo">
      <CubeWire size={92} tone="violet" opacity={0.42} />
      <h2 className="bb-t-title" style={{ margin: 0 }}>{t(`beatbox.screen.${route.id}.title`)}</h2>
      <p>{t(`beatbox.screen.${route.id}.blurb`)}</p>
      <Pill tone="violet" dot={false}>{t('beatbox.todo.phase', { phase: route.phase })}</Pill>
      <code className="bb-mono bb-faint" style={{ fontSize: 11 }}>#/{route.path}</code>
    </div>
  );
}
