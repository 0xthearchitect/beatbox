// The layout check `beatbox --proof walk` runs on every route and width.
(() => {
  const bad = [];

  // Panels that touch. Two *painted* blocks -- a border or a background of
  // their own -- stacked with no air between them, and not of the same kind.
  //
  // "Not of the same kind" keeps lists out: rows sharing a hairline are a
  // table and should look like one. Two different things edge to edge is
  // what a person spots in a screenshot.
  const painted = (el) => {
    const style = getComputedStyle(el);
    if (style.display === 'none' || style.position !== 'static') return false;
    const border = ['Top', 'Right', 'Bottom', 'Left']
      .some((side) => parseFloat(style['border' + side + 'Width']) > 0);
    const filled = style.backgroundColor !== 'rgba(0, 0, 0, 0)' && style.backgroundColor !== 'transparent';
    return (border || filled) && el.clientHeight > 12 && el.clientWidth > 40;
  };
  const name = (el) => (typeof el.className === 'string' && el.className.trim())
    ? '.' + el.className.trim().split(/\s+/).join('.') : el.tagName.toLowerCase();

  // A table lays its own cells out, and an element nobody named is not a
  // panel anyone designed.
  const INSIDE_A_TABLE = 'table, thead, tbody, tfoot, tr, th, td';
  const panels = [...document.querySelectorAll('body *')].filter(
    (el) => painted(el) && !el.matches(INSIDE_A_TABLE) && name(el).startsWith('.'),
  );
  for (let i = 0; i < panels.length; i++) {
    for (let j = 0; j < panels.length; j++) {
      if (i === j) continue;
      const a = panels[i], b = panels[j];
      if (a.contains(b) || b.contains(a)) continue;
      if (name(a) === name(b)) continue;
      const top = a.getBoundingClientRect(), bottom = b.getBoundingClientRect();
      const gap = bottom.top - top.bottom;
      if (gap < -0.5 || gap >= 2) continue;
      const across = Math.min(top.right, bottom.right) - Math.max(top.left, bottom.left);
      if (across < Math.min(top.width, bottom.width) * 0.3) continue;
      bad.push(`${name(a)} touches ${name(b)}  [gap ${gap.toFixed(1)}px]`);
    }
  }

  for (const el of document.querySelectorAll('body *')) {
    if (!el.clientWidth) continue;
    if (el.scrollWidth <= el.clientWidth + 1) continue;
    const style = getComputedStyle(el);
    // Only `hidden` and `clip` lose anything. `auto`/`scroll` put it a scroll
    // away, and `visible` (what a tooltip or badge outside its box relies on)
    // paints it in full.
    if (style.overflowX !== 'hidden' && style.overflowX !== 'clip') continue;
    if (style.textOverflow === 'ellipsis') continue;
    // Screen-reader-only text: a 1x1 clipped box is always overflowing on
    // purpose, so skip it.
    if (el.clientWidth <= 1 && el.clientHeight <= 1) continue;
    // A text field reports the width of everything typed in, but the caret
    // scrolls it, so nothing is lost.
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName)) continue;
    const label = (el.className && typeof el.className === 'string')
      ? '.' + el.className.trim().split(/\s+/).join('.')
      : el.tagName.toLowerCase();
    bad.push(`${label}  ${el.scrollWidth} > ${el.clientWidth}  [overflow-x: ${style.overflowX}]`);
  }
  return JSON.stringify({
    sideways: document.documentElement.scrollWidth > document.documentElement.clientWidth + 1,
    bad: [...new Set(bad)],
  });
})()
