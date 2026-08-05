# Known Issues

Issue tracker. Gate open (2026-07-27); **assemble → decode → validate → run all working** (T0–T6 done,
v0.1.0–v0.7.0). This records the **inherited concerns** from the frozen wazmrt oracle, the **port notes /
intentional divergences**, and the **open decisions** (now task-list gates). Log real wasmrt bugs here
(file:line + surfacing condition) as they appear, mirroring wazmrt's ledger.

## Spec-suite punch-list (2026-08-05, post shared-store fixes — 98.6%, 851 failing)

The table/memory family below is **FIXED**. Three defects, all one root cause plus one assembler gap:

1. **`Op::CallIndirect` indexed `store.tables` with the raw module-local immediate** — it never went
   through `ctx.maps.table()`. `interp.rs` ~1708. With one instance per store the two indices coincide,
   so this was invisible until `register` put 52 modules in one store; then module N's `call_indirect`
   read module 0's table ("got 5, expected 3"). This is the same class clippy caught earlier via the
   `unused variable: maps` warning, but at a site the compiler could not flag, because `ci.table` is a
   different expression from `table_imm(instr)`.
2. **`exec_memory_init` indexed `ctx.module.data` with the *store* index `di`** — the mirror image of
   (1): the drop flags are store-wide, but the segment bytes live in the module's own list, so that one
   needs the module-local index. `interp.rs` ~2386.
3. **The assembler rejected the `table.copy` / `table.init` index shorthands** (both table indices may
   be omitted, defaulting to 0). Flat form needed a matching fix so a bare `table.copy` does not eat the
   next instruction's two atoms — `has_optional_indices()` in `wat.rs`.

| file | before | after |
| --- | --- | --- |
| `table_copy` | 216 | **0** |
| `table_init` | 68 | **0** |
| `memory_init` | 27 | **0** |
| `linking` | 23 | 16 |
| suite total | 1,521 failing / 97.4% | **851 failing / 98.6%** |

Suite now: **59,261 passed, 851 failed, 4,720 skipped** (284 files, 1 unparseable).

Regression tests added for all three (`wast.rs`: `call_indirect_uses_the_callers_own_table`,
`memory_init_reads_the_instances_own_data_segment` — both keep a *second* instance alive so the store
and module indices cannot coincide; `wat.rs`: `assembles_the_table_index_shorthands`). Workspace: 254.

**Standing lesson:** with a shared store, every pool access must route through `IndexMaps`, and every
`ctx.module.*` access must use the module-local index. A test with one instance per store proves
nothing about either — the indices are equal. New tests in this area must instantiate two modules.

Remaining after the fix: `table_copy64` 22 / `table_init64` 3 are the 64-bit **table index type**
(`(table $t i64 30 30 funcref)`), still out of scope per the recorded invariant. The rest of the
worst-file list is the T6-era literal/binary punch-list, unchanged.

## Superseded punch-list (2026-08-04, post-linking — 97.4%, 1,521 failing)

**The pass RATE fell while the capability rose, and that is the honest reading.** Wiring `register` + a
`spectest` provider into the `.wast` runner moved **2,784 assertions out of "skipped"**: 2,032 became
passes (54,509 → **56,541**) and ~649 became *visible* failures (872 → 1,521). Nothing regressed — those
649 were always broken, just hidden behind a skip. A harness that folded skips into passes would have
shown a rise here; ours shows the truth.

| | before | after |
| --- | --- | --- |
| passed | 54,509 | **56,541** |
| failed | 872 | 1,521 |
| skipped | 9,605 | **6,821** |
| rate | 98.4% | 97.4% |

**Newly exposed, biggest first — these are now the real punch-list:**

- **`table_copy` 216 + `table_copy64` 238 + `table_init` 68 + `table_init64` 71.** These files now *run*
  (916 assertions in `table_copy` alone, previously all skipped) and return **wrong values** — e.g. got
  5 where 3 was expected. A genuine `table.copy`/`table.init` or elem-segment-with-imports bug, and the
  single biggest win available. **Start here.**
- `memory_init` 27 + `memory_init64` 27 — likely the same family.
- `linking.wast` 23 — the linking semantics themselves.
- The T6-era literal/binary items below are unchanged.

Still skipped (6,821): imported **memories and tables**, which need shared-resource linking beyond
functions — `imports.wast` alone still skips 168.

## Earlier punch-list (2026-08-03, v0.7.0 — 98.4%, 871 failing)

Deliberately **not** chased before T7 (owner's call): most of the 9,608 skips need host imports, so the
picture will change substantially once those land. Re-run `wasmrt wast <testsuite>` after T7 before
triaging further. Worst files as of the v0.7.0 run:

| file | failing | likely cause |
| --- | --- | --- |
| `simd_const.wast` | 47 | remaining malformed-literal forms the assembler still accepts |
| `binary.wast` (×2) | 43 each | `(module binary …)` edge cases — some rejected at the wrong stage |
| `type-subtyping.wast` | 36 | GC subtyping depth the validator does not model yet |
| `table_copy64.wast` | 33 | 64-bit table indices — out of scope (tables stay 32-bit, matching the oracle) |
| `i31.wast` | 30 | i31 edge semantics |
| `const.wast` / `float_literals.wast` | 26 each | literal forms still over-accepted |

Two are known-and-intended rather than bugs: **`table_copy64`** exercises 64-bit tables, which wasmrt
rejects on purpose (the oracle does too), and **`legacy/try_delegate.wast`** exercises `delegate`, which
all three of assembler/validator/interpreter reject on purpose.

## Port notes / intentional divergences (T0–T6)

- **Owned `Vec`/`String` data model instead of wazmrt's arena** (`module.rs` T3, `interp.rs` T5). Frees
  on drop — no `deinit`, no allocator-error threading. `Instance` **owns** its `Module` (the retain-invariant,
  free). Observable behavior identical; an idiomatic-Rust internal choice.
- **Interpreter borrow split** (`interp.rs`, T5): immutable `module`/`func_bodies` are threaded separately
  from `&mut globals`, so a recursive `call` reborrows cleanly — no `RefCell`, no self-referential borrow.
- **Two slices were split core-first, exotic-later** because they're a correctness promise AND their
  exotic tests needed the WAT assembler (T6): **T4 validate** (core language first; SIMD/atomics/GC-objects/EH
  typing landed in v0.7.0) and **T5 interp** (integer v0.6.0 → float v0.6.1 → linear memory v0.6.2 → tables/reftypes
  v0.6.3 → GC v0.6.4 → SIMD v0.6.5 → threads/memory64/EH in later 0.6.x). Deferred ops in both **reject loudly** (`UnsupportedValidation` / `UnsupportedInstruction`),
  never silent-accept — so a verdict/result is always trustworthy.
- **The interpreter value slot is 128-bit (`Value = u128`)** since the SIMD slice (`interp.rs`, T5 slice 6
  / v0.6.5). wazmrt stores a `v128` as **two `u64` slots** and carries width tables (`slotWidth`,
  `local_map`/`local_w`, `drop_select_w`, slot-counted arity) to size `drop`/`select` — an explicit
  "stack-desync hazard if missed." wasmrt instead widens the slot so a `v128` is **one** slot: the whole
  engine stays "one slot per value" (select/drop/arity/locals/call-marshaling never reason about width),
  eliminating that hazard class. Cost: every slot is 16 bytes (runtime memory, not binary size — the
  "small" ethos is about binary size). Scalars/refs live in the low 64 bits, so the `NULL_REF` (`u64::MAX`)
  / `I31_TAG` (`1<<63`) sentinel invariants are unchanged. Observable behavior identical → parity holds.
  A `const _: () = assert!(I31_TAG == 1u128 << 63)` guards the sentinel placement.
- **Atomics execute with single-threaded semantics** (`interp.rs exec_atomic`, v0.6.7). The `0xFE` family
  runs, but the engine has one thread: every atomic access is trivially atomic, `atomic.fence` is a no-op,
  `memory.atomic.wait*` never blocks (value mismatch → 1 "not equal", match → 2 "timed out" since nothing
  can `notify`), and `notify` wakes 0. This is a **conforming** implementation for a single-threaded host,
  and matches the frozen oracle — genuine parallel execution is out of scope for the interpreter. Atomics
  add two stricter-than-normal traps: `UnalignedAtomic` (the effective address must be naturally aligned
  to the access width) and `ExpectedSharedMemory` (`wait*` requires a `shared` memory). The `shared` flag
  is decoded (limits bit 1) and now threaded onto the runtime `Memory`.
- **A DEBUG build can overflow the native stack before the 512-frame recursion cap fires**
  (found 2026-08-03 while building the `.wast` runner, via `assert_exhaustion` on a self-recursive
  function). `MAX_CALL_DEPTH = 512` matches the frozen oracle exactly, and **release builds are fine** —
  the guard fires and yields `CallStackExhausted` as it should. But the interpreter recurses on the host
  stack (`call_function` → `run` → `call_function`), and an un-inlined debug `run` frame is large enough
  that 512 of them can exhaust a default 8 MB thread stack first, aborting the process.
  **Deliberately NOT "fixed" by lowering the cap** — that would diverge from the oracle on legal
  deeply-recursive programs, and the release profile (what ships) behaves correctly. The runner's own
  test spawns a 32 MB-stack thread. **Revisit at T8**: an embedder linking the debug cdylib is exposed,
  so the real fix is either a configurable depth limit on the C ABI or shrinking the per-frame footprint.
  Worth re-measuring once the spec suite runs, since it has deeper recursion cases than the hand vectors.
- **`delegate` is rejected, inherited from the oracle** (`interp.rs throw_exception`, v0.6.9). `delegate l`
  re-raises an exception "at label `l`", routing that can SKIP handlers an ordinary outward unwind would
  run. wazmrt does not implement that label arithmetic (no reference impl remained to verify it against)
  and its **validator rejects `delegate` outright**; its interpreter traps loudly as the defense for the
  unvalidated run path. wasmrt matches exactly: reaching a delegating `try` while unwinding yields
  `UnsupportedInstruction` rather than silently mis-routing. Every other legacy construct
  (`try`/`catch`/`catch_all`/`rethrow`) is fully supported. **As of v0.7.0 the validator AND the assembler
  reject it too**, so all three agree and no module can validate yet mis-route.
- **EH has two structurally different unwind paths, and conflating them is the bug to avoid** (v0.6.9).
  A `try_table` clause branches **out of** the try_table to its target label (label popped); a legacy
  `catch` runs **inside** the try, whose label stays live so `rethrow` can name it. Two consequences are
  load-bearing: (1) a `throw` from inside a legacy handler must propagate OUTWARD — the `caught.is_some()`
  skip in `throw_exception` — or the idiom `catch (e) { throw e; }` re-matches its own handler forever;
  (2) `rethrow` pops its try BEFORE re-raising, so the same handler cannot catch it again.
- **Exceptions are not garbage-collected.** `catch_ref`/`catch_all_ref` box an exception into
  `Store.exn_store` so it can be an `exnref` value; the box lives until the invocation ends, bounded by
  `MAX_EXN_BOXES` (`ExnStoreExhausted` past that) — the same no-collector treatment as the GC heap. An
  ordinary `throw`/`catch` round-trip never boxes, so a throwing loop does not grow the store. EH state
  (`pending_exn` + `exn_store`) resets per invocation, so nothing leaks between calls.
- **memory64 needed no new engine code either** (v0.6.8) — the second "already worked, now proven" slice.
  The 64-bit plumbing was built generically in the linear-memory slice (0.6.2): the `is64` limits flag,
  `u64` `memarg` offsets, `mem_addr_ty` per-memory address typing in the validator, and the interpreter's
  `pop_mem(is64)`. v0.6.8's deliverable is the 18 conformance vectors + the flipped use-case cell. Three
  facts worth not drifting on: (1) **tables stay 32-bit-indexed** — the memory64 proposal's 64-bit *table*
  extension is out of scope because the frozen oracle rejects an `i64` table type as `MalformedFlag`
  (`module.rs read_table_type`), and wasmrt matches; (2) a `memarg` static offset above `u32` is legal
  only on a 64-bit memory (`validate.rs check_mem_offset`) — a decode-time `u64` read, a validate-time
  rule; (3) **`memory.copy` between a 64-bit and a 32-bit memory takes an `i32` count** (the narrower of
  the two index types) while each address keeps its own — the one place the two widths meet.
- **A memory64 memory may declare far more than the instance will back.** The declared minimum is checked
  against the per-instance budget (`DEFAULT_MAX_MEMORY_BYTES`, 1 GiB) at instantiation, so a module
  declaring e.g. 2^40 pages fails with `MemoryLimitExceeded` rather than attempting the allocation; the
  size computation is overflow-checked before the budget test. Validation separately caps the *declared*
  limits at the type ceiling (2^48 pages for a 64-bit memory, 2^16 for a 32-bit one). Guests that assume
  a genuinely huge memory will not run until the budget is made configurable (a T8/T9 concern).
- **Multi-memory needed no new engine code** (v0.6.6). The memory-index plumbing was built generically in
  the linear-memory slice (0.6.2) — `Vec<Memory>`, `memarg` memory index, `require_memory` in validate,
  cross-memory `memory.copy`, flag-`0x02` data segments, per-memory instantiation. v0.6.6's deliverable is
  the conformance vectors that prove it (distinct routing, active data → mem1, cross-mem copy) + the flipped
  use-case cell — an honest "already worked, now tested" release, not new implementation.
- **SIMD is complete, incl. relaxed SIMD** (`interp.rs exec_simd`, v0.6.5) — the deferral is gone. Relaxed
  ops each take **one fixed deterministic choice** (matching the frozen oracle): `relaxed_trunc` →
  saturating (`trunc_sat`), `relaxed_madd`/`nmadd` → double-rounding (`a*b` then `±c`), `relaxed_laneselect`
  → full bitselect, `relaxed_min`/`max` → the `@min`/`@max` (fmin/fmax) choice, `relaxed_dot` → signed with
  saturation. Also **WasmGC `v128` fields now execute** (the 0.6.4 `field_is_v128` reject guard is removed —
  a field is one `Value`), and `v128.const` is valid in constant expressions (`v128` globals).
- **WasmGC executes over a `Store`-owned managed heap** (`interp.rs`, T5 slice 5 / v0.6.4):
  `gc_heap: Vec<HeapObject>` grown per allocation, bounded by a per-run object budget — no collector yet
  (objects live until the store drops; fine for the run-to-completion interpreter). The load-bearing
  **slot-encoding order is honored: `NULL_REF` (`u64::MAX`) is checked BEFORE `I31_TAG` (`1<<63`)** so a
  null ref never reads as an `i31`. `v128` struct/array fields were deferred here and **landed in v0.6.5**
  (SIMD slice). Still **deferred, reject loudly**: GC allocation inside constant expressions
  (`struct.new`/`array.new`/`ref.i31` in a global initializer) traps `UnsupportedInstruction` rather than
  silently mis-execute. (Non-GC `v128.const` in const-exprs IS supported as of v0.6.5.)
- **`sqrt` is `std`-gated** (`interp.rs`, T5 float): uses the platform math lib with the default `std`
  feature; a freestanding `no_std` build traps on `sqrt` alone. The one no_std float gap — revisit with a
  software sqrt (or `libm`, if the zero-dep stance relaxes) when the freestanding-wasm target is finished.
- **Rust `f as int` == wasm saturating truncation** (`interp.rs`, T5 float): NaN→0 + saturate-to-min/max
  matches `trunc_sat_*` exactly, so those 8 ops are one-liners. Noted so nobody "fixes" it into a manual
  clamp.
- **`interp` runs import-free modules only for now** (`ImportsUnsupported`). Host imports (which WASI
  needs) fold in during the 0.6.x execution slices.
- **Raw single-byte `0xC5`–`0xCC` accepted as saturating-truncation ops** (`opcode.rs`, T2), mirroring the
  wazmrt oracle (canonical encoding is `0xFC 0x00`–`0x07`). Kept for parity; re-examine against the spec
  suite at T6.
- **Deferred, tracked** (land with their consumer): only `decode_body_tracked` byte-offsets for trap
  backtraces remains (T8). The `fc`/`gc` reverse maps landed with the assembler and the SIMD/atomic
  natural-align tables with the validation arms, both in v0.7.0; `natural_align_log2` landed at T4.
- No real wasmrt *bugs* logged yet. Each release is parity-gated (ported oracle vectors) + clippy-clean.

## Inherited from wazmrt — relevant to the port

- **#17 (final-component TOCTOU) + #18 (Zig std Windows crash) + #23 (Zig std Windows `Io` gaps)** —
  all **Zig-0.16-std-specific**. wazmrt's Windows `openFile(.follow_symlinks=false)` crashes the host,
  forcing a stat-then-open-with-follow that leaves a narrow final-component TOCTOU on `path_open`, and
  `setTimestamps`/`hardLink` path-forms are unavailable on Windows. **wasmrt should implement the CLEAN
  version** (atomic no-follow open via std / `cap-std` / `openat2(RESOLVE_BENEATH)`), closing #17's
  residual and dodging #18/#23 — same behavior, better mechanism. See `security-model.md`.
- **wasm-c-api refcount object model** — wazmrt's single highest-risk file (`wasm_c_api.zig`, 6
  memory-safety invariants, a lifecycle fuzz). wasmrt **avoids it** by designing `wasmrt.h` around
  lightweight `{id}` handles instead of refcounted objects (`design-decisions.md`). Still fuzz instance/
  memory lifetimes under Miri (`testing.md`).
- **wazmrt deferred C-ABI residuals** (shared-mutable imported globals; externref via `wasm_table_get`)
  — likely moot: none of the loaders need them (`docs/port/08`).

## Scope gap (not a bug — a work item) — SHRUNK TO ONE ITEM at the 2026-07-27 freeze

- **Only the tail-call proposal (`return_call`/`return_call_indirect`) has no wazmrt oracle.** wazmrt
  has `return_call_ref` (function-references) but not base tail calls. wasmrt's scope requires them
  (full browser-standard parity) → conform against **wasmtime + the official spec testsuite**.
- **Everything else wasmrt targets now HAS a wazmrt oracle.** SIMD (full 0xFD incl. relaxed),
  multi-memory, threads/atomics (0xFE), memory64, and exception handling (both exnref and legacy) all
  landed in wazmrt before the freeze — they moved from the wasmtime side of the oracle split to the
  wazmrt side. memory64 is **in scope** (owner, 2026-07-27). Re-check only if the frozen oracle drifts.

## Open decisions (owner — from `design-decisions.md`) — deferred as task-list GATES (2026-07-27)

The owner chose (2026-07-27) to **defer these as decision-gates at the relevant conversion step** rather
than resolve them up front. Decide each when the port reaches its task (see `roadmap.md`):

- `random_get`: parity PRNG vs OS CSPRNG → **WASI task** (wazmrt now uses a ChaCha CSPRNG, so parity ≈ CSPRNG).
- Zero-dep vs. `cap-std`/`openat2` to close #17 TOCTOU → **WASI-sandbox task**.
- `wasmrt.h` review — the "held until wazmrt finalizes" block is now **lifted** (oracle frozen) →
  **C-ABI task** (finalize with the owner before writing `wasmrt-capi`).
- core+capi crate split vs. single multi-target crate → **scaffold task**.

## Triggers (from `INDEX.md`)

Recheck the Zig-std items (#17/#18/#23) framing on every wazmrt sync — they may change as wazmrt or
Zig evolves, and they inform how much of the sandbox code is a straight port vs. a clean Rust rewrite.
