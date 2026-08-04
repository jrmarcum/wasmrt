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

## Spec-suite conformance — the T6 gate result (2026-08-03, v0.7.0)

First full run of `wasmrt wast <testsuite>` over the 284 vendored files:

| | count |
| --- | --- |
| **passed** | **54,509** |
| failed | 871 |
| skipped | 9,608 |
| **pass rate** | **98.4%** of 55,380 adjudicated assertions |

**Skips are never folded into passes.** A construct this build cannot put to the test is not a pass —
that is the runner's honesty rule (`wast.rs`), and it is why the number is trustworthy. The skip count is
dominated by modules needing **host imports**, which `Instance::new` still rejects; expect it to fall
sharply at T7.

Worst remaining files: `simd_const` 47, `binary` 43 (×2 copies), `type-subtyping` 36, `table_copy64` 33,
`i31` 30, `const` 26, `float_literals` 26. **Owner's call (2026-08-03): stop here and re-check after
T7** rather than grinding the assembler edges first.

The first run scored 96.7% and surfaced four bugs the hand vectors could not — a panic, an element-segment
encoding no decoder could read, truncated out-of-range constants, and mis-placed digit separators (all in
`roadmap.md`'s T6-gate entry, all pinned by regression tests). **That is the argument for the suite:** it
tests the assembler against the decoder, validator and interpreter at a scale hand vectors cannot reach.

## Current test state (2026-08-03, v0.7.0)

**218 `wasmrt-core` unit tests, all green** under native + (compile) `wasm32` no_std; clippy clean.

**The T6 layers added 95.** The **`.wast` runner** added 15 (including the one that pins the honesty
rule: an unknown mnemonic inside an `assert_invalid` must **skip**, not pass). The **assembler** added 55
across its layers, plus 4 regressions for the bugs the spec suite found. The **validator completion**
added 12 (SIMD alignment + missing-memory +
operand typing, exact atomic alignment vs. the scalar hint, GC field/mutability/packing, EH try_table
clause-vs-label, throw tag checks, legacy catch framing, `delegate` rejected). **`sexpr`** added 10
(comments, escapes, the lone-`;` hang regression, depth cap). **The opcode name table** added 3, incl. a
round-trip over every single-byte op — that property is what keeps the assembler's reverse map from
drifting off the decoder. **The assembler** added 28.

**The assembler's tests are its real gate: they assemble → decode → validate → instantiate → invoke.**
Byte-level assertions would only prove the assembler agrees with itself; running what it produced proves
it agrees with the decoder, the type-checker and the interpreter. Both bugs found in the assembler core
(folded memargs, `call_indirect`) surfaced as failed *executions*, not mismatched bytes. Coverage so far:
flat + folded forms, recursion (`fac 10`), a named-label loop (`sum 100`), `call_indirect` dispatch,
data + load, globals, explicit memargs, forward-referencing exports, multi-value and parameterised block
types, and float arithmetic — all from text source. Plus the hex-float rounding vectors the oracle
documents (a truncating parser is one ULP low — a *wrong value*, not a rejected one) and the subnormal
round-vs-flush edge cases.

The
EH slice added 11 — a `try_table` catch, an uncaught throw, `catch_all` binding nothing (a local records
which path ran, since a catch_all target label must be void), an exception unwinding **across a call**, a
`catch_ref` → `throw_ref` round-trip through the exnref box, legacy `try`/`catch`, legacy `catch_all`,
legacy `rethrow` propagating outward, a throw from inside a handler escaping its own try (the idiom that
loops forever without the `caught` guard), `delegate` trapping while unwinding, and no EH state leaking
between invocations. The
memory64 slice added 18 — 12 execution vectors (i64-address store/load round-trip, `memory.size`/`grow`
as i64, grow-past-max → −1, an i64 active data-segment offset, **an address of 2^32 trapping instead of
wrapping** — the case that distinguishes a real 64-bit address from a truncated one — a `memarg` offset
above `u32`, i64 bulk `fill`/`copy`, an i64 `memory.init` destination, i64 addresses through the `0xFE`
atomic and `v128` families, a huge declared minimum hitting the instance budget, and a mixed 64/32-bit
`memory.copy`) and 6 validator vectors (i32 address rejected / i64 accepted on a 64-bit memory,
`memory.size` yielding i64, a `memarg` offset above `u32` rejected on a 32-bit memory but legal on a
64-bit one, the data-segment offset's index type, limits above the 2^48 ceiling, and an `i64` table type
rejected as malformed). The
atomics slice added 5 — `i32.atomic.rmw.add`, `cmpxchg`, an unaligned-atomic trap, a `wait*`-on-non-shared
trap, and a `wait32`-on-shared mismatch (returns 1). The
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

## Where the spec testsuite lives (owner, 2026-08-03)

The official WebAssembly spec testsuite is **already vendored locally** — no clone needed:

```
D:\Programs\_ProgramExamples\Example_Programs\wasmExamples\wasmtk\tests\module\wasm_wast\testsuite-main\
```

**257 core `.wast` files** plus `proposals/` (custom-descriptors 14, threads 4, custom-page-sizes 4,
wide-arithmetic 1) and `legacy/` (4 — the legacy EH encoding: `throw`, `rethrow`, `try_catch`,
`try_delegate`). Mirrored on GitHub at <https://github.com/jrmarcum/wasmtk/tree/main/tests>. The wider
`wasmtk/tests` tree also holds the **WASI corpus** used at T7 and 534 `.wat` files.

This is the **T6 conformance gate**: the `.wast` runner walks these and must reach wazmrt's ~60k-assertion
pass profile. Note `legacy/try_delegate.wast` is expected to fail its `delegate` cases — wasmrt rejects
`delegate` deliberately, matching the frozen oracle (see `known-issues.md`); record it as a known
divergence rather than chasing it.

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
