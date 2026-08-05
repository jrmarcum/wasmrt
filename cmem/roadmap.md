# Roadmap

## Status (2026-07-27) — PORT phase; gate OPEN, oracle FROZEN

The conversion has **begun**. The `wazmrt` oracle reached full parity and is **frozen** at
`wazmrt@dadc727`; `zig build test` passes (489/493, 4 skip; Debug + ReleaseSafe green) — gate verified
2026-07-27. `scripts/check-wazmrt.sh` now watches for **oracle drift**, not freeze-readiness. The oracle
covers **every wasm proposal wasmrt targets except tail calls** (`return_call`/`return_call_indirect`),
so the oracle split has collapsed to that one item (see `design-decisions.md`, `testing.md`). **memory64
is in scope** (owner, 2026-07-27). **T0–T6 ALL DONE.** T5's ten interpreter slices (v0.6.0 integer →
v0.6.9 exception handling) completed the interpreter's wasm-proposal coverage; **v0.7.0 then finished T4
(the deferred SIMD/atomic/GC/EH validation arms) and all of T6 (the text toolchain) together.** wasmrt
now assembles, decodes, type-checks and runs WebAssembly. **ALL of T7 is DONE (2026-08-05), published as v0.8.0** — host imports, module linking on the shared store, and WASI preview 1 including the sandboxed
filesystem — together with the **safety pass** (`#![forbid(unsafe_code)]` in core and the CLI) and the
**literal/text edges**, scoring **98.8% on the official spec testsuite** (61,013 / 751 / 3,094) with all
284 files parsing. The pre-T8 known-issues review is **done** (`known-issues.md`) and the resolver's
TOCTOU question is **decided** (accept + document). **Next: T8 — the `wasmrt.h` C ABI**, whose
decision-gate is finalizing the header shape with the owner before any code is written.

## ✅ v0.7.0 — SHIPPED and published (2026-08-03)

**v0.7.0 = the text toolchain (T6) + the validator's deferred typing arms (completing T4).** Live on
crates.io; tag `v0.7.0` pushed. Commits: `8a37795` validator arms · `5e82d08` sexpr · `5fd58bd` opcode
name map · `523ad3d` assembler core · `6d8d56c` floats + block types · `4e4b4de` SIMD/atomics ·
`0a7dc7e` GC/EH text forms · the `.wast` runner · the conformance runner + its findings.

Conformance at the v0.7.0 tag was **98.4%** (54,509 / 871 / 9,608). **v0.8.0 takes it to 98.8%**
(61,013 / 751 / 3,094), with **all 284 files parsing for the first time** — see the T7 entry below and
`cmem/testing.md` for the three-column history and why the middle column dipped.

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
- **T4 — `validate`. ✅ DONE — core 2026-07-28 (v0.5.0), exotic arms 2026-08-03 (v0.7.0).** The spec §3 validation
  algorithm (value + control-frame stacks, `unknown` bottom), `subtype_of`, `simple_sig`, module-level
  checks (count-match, const-expr typing, elements/data, limits §3.2.5, tags, dup-exports, start,
  `C.refs`), local-init tracking, `natural_align_log2` (the deferred T2 table), per-memory (memory64)
  address typing. **Owner decision (2026-07-28):** land the **core language** now (MVP/refs/bulk-memory/
  tables/i31 — hand-testable), **defer SIMD/atomics/GC-objects+casts/EH typing to 0.5.x** (real coverage
  at T6 via the spec suite). Deferred ops reject loudly (`UnsupportedValidation`), never silent-accept.
  CLI prints a validation verdict. 54 core tests, clippy clean, native + `wasm32` no_std green.
  **Gate note:** `assert_invalid`/`assert_malformed` spec-suite parity is the T6 gate (needs the `.wast`
  runner); at T4 the gate is the ported oracle hand-vectors + no over-acceptance on the core set. `[◐]`
  - **0.5.x follow-up — ✅ DONE 2026-08-03 (v0.7.0), landed after T5 rather than before it.** All four
    deferred arms ported from wazmrt `validate.zig`: **`simd_sig`** (the whole `0xFD` fixed-width +
    relaxed signature table) with the checks the stub had skipped — `require_memory`, memarg index range,
    alignment ≤ `simd_natural_align_log2`, and the memory64 address type; **atomic typing** (notify/wait/
    load/store/rmw/cmpxchg) where alignment must be **exactly** natural, not a maximum; **GC typing**
    (struct/array new/get/set/len, `ref.test`/`ref.cast`, `br_on_cast`/`_fail`) popping the **concrete**
    ref type, never the family head, so `struct.get $b` can't be applied to a `(ref $a)`; and **EH typing**
    (`try_table` + per-clause label checks via `check_catch`, `throw`, `throw_ref`, legacy
    `try`/`catch`/`catch_all`/`rethrow`) with `delegate` **rejected** to match the oracle. Also added the
    `simd_natural_align_log2` / `simd_is_memory_op` / `atomic_natural_align_log2` tables to `opcode.rs`
    (the T2 deferral) and `FuncValidator.body_len` to bound `array.new_fixed`'s unvalidated `n` (in
    unreachable code `pop_expect` yields `Unknown` instead of underflowing, so an unbounded loop could
    spin 2^32 times on a tiny module). **T4 is now COMPLETE** — the validator covers everything the
    interpreter runs, and `wasmrt <file>` never prints "validation SKIPPED". 135 core tests (12 new),
    clippy clean. Full `assert_invalid`/`assert_malformed` conformance remains the **T6** gate. `[x]`
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
  - **Slice 9 — memory64. ✅ DONE 2026-08-03 (v0.6.8).** **Already-built infrastructure, now
    conformance-tested + matrix-flipped** (the second such slice, after multi-memory). The 64-bit plumbing
    landed with linear memory (0.6.2): `Limits.is64` (flag bit 2) decoded with `u64` min/max, the `memarg`
    offset read as a full `u64`, `validate`'s `mem_addr_ty`/`check_mem_offset` typing each address per
    memory, and the interpreter's `pop_mem(is64)` on every address/count. This slice proved it with **18
    vectors** — 12 execution (i64 store/load, i64 `memory.size`/`grow` + grow-past-max → −1, i64 active
    data offset, **address 2^32 traps instead of wrapping**, `memarg` offset > `u32`, i64 bulk
    `fill`/`copy`, i64 `memory.init` dst, i64 addresses through the `0xFE` atomic and `v128` families,
    huge declared minimum → `MemoryLimitExceeded`, mixed 64/32-bit `memory.copy`) and 6 validator (i32
    address rejected / i64 accepted, `memory.size` → i64, offset > `u32` rejected on a 32-bit memory,
    data-offset index type, limits above the 2^48 ceiling, `i64` table type malformed). **Scope
    boundary held: tables stay 32-bit-indexed** — the proposal's 64-bit *table* extension has no oracle
    (wazmrt `read_table_type` rejects it), so wasmrt rejects it identically. Verified end-to-end through
    the CLI on a real memory64 module (`validation OK` → correct i64 result). No new engine code;
    112 core tests, clippy clean, native + wasm32 no_std. `[✅]`
  - **Slice 10 — exception handling. ✅ DONE 2026-08-03 (v0.6.9).** **Real new engine code** (unlike
    slices 7 and 9): the whole EH runtime, ported from wazmrt `interp.zig` `throwException`/`onCallError`.
    **Both encodings.** *exnref:* `try_table` with all four clause kinds (`catch`/`catch_ref`/`catch_all`/
    `catch_all_ref`), `throw`, `throw_ref`. *Legacy:* `try`/`catch`/`catch_all`/`rethrow`. New data model:
    `Exception { tag, values }`; `Store.exn_store` (boxed exceptions — an `exnref` value is an index,
    bounded by `MAX_EXN_BOXES`) + `Store.pending_exn` (an exception in flight across frames); `FuncBody
    .try_info` (each legacy `try`'s handlers, collected by `precompute_control_flow` on the same opener
    stack that matches `end`s); `Label` gained `try_table_pc`/`legacy_pc`/`caught`. Two new traps —
    `UncaughtException`, `ExnStoreExhausted` — plus `UndefinedTag`.
    **The load-bearing asymmetry:** a `try_table` clause branches **out of** the try_table to its target
    label (so the label is popped), while a legacy `catch` runs **inside** the try, whose label stays live
    for `rethrow`. Two guards fall out of that: a `throw` from inside a legacy handler must propagate
    outward, not re-match its own handler (else the `catch (e) { throw e; }` idiom loops forever — the
    `caught.is_some()` skip), and `rethrow` pops the try *before* re-raising.
    **`delegate` is rejected — oracle-faithful.** `delegate l` re-raises "at label l", routing wazmrt does
    not implement and its validator refuses; reaching a delegating try while unwinding traps
    `UnsupportedInstruction` rather than silently mis-routing. **Idiomatic divergences:** exceptions are
    owned `Vec<Value>` (no arena); `Label` identifies its construct by **pc** rather than borrowing the
    immediate, so it stays cheap and free of a second body borrow; EH state resets per invocation.
    **Rust note:** `Label` lost `Copy` (it owns `caught`), so `branch` reads its scalars out first.
    11 EH vectors: try_table catch, uncaught throw, catch_all binds nothing, unwind across a call,
    catch_ref→throw_ref round-trip, legacy try/catch, legacy catch_all, legacy rethrow, throw-from-handler
    escapes, delegate traps, no state leak between invocations. Verified end-to-end via the CLI.
    123 core tests, clippy clean, native + wasm32 no_std. **EH *typing* stays deferred to the 0.5.x
    validator arm** (per-slice precedent: SIMD/atomics/GC also landed exec-first); `wasmrt <file>` prints
    `validation SKIPPED` for an EH body. `[✅]`
  - **0.6.x remaining work — RESOLVED.** The deferred validation arms (incl. EH typing, which rejects
    `delegate` as the oracle does) landed in v0.7.0. Host imports remain, and move to **T7**.
    Original T5 detail:
- **T5 (original spec) — `interp`.** Untyped `u64` slots; the slot-encoding order invariant
  (`null_ref` before `i31_tag`); `#[cold]`/`#[inline(never)]` trap path with lazy byte-offset resolve;
  shared `Memory`/`Table` (`Rc<RefCell>`, `Cell<u32>` refcount — single-thread ABI); `Instance` retains
  its `Module` (UAF fix). Build in tested slices mirroring wazmrt: int/float/control → memory/globals →
  call/call_indirect (multi-table) → reference types + table ops → bulk mem/table → **full WasmGC** →
  **SIMD (full 0xFD + relaxed)** → **multi-memory** → **threads/atomics (0xFE)** → **memory64** →
  **exception handling (exnref + legacy)**. Gate per slice: golden-vector parity Rust↔wazmrt + the
  relevant `.wast` files. `[ ]`
- **T6 — Text toolchain (`sexpr` → `wat` → `wast`). ✅ DONE 2026-08-03 (v0.7.0), at 98.4% of the suite.** S-expr parser →
  WAT→binary assembler (reuse the opcode table in reverse) → WAST script runner. Gate: run the official
  spec testsuite; match wazmrt's ~60k-assertion pass profile (the assembler has no gaps in the oracle —
  hold that bar). **Scale note: the oracle's text toolchain is 6,465 lines** (`sexpr.zig` 276, **`wat.zig`
  4,936**, `wast.zig` 1,253) — `wat.zig` rivals `interp.zig` (5,049), which shipped across ten releases.
  Built in layers; each is committed separately. `[🚧]`
  - **T6a — `sexpr`. ✅ DONE (commit `5e82d08`).** Lexer/parser over `.wat`/`.wast`: atoms (raw source
    text), strings (escapes decoded to real bytes), lists; `;;` + nestable `(; … ;)` trivia. Hardened as
    the oracle is: `MAX_DEPTH` paren-bomb cap, **a lone `;` is a hard error** (trivia consumes only `;;`
    and `(;`, and atom scanning treats `;` as a terminator, so it would yield an EMPTY atom without
    advancing — the wazmrt regression where `(module) ; x` hung the CLI at 10 GB RSS), and
    **overflow-checked `\u{…}`** so `\u{100000041}` is rejected rather than truncated mod 2^32 to `'A'`.
    Errors carry a byte offset. 10 tests.
  - **T6b-1 — the opcode text-name table. ✅ DONE (commit `5fd58bd`; this was the deferred T2 reverse
    map).** wazmrt gets name→op free from Zig's `stringToEnum` over snake_case variants; wasmrt's are
    PascalCase, so the map is explicit. It lives **inside `define_ops!`** beside the byte
    (`Unreachable = 0x00 => "unreachable"`), generating `Op::text_name()` + `Op::from_text_name()`, so the
    binary and text spellings are one authority and cannot drift. Two details: **`SelectT` carries the
    sentinel name `"select.t"`** (both select variants would otherwise claim `"select"` and make the
    second match arm unreachable), and the `0xFD`/`0xFE` family tags carry `""` (their members are named
    per sub-opcode) and are excluded from the reverse map. A round-trip test pins every single-byte op.
  - **T6b-2/3 — the assembler core. ✅ DONE (commit `523ad3d`).** Multi-pass, because the text format lets
    names point forward: pre-pass A collects every `(type …)` name (a concrete `(ref $t)` may name a type
    declared later — a `(rec …)` group routinely does), pre-pass B parses the bodies, pass 1 walks
    definitions filling the per-kind index spaces (imports must precede definitions §6.6.13), pass 2
    resolves module-level `(export …)` forms (which may name something declared further down — exactly
    what binaryen emits; resolving in-pass reports `UnknownIdentifier` on a good module). Covers all
    module fields, inline import/export clauses, the `(memory (data …))` / `(table … (elem …))`
    shorthands, memory64 + shared limits, sections 1–13, flat **and** folded instruction forms, named
    labels, `br_table`, `call_indirect`, bulk-memory/table families, memargs, `ref.null` heap types.
    **Key encoding rule: in a folded instruction the immediates are the leading ATOMS and the operands are
    the parenthesized sub-expressions after them** — folded operands are always parenthesized, so the
    atom/list split IS the immediate/operand split, covering fixed-arity immediates and the variable
    `offset=`/`align=` memarg atoms through one rule. **Traps to remember: `table.init`'s text order is
    `$table $elem` but its binary order is elem-then-table; `memory.init` likewise emits data-then-memory.**
  - **T6b-4 — float literals + multi-value block types. ✅ DONE (commit `6d8d56c`).** **Rust parses no hex
    floats at all**, so `0x1.abcp+3` (and the exponent-less `0xABC` form) is parsed here — with **correct
    rounding**, because truncating a long hex mantissa emits a constant **one ULP low**: a *wrong value*,
    not a rejected one, so the same number in decimal and hex would compile to different modules (the
    oracle found this on `simd_f64x2_rounding.wast`; its vectors are tests here). The significand
    accumulates into a `u128` with a sticky bit and rounds to the target ULP in **one step**, the ULP
    exponent being the coarser of the normalised one and the smallest subnormal's — that single `max`
    makes normal/subnormal/below-subnormal one path; rounding in two stages discards the sticky bit and
    flushes `0x1.8p-1075` to zero instead of rounding up. Plus `nan:canonical`/`nan:arithmetic`/
    `nan:0x<payload>`. The parser is `pub(crate)` so the `.wast` runner shares it — **one authority for
    what a literal means**, so an expectation and the module it checks can never disagree.
    Multi-value block types forced a **reordering: all bodies and const-exprs are now encoded BEFORE any
    section is written**, since interning a block signature appends to the type table (the oracle's order
    too). That also let `call_indirect` intern an inline signature. `Ctx` borrows the name tables
    field-by-field alongside a mutable `sigs`; disjoint field borrows make interning-during-encoding sound.
  - **T6b-5 — SIMD / atomics / GC / EH text forms. ✅ DONE (commits `4e4b4de`, `0a7dc7e`).** The `0xFD`
    name table (~230 entries) with each op's immediate shape; the `0xFE` names **generated** from their
    layout (groups of 7 from `0x1e`, the same layout `atomic_natural_align_log2`/`atomic_val_type`
    encode — described once, so the three cannot drift, and the non-existent spellings fall out as
    errors for free). GC: the type table became `Vec<TypeDef>` (func | struct | array) with parallel
    supertype and field-name tables, so `(sub …)`, packed `i8`/`i16` and `struct.get $T $field` by name
    all work. EH: `try_table` in both forms with all four clause kinds, and the folded legacy
    `(try (do …) (catch …))`. **`delegate` is rejected here too**, so assembler, validator and
    interpreter agree. **Gotcha:** the cast ops take a **list** immediate `(ref null? ht)`, which the
    folded-form atom/list rule mistook for an operand — they join `call_indirect` as forms whose leading
    lists are immediates. `[x]`
  - **T6c — the `.wast` script runner. ✅ DONE (commit `0a7dc7e`+).** All the assert commands, the
    invoke/get/register actions, named modules, and the `binary`/`quote` module forms. **The honesty
    rule is the design:** never count "we couldn't build it" as a pass — `assert_invalid` needs a
    *validation* rejection and `assert_malformed` a *decode/parse* one, while an assembler gap is a
    **skip**; skips are reported separately so the conformance number cannot be inflated by what we
    don't handle. Shares the assembler's float parser, so an expectation and the module it checks can
    never disagree about a literal. **Payoff from the u128 slot:** a `v128` result is ONE slot, so the
    arity check compares directly — the oracle needs a slot-vs-form adjustment that had silently
    disabled every SIMD assertion in its suite until found. `[x]`
  - **T6 gate — the conformance run. ✅ DONE.** `wasmrt wast <file|dir>…` walks the vendored testsuite.
    **First run 96.7% → 98.4% after four fixes** (54,509 passing / 871 failing / 9,608 skipped, 284
    files). The prediction held: the suite found bugs the hand vectors could not. **What it caught:**
    (1) a **panic** — `v128.const i64x2` fell into an `unreachable!()`, aborting the run; a library must
    reject a module, never abort the embedder, so that catch-all is now a rejection; (2) **element
    segments emitted in a form no decoder can read** — the eight flag forms split into two families
    (0–3 elemkind byte + bare func indices, 4–7 reftype + const-exprs) and we mixed them, so every
    `table_copy`/`table_init` module failed to build; (3) **out-of-range constants truncated instead of
    rejected** (`(i32.const 0x100000000)` quietly became 0); (4) **digit separators accepted anywhere**
    (`_` is legal only *between* digits). All four are pinned by regression tests. `[x]`
- **T7 — Host imports + WASI preview 1 + CLI. ✅ DONE (2026-08-05, v0.8.0). GATE MET:** the wasmtk WASI
  corpus runs — **426/441 stdout byte-identical to the frozen oracle, and 441/441 identical in
  engine-observable behaviour** (the 15 diffs are CLI shape: 12 modules have no `_start`, 3 are
  deliberate-throw tests). The `.wat` corpus assembles 532/534. Numbers + method in `cmem/testing.md`;
  the 5 open defects it surfaced are in `cmem/known-issues.md`. Original gate text: **wasi-gate** compiles
  real Zig/C/Rust guests and matches stdout; run the wasmtk WASI corpus (400 runnable) to wazmrt's
  result. `[🚧]`
  - **✅ Both decision-gates RESOLVED (owner, 2026-08-04) — both the zero-dependency option, which keeps
    the safety directive and the smallest-binary goal intact:**
    - **`random_get` = a ChaCha20 CSPRNG seeded once from the OS** (oracle parity). No dependency, no
      `unsafe`, auditable, and it still works on the freestanding `wasm32` self-embed target where a
      syscall has nothing to call. **If OS entropy is unavailable, fail loudly — never emit predictable
      bytes**, which is the one failure mode that turns a CSPRNG into a security hole.
    - **Secure path resolver = the zero-dep handle-stack walker** (wazmrt's `walkFull` design): resolve a
      path **component-by-component against open directory handles, never re-opening by full path**, so
      there is no TOCTOU window to lose — that is what closes wazmrt's **#17** by construction rather
      than by checking. Rejected `cap-std`/`openat2`: a large dependency tree against the smallest-binary
      goal, strongest on Linux and uneven elsewhere, and it would be wasmrt's first runtime dependency.
  - **T7a — host imports. ✅ DONE (commit `981c6b6`).** `Instance::new_with_imports` links host backings
    per kind in declaration order; `HostFunc` is a **boxed closure, not a fn-pointer + `void*` ctx** —
    the context-pointer shape cannot be expressed without `unsafe`, so this is the safety directive's
    first real application, and the C ABI (T8) will need the same treatment. `Caller` gives
    **bounds-checked** `read`/`write`/`memory_len` over guest memory (every accessor returns `Option`, so
    a wild guest pointer yields `None` rather than panicking the embedder). **Imported functions occupy
    the LOW function indices**, so `call_function` subtracts the import count — a test pins that `call 0`
    reaches the host rather than re-entering the export. Imported globals precede defined ones, so a
    defined initializer may read an imported global. **Imported memories/tables reject loudly**
    (`UnsupportedImportKind`) pending T7b's shared-ownership model. `[x]`
  - **T7b — module linking** (wasm→wasm imports; shared memories/tables) **+ a `spectest` provider in the
    `.wast` runner**. **✅ DONE (2026-08-05).** Built on the **shared-store model** (owner approved
    2026-08-05: *"Proceed with the shared store. I agree with doing it like wasmtime."*). `Store` owns
    `code: Vec<InstanceData>` + `pools: Pools` as **separate fields**, so a cross-instance call borrows
    two disjoint pieces — no `Rc`, no `RefCell`, no `unsafe`, and no borrow check on the hot path. Each
    instance holds an `IndexMaps` from its own index space into the pools; `IndexMaps::get` returns
    `usize::MAX` out of range, so a bad index **traps instead of aliasing another instance**. `Instance`
    survives as a thin `{ store, id }` wrapper, so the single-instance API is unchanged. `Imports` keeps
    host and wasm backings in **one ordered vector** — two vectors silently reordered any module that
    mixed `(import "spectest" …)` with `(import "a" …)`. `[x]`
    - **The refactor's own defect class, found and fixed 2026-08-05:** with every instance sharing one
      pool, any site that indexes a pool with a raw module-local immediate reads *another module's*
      resource — and it is **invisible under one instance per store**, where the two indices are equal.
      Three such sites: `Op::CallIndirect` (raw `ci.table`), `exec_memory_init` (store index into
      `ctx.module.data`), plus the assembler's missing `table.copy`/`table.init` index shorthands.
      **Suite: 1,521 failing → 851; 97.4% → 98.6%** (59,261 / 851 / 4,720). Clippy's `unused variable:
      maps` caught three of these; the rest needed a hand audit, because `ci.table` is a different
      expression from `table_imm(instr)` and no lint can see the difference. Full write-up +
      the standing **two-instance rule** for tests: `cmem/known-issues.md`, `cmem/testing.md`.
    - **⚠️ Measured correction (2026-08-04): host imports alone did NOT move the conformance skips** —
      9,608 → 9,605 after T7a. The earlier claim that T7a would "unblock most of the skips" was wrong,
      and the measurement is what caught it. T7a gave the *engine* the capability; the **`.wast` runner
      has no linking layer using it** — it still calls `Instance::new(md)` with no imports, so any module
      with imports fails to build and is skipped.
    - **What the skips actually need**, counted across the suite: `(import "spectest" …)` **174** — the
      spec's standard host module (`print*`, `global_i32/i64/f32/f64`, `memory`, `table`); and
      `(import "a"/"test"/"reexport_f" …)` **362** — modules published by `(register …)`, i.e. genuine
      wasm→wasm linking. So the unblock needs **both** a `spectest` provider *and* module linking, not
      host imports alone.
    - **Design note for the linking work:** sharing a memory/table between instances cannot use the
      oracle's `*Memory` raw pointer (safety directive). `Rc<RefCell<Memory>>` would put a borrow check
      on the interpreter's hottest path. The architecturally right answer is the **shared-store model**
      (what wasmtime does): move memories/tables/globals into one `Store` that all linked instances index
      into, so instances hold base offsets rather than owned resources. The interpreter already threads
      `&mut Store` everywhere, so this fits the existing shape — but it is a deliberate refactor of
      `Instance`, not an incremental add. Decide before starting.
  - **T7c — WASI preview 1. ✅ DONE (2026-08-05).** T7c-1: stdio, `args_*`, `environ_*`, clocks,
    `random_get` (ChaCha20), `proc_exit`. **T7c-2: the sandboxed filesystem** — `wasi/fs.rs` carries the
    fd table, the rights lattice, the resolver, and ~20 `fd_*`/`path_*` calls; the CLI gained
    `--dir <host>[::<guest>]` / `--ro-dir`. **With no `--dir` every path call is `BADF`** — there is no
    implicit cwd, so the default is total denial. `[x]`
    - **⚠️ The 2026-08-04 resolver decision rested on a FALSE premise, corrected 2026-08-05:**
      `cmem/security-model.md` had claimed Rust's `std` could do the atomic no-follow open. **It cannot
      — `std` has no dir-relative open on any platform** (no `openat`, no re-openable `O_PATH` handle;
      verified against the 1.99-nightly sysroot). `symlink_metadata` is a no-follow *stat*, which does
      not pin an inode. So **zero-dep + no-`unsafe` + hold-real-handles cannot all be true**; the port
      keeps the first two and accumulates a path instead. **Every escape property is unaffected** (they
      are lexical: `..` cannot pop below the bottom, absolute targets re-base to the preopen root,
      symlink targets go through the same loop, `SYMLINK_MAX` bounds cycles); **only inode pinning is
      lost**, which needs a second process writing inside the sandbox to exploit. `verify_beneath`
      re-checks the canonical result against the canonical root as a compensating control. **Open
      decision for the owner at T8/T9:** spend `cap-std` or a narrow audited `unsafe` shim, or accept
      and document. Full argument in `cmem/security-model.md`.
    - **The mandated canary test is real and bites:** it asserts *no walk can produce a path that reads
      the canary* — the outcome, not an errno — over absolute, relative, chained, and symlinked-directory
      escapes. Mutation-checked: deleting the `..` guard fails it. End-to-end through the CLI, a guest
      granted one directory reads its own file and gets **76 `NOTCAPABLE`** for `../canary.txt`.
    - Deliberately **`NOSYS`, not a silent success**: `fd_allocate`, `fd_filestat_set_times`,
      `path_filestat_set_times`, and the socket calls. A guest that needs them must learn we did not
      do them. 272 workspace tests (was 254), clippy clean.
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
