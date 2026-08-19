# Testing

**Definition of done = conformance to the WebAssembly specification on both targets** (native + wasm),
plus the size and speed numbers that decide inclusion. Detail:
`docs/port/06-build-docs-licensing.md`; the test tree layout is in `tests/README.md`.

## 🔒 Conformance anchors (owner, 2026-08-11 — the oracle is retired)

**wazmrt is no longer a test anchor.** The two runtimes compete independently for inclusion in wasmtk and
the universalWasmLoader-\* runtimes, so parity with one is not evidence about the other. What answers
"is this correct?", in order of authority:

1. **The official WebAssembly spec testsuite** — 62,616 adjudicated assertions over 284 files, vendored
   at `../wasmtk/tests/module/wasm_wast/testsuite-main`. This is the anchor: it is the specification,
   executable. `wasmrt wast <dir>` runs it.
2. **wasmtime's observable behaviour**, for questions the suite does not adjudicate — error wording,
   diagnostic offsets, API shape. ⚠️ **Run the real binary; do not answer from memory about it.** That
   rule was paid for: "what does wasmtime's `Module::new` do" invited a recalled answer, and three lines
   through wasmtime 47 settled it in seconds *and* produced two byte offsets to assert against
   (`best-practices.md`).
3. **The wasmtk WASI corpus** — real compiled guests, which is the only thing that exercises the WASI
   surface end to end.
4. **wasmrt's own tests** — 458 of them, for the internal invariants none of the above can reach
   (handle tagging, store isolation, encoding invariants, the `size_of` pins).

✅ **Tail calls** (`return_call`/`return_call_indirect`) were conformed exactly this way and **landed 2026-08-14** — they were the one
wasmrt-target feature the oracle never covered. That is now simply how everything is done.

*Historical: through T9 the strategy was a split — golden-vector parity Rust↔wazmrt for everything the
frozen oracle implemented, spec-suite-and-wasmtime for tail calls, re-checked whenever
`scripts/check-wazmrt.sh` reported drift. That script is deleted.* ⚠️ **Two punch-list items were false
leads produced by that arrangement** ("the oracle runs this, so our type-checker is wrong" — it was
running an entry point that skipped validation, `best-practices.md` §2.3a). Retiring the oracle removes
the class; do not reintroduce it by reasoning about what wazmrt would do.

## Spec-suite conformance — current (2026-08-14, T9f tail calls) — **99.4%**

`wasmrt wast <testsuite>` over the 284 vendored files:

| | T6 gate (08-03) | post-linking (08-04) | post-T7 (08-05) | T8 (08-06) | type-use I (08-08) | type-use II (08-08) | T9a#4 tables (08-08) | T9a#5 GC consts (08-08) | T9a#7 + start (08-08) | charset (08-08) | **T9a#8 (08-08)** | **T9f tail calls (08-14)** |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **passed** | 54,509 | 56,541 | 61,013 | 61,033 | 61,778 | 61,802 | 61,887 | 61,975 | 61,987 | 62,037 | **62,113** | **62,238** |
| failed | 871 | 1,521 | 751 | 738 | 496 | 472 | 457 | 453 | 441 | 393 | **385** | **378** |
| skipped | 9,608 | 6,821 | 3,094 | 3,075 | 2,466 | 2,466 | 2,339 | 2,247 | 2,247 | 2,245 | **2,163** | **2,038** |
| **pass rate** | 98.4% | 97.4% | 98.8% | 98.8% | 99.2% | 99.2% | 99.3% | 99.3% | 99.3% | 99.4% | **99.4%** of 62,498 | **99.4%** of 62,616 |

### T9a#9 (2026-08-08) — no column, because the suite did not move, and should not have

**NOT A DEFECT.** The fixture is an invalid module; wasmrt was right to reject it. #9 read "the oracle
assembles **and runs** them, so this is our type-checker being wrong" — true observation, wrong
conclusion, because `wazmrt <module> <export>` **does not validate**. Through `wazmrt <module>`, its
validating path, the oracle reports `validation: FAILED — TypeMismatch`, **agreeing with wasmrt**.

The module has `if (result f64)` with **both arms pushing i32** — `if.wast`'s
`type-then-value-num-vs-num` is an `assert_invalid` for exactly that, and our `if.wast` is at 0
failures. The two files are also **byte-identical duplicates** and **stale**: 8 functions / 10 types
against the source binary's 14 / 13. The real `.wasm` and the hand-written `.wat` both validate OK.

**What landed instead:** validation failures now name the function (`validation FAILED in function 8`).
Localizing this one by hand, to body #6 of 19, is what the diagnostic's absence cost. 4 tests, one of
which pins the construct so #9 is not re-opened. ⚠️ **Cite the subcommand, not the tool** — §2.3a.

### The thirteenth 08-08 column: T9a#8. **Logged at "1 file", delivered 76 assertions**

**+76 passes, −8 failures, −82 skips.** Four files to zero: `call_ref.wast` 4/4/27 → **31/0/0**,
`br_on_null.wast` 1/3/6 → **7/0/0**, `ref_as_non_null.wast` 1/1/4 → **5/0/0**, `unreached-valid.wast`
1/2/9 → **10/0/0**; `return_call_ref.wast` 10/5/36 → 40/7/0. No file lost a pass. **438 tests.**

🆕 **A new corpus measurement, and it is the one that mattered.** The `.wat` gate had only ever run
*assemble* — 533/534. Running the full **assemble → decode → validate** round trip instead shows the
real state: **534/534 assemble, 0 decode failures**, and the only 2 validate failures are the
`39_JstyperMixed` pair (T9a#9). Before this pass the same round trip found the `call_ref` module
undecodable — **"the assembler returned Ok" was never evidence, and the gate had been asserting only
that.**

**The defect:** `immediate_arity` ended in `_ => 0` and the emitter's match in `_ => {}`, so four
instructions were emitted as **bare opcodes with their operand left in the token stream** —
`call_ref`, `return_call_ref` (typeidx), `br_on_null`, `br_on_non_null` (labelidx). All four decode
and execute correctly; only the assembler was wrong.

⚠️ **None of the three symptoms named the assembler.** Folded `call_ref` shifted every following byte
and produced a body the *decoder* rejected for a missing `end`; flat `call_ref` and `br_on_null` both
reported `UnknownInstr` about the *next* token — which is why `gc-linked-list.wat` sat as an
unexplained "1 of 534" logged **separately from #8**. One cause, three symptoms, two punch-list items.

⚠️ **§1.3 again:** the entry read "1 wasmtk file, undiagnosed" because that is the one file someone
noticed; the cost lived in the 82 skips of five spec files that could not build.

**The generalization:** `Op::from_u8` makes the opcode space enumerable, so a test now walks all 256
single-byte opcodes and asserts the **decoder** and **assembler** agree on which ops take an
immediate, both directions, with bespoke-emitter ops listed explicitly. This is T10a's field-coverage
sweep applied to instructions. ⚠️ **Mutation-verified — and the first attempt lied**: a `perl`
substitution silently failed to match, the test passed, and the natural reading was "the sweep is
decoration". `sed` plus a `grep` confirming the line was gone made it fail and name all four ops
(§4.2a).

### The twelfth 08-08 column: the text format's source character set (§6.2/§6.3)

**+50 passes, −48 failures**, 2 assertions newly adjudicable. **`id.wast` reached 6/0/1 — a file at zero
failures**; `annotations.wast` 12/51 → **56/8**. No file lost a pass, and **the `.wat` corpus held at
533/534** — the measurement that matters when *tightening* a parser, since the suite alone would reward
a rule that rejects valid input. **435 tests.**

**The pick came from surveying by cause, not from the punch list.** The open items were #8 (1 file),
#9 (2 files) and `func.wast` 8; grouping all 441 failures by message put text-assembler errors far
ahead (`BadNumber` 63, `BadForm` 49, `BadValType` 18, `BadModuleField` 16), with `annotations.wast` at
51 the single worst file — **and T9g had written it off as an untargeted proposal.**

⚠️ **The scope note was wrong the way §1.1 says cost figures are wrong.** Four one-line probes, none
containing an annotation, were all accepted: a control character in an identifier, invalid UTF-8 in an
identifier, and a raw control byte in a string. So ~44 of the 51 were **generic §6.2/§6.3 lexer rules**
that apply to every `.wat` file. The confirmation is `id.wast` — unrelated to annotations, 5 failures,
now zero. See `best-practices.md` §1.1a.

🆕 **`from_utf8_lossy` in the lexer was not merely over-accepting — it silently RENAMED.** `$a\xffb`
and `$a\xfeb` both became `$a\u{FFFD}b`, so two distinct identifiers collided on one name; the quoted
form `$"…"` carried the same bug. Restricting atoms to `idchar` makes the slice ASCII by construction,
so the conversion cannot lose anything.

⚠️ **The fourth probe was NOT a defect and was left alone**: `linechar ::= c:char (if c ≠ U+0A)` admits
any character but a newline, so a control byte in a comment is legal. A test now pins that it stays
accepted — **tightening what the grammar permits is the same error as accepting what it forbids**
(§4.6a).

### The eleventh 08-08 column: T9a#7 trap backtraces — and the start function, which never ran

**+12 passes, −12 failures, nothing regressed** — and **all 12 came from the second item**. T9a#7 was
predicted to be worth 0 suite assertions and was: it is diagnostics. What it *did* produce was the
question "where does an **instantiation** trap get its frames?", which led to asking who runs
`Module::start`. Nobody did — §4.5.5 step 11 was simply absent, though the field was decoded,
validated, and printed by the CLI.

`start.wast` **8/7 → 15/0** · `start0.wast` 5/3 → **8/0** · `linking3.wast` 11/1 → **12/0** ·
`linking.wast` +1. **426 tests** (398 core + 28 capi), Miri **28/28**, C-ABI gate 74/74 + `c_smoke`
(now asserting real frames, not the stub), clippy clean, all four surfaces build.

⚠️ **10 of those 12 assertions had been failing in files named `start.wast` and `start0.wast` for five
releases.** They were never diagnosed because their messages said nothing useful, and triage read
messages. See `best-practices.md` §3.1a.

### The tenth 08-08 column: T9a#5, GC constant expressions. **Logged cost 6, real value 88**

+88 passes, −4 failures, **−92 skips**. `i31.wast` **0/6/66 → 61/2/5**, `array.wast` **6/2/43 → 18/2/29**,
`struct.wast` **6/3/17 → 21/3/0**. Nothing regressed.

Six forms in both validator and interpreter — `struct.new`, `struct.new_default`, `array.new`,
`array.new_default`, `array.new_fixed`, `ref.i31` — **the same six on both sides**, so the two cannot
disagree about what a constant expression is.

⚠️⚠️ **THE MEASUREMENT LESSON IN A NEW DIRECTION: a cost counted in FAILURES understates any defect that
stops a module BUILDING.** `ConstantExpressionRequired` on a global initializer fails the whole module, and
every later assertion in the file then has no target and is **skipped**. `i31.wast` was 0 passed / 6 failed
/ **66 skipped** — the 6 was the visible cost, the 66 the actual one. **Read the skip column when
triaging**, especially for defects in module-level positions (global initializers, type definitions,
sections).

### The ninth 08-08 column: T9a#4 COMPLETE — the funcref encoding, then imported tables

+85 passes, −15 failures, **−127 skips**. `elem.wast` **63/13/17 → 75/6/0**, `imports.wast` +34 passes and
−69 skips, `linking.wast` +24/−20, `table_grow.wast` → **50/0/0**, `imports4.wast` → **11/0/0**. No file
lost a pass.

**A `funcref` now carries its owning instance** (instance in bits 62..32, function index in 31..0 — bit 63
is `I31_TAG`). ✅ **The property that made it safe: instance 0 packs to the bare index**, so the encoding
change alone moved the suite by **+1/−1** — one genuine cross-instance case in `elem.wast` that had been
silently wrong. **A value-model change verifiable as a no-op on the existing corpus is worth arranging.**

🆕 **It caught a defect in the memory work from earlier the same day.** A table or memory *instance*'s
type has `min = its CURRENT size`; `grow` updates it (§4.5.9). The memory pass had stored the *declared*
minimum **and asserted that in a test**. No memory case in the suite contradicted it; the table case did.
⚠️ **A test can encode a misreading of the spec and pass forever if nothing exercises it** — the sibling
feature found this, not review.

⚠️ **Unattributed: ~5% steady-state regression, handed to T11.** Cold start unchanged (~4.46 vs ~4.50 ms).
Steady `sum(1000000)` ~55.7 vs ~52.7 ms, consistent across A/B/A, on a loop that touches no funcref, table
or type. Two hypotheses tested and **rejected**: moving the new `TypeRegistry` to the end of `Pools`, and
boxing it. Likely code layout in the giant `run` match — recorded as measured-but-unexplained rather than
asserted.

### The eighth 08-08 column: the same type-use rules at their OTHER TWO sites. `call_indirect` → 0

+24 passes, −24 failures. **`call_indirect.wast` 158/11 → 169/0/0** and **`func.wast` 147/21 → 160/8**; no
file lost a pass, none gained a failure, and the `.wat` corpus held at 533/534.

**Block types, `call_indirect`, and function definitions each had their OWN COPY of the type-use loop** —
and therefore its own copy of all three defects. `parse_type_use` is now the single authority for the first
two; the function path enforces the same order inline, because its loop also owns `import`/`export`/`local`
and the body. **Two copies of a grammar drift — three copies drifted identically.**

⚠️ **One rule was attempted, measured, and withdrawn.** "No declaration after the body begins" looked
obviously right and broke `select.wast`, `stack.wast` and `call_indirect.wast`: in **flat** instruction form
each immediate is its own top-level item, so `select (result i32)` puts a `result` form exactly where a
misplaced declaration would sit. Keyword scanning cannot tell them apart at that layer — deciding it needs
the body's instruction structure. `(func (nop) (local i32))` therefore still assembles; logged as open.
**A rule that is obviously right is still a hypothesis until measured.**

### The seventh 08-08 column: type-use well-formedness. **`block`, `if`, `loop` all reach ZERO failures**

+40 passes, −40 failures; **`block.wast` 13→0, `if.wast` 13→0, `loop.wast` 13→0**, `type.wast` 1→0. No file
lost a pass; the `.wat` corpus held at 533/534, which is the check that matters when *tightening* a parser.

**The text format's own grammar was not being enforced.** A **type use** has a fixed clause order (§6.4.4) —
`(type x)?` then `(param …)*` then `(result …)*` — and the assembler collected clauses in whatever order they
appeared, so `(block (result i32) (param i32))` assembled and the **validator** then reported a stack-height
mismatch. Wrong stage, and 36 assertions across three files on that one rule. Two more from the same place:
a block parameter **cannot be named** (only functions have local slots to name), and giving both a
`(type x)` *and* explicit clauses requires them to **match** — the assembler returned on the type index and
silently discarded the clauses, so the module meant something the text did not say.

⚠️ **Where the rule lives matters.** The first attempt put the order check in `parse_sig` and moved **one**
assertion: `parse_block_type` calls `parse_sig` **one clause at a time**, so its order state reset on every
call and could never see a sequence. The check has to be where the loop is. **A guard placed one call-level
away from the iteration it is guarding is not a weaker check — it is no check at all**, and the measurement
is what said so.

### The sixth 08-08 column: T9h, the `Store` type registry. **`type-subtyping.wast` reaches 72/0/0**

+14 passes, −18 failures, no file lost a pass. `type-subtyping` **62/13 → 72/0/0 — a file at 100%**, from
36/44 at the start of the day; `type-rec` 7/9 → **11/5**; `type-equivalence` 10/2 → **10/1**. **Every
`Unlinkable: module linked` and `an import does not match` in the suite is gone.** Cold start within noise
(~4.66 vs ~4.63 ms A/B/A).

⚠️ **The finding: comparing SIGNATURES can never answer an IDENTITY question.** The registry alone moved
only **2** of the ~11, because two functions can both be the empty `(func)` and still be *different types*
— rec-group membership is part of identity, and params/results cannot express it. Only the type **index**
carries it, and the decoder had been resolving an import's typeidx to a `FuncType` and **discarding the
index**. Same shape as the emitter defects T10a is about: the information was present and thrown away
because nothing needed it yet.

Also: **§4.5.9 matching is subtyping, not equality** (so the registry records supertypes store-wide —
equality refused three valid modules), and 🆕 **`call_indirect` was the third site with the identical
defect**, worth 7 runtime assertions.

### The fifth 08-08 column: type canonicalisation. +12 passes, −24 failures, **six files improved, none regressed**

`type-subtyping` 57/23 → **62/13**, `type-equivalence` 7/10/3 → **10/2/0**, `type-rec` 7/11 → **7/9**,
`ref_cast` and `ref_test` to **zero failures**, `br_on_cast_fail` 13/3 → 15/1. Cold start unmoved
(~4.59 vs ~4.69 ms — type sections are small, so canonicalising them is free at this scale).

⚠️ **A MEASUREMENT-TOOLING FINDING that affects how every conformance diff in this file was made.** The
per-file line prints only when `verbose || failed > 0` ([main.rs](../crates/wasmrt/src/main.rs)), so a
file reaching **zero failures disappears from a non-verbose run** — and a diff keyed on those lines reads
that as the file losing all its passes. It raised exactly that false alarm here (`br_on_cast_fail` looked
like −6 passes; it was actually +2). The direction is always *false alarm*, never *false all-clear*, so
no earlier conclusion in this file was wrong — but **run the suite with `-v` on both sides when diffing
per-file**, or a genuine improvement is indistinguishable from a catastrophe.

### The fourth 08-08 column: declared subtyping. +21 passes, −21 failures, skips unchanged

`type-subtyping.wast` **36/44/0 → 57/23/0**, and nothing else in the suite moved — a validation rule
being added, with no accounting effects at all.

⚠️ **The third time a logged cause turned out to be wrong.** T9a#6 read "GC subtyping depth not modelled
by the validator". The measured top item was **21 invalid modules being accepted**, because there was no
declared-subtype validation of any kind. Different defect, different fix, same file.

**A strict version of the check was measured and rejected.** Written to refuse every undecidable pair it
turned away **6 valid modules** whose fields compare `(ref $f1)` against `(ref $f2)` from two
structurally identical rec groups. Accepting the undecidable case is right *here* — it preserves existing
behaviour where refusing would break valid input — and is the opposite of the call made for cross-store
import matching one pass earlier, where accepting would mean a wrong call. **The direction to err in is a
property of the consequence, not a house style.**

### The second 08-08 column: decoder strictness. +98 passes, −98 failures, skips **unchanged**

The cleanest column in the table: no accounting movement at all, because nothing changed about what
gets adjudicated — 98 assertions simply started passing. `binary.wast` **128/88 → 208/8** (both copies
summed), **`binary-leb128.wast` → 58/0/0, a file at 100%**, `exact.wast` +1. The `.wat` corpus went
532/534 → **533/534**; the `wasm_mod` corpus still validates 12/12.

One theme: **the decoder must be the stage that rejects a malformed binary.** Seven checks, all of
which the validator had been catching one stage too late (or not at all) — section order and
uniqueness, section size, func/code count, bodies decoded at decode time, const-expr encodings, the
`end` terminator, and the 2^32−1 locals ceiling. Details and the two findings in `known-issues.md`.

### The 08-08 column: +346 passes, +42 failures, −463 skips, and **no file lost a single pass**

Two changes produced it, and the second one is the one worth remembering.

- **Imported memories now link** (T9a#4's memory half). `imports.wast` **25/6/108 → 196/13/95** — the
  largest single-file gain since the `register` work — `linking.wast` **55/15/78 → 107/11/28** (failures
  *down* as well as skips), `linking3.wast` 4/4/4 → 8/2/2.
- **`assert_unlinkable` is adjudicated for the first time.** It had been an unconditional skip since the
  runner was written, which meant **nothing had ever checked that a badly-typed import is refused** — and
  the first thing switching it on revealed is that imports were **not type-checked at link time at all**.
  A module importing `(func (param i32))` bound to a `(func)` linked and then ran, caller and callee
  disagreeing about the stack. That is the silent-wrong-output class, found by making a skip honest.

**The +42 failures are all previously-unadjudicated assertions becoming visible verdicts**, the same
accounting as 08-04 and 08-06 — plus ~8 in files T9g already lists as out of scope
(`custom-page-sizes-invalid`, `memory64-imports`, `exact-func-import`). Every failure increase is paired
with a skip decrease. The check that matters is **no file regressed**: pass counts rose or held on all 284.

⚠️ **A blanket skip is not a neutral placeholder.** `assert_unlinkable` was skipped for a stated reason
that had been obsolete since T7b, and while it was skipped it hid a defect class in the *engine*, not just
a gap in the runner. Any assertion category the runner declines wholesale should carry a note saying what
would be measured if it stopped declining — otherwise the skip silently insures the code it covers.

**The 08-07 column is the cleanest movement in the table: failures down, skips down, passes up, and
not one of the 284 files regressed.** Two distinct effects, both worth telling apart:

- **Real capability** — `br_table.wast` went **24/1/161 → 185/0/0**, `memory_size.wast` 16 failures → 0
  (a `memory.size` that read another instance's memory), `memory_grow` 2 → 0, `store1` 4 → 0.
- **Honest re-accounting** — fixing the runner's failed-build redirect turned phantom "value mismatch"
  failures back into skips, which is what they always were: `i31.wast` 31 → 6, `load1.wast` 15 → 5,
  `exact-func-import.wast` 15 → 6, `custom-page-sizes.wast` 21 → 12. **Those files did not improve; the
  measurement did.** The 08-06 note that "98.8% is if anything understated" was correct, and this is the
  correction landing.

⚠️ **Methodological finding.** The `br_table` win was logged against `ref.null $t` — one match arm.
That fix was real and necessary but moved **other** files; `br_table.wast` needed four independent fixes
(see `known-issues.md`). **A cost written beside a defect is a hypothesis about the cause unless someone
measured the fix.** Re-measure after each punch-list item rather than banking its predicted value.

The dip at 08-04 was capability, not regression: wiring `register` + `spectest` moved 2,784 assertions
out of *skipped*, and ~649 of them were already-broken code that had been hidden behind a skip. The
08-05 column is that debt paid off — see the shared-store punch-list in `known-issues.md`.

**The 08-06 (T8) column moved for one reason worth remembering: making the linker resolve a registered
module's exported globals made previously-unbuildable modules build, and what they exposed was two
silent-wrong-output defects** (dropped table initializers, and element-segment form 4 rewriting a
segment's type). `table.wast` 12 failures → 2, `elem.wast` 17 → 13, `linking.wast` +4 passes. **A
module that fails to build costs exactly one `skipped`** — not one per assertion — which is why the
*total* adjudicated count moves when modules start building; do not read that as assertions vanishing.
The one file that got worse is `i31.wast` (+1 visible failure, −1 skip): a module that now builds and
meets the already-logged GC-const-expr gap. Capability up, honestly accounted.

**Two-instance rule.** Every conformance defect fixed on 08-05 was a store-index/module-index
conflation that is *unobservable* with a single instance per store. Regression tests touching pools
(tables, memories, globals, data/elem segments) MUST instantiate a second module first, or they prove
nothing.

**Skips are never folded into passes.** A construct this build cannot put to the test is not a pass —
that is the runner's honesty rule (`wast.rs`), and it is why the number is trustworthy. As of 08-08 the
runner also distinguishes **"nothing defines this import"** (a real unlinkable verdict) from **"wasmrt
cannot back this kind"** (a gap → skip); collapsing the two, as `BuildErr::Unresolved` did, is precisely
what made `assert_unlinkable` unimplementable.

Worst remaining files (2026-08-08, after the charset pass): `memory64-imports` 26 (out of scope),
`table_copy64` 22 (out of scope), `custom-page-sizes-invalid` 20, `exact` 17. ⚠️ `annotations` was top
of this list at 51 and is now **8** — see the twelfth column above for why "out of scope" was the wrong
reading of it. **All 284 files parse (0 unparseable).**

**The largest IN-SCOPE cluster left is type canonicalisation** — ~40 assertions across `type-subtyping`,
`type-rec` and `type-equivalence`, every one the same cause: wasmrt compares concrete types by **index**,
the spec compares them by **structure**, so two structurally identical rec groups are treated as two
types. It shows up as false rejections, undecidable import matching, and wrong `ref.test` results at once.
See `known-issues.md`; the ranked punch list is there too.

**The remaining `func.wast` 21 are the TEXT parser, not the decoder** — "unexpected token" 9, "inline
function type" 3, "duplicate local" 3, "duplicate func" 1, plus 2 wrong results and 2 malformed imports
the decoder still accepts. A separate cluster from the binary work above, and the assembler is where it
lives.

The first run scored 96.7% and surfaced four bugs the hand vectors could not — a panic, an element-segment
encoding no decoder could read, truncated out-of-range constants, and mis-placed digit separators (all in
`roadmap.md`'s T6-gate entry, all pinned by regression tests). **That is the argument for the suite:** it
tests the assembler against the decoder, validator and interpreter at a scale hand vectors cannot reach.

## The wasmtk corpora — first full run (2026-08-05, at v0.8.0)

Two corpora beyond the spec testsuite, both in `wasmtk/tests` ([[spec-testsuite-location]]). **This is
the T7 `wasi-gate`**, and it now runs.

**WASI corpus — 441 `.wasm`, stdout diffed against the frozen oracle `wazmrt@dadc727`:**

| | count |
| --- | --- |
| **stdout byte-identical to the oracle** | **426** |
| differing | 15 |

**Engine-observable behaviour matches on 441/441.** All 15 differences are CLI shape, not execution: 12
are library/reactor modules with no `_start` (the two CLIs disagree about what to do with those), and 3
are deliberate-throw tests where both runtimes print the value and then trap. Detail in
`known-issues.md`.

**`.wat` corpus — 532 files, assemble → decode → validate (2026-08-10, after the validation-gap fix):**

| stage | result |
| --- | --- |
| assembled | **532 / 532** |
| decoded | **532** (0 failures) |
| validated | **532** |
| failed | **0 — a clean sweep at every stage** |

⚠️ **Run all three stages, not just `wat -o`.** Until T9a#8 this gate ran *assemble only* and read
533/534, while the module `call_ref` produced was **undecodable** — the assembler returned `Ok` and the
bytes were garbage. "Assembled without error" is not evidence about an emitter; it is evidence about
the parser. The round trip is what turned one vague "1 wasmtk file" entry into a 76-assertion fix.

**Method notes worth keeping.** Compare **stdout only** (`2>/dev/null`) — wasmrt writes diagnostics to
stderr and the oracle writes them to stdout, so an unseparated diff reports differences that are not
there. And give each file its **own** output path: reusing one `out.wasm` across a 534-file loop hits
Windows file locking and invents phantom failures — it did so again on 2026-08-08, reporting 6
assemble failures where there was 1, which is exactly long enough to start diagnosing the wrong thing.
**A re-run that disagrees with the first is the tell; the numbers above are from clean runs.**

### 🔬 T13-0 round 3 (2026-08-19) — `BadForm` attributed, and it is custom-descriptors

**Measured, not guessed** — the roadmap said *"attribute `BadForm` BEFORE scheduling it, because that is
the mistake this section just caught"*, and it was the right call twice over. Method: run every `.wast`
with `-v`, collect the `module failed to build:` lines, group by file. **116 module-build failures
remain** (was 216 at the T13 baseline).

| cause | modules | attributed to |
| --- | --- | --- |
| `BadForm` | **37** | ⚠️ **33 are custom-descriptors** (`ref_get_desc` 6, `descriptors` 6, `struct_new_desc` 5, `br_on_cast_desc_eq_fail` 5, `br_on_cast_desc_eq` 5, `ref_cast_desc_eq` 3, `exact-casts` 3) · 2 `imports` · 2 `custom-page-sizes` |
| `BadValType` | **18** | ⚠️ **all 18 are `exact`** (`exact` 14, `exact-func-import` 3, `array_new_exact` 1) — the custom-descriptors type former |
| `BadModuleField` | 16 | `instance` 5, `binary` 4, `exact-func-import` 3, then `table64`/`table`/`memory64` 1 each |
| `validate: TypeMismatch` | 14 | `elem` 5, `br_on_non_null` 3, `imports` 2, `try_table`/`call_indirect64`/`br_on_cast_fail` 1 each |
| `UnknownInstr` | 4 | `id`, `select` ×2, `stack` — **newly visible**, surfaced by the scoring split |
| decode / link / other | 27 | mixed |

### ⚠️⚠️ Three error-name clusters, three features. The pattern is now the finding.

| cluster | looked like | actually was |
| --- | --- | --- |
| `BadNumber` 63 | a number-parsing bug | **table64** |
| `BadForm` 37 | a text-format shape bug | **custom-descriptors** (33 of 37) |
| `BadValType` 18 | a value-type bug | **`exact`** (18 of 18) |

🎓 **An error name describes the STAGE THAT NOTICED, never the feature that is missing.** Grouping
failures by message — which is the natural triage, and which T9 did — produces a work-list of *parser
areas* when the real work-list is *proposals*. **Only attributing each failure to its FILE recovers the
cause.** This is §1.1 (*a cost logged beside a defect is a hypothesis about its cause*) at the level of
a whole triage rather than a single entry.

### The re-ranked remainder

**Track D (custom-descriptors + `exact`) is 54 module-build failures — 33 `BadForm` + 18 `BadValType` +
3 `BadModuleField` — plus ~420 direct skips.** It is the largest single lever left by a wide margin.

⚠️ **But it does not go first, by the rule that already promoted the GC array ops: IN-SCOPE OUTRANKS
UNTARGETED.** A missing in-scope instruction is a **correctness gap**; an unimplemented untargeted
proposal is a **scope decision**. Sorting the remainder that way:

1. **The in-scope cluster — ~31 modules and rising.** `instance` 5 + `binary` 4 `BadModuleField`;
   `elem` 5 + `br_on_non_null` 3 + `try_table` 1 + `br_on_cast_fail` 1 `TypeMismatch`; `id`/`select`/
   `stack` 4 `UnknownInstr`; plus the decode/link residue. **None of these is a proposal we declined —
   they are defects in language wasmrt claims to run**, and `br_on_non_null` / `try_table` /
   `br_on_cast_fail` are especially pointed, because those files test features recorded as DONE.
2. **Track D** — custom-descriptors + `exact`, 54 modules + ~420 skips.
3. **Track W** — wide-arithmetic, 108 skips, 4 mnemonics.
4. P / M / A / L.

⚠️ **`imports.wast` appears in three different clusters** (2 `BadForm`, 2 `TypeMismatch`, plus link
failures). Worth treating as one investigation rather than three tickets — the same-file coincidence is
usually one cause, as `call_ref` was at T9a#8.

## 🔬 T13-0 COMPLETE (2026-08-19) — all 2,038 skips attributed, and the track order was WRONG

**Method:** every one of the 13 skip sites in `wast.rs` rewritten to a `skip_at(line!())` probe, plus a
probe on the module-build failure arm; corpus run; **probe reverted from a file copy taken beforehand**,
verified byte-identical to `HEAD`, rebuilt, and the suite re-run to confirm **62,238 / 378 / 2,038** is
unchanged. *(A backup copy, not `git checkout` — §8.1.)*

#### Where the 2,038 skips actually come from

| site | count | what it is |
| --- | --- | --- |
| `assert_return` → `NoTarget` | **1,157** | the module never built — **cascade** |
| `assert_trap` → `NoTarget` | **427** | **cascade** |
| `assert_invalid`/`_malformed` → `is_unsupported` | 303 | **300 are `UnknownInstr`** — mostly correct rejections banked as skips |
| `invoke`/`get` → `NoTarget` | 51 | **cascade** |
| unhandled command kind | 44 | |
| module build → `is_unsupported` | 34 | the modules themselves |
| `assert_return` sub-form | 15 | |
| `assert_unlinkable` → `is_unsupported` | 7 | |

⚠️⚠️ **1,635 of 2,038 — 80% — are `NoTarget` CASCADES.** They are not 1,635 pieces of work; they are
the shadow of **216 modules that fail to build**. *A skip total is dominated by cascades — ask how many
MODULES are behind it before ranking the work.* The exchange rate here is **~7.6 assertions unblocked
per module fixed**.

#### The 216 failing modules, by cause — this is the actual work-list

| cause | modules |
| --- | --- |
| `assemble: BadNumber` | **63** |
| `assemble: BadForm` | **46** |
| `assemble: UnknownInstr` | 28 |
| `assemble: BadValType` | 18 |
| `assemble: BadModuleField` | 16 |
| `validate: TypeMismatch` | 14 |
| `link: unknown import` (13 distinct) | 13 |
| `cannot link an imported Tag yet` | 5 |
| `decode:` (3 kinds) | 8 |
| other | 5 |

#### ⚠️⚠️ The finding that re-ranks T13: `BadNumber` is **table64**, not a number bug

The provisional track order in T13 was written from T9g's file-name groupings and put
**custom-descriptors (D) first**. That is wrong. Attributing `BadNumber` by file:

`table_copy64` 22 · `memory64-imports` 20 · `table64` 11 · `table_init64` 3 · `table_size64`,
`table_set64`, `table_grow64`, `table_get64`, `table_fill64`, `table_copy_mixed`, `elem` 1 each.

**62 of the 63 are the 64-bit table family.** The assembler is refusing an `i64` table index type —
which is **Track T (table64)**, the "tables stay 32-bit" invariant T13 reverses. It is **one feature
wearing a parser error's name.**

🎓 **This is §1.1 exactly — *a cost logged beside a defect is a hypothesis about its CAUSE*, and it has
now been wrong five times.** The T9 triage grouped failures by message, saw `BadNumber 63`, and filed it
as a **text-assembler** cluster; the same 63 measured from the module-build side are **table64**. **The
message named the symptom; only the module-build attribution named the cause.**

#### The re-ranked order

1. **Track T — table64.** 63 build failures + ~326 direct skips in the `table_*64` / `float_memory64`
   files. **The single largest lever on the board**, and it is one coherent feature rather than six.
2. **The assembler scoring split** — ~300 assertions, **no feature work**, independent of every track.
   Cheapest thing on the list per unit of effort.
3. **`BadForm` 46 modules** — not yet attributed to a feature. ⚠️ **Attribute it before scheduling it**,
   because that is the mistake this section just caught.
4. **Track D — custom-descriptors/`exact`** (~420 direct skips), **Track W — wide-arithmetic** (108),
   then P / M / A / L.

⚠️ **`BadForm`, `BadValType` and `BadModuleField` (80 modules between them) are still unattributed.**
They may collapse into the same tracks the way `BadNumber` did, or they may be genuine text-format
gaps. **Nobody knows yet, and the correct entry for that is "unknown", not a guess** — the
last guess cost a whole track's ordering.

## 🔬 T13-0, first measurement (2026-08-19) — 300 skips are CORRECT REJECTIONS

**Baseline re-measured rather than quoted**, by running the corpus: **62,238 passed / 378 failed /
2,038 skipped of 62,616 adjudicated, 284 files, `0 unparseable`.** Identical to what the memory
recorded on 2026-08-14, so the numbers were sound — ✅ and the runner **already prints the fourth
number**, which is the thing a pass/fail/skip triple cannot tell you (`best-practices.md` §3A.2).

### The probe

Instrumented the `assert_invalid` / `assert_malformed` skip site in `wast.rs`
(`if e.is_unsupported() { skipped += 1 }`), ran all 284 files, **reverted the edit by hand** — not with
`git checkout`, which would have taken the uncommitted work in that file with it (§8.1).

| cause at that site | count |
| --- | --- |
| **`assemble: wat error: UnknownInstr`** | **300** |
| `unsupported text construct: legacy delegate` | 2 |
| `cannot link an imported Tag yet` | 1 |
| **total at this site** | **303** |

### ⚠️⚠️ The finding: `UnknownInstr` conflates two different answers

`is_unsupported()` lists `wat::Error::UnknownInstr`, and that error means **both** *"this mnemonic
exists in no wasm proposal"* — a **malformation wasmrt is entitled to diagnose** — and *"our assembler
has a gap"*, which must never score as a pass. Because the two are indistinguishable at the scoring
site, **every one of them is banked as a skip, including the right answers.**

🎯 **The decisive case, `load.wast` — 13 of the 300.** The file asserts that `f32.load32`,
`i32.load32`, `i32.load64`, `i32.load32_s`, `f64.load64` … are **malformed**. ⚠️ **None of those is a
WebAssembly instruction in any proposal** — being unknown *is* the malformation under test. wasmrt
answers `UnknownInstr`, which is exactly right, and **scores a skip instead of a pass.**

### ⚠️ DO NOT "fix" this by deleting `UnknownInstr` from `is_unsupported()`

The conservatism is **load-bearing**, and the asymmetry decides the direction: an omission from the
"we-are-incomplete" side is a **FALSE PASS**, and a false pass is the one direction that **cannot be
noticed afterwards**. Deleting the entry would bank real assembler gaps as conformance.

**The principled fix is to make the ASSEMBLER draw the distinction, at the point where it is known** —
a mnemonic that exists in no proposal → a real malformed verdict; a mnemonic that exists but is
unimplemented → our gap. *An error name that conflates "your input is bad" with "we are incomplete"
cannot be scored correctly by any caller*, and the fix belongs upstream of the scoring table, never in
it.

✅ **wasmrt only became eligible for this on 2026-08-14.** The distinction is only drawable once
**every in-scope proposal is implemented** — otherwise "exists in no proposal" cannot be decided. T9f
(tail calls) closed that, which is the same precondition the sibling runtime needed.

### ⚠️ What this measurement does NOT say — 303 of 2,038, one site of seventeen

`wast.rs` has **17 skip sites** and only one was instrumented. **The other ~1,735 skips are
unattributed**, and they are demonstrably a different population: `br_on_cast.wast` (27 skips) and
`try_table.wast` (47) produced **zero** at this site, and both are files for features wasmrt fully
implements. **Do not extrapolate this result to the whole skip total** — instrument the remaining
sites before ranking the rest of the work.

**The file-level shape of the rest** (skips per file, top of the list) is dominated by the untargeted
proposals T13 brings into scope: `exact-casts` 108 · `wide-arithmetic` 108 ·
`br_on_cast_desc_eq`/`_fail` 98 each · `ref_cast_desc_eq` 94 — with **table64** spread across
`table_init64` 93, `float_memory64` 84, `table_fill64` 70, `table_size64` 36, `table_copy64` 22,
`table_grow64` 21.

🎓 **Ranked by assertions unblocked, the scoring fix leads** — ~300 assertions, **no feature work**,
independent of every track, and cheaper than any of them. That ordering is the whole reason T13-0 runs
before T13's tracks rather than after.

## Current test state (2026-08-14, T9f tail calls; audited 2026-08-19)

**458 workspace tests, all green** (420 core + 28 capi + 10 CLI integration), clippy clean on all four
build surfaces. Suite **62,238 / 378 / 2,038 — 99.4%** of 62,616.

### ⚠️ The `.wat` corpus figure is UNVERIFIED — its denominator moved (found 2026-08-19)

**The corpus is not ours**: the `.wat` files live in the `wasmtk` tree, which is a live repo, and it has
**532** `.wat` files today. Every number this memory records is against a different one:

| recorded | where | what it was |
| --- | --- | --- |
| 533/534 | pre-T9a#8 | assemble **only** — and that gate was the §1.5a defect |
| **534/534 assemble, 0 decode failures**, 2 validate failures | T9a#8, 2026-08-08 | the last honest full round trip |
| 533/533 "clean" | this file, `overview.md`, `roadmap.md`, `INDEX.md`, `loaders.md`, `CLAUDE.md` | **denominator unexplained**; propagated unmeasured |

The 2 validate failures were the `39_JstyperMixed` pair, which **T9a#9 adjudicated as an invalid stale
fixture — wasmrt was right and the file was wrong**. Those files are now absent from wasmtk, which
accounts for 534 → 532 exactly. So the likely truth is *532/532 through assemble→decode→validate* —
**but that is an inference, not a measurement. Re-run the round trip and record the number with its
denominator and its date before quoting it anywhere.**

🎓 **Three rules of ours converge on this one entry**: a gate's number must be reproducible **in the
configuration it was recorded in**, and a corpus you do not own is part of that configuration
(`best-practices.md` §3A.2); when two files disagree, **measure** rather than picking the newer; and
**quote all four numbers** — passed / failed / skipped **and the file-error count** — because a file
that dies before it runs contributes to none of the first three.

### ⚠️⚠️ The kind of test the spec suite CANNOT replace

`tests/tail_call_is_a_real_tail_call.wast` exists because **conformance checks results and the tail-call
proposal is about a resource.** `return_call_ref` shipped for releases as "call the callee, then jump to
the end of the body": every answer correct, native stack growing on every hop. Its conformance file read
**40 passed / 7 failed**, which nobody reads as *the feature is absent*.

Measured deliberately, with the fake restored by mutation: the property test fails **5 of 6** while the
three tail-call spec files still score **38/6, 72/4, 40/6**. Record that pair of numbers — it is the
evidence that a 90% conformance score can coexist with a missing feature. Full lesson:
`best-practices.md` §3.10.

The file pins four chains at **1,000,000 deep** — self-recursion, **mutual** recursion (the case a
self-call optimizer would miss), through a **table**, and through a **typed reference** — plus one
shallow tail call so the ordinary path stays covered.

### `tests/*.wast` runs under `cargo test`

`crates/wasmrt-core/tests/regression_wast.rs` executes **every** `.wast` in the repo's `tests/`
directory, asserting zero failures **and zero skips** (a construct that regresses into "unsupported" is
silently not a pass) and failing if the directory is empty so it cannot pass vacuously. Anything dropped
in `tests/` is covered without anyone remembering to wire it up. ⚠️ It exists because the three
regression reproducers were green and **nothing ran them** — *a gate with no trigger is a preference*
(`best-practices.md` §3A).

The six new CLI tests pin that `run`, `wasi` and summarize all accept `.wat`, that binaries still work,
that an ill-typed `.wat` is **still refused with the full wasmtime-shaped diagnostic** (a new input format
must not be a side door around validation), that the *stage* blamed stays honest (malformed text →
`cannot assemble`, malformed bytes → `decode failed`), and that the extension match ignores case. They are
integration tests because the defect lived in the CLI's file loading, which no core-level test reaches —
which is also why nothing caught it for the whole port (`best-practices.md` §3.8).

**Previously (2026-08-08, T9a#9 resolved as a non-defect):** 450 tests (418 core + 28 capi + 4 CLI); the
C-ABI gate (74/74 + `c_smoke`) and Miri (28/28) pass. This pass added 15: six pinning the backtrace
(three frames innermost-first, offsets that advance within a body, a caught exception leaving none
behind), five pinning the start function (it runs, it runs AFTER the segments, a trap in it fails the
instantiation, once per instance), three in the C ABI, and one pinning `size_of::<Instr>() == 80` so
the claim that the byte offset is free fails the build the day it stops being true. The decoder pass added 11, and two of them are
there because writing the check the obvious way is wrong: `a_passive_segment_has_no_offset_expression_to_check`
(the const-expr sweep must key on the segment's **mode**, not on whether the byte string is empty —
keyed on emptiness it would also excuse an *active* segment with a missing offset, and that is the
version this test fails), and `rejects_an_expression_with_no_terminating_end`, whose last assertion pins
the **limitation**: `block … end` with the function's own `end` missing still ends in an `end`, so the
terminator check passes it. Full nesting balance stays `precompute_control_flow`'s job. A test that
asserts where a check *stops* working is worth as much as one asserting where it starts.

Previous (2026-08-08, T9a#4 memory half): **375 workspace tests** (349 core + 26 capi); the
C-ABI gate (74/74 + `c_smoke`) and Miri (26/26) pass. T9a#4 added 12, each aimed at a way the memory half
could be wrong rather than at "it links": a write through the *importer* is visible to the *exporter* (a
copy-at-link-time bug passes any one-instance test); a **two-provider** case so the importer's index 0 and
the pool's slot 0 differ — the standing two-instance rule, one level deeper; an active data segment
targeting the imported memory, which is the single place the "which memory" code path forks; §4.5.9 limits
matching in both directions plus the *grown-memory* case (matching compares declared types, so a
`memory.grow` in the exporter must not change what links); function- and global-import type mismatch
across five and four shapes respectively, each with the matching declaration asserted to still link — a
check that refuses everything would otherwise look identical; and the table refusal, kept as a test so it
cannot be "fixed" by accident before the funcref encoding is decided.

Previous (2026-08-07, T9 first pass): **363 workspace tests** (337 core + 26 capi), clippy clean; the
C-ABI gate (74/74 symbols + `c_smoke` compiled by clang) and Miri (26/26 incl. the lifecycle fuzz) both
pass. T9 added 12: the four `br_table.wast` blockers (each pinned separately, because each one alone
still left the file failing — a single "the file builds now" test would not say which fix mattered), the
two `.wast`-runner cases (a failed build's assertions are skipped **and** the wanted fall-back still
works), the four `br_table` typing rules, `memory.size` under two instances, and the data-count section.

Two of them earn their keep beyond coverage:

- **`memory_size_reads_its_own_instance_not_the_pool_slot` is mutation-verified** — reverting the single
  expression makes it report `5` (module one's page count) instead of `1`, which is exactly the defect.
- **`the_data_count_section_is_emitted_only_when_required` walks the section list** rather than
  searching the module for the byte `0x0c`. The byte occurs all over a payload, so the scanning version
  would have passed by accident — the same failure mode as a gate that cannot fail.

### Benchmark (T9c, added 2026-08-07)

`cargo run --release -p wasmrt-core --example bench` — cold (decode+validate+instantiate+call) and
steady (`sum(n)` dispatch throughput), reported separately and never conflated. It **asserts the
workload's own result before timing**, so it cannot measure a computation that is wrong, and it warms up
outside the timer. Numbers live in `vision.md` and `roadmap.md` T9c. Compare **same-session A/B/A only**.
T8 added 70 over v0.8.0's 281: the proposal-gate vectors (14 modules, each checked **both** ways —
valid with all features on, and refused *naming that exact proposal* with one flag cleared, because a
one-sided "assert it errors" test proves nothing about *why*), the resource-ceiling tests, 14 linker
tests, 7 regressions for the table-initializer defects, and 26 C-ABI tests.

**The three C-ABI gates are real, and each catches something the others cannot:**

- **`tests/abi_symbols.c` — 74/74.** Takes the address of every function the header declares, forcing
  the linker to resolve it. A function DECLARED but not EXPORTED fails the build; nothing else catches
  that, because compiling a declaration always succeeds.
- **`tests/c_smoke.c` — behaviour, from real C.** Compiled by a C compiler against the shipped header,
  so it proves two things no Rust test can: that `wasmrt.h` is valid C, and that its declarations match
  the exported symbols.
- **Miri (`scripts/miri-gate.sh`) — 28/28 including `lifecycle_fuzz`**, which drives randomized
  creation/use/destruction orders (including the ones the header discourages) and touches handles whose
  store is already gone. **A normal allocator cannot tell a use-after-free from a pass** — it hands back
  freed memory that still looks right. The fuzz is seeded and reproducible on purpose: one that finds a
  fault on Tuesday and cannot reproduce it on Wednesday has found nothing. It scales itself down under
  `cfg!(miri)`, so the gate finishes in ~18s rather than minutes.

**Mutation-verified:** deleting the store-tag check from handle unpacking makes
`a_handle_from_another_store_is_rejected_not_aliased` fail — so the handle checking is load-bearing,
not decoration. (The same standard `tests/README.md` sets: a gate that cannot fail is decoration.)

### Superseded — post-T7 state (2026-08-05)

**281 workspace tests, all green** under native + (compile) `wasm32` no_std; clippy clean on all four
build surfaces. T7 plus the literal/text pass added 63 over v0.7.0's 218: host imports and the shared store (incl. the two
**two-instance** regressions above), the WASI process surface, and the sandbox — whose resolver tests
carry a canary *outside* the preopen and assert the **outcome** (no walk may produce a path that reads
it) rather than a particular errno, so they survive a change of mechanism.

**Two lint-shaped guarantees, both mutation-verified** (a lint nobody has watched fail is not
enforcement): `#![forbid(unsafe_code)]` in `wasmrt-core` and the CLI — adding an `unsafe` block fails
the build; and the sandbox's `..` guard — deleting it fails the canary test.

**Run the `wasm32` no_std target, not just the native one.** The `#![forbid]` work surfaced a
`path_symlink` that compiled only under `cfg(unix)`/`cfg(windows)`; no test caught it, the second target
did.

**The T6 layers added 95.** The **`.wast` runner** added 15 (including the one that pins the honesty
rule: an unknown mnemonic inside an `assert_invalid` must **skip**, not pass). The **assembler** added 55
across its layers, plus 4 regressions for the bugs the spec suite found. The **validator completion**
added 12 (SIMD alignment + missing-memory +
operand typing, exact atomic alignment vs. the scalar hint, GC field/mutability/packing, EH try_table
clause-vs-label, throw tag checks, legacy catch framing, `delegate` rejected). **`sexpr`** added 10
(comments, escapes, the lone-`;` hang regression, depth cap). **The opcode name table** added 3, incl. a
round-trip over every single-byte op — that property is what keeps the assembler's reverse map from
drifting off the decoder. **The assembler** added 28.

**The assembler's tests are its real gate: they assemble → decode → validate → instantiate → invoke.**
Byte-level assertions would only prove the assembler agrees with itself; running what it produced proves
it agrees with the decoder, the type-checker and the interpreter. Both bugs found in the assembler core
(folded memargs, `call_indirect`) surfaced as failed *executions*, not mismatched bytes. Coverage so far:
flat + folded forms, recursion (`fac 10`), a named-label loop (`sum 100`), `call_indirect` dispatch,
data + load, globals, explicit memargs, forward-referencing exports, multi-value and parameterised block
types, and float arithmetic — all from text source. Plus the hex-float rounding vectors the oracle
documents (a truncating parser is one ULP low — a *wrong value*, not a rejected one) and the subnormal
round-vs-flush edge cases.

The
EH slice added 11 — a `try_table` catch, an uncaught throw, `catch_all` binding nothing (a local records
which path ran, since a catch_all target label must be void), an exception unwinding **across a call**, a
`catch_ref` → `throw_ref` round-trip through the exnref box, legacy `try`/`catch`, legacy `catch_all`,
legacy `rethrow` propagating outward, a throw from inside a handler escaping its own try (the idiom that
loops forever without the `caught` guard), `delegate` trapping while unwinding, and no EH state leaking
between invocations. The
memory64 slice added 18 — 12 execution vectors (i64-address store/load round-trip, `memory.size`/`grow`
as i64, grow-past-max → −1, an i64 active data-segment offset, **an address of 2^32 trapping instead of
wrapping** — the case that distinguishes a real 64-bit address from a truncated one — a `memarg` offset
above `u32`, i64 bulk `fill`/`copy`, an i64 `memory.init` destination, i64 addresses through the `0xFE`
atomic and `v128` families, a huge declared minimum hitting the instance budget, and a mixed 64/32-bit
`memory.copy`) and 6 validator vectors (i32 address rejected / i64 accepted on a 64-bit memory,
`memory.size` yielding i64, a `memarg` offset above `u32` rejected on a 32-bit memory but legal on a
64-bit one, the data-segment offset's index type, limits above the 2^48 ceiling, and an `i64` table type
rejected as malformed). The
atomics slice added 5 — `i32.atomic.rmw.add`, `cmpxchg`, an unaligned-atomic trap, a `wait*`-on-non-shared
trap, and a `wait32`-on-shared mismatch (returns 1). The
SIMD slice added 10 hand-built vectors — splat+extract, `v128.const`+extract_lane_u, `i32x4.add`,
`i32x4.eq`+bitmask, `i32x4.shl`, `f32x4.add`, `i8x16.add_sat_s` (saturation), `v128.load`/`store`
round-trip, a `v128` struct field, and a `v128` global — representative of each op category (full
per-opcode SIMD conformance comes from the spec suite at T6). The multi-memory slice added 3 — distinct
mem0/mem1 routing, an active data segment (flag `0x02`) initializing memory 1, and a cross-memory
`memory.copy`. The
per-slice method has held: each task ports **wazmrt's own test vectors 1:1** where they're hand-buildable
(T1 LEB accept/reject + ValType bit ops; T2 `decode_body` vectors incl. prefix/rejection; T3 the 15
module decode/malformed vectors; T4 9 validator vectors; T5 add/factorial/loop-sum + traps + the delicate
float helpers directly — nearest ties-even, NaN min/max, trunc/floor/ceil, trap/sat conversions — and
the memory ops — store/load round-trip, `memory.size`/`grow`, active data segment, OOB trap; the
table/ref ops — `call_indirect` dispatch + OOB trap, `ref.null`/`is_null`, `table.set`/`get` via
`ref.func`; and the **GC ops** — struct `new_default`/set/get, array `new`/set/get/len, `i31.get_s`/`get_u`,
`ref.test`), plus a few extras and end-to-end CLI runs (`add.wasm`, `fac.wasm`, `fadd.wasm`, `rt.wasm`). The
oracle's `wasm_mod` corpus lives on removable media, so the ported oracle unit tests stand in for it.

**Two constraints shape T4/T5 coverage** (recorded so future slices plan for them): (1) most of the
validator's tests and the interpreter's spec vectors need the **WAT assembler**, which doesn't exist
until **T6** — so exotic validate/exec arms can't be verified until then. (2) That's exactly why T4/T5
were **sliced** (core-first, exotic-later) with deferred ops rejecting loudly. The `.wast`/spec-testsuite
conformance gates come online at **T6** (text toolchain) and are where SIMD/GC/atomics/EH validation +
execution finally get real coverage; the C-ABI/Miri gates at **T8**.

## Where the spec testsuite lives (owner, 2026-08-03)

The official WebAssembly spec testsuite is **already vendored locally** — no clone needed:

```
D:\Programs\_ProgramExamples\Example_Programs\wasmExamples\wasmtk\tests\module\wasm_wast\testsuite-main\
```

**257 core `.wast` files** plus `proposals/` (custom-descriptors 14, threads 4, custom-page-sizes 4,
wide-arithmetic 1) and `legacy/` (4 — the legacy EH encoding: `throw`, `rethrow`, `try_catch`,
`try_delegate`). Mirrored on GitHub at <https://github.com/jrmarcum/wasmtk/tree/main/tests>. The wider
`wasmtk/tests` tree also holds the **WASI corpus** used at T7 and 534 `.wat` files.

This is the **T6 conformance gate**: the `.wast` runner walks these and must reach wazmrt's ~60k-assertion
pass profile. Note `legacy/try_delegate.wast` is expected to fail its `delegate` cases — wasmrt rejects
`delegate` deliberately, matching the frozen oracle (see `known-issues.md`); record it as a known
divergence rather than chasing it.

## Test layers (mirror wazmrt, ported)

- **Unit tests** — port wazmrt's test corpus module-by-module (decode/validate/interp/text/wasi/pin).
  At the freeze wazmrt prints **489/493 pass (4 skip)** — ~216 distinct tests, roughly doubled because
  the `cabi` target re-runs the core tests (see wazmrt `testing.md` "Reading the test count"). Start with
  the LEB accept/reject vectors and the ValType-packing bit ops (pure, high-value).
- **C-ABI behavior — ✅ BUILT at T8 (2026-08-06).** `tests/c_smoke.c`: compile → instantiate (with a host
  import) → call → read/write memory → global → trap → teardown, plus a foreign-handle rejection. Run
  via `scripts/c-gate.sh`.
- **C-ABI link completeness — ✅ BUILT at T8. `tests/abi_symbols.c`, 74/74 symbols.** Same runner.
- **C-ABI memory safety — ✅ BUILT at T8.** `scripts/miri-gate.sh` runs the whole capi surface under
  **Miri**, including the randomized `lifecycle_fuzz` (wazmrt #22). A normal allocator can't catch a
  double-free/UAF; the detecting allocator is the oracle. Less risk than wazmrt here because wasmrt's
  capi uses **checked value handles**, not a refcount object model — the fuzz still guards
  instance/memory lifetimes, and additionally proves a handle outliving its store is *refused*.
  (ASAN was not needed once Miri was clean; revisit only if a platform Miri cannot model matters.)
- **Conformance gates:**
  - **spec testsuite** — `wasmrt <file.wast>` runs the official corpus (positive + `assert_invalid`/
    `assert_malformed`/`assert_trap`/`assert_unlinkable`). The corpus lives outside the repo (wazmrt keeps
    it on removable media at `wasmtk/tests/module/wasm_wast/testsuite-main`).
  - **wasi-gate** — compile real Zig/C/Rust `wasm32-wasi` guests (`examples/`) and run them through the
    wasmrt CLI asserting exact stdout. A regression fails the build. "A gate that can't fail is
    decoration" — verify it fails on a wrong expected string.

## Method discipline (carry from wazmrt)

- **Diff the OUTPUT (N passed / N failed), not exit codes** — a build that exits 0 while silently
  dropping passes is a regression.
- Benchmark against a **same-session baseline** (`git stash` A/B/A); never compare Mops/s across days
  (~8% run-to-run spread). See `bench/README.md`.
- Each feature slice adds unit tests + a hand-written `.wast` (or corpus gate) + verified zero
  regressions across all build surfaces (`cargo test` / native / wasm, + c-smoke where relevant).
- Track binary **size** per artifact as a first-class metric (criterion (c)).
