# tests/

Test suite for wasmrt — the Rust ports/equivalents of wazmrt's `tests/` plus the
port's parity gates.

**Status: the C-ABI gates are LIVE as of T8 / v0.9.0 (2026-08-06).** Definition of
done = **full Rust↔oracle parity on both targets** (see `../cmem/testing.md`).

## C-ABI gates — built, and each catches what the others cannot

Run both with **`../scripts/c-gate.sh`** (add `--release` for the release lib).

- **`abi_symbols.c` ✅ 74/74 symbols resolve.** Takes the address of every function
  `wasmrt.h` declares, forcing the linker to resolve it. A function DECLARED in the
  header but never EXPORTED fails the **build** — the one failure mode nothing else
  catches, because compiling a declaration always succeeds. **Keep it in step with
  the header: a new declaration means a new line here.**
- **`c_smoke.c` ✅ PASSED.** Compiled by a C compiler against the *shipped* header,
  so it proves two things no Rust test can — that `wasmrt.h` is valid C, and that
  its declarations match the exported symbols. Drives config → compile → link a
  host import → instantiate → call → memory both ways → global → trap → teardown,
  and checks that a foreign-store handle is refused. Its module bytes are embedded
  rather than loaded, so the gate has no fixture dependency and fails for exactly
  one reason: the ABI.
- **Miri ✅ 26/26** via **`../scripts/miri-gate.sh`**, including `lifecycle_fuzz`
  (in `crates/wasmrt-capi/src/tests.rs`), which drives randomized creation, use and
  destruction orders — including the ones the header discourages — and touches
  handles whose store is already gone. A normal allocator hands back freed memory
  that still looks right; Miri calls it an error.

**These gates can fail** — the standard this file sets below. Mutation-verified:
deleting the store-tag check from handle unpacking makes the cross-store test fail.

### Rust unit + parity (in-crate `#[cfg(test)]` + here)
- **Unit tests** mirroring wazmrt's ~132 distinct tests (decode/validate/interp/
  text/wasi/pin), ported module-by-module.
- **Golden-vector parity** — identical inputs → identical outputs vs. the wazmrt
  oracle (native `bench` == wasm `bench` == wazmrt for the same seed). For
  features wazmrt lacks (SIMD, multi-memory, threads, tail calls), oracle against
  **wasmtime + the official spec testsuite** instead.
- **C-ABI memory safety** — run the C entry points under **Miri**/ASAN + a
  randomized **lifecycle fuzz** (wazmrt #22). A normal allocator can't catch a
  double-free/UAF; Miri/ASAN is the real oracle.

### Conformance gates (build-graph)
- **spec testsuite** — `wasmrt <file.wast>` runs the official WebAssembly testsuite
  (positive + `assert_invalid`/`assert_malformed`/`assert_trap`/`assert_unlinkable`),
  diffing `N passed / N failed` against a baseline.
- **wasi-gate** — compile real Zig/C/Rust `wasm32-wasi` guests (from `../examples/`)
  and run them through the wasmrt CLI asserting exact stdout. A gate that can't
  fail is decoration — verify it fails on a wrong expected string.

Reference originals: `../../wazmrt/tests/` (c_smoke.c, c_abi_symbols.c) and the
external corpora noted in `../cmem/testing.md`.
