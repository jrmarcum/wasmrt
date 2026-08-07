# Architecture

The Rust architecture for `wasmrt`. It reproduces wazmrt's **decode → validate → instantiate → execute**
pipeline and its **dual-target contract**, in idiomatic Rust. Detail: `docs/port/` (esp.
`00-synthesis.md`, `02-decode-core.md`, `03-validate-interp.md`, `06-build-docs-licensing.md`).

**Added at T8 / v0.9.0 — the crate tree is now complete.** `wasmrt-capi` is real: **~74 exported
functions** over core, plus `features` (proposal gating) and `linker` (name-based import resolution) in
`wasmrt-core`. Three shapes are load-bearing:

- **Two handle kinds, and the split IS the ownership story.** Opaque pointers the caller owns (one
  `_delete` each) versus **checked value handles** (`wasmrt_func_t` and friends) that are never freed.
  Each value handle packs the identity of the store that issued it, so a handle from another store — or
  a deleted one — is **rejected, never followed** into another instance's resources. This is what
  replaces the wasm-c-api refcount model (wazmrt's highest-risk file): the same property, obtained by
  construction rather than by six hand-held invariants. The `+1` in the packing keeps a
  zero-initialized handle permanently invalid.
- **`crates/wasmrt-capi/src/ffi.rs` is the crate's ONLY raw-pointer boundary.** Every primitive is
  justified once, rejects null, and never invents a length. The 74 exports are ordinary safe Rust
  calling into it — which is what keeps `deny(unsafe_code)` a real constraint instead of 74 rubber
  stamps. `wasmrt-core` stays `#![forbid(unsafe_code)]`.
- **The env-finalizer lifecycle is closed by construction.** An instance keeps the host callback it
  linked against, so deleting the linker first would finalize an environment a live instance still
  points at. The environment lives behind an `Rc` shared by the linker *and* every closure linked from
  it, so the finalizer runs when the last holder drops — in whatever order the embedder tears down. No
  documented ordering requirement, and therefore none to get wrong.

Gating happens at **validation**, never at execution (`validate_with_features`), so a refused proposal
can never be half-checked. `ResourceLimits` lives on `Pools`, which the runtime already threads down
every execution path, so every site that must consult a ceiling already holds one.

**Realized before T8 (through T7 / v0.8.0):** the workspace + all three crates exist; `wasmrt-core`
has `types`, `reader`, `opcode`, `module` (decode), `validate` (core-language type-checker), and `interp`
(switch interpreter — integer + float compute + linear memory incl. **multi-memory** and **memory64** +
tables/`call_indirect`/reference types + **WasmGC** structs/arrays/`i31`/casts over a `Store`-owned GC heap +
**SIMD** the full `v128` fixed-width + relaxed set + **threads/atomics** the `0xFE` family, single-threaded
semantics + **exception handling** in both encodings) implemented and parity-tested — the interpreter's
proposal coverage is complete — plus the **text toolchain**: `sexpr` (the shared front-end), `wat` (the
`.wat` assembler, every opcode family) and `wast` (the spec-script runner).

**Added at T7 / v0.8.0:** **host imports** (`Instance::new_with_imports`; `HostFunc` is a boxed closure,
not a fn-pointer + `void*` ctx — that shape cannot be expressed without `unsafe`), **module linking on a
shared store**, and **`wasi`** — complete for preview 1, split into `wasi/mod.rs` (the process surface:
stdio, `args_*`, `environ_*`, clocks, `random_get`, `proc_exit`) and `wasi/fs.rs` (the fd table, the
rights lattice, the sandbox resolver, and ~20 `fd_*`/`path_*` calls). **`pin` remains a doc-comment stub
— now T9** (it was slated for T7, then T8, and slipped both times; `security-model.md` carries the
warning that wasmrt therefore performs no authenticity check today).

**The shared store is the load-bearing T7 shape.** `Store` holds `code: Vec<InstanceData>` and
`pools: Pools` as **separate fields**, so a cross-instance call borrows two disjoint pieces — no `Rc`, no
`RefCell`, no `unsafe`, and no borrow check on the interpreter's hot path. Each instance carries an
`IndexMaps` from its own index space into the pools, and `IndexMaps::get` yields `usize::MAX` out of
range so a bad index **traps rather than aliasing another instance**. `Instance` survives as a thin
`{ store, id }` wrapper, so the single-instance API is unchanged.

The CLI does summarize + validate + `run` + `wasi` (with `--dir`/`--ro-dir` preopens) + `wat` (assemble)
+ `wast` (conformance). **A memory's index type is a property of the
memory, not the engine:** `Memory.is64` drives one `pop_mem(is64)` on the address/count path and one
`mem_addr_ty(index)` in the validator, so every memory op (incl. the `0xFE` atomic and `v128` families)
is memory64-aware through a single choke point rather than duplicated 32/64 opcode arms. **Exceptions
unwind through the ordinary `Result` path:** a throw with no local handler parks itself in
`Store.pending_exn` and returns `Trap::UncaughtException`, which each `call` site offers to its own
handlers (`on_call_error`) before re-raising — so cross-frame unwinding needs no separate stack walker.
Three idiomatic divergences worth noting:
owned `Vec`/`String` data (frees on drop, no arena/`deinit`); the interpreter's
immutable-`module`/`func_bodies` vs `&mut Store` (globals + memories + tables + dropped-data + `gc_heap`)
borrow split (recursive `call` reborrows cleanly, no `RefCell`); and the **128-bit value slot**
(`Value = u128`) so a `v128` is one slot — wazmrt uses two `u64` slots + width tables, wasmrt keeps
"one slot per value" (behavior identical; scalars/refs in the low 64, sentinels unchanged).

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
  - `wasi` — WASI preview 1 (`wasi/mod.rs` process surface + `wasi/fs.rs` fd table, rights lattice and
    the sandbox resolver). The resolver walks **one component at a time**, accumulating a path rather
    than holding dir handles — Rust's `std` has no dir-relative open, and both the zero-dep and
    no-`unsafe` lines are held instead. Escape is unrepresentable either way; see `security-model.md`
    for exactly what that does and does not buy.
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

Switch dispatch over a **pre-decoded IR**, untyped **128-bit** value slots (`Value = u128`; validation
proves types). The slot was `u64` through v0.6.4; widened at the SIMD slice (v0.6.5) so a `v128` occupies
one slot (scalars/refs use the low 64) — wazmrt instead uses two `u64` slots + width tables. Keep
the IR a clean seam so a register-machine pass (Option B) is layerable later if benchmarks demand it.
**Not a JIT/AOT** — a native codegen backend violates smallest-binary *and* can't run on the
`wasm32`-freestanding self-embed target. See [design-decisions.md](design-decisions.md) for the perf
ladder and the load-bearing invariants (ValType packing, slot encoding, the `#[inline(never)]` trap
path, shared Memory/Table, etc.).

## Libc-free / allocator

Core links no libc. Native builds and the C-ABI lib pick a global allocator (system/`smp`-equivalent);
the freestanding-`wasm32` build uses a bump/`wasm`-page allocator. Never assume libc `malloc` in the
C-ABI path. Rust: keep core `no_std`-compatible; the capi crate sets `#[global_allocator]`.
