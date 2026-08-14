# Best Practices — the lessons this port actually paid for

Every rule here was **bought with a defect**. Each carries the evidence, because a rule without its
scar is advice, and advice does not survive a deadline. Written 2026-08-08, after the T9 passes made
the same few mistakes visible enough to name.

Read this before starting a task. It is not a style guide — it is a list of the ways this codebase has
been wrong, which is a much better predictor of how it will be wrong next.

---

## 1. Measurement

### 1.1 A logged cost is a hypothesis about a CAUSE, not a measurement

**Wrong four times.** (a) `ref.null $t` was credited with "161 skipped assertions in `br_table.wast`" —
the fix was real but moved *other* files; `br_table` was failing three fixes earlier and needed **four**
independent fixes. (b) T9a#11 and #12 were logged as separate items with different symptoms; they were
**one theme in two halves**. (c) T9a#6 read "GC subtyping depth not modelled"; the measured top item was
**21 invalid modules being accepted**, because there was no declared-subtype validation *at all*.
(d) T9a#5 was logged at 6 and delivered **88** — a different failure of estimation again, and the one
§1.3 below is about.

**Apply:** re-measure before starting any punch-list item. A cost figure assigned by reading a file for
"the first construct that looks unsupported" is invention. Do not bank predicted value.

### 1.1a A SCOPE note is a hypothesis about a cause too — and it is the one nobody re-measures

T9g listed "`annotations` **51** — a proposal wasmrt does not target" under *scope confirmations
(NOT bugs — record, do not "fix")*. True of the file. False of **44 of its 51 assertions**, which
were generic §6.2/§6.3 lexer rules applying to every `.wat` wasmrt reads.

Disproving it took four one-line files and about a minute — none of them containing an annotation:

```
(module (func $a\x01b))    accepted; \x01 is not an idchar
(module (func $a\xffb))    accepted; source text must be valid UTF-8
(module (data "\x01"))     accepted; stringchar requires c >= U+20
(module (func) ;;\x01 )    accepted — and CORRECTLY: linechar allows it
```

The proof it was generic is `id.wast`, which has nothing to do with annotations and went from 5
failures to **zero** on the same fix.

A cost figure gets re-measured because §1.1 says so. A *scope* note reads as settled — it is filed
under "not a bug", so nobody returns to it. That makes it the more dangerous of the two.

**Apply:** a scope note names a **file** or a **proposal**; a defect has a **cause**. Before writing a
cluster off, spend the minute it takes to reproduce it *outside* the thing you scoped out. If it
reproduces, the scope note is about the tests, not about the bug.

### 1.2 Survey the measurement, don't just read the entry

The roadmap said the remaining text-parser work was "`func.wast` 21". Surveying the worst in-scope files
showed **`block`/`if`/`loop` at 13 failures each with an identical breakdown** — the signature of one
shared cause, and nearly twice the logged size. Identical numbers across sibling files are a tell.

### 1.3 A cost counted in FAILURES understates any defect that stops a module BUILDING

**T9a#5 was logged at 6 and delivered 88.** The discrepancy was structural, not an estimating error:
`ConstantExpressionRequired` on a global initializer fails the whole *module*, and every later assertion in
that file then has no target and is **skipped**. `i31.wast` read 0 passed / 6 failed / **66 skipped** — the 6
was the visible cost, the 66 the actual one. Three GC files carried 126 skips between them.

**Apply:** read the **skip** column, not just the failure column. A defect in a module-level position — a
global initializer, a type definition, a section — is worth its file's skips.

### 1.4 Diff the OUTPUT counts, not exit codes

A pass/fail count that moved is evidence; an exit code is not. This is binding across the project.

### 1.5 Know what your measurement tool omits

The `.wast` runner prints a per-file line only when `verbose || failed > 0`. So a file reaching **zero
failures disappears** from a non-verbose run, and a diff keyed on those lines reads that as the file
losing all its passes — a false alarm it raised in earnest (`br_on_cast_fail` looked like −6 passes; it
was +2). **Run `wasmrt wast <dir> -v` on both sides when diffing per-file.**

The direction of that flaw was *false alarm*, never *false all-clear*. Work out which direction your
tooling can lie in; one of them is survivable and the other is not.

### 1.5a A gate that runs one stage of a pipeline is evidence about that stage ONLY

The `.wat` corpus gate ran `wasmrt wat <file> -o out` and counted exit codes: **533/534**. It had been
quoted for months as evidence that the text toolchain was sound. It was not — it was evidence that the
**parser** accepted 533 files. Meanwhile the bytes the *emitter* produced for `call_ref` were
**undecodable**, and the gate could not see it, because it never decoded anything.

Re-running the same corpus as **assemble → decode → validate** immediately showed the real state, and
turned a punch-list entry logged as "1 wasmtk file, undiagnosed" into a 76-assertion fix.

**"The tool returned Ok" is a claim about the half of the tool that runs before it returns.**

**Apply:** for any pipeline (assemble → decode → validate → run), a gate must traverse **every** stage
whose output the next stage consumes. When you inherit a gate, ask what it actually executes, not what
it is named. `wat -o /dev/null` and `wat -o out && decode out` differ by one defect class, and it is
the class this project keeps finding (see §3.2).

### 1.6 Benchmarks: same-session A/B/A, and say when it is noise

Run-to-run spread is several percent and a single session drifted **~10%**. Report "unchanged within
noise" when that is what the numbers say — do not pick the favourable run. Cold-start numbers are
meaningless without the module size beside them.

### 1.7 The PLUMBING for a cold feature can cost the hot path — measure the plumbing, not the feature

T9a#7 (trap backtraces) is pure diagnostics: it does nothing until something traps. But making the
trapping position *observable* meant getting `pc` out of `run`, and the obvious spelling —
`pc: &mut usize` threaded through the loop — **measured 3.6% slower** on the steady-state benchmark
(2160 ms vs 2083 ms, A/B/A). The deref does not survive the opaque calls in the loop body. Wrapping
the loop in a one-shot closure keeps `pc` a plain local that LLVM holds in a register, while still
leaving it readable after any exit: **2087 ms against a 2083–2095 baseline band** — free.

The trap is that the *feature* is obviously cold, so the change reads as obviously free, and nobody
benchmarks it. A permanent 3.6% tax on a stated success axis would have been paid invisibly.

**Apply:** benchmark any change that touches a hot loop's *locals or signature*, however cold the
feature motivating it. And when a design rests on a size or layout claim ("this field is free, it
fits in existing padding"), **pin it with a `size_of` test** so the day the claim stops holding is a
build failure, not a silent regression.

---

## 2. What to distrust

### 2.1 A stated constraint is worth probing, not agreeing with

The owner stated a sharing constraint that implied three properties. Two held. The third had shipped
**broken that morning** — an `InstanceId` carried no record of its issuing store, so an id from store X
resolved against store Y and a guest silently shared the wrong memory. Agreeing would have left it in.

**Apply:** turn each clause of a stated constraint into a property and test it. Agreement is not
verification.

### 2.2 A pre-implementation design document is a hypothesis — diff it against the code

Four shapes in `docs/port/wasmrt.h.draft` never matched the real core: per-proposal config toggles (core
had no gating, so they would have been silent no-ops), a name-keyed linker (core resolved
*positionally*), a store-attached WASI config (core builds WASI per module), and a promise of backtrace
text (there were none until T9a#7, two stages later).

### 2.2a A stated BENEFIT is a hypothesis about someone ELSE's code — go read theirs

wazmrt chose to implement the **standard wasm-c-api** partly on this recorded payoff: *"wasmtime also
implements wasm-c-api, so ports already on its C API are close to drop-in."* The intended consumer was
always the `universalWasmLoader-*` projects — they are the layer meant to standardise imports across
languages, and the runtime beneath them only ever had to serve them.

**Nobody read the loader's source until wasmrt's T8 survey.** wasmtime ships **two** C surfaces —
the standard `wasm.h` and its own richer `wasmtime.h` — and the loader uses the **`wasmtime_*`**
store/context/linker model, not wasm-c-api at all. The payoff never existed.

And the mismatch was structural, not cosmetic: **wasm-c-api's host callback gets no handle to the
caller's memory**, which is the one thing essentially every loader host import needs. The chosen ABI
could not do the consumer's core job.

**Cost:** `wasm_c_api.zig` — 319 declared functions, the largest file in that project and the only one
that hands raw ownership across a C boundary. Three whole audit findings (#20, #21, #22 — 180 missing
symbols, then a double free, use-after-free, uninitialised refcount and leak) lived in it. wasmrt
replaced the entire surface with ~74 functions and value handles.

**Apply:** when a design rests on "consumer X already does Y", open X's source and grep for Y before
the decision, not after the implementation. A benefit claimed on the consumer's behalf is exactly as
much a hypothesis as a cost figure (§1.1) or a scope note (§1.1a) — and it is the one that quietly
selects an architecture. ⚠️ The tell here was available from day one: the loader header is in a sibling
repo, and reading it takes minutes.

### 2.3 A decision resting on a false premise must be re-derived, not defended

`security-model.md` recorded that Rust's `std` could do an atomic no-follow open. **It cannot** — there
is no dir-relative open on any platform. The decision built on it had to be re-taken, not patched.

### 2.3a "The oracle accepts it" is only evidence if you know WHICH PATH you invoked

T9a#9 sat on the punch list for two releases as *"the oracle assembles **and runs** them, so this is
our type-checker being wrong, not the input."* Every word of that observation was true. The conclusion
was not.

`wazmrt <module> <export>` **does not validate** — it decodes and runs. `wazmrt <module>` (no export)
summarizes *and validates*. Put the same file through the second path and the oracle reports
`validation: FAILED — TypeMismatch`, agreeing with wasmrt exactly. The module really was ill-typed —
`if (result f64)` with both arms pushing `i32`, which `if.wast`'s `type-then-value-num-vs-num` makes
an `assert_invalid`.

So "it runs" was never evidence of validity; it was evidence that **one particular entry point skips
the check you were asking about**. This is §1.5a again with the oracle in place of our own gate.

**Apply:** when citing a reference implementation, name the subcommand. Before concluding "the oracle
disagrees with us", find its *validating* path and re-run there — and check the reverse direction too:
a runtime that executes an invalid module is over-permissive, not authoritative. A three-line
reduction put through both tools settles it in a minute; this one had gone undiagnosed since T7.

### 2.4 Assume the numeric order of an enum is not the wire order

Section ids: `DataCount` is **12** but must precede `Code` (**10**); `Tag` is **13** but belongs between
`Memory` and `Global`. A `>` on raw ids accepts both in the wrong place and rejects them in the right
one. That is why `SectionId::order()` is a table.

---

## 3. Where the defects actually are

### 3.1 Silent-wrong-output is the worst class, and it is where every serious defect has been

**A module that is rejected announces itself; one that runs and answers wrongly does not.** The list:
dropped table initializers; element-segment form 4 rewriting a segment's type; `br_table`'s missing
label vector; `(data "a""b")` concatenating; `Op::MemorySize` reading another instance's memory; a
repeated section silently replacing the first; a cross-store `InstanceId` sharing the wrong memory; the
assembler emitting open types as final and flattening rec groups; **the start function never running**; **`from_utf8_lossy` in the lexer silently renaming malformed identifiers** so two distinct ones collided.

**Apply:** when hunting, weight this class above everything else. Ask "what would a wrong answer look
like?" before "what would an error look like?"

### 3.1a A feature can be fully decoded, validated and PRINTED, and still never execute

`Module::start` was parsed by the assembler, checked by the validator, reported by `wasmrt <file>` —
and no code path ever called it. §4.5.5 requires it to run as the last step of instantiation. Every
stage that *inspects* a feature passed, so every stage that could have noticed was satisfied.

The cost was 10 assertions sitting failed for five releases in files literally named `start.wast` and
`start0.wast`. The triage habit had been to read failures for a *diagnosis* — what does the message
say — and these said nothing useful, so they stayed on the pile.

**Apply:** before diagnosing a file's failures, spend one minute on the cruder question: **does the
feature this file is named for work end to end?** Write the five-line module, run it, look at the
answer. Presence in the decoder, the validator and the printer is not evidence that anything runs it —
grep for the field's *reader*, not its writer.

### 3.2 The emitter reconstructs a form from a SUBSET of the parser's facts

Three times: element-segment reftype (form 4 has no reftype field → `funcref` hardcoded), **finality**
(the wrapper was keyed on the supertype's presence alone, so `(sub (struct …))` emitted a *bare*
comptype, which means `final`), and **rec-group extent** (`0x4e` emitted nowhere, so every `(rec …)`
became singletons). Each produced a *valid* module — just not the module the text described.

Generalised: **information was present and discarded because nothing needed it yet.** The same shape
appeared on the decode side — an import's typeidx was resolved to a `FuncType` and the index thrown
away, which later made cross-module identity undecidable.

**Apply:** for every field the parser records, ask whether the emitter reads it. An unread field is a
dropped fact waiting to become semantic. This is T10a.

### 3.3 Comparing STRUCTURE can never answer a question about IDENTITY

Two functions can both be the empty `(func)` and still be **different types**, because rec-group
membership is part of identity. Any params/results comparison links them. Only the type *index*,
resolved to a canonical id, carries identity.

**Apply:** when a check compares shapes, ask whether the real rule is about *identity*. If so the shape
comparison is not approximate — it is answering a different question.

### 3.4 An asymmetry between layers is a bug, not a style difference

The C ABI tagged its value handles with the issuing store from T8; core's `InstanceId` did not. **Core
held the weaker of the two guarantees, and that asymmetry *was* the defect.** When one layer defends
against something, ask why the other does not.

**Second instance, 2026-08-08 — and it was in two runtimes at once.** `wasmrt wasi` validated before
executing; `wasmrt run` did not, so it would execute an ill-typed module and print a plausible answer.
The same split existed in `wazmrt`: its summarize and `.wast` paths validated, while **both** of its
execute paths and its C ABI's `wasm_module_new` did not. In both projects the paths that *actually run
code* were the ones not checking it — the exact inversion of what matters.

**Apply:** enumerate the entry points, in a table, and check the property at every one. The bug is not
usually "nobody does X"; it is "three of the four do X". Prefer one guard on a predicate the paths
already share (`will_execute` in wazmrt's `run()`, which existed to gate pin verification) over N
copies, so a path added later inherits it instead of having to remember.

### 3.5 The two-instance rule

Store-index vs module-index conflation is **invisible with one instance per store**, because the two
indices are equal. Any regression test touching pooled resources (memories, tables, globals, data/elem
segments) MUST instantiate a second module first, or it proves nothing.

### 3.6 Rejection STAGE is part of correctness

Malformed input must be refused by the decoder, ill-typed input by the validator, unlinkable input at
link time. wasmrt was refusing malformed binaries *at validation* — the module was rejected, so nothing
looked broken, but the stage was wrong and the spec suite says so with `assert_malformed`. Fixing the
stage was worth ~100 assertions and closed real over-acceptance alongside it.

### 3.7 The stage that REPORTS a failure is usually not the stage that caused it

T9a#8 was one defect in the **assembler** — four instructions emitted without their immediate. It was
reported, variously, as:

- a **decoder** error (`missing END`) — because the absent operand shifted every following byte;
- `UnknownInstr` naming the **next token**, `$t`, rather than `call_ref`;
- the same `UnknownInstr` in a different file, which got logged as an **unrelated** corpus gap.

So one cause produced three symptoms, filed as two separate punch-list items, and every symptom pointed
away from the assembler. It had been undiagnosed since T6.

The general shape: a component that emits a *stream* fails by shifting everything downstream, so the
first thing to notice is whoever reads the stream next. Off-by-one and missing-field defects in an
encoder are systematically misattributed to its decoder.

**Apply:** when an error names a position or a token, ask what wrote the bytes just before it, not just
what read them. And when two entries on a punch list have the same *message* in different files, treat
them as one hypothesis rather than two items — the identical-numbers tell of §1.2 applies to identical
messages too.

### 3.8 A port/oracle divergence can be a MISSING CAPABILITY, and no gate here compares capabilities

`wasmrt run prog.wat` failed with *"not a WebAssembly binary (bad magic)"* for the whole port. The
oracle accepted `.wat` on its run path from its own first release. The assembler was in the same
executable, one function away, reachable only as a separate `wasmrt wat` step.

Nothing caught it, and nothing **could** have: every gate this project runs compares *answers* on inputs
both runtimes accept — the spec suite, the `.wat` corpus, the WASI corpus, and (while it existed) the
oracle drift monitor. A feature
one runtime simply does not offer produces no differing answer to diff. It is invisible in exactly the
way §3.1's silent-wrong-output is invisible, for the opposite reason: not a wrong answer, but **no
answer at all, on an input nobody in the harness ever hands it**.

Note where it sits relative to §3.1a. There the feature was decoded, validated and printed but never
executed. Here the feature *worked perfectly* — it was simply never wired to the path a user would
reach it from. Both are "the code is present and correct and does nothing", found by asking a question
about the product rather than by running a test.

⚠️⚠️ **It was found by the OWNER asking whether the runtime ran `.wat` files — the second finding in two
days that came from an owner question rather than a test** (§2.3's validation gap was the first).
That is not luck twice; it is a category the harness structurally cannot reach, because a test is
written from the same mental model as the code, and a missing capability is a hole in that model.

**Apply:** periodically enumerate what a *reference* runtime **accepts** — its subcommands, file types and
flags — as a table, and check wasmrt entry by entry. Same shape as the §3.4 remedy (enumerate the entry
points, don't reason about them), applied to the outside of the tool rather than the inside. Cheap, and
it is the only thing that finds this class.

⚠️ **Since 2026-08-11 the reference for this is `wasmtime`, not `wazmrt`** — the oracle is retired and
wazmrt is a competitor (`design-decisions.md`). That does not weaken the check: wasmtime's CLI is the
larger surface, and §2.3's rule applies to it too — **run the binary, do not enumerate from memory.**

**The same question asked of the size axis** finds the open instance of this today: every recorded size
figure (CLI, cdylib, freestanding wasm32) is for an artifact `rsxtk` — the *default* consumer — does not
link. Measuring what is in front of you rather than what the consumer uses is this section's mistake
wearing different clothes.

### 3.9 A shared registry wired into ONE consumer is a bug in the others

wasmrt built a store-wide type registry at T9h and wired it into import matching and `call_indirect`.
Both were correct. **The other two consumers of the same question kept comparing raw indices across
modules** — `ref_matches`' `Any` arm (GC objects) and its `Func` arm (funcrefs) — and both were wrong.

The `Func` arm is the instructive one. It had *already been fixed once* for this exact class: its doc
comment says a funcref's type lives in its owning instance's module. It fetched the index from the right
module and then compared it in the wrong one, and logged that as **"approximate"**. A defect described
in a comment is still a defect; "approximate" was the word doing the concealing.

**Apply:** when a facility exists to answer a question exactly, **enumerate every site that asks that
question** and convert them in one pass. A registry, a canonicalizer, a validator helper — each is a
claim that the question now has one right answer, and every unconverted caller is a place still giving a
different one. ⚠️ Note this is §3.4's rule (enumerate the entry points) pointed at a *capability* rather
than an entry point, and it found **two of four sites wrong**.

⚠️ **The tell is a hedging word in a comment** — "approximate", "best-effort", "close enough",
"good enough for now". Grep for them. Each marks a place where someone knew the answer was wrong and
decided the cost of being right was higher than it turned out to be.

---

## 3A. Borrowed lessons — from wazmrt's `best-practices.md` (owner-authorized read, 2026-08-14)

🔒 **Scope of this exception.** The oracle is retired for *correctness answers and design* (§
`design-decisions.md`): wazmrt is a competitor and its implementation choices are not evidence about
wasmrt's. **Reading its accumulated METHOD is a different act** and the owner authorized it explicitly.
These are process lessons, adopted because they are true independent of either codebase — not because
wazmrt holds them. Nothing here reopens the oracle.

**Several arrived as independent confirmation**, which is worth more than novelty: *"our assembler is not
an oracle for our decoder"* (wazmrt: four occurrences in two days; wasmrt: four, §3.7 + T10a) and
*"a new test that has never failed has not been shown to test anything"* (wasmrt's §4.2a, which adds the
harder half — **confirm the mutation applied**, since a no-op mutation and a worthless check produce the
same observation).

What is genuinely **new to wasmrt**, and all of it bears on T11:

- ⚠️ **"A goal with no gate is a preference."** *"Smallest binary" was a stated goal for a month with
  nothing measuring it, and the artifacts doubled.* wasmrt has size **measurements** and **no size gate
  at all** — and size is now the axis the inclusion contest is decided on (`vision.md`).
- ⚠️⚠️ **"A gate only gates the commits that RUN it."** wazmrt *built* a size gate that works correctly
  and still accrued +22 KB exe / +24 KB lib / +19 KB dll of undetected drift, because commits did not
  invoke it. **A gate needs a trigger, not just an existence.** Applied here the same day: the two
  cross-module reproducers were green and nothing ran them — now `regression_wast.rs` does, under
  `cargo test`.
- ⚠️ **"A size gate reads whatever is on disk, including yesterday's artifact."** *A number that matches
  the ceiling to the byte is evidence of a stale file, not of a change that cost nothing.* Directly
  threatens T11's measurements: **build every artifact you are about to report**, in the same run.
- **"Attribute an overshoot before paying for it."** Measure the parent commit in a worktree rather than
  assuming the growth is yours — the answer came back both ways on different releases, and both answers
  were only worth having because the measurement was made. ⚠️ **This is the missing method for the
  unattributed ~5% steady-state regression**, which two hypotheses failed to explain: bisect it against
  the parent commits rather than reasoning about likely causes.
- ⚠️⚠️ **"Two consumers agreeing is not corroboration when they share the mistake — and there can be
  THREE."** A `try_table` catch label was resolved one frame too deep by the assembler, the validator
  **and** the interpreter, identically, so every round trip was self-consistent and the whole corpus was
  green. **No test could have found it; only the spec rule did.** *Count the implementations of a rule
  before trusting that they check each other* — and note this is the exact hazard §3.9 above just
  realized, from the other direction.
- **"An encoding chosen to make EXECUTION agree can erase the distinction VALIDATION runs on."** Ask what
  an encoding throws away, not only what it preserves. → a named item for **T10a**'s shorthand review.
- **"A tag added to one of two readers works in half the positions."** When a type gains an encoding,
  grep for **every** reader of that encoding.
- **"Never green-wash our own gaps."** An unimplemented form is not evidence a module is invalid.
  *(wasmrt already does this — the runner separates "nothing defines this import" from "wasmrt cannot
  back this kind" — but it is worth keeping stated, because it is the failure mode that makes a
  conformance number a lie.)*

---

## 4. Checks and gates

### 4.1 A gate that cannot fail is decoration

`if ( cd "$WAZ" && zig build test ) 2>&1 | tail -20; then` tests the exit status of `tail`, which always
succeeds. The oracle monitor reported `tests: GREEN` regardless of what the build did — and it
underwrote every parity claim in the port. **Verify that a gate fails when it should**, by breaking the
thing it guards.

### 4.2 Mutation-verify any check that matters

Delete the check, confirm the test fails, restore. Done for: the C-ABI store-tag check, `Op::MemorySize`
routing, the sandbox `..` guard, and the cross-store `InstanceId` check. A test that passes both with
and without the code it covers is not a test.

### 4.2a Confirm the MUTATION applied before believing the mutation test

Mutation-verifying the T9a#8 emitter sweep, a `perl -pi -e` substitution silently failed to match, the
test passed, and the obvious reading was "the sweep is decoration" — the §4.1 finding, apparently
confirmed. It was not: re-doing the mutation with `sed` and **grepping to confirm the line was gone**
made the test fail and name all four affected ops exactly.

A no-op mutation and a worthless check produce **the same observation**: an edit, and a green test.

**Apply:** after mutating, `grep` for the thing you removed and assert it is gone. Only then read the
test result. And note which direction this error runs — it manufactures false *"my check is useless"*
verdicts, which cost a good check rather than shipping a bad one, but the wasted work is real and the
temptation is to delete the check.

### 4.3 A guard placed one call-level away from the iteration it guards is NO check

The type-use clause-order rule was first written in `parse_sig` and moved **one** assertion out of
forty, because `parse_block_type` calls `parse_sig` **one clause at a time** — so the order state was
built and destroyed per clause and could never observe a sequence. The code read as correct. Only the
measurement said otherwise.

### 4.4 A check that is harmless "because that case cannot occur" becomes a bug the moment it can

The T8 third-order lesson. Fixing dropped table initializers made non-nullable element types expressible
for the first time, which exposed that the active-element check compared *families* with nullability
normalised away.

### 4.5 A blanket skip is not a neutral placeholder — it insures the code it covers

`assert_unlinkable` was an unconditional skip, justified by a reason obsolete since T7b. While it sat
there, **nothing had ever checked that a badly-typed import is refused** — and imports turned out to
have no link-time type checking at all. **Apply:** any assertion category declined wholesale must carry
a note saying what would be measured if it stopped being declined.

### 4.6 Which direction to err in is a property of the CONSEQUENCE, not a house style

Two undecidable comparisons, opposite calls, both measured:
- **Cross-store function-import matching** → err toward **refusing**. Accepting binds a call to a
  mismatched signature: a silent wrong call.
- **Declared subtyping** → err toward **accepting**. Refusing turned away 6 valid modules; accepting
  only preserved behaviour that already existed.

Decide from what happens when you are wrong, then measure both ways if you can.

### 4.6a Tightening what the grammar PERMITS is the same error as accepting what it forbids

Four probes of the lexer's character handling all looked like defects; **three were**. The fourth — a
control byte inside a `;;` comment — is legal, because `linechar ::= c:char (if c ≠ U+0A)` admits
anything but a newline. Fixing "all four" would have rejected valid `.wat`, and the only thing that
separated them was reading the production rather than pattern-matching on "control character = bad".

The corresponding gate: when tightening a *parser*, the measurement that matters is not the suite but
the **corpus of things that must still parse** — the 534-file `.wat` corpus, checked on both sides, and
**round-tripped through decode**, not merely assembled (§1.5a). A tightening that improves the suite and
drops a corpus file has not made the parser more correct.

**Apply:** for each rule you are about to add, quote the production it comes from. A rule you cannot
cite is a guess, and half the guesses here were wrong. Then pin the *negative* case with a test — this
pass has one asserting a control byte in a comment stays accepted, so a later "cleanup" cannot quietly
extend the rule to where it does not belong.

### 4.7 Prefer a hard `Err(Unsupported*)` over silent-wrong

Standing since T4. An unhandled input that emits a stub or a placeholder is the worst category in the
"look for code issues" trigger. Deferred features reject **loudly**.

### 4.8 A library must reject a module, never abort the embedder

`v128.const i64x2` fell into an `unreachable!()` and aborted the whole conformance run. Under
`panic = "abort"` in the release profile, **any reachable panic is a host-process kill**. Decode and
validate must be *total* on hostile input: prefer a sentinel the next stage will reject over a panic, and
prefer an error over both.

---

## 5. Tests

### 5.1 Assert where a check STOPS working, not only where it starts

`decode_body` requires a terminating `end` but does **not** model nesting, so `block … end` with the
function's own `end` missing still passes it. A test pins that boundary, so the limitation is visible in
the tests instead of being discovered by someone trusting the error's name.

### 5.2 Every existing test asserted that the module RUNS — so defects that run were invisible

All three emitter defects produced modules that ran. Assertions about behaviour cannot catch a module
that is subtly *not the module you wrote*. That is why T10a specifies a **round-trip property test**
(assemble → decode → compare against what the parser recorded) rather than more behavioural tests.

### 5.3 Our own fixtures were malformed modules, and nothing could say so

Four hand-built vectors put the export section after code; one declared a function section with no code
section. They had always been malformed. **A permissive checker lets bad fixtures accumulate, and they
then encode the wrong rule** — so tightening a checker will break your own tests first, and that is a
finding, not an obstacle.

### 5.4 Key a test on the flag that carries the meaning, not on an incidental symptom

The const-expr sweep must key on a segment's **mode**: a *passive* segment has no offset expression,
while an *active* one with none is malformed. Filtering on `is_empty()` conflates them and excuses the
second. The test is written so the sloppy version fails.

### 5.5 Skips are never folded into passes

A construct the build cannot put to the test is not a pass. Skips are counted separately so the
conformance number cannot be inflated by what we do not handle. Equally: distinguish "nothing defines
this" (a real verdict) from "we cannot back this kind" (a gap) — collapsing them is what made
`assert_unlinkable` unimplementable.

---

## 6. Robustness on untrusted input

### 6.1 Anything keyed by an attacker-controlled count must be O(n log n), not O(n²)

Rec-group interning uses a `BTreeMap`, not a scan over previously-seen keys: group counts come from the
module, so a scan is a denial of service **on the decoder**. `alloc`'s `BTreeMap` needs no dependency and
no hasher.

### 6.2 Decode must be total

Canonicalisation encodes an out-of-range forward reference as a distinct **sentinel** rather than
erroring, so the decoder never fails or panics there and the bad index stays the validator's to report.
Totality on hostile input is worth an odd-looking branch.

### 6.3 Say what you did not do

`NOSYS`, not a silent success, for `fd_allocate`, `fd_filestat_set_times`, `path_filestat_set_times` and
the socket calls. A guest that needs them must learn we did not do them.

---

## 7. Reference projects

### 7.1 "wasmtime's SHAPE, our code"

Adopt the architecture; write the implementation. No code, no symbols, no headers, no data structures
transcribed. Three applications: the `wasmrt.h` surface (T8), the shared store (T7b), the engine-level
type registry (T9h). **The Component Ledger stays empty** — the "evaluate a reference project" trigger
requires an entry for *copying or porting* code, and reading an architecture is free.

**The compliance test:** each borrow visibly **diverges** where wasmrt's constraints differ — checked
value handles instead of refcounted objects, the `code`/`pools` split for disjoint borrows, zero
dependencies, `forbid(unsafe_code)`. A design that cannot be re-derived under those constraints was
being copied rather than understood.

---

## 8. Working method

### 8.1 Do not `git checkout <file>` to remove a temporary edit

It discards *all* uncommitted work in that file. Doing this to drop a debug probe destroyed a completed
canonicalisation implementation, which then had to be rewritten. Delete the added block, or stash.

### 8.1a Do not run `perl -pi -e` with a replacement containing `$` or `@`

In a Perl replacement, `$'` is the **postmatch variable** — everything after the match. A one-line
substitution meant to add an enum variant documented as ``` `id ::= '$' idchar+` ``` instead spliced
the entire remainder of the file into the middle of the enum, ~100 junk lines. Rust is full of `$`
(macros, and any prose about `.wat` identifiers), so this is not a rare shape.

**Apply:** use the `Edit` tool for any replacement text containing `$`, `@`, or a backslash. **`@` bites
as often as `$`** and did so three more times in one session: `@as(u64, 0)` became `(u64, 0)` in a Zig
test, and two commit hashes written `wazmrt@4a6d745` lost the `@4`/`@1` and shipped as `wazmrta6d745`.
Each was caught by a build error or a re-read — but a mangled hash in a doc is exactly the kind of thing
that is not caught. When a
bulk edit *is* right, prefer a pattern with none of them. And if this does go wrong, **repair — do not
`git checkout`** (§8.1): the corruption was a contiguous, identifiable span, and splicing it out
preserved the rest of the pass.

### 8.2 Report faithfully, including the direction of an error

When a measurement moves the wrong way, say so and say by how much. When a number is inside the noise,
say "unchanged", not "improved". When an earlier note is superseded by a better measurement — the
"residual: 1 assertion" that turned out to be ~40 — record the correction where the old number lived.
