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

## 🔒 Safety directive — no unsafe constructs migrated from Zig (owner, 2026-08-04)

**The rule.** The Rust codebase shall carry **no unsafe constructs inherited from the Zig oracle.** A
boundary-faithful port must reproduce wazmrt's *observable behavior*, never its *memory-safety posture* —
Zig's manual allocation, raw pointers, arena lifetimes and `@intCast` (UB in ReleaseFast) have no
business surviving translation.

**The sequencing constraint, equally binding: prove the concept first.** Do **not** refactor for safety
on unproven code. A hardening pass over behavior that isn't yet demonstrated correct trades one unknown
for two — you can no longer tell a safety regression from a behavior bug. Establish provability
(conformance + parity green), *then* harden. Hence the order: finish **T7**, review the known issues,
*then* take this on before/with **T8**.

**Baseline as of 2026-08-04 (measured, v0.7.0):** **zero `unsafe` blocks or `unsafe fn` in the whole
workspace.** The only two occurrences of the token are `#[unsafe(no_mangle)]` in `wasmrt-capi` (an
Edition 2024 *attribute syntax* asserting symbol-name uniqueness — not an unsafe block) and the word
"unsafely" in an `interp.rs` comment. The property currently holds **by discipline, not by enforcement**:
there is no `unsafe_code` lint in `[workspace.lints]`. Making it structural is the cheap win.

**What "unsafe constructs from Zig" actually means in a Rust port** — the literal `unsafe` keyword is the
*least* of it, and today's spec-suite run proved the point. The real inherited-hazard surface is:

1. **Panic on guest-controlled input.** Rust turns Zig's UB into a panic, which for an embedder is a
   process abort — a denial-of-service, not a safety win. The suite found exactly this: `unreachable!()`
   on `v128.const i64x2`, and an `item[0]` index on an empty `(item)`. **A library must reject a module,
   never abort its host.** `unwrap`/`expect`/`unreachable!`/direct indexing on any guest-derived path are
   all in scope.
2. **Silent truncation where Zig used `@intCast`.** `as u8` / `as u32` quietly discard bits where the
   oracle would have hit UB. Also found by the suite: out-of-range `i32.const` becoming 0, and
   `v128.const i8x16 0x100` becoming all-zero lanes. Wrong values are worse than rejections.
3. **Unchecked index/length arithmetic** that happens to be in-bounds today.

**The T8 nuance to settle before starting.** The C ABI *cannot* be zero-unsafe: `extern "C"` entry points
receive raw pointers from C callers and must dereference them. So the realistic target is
**`unsafe_code = "forbid"` in `wasmrt-core` (achievable today) and `deny` + narrowly-scoped, individually
justified `allow` in `wasmrt-capi`**, with every such site documented as to what the caller must
guarantee. "Zero unsafe everywhere" would be a promise the FFI boundary makes impossible to keep;
"zero unsafe in the engine, confined and audited at the boundary" is both achievable and honest.

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

## Port-implementation decisions (as built, T0–T5)

Choices made while porting; consistent with "boundary-faithful behavior, idiomatic-Rust internals."

- **Workspace of 3 crates** (`wasmrt-core` / `wasmrt-capi` / `wasmrt`), **edition 2024**
  (`#[unsafe(no_mangle)]` on the C ABI), size-first `[profile.release]` (`opt-level="z"` + LTO +
  `codegen-units=1` + strip + `panic="abort"`). **Windows build host = `x86_64-pc-windows-gnullvm`**
  (see [architecture.md](architecture.md)).
- **Owned data model over an arena.** The decoder returns owned `Vec`/`String`; a `Module` frees on drop
  (no `deinit`, no allocator-error threading). Idiomatic-Rust divergence, behavior identical.
- **`Op` = a macro-defined `#[repr(u8)]` enum** (PascalCase variants) with `from_u8` generated from one
  wire/internal list; **immediates that own data hold a `Vec`**, so dropping the IR frees them (replaces
  wazmrt's manual `freeBody`). The internal-tag-vs-wire-byte invariant is preserved (raw `0xD7`–`0xFA`
  rejected). Raw `0xC5`–`0xCC` accepted as sat-trunc to mirror the oracle (see [known-issues.md](known-issues.md)).
- **CLI grows with the pipeline** — `wasmrt <file>` summarizes + validates (T3/T4); `wasmrt run <file>
  <fn> [args]` executes (T5), parsing args to the export's param types. Faithful to wazmrt's CLI role.
- **Sliced the two correctness-critical, hard-to-test-early modules** (owner-approved): **T4 validate**
  and **T5 interp** land core-first with the exotic proposal arms deferred to `.x` patch releases, because
  each is a trustworthiness promise and most of their tests need the WAT assembler (T6). Deferred ops
  **reject loudly**, never silent-accept. See [known-issues.md](known-issues.md).
- **Interpreter internals** (T5): untyped value slots — `u64` through v0.6.4, **widened to `u128` at the
  SIMD slice (v0.6.5)** so a `v128` occupies ONE slot and the engine stays "one slot per value"
  (select/drop/branch-arity/locals/call-marshaling never reason about slot width). wazmrt instead uses two
  `u64` slots + width tables (`slotWidth`/`local_map`/`drop_select_w`); wasmrt's wider slot trades runtime
  memory (16 B/slot) for eliminating the slot-desync hazard class — an idiomatic divergence, behavior
  identical, scalars/refs in the low 64 so the sentinel invariants hold. `Instance` **owns** its `Module`;
  the immutable `module`/`func_bodies` are threaded separately from `&mut Store` so a recursive `call`
  reborrows cleanly (no `RefCell`); control flow via a precomputed `end_of`/`else_of` table + a label
  stack. Float rounding is bit-manipulation (no_std); **`sqrt` is `std`-gated** (the one no_std float gap).
- **Versioning = port-progress ladder**, per-task release to crates.io (see [releasing.md](releasing.md)).

## Open decisions — now carried as task-list GATES (owner, 2026-07-27: defer, don't resolve up front)

These four were "raise before/at scaffolding." At the freeze the owner chose to **defer them as explicit
decision-gates at the relevant task-list step** (see [roadmap.md](roadmap.md)) rather than resolve them
all now — each is decided when the port reaches the task that needs it.

- ~~`random_get`: non-crypto PRNG vs. an OS CSPRNG?~~ **✅ RESOLVED (owner, 2026-08-04): a ChaCha20
  CSPRNG seeded once from the OS**, matching the oracle (wazmrt moved to ChaCha on 2026-07-20, so parity
  *means* a CSPRNG). Zero dependencies, no `unsafe`, auditable, and it still works on the freestanding
  `wasm32` self-embed target where a syscall has nothing to call. **If OS entropy is unavailable, fail
  loudly — never emit predictable bytes**, the one failure mode that turns a CSPRNG into a security hole.
- ~~Zero-dep vs. `cap-std`/`openat2` for the sandbox path resolver?~~ **✅ RESOLVED (owner, 2026-08-04):
  the zero-dep handle-stack walker** (wazmrt's `walkFull`). Resolve a path **component-by-component
  against open directory handles, never re-opening by full path** — that removes the TOCTOU window
  rather than checking for it, which is how wazmrt's **#17** gets closed *by construction*. Rejected
  `cap-std`/`openat2`: a large dependency tree against the smallest-binary goal, strongest on Linux and
  uneven elsewhere, and it would be wasmrt's first runtime dependency.
- `wasmrt.h` review (naming, the store simplification, the `{id}`-handle model). The "held until wazmrt
  finalizes" condition is now **met** (oracle frozen) → **gate at the start of the C-ABI task** (finalize
  the draft with the owner before writing `wasmrt-capi`).
- core+capi crate split (recommended) vs. a single multi-target crate. → **gate at the scaffold task.**
