# Contributing

Thanks for wanting to help. BeatBox is young, and help is welcome.

## Where to start

Issues labelled
[good first issue](https://github.com/0xthearchitect/beatbox/labels/good%20first%20issue)
are small and say where to look. Four kinds of help need little or no
knowledge of the code:

- **Try it on your computer.** Install it on a Windows PC or a Linux
  system, and tell us in an issue what happened, good or bad. Windows'
  hardware acceleration can only be tried on a real PC, not in a virtual
  machine.
- **A new kind of service.** Each one is a JSON file in
  `crates/server/definitions/`; `postgresql.json` is a good one to start
  from.
- **A new network template.** Each one is a JSON file in `web/src/labs/`,
  like `dmz.json`.
- **A new language.** BeatBox first needs room for more than two
  ([#13](https://github.com/0xthearchitect/beatbox/issues/13)). Then a
  language is a folder beside `locales/en/`.
  It can be partly done and grow, and BeatBox offers it once every text is
  translated.

For anything bigger, open an issue first and say what you'd like to do, so
we agree on it before you spend your time.

## Sending a pull request

1. Fork the repository and branch from `main`.
2. Build it as [the README](README.md#build-it-yourself) says, and try your
   change in the app.
3. If you can, run `scripts/local_ci.sh` (it needs
   `cargo install cargo-deny`). It runs the same checks as GitHub, on your
   own computer (Linux). GitHub runs them on your pull request anyway.
4. Open the pull request against `main`. Say what it changes and how you
   tried it.

What the checks will ask of you:

- Text on screen goes in `locales/en/` and `locales/pt_PT/` together. If
  you don't write Portuguese, put the English text in both and say so; we
  will translate it.
- A change under `web/src` or `locales/` needs `web/dist` rebuilt
  (`npm --prefix web run build`) and committed.
- Every public Rust item has a doc comment.

BeatBox is for people who may know little about computers. Labels are short
and plain, and explanations go behind an ⓘ, not on the screen.

Use whatever tools you like. Every change is read closely all the same, and
anything that doesn't make sense won't go in, however it was made.

## What happens next

We read every pull request and answer within a few days. Once it is merged,
it goes out with the next release, and the changelog thanks you by name.

## Licence

BeatBox is under MIT OR Apache-2.0. By sending a pull request you agree that
your change is under the same terms.

## Questions

Questions, and labs you built, go to
[Discussions](https://github.com/0xthearchitect/beatbox/discussions).
