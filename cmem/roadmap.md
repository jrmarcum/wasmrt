# Roadmap

## Status (2026-07-27) — PORT phase; gate OPEN, oracle FROZEN

The conversion has **begun**. The `wazmrt` oracle reached full parity and is **frozen** at
`wazmrt@dadc727`; `zig build test` passes (489/493, 4 skip; Debug + ReleaseSafe green) — gate verified
2026-07-27. `scripts/check-wazmrt.sh` now watches for **oracle drift**, not freeze-readiness. The oracle
covers **every wasm proposal wasmrt targets except tail calls** (`return_call`/`return_call_indirect`),
so the oracle split has collapsed to that one item (see `design-decisions.md`, `testing.md`). **memory64
is in scope** (owner, 2026-07-27). **T0–T3 DONE + T4 core (v0.5.0) + T5 slices 1–8 (v0.6.0 integer,
v0.6.1 float, v0.6.2 linear memory, v0.6.3 tables + reference types, v0.6.4 WasmGC, v0.6.5 SIMD,
v0.6.6 multi-memory, v0.6.7 threads/atomics) DONE. Next: 0.6.x — memory64 → EH + host imports; + the
deferred 0.5.x validation arms.**

**Prep DONE (pre-freeze):** scope reconciled (a faithful runtime port; fidelity = boundary-faithful +
idiomatic Rust; success = **canonical / fast / small**, `vision.md`); full **deep-read of wazmrt** (6
subsystem maps, `docs/port/00`–`06`); the **`universalWasmLoader` survey** + ABI strategy + 10-loader
phased list (`docs/port/07`–`08`, `loaders.md`); the **`wasmrt.h` v0 draft** (`docs/port/wasmrt.h.draft`,
review-block now lifted); the **oracle monitor**; and the **project scaffold** (`cmem/`, `bench/`,
`examples/`, `tests/`, dual licenses, `NOTICE`, `third_party/LICENSES.md`) — committed + pushed to
`origin`.

## Conversion task list (bottom-up, parity-gated each step)

Port order mirrors wazmrt's `src/` so every module can be oracle-diffed against its Zig counterpart.
Each task is DONE only when it adds unit tests **and** passes its parity/conformance gate with **zero
regressions across all build surfaces** (`cargo test` / native / `wasm32` / c-smoke where relevant) —
diff the OUTPUT counts, not exit codes (`testing.md`). `[ ]` = not started.

- **T0 — Crate scaffold + dual-target build. ✅ DONE 2026-07-27.** *Decision-gate resolved (owner):*
  **workspace of 3** — `crates/wasmrt-core` (`no_std`-friendly, `default=["std"]`; `wasi` behind `std`),
  `crates/wasmrt-capi` (`staticlib`+`cdylib`+`rlib`, ships `include/wasmrt.h`), `crates/wasmrt-cli` (bin
  `wasmrt`). Edition 2024, size-first `[profile.release]` (`opt-level="z"`+LTO+`codegen-units=1`+strip+
  `panic="abort"`), workspace clippy lints. core's module tree is stubbed to mirror wazmrt `src/` (each
  stub cites its port task + invariant). **Gate met:** all four surfaces build — CLI bin, `libwasmrt_capi.a`,
  `wasmrt_capi.dll`, and freestanding `wasm32-unknown-unknown` core (`--no-default-features`, no_std,
  libc-free); `cargo test` 3/0, `cargo clippy` clean. **Build-host note:** on Windows use the
  **`x86_64-pc-windows-gnullvm`** host (LLVM-MinGW + UCRT — matches the libc-free/no-MSVC ethos); plain
  `-gnu` fails to link (no classic `libgcc`). `rust-toolchain.toml` pins bare `nightly` (portable) — set
  the machine default-host to gnullvm. `cargo-zigbuild` + build-std tuning deferred to when cross-native/
  size builds are actually needed. `[x]`
- **T1 — `types` + `reader`. ✅ DONE 2026-07-28 (v0.2.0).** `ValType` `u32` newtype (bit-packed concrete
  refs — invariant held), `RefHeap` + GC subtyping (`is_subtype_of`), `SectionId`/`ExternKind`/`DecodeError`;
  zero-copy `Reader` with spec-correct LEB128 (`u32`/`u64`/`i32`/`i64`/`s33`, over-long / too-large
  rejection) + fixed/float reads. **Gate met:** wazmrt's LEB accept/reject vectors ported 1:1 (+ u64/s33/
  skip/vec-len) and ValType bit-op/subtyping vectors; 20 core tests pass, clippy clean, native +
  `wasm32` no_std green. `[x]`
- **T2 — `opcode` (the shared IR table). ✅ DONE 2026-07-28 (v0.3.0).** Complete `Op` table (macro-defined
  enum + `from_u8`; PascalCase variants), `Imm`/`Instr` with `Vec`-owning immediates (Drop replaces
  wazmrt `freeBody`), `immediate_kind`, all four prefix decoders (`0xFC`/`0xFD`/`0xFB`/`0xFE`), and
  `decode_body`. Invariant held: internal tags `0xD7`–`0xFA` ≠ wire bytes, raw ones rejected;
  lane/heap/block-type range-checked at decode. **Gate met:** wazmrt's `decodeBody` test vectors ported
  1:1 (+ prefix/rejection cases); 30 core tests, clippy clean, native + `wasm32` no_std green.
  **Deferred (land with their consumers):** the `fc`/`gc` reverse maps + natural-align tables (assembler
  T6 / validator T4); `decode_body_tracked` byte-offsets (trap backtraces, T5/T8). `[x]`
- **T3 — `module` (decode). ✅ DONE 2026-07-28 (v0.4.0).** All core sections + resolved import/export
  externs + bodies; two-pass type-section decode (rec-group forward refs); custom `name` section;
  data-count check; reserved-byte + non-UTF-8 rejection; 64-bit limits flag (memory64). Owned data model
  (`Vec`/`String`) replaces wazmrt's arena — no `deinit`. Also wired **`wasmrt <file.wasm>`** decode
  summary (+ `-h`/`-v`). **Gate met:** wazmrt's 15 decode/rejection vectors ported 1:1; verified on a
  real `add.wasm` via the CLI; 45 core tests, clippy clean, native + `wasm32` no_std green. (`wasm_mod`
  corpus lives on removable media — the ported oracle tests stand in for it.) `[x]`
- **T4 — `validate`. ◐ CORE DONE 2026-07-28 (v0.5.0); exotic arms → 0.5.x.** The spec §3 validation
  algorithm (value + control-frame stacks, `unknown` bottom), `subtype_of`, `simple_sig`, module-level
  checks (count-match, const-expr typing, elements/data, limits §3.2.5, tags, dup-exports, start,
  `C.refs`), local-init tracking, `natural_align_log2` (the deferred T2 table), per-memory (memory64)
  address typing. **Owner decision (2026-07-28):** land the **core language** now (MVP/refs/bulk-memory/
  tables/i31 — hand-testable), **defer SIMD/atomics/GC-objects+casts/EH typing to 0.5.x** (real coverage
  at T6 via the spec suite). Deferred ops reject loudly (`UnsupportedValidation`), never silent-accept.
  CLI prints a validation verdict. 54 core tests, clippy clean, native + `wasm32` no_std green.
  **Gate note:** `assert_invalid`/`assert_malformed` spec-suite parity is the T6 gate (needs the `.wast`
  runner); at T4 the gate is the ported oracle hand-vectors + no over-acceptance on the core set. `[◐]`
  - **0.5.x follow-up:** port the deferred validation arms — `simd_sig`, atomic typing, GC struct/array/
    cast typing, EH (`try_table`/`throw`/legacy) — + the SIMD/atomic natural-align tables. Then T5. `[ ]`
- **T5 — `interp` (the switch interpreter). ◐ FIRST SLICE DONE 2026-07-28 (v0.6.0): integer compute.**
  Ported the value model (`u64` slots), `Instance`/instantiation (decode bodies + `precompute_control_flow`
  end_of/else_of + eval global inits), `Frame` + `branch` (label stack) + the `run` dispatch loop, and
  `exec_numeric` for **i32/i64** arith/compare/bitwise/shift/rotate/extend/wrap. Control flow
  (block/loop/if/else/br/br_if/br_table/return), direct `call` incl. **recursion** (depth-capped),
  `local.*`/`global.*`/drop/select/const. **Rust ownership:** split immutable `module`/`func_bodies`
  from `&mut globals` so recursive `call` reborrows cleanly; `Instance` owns its `Module`. CLI **`wasmrt
  run <file> <fn> [args]`** (verified: `fac 10`→3628800, `add 40 2`→42). Deferred ops trap loudly
  (`UnsupportedInstruction`); import-free modules only. 60 core tests (add/fac/loop-sum/traps/i64),
  clippy clean, native + wasm32 no_std. `[◐]`
  - **Slice 2 — float arith. ✅ DONE 2026-07-28 (v0.6.1).** f32/f64 arith/compare/`min`/`max` (NaN-prop +
    signed-zero)/`abs`/`neg`/`copysign`/rounding (bit-based `ceil`/`floor`/`trunc`/`nearest` ties-to-even)
    + float↔int trapping & saturating (Rust's `as` cast matches wasm sat exactly) + demote/promote/
    reinterpret. **`sqrt` is `std`-gated** (platform libm; no_std build traps on sqrt only — everything
    else float is no_std-clean). 65 core tests, clippy clean.
  - **Slice 3 — linear memory. ✅ DONE 2026-07-28 (v0.6.2).** All loads/stores (widths + sign/zero
    extend), `memory.size`/`grow`, bulk (`memory.copy`/`fill`/`init`, `data.drop`), active data-segment
    init at instantiation; overflow-safe effective-address bounds checks; per-memory (memory64) address
    typing; 1 GiB per-instance budget; `alloc_zeroed`-backed (demand-zero). **Refactor:** the mutable
    runtime state (globals/memories/data_dropped) is now a `Store` threaded as `&mut` (recursive `call`
    reborrows cleanly). 69 core tests; store/load round-trips via CLI. `wasm_mod`-class guests now run.
  - **Slice 4 — tables + reference types. ✅ DONE 2026-07-28 (v0.6.3).** `Table { entries: Vec<Value>,
    max }` in the `Store`; `call_indirect` (table lookup + runtime signature check), `table.get`/`set`/
    `size`/`grow`/`fill`/`init`/`copy`, `elem.drop`; `ref.null`/`is_null`/`func`/`as_non_null`/`br_on_null`/
    `br_on_non_null`/`call_ref`/`return_call_ref`; element-segment init at instantiation; `ref.null`/
    `ref.func` in const-exprs. **Slot-encoding invariant in place:** `NULL_REF = u64::MAX` (funcref = small
    function index; i31_tag checked after it, at GC). Per-instance table-entry budget. 72 core tests.
  - **Slice 5 — WasmGC. ✅ DONE 2026-07-31 (v0.6.4).** A managed GC heap on the `Store`
    (`gc_heap: Vec<HeapObject { type_index, fields: Vec<Value> }>`, per-run object budget). Unboxed `i31`
    (`ref.i31`/`i31.get_s`/`get_u`) with the **slot-encoding order honored — `NULL_REF` checked BEFORE
    `I31_TAG = 1<<63`**; `ref.eq`; heap **structs** (`struct.new`/`new_default`/`get`/`get_s`/`get_u`/`set`)
    and **arrays** (`array.new`/`new_default`/`new_fixed`/`get`/`get_s`/`get_u`/`set`/`len`) with packed
    `i8`/`i16` field storage (sign/zero-extend on the narrow gets); casts `ref.test`/`ref.cast` +
    `br_on_cast`/`br_on_cast_fail` over a runtime subtype check (`ref_matches`). **Deferred, reject loudly:**
    `v128` GC fields (land with SIMD) and GC allocation in const-exprs (`struct.new` in a global init).
    76 core tests, clippy clean, native + wasm32 no_std. `[✅]`
  - **Slice 6 — SIMD. ✅ DONE 2026-07-31 (v0.6.5).** The **entire fixed-width + relaxed SIMD set** (`0xFD`
    `v128`, ~230 sub-opcodes): const/splat/extract/replace, shuffle/swizzle, lane-wise int+float
    arith/cmp/shift, sat add/sub, min/max, avgr, abs/neg/popcnt, bitwise+bitselect, any/all_true/bitmask,
    narrow/extend/extmul/extadd/dot/q15, int↔float convert/trunc_sat/demote/promote, all v128 loads/stores
    (splat/extend/zero/lane), relaxed ops (each pinned to one deterministic choice). **Load-bearing design
    decision: the value slot was widened to 128-bit (`Value = u128`)** so a `v128` is ONE slot — the whole
    engine stays "one slot per value" (select/drop/arity/locals/call-marshaling untouched), an idiomatic
    divergence from wazmrt's 2-`u64`-slots + width-tables. Scalars/refs in the low 64; `NULL_REF`/`I31_TAG`
    invariants unchanged. Also: `v128.const` in const-exprs (v128 globals) + **the GC `v128`-field
    deferral lifted** (a field is one `Value`). Ported opcode-for-opcode from wazmrt `interp.zig execSimd`.
    86 core tests, clippy clean, native + wasm32 no_std. `[✅]`
  - **Slice 7 — multi-memory. ✅ DONE 2026-07-31 (v0.6.6).** **Already-built infrastructure, now
    conformance-tested + matrix-flipped.** The memory-index plumbing landed with linear memory (0.6.2):
    `store.memories` is a `Vec`, the `memarg` decodes its memory index (alignment-flag `0x40`), `validate`
    checks indices via `require_memory`, `memory.copy` already handles cross-memory (dst≠src), data
    segments carry an explicit `mem_index` (flag-`0x02`), and instantiation creates every memory + applies
    each active segment to its own memory. This slice added 3 conformance vectors — distinct-memory
    routing (store/load to mem0 vs mem1), an active data segment (flag 2) initializing mem1, a
    cross-memory `memory.copy` — all pass. No new engine code; the deliverable is the proof + the flipped
    use-case cell. 89 core tests, clippy clean, native + wasm32 no_std. `[✅]`
  - **Slice 8 — threads / atomics. ✅ DONE 2026-07-31 (v0.6.7).** The `0xFE` atomic family in a new
    `exec_atomic`: atomic loads/stores (i32/i64 + 8/16/32-bit widths), RMW (add/sub/and/or/xor/xchg) +
    cmpxchg, `memory.atomic.wait32`/`wait64`/`notify`, `atomic.fence`. **Single-threaded semantics**
    (frozen-oracle parity): every access trivially atomic, `fence` a no-op, `wait*` never blocks (mismatch
    → 1, match → 2 "timed out"), `notify` wakes 0. New: `shared` flag threaded onto `Memory` (from
    `limits.shared`); two traps — **`UnalignedAtomic`** (atomic EA must be naturally aligned, stricter than
    plain loads/stores) and **`ExpectedSharedMemory`** (`wait*` needs a shared memory). Ported from wazmrt
    `interp.zig execAtomic`. 94 core tests (5 new: rmw.add, cmpxchg, unaligned trap, wait-nonshared trap,
    wait-shared mismatch), clippy clean, native + wasm32 no_std. `[✅]`
  - **0.6.x remaining slices (per the roadmap order below):** memory64 → EH. Also fold in host imports
    (WASI needs them) + the deferred 0.5.x validation arms.
    Original T5 detail:
- **T5 (original spec) — `interp`.** Untyped `u64` slots; the slot-encoding order invariant
  (`null_ref` before `i31_tag`); `#[cold]`/`#[inline(never)]` trap path with lazy byte-offset resolve;
  shared `Memory`/`Table` (`Rc<RefCell>`, `Cell<u32>` refcount — single-thread ABI); `Instance` retains
  its `Module` (UAF fix). Build in tested slices mirroring wazmrt: int/float/control → memory/globals →
  call/call_indirect (multi-table) → reference types + table ops → bulk mem/table → **full WasmGC** →
  **SIMD (full 0xFD + relaxed)** → **multi-memory** → **threads/atomics (0xFE)** → **memory64** →
  **exception handling (exnref + legacy)**. Gate per slice: golden-vector parity Rust↔wazmrt + the
  relevant `.wast` files. `[ ]`
- **T6 — Text toolchain (`sexpr` → `wat` → `wast`).** S-expr parser → WAT→binary assembler (reuse the
  opcode table in reverse) → WAST script runner. Gate: run the official spec testsuite; match wazmrt's
  ~60k-assertion pass profile (the assembler has no gaps in the oracle — hold that bar). `[ ]`
- **T7 — WASI preview 1 + CLI.** Native host imports (stdio/args/environ/clocks/`poll_oneoff`/random/
  `proc_exit`) + the sandboxed filesystem. ⛔ *Decision-gates:* **`random_get`** (parity ≈ wazmrt's
  ChaCha CSPRNG) and **zero-dep vs `cap-std`/`openat2`** for the secure path resolver (`walkFull`
  equivalent; close wazmrt's #17 TOCTOU cleanly). Gate: **wasi-gate** compiles real Zig/C/Rust guests and
  matches stdout; run the wasmtk WASI corpus (400 runnable) to wazmrt's result. `[ ]`
- **T8 — `wasmrt.h` C ABI (redesign, not transliteration).** ⛔ *Decision-gate (block lifted):*
  **finalize `wasmrt.h`** with the owner first (naming, store simplification, `{id}`-handle model) —
  from `docs/port/wasmrt.h.draft`. Then implement `#[no_mangle] extern "C"` over core: lightweight `{id}`
  handles (no wasm-c-api refcount model), caller-based host callbacks, `#[repr(C)]` boundary structs, real
  pointers. Gate: `tests/c_smoke.c` (decode→instantiate→call→memory→global→trap) + `tests/abi_symbols.c`
  link-completeness + lifecycle fuzz under **Miri**. `[ ]`
- **T9 — Licensing, docs, size, and all gates green.** Add `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/
  `third_party/LICENSES.md` (name wazmrt→wasmrt, "Jon Marcum" 2026); minimize every artifact (`opt-level=z`
  + LTO + `codegen-units=1` + strip + `wasm-opt -Oz`); cold-vs-steady bench. Gate: DoD below. `[ ]`

**Extended proposal — the one oracle-split residual:** **tail calls** (`return_call`/`return_call_indirect`)
have no wazmrt oracle → conform against **wasmtime + the official spec testsuite**. Slot this into T5/T6
alongside the rest; everything else parity-tests directly against the frozen wazmrt.

**Definition of done** — full Rust↔oracle parity on **both** targets (native + `wasm32`), all gates green
(spec testsuite, wasi-gate, c-smoke, Miri fuzz), size minimized, and the drift monitor clean (or the
oracle re-baselined deliberately).

## Loader integration phases (parallel track — see `loaders.md`)

- **Phase 1:** c, v, zig (`wasmrt.h`) + rs (native Rust crate) — primary early validation.
- **Phase 2:** dotnet, py (thin bindings over `wasmrt.h`).
- **Phase 3:** go (was wazero), jvm (was Chicory).
- **Phase 4:** js, dart (wasmrt-compiled-to-wasm, wasm-in-wasm).
