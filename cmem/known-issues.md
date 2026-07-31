# Known Issues

Issue tracker. Gate open (2026-07-27); decode → validate → run all working (T0–T3 + T4-core + T5 slices
1–7 done, v0.1.0–v0.6.6). This records the **inherited concerns** from the frozen wazmrt oracle, the
**port notes / intentional divergences** logged so far, and the **open decisions** (now task-list gates).
Log real wasmrt bugs here (file:line + surfacing condition) as they appear, mirroring wazmrt's ledger.

## Port notes / intentional divergences (T0–T5)

- **Owned `Vec`/`String` data model instead of wazmrt's arena** (`module.rs` T3, `interp.rs` T5). Frees
  on drop — no `deinit`, no allocator-error threading. `Instance` **owns** its `Module` (the retain-invariant,
  free). Observable behavior identical; an idiomatic-Rust internal choice.
- **Interpreter borrow split** (`interp.rs`, T5): immutable `module`/`func_bodies` are threaded separately
  from `&mut globals`, so a recursive `call` reborrows cleanly — no `RefCell`, no self-referential borrow.
- **Two slices were split core-first, exotic-later** because they're a correctness promise AND their
  exotic tests need the WAT assembler (T6): **T4 validate** (core language now; SIMD/atomics/GC-objects/EH
  typing → 0.5.x) and **T5 interp** (integer v0.6.0 → float v0.6.1 → linear memory v0.6.2 → tables/reftypes
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
- **Deferred, tracked** (land with their consumer): the `fc`/`gc` reverse maps + SIMD/atomic natural-align
  tables (assembler T6 / the 0.5.x validation arms); `decode_body_tracked` byte-offsets for trap
  backtraces (T8). `natural_align_log2` already landed (T4).
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
