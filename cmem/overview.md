# Overview

`wasmrt` is an **idiomatic-Rust WebAssembly runtime**, a port of the Zig runtime **`wazmrt`** (sibling
repo `../wazmrt`). Same north star as wazmrt — **blazingly fast** on cold-start + boundary, **smallest
possible binary**, and **itself compilable to `wasm32`** so it can be embedded inside another wasm host
— but written to **replace wasmtime** as the native engine beneath the owner's `universalWasmLoader-*`
projects (see [loaders.md](loaders.md)).

## The oracle

The **passing `wazmrt` Zig build is the reference oracle**, now **frozen** at `wazmrt@dadc727`
(2026-07-27; `scripts/wazmrt-baseline.txt`). wasmrt reproduces its behavior/outputs (byte-for-byte at
the boundary) for every feature wazmrt implements — which, at the freeze, is **every wasm proposal
wasmrt targets except the tail-call proposal** (`return_call`/`return_call_indirect`). SIMD,
multi-memory, threads/atomics, memory64, exception handling, and full WasmGC are all in the oracle now.
Only tail calls have no wazmrt oracle → conform those against **wasmtime + the official spec testsuite**
(see [testing.md](testing.md), [design-decisions.md](design-decisions.md)). Full deep-read of wazmrt is
in `docs/port/00-synthesis.md` (+ 6 subsystem maps).

## Status (2026-08-05) — PORT phase; **assemble → decode → validate → run → WASI all working**

The oracle is frozen (`wazmrt@dadc727`, `zig build test` 489/493 green) and the port is underway,
released stage-by-stage to crates.io (see [releasing.md](releasing.md)). Scope at the freeze:
**memory64 is in**; the oracle covers everything wasmrt targets **except tail calls**.

**Done — T0–T7 (v0.1.0 → v0.8.0):**
- **T0 (v0.1.0)** — 3-crate workspace (`wasmrt-core` / `wasmrt-capi` / `wasmrt`) builds on all four
  surfaces (native CLI, staticlib, cdylib, freestanding `wasm32`).
- **T1 (v0.2.0)** — `types` (ValType u32 newtype + RefHeap/subtyping, SectionId, DecodeError) + `reader`
  (zero-copy cursor, spec-correct LEB128).
- **T2 (v0.3.0)** — `opcode` (the complete shared `Op`/`Imm`/`Instr` table + `decode_body`, all four
  prefix families).
- **T3 (v0.4.0)** — `module` (full binary decode of every core section; owned data model). **`wasmrt
  <file.wasm>` decodes + summarizes.**
- **T4 core (v0.5.0)** — `validate` (spec §3 type-checker: value/control stacks, module-level checks,
  const-expr, C.refs). Core language only at the time — SIMD/atomics/GC-objects/EH typing was deferred
  and **landed later, in v0.7.0** (see below); until then those ops rejected loudly rather than
  silent-accepting. CLI prints a validation verdict.
- **T5 slices 1–10 (v0.6.0 integer, v0.6.1 float, v0.6.2 linear memory, v0.6.3 tables + reference types,
  v0.6.4 WasmGC, v0.6.5 SIMD, v0.6.6 multi-memory, v0.6.7 threads/atomics, v0.6.8 memory64, v0.6.9
  exception handling)** — `interp`
  (switch interpreter over the IR). **`wasmrt run <file> <fn> [args]` runs compute + linear memory
  (multi-memory + memory64) + indirect calls + GC + SIMD + atomics + EH** (control flow, recursion, locals,
  globals, all i32/i64/f32/f64 ops incl. NaN-correct min/max + ties-to-even nearest + trap/sat conversions;
  loads/stores + `memory.size`/`grow` + bulk memory + active data init; `call_indirect` + full `table.*` +
  `ref.*` + element-segment init; **GC struct/array heap + `i31` + `ref.test`/`ref.cast`/`br_on_cast`**;
  **the full `v128` SIMD set incl. relaxed**; `NULL_REF = u64::MAX` checked before `I31_TAG = 1<<63` —
  slot-encoding invariant anchored). **The value slot is now 128-bit (`Value = u128`)** so a `v128` is one
  slot (idiomatic divergence from wazmrt's 2-`u64`-slots; scalars/refs in the low 64). **A memory carries
  its own index type** — every address/count/page-count is `i64` on a 64-bit memory, `i32` otherwise;
  **tables stay 32-bit** (the oracle rejects an `i64` table type as malformed). **EH runs in both
  encodings** — `try_table` (all four clause kinds) + `throw`/`throw_ref`, and legacy `try`/`catch`/
  `catch_all`/`rethrow` — unwinding across call frames; **`delegate` is rejected, matching the oracle**.

**T4 and T6 are complete too (v0.7.0): the validator covers every construct the interpreter runs, and
the text toolchain assembles `.wat`, runs `.wast`, and scored 98.4% on the official spec testsuite
(54,509 assertions).**

- **T7 (v0.8.0)** — **host imports, module linking, and WASI preview 1 including the sandboxed
  filesystem.** `Instance::new_with_imports` links host backings in declaration order (`HostFunc` is a
  **boxed closure**, not a fn-pointer + `void*` ctx — that shape needs `unsafe`, so this is the safety
  directive's first real application, and T8's C ABI will need the same treatment). Linking runs on a
  **shared store** (wasmtime-style): resources are owned once by the `Store`, instances hold `IndexMaps`
  into it, and the borrow splits cleanly with no `Rc`, no `RefCell`, no `unsafe`. **`wasmrt wasi <file>`
  runs a preview-1 program**; a guest reaches only what `--dir` / `--ro-dir` preopens, and with no
  `--dir` every path call is `BADF` — there is no implicit cwd. **The sandbox is the resolver**: `..`
  cannot rise above the preopen, absolute symlink targets re-base to the preopen root, and a canary
  outside the preopen is a mutation-verified test. v0.8.0 also made the **safety directive mechanical**
  (`#![forbid(unsafe_code)]` in core + the CLI) and closed the **literal/text edges**, which took the
  suite to **98.8%** (61,013 passing) with **all 284 files parsing for the first time**.

**Next: T8 — the `wasmrt.h` C ABI.** Its decision-gate is finalizing the header shape with the owner
before any code is written; the draft lives in `docs/port/`.
See the task list in [roadmap.md](roadmap.md). **281 workspace tests** green; clippy clean; native +
`wasm32` no_std all build.

## Planned repo / crate layout

- **`wasmrt-core`** — the runtime library, `#![no_std]`-friendly and `wasm32`-freestanding-clean (no
  libc). Modules mirror wazmrt's `src/` for oracle-diffing: `types` (ValType `u32` newtype), `reader`,
  `module` (decode), `opcode` (the shared IR table), `validate`, `interp`, `sexpr`, `wat`, `wast`,
  `wasi`, `pin`, `lib`.
- **`wasmrt-capi`** — `crate-type = ["staticlib", "cdylib"]`; the public **`wasmrt.h`** C ABI
  (`#[no_mangle] extern "C"`) over core. Ships `wasmrt.h`.
- **`wasmrt`** — the CLI binary.
- **Dual-target contract** (from wazmrt): native CLI + static lib + cdylib + a freestanding-`wasm32`
  build of the runtime, plus the gates (a c-smoke equivalent under Miri, a wasi-gate compiling real
  guests, a cold-vs-steady bench). See [architecture.md](architecture.md).

## Mental model

- **Pipeline:** decode → validate → instantiate → execute (a **switch interpreter over a pre-decoded
  IR** — Option A, deliberately not a JIT), with a WAT/WAST text toolchain and WASI preview 1.
- **Boundary-faithful, idiomatic internals.** The observable behavior + the `wasmrt.h` contract are
  fixed; the Rust internals use ownership/enums/`Result`, not a Zig transliteration.
- **Own `wasmrt.h`.** The public C API is wasmrt's own lean surface (not the wasm-c-api, not wasmtime's
  symbols); the loaders do the Component-Model Canonical ABI marshalling on top. See [loaders.md](loaders.md).
