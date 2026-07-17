# Testing

The port's **definition of done = full Rust↔oracle parity on both targets** (native + wasm). Detail:
`docs/port/06-build-docs-licensing.md`; the test tree layout is in `tests/README.md`. Nothing is built
yet (gate closed) — this file fixes the strategy.

## Oracle strategy (the split)

- **Features wazmrt implements** → **golden-vector parity Rust↔wazmrt**: identical inputs must yield
  identical outputs. The proven technique — native `bench` result == wasm `bench` result == wazmrt
  result for the same seed. Diff decode-coverage snapshots, validation snapshots, and `.wast`
  pass/fail counts against the oracle.
- **Features wazmrt lacks** (SIMD, multi-memory, threads/atomics, tail calls; EH until stable) → no
  wazmrt oracle → conform against **wasmtime + the official WebAssembly spec testsuite** directly.
- Re-check the split at each wazmrt freeze (wazmrt gains features over time — EH core landed 2026-07-17).

## Test layers (mirror wazmrt, ported)

- **Unit tests** — port wazmrt's ~132 distinct tests module-by-module (decode/validate/interp/text/
  wasi/pin). Start with the LEB accept/reject vectors and the ValType-packing bit ops (pure, high-value).
- **C-ABI behavior** — `tests/c_smoke.c` equivalent: compile → instantiate (with a host import) → call
  → read memory → global → trap. Plus `tests/abi_symbols.c`, a **link-time completeness gate** for every
  `wasmrt.h` symbol.
- **C-ABI memory safety** — run the C entry points under **Miri**/ASAN + a randomized **lifecycle fuzz**
  (wazmrt #22). A normal allocator can't catch a double-free/UAF; the detecting allocator is the oracle.
  Less risk than wazmrt here because wasmrt's capi uses lightweight `{id}` handles, not a refcount object
  model — but the fuzz still guards instance/memory lifetimes.
- **Conformance gates:**
  - **spec testsuite** — `wasmrt <file.wast>` runs the official corpus (positive + `assert_invalid`/
    `assert_malformed`/`assert_trap`/`assert_unlinkable`). The corpus lives outside the repo (wazmrt keeps
    it on removable media at `wasmtk/tests/module/wasm_wast/testsuite-main`).
  - **wasi-gate** — compile real Zig/C/Rust `wasm32-wasi` guests (`examples/`) and run them through the
    wasmrt CLI asserting exact stdout. A regression fails the build. "A gate that can't fail is
    decoration" — verify it fails on a wrong expected string.

## Method discipline (carry from wazmrt)

- **Diff the OUTPUT (N passed / N failed), not exit codes** — a build that exits 0 while silently
  dropping passes is a regression.
- Benchmark against a **same-session baseline** (`git stash` A/B/A); never compare Mops/s across days
  (~8% run-to-run spread). See `bench/README.md`.
- Each feature slice adds unit tests + a hand-written `.wast` (or corpus gate) + verified zero
  regressions across all build surfaces (`cargo test` / native / wasm, + c-smoke where relevant).
- Track binary **size** per artifact as a first-class metric (criterion (c)).
