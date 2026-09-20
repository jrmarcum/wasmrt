# wasmrt

An idiomatic-**Rust** WebAssembly runtime — a port of the Zig runtime **`wazmrt`** (`../wazmrt`),
built to **replace wasmtime** as the engine beneath the owner's `universalWasmLoader-*` projects.

## 🔒 THE ORACLE IS RETIRED (owner, 2026-08-11). Finish, and compete.

**Do not refer back to the `wazmrt` repo.** The port is done being a port. wasmrt and wazmrt are now
**independent entrants** competing for inclusion in **wasmtk** and the **universalWasmLoader-\***
runtimes, decided on **the smallest and fastest binary**; **`rsxtk` takes wasmrt by default** through the
native Rust interface. wazmrt is running its own size/self-ownership program for the same contest, so its
head is a *competitor's* design — following it is exactly backwards.

`scripts/check-wazmrt.sh` is **deleted**; its baseline survives as `scripts/wazmrt-provenance.txt`, which
nothing reads. **Correctness anchors on the official spec testsuite, wasmtime's observable behaviour, and
the wasmtk WASI corpus** — always the harder tests. **`1.0.0` no longer means parity** — and since
2026-08-19 it no longer means "complete on our own terms" either: 🆕 **`1.0.0` IS THE CONFORMANCE
CLEAR-OUT** (T13 — the corpus to zero failed / zero skipped / zero unrun, empty baseline, zero deliberate
deviations), with hardening / bug hunt / optimization / security behind it as `1.0.1`–`1.0.4`. Provenance is **not** retired — `cmem/licensing.md`'s attribution stays.

⚠️ **This re-weights the remaining work.** While the oracle defined success, *canonical* was the gate and
fast/small were aspirations with a footnote. **Now fast and small ARE the gate**, so **T11 decides the
contest** and carries three items that were footnotes: the unattributed **~5% steady-state regression**;
the fact that **the rlib has never been measured** though it is what `rsxtk` links (every recorded size
figure is for an artifact the *default* consumer does not use); and **no same-machine comparison** against
wasm3, WAMR or wazmrt. ⚠️ The second is §3.8's mistake again — measuring the thing in front of you rather
than the thing the consumer uses.

## Where the port actually is (keep this line current)

**T0–T8 DONE (published through v0.9.0); T9's eighteen passes landed 2026-08-14; 🆕 T13 — the
CONFORMANCE CLEAR-OUT (`1.0.0`) — IS IN PROGRESS, day 4 landed 2026-09-19 (track A done as the custom-annotations feature; the shipped cdylib −62% after `rlib` was found defeating LTO; the import type-use check #4 closed; TRACK P custom-page-sizes done — `PAGE_SIZE` deleted, byte-granular bounds cross-checked on wasmtime; 🎯 **TRACK D (D1–D4) done — the corpus is at ZERO failed / ZERO skipped / ZERO unrun**), all unreleased.**
wasmrt assembles, decodes, validates, runs, does WASI preview 1 with a sandboxed filesystem, and is
**embeddable from C** via `wasmrt.h`. Spec suite 🎯 **100%** (**64,598 / 0 / 0** over **288**
files — per-file feature sets, `wast::features_for_script`), **564 workspace tests**, clippy clean, C-ABI gate PASSED, `.wat` corpus **531/535** ⚠️ *(the gate keyed on an exit status that is always 0; two pre-existing failures were invisible — all four are corpus defects wasmtime also refuses)*, no file lost a TRUE pass in any pass (day 4 removed one false one — `cmem/roadmap.md`, DAY 4). ✅ **Miri 32/32 PASSED (day 4, 2026-09-19)** — re-count it every run; it was 28 on record while the crate had 31.
🎯 **DAY 3 CLOSED X1, X2, X3/T10b, THE WHOLE M/A TRACK AND TRACK W.** `wide-arithmetic.wast`
**0/1/108 → 107/0/0**; custom-page-sizes **34 failures → 0**; and **`proposals/threads/` is at
497 / 0 / 0** after the owner-directed vendored patch. **custom-descriptors is now 65/451 — 98% of
everything that remains**; the rest is `custom/` 1/20 (track A) and custom-page-sizes 0/78 (track P).
🔻 **THE CORPUS MOVED AND THE TOTALS HID IT** — wasmtk synced the vendored testsuite to upstream on
2026-08-20 at 18:34, so day 3's real starting point was **63,963 / 112 / 604 over 288 files**, not
63,807/112/584 over 284: +4 files, +156 passes, +20 skips, and ⚠️⚠️ **the failure count matched on both
sides by COINCIDENCE**. 🎓 *When the corpus can move, the denominator is a measurement*
(`cmem/best-practices.md` §1.7).
🔴 **TWO GATE FINDINGS, both the same shape.** `op_feature`'s doc comment claimed its match was
"exhaustive over `Op`" so a new opcode could not silently default to allowed — **it ends in
`_ => return None`**, and Track W's four ops shipped **refusable by no flag at all** (measured: 182
ungated ops without the gate arm, 178 with). Then `WideArithmetic` reached the Rust enum and the struct
but **not `wasmrt_feature_t`**, so it was gated at one entry point of two — exactly what X3 exists to
prevent, and what T10b had predicted. Both are pinned by DATA now. 🎓 **A gate that cannot fail is
decoration; a gate that exists only in a doc comment is not even that** (§4.4).
🔬 **A THIRD HOLE IN THE PER-FILE GATE**: a clean file carried no row, so passes it lost to **skips**
were invisible — `memory_max.wast` went 2 passed → 0 and `conformance-diff.ts` printed *"no file lost a
pass"*. Every file prints a row now, and the same gate then reported four affected files instead of two.
✅ **`Op` IS `#[repr(u16)]` WITH ITS TAGS ABOVE `0xff` (owner-directed, 2026-09-17) — TRACK D IS
UNBLOCKED** with 0xEB free tags. Measured before adopting: **free on size** (`Instr` stays 80 bytes),
**canonical behaviour unchanged** (the 288-file suite under `u16` was **byte-identical**; `Op` never
crosses the C ABI, so the T8 freeze holds), **+7–9% faster** on the steady axis. ⚠️⚠️ **That gain has no
explained mechanism** — the number is measured, the cause is not — so it is a **T11 lead beside the
unattributed ~5% regression and NOT a figure to quote.** ✅ `decode_body`'s raw-internal-tag guard is
**deleted and cannot return**: `from_u8` takes a `u8` and no tag fits in one.
🔴 **The repartition found the SIXTH emitter defect**: `i32.trunc_sat_*` and its seven siblings were
emitted as the raw byte `0xc5`–`0xcc`, which is an **illegal opcode** — every module wasmrt assembled
containing a saturating truncation was **not WebAssembly** (LLVM emits these for ordinary float→int
casts). Our decoder accepted the same bytes, so the round trip was green. `wasmtime compile` on our
output found it in one command (§3.8b). Now `0xFC 0x00`–`0x07`, verified.
🚦 **PICKING UP TOMORROW? The handoff list is in `cmem/roadmap.md`, "HANDOFF — where to pick up".**
Nothing is half-finished. The only work-in-progress anywhere is the threads patch sitting uncommitted
in the **wasmtk** tree, on purpose, for wasmtk's own session to commit — now three passes on 2026-09-17 and 2026-09-19, both owner-directed: 8 era-pinned `assert_invalid` cases (multi-memory/multi-table) and 3 `assert_malformed` cases (`(memory 0x1_0000_0000)` — Wasm 3.0 makes it INVALID, as core `memory.wast` asserts).
🔴 **A RECORDED DECISION TURNED OUT TO BE A NO-OP, and was then done properly.** The era-pinned
`proposals/threads/` fix — "refresh the vendored snapshot" — could not be executed: the 08-20 sync
**did not touch `proposals/threads/`**, so the vendored copy already IS upstream and upstream is the
stale one. 🎓 *Verifying the premise of an option is not verifying the option* (§8.4). The owner directed
**"patch and report"**, so the snapshot is patched in place with annotated blocks quoting the eight
removed assertions, and `proposals/threads/` is now **497 / 0 / 0**. ⚠️ The patch is **uncommitted in the
wasmtk tree on purpose** (the write was authorised; committing in another repo is not) and is designed
to be overwritten by the next corpus sync — **re-check after every sync.**
🔒 **T9e + T9i LANDED 2026-09-19 — `pin` was the LAST T9 ITEM, so T9 is closed.** The pin gate
(SHA-256 in core, load-once as a TYPE, the gate before validation on every executing path **including
`wast`**, `wasmrt pin`, fail-closed policy) and the execution bound (**one loop back-edge or one
tail-call hop**, `1<<30`, `--max-iterations`) both ship, and **the CLI now accepts wazmrt's whole
shape** — bare-path run modes, `--features` (BEFORE the module, an error elsewhere), `--env`, the
ceilings, the verification flags, `--`, both `--dir` separators. 🔒 **The pin DB path is the owner's
decision: shared `/etc/wasmtk/pins` with a warning rather than a silent disarm.** ⚠️ **New finding —
`.wat` pin digests are NOT portable** (529 of 535 differ: we emit a `name` section, wazmrt does not);
`.wasm` digests agree. Handed over as **Z4**.
🚦 **NEXT: `1.0.0` release prep** (`cmem/releasing.md`). 🤝 **`coordinate` ran 2026-09-19 on exit codes**: `interop.md` §2.3 row 2 was already AGREED and wasmrt breached it three times (invalid-module summarize, no args, `wast` on FAILED assertions, all rc 0). Fixed with no contract change; annex §2.3m is offered to wazmrt. The exit status is now a usable gate. 🔒 **Then two OWNER DECISIONS (2026-09-19): `interop.md` §2.4a** (an unknown flag in a host-flag position → `unknown flag`, rc 1; guest positions after `--` or after the first guest argument are never examined — and **after the module path a SINGLE-DASH token is the guest's**, since every host flag there is double-dash, so `prog.wasm -la` needs no `--`; **no "looks like" heuristic**, by owner decision) **and §2.5** (no output line may call a module valid unless it validated; the one output-text exception to §0). **Z1–Z3 are handed to wazmrt through the contract (§2.5h)**, and wasmrt already complies. ⚠️ wasmrt's own **F1** (`wasmrt m.wasm f` summarizes with rc 0) is still open under §2.1. ✅ The two 🟡 OPEN decoder defects are CLOSED (2026-09-19) — and were THREE: 20 invalid value-type encodings accepted, 3 valid block types refused, `select (result (ref …))` undecodable; one reader now (`opcode::read_value_type_from`), found by sweeping all 64 one-byte encodings rather than reading the table (`cmem/best-practices.md` §1.8); `tests/value-type-encodings.wast` is passed whole by wasmtime 48. TRACK D3/D4 DONE 2026-09-19: corpus **64,598 / 0 / 0**; `tests/descriptor-casts.wast` is the soundness checkpoint; ⚠️ core and proposal `br_on_cast.wast` contradict each other, so the runner now picks features PER FILE. *(Was: NEXT TRACK D3/D4 —* D1 (exact types) and D2 (descriptor/describes) DONE 2026-09-19; D2 removed 71 FALSE passes (modules that failed to assemble were scoring `assert_invalid` passes). D1 with its soundness checkpoint met (`tests/exact-type-confusion.wast`). ⚠️ wasmtime 48 has NO custom-descriptors: wasm-tools is the outside reader (encoding + validity). **TRACK D — custom-descriptors** (2026-09-19). Tracks A and P, #4, the cdylib dead-code leak and both
threads-snapshot patches are DONE (`cmem/roadmap.md`, DAY 4 parts 1–3). *(Was: track A → the two owner decisions
above → track P → track D.)* The 2026-08-20 scoping below is now the record of how it was planned; its
cross-cutting items and track W are done. *(Superseded scoping follows.)* — `cmem/roadmap.md`, "THE REMAINING
WORK, SCOPED". Three cross-cutting items first — **X1** refuse `(pagesize N)`, **X2** the `ModuleBuild`
field-coverage sweep (T10a's open half), **X3** a proposal's `Feature` gate is a track DELIVERABLE
(`Feature` had 15 members and **none of the three new proposals**; it has **16** now, with wide-arithmetic gated at both entry points) — then four tracks ranked
**W** wide-arithmetic (1f/108s, best ratio) → **P** custom-page-sizes (34f/7s, 🔒 the memory-safety
one) → **M/A** threads (13f/18s, ⚠️ triage before costing) → **D** custom-descriptors (64f/451s,
largest and riskiest; **D1 `(ref (exact $t))` changes SUBTYPING** and carries a type-confusion
soundness checkpoint). ⚠️ **436 of D's 451 skips and 99 of W's 108 are CASCADES** — read the skip
column to RANK, work the roots to FIX. ⚠️ Two predictions are recorded so honest movement is not
read as regression: **passes go DOWN when `exact` lands** and **skips RISE when X1 lands**.
🔴 **SCOPING FOUND A LIVE DEFECT — `(memory 1 (pagesize 1))` assembles, validates and RUNS today**,
emitting bytes byte-identical to `(memory 1)`: the clause is parsed and thrown away, so a guest
asking for a 1-byte memory silently gets 64 KiB and every access that must trap succeeds. wasmtime
refuses the same **source**. ⚠️ The first cross-check against wasmtime *agreed*, because it was given
our emitted bytes rather than the source — 🔒 **when the emitter is the suspect, compare the SOURCE
across engines.** Fifth instance of the T10a emitter mechanism, and like the first four it was found
by accident. `cmem/known-issues.md` (top), `cmem/best-practices.md` §5.8.
⚠️⚠️ **THE FINDING IS NOT IN THE CONFORMANCE NUMBERS.** Two of the day's fixes were **wire-format
divergences the spec suite structurally could not see**, because wasmrt's assembler and decoder agreed
with each other: every **non-null abstract reference type** was emitted as `ValType`'s *internal tag*
(`(ref any)` as the single byte `0x66`) instead of §5.3.5's `0x64 <heaptype>`, and **`try_table`'s
catch label was off by one** in the assembler, the validator and the interpreter alike. **wasmtime 47
refuses both.** 🎓 *Agreement between components that learned the convention from each other is not
evidence* (`cmem/best-practices.md` §3.8b) — **hand wasmrt's output to an outside reader**
(`wasmtime compile out.wasm`) whenever a format-level change lands. It is one command and it found
both in an afternoon.
⚠️ Four more were **wrong ANSWERS wearing a conformance-failure label**: a flat-form `offset=` was
silently dropped (`i32.const 0 i32.load offset=4` read at offset **0**), reference locals were
zero-filled so an uninitialized `(ref null $t)` read as **GC object 0**, an exception's tag was a
module-local index so tag 0 caught tag 0 across a link, and an imported global was bound **by value**
rather than shared. Each needs two modules or a shared store to be visible — which is why 63,000
assertions had not seen them.
⚠️ **The `.wat` corpus went 532/532 → 529/532, and all three are CORPUS defects** wasmtime refuses on
the same lines (two ArtOfWebAssembly files spell `anyfunc`; `dynrt_lib_modc.wat` declares
`(local $alist i32)` four times in one function). Reported, not worked around — not our files.
🔒 **The order held.** "All failures first, then the skips" was probed rather than adopted and played
out as measured: the failure pass cleared most of the cascades, and S1 — the externref bridge, the one
item phase 1 provably could not reach — delivered 229 assertions against a logged 181.

*Day 1 (2026-08-19) moved 62,238 / 378 / 2,038 → 63,333 / 172 / 1,024 across ~20 fixes and ended on the
INSTRUMENT: the runner listed a file only when it had failures, so 234 skips lived in files that printed
nothing, and a skip was a bare counter at all twenty sites. The census that followed showed 928 of 1,024
skips (91%) were CASCADES behind a handful of roots, which is what produced the F1–F7 / S1–S7 scoping.
🎓 A number the report cannot attribute is not a measurement (`cmem/best-practices.md` §5.6).*
✅ **The `.wat` corpus is VERIFIED at 529/532** (2026-08-20, assemble→decode→validate over all 532
files in the wasmtk tree — the denominator that had been ⚠️ unverified since 2026-08-19). The three
failures are **corpus defects wasmtime refuses on the same lines**, not ours.
**Every CLI command that takes a module
now accepts that `.wat` directly** — `run`, `wasi` and summarize assemble text before decoding, through one
shared loader. ⚠️⚠️ **That gap had been open for the whole port and was found by an owner QUESTION, not a
test — the second in two days.** It is structural: every gate here compares *answers* on inputs both
runtimes accept, so a capability one runtime simply does not offer produces nothing to diff
(`cmem/best-practices.md` §3.8). **T9b/T9c/T9d done, so all three success axes carry a real
measurement** — cold start **4.48 ms** at 48 KB, **~237 Mops/s** steady, CLI **621 KiB** / cdylib
**493.5 KiB** / freestanding `wasm32` engine **158.1 KiB** (137.5 KiB with `wasm-opt -Oz`).
**2026-08-08: T9a#4's memory half landed** (owner chose option 2 — imported **memories** link and are
genuinely shared; imported **tables** stay refused because a `funcref` carries no instance identity, so a
shared table would dispatch to the wrong function). That pass also implemented `assert_unlinkable`, whose
first run showed **imports had never been type-checked at link time** — ⚠️ **a blanket skip is not a
neutral placeholder; that one had been insuring a silent-wrong-call defect since T7b.**
**The second pass closed T9a#11 and most of #12 — decoder strictness**: seven checks making the *decoder*
the stage that rejects a malformed binary (section order/uniqueness, section size, func/code count, bodies
decoded at decode with the IR stored in `Code`, const-expr encodings, the `end` terminator, the 2^32−1
locals ceiling). ⚠️ **Section order is not id order** (`DataCount` is 12 but precedes `Code`; `Tag` is 13
but precedes `Global`), and a repeated section had been **silently replacing** the first.
**The third pass fixed a defect the owner's own constraint surfaced** (*"the memory needs to be shared once
pulled in; two memory profiles pulling from each other does not work"*): an `InstanceId` was a bare index
with no record of its issuing store, so an id from store X resolved against store Y and a guest **silently
shared the wrong memory** — and `code[id]` indexing **panicked** on a foreign id, a process kill under
`panic = "abort"`. Now `InstanceId { store, index }` with every accessor through one `Store::slot()` — the
defence the C ABI already applied to its value handles at T8; core held the weaker of the two, and that
asymmetry *was* the bug. Mutation-verified; conformance unchanged, because it is a misuse path the spec
suite cannot reach. ⚠️ **A stated constraint is worth PROBING, not agreeing with** — two of the three
properties it implied held, the third had shipped broken that morning.
**The fourth pass fixed declared subtyping, which was never validated at all** — 21 invalid modules were
accepted (T9a#6, whose logged cause was again wrong). Finality is now enforced (the decoder had read
`0x50` and `0x4f` identically, discarding it) plus §3.4.5 structural matching; it also caught the
**assembler silently turning open types final**. ⚠️ **A strict version was measured and rejected — it
refused 6 VALID modules; the direction to err in is a property of the consequence, not a house style.**
**The fifth pass implemented TYPE CANONICALISATION.** Rec groups are reduced to structural keys at decode,
so two groups spelling out the same shape are **one type**; `is_subtype` compares canonical ids, and since
every subtype question in the engine funnels through it, one line carried the fix. It also caught
`call_indirect` comparing signatures by **raw bits** (a second site), and 🆕 **the assembler flattening every
`(rec …)` group — `0x4e` was never emitted**, silently changing what its types *were*. That is the **third**
"assembler emits a different module than the text describes" defect in two passes, so the emitter deserves a
dedicated look at T10. Canonicalisation also let the previous pass's `decl_subtype_of` approximation be
**deleted**, with byte-identical suite results.
**The sixth pass is T9h — DONE: the `Store` type registry**, wasmtime's *shape* and our code (the third
application of the 🔒 *"wasmtime's SHAPE, our code"* rule in `design-decisions.md`; the **Component Ledger
stays empty**). Rec groups intern as modules join the store, so cross-module matching is an integer
comparison at **link** time, never on a hot path. **`type-subtyping.wast` reached 72/0/0** and every
`Unlinkable: module linked` in the suite is gone. ⚠️ **The finding: comparing SIGNATURES can never answer an
IDENTITY question** — two functions can both be `(func)` and still be different types, because rec-group
membership is part of identity and only the type **index** carries it; the decoder had been resolving an
import's typeidx to a structure and **discarding the index**. Also **§4.5.9 matching is subtyping, not
equality** (so the registry records supertypes store-wide), and 🆕 **`call_indirect` was a third site with
the identical defect**, worth 7 runtime assertions.
**T10a — the EMITTER audit**: three "the assembler emits a different module than the text describes"
defects in two passes, **all three found by accident** by some other check reading a field the emitter had
dropped. T10a names the mechanism (the emitter reconstructs a form from a *subset* of the parser's facts)
and specifies a **round-trip property test** + a **`ModuleBuild` field-coverage sweep** instead of a
read-through — every existing test asserts the module *runs*, and all three defects produced modules that ran.
**The seventh pass enforced the TEXT format's own grammar.** A type use has a fixed clause order (§6.4.4) —
`(type x)?` then `(param …)*` then `(result …)*` — which the assembler ignored, so
`(block (result i32) (param i32))` assembled and the *validator* reported it as a stack-height mismatch.
**`block`/`if`/`loop` all went 13 failures → 0.** Also: a block parameter cannot be named, and `(type x)`
plus explicit clauses must match (they were silently discarded). ⚠️ **Where a guard lives matters — the
first attempt put the rule in `parse_sig` and moved ONE assertion of forty, because `parse_block_type`
calls it one clause at a time; a guard one call-level away from the iteration it guards is no check at all.**
**The eighth pass applied those same three rules at their OTHER TWO sites.** Block types, `call_indirect`
and function definitions each had their **own copy of the type-use loop**, and so its own copy of all three
defects; `parse_type_use` is now one authority for the first two. **`call_indirect.wast` → 169/0/0**,
`func.wast` 21 → 8. ⚠️ **One rule was attempted, measured and WITHDRAWN** — "no declaration after the body
begins" broke `select`/`stack`/`call_indirect`, because in **flat** instruction form `select (result i32)`
puts its immediate exactly where a misplaced declaration would sit, so keyword scanning cannot tell them
apart. **A rule that is obviously right is still a hypothesis until measured.**
**The ninth pass COMPLETED T9a#4.** A `funcref` now carries its owning instance (bits 62..32 — bit 63 is
`I31_TAG`), so imported **tables** are linkable and `call_indirect`/`call_ref` dispatch into the
reference's *owner*. ✅ **Instance 0 packs to the bare index**, so the encoding alone moved the suite by
+1/−1 — a value-model change arranged to be verifiably a no-op on the existing corpus. 🆕 **It caught a
defect in that morning's memory work**: a table's or memory's *instance* type has `min = CURRENT size`
(`grow` updates it, §4.5.9), but the memory pass stored the **declared** minimum and asserted that in a
test — ⚠️ **a test can encode a misreading of the spec and pass forever if nothing exercises it.**
⚠️ **~5% steady-state regression, unattributed and handed to T11** (two hypotheses tested and rejected;
cold start unchanged).
**The tenth pass did T9a#5, GC constant expressions** — six forms in both validator and interpreter.
⚠️⚠️ **Its lesson: a cost counted in FAILURES understates any defect that stops a module BUILDING.** Logged
at 6, it delivered **88**: `ConstantExpressionRequired` on a global initializer fails the whole module, so
every later assertion in the file was skipped. `i31.wast` was 0/6/**66 skipped** → **61/2/5**. **Read the
skip column when triaging.**
**The eleventh pass did T9a#7, trap backtraces — and found that THE START FUNCTION NEVER RAN.** #7 is
diagnostics and was correctly predicted to be worth 0 assertions; `Instr` gained an `offset: u32` that
is **free** (it fits the padding `Imm`'s alignment already forced — pinned by a `size_of` test so a
future `Imm` shrink fails the build), frames are built **on the way out** rather than as a shadow
stack, and the C ABI frozen at T8 went live with **no ABI change**. ⚠️ **The lesson: the PLUMBING for a
cold feature can cost the hot path.** Threading `pc: &mut usize` through `run` **measured 3.6% slower**
on the steady loop; a one-shot closure recovered it — a permanent tax on the "fast" axis would have
been paid invisibly. 🆕 Asking where an *instantiation* trap gets its frames led to asking who runs
`Module::start`: **nobody**. §4.5.5 step 11 was absent while the field was decoded, validated and
printed by the CLI — worth **+12 assertions**, 10 of them sitting in files named `start.wast` and
`start0.wast` for five releases. ⚠️⚠️ **A feature can be fully decoded, validated and printed and still
never execute; before diagnosing a file's failures, check that the feature it is NAMED for works.**
**The twelfth pass enforced the TEXT FORMAT'S SOURCE CHARACTER SET** (§6.2/§6.3) — and its finding is
about a **scope note**, not a bug. T9g had `annotations` **51** filed under *scope confirmations — NOT
bugs, do not "fix"*. True of the **file**; false of **44 of its 51 assertions**, which were generic
lexer rules applying to every `.wat` wasmrt reads. Four one-line probes, **none containing an
annotation**, were all accepted: a control character in an identifier, invalid UTF-8 in an identifier,
a raw control byte in a string. ⚠️⚠️ **A SCOPE note is a hypothesis about a cause too — and it is the
one nobody re-measures, because it is filed under "not a bug".** The proof the cause was generic is
`id.wast`, unrelated to annotations: **5 failures → 0**. 🆕 It also found `from_utf8_lossy` in the
lexer **silently RENAMING** malformed identifiers — `$a\xffb` and `$a\xfeb` both became `$a\u{FFFD}b`,
so two distinct identifiers collided on one name. ⚠️ **One probe of the four was NOT a defect and was
left alone** — `linechar` admits any character but a newline, so a control byte in a *comment* is
legal, and a test pins that it stays accepted: **tightening what the grammar permits is the same error
as accepting what it forbids.**
**The thirteenth pass did T9a#8 — logged at "1 wasmtk file", it delivered 76 assertions.**
`immediate_arity` ended in `_ => 0` and the emitter's match in `_ => {}`, so **four instructions were
emitted as bare opcodes with their operand left in the token stream**: `call_ref`/`return_call_ref`
(typeidx) and `br_on_null`/`br_on_non_null` (labelidx). All four decode and execute correctly — only
the *assembler* was wrong. ⚠️ **None of the three symptoms named the assembler**: folded `call_ref`
shifted every following byte into a body the **decoder** rejected for a missing `end`, and the flat
forms blamed the *next* token with `UnknownInstr` — which is why `gc-linked-list.wat` sat as an
unexplained "1 of 534" logged **separately from #8**. One cause, three symptoms, two punch-list items.
`call_ref.wast` → **31/0/0**, `br_on_null.wast` → **7/0/0**, `ref_as_non_null.wast` → **5/0/0**,
`unreached-valid.wast` → **10/0/0**. 🆕 **The gate had been measuring the wrong thing** — the `.wat`
corpus check ran *assemble only* and read 533/534 while `call_ref`'s output was undecodable.
**"The assembler returned Ok" is evidence about the parser, not the emitter**; the corpus gate is now
assemble→decode→validate, and reads **534/534 with 0 decode failures**. ✅ **T10a's field-coverage
sweep landed for opcodes** — `Op::from_u8` makes the opcode space enumerable, so a test walks all 256
single-byte opcodes and asserts the decoder and assembler agree about which take an immediate. That is
the **fourth** instance of the emitter mechanism; T10a predicted "more than three" and is 4-for-4.
⚠️ **Mutation-verified — and the first attempt LIED**: a `perl` substitution silently failed to match,
the test passed, and that reads exactly like "my check is decoration". **Confirm the mutation applied
before believing the mutation test.**
**The fourteenth pass resolved T9a#9 as NOT A DEFECT — the right outcome was to change nothing.** The
fixture is ill-typed (`if (result f64)` with both arms pushing i32, which `if.wast` makes an
`assert_invalid`); the two files are **byte-identical duplicates** of one **stale** artifact
(8 funcs/10 types vs the source binary's 14/13). ⚠️⚠️ **The logged premise was the error: "the oracle
assembles AND RUNS them" is true, but `wazmrt <module> <export>` DOES NOT VALIDATE** — through its
validating path the oracle reports `TypeMismatch` too, agreeing with wasmrt. **Cite the subcommand,
not the tool.** What landed instead: validation failures now name the function
(`validation FAILED in function 8`), since localizing this by hand is what the item actually cost.
**2026-08-10 — the validation / symlink round.** ⚠️ **`wasmrt run` executed WITHOUT VALIDATING** while
`wasmrt wasi` refused the same bytes — **the asymmetry was the bug**, and the oracle had it worse (both
execute paths *and* its C ABI). Both fixed; an invalid module now reports offset + function +
expected-vs-found, **matched byte-for-byte against wasmtime 47** (same offsets, 33 and 61, pinned as
tests). 🔒 **Symlink CREATION is now denied by default** (owner): `--dir` grants `ALL & !PATH_SYMLINK`,
`--allow-symlink` opts in for installers — enforced by `const` assertions, so a violation fails the
**build**. ⚠️ Chasing that found **a real hole in the oracle**: wazmrt had **no `path_symlink` right at
all**, so `--ro-dir` never stripped it and a guest could plant links in a read-only preopen.
🔒 **The oracle is no longer at its original freeze** — it moved four times on 2026-08-10, each
owner-authorized and deliberately re-baselined; `check-wazmrt.sh` reports NO DRIFT.
**T9a #1–#9 and #11 are now all closed.** ✅ **Tail calls landed 2026-08-14 (T9f), so no in-scope
proposal is missing**, and `func.wast` 8 (the withdrawn body-order rule + duplicate identifiers) moved
to T10 with the other bugs (owner, 2026-08-14). ~~**`T9e pin` is the ONLY T9 item left.**~~ ✅ **T9e AND T9i landed 2026-09-19 — T9 is CLOSED.**
🆕 **THE LADDER WAS RE-CUT (owner, 2026-08-19).** `1.0.0` is no longer "complete on our own terms" — it
is **T13, the CONFORMANCE CLEAR-OUT**: the corpus to **zero failed / zero skipped / zero unrun**, an
empty baseline and **zero deliberate deviations**, which brings the proposals the corpus contains into
scope (custom-descriptors/`exact`, table64, wide-arithmetic, custom-page-sizes, custom-annotations,
memory64-imports, legacy `delegate`). **It runs FIRST.** Behind it: **T9 hardening `1.0.1`**, **T10 bug
hunt `1.0.2`**, **T11 optimization review `1.0.3`**, **T12 security review `1.0.4`** — **clear → measure
→ find → optimize → attack**. ⚠️ **T-numbers are IDENTIFIERS, not an order** (they are cited across
every `cmem/` file), so only the ordering moved. ⚠️ **T13-0 comes before any implementation**:
instrument the skips and audit the runner's SCORING — that phase alone was worth 292 skips→passes and
14 misfiled failures in the other runtime. **Rank by ASSERTIONS UNBLOCKED, not by failure count**, and
expect failures to RISE honestly as skips convert — the gate is *no file lost a pass*, per-file.
✅ **`pin` is DECIDED (owner, 2026-08-19)** — gate load-once, build it to wazmrt’s application, and be **SWAPPABLE**: *“if our CLI options are the same they are swappable — if our security checks are the same they are also swappable.”* Buildable spec in `cmem/security-model.md`; work breakdown in `roadmap.md` T9e. ⚠⚠ Two swappability breaks are live **today, unrelated to pin**: `--dir` uses `:` in wazmrt and `::` in wasmrt (so `--dir .:/` silently preopens the wrong thing here), and aligning the run modes makes a **bare path EXECUTE** a `_start` module where wasmrt currently summarizes — land the pin gate before that, never after.
⚠️ **T9a#1 taught that a cost logged beside a defect is a hypothesis about its cause**: the `ref.null`
fix was real but did not move `br_table.wast`, which needed three further unlisted fixes. The full set
of recurring lessons is in [`cmem/best-practices.md`](cmem/best-practices.md); detail is in
[`cmem/roadmap.md`](cmem/roadmap.md); [`cmem/INDEX.md`](cmem/INDEX.md) carries the fuller status.

## Project memory lives in `cmem/` — read it first

Portable project memory (committed, travels with the repo) is in **[`cmem/`](cmem/)**, one topic file
per domain — same convention as wazmrt. **Start with [`cmem/INDEX.md`](cmem/INDEX.md).**

- [`cmem/overview.md`](cmem/overview.md) · [`cmem/vision.md`](cmem/vision.md) — what/why (canonical / fast / small)
- [`cmem/architecture.md`](cmem/architecture.md) · [`cmem/design-decisions.md`](cmem/design-decisions.md) — crates, dual-target, invariants NOT to drift
- 🤝 [`cmem/interop.md`](cmem/interop.md) — 🆕 **THE wasmrt ⇄ wazmrt SWAPPABILITY CONTRACT (owner, 2026-08-19).** The **only** file either project may treat as binding on the other, IDENTICAL in both repos, **CONTRACT VERSION 1**. 🔒 The oracle stays retired — this is a contract about *observable behaviour*, not permission to follow a competitor’s design. **IN: the CLI options + the security checks. OUT: the C ABI, internals, and performance/size** (that is the contest). **Read it before changing a CLI flag, the pin mechanism, the WASI rights masks or a resource ceiling** — those are contract surfaces now, and neither side edits it unilaterally.
- [`cmem/loaders.md`](cmem/loaders.md) — the `universalWasmLoader-*` consumers + the `wasmrt.h` surface
- [`cmem/testing.md`](cmem/testing.md) · [`cmem/security-model.md`](cmem/security-model.md) · [`cmem/licensing.md`](cmem/licensing.md)
- 🎓 [`cmem/best-practices.md`](cmem/best-practices.md) — **the lessons this port paid for, each with the
  defect that bought it. READ THIS BEFORE STARTING A TASK.** Among them: a logged cost is a hypothesis
  about a *cause* (wrong 3×); a stated constraint is worth **probing**, not agreeing with; silent-wrong-output
  is where every serious defect has been; a gate that cannot fail is decoration; a guard one call-level away
  from the iteration it guards is **no check**; which direction to err in is a property of the *consequence*.
- [`cmem/roadmap.md`](cmem/roadmap.md) · [`cmem/known-issues.md`](cmem/known-issues.md) · [`cmem/reference-projects.md`](cmem/reference-projects.md)
- [`cmem/releasing.md`](cmem/releasing.md) — versioning ladder + the binding per-release checklist

Detailed engineering reference (the wazmrt deep-read maps) is in [`docs/port/`](docs/port/).
⚠️ **`docs/port/wasmrt.h.draft` is HISTORICAL** — four of its shapes never matched the code. The
finalized C ABI is [`crates/wasmrt-capi/include/wasmrt.h`](crates/wasmrt-capi/include/wasmrt.h).

## 🤝 "**coordinate**" = sync with `wazmrt` through the contract (owner, 2026-08-19)

🗓️ **CADENCE — who holds the pen (owner, 2026-08-19).** **Regime A is in force: `wazmrt` LEADS.** It is
finishing its hardening stage, **its copy of the contract is the latest**, and wasmrt **mirrors from it
and does not originate version numbers** — wasmrt still measures, still objects, and contributes
findings as an **annex** for wazmrt to fold in. **Regime B starts once wasmrt also finishes hardening**
(T9 → `1.0.1`): the two then **cross-coordinate at the END OF EACH STAGE** of the `1.0.x` ladder.
⚠️⚠️ **That is a gate at a stage boundary, not a background activity** — reconcile against code that has
stopped moving. 🎓 **A CONTRACT VERSION is a PIN, not a LOCK**: it makes drift detectable and cannot
prevent a simultaneous write, which is why three mid-edit collisions happened in one session on
2026-08-19. **wasmrt is two stages from regime B** (`1.0.0`, the clear-out, is at its conformance target — release prep is next).

**One word, binding.** When the owner says **"coordinate"**, synchronise this project with the sibling
runtime through [`cmem/interop.md`](cmem/interop.md) — the swappability contract, versioned and kept
identical in both repos. Read it first; **byte-compare the two copies**; **verify rows by RUNNING both,
never by reading either**; record status + date + evidence; **bump the CONTRACT VERSION in THIS repo's
copy**; and treat a disagreement as an **observation until its cause is traced** — neither runtime is
the oracle.

🔒🔒 **NEVER WRITE INTO THE OTHER PROJECT'S TREE** — not its source, not its `cmem/`, **not its copy of
`interop.md`** — **unless the owner directs it that time** (owner, 2026-08-19: *"each project needs to
edit their own files … important for tracking and integrity"*). **Reading the sibling is free; writing
to it is not.** Propose the change and let their own session adopt it in their own commit: a cross-repo
write launders authorship and destroys uncommitted work, both of which happened on 2026-08-19. A
version mismatch between the copies is therefore a **normal in-flight state** — an unadopted proposal —
not an error; only a row both copies carry is binding. 🔒 Scope is **the CLI options and the security checks only**; this does *not* reopen the oracle
for design, and **performance/size are explicitly out** — that is the contest.

⚠️ **The inverse binds too, and it is the half that gets skipped: coordinate BEFORE shipping a change
to a contract surface, not after** — any CLI option, any security check (pin, WASI rights,
`--dir`/`--ro-dir`), any resource ceiling, any exit code. A change shipped without coordinating is how
the two stop being swappable, and it will not announce itself. The full procedure is the binding trigger
in [`cmem/INDEX.md`](cmem/INDEX.md).

## 🔒 Tooling rules (owner, 2026-09-19)

**Scripts are TypeScript, run by Deno or Bun** — `deno run -A scripts/x.ts` / `bun scripts/x.ts`,
importing `node:` builtins only so one file runs under either. That covers `scripts/` *and* ad-hoc
scripting in a session. The four gates were ported the same day and each was verified to still FAIL
when it should. **⛔ And no heredocs**: write the script to a file and run the file. A heredoc's
quoting has silently mangled non-ASCII, executed backticks it should have quoted, and made an edit
apply nothing while reporting success — the silent-wrong class. Detail and the evidence:
`cmem/design-decisions.md`, "TOOLING RULES".

## "Update the project memory" = update `cmem/`

When asked to record/remember anything for the project, fold it into the matching `cmem/` file(s) and
refresh the `cmem/INDEX.md` Files table (see the binding triggers in `cmem/INDEX.md`), then commit +
push (`origin` = github.com/jrmarcum/wasmrt). `README.md` is public-facing docs, NOT project memory.

## 🔒 Release-prep directive — docs BEFORE the publish handoff (owner, 2026-07-31)

**When a release is finished, the project memory (`cmem/`) AND the roadmap SHALL be fully updated and
committed BEFORE the owner is notified of the publish needs** — so the very commit that gets published
already carries the updated docs. Do not defer the memory/roadmap sync to "after publish": that ships a
release whose docs lag the code. Concretely, the release commit must land *all* of `cmem/` +
`ROADMAP.md` + `CHANGELOG.md` + `README.md` + the version bump together, and only *then* hand over the
`cargo publish` / `git tag` / `gh release` commands. The binding per-release checklist lives in
[`cmem/releasing.md`](cmem/releasing.md).
