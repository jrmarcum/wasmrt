# Known Issues

Issue tracker. Gate open (2026-07-27); **assemble → decode → validate → run → WASI → embed-from-C all
working** (T0–T8 done, published through v0.9.0). This records the **inherited concerns** from the frozen
wazmrt oracle, the **port notes /
intentional divergences**, and the **four deferred decisions (all now RESOLVED)**. Log real wasmrt bugs here
(file:line + surfacing condition) as they appear, mirroring wazmrt's ledger.

## ✅ Fixed 2026-08-06 — the oracle monitor's test check could not fail

Found by a consistency sweep, not by a test. `scripts/check-wazmrt.sh` ran:

```bash
if ( cd "$WAZ" && zig build test ) 2>&1 | tail -20; then
    echo "tests: GREEN"; test_green=1
```

`if` tests the exit status of the **pipeline**, which in bash is the status of the **last**
command — `tail`, which always succeeds. (`set -u` is on; `pipefail` is not.) So `test_green=1`
was set **regardless of what the oracle build did**, and the script printed `error: Unexpected`
immediately above `tests: GREEN` without contradiction.

**Why this one matters more than its size.** The monitor underwrites *every* parity claim in the
port — "parity-gated at each step" means gated by this. **A gate that cannot fail is decoration**
(the standard `tests/README.md` sets), and this one had been decoration for as long as the pipe
had been there.

**The oracle itself is fine.** Once the check was honest it reported FAILED, and a run with a
throwaway cache dir passed cleanly (exit 0) — so the failure was purely the known corrupt local
`.zig-cache`, which throws `error: Unexpected` *before compiling anything*. The script now
captures output to a file and tests the **real** status, and on that specific signature retries
once against a fresh cache, reporting the retry explicitly rather than as a plain pass:
"it only builds with a fresh cache" is itself worth knowing.

The verdict text was stale in the same pass — it still said *"FREEZE CANDIDATE … the gate may
open"* / *"GATE STAYS CLOSED"*, the pre-freeze framing, contradicting the script's own header,
which has said since 2026-07-27 that its role **inverted** to drift detection. It now speaks in
drift terms and distinguishes *drift* from *oracle tests not green*.

## ✅ Fixed 2026-08-07 — the T9a top of the punch list, and what chasing it actually found

**Suite: 61,033 / 738 / 3,075 (98.8%) → 61,247 / 655 / 2,932 (98.9%).** 337 core tests (was 325);
clippy clean; all four build surfaces, the C-ABI gate and Miri green; the per-file diff moves in the
right direction on **every** line — no file regressed.

Six defects, of which **three were not on the list**. The three logged ones (A, B below, and
`ref.null $t`) are fixed as written. The other three came out of a single question — *why is
`br_table.wast` still 161 skips after the `ref.null` fix?* — and are the more interesting half.

### The roadmap's #1 was **misdiagnosed**, and the measurement is what caught it

T9a#1 predicted that `ref.null $ConcreteType` was worth "161 skipped assertions in `br_table.wast`
alone". It was fixed, it was necessary, and it moved **`ref_null.wast`, `ref_test.wast` and
`ref_is_null.wast`** — but `br_table.wast` did not move by a single assertion. Its module was failing
at `BadModuleField`, nowhere near `ref.null`. Getting the file to build took **four** independent
fixes, each of which alone left it still failing:

1. **`ref.null $t` rejected by the assembler** (the logged one). `O::RefNull` matched only abstract
   heads. Now falls through to type-name resolution, exactly as `(ref $t)` already did.
   **It also carried a silent-wrong-value bug**: the hand-rolled table mapped `nofunc → -0x10` (that is
   `func`) and `noexn → -0x17` (`exn`). `ref.null nofunc` therefore assembled to a **`funcref` null**.
   Since `(ref null func)` is not a subtype of `(ref null nofunc)`, that turns valid modules invalid and
   could let invalid ones pass. The arm now reads the one `abstract_heap_code` table, so the codes
   cannot drift again. `wat.rs` ~3059.
2. 🆕 **`(table $x (ref null $t) (elem $f))` was refused outright.** The inline table-elem shorthand
   built its segment with `use_exprs: false`, and the funcidx shorthand (elem forms 0–3) denotes
   `funcref` and nothing else — so `emit_elem_segment` correctly refused it. The entries were *already*
   const-expr forms; only the encoding flag was wrong. Now `use_exprs = elem_type != FUNCREF`, so
   `funcref` keeps its compact form-0 encoding and everything else uses the expression family.
   `wat.rs` ~1647.
3. 🆕 **A block type of concrete reference type was UNDECODABLE.** `(block (result (ref null $t)))`
   encodes as `0x63 <typeidx>` — which as an s33 reads as **-29**, an arm `read_block_type` did not
   have. It consumed the `0x63`, then read the type index as an **opcode**, and rejected a valid module
   as `UnsupportedOpcode`. The fix needed a new `BlockType::ConcreteRef { nullable, type_index }`
   variant, because a concrete `ValType` carries its family head (func/struct/array) and only the
   module's type section knows which one `$t` is — `decode_body` has no module context by design, so the
   index travels unresolved and the **validator** maps it. `opcode.rs` ~254/~632, `validate.rs`
   `block_sig`, `interp.rs` `block_arity`.
4. 🆕 **`br_table` label typing was wrong in two directions.** §3.3.5.8 asks for **one operand sequence
   that satisfies every target**; the validator instead compared the targets **to each other**, and
   after checking a target pushed *the target's* types back onto the stack instead of the operands it
   had popped. So:
   - a later target **narrower** than an earlier one saw a widened stack and was refused
     (`meet-funcref`), and
   - two targets that are unrelated **to each other** were refused even when the operands are bottom —
     `(unreachable) (br_table 0 1 …)` between an `f32` block and an `f64` one is **valid**, because
     bottom is a subtype of both (`meet-bottom`).

   The pairwise check is now gone entirely: in reachable code it is redundant (each target is checked
   against the real operands), and in unreachable code it was simply wrong. The **arity** check stays —
   that part of the rule is real. `validate.rs` `Op::BrTable`.

   ⚠️ This is the one change here that *relaxes* the validator. Verified it did not over-accept: the
   full suite's failures went **down** on every file and up on none, and a test pins that a target the
   real operands do not fit is still refused.

**Result: `br_table.wast` 24 passed / 1 failed / 161 skipped → 185 / 0 / 0.**

**The lesson worth keeping:** a cost written next to a defect is a *hypothesis about the cause*, not a
measurement of it. #1's cost was attributed by reading the file for the first construct that looked
unsupported. The fix was right and the file was right; the causal link between them was invented. Any
punch-list item whose cost was assigned that way should be re-measured after its fix, not assumed
banked.

### A. `Op::MemorySize` reads another instance's memory — SILENT WRONG OUTPUT ✅ FIXED 2026-08-07

Fixed by routing through `ctx.maps.mem(mi)`. The regression test keeps a **second** instance alive per
the standing two-instance rule, and is **mutation-verified**: reverting the one expression makes it
report `5` — module one's page count — instead of `1`. `memory_size.wast` **16 failures → 0**
(`memory_grow.wast` 2 → 0 and `store1.wast` 4 → 0 came with it).

Original write-up:

`interp.rs:2374` indexes the shared pool with the **raw module-local immediate**:

```rust
Op::MemorySize => {
    let Imm::MemIndex(mi) = instr.imm else { … };
    let mem = store.memories.get(mi as usize).ok_or(Trap::NoMemory)?;   // ← never maps
```

`Op::MemoryGrow`, on the very next line of the same `match`, correctly calls
`memory_grow(frame, store, maps, instr)`, which does `maps.mem(mi)`.

**This is the FOURTH instance of the shared-store defect class** documented below (after
`Op::CallIndirect`, `exec_memory_init`, and the assembler's `table.copy`/`table.init` shorthands), and —
verified by auditing every `store.{memories,tables,globals,elem_values,data_dropped,elem_dropped}.get*`
site in `interp.rs` — **the only unmapped access left**.

**Why nothing caught it.** It is invisible with one instance per store, where the two indices are equal
(the standing **two-instance rule**). Clippy's `unused variable: maps` — which caught siblings of this
class before — *cannot* fire here, because `exec_memory` does use `maps`, in the `MemoryGrow` arm.

**Cost:** `memory_size.wast` 16 failures. All four of its modules report `5` pages, which is *module
one's* size. A core MVP instruction returning another module's answer.
**Fix:** route through `ctx.maps.mem(mi)` + a regression test that keeps a **second** instance alive.

### B. The `.wast` runner redirects a failed module's assertions to an unrelated module ✅ FIXED 2026-08-07

Fixed with a `last_build_failed` flag distinct from `current == None`, so the fall-back still serves the
case it was written for (a file that names every module must still run bare actions — pinned by its own
test) but never after a failed build. Both halves are tested.

**It behaved exactly as predicted: failures fell, skips rose.** `i31.wast` 31 failed → 6 (+25 skips),
`load1.wast` 15 → 5 (+10 skips), `exact-func-import.wast` 15 → 6, `custom-page-sizes.wast` 21 → 12. Those
were never real mismatches; they were assertions run against the wrong instance. The headline number was
understated, as recorded.

Original write-up:

`wast.rs:290`:

```rust
None => self.current.or_else(|| self.named.last().map(|(_, i)| *i)),
```

`define_module` correctly sets `current = None` when a build fails — but `target(None)` then **falls
back to the most recently *named* module**, so every assertion belonging to the module that failed runs
against a different instance and is reported as a **value mismatch**.

`load1.wast` is the clean example: `(module $M …)` builds and registers; the next module imports `M`'s
memory, which the linker cannot yet satisfy, so it is skipped; its assertions then silently execute
against `$M` and report *"got 0x0, expected 1"*. **That diagnostic sends you hunting a load bug that
does not exist** — it did exactly that during this audit.

The fallback itself is wanted (a `.wast` naming every module must still run bare actions). It must
simply not apply after a **failed** build. **Fix:** track "the last build failed" as distinct from
"there is no unnamed current module".

**This inflates FAILURES, never passes** — so the headline 98.8% is, if anything, understated. It is
still a correctness bug in the harness, and the harness's whole design premise is that its numbers mean
what they say.

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

## ✅ Fixed 2026-08-08 — the text format's own grammar was not enforced (type-use well-formedness)

**Suite 61,738 / 536 / 2,466 → 61,778 / 496 / 2,466 — 99.2%.** +40 passes, −40 failures.
**`block.wast` 13 → 0, `if.wast` 13 → 0, `loop.wast` 13 → 0** — three files to zero failures — plus
`type.wast` 1 → 0. No file lost a pass; **the `.wat` corpus held at 533/534**, which is the check that
matters when *tightening* a parser rather than a checker. 404 workspace tests.

Found by measurement, not from the list: the roadmap's #12 remainder was logged as "`func.wast` 21, the text
parser", but surveying the worst in-scope files showed `block`/`if`/`loop` at 13 failures **each with an
identical breakdown** — the signature of one shared cause. It was the largest in-scope cluster left.

**A type use has a fixed clause order (§6.4.4):** `(type x)?` then `(param …)*` then `(result …)*`. The
assembler collected clauses in whatever order they appeared and ignored `(type …)` outright, so
`(block (result i32) (param i32))` assembled — and the **validator** then reported the resulting module as a
stack-height mismatch. Wrong stage, 36 assertions on that one rule. Two more from the same function:

- **A block parameter cannot be named.** `(block (param $x i32))` is malformed: only a function's parameters
  bind identifiers, because only a function has local slots for them to name.
- **`(type x)` plus explicit clauses must MATCH.** The assembler returned on the type index and silently
  discarded the explicit `(param …)`/`(result …)`, so `(block (type $sig) (result i32))` against a
  `(type $sig (func))` assembled as `$sig` and the module meant something the text did not say. The suite
  calls this "inline function type". **Same class as the emitter defects T10a is about, reached from the
  parser side** — a fact was present and dropped.

### ⚠️ Where a guard lives matters: one call-level away is no check at all

The first attempt put the order check in `parse_sig` and moved **one** assertion out of forty. The reason is
worth keeping: **`parse_block_type` calls `parse_sig` one clause at a time** (`from_ref(s)`), so the order
state was constructed and destroyed per clause and could never observe a sequence. The check had to move to
the loop that actually iterates. A guard placed one call-level away from the iteration it guards is not a
weaker check — it is **no check**, and only the measurement said so.

**Still open in this cluster:** `func.wast` (9 "unexpected token", 3 "duplicate local", 1 "duplicate func",
plus 2 wrong results and 2 malformed imports) and `call_indirect.wast` (7 "unexpected token"). Duplicate
**identifiers** — locals, funcs, types — are a distinct rule from clause order and were not touched.

## ✅ T9h DONE 2026-08-08 — cross-module type identity via a `Store` type registry

**Suite 61,724 / 554 / 2,466 → 61,738 / 536 / 2,466 — 99.1%.** +14 passes, −18 failures, no file lost a
pass. **`type-subtyping.wast` is 72/0/0 — a file at 100%**, from 36/44 at the start of the day;
`type-rec` 7/9 → **11/5**, `type-equivalence` 10/2 → **10/1**. 401 workspace tests. Cold start within
noise (~4.66 vs ~4.63 ms A/B/A). **Every `Unlinkable: module linked` and `an import does not match` in
the suite is gone.**

`Store` now holds a `TypeRegistry` that interns rec groups as each module joins. A group's key
references outside targets by their already-assigned **store-wide** id — available because groups are
interned in index order and an outside reference is always to an earlier group — so interning is
content-addressed and two modules spelling out the same group land on the same id. Cross-module matching
becomes an integer comparison **at link time, never on a hot path**. Per-instance `type_ids` maps
module-local index → store-wide id. `BTreeMap`, not a scan, for the reason `canonicalize` uses one:
group counts are attacker-controlled.

### ⚠️ The finding: comparing SIGNATURES can never answer an IDENTITY question

The registry alone moved only **2** of the ~11. The reason is worth keeping:

> Two functions can both be the empty `(func)` and still be **different types**, because rec-group
> membership is part of identity — and a signature's params/results cannot express that.

`type-subtyping.wast`'s `M10` exports a `(func)` whose declared type sits in a group whose sibling refers
*outward*; the importer declares a `(func)` from a group whose sibling refers *inward*. Both signatures
are empty, so any structural comparison links them, and the spec says they must not link. **Only the type
*index* carries identity.** The decoder had been resolving an import's typeidx to a `FuncType` and
**throwing the index away**, so `Import` gained `func_type_index`. This is the same shape as the emitter
defects T10a is about: the information was present and discarded because nothing needed it yet.

Two more consequences:

- **§4.5.9 matching is subtyping, not equality.** `M` exporting `f1: $t1` links against a declared `$t0`
  when `$t1 <: $t0`; equality refused three valid modules. The registry therefore records each type's
  supertype **store-wide**, and matching walks that chain. Termination is structural: a supertype is
  always a lower id (an earlier group member, or a group interned earlier).
- 🆕 **`call_indirect` was the THIRD site with the identical defect** — it compared the declared and
  actual *signatures*. Now it compares type identity with subtyping (§4.4.8), by index, module-locally
  (a funcref is a bare index resolved against the calling instance, and imported tables are refused).
  Worth 7 runtime assertions: 6 `assert_trap`-got-a-result plus 1 spurious trap.

**Residual, deliberate:** a **re-exported import** has no defining type index in the exporting module, so
those fall back to the structural comparison rather than being refused — correct whenever no concrete
reference is involved, and the honest option otherwise. Also: repeated *failed* instantiations of
distinct type sections grow the registry, since interning happens before the type checks that need it.
Nothing references the orphans and an identical group reuses them, but it is unbounded; worth a ceiling
if T12 finds it reachable.

**Still open in these files (6, a different class):** validator over-acceptance — 3 `type mismatch` and
3 `unknown type` in `type-rec` / `type-equivalence`.

## ✅ Fixed 2026-08-08 — type canonicalisation: types were compared by index, the spec compares by structure

**Suite 61,712 / 578 / 2,469 → 61,724 / 554 / 2,466 — 99.1%.** +12 passes, −24 failures. **Six files
improved and none regressed:** `type-subtyping` 57/23 → **62/13**, `type-equivalence` 7/10/3 → **10/2/0**,
`type-rec` 7/11 → **7/9**, `ref_cast` and `ref_test` to **zero failures**, `br_on_cast_fail` 13/3 → 15/1.
397 workspace tests. **Cold start unmoved** (~4.59 vs ~4.69 ms A/B/A) — type sections are small, so
canonicalising them costs nothing at this scale.

Rec groups are the unit of type identity (§3.1.4): two groups spelling out the same shape are **one type**.
`Module` now carries `type_canon`, where `type_canon[t]` is the lowest type index structurally equal to `t`,
computed at decode by reducing each group to a structural key — a reference to a *member of the same group*
becomes its **position** (so the group's shape is independent of where it landed in the index space), a
reference *outside* becomes the target's already-assigned canonical id. `Module::is_subtype` compares those
ids, and since **every** subtype question in the engine funnels through it — the validator's `subtype_of`,
the declared-supertype check, and `ref.test`/`ref.cast` at run time — one line carried the whole fix.

Three details worth keeping:

- **`canonicalize` uses a `BTreeMap`, not a linear scan** over previously-seen keys. The number of rec
  groups is attacker-controlled; a scan is O(groups²), so a module of 100k singleton groups would be a
  denial of service **on the decoder**. `alloc`'s BTreeMap needs no dependency and no hasher.
- **It is total.** A reference forward out of its group is invalid and not canonicalisable; it is keyed by
  a distinct sentinel rather than treated as an error, so the decoder never panics on hostile input and the
  bad index stays the validator's to report.
- **`call_indirect` was a second, separate site.** It compared signature `ValType` vectors by raw bits, so
  two spellings of one type trapped as `indirect call type mismatch`. Now `func_types_equal`, which tries
  the slice compare **first** — one memcmp, what almost every call sees — and only walks canonically when
  the bits actually differ, so the hot path is unchanged.

### 🆕 The assembler was flattening every `(rec …)` group — `0x4e` was never emitted

The blocker, and the reason canonicalisation initially *regressed* `type-rec.wast`: `(rec …)` was parsed by
appending its members individually, so the emitted type section contained **singleton groups** and the rec
grouping was gone. Since the group is the unit of identity, that silently changed what the types *were* —
`(rec (type (func)) (type (func)))` became two standalone `(func)`s, which are different types. The
emitter now walks the index space emitting `0x4e`-wrapped groups where one starts and singletons elsewhere,
and a test asserts the wrapper is actually present in the bytes.

**This is the third "the assembler emits a different module than the text describes" defect in two passes**
— after `(sub …)` marking open types final, and element-segment form 4 rewriting a segment's type at T8.
The pattern: the emitter reconstructs a form from *some* of the facts the parser saw, and the dropped fact
turns out to be semantic. Worth a dedicated look at the emitter in T10 rather than waiting for the next one.

### What canonicalisation let us delete

`decl_subtype_of` — the "accept an undecidable pair" approximation added one pass earlier — is **gone**, and
the suite is byte-identical without it. Its test was rewritten from *"structurally equal rec groups are
accepted because they cannot be told apart"* to *"…are the same type"*, plus the converse the approximation
could never have expressed: a group referring to **its own** member is a different type from one referring
**outward**, which `type-rec.wast` asserts and wasmrt used to get wrong.

### 🔧 Still open: **cross-module** type identity (~11 assertions)

Deliberately out of this pass. A canonical id is module-local, and a key containing such ids is not
comparable across modules; making keys self-contained (inlining each referenced group) risks exponential
blowup on chained groups. The real answer is an **engine-level type registry on the `Store`**, interning
rec groups as modules are added, so cross-module comparison is id equality — what wasmtime does. Measured
cost of not having it: **7 "Unlinkable: module linked" + 4 "an import does not match"** across
`type-subtyping`/`type-rec`/`type-equivalence`. It is a design decision (a new structure on `Store`), not a
patch. The remaining ~10 in those files are separate: 6 `assert_trap`-got-a-result and ~4 over-acceptances
in `type-rec` ("type mismatch", "unknown type").

## ✅ Fixed 2026-08-08 — declared subtyping was never validated (21 invalid modules accepted)

**Suite 61,691 / 599 / 2,469 → 61,712 / 578 / 2,469 — 99.1%.** `type-subtyping.wast` **36/44/0 →
57/23/0**: +21 passes, −21 failures, nothing else moved and no file lost a pass. 395 workspace tests.

**T9a#6's logged cause was wrong, and re-measuring is what caught it — again.** The roadmap said "GC
subtyping depth not modelled by the validator, `type-subtyping.wast` 36". The measured top item was
**21 × "Invalid: module was accepted (should be rejected: sub type)"** — not a missing depth model but
**no declared-subtype validation at all**. `module.supertypes` was filled in at decode and then only ever
*walked* by `Module::is_subtype`, which trusts it, so a module could declare any type as any other's
supertype and the whole reference-subtyping story rested on that claim.

Two independent rules, both now enforced in `check_declared_subtyping` (`validate.rs`):

1. **Finality.** A type is final *by default*: only the `0x50` (`sub`) wrapper opens one. `0x4f` is
   `sub final`, and a bare composite type is shorthand for `sub final ϵ`. The decoder had been reading
   `0x50` and `0x4f` **identically and discarding the distinction**, so `Module` now carries
   `type_finals`, and a final type cannot be extended.
2. **Structural matching (§3.4.5).** Same composite kind; functions **contravariant in parameters,
   covariant in results**; structs may only *append* fields; each shared field must match, with
   mutability equal and deciding the variance (immutable covariant, mutable invariant); a packed field
   matches only the identical packing.

### 🆕 The assembler was silently turning open types into final ones

Caught only because the finality check started reading the flag. The emitter chose the `sub` wrapper
**from the presence of a supertype alone**, so `(sub (struct …))` — open, no supertype — assembled to a
*bare* composite type, i.e. **final**. The module the assembler produced was not the module the text
described, and a valid hierarchy became an invalid one. Same class as element-segment form 4 rewriting a
segment's type at T8: a silent semantic change in the output rather than a rejection. The emitter now
chooses from both facts independently — `0x50` open, `0x4f` final-with-supertype, bare composite for
final-with-none.

### The limit of the check, and why it accepts rather than refuses

**wasmrt compares concrete types by INDEX; the spec compares them by STRUCTURE**, so two structurally
identical rec groups are one type with two indices. Written strictly, the check refused **6 valid**
`type-subtyping.wast` modules whose fields are `(ref $f1)` against `(ref $f2)` where both rec groups
spell out the same `(func)`. So `decl_subtype_of` treats a genuinely undecidable pair as **matching** —
the opposite of the choice made for cross-store function-import matching, and deliberately so: there,
accepting binds a call to a mismatched signature (silent wrong call); here, accepting only preserves
behaviour that already existed, while refusing turns valid modules away.

"Undecidable" is kept as narrow as the information allows: both sides concrete, **same family head**,
nullability still respected, and — the refinement a first attempt got wrong — **not** related by a
declared chain in the *opposite* direction, since that is an answer rather than an unknown. Without that
last clause a contravariance breach slipped through; a test now pins it.

### 🔧 The next item this measurement identifies: **no type canonicalisation**

Every remaining failure in `type-subtyping` (23), `type-rec` (11) and `type-equivalence` (10) traces to
this one cause — roughly **40 assertions**, which makes it the largest in-scope item left:

- **15 false rejections** (`TypeMismatch` at build): structurally equal types treated as distinct.
- **7 "Unlinkable: module linked"** + **4 "an import does not match"**: cross-module matching cannot
  decide, so it guesses, and both directions of guess are wrong somewhere. ⚠️ This **supersedes the
  earlier "residual: 1 assertion"** note — that was written before this file's failures were classified.
  **40 is the measured figure.**
- **~11 runtime results** (`assert_trap` got a result, `ref.test` mismatches): `is_subtype` at run time,
  the same index-vs-structure gap.
- **5 remaining over-acceptances** in `type-rec` ("type mismatch", "unknown type").

The fix is canonical rec-group identity with recursive structural equality up to rolling. That is a
feature, not a patch, and should be scoped on its own.

## ✅ Fixed 2026-08-08 — an `InstanceId` from another store reached THIS store's instance (silent wrong memory)

**Found by the owner's constraint, not by a test.** Told that *"the memory needs to be shared once pulled
in; having two memory profiles that are trying to pull from each other does not work"*, I probed the three
properties that statement implies rather than asserting them. Two held. The third was a live defect in the
imported-memory work committed hours earlier.

**`InstanceId` was a bare `usize` with no record of which store issued it.** Hand an id from store X to
store Y and Y indexed *its own* `code` vector: if the index was in range — and index 0 always is — the
import linked and the guest silently shared **Y's memory** while believing it shared X's. Measured: a
guest importing store X's memory (holding `0x11`) read `0x99`, store Y's own. Silent wrong memory, the
class every serious defect in this port has belonged to. It was also a **panic**: `module_of` and
`export_func` indexed `code[id]` directly, so a foreign id with an out-of-range index aborted the process
under `panic = "abort"` (T12a's exact concern).

**Fix: `InstanceId { store, index }`, with every accessor routed through one `Store::slot()`** that checks
the tag before the bounds. This is the defence the **C ABI already applied to its value handles** at T8 —
each carries the identity of the issuing store so a foreign or stale one is refused rather than followed.
Core had the weaker guarantee of the two; that asymmetry was the bug. Store identities come from a
`static AtomicU64` counter starting at **1**, so a zero-initialized id can never name a real store — the
same reasoning as the C ABI's `+1` packing. Verified to build on freestanding `wasm32` no_std.

`Imports` now keeps the whole tagged id for wasm-backed function and memory imports and lowers it to a
slot only inside `instantiate`, so `FuncTarget` stays tag-free on the hot path. **`Store::module_of`
returns `Option` now** (it indexed directly before); the one C-ABI caller reports `false` rather than
unwrapping. The same hole existed for `with_instance_func` since **T7b** and is closed by the same change.

**Mutation-verified:** deleting `id.store == self.id` from `Store::slot` makes the test link the foreign
memory instead of refusing it. Conformance **unchanged** at 61,691/599/2,469 — this is a misuse path the
spec suite cannot reach, which is exactly why it survived three passes of it.

### The two properties that did hold — now pinned instead of assumed

- **Sharing survives a re-export chain.** A defines a memory, B imports and re-exports it, C imports from
  B: C reaches *A's* bytes. B's exported memory is B's own index 0, which is itself an import, so this only
  works by following B's map — a naive implementation gives C a fresh slot or a slot B never owned.
- **A cycle is unrepresentable, not rejected.** An `InstanceId` exists only once its instance does, so B
  can import from A only after A is built and A can never name B. There is no "make a cycle, expect
  failure" test because one cannot be written; the test pins the property that makes that so. The owner's
  *"only when they are not interactive does that work"* is this: the linking graph is a DAG by construction.

## ✅ Fixed 2026-08-08 — decoder strictness: the decoder is now the stage that rejects a malformed binary

**Suite 61,593 / 697 / 2,469 → 61,691 / 599 / 2,469 — 99.0%, the first time over 99.** +98 passes,
−98 failures, **skips unchanged**: no accounting movement, 98 assertions simply started passing.
`binary.wast` **128/88 → 208/8**, **`binary-leb128.wast` → 58/0/0 (a file at 100%)**. 386 workspace tests.

This came out of measurement rather than the punch list: T9a#11 and #12 named ~78 assertions across
three files, and reading the actual failures showed they were one theme in two halves — *rejected at the
wrong stage* (the module is refused, but by the validator) and *module was accepted* (over-acceptance,
the worse half). Seven checks, all at decode:

1. **Section order and uniqueness (§5.5.2)** — worth 16 assertions on its own, the largest single item.
   A repeated section had been **silently replacing** the first, because every arm assigns
   (`functions = decode_function_section(…)`): a module with two function sections ran as the second
   one. That is the silent-wrong-output class. ⚠️ **The order is NOT the id order**, which is why
   `SectionId::order()` is a table and not a comparison: `DataCount` is id **12** but must precede
   `Code` (id 10), and `Tag` (id 13) belongs between `Memory` and `Global` — EH inserted it mid-list.
   Comparing raw ids would accept both in the wrong place and reject them in the right one.
2. **A section's contents must occupy exactly its declared size** — 7 assertions. Leftover bytes were
   simply *absent* from the decoded module: the outer reader had already skipped `size`, so producer and
   decoder disagreed about the contents while still agreeing where the next section began.
3. **func/code count mismatch (§5.5.13) at decode**, not validation — 8 assertions.
4. **Function bodies are decoded at decode time**, with the IR stored in `Code`. A malformed instruction
   encoding is a decode error by the spec; deferring it meant `decode` accepted modules the validator
   then rejected (`binary-leb128.wast`'s 15). This also **replaced** `Code::body: Vec<u8>` rather than
   joining it — see the note on cost below.
5. **Constant expressions are structurally checked at decode** — global inits, table inits, element and
   data offsets, element expressions. They are stored raw and read by the validator *and* the
   interpreter, each with its own little reader, so a malformed encoding surfaced during validation.
6. **`decode_body` requires the terminating `end`** (§5.4.9). Deliberately a *terminator* check, not a
   nesting one: full balance is `precompute_control_flow`'s job, and two authorities on one rule is how
   they drift. The limitation is pinned by a test rather than left to be discovered.
7. **The 2^32−1 locals ceiling at decode**, kept distinct from the validator's `MAX_LOCALS` resource cap.
   The two say different things — "these bytes cannot mean anything" versus "we decline to allocate
   that much" — and collapsing them would lose that.

### Two findings from doing it

- **Our own test fixtures were malformed modules.** Four hand-built vectors put the export section (7)
  *after* code (10), and one declared a function section with no code section at all. They had always
  been malformed; nothing had ever been in a position to say so. A decoder that accepts what no
  conforming producer emits lets malformed fixtures accumulate, and they then encode the wrong rule.
- **The first version of the const-expr sweep was wrong in a way emptiness-checking would have hidden.**
  It fed the offset expression of *every* segment to the check, but a **passive** segment has no offset
  expression, so three tests failed on empty input. The tempting fix — skip empty byte strings — also
  excuses an *active* segment whose offset is genuinely missing. Keying on the segment's **mode** keeps
  the two apart, and `a_passive_segment_has_no_offset_expression_to_check` is written so the sloppy
  version fails it.

### Cost: cold start unchanged within noise

Bodies are now decoded once at decode instead of twice later (validator + each instantiation), and
instantiation clones the IR rather than re-walking bytes — but `Code` briefly held **both** the raw bytes
and the IR, a second copy of every function body in every module, which measured as a ~5% cold-start
regression. Removing the now-redundant `body` field (nothing read it once the IR existed; `body_offset`
is what the raw form was needed for) brought it back. Same-session A/B/A: **~4.5 ms vs ~4.4 ms at 48 KB,
a 2–3% difference inside the recorded run-to-run spread** — so: unchanged, not improved. Steady-state is
untouched by construction (no interpreter change). ⚠️ The session drifted ~10% between A/B/A runs, which
is itself the reminder that `bench/README.md`'s "same-session A/B/A only" rule is not a formality.

## ✅ DECIDED + half-shipped 2026-08-08 — T9a#4: imported **memories** land, **tables** stay refused

**Owner decision (2026-08-08): option 2 — ship imported memories, keep tables refused, leave the funcref
encoding as its own later decision.** The gate below stands unchanged for the table half; do not
re-litigate the memory half.

**Why "half" is the wrong word for the memory part.** An imported memory needed no value-model change at
all — a memory is bytes indexed through `IndexMaps`, so publishing one under a name is *finished* work,
not scaffolding for tables. `Imports` grew a memory vector holding `(instance, that instance's memory
index)` — deliberately **not** a store slot, since an embedder holds `InstanceId`s and has no business
knowing pool layout — and `instantiate` resolves each to the exporter's existing slot. The importer's map
is imported-slots-then-fresh-slots, matching the module's own memory index space.

Three details that were not obvious from the plan:

1. **`module.memories` is the whole index space, imports first.** The old code allocated a fresh memory
   for *every* entry, which is exactly why the blanket `n_mems > 0` refusal was load-bearing: without it
   an imported memory would have been silently given a private allocation. Only the tail
   (`module.memories[n_mems..]`) is now allocated.
2. **Active data segments fork on the index.** A segment targeting an imported memory writes into the
   pools (the memory is already there); one targeting a defined memory writes into the still-local vector.
   Keeping defined resources out of the store until every step succeeds is what stops a *later* failure
   from leaving orphaned slots — the store stays clean on a failed instantiation, as it did before.
3. **Limits matching compares declared types, not current sizes** (§4.5.9), so `Memory` now carries its
   declared `min`. Matching on `bytes.len()` would let a `memory.grow` in the exporter change what links.

### 🆕 What switching on `assert_unlinkable` found: imports were never type-checked

`assert_unlinkable` had been an **unconditional skip** since the runner was written. Implementing it was
in scope (the T7b entry already noted it was gated on this work), and the first thing it revealed is a
defect in the *engine*: **a function or global import was bound with no type check at all**. A module
importing `(func (param i32))` against a `(func)` linked and then ran, caller and callee disagreeing about
the stack — the silent-wrong-output class, sitting behind a skip nobody had questioned.

Fixed in the two places where the type is actually known, which is not the same place:

- **Functions — in `Store::instantiate`**, so a hand-built `Imports` gets the same check the `Linker`
  does. Only a **wasm-backed** import can be checked: a `HostFunc` is a bare closure with no declared
  signature (the C ABI cannot express one either), so a host import is still taken on trust.
- **Globals — in `Linker::resolve`**, because `Imports` carries a bare `Value`. That is not a split
  authority but a consequence of linking globals *by value*: a value cannot say `i32` from `f32`, let
  alone mutable from immutable, so the check has to happen while the definition's `GlobalType` is still in
  hand (new `Store::export_global_type`).

**The function check uses structural equality, and the choice was measured, not assumed.** §4.5.9 matching
is subtyping, and a `ValType` naming a *concrete* GC type packs a **module-local type index**, so deciding
it properly needs cross-module type canonicalisation this engine does not do. Exempting concrete-typed
signatures was tried and is **worse both ways**: it costs 3 correct refusals in `type-subtyping.wast` to
recover 1 false one in `type-equivalence.wast` — three silent mis-links traded for one loud
over-strictness. Equality errs toward **refusing** a link, which is the direction the standing rule wants.
**Residual: exactly 1 assertion**, and the real fix is cross-module canonicalisation (open, below).

**The lesson, which outranks the numbers again: a blanket skip is not a neutral placeholder.** This one was
skipped for a stated reason that had been obsolete since T7b, and while it sat there it insured a defect
class in the engine. Any assertion category the runner declines wholesale should carry a note saying what
would be measured if it stopped declining.

### ⚠️ For whenever the funcref decision is taken: the obvious layout is wrong

The natural proposal — *instance id in bits 32–63, function index in 0–31* — **collides with
`I31_TAG = 1 << 63`**. Any packing has to fit under bit 63 (e.g. instance in 32–62) or restructure the
tag bits deliberately. Worth having on paper before that decision rather than discovering it during.

## 🚦 DECISION-GATE — the TABLE half of T9a#4 is still open (found 2026-08-07)

*(The memory half is DONE — see the section above. What follows applies to **tables only**.)*

T9a#4 read as plumbing: instances already share memories and tables through the store, so "there is
just no way to *name* one as a linker definition". For memories that turned out to be true and it shipped.

**But a shared TABLE cannot be made correct without deciding how a `funcref` is represented.**

A `funcref` value is currently a **bare function index with no instance identity**
(`interp.rs`: *"funcref = function index; NULL_REF = null"*), and `Op::CallIndirect` resolves the entry
it reads against `ctx.inst` — the *calling* instance. So the moment two instances share a table:

- instance A stores `ref.func $a` (say index 3) into the shared table;
- instance B does `call_indirect` on that slot and calls **B's** function 3.

That is a **silent wrong call**, the same class as every serious defect this port has found, and it is
the shared-store index-conflation class again — one level deeper, in the *value* encoding rather than in
a pool index.

**Fixing it means putting the owning instance into the funcref value.** There is room: the value slot is
`u128` and a funcref uses the low 32 bits. But the slot encoding is a **recorded invariant**
(`design-decisions.md`: `NULL_REF` checked before `I31_TAG`, one slot per value), and it is read by the
interpreter's hottest path, the GC heap, the C ABI's value marshalling and the `.wast` runner's result
comparison. That is an owner decision, not an improvisation — the same shape as the shared-store choice
of 2026-08-05.

**Options, for that decision:**
1. **Pack the instance id into the funcref slot** — instance in bits **32–62**, index in 0–31, keeping
   `NULL_REF = u64::MAX` distinct. ⚠️ **Not 32–63**: bit 63 is `I31_TAG`. Correct and complete; touches the
   recorded invariant and every site that reads a funcref (~27 `NULL_REF` sites, 21 of them in `interp.rs`
   including the hottest dispatch path, plus the GC heap, the C ABI's value marshalling and the `.wast`
   runner's result comparison).
2. ~~Ship imported *memories* only~~ — **CHOSEN and DONE 2026-08-08.**
3. **Leave tables refused** and take the remaining skips.

⚠️ **Do not implement imported tables without option 1** — a table that links successfully and dispatches
to the wrong function is worse than one that refuses to link. `LinkError::UnsupportedImportKind` /
`Trap::UnsupportedImportKind` are a correct, loud refusal, and **two tests now pin that refusal** so it
cannot be lifted by accident.

## 🔧 Open — found 2026-08-08 by the T9a#4 work

- **No cross-module type canonicalisation.** A `ValType` naming a concrete GC type carries a module-local
  type index, so import matching across two modules cannot decide subtyping and falls back to structural
  equality. Measured residual: **1 assertion** (`type-equivalence.wast`). Errs toward refusing a valid
  link, never toward binding an invalid one. `interp.rs` (the func-import check in `instantiate`).
- **Host function imports are unchecked by construction.** A `HostFunc` is a bare closure with no declared
  signature, so nothing can be compared against the guest's declaration — and the C ABI cannot express one
  either. Not a defect so much as a limit of the callback shape; worth revisiting only if the C ABI grows a
  way to declare a host function's type.
- **Malformed modules rejected at the *link* stage.** Newly visible now that link failures are adjudicated
  rather than skipped: several `assert_malformed` modules in `memory.wast` / `global.wast` reach linking
  with **empty module and field names**, meaning the *decoder* accepted a malformed import. Same class as
  T9a#11 (wrong-stage rejection) but **not fixed by the 08-08 decoder pass**, which covered section
  structure and instruction encodings, not import-name well-formedness. ~4 assertions.
- **Full control-flow nesting is not checked at decode.** `decode_body` requires the terminating `end`
  (§5.4.9) but does not model which `end` closes which opener, so `block … end` with the function's own
  `end` missing decodes and is refused later by `precompute_control_flow` / the validator's control stack.
  Deliberate — two authorities on one nesting rule is how they drift — and pinned by the last assertion in
  `opcode::tests::rejects_an_expression_with_no_terminating_end` so the limitation is visible rather than
  discovered. Costs nothing measured today.
- **Imported globals link by value, so a *mutable* global import is a snapshot.** Pre-existing and
  documented on `Store::export_global`; the new type check enforces that mutability *matches*, which does
  not make the sharing live. Only a concern if a consumer imports a mutable global and expects writes to
  propagate.

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
| 6 | **Debug-build stack depth** — deep recursion can overflow the native stack before the 512-frame cap fires (release is correct). | Debug only. Deliberately not "fixed" by lowering the cap (oracle parity). | **✅ ADDRESSED at T8 (2026-08-06)** — `max_call_depth` is per-store config (`ResourceLimits`, `wasmrt_config_set_max_call_depth`), so an embedder linking the debug `cdylib` can lower it. **The default stays 512** for oracle parity. |

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

The oracle assembles and validates it. Hits `gc-collect-demo.wat` and `gc-linked-list.wat` — the only
2 of 534 `.wat` files wasmrt cannot assemble.

**⚠️ SEVERITY RAISED 2026-08-06 — this is the single largest conformance win available, not a
cosmetic 2-file gap.** The T9 scoping audit found `br_table.wast:1052` uses
`(br_table $l1 $l2 $l1 (ref.null $t) (local.get 0))` — a concrete heap type. That one rejection makes
the file's **single module** fail to assemble, so **all 161 of its `assert_return`s are skipped**:

```
br_table.wast: 24 passed, 1 failed, 161 skipped
    module failed to build: assemble: wat error: BadImmediate
```

161 assertions is the biggest concentrated block in the whole suite, and the fix is **one match arm**.
Ranked first in T9a for that reason. **General lesson: a defect's logged blast radius was measured on
the corpus that happened to surface it — re-measure before believing it is small.**

> **✅ FIXED 2026-08-07 — and the causal claim above was WRONG.** The one match arm was necessary and is
> done, but it did not move `br_table.wast` at all: that file was failing at `BadModuleField` on
> `(table $t (ref null $t) (elem $tf))`, three fixes earlier in the pipeline. Getting the 161 took four
> independent fixes — see the T9a entry at the top of this file. The lesson stated just above turned out
> to apply to *this very item*, in the other direction: the blast radius was right by accident and the
> **cause was invented**. `br_table.wast` is now **185 / 0 / 0**.

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
offsets there is no frame to report. *(Keep writing diagnostics to **stderr** — that is a
deliberate divergence, not a bug: `wasmrt wasi prog.wasm > out.txt` then captures only guest output,
whereas the oracle would mix its trap report into that file.)*

**Status after T8 (2026-08-06): still open, but the ABI cost is now bounded.** The C ABI ships the frame
API in its **final shape** — `wasmrt_trap_frame_count` / `wasmrt_trap_frame` — and that shape **always
reports 0 frames**, deliberately: an approximate frame is worse than none, and committing the signature
now means real backtraces land at **T9 without a breaking ABI change**. `wasmrt_trap_message` carries the
reason meanwhile. So the remaining work is the byte offsets themselves, not the surface.

### 5. An unconditional `data_count` section wastes 3 bytes — against the "small" axis ✅ FIXED 2026-08-07

Assembling the book's `helloworld.wat` gave **135 bytes vs wat2wasm's 132**, and the whole difference was
a `data_count` section we always emitted. The spec requires it **only** when `memory.init`/`data.drop`
appear; otherwise it is optional. Legal, but dead weight on every module with data segments, and
**smallest-binary is one of the three stated axes** (`vision.md`).

**Fixed (T9b):** `ModuleBuild.needs_data_count` is set while encoding a body that emits `memory.init` or
`data.drop`, and the section is emitted only then. Tracked at the *emission site* rather than by scanning
the finished bytes for `0xFC 0x08`/`0x09`, which would also match those bytes inside an immediate.
The regression test walks the **section list** rather than searching for the byte `0x0c`, which occurs
all over a module's payload and would have let the test pass by accident. **Zero conformance drift** —
the per-file diff across all 284 files is byte-identical before and after.

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
  test spawns a 32 MB-stack thread. **✅ ADDRESSED at T8 (2026-08-06) — the first of the two options:**
  the depth limit is now per-store configuration (`ResourceLimits::max_call_depth`, exposed as
  `wasmrt_config_set_max_call_depth`), so an embedder linking the debug `cdylib` can lower it without the
  shipped default drifting from the oracle. **The default remains 512.** Shrinking the per-frame
  footprint stays available if it ever becomes the better answer.
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

## ✅ The four deferred decisions — ALL RESOLVED (queue empty as of 2026-08-06)

The owner chose (2026-07-27) to defer these as decision-gates at the relevant conversion step rather
than resolve them up front. **Every one has now been decided.** Full reasoning in
`design-decisions.md`; summarized here so nothing gets re-opened by accident:

| Decision | Resolved | Answer |
| --- | --- | --- |
| core+capi crate split vs. one multi-target crate | **T0**, 2026-07-27 | **Workspace of three.** It earned its keep at T8: core stays `#![forbid(unsafe_code)]` while the C boundary — which cannot be — is `deny` in a crate of its own. |
| `random_get`: parity PRNG vs OS CSPRNG | **T7**, 2026-08-04 | **ChaCha20 CSPRNG** seeded from the OS (= oracle parity). Zero-dep. **Fail loudly if entropy is unavailable** — never emit predictable bytes. |
| Zero-dep vs. `cap-std`/`openat2` for the resolver | **T7**, 2026-08-05 | **Zero-dep; accept + document the TOCTOU residual.** Both alternatives rejected (first runtime dependency / would breach the new `forbid`). |
| `wasmrt.h` shape | **T8**, 2026-08-06 | Four answers — real proposal gating, linker in **core**, raw memory pointer + checked copies, trap-frame shape now. |

**No decision-gates remain.** If a new one arises, add it to `design-decisions.md` and give it a
task-list step in `roadmap.md` — do not resurrect this list.

## Triggers (from `INDEX.md`)

Recheck the Zig-std items (#17/#18/#23) framing on every wazmrt sync — they may change as wazmrt or
Zig evolves, and they inform how much of the sandbox code is a straight port vs. a clean Rust rewrite.
