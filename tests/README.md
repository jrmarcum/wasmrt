# tests/

Test suite for wasmrt — the Rust ports/equivalents of wazmrt's `tests/` plus the
port's parity gates.

**Status: PREP placeholder.** No test code yet — the port gate is closed
(see `../cmem/roadmap.md`), and the C-ABI tests depend on the finalized
`wasmrt.h` (held for review). This README fixes the intended set so testing
matches the oracle. Definition of done = **full Rust↔oracle parity on both
targets** (see `../cmem/testing.md`).

## Planned tests

### C-ABI (equivalents of wazmrt `tests/c_smoke.c` + `c_abi_symbols.c`)
- **`c_smoke.c`** — links the wasmrt **cdylib**/static lib and exercises `wasmrt.h`
  behavior from C: compile → instantiate (with a host import) → call an export →
  read linear memory → read a global → trap + message. The behavioral gate.
- **`abi_symbols.c`** — takes the address of every function `wasmrt.h` declares:
  a **link-time completeness gate** (a missing symbol fails the build). Regenerate
  from the finalized header.
- Both depend on the finalized `wasmrt.h`.

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
