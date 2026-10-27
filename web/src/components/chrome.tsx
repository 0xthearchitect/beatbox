import { Logo } from './Logo';

/**
 * Logo plus name, nothing else. One size in the rail and bars; `large` only
 * in the hero, where at rail size it read like a caption.
 */
export function Wordmark({ large = false }: { large?: boolean }) {
  return (
    <span className={`bb-mark${large ? ' bb-mark--lg' : ''}`}>
      <Logo size={large ? 30 : 22} />
      <b>BEATBOX</b>
    </span>
  );
}
