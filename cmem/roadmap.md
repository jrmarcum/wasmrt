# Roadmap

## 🎯 THE 1.0 PROGRAM — the ladder RE-CUT (owner, 2026-08-19). **THIS IS THE NEXT WORK.**

**The version ladder changed shape.** `1.0.0` is no longer "complete on wasmrt's own terms" reached by
finishing T9–T12; it is a **specific, measurable deliverable of its own** — *clearing the skips and
failures out of the run list* — and the four review phases become the `1.0.x` line behind it.

| stage | version | what it is | shape |
| --- | --- | --- | --- |
| **T13 — Conformance clear-out** | **`1.0.0`** | drive the spec corpus to **zero failures, zero skips, zero unrun**, with an empty baseline | do the work |
| **T9 — Hardening** | `1.0.1` | `pin`, and making the *shipped* configuration defensible | do the work |
| **T10 — Bug hunt + code hygiene** | `1.0.2` | the unknown ones, across tested **and untested** paths | do the work |
| **T11 — Optimization review** | `1.0.3` | measured options presented for a decision | **review + recommend** |
| **T12 — Security review** | `1.0.4` | penetration surfaces, recommended plugs | **review + recommend** |

⚠️ **The T-numbers are IDENTIFIERS, not an order.** They are load-bearing across every `cmem/` file
(`T9a#7`, `T10a`, `T12z`…), so renumbering them would break more than it clarifies. The **order** is now
**T13 → T9 → T10 → T11 → T12**. *(This is the "renaming one member of a set" hazard from
`best-practices.md` §3A.2 — so the whole set keeps its names and only the ordering column moves.)*

🔒 **`measure → find → optimize → attack` still holds inside the `1.0.x` line**, and the clear-out sits
in front of all of it for a reason of its own: **T10's bug hunt and T11's baselines are both measured
against the corpus**, so running them while 2,416 assertions are unadjudicated means hunting bugs in code
whose conformance is unknown and optimizing an engine that is about to grow several proposals.

⚠️ **Consequence to state plainly, because the reordering causes it:** `pin` moves *behind* 1.0.0, so
**`1.0.0` ships with no authenticity check of any kind** (`pin.rs` is still a doc-comment stub). That is
a deliberate consequence of putting conformance first, not an oversight. The **authority** half of the
security model — the WASI sandbox — is built and unaffected.

---

### T13 — the CONFORMANCE CLEAR-OUT. `1.0.0` `[ ]`

**Target: 0 failed / 0 skipped / 0 files unrun, and an EMPTY baseline.** The starting position is
**62,238 / 378 / 2,038** — so **2,416 assertions are not passing**, and the second number is the smaller
one.

🚦 **This REVERSES two recorded scope invariants, and the reversal is the owner's decision, recorded
here rather than drifting in silently** (`design-decisions.md`):

1. **"Tables stay 32-bit"** — table64 is the largest in-scope-adjacent block and it is in the corpus.
2. **"Untargeted proposals: record, do not fix"** (T9g) — custom-descriptors, `exact`, custom-page-sizes,
   wide-arithmetic, custom-annotations, memory64-imports all come **into** scope, because a corpus at
   zero cannot contain them as permanent baseline lines.

**WASI preview 2/3 and the component model stay OUT** — they are not in this corpus and nothing here
touches them.

⚠️ **The honest security framing, taken from the runtime that already did this:** the untargeted-proposal
assertions are modules wasmrt **refuses**, and *a module that will not run cannot do harm*. **Implementing
them ADDS attack surface rather than closing a hole.** That is a legitimate thing to want — it is
completeness, and completeness is the stated goal — but it means **every track below carries a soundness
checkpoint, not just an assertion count**, and every proposal ships **with a feature gate on the same
day** (see T13-F).

#### 🔬 T13-0 — SCORE AND INSTRUMENT FIRST. Nothing else starts until this lands. `[◐]`

  ✅ **FIRST RUN DONE 2026-08-19 — and it found ~300 assertions of pure scoring, exactly as predicted.**
  Full write-up in [testing.md](testing.md). Baseline **re-measured, not quoted**: 62,238 / 378 / 2,038
  of 62,616, 284 files, **0 unparseable**. Instrumented the `assert_invalid`/`assert_malformed` skip
  site, ran all 284 files, reverted the probe **by hand** (§8.1 — not `git checkout`). Result: **303
  skips at that site, 300 of them `UnknownInstr`.** 🎯 `load.wast` alone is 13 of them, asserting that
  `f32.load32` / `i32.load32` / `i32.load64` are malformed — ⚠️ **none of those is an instruction in any
  proposal**, so being unknown IS the malformation, wasmrt answers correctly, and scores a **skip**.
  ⚠⚠ **The fix is in the ASSEMBLER, never in the scoring table** — deleting `UnknownInstr` from
  `is_unsupported()` would bank real assembler gaps as passes, and a false pass is **the one direction
  that cannot be noticed afterwards**. ✅ wasmrt only became eligible for the split on **2026-08-14**,
  because “exists in no proposal” is undecidable until every in-scope proposal is implemented (T9f).
  ⚠ **303 of 2,038 — ONE site of SEVENTEEN.** `br_on_cast.wast` (27 skips) and `try_table.wast` (47)
  produced **zero** here and are files for features wasmrt fully implements, so the rest is a different
  population. **Do not extrapolate; instrument the remaining sites before ranking the tracks.**


**This is the single highest-value item and it implements nothing.** In the other runtime this phase
alone converted **292 skips into passes** and reclassified **14 of 104 "failures"** — before a line of
proposal code was written.

- **Instrument every skip site** — source line + error name at each `skip`, run the whole corpus, revert
  the probe. ⚠️ **Do not reason about the skips from reading the files**: when that was tried, **two of
  four cases classified as SCORING bugs turned out to be decoder and assembler gaps rejecting VALID
  modules.** *A wrong diagnosis that agrees with the evidence you chose to look at is the expensive kind.*
- **Audit the runner's scoring for the two known defects**, both of which wasmrt is structurally exposed
  to:
  - ⚠️ **A classification rule that one call site does not consult.** There, the bare `(module …)` arm
    never called `isOurLimitation`, so the identical error scored as a *skip* on one path and a *defect*
    on another — 14 of 104. **Score the same error the same way on every path.**
  - ⚠️ **An error name that conflates "your input is bad" with "we are incomplete"** cannot be scored
    correctly by any caller, so every instance had to be banked as our gap — **292 correct rejections
    across CORE files were counted as skips.** Split it *where the information is*, at the point of
    failure. **And note the asymmetry when you do:** an omission from the "we-are-incomplete" side is a
    **false pass**, which is the one direction that cannot be noticed afterwards — so that list must be
    over-inclusive and guarded by "the untargeted directories never gain passes".
- **Report the FOURTH number.** A file that dies in the lexer contributes to none of passed / failed /
  skipped. Quote *passed / failed / skipped / files-with-errors* from here on.

**Gate:** a per-cause table of the whole residual, **ranked by ASSERTIONS UNBLOCKED**, with the
files-with-errors count beside it. That table — not the estimates in T9g below, which are hypotheses —
decides the order of everything after it.

#### 📊 The ranking rule, and why the failure column is the wrong one to plan from

⚠️⚠️ **A module that fails to BUILD takes every assertion targeting it into skips.** So:

- **An item triaged from the FAILURE column undercounts any defect whose symptom is a SKIP** — three
  times in a row over there, and once here already: T9a#5 was logged at 6 and delivered **88**; T9a#8 was
  logged at "1 file" and delivered **76**. Their R3 was triaged at ~10 failures and unblocked **225
  assertions**; R5 was filed as "~23" and was **1,291** — *more than half of every skip in the suite*,
  from one `return error.BadCommand`.
- **Read the skip column in the same row.** A file at `0 passed / 1 failed / 34 skipped` is not a small
  item; it is a module that will not build.
- **A skip total is dominated by CASCADES** — measure the causes, not the column. Of one 144-skip
  residual, **109 were a single untargeted proposal** and only ~34 direct: ten modules that fail to
  assemble, and 99 `assert_return`s cascading behind them.
- 🚦 **EXPECT THE FAILURE COUNT TO RISE, and do not treat that as a regression.** When 978 suppressed
  assertions started executing there, failures went **143 → 216** while passing went **61,429 → 62,333**.
  ⚠️ **The gate is therefore NOT the totals — it is "no file lost a pass", verified by joining the
  PER-FILE pass counts.** wasmrt has held that check through all eighteen T9 passes; it is the only one
  that stays meaningful while skips convert into verdicts.
- ⚠️ **Some causes are rules we INVENTED, not rules we missed.** Two of one batch's six were checks the
  validator had added that the spec does not have — one deleted outright, not narrowed. **A clear-out is
  not only "implement more".** wasmrt has already hit this once: a strict declared-subtyping version was
  measured, found to refuse 6 VALID modules, and rejected.

#### 🗂️ The tracks — provisional, to be RE-RANKED by T13-0's table

Named after the equivalent tracks in the runtime that already cleared these, so the two records read
against each other. **Counts are from T9g and are hypotheses about causes, not measurements.**

| track | what | logged assertions | notes |
| --- | --- | --- | --- |
| **T13-F** | **the GATE for every proposal below** | — | ⚠️ **A proposal that ships without a gate is not "enabled by default" — it is UNREFUSABLE.** wasmrt already has real gating (15 flags, enforced at validation), so this is small: **one `Feature` per track, landed the same day as the track**, plus **T10b**'s pin so the three spellings of the list cannot drift. A per-proposal checklist cannot ask this question about itself — it lives here. |
| **T13-D** | custom-descriptors + `exact` | ~460 | the largest feature since GC. `br_on_cast_desc_eq`/`_fail` 98 each, `ref_cast_desc_eq` 94, its `binary.wast` 44; `exact`/`exact-casts` 18 + 108. ⚠️ **It RETYPES `br_on_cast` rather than only adding instructions**, so the same module is `assert_invalid` in core and VALID in the proposal snapshot — the corpus states this by shipping it twice with opposite verdicts. Soundness checkpoint: *a cast that admits a subtype where the spec demands exact is type confusion*, and the score cannot see it — **write the wrong answer down.** |
| **T13-T** | 64-bit tables | ~185 | ✅ **DONE 2026-08-19** — types, assembler, validator typing, elem/data offsets. Suite **62,435/378/1,825 → 62,796/306/1,466**, rate **99.4% → 99.5%**, **no file lost a pass**. `table_copy64` 0→1,676, `table_init64` 0→745, `table_get64`/`table_set64` 0→10/18, `float_memory64` 0→clean. ✅ Confirms T13-0: **`BadNumber` WAS table64** and the cluster is gone. Residual: `table64.wast` 2/2 and `call_indirect64` 0/1/1 (a `spectest.table64` import the runner does not define — harness, not engine). *(was: ◐ first slice)* — types, assembler and validator typing. Suite **62,435/378/1,825 → 62,566/342/1,696**, rate **99.4% → 99.5%**, no file lost a pass; `table_copy64` 0→1,654 passing, `table_init64` 0→745. ✅ Confirms T13-0: **`BadNumber` WAS table64** and the cluster is gone. Still open: `table_get64` 0/1/10, `table_set64` 0/1/18, `float_memory64` 0/6/84 — the interpreter side. | `table_copy64` 22, `table_init64` 93, `table_fill64` 70, `float_memory64` 84. Reverses the "tables stay 32-bit" invariant. The memory64 work is the template — *a memory carries its own index type* already exists. |
| **T13-W** | wide-arithmetic | 108 | ⚠️ **Take the wire format from the testsuite's `(module binary …)`, not by guessing** — the overlong LEBs there exist to pin those sub-opcodes. Had they been guessed wrong, **every TEXT assertion would still have passed**, because our assembler and decoder would agree with each other. Soundness checkpoint: the 128-bit halves travel as a pair of i64s, so **transposing them validates cleanly and returns wrong numbers** — pin a carry, a borrow, and a signed/unsigned pair that differs only in the high half. |
| **T13-P** | custom-page-sizes | ~39 | small, and it proves the limits plumbing. ⚠️ Enumerate every hardcoded `65536`; demand a byte-granularity out-of-bounds test. |
| **T13-M** | memory64-imports | 20 | |
| **T13-A** | custom-annotations | 8 | what is left after T9a's lexer work: 7 × "empty annotation id" + one annotation-id UTF-8 case. ⚠️ The character class is **printable ASCII plus tab/nl/CR** — not "idchars", not "non-control", not "valid UTF-8"; each of those three plausible readings passes most of the file. **A residue clustered in one rule means the rule is wrong, not that the edge cases are exotic.** |
| **T13-L** | legacy `delegate` | — | removes wasmrt's **last deliberate spec deviation**. ⚠️ **Enumerate what the blanket refusal is currently catching before deleting it** — over there the same `return error` was also the only thing rejecting a bare `(func (delegate 0))`, which the assembler happily emits and the spec calls malformed; removing it without adding the enclosing-frame rule converts a malformation into an accept-invalid. The three tests asserting the refusal are **tests of a DECISION and expire with it — rewrite them in place**, one of them has a security half that must survive verbatim. |
| **T13-S** | `sqrt` is `std`-gated | 1 | the single no_std float gap (platform libm). Only if the freestanding target needs it. |

⚠⚠ **RE-RANKED 2026-08-19 by T13-0 — the order above was WRONG, and predicting that is why T13-0 runs
first.** *(The line here read “do not fix this order now — these are estimates read off error messages,
and every previous estimate from that source was low.” It held.)* **Measured: 1,635 of 2,038 skips (80%)
are `NoTarget` CASCADES** behind **216 modules that fail to build** — ~7.6 assertions per module, so the
work-list is **216 items, not 2,038**. And the largest build-failure cause, **`BadNumber` (63 modules),
is TABLE64**: 62 of the 63 are `table_copy64` / `memory64-imports` / `table64` / `table_*64`, the
assembler refusing an `i64` table index type. **One feature wearing a parser error's name.**
🎓 *A cost logged beside a defect is a hypothesis about its CAUSE* — wrong for the fifth time: the T9
triage grouped failures by message, read “`BadNumber` 63”, and filed a **text-assembler** cluster.

**Order now:** **(1) Track T — table64**, 63 build failures + ~326 direct skips, the biggest single lever
and one coherent feature rather than six; **(2) ✅ DONE 2026-08-19 — the assembler scoring split**, ~300 assertions with **no
feature work**, independent of every track; **(3) attribute `BadForm` (46 modules) BEFORE scheduling
it**; then D, W, P, M, A, L. ⚠ **`BadForm` / `BadValType` / `BadModuleField` — 80 modules between them —
remain UNATTRIBUTED, and the correct entry for that is “unknown”, not a guess.** Full breakdown in
[testing.md](testing.md).

#### 🧾 The baseline, and what "done" means

- **A baseline is only honest if every line carries a reason** — otherwise it is a way to make a red
  number green, and adding a line to pass the build is the failure mode it invites.
- ⚠️ **"By design" is not "pass", and it is not "fail" either.** Calling an unimplementable module a pass
  is green-washing; calling it a failure overstates the defect count by an order of magnitude. It is a
  **refusal**, and it belongs in an explained baseline that gates on **regressions**.
- ⚠️ **A well-argued entry in a baseline is still an entry**, and the better the argument the longer it
  sits unexamined. **The target is an EMPTY baseline**, which is what the other runtime reached — its
  own last-standing entry carried the best reasoning in the file and was closed in about 40 lines once
  someone asked whether it had to stay.
- **Never green-wash our own gaps** — but the mirror rule earned 292 assertions: **being conservative is
  not free. It is a claim about our own ignorance, and that claim can be checked.** A mnemonic that
  exists in no wasm proposal is a malformation, and refusing it is a verdict wasmrt is entitled to give.

**Gate for T13 as a whole:** 0 failed / 0 skipped / 0 files with errors across all 284 files; the
baseline file empty; **zero deliberate spec deviations**; every new proposal carries a `Feature` flag
**and a test that the flag refuses it**; no file lost a pass at any step (per-file join, not totals); and
the four workspace surfaces + C-ABI + Miri gates green throughout.

---
## Status (2026-08-10) — PORT phase; gate OPEN, oracle **RE-BASELINED**. **T0–T8 DONE; T9 IN PROGRESS.**

**Current tree (unreleased, ahead of the published v0.9.0; numbers re-audited 2026-08-19):** suite
**62,238 / 378 / 2,038 — 99.4%** of 62,616 adjudicated, **458 workspace tests** (420 core + 28 capi +
10 CLI), Miri **28/28**. ⚠️ **The `.wat` corpus figure needs a re-run before it is quoted** — the last
recorded round trip read **534/534 assemble, 0 decode failures**, but the wasmtk tree holds **532**
`.wat` files today (the two stale `39_JstyperMixed` duplicates are gone), so every figure on record —
533/534, 534/534, 533/533 — is against a denominator that no longer exists. *(This block itself read
62,113 / 385 / 2,163 and "457 tests" until the 2026-08-19 audit — five days stale, in the roadmap's own
headline. `best-practices.md` §3A.2.)* T9's first pass landed
T9a #1/#2/#3 plus three unlisted defects, and all of **T9b (size)**, **T9c (performance)** and
**T9d (licensing/docs)**. **Fifteen further passes have landed**, closing T9a **#4 (both halves, via a
funcref that carries its owning instance), #5, #6, #7, #8, #9, #11** and most of **#12**, plus defects
the list never had: a cross-store `InstanceId` that let a guest silently share the wrong memory, and —
found while wiring #7 — **the start function, which was decoded, validated and printed but never
executed** (§4.5.5).

🔒 **The oracle is no longer at its original freeze.** `wazmrt` moved twice on 2026-08-10, both
owner-authorized and both re-baselined deliberately; `scripts/wazmrt-baseline.txt` now pins
**`wazmrt@6b7795a`** and `check-wazmrt.sh` reports NO DRIFT. The cause was a defect **both** runtimes
shared: the CLI paths that *execute* did not validate first, while the paths that merely *inspect* did.
Fixed in both, along with the diagnostic — an invalid module now reports the byte offset, the function
and expected-vs-found, **matched byte-for-byte against wasmtime 47.0.2**.
⚠️ **The 489/493 "4 skipped" in the oracle's suite is NOT unqualified** — those four are the symlink
sandbox-escape tests, which self-skip because this host denies native symlink creation. See **T12y**.

**Still open in T9:** the text-parser remainder of **#12** (`func.wast` 8), **T9e `pin`**,
**T9f tail calls DONE 2026-08-14 — every in-scope proposal is now implemented.** **All of T9a #1–#9 and
#11 are closed**, #10 stays a non-issue by design, and **`T9e pin` is the ONLY T9 item left**:
`func.wast` 8 moved to T10 with the other bugs (owner, 2026-08-14 — *"implement T9 first then tackle all
of the bugs which is T10 anyway"*). The T8 block below is the v0.9.0 release record.

### Superseded — the T8 / v0.9.0 record (2026-08-06)

**T8 (the `wasmrt.h` C ABI) landed 2026-08-06 as v0.9.0 — PUBLISHED** (release commit `a7abd83`, tag
`v0.9.0`), with all three of its gates green —
`abi_symbols.c` (74/74 symbols), `c_smoke.c` (compiled by a C compiler against the shipped header), and
**Miri** over a randomized lifecycle fuzz. wasmrt is now embeddable from C. The release also added
**proposal gating** and **configurable resource ceilings** (the owner chose real gating over
limits-only), a **`Linker` in core** shared by the C ABI / native crate / WASI / `.wast` runner, and
fixed **two silent-wrong-output defects** that work surfaced. Suite **61,033 / 738 / 3,075 — 98.8%**.
🆕 **SUPERSEDED 2026-08-19 by the 1.0 PROGRAM at the top of this file.** This line read *"Next: T9 (0.10.0) … then T10 (0.11.0), T11 (0.12.0)"*. The ladder was re-cut by the owner: **T13 conformance clear-out ships as `1.0.0` and runs FIRST**, then T9 hardening `1.0.1`, T10 bug hunt `1.0.2`, T11 optimization `1.0.3`, T12 security `1.0.4`. **clear → measure → find → optimize → attack**, in that order and for a reason (see the Definition of done).

## 🔒 ANCHOR CHANGE (owner, 2026-08-11) — the oracle is retired; finish and compete

**wasmrt no longer refers back to the `wazmrt` repo.** Everything below that reads "parity-gated",
"against the oracle" or "the frozen oracle" describes how T0–T9 were run and is **historical**. From here
the runtime is finished on its own terms and competes for inclusion in **wasmtk** and the
**universalWasmLoader-\*** runtimes on **the smallest and fastest binary**; **`rsxtk` takes wasmrt by
default** through the native Rust interface. `scripts/check-wazmrt.sh` is deleted; correctness anchors on
the spec testsuite, wasmtime and the wasmtk WASI corpus. Full rationale in
[design-decisions.md](design-decisions.md); the consequences for what to work on next are in
[vision.md](vision.md).

**The remaining plan does not change shape, but its weighting does.** Finish T9 (tail calls, `pin`,
`func.wast` 8) → **T10** bug hunt → **T11** optimization review → **T12** security review → 1.0. The
order *measure → find → optimize → attack* is still deliberate. What changes is that **T11 is now the
stage that decides the contest**, not a late polish pass, and it carries three items that were footnotes
while the oracle defined success:

1. the **unattributed ~5% steady-state regression** from the ninth T9 pass (two hypotheses tested and
   rejected — it is still unexplained);
2. **the rlib has never been measured**, and it is what `rsxtk` links — every size figure on record
   (CLI 621 KiB, cdylib 493.5 KiB, freestanding wasm32 158.1 KiB) is for an artifact the *default*
   consumer does not use;
3. **no same-machine comparison against any competing runtime** — wasm3, WAMR, or wazmrt itself.

⚠️ Note the second one is the same mistake §3.8 just cost a whole port: **measuring the thing in front of
you rather than the thing the consumer uses.** The `.wat` corpus gate ran `wat -o` and called it
verified; the size axis measures three artifacts and skips the default one.

### 🎯 T11's headline experiment — rsxtk on wasmtime vs rsxtk on wasmrt (added 2026-08-11)

`rsxtk` (`D:\Programs\_ProgramExamples\Example_Programs\Rust\rsxtk`, v0.4.4, one 463-line `main.rs`) runs
on **wasmtime 40.0.1 + wasmtime-wasi 40.0.1** today, over a **narrow** surface that wasmrt already covers
in full — see [loaders.md](loaders.md) for the call-site table. It is therefore a **real, buildable
head-to-head**, not a synthetic benchmark: same host program, same modules, one dependency swapped.

**The owner has decided `.cwasm` will not be the default** (plain `.wasm`, for cross-platform
compatibility), which is the condition that makes the comparison favourable *and* fair to run: with the
AOT cache off, wasmtime pays Cranelift compile cost per run — wasmrt's cold-start regime — while wasmrt's
accepted weakness (hot loops, ceded to a JIT) loses its head start. **Measure it; do not assert it, and
do not put it in the README until it has been run.**

Two things to fix in rsxtk while there, both found by reading it: the `component-model` feature is
enabled in `Cargo.toml` and **used nowhere in `src/`** (dead cost, and it makes wasmrt look like it is
missing a requirement it is not), and the `.cwasm` path reaches native code through **`unsafe
Module::deserialize_file`**, which cannot validate what it loads.

## Superseded status block (opened 2026-07-27, last amended 2026-08-05) — kept for the freeze record

*(Mixed dates by accretion: it starts at the freeze and was amended through T7. The **current** status is
the block above; read this one only for the gate/freeze conditions and the T5–T7 narrative.)*

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
      re-checks the canonical result against the canonical root as a compensating control. **✅ DECIDED
      by the owner 2026-08-05: accept + document.** `cap-std` was rejected (it would be wasmrt's first
      runtime dependency, against the smallest-binary axis) and an `unsafe` shim was rejected (it would
      breach the `#![forbid(unsafe_code)]` the same release introduced). Zero-dep and zero-`unsafe` both
      hold; the deployment assumption — no untrusted second process writing inside a preopen while a
      guest runs — is written up in `cmem/security-model.md`. **Do not re-litigate.**
    - **The mandated canary test is real and bites:** it asserts *no walk can produce a path that reads
      the canary* — the outcome, not an errno — over absolute, relative, chained, and symlinked-directory
      escapes. Mutation-checked: deleting the `..` guard fails it. End-to-end through the CLI, a guest
      granted one directory reads its own file and gets **76 `NOTCAPABLE`** for `../canary.txt`.
    - Deliberately **`NOSYS`, not a silent success**: `fd_allocate`, `fd_filestat_set_times`,
      `path_filestat_set_times`, and the socket calls. A guest that needs them must learn we did not
      do them. 272 workspace tests (was 254), clippy clean.
- **T8 — `wasmrt.h` C ABI. ✅ DONE 2026-08-06 (v0.9.0). ALL THREE GATES GREEN.** ~74 exported functions,
  wasmtime-*shaped* under our own names. **Decision-gate RESOLVED with the owner 2026-08-06** — the four
  answers are recorded in `design-decisions.md`; the two that changed the plan were **real proposal
  gating** (not just resource limits) and **the linker in core** (not in the C-ABI crate). `[x]`
  - **The draft did not survive contact with the code.** Four things in `docs/port/wasmrt.h.draft` were
    written before the port existed and did not match it: per-proposal config toggles (core had *no*
    feature gating, so they would have been silent no-ops), `wasmrt_linker_t` (core resolves imports
    **positionally**, by declaration order — name resolution existed only inside `wasi::link`),
    a store-attached WASI config (core builds WASI **per module**), and `wasmrt_trap_message`
    promising "+ backtrace text" (there were none until T9a#7, two stages later). Each was fixed
    rather than papered over.
  - **T8a — proposal gating (`features.rs`).** 14 flags, one per proposal wasmrt actually implements,
    **all ON by default**, plus a `Features::mvp()` floor. Gating fires at **validation**, never at
    execution, so nothing half-checked reaches the interpreter. Three things keep it honest: (1) **there
    is no `tail_call` flag** — `return_call`/`return_call_indirect` (`0x12`/`0x13`) are not in the opcode
    table at all, so the draft's `wasmrt_config_set_tail_call` would have gated *nothing* while reading
    as a security control; (2) **one `op_feature` table**, consulted once per instruction, so an arm
    cannot be gated in one place and forgotten in another; (3) **types are gated as well as opcodes** —
    `(local v128)`, a `(struct …)` type, an i64 memory and a passive segment all name a proposal with no
    instruction in sight. Incoherent sets (`gc` without `function_references`) are **reported, not
    repaired**. 14 gate vectors, each checked BOTH ways. **Spec suite unchanged** — the all-features path
    short-circuits before any gate.
  - **T8a2 — `ResourceLimits`.** The five compile-time ceilings (memory bytes, table elems, call depth,
    GC objects, exn boxes) became per-store configuration, defaults byte-identical. `max_call_depth`
    **stays 512 for oracle parity**: an embedder hitting the debug-build stack finding lowers it per
    store rather than the shipped default drifting.
  - **T8b — `Linker` in core** (owner's choice, so the C ABI / native crate / WASI / `.wast` runner share
    **one** resolution authority). Name-keyed, with namespace catch-alls (WASI routes ~45 calls by name)
    and a `define_fallback` — which is also what makes `define_unknown_imports_as_traps` implementable.
    `wasi::link` keeps its exact behaviour: the wasmtk corpus is **byte-identical across all 376
    modules**. New `Store::export_global`/`has_export`/`memory`/`global`/`func_type`/`export_index`.
  - **T8b2 — two SILENT-WRONG-OUTPUT defects the linker work surfaced** (the worst category by
    `INDEX.md`'s own standard). Exposed because `export_global` made `table.wast`'s module buildable:
    **(1) table initializer expressions were DROPPED** — `(table 3 funcref (ref.func $f))` assembled to a
    table of nulls, and the `0x40 0x00 tabletype expr` binary form was rejected as an undefined value
    type (7 `elem.wast` modules were failing on exactly that); **(2) element-segment form 4 hardcodes
    `funcref`** and has no reftype field, so `(elem (i32.const 0) (ref func) …)` on table 0 had its type
    silently rewritten — it now promotes to form 6. Fixing (1) made non-nullable table element types
    expressible for the first time, which exposed that the active-element check compared **families**
    with nullability normalized away; §3.5.9 is **subtyping**, so `funcref` no longer satisfies
    `(ref func)`.
  - **T8c/d — the header + implementation.** Two handle kinds: opaque pointers you own (one `_delete`
    each), and **checked value handles** you never free. Each value handle carries the identity of the
    store that issued it, so one from another store — or a deleted one — is **rejected rather than
    followed**; the `+1` in the packing keeps `wasmrt_func_t f = {0};` permanently invalid. **All
    raw-pointer work is confined to `ffi.rs`**, justified once and rejecting null everywhere, so the 74
    exports are ordinary safe Rust and `deny(unsafe_code)` stays meaningful instead of becoming 74
    rubber stamps. **The env-finalizer hazard is closed by construction**: an instance keeps the callback
    it linked, so the environment lives behind an `Rc` shared by the linker *and* every closure — the
    finalizer runs when the last holder goes, in whatever order the embedder deletes things.
    Other header decisions: **one `set_feature(enum)` rather than 14 named setters** (adding a proposal
    must not add a symbol); **v128 and the GC types cannot cross the boundary** — a call whose signature
    contains one returns an **error**, never a wrong value; **the trap frame API ships its final shape
    but always reports ZERO frames** (byte offsets are not recorded yet, and an approximate frame is
    worse than none — fixing the shape now avoids a breaking change at T9); `wasmrt_trap_new` was added
    after clippy found `trap_obj` dead, which exposed that the header told callbacks to return a trap
    with no way to make one. `abi_version()` 0 → **1**.
  - **T8e — the gates, all three green.** **`tests/abi_symbols.c` 74/74 symbols resolve** (link
    completeness: a function declared but not exported fails the BUILD). **`tests/c_smoke.c` PASSED** —
    compiled by clang against the shipped header, which proves what no Rust test can: that `wasmrt.h` is
    valid C and its declarations match the exported symbols. **Miri 26/26 including `lifecycle_fuzz`**,
    which drives randomized creation/use/destruction orders (including discouraged ones) and touches
    handles whose store is gone — a normal allocator cannot tell a use-after-free from a pass.
    **Mutation-verified:** deleting the store-tag check makes the cross-store test fail. Runners:
    `scripts/c-gate.sh`, `scripts/miri-gate.sh`.
  - **Conformance: 61,013/751/3,094 → 61,033/738/3,075 (98.8%)** — `table.wast` 12 failures → 2,
    `elem.wast` 17 → 13, `linking.wast` +4 passes. The one file that got worse is `i31.wast`
    (+1 visible failure, −1 skip): a module that now builds and meets the already-logged GC-const-expr
    gap. 351 workspace tests, clippy clean, all four surfaces.
- **T9 — Hardening: correctness punch-list, `pin`, licensing, docs, size, all gates green.** 🆕 **Ships as `1.0.1`, and now runs AFTER T13 (owner, 2026-08-19).** `[◐]`

  **Scoped 2026-08-06 from a measured audit**, not from old notes: every item below was re-verified
  against the current build, and each carries what it actually costs. Two were **found during that
  audit and are new**. Ordered by measured value — do them top-down.

  ### ◐ Progress 2026-08-07 — first pass done. Suite **98.8% → 98.9%**, 61,247 / 655 / 2,932.

  **Landed:** T9a #1, #2, #3 · **three defects the list did not have** (see below) · all of **T9b** ·
  all of **T9d** · all of **T9c**. 337 core tests (was 325). Clippy clean; all four build surfaces, the
  C-ABI gate (74/74 + `c_smoke`) and Miri (26/26) green. **No file in the suite regressed.**

  **Two things worth carrying forward more than the numbers:**

  1. **#1's stated cause was wrong.** `ref.null $t` was a real defect and is fixed, but it moved
     `ref_null`/`ref_test`/`ref_is_null` — **not `br_table.wast`**, whose module was failing three fixes
     earlier at `BadModuleField`. Reaching its 161 assertions took **four** independent fixes, three of
     which were not on any list: an inline table-elem shorthand that could not express a non-`funcref`
     element type; a **block type of concrete reference type being undecodable** (`0x63 <typeidx>` reads
     as s33 `-29`, an arm `read_block_type` lacked, so the type index was then read as an *opcode*); and
     **`br_table` label typing wrong in two directions** (§3.3.5.8 wants one operand sequence satisfying
     every target — the validator compared targets *to each other* and pushed the target's types back
     instead of the operands it popped). `br_table.wast`: 24/1/161 → **185/0/0**.
     ⚠️ **Every remaining cost figure in the table below was assigned the same way** — by reading a file
     for the first construct that looked unsupported. Treat them as hypotheses; re-measure after each fix.
  2. **#4 is not plumbing — it is a decision-gate.** See `known-issues.md`: a `funcref` is a bare
     function index with **no instance identity**, and `call_indirect` resolves it against the *calling*
     instance, so a shared table dispatches to the wrong function. Imported **memories** are genuinely
     just plumbing; imported **tables** need the funcref encoding decided first, and that touches a
     recorded invariant. **Do not implement imported tables without it.**

  **Still open in T9** (as of the eighteenth pass): **T9e `pin`** ONLY. ✅ T9f tail calls landed
  2026-08-14; the text-parser remainder of **#12** (`func.wast` 8) moved to T10 with the other bugs
  (owner, 2026-08-14: "implement T9 first then tackle all of the bugs which is T10 anyway"). #10 stays a non-issue by design. Note 2 above is
  now **resolved** — the funcref carries its owning instance and imported tables ship.

  ### ◐ Progress 2026-08-08 — T9a#4 memory half. Suite 61,247/655/2,932 → **61,593 / 697 / 2,469**

  **+346 passes, +42 failures, −463 skips — and no file lost a single pass.** 375 workspace tests (was
  363); clippy, all four surfaces, the C-ABI gate (74/74 + `c_smoke`) and Miri (26/26) green.

  **Owner decision (2026-08-08): option 2** — imported **memories** ship, imported **tables** stay refused
  until the funcref encoding is decided. Rationale and the three non-obvious implementation details are in
  `known-issues.md`; the short version is that the memory half needed no value-model change, so it is
  finished work rather than scaffolding.

  Measured: `imports.wast` **25/6/108 → 196/13/95** (+171, the largest single-file gain since `register`),
  `linking.wast` **55/15/78 → 107/11/28** (failures down too), `linking3.wast` 4/4/4 → 8/2/2.

  **The finding that outranks the numbers: `assert_unlinkable` had been an unconditional skip, and it was
  insuring a real defect.** Implementing it — in scope, since the T7b entry already noted it was gated on
  this work — immediately showed that **imports were never type-checked at link time**: a module importing
  `(func (param i32))` bound to a `(func)` linked and then ran with caller and callee disagreeing about the
  stack. Now checked in the two places the type is actually known: functions in `Store::instantiate` (so a
  hand-built `Imports` is checked too; a `HostFunc` has no declared signature and is still trusted), globals
  in `Linker::resolve` (because `Imports` carries a bare `Value`, which cannot say `i32` from `f32`).
  ⚠️ **A blanket skip is not a neutral placeholder** — this one was justified by a reason obsolete since
  T7b. Any category the runner declines wholesale should say what would be measured if it stopped.

  The equality-vs-subtyping choice for function matching was **measured both ways**, not assumed: exempting
  concrete GC types costs 3 correct refusals to recover 1 false one, so equality stays. Residual: 1
  assertion, logged.

  ### ◐ Progress 2026-08-08 (second) — decoder strictness. **61,593/697 → 61,691 / 599 / 2,469 = 99.3%**

  **+98 passes, −98 failures, skips UNCHANGED** — the cleanest column in `testing.md`'s table, because
  nothing changed about what gets adjudicated; 98 assertions simply started passing. `binary.wast`
  **128/88 → 208/8**, **`binary-leb128.wast` → 58/0/0 (100%)**, `.wat` corpus 532/534 → **533/534**.
  386 workspace tests. Clippy, four surfaces, C-ABI gate and Miri green.

  **This closed T9a#11 and most of #12 — and showed they were never two items.** Reading the actual
  failures instead of the logged descriptions showed one theme in two halves: *rejected at the wrong
  stage* (the validator refuses it, the decoder should have) and *module was accepted* (over-acceptance,
  the worse half, and T12-relevant since it is hostile input reaching further than it should). Seven
  checks, all at decode: section order + uniqueness (16 assertions, and a repeated section had been
  **silently replacing** the first — the silent-wrong-output class), section size (7), func/code count
  (8), bodies decoded at decode time, const-expr encodings, the `end` terminator, and the 2^32−1 locals
  ceiling. Full write-up in `known-issues.md`.

  ⚠️ **The order of sections is not the order of their ids** — `DataCount` is 12 but precedes `Code`
  (10); `Tag` is 13 but sits between `Memory` and `Global`. `SectionId::order()` is a table for that
  reason; a `>` on raw ids gets both backwards.

  **Two findings worth keeping.** *(1)* Four of our own hand-built test fixtures were **malformed
  modules** (export section after code; a function section with no code section) — always malformed,
  never in a position to be told. A permissive decoder lets bad fixtures accumulate, and they then
  encode the wrong rule. *(2)* The const-expr sweep must key on a segment's **mode**, not on whether its
  offset bytes are empty: a *passive* segment has no offset expression, but an *active* one with none is
  malformed, and the emptiness shortcut excuses the second. A test is written so the shortcut fails.

  **Cost: cold start unchanged within noise.** Bodies decode once now instead of twice later, but `Code`
  briefly held both the raw bytes *and* the IR — a second copy of every body in every module, measuring
  ~5% slower cold. Removing the redundant `body` field recovered it: same-session A/B/A gives ~4.5 ms vs
  ~4.4 ms at 48 KB, a 2–3% difference inside the recorded spread. Steady-state untouched by construction.

  ### ◐ Progress 2026-08-08 (fifth) — type canonicalisation. **61,712/578 → 61,724 / 554 / 2,466**

  **+12 passes, −24 failures; six files improved and none regressed** — `type-subtyping` 57/23 → **62/13**,
  `type-equivalence` 7/10/3 → **10/2/0**, `type-rec` 7/11 → **7/9**, `ref_cast` and `ref_test` to zero
  failures, `br_on_cast_fail` 13/3 → 15/1. Cold start unmoved (~4.59 vs ~4.69 ms A/B/A). 397 tests.

  Rec groups are the unit of type identity (§3.1.4), so `Module` gained **`type_canon`** — the lowest type
  index structurally equal to each type — computed at decode by reducing every group to a structural key in
  which a reference to a *member of the same group* becomes its **position** and one *outside* becomes the
  target's canonical id. `Module::is_subtype` compares those ids, and because **every** subtype question in
  the engine funnels through it, one line carried the fix. `call_indirect` was a **second site**, comparing
  signature `ValType` vectors by raw bits; `func_types_equal` now tries the slice compare first so the hot
  path is unchanged. `canonicalize` interns via `BTreeMap`, not a scan: rec-group count is attacker-controlled
  and O(groups²) would be a DoS on the decoder.

  🆕 **The assembler was flattening every `(rec …)` group — `0x4e` was never emitted.** Since the group IS the
  unit of identity, that silently changed what the types were, and it is why canonicalisation initially
  *regressed* `type-rec.wast`. **Third "assembler emits a different module than the text describes" defect in
  two passes** — give the emitter a dedicated look at T10 rather than waiting for a fourth.

  ✅ It let the previous pass's `decl_subtype_of` approximation be **deleted** with byte-identical results.
  ⚠️ **Measurement-tooling finding:** the per-file line prints only when `verbose || failed > 0`, so a file
  reaching **zero failures vanishes** from a non-verbose run and a line-keyed diff reads that as total loss.
  It raised exactly that false alarm here. **Diff with `-v` on both sides.**

  🔧 **Still open: cross-module type identity (~11 assertions).** A canonical id is module-local, and
  self-contained keys risk exponential blowup on chained groups; the answer is an **engine-level type registry
  on `Store`** (what wasmtime does), which is a design decision rather than a patch.

  ### ◐ Progress 2026-08-08 (fourth) — declared subtyping was never validated. **99.0% → 99.3%**

  **61,691/599 → 61,712 / 578 / 2,469.** `type-subtyping.wast` **36/44/0 → 57/23/0** (+21 passes,
  −21 failures); nothing else in the suite moved and no file lost a pass. 395 workspace tests.

  ⚠️ **T9a#6's logged cause was wrong — the third time re-measuring has caught one.** It read "GC
  subtyping depth not modelled by the validator, 36". The measured top item was **21 × "Invalid: module
  was accepted (should be rejected: sub type)"**: there was **no declared-subtype validation of any
  kind**. `module.supertypes` was populated at decode and thereafter only *walked* by
  `Module::is_subtype`, which trusts it — so the entire reference-subtyping story rested on an unchecked
  claim. Now enforced in `check_declared_subtyping`:

  - **Finality.** A type is final *by default*; only `0x50` (`sub`) opens one, `0x4f` is `sub final`, and
    a bare composite type is shorthand for `sub final ϵ`. **The decoder had been reading `0x50` and
    `0x4f` identically and discarding the distinction**, so `Module` now carries `type_finals`.
  - **Structural matching (§3.4.5).** Same kind; functions contravariant in parameters and covariant in
    results; structs append-only; each shared field matching with mutability equal and deciding the
    variance; packed fields matching only identical packing.

  🆕 **The assembler was silently turning open types into final ones.** It chose the `sub` wrapper from
  the presence of a supertype alone, so `(sub (struct …))` with no supertype emitted a **bare** composite
  type — which *means* final. The module produced was not the module the text described, and a valid
  hierarchy became invalid: the same class as element-segment form 4 rewriting a type at T8. Found only
  because the finality check started reading the flag.

  **A strict check was measured and rejected.** Refusing every pair it could not decide turned away **6
  valid** modules whose fields compare `(ref $f1)` against `(ref $f2)` from two structurally identical
  rec groups. So `decl_subtype_of` accepts the undecidable case — the **opposite** of the call made for
  cross-store import matching one pass earlier. ⚠️ **The direction to err in is a property of the
  consequence, not a house style:** there, accepting binds a call to a mismatched signature; here, it only
  preserves behaviour that already existed, while refusing breaks valid input.

  **🔧 And this measurement names the next item: there is no type canonicalisation.** wasmrt compares
  concrete types by **index**; the spec compares them by **structure**, so two structurally identical rec
  groups are one type with two indices. Every remaining failure in `type-subtyping` (23), `type-rec` (11)
  and `type-equivalence` (10) traces to it — **~40 assertions**, which makes it the largest in-scope
  cluster left and **supersedes the "residual: 1 assertion"** note from the previous pass (written before
  those files were classified). It shows up four ways at once: 15 false rejections, 11 undecidable import
  matches, ~11 wrong runtime `ref.test`/trap results, and 5 remaining over-acceptances. The fix is
  canonical rec-group identity with recursive structural equality up to rolling — **a feature, not a
  patch**, and it should be scoped on its own.

  ### ◐ Progress 2026-08-08 (third) — a cross-store `InstanceId` silently shared the wrong memory

  **Found from a constraint the owner stated, not from a test** — and it was a defect in the
  imported-memory work committed hours earlier. `InstanceId` was a bare `usize` with **no record of its
  issuing store**, so an id from store X indexed store Y's own instance vector; index 0 is always in
  range, so the import linked and the guest silently shared **Y's** memory (measured: `0x99`, its own,
  instead of X's `0x11`). It was also a **panic** — `module_of` and `export_func` indexed `code[id]`
  directly, which under `panic = "abort"` is a process kill. T12a's concern, arriving early.

  Fixed by tagging: `InstanceId { store, index }`, every accessor routed through one `Store::slot()` that
  checks the tag before the bounds. **This is the defence the C ABI already applied to its value handles
  at T8** — core held the weaker guarantee of the two, and that asymmetry *was* the bug. Store ids come
  from a `static AtomicU64` starting at 1, so a zero-initialized id can never name a real store (the same
  reasoning as the C ABI's `+1` packing); verified to build on freestanding `wasm32` no_std.
  `Store::module_of` returns `Option` now. The same hole existed for `with_instance_func` since **T7b**
  and closes with it. Mutation-verified; **conformance unchanged** at 61,712/578/2,469, because this is a
  misuse path the spec suite cannot reach — which is precisely why it survived three passes of it.

  **Two properties the same constraint implied DID hold, and are now pinned rather than assumed:** sharing
  survives a **re-export chain** (A → B → C reaches A's bytes, which only works by following B's map, not
  by allocating for B or reading a slot B never owned), and the linking graph is a **DAG by
  construction** — an `InstanceId` exists only once its instance does, so a cycle is *unrepresentable*
  rather than rejected. That is what the owner's *"only when they are not interactive does that work"* is.

  ⚠️ **A stated constraint is worth probing, not agreeing with.** Two of three properties held; the third
  had shipped broken that morning, and no spec-suite assertion could have found it.

  ### ◐ Progress 2026-08-08 (seventh) — the TEXT format's grammar. **99.1% → 99.3%**

  **61,738/536 → 61,778 / 496 / 2,466.** +40 passes, −40 failures. **`block.wast` 13 → 0, `if.wast` 13 → 0,
  `loop.wast` 13 → 0** and `type.wast` 1 → 0 — three files to zero. No file lost a pass, and the **`.wat`
  corpus held at 533/534**, which is the check that matters when tightening a *parser*. 404 tests.

  **Found by measurement, not from the list.** #12's remainder was logged as "`func.wast` 21, the text
  parser". Surveying the worst in-scope files showed `block`/`if`/`loop` at 13 failures **each with an
  identical breakdown** — the signature of one shared cause, and the largest in-scope cluster left.

  **A type use has a fixed clause order (§6.4.4):** `(type x)?` then `(param …)*` then `(result …)*`. The
  assembler collected clauses in whatever order they appeared and ignored `(type …)` outright, so
  `(block (result i32) (param i32))` assembled and the **validator** reported the result as a stack-height
  mismatch — the wrong stage, on 36 assertions. Two more from the same function: a block parameter **cannot
  be named** (only functions have local slots to name), and `(type x)` **plus** explicit clauses must
  **match** — they were silently discarded, so the module meant something the text did not say, which is the
  emitter-defect class reached from the parser side.

  ⚠️ **Where a guard lives matters: one call-level away is NO check.** The first attempt put the order rule in
  `parse_sig` and moved **one** assertion of forty, because `parse_block_type` calls `parse_sig` **one clause
  at a time** — the order state was built and destroyed per clause and could never observe a sequence. The
  check had to move to the loop that iterates. Only the measurement said so.

  **Still open in this cluster:** `func.wast` 17 and `call_indirect.wast` 7 — duplicate **identifiers**
  (locals, funcs, types) are a distinct rule from clause order and were not touched.

  ### ◐ Progress 2026-08-08 (eighth) — the same type-use rules at their other two sites

  **61,778/496 → 61,802 / 472 / 2,466.** +24 passes, −24 failures. **`call_indirect.wast` 158/11 →
  169/0/0**, **`func.wast` 147/21 → 160/8**. No file lost a pass, none gained a failure, `.wat` corpus
  held at 533/534. 405 tests.

  **Block types, `call_indirect` and function definitions each had their OWN COPY of the type-use loop**,
  and therefore its own copy of all three defects (clause order, named parameter, inline signature silently
  overridden). `parse_type_use` is now the single authority for the first two; the function-definition path
  enforces the same order inline, because its loop also owns `import`/`export`/`local` and the body and so
  cannot delegate wholesale. **Two copies of a grammar drift — three copies drifted identically**, which is
  a better argument for one authority than any reasoning about it.

  ⚠️ **One rule was attempted, MEASURED, and WITHDRAWN — and it looked obviously right.** "No declaration
  after the body begins" (`(func (nop) (local i32))` is malformed) broke **`select.wast`, `stack.wast` and
  `call_indirect.wast`**: in **flat** instruction form each immediate is its own top-level item, so
  `select (result i32)` and `call_indirect (type )` put a `result`/`type` form exactly where a misplaced
  declaration would sit. Keyword scanning cannot tell an immediate from a declaration at that layer.
  Withdrawn rather than forced; ~4 assertions stay open, worth revisiting only with a body-structure-aware
  pass. **A rule that is obviously right is still a hypothesis until it is measured.**

  ### ◐ Progress 2026-08-08 (tenth) — T9a#5, GC constant expressions. **Logged 6, delivered 88**

  **61,887/457 → 61,975 / 453 / 2,247.** +88 passes, −4 failures, **−92 skips**. `i31.wast` **0/6/66 →
  61/2/5**, `array.wast` 6/2/43 → **18/2/29**, `struct.wast` 6/3/17 → **21/3/0**. Nothing regressed.
  411 tests.

  Six forms in both the validator and the interpreter: `struct.new`, `struct.new_default`, `array.new`,
  `array.new_default`, `array.new_fixed`, `ref.i31`. **The same six on both sides**, deliberately — a
  validator that accepts what the evaluator rejects (or the reverse) is the disagreement class that once
  made `v128.const` in a const-expr a false rejection. `eval_const_expr` takes an
  `Option<(&Module, &mut Pools)>` for the field layouts and the heap, passed as `None` at the one site
  that cannot produce a reference -- a segment *offset* is an integer.

  ⚠️⚠️ **THE MEASUREMENT LESSON IN A NEW DIRECTION: a cost counted in FAILURES understates any defect that
  stops a module BUILDING.** `ConstantExpressionRequired` on a global initializer fails the whole module,
  and every later assertion in that file then has no target and is **skipped**. `i31.wast` was 0 passed /
  6 failed / **66 skipped** — the 6 was the visible cost, the 66 the real one, and the three GC files
  carried 126 skips between them. **Read the skip column when triaging**, especially for a defect sitting
  in a module-level position: a global initializer, a type definition, a section.

  ### ◐ Progress 2026-08-08 (eleventh) — T9a#7 trap backtraces, **+ the start function never ran**

  **61,975/453 → 61,987 / 441 / 2,247 = 99.3%.** +12 passes, −12 failures, **nothing regressed** —
  and every one of those 12 came from the *second* item, not the one on the list. **426 tests**
  (398 core + 28 capi). Miri **28/28**. CLI size **unchanged to the byte**; cdylib **+512 B**.

  **T9a#7 — trap backtraces, real frames end to end.** Three decisions, each measured:

  1. **Where the byte offset lives.** `Instr` gained a `pub offset: u32` that is **free**: `Imm` is 64
     bytes at 16-byte alignment, so `Instr` was already 80 bytes with 15 of padding after the one-byte
     opcode. A test pins `size_of::<Instr>() == 80` so that an optimization pass which shrinks `Imm`
     (a live T11 candidate) **fails loudly** instead of silently starting to pay 16 bytes per
     instruction. The alternatives — a parallel `Vec<u32>`, or restoring the `Code::body` bytes removed
     in T9b at a ~5% cold-start cost — were both rejected on that basis.
  2. **How the pc escapes `run`.** ⚠️ **THE LESSON OF THIS PASS: the plumbing for a cold feature can
     cost the hot path, so measure the plumbing.** `run` has 51 `return Err` sites plus a long tail of
     `?`, so per-site recording is untenable; the obvious fix is `pc: &mut usize`. **Measured, that
     cost 3.6%** (2160 ms vs 2083 ms on the steady-state loop, bracketed A/B/A) — the deref does not
     survive the opaque calls in the loop body. Wrapping the loop in a **one-shot closure** keeps `pc`
     a plain local that LLVM holds in a register while still leaving it readable after any exit:
     re-measured at **2087 ms against a 2083–2095 baseline band**, i.e. free. A diagnostics-only
     feature paying 3.6% of the "fast" axis forever would have been a bad trade made invisibly.
  3. **When frames are built.** On the way **out**, one per frame as the error passes through
     `call_function` — not as a shadow stack maintained during execution. A shadow stack costs a push
     and a pop on every call whether or not anything ever traps; this costs nothing until something
     does. `Pools.backtrace` is cleared per invocation and **when EH catches**, so a caught exception
     never leaves its frames for the next real trap to inherit.

  The C ABI's frame API was frozen in its final shape at T8 and stubbed to 0 frames; it is now live
  with **no ABI change**, exactly as designed. A `wasmrt_trap_t` **copies** the frames, because it
  outlives the single backtrace the engine keeps. Offsets are reported **absolute from the start of
  the module** — the form `wasm-objdump` prints — so a consumer needs no rebasing.

  ### 🆕 The start function was decoded, validated, printed — and never executed

  **Found while tracing where an instantiation trap gets its frames**, by asking who runs
  `Module::start`. Nobody did. §4.5.5 step 11 requires it to run as the last step of instantiation;
  wasmrt's `Store::instantiate` simply returned. `(start $f)` parsed, assembled, type-checked, and was
  printed by `wasmrt <file>` — every stage but the one that matters.

  **This is the silent-wrong-output class again**, and the worst instance so far: a module whose entire
  initialization is a start function ran with every global at its declared default and every effect
  missing, returning a plausible answer with no error anywhere. Fixed in `instantiate`, after the data
  and element segments so it can observe them (a test pins that ordering, since running it earlier
  still passes the naive test). A trap in it fails the instantiation.

  `start.wast` **8/7 → 15/0**, `start0.wast` 5/3 → **8/0**, `linking.wast` +1, `linking3.wast`
  **11/1 → 12/0**. Five core tests added.

  ⚠️⚠️ **THE LESSON: 10 assertions in a file literally named `start.wast` sat failing for five
  releases.** The triage habit had been to read failures for a *diagnosis* — what does the message say
  — and these said nothing useful. The question never asked was the simpler one: **does this file's
  NAME describe a feature, and does that feature work at all?** A feature can be fully decoded, fully
  validated, and reported by the CLI while never executing; every stage that *inspects* it passes, so
  only running it finds out. **Before diagnosing a file's failures, spend one minute asking whether its
  headline feature works end to end.**

  ### ◐ Progress 2026-08-08 (twelfth) — the text format's source character set (§6.2/§6.3)

  **61,987/441/2,247 → 62,037 / 393 / 2,245 = 99.4%.** +50 passes, −48 failures, and 2 assertions
  became adjudicable. **`id.wast` reached 6/0/1 — a file at zero failures**; `annotations.wast`
  12/51 → **56/8**. No file lost a pass; the `.wat` corpus held at **533/534**, which is the check
  that matters when *tightening* a parser. 435 tests.

  **How this got picked is the point.** The remaining punch-list items were #8 (1 file), #9 (2 files)
  and `func.wast` 8. Surveying the failures by *cause* instead — §1.2 — put text-assembler errors at
  the top by a wide margin (`BadNumber` 63, `BadForm` 49, `BadValType` 18, `BadModuleField` 16), and
  the single worst file was `annotations.wast` at 51, which **T9g had written off as an untargeted
  proposal**.

  ⚠️ **That scope note was wrong in the way §1.1 describes, applied to scope rather than cost.** Four
  one-line probes, none containing an annotation, were all accepted:

  ```
  (module (func $a\x01b))    control character in an identifier   -- not an idchar
  (module (func $a\xffb))    invalid UTF-8 in an identifier       -- source is Unicode
  (module (data "\x01"))     raw control byte in a string         -- stringchar needs c >= U+20
  (module (func) ;;\x01 )    control character in a comment       -- LEGAL, linechar allows it
  ```

  Three defects, one non-defect. **`lexer` accepted them all**, so the gap was generic and the file
  that exposed it merely happened to be the proposal's. The confirmation is `id.wast`: nothing to do
  with annotations, 5 failures, now zero.

  What landed, all in `sexpr.rs`:
  - **`idchar` enforcement in `parse_atom`.** It consumed any non-delimiter byte and then ran
    `from_utf8_lossy` — 🆕 **which did not merely over-accept, it silently rewrote**: `$a\xffb` and
    `$a\xfeb` both became `$a\u{FFFD}b`, so two distinct identifiers collided on one name. Restricting
    to `idchar` makes the slice ASCII by construction, so the conversion cannot lose anything.
  - **`stringchar` (§6.3.3)** — a raw byte `< U+20` or `== U+7F` in a string is malformed; those
    bytes reach a data segment through **escapes**, which a test pins so the rule cannot break
    `(data "\00…")`.
  - **UTF-8 validity for raw non-ASCII in strings** — `"héllo"` is legal, a lone `\xff` is not.
  - **`id ::= '$' idchar+`** — a bare `$`, `$""` and `$ "a"` all name nothing. The quoted form carried
    the *same* `from_utf8_lossy` rewrite, so `$"\ef"` was accepted **and renamed**.
  - **The charset rule reaches inside a skipped annotation.** Ignoring what an annotation *says* is
    what the proposal requires; exempting its bytes from being source is not.

  ⚠️ **The comment case was measured and left alone.** `linechar ::= c:char (if c ≠ U+0A)` admits any
  character but a newline, so a control byte in a comment is legal — one probe of the four was not a
  defect, and a test now pins that it stays accepted. **Tightening what the grammar permits is the
  same error as accepting what it forbids**, and the only thing separating them was reading the
  production.

  ### ◐ Progress 2026-08-08 (thirteenth) — T9a#8. **Logged at 1 file, delivered 76 assertions**

  **62,037/393/2,245 → 62,113 / 385 / 2,163 = 99.4%.** +76 passes, −8 failures, **−82 skips**. Four
  files reached zero: `call_ref.wast` 4/4/27 → **31/0/0**, `br_on_null.wast` 1/3/6 → **7/0/0**,
  `ref_as_non_null.wast` 1/1/4 → **5/0/0**, `unreached-valid.wast` 1/2/9 → **10/0/0**;
  `return_call_ref.wast` 10/5/36 → 40/7/0. No file lost a pass. 438 tests.

  🆕 **The entire `.wat` corpus now assembles — 534/534 for the first time**, with **zero** decode
  failures on a full assemble→decode→validate round trip. Only the two `39_JstyperMixed` files (T9a#9)
  fail, at validation.

  **The defect.** `immediate_arity` ended in `_ => 0` and the emitter's match ended in `_ => {}`, so an
  instruction nobody had listed was emitted as a **bare opcode with its operand left in the token
  stream**. Four were in that state: `call_ref` and `return_call_ref` (a type index each),
  `br_on_null` and `br_on_non_null` (a label each). All four are in the opcode table, decode
  correctly, and run correctly — only the *assembler* did not know they take an immediate.

  ⚠️ **Not one of the three symptoms said "the assembler does not know this instruction."**
  `call_ref` folded produced a module the **decoder** rejected for a missing `end` (the operand shifted
  every following byte); `call_ref` flat reported `UnknownInstr` about the *next* token; `br_on_null`
  did the same, which is why `gc-linked-list.wat` had sat as an unexplained "1 of 534" separate from
  #8 entirely. **One cause, three unrelated-looking bugs, in two separately-logged items.**

  ⚠️ **Logged as "1 wasmtk file, undiagnosed"; it was worth 76 spec assertions** — §1.3 again, and the
  reason is the same: a module that will not build takes its whole file's assertions into *skips*, and
  the punch-list entry counted only the one file someone had happened to notice.

  **The generalization — T10a's field-coverage sweep, now landed.** `Op::from_u8` makes the opcode
  space enumerable, so a test walks all 256 single-byte opcodes and asserts: **if the decoder reads an
  immediate for an op, the assembler must write one** (and the converse). The decoder is the right
  oracle because it is the half that defines the binary format. Ops with bespoke emitters — block
  types, `br_table`, `call_indirect`, `select_t`, memargs — are listed explicitly, so a *new* op is
  neither generic nor special until someone classifies it.

  ⚠️ **Mutation-verified — and the first attempt lied.** A `perl` substitution silently failed to
  match, the test passed, and the obvious reading was "the sweep is decoration". Re-doing it with
  `sed` **and grepping to confirm the line was gone** made it fail and name all four ops exactly.
  **A no-op mutation and a worthless check produce the same observation** (§4.2a).

  ### ◐ Progress 2026-08-11 (sixteenth) — the CLI could not run a `.wat` file at all

  **Suite unchanged at 62,113 / 385 / 2,163 = 99.4%.** 456 tests (was 450). The `.wat` corpus is a clean
  **533/533** through assemble→decode→validate. Again the correct outcome: the spec suite drives the
  engine directly and never touches the CLI's file loading.

  `wasmrt run prog.wat f` answered *"not a WebAssembly binary (bad magic)"*, and so did
  `wasmrt wasi prog.wat` and plain `wasmrt prog.wat`. The assembler had been in the same executable since
  T6, reachable only as a separate `wasmrt wat -o out.wasm` step. **The oracle assembled `.wat` on its run
  path from its first release** — so this was a port/oracle divergence of a kind the earlier ones were not:
  not a wrong answer, an **absent** one.

  All three module loaders now share one `read_module_bytes` helper. Three copies of the sniff is precisely
  how `run` and `wasi` drifted into different validation behaviour the day before (§3.4), so the fix is one
  helper rather than three call sites doing the same thing.

  Two decisions worth keeping:

  1. **Dispatch on the extension, not the content.** Content-sniffing looks more robust and is worse here:
     it would hand a corrupt *binary* to the assembler and report a syntax error for it. With the extension
     test, a malformed `.wat` reports `cannot assemble` and a malformed `.wasm` reports `decode failed` —
     the stage blamed stays honest (§3.6, §3.7). Pinned by a test that checks both messages.
  2. **Validation still runs, on the assembled bytes**, pinned by a test asserting the full wasmtime-shaped
     diagnostic survives the text path. A new input format must not become a side door around the check the
     fifteenth pass had just installed.

  ⚠️⚠️ **Found by the owner asking whether the runtime ran `.wat` files — the second finding in two days
  from an owner question rather than a test.** That is structural, not luck: every gate here compares
  *answers* on inputs **both** runtimes accept, so a capability one runtime does not offer generates nothing
  to diff. Written up as **`best-practices.md` §3.8**, with the remedy — enumerate the oracle CLI's
  subcommands, file types and flags as a table and check the port entry by entry, which is §3.4's
  enumerate-don't-reason applied to the outside of the tool. 🚦 Filed for **T12x**, which already carries
  "diff the two runtimes' tables against each other".

  🆕 Also fixed in passing: `print_help`'s `--ro-dir` line had lost its `\n\` continuation when
  `--allow-symlink` was added, so the help text printed a stray hard-wrapped line.

  ### ◐ Progress 2026-08-10 (fifteenth) — `wasmrt run` executed WITHOUT VALIDATING. **Both runtimes did**

  **Suite unchanged at 62,113 / 385 / 2,163 = 99.4%.** 450 tests (was 442). No conformance movement is
  the correct outcome: the spec suite cannot reach a CLI entry point.

  ⚠️⚠️ **Found by the owner questioning a claim, not by a test.** Resolving T9a#9 I wrote that "the
  oracle's execution path skips validation" as though it were wazmrt's peculiarity. Asked to justify it,
  the check that should have come first showed **wasmrt had the same hole**:

  | entry point | before | now |
  | --- | --- | --- |
  | `wasmrt <file>` (summarize) | ✅ | ✅ |
  | `wasmrt wasi` | ✅ | ✅ |
  | **`wasmrt run`** | ❌ **executed unvalidated** | ✅ |
  | C ABI `wasmrt_module_new` | ✅ | ✅ |
  | Rust `Instance::new` | ⚠️ documented precondition | ⚠️ by design, documented properly |

  `wasmrt run ill-typed.wasm f` printed `1` and exited **0**. §4.5.1 defines instantiation only for a
  *valid* module. **The asymmetry was the bug** — second instance of `best-practices.md` §3.4, after the
  C ABI holding a store tag core lacked at T9a#3. Severity is bounded by `forbid(unsafe_code)` to a
  wrong answer, never memory unsafety (a type-confusion probe trapped cleanly) — but wrong-answer is the
  class this project ranks worst.

  **The oracle had it worse, and was fixed concurrently** (`wazmrt@baf0a38`): its summarize and `.wast`
  paths validated while **both** execute paths *and* `wasm_module_new` did not. Its own source recorded
  part of the cost — an export index reaching a `.?` was *undefined data in ReleaseFast, a segfault from
  a 31-byte module* — patched defensively at the one site noticed, root cause left in place. Zig's
  ReleaseFast/ReleaseSmall remove the checks that make such a slip survivable, and the C ABI is the
  embedding surface, so that is where it mattered most.

  **Then the diagnostic, decided by measuring wasmtime rather than arguing the API** (`wazmrt@6b7795a`
  + wasmrt). The owner's rule: base it on what wasmtime does. wasmtime 47.0.2 refuses with
  `Invalid input WebAssembly code at offset 33: type mismatch: expected i32, found i64` — so both
  runtimes now match that action. **The offset is byte-identical across all three tools** because all
  three count from the start of the module; verified side by side on two modules (offsets **33** and
  **61**) and pinned as tests. Implemented as a side channel in both, not a widened error type: in Rust
  because `ValidateError` is `Copy`, exhaustively matched and crosses the C ABI; in Zig because an error
  set **cannot** carry a payload at all. `Instr.offset` (free padding, T9a#7) paid for its second
  feature here; wazmrt derives the offset by re-decoding one body on the cold path instead, the same
  trick `Instance.frameOffset` already used.

  ⚠️ **Method lesson, now in `design-decisions.md`: when a decision is to be based on a reference
  implementation, RUN it.** "What does wasmtime's `Module::new` do" invites answering from memory; three
  lines through the real binary settled it in seconds *and* produced two numbers to assert against.

  ### 🔒 Then the symlink-creation policy (owner, 2026-08-10)

  Chasing the same question into the **rights masks** found a real hole in the oracle and produced a
  policy decision in both.

  ⚠️ **wazmrt had no `path_symlink` right at all** — the bit list jumped `1 << 23` to `1 << 25`. So
  `write_mask` could not strip it and the handler demanded `path_open`, a READ right: **a guest could
  plant symlinks inside a `--ro-dir` read-only preopen.** wasmrt was correct throughout
  (`PATH_SYMLINK = 1 << 24`, in `WRITE_MASK`, demanded by `path_symlink`). Fixed in `wazmrta6d745`.
  ⚠️⚠️ **wazmrt's own read-only test passed the whole time** — it asserts `ro & write_mask == 0`, which
  is **trivially true for a right that is not in the mask**. The guard and the gap shared a blind spot.

  **The owner's policy:** *"symlink creation during runtime for the purpose of running processes shall
  be illegal; creating symlinks for a program install situation should be legal."* So `--dir` now grants
  `READ_WRITE = ALL & !PATH_SYMLINK` and `--allow-symlink` opts back in. The premise is sound: composing
  modules over shared linear memory is the **store's** job (imported memories are genuinely shared since
  T9a#4), so a workload run never needs new links on disk — and denying creation shrinks what an
  external process could later repoint, which is what makes the accepted TOCTOU residual survivable.

  **Enforced at COMPILE TIME** (`const _: () = assert!(…)`), because every operand is a constant: a
  violation fails the **build**, including for a crate depending on `wasmrt-core`, which a `#[test]`
  would not. Mutation-verified. ⚠️ It governs **creation, not traversal** — following a pre-existing link
  needs `PATH_OPEN`, which both grants keep, and that is asserted too.

  🚦 Three T12 items came out of this — **T12x** (diff the two runtimes' security tables against each
  other; both of today's defects were "the property holds in one and not the other, and nothing compared
  them"), **T12z** (sweep an invariant across every entry point, because the
  bug is never "nobody does X" but "three of the four do X") and **T12y** (the oracle's sandbox-escape
  tests do not run on this host).

  ### ◐ Progress 2026-08-08 (fourteenth) — T9a#9 is NOT a defect. **The right outcome was to change nothing**

  **Suite unchanged at 62,113 / 385 / 2,163 = 99.4%.** 442 tests (was 438) — all four new ones pin a
  conclusion rather than a fix.

  #9 read: *"`39_JstyperMixed.wasm.{rt,roundtrip}.wat` → `TypeMismatch`, oracle assembles **and runs**
  them — so this is our type-checker being wrong, not the input."* Every observation in it was true.
  The conclusion was wrong three times over:

  1. **The module is genuinely ill-typed.** Defined function #6 is `if (result f64)` with **both arms
     pushing `i32`** — §3.3.5, and `if.wast`'s `type-then-value-num-vs-num` is an `assert_invalid` for
     precisely that shape. Our `if.wast` is at 0 failures, so wasmrt already enforces the rule.
  2. ⚠️⚠️ **"The oracle runs them" was never evidence of validity.** `wazmrt <module> <export>` decodes
     and executes **without validating**; `wazmrt <module>` summarizes **and validates**. Through the
     second path the oracle reports `validation: FAILED — TypeMismatch` on the very same construct —
     **it agrees with wasmrt**. Confirmed with a blatant control (`(func (result i32) i64.const 1)`):
     the run path printed a result, the summarize path caught it.
  3. **The fixture is stale and double-counted.** The two files are **byte-identical**, so it was one
     fixture reported as two; and it has **8 functions / 10 types** where the source
     `39_JstyperMixed.wasm` has **14 / 13** — it is not a round trip of that binary at all. The real
     binary and the hand-written `.wat` both validate **OK**.

  **What actually landed** — the diagnostic whose absence made this cost so much. `TypeMismatch` named
  no location, so localizing the failure to one of nineteen bodies took a temporary probe. Validation
  failures now carry the function index (`validation FAILED in function 8: …`), via a thread-local
  rather than a change to `ValidateError` — the error type is `Copy`, matched exhaustively in several
  places, and crosses the C ABI, so widening it for a diagnostic would be a breaking change. Cleared
  on entry, so a module-level failure reports **no** location instead of inheriting the last one.
  `no_std` returns `None`: it costs a thread-local and a freestanding embedder has nowhere to print it.

  ⚠️ **The lesson is about citing a reference implementation: name the subcommand.** "The oracle
  accepts it" is only evidence if you know which path you invoked, and a runtime that *executes* an
  invalid module is over-permissive, not authoritative. A three-line reduction through both tools
  settles it in a minute; this had been undiagnosed since T7. Recorded as §2.3a — the sibling of
  §1.5a, which is the same error made about our own gate.

  ### T9a — Correctness defects (real bugs) `[◐]`

  **Status 2026-08-08:** #1 ✅ (plus 3 unlisted defects it uncovered) · #2 ✅ · #3 ✅ · **#4 ✅** ·
  **#5 ✅** · **#6 ✅** · **#7 ✅** · **#8 ✅** · **#9 ✅ (NOT a defect — the fixture is invalid)** · **#11 ✅** · **#12 ◐ (binary half done;
  the text-parser remainder moved to `wat.rs`)** · #10 not a defect ·
  **#13 ✅ (unlisted — the start function never ran)**.
  Measured moves: `br_table` 161 skips → 0 · `memory_size` 16 fails → 0 · `memory_grow` 2 → 0 ·
  `store1` 4 → 0 · `ref_is_null` 1 → 0 · `i31` 31 → 6 · `load1` 15 → 5 · `type-subtyping` 20 skips → 8 ·
  `imports` +171 passes · `linking` +52 passes / −4 failures.

  | # | Defect | Where | Measured cost |
  | --- | --- | --- | --- |
  | 1 | **`ref.null $ConcreteType` rejected by the assembler.** The `O::RefNull` arm matches only the *abstract* heap types and its `_ =>` returns `BadImmediate`. A concrete heap type is legal and encodes as a **positive s33 type index** — the same encoding `(ref $t)` already uses — so the fix is to fall through to type-name resolution. | `wat.rs` ~3059 | **161 skipped assertions in `br_table.wast` alone** (line 1052 is `(br_table … (ref.null $t) …)`, so the file's single module fails to build and *every* `assert_return` in it is skipped) + the only 2 of 534 wasmtk `.wat` files we cannot assemble. **The largest concentrated win available**, and it was previously logged as a cosmetic 2-file gap. |
  | 2 | 🆕 **`Op::MemorySize` reads ANOTHER instance's memory — SILENT WRONG OUTPUT.** It indexes `store.memories` with the **raw module-local immediate**, never routing through `ctx.maps.mem()`. `Op::MemoryGrow`, one line below, does it correctly. **The fourth instance of the shared-store defect class** (after `CallIndirect`, `exec_memory_init`, and the assembler shorthands) and the *only* remaining unmapped pool access — verified by auditing every `store.{memories,tables,globals,elem_values,data_dropped,elem_dropped}.get*` site. Clippy's `unused variable: maps` could not fire, because the same function's `MemoryGrow` arm does use `maps`. | `interp.rs` 2374 | `memory_size.wast` 16 failures — all four of its modules report `5`, which is *module 1's* page count. A core MVP instruction returning another module's answer. **Fix + a two-instance regression test.** |
  | 3 | 🆕 **The `.wast` runner redirects a failed module's assertions to an unrelated earlier module.** When a build fails, `current = None` — and `target(None)` then falls back to `self.named.last()`. So assertions belonging to the module that failed silently run against a *different* instance and are reported as **value mismatches**. The fallback itself is wanted (a file naming every module must still run bare actions); it must simply not apply after a *failed* build. | `wast.rs` ~290 | Inflates **failures**, never passes — so 98.8% is if anything understated. The real damage is diagnostic: `load1.wast` reports "got 0x0, expected 1", which sends you hunting a load bug that does not exist. Fix by tracking "the last build failed" distinctly from "there is no unnamed current module". |
  | 4 | ✅ **DONE 2026-08-08, both halves.** Memories first (`Linker::define_memory`, shared never copied). Then **tables**, once a `funcref` carried its **owning instance** (bits 62..32 — bit 63 is `I31_TAG`): `call_indirect`/`call_ref` now dispatch into the reference's *owner*. ✅ **Instance 0 packs to the bare index**, so the encoding alone moved the suite +1/−1 — a value-model change arranged to be verifiably a no-op. 🆕 Also caught that a table's/memory's *instance* type has `min = CURRENT size`, not the declared one. | `interp.rs`, `linker.rs`, `wast.rs` | Was `imports.wast` 108 skips + `linking.wast` 80 skips/16 fails. **Now `imports.wast` 230/13/26, `linking.wast` 131/4/8, `elem.wast` 75/6/0, `table_grow.wast` 50/0/0.** |
  | 5 | ✅ **DONE 2026-08-08.** Six forms in both validator and interpreter — `struct.new`, `struct.new_default`, `array.new`, `array.new_default`, `array.new_fixed`, `ref.i31` — the **same set on both sides**, so the two cannot disagree about what a constant expression is (the `v128.const` false-rejection class). `eval_const_expr` takes an `Option<(&Module, &mut Pools)>`, `None` at the offset site that cannot produce a reference. | `validate.rs`, `interp.rs` | ⚠️ **Logged cost 6; real value 88.** A rejected global initializer fails the whole MODULE, so every later assertion in the file was skipped: `i31.wast` **0/6/66 → 61/2/5**, `array.wast` 6/2/43 → **18/2/29**, `struct.wast` 6/3/17 → **21/3/0**. **A cost counted in failures understates a defect that stops modules building — read the skip column.** |
  | 6 | ✅ **DONE 2026-08-08, in two halves — and the logged cause was WRONG.** Not a missing depth model: there was **no declared-subtype validation at all**. Half one: finality (the decoder read `0x50`/`0x4f` identically) + §3.4.5 structural matching. Half two: **type canonicalisation** — rec groups reduced to structural keys, `is_subtype` comparing canonical ids, and `call_indirect` no longer comparing signatures by raw bits. Together they caught **two assembler defects**: open types emitted as final, and every `(rec …)` group flattened (`0x4e` never emitted). | `validate.rs`, `module.rs`, `wat.rs`, `interp.rs` | `type-subtyping.wast` 36/44 → **62/13**; `type-equivalence` 7/10/3 → **10/2/0**; `ref_cast`/`ref_test` → 0 failures. Residual: **cross-module** identity, ~11, needing a `Store` type registry. |
  | 7 | ✅ **DONE 2026-08-08.** The byte offsets deferred at **T2**. `Instr.offset: u32` rides in existing padding (pinned by a `size_of` test); frames are built **on the way out** through `call_function`, not as a shadow stack; the frozen C ABI went live with **no ABI change**. ⚠️ The obvious `pc: &mut usize` plumbing **measured 3.6% slower** on the steady loop — a one-shot closure recovered it. | `opcode.rs`, `interp.rs`, `capi` | Diagnostics only (0 suite assertions, as predicted) — but tracing where an *instantiation* trap gets its frames is what uncovered #13. |
  | 13 | 🆕 ✅ **DONE 2026-08-08 — the start function never ran. SILENT WRONG OUTPUT.** §4.5.5 step 11. `Module::start` was decoded, validated, assembled and printed by the CLI; nothing executed it. A module initialized entirely by `(start $f)` returned defaults with no error at any stage. Fixed in `instantiate`, **after** the segments so it can observe them; a trap in it fails the instantiation. | `interp.rs` | **+12 assertions.** `start.wast` 8/7 → **15/0**, `start0.wast` 5/3 → **8/0**, `linking3.wast` 11/1 → **12/0**, `linking.wast` +1. **10 of them sat in files named for the feature for five releases** — see the lesson above. |
  | 8 | ✅ **DONE 2026-08-08 — the assembler did not know four instructions took immediates.** `immediate_arity` ended in `_ => 0` and the emitter in `_ => {}`, so `call_ref`, `return_call_ref`, `br_on_null` and `br_on_non_null` were emitted as **bare opcodes with their operand left in the token stream**. All four decode and run correctly; only the assembler was wrong. ⚠️ None of the three symptoms named the assembler — folded `call_ref` produced a module the *decoder* rejected for a missing `end`, flat `call_ref` and `br_on_null` reported `UnknownInstr` about the *next* token. A **field-coverage sweep** over all 256 single-byte opcodes (T10a's, now landed) asserts the decoder and assembler agree about which ops take immediates. | `wat.rs` | ⚠️ **Logged at "1 wasmtk file"; delivered 76 assertions** (§1.3 — the punch list counted the one file someone noticed, not the skips). `call_ref.wast` → **31/0/0**, `br_on_null.wast` → **7/0/0**, `ref_as_non_null.wast` → **5/0/0**, `unreached-valid.wast` → **10/0/0**, `return_call_ref.wast` 10/5/36 → 40/7/0. 🆕 It also fixed `gc-linked-list.wat`, the long-standing "1 of 534" logged *separately* — **the `.wat` corpus now assembles 534/534 for the first time**. |
  | 9 | ✅ **RESOLVED 2026-08-08 — NOT A DEFECT. wasmrt is correct and the fixture is invalid.** The module contains `if (result f64)` whose **both arms push `i32`** — ill-typed by §3.3.5, and `if.wast`'s `type-then-value-num-vs-num` makes exactly that an `assert_invalid`. ⚠️ **The logged premise was the error**: "the oracle assembles *and runs* them" is true, but `wazmrt <module> <export>` **does not validate**. Through `wazmrt <module>`, its *validating* path, the oracle reports `validation: FAILED — TypeMismatch` — **agreeing with wasmrt exactly**. Also: the two files are **byte-identical duplicates** (one fixture counted twice) and **stale** — 8 functions / 10 types against the source binary's 14 / 13, so not a round trip of it at all. The real `39_JstyperMixed.wasm` and the hand-written `.wat` both validate **OK**. | *(no change needed)* | **0 assertions — the correct outcome was to change nothing.** A test pins the construct so it is not re-opened. See `best-practices.md` §2.3a. |
  | 10 | **`wasmrt_caller_get_memory` always returns `false`.** A durable handle must be tagged against a live store, and during a callback the store is mid-borrow. Callbacks use `wasmrt_caller_read`/`_write` instead — the shape the loaders actually need. | `capi/src/lib.rs` | None today. Revisit only if a loader needs the handle form. |
  | 11 | ✅ **DONE 2026-08-08.** Malformed modules were rejected at the wrong STAGE — during *validation* where the decoder should have refused them. Seven decode-stage checks landed (section order/uniqueness, section size, func/code count, bodies decoded at decode, const-expr encodings, the `end` terminator, the 2^32−1 locals ceiling). | `module.rs`, `opcode.rs`, `types.rs` | Was `binary-leb128.wast` 15 → **58/0/0, a file at 100%**. |
  | 12 | ◐ **Mostly DONE 2026-08-08 — and it was never "mixed symptoms".** Reading the actual failures showed #11 and #12 were **one theme in two halves**: wrong-stage rejection, and outright over-acceptance. `binary.wast` **128/88 → 208/8** (both copies). `load1.wast` was indeed explained by #3, as suspected. **What remains is a different cluster: the TEXT parser**, `func.wast` 21 — "unexpected token" 9, "inline function type" 3, "duplicate local" 3, "duplicate func" 1, 2 wrong results (`0x2a` where `0` expected), 2 malformed imports still accepted. | `wat.rs` (not `module.rs`) | ~78 → **~21, and they moved crate-module**. |

  ### T9b — Size (the "small" axis) ✅ MEASURED 2026-08-07 `[x]`
  - **Unconditional `data_count` section — FIXED.** Emitted only when `memory.init`/`data.drop` actually
    appear, tracked at the emission site. Zero conformance drift across all 284 files.
  - **The first size numbers wasmrt has ever recorded** (release profile: `opt-level="z"` + LTO +
    `codegen-units=1` + strip + `panic="abort"`; host `x86_64-pc-windows-gnullvm`):

    | Artifact | Size | + `wasm-opt -Oz` |
    | --- | ---: | ---: |
    | CLI `wasmrt.exe` | **621.0 KiB** (635,904 B) | — |
    | `wasmrt_capi.dll` (cdylib) | **493.5 KiB** (505,344 B) | — |
    | freestanding `wasm32`, **engine only** (decode+validate+run) | **158.1 KiB** (161,896 B) | **137.5 KiB** (−13.1%) |
    | freestanding `wasm32`, engine + text toolchain | **260.9 KiB** (267,116 B) | **228.6 KiB** (−12.4%) |

    Method: the `.a`/`.rlib` are archives carrying metadata, so they are **not** shipped size and are
    deliberately not quoted. The freestanding figures come from a minimal `cdylib` embedder (bump
    allocator + panic handler) that calls decode→validate→instantiate→invoke, so the linker retains the
    whole engine — the honest "wasmrt compiled to wasm" number.
  - **Still to do:** the wasm3 / WAMR comparison (neither is present on this machine — it needs their
    binaries, not an estimate). `wasm-opt -Oz` is worth ~13% and is **not** in any build script yet.

  ### T9c — Performance (the "fast" axis) ✅ MEASURED 2026-08-07 `[x]`
  - **`crates/wasmrt-core/examples/bench.rs`** — `cargo run --release -p wasmrt-core --example bench`.
    Placed as an example rather than at `bench/bench.rs` because a target path outside the package
    directory breaks `cargo package`. `bench/README.md` keeps the methodology and now points at it.
  - **The first wasmrt performance numbers ever recorded** (same host, release):

    | Regime | Measurement |
    | --- | --- |
    | cold — toy module (70 B) | **3.5 µs** |
    | cold — real module (48,067 B) | **4.48 ms** ← the figure to quote |
    | steady — `sum(n)` tight `loop`/`br_if` | **~237 Mops/s** (232–240 across 1M/10M/50M iterations) |

  - The workload asserts its own result (`sum(0..n) = n(n-1)/2`) before timing — a benchmark that
    computes the wrong thing measures nothing — and a warm-up run sits outside the timer.
  - **Against the oracle, for context only** (`bench/README.md` records wazmrt at ~4.4 ms cold for a
    46 KB guest and ~264 Mops/s steady): wasmrt is at **cold parity** and **~90% of steady throughput**.
    The cold module here is deliberately sized to ~46 KB so the two are comparable — a cold-start number
    without its module size beside it is meaningless. **These remain the oracle's numbers, measured on a
    different machine, so this is a sanity check, not a benchmark result.**
  - Run-to-run spread is several percent: compare only **same-session A/B/A**, never across days.

  ### T9d — Licensing + docs ✅ DONE 2026-08-07 `[x]`
  - **Already done, verified 2026-08-06:** `LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE` and
    `third_party/LICENSES.md` have existed since the **T0 scaffold**; the Component Ledger is **empty**
    and there are **zero third-party dependencies**.
  - **SPDX tag added to `README.md`** — the one gap `licensing.md` recorded is closed, together with a
    pointer to the empty Component Ledger.
  - **Per-crate crates.io listing metadata added** to all three crates: `keywords`, `categories`, and a
    per-crate `readme`. Each crate got its **own short README**, because cargo packages the readme and
    rejects a `../../README.md` path — the trap `releasing.md` warned about. Verified with
    `cargo package --no-verify` on all three: clean.

  ### T9e — `pin` + CLI swappability. `1.0.1` ✅ **DECIDED 2026-08-19** `[ ]`

  🔒 **Owner decisions, all three on 2026-08-19:** *(1)* gate load-once and **re-scope** `pin` to a
  default-`off` mechanism rather than deleting it; *(2)* **"the idea is to be swappable with the wazmrt
  project"**; *(3)* **"if our CLI options are the same they are swappable — if our security checks are
  the same they are also swappable."**

  **The full buildable spec is in [security-model.md](security-model.md)** — do not re-derive it. What
  follows is the work breakdown and the findings that change other tasks.

  - **T9e-1 — the load-once GATE (do this first; it is small and it is already true in behaviour).**
    Make `read_module_bytes` return a `Loaded { bytes, digest }` and have every executing entry point
    require that type, so re-reading from a path **cannot produce** the value execution demands. ⚠️ This
    deliberately diverges from wazmrt, which passes a byte slice and re-derives the digest inside the
    gate: Rust can make *"these bytes came from one read"* a compiler-checked capability. It also
    computes the digest exactly once, at the single point where bytes enter the process.
  - **T9e-2 — `wasmrt-core/src/pin.rs`**: `Digest`/hex, `Mode {Off<Warn<Enforce}`, `stricter`,
    `mode_from_db`, `Action`, the pure `decide(explicit, pinned, opt_out, tty, armed)`, and `Db`.
    ⚠️ **Two fail-closed rules are load-bearing** — an unrecognised `# mode:` value means **`Enforce`**,
    not "no policy"; and a non-hex content line is an **error**, not a skipped line. ⚠️ **wasmrt must
    implement SHA-256 itself** (Rust `std` has none, zero-deps is an invariant) — acceptable *here*
    because there is no key and no secret, and pinned by the NIST vectors. The same argument does **not**
    extend to Ed25519, which is why signatures stay design-only.
  - **T9e-3 — the CLI gate**: `verify_gate` on a `will_execute` predicate, running **before**
    validation. ⚠️⚠️ **`wasmrt wast` must be gated** (`main.rs:307`) — a `.wast` instantiates and invokes
    the modules it contains, including `(module binary "…")` payloads, and **that exact bypass shipped in
    wazmrt**: any wasm can be wrapped in a script, the attacker picks the extension, and it needed no
    privilege. ⚠️ **The `--no-verify`/`--yes` flag region** must be the leading run of wasmrt flags only —
    `wasmrt wasi prog.wasm install --yes` has no `--`, so scanning "before `--`" searches the *guest's*
    argv and a `--yes` meant for the guest silently disables verification.
  - **T9e-4 — `wasmrt pin <file|dir> [--db <path>]`**, hashing the **assembled** bytes for `.wat`.

  #### 🔀 The swappability half — and it turned up a break that exists TODAY

  ⚠️⚠️ **`--dir` uses a different separator in each runtime**: wazmrt `<host>[:<guest>]`, wasmrt
  `<host>[::<guest>]`. `--dir .:/` is a working wazmrt invocation; on wasmrt `split_once("::")` finds
  nothing, so host **and** guest both become the literal `.:/` — **it does not error, it preopens the
  wrong thing.** Each side has a real reason (a single `:` collides with a Windows drive letter), so
  **both should accept both** rather than converging on one spelling and breaking existing commands.
  **This has nothing to do with `pin` and is live right now.**

  ⚠️⚠️ **Aligning the run modes changes what a bare path DOES.** wazmrt's `will_execute` is *"an export
  was named **or** the module exports `_start`"*, so `wazmrt prog.wasm` **runs** a WASI command while
  `wasmrt prog.wasm` today **summarizes** it. Alignment means the most casual invocation there is starts
  **executing** code where it used to only inspect it. 🔒 **Land the pin gate before, or with, that
  change — never after.**

  **Also missing at wasmrt's CLI** (all present in wazmrt, and all needed for a swap): `--env`,
  `--max-memory`, `--max-table-elems`, `--features`, and `--` as an end-of-host-flags marker. ⚠️ Note
  that the last two already exist **in the C ABI** and simply have no CLI surface — so this is mostly
  exposure, not new engine work. `wasmrt wat` is wasmrt-only and has no wazmrt equivalent.

  **Recommended shape: ADDITIVE.** wasmrt keeps `run`/`wasi`/`wat`/`wast` and *additionally* accepts the
  positional forms, so a command written for either runtime drives both; symmetric swappability needs
  wazmrt to accept the subcommand forms, which is wazmrt's half.

  🆕 **A new differential for T12x:** the pin DB is a **shared on-disk artifact**, and a `.wat` digest is
  the hash of the **assembled** bytes — produced by **two independent assemblers**. **Assemble the shared
  corpus with both runtimes and diff the SHA-256s.** If they disagree, only `.wasm` digests are portable.
  ⚠️ And the DB **path** must be shared (`/etc/wasmtk/pins`), or swapping the binary finds no DB, reports
  `armed = false`, and **silently runs everything** — a security downgrade with no error message.


  ### T9f — Tail calls ✅ **DONE 2026-08-14** — and `return_call_ref` had been a FAKE tail call

  **`return_call.wast` 44/0/0 · `return_call_indirect.wast` 76/0/0** (was 27/3/**49 skipped**) **·
  `return_call_ref.wast` 46/0/0** (was 40/7/0). Suite **62,238 / 378 / 2,038**: +125 passes, −125
  skips. 🚦 **Every in-scope proposal is now implemented; 1.0 is no longer blocked on a feature.**

  ⚠️⚠️ **This entry's own warning came true about the code that already shipped.** It read: *"a real
  tail call must replace the current frame, not grow the stack — a naive call-then-return
  implementation passes the tests and misses the feature."* That is precisely what `return_call_ref`
  had been doing since T5, scoring 40/7 on its conformance file the whole time. **The prediction was
  written as a caution about future work and was already a description of the present.**

  **What landed:**
  - `run` no longer recurses on a tail call. It reports the callee through a `TailCall`
    out-parameter and unwinds; `call_function` **loops**, reusing its native frame at the same
    `depth`. Constant native stack for an unbounded chain. An out-parameter, not a richer return
    type, because `run`'s return value is on the hot path and the last change to it cost 3.6% (§1.7).
  - The three tail forms share **one** `check_tail_results`, which is **subtyping, not equality**
    (§3.3.8) — the equality version had been *refusing valid modules*.
  - `WASMRT_FEATURE_TAIL_CALL` = 14. The flag exists **only now that the feature does**, per
    `features.rs`'s own rule; `abi_version()` stays **1** because adding an enum value is additive.
    Pinned by a test asserting both directions *and* that the neighbouring function-references flag
    does **not** gate it.
  - `return_call_ref` stays under `FunctionReferences`, deliberately: that proposal defines it and its
    typed operand is not expressible without it, so moving it would change what an existing
    embedder's config rejects for no safety gain.

  ⚠️ **Conformance could not have caught the fake** — see `best-practices.md` §3.10 and the property
  test `tests/tail_call_is_a_real_tail_call.wast`. Under a mutation restoring it, the spec files still
  score 38/6, 72/4, 40/6.

  ⚠️ **Performance measured A/B/A** (the signature changed): tail calls **227/226/226** and
  **231/234/231** Mops/s; parent **214/219/217**. **Faster, not slower.** 🚦 But the recorded ~237
  baseline does not reproduce here at all, so **build-to-build variance (~7%) exceeds the ~5%
  regression T11 is chasing** — T11's first job is a benchmark that can resolve 5%.

  ### T9i — The ITERATION BUDGET: bound non-termination. `1.0.1` ✅ **DECIDED 2026-08-19** `[ ]`

  ✅ **UNBLOCKED 2026-08-19 — the flag-position fix landed** (`known-issues.md`): host flags are now
  accepted in both positions, an unknown leading `--flag` is an error rather than a path, a stranded
  host flag warns, and nothing after `--` is examined. `--max-iterations` can be added without the
  fail-open hazard below. *(Original blocker, kept because the reasoning is why it was sequenced
  first:)*

  🔒⚠⚠ **WAS BLOCKED ON THE FLAG-POSITION FIX (coordinate, 2026-08-19).** `--max-iterations` is a
  RESTRICTION flag, and wasmrt’s CLI currently donates any flag written after the module path to the
  **guest**, silently (`known-issues.md`). Today that fails **closed** — the only such flags are
  preopens, so a misplaced one grants *less* access. **`--max-iterations` makes it fail OPEN**: the user
  asks for a bound, gets no error, and the guest runs unbounded. Same for T9e’s `--verify` / `--pins`.
  **Fix the parser before adding the flags that make it dangerous.**


  🔒 **Owner, 2026-08-19: *"3 has already been decided in the wazmrt project, just follow their lead."***
  It is a **contract row now** — [`interop.md`](interop.md) §3.7/§3.7a, CONTRACT VERSION 2 — so this is
  not a wasmrt design decision to re-open, and a divergence here breaks swappability under load.

  **The gap it closes, which is live in wasmrt today:** nothing bounds execution time.
  `max_call_depth` (512) bounds recursion, the ceilings bound allocation, and `(loop br 0)` runs
  forever with no interrupt and no error. The embedder has no timeout. **A hostile guest can hang the
  host** — and under `panic = "abort"` there is nothing to catch.

  **What to build** (values fixed by the contract, not by us):

  - **An iteration budget — a COUNT, not a clock.** Default **`1 << 30`**, in `ResourceLimits`
    beside `max_memory_bytes` / `max_table_elems` / `max_call_depth`.
  - ⚠️⚠️ **TICK IN TWO PLACES, and the second is the one that matters.** The loop back-edge is the
    obvious site. The other is **the tail-call trampoline** — and **wasmrt built exactly that
    trampoline at T9f on 2026-08-14**: `run` reports the callee through a `TailCall` out-parameter and
    `call_function` **loops at the same `depth`**. A local `return_call` therefore makes **no backward
    branch and grows no call depth**, so `max_call_depth` cannot see it and neither can a back-edge
    counter. **`(func $f (return_call $f))` runs forever in wasmrt right now.** 🎓 *The feature that was
    a correctness win five days ago is the hole in this limit — the constant-stack property is exactly
    what makes it invisible to both existing bounds.*
  - **Refill per TOP-LEVEL invocation; re-entry inherits the remainder** — the same rule re-entrancy
    already follows, and for the same reason: **a guest that can restart its own budget by bouncing
    through a host callback does not have a budget.**
  - **"Unlimited" is `u64::MAX` filled at refill**, so both tick sites keep exactly one test and the
    hot path never branches on a mode. ⚠️ **T9a#7's lesson applies directly** — threading `pc` through
    `run` measured **3.6% slower** and had to be redone; this touches the same loop, so **benchmark
    A/B/A before and after** and treat a regression as a design problem, not a cost of doing business.
  - **Two surfaces, two conventions for `0`, each documented where it is used:** the CLI's
    `--max-iterations 0` means **unlimited**; the C ABI's `wasmrt_config_set_max_iterations(cfg, 0)`
    means **leave at the default**, like every other ceiling — an embedder wanting none passes
    `UINT64_MAX`. ✅ **Additive to the C ABI, so `abi_version()` stays 1.**
  - **A distinct error**, not a §4.2 trap. ⚠️ **The `.wast` runner must NOT admit it as a runtime
    trap** — that is what keeps the corpus a live gate on the number: too low a budget then fails the
    run **loudly** instead of banking a timeout as the trap an `assert_trap` was asking for.
    (`CallStackExhausted` **is** admitted, deliberately — `call.wast`'s runaway cases exist to make an
    engine's recursion cap fire. **No spec test asks an engine to loop forever.**)
  - **The message names the flag and does not overclaim:** *"did not terminate within N iterations"* —
    **never** "infinite loop detected". It bounds non-termination; it does not *detect* an infinite
    loop, which is not detectable, and a legitimately long-running module trips the same limit.

  ⚠️ **RE-RUN THE DESCENT ON OUR OWN CORPUS — do not inherit the number blind.** wazmrt measured its
  default by re-running its corpus at descending budgets until it broke: green at `1 << 20`, 11
  failures at `1 << 18` (**only** the three `return_call*` files), 36 across 8 files at `1 << 14` — so
  the heaviest legitimate workload in the suite is the tail-call family, and `1 << 30` sits ~1000×
  above it. **The value is contract-fixed; the measurement is ours to reproduce.** If our corpus breaks
  at a different budget, that disagreement is a finding about one of the two engines, not a reason to
  pick a different default.

  **Gate:** `(func $f (return_call $f))` and `(loop br 0)` both terminate with the iteration error
  (**both**, or the second tick site is untested); the property is mutation-verified; the corpus descent
  is re-run and recorded; steady-state throughput measured A/B/A with no regression; suite counts
  unchanged; and the C-ABI + Miri gates green with `abi_version()` still **1**.

  ### T9h — Cross-module type identity: a type registry on the `Store` ✅ **DONE 2026-08-08** *(approach approved by the owner the same day)*

  **Suite 61,724/554 → 61,738 / 536 / 2,466. `type-subtyping.wast` reached 72/0/0** (from 36/44 that
  morning), `type-rec` 7/9 → 11/5, `type-equivalence` 10/2 → 10/1. **Every `Unlinkable: module linked` and
  `an import does not match` in the suite is gone.** 401 tests; cold start within noise (~4.66 vs ~4.63 ms).

  ⚠️ **The finding, which the plan below did not anticipate: comparing SIGNATURES can never answer an
  IDENTITY question.** The registry alone moved only **2** of the ~11 — because two functions can both be
  the empty `(func)` and still be *different types*, rec-group membership being part of identity. Only the
  type **index** carries that, and the decoder had been resolving an import's typeidx to a `FuncType` and
  **throwing the index away**; `Import` gained `func_type_index`. Same shape as the emitter defects T10a is
  about: information present, discarded because nothing needed it yet.

  Two more: **§4.5.9 matching is subtyping, not equality** (equality refused 3 valid modules, so the
  registry records supertypes store-wide and matching walks the chain — terminating because a supertype is
  always a lower id), and 🆕 **`call_indirect` was a THIRD site with the identical defect** (it compared
  signatures; now type identity with subtyping per §4.4.8), worth 7 runtime assertions.

  **Residual, deliberate:** a re-exported *import* has no defining type index, so those fall back to the
  structural comparison; and repeated **failed** instantiations grow the registry, since interning precedes
  the checks that need it — unbounded, though nothing references the orphans. Both logged.

  Original scoping:

  **The residual of the canonicalisation pass, and the last known in-scope correctness cluster of any
  size: ~11 assertions** — 7 `Unlinkable: module linked` + 4 `an import does not match` across
  `type-subtyping` / `type-rec` / `type-equivalence`.

  **Why module-local canonicalisation cannot finish the job.** `Module.type_canon` gives each type the
  lowest *index in that module* structurally equal to it. Two modules number their types independently,
  so those ids say nothing across a boundary — and the structural **key** cannot be compared either,
  because it embeds those ids by reference. Making keys self-contained (inlining each referenced group)
  is correct but risks exponential blowup on chained groups, which is a denial-of-service surface on
  exactly the untrusted path.

  **The approach (owner-approved 2026-08-08): an engine-level type registry, wasmtime's SHAPE, our
  code.** Rec groups are interned into a registry as each module joins the store; interning returns an
  id that is meaningful store-wide; cross-module import matching then compares ids, not structures. That
  is architecturally what wasmtime does and it is the right answer for the same reason the shared store
  was at T7b: the alternative is re-deriving structural equality at every boundary crossing.

  ⚠️ **Shape only — no code, no symbols, no data structures transcribed.** This is the **third**
  application of a rule already recorded in `reference-projects.md` and `design-decisions.md` (after the
  `wasmrt.h` shape at T8 and the shared store at T7b), not a new policy, and the **Component Ledger
  stays empty**: `INDEX.md`'s "evaluate a reference project" trigger requires a ledger entry for
  *copying or porting code*, and reading an architecture is explicitly free. wasmrt remains 100%
  original Rust.

  **What it touches, and why it is a decision rather than a patch:** a new structure on `Store` (which
  every instance shares), interning at `instantiate`, and the import-matching path in `Store` and
  `Linker`. It also lets two approximations be deleted — the structural-equality fallback in the
  function-import check, and the note in `known-issues.md` about host imports being uncheckable stays,
  but the *wasm→wasm* half becomes exact.

  **Constraints it must respect:** zero third-party dependencies, `#![forbid(unsafe_code)]` in core, no
  `Rc`/`RefCell` on the interpreter's hot path (the registry is consulted at **link** time, never per
  call), and the freestanding `wasm32` no_std build. Interning must be **O(n log n)**, not a scan, for
  the same reason `canonicalize` uses a `BTreeMap`: group counts are attacker-controlled.

  **Gate:** the 11 assertions convert to passes; the whole suite diffed **with `-v` on both sides** (see
  `testing.md` — a file reaching zero failures vanishes from a non-verbose run); no file loses a pass;
  cold start measured A/B/A, since this adds work at instantiation.

  ### T9g — Scope confirmations (NOT bugs — record, do not "fix") `[ ]`

  ⚠️ **2026-08-08 — this list had the same flaw §1.1 warns about, one level up: a scope note is a
  hypothesis about a CAUSE.** It read "`annotations` **51** — a proposal wasmrt does not target", and
  that was true of the *file* and false of **44 of its 51 assertions**, which were generic §6.2/§6.3
  lexer rules that apply to every `.wat` wasmrt reads. Probing it took four one-line files and
  disproved it in a minute: `(module (func $a\x01b))`, `$a\xffb` and `(data "\x01")` were all accepted
  and contain no annotation. **`id.wast` — which has nothing to do with annotations — went 0/5 to
  6/0/1 on the same fix**, which is the proof the cause was generic. Before writing off a cluster by
  the name of the file it sits in, check whether the *defect* is scoped the way the *file* is.

  - **Out of scope by recorded invariant — leave these failing:** 64-bit **tables**
    (`table_copy64` 22, `table_init64` 93 skips, `table_fill64` 70 skips, `float_memory64` 84 skips),
    and the untargeted proposals `annotations` **8 remaining** (was 51; the 43 generic lexer ones are
    fixed — what is left is genuinely annotation semantics: 7 × "empty annotation id" and one
    annotation-id UTF-8 case), `custom-descriptors`
    (`br_on_cast_desc_eq`/`_fail` 98 each, `ref_cast_desc_eq` 94, its own `binary.wast` 44),
    `exact`/`exact-casts` (18 + 108), `custom-page-sizes` (21 + 18), `memory64-imports` 20,
    `wide-arithmetic` 108. **Together these are the bulk of the residual** — the in-scope remainder is
    much smaller than the raw 738/3,075 suggests.
  - **`sqrt` is `std`-gated** — the single no_std float gap (platform libm). Revisit with a software
    sqrt only if the freestanding target needs it.

  **Gate:** the DoD below, plus a re-measured suite. **Expect the conformance number to move for
  honest reasons in both directions** — #1 and #4 convert large skip blocks into visible results, and
  some of those will be failures at first (the same accounting as the `register` work on 08-04 and the
  linker work on 08-06).

- **T10 — Bug hunt + code hygiene.** *(Owner, 2026-08-06.)* 🆕 **Ships as `1.0.2` (owner, 2026-08-19).** `[ ]`

  **This is the `INDEX.md` "look for code issues" trigger run as a scheduled task**, not a new process —
  read that trigger first; it is binding and already specifies the method (fan out parallel read-only
  investigators per category, consolidate, report `file:line` + one-line + severity, fix the safe ones,
  and **keep the suite green by diffing the OUTPUT counts, not exit codes**). The owner's five
  categories:

  1. **Bugs.** Inverted logic, LEB off-by-ones, wrong-tagged enum access, stack-order, missing bounds
     checks. **Hunt the silent-wrong-output class hardest** — every serious defect this port has found
     was one (dropped table initializers, element-segment form 4 rewriting a type, `br_table`'s missing
     label vector, `(data "a""b")` concatenating, `Op::MemorySize` reading another instance's memory).
     A module that is *rejected* announces itself; one that runs and answers wrongly does not.
  2. **Fall-throughs (worst).** Unhandled input that emits a stub or placeholder instead of erroring.
     Prefer a hard `Err(Unsupported*)` over silent-wrong — the standing rule since T4.
  3. **Stale workarounds** — `TODO`, "for now", anything written around a limitation that has since
     been lifted. Several deferrals were resolved by later tasks; the guards may still be in place.
  4. **Dead code.** ⚠️ **The C ABI changed this check.** `cargo`/clippy judge reachability in *Rust*
     terms, and ~74 symbols are now reached **only from C** — they will look unused and are not.
     **`tests/abi_symbols.c` is the authority on what must exist**; grep-verify against it (and against
     re-exports) before deleting anything.
  5. **Missing function documentation.** Consider making it mechanical rather than aspirational —
     `#![warn(missing_docs)]` on the public surface, the same move that turned the safety directive from
     a convention into a compile error at v0.8.0. A rule nobody has watched fail is not enforcement.

  **Gate:** zero regressions across all four surfaces + the C and Miri gates; suite counts diffed
  against the pre-hunt baseline; every finding either fixed or logged in `known-issues.md` with its
  `file:line` and why it was left.

  ### T10a — The EMITTER audit: forms reconstructed from partial facts *(added 2026-08-08)* `[◐]`

  ✅ **The opcode half landed 2026-08-08 with T9a#8, and it paid immediately.** The sweep below,
  applied to the *instruction* table rather than to `ModuleBuild`, found **four instructions the
  assembler did not know took immediates** (`call_ref`, `return_call_ref`, `br_on_null`,
  `br_on_non_null`) — emitted as bare opcodes with their operand left in the token stream. That is a
  **fourth instance** of this mechanism, and again nothing looked for it directly: it surfaced as
  three unrelated-looking bugs across two separately-logged punch-list items. `Op::from_u8` makes the
  opcode space enumerable, so the check walks all 256 single-byte opcodes and asserts the **decoder**
  and the **assembler** agree about which ops take an immediate, in both directions. ⚠️ Expect the
  prediction below ("more than three") to keep holding: it is now 4-for-4.

  **Still open: the `ModuleBuild` half** — the round-trip property test and the field-coverage grep
  described below, plus the shorthand review. Those cover *module structure*; the opcode sweep covers
  only *instruction immediates*.

  **A named sub-task because the same defect has now occurred four times, and each was found by
  accident** — by some *other* check happening to start reading a field the emitter had dropped. This
  is not a category from the five above; it is a specific mechanism with a specific audit.

  **The mechanism.** `wat.rs`'s parser records a set of facts about a form; the emitter then
  reconstructs the binary form from **a subset** of them, and the dropped fact turns out to be
  *semantic*. The output is a valid module — just **not the module the text described**. Nothing
  rejects it, so it is the silent-wrong-output class arriving through the toolchain rather than the
  engine.

  The three instances, all fixed, listed because the pattern is the point:

  | # | The dropped fact | What the emitter produced instead | Found by |
  | --- | --- | --- | --- |
  | 1 | Element-segment **reftype** (form 4 has no reftype field) | `funcref` hardcoded, so `(elem (ref func) …)` had its type rewritten | T8's `export_global` making `table.wast` buildable |
  | 2 | **Finality** — the parser saw `sub` vs `sub final` vs bare | keyed the wrapper on *the supertype's presence alone*, so `(sub (struct …))` emitted a **bare** comptype = `final` | the new §3.4.5 finality check reading the flag |
  | 3 | **Rec-group extent** — `(rec …)` boundaries | flattened to singleton groups; `0x4e` was emitted **nowhere** | type canonicalisation, which made group identity observable |
  | 4 | **That an instruction HAS an immediate** — `call_ref`/`return_call_ref` (typeidx), `br_on_null`/`br_on_non_null` (labelidx) | a bare opcode, operand left in the token stream: folded `call_ref` shifted every following byte into an undecodable body, flat forms blamed the *next* token with `UnknownInstr` | T9a#8, chasing one `.wat` file — ✅ **now caught by the sweep** |

  **The audit, which must not be another read-through.** Two mechanical checks, either of which would
  have caught all three:

  - **A round-trip property test.** Assemble text → decode the bytes → compare the decoded `Module`
    against what the *parser* recorded (type defs, `type_finals`, rec-group extents, segment modes and
    reftypes, limits flags, memory indices, global mutability). Any field the parser fills and the
    decoder cannot recover is, by definition, a fact the emitter dropped. This is the check the project
    has been missing: every existing test asserts the module *runs*, and all three defects produced
    modules that ran.
  - **A field-coverage sweep.** For every field on `ModuleBuild`, grep whether the emitter reads it.
    Defect 2 and 3 were both *unread fields* — `type_finals` and `rec_groups` did not exist because
    nothing needed them; the parser had the information and threw it away.

  **Then the shorthand review.** Every binary form with more than one legal encoding is a place this can
  recur, because choosing the shorthand is only safe when it is semantics-preserving. Known candidates
  to check deliberately: **element segments** (8 flag forms, two families — already burned once),
  **data segments** (active / passive / active-with-memidx, and the `mem_index`), **limits** flags
  (`shared`, `is64`, has-max), **memargs** (alignment, offset, and the `0x40` memory-index flag),
  **block types** (empty / single valtype / typeidx), and **table** definitions (init-expr vs the inline
  `(elem …)` shorthand — also already burned).

  **Gate:** the round-trip property test exists and is green over the whole vendored `.wast` corpus (it
  is a stronger statement than the conformance number, which only says the modules behave); every
  `ModuleBuild` field is either read by the emitter or documented as deliberately not emitted; and the
  suite/`.wat`-corpus counts do not regress. **Expect this to find more than three** — the mechanism has
  a 3-for-3 record and nothing has ever looked for it directly.

  🚨🚨 **GATE AMENDED 2026-08-19 — a round trip proves agreement with YOURSELF, and that is a whole
  defect class this gate cannot see.** *(Borrowed method — `best-practices.md` §3A.2.)* wazmrt emitted
  its internal synthetic valtype tags raw, so `(ref i31)` assembled to one byte where the spec form is
  two. Its decoder accepted both spellings, so **its own output round-tripped perfectly and everyone
  else's input read fine** — every module it produced was invalid to every other runtime for nine
  months, and its whole conformance corpus was blind by construction (the runs before and after the fix
  are byte-identical). **The round-trip test above would have passed throughout.**

  **So T10a's gate gains a third, EXTERNAL arm:** for a sample spanning every encoding shorthand in the
  list above, **assert the emitted BYTES** (golden or hand-checked) **and hand them to wasmtime**, which
  is the only party with no stake in our conventions. **When a bug is visible only to a third party, the
  test has to BE a third party.** And the design half, which is a standing invariant rather than a test:
  **an internal tag belongs OUTSIDE the format's encoding space, or gets converted at the boundary** —
  wasmrt holds the decode half since T2 (internal tags `0xD7`–`0xFA`, raw ones rejected), and what is
  unproven is the **emit** half. "Currently unused" is a statement about *today's* spec: the byte wazmrt
  borrowed became the custom-descriptors `Exact` prefix by 2026.

  ### T10b — Pin the three spellings of the proposal list *(added 2026-08-19)* `[ ]`

  Logged in [known-issues.md](known-issues.md). `enum Feature` (core), the `u32` map `feature_of`
  (`wasmrt-capi/src/lib.rs:332`) and `wasmrt_feature_t` (`wasmrt.h:151`) are three hand-written copies
  of one list, and **nothing compares them**. `feature_of` matches on `u32` with a `_` arm, so Rust's
  exhaustiveness check does not fire: a sixteenth proposal would compile, ship a header constant, and be
  **unreachable from C with no test failing**. Core's `EVERY` array pins the Rust side only.
  **Gate:** a test walking `EVERY` that round-trips every variant through `feature_of` in both
  directions and pins the header's constants by **name and value**, not by count. *(wazmrt shipped this
  exact defect — its header advertised `TAIL_CALL = 14` while its C-ABI bound stopped at 13.)*

- **T11 — Optimization review (a DISCUSSION, not a blind pass).** *(Owner, 2026-08-06.)* 🆕 **Ships as `1.0.3` (owner, 2026-08-19).** `[ ]`

  **The deliverable is options presented to the owner with measurements and trade-offs — then a
  decision — not unilateral optimization.** Scope: making the code more efficient, faster and smaller,
  **judged from the standpoint of the final binary and the C ABI** rather than of micro-benchmarks.

  - **Depends on T9's baselines.** T9b and T9c are where size and speed are measured for the first time
    (both axes are currently **unproven** — `vision.md`). **An optimization review without a baseline is
    guesswork**; do not start T11 until those numbers exist, and record them so every proposal can be
    stated as a delta.
  - **Judge at the artifact boundary.** The four shipped surfaces are the CLI binary, the `staticlib`,
    the `cdylib`, and the freestanding `wasm32` core — plus the C ABI's own call overhead. A change that
    wins in a micro-benchmark and does not move any of those has not earned its complexity.
  - **Invariants that constrain what may be proposed** (breaking one is a decision-gate, not an
    optimization): the **`u128` value slot** (a `v128` is one slot — reverting to 2×`u64` reintroduces
    the stack-desync hazard class wazmrt carries), **`#![forbid(unsafe_code)]`** in core and the CLI,
    **zero third-party dependencies**, **oracle parity** on observable behaviour, and the **frozen C ABI
    at `abi_version() == 1`** — a change that alters the boundary must bump it deliberately, and the
    header/library pair is pinned by a test.
  - Candidate areas to *evaluate* (not a commitment): dispatch shape in `interp.rs` (the switch loop),
    per-instruction decode cost vs. a denser IR, allocation churn in hot paths, monomorphisation and
    generic bloat against binary size, `opt-level` / LTO / `panic` strategy per artifact, `wasm-opt`
    passes for the freestanding target, and whether the C ABI's marshalling copies more than it must.
  - **Gate:** for each accepted change, a measured before/after on the artifact(s) it targets, zero
    behavioural drift against the oracle, and the full suite + C + Miri gates green. **Reject anything
    that trades a recorded invariant for a number** unless the owner takes that decision explicitly.

  ### T11 — HOW to measure, added 2026-08-19 (borrowed method, `best-practices.md` §3A.2)

  T9f left T11 a first job — *"a benchmark that can resolve 5%"*, because build-to-build variance (~7%)
  currently exceeds the ~5% regression being chased. Four rules make that job well-posed, each bought by
  a defect in the other runtime:

  1. ⚠️⚠️ **Measure the FLOOR before attributing a difference, and report it beside every number.**
     wazmrt's end-to-end CLI harness showed 2.4× over wasmtime; excluding process spawn, the engines
     differ **20–55×**. A ~30 ms spawn floor did not add noise to a sub-millisecond quantity — it hid the
     entire effect and flattened a real module-size dependence into "nothing moves". Its `--version`
     (no wasm work at all) cost 30 ms vs 76 ms, so **~46 ms of a ~50 ms "engine win" existed before
     either engine started.** ⇒ **T11's benchmark must be in-process**, and must state its floor.
  2. **Quote the DIFFERENCE, or the ratio with load conditions attached.** The same harness gave 5.3× on
     a quiet box and 2.4× on a loaded one with the absolute gap unchanged (~29–48 ms): a fixed per-process
     cost shared by both entrants inflates both sides and compresses the ratio.
  3. ⚠️⚠️ **A size number must be reproducible in the CONFIGURATION it was recorded in — including
     WHERE.** One unchanged wazmrt commit measured **four different static-archive sizes**, varying only
     with the source-tree and cache paths, because an unpadded archive embeds object/source paths; its
     `.exe`/`.dll` hid the same variance inside PE alignment and looked perfectly stable. **This lands
     directly on T11's promoted footnote: the never-measured artifact is the RLIB, which is exactly that
     kind of archive.** Measure it from a fixed path; re-measure the parent commit in the same
     configuration before charging a sub-KB delta to a change.
  4. **Attribute the ~5% regression by BISECTING against parent commits in a worktree**, not by
     reasoning about likely causes — two hypotheses have already been tested and rejected that way.

  🆕 **And a task-sorting rule: T11 holds two different KINDS of work.** The optimization review is a
  **fix task** (it changes code, and it ends). The same-machine comparison against wasm3 / WAMR / wazmrt
  is a **compare task** — it can never be "done", because rivals ship new versions and corpora grow, and
  ⚠️ **it is the only kind of task that pulls external dependencies into a project whose stated invariant
  is zero of them.** Schedule it when a NUMBER is wanted; do not let it sit in the fix queue making that
  queue look permanently non-empty.

  ### 🏁 NOTATION: T11 holds an ISSUE and a BAKE-OFF, and they do not share a queue

  🔒 **Owner, 2026-08-19 — make the distinction visible** (`best-practices.md` §8.3). **An ISSUE changes
  the code and CLOSES; a BAKE-OFF measures against something else and NEVER closes**, because rivals
  ship and corpora grow, so its residuals regenerate by construction.

  | T11 item | kind | notation |
  | --- | --- | --- |
  | the optimization review itself (dispatch shape, allocation churn, LTO/`opt-level`, C-ABI marshalling) | **ISSUE** — a fix task | `[ ]` → `[x]`, closes |
  | the ~5% steady-state regression | **ISSUE** — bisect it and close it | `[ ]` → `[x]`, closes |
  | measuring the **rlib** `rsxtk` links | **ISSUE** once (it has never been measured) then a 🏁 row | first run closes; the number thereafter is dated |
  | **same-machine comparison vs wasm3 / WAMR / wazmrt** | 🏁 **BAKE-OFF** | **never a checkbox** — *“measured `<date>`: X, config Y”* |

  ⚠⚠ **Never mark a bake-off ✅ DONE**, and never let it sit in the fix queue — parked there it reads as
  permanent unfinished work and devalues every real `[ ]` beside it. ⚠ It is also **the only T11 item
  that may reach outside this repo** (rival binaries, foreign harnesses, other trees), which matters in a
  project whose invariant is zero third-party dependencies: **nothing it needs may leak into a shipped
  artifact.** 🎓 **A comparison is evidence, never a target** — no invariant or design choice may be
  justified by *“the other one is faster”*.

  ### 📌 A competitor has already published head-to-head numbers — treat them as a PROMPT, not a result

  wazmrt's repo records a first head-to-head on this box, both runtimes size-tuned (2026-08-14):

  | artifact | wazmrt | wasmrt | note |
  | --- | --- | --- | --- |
  | C-ABI shared library | **222 KB** | **554 KB** | wazmrt 2.5× smaller — *"the embed footprint is wazmrt's strongest card"* |
  | CLI | 890 KB | **684 KB** | wasmrt smaller |
  | end-to-end run | — | — | its bake-off notes wasmrt **TIES** wazmrt end to end |

  ⚠️ **These are a competitor's measurements of us, and they do NOT satisfy T11's third footnote** — they
  raise its priority. Two reasons not to adopt either figure: they are explicitly *not*
  feature-parity-verified, and **the 554 KB / 684 KB do not match our own record** (cdylib 493.5 KiB,
  CLI 621 KiB) — which is rule 3 above restated, since a build config, flags and date we cannot see are
  part of the number. **Run our own same-box comparison and publish that.** The one figure to take
  seriously without re-deriving is the *shape*: the **cdylib is where wasmrt is behind**, and the cdylib
  is the artifact wasmtk and every C consumer link.

- **T12 — Security review: find the penetration surfaces, recommend the plugs.** *(Owner, 2026-08-06.)* 🆕 **Ships as `1.0.4` (owner, 2026-08-19).** `[ ]`

  **A review-and-recommend phase, like T11** — the deliverable is *findings + recommended mitigations
  with their costs*, presented for a decision. Do not unilaterally harden: several plausible mitigations
  trade against recorded invariants (zero dependencies, `forbid(unsafe_code)`, smallest-binary, oracle
  parity), and those trades are the owner's to make.

  **Frame it around the actual threat model, which has three distinct adversaries** — conflating them is
  how a review misses things:

  1. **A hostile GUEST module** (the main one — wasmrt's entire job is running untrusted code).
  2. **A hostile INPUT to the tooling** — a malformed `.wasm`/`.wat`/`.wast` fed to the decoder,
     validator or text parser, possibly never executed.
  3. **A careless or hostile EMBEDDER** misusing the C ABI. (Distinct from a hostile guest, and the only
     adversary that meets `unsafe` code.)

  ### T12z — Sweep an INVARIANT across every entry point, not one site at a time *(added 2026-08-10)* `[ ]`

  **The owner's framing, and it is the right one for a security review:** *"better to identify an issue
  and fix it than let it trip us up later — I foresee more of these as we approach completion and
  security review."* Two defects this week were the same shape, and neither was "nobody does X":

  | invariant | held at | missing at |
  | --- | --- | --- |
  | validate before executing | summarize, `wasi`, C ABI | **`wasmrt run`** (and 3 wazmrt sites) |
  | a value handle carries its issuing store | the C ABI (T8) | **core's `InstanceId`** (until T9a#3) |
  | the feature GATE and the typing rules it constrains are **one call**, not two | wasmrt's C ABI calls `validate_with_features(&md, &e.features)` (`wasmrt-capi/src/lib.rs:693`, `:725`) | ⚠️ **a new row, added 2026-08-19** — enumerate the rest |

  ⚠️ **That third row is borrowed from a shipped wazmrt defect** (`best-practices.md` §3A.2): its gate
  sat *beside* `validate`, so its C ABI gated with the embedder's feature set and then validated with
  **all** features — the set decided which proposals were ADMISSIBLE while the all-features rules decided
  what they MEANT, and one instruction was typed by a relaxed proposal rule with that proposal switched
  off. **No gating test could see it**, because the instruction exists either way and was never refused.
  wasmrt's C ABI looks right at the two sites above; **the method is to enumerate every entry point, not
  to spot-check one** — that is the whole point of T12z. Add to the candidate list: **every proposal has
  a gate, and that gate is tested** — *a proposal that ships without one is not "enabled by default", it
  is unrefusable*, and a per-proposal checklist cannot ask this question about itself.

  **The bug is "three of the four do X".** So the T12 method is not "audit file by file" but: name an
  invariant, enumerate **every** entry point in a table, and check the property at each. Candidates
  worth a table each — validation before execution ✅ done; **bounds-checking guest-derived indices**;
  **store-tag checking on every handle**; **resource ceilings honoured on every allocation path**;
  **`no_std` parity** (a `#[cfg(feature = "std")]` guard that silently no-ops is the same class);
  **every `wasmrt_*` C entry point rejecting a NULL/foreign/stale handle**.

  Prefer one guard on a predicate the paths already share over N copies — wazmrt's `will_execute`
  already existed to gate pin verification, so validation hung off it and a future execute path
  inherits it. `best-practices.md` §3.4 carries the generalization.

  ### T12x — DIFF THE TWO RUNTIMES' SECURITY TABLES AGAINST EACH OTHER *(added 2026-08-10)* `[ ]`

  ⚠️ **RESHAPED 2026-08-19, because the oracle is retired.** The task survives — two implementations of
  one spec are still a free differential oracle, and the arrangement already caught a real authority
  breach in each direction — but its *form* changes: **not "diff against the oracle" (there isn't one),
  but "require every implementation to AGREE, and record each disagreement as an OBSERVATION until it is
  traced."** wazmrt built exactly that harness and it found a one-byte disagreement on its first run,
  across five implementations including V8, against wasmtime 47.0.3 — **and deliberately recorded it
  without a diagnosis, because the cause was not traced.** *A differential check with no privileged
  oracle finds things a golden file cannot* (`best-practices.md` §3A.2). ⚠️ Note the corollary for
  scoping this against T10/T11: **a refusal is not a hole.** A module wasmrt *rejects* cannot do harm, so
  implementing the proposal behind a block of failing assertions **adds** attack surface. A conformance
  total counts disagreements, not exposure.

  ⚠️ **Found the hard way, twice in one day.** Both defects were "the property holds in one
  implementation and not the other, and nothing compared them":

  | property | wasmrt | wazmrt (before) |
  | --- | --- | --- |
  | `wasmrt run` / execute paths validate | ❌ then ✅ | ❌ then ✅ |
  | `PATH_SYMLINK` exists, is in the write mask, and gates `path_symlink` | ✅ all along | ❌ **bit 24 did not exist** — `--ro-dir` did not strip it, and the handler demanded `path_open` |

  The second was a real authority breach: **a guest could plant symlinks inside a read-only preopen**,
  and wazmrt's own read-only test passed throughout because `ro & write_mask == 0` is trivially true
  for a right that is **not in the mask**.

  **The task:** mechanically diff the two runtimes' security-relevant tables — the WASI **rights bit
  definitions**, the **write mask**, which right each `path_*` handler demands, the `oflags`/`fdflags`
  sets, and the resource ceilings. Any row present in one and absent in the other is either a port gap
  or an oracle gap, and today it was both directions. **Two implementations of the same spec are a free
  differential oracle; not using them against each other is the waste.**

  ### T12y — The oracle's sandbox-escape tests DO NOT RUN on this machine `[ ]`

  ⚠️ **Found while answering "are the 4 skipped tests meant to be skipped?" — they are conditional, and
  the condition is met here, so they skip.** wazmrt's `zig build test` reports **489/493, 4 skipped**;
  all four are the same two tests in `src/wasi.zig`, once per test binary:

  - `symlink traversal: in-sandbox links follow, escaping links refused (#17/4.3)`
  - `symlink resolver fuzz: no adversarial topology reaches outside the preopen`

  ⚠️⚠️ **CORRECTED — the cause is exFAT, not privilege, and the properties ARE verified.** Developer
  Mode is already enabled here; the Zig error is `INVALID_DEVICE_REQUEST`, not access-denied.
  `std.testing.tmpDir` puts its scratch under `.zig-cache/tmp` **relative to the CWD**, and the **D:
  drive is exFAT** — no reparse points. **From an NTFS cwd: `zig build test` is 493/493 with ZERO
  skips and `zig build test-security` is 3/3 green.** So "489/493, 4 skipped", carried in the freeze
  record since 2026-07-27, is an **environment artifact, not a code property**. Same root cause as
  `zig build`'s "error: Unexpected" on D: and cargo's hard-link warnings: exFAT has neither reparse
  points nor hard links. **One filesystem, three symptoms, none of them code.**

  **These are the sandbox-escape tests.** The skip is honest — with no link there is nothing to
  traverse — but the effect is that **the escape properties are unverified in this environment**, which
  is exactly the wrong thing to discover during a security review rather than before it. The source
  notes one case is covered on Windows another way (`examples/wasi_symlink_traversal.zig`, the
  absolute-target canary), so the coverage is partial, not absent.

  ⚠️ **Do NOT read the skip as "the escape was refused, so it passed."** The reading is natural — these
  ARE negative tests, and inside them `refused(openErrno(…))` genuinely means "failing to open is the
  success condition". But the skip fires during **fixture setup**, at `pre.symLink(…) catch { return
  error.SkipZigTest; }`, **before a single assertion runs**: nothing was refused by the runtime, the
  HOST refused the test harness a symlink, and the resolver was never invoked. Two facts settle it —
  the first assertion expects **`errno.success`** (an in-sandbox link must be FOLLOWED, so "refusal =
  pass" is not even true of the whole test), and the skip returns before `Wasi.init`, so no wasi call
  happens at all. **A skip here means zero of the three properties were checked, not that they held.**

  ✅ **Action, now known:** run the oracle suite from an **NTFS** working directory, and use
  `zig build test-security` — the gate added 2026-08-10 that makes a skip a FAILURE, so an unverified
  sandbox is red rather than averaged into a green summary. wasmrt's own sandbox tests were never
  affected: `Scratch::new` uses `std::env::temp_dir()`, which is on C: (NTFS), so they have been
  running all along. wasmrt's own
  sandbox tests should get the same treatment — its resolver is the one place the port knowingly
  diverges (`std` has no dir-relative open, `security-model.md`).

  ### T12a — Reachable panics are a DoS surface, and the release profile makes them fatal `[ ]`
  **`[profile.release]` sets `panic = "abort"`.** For a library whose purpose is to contain untrusted
  code, **any panic reachable from hostile input kills the embedder's process outright** — no unwind, no
  error, no recovery. The suite already caught one (`v128.const i64x2` hit an `unreachable!()` and
  aborted the conformance run); the standing rule from that fix is *a library must reject a module,
  never abort the embedder*. **Audit for reachable panics on hostile input:** `unwrap`/`expect`,
  `unreachable!`/`panic!`/`todo!`, slice indexing and slicing, division, and `as`-cast assumptions —
  across decode, validate, interp, the text toolchain, WASI, and the C ABI. Recommend, per site, either
  a real error path or a proof it is unreachable.

  ### T12b — Arithmetic behaves differently in release than in debug `[ ]`
  There are **no `overflow-checks` in the release profile**, so integer arithmetic **wraps silently in
  release and panics in debug**. Both are bad on hostile input, in opposite ways: debug turns an
  overflow into T12a's process abort, release turns it into a *wrong value* that may then be used as an
  index, length or offset. The engine already computes effective addresses overflow-safe on purpose
  (`mem_range`, `checked_add`/`checked_mul` in the limits paths) — **this task is to establish whether
  that discipline is complete**, and to recommend whether `overflow-checks = true` should be on in
  release despite the size and speed cost.

  ### T12c — Resource exhaustion `[ ]`
  Enumerate every unbounded-by-input quantity and confirm each has a ceiling an embedder can reach.
  Known caps: `ResourceLimits` (memory bytes, table elems, call depth, GC objects, exn boxes — all
  configurable from C as of T8), `MAX_CTRL_DEPTH`, `MAX_LOCALS`, the text parser's paren-depth cap, and
  `FuncValidator.body_len` bounding `array.new_fixed`'s unvalidated `n`. **Look for the ones nobody has
  listed** — allocation driven by a decoded count before that count is validated is the classic shape,
  and *quadratic* behaviour on adversarial input is as effective as an unbounded allocation.

  ### T12d — The sandbox, re-examined as an attacker would `[ ]`
  Re-derive the escape properties rather than re-reading them: `..` cannot rise above a preopen,
  absolute targets re-base to the preopen root, symlink targets go through the same loop, `SYMLINK_MAX`
  bounds cycles, rights only ever narrow. **The accepted TOCTOU residual is in scope to re-examine, not
  to re-litigate** — the decision (accept + document, 2026-08-05) stands unless T12 produces *new*
  evidence that the deployment assumption is weaker than believed. Confirm the canary test still fails
  when the guard is removed (mutation), and check Windows/UNC/device-name handling specifically.

  ### T12e — The C ABI as an attack surface `[ ]`
  It is the only place `unsafe` exists. Miri covers the lifecycle under *well-formed* use; T12 asks what
  a **misusing** embedder can provoke — null and dangling pointers, wrong-store handles, lying lengths,
  re-entrancy from inside a host callback, a callback that traps or unwinds, `env_finalizer` interaction
  with teardown order, and the documented invalidation rule for `wasmrt_memory_data()`. Recommend where
  a check is worth its cost and where the contract should simply be stated more sharply.

  ### T12f — Method + the one dependency question it will raise `[ ]`
  - **Fuzzing is the right tool for adversaries 1 and 2** (decoder, validator, text parser, and a
    differential fuzz against the frozen oracle). It needs `cargo-fuzz`/AFL or similar.
    **Decision to put to the owner:** these are **dev-dependencies, not runtime dependencies** — they do
    not ship in any artifact and so do not breach the zero-dependency posture, which is about what the
    binary contains. Worth stating explicitly, because "zero dependencies" has been applied strictly and
    someone will otherwise refuse the tool that finds the bugs.
  - Differential-test malformed input against the oracle: *both* must reject, and neither may crash.
  - **Authenticity is T9's `pin`, not T12's** — but T12 should review the pin design once implemented,
    and the still-design-only signature path (`security-model.md`).
  - **Record findings even when the recommendation is "accept"**, with the deployment assumption that
    makes it acceptable — the pattern already set by the resolver residual.

  **Gate:** a written review covering all three adversaries, every finding logged in `known-issues.md`
  with a severity and a recommendation, and the owner's decision recorded for each. Any mitigation that
  lands must keep every gate green and not silently trade an invariant.

**Extended proposal — the one oracle-split residual:** **tail calls** (`return_call`/`return_call_indirect`)
have no wazmrt oracle → conform against **wasmtime + the official spec testsuite**. Slot this into T5/T6
alongside the rest; everything else parity-tests directly against the frozen wazmrt.

**Definition of done (1.0)** — full Rust↔oracle parity on **both** targets (native + `wasm32`), all gates
green (spec testsuite, wasi-gate, c-smoke, abi-symbols, Miri fuzz), size minimized, and the drift monitor
clean (or the oracle re-baselined deliberately).

**Four tasks stand between v0.9.0 and that** (T10–T12 added by the owner 2026-08-06):

| Task | Version | What it is | Why it is separate |
| --- | --- | --- | --- |
| **T13** | **1.0.0** | **Conformance clear-out** — the corpus to zero failed / zero skipped / zero unrun, empty baseline | 🆕 **Added by the owner 2026-08-19 and placed FIRST.** T10 and T11 are both measured against the corpus, so hunting bugs or setting baselines while 2,416 assertions are unadjudicated measures the wrong engine. |
| **T9** | **1.0.1** | Hardening — correctness punch-list, `pin`, licensing/docs, size + perf measurement | Closes the *known* gaps — everything already written down |
| **T10** | **1.0.2** | Bug hunt + code hygiene | Finds the *unknown* ones, across tested **and untested** paths. Distinct from T9 on purpose: T9 works a list, T10 goes looking. |
| **T11** | **1.0.3** | Optimization review | A **discussion with measurements**, not a pass. **Cannot start before T9's baselines exist** — optimizing without a baseline is guesswork. |
| **T12** | **1.0.4** | Security review — find penetration surfaces, recommend plugs | Also **review-and-recommend**, and deliberately **last**: it must audit the code that ships, so it has to follow the bug hunt *and* the optimization pass. An optimization can introduce a surface (`overflow-checks`, a fast path that skips a bounds check), so reviewing before T11 would audit code that is about to change. |

Ordering is deliberate and should not be shuffled: **clear the corpus (T13) → measure (T9) → find (T10)
→ optimize (T11) → attack (T12)**. Optimizing before the bug hunt risks micro-tuning code that is about to change or be
deleted; reviewing performance before any baseline exists produces opinions rather than deltas; and a
security review is only worth the paper it is written on if it examines the *final* code.

**T11 and T12 both end in a decision, not a diff.** Each produces findings and recommended changes with
their costs — several of which trade against recorded invariants (zero dependencies,
`forbid(unsafe_code)`, smallest-binary, oracle parity) — and those trades belong to the owner.

## Loader integration phases (parallel track — see `loaders.md`)

- **Phase 1:** c, v, zig (`wasmrt.h`) + rs (native Rust crate) — primary early validation.
- **Phase 2:** dotnet, py (thin bindings over `wasmrt.h`).
- **Phase 3:** go (was wazero), jvm (was Chicory).
- **Phase 4:** js, dart (wasmrt-compiled-to-wasm, wasm-in-wasm).
