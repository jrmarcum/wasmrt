#!/usr/bin/env bash
# miri-gate.sh — run the C-ABI surface under Miri.
#
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Jon Marcum
#
# WHY. `wasmrt-capi` is the one crate that cannot be `#![forbid(unsafe_code)]`, because a C
# ABI is an unsafe boundary. A normal allocator will happily hand back freed memory that
# still looks right, so an ordinary test run cannot tell a use-after-free from a pass. Miri
# interprets the program and reports it as an error.
#
# What it covers: every `wasmrt-capi` test, including `lifecycle_fuzz`, which drives objects
# through randomized creation, use and destruction orders — including the orders the header
# tells embedders not to use — and touches handles whose store is already gone.
#
# The Miri run is DELIBERATELY not part of `cargo test`: it is ~100x slower, and the fuzz
# scales itself down under `cfg!(miri)` so this finishes in seconds rather than minutes.
#
# Requires: rustup component add miri
set -euo pipefail

cd "$(dirname "$0")/.."

if ! cargo miri --version >/dev/null 2>&1; then
    echo "miri is not installed; run: rustup component add miri" >&2
    exit 1
fi

# `-Zmiri-disable-isolation` lets the WASI-touching paths read the real clock/entropy if a
# test reaches them; without it Miri blocks those syscalls and the run dies for a reason
# that has nothing to do with memory safety.
export MIRIFLAGS="${MIRIFLAGS:--Zmiri-disable-isolation}"

echo "== miri: wasmrt-capi =="
cargo miri test -p wasmrt-capi

echo "Miri gate: PASSED"
