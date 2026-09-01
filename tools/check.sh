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

# CI builds the engine on the oldest supported Rust as well as stable, and
# that job has failed on its own before now: a dependency moved to edition
# 2024, which older Cargo cannot parse, without anything here changing.
msrv="$(grep -m1 '^rust-version' Cargo.toml | cut -d'"' -f2)"
if rustup toolchain list 2>/dev/null | grep -q "^${msrv}"; then
    step "Build engine (${msrv})"  cargo "+${msrv}" build -p cheapazsla-core
    step "Test engine (${msrv})"   cargo "+${msrv}" test -p cheapazsla-core
    # Clippy's lints differ between releases in both directions: the older one
    # here has caught things the newer one no longer mentions.
    step "Clippy engine (${msrv})" cargo "+${msrv}" clippy -p cheapazsla-core -- -D warnings
else
    printf '\n=== Oldest supported Rust (%s)\n    skipped: rustup toolchain install %s\n' "$msrv" "$msrv"
fi

step "Build engine"        cargo build -p cheapazsla-core
step "Test engine"         cargo test -p cheapazsla-core
# Clippy caches its findings, so a second run over unchanged sources reports
# nothing and looks like a pass. Touching the crate roots forces it to think
# again — a run that reported clean this way had three real lints waiting.
touch crates/*/src/lib.rs crates/*/src/main.rs 2>/dev/null || true

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
