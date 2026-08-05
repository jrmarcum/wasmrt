# Changelog

All notable changes to wasmrt are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and wasmrt uses a **port-progress**
versioning scheme: `0.x` releases climb toward **1.0.0 = full parity** with the
[`wazmrt`](https://github.com/jrmarcum/wazmrt) reference oracle. See [ROADMAP.md](ROADMAP.md) for the
stage ladder and the live use-case matrix.

The three crates share one version and are released together: `wasmrt` (CLI), `wasmrt-core` (library),
`wasmrt-capi` (C ABI).

## [Unreleased]

_Next: T8 — the `wasmrt.h` C ABI._

## [0.8.0] — WASI preview 1, host imports and module linking (stage T7)

wasmrt can now **run real compiled programs**. This release adds host imports, module-to-module
linking, and WASI preview 1 including the sandboxed filesystem — and makes the project's no-`unsafe`
rule mechanical rather than aspirational.

### Added — running real programs

- **Host imports** (`Instance::new_with_imports`). A module's declared imports link to host backings in
  declaration order. `HostFunc` is a **boxed closure**, not a function pointer plus a `void*` context —
  the context-pointer shape cannot be expressed without `unsafe`. `Caller` gives hosts **bounds-checked**
  `read`/`write`/`memory_len` over guest memory, so a wild guest pointer yields `None` rather than
  panicking the embedder.
- **Module linking on a shared store.** Resources are owned once by a `Store`; each instance holds an
  index map into it. A cross-instance call borrows two disjoint fields, so there is no `Rc`, no
  `RefCell`, no `unsafe`, and no borrow check on the interpreter's hot path. `Instance` is unchanged for
  single-module use.
- **WASI preview 1** (`wasmrt wasi <file.wasm> [args…]`): stdio, `args_*`, `environ_*`, clocks,
  `proc_exit`, and `random_get` backed by a **ChaCha20 CSPRNG seeded from the OS** — which fails loudly
  if the OS will not supply entropy rather than emitting predictable bytes.
- **A sandboxed filesystem**, reached only through explicit preopens: `--dir <host>[::<guest>]` and
  `--ro-dir` for read-only, which propagates to the whole subtree. **With no `--dir`, every path call
  returns `BADF`** — there is no implicit working directory. Roughly twenty `fd_*` / `path_*` calls,
  a rights lattice, and a resolver in which the escape properties hold *by construction*: `..` cannot
  rise above the preopen, an absolute symlink target re-bases to the preopen root, symlink targets are
  expanded through the same loop that vets the original path, and device/NT-namespace components are
  refused before the filesystem is touched. Calls that are not implemented report `NOSYS` rather than a
  silent success.

### Added — safety

- **`#![forbid(unsafe_code)]`** in `wasmrt-core` and the CLI; `#![deny(unsafe_code)]` in `wasmrt-capi`
  with a single documented exception for the `#[unsafe(no_mangle)]` a C export requires. Both crates
  already carried zero `unsafe`; the lints make the next one a compile error.

### Fixed

- **`call_indirect` read the wrong module's table** when several modules shared a store, because it
  indexed the store with a module-local index. `memory.init` had the mirror-image defect. Together these
  accounted for over 500 spec-suite failures.
- **A folded `br_table` emitted no label vector at all** — the assembler reported success for bytes no
  decoder could read.
- **`(data "a""b")` was accepted and silently concatenated** to `ab`. Tokens must now be separated by
  whitespace or a parenthesis.
- **Float literals that overflow are rejected** instead of quietly becoming infinity; only `inf` and
  `nan` may denote a non-finite value. The float grammar now follows the spec rather than Rust's parser
  (`.0` is malformed, `1.` is not), digit separators must sit between two **digits** (`0x_1` and `1_e1`
  were accepted), and a NaN payload is range-checked rather than masked into a different NaN.
- **`v128.const` is a constant expression.** The interpreter had evaluated it since 0.6.5 while the
  validator rejected it, so a module with a `v128` global was refused despite running correctly.
- **`table.copy` / `table.init` accept their index shorthands** (both indices may be omitted).
- **Quoted identifiers** (`$"a b"`) parse, and **annotations** (`(@id …)`) are skipped as the annotations
  proposal requires of a tool that does not implement them.

### Conformance

**98.8%** of the official WebAssembly spec testsuite — 61,013 passing, 751 failing, 3,094 skipped across
284 files, up from 98.4% (54,509 passing) at 0.7.0. **Every one of the 284 files now parses.** Skips are
never counted as passes: a construct this build cannot put to the test is reported separately.

## [0.7.0] — Text toolchain (stage T6) + the validator's deferred typing arms (completes stage T4)

Two stages in one release: wasmrt can now read and write the **WebAssembly text format**, run the
official **spec testsuite**, and type-check every construct it executes.

### Added — the text toolchain (stage T6)

- **`.wat` assembler** (`wasmrt::wat`, and `wasmrt wat <file.wat> [-o out.wasm]`). Assembles the text
  format to a binary: every module field, inline import/export clauses, the `(memory (data …))` and
  `(table … (elem …))` shorthands, sections 1–13, and both the flat and folded instruction forms.
  Multi-pass name resolution means names may point forward — a concrete `(ref $t)` can name a type
  declared later, and a module-level `(export …)` can name a function declared further down (the order
  binaryen emits).
- **Full opcode coverage**: the core language, bulk memory and table ops, **SIMD** (the whole `0xFD`
  fixed-width + relaxed set, including `v128.const`, shuffles and lane-carrying loads/stores),
  **atomics** (`0xFE`), **WasmGC** (`(type (struct …))` / `(array …)` / `(sub …)` definitions with
  packed `i8`/`i16` fields, struct fields resolvable **by name**, and the cast ops), and **exception
  handling** in both encodings.
- **Correctly-rounded float literals**, including hexadecimal (`0x1.abcp+3`) and the wasm
  `nan:canonical` / `nan:arithmetic` / `nan:0x…` spellings. Rust parses no hex floats at all, and a
  truncating parser emits a constant one ULP low — a *wrong value*, not a rejected one, so the same
  number written in decimal and in hex would compile to different modules.
- **`.wast` script runner** (`wasmrt::wast`, and `wasmrt wast <file|dir>…`): `assert_return`,
  `assert_trap`, `assert_exhaustion`, `assert_invalid`, `assert_malformed`, `assert_unlinkable`, plus
  the `invoke` / `get` / `register` actions, named modules, and the `binary` and `quote` module forms.
- **`wasmrt wast <dir>`** walks a testsuite and prints a pass profile with the worst files.

### Added — the validator's deferred typing arms (completes stage T4)

- **SIMD**, **atomics**, **WasmGC** and **exception-handling** typing. The validator now covers every
  construct the interpreter executes, so `wasmrt <file.wasm>` never reports `validation SKIPPED`.
  Atomics must be **exactly** naturally aligned (unlike a scalar or SIMD access, where a smaller
  alignment is a valid hint); struct and array references are checked at their **concrete** type rather
  than the family head, so `struct.get $b` cannot be applied to a `(ref $a)`.

### Conformance

First run against the official spec testsuite (284 files): **98.4% — 54,509 assertions passing**, 871
failing, 9,608 skipped. Skips are counted and reported **separately from passes** on purpose: a
construct this build cannot put to the test is not a pass, and most of them are modules needing host
imports, which arrive at T7. The remaining failures are tracked in the project's known-issues list.

### Notes

- **`delegate` is rejected by the assembler, the validator and the interpreter alike**, matching the
  frozen oracle. Assembling it would produce a module that type-checks yet silently mis-routes at run
  time.
- A **debug** build can overflow the native stack before the 512-frame recursion cap fires (release is
  correct). The cap deliberately matches the oracle rather than being tuned to one profile; see the
  project's known-issues list.

### Added
- **SIMD typing** (the `0xFD` family): a full signature table for the fixed-width and relaxed sets, plus
  the checks the deferred arm had skipped entirely — a memory-touching SIMD op now requires a memory to
  exist, its `memarg` memory index to be in range, its alignment to be within the op's natural maximum,
  and (memory64) its address operand to take the memory's own index type.
- **Atomics typing** (`0xFE`): notify/wait, atomic loads/stores, RMW, and cmpxchg. Atomics must be
  **exactly** naturally aligned — unlike a scalar or SIMD access, where a smaller alignment is a valid
  hint — so the validator enforces equality, not a maximum.
- **WasmGC typing**: `struct.new`/`new_default`/`get`/`get_s`/`get_u`/`set`, `array.new`/`new_default`/
  `new_fixed`/`get`/`get_s`/`get_u`/`set`/`len`, `ref.test`/`ref.cast`, and `br_on_cast`/`br_on_cast_fail`.
  Struct and array references are checked at their **concrete** type rather than the family head, so
  `struct.get $b` cannot be applied to a `(ref $a)` and reinterpret one field type as another. Packed
  `i8`/`i16` fields require the sign-aware `*_get_s`/`*_get_u`; immutable fields reject `*.set`.
- **Exception-handling typing**: `try_table` (each catch clause checked against its target label's types),
  `throw`, `throw_ref`, and the legacy `try`/`catch`/`catch_all`/`rethrow` frames. A legacy handler starts
  from the try's *entry* local-init state, since locals set in the body are not guaranteed on the path
  that arrived via a thrown exception — the same rule `else` follows.

With this, **stage T4 (validate) is complete**: the validator covers every construct the interpreter
executes. `wasmrt <file>` no longer reports `validation SKIPPED` for SIMD, atomic, GC, or EH modules.

### Notes
- **`delegate` is rejected by the validator too**, matching the frozen oracle — so the text and binary
  paths agree, and a module that validates is one the interpreter can actually run.
- Full `assert_invalid` / `assert_malformed` conformance against the official spec testsuite is the **T6**
  gate; this release's gate is the ported oracle typing rules plus per-arm vectors.

## [0.6.9] — Interpreter: exception handling (stage T5, slice 10)

### Added
- **Exception handling in both encodings.** The modern **exnref** encoding — `try_table` with all four
  clause kinds (`catch`, `catch_ref`, `catch_all`, `catch_all_ref`), `throw`, and `throw_ref` — and the
  **legacy** encoding — `try`/`catch`/`catch_all`/`rethrow`. Tags are resolved from the tag section, a
  thrown exception carries its tag's parameters as its payload, and `catch_ref`/`catch_all_ref`
  materialize an `exnref` that `throw_ref` can re-raise later.
- **Exceptions unwind across call frames**: an exception thrown in a callee is offered to each caller's
  handlers as it unwinds, and reaching the top of an invocation with no match surfaces as an
  uncaught-exception trap. A real trap (or an exception no handler matches) keeps unwinding untouched.
- Two new traps: **uncaught exception** and **too many live exception references** (the `exnref` box
  budget — there is no collector, matching the GC heap's treatment).

### Notes
- **`delegate` is rejected, matching the frozen wazmrt oracle.** `delegate l` re-raises "at label `l`",
  routing that can skip handlers an ordinary outward unwind would run; the oracle does not implement it
  and its validator refuses it outright. wasmrt matches: reaching a delegating `try` while unwinding
  traps loudly rather than silently mis-routing. Every other legacy construct is fully supported.
- The two encodings unwind differently, and the difference is load-bearing: a `try_table` clause branches
  **out of** the try_table to its target label, while a legacy `catch` runs **inside** the try, whose
  label stays live so `rethrow` can still name it. A `throw` from inside a legacy handler propagates
  outward rather than re-matching the handler it is already in.
- EH **typing** remains part of the deferred 0.5.x validator work — the validator reports EH bodies as
  unchecked rather than guessing, and `wasmrt <file>` prints `validation SKIPPED` for them. Execution is
  unaffected.

## [0.6.8] — Interpreter: memory64 (stage T5, slice 9)

### Added
- **memory64** — a module declaring a 64-bit-indexed linear memory (the limits `i64` flag) now runs end
  to end and is conformance-tested. Addresses, counts, and page counts carry the memory's own index type:
  loads/stores, `memory.size`/`grow`/`fill`/`copy`/`init`, active data-segment offsets, the `0xFE` atomic
  family, and the `v128` load/store family all take an `i64` address on a 64-bit memory. A `memarg`'s
  static offset may exceed `u32` on such a memory (a 32-bit memory still rejects that at validation), and
  a 64-bit memory may declare up to 2^48 pages.
- **Mixed-width `memory.copy`** — with one 64-bit and one 32-bit memory, each address keeps its own index
  type and the count is the narrower of the two (`i32`), per the proposal.

The 64-bit plumbing itself shipped with linear memory in **0.6.2** (the `is64` limits flag, `u64`
`memarg` offsets, per-memory address typing in the validator, and the interpreter's index-type-aware
address pop); this release proves it with 18 conformance vectors and lifts the use-case matrix cell.

### Notes
- **Tables stay 32-bit-indexed.** The memory64 proposal's 64-bit *table* extension is out of scope: the
  frozen wazmrt oracle rejects an `i64` table type as malformed, and wasmrt matches that byte for byte.
- A 64-bit memory may *declare* far more than this instance will back. The per-instance memory budget
  (1 GiB) still applies, so a huge declared minimum fails instantiation with a memory-limit error rather
  than attempting the allocation.

## [0.6.7] — Interpreter: threads / atomics (stage T5, slice 8)

### Added
- Execution for the **threads proposal's atomic operations** (the `0xFE` family): atomic loads/stores
  (`i32`/`i64` and the 8/16/32-bit widths), the read-modify-write ops (`add`/`sub`/`and`/`or`/`xor`/
  `xchg`) and `cmpxchg`, `memory.atomic.wait32`/`wait64`/`notify`, and `atomic.fence`. A `shared` memory
  (limits flag) is now tracked through instantiation.
- Two new traps: **unaligned atomic access** (an atomic's effective address must be naturally aligned to
  its width — stricter than ordinary loads/stores) and **`wait*` on a non-shared memory**.

### Notes
- **Single-threaded semantics** (matching the frozen wazmrt oracle): every atomic access is trivially
  atomic and `atomic.fence` is a no-op. `wait*` never blocks — a value mismatch returns `1` ("not
  equal"), a match returns `2` ("timed out", since no other thread can `notify`); `notify` always wakes
  `0`. This is a conforming implementation for a single-threaded engine; genuine multi-threaded execution
  is out of scope for the interpreter.

## [0.6.6] — Interpreter: multi-memory (stage T5, slice 7)

### Added
- **Multi-memory** — running a module with more than one linear memory is now conformance-tested and
  supported end to end: a load/store's `memarg` selects its memory (the alignment-flag `0x40` + memory
  index), `memory.copy` can move bytes **between two different memories**, `memory.size`/`grow`/`fill`/
  `init` act on their memory index, and an active data segment can target any memory (the flag-`0x02`
  explicit-`memidx` form). Instantiation creates every declared memory and applies each active data
  segment to its own memory.

The memory-index infrastructure itself shipped with linear memory in **0.6.2** (indices were threaded
through decode, validate, and every memory op from the start); this release adds the multi-memory
conformance vectors — distinct-memory routing, an active data segment initializing memory 1, and a
cross-memory `memory.copy` — and lifts the use-case matrix cell.

## [0.6.5] — Interpreter: SIMD (stage T5, slice 6)

### Added
- Execution for the **entire fixed-width SIMD proposal plus relaxed SIMD** (the `0xFD` `v128` family,
  ~230 sub-opcodes): `v128.const`, all lane shapes' splat / extract_lane / replace_lane, `i8x16.shuffle`
  / `swizzle`, lane-wise integer & float arithmetic, comparisons (lane masks), shifts, saturating
  add/sub, min/max, `avgr_u`, `abs`/`neg`/`popcnt`, bitwise ops + `bitselect`, `any_true`/`all_true`/
  `bitmask`, narrow/extend/`extmul`/`extadd_pairwise`/`dot`/`q15mulr_sat`, int↔float conversions
  (convert / `trunc_sat` / demote / promote), all v128 loads/stores (incl. `load_splat`, `load_extend`,
  `loadN_zero`, `loadN_lane` / `storeN_lane`), and the relaxed ops (each pinned to one deterministic
  choice — e.g. `relaxed_trunc` → saturating, `relaxed_madd` → double-rounding).
- `v128.const` is now valid in **constant expressions**, so a module may declare a `v128` global.
- **WasmGC `v128` fields** now execute (a struct/array field of type `v128`) — the 0.6.4 deferral is lifted.

So `wasmrt run` executes SIMD modules — e.g. `i32x4.extract_lane(i32x4.add(splat a, splat b))`.

### Changed
- **The interpreter value slot is now 128-bit** (`Value = u128`). A `v128` occupies a single stack /
  local / table / global / GC-field slot, so the whole engine stays on the "one slot per value" model —
  `select`, `drop`, branch arity, locals, and call marshaling never reason about slot width. This is an
  idiomatic-Rust divergence from wazmrt (which stores a `v128` as two `u64` slots and carries width
  tables); observable behavior is identical. Scalars and references live in the low 64 bits, so the
  `NULL_REF` / `I31_TAG` sentinel invariants are unchanged.

## [0.6.4] — Interpreter: WasmGC (stage T5, slice 5)

### Added
- Execution for the WasmGC reference-types proposal: unboxed `i31` (`ref.i31`, `i31.get_s`/`get_u`),
  `ref.eq`, heap **structs** (`struct.new`/`struct.new_default`/`struct.get`/`get_s`/`get_u`/`struct.set`),
  heap **arrays** (`array.new`/`new_default`/`new_fixed`/`get`/`get_s`/`get_u`/`set`/`array.len`), and the
  casts `ref.test`/`ref.cast` + `br_on_cast`/`br_on_cast_fail`.
- A managed GC heap on the `Store` (`Vec<HeapObject>`), with packed `i8`/`i16` struct/array field storage
  (sign/zero-extended on `struct.get_s`/`get_u`), a per-run object budget, and the runtime subtype check
  behind the casts.

So `wasmrt run` now executes GC modules — allocate a struct/array, mutate a field, read it back, and
test/cast references at runtime.

### Notes
- The load-bearing sentinel order holds: `null_ref` (`u64::MAX`) is checked **before** `i31_tag`
  (`1<<63`), so a null reference never masquerades as an `i31`.
- Deferred to later slices (reject loudly, never silent-accept): `v128` GC fields (land with SIMD), and
  GC allocation inside constant expressions (e.g. `struct.new` in a global initializer).

## [0.6.3] — Interpreter: tables + reference types (stage T5, slice 4)

### Added
- Execution for tables and reference types: `call_indirect` (table lookup + runtime signature check),
  `table.get`/`set`/`size`/`grow`/`fill`/`init`/`copy`, `elem.drop`, and the reference ops `ref.null`,
  `ref.is_null`, `ref.func`, `ref.as_non_null`, `br_on_null`/`br_on_non_null`, `call_ref`/`return_call_ref`.
- Element-segment initialization at instantiation (active segments applied to their table; passive kept
  for `table.init`; declarative dropped), and `ref.null`/`ref.func` in constant expressions.
- A per-instance table-entry budget bounds a hostile `table` declaration or `table.grow`.

So `wasmrt run` now executes indirect calls — e.g. a `dispatch(op, a, b)` that calls `table[op](a, b)`.

### Notes
- The load-bearing slot-encoding sentinel is in place: `null_ref` = `u64::MAX` (a funcref is a small
  function index, so they never collide); GC's `i31_tag` will be checked after it.

## [0.6.2] — Interpreter: linear memory (stage T5, slice 3)

### Added
- Execution for linear memory: all loads/stores (i32/i64/f32/f64, the 8/16/32-bit widths with sign/zero
  extension), `memory.size`/`memory.grow`, the bulk ops (`memory.copy`/`fill`/`init`, `data.drop`), and
  **active data-segment initialization at instantiation**. Effective addresses are computed and
  bounds-checked overflow-safely, with per-memory (memory64) address typing.
- A per-instance memory budget (1 GiB default) bounds a hostile declaration/grow. Memory is
  `alloc_zeroed`-backed, so a large declared minimum costs address space, not resident memory (matching
  the oracle's demand-zero behavior without a raw page allocator).

So `wasmrt run` now executes functions that use linear memory — a value stored into memory and loaded
back round-trips correctly.

### Changed
- The interpreter's mutable runtime state (globals, memories, dropped-data flags) is now bundled in a
  `Store` threaded as `&mut`, so a recursive `call` reborrows it cleanly.

## [0.6.1] — Interpreter: float arithmetic (stage T5, slice 2)

### Added
- Execution for f32/f64: arithmetic (`add`/`sub`/`mul`/`div`), NaN-propagating `min`/`max` (with correct
  signed-zero results), `abs`/`neg`/`copysign`, comparison, and rounding (`ceil`/`floor`/`trunc`/`nearest`
  with ties-to-even); float↔int conversions — trapping (`trunc_f*`) and saturating (`trunc_sat_f*`),
  `convert_*`, `demote`/`promote`, and `reinterpret`. So `wasmrt run` now handles floating-point compute
  (`wasmrt run fadd.wasm add 1.5 2.25` → `3.75`).
- Rounding is implemented with bit manipulation, so it works in the freestanding `no_std` build too.

### Notes
- `sqrt` uses the platform math library and so is available with the default `std` feature; a
  freestanding `no_std` build traps on `sqrt` until a software implementation lands. Everything else float
  is `no_std`-clean.

## [0.6.0] — Interpreter: integer compute (stage T5, first slice)

wasmrt can now **run** a WebAssembly function — `wasmrt run <file.wasm> <fn> [args...]`.

### Added
- `wasmrt-core::interp` — a switch-dispatched interpreter over the IR (untyped `u64` value slots;
  validation has proven the types). `Instance::new` instantiates an import-free module (decodes each
  body, precomputes control-flow targets, evaluates global initializers); `invoke` runs an exported
  function.
- Execution for the **integer-compute** language: i32/i64 arithmetic, comparison, bitwise, shifts/rotates,
  sign-extension and wrap/extend conversions; structured control flow (`block`/`loop`/`if`/`else`/`br`/
  `br_if`/`br_table`/`return`); direct `call` including **recursion** (depth-capped); `local.*`,
  `global.*`, `drop`, `select`, and constants. Integer div/rem trap on zero and `INT_MIN / -1`.
- **`wasmrt run`** — parses arguments to the export's parameter types and prints the typed results
  (e.g. `wasmrt run fac.wasm fac 10` → `3628800`).

### Scope
- This first slice is **integer compute**. Float arithmetic, linear memory, tables, reference types,
  GC, SIMD, threads, and exception handling execute in later 0.6.x slices; until then those ops **trap
  loudly** (`UnsupportedInstruction`), never silent-wrong. Modules with imports aren't runnable yet
  (`ImportsUnsupported`).

## [0.5.0] — Validator, core language (stage T4)

wasmrt now **type-checks** a module against the WebAssembly spec (§3) — `wasmrt <file.wasm>` reports
`validation OK` / `FAILED`.

### Added
- `wasmrt-core::validate` — the spec validation algorithm (an abstract operand-value stack + a
  control-frame stack with a bottom `unknown` for unreachable code), plus module-level checks:
  function/code count match, const-expr typing (globals, element/data offsets), element & data segment
  rules, memory/table limits (§3.2.5), tag types, pairwise-distinct export names, the start signature,
  and the `C.refs` undeclared-function-reference rule (§3.4.10).
- Per-op typing for the **core language**: control flow, `call`/`call_indirect`/`call_ref`, parametric
  (`drop`/`select`), variable + local-initialization tracking, references (`ref.null`/`func`/`is_null`/
  `as_non_null`/`br_on_null`/`br_on_non_null`/`eq`/`i31`), tables, bulk memory, and all loads/stores/
  numeric ops — with load/store alignment and per-memory (memory64) address typing.
- Resource caps (max control depth, max locals) that refuse amplification a tiny module could drive.
- `wasmrt <file.wasm>` now prints a validation verdict alongside the summary.

### Scope
- Validation typing for **SIMD, threads/atomics, GC struct/array objects + casts, and exception
  handling is deferred to v0.5.x** (it gets real coverage against the spec testsuite at T6). Those ops
  **reject loudly** (`UnsupportedValidation` → the CLI prints "validation SKIPPED") rather than
  silently accept — so an "OK" verdict is always trustworthy.

## [0.4.0] — Module decode (stage T3)

wasmrt can now decode a whole WebAssembly module — the first release you can point at a `.wasm`.

### Added
- `wasmrt-core::module` — a full binary decoder: header + every core section (type, import,
  function, tag, table, memory, global, export, element, code, data, data-count, start) and the custom
  `name` section. Every import and export is resolved to its full `Extern` type.
- Two-pass type-section decode (a kind pre-scan) so a `(ref $t)` can forward-reference a later type in
  the same rec group; GC struct/array composite types with packed fields and declared subtyping.
- memory64 support in limits (the 64-bit index flag → u64 page counts); multi-value, reference, and
  typed-ref value types; all 8 element-segment variants and the 3 data-segment forms.
- Malformed-binary rejection ported from the oracle: bad magic/version, undefined value types, reserved
  mutability/limits flags, non-UTF-8 names (§5.2.4), self-referential supertypes, data-count mismatch.
- **`wasmrt <file.wasm>`** now decodes a module and prints a summary (sections, types, functions,
  imports/exports, memories/tables/globals, data/element segments, start) — plus `-h`/`--help`.
- Owned data model (`Vec`/`String`), so a decoded `Module` outlives its input bytes and frees on drop.

### Changed
- 15 decode/rejection test vectors ported from wazmrt 1:1.

## [0.3.0] — Opcode IR + body decoder (stage T2)

The shared instruction authority, ported from the wazmrt oracle.

### Added
- `wasmrt-core::opcode` — the complete `Op` table (every core, reference, GC, SIMD, atomic,
  bulk-memory, table, and exception-handling opcode), the `Instr` IR with pre-parsed immediates, and
  `decode_body`, which turns a function body's raw bytes into a `Vec<Instr>`.
- All four prefix families decode: `0xFC` (saturating truncation, bulk memory, table ops), `0xFD` (the
  full v128 SIMD set), `0xFB` (WasmGC), and `0xFE` (threads/atomics). Immediates that own data
  (`br_table`, typed `select`, `try_table`) hold a `Vec` and free themselves — no manual cleanup.
- The internal-tag-vs-wire-byte invariant is enforced: a raw byte in `0xD7`–`0xFA` (a tag whose real
  encoding is a prefix + sub-opcode) is rejected, and lane/heap/block-type immediates are range-checked
  at decode.
- Decode test vectors ported from wazmrt 1:1 (local/add, block+const+load, br_table, s33-range
  rejection, typed-select validation, SIMD `v128.const`, unknown-opcode and raw-internal-tag rejection).

### Note
- Consistent with the oracle, the saturating-truncation bytes `0xC5`–`0xCC` are also accepted as raw
  single-byte forms (their canonical encoding is `0xFC 0x00`–`0x07`).

## [0.2.0] — Types + reader (stage T1)

The decode primitives, ported from the wazmrt oracle and verified 1:1 against its test vectors.

### Added
- `wasmrt-core::types` — WebAssembly value types as a `ValType` `u32` newtype with concrete typed
  references bit-packed in the high bits (bit 31 concrete, bit 30 nullable, bits 28–29 family, bits 0–27
  index); `RefHeap` with the WasmGC subtyping relation; `SectionId`, `ExternKind`, and the `DecodeError`
  set.
- `wasmrt-core::reader` — a zero-copy `Reader` cursor: bounds-checked reads and spec-correct LEB128
  (`u32`/`u64`/`i32`/`i64`/`s33`) that reject over-long encodings and out-of-range values (§5.2.2), plus
  fixed little-endian and float-bit reads.
- Test vectors ported from wazmrt (LEB accept/reject edges) plus ValType bit-packing and subtyping tests.
  All modules stay `no_std` and compile libc-free to `wasm32`.

## [0.1.0] — Scaffold (stage T0)

First release: the crate exists and the build is real on every target surface. No runtime yet.

### Added
- Three-crate Cargo workspace: `wasmrt-core` (no_std-friendly runtime library), `wasmrt-capi`
  (`staticlib` + `cdylib` C ABI, ships `wasmrt.h`), and `wasmrt` (the CLI binary).
- `wasmrt-core` module tree stubbed to mirror the wazmrt source layout, populated bottom-up in later
  releases.
- C ABI version handshake (`wasmrt_abi_version`) proving the `extern "C"` boundary links as both a
  static and a shared library.
- Size-first release profile; builds verified on native (CLI + static lib + cdylib) and freestanding
  `wasm32-unknown-unknown` (no_std, libc-free).

[Unreleased]: https://github.com/jrmarcum/wasmrt/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/jrmarcum/wasmrt/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/jrmarcum/wasmrt/compare/v0.6.9...v0.7.0
[0.6.9]: https://github.com/jrmarcum/wasmrt/compare/v0.6.8...v0.6.9
[0.6.8]: https://github.com/jrmarcum/wasmrt/compare/v0.6.7...v0.6.8
[0.6.7]: https://github.com/jrmarcum/wasmrt/compare/v0.6.6...v0.6.7
[0.6.6]: https://github.com/jrmarcum/wasmrt/compare/v0.6.5...v0.6.6
[0.6.5]: https://github.com/jrmarcum/wasmrt/compare/v0.6.4...v0.6.5
[0.6.4]: https://github.com/jrmarcum/wasmrt/compare/v0.6.3...v0.6.4
[0.6.3]: https://github.com/jrmarcum/wasmrt/compare/v0.6.2...v0.6.3
[0.6.2]: https://github.com/jrmarcum/wasmrt/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/jrmarcum/wasmrt/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/jrmarcum/wasmrt/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/jrmarcum/wasmrt/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/jrmarcum/wasmrt/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jrmarcum/wasmrt/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jrmarcum/wasmrt/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jrmarcum/wasmrt/releases/tag/v0.1.0
