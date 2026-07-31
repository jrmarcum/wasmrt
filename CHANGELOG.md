# Changelog

All notable changes to wasmrt are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and wasmrt uses a **port-progress**
versioning scheme: `0.x` releases climb toward **1.0.0 = full parity** with the
[`wazmrt`](https://github.com/jrmarcum/wazmrt) reference oracle. See [ROADMAP.md](ROADMAP.md) for the
stage ladder and the live use-case matrix.

The three crates share one version and are released together: `wasmrt` (CLI), `wasmrt-core` (library),
`wasmrt-capi` (C ABI).

## [Unreleased]

_Next (0.6.x interpreter slices): SIMD, threads, memory64, and exception handling — plus host imports
(for WASI). Plus the deferred 0.5.x validation arms._

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

[Unreleased]: https://github.com/jrmarcum/wasmrt/compare/v0.6.3...HEAD
[0.6.3]: https://github.com/jrmarcum/wasmrt/compare/v0.6.2...v0.6.3
[0.6.2]: https://github.com/jrmarcum/wasmrt/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/jrmarcum/wasmrt/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/jrmarcum/wasmrt/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/jrmarcum/wasmrt/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/jrmarcum/wasmrt/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jrmarcum/wasmrt/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jrmarcum/wasmrt/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jrmarcum/wasmrt/releases/tag/v0.1.0
