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

## Status (2026-07-27)

**PORT phase — gate OPEN.** The oracle is frozen (`wazmrt@dadc727`, `zig build test` 489/493 green) and
the conversion has begun. Prep is complete: scope reconciled, full deep-read of wazmrt, the
`universalWasmLoader` survey, the `wasmrt.h` v0 draft, and the (now drift-watching) oracle monitor.
Scope was refreshed at the freeze: **memory64 is in** (owner, 2026-07-27); the oracle covers everything
wasmrt targets **except tail calls**. **T0 (the 3-crate workspace scaffold) is DONE** — `wasmrt-core` /
`wasmrt-capi` / `wasmrt-cli` build on all four surfaces (native CLI, staticlib, cdylib, freestanding
`wasm32`). Next: **T1 (types + reader)**, bottom-up and parity-gated. See the task list in
[roadmap.md](roadmap.md).

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
