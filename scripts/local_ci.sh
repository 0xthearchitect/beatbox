#!/usr/bin/env bash
# The CI (.github/workflows/rust.yml), run on this machine: every job's
# steps, in order, each result kept, and a summary at the end. It is what a
# pull request has to pass before it is opened (AGENTS.md), and what stands
# in for GitHub Actions when Actions will not run.
#
#   scripts/local_ci.sh            run everything on the checkout as it is
#   scripts/local_ci.sh --pr 76    then, if everything passed, say so on pull
#                                  request 76: a "local-ci" status on its
#                                  head commit and a comment with the summary
#
# What it cannot do: run the tests on Windows. The windows-latest job is
# stood in for by compiling and linting the workspace for the Windows target
# (x86_64-pc-windows-gnu) when it is installed; CI itself builds with MSVC.
#
# With --pr, the checkout has to be clean and its HEAD the pull request's
# head, so what was checked is exactly what was pushed.
set -uo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"
pr=
[ "${1:-}" = --pr ] && pr=${2:?--pr takes a pull request number}

head=$(git rev-parse HEAD)
if [ -n "$pr" ]; then
  [ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "local_ci: uncommitted changes; commit them first" >&2; exit 2; }
  pushed=$(gh pr view "$pr" --json headRefOid -q .headRefOid) || exit 2
  [ "$pushed" = "$head" ] || { echo "local_ci: HEAD is ${head:0:7}, pull request $pr is at ${pushed:0:7}; push or check out first" >&2; exit 2; }
fi

export CARGO_TERM_COLOR=never RUSTFLAGS="-D warnings"
results=()
step() {
  local name=$1; shift
  echo "::: $name"
  if "$@"; then results+=("pass  $name"); else results+=("FAIL  $name"); fi
}

# build (ubuntu-latest)
step "build: cargo fmt --all --check" cargo fmt --all --check
step "build: cargo clippy --workspace --all-targets --locked" cargo clippy --workspace --all-targets --locked
step "build: cargo test --workspace --locked" cargo test --workspace --locked
# build (windows-latest), as far as this machine can
if rustup target list --installed 2>/dev/null | grep -qx x86_64-pc-windows-gnu; then
  step "build, Windows target (compile and lints only): cargo clippy --workspace --all-targets --locked --target x86_64-pc-windows-gnu" \
    cargo clippy --workspace --all-targets --locked --target x86_64-pc-windows-gnu
else
  results+=("skip  build, Windows target: rustup target add x86_64-pc-windows-gnu to check it")
fi
# dependencies
step "dependencies: cargo deny check" cargo deny check
# web
[ -d web/node_modules ] || (cd web && npm ci)
step "web: npm run locales:check" bash -c 'cd web && npm run -s locales:check'
step "web: npm run build" bash -c 'cd web && npm run -s build'
step "web: git diff --exit-code -- dist" git diff --exit-code --stat -- web/dist
step "web: npm test" bash -c 'cd web && npm test'
# shell
step "shell: cargo fmt --check" bash -c 'cd shell && cargo fmt --check'
step "shell: cargo clippy --all-targets --features proof --locked" bash -c 'cd shell && cargo clippy --all-targets --features proof --locked'
step "shell: cargo test --locked" bash -c 'cd shell && cargo test --locked'
step "shell: cargo deny check" bash -c 'cd shell && cargo deny check'

summary=$(printf '%s\n' "${results[@]}")
echo "::: summary for ${head:0:7}"
echo "$summary"
if grep -q '^FAIL' <<<"$summary"; then
  [ -n "$pr" ] && echo "local_ci: not published to pull request $pr: something failed" >&2
  exit 1
fi

if [ -n "$pr" ]; then
  slug=$(gh repo view --json nameWithOwner -q .nameWithOwner) || exit 2
  gh api -X POST "repos/$slug/statuses/$head" -f state=success -f context=local-ci \
    -f description="scripts/local_ci.sh passed on $(uname -sm)" >/dev/null || exit 2
  gh pr comment "$pr" --body "$(printf '**local CI passed** on `%s` (`scripts/local_ci.sh`, %s)\n\n```\n%s\n```\n' "${head:0:7}" "$(uname -sm)" "$summary")" >/dev/null || exit 2
  echo "local_ci: published to pull request $pr"
fi
