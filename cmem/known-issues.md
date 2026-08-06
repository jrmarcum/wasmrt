# Known Issues

Issue tracker. Gate open (2026-07-27); **assemble → decode → validate → run all working** (T0–T6 done,
v0.1.0–v0.7.0). This records the **inherited concerns** from the frozen wazmrt oracle, the **port notes /
intentional divergences**, and the **open decisions** (now task-list gates). Log real wasmrt bugs here
(file:line + surfacing condition) as they appear, mirroring wazmrt's ledger.

## ✅ Fixed at T8 (2026-08-06) — two SILENT-WRONG-OUTPUT defects

Both are the worst category by this project's own standard (`INDEX.md`: *"prefer a hard
`Err(Unsupported*)` over silent-wrong"*): the assembler **accepted** the source and produced a module
that decoded, validated and **ran, giving the wrong answer**. Neither was reachable from the spec suite
until the T8 linker work made the enclosing modules buildable.

1. **Table initializer expressions were DROPPED.** `parse_table_field` read the limits and element type
   and ignored everything after, so `(table 3 funcref (ref.func $f))` assembled to a table of **nulls** —
   `table.get` returned null where every entry should have been `$f`. The `0x40 0x00 tabletype expr`
   binary form (function-references) was also never emitted, and the decoder rejected it outright as
   *"byte is not a defined value type"* — **7 modules in `elem.wast` were failing on exactly that**. Now
   parsed, encoded, decoded, validated and applied at instantiation. Two related refusals added: a plain
   table with a **non-nullable** element type (uninhabited without an initializer, so the plain form
   cannot express it) and an **imported** table carrying one (the format has nowhere to put it).
2. **Element-segment form 4 hardcodes `funcref`.** Of the eight forms, only 2/6 and 5/7 carry a type
   selector; **form 4 has no reftype field at all**. Emitting it for `(elem (i32.const 0) (ref func) …)`
   silently rewrote the segment's type to `funcref`. It now promotes to form 6, and the funcidx
   shorthand (forms 0–3) refuses a non-`funcref` type outright.

**And the third-order consequence, which is the interesting part.** Fixing (1) made non-nullable table
element types expressible *for the first time* — which exposed that the active-element-segment check
compared **families with nullability normalized away**. That was harmless only while `(ref func)` could
not decode. §3.5.9 is **subtyping**, and it is directional: a nullable `funcref` segment must no longer
satisfy a `(ref func)` table. **Lesson: a check that is "harmless because the case cannot occur" becomes
a bug the moment the case can occur — and nothing will remind you.**

Suite: `table.wast` 12 failures → **2**, `elem.wast` 17 → **13**, `linking.wast` +4 passes.
Seven regression tests in `wat.rs`, including one asserting the `0x40` marker is in the emitted bytes
(behaviour alone would not catch a wrong *encoding* that another decoder must read).

## ✅ Closed at T8 (2026-08-06)

- **Item 6 — the debug-build stack-depth exposure — is ADDRESSED, not fixed.** `max_call_depth` is now
  per-store configuration (`ResourceLimits`), so an embedder linking the debug `cdylib` can lower it.
  **The default stays 512 for oracle parity**; lowering the shipped default would diverge from the
  frozen oracle on legal deeply-recursive programs. Release builds remain correct at the default.
- **The wasm-c-api refcount risk is designed out**, not merely avoided. wazmrt's highest-risk file holds
  six memory-safety invariants by hand; `wasmrt.h` uses value handles that carry the identity of the
  issuing store and are **checked on use**, so a stale or foreign handle is rejected rather than
  followed. Mutation-verified, and exercised by a Miri lifecycle fuzz.

## 🔧 Open — noted at T8 (2026-08-06)

- **Trap backtraces are still empty, and the C ABI now says so out loud.** `wasmrt_trap_frame_count`
  ships its final shape but always returns **0** — per-instruction byte offsets (the `decode_body_tracked`
  work deferred at T2) are not recorded, and an approximate frame is worse than none. Fixing the API
  shape now means no breaking change when real frames land at **T9**. This is defect 4 below, unchanged
  in substance; what changed is that the shape is committed.
- **`wasmrt_caller_get_memory` returns `false` always.** A durable memory handle must be tagged against
  a live `wasmrt_store_t`, and during a host callback the store is mid-borrow — there is nothing to tag
  against. Callbacks use `wasmrt_caller_read`/`_write`/`_memory_size` instead, which is the shape that
  actually matters (they are what the loaders need). The function exists so the wasmtime-shaped call
  sequence compiles, and it **reports honestly that it produced nothing** rather than handing back a
  handle that would fail later. Revisit if a loader genuinely needs the handle form.
- **Imported memories and tables still cannot be named as linker definitions** (`LinkError::
  UnsupportedImportKind`). Instances *can* share memories and tables through the store; there is simply
  no way to publish one under a name yet. Refused loudly, never half-linked. `imports.wast` still skips
  108 on this.

## 📋 Pre-T8 review (2026-08-05) — the punch list the owner asked to discuss

Suite: **59,395 passed / 843 failed / 4,586 skipped — 98.6%** of 60,238 adjudicated.
Workspace: **274 tests**, clippy clean, all four build surfaces green.

**Fixed during the review:**

- **`v128.const` in a const-expr was a validator/interpreter DISAGREEMENT** (`validate.rs` ~344). The
  interpreter has evaluated it since v0.6.5; the validator still rejected it, so a module with a `v128`
  global was refused as invalid *despite running correctly*. This is the harder class to notice — not
  unsafe (validation runs first), just a **false rejection**. Fixed + pinned by two tests, including one
  asserting the other ~230 `0xfd` opcodes are still refused. **+134 passes, −8 failures, −134 skips.**
- **`path_symlink` did not compile off unix/windows** — `let r` existed only under two `cfg`s, so the
  `wasm32` no_std target broke. Caught by the dual-target build during the `#![forbid]` work, not by any
  test. Now an explicit third arm returning `NOSYS`.

**Open, ranked by what they cost:**

**Item 1 is CLOSED — owner decided 2026-08-05: accept + document the resolver TOCTOU residual.** Zero-dep
and zero-`unsafe` both hold; the deployment assumption (no untrusted second process writing inside a
preopen while a guest runs) is written up in `security-model.md`. Do not re-litigate.


| # | Item | Cost / risk | Recommendation |
| --- | --- | --- | --- |
| 2 | **GC constant expressions** (`struct.new`, `array.new*`, `ref.i31` in global inits) rejected by **both** validator and interpreter. | ~30 `i31.wast` failures + some of `type-subtyping`. Consistent, so no disagreement — an honest missing feature. | Fix at T9 hardening, or sooner if a loader needs GC globals. |
| 3 | ~~**Literal/text edges**~~ **DONE 2026-08-05** (owner chose to grind these before T8) — see below. | — | ✅ |
| 4 | **64-bit table index type** (`(table $t i64 …)`) — `table_copy64` 22, `table_init64` 3. | Out of scope by a recorded invariant ("tables stay 32-bit", matching the oracle). | Leave unless a loader needs it — it is a **scope change**, not a bug. |
| 5 | **Proposals not targeted** — `custom-page-sizes` 21+18, `exact` 18, `memory64-imports` 20. | Not in the feature set. | Leave; revisit only if the spec promotes them. |
| 6 | **Debug-build stack depth** — deep recursion can overflow the native stack before the 512-frame cap fires (release is correct). | Debug only. Deliberately not "fixed" by lowering the cap (oracle parity). | Revisit at T8 alongside the C ABI's own re-entrancy limits. |

**Not an issue, recorded so it is not re-litigated:** `delegate` is rejected by assembler, validator and
interpreter alike — deliberate, oracle-faithful. `legacy/try_delegate.wast` failing is the *correct*
outcome.

## 🔧 Open — found by the wasmtk corpus run (2026-08-05), all unfixed

The spec testsuite never exercised these; the **wasmtk** corpus did. All four are measured against the
frozen oracle, so each says what wazmrt does with the same input.

### 1. `ref.null $ConcreteType` is rejected by the assembler — REAL GAP

[`wat.rs`'s `O::RefNull` arm](../crates/wasmrt-core/src/wat.rs) matches only the **abstract** heap types
(`func`/`extern`/`any`/`eq`/`i31`/`struct`/`array`/`exn`/`none`) and its `_ =>` returns `BadImmediate`.
A **concrete** heap type is legal there and encodes as a **positive s33 type index** — the same encoding
`(ref $t)` already uses elsewhere, so the fix is to fall through to type-name resolution instead of
erroring.

```wat
(module (type $A (array (mut i32))) (global $h (ref null $A) (ref.null $A)))   ;; BadImmediate
```

The oracle assembles and validates it. Hits `gc-collect-demo.wat` and `gc-linked-list.wat` — **the only
2 of 534 `.wat` files wasmrt cannot assemble.**

### 2. `reference-types.wat` — `UndefinedType`, oracle says valid

Assembles, then fails **validation** with `UndefinedType`. Oracle: `valid wasm v1, 6 sections`. Not yet
diagnosed. `module/wasm_wast/wasm-wat-samples-main/.../reference-types/reference-types.wat`.

### 3. `39_JstyperMixed.wasm.{rt,roundtrip}.wat` — `TypeMismatch`, oracle **runs** them

Two files (same defect). We reject at validation; the oracle assembles *and executes* them, printing
`3 / 0 / 7`. That the oracle gets a result means this is our type-checker being wrong, not the input.
`wasi/wasm_wasi/`.

### 4. Trap diagnostics are one line — no backtrace

Both runtimes trap identically, but the **reports differ in quality**:

```
wasmrt  stderr: wasmrt: <path>: trap: uncaught exception
wazmrt  stdout: trap: UncaughtException
                  at fn[13] +57
                  by fn[14] +77
                  (no name section: rebuild the guest unstripped for symbols)
```

The missing piece is the **`decode_body_tracked` byte-offset work deferred at T2** — without per-instruction
offsets there is no frame to report. **A C-ABI embedder needs this more than the CLI does**: at T8 a trap
crossing the boundary is otherwise completely opaque. *(Keep writing diagnostics to **stderr** — that is a
deliberate divergence, not a bug: `wasmrt wasi prog.wasm > out.txt` then captures only guest output,
whereas the oracle would mix its trap report into that file.)*

### 5. An unconditional `data_count` section wastes 3 bytes — against the "small" axis

Assembling the book's `helloworld.wat` gives **135 bytes vs wat2wasm's 132**, and the whole difference is
a `data_count` section we always emit. The spec requires it **only** when `memory.init`/`data.drop`
appear; otherwise it is optional. Legal, but it is dead weight on every module with data segments, and
**smallest-binary is one of the three stated axes** (`vision.md`). Fix at T9 with the other size levers.

### Not a defect — recorded so it is not chased twice

- **12 wasmtk modules have no `_start`** (`*_lib_modc`, `1_print`, `18_symbol_table`): they are
  library/reactor modules exporting named functions for a loader to drive. `wasmrt wasi` reports "no such
  exported function"; wazmrt's CLI falls back to summarize-and-validate. **A CLI-shape difference, not an
  engine one.**
- **`15_panic` / `15_Trap-On-Error` "fail" correctly** — both `console.log` a value and *then* throw.
  wasmrt prints `5`, then traps. So does the oracle.
- **`SumSquared.wat` and `loop.wat` were corrupt fixtures**, not wasmrt bugs — book line numbers had been
  pasted into the source (`(export 1"SumSquared")`, `(6local.get`), which is why the oracle rejected them
  too. **Repaired in the wasmtk repo 2026-08-05**; both now assemble, validate, and compute correctly
  (`SumSquared(3,4)=49`; `loop_test` yields 1/6/120/720 = n!).
- **`helloworld.wat` needs no repair.** Both copies assemble to byte-identical output and validate in both
  runtimes. `(call $print_string (global.get $string_len))` passing a *length* is correct: the shipped
  `Chapter2/helloworld.js` reads from a fixed `start_string_index = 100` and takes a length.

## ✅ Literal/text edges — DONE 2026-08-05 (item 3 above)

`simd_const` 46→2, `const` 26→2, `float_literals` 26→**0**, `token` 22→**0**. Suite **59,395 → 61,013
passing**, and **every one of the 284 files now parses (0 unparseable, was 1)**. Six defects, and two of
them were *not* the over-acceptance the punch list predicted:

1. **A folded `br_table` emitted its opcode with NO label vector** (`wat.rs`). The label emission lived
   only in the flat emitter; the folded path fell through to `emit_op_with_immediates`, which has no
   `BrTable` arm. **The assembler reported "assembled" for bytes no decoder could read** — a
   silent-wrong-output bug, the worst category, and the cause of the `token.wast` "unexpected end of
   input" failures. Now one shared `emit_br_table_labels` used by both. **Lesson: an op with a
   variable-length immediate must be reachable from both spellings, or one silently truncates.**
2. **Adjacent tokens were accepted** (`sexpr.rs`). `(data "a""b")` lexed as two strings and silently
   **concatenated to `ab`** — again a wrong value, not a rejection. Now every token must end at
   whitespace, a paren, a comment, or EOF.
3. **Float literals that overflow returned infinity** instead of being refused. Only `inf`/`nan` may
   denote a non-finite value, and they are now matched exactly — Rust's `FromStr` would also take
   `infinity`, `Inf`, `NaN`, all malformed in wasm.
4. **The float grammar was Rust's, not wasm's.** The integer part is mandatory (`.0` is malformed), an
   empty fraction is fine (`1.` is legal), and an exponent marker needs at least one digit (`0e`,
   `0.0e-` are not). `check_float_syntax` decides this now rather than deferring to `FromStr`.
5. **Digit separators were checked "between two alphanumerics"** instead of *between two digits*, which
   admits `0x_1`, `1_e1`, `1e_1` — `x`, `e`, `p` are alphanumeric. The check is now radix-aware: `e` is a
   digit in hex, an exponent marker in decimal.
6. **NaN payloads were masked, not range-checked** — an over-wide payload became a *different* NaN. Now
   a payload must be non-zero and fit the mantissa.

**Two spec-correctness notes discovered on the way:**

- **`$"quoted identifier"` is ONE token** — `$` immediately followed by a string. Tightening (2) broke
  `id.wast` until this was handled; the contrast with the rejected `(data $l"a")` is that there the atom
  is `$l`, not a bare `$`.
- **Annotations `(@id …)` are skipped as trivia.** The proposal says a tool that does not implement
  annotations must ignore them, and their bodies are raw token sequences where the separation rule does
  not apply (`(@a x-y$yz"aa"-2)`), so they must be skipped *lexically*. `)` inside a string or a comment
  is not a terminator. This made `annotations.wast` parse for the first time; it now contributes **51
  visible failures** (it is a proposal wasmrt does not target), which is why the headline rate reads
  98.8% rather than the 98.9% the run showed while the file was still unparseable. **Capability up,
  rate honestly down** — the same accounting as the `register` work on 08-04.

## Spec-suite punch-list (2026-08-05, post shared-store fixes — 851 failing)

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
