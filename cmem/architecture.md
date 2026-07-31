# Architecture

The Rust architecture for `wasmrt`. It reproduces wazmrt's **decode → validate → instantiate → execute**
pipeline and its **dual-target contract**, in idiomatic Rust. Detail: `docs/port/` (esp.
`00-synthesis.md`, `02-decode-core.md`, `03-validate-interp.md`, `06-build-docs-licensing.md`).

**Realized so far (through T5 slice 5 / v0.6.4):** the workspace + all three crates exist; `wasmrt-core`
has `types`, `reader`, `opcode`, `module` (decode), `validate` (core-language type-checker), and `interp`
(switch interpreter — integer + float compute + linear memory + tables/`call_indirect`/reference types +
**WasmGC** structs/arrays/`i31`/casts over a `Store`-owned GC heap) implemented and parity-tested; `text` /
`wasi` / `pin` are still stubs, filled in bottom-up per `roadmap.md`. The CLI does summarize + validate +
`run`. Two idiomatic divergences worth noting: owned `Vec`/`String` data (frees on drop, no
arena/`deinit`), and the interpreter's immutable-`module`/`func_bodies` vs `&mut Store` (globals +
memories + tables + dropped-data + **`gc_heap`**) borrow split (recursive `call` reborrows cleanly, no
`RefCell`).

_(Earlier snapshot, superseded:)_ Through T3 / v0.4.0: the workspace + all three crates exist; `wasmrt-core` has
`types`, `reader`, `opcode` (the shared IR + `decode_body`), and `module` (full binary decode)
implemented and parity-tested. `validate` / `interp` / text / `wasi` / `pin` are still stubs, filled in
bottom-up per `roadmap.md`. One idiomatic divergence worth noting: the decoder uses **owned `Vec`/`String`
data** rather than wazmrt's arena (frees on drop; no `deinit`).

## Crates

- **`wasmrt-core`** — the runtime library. **`#![no_std]`-friendly** and **`wasm32`-freestanding-clean**
  (no libc), so the same code targets native *and* embed-inside-wasm. Modules mirror wazmrt `src/`:
  - `types` — `ValType` as a **`u32` newtype** with concrete refs bit-packed; `RefHeap`/subtyping.
  - `reader` — zero-copy cursor + spec-correct LEB128.
  - `module` — decode (two-pass type section for rec-group forward refs); owned data model.
  - `opcode` — the **single shared opcode table + IR** feeding validate, interp, AND the assembler in
    reverse. Define once; never drift.
  - `validate` — the spec §3 Appendix type-checker; `subtypeOf`; const-expr checks.
  - `interp` — `Instance` (retains its `Module`) + the switch interpreter over untyped `u64` slots;
    GC heap; host imports; trap backtrace.
  - `sexpr` / `wat` / `wast` — the text toolchain (assembler + spec-testsuite runner).
  - `wasi` — WASI preview 1 + the secure sandbox (`walkFull` handle-stack resolver).
  - `pin` — module pin verification (SHA-256 allow-list; hash the bytes you run).
- **`wasmrt-capi`** — `crate-type = ["staticlib", "cdylib"]`. The public **`wasmrt.h`** surface as
  `#[no_mangle] pub extern "C"` functions over core. Uses **lightweight `{id}` handles** into a store
  (no wasm-c-api-style refcounted object model) and **caller-based host callbacks**. Ships `wasmrt.h`.
  See [loaders.md](loaders.md).
- **`wasmrt`** — the CLI binary (run a module / export / `_start`; assemble `.wat`; run `.wast`; `pin`).

## Dual-target contract (reproduce from wazmrt)

One core source produces: **native CLI**, **static lib**, **cdylib** (`.so`/`.dylib`/`.dll`), and a
**freestanding `wasm32`** build of the runtime itself. Cross-platform native builds go through
`cargo-zigbuild` until `rust-lld` is the native default beyond `x86_64-unknown-linux-gnu`; `wasm32`
linking needs only the bundled `rust-lld`. **Pointer-width rule:** type ABI pointer params as real
pointers / `#[repr(C)]` structs (`*const u32`, `*mut c_void`), never hardcoded `i32` — lowers to `i32`
on `wasm32` and true 64-bit on native, C-ABI-correct on both.

**Windows build host (decided at T0, 2026-07-27):** use the **`x86_64-pc-windows-gnullvm`** host
(LLVM-MinGW toolchain + UCRT) — it links with `lld`/`compiler-rt`/`libunwind` and needs no MSVC and no
classic `libgcc`, which fits the libc-free / no-MSVC ethos carried from wazmrt. The plain
`x86_64-pc-windows-gnu` host fails to link here (missing `libgcc`/`libgcc_eh`), and `-msvc` would
reintroduce the MSVC dependency wazmrt deliberately avoided. `rust-toolchain.toml` pins bare `nightly`
(portable across OSes); the machine's rustup **default-host** selects gnullvm on Windows.

## Interpreter shape — Option A (carried from wazmrt)

Switch dispatch over a **pre-decoded IR**, untyped `u64` value slots (validation proves types). Keep
the IR a clean seam so a register-machine pass (Option B) is layerable later if benchmarks demand it.
**Not a JIT/AOT** — a native codegen backend violates smallest-binary *and* can't run on the
`wasm32`-freestanding self-embed target. See [design-decisions.md](design-decisions.md) for the perf
ladder and the load-bearing invariants (ValType packing, slot encoding, the `#[inline(never)]` trap
path, shared Memory/Table, etc.).

## Libc-free / allocator

Core links no libc. Native builds and the C-ABI lib pick a global allocator (system/`smp`-equivalent);
the freestanding-`wasm32` build uses a bump/`wasm`-page allocator. Never assume libc `malloc` in the
C-ABI path. Rust: keep core `no_std`-compatible; the capi crate sets `#[global_allocator]`.
