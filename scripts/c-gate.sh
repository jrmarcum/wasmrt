#!/usr/bin/env bash
# c-gate.sh — build the wasmrt static library and run the two C-ABI gates.
#
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Jon Marcum
#
#   tests/abi_symbols.c  — link completeness: every function `wasmrt.h` DECLARES must be
#                          EXPORTED. Taking a symbol's address forces the linker to resolve
#                          it, so a header/library mismatch fails the build.
#   tests/c_smoke.c      — behaviour: compile -> link a host import -> instantiate -> call
#                          -> memory -> global -> trap, driven from real C.
#
# Both compile against the SHIPPED header, so they also prove `wasmrt.h` is valid C — which
# no Rust test can.
#
# Usage: scripts/c-gate.sh [--release]
set -euo pipefail

cd "$(dirname "$0")/.."

PROFILE_DIR=debug
CARGO_FLAGS=()
if [[ "${1:-}" == "--release" ]]; then
    PROFILE_DIR=release
    CARGO_FLAGS+=(--release)
fi

CC=${CC:-cc}
INCLUDE=crates/wasmrt-capi/include
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

echo "== building the static library =="
cargo build -p wasmrt-capi "${CARGO_FLAGS[@]}"

LIB=""
for candidate in "target/$PROFILE_DIR/libwasmrt_capi.a" "target/$PROFILE_DIR/wasmrt_capi.lib"; do
    if [[ -f "$candidate" ]]; then LIB=$candidate; break; fi
done
if [[ -z "$LIB" ]]; then
    echo "no static library found under target/$PROFILE_DIR" >&2
    exit 1
fi
echo "   using $LIB"

# A Rust staticlib does not carry its own system dependencies; the host libc and friends
# must be named explicitly at link time.
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) SYSLIBS=(-lws2_32 -lbcrypt -luserenv -lntdll -lole32 -loleaut32 -ladvapi32) ;;
    Darwin)               SYSLIBS=(-framework CoreFoundation -framework Security) ;;
    *)                    SYSLIBS=(-lpthread -ldl -lm) ;;
esac

fail=0
for gate in abi_symbols c_smoke; do
    echo "== $gate =="
    if ! "$CC" -std=c11 -Wall -Wextra -Werror -I"$INCLUDE" \
            "tests/$gate.c" "$LIB" "${SYSLIBS[@]}" -o "$OUT/$gate"; then
        echo "   BUILD FAILED" >&2
        fail=1
        continue
    fi
    if ! "$OUT/$gate"; then
        echo "   RUN FAILED" >&2
        fail=1
    fi
done

if [[ $fail -ne 0 ]]; then
    echo "C-ABI gate: FAILED" >&2
    exit 1
fi
echo "C-ABI gate: PASSED"
