# Translation workflow

Edit `en/<domain>.json` and `pt_PT/<domain>.json`. English is the source
catalogue. Keys remain flat and unchanged; `domains.json` assigns their existing
prefixes to eleven product domains, including the corresponding screen titles.
Add a prefix to that manifest when introducing a new family.

After editing, run from the repository root:

```sh
npm --prefix web run locales:generate
npm --prefix web run typecheck
npm --prefix web test
npm --prefix web run build
```

Commit the domain files, generated `en.json` and `pt_PT.json`, generated
`web/src/i18n/keys.d.ts`, and rebuilt `web/dist/`. The flat files remain the
interface embedded by Rust; building the desktop application still needs no
Node. Do not edit the generated files directly.

`locales:check` runs before web builds, type checks and tests, and in CI. It
rejects missing or extra translations, mismatched `{placeholders}`, duplicate
keys (including escaped JSON keys), misplaced domains, new unused keys, and
outdated generated files. No new translation library is required: the inventory
uses Rolldown's TypeScript parser, already part of Vite's toolchain.

Use `t()` and `entryText()` for static keys. Both accept the generated `Key`
union. `KeySuffix<'beatbox.lifecycle.'>` describes a closed family:

```ts
import { t, type KeySuffix } from './i18n';
const state: KeySuffix<'beatbox.lifecycle.'> = 'running';
t(`beatbox.lifecycle.${state}`);
```

Engine values and extensions can introduce names unknown to this version of
the interface. Use `isKey()` to narrow a runtime string, or `runtimeText()` for
an open-ended computed key. Unknown names retain their existing visible-key
behavior. Development falls back to English when the loaded locale lacks a
known key; production does not. Locale parity is required in both build modes.
Entry screens retain their existing English copy and now use typed keys too.

Run `npm --prefix web run locales:report` to review entries without statically
identified usage and the count of identical English/Portuguese values. The
inventory is conservative: a computed template or lookup prefix keeps its
whole matching family, and Rust/Python reason names are scanned as literals.
It cannot prove individual members of a runtime family unused. The existing
Rust tests still expand live engine tables and probe helper-generated reasons;
the remaining web tests check engine reasons and screen behavior.

`usage-exceptions.json` documents 24 existing entries retained for a lossless
migration. Four are expanded by the Rust network-mode test; the rest await
removal review. New unused keys fail validation unless given an explicit,
nonempty explanation there. Removed keys and exceptions that become statically
used must also be removed from that file. Identical translations are review
candidates, not failures: proper nouns and commands can legitimately match.

The original migration is reproducible with
`node web/scripts/locales.mjs --migrate` on a checkout containing only the flat
catalogues and `domains.json`. It validates both languages before writing and
refuses to overwrite existing domain directories. Plurals, `Intl` formatting,
vocabulary simplification, and Fluent are follow-up work.
