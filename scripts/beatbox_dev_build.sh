#!/usr/bin/env bash
# After a change anywhere in BeatBox, in one call: the interface into
# web/dist/ (the bundle the window embeds, committed), then beatboxd, then
# the window itself, so it embeds what web/dist/ now has.
#
#   scripts/beatbox_dev_build.sh           release/, what a plain launch of
#                                           shell/target/release/beatbox runs
#   scripts/beatbox_dev_build.sh --debug   debug/, for scripts/beatbox_shell_walk.sh
#
# Release by default and not debug: a stale window binary looks exactly like
# a rebuild that did nothing, and whoever runs BeatBox by hand from
# shell/target/ almost always runs the release one. Either way the running
# window has to be quit and reopened -- Tauri does not reload a window that
# is already up.
#
# beatboxd is copied beside the window, as a package puts it. A release
# window looks there; a debug window deliberately prefers target/debug so
# a stale copied sidecar cannot mask the daemon a developer just compiled
# (shell/src/link.rs). A daemon already running outlives the window and
# keeps its old build, so this says when one is.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"

profile=release
flag=--release
[ "${1:-}" = --debug ] && { profile=debug; flag=; }

(cd web && npm run build)
cargo build $flag -p beatboxd
cargo build $flag --manifest-path shell/Cargo.toml
# By rename: a daemon running the old copy holds it open, and cp over it is
# "Text file busy".
cp "target/$profile/beatboxd" "shell/target/$profile/.beatboxd.new"
mv -f "shell/target/$profile/.beatboxd.new" "shell/target/$profile/beatboxd"

echo "beatbox_dev_build: shell/target/$profile/beatbox, with its beatboxd beside it -- quit and reopen the window to see it"
if pgrep -x beatboxd >/dev/null; then
    echo "beatbox_dev_build: a beatboxd is still running the build from before; it must go too (pkill -x beatboxd -- machines keep running)"
fi
