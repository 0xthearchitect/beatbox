# Avatars

Drop a square image here, named `avatar-<handle in lower case>.<ext>`:

    avatar-thearchitect.png
    avatar-zerodot0ne.png
    avatar-devilboy.png
    avatar-0xadamastor.png

`.png`, `.jpg`, `.jpeg` and `.webp` are all picked up. **The extension has to
match what the file actually is** — `file avatar-*.*` will tell you. Two of the
first four were JPEGs named `.png`, which happens to survive because Chromium
sniffs image bytes and ignores the label, but the one small enough to be inlined
went into the bundle as `data:image/png;base64,/9j/...` — a data URI declaring a
type its own payload contradicts.

256x256 is plenty — they render at 34px, and the file is inlined into the
committed `dist/`, so bigger only makes the repository heavier.

**Rebuild after adding one.** `dist/` is committed and is what the application
loads; a file dropped in here is invisible until `npm run build` has run.

Nothing here is required. `TeamCorner` reads this folder with `import.meta.glob`
and falls back to a member's initials for anyone whose file is missing, so the
build never breaks over an avatar that has not been drawn yet.

**The basename has to be unique across the whole bundle.** `vite.config.ts` emits
assets as `assets/[name][extname]` with no content hash — deliberately, so a
committed `dist/` does not churn on every build — which means two files called
`avatar.png` anywhere in `src/assets/` would collide. Hence the prefix.
