# Testing

The port's **definition of done = full Rust↔oracle parity on both targets** (native + wasm). Detail:
`docs/port/06-build-docs-licensing.md`; the test tree layout is in `tests/README.md`. The gate is
**open** and the oracle is **frozen** at `wazmrt@dadc727` (2026-07-27) — this file fixes the strategy.

## Oracle strategy (the split) — re-checked at the 2026-07-27 freeze

- **Features wazmrt implements** → **golden-vector parity Rust↔wazmrt**: identical inputs must yield
  identical outputs. The proven technique — native `bench` result == wasm `bench` result == wazmrt
  result for the same seed. Diff decode-coverage snapshots, validation snapshots, and `.wast`
  pass/fail counts against the oracle. **At the freeze this covers nearly the whole scope** — incl.
  SIMD, multi-memory, threads/atomics, memory64, and exception handling (both encodings), which the
  frozen oracle now implements (they were *not* in the oracle before the freeze).
- **The only feature wazmrt lacks** is the **tail-call proposal** (`return_call`/`return_call_indirect`;
  wazmrt has `return_call_ref` but not base tail calls) → no wazmrt oracle → conform against **wasmtime
  + the official WebAssembly spec testsuite** directly.
- **Re-check only on oracle drift.** The split was re-checked at the freeze and collapsed to the one
  item above; it changes again only if `scripts/check-wazmrt.sh` reports the frozen oracle moved.

## Current test state (2026-07-31, through T5 slice 7 / v0.6.6)

**89 `wasmrt-core` unit tests, all green** under native + (compile) `wasm32` no_std; clippy clean. The
SIMD slice added 10 hand-built vectors — splat+extract, `v128.const`+extract_lane_u, `i32x4.add`,
`i32x4.eq`+bitmask, `i32x4.shl`, `f32x4.add`, `i8x16.add_sat_s` (saturation), `v128.load`/`store`
round-trip, a `v128` struct field, and a `v128` global — representative of each op category (full
per-opcode SIMD conformance comes from the spec suite at T6). The multi-memory slice added 3 — distinct
mem0/mem1 routing, an active data segment (flag `0x02`) initializing memory 1, and a cross-memory
`memory.copy`. The
per-slice method has held: each task ports **wazmrt's own test vectors 1:1** where they're hand-buildable
(T1 LEB accept/reject + ValType bit ops; T2 `decode_body` vectors incl. prefix/rejection; T3 the 15
module decode/malformed vectors; T4 9 validator vectors; T5 add/factorial/loop-sum + traps + the delicate
float helpers directly — nearest ties-even, NaN min/max, trunc/floor/ceil, trap/sat conversions — and
the memory ops — store/load round-trip, `memory.size`/`grow`, active data segment, OOB trap; the
table/ref ops — `call_indirect` dispatch + OOB trap, `ref.null`/`is_null`, `table.set`/`get` via
`ref.func`; and the **GC ops** — struct `new_default`/set/get, array `new`/set/get/len, `i31.get_s`/`get_u`,
`ref.test`), plus a few extras and end-to-end CLI runs (`add.wasm`, `fac.wasm`, `fadd.wasm`, `rt.wasm`). The
oracle's `wasm_mod` corpus lives on removable media, so the ported oracle unit tests stand in for it.

**Two constraints shape T4/T5 coverage** (recorded so future slices plan for them): (1) most of the
validator's tests and the interpreter's spec vectors need the **WAT assembler**, which doesn't exist
until **T6** — so exotic validate/exec arms can't be verified until then. (2) That's exactly why T4/T5
were **sliced** (core-first, exotic-later) with deferred ops rejecting loudly. The `.wast`/spec-testsuite
conformance gates come online at **T6** (text toolchain) and are where SIMD/GC/atomics/EH validation +
execution finally get real coverage; the C-ABI/Miri gates at **T8**.

## Test layers (mirror wazmrt, ported)

- **Unit tests** — port wazmrt's test corpus module-by-module (decode/validate/interp/text/wasi/pin).
  At the freeze wazmrt prints **489/493 pass (4 skip)** — ~216 distinct tests, roughly doubled because
  the `cabi` target re-runs the core tests (see wazmrt `testing.md` "Reading the test count"). Start with
  the LEB accept/reject vectors and the ValType-packing bit ops (pure, high-value).
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
