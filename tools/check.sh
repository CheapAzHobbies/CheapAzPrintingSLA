#!/usr/bin/env bash
# Run exactly what CI runs, before pushing.
#
# CI builds with RUSTFLAGS=-D warnings; a plain `cargo test` does not, so a
# warning that CI treats as a fatal error passes locally and fails on push.
# That went unnoticed for eight commits, each of which mailed a failure notice.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
export RUSTFLAGS="-D warnings"
export CARGO_TERM_COLOR=always

failed=0
step() {
    local name="$1"
    shift
    printf '\n=== %s\n' "$name"
    if "$@"; then
        printf '    ok\n'
    else
        printf '    FAILED: %s\n' "$name"
        failed=1
    fi
}

step "Build engine"        cargo build -p cheapazsla-core
step "Test engine"         cargo test -p cheapazsla-core
step "Clippy engine"       cargo clippy -p cheapazsla-core -- -D warnings
step "Formatting"          cargo fmt --all -- --check
step "Build everything"    cargo build --workspace
step "Test the interface"  cargo test --workspace --bins
step "Clippy everything"   cargo clippy --workspace -- -D warnings

# The engine must not pull in a GUI toolkit; that separation is the main
# architectural promise here, so it is checked rather than assumed.
printf '\n=== Engine stays free of the interface\n'
if cargo tree -p cheapazsla-core --prefix none 2>/dev/null |
    grep -Eq '^(gtk4|libadwaita|gdk4|glib) '; then
    printf '    FAILED: a GUI crate reached the engine\n'
    failed=1
else
    printf '    ok\n'
fi

if [ "$failed" -eq 0 ]; then
    printf '\nAll checks passed.\n'
else
    printf '\nSomething failed. Do not push yet.\n'
fi
exit "$failed"
