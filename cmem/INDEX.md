# cmem — Portable Project Memory for wasmrt

This folder is the **authoritative, portable project memory** for `wasmrt`. It lives inside the
project tree, so it travels with the project (USB drive, clones, GitHub) and is **committed to git** —
unlike a machine-local `CLAUDE.md`. Mirrors the `cmem/` convention of the source project `wazmrt`.

**Format:** plain Markdown — one focused topic file per domain, so any single concern can be reviewed
and revised without wading through one giant file. Keep files small and single-topic.

**What `wasmrt` is:** an idiomatic-**Rust** port of the Zig WebAssembly runtime **`wazmrt`** (sibling
repo `../wazmrt`), built to **replace wasmtime** as the engine beneath the owner's
`universalWasmLoader-*` projects. See [overview.md](overview.md).

## 🔒 THE ORACLE IS RETIRED (2026-08-11) — wasmrt stands alone and competes on merit (read first)

**wasmrt no longer refers back to the `wazmrt` repo.** The port is done being a port. The two runtimes
are now **independent entrants** for inclusion in **wasmtk** and the **universalWasmLoader-\*** runtimes,
decided on **the smallest and fastest binary**; **`rsxtk` takes wasmrt by default** through the native
Rust interface. wazmrt is running its own size program for the same contest, so its head is a
*competitor's* design — following it is exactly backwards.

`scripts/check-wazmrt.sh` is **deleted**; its baseline survives as `scripts/wazmrt-provenance.txt`, a
historical record nothing reads. **Correctness now anchors on the official spec testsuite, wasmtime's
observable behaviour, and the wasmtk WASI corpus** — always the harder tests; the oracle was the
convenient one. **`1.0.0` no longer means "parity with wazmrt"**: it means every in-scope proposal
implemented, conformance at its ceiling, the C ABI stable, and the size/speed numbers measured and
defended.

⚠️ **What this makes urgent:** while the oracle defined success, *canonical* was the gate and fast/small
were aspirations with a footnote. **Now fast and small ARE the gate**, and three gaps promote to the
critical path — the unattributed ~5% steady-state regression from T9's ninth pass, the fact that
**nobody has ever measured what `rsxtk` actually links** (the rlib, not the cdylib), and the absence of
any same-machine comparison against a competing runtime. All three are **T11**, which is no longer a
late-stage nicety. See [vision.md](vision.md) and [design-decisions.md](design-decisions.md).

⚠️ **Provenance is NOT retired.** wasmrt is derived from wazmrt in design; both are the owner's under the
same dual MIT/Apache licence, so nothing is owed to a third party, but `licensing.md`'s attribution
stays. Retiring a gate is not rewriting where the work came from.

*Historical (2026-07-27 – 2026-08-10): the gate opened when `wazmrt` reached full parity and `zig build
test` passed, freezing it as a reference oracle at `wazmrt@dadc727`; the baseline was re-pinned six times
under owner authorization. Tail calls (`return_call`/`return_call_indirect`) were always the one
wasmrt-target feature with **no** oracle and were planned against wasmtime + the spec testsuite — which
is now simply how everything is done. memory64 **is** in scope (owner, 2026-07-27).*

## ◐ T9 IN PROGRESS (2026-08-14) — eighteen passes landed, unreleased

Working tree is **ahead of the published v0.9.0**. Suite **62,238 / 378 / 2,038 — 99.4%** of 62,616
adjudicated; the **`.wat` corpus is a clean 533/533** through assemble→decode→validate;
**458 tests** (420 core + 28 capi + 10 CLI); clippy, all four surfaces, the C-ABI gate
(74/74 + `c_smoke`, now asserting real trap frames) and Miri (28/28) green. **No file lost a single
pass** in any of the eighteen passes — the check that matters whenever skips convert into verdicts.

### 2026-08-14 (eighteenth) — T9f TAIL CALLS: the last unimplemented in-scope proposal

`return_call` (`0x12`) and `return_call_indirect` (`0x13`) are implemented — decoder, validator,
assembler, interpreter, and a C ABI flag. Spec files: `return_call.wast` **44/0/0**,
`return_call_indirect.wast` **76/0/0** (was 27/3/**49 skipped**), `return_call_ref.wast` **46/0/0**
(was 40/7/0). Suite **62,238 / 378 / 2,038**, +125 passes and −125 skips. **1.0 is no longer blocked
on a missing proposal.**

⚠️⚠️ **The finding: `return_call_ref` had shipped for releases as a FAKE tail call.** It called
`call_function` and then jumped to the end of the body — "call then return". Every answer it produced
was correct, and the native stack grew on every hop, so the one thing the proposal exists for did not
work. The roadmap predicted this exact trap ("a naive call-then-return passes the tests and misses the
feature") and it was already in the tree.

**The fix is structural**: `run` no longer recurses on a tail call. It reports the intended callee
through a `TailCall` out-parameter and unwinds; `call_function` **loops**, reusing its own native frame
at the same `depth`. An unbounded chain now runs in constant native stack. `countdown(5,000,000)`
returns; the identical function using `call` traps. ⚠️ Deliberate consequence, recorded rather than
discovered later: **a replaced frame leaves no backtrace entry**, because that is what replacing it
means.

⚠️⚠️ **Conformance could not have caught the fake, and this is the transferable part.** Under a
mutation restoring "call then return", the new property test fails **5 of 6** while the three spec
files still score 38/6, 72/4, 40/6 — high-80s-to-90s, which reads as "nearly done, a few edge cases".
That is precisely how 40/7 was read for releases. **Every conformance assertion checks a RESULT; the
tail-call proposal is about a RESOURCE.** Pinned by `tests/tail_call_is_a_real_tail_call.wast`
(self-recursion, *mutual* recursion, through a table, and through a typed ref — 1,000,000 deep each),
which `regression_wast.rs` runs under `cargo test`.

🆕 **Two defects found on the way, both pre-existing:**
- **`return_call_ref`'s validator used EQUALITY where the spec says subtyping** (§3.3.8), so it
  *refused valid modules* — `(result (ref null $t))` tail-calling a callee returning `(ref $t)` is
  legal. Wrong in the refusing direction, which is why nothing noticed: a rejected valid module is a
  failing assertion, never a wrong answer. All three forms now share **one** `check_tail_results`.
- 🆕 **Instance #5 of the T10a emitter mechanism, created and caught within the hour**:
  `emit_call_indirect` hard-coded `push(0x11)`, so routing `return_call_indirect` through it emitted
  a plain `call_indirect` — the text said one thing, the module another. It surfaced as a
  **`StackHeightMismatch` from the validator**, three stages away (§3.7). The opcode is a parameter now.

✅ **T10a's opcode sweep caught the missing immediates the moment they appeared**, before the defect
could ship — the first time one of these gates has fired *preventively* rather than forensically.

⚠️ **Performance: MEASURED, A/B/A, and the answer came back the other way.** `run` gained a parameter,
and the last such change cost 3.6%. Steady state: with tail calls **227/226/226** and **231/234/231**
Mops/s; parent commit **214/219/217**. The change is **faster**, not slower. 🚦 **But the recorded
~237 Mops/s baseline does not reproduce on this machine at all** — the parent measures ~216 — so
build-to-build variance here (~7%) is larger than the "unattributed ~5% regression" T11 is chasing.
**T11's first job is a benchmark methodology that can resolve 5%, not the 5% itself.**

### 2026-08-14 (seventeenth) — cross-module reference IDENTITY, reported from wazmrt

wazmrt hit a cross-module GC-reference defect, fixed it, swept for the same class here and **filed a
report in this repo** (`known-issues.md`, commit `30ef881`) with a committed reproducer. All three
findings are now closed — and the sweep found **a second defective site the report did not**.

**The bug:** a GC object's `type_index` was read against the **reader's** module. `gc_heap` lives on
`Pools`, shared across a linking group, so objects genuinely cross modules — and the index that
describes one meant something different on the other side. Wrong in **both** directions: it accepted a
`ref.cast` to a structurally different type (after which `struct.get` reads a field at another type's
width — a silent wrong value from a check that believed it had verified the type) and rejected the
correct one. Fix: `HeapObject` carries its allocating instance, exactly as a funcref packs its owner in
bits 62..32, and a concrete cross-instance match resolves **both** sides through the store-wide registry.
`owner` is **free** — it lands in padding the `Vec`'s alignment already forced, pinned by `size_of`.

🆕 **The second site: the `Func` arm of the same function.** It had already been fixed once for this
class — it fetched the funcref's type index from the owner's module and then compared it in the
**testing** module anyway. Its own comment called that **"approximate"**. ⚠️⚠️ **A defect described in a
comment is still a defect; the hedging word was doing the concealing.** The full spec suite never moved
when it was fixed, because nothing in 62,498 assertions casts a funcref to a concrete type across a
module boundary.

⚠️ **The generalization is `best-practices.md` §3.9** — *a shared registry wired into ONE consumer is a
bug in the others.* Prompted by wazmrt's own note (*"you built the registry and wired it into
`call_indirect` only"*), every type-identity comparison was **enumerated instead of reasoned about**:
import matching ✅, `call_indirect` ✅, `ref_matches`/Any 🔴, `ref_matches`/Func 🔴. **Two of four wrong,
and only one was in the report.**

Also: the shared-heap **immunity is now a stated 🔒 invariant** on `gc_heap` (do not consolidate it onto
`InstanceData`); and the latent **externref/GC index overlap** is blocked *at the point of entry* — the
absent `0xfb 0x1a`/`0x1b` opcode arms carry the warning and the fix (`Value` is a `u128` with the high
64 bits free for a tag, so **no widening is needed**), so an implementer meets it where they will be.

🆕 **A gate the reproducers themselves needed**: both `.wast` files were green and **nothing ran them**.
`regression_wast.rs` now runs every `tests/*.wast` under `cargo test`, asserting zero failures **and zero
skips**, failing if the directory is empty. *That is wazmrt's* **"a gate only gates the commits that RUN
it"** *applied within the hour of reading it* — see §3A, the owner-authorized borrow of wazmrt's
**method** (not its design; the oracle stays retired).

**457 tests** (was 456). Suite **byte-identical** at 62,113 / 385 / 2,163 — correct, since this is a
cross-module path the suite cannot reach. All four changes mutation-verified with the mutation
**confirmed applied** first.

### 2026-08-11 (sixteenth) — the CLI could not run a `.wat` file at all

`wasmrt run prog.wat f` answered *"not a WebAssembly binary (bad magic)"*, and so did
`wasmrt wasi prog.wat` and plain `wasmrt prog.wat`. The assembler was in the same executable the whole
time, reachable only as a separate `wasmrt wat -o out.wasm` step. The oracle assembled `.wat` on its run
path from its first release, so this was a **port/oracle divergence** in the port's favour-of-nobody: not
a wrong answer, an absent one.

All three loaders now share one `read_module_bytes` helper that sniffs the extension and assembles first,
so they cannot drift into accepting different things. Dispatch is on the **extension**, deliberately, so
the stage blamed for a failure stays honest — malformed text reports `cannot assemble`, malformed bytes
`decode failed`; content-sniffing would hand a corrupt binary to the assembler and report a syntax error
for it. Validation still runs, on the assembled bytes, pinned by a test: accepting text must not become a
side door around the check the fifteenth pass just installed.

⚠️⚠️ **Found by the owner asking whether the runtime ran `.wat` files — the second finding in two days
that came from an owner question rather than a test**, and the reason is structural: every gate here
compares *answers* on inputs both runtimes accept, so a capability one runtime simply does not offer
produces nothing to diff. Written up as **§3.8** in `best-practices.md`, whose remedy — enumerate the
oracle CLI's subcommands, file types and flags in a table and check the port entry by entry — is §3.4's
remedy applied to the outside of the tool instead of the inside.

### 2026-08-10 (fifteenth) — `wasmrt run` executed WITHOUT VALIDATING, and so did the oracle

**Found by the owner questioning a claim, not by a test.** Resolving T9a#9 I wrote that "the oracle's
execution path skips validation" as if it were wazmrt's peculiarity. Asked to justify it, the check
that should have come first showed **wasmrt had the same hole**: `wasmrt run ill-typed.wasm f` printed
`1` and exited **0**, while `wasmrt wasi` one function away refused the same bytes.

⚠️ **The asymmetry was the bug** — the second instance of §3.4 (the first was the C ABI holding a
defence core lacked at T9a#3). And it was in **both runtimes at once**: wazmrt's summarize and `.wast`
paths validated while **both** execute paths and **`wasm_module_new`** did not.

🔒 **Fixed concurrently in both repos, with a deliberate oracle re-baseline** to `wazmrt` —
the first oracle movement since the 2026-07-27 freeze. wazmrt `zig build test` **489/493 in ALL FOUR
modes** (the freeze record only ever claimed Debug + ReleaseSafe; ReleaseFast and ReleaseSmall are
green and that gap in the record is now closed). wasmtk WASI **376/376 unaffected**; wasmrt's `.wat`
corpus a clean **532/532**. The `39_JstyperMixed` fixture now gives **byte-identical stdout from both
runtimes**. Severity is low for wasmrt — `forbid(unsafe_code)` bounds it to a wrong answer, and a
type-confusion probe trapped cleanly — but wrong-answer is the class this project ranks worst.

✅ **The owner then resolved the open question by measuring wasmtime**, not by arguing the API: wasmtime
47 refuses with `Invalid input WebAssembly code at offset 33: type mismatch: expected i32, found i64`,
so **wasmrt matches that action**. Our diagnostic now carries the **same byte offset** (from the start of
the module — verified byte-identical on two modules, 33 and 61, both pinned as tests), the same wording,
plus the function index wasmtime does not print. `Instance::new` keeps the compile/instantiate split,
which is safe in practice because every shipped entry point validates. See
[design-decisions.md](design-decisions.md). ⚠️ **Method note: when a decision is to be based on a
reference implementation, RUN it** — three lines through the real binary settled in seconds what the API
docs invite answering from memory.

### 2026-08-08 (fourteenth) — T9a#9 is **NOT a defect**. The right outcome was to change nothing

**Suite unchanged at 62,113 / 385 / 2,163 — correctly.** 442 tests; the four new ones pin a conclusion
rather than a fix. **T9a #1–#9 and #11 are now all closed.**

#9 read: *"the oracle assembles **and runs** them, so this is our type-checker being wrong, not the
input."* Every observation true; the conclusion wrong three ways:

1. **The module is ill-typed.** `if (result f64)` with **both arms pushing i32** — §3.3.5, and
   `if.wast`'s `type-then-value-num-vs-num` is an `assert_invalid` for exactly that shape.
2. ⚠️⚠️ **"It runs" was never evidence of validity.** `wazmrt <module> <export>` executes **without
   validating**; `wazmrt <module>` validates. Through that path the oracle reports
   `validation: FAILED — TypeMismatch` — **agreeing with wasmrt**. Verified with a blatant control.
3. **The fixture is stale and double-counted** — the two files are **byte-identical**, and it has
   8 functions / 10 types against the source binary's 14 / 13, so it is not a round trip of it at all.
   The real `.wasm` and the hand-written `.wat` both validate **OK**.

**What landed instead:** validation failures now name the function
(`validation FAILED in function 8: …`). `TypeMismatch` carried no location, so localizing this to
body #6 of 19 needed a temporary probe — that absence is what the item really cost. Implemented as a
thread-local rather than widening `ValidateError`, which is `Copy`, exhaustively matched, and crosses
the C ABI; cleared on entry so a module-level failure reports **no** location instead of inheriting
the last one.

⚠️ **The lesson: cite the subcommand, not the tool.** "The oracle accepts it" is evidence only if you
know which path you invoked, and a runtime that *executes* an invalid module is over-permissive, not
authoritative. Recorded as [best-practices.md](best-practices.md) §2.3a — the sibling of §1.5a, which
is the same mistake made about our own gate.

### 2026-08-08 (thirteenth) — T9a#8. **Logged at "1 file", delivered 76 assertions**

**+76 passes, −8 failures, −82 skips.** Four files to zero — `call_ref.wast` **31/0/0**,
`br_on_null.wast` **7/0/0**, `ref_as_non_null.wast` **5/0/0**, `unreached-valid.wast` **10/0/0** —
plus `return_call_ref.wast` 10/5/36 → 40/7/0. 🆕 **The `.wat` corpus now assembles 534/534 for the
first time**, with 0 decode failures; only the `39_JstyperMixed` pair (T9a#9) still fails, at
validation.

**The defect:** `immediate_arity` ended in `_ => 0` and the emitter's match in `_ => {}`, so four
instructions were emitted as **bare opcodes with their operand left in the token stream** —
`call_ref`, `return_call_ref` (typeidx), `br_on_null`, `br_on_non_null` (labelidx). All four decode
and execute correctly; only the assembler was wrong.

⚠️ **None of the three symptoms named the assembler.** Folded `call_ref` shifted every following byte
into a body the *decoder* rejected for a missing `end`; the flat forms blamed the *next* token with
`UnknownInstr` — which is why `gc-linked-list.wat` sat as an unexplained "1 of 534" logged
**separately from #8**. One cause, three symptoms, two punch-list items.

🆕 **The gate had been measuring the wrong thing.** The `.wat` corpus check ran *assemble only* and
read 533/534 while `call_ref`'s output was undecodable. **"The assembler returned Ok" is evidence
about the parser, not the emitter** — the assemble→decode→validate round trip is what turned a vague
one-file entry into 76 assertions. Now recorded as the corpus gate in [testing.md](testing.md).

**The generalization — T10a's field-coverage sweep, landed for opcodes.** `Op::from_u8` makes the
opcode space enumerable, so a test walks all 256 single-byte opcodes and asserts the **decoder** and
**assembler** agree about which take an immediate, in both directions. This is the **fourth** instance
of "the emitter reconstructs a form from a subset of the parser's facts"; T10a predicted more than
three and is now 4-for-4. ⚠️ **Mutation-verified, and the first attempt lied** — a `perl` substitution
silently failed to match, the test passed, and that reads exactly like "my check is decoration"
(§4.2a).

### 2026-08-08 (twelfth) — the text format's source character set (§6.2/§6.3)

**+50 passes, −48 failures.** **`id.wast` reached 6/0/1 — a file at zero failures**; `annotations.wast`
12/51 → **56/8**. No file lost a pass; the `.wat` corpus held at **533/534**, the check that matters
when *tightening* a parser.

⚠️⚠️ **THE FINDING IS ABOUT A SCOPE NOTE, NOT A BUG.** T9g had `annotations` **51** filed under *scope
confirmations — NOT bugs, do not "fix"*, as an untargeted proposal. That was true of the **file** and
false of **44 of its 51 assertions**, which were generic §6.2/§6.3 lexer rules applying to every `.wat`
wasmrt reads. Four one-line probes, none containing an annotation, were all accepted: a control
character in an identifier, invalid UTF-8 in an identifier, a raw control byte in a string. **The proof
the cause was generic is `id.wast`** — unrelated to annotations, 5 failures, now zero. **A scope note is
a hypothesis about a cause too, and it is the one nobody re-measures** ([best-practices.md](best-practices.md) §1.1a).

🆕 **`from_utf8_lossy` in the lexer did not merely over-accept — it silently RENAMED.** `$a\xffb` and
`$a\xfeb` both became `$a\u{FFFD}b`, so two distinct identifiers collided on one name; the quoted form
`$"…"` carried the same bug. Restricting atoms to `idchar` makes the slice ASCII by construction.

⚠️ **One of the four probes was NOT a defect and was deliberately left alone** — `linechar` admits any
character but a newline, so a control byte in a comment is legal, and a test now pins that it stays
accepted. **Tightening what the grammar permits is the same error as accepting what it forbids** (§4.6a).

### 2026-08-08 (eleventh) — T9a#7 trap backtraces, **and the start function that never ran**

**+12 passes, −12 failures** — and all 12 came from the *second* item. T9a#7 is diagnostics and was
correctly predicted at 0 suite assertions. `Instr` gained an `offset: u32` that is **free** (it fits
padding `Imm`'s 16-byte alignment already forced; a `size_of` test pins it so a future `Imm` shrink
fails the build instead of silently paying 16 bytes per instruction). Frames are built **on the way
out** through `call_function`, not as a shadow stack, so nothing is paid until something traps. The C
ABI frozen at T8 went live with **no ABI change** — the bet paid off. The CLI now prints an
oracle-shaped backtrace to stderr.

⚠️ **THE PLUMBING FOR A COLD FEATURE CAN COST THE HOT PATH.** Threading `pc: &mut usize` through `run`
**measured 3.6% slower** on the steady loop (2160 vs 2083 ms, A/B/A); a one-shot closure keeps `pc` in
a register and re-measured inside the 2083–2095 baseline band. Benchmark anything that touches a hot
loop's locals or signature, however cold the feature motivating it.

🆕 **Asking where an *instantiation* trap gets its frames led to asking who runs `Module::start` —
nobody.** §4.5.5 step 11 was simply absent while the field was decoded, validated, and printed by the
CLI. A module initialized entirely by `(start $f)` returned defaults with no error at any stage:
**silent wrong output**, the worst class. `start.wast` **8/7 → 15/0**, `start0.wast` 5/3 → **8/0**,
`linking3.wast` → **12/0**, `linking.wast` +1. ⚠️⚠️ **10 of those had been failing for five releases in
files named for the feature** — before diagnosing a file's failures, check whether the feature it is
NAMED for works end to end. Both lessons are in [best-practices.md](best-practices.md) (§1.7, §3.1a).

### 2026-08-08 (tenth) — T9a#5, GC constant expressions. **Logged cost 6, delivered 88**

+88 passes, −4 failures, **−92 skips**. `i31.wast` **0/6/66 → 61/2/5**, `array.wast` **6/2/43 → 18/2/29**,
`struct.wast` **6/3/17 → 21/3/0**. Nothing regressed.

Six forms in **both** the validator and the interpreter — `struct.new`, `struct.new_default`, `array.new`,
`array.new_default`, `array.new_fixed`, `ref.i31`. The same set on both sides deliberately: a validator that
accepts what the evaluator rejects is the disagreement class that once made `v128.const` in a const-expr a
false rejection.

⚠️⚠️ **THE MEASUREMENT LESSON IN A NEW DIRECTION: a cost counted in FAILURES understates any defect that
stops a module BUILDING.** `ConstantExpressionRequired` on a global initializer fails the whole module, and
every later assertion in that file then has no target and is **skipped**. `i31.wast` was 0 passed / 6 failed
/ **66 skipped** — the 6 was the visible cost, the 66 the real one; the three GC files carried 126 skips
between them. **Read the skip column when triaging**, especially for a defect in a module-level position
(a global initializer, a type definition, a section).

### 2026-08-08 (ninth) — **T9a#4 COMPLETE**: the funcref encoding, then imported tables

+85 passes, −15 failures, **−127 skips**. `elem.wast` **63/13/17 → 75/6/0**, `imports.wast` +34/−69 skips,
`linking.wast` +24/−20, `table_grow.wast` → **50/0/0**, `imports4.wast` → **11/0/0**; `imports0`,
`linking0`, `linking3` to zero skips. No file lost a pass.

**A `funcref` now carries its owning instance** — instance in bits 62..32, function index in 31..0.
⚠️ Bit 63 is `I31_TAG`, which is why the field is 31 bits and not 32; that collision was recorded before
the work began. ✅ **The property that made a value-model change safe to introduce: instance 0 packs to the
bare index**, so the encoding alone moved the suite by **+1/−1** — one genuine cross-instance case in
`elem.wast` that had been silently wrong. Producers (`ref.func`, element segments, const-exprs) stamp the
defining instance; consumers (`call_ref`, `call_indirect`, `ref_matches`) resolve against the **owner**.
Then imported tables, with §4.5.9 matching and the spectest `table`.

🆕 **It caught a defect in the memory work from earlier the same day.** A table or memory *instance*'s type
has `min = its CURRENT size`; `grow` updates it (§4.5.9). The memory pass stored the **declared** minimum
**and asserted that in a test**. Nothing in the suite contradicted it; the table case did, and says so in
its own comment. ⚠️ **A test can encode a misreading of the spec and pass forever if nothing exercises it.**

⚠️ **Unattributed: ~5% steady-state regression, handed to T11.** Cold start unchanged. Steady
`sum(1000000)` ~55.7 vs ~52.7 ms, consistent across A/B/A, on a loop touching no funcref, table or type.
Two hypotheses tested and **rejected** (moving the `TypeRegistry` to the end of `Pools`; boxing it). Likely
code layout in the giant `run` match — recorded as measured-but-unexplained rather than asserted.

### 2026-08-08 (eighth) — the same type-use rules at their OTHER TWO sites. `call_indirect` → 0

+24 passes, −24 failures. **`call_indirect.wast` 158/11 → 169/0/0**, **`func.wast` 147/21 → 160/8**; no file
lost a pass, none gained a failure, `.wat` corpus held at 533/534.

**Block types, `call_indirect` and function definitions each had their OWN COPY of the type-use loop** — and
so its own copy of all three defects. `parse_type_use` is now the single authority for the first two; the
function path enforces the same order inline, since its loop also owns `import`/`export`/`local` and the
body. **Two copies of a grammar drift — three copies drifted identically.**

⚠️ **One rule was attempted, measured and WITHDRAWN, and it looked obviously right.** "No declaration after
the body begins" broke `select.wast`, `stack.wast` and `call_indirect.wast`: in **flat** instruction form
each immediate is its own top-level item, so `select (result i32)` puts a `result` form exactly where a
misplaced declaration would sit — keyword scanning cannot tell them apart at that layer, and deciding it
needs the body's instruction structure. `(func (nop) (local i32))` therefore still assembles (~4 assertions,
logged). **A rule that is obviously right is still a hypothesis until measured.**

### 2026-08-08 (seventh) — the text format's own grammar was not enforced. **`block`/`if`/`loop` → 0 failures**

+40 passes, −40 failures; **`block.wast` 13→0, `if.wast` 13→0, `loop.wast` 13→0**, `type.wast` 1→0. No file
lost a pass, and the **`.wat` corpus held at 533/534** — the check that matters when *tightening* a parser.

**Found by measurement, not from the list.** #12's remainder was logged as "`func.wast` 21"; surveying the
worst in-scope files showed `block`/`if`/`loop` at 13 each with an **identical breakdown** — the signature of
one shared cause, and the largest in-scope cluster left. A **type use** has a fixed clause order (§6.4.4):
`(type x)?` then `(param …)*` then `(result …)*`. The assembler collected clauses in any order and ignored
`(type …)` outright, so `(block (result i32) (param i32))` assembled and the **validator** reported the
result as a stack-height mismatch. Two more from the same function: a block parameter **cannot be named**,
and `(type x)` plus explicit clauses must **match** (the assembler returned on the index and silently
discarded them — the emitter-defect class, reached from the parser side).

⚠️ **Where a guard lives matters: one call-level away is NO check.** The first attempt put the order rule in
`parse_sig` and moved **one** assertion of forty — because `parse_block_type` calls `parse_sig` **one clause
at a time**, so the order state was built and destroyed per clause and could never see a sequence. The check
had to move to the loop that iterates. Only the measurement said so.

**Still open in this cluster:** `func.wast` (9 "unexpected token", 3 "duplicate local", 1 "duplicate func",
2 wrong results, 2 malformed imports) and `call_indirect.wast` (7 "unexpected token"). Duplicate
**identifiers** are a distinct rule and were not touched.

### 2026-08-08 (sixth) — T9h ✅ the `Store` type registry. **`type-subtyping.wast` reaches 72/0/0**

+14 passes, −18 failures, no file lost a pass. `type-subtyping` **62/13 → 72/0/0** (from 36/44 that
morning), `type-rec` 7/9 → **11/5**, `type-equivalence` 10/2 → **10/1**. **Every `Unlinkable: module linked`
and `an import does not match` in the suite is gone.** Cold start within noise (~4.66 vs ~4.63 ms).

`Store` holds a `TypeRegistry` interning rec groups as each module joins; a group's key references outside
targets by their **store-wide** id, so interning is content-addressed and two modules spelling the same
group land on one id. Matching is an integer comparison at **link** time, never on a hot path. **wasmtime's
SHAPE, our code** — the third application of that standing rule, not a new policy (🔒 entry in
[design-decisions.md](design-decisions.md)); **the Component Ledger stays empty** and wasmrt remains 100%
original Rust.

⚠️ **The finding the plan did not anticipate: comparing SIGNATURES can never answer an IDENTITY question.**
The registry alone moved only **2** of the ~11, because two functions can both be the empty `(func)` and
still be *different types* — rec-group membership is part of identity, and params/results cannot express it.
Only the type **index** carries it, and the decoder had been resolving an import's typeidx to a `FuncType`
and **throwing the index away**. Same shape as the emitter defects T10a is about: the information was
present and discarded because nothing needed it yet.

Two more: **§4.5.9 matching is subtyping, not equality** (equality refused 3 valid modules, so the registry
records supertypes store-wide), and 🆕 **`call_indirect` was a THIRD site with the identical defect** —
comparing signatures rather than type identity — worth 7 runtime assertions.


### 2026-08-08 (fifth) — type canonicalisation: types were compared by index, the spec compares by structure

**+12 passes, −24 failures. Six files improved, none regressed** — `type-subtyping` 57/23 → **62/13**,
`type-equivalence` 7/10/3 → **10/2/0**, `type-rec` 7/11 → **7/9**, `ref_cast` and `ref_test` to **zero
failures**, `br_on_cast_fail` 13/3 → 15/1. Cold start unmoved (~4.59 vs ~4.69 ms).

Rec groups are the unit of identity (§3.1.4). `Module` now carries **`type_canon`** — the lowest type index
structurally equal to each type — built at decode by reducing every group to a structural key where a
reference to a *member of the same group* becomes its **position** and one *outside* becomes the target's
canonical id. `Module::is_subtype` compares those ids, and because **every** subtype question funnels through
it (validator, declared-subtype check, `ref.test`/`ref.cast`), one line carried the fix.
`call_indirect` was a **second site**, comparing signatures by raw bits; now `func_types_equal`, which tries
the slice compare first so the hot path is untouched.

🆕 **The assembler was flattening every `(rec …)` group — `0x4e` was never emitted.** Since the group *is* the
unit of identity, that silently changed what the types were, and it is why canonicalisation initially
*regressed* `type-rec.wast`. **Third "the assembler emits a different module than the text describes" defect
in two passes** (after `(sub …)` marking open types final, and element-segment form 4 at T8) — the emitter
should get a dedicated look at T10 rather than waiting for a fourth.

✅ **It let the previous pass's `decl_subtype_of` approximation be deleted**, with byte-identical suite
results, and its test was rewritten from "cannot be told apart" to "are the same type" plus the converse.
⚠️ **A measurement-tooling finding:** the per-file line prints only when `verbose || failed > 0`, so a file
reaching **zero failures vanishes** from a non-verbose run and a line-keyed diff reads it as total loss. It
raised exactly that false alarm here. Always diff with `-v` on both sides.

📋 **T10a added to the task list 2026-08-08 — the EMITTER audit.** The `(rec …)` flattening was the **third**
"the assembler emits a different module than the text describes" defect in two passes, and **all three were
found by accident** — by some *other* check happening to start reading a field the emitter had dropped. T10a
names the mechanism (**the emitter reconstructs a binary form from a SUBSET of the facts the parser saw, and
the dropped fact turns out to be semantic**) and specifies two mechanical checks rather than another
read-through: a **round-trip property test** (assemble → decode → compare against what the *parser* recorded)
and a **field-coverage sweep** over every `ModuleBuild` field. The reason nothing caught these: every
existing test asserts the module *runs*, and all three defects produced modules that ran.

### 2026-08-08 (fourth) — declared subtyping was never validated (21 invalid modules accepted)

`type-subtyping.wast` **36/44/0 → 57/23/0** — +21 passes, −21 failures, nothing else in the suite moved.
⚠️ **The third time a logged cause proved wrong.** T9a#6 read "GC subtyping depth not modelled"; the
measured top item was **21 invalid modules being accepted**, because there was **no declared-subtype
validation at all** — `module.supertypes` was filled in at decode and thereafter only *walked* by
`is_subtype`, which trusts it. Now enforced: **finality** (a type is final unless declared `(sub …)`; the
decoder had been reading `0x50` and `0x4f` **identically and discarding the distinction**) and
**structural matching** (§3.4.5 — kind, func contravariance/covariance, struct append-only, field
mutability and packing).

🆕 **The assembler was silently turning open types into final ones** — it chose the wrapper from the
presence of a supertype alone, so `(sub (struct …))` with none emitted a *bare* composite type, which
means `final`. The module produced was not the module the text described; same class as element-segment
form 4 at T8. Found only because the new finality check started reading the flag.

**A strict version was measured and rejected:** refusing every undecidable pair turned away **6 valid**
modules, so `decl_subtype_of` accepts the undecidable case — the *opposite* of the call made for
cross-store import matching one pass earlier, because there accepting means a wrong call while here it
only preserves existing behaviour. ⚠️ **The direction to err in is a property of the consequence, not a
house style.**

**🔧 This measurement names the next item: no type canonicalisation** — wasmrt compares concrete types by
**index**, the spec by **structure**, so structurally identical rec groups are two types. **~40 assertions**
across `type-subtyping`/`type-rec`/`type-equivalence`, and it **supersedes the earlier "residual: 1
assertion"** estimate. Largest in-scope cluster left; a feature, not a patch.

### 2026-08-08 (third) — an `InstanceId` from another store reached THIS store's memory

**Found from a constraint the owner stated, not from a test**: *"the memory needs to be shared once pulled
in; having two memory profiles that are trying to pull from each other does not work."* Probing the three
properties that implies found two holding and one broken — in the imported-memory work committed hours
earlier. **`InstanceId` was a bare `usize` with no record of its issuing store**, so an id from store X
indexed store Y's own instance vector; index 0 is always in range, so the import linked and the guest
silently shared **Y's** memory (measured: read `0x99`, its own, instead of X's `0x11`). It was also a
**panic** — `module_of`/`export_func` indexed `code[id]` directly, which under `panic = "abort"` is a
process kill (T12a's concern, arriving early).

Fixed by tagging: `InstanceId { store, index }`, every accessor through one `Store::slot()`. **This is the
defence the C ABI already applied to its value handles at T8** — core held the weaker guarantee of the two,
and that asymmetry *was* the bug. Mutation-verified; conformance **unchanged** (61,712/578/2,469), because
this is a misuse path the spec suite cannot reach, which is why it survived three passes of it. The same
hole existed for `with_instance_func` since **T7b** and closes with it. **363 core tests.**

The two properties that did hold are now pinned rather than assumed: **sharing survives a re-export chain**
(A → B → C reaches A's bytes), and **the linking graph is a DAG by construction** — an `InstanceId` exists
only once its instance does, so a cycle is unrepresentable rather than rejected.

⚠️ **Lesson: a stated constraint is worth probing, not agreeing with.** Two of three properties held; the
third had shipped broken that morning, and no spec-suite assertion could have found it.

### 2026-08-08 (second) — decoder strictness: the decoder now rejects malformed binaries itself

**+98 passes, −98 failures, skips UNCHANGED** — no accounting movement at all; 98 assertions simply
started passing. `binary.wast` **128/88 → 208/8**, **`binary-leb128.wast` → 58/0/0 (a file at 100%)**,
`.wat` corpus 532/534 → **533/534**.

This closed **T9a#11 and most of #12**, and showed they were never two items: reading the actual failures
rather than the logged descriptions revealed one theme in two halves — *rejected at the wrong stage* and
*module was accepted*, the second being over-acceptance of hostile input and therefore T12 material.
Seven checks at decode: **section order + uniqueness** (16 assertions — and a repeated section had been
**silently replacing** the first, so the module that ran was not the module on disk), section size (7),
func/code count (8), **bodies decoded at decode time** (the IR now lives in `Code`, *replacing* the raw
bytes), const-expr encodings, the `end` terminator, and the 2^32−1 locals ceiling.

⚠️ **Section order is not id order** — `DataCount` is id 12 but precedes `Code` (10), `Tag` is 13 but sits
between `Memory` and `Global`. `SectionId::order()` is a table for that reason; comparing raw ids gets
both backwards. **Two findings:** four of our own test fixtures were **malformed modules** all along
(export after code; a function section with no code section) — a permissive decoder lets bad fixtures
accumulate and they then encode the wrong rule; and the const-expr sweep must key on a segment's **mode**,
not on whether its offset bytes are empty, because a *passive* segment has none while an *active* one
with none is malformed. **Cost: cold start unchanged within noise** (~4.5 vs ~4.4 ms at 48 KB, inside the
recorded spread) once the redundant `Code::body` copy was removed — keeping both it and the IR measured
~5% slower.

### 2026-08-08 (first) — T9a#4: imported **memories** ship; **tables** stay refused (owner chose option 2)

**+346 passes, +42 failures, −463 skips.** `imports.wast` **25/6/108 → 196/13/95** (+171, the largest
single-file gain since the `register` work), `linking.wast` **55/15/78 → 107/11/28** (failures down as
well), `linking3.wast` 4/4/4 → 8/2/2. The memory half needed **no value-model change** — a memory is bytes
indexed through `IndexMaps` — so it is finished work, not scaffolding for tables. `Imports` holds
`(instance, that instance's memory index)`, never a store slot; `Linker::define_memory` publishes one by
name; §4.5.9 limits matching compares **declared types**, so `Memory` now carries its declared `min` (a
`memory.grow` in the exporter must not change what links).

**The finding that outranks the numbers: `assert_unlinkable` had been an unconditional skip, and while it
sat there it insured a real defect in the engine — imports were NEVER type-checked at link time.** A module
importing `(func (param i32))` against a `(func)` linked and then ran, caller and callee disagreeing about
the stack: the silent-wrong-output class. Now checked where the type is actually known — functions in
`Store::instantiate` (so a hand-built `Imports` is checked too; a `HostFunc` has no declared signature and
is still trusted), globals in `Linker::resolve` (an `Imports` global is a bare `Value`, which cannot say
`i32` from `f32`, let alone mutable from not). ⚠️ **A blanket skip is not a neutral placeholder** — this one
was justified by a reason obsolete since T7b. Any category the runner declines wholesale should record what
would be measured if it stopped declining. The equality-vs-subtyping choice for function matching was
**measured both ways** (exempting concrete GC types costs 3 correct refusals to recover 1 false one), so
equality stays and errs toward refusing — residual 1 assertion, logged.

**Still gated:** the **table** half. A `funcref` carries no instance identity. ⚠️ And the obvious packing
is wrong — *instance in bits 32–63* collides with `I31_TAG = 1<<63`; it must fit under bit 63.

### Previous — 2026-08-07, T9 first pass

**Done:** T9a #1/#2/#3 · **T9b (size)** · **T9c (performance)** · **T9d (licensing/docs)**.
**Both previously-unmeasured axes now have numbers** — see [vision.md](vision.md): cold start
**4.48 ms** at 48 KB and **~237 Mops/s** steady; CLI **621 KiB**, cdylib **493.5 KiB**, freestanding
`wasm32` engine **158.1 KiB** (**137.5 KiB** after `wasm-opt -Oz`). That unblocks **T11**, which could
not start without baselines.

**Two findings that outrank the numbers:**

1. **T9a#1's stated cause was wrong.** `ref.null $t` was a real defect and is fixed — but it moved
   `ref_null`/`ref_test`/`ref_is_null`, **not** `br_table.wast`, which was failing three fixes earlier.
   Its 161 skipped assertions took **four** fixes, three of them unlisted: an inline table-elem shorthand
   that could not express a non-`funcref` element type, a **block type of concrete reference type being
   undecodable**, and **`br_table` label typing wrong in two directions**. `br_table.wast` → **185/0/0**.
   ⚠️ **Every remaining T9a cost figure was assigned the same way — by reading a file for the first
   construct that looked unsupported. They are hypotheses; re-measure after each fix.**
2. **T9a#4 (imported memories/tables) is a DECISION-GATE, not plumbing.** A `funcref` is a bare function
   index with **no instance identity**, and `call_indirect` resolves it against the *calling* instance —
   so a shared table dispatches to the wrong function. Imported *memories* are genuinely plumbing;
   imported *tables* need the funcref encoding decided first, and that touches a recorded invariant.
   **Do not implement imported tables without it.** Options are laid out in [known-issues.md](known-issues.md).

**Still open in T9:** #12's **text-parser** remainder (`func.wast` 8, in `wat.rs` not `module.rs`) · **T9e `pin`** (still a stub — a build performs **no** authenticity check) ·
**T9f tail calls** (1.0 cannot be claimed without them). #4, #5, #6, #7 and #11 are **done**; the
decision-gate above is **resolved** — the funcref carries its owning instance and imported tables ship.

**Previous (2026-08-06): T0–T8 DONE; ✅ PUBLISHED through v0.9.0** (release commit `a7abd83`, tag
`v0.9.0`). T8 shipped the
**`wasmrt.h` C ABI** — ~74 functions, wasmtime-*shaped* under our own names — so wasmrt is now embeddable
from C. Its three gates are green: `abi_symbols.c` (74/74 symbols, link completeness), `c_smoke.c`
(compiled by a C compiler against the shipped header, proving it is valid C and matches the library), and
**Miri** over a randomized lifecycle fuzz. The wasm-c-api refcount object model is **designed out**:
value handles carry the identity of the store that issued them, so a stale or foreign handle is rejected
rather than followed — mutation-verified. Also landed: **proposal gating** (14 flags, all on by default,
enforced at *validation*; deliberately **no tail-call flag**, since that proposal is unimplemented and a
toggle would gate nothing), **configurable resource ceilings**, and a **`Linker` in core** shared by the
C ABI / native crate / WASI / `.wast` runner. That linker work surfaced and fixed **two
silent-wrong-output defects** — dropped table initializer expressions, and element-segment form 4
silently rewriting a segment's type. Suite **61,033 / 738 / 3,075 — 98.8%**; **351 workspace tests**.
**Next: T9 (0.10.0) — now scoped from a measured audit (`roadmap.md`), not from old notes.** Its top item
is a **one-match-arm** assembler fix (`ref.null $ConcreteType`) worth **161 skipped assertions in
`br_table.wast` alone**, and the audit turned up **2 new open defects** — a `memory.size` that reads
another instance's memory, and a `.wast` runner that redirects a failed module's assertions elsewhere.
T9 also carries **tail calls**, the last unimplemented in-scope proposal (1.0 = parity cannot be claimed
without them).

**Then three more tasks stand before 1.0** (owner, 2026-08-06): **T10 (0.11.0) — bug hunt + code
hygiene** (the "look for code issues" trigger below, run as a scheduled task: bugs, fall-throughs, stale
workarounds, dead code, missing docs); **T11 (0.12.0) — optimization review**; and **T12 (0.13.0) —
security review**, an adversarial look at the penetration surfaces. **T11 and T12 both end in a
decision, not a diff** — findings and recommended changes with their costs, several of which trade
against recorded invariants, and those trades are the owner's.

The order **measure (T9) → find (T10) → optimize (T11) → attack (T12)** is deliberate: optimizing before
the bug hunt tunes code that may be about to change; reviewing performance before T9 produces a baseline
yields opinions rather than deltas; and a security review is only worth anything if it examines the
*final* code — an optimization can itself introduce a surface. **T12's first finding is already
written down:** `[profile.release]` sets `panic = "abort"`, so **any panic reachable from hostile input
kills the embedder's process** — and release builds carry no `overflow-checks`, so arithmetic wraps
there and panics in debug.

**Previous (2026-08-05): T0–T7 DONE; published through v0.8.0** (release commit `81d1c24`, tag `v0.8.0`).
v0.7.0 shipped the **text toolchain** (T6) **and the validator's deferred typing arms** (completing T4).
Since then **T7 landed in full**: host imports, module linking on a **shared store** (wasmtime-style),
and **WASI preview 1 including the sandboxed filesystem**. The suite is at **98.8%** (61,013 passed / 751 failed / 3,094 skipped). **Known-issues review DONE (2026-08-05)**, and with it the safety pass (`#![forbid(unsafe_code)]` in core
and the CLI, `deny` + one justified `allow` in capi) and the literal/text edges (all 284 suite files now
parse). The resolver TOCTOU residual is **decided: accept + document** (`security-model.md`). **Next: T8 —
the `wasmrt.h` C ABI**, whose decision-gate is finalizing the header shape with the owner.

`wasmrt-core` has `types` + `reader` + `opcode` (the shared IR + `decode_body` + the text-name reverse
map) + `module` (decode) + `validate` (spec §3 type-checker — **complete**, incl. SIMD/atomics/GC/EH) +
`interp` (switch interpreter: integer + float compute, linear memory incl. multi-memory and memory64,
tables/`call_indirect`/reference types, WasmGC, the full `v128` SIMD set incl. relaxed, threads/atomics
single-threaded, and exception handling in both encodings) + the text toolchain `sexpr` / `wat` / `wast`.
**`wasi` is complete for preview 1** (`wasi/mod.rs` = process surface, `wasi/fs.rs` = fd table + rights
lattice + the sandbox resolver); **`features` (proposal gating) and `linker` (name-based import
resolution) were added at T8**; `pin` remains a stub (now **T9**). **`wasmrt-capi` is real as of T8** —
~74 exported functions, `include/wasmrt.h`, `src/ffi.rs` as its only raw-pointer boundary.
**CLI: `wasmrt <file>` summarizes +
validates; `run` executes an export; `wasi` runs a preview-1 program with `--dir`/`--ro-dir` preopens
(no `--dir` ⇒ every path call is `BADF`); `wat` assembles `.wat`; `wast` runs spec scripts.**
The interp value slot is 128-bit
(`Value = u128`) so a `v128` is one slot; a memory carries its own index type (`i64` addresses on a
64-bit memory) while **tables stay 32-bit**; `delegate` is rejected everywhere (oracle-faithful).
**351 workspace tests** green, clippy clean, all four build surfaces. Each task ships a crates.io
release ([releasing.md](releasing.md)) + a flip on the public `ROADMAP.md` matrix — with the full `cmem/`
sync committed **before** the publish handoff (owner directive, 2026-07-31; see `releasing.md`).

## Policy (durable — mirrors the wazmrt owner policy, adopted 2026-07-17)

- **`cmem/` is the single home for ALL project memory.** When the owner (or anyone) says "**update the
  project memory**," fold the latest decisions, found bugs, design changes, and current state into the
  matching `cmem/` topic file(s) — then refresh its one-line pointer in the Files table below. Convert
  relative dates to absolute; update existing entries rather than duplicating.
- **`README.md` is NOT project memory.** It is the public, user-facing document. Keep internal decision
  logs and post-mortems here.
- **`docs/port/` holds the detailed wazmrt deep-read maps** (engineering reference for the port), not
  memory. `cmem/` records the decisions and current state and points into `docs/port/` for depth.

### The "update the project memory" trigger (binding on every agent)

When the owner says **"update the project memory"** (or a synonym — "update memory", "record this",
"remember this for the project"), do BOTH:

1. **Revise all relevant `cmem/` files** — fold in the latest decisions/state; refresh the Files-table
   pointer; convert relative dates to absolute; update existing entries instead of duplicating.
2. **Sync `README.md` only where the change is user-relevant** (status, eventual install/usage/API).
3. **Commit and push** — the repo is synced to `origin` (github.com/jrmarcum/wasmrt); keep the portable
   memory current on GitHub.

### The "evaluate a reference project" trigger (binding on every agent)

Before incorporating or adapting code from any reference runtime (see [reference-projects.md](reference-projects.md)),
complete an Adoption Checklist (benefit-vs-drawback + license compliance) in `third_party/LICENSES.md`,
add a Component Ledger entry, and update `reference-projects.md`. "Looking at" a project is free;
"copying/porting from" it always requires the ledger entry. **wasmrt is intended to be 100% original
Rust** (the wazmrt port is the owner's own code, not a third-party adoption).

### The "look for code issues" trigger (binding on every agent — applies once code exists)

When the owner says **"look for code issues"** (or "code audit", "hunt for bugs"), perform a
**COMPREHENSIVE audit across BOTH tested AND untested paths** — catch what won't surface in today's
tests but will bite a future change. Four categories: (1) stale workarounds/`TODO`/"for now"; (2) dead
code (grep-verify before calling it dead — a symbol may be reached via the C ABI or a re-export);
(3) bugs (inverted logic, LEB off-by-ones, wrong-tagged enum access, stack-order, missing bounds
checks); (4) **fall-throughs (worst)** — unhandled input that emits a stub/placeholder instead of
erroring; prefer a hard `Err(Unsupported*)` over silent-wrong. **Method:** fan out parallel read-only
investigators per category, consolidate, report `file:line` + one-line + severity, fix the safe ones,
and **keep the suite green — diff the OUTPUT (N passed / N failed), not exit codes** (`cargo test` +
`cargo clippy` + the affected spec/parity vectors vs the pre-change baseline).

## Files

| File | What it holds |
| --- | --- |
| [best-practices.md](best-practices.md) | 🎓 **The lessons this port paid for, each with the defect that bought it** (2026-08-08). **Read it before starting a task.** Measurement (**a logged cost is a hypothesis about a CAUSE — wrong 4×**; ⚠️ **a SCOPE note is one too, and nobody re-measures it** — "annotations 51, a proposal we do not target" was true of the file and false of 44 of its 51 assertions, which were generic lexer rules;; survey the measurement, not the entry; know which direction your tooling can lie in; same-session A/B/A; ⚠️ **the PLUMBING for a cold feature can cost the hot path** — trap backtraces are diagnostics, but getting `pc` out of `run` the obvious way measured 3.6% slower, so benchmark anything touching a hot loop's locals, and pin any size/layout claim with a `size_of` test). What to distrust (**a stated constraint is worth PROBING, not agreeing with**; a design doc is a hypothesis; an enum's numeric order is not the wire order). Where the defects are (**silent-wrong-output is the worst class and where every serious defect has been**; ⚠️ **a feature can be fully decoded, validated and PRINTED and still never execute** — the start function never ran for five releases while `start.wast` failed, so check that the feature a file is NAMED for works before diagnosing its messages; **the emitter reconstructs a form from a SUBSET of the parser's facts** — 3×; **comparing STRUCTURE cannot answer a question about IDENTITY**; an asymmetry between layers is a bug; the two-instance rule; **rejection STAGE is part of correctness**). Checks and gates (**a gate that cannot fail is decoration**; mutation-verify; **a guard one call-level away from the iteration it guards is NO check**; **a blanket skip insures the code it covers**; **which direction to err in is a property of the CONSEQUENCE, not a house style**; loud `Unsupported` over silent-wrong; a library must never abort the embedder). Tests (**assert where a check STOPS working**; every test asserted the module *runs*, so defects that ran were invisible; our own fixtures were malformed). Untrusted input (O(n log n) not O(n²); decode must be total; say what you did not do). The 🔒 **wasmtime shape-not-code** rule and its compliance test. Working method. |
| [overview.md](overview.md) | What wasmrt is, **why the oracle was retired (2026-08-11)**, repo/crate layout, mental model, and the **stage-by-stage status through T9's eleven passes** — 99.3%, 426 tests, and the **eight defects no list had**, not one of which was found by reading the punch-list |
| [vision.md](vision.md) | 🔒 **WHAT wasmrt IS COMPETING FOR (2026-08-11): inclusion in wasmtk + universalWasmLoader-*, decided on the smallest and fastest binary; rsxtk takes it by default via the native Rust interface. TWO ARTIFACTS with different levers — the rlib rsxtk links (never measured) and the cdylib the C consumers link (493.5 KiB). FAST AND SMALL ARE NOW THE GATE, not aspirations with a footnote.** The goal — **plus where the three axes actually stand, now that ALL THREE are measured (2026-08-07)**: *canonical* 98.9%; *fast* **4.48 ms cold at 48 KB / ~237 Mops/s steady**; *small* **CLI 621 KiB, cdylib 493.5 KiB, freestanding wasm32 engine 158.1 KiB → 137.5 KiB with `wasm-opt -Oz`**. What is still missing on fast/small is the **comparison to another runtime** (wasm3/WAMR/Deno), which needs those binaries present. Still: do not quote wazmrt's benchmarks as wasmrt's. fast + smallest-binary + wasm-compilable runtime that **replaces wasmtime** under the `universalWasmLoader-*` projects |
| [architecture.md](architecture.md) | 🔒 **The call path: recursion for calls, a LOOP for tail calls (T9f)** — `run` reports a tail target through an out-parameter and unwinds; `call_function` loops at the SAME depth, so an unbounded chain uses constant native stack. **Do not "simplify" it back into a recursive call** — that spelling shipped for releases, gave correct answers, and was not a tail call. Crate/module map — incl. **`validate::last_failure_site()`** (function index + absolute module offset + expected-vs-found, in a thread-local rather than widened into the `Copy`, C-ABI-crossing `ValidateError`; it is what makes the CLI diagnostic match wasmtime byte-for-byte) and ⚠️ **`Instance::new` takes an UNVALIDATED `Module` by design** (wasmtime-style compile/instantiate split; every shipped entry point validates first). Plus: Rust architecture — **the T8 C ABI** (two handle kinds: opaque pointers you own vs. **checked value handles** that carry their issuing store, so a foreign/stale one is refused not followed; `ffi.rs` as the crate's ONLY raw-pointer boundary; the env finalizer behind an `Rc` so teardown order cannot matter); `wasmrt-core` (no_std-friendly) + `wasmrt-capi` (cdylib/staticlib, the `wasmrt.h` surface) + `wasmrt` CLI; decode→validate→instantiate→execute; the dual-target contract; the shared opcode IR seam; **the T7 shared store** (`code` + `pools` as separate fields so cross-instance calls borrow disjointly — no `Rc`/`RefCell`/`unsafe`) |
| [design-decisions.md](design-decisions.md) | 🔒 **THE ORACLE IS RETIRED (owner, 2026-08-11) — wasmrt and wazmrt are now independent entrants for inclusion in wasmtk + universalWasmLoader-*, so following the oracle would mean adopting a COMPETITOR's design; `check-wazmrt.sh` deleted, provenance kept. Correctness anchors externally, which was always the harder test.** Also 🔒 **one helper for every module-loading CLI path** (`.wat` assembled before decode; dispatch on EXTENSION so the stage blamed stays honest). ✅ **RESOLVED 2026-08-10 — an invalid module is REFUSED, with a wasmtime-shaped diagnostic**, decided the owner's way: by MEASURING wasmtime (47.0.2), not arguing its API. Byte offset **identical** to wasmtime's on the same file (verified at 33 and 61, pinned as tests) + expected/found + the function index wasmtime omits; `Instance::new` keeps the compile/instantiate split. ⚠️ **Method rule: when a decision rests on a reference implementation, RUN it** — three lines through the real binary beat any recollection of its docs. Plus: Load-bearing decisions + invariants NOT to drift — the 🔒 **"wasmtime's SHAPE, our code" rule (reaffirmed by the owner 2026-08-08)**: adopt the architecture, write our own implementation, no code/symbols/headers/structures transcribed. **Three applications** — the `wasmrt.h` surface (T8), the shared store (T7b), the **engine-level type registry** (T9h). **The Component Ledger stays empty** and wasmrt remains 100% original Rust, because the "evaluate a reference project" trigger requires an entry for *copying* code and reading an architecture is free. The compliance test: each borrow visibly **diverges** where wasmrt's constraints differ (checked value handles vs refcounted objects; `code`/`pools` split for disjoint borrows; zero deps; `forbid(unsafe_code)`) — a design that cannot be re-derived under those constraints was being copied, not understood. Also the **4 T8 header answers (owner, 2026-08-06)**: real proposal gating (not limits-only), the linker in **core**, raw memory pointer + checked copies, **trap-frame shape now / backtraces at T9 — ✅ VINDICATED: real frames landed at T9a#7 with NO ABI change, so freezing a knowable SHAPE while the DATA is still missing costs nothing and buys a non-breaking fill-in** — and why there is **no tail-call flag**; the 🔒 **safety directive** (no unsafe constructs migrated from Zig; prove the concept BEFORE hardening; **ENFORCED at v0.8.0**: `#![forbid(unsafe_code)]` in core + CLI, `deny` + one justified `allow` in capi, mutation-verified); boundary-faithful/idiomatic-Rust; **public API = own `wasmrt.h`** (clean `wasmrt_*` + native Rust crate, NOT wasm-c-api/wasmtime symbols); **feature scope = full wasmtime browser-standard parity + memory64, WASI p1 only**; Option-A interpreter; the ValType/slot/opcode/trap invariants; size levers; the collapsed oracle split (tail-calls only); **all 4 deferred decisions are now RESOLVED** (crate split T0, `random_get` + resolver T7, header shape T8) — the gate queue is empty |
| [loaders.md](loaders.md) | 🆕 **TWO consumer surfaces now (2026-08-11): `rsxtk` links the wasmrt-core RLIB natively (no C ABI, no FFI) and takes wasmrt BY DEFAULT; wasmtk + universalWasmLoader-* link the cdylib via wasmrt.h, where wasmrt is an ENTRANT, not a given.** 🎯 **rsxtk READ, not assumed** (v0.4.4, one 463-line main.rs, on wasmtime 40.0.1 + wasmtime-wasi 40.0.1) — the engine surface it uses is NARROW and wasmrt covers all of it; `wasmprinter`/`walrus` are TOOLKIT deps, **not blockers and not wasmrt scope**; `component-model` is enabled in its Cargo.toml and **used nowhere**, so it only LOOKS like a requirement wasmrt lacks. 🔒 **`.cwasm` will NOT be the default (owner) — plain `.wasm`, for cross-platform compatibility**: a .cwasm is target- AND version-bound machine code reloaded through `unsafe deserialize_file`; an interpreter has no AOT artifact to attack, and with the cache off wasmtime pays compile cost per run — **a measurable T11 experiment, NOT a README claim yet.** 🎓 **The record that falsified wazmrt's wasm-c-api payoff (2026-08-10).** The loaders are the layer meant to standardise imports across languages, so the runtime only ever had to serve THEM — and the C loader uses wasmtime's **`wasmtime_*` store/context/linker/typed-val model, NOT the wasm-c-api instance/func model**. wazmrt implemented wasm-c-api on the premise that "ports already on wasmtime's C API are close to drop-in"; nobody had read the loader's header. Worse, the mismatch is **structural** — wasm-c-api's host callback gets **no handle to the caller's memory**, the one thing every loader host import needs ("the load-bearing gap over wazmrt's shape"), which is why `wasmrt.h` adopts wasmtime's **caller-based** model. Plus: **The consumers.** How `universalWasmLoader-*` work (hand-rolled Canonical ABI over core modules + WIT sidecar), the ~38-fn engine surface that drives `wasmrt.h`, the caller-based host-callback requirement, the 3 substrates + 10 targets (phased), and **what the shipped `wasmrt.h` actually gives them** (T8 / v0.9.0) — including the one caveat to carry into the loader ports: `wasmrt_caller_get_memory` always returns false, so callbacks use `wasmrt_caller_read`/`_write`. The v0 draft is **historical**; four of its shapes never matched the code |
| [testing.md](testing.md) | 🆕 **`tests/*.wast` now runs under `cargo test`** (`regression_wast.rs`, zero failures AND zero skips, fails if empty) — the three reproducers were green and NOTHING RAN THEM. ⚠️⚠️ **`tail_call_is_a_real_tail_call.wast` is the test the spec suite cannot replace**: with the fake `return_call_ref` restored by mutation it fails 5/6 while the tail-call spec files still read 38/6, 72/4, 40/6. 🔒 **THE ORACLE IS RETIRED (2026-08-11) — conformance now anchors on the spec testsuite, wasmtime's observable behaviour, the wasmtk WASI corpus and wasmrt's own 456 tests, in that order of authority; two T9 punch-list items were FALSE LEADS produced by the old parity arrangement.** Previously the parity/oracle split — **live conformance 99.4% (62,113 / 385 / 2,163 of 62,498) and 438 tests as of 2026-08-08; the `.wat` corpus assembles 534/534 with 0 decode failures — and why that gate must run assemble→decode→validate, not `wat -o` alone**; the decoder-strictness column (+98 passes, −98 failures, **skips unchanged**: no accounting movement, 98 assertions simply started passing) and why a test that pins where a check STOPS working earns its place; the 08-08 column (+346 passes / −463 skips, **no file losing a pass**) and why **a blanket skip is not a neutral placeholder** — `assert_unlinkable` was skipped for a reason obsolete since T7b and was insuring a real engine defect; the runner now separates *"nothing defines this import"* (a real unlinkable verdict) from *"wasmrt cannot back this kind"* (a gap → skip), which is exactly what had made the assertion unimplementable; the 08-07 column split into *real capability* vs *honest re-accounting* (the runner's failed-build redirect was inflating failures); the **T9c benchmark** and why it asserts its own result before timing; the methodological finding that **a cost logged beside a defect is a hypothesis about its cause**; **the three C-ABI gates and what each catches that the others cannot** (link completeness / header-is-valid-C / Miri sees a UAF a normal allocator cannot); the mutation check on handle tagging; **351 tests**; Rust↔wazmrt golden vectors for shared features; **the vendored spec testsuite + its live conformance numbers (98.8% — 61,033/738/3,075 at v0.9.0; all 284 files parse) and where it lives**; **why the total adjudicated count moves when modules start building** (a module that fails to build costs exactly ONE skip, not one per assertion); `wasmrt-capi` under Miri + a lifecycle fuzz; a wasi-gate compiling real guests; bench cold vs steady; DoD = full parity on both targets |
| [security-model.md](security-model.md) | 🔒 **THIS FILE IS NOW AUTHORITATIVE (2026-08-11)** — it used to defer to wazmrt's copy; that pointer is retired with the oracle, and the file itself records WHY inheriting was wrong: wazmrt had **no `path_symlink` right at all** while wasmrt gated creation correctly, so faithful replication would have replicated the hole. ⚠️ **`pin` is a STUB — Authenticity is NOT implemented.** 🔒 **Symlink CREATION denied by default (owner, 2026-08-10)** — `--dir` grants `ALL & !PATH_SYMLINK`, `--allow-symlink` opts in for installer work, `--ro-dir` denied it already; enforced by `const` assertions so a violation fails the BUILD, and mutation-verified. ⚠️ **Governs CREATION, not traversal** (following a pre-existing link needs `PATH_OPEN`, which both grants keep — also asserted). Composing modules over shared memory is the STORE's job, so a workload run never needs new links; denying creation shrinks what an external racer can repoint, which is what makes the accepted TOCTOU residual survivable. ⚠️ The oracle had **no `path_symlink` right at all** and wasmrt did not — see roadmap **T12x**. Also 🔒 **Validation is a SECURITY boundary and every entry point must cross it (2026-08-10)** — `wasmrt run` did not, so an ill-typed module executed and printed a plausible answer while `wasmrt wasi` refused the same bytes; the oracle was worse (**both** execute paths + `wasm_module_new`). Carries the entry-point table, why the Rust `Instance::new` split is the deliberate exception, and ⚠️ **the same missing check is a different SEVERITY in the two languages** — `forbid(unsafe_code)` bounds wasmrt to a wrong answer, while Zig ReleaseFast reached a segfault from a 31-byte module. Plus: 🔎 **A dedicated adversarial review is scheduled — T12 (0.13.0)**, framed on three adversaries (hostile guest / malformed input / misusing embedder); two surfaces already listed from the release profile alone: `panic = "abort"` makes any reachable panic a **host-process kill**, and no `overflow-checks` in release means arithmetic **wraps there and panics in debug**. ⚠️ **`pin` is a STUB in wasmrt — a build today performs NO authenticity check**; that section describes the ORACLE, not current behaviour (T9). The *authority* half (the WASI sandbox) IS built. **Two new authority controls at T8:** proposal gating + resource ceilings, both reachable from C. Sandbox **secure by construction** + ⚠️ the **CORRECTED resolver note (2026-08-05)**: Rust's `std` has **no** dir-relative open, so zero-dep + no-`unsafe` + hold-real-handles cannot all hold. Shipped a component-accumulated path — every **escape** property intact, **inode pinning** lost, `verify_beneath` compensating; closing the residual was **DECIDED 2026-08-05: accept + document** (zero-dep and zero-`unsafe` both held). Also: rights-narrowing preopens; the mandated canary test (mutation-verified); pin verify (hash the in-memory bytes you run; root-owned DB; enforce denies before opt-out); authenticity vs authority |
| [licensing.md](licensing.md) | 🔒 **Provenance SURVIVES the oracle's retirement (2026-08-11) — nothing here changes.** Same owner, same dual MIT/Apache, so no third-party obligation; the wazmrt attribution stays anyway because it is accurate. **Retiring a gate is not rewriting where the work came from.** Component Ledger stays EMPTY. **Verified 2026-08-06: all four license files exist since T0, the Component Ledger is EMPTY and there are ZERO third-party dependencies.** The `cap-std` question is **CLOSED** (rejected 2026-08-05), not open. Where SPDX actually is, per file — **the `README.md` gap was closed 2026-08-07 (T9d), so the convention is now fully satisfied with no remaining gaps.** **License = `MIT OR Apache-2.0`** (dual). Carry `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/`third_party/LICENSES.md` verbatim, name wazmrt→wasmrt, "Jon Marcum" 2026. Vendored-`wasm.h` attribution only if that code is reused (it is NOT — we ship our own `wasmrt.h`) |
| [reference-projects.md](reference-projects.md) | 🔒 **wazmrt is a COMPETITOR now, not the oracle (2026-08-11)** — it moves into the benchmark column beside wasm3/WAMR. **Benchmark it; do NOT follow it** (it is running its own size program, so its head reflects ITS constraints). ⬜ None of the four comparisons has ever been run on this machine. **wasmtime SHAPED `wasmrt.h` — shape only, no code/symbols/headers taken**; the wasm-c-api `wasm.h` was NOT vendored even at the C ABI, the moment that shortcut was most tempting. The runtimes evaluated (same set as wazmrt) + **wasmtime is the feature-parity target** ("run what it runs") and the thing being replaced under the loaders. 100% original |
| [roadmap.md](roadmap.md) | 🔒 **ANCHOR CHANGE 2026-08-11 — the plan keeps its shape (finish T9 -> T10 -> T11 -> T12 -> 1.0) but T11 now DECIDES THE CONTEST rather than polishing it, carrying three promoted footnotes: the unattributed ~5%% steady regression, the NEVER-MEASURED rlib that rsxtk actually links, and no same-machine comparison to any competitor.** 🚦 **T12y + T12z added 2026-08-10.** T12z = the security-review METHOD: name an invariant, enumerate EVERY entry point in a table, check each — because both defects this week were "three of the four do X", never "nobody does X". T12y = ⚠️ **the oracle's sandbox-escape tests DO NOT RUN on this host** (the 4 in "489/493, 4 skipped" are the symlink traversal + resolver-fuzz tests; they self-skip because Windows denies native symlink creation, so the escape properties are UNVERIFIED here — enable Developer Mode before T12 and treat any resulting FAILURE as a finding the skip was hiding). Plus: **T9 IS IN PROGRESS — FIVE passes landed 2026-08-08** (memory imports, decoder strictness, the cross-store `InstanceId` fix, declared subtyping, type canonicalisation). **T10a ADDED 2026-08-08 — the EMITTER audit**: three "emits a different module than the text describes" defects in two passes, all found by accident, so T10a specifies a **round-trip property test** + a **`ModuleBuild` field-coverage sweep** rather than a read-through. **T9h ADDED 2026-08-08 — cross-module type identity via a `Store` type registry, approach APPROVED (wasmtime's SHAPE, our code; ~11 assertions)**. Earlier detail:: T9a#4's **memory half ✅** + `assert_unlinkable` + link-time import type checking, then **T9a#11 ✅ and most of #12 ◐** — the decoder-strictness cluster (seven checks; 99.0%, first time over 99). #12's remainder is the **text parser**, not the decoder. First pass (08-07): T9a #1/#2/#3 (+3 unlisted defects), **T9b size ✅ measured**, **T9c perf ✅ measured** (`examples/bench.rs`), **T9d licensing/docs ✅ done**. Open: T9a#4's **table** half (🚦 decision-gate), #5–#9, #11, #12, T9e `pin`, T9f tail calls. Original scoping (2026-08-06) — T9a correctness punch-list (12 items, each with its measured cost, ordered by value), T9b size, T9c perf, T9d licensing/docs, **T9h — cross-module type identity via a `Store` type registry (approach APPROVED 2026-08-08: wasmtime's SHAPE, our code; ~11 assertions)**, T9e `pin`, T9f tail calls (the last unimplemented IN-SCOPE proposal — 1.0 cannot be claimed without them), T9g scope confirmations (what to leave failing, and why the in-scope residual is far smaller than 738/3,075 suggests). **T10 (0.11) = bug hunt + code hygiene, now including T10a — the EMITTER audit (added 2026-08-08: three "emits a different module than the text describes" defects in two passes, all found by accident; T10a specifies a round-trip property test + a ModuleBuild field-coverage sweep instead of a read-through)**, **T11 (0.12) = optimization review** and **T12 (0.13) = security review** were added by the owner 2026-08-06; the ordering **measure → find → optimize → attack** is deliberate, and T12 records that `panic = "abort"` makes any reachable panic a host-process kill. Current status (**PORT phase; T0–T8 done, published through v0.9.0, oracle frozen @dadc727**) + the **conversion task list** T0–T12 (scaffold → types/reader → opcode → decode → validate → interp slices → text → wasi → C-ABI → hardening → bug hunt → optimization review → security review), parity-gated, with the 4 decision-gates inlined (**all now resolved**) and T9 scoped to what actually remains. Loader phases 1–4 |
| [known-issues.md](known-issues.md) | Issue tracker — ⚠️⚠️ **THE START FUNCTION NEVER RAN (fixed 2026-08-08)**: `Module::start` was decoded, validated twice, assembled and printed by the CLI, and no code path ever called it (§4.5.5 step 11). A module initialized entirely by `(start $f)` returned defaults with **no error at any stage** — the worst silent-wrong-output defect the port has produced, and 10 of its 12 assertions had been failing for **five releases** in files literally named `start.wast`. **Triage read messages for a diagnosis; the cruder question — does the feature this file is NAMED for work at all? — was never asked.** Found by wiring trap backtraces and asking where an *instantiation* trap gets its frames. Also ✅ **trap backtraces DONE (T9a#7)** — the T8 bet paid off, real frames with **no ABI change**; ⚠️ its plumbing was not free by default (`pc: &mut usize` measured 3.6%% slower; a one-shot closure recovered it). Plus: ✅ **Decoder strictness DONE 2026-08-08** (T9a#11 + most of #12): seven decode-stage checks, **99.3%**, and the finding that #11/#12 were never two items but one theme in two halves (*wrong stage* and *over-acceptance*). ⚠️ **Section order is not id order.** A repeated section had been **silently replacing** the first; four of our own fixtures turned out to be malformed modules; and the const-expr sweep must key on segment **mode**, not on empty bytes. Cold start unchanged within noise once the redundant `Code::body` copy went. Also ✅ **T9a#4's memory half DONE 2026-08-08** (owner chose option 2: memories ship, tables stay refused), with the three non-obvious details — `module.memories` is the whole index space so only its tail is allocated; active data segments fork on the index so a later failure leaves no orphaned slots; limits matching compares **declared** types, so `Memory` carries its `min`. **Plus what unskipping `assert_unlinkable` found: imports were NEVER type-checked at link time** (silent-wrong-call), now fixed in the two places the type is known, with the equality-vs-subtyping trade **measured** rather than argued. ⚠️ **The obvious funcref packing is wrong — bits 32–63 collide with `I31_TAG`.** New open items: **no type canonicalisation (~40 assertions, the largest in-scope cluster left)**, host imports unchecked by construction, and malformed modules reaching the *link* stage. Also: ✅ **T9a first pass FIXED 6 defects (2026-08-07)**, three of which were not on any list: `Op::MemorySize` reading another instance's memory (mutation-verified fix), the `.wast` runner's failed-build redirect, `ref.null $t` (**plus a silent-wrong-value bug in the same arm — `nofunc` assembled as `func`**), the inline table-elem shorthand, an **undecodable concrete-ref block type**, and **`br_table` label typing wrong in two directions**. ⚠️ **`ref.null`'s logged cause was WRONG** — it did not move `br_table.wast`, which needed all four assembler/decoder/validator fixes; treat every remaining cost figure as a hypothesis. 🚦 **NEW DECISION-GATE: T9a#4 is not plumbing** — a `funcref` carries no instance identity, so an imported *table* would dispatch to the wrong function; imported *memories* are safe, imported tables need the value encoding decided. Plus: the **2 silent-wrong-output defects fixed at T8** (dropped table initializers; element-segment form 4 rewriting a type) **and the third-order lesson**: a check that is harmless "because the case cannot occur" becomes a bug the moment it can; what T8 closed and what it left open; the **ranked pre-T8 punch list** (2026-08-05), the **5 open defects the wasmtk corpus surfaced** (`ref.null $T` unassemblable · 2 validator gaps the oracle passes · no trap backtrace · an unconditional `data_count` section), the fixed shared-store and literal/text defect write-ups, the port's intentional divergences (single-threaded atomics, `delegate` rejected everywhere, the debug-build stack-depth finding), the wazmrt residuals (#17/#18/#23), and the open decisions |
| [releasing.md](releasing.md) | 📦 **The distribution manifest (2026-08-10): exactly which files a user needs**, measured on a clean `PATH` rather than read off the build files — CLI = `wasmrt.exe` alone; C ABI = the `.dll`/`.a` + `include/wasmrt.h` (the `.dll.a` import lib is a LINK-time file, not a runtime one). ⚠️ `libwasmrt_capi.a` is ~22 MB of archive metadata and is NOT shipped size. 🔒 The CLI was **not standalone** until this date — it imported `libunwind.dll` from the mingw toolchain and died with **exit 127** on a clean machine while every dev-box test passed; pinned by `crt-static` in `.cargo/config.toml` so it cannot regress. **"It runs here" is not evidence that it ships.** Plus: **Versioning + publishing.** Port-progress ladder (`0.x` → `1.0` = full oracle parity); the 3 crates share one version, CLI is published as `wasmrt`; per-task manual releases; the per-release checklist + the 🔒 binding **pre-publish doc-sync gate** (full `cmem/` + ROADMAP/CHANGELOG/README committed BEFORE the owner is notified to publish — owner directive 2026-07-31); crates.io names reserved |

## Related files outside cmem

- `README.md` — the public, user-facing doc. NOT project memory.
- `ROADMAP.md` + `CHANGELOG.md` — the **public progress trackers** (stage ladder + use-case matrix +
  release notes). Kept in lockstep with each release per the `releasing.md` trigger. NOT project memory.
- `docs/port/*.md` — the detailed wazmrt deep-read maps (6 subsystems) + the loader survey + `wasmrt.h.draft`.
- `scripts/wazmrt-provenance.txt` — 🔒 **RETIRED 2026-08-11.** Was the oracle monitor's baseline
  (`scripts/check-wazmrt.sh`, now **deleted**). Kept as a historical provenance record naming the wazmrt
  commits the port was developed against; **nothing reads it.**
- `scripts/c-gate.sh` — the C-ABI gate (link-completeness 74/74 + `c_smoke` compiled against the shipped
  header). `scripts/miri-gate.sh` — `wasmrt-capi` under Miri (28/28), incl. a lifecycle fuzz.
- `LICENSE` — current; `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/`third_party/LICENSES.md` to be added at scaffold time.
