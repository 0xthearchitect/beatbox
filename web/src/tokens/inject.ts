import { cssVariables, typeClasses } from './index';

/**
 * Put the tokens into the document before React paints.
 *
 * Generated, not a hand-written stylesheet: a value kept in two places is
 * always wrong in the one nobody edited.
 */
export function injectTokens(doc: Document = document): void {
  const id = 'beatbox-tokens';
  const existing = doc.getElementById(id);
  const style = existing ?? doc.createElement('style');
  style.id = id;
  style.textContent = cssVariables() + typeClasses();
  if (!existing) doc.head.prepend(style);
}
