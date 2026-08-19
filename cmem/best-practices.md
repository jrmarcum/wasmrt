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

### 3.10 A conformance suite checks RESULTS; some features are about RESOURCES

`return_call_ref` shipped for releases as "call the callee, then jump to the end of the body". Every
answer it produced was correct. It recursed natively on every hop, so unbounded mutual recursion —
the entire reason the tail-call proposal exists — still exhausted the stack.

Its conformance file read **40 passed / 7 failed**. Nobody looks at 40/7 and concludes *the feature is
absent*; it reads as "nearly done, a few edge cases". Measured deliberately afterwards, a mutation
restoring the fake still scores **38/6, 72/4, 40/6** across the three tail-call files.

**The reason is structural, not a gap in the suite.** `assert_return` compares a value. A tail call's
defining property is that it does not consume a resource, and no assertion about a *result* can
observe that. The same blindness covers anything whose contract is consumption rather than output:
memory that must not grow, a cache that must not be rebuilt, an allocation that must not happen.

**Apply:** when a feature's *point* is a resource property, write the property test yourself and make
it large enough that only the real implementation survives — 1,000,000 frames, not 100. Then
mutation-verify it against the conformance number and record both: *"the suite still reads 90% under
the mutation"* is the sentence that stops the next person trusting the percentage. ⚠️ This is §3.1's
silent-wrong-output inverted — there the answer was wrong and nothing said so; here the answer is
right and the feature is missing anyway.


### 3.11 WIDENING A FIELD DOES NOT SURFACE ITS CALL SITES — an identity conversion still compiles

**table64 (T13) widened `TableDef.min` from `u32` to `u64`.** Two of the three emit sites were found by
the compiler; **the third was not**, because it read `emit_limits(…, u64::from(t.min), …, false)`. While
the field was `u32` that was a **widening**; the moment it became `u64` it turned into `u64::from(u64)`
— an **identity conversion that type-checks perfectly** — so the site kept compiling with its hardcoded
`is64: false` beside it. The result assembled `(table i64 …)` as a **32-bit table**: a wrong module, not
a rejected one.

⚠️ **The habit: after widening a type, grep for the conversions that used to DO the widening**
(`u64::from`, `try_into`, `as`). They are exactly the sites the type checker can no longer see, and they
are disproportionately the sites doing something *else* wrong too — a conversion wrapper is where a
hardcoded neighbour hides.

🎓 **It cost a second mistake worth naming: a replace-first on a pattern that occurred twice.** The
patch script asserted “patched 2 sites” and was satisfied — but it **counted patches, not occurrences**,
and two of the three lines were byte-identical, so one replacement consumed both checks. *Count what you
expect to FIND before replacing, and assert none remain after* — §4.2a wearing different clothes:
confirming a change applied is not confirming it applied **everywhere**.


### 3.12 A WRONG TYPE WITH A RIGHT ARITY PASSES EVERY SHAPE CHECK

`br_on_non_null`’s validator popped its label’s types **wholesale**, so it demanded `(ref ht)` from a
stack holding `(ref null ht)` — and **rejected the canonical idiom the instruction exists for**.

⚠️ **The stack EFFECT was already correct** (pop all, push all, pop the ref), so every arity and
balance check agreed with it and the arm read as plausible. Only the *type* it asked for was wrong.
**Arity is the property tests and eyeballs check; the type is the one the spec cares about.**

🎓 **And it survived because it erred in the REFUSING direction** — a rejected valid module is a
failing assertion, never a wrong answer, so nothing miscompiled and no user hit a bad result. That is
the same asymmetry §4.6 records from the other side: *which direction to err in is a property of the
consequence*. A refusing bug is cheap to live with and therefore **cheap to not notice**, so the place
to look for one is a conformance file that has sat at a few failures for a long time — not a bug
report.

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

### 3A.2 Second borrow round (2026-08-19) — the method wazmrt accumulated since 2026-08-14

🔒 **Same scope note as §3A.** These are process rules, read from a competitor's `best-practices.md`
under the owner's standing authorization to borrow *method*. Nothing here is evidence about wasmrt's
design, and nothing here reopens the oracle. Each is filed against the wasmrt task it bears on, because
a lesson with no task attached is the kind nobody re-reads.

#### For T11 — measurement, which is now the axis the contest is decided on

⚠️⚠️ **A benchmark whose FLOOR is larger than its signal measures the floor.** wazmrt's end-to-end CLI
harness put it 2.4× ahead of wasmtime; with process spawn excluded the two engines differ by **20–55×**.
A ~30 ms spawn floor did not add noise to a sub-millisecond quantity — it **hid the entire effect**, and
flattened a real module-size dependence into "nothing moves". **Measure the floor first and report it
beside every number**: `--version`, which does no wasm work at all, cost 30 ms vs 76 ms, so ~46 ms of a
~50 ms "engine win" existed before either engine started. → T9f already named T11's first job as *"a
benchmark that can resolve 5%"*; this says the harness must be **in-process**, and must state its own
floor.

**A RATIO is load-dependent while the DIFFERENCE is not.** The same benchmark gave 5.3× on a quiet box
and 2.4× on a loaded one while the absolute gap stayed ~29–48 ms — a fixed per-process cost shared by
both entrants inflates both sides and compresses the ratio. Quote the difference, or quote the ratio
with the load conditions attached.

⚠️ **A gate's number must be reproducible in the CONFIGURATION it was recorded in — and "configuration"
includes WHERE.** One unchanged wazmrt commit measured four different static-archive sizes, varying only
with the source-tree path and the cache path, because an unpadded archive embeds object/source paths;
the `.exe` and `.dll` absorbed the same difference inside PE alignment and looked perfectly stable. A
clean HEAD read **+152 bytes over its ceiling** and nearly bought a ceiling raise for bytes no change had
caused. ⚠️⚠️ **This is a direct hazard for T11's promoted footnote** — the artifact `rsxtk` links is the
**rlib**, which is exactly that kind of archive (metadata plus embedded paths). Measure it from a fixed
path, and **re-measure the parent commit in the same configuration** before charging a sub-KB delta to a
change. *(Third hole in this family, after "a size gate reads yesterday's artifact" in §3A.)*

**A recorded prediction that the measurement REFUTES is worth more than one quietly deleted.** Keep the
struck text in place beside the number that killed it.

**A FIX task changes the code; a COMPARE task measures it against something else — do not keep them in
one queue.** A compare task can never be "done": rivals ship new versions and corpora grow, so its
residuals regenerate by construction, and it is scheduled when a NUMBER is wanted rather than when a gap
is found. ⚠️ It is also the only kind of task that pulls **external dependencies** into a project whose
stated invariant is zero of them. **T11 holds both kinds**: the optimization review is a fix task; the
same-machine comparison against wasm3 / WAMR / wazmrt is a compare task. Sort work by what it CHANGES
before sorting it by priority.

#### For T10 / T10a — the emitter audit, and what its planned gate cannot see

🚨🚨 **A ROUND TRIP PROVES AGREEMENT WITH YOURSELF.** wazmrt gave its non-null abstract reference types
synthetic valtype bytes "in an otherwise-unused range", and its emitter wrote them out raw — so
`(ref i31)` assembled to the single byte `0x62`, where the spec form is `0x64 heaptype`. Every module it
produced was invalid to every other runtime **for nine months**, and its entire conformance corpus was
blind by construction: the decoder accepted both spellings, so its own output round-tripped and
everyone else's input read fine. **The corpus run before and after the fix is byte-identical.**
⚠️⚠️ **T10a's gate is a round-trip property test, and this is precisely the class a round trip cannot
see.** Add an external arm — hand the assembled bytes to wasmtime, or assert the bytes directly.
**When a bug is only visible to a third party, the test has to BE a third party.** The design half is
worth having too: **reserve internal tags OUTSIDE the format's space, or convert at the boundary —
"currently unused" is a statement about today's spec** (by 2026 `0x62` had become the
custom-descriptors `Exact` prefix, so wasmtime rejected the output with *"unexpected exact type"*).
*wasmrt holds the DECODE half of this invariant since T2 — internal tags `0xD7`–`0xFA`, raw ones
rejected. What T10a has to check is the EMIT half.*

**A trailing-element grammar must be CHECKED FOR LENGTH, not indexed from the end.** Three separate
wazmrt parsers took `l[l.len - 1]` and silently ignored everything between, and all three failed in the
same direction — **a module that VALIDATES with weaker typing than its source asked for**. Grep `wat.rs`
for the Rust spellings (`.last()`, `[len - 1]`) at T10; three instances in one session is a pattern.

**A BLANKET REFUSAL CAN HIDE A MISSING RULE — enumerate what it is catching before deleting it.**
wazmrt's wholesale `delegate` refusal was also the only thing rejecting a bare `(func (delegate 0))`,
which its assembler happily emits and the spec calls malformed; removing the refusal without adding the
enclosing-frame rule would have converted a spec malformation into an accept-invalid. ⚠️ **wasmrt
rejects `delegate` the same way today** (a recorded divergence, `known-issues.md`) — so this is the
checklist for the day that decision changes. Related: **a test that asserts a REFUSAL is a test of a
DECISION and expires with it — rewrite it in place rather than deleting it**, because the reasoning in
a refusal test is usually the best available summary of what the replacement rules must cover.

**THREE SPELLINGS OF ONE LIST WILL DRIFT — derive it, or pin NAMES AND VALUES, not lengths.** wazmrt
shipped a header advertising `TAIL_CALL = 14` while its C-ABI enum's validity check hardcoded `<= 13`:
the setter returned false and did nothing, while its all-features call disabled the feature anyway.
**The header advertised a switch that was not there.** 🆕 **Checked against wasmrt the same day, and the
gap is real** — logged in `known-issues.md`: the proposal list is spelled three times and **nothing
compares them**.

**SCORE THE SAME ERROR THE SAME WAY ON EVERY PATH.** A classification rule that one call site does not
consult is a rule with an exception nobody wrote down — 14 of a reported 104 "failures" were that one
inconsistency.

#### For triage — how to read what is left (172 failures, 1,024 skips)

✅ **DONE 2026-08-19, and the borrowed rule paid twice.** The skip census is now **permanent**
(`Summary::skips`), not a throwaway probe — see §5.6 and `testing.md`. It showed 91% of the skips were
cascades and reduced 1,024 of them to **seven items**.

⚠️ **When a skip has no recorded reason, INSTRUMENT it — do not reason about it.** A throwaway probe
(source line plus error name at every skip site, run over the corpus, then reverted) produced wazmrt's
whole breakdown in one pass — **and overturned the scoping written from reading the files: two of the
four cases classified as SCORING bugs were decoder and assembler gaps that rejected VALID modules.**
A wrong diagnosis that agrees with the evidence you *chose to look at* is the expensive kind.

**A SKIP TOTAL IS DOMINATED BY CASCADES — ask how many MODULES are behind it before ranking the work.**
144 skips looked like five separate problems; 109 were one untargeted proposal and only ~34 were direct
— ten modules that fail to assemble, and 99 `assert_return`s cascading behind them. This is §1.3 read in
the other direction: the same cascade that makes a small fix worth hundreds of passes makes a skip total
a poor size estimate. Measure the causes, not the column.

**A "0 FAILED / 0 SKIPPED" CORPUS IS NOT "EVERYTHING RAN" — read the file-error count.** A file that
dies in the lexer contributes to none of the three usual columns, so the score can look perfect with a
whole file unexecuted. **Quote all four numbers.** *(wasmrt has paid this once already in a different
shape — the `.wat` gate that ran assemble only, §1.5a.)*

**A REFUSAL IS NOT A HOLE — rank security work by what a gap PERMITS, not by the size of the failing
number.** A module the runtime *rejects* cannot do harm, so implementing the proposal behind those
assertions **adds** attack surface rather than removing it. A conformance total counts disagreements,
not exposure; they are different axes and a big number on one says nothing about the other. → T12
scoping.

**A well-argued entry in a baseline is still an entry** — and the better the explanation, the longer it
sits unexamined, because nothing prompts you to revisit the ones you already justified.

#### For T12 — the security review

⚠️ **AN ENFORCEMENT ARM THAT RUNS *BESIDE* THE THING IT CONSTRAINS ENFORCES ONLY WHAT ITS CALLER
REMEMBERED TO ASK FOR.** wazmrt's feature gate sat next to `validate`, so its C ABI gated with the
embedder's feature set and then validated with **all** features — the set decided which proposals were
ADMISSIBLE while the all-features rules decided what they MEANT, and one instruction was typed by a
relaxed proposal rule with that proposal switched off. No gating test could see it, because the
instruction exists either way and was never refused. **When two calls must agree, make them one call —
"kept in step" is a property of the last person to edit.** 🆕 **A new row for T12z's invariant table.**
*(wasmrt's C ABI does call `validate_with_features(&md, &e.features)` — `crates/wasmrt-capi/src/lib.rs`
:693 and :725 — so it appears to hold at that site; T12z's whole method is to enumerate EVERY entry
point rather than spot-check one.)*

**A PROPOSAL THAT SHIPS WITHOUT A GATE IS NOT "ENABLED BY DEFAULT" — IT IS UNREFUSABLE**, and a
per-proposal checklist cannot ask this question about itself; it lives one level up. Put *"does it have
a gate, and is that gate tested?"* in every proposal's deliverables, and when scoping a track that
claims a gate exists, **grep for the gate**. *(T9f did this right — the flag exists only now that the
feature does. The lesson is to keep that structural rather than remembered.)*

**A front end that grows its FIRST DECISION needs its first test target in the same commit** — and if it
parses untrusted input, it belongs in the **memory-safety gate**, not only in `test`. wazmrt's
`--features` parser sized a buffer from a TYPE (`count * 2`) and never bounded the index: every item had
to be a valid proposal name to be stored, which is exactly what made it read as safe, but nothing stops
a caller *repeating* one. **"Every element is validated" is not "the count is bounded."** ⚠️ The rewrite
that removed the array also removed 512 bytes — the fix for a memory-safety bug came out smaller than
the bug.

**AN AMBIGUOUS INPUT SHAPE IS REFUSED, NOT RESOLVED** — choosing a reading would be a precedence rule
nobody reviewed. And the mirror: **an unrecognised item is an ERROR, never a skip**, or the user believes
they restricted something they did not.

#### For tests — and for mutation verification specifically

⚠️ **An inversion that catches NOTHING has three causes, and they need three different responses:**

1. **The arm is genuinely redundant** → *delete it.* **A redundant guard carrying a false justification
   is worse than no guard: it teaches the next reader the wrong rule.**
2. **The arm has a MIRROR that catches the case instead** → *keep both, and say so.* Delete either on
   the strength of a green suite and the module is accepted the moment its mirror is touched.
3. **The arm defends a path the tests cannot reach** → *keep it and write down why* — e.g. the
   unvalidated run path, where a hand-built module can pair any value with any descriptor.

⚠️ **The failure mode is treating all three as case 1. "No test caught it" is a question, not a verdict**
— the same shape as *finding a real defect at a layer is not evidence it causes your symptom* below.

🔒 **For a SOUNDNESS rule, write the WRONG ANSWER down.** A score cannot tell you a rule is enforced,
only that the files you looked at got better: wazmrt landed correct exactness arms in both subtyping
paths, measured 0 regressions and 7 improvements, and `ref.test (ref (exact $super))` still answered
**1** for a subtype, because four cast sub-opcodes read their target through a path that dropped the
prefix. **Three soundness defects on that branch passed the entire corpus and were found only by
constructing the case.** The test to write is not *"does the feature work"* but *"does the thing that
must NOT match, not match"* — plus its neighbours, so a blanket refusal cannot pass for the right
answer. **A green suite is evidence about the tests, not about the code.**

**Type the block at the DISTINGUISHING type, and balance the stack EXACTLY**, when the property under
test is which branch fires or which operand is consumed. A block typed at the top type accepts either
branch shape, so a direction bug passes every execution test; a stack-polymorphic `return` swallows a
leftover operand, so a missing pop type-checks cleanly.

**A test that fails because its EXAMPLE was implemented is stale, not broken.** Three outcomes are
genuinely different — the code is wrong, the property is wrong, or **the example drifted** — and the
third is the one that looks like the first. Keep the property, re-pick the example, and **pin the
inverse case beside it** so the pair can only pass if the distinction is actually being drawn.

#### For scoping, and for this memory itself

🚨 **BEFORE SCOPING A TRACK, GREP FOR THE THING YOU ARE ABOUT TO BUILD.** wazmrt scoped a ~5-item
security track that was **~70% already built** — per-proposal gating had shipped six days earlier, and
**the same file contradicted itself two sections up**, where its own change table marks the step done.
🎓 **This is the exact inverse of recording work as DONE that was not, and the root cause is identical:
a status line written from an ARGUMENT rather than from the code.** Both directions cost real time and
both are prevented by the same five-second habit.

⚠️ **"BY DESIGN" AND "OUT OF SCOPE" AGE BADLY BY DEFINITION.** They are statements about SCOPE on one
date, so every line expires the moment the scope moves — and nothing about them fails a build, which is
why they are the entries least likely to be checked and most likely to read as current. wazmrt's
by-design list named three untargeted proposals and two of the three shipped within a week. **Re-read
scope invariants whenever a track closes. And a CORRECTION is a dated claim too** — a note marking a
list stale was itself stale within the hour.

⚠️ **A REOPEN CONDITION IS NOT SELF-CHECKING — re-test it when you price the entry.** wazmrt's standing
delta carried the condition *"an oracle appears"*, which was **false the day it was written**: the tool
that implements exactly that behaviour lives one sibling directory away. **"We cannot" and "we have
chosen not to" are different claims, and only one of them ages.**

⚠️⚠️ **"UPDATE THE PROJECT MEMORY" MEANS AUDIT FOR STALE LIVE CLAIMS, NOT EDIT THE FILES YOU HAPPENED TO
TOUCH.** A wazmrt session that revised six `cmem` files left six stale claims standing — including the
headline score heading, the single line most likely to be quoted, in a file that had just been edited
twice. Two numbers were stale in **both directions and disagreed with each other**. **The audit that
works is a grep for the OLD VALUES, then classifying every hit as live (fix) or dated history (leave,
and mark it superseded if it claims to be current). When two files disagree, do not pick the newer one —
MEASURE.** Watch for the words *"final"* and *"the number to quote"*, which both appeared on figures
superseded within days. 🆕 **Applied to wasmrt on 2026-08-19 and it paid on the first grep**: the roadmap's
own status block and `overview.md` were still quoting the pre-tail-call suite, and every recorded `.wat`
corpus figure (533/534, 534/534, 533/533) is against a denominator that **no longer exists** — the tree
holds **532** `.wat` files today. See `testing.md`. This rule is now part of the binding trigger in
`INDEX.md`.

**When a claim will LEAVE this repo — a commit message, a cross-project report, a number in a README —
verify it against the ARTIFACT, not against something that talks about the artifact.** Three
read-not-verify errors in one wazmrt session: a CLI's `valid wasm v1` header read as a verdict, a
benchmark's missing `--` read as a competitor's defect, and a doc comment's SUBJECT read as a field
name — the last of which arrived here as advice naming a `Pools.type_canon` that does not exist
(`known-issues.md`). **Advice with a wrong mechanism is worse than no advice, because the recipient has
to disprove it.** ⚠️ This binds symmetrically: wasmrt sends reports back the other way.

**A DIFFERENTIAL CHECK WITH NO PRIVILEGED ORACLE FINDS THINGS A GOLDEN FILE CANNOT.** Requiring every
runtime to *agree* — rather than trusting one as the answer — found a one-byte disagreement on its first
run, across five implementations including V8, against wasmtime 47.0.3. ⚠️ And the discipline continues
past the finding: **the cause was not traced, so it is recorded as an observation, not a diagnosis.**
→ this is the shape T12x should take now that the oracle is retired: not "diff against the oracle" but
"require agreement, and record disagreements as observations".

#### For this machine — the `D:` build blocker, misdiagnosed FOUR times

⚠️⚠️ **FINDING A REAL DEFECT AT A LAYER IS NOT EVIDENCE THAT IT CAUSES YOUR SYMPTOM.** wazmrt's build
failure on `D:` was blamed on failing hardware (six `disk`/event-51 entries — **all a single burst from
a week earlier; the timestamps were never read**), then on antivirus (refuted: Defender is *stopped* on
this machine and the active agent is Datto EDR), then on a genuinely damaged exFAT volume
(`Get-Volume -DriveLetter D` → `Full Repair Needed`). **The repair worked, and the build failed exactly
as before.** Each of the three found something real and stopped there, *because a confirmed defect feels
like an answer*.

**What finally worked was varying ONE THING AT A TIME** rather than reasoning about which layer looked
guiltiest — and the cause is the **cache LOCATION**: a build cache on that exFAT volume survives exactly
one build and is then poisoned. ⚠️ **A workaround that works exactly once is a clue about FREQUENCY, not
a failed fix — ask what makes the second attempt different.** Two further rules from the same incident:
**identify the LAYER (physical disk → volume → OS → application) before naming a cause**, since each has
its own cheap health query; and **search `cmem` for the environment before diagnosing the environment**,
because both repos already recorded "`D:` is exFAT" and two wrong causes shipped in four commit messages
for want of a five-second grep. → For wasmrt this predicts the same shape for a cargo `target/` on `D:`;
the fix is `CARGO_TARGET_DIR` on an NTFS path, and it belongs with the recorded hard-link warnings.

#### Third borrow round (2026-08-19, `coordinate`) — one rule, and both projects had just proved it

⚠⚠ **A CHANGE’S OWN NEW SURFACE IS THE ONE PLACE THE AUDIT THAT PRODUCED IT WILL NOT LOOK.** wazmrt
found that a host flag written after a guest argument is **silently donated to the guest and never
applied** — and demonstrated it with `--max-iterations`, **a flag its own track had added hours
earlier**. The audit that introduced the surface did not re-audit it.

🎓 **This project produced an independent instance the same day, which is why it is being written down
rather than merely borrowed.** Implementing the six GC array bulk ops meant adding six new internal `Op`
tags — and three of them (`0xcd..0xcf`) landed **outside** the decoder’s internal-tag guard, so a raw
`0xcd` byte would have decoded as `array.copy`. **That is the tag-space hazard recorded in §3A.2 the
same morning, re-created within hours, in the surface the change itself created.** Neither the existing
tests nor the corpus would have caught it; the guard was extended only because the rule was fresh.

**The habit that follows:** when a change adds a *kind of thing* — a flag, an opcode tag, an error
variant, a config field — **re-run the audit that governs that kind of thing against the new members,
before the change lands.** The check already exists; what is missing is applying it to what you just
wrote. *(Both instances: wazmrt H7 / interop v10; wasmrt’s `0xcd..0xcf` guard, `known-issues.md`.)*

🔻 **And the pen-holder’s duty this round taught, worth keeping:** *a refuted claim left in the table is
what the next reader quotes.* An annex records a finding; **the table is what gets believed** — so
folding a correction into the row it refutes is the job, not an optional tidy-up. wazmrt folded wasmrt’s
F4 into `interop.md` §2.2 for exactly that reason.

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

### 5.5a 🔒 Read the skip column to RANK; work the FAILURE column to FIX

**Borrowed from wazmrt, and measured here before adopting (owner, 2026-08-19):** *"fixing the failures
first cleared a lot of the skips."*

Probed against our own corpus rather than agreed with, and it holds: of 930 cascade skips, **739 (79%)
sit in files that also carry failures**, and only 191 sit in files with none. `exact-casts.wast` has
**3 failures, 108 skips and zero capability gaps** — all 108 clear when the 3 are fixed.

⚠️ **This is the inverse of §5.6 and both are true, because they answer different questions.** §5.6
says a failure count *undercounts* a defect whose symptom is a skip — so **rank** with the skip column.
This says a failure is a root you can act on while a cascade is not actionable until its root moves —
so **fix** down the failure column. Reading one rule as contradicting the other is how a work-list ends
up ordered by neither.

🔻 **The residue is the part that matters, and it is where the biggest item hides.** After the failure
pass, what remains is *by construction* the skips no failure could reach — here, 160 of the surviving
191 are one item (the externref/anyref bridge) across four files with **zero failures between them**.
**A failures-first order is a sequencing rule, not a claim that the skip column is derivative** — so
the second phase is mandatory, and it is re-measured, never carried over from the first estimate.

### 5.6 🎓 A number the report cannot ATTRIBUTE is not a measurement

**Bought by (2026-08-19):** "1,024 skipped" was printed in every conformance summary for the whole
port, and it was **unscopeable**. Two holes, both in the *instrument*:

1. The runner printed a per-file line only when `failed > 0`. **234 of 1,024 skips lived in files that
   produced no output at all** — `ref_test.wast` (68 skips), `ref_cast.wast` (42), both
   `br_on_cast*.wast` (27 each) had **never once appeared in a conformance report**.
2. A skip was a bare counter at all twenty skip sites, so even a *listed* file could not say why.

Adding a reason to every skip showed that **928 of 1,024 (91%) were cascades** — assertions stranded
behind a module that never built — and that only **96** were real gaps, resolving to **seven items**.
One of them (the externref/anyref bridge) accounted for ~176 assertions across five files.

**The rules, and they generalise past conformance:**

- **A count with no cause cannot be ranked.** Planning off the skip column would have put a 108-skip
  cascade *above* the single root that produces it. Rank by cause, and rank by **assertions unblocked**.
- **A report that lists only the loud category cannot scope the quiet one.** `failed > 0` is a
  perfectly sensible filter for reading failures and a total blind spot for everything else. Ask what
  the report **omits**, not only what it says — the sibling of §3.8.
- **Instrument BEFORE triaging, every time.** T13-0 already established this and was worth 292
  skips→passes; the day it was skipped for the skip column, the same lesson cost the same way again.
- **Give a counter a reason at the site, not a post-hoc classifier.** The reason is free where the
  decision is made and unrecoverable afterwards.

Pinned by `every_skip_records_a_reason`, which asserts both that every skip carries a reason **and
that the reasons distinguish the paths** — a single catch-all string would satisfy a count-only check,
which is §4.1 (a gate that cannot fail is decoration) applied to the fix for this very lesson.

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


### 8.3 🏁 "BAKE OFF" vs. "ISSUE" — a NOTATION rule, because the two are different kinds of work

🔒 **Owner, 2026-08-19: make the distinction visible in the notation.** The two look alike in a task
list and behave nothing alike, and mixing them makes the fix queue look permanently non-empty.

| | **ISSUE** — a fix task | **BAKE-OFF** — a compare task |
| --- | --- | --- |
| what it does | **changes the code** | **measures it against something else** |
| notation | `[ ]` → `[x]`, an entry in `known-issues.md` | 🏁 **never a checkbox**; a dated row in a measurements table |
| when it is scheduled | when a **gap** is found | when a **number** is wanted |
| what "done" means | ✅ **closed** — the defect is gone | ⚠️ **there is no done.** It is *measured on a date*, and the measurement decays |
| how it is reported | fixed / open / won't-fix, with a reason | **value + date + configuration**, never a checkmark |
| external dependencies | must not need any | ⚠️ **the only kind that legitimately pulls rivals, other trees and foreign harnesses in** |

**The rules that follow from it:**

1. ⚠️ **NEVER put a compare task in the fix queue.** *A compare task can never be finished* — rivals
   ship new versions, corpora grow, and the machine changes underneath, so its residuals regenerate by
   construction. Parked in a fix list it reads as permanent unfinished work and quietly devalues every
   real `[ ]` beside it.
2. **Never mark one ✅ DONE.** Write *"measured 2026-08-19: X"*. A bake-off row without a **date and a
   configuration** is worse than no row, because a stale number reads as current — and this project has
   already been bitten by a size figure that matched a ceiling to the byte because it was reading
   yesterday's artifact.
3. ⚠️⚠️ **A bake-off is the ONLY task type that may reach outside the repo**, which makes it the one to
   watch in a project whose invariant is **zero third-party dependencies**. Its harness, its rivals and
   its corpora all live *outside* the shipped artifact, and nothing it needs may leak into one.
4. **Sort work by what it CHANGES before sorting it by priority.** That is the whole rule in one line.
5. **A comparison is evidence, never a target.** ⚠️ A contract row, an invariant or a design choice must
   never be justified by *"the other one is faster/smaller"* — that is what `interop.md` §0 puts
   performance and size out of scope for. **A bake-off tells you where you stand; it does not tell you
   what to build.**

**Applied here:** **T11** holds both kinds and they are now labelled as such — the optimization review
is an ISSUE-shaped fix task that ends, while the **same-machine comparison against wasm3 / WAMR /
wazmrt is a 🏁 BAKE-OFF** that is scheduled when a number is wanted and never closes. The competitor's
published head-to-head figures (`vision.md`) are a 🏁 row too: dated, configuration-bound, **and not
adoptable as ours**.

*(Borrowed method — wazmrt reclassified its own bake-off out of its work queue on 2026-08-18 for exactly
this reason, and the reclassification is what made its remaining fix list read honestly.)*
