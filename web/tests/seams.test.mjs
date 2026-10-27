// Joins between files held only by a string: a CSS class a component must
// set, a component every caller must use. Rename one end and everything
// builds, everything passes, and something on screen quietly breaks.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { all, read, relativeToSrc, sources, stylesheet, text } from './support.mjs';

// .bb-filters only via ChipRow, so every chip row animates the same.
test('every chip row goes through the one component', () => {
  const loose = sources()
    .filter((path) => text(path).includes('className="bb-filters"') && !path.endsWith('Burst.tsx'))
    .map(relativeToSrc);
  assert.deepEqual(loose, [], 'these write a chip row instead of using ChipRow');
});

// No WebGL or reduced motion: the CSS grid is the hero's background. It only
// shows when the field explicitly says "no", so both failure paths must say it.
test('the hero keeps a ground when there is no webgl', () => {
  const css = stylesheet();
  assert.ok(css.includes('.bb-hero::before'), 'the fallback grid is gone');
  assert.ok(css.includes('repeating-linear-gradient'));

  const shows = css.match(/\.bb-hero--(\w+)::before\s*\{[^}]*opacity:\s*1/);
  assert.ok(shows, 'nothing shows the grid, so a machine without WebGL gets nothing');
  const modifier = `bb-hero--${shows[1]}`;

  const hero = read('screens/Hero.tsx');
  assert.ok(hero.includes(modifier), `${modifier} is never applied`);
  // On an explicit "no field", never on "not known yet".
  assert.match(hero, new RegExp(`field === false \\? ' ${modifier}' : ''`));
  assert.ok(hero.includes('onLive={setField}'));

  // Checked per branch, not by counting calls: lost-context and unmount call it too.
  const field = read('components/TerminalField.tsx');
  const calm = field.match(/prefers-reduced-motion[^\n]*\)\s*\{(.*?)\n {4}\}/s);
  assert.ok(calm, 'the reduced-motion guard is gone or was reshaped');
  assert.ok(calm[1].includes('report.current?.(false)'),
    'reduced motion returns without asking for the grid, so that reader gets nothing');
  const nowebgl = field.match(/\}\s*catch\s*\{(.*?)\n {4}\}/s);
  assert.ok(nowebgl, 'the no-WebGL catch is gone');
  assert.ok(nowebgl[1].includes('report.current?.(false)'));
});

// A descendant selector once stacked "Break the box." one letter per line.
test('the hero accent rule reaches the line and not its letters', () => {
  const css = stylesheet();
  assert.ok(css.includes('.bb-hero-title > span {'), 'the accent rule lost its child combinator');
  assert.doesNotMatch(css, /\.bb-hero-title span\s*\{/,
    'a descendant selector here styles every character of the headline');
});

// Screen readers must get the real headline: noise aria-hidden, text in a
// visually-hidden span, and no CSS that undoes that.
test('text that scrambles is still announced', () => {
  const source = read('components/DecryptedText.tsx');
  assert.ok(source.includes('className="bb-sr">{text}'), 'the real text is not rendered for a reader');
  assert.ok(source.includes('aria-hidden="true"'), 'the noise is offered to screen readers');

  const rule = stylesheet().match(/\.bb-sr\s*\{([^}]*)\}/);
  assert.ok(rule, 'the visually-hidden class is gone');
  assert.ok(rule[1].includes('clip:'));
  for (const wrong of ['display: none', 'visibility: hidden']) {
    assert.ok(!rule[1].includes(wrong), `${wrong} hides it from screen readers too`);
  }
});

// Only guest works, so the hero skips the account screen.
test('the hero goes straight in while guest is the only way in', () => {
  const hero = read('screens/Hero.tsx');
  assert.ok(hero.includes("navigate('host/machine')"), "the hero's call to action no longer goes straight to the sandbox");
  assert.ok(!hero.includes("navigate('account'"), 'the hero sends people through the account screen again');
  // `next` is part of the window's hash, so it is a string anyone can type.
  assert.match(read('screens/Account.tsx'), /routeFor\(params\.get\('next'\)/,
    'the account screen navigates to whatever `next` said, unchecked');
});

// Sign in/up are drawn but inert: only one acts, navigation checks `live`, the dead ones say "soon".
test('the two unbuilt ways in are drawn and inert', () => {
  const source = read('screens/Account.tsx');
  const picks = [...source.matchAll(/\{ id: '(\w+)', live: (true|false) \}/g)].map((m) => [m[1], m[2]]);
  assert.deepEqual(picks, [['guest', 'true'], ['signin', 'false'], ['signup', 'false']],
    'the ways in changed; Account.tsx says what is left before widening this');
  assert.match(source, /pick\.live\)\s*navigate\(next\)/, 'the click is no longer gated on `live`');
  assert.ok(source.includes('aria-disabled={pick.live ? undefined : true}'),
    'a cube that will not act has to say so to a screen reader too');
  assert.match(source, /pick\.live \? null : <span className="bb-way-soon">/,
    'the unbuilt cubes lost the label that says they are unbuilt');
  assert.ok(source.includes('CubeWire'), 'the unbuilt cubes are no longer wireframes');
});

// Selection brightens the cube; it doesn't spin.
test('the way-in cube no longer spins', () => {
  const css = stylesheet();
  assert.ok(!css.includes('bb-way-turn'), "the way-in cube's spin keyframe is back");
  const glide = css.match(/\.bb-way\.on \.bb-way-spin \{([^}]*)\}/);
  assert.ok(glide, "the selected way-in cube's own rule is gone");
  assert.ok(!glide[1].includes('animation'), 'the selected way-in cube animates again');
});

// The sidebar component forgets collapsed state; we keep it across launches.
test('the rail remembers being narrowed', () => {
  const shell = read('components/Shell.tsx');
  assert.match(shell, /localStorage\.getItem\(RAIL_KEY\)/, 'the rail no longer reads the choice back');
  assert.match(shell, /onOpenChange=\{\(next\) => \{ setOpen\(next\); remember\(next\); \}\}/,
    'the rail no longer writes the choice down');
});

// Collapsed rail hides sub-menus, so the sandbox icon must navigate by itself.
test('a narrowed sandbox still reaches what it offers', () => {
  assert.match(read('components/Shell.tsx'), /href=\{href\(sandbox\.options\[0\]!\.path\)\}/,
    'the icon of a narrowed sandbox opens nothing');
});

// Card titles are headings, so screen readers can jump card to card.
test("a card's title is a heading", () => {
  assert.match(read('components/primitives.tsx'), /<h2 className="bb-panelhd-title">\{title\}<\/h2>/,
    "a card's title is no longer a heading");
});
