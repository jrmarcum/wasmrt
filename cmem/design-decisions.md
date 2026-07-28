# Design Decisions & Invariants

Load-bearing decisions for the `wasmrt` port. **Do not silently revert these.** Detail + rationale:
`docs/port/` (esp. `00-synthesis.md`, `06-build-docs-licensing.md`).

## Port-level decisions (owner, 2026-07-17)

- **Boundary-faithful, idiomatic Rust.** The observable behavior/outputs and the `wasmrt.h` contract
  are fixed (verified by Rust↔oracle parity); internals are idiomatic Rust (ownership, enums,
  `Result`), not a Zig transliteration. File/module structure mirrors wazmrt for reviewability.
- **Public API = wasmrt's own `wasmrt.h`** — NOT the standard wasm-c-api, NOT wasmtime's exact symbols.
  Strategy: **clean lean `wasmrt_*` C ABI (wasmtime-*shaped*) + a native `wasmrt` Rust crate.** No
  exact-wasmtime compat shim. This drops the entire wasm-c-api refcount object model (wazmrt's
  highest-risk file) — big win for size and safety. See [loaders.md](loaders.md).
- **Feature scope = full wasmtime browser-standard parity + memory64, WASI preview 1 only.** In: MVP,
  reference types, multi-table, bulk memory/table, extended-const, function-references, full WasmGC,
  sat-trunc, **SIMD, multi-memory, threads/atomics, tail calls, exception handling (both exnref and
  legacy), and memory64**. Out: WASI p2/p3 and the **component model** (non-browser-standard).
  **memory64 added to scope 2026-07-27** (owner) — the frozen oracle implements it, so "match the
  oracle" now includes it. This **overrides** wazmrt's earlier narrower "browser-standard bar defers
  SIMD/etc." rule.
- **Size is a first-class goal:** minimize every artifact. Levers — `opt-level="z"` + LTO +
  `codegen-units=1` + strip + `wasm-opt -Oz` for the shipped wasm; pick per artifact.
- **Oracle split — re-checked at the 2026-07-27 freeze; it has all but collapsed.** Features wazmrt
  implements → parity Rust↔wazmrt (golden vectors); at the freeze that is **everything wasmrt targets
  except the tail-call proposal** (`return_call`/`return_call_indirect` — wazmrt has `return_call_ref`
  via function-references but not base tail calls). **Tail calls alone** → oracle against **wasmtime +
  the official spec testsuite**. SIMD, multi-memory, threads/atomics, memory64, and EH (both encodings)
  are now on the wazmrt side of the split (they were on the wasmtime side pre-freeze). See
  [testing.md](testing.md).

## Interpreter = Option A + the perf ladder (from wazmrt)

Switch over a pre-decoded IR; untyped `u64` slots; one shared opcode table for validate + interp +
assembler-in-reverse. Interpreter, **not** JIT/AOT (smallest-binary + wasm-freestanding self-embed).
Perf ladder: A → A.5 (superinstructions / partial-eval) → B (register machine) → JIT (native-only,
later). Decide A→B with benchmarks, not upfront.

## Invariants that must survive the port (easy to get wrong)

- **`ValType` = a `u32` newtype**, concrete refs bit-packed (bit31 concrete, bit30 nullable, bits28-29
  family func/struct/array, bits0-27 index). NOT a plain enum. All accessors are pure bit ops.
- **Slot encoding:** `null_ref = u64::MAX` checked **before** `i31_tag = 1<<63`; heap/func/extern are
  small indices. Three-way discrimination depends on that order.
- **`Op` discriminants are internal tags ≠ wire bytes** for 0xFC/0xFB-prefixed ops; the `fc/gcSubOpcode`
  reverse maps are the emit-side truth. One shared opcode table; keep values stable or the assembler
  breaks.
- **Two-pass type-section decode** (pre-scan kinds) for rec-group forward references.
- **LEB128 over-long/too-large rejection** transcribed exactly (5th-byte `>>4` + sign bits; 10th-byte
  `v ∈ {0,0x7f}`) — conformance suites probe this.
- **`Instance` retains its `Module`** (UAF fix). Tie module lifetime to the instance (Arc/borrow).
- **Trap-record path `#[inline(never)]`/`#[cold]`** — inlining it into the ~200-arm dispatch loop is a
  measured ~14% i-cache regression. Trap byte offsets resolved **lazily** by re-decoding one body; never
  stored per-instruction.
- **Shared `Memory`/`Table`** so `grow` is visible to importers (growth reassigns the backing slice in
  place). Rust: `Rc<RefCell<…>>` / raw shared ownership; refcount `Cell<u32>`, NOT `Arc` (single-thread
  ABI assumption).
- **C-ABI (capi):** lightweight `{id}` handles into a store (no wasm-c-api refcount model);
  **caller-based host callbacks** so a host import can read/write guest memory and return a value (the
  one capability wazmrt's wasm-c-api callback lacks); `#[repr(C)]` for boundary structs; real pointers
  (`*mut c_void`), never hardcoded `i32`.
- **WASI sandbox secure BY CONSTRUCTION** — never resolve a full guest path string against a dir API;
  resolve one component at a time through held handles (`walkFull`); `..` never rises above the preopen;
  absolute symlink targets re-base to the preopen root; `symlink_max`→ELOOP. Rust may use
  `cap-std`/`openat2(RESOLVE_BENEATH)` to close wazmrt's final-component TOCTOU residual (#17). See
  [security-model.md](security-model.md).
- **Pin verify hashes the in-memory bytes it runs** (bytes-hashed == bytes-run); `enforce` denies
  before consulting the opt-out; DB parse fails closed.

## Open decisions — now carried as task-list GATES (owner, 2026-07-27: defer, don't resolve up front)

These four were "raise before/at scaffolding." At the freeze the owner chose to **defer them as explicit
decision-gates at the relevant task-list step** (see [roadmap.md](roadmap.md)) rather than resolve them
all now — each is decided when the port reaches the task that needs it.

- `random_get`: keep wazmrt's non-crypto PRNG (exact parity) or upgrade to an OS CSPRNG in Rust?
  → **gate at the WASI task.** (Note: wazmrt itself moved `random_get` to a real ChaCha CSPRNG on
  2026-07-20 — so "exact parity" now *means* a CSPRNG; confirm the wazmrt behavior when we get there.)
- Zero-dep (wazmrt has zero deps) vs. allow `cap-std`/`openat2` to close the #17 TOCTOU on the Rust side?
  → **gate at the WASI-sandbox task.**
- `wasmrt.h` review (naming, the store simplification, the `{id}`-handle model). The "held until wazmrt
  finalizes" condition is now **met** (oracle frozen) → **gate at the start of the C-ABI task** (finalize
  the draft with the owner before writing `wasmrt-capi`).
- core+capi crate split (recommended) vs. a single multi-target crate. → **gate at the scaffold task.**
