#!/usr/bin/env bash
#
# Build the `showcase` example — the all-widgets interactive app — with the
# platform backend, so it can run on a real VT or inside a QEMU Linux guest
# (see scripts/run-showcase-qemu.sh and fbui-platform/docs/qemu.md).
#
#   ./scripts/build-showcase.sh            # debug build
#   ./scripts/build-showcase.sh --release  # optimized build
#
# Prints the path to the built binary on stdout (so run-showcase-qemu.sh can
# capture it); all logging goes to stderr.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

log() { printf '\033[1;34m[build-showcase]\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m[build-showcase]\033[0m %s\n' "$*" >&2; exit 1; }

# Find cargo even under sudo (which often resets PATH/HOME) — mirrors qemu-test.sh.
find_cargo() {
  if command -v cargo >/dev/null 2>&1; then command -v cargo; return; fi
  for c in "$HOME/.cargo/bin/cargo" "${SUDO_USER:+/home/$SUDO_USER/.cargo/bin/cargo}" /root/.cargo/bin/cargo; do
    [ -x "$c" ] && { echo "$c"; return; }
  done
  die "cargo not found; install Rust (https://rustup.rs)"
}

PROFILE=debug
CARGO_ARGS=()
for arg in "$@"; do
  case "$arg" in
    --release) PROFILE=release; CARGO_ARGS+=(--release) ;;
    -h|--help|help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $arg (try --release)" ;;
  esac
done

cargo="$(find_cargo)"
log "building showcase ($PROFILE) with --features platform"
"$cargo" build -p fbui --example showcase --features platform "${CARGO_ARGS[@]}" >&2

bin="$ROOT/target/$PROFILE/examples/showcase"
[ -x "$bin" ] || die "build reported success but $bin is missing"
log "built: $bin"
echo "$bin"
