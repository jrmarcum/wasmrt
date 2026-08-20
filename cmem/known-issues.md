# Known Issues

## ✅ CLOSED 2026-08-20 — the T13 day-2 sweep. **Read this before believing anything below it.**

**F1–F7 and S1, S2, S3, S6, S7 are all closed**, and the 257 CORE spec files are at 0 failed / 0
skipped (63,807 / 112 / 584 — see `roadmap.md` and `testing.md`). Entries further down this file were
written while those were open; several state as a *deliberate decision* something that is now simply
implemented. Specifically **now DONE, and their notes below are historical**:

| was recorded as | now |
| --- | --- |
| S1 — `any.convert_extern` / `extern.convert_any` "deliberately absent" | **implemented**, on the tagged `Value` representation the old note specified (`HOST_TAG` bit 64, `EXTERN_TAG` bit 65) |
| "imported tags cannot link" | **implemented** — a tag is a store slot, matched on its type INDEX |
| "imported globals link **by value**, so a mutable global import is a snapshot" | **implemented** — an imported global is the exporter's store slot, shared |
| "`delegate` is rejected, inherited from the oracle" | **implemented** — the oracle retired on 2026-08-11 and the rationale with it |
| "`assert_exception` unhandled" / "`get` action unsupported" | **implemented** |
| "`nullfuncref`/`nullexternref`/`nullexnref` collapse onto their tops" | **implemented** as distinct types |
| "`anyfunc` accepted for MVP-era tools" | **refused**, as the spec and wasmtime require |

🆕 **What replaced them is one open item and four proposals.** ⚠️⚠️ **wasmrt's emitted binaries were
not readable by another engine** whenever they contained a non-null abstract reference type or a
`try_table` — both now fixed and pinned by tests that assert the BYTE. The lesson is
`best-practices.md` §3.8b, and the standing instruction is: **run a format-level change through
`wasmtime compile out.wasm`.** The remaining work is proposal implementation only —
custom-descriptors (64 failures / 451 skips), custom-page-sizes (34 / 7), threads (13 / 18),
wide-arithmetic (1 / 108).

---

## 🗄 HISTORICAL — S1: the externref/anyref bridge. **Closed 2026-08-20; kept for the reasoning.** **Phase 2, and the one item the failure pass cannot reach.**

🔒 **Scheduled behind the failure pass (owner, 2026-08-19): all 172 failures first, then the skips.**
That order is measured-correct — 739 of 930 cascade skips live in files that also carry failures — but
**S1 is in the 191 that do not.** `ref_test`, `ref_cast`, `br_on_cast` and `br_on_cast_fail` have
**zero failures between them** and hold 160 cascades, so no amount of failure work will surface this.
It heads phase 2. See `roadmap.md` and `best-practices.md` §5.5a.

`any.convert_extern` (`0xFB 0x1a`) and `extern.convert_any` (`0x1b`) are **deliberately absent** —
recorded in `opcode.rs` with the reason, and that deferral is still correct. What was **not** known
until the skip census of 2026-08-19 is what it costs: **~181 assertions**.

Five modules fail to assemble because of it, and each strands every assertion behind it:

| file | passed | failed | skipped | root |
| --- | --- | --- | --- | --- |
| `ref_test.wast` | 2 | 0 | **68** | 1 unassemblable module |
| `ref_cast.wast` | 2 | 0 | **42** | 1 |
| `br_on_cast.wast` | 8 | 0 | **27** | 1 |
| `br_on_cast_fail.wast` | 8 | 0 | **27** | 1 |
| `extern.wast` | 0 | 1 | **17** | 1 |

⚠️⚠️ **None of these five files had ever appeared in a conformance report.** The runner listed a file
only when it had *failures*, and four of the five have none — so 164 skips sat behind a filter for the
whole port. See `best-practices.md` §5.6.

🔒 **Fix the REPRESENTATION first, then add the two decoder arms.** An `externref` crosses the C ABI as
a raw pass-through `uint64_t`, while `ref_matches`' `Any` arm treats any non-null, non-i31 reference as
a **GC heap index**. They share one numeric space, so the moment a host handle can become an `anyref`,
**host handle 2 reads as GC object #2** — a type-confused read the cast believes it verified. `Value`
is a `u128` and every reference form lives in the low 64 bits, so the high 64 are free for an externref
tag: no widening, and the C ABI's pass-through contract survives by tagging on entry and stripping on
exit in `wasmrt-capi`'s two conversion functions. **Adding the arms first is the silent-wrong-answer
order.**

Also blocked on it: `ref.host` as a value literal (`extern.wast`, 1 failure — item F7).


## ⚠️⚠️ OPEN (proven 2026-08-19, fix ATTEMPTED AND REVERTED) — try_table catch labels resolve one frame too deep

**The defect is proven; the fix is not.** `try_table`’s catch-clause labels resolve **with the
try_table’s own frame on the label stack**, when the spec resolves them in the context OUTSIDE it — a
handler runs once the try_table has been exited, so its label cannot name it.

🎯 **The evidence is a clean INVERSION**, which is what makes it certain rather than suspected:

| module | spec says | wasmrt |
| --- | --- | --- |
| `(func (result exnref) (try_table (catch_all 0)) (unreachable))` | **invalid** | ❌ accepted |
| `(func (result exnref) (try_table (catch_all_ref 0)) (unreachable))` | **valid** | ❌ rejected |

Both directions wrong at once, which is exactly what an off-by-one label produces — and why the net
conformance movement is small enough to hide.

⚠️⚠️ **THREE consumers share the mistake**: the assembler pushes the label before resolving the
clauses (its comment even says *“label 0 = the try_table itself”*), the validator does the same, and the
interpreter branches to `d + c.label`. **Every text round trip therefore agrees with itself and the
corpus stays green on the valid cases** — only the spec’s own `assert_invalid` cases can see it.
🎓 *Two consumers agreeing is not corroboration when they share the mistake — and there can be THREE.*
wazmrt hit the identical defect (their R4).

🔻 **A fourth party pins the wrong convention: our own test.** `eh_try_table_catches_throw` is
hand-written binary with the comment *“label 1 = the enclosing block”*. **A test can encode a misreading
of the spec and pass forever if nothing exercises it** — and this one made the convention look
deliberate.

### Why the fix was reverted

Correcting the assembler, the validator **and** the interpreter together still regressed three
previously-clean files — `instance.wast`, `throw.wast`, `throw_ref.wast` — so the model is incomplete:
there is at least a fourth site (the legacy `try`/`catch` unwind path shares `throw_exception`, and
`branch()` may count frames differently again). **Reverted rather than shipped**: regressing three files
to fix two assertions is a bad trade, and guessing at the remaining site is how the day’s other confident
wrong claims happened.

**For the focused pass:** work from the spec text for §3.3.8.10, fix all sites in one change, rewrite
`eh_try_table_catches_throw`’s comment and bytes, and use `scripts/conformance-diff.sh` — which now
catches the newly-failing direction that hid these three.


## ✅ FIXED 2026-08-19 (same day) — the SIX missing WasmGC array instructions now ship

**`array.new_data` / `array.new_elem` / `array.init_data` / `array.init_elem` / `array.fill` /
`array.copy`** are implemented end to end — `Op` tags, decoder (`0xFB 0x09/0x0a/0x10..0x13`),
validator, interpreter and assembler. **Suite 62,238 / 378 / 2,038 → 62,435 / 378 / 1,825**: **+197
passes, −213 skips, failures UNCHANGED**, and **no file lost a pass** (verified by joining the per-file
counts, not by reading the totals). 460 workspace tests, clippy clean.

⚠⚠ **The pass introduced an ACCEPT-INVALID and the corpus caught it on the first run.** `array.copy`’s
type check compared `storage.unpacked()`, which maps **both `i8` and `i16` onto `i32`** — so copying
between differently-packed arrays was accepted. `array_copy.wast`’s *“array types do not match”* case
failed immediately. 🎓 *An encoding chosen to make EXECUTION agree can erase the distinction
VALIDATION runs on* — the projection is right for the operand stack and wrong for a type-identity
question. Fixed by comparing **storage** types, and pinned by
`array_copy_between_differently_packed_arrays_is_refused`, which asserts the refusal in **both**
directions **and** that the matching cases still pass, so a blanket ban cannot masquerade as the rule.
✅ **Mutation-verified, with the mutation confirmed applied before the result was believed** (§4.2a).

⚠⚠ **A second hazard, caught while writing it:** the six new internal `Op` tags include `0xcd..0xcf`,
which sit **outside** the decoder’s existing `0xd7..=0xfa` internal-tag guard — so a raw `0xcd` byte in a
function body would have **decoded as `array.copy`**. That is exactly *a synthetic internal tag placed
in a real encoding space eventually means something else*, recorded in `best-practices.md` §3A.2 the
same morning and re-created within hours. The guard now covers both ranges, with a comment saying it
moves whenever a tag is added.

*(Original entry below, kept because the diagnosis is the reusable part.)*

### ⚠️⚠️ How it was found — SIX WasmGC array instructions were MISSING, in a proposal recorded as DONE

🔻 **CORRECTED within the hour: it is SIX, not seven.** The seventh, `any.convert_extern`, is **not an
oversight** — `opcode.rs` carries a ⚠⚠ comment at the `0xFB 0x1a`/`0x1b` decode site saying both convert
ops are **DELIBERATELY ABSENT**, with a soundness reason: an `externref` crosses the C ABI as a raw
`uint64_t` while `ref_matches`' `Any` arm treats any non-null non-i31 reference as a **GC heap index**,
so converting a host handle to `anyref` makes host handle 2 read as GC object 2 — **type confusion the
cast believes it verified.** The recorded fix is to tag externrefs in `Value`'s free high 64 bits
**first**. ⚠️ **My error, and it is the one this project keeps paying for in the other direction: I read
the opcode TABLES and not the COMMENTS beside them, then wrote “no recorded reason”.** *Read the struct,
not the prose above it* has a mirror — **read the prose, not only the table.**

**`overview.md` has said since v0.6.4 that WasmGC landed** — *"GC struct/array heap + `i31` +
`ref.test`/`ref.cast`/`br_on_cast`"* — and `vision.md`'s canonical axis says **"every proposal in the
scope list now runs."** ⚠️ **Both are wrong by seven instructions**, all of them in-scope GC:

| mnemonic | what it is |
| --- | --- |
| `array.new_data` · `array.new_elem` | construct an array from a data / element segment |
| `array.init_data` · `array.init_elem` | fill an existing array from a segment |
| `array.fill` · `array.copy` | the array bulk operations |
| `any.convert_extern` | the `extern` → `any` half of the externref bridge |

**Not an assembler gap — genuinely absent.** There is **no `Op` variant, no `from_text_name` entry and
no interpreter arm** for any of them (`grep` for `ArrayFill`/`ArrayCopy`/`AnyConvertExtern` returns
nothing in `opcode.rs` or `interp.rs`). A `.wat` using them fails to assemble; the binary form has no
opcode to decode.

**How it stayed invisible:** the failure surfaces as `UnknownInstr` from the *assembler*, which
`is_unsupported()` scores as a **skip** — so seven missing instructions in a shipped proposal produced
**no failures at all**, only quiet skips, in files (`array.wast` 29 skipped, `extern.wast` 17) whose
names do not say "array bulk ops". 🎓 **A conformance number cannot report a gap that only ever
produces skips** — which is the whole reason T13-0 instruments them.

⚠️ **This is the sibling of the start-function defect**, one level up: *a feature can be decoded,
validated and printed and still never execute* — and here, **a PROPOSAL can be recorded DONE with
instructions missing**, because "GC" was ticked off as a unit and nobody enumerated its instruction
list against the spec. **Tick a proposal off against its instruction list, never against its name.**

🔻 **Independent confirmation that the shape is real:** the sibling runtime hit the *same seven*, and
recorded that its own item "named four ops — SIX were missing", plus a separate finding that the
extern/any converts "existed only in const-exprs" and were worth **172 assertions**. Two
implementations, two independent triages, the same list.

**Priority: this outranks every untargeted-proposal track in T13.** GC is **in scope**; descriptors and
wide-arithmetic are not. A missing in-scope instruction is a correctness gap; an unimplemented
untargeted proposal is a scope decision.

⚠️ **And it changes how the scoring split must be built.** Had the split been written first, from the
plausible reading that "the assembler does not know this mnemonic → the mnemonic is not real", these
seven would have been classified **malformed** and their `assert_malformed` cases would have banked as
**false passes** — the one direction that cannot be noticed afterwards. **The over-inclusive rule is
what saves it: the "we are incomplete" list must be a positive allow-list of real instructions we lack,
and everything reclassified as malformed must be provably not an instruction in any proposal.**

## ✅ FIXED 2026-08-19 — a trailing `--dir` was handed to the GUEST; the sandbox was silently never granted

**Fixed the same day it was found, and BEFORE the flags that would have made it dangerous.** Host
flags are now accepted in **both** positions — before the module path (wasmrt’s spelling) and
immediately after it (wazmrt’s) — so a command line does the same thing under either runtime with only
the program name changed (`interop.md` §2.2). Four behaviours, five tests
(`crates/wasmrt/tests/cli_flag_position.rs`):

| case | behaviour |
| --- | --- |
| `wasi --dir D <mod>` / `wasi <mod> --dir D` | **identical** — the preopen is applied either way |
| `wasi --typo <mod>` | ⚠️ **error naming the unknown OPTION** — it used to be read as the module path, reporting `cannot read '--typo'` |
| `wasi <mod> arg --dir D` | **warns**, never refuses — a guest may legitimately take `--dir` as its own argument |
| `wasi <mod> -- --dir D` | **silent** — `--` is the user saying “the rest is the guest’s” |

⚠️ **The `--` case failed on the first implementation**: the marker was stripped *before* the warning
scan, so the scan never saw its own stop condition and warned about a flag the user had explicitly
handed over. Found by probing all five cases rather than by re-reading the code — and the inverse is
now pinned beside the positive, so a blanket warn cannot pass for the rule.

🎓 **The sequencing is the point.** Today the only host flags are preopens, so a misplacement fails
**closed**. `--verify`/`--pins` (T9e) and `--max-iterations` (T9i) would have made the identical slip
fail **OPEN** — restriction requested, no error, no restriction. **The parser was fixed before the
flags that make it dangerous arrive**, which is the whole reason it was promoted ahead of them.

*(Diagnosis kept below — it is the reusable part, and it came from the first real run of `interop.md`
§4 check 5.)*

### ⚠️⚠️ How it was found — a trailing `--dir` handed to the GUEST

**Severity: live, security-relevant, and silent.** `wasmrt wasi <module> --dir <spec>` **does not
error.** `take_dir_flags` (`crates/wasmrt/src/main.rs:55`) consumes preopen flags only from the
**leading** run of arguments and stops at the first non-flag — which is the module path. Everything
after it, `--dir <spec>` included, becomes the **guest's argv**.

**So the guest runs with NO preopen at all**, every path call returns `BADF`, and **nothing warns**.
It reads as a guest bug, not as a missing grant, which is what makes it expensive to diagnose.

⚠️ **It is also a swappability break, and that is how it was found**: wazmrt's flags come **after** the
module path (`wazmrt <module> --dir …`) and wasmrt's come **before**. A wazmrt user's muscle memory
under wasmrt therefore produces an unsandboxed run rather than an error. Found by the first real run of
`interop.md` §4 check 5 — **running both binaries, which is a check no amount of reading either
codebase would have produced** (`interop.md` §2.1m, finding F3).

**Fix (with T9e's CLI alignment):** accept preopen flags in **both** positions, and — the load-bearing
half — **refuse an unrecognised leading `--flag` rather than treating it as the module path**, so the
mistake cannot be silent in either direction. ⚠️ **Do not simply move the parse to the trailing
position**: that would break the documented form and swap one silent failure for another. **The
direction to err in is a property of the consequence** — an unsandboxed run is worse than a rejected
command line.

⚠⚠ **ESCALATED 2026-08-19 (coordinate, interop v10): TODAY IT FAILS CLOSED; T9e AND T9i MAKE IT FAIL
OPEN.** wazmrt hit the mirror of this defect and fixed it (their H7), and their analysis carries the
split mine did not: **which direction a donated flag fails in depends on what the flag DOES.**

| flag | donated to the guest means | direction |
| --- | --- | --- |
| `--dir` / `--ro-dir` / `--allow-symlink` (today) | the preopen is never granted — the guest gets **less** access | fail-**closed**, survivable |
| `--verify` / `--pins` (T9e) | the verification policy is never applied — **an unverified module runs** | ⚠⚠ fail-**OPEN** |
| `--max-iterations` / `--max-memory` / `--max-table-elems` (T9i) | the ceiling is never applied — **the guest runs unbounded** | ⚠⚠ fail-**OPEN** |

🔒 **So this is a PREREQUISITE for T9e and T9i, not an independent cleanup.** The moment a restriction
flag exists, *“the user asked for a restriction, got no error, and ran without it”* becomes reachable —
and a silent absence of a security control is exactly the class this project ranks worst. **Fix the
parser before adding the flags that make it dangerous.** ⚠️ wazmrt chose to **warn, not refuse** (a guest
may legitimately take `--dir` as its own argument) and to examine **nothing after an explicit `--`**;
both choices are contract-relevant and should match.

🎓 **The generalisable finding: a flag that is silently REINTERPRETED as data is worse than a flag that
is rejected.** Sibling of the `--no-verify`/`--yes` flag-region hazard recorded for T9e, and of the
`--dir` separator row — three defects in one small parser, all of the same shape: **an argument that
means one thing to the host and another to the guest, with nothing marking the boundary.** The `--`
end-of-flags marker wasmrt still lacks is exactly that marker.

## ⬜ OPEN (found 2026-08-19) — the proposal list is spelled THREE times and nothing compares them

**Severity: latent, not live.** No proposal is mis-gated today. What is missing is the check that keeps
that true, and the defect it would catch is the one wazmrt actually shipped: *a header advertising a
switch that is not there.*

The set of proposals exists in three hand-written forms:

| # | spelling | file |
| --- | --- | --- |
| 1 | `enum Feature` + `Features`' struct fields + `all()` / `mvp()` / `name()` | `crates/wasmrt-core/src/features.rs` |
| 2 | the `u32` → `Feature` map `feature_of` | `crates/wasmrt-capi/src/lib.rs:332` |
| 3 | `wasmrt_feature_t`'s 15 constants | `crates/wasmrt-capi/include/wasmrt.h:151` |

⚠️ **Rust's exhaustiveness cannot help at #2**: `feature_of` matches on `u32` with `_ => return None`,
so **adding a sixteenth proposal compiles**. It would ship a header constant, and
`wasmrt_config_set_feature` would return `false` for it — the proposal unreachable from C, with **no
test failing**. Core's own `EVERY: [Feature; 15]` already pins #1 (three tests walk it); nothing walks
#2 or #3, and `tests/c_smoke.c` touches only `SIMD` and `FUNCTION_REFERENCES`.

⚠️ **T9f's tail-call flag is exactly the case that went wrong in the other runtime**, and it is the
newest entry in all three lists — `WASMRT_FEATURE_TAIL_CALL = 14` / `14 => Feature::TailCall` /
`Feature::TailCall`. Verified consistent by hand on 2026-08-19; the point is that "verified by hand"
is the state this entry exists to end.

**The fix (T10):** a test that walks `EVERY`, round-trips each variant through `feature_of` in **both**
directions, and asserts the count matches the header's — comparing **names and values, not lengths**,
since two lists of equal length can still disagree and a value mismatch gates a *different* proposal
than the caller selected. Deriving beats pinning where the shape allows it: **a pin makes drift
detectable, deriving makes it unrepresentable.** — borrowed method, `best-practices.md` §3A.2.

## ✅ Fixed 2026-08-14 (T9f) — `return_call_ref` was NOT A TAIL CALL, and conformance said 40/7

**Three defects, one pass.** All pre-existing; all found while implementing `return_call` /
`return_call_indirect`.

### 1. ⚠️⚠️ `return_call_ref` grew the native stack — a FAKE tail call that answered correctly

`interp.rs` called `call_function(.., depth + 1)` and then set `pc = ir.len()`. Every result it
produced was **correct**. The native stack grew on every hop, so unbounded mutual recursion — the
entire reason the proposal exists — still exhausted it.

**Its conformance file read 40 passed / 7 failed for releases.** Nobody reads 40/7 as *the feature is
absent*; it reads as "nearly done". `cmem/roadmap.md`'s T9f entry had predicted this exact trap in
writing — *"a naive call-then-return implementation passes the tests and misses the feature"* — while
it was already in the tree, applying to the form that already shipped.

**Fixed structurally.** `run` no longer recurses on a tail call: it reports the callee through a
`TailCall` out-parameter and unwinds; `call_function` **loops**, reusing its native frame at the same
`depth`. `countdown(5_000_000)` returns; the identical function written with `call` traps.
⚠️ Deliberate consequence, recorded rather than discovered later: **a replaced frame leaves no
backtrace entry** — that is what replacing it means.

⚠️ The out-parameter is deliberate over a richer return type: `run`'s return value is on the hot path
of every call, and the previous change to it cost 3.6% (§1.7).

**Measured, because the suite cannot see this**: with the fake restored by mutation, the new property
test fails **5 of 6** while the three tail-call spec files still score **38/6, 72/4, 40/6**. Written up
as `best-practices.md` §3.10 — *a conformance suite checks RESULTS; some features are about RESOURCES.*

### 2. `return_call_ref` REFUSED VALID MODULES — equality where §3.3.8 says subtyping

Its validator required the callee's results to *equal* the enclosing function's. The spec says
**subtype**: a function returning `(ref null $t)` may tail-call one returning `(ref $t)`, because a
non-nullable reference is a subtype of the nullable one. ⚠️ Wrong in the **refusing** direction, which
is exactly why it survived — a rejected valid module is a failing assertion, never a wrong answer, and
this project's instincts are tuned for silent-wrong-output. All three tail forms now share one
`check_tail_results`.

### 3. 🆕 `return_call_indirect` assembled as `call_indirect` — emitter instance #5

`emit_call_indirect` hard-coded `ctx.out.push(0x11)`, so routing the tail form through it emitted the
**non-tail** instruction: the text said one thing and the module said another. **Created and caught
within the hour**, which is the useful part — it is the T10a mechanism (an emitter reconstructing a
form from a *subset* of the parser's facts) and it caught me while I was writing about it. It surfaced
as a **`StackHeightMismatch` from the validator**, three stages away (§3.7). The opcode is a parameter
now.

✅ **T10a's opcode sweep fired preventively** for the first time: adding `0x12`/`0x13` without the
assembler's immediate entries failed the build-time test immediately, before the defect could ship.

**Result:** `return_call.wast` **44/0/0**, `return_call_indirect.wast` **76/0/0** (was 27/3/**49
skipped**), `return_call_ref.wast` **46/0/0**. Suite **62,238 / 378 / 2,038**. **458 tests.**
🚦 **Every in-scope proposal is now implemented** — 1.0 is no longer blocked on a missing feature.

## ✅ RESOLVED 2026-08-14 — all three wazmrt-reported findings closed

The report below is kept verbatim as the record; this is what was done about it.

**#1 — the real bug: FIXED, and it was worse than reported.** `HeapObject` now carries `owner`, the
allocating instance, exactly as a funcref packs its owner into bits 62..32. `ref_matches` takes the
testing instance, resolves an abstract target from the object's shape alone (no index involved), and
for a **concrete** target either takes the cheap module-local path (`owner == inst`, the common case,
exact) or resolves **both** sides to store-wide ids and asks `Pools.types` — the same mechanism import
matching and `call_indirect` already used. `owner` is **free**: it lands in padding the `Vec`'s
alignment already forced, pinned by a `size_of` assertion so a future change that makes it cost real
memory fails the build. Reproducer now 3/3.

🆕 **The sweep found a SECOND site the report did not: the `Func` arm of the same function.** It had
learned half the rule — it fetched a funcref's type index from the owner's module and then compared it
against the **testing** module's table anyway. Its own comment called this "approximate" and logged it.
That is the identical defect one level subtler, and **the full spec testsuite never moved when it was
fixed**: nothing in 62,498 assertions casts a funcref to a concrete type across a module boundary. It
is now pinned by `tests/funcref_cross_module_type_index.wast`.

⚠️ **This is exactly what the wazmrt note warned about** — *"wasmrt built the registry and wired it
into `call_indirect` only; if you build it, wire it into every consumer at once."* Acting on that,
every type-identity comparison in the engine was enumerated rather than reasoned about: import
matching ✅, `call_indirect` ✅, `ref_matches`/Any 🔴→✅, `ref_matches`/Func 🔴→✅. **Two of four were
wrong, and only one of the two was in the report.**

**#2 — the immunity: PROTECTED.** `Pools.gc_heap`'s doc comment now states as a 🔒 invariant that the
heap is one-per-store and must not be "consolidated" onto `InstanceData`, with the reason (a bare index
would select a different object across a link — object substitution, silently). It also records that
this is *why* `HeapObject::owner` is necessary rather than redundant: the two decisions are a pair.

**#3 — the latent externref/GC overlap: BLOCKED AT THE POINT OF ENTRY, not just noted.** The absent
`0xfb 0x1a`/`0x1b` arms in `opcode.rs` now carry a ⚠️⚠️ block explaining that implementing
`any.convert_extern`/`extern.convert_any` without first changing the representation opens the hole, and
naming the fix: **`Value` is a `u128` and every reference form lives in the low 64 bits**
(`NULL_REF = u64::MAX`, `I31_TAG = 1<<63`, funcref owner in 62..32, GC index bare), so the high 64 bits
are free for an externref tag — **no widening required**, and the C ABI's pass-through contract survives
by tagging on entry and stripping on exit in `wasmrt-capi`'s two conversion functions. The warning sits
where an implementer will physically be, not in a document they might not read. **Representation first,
then the opcodes.**

🆕 **A gate the reproducers themselves needed.** Both `.wast` files were green and **nothing ran them**
except a human typing `wasmrt wast tests/`. `crates/wasmrt-core/tests/regression_wast.rs` now runs every
`tests/*.wast` under `cargo test`, asserts zero failures **and zero skips** (a construct that regresses
into "unsupported" is silently not a pass), and fails if the directory is empty so the gate cannot pass
vacuously. Any future reproducer dropped in `tests/` is covered without anyone remembering to wire it
up. *This is wazmrt's* **"a gate only gates the commits that RUN it — a gate with no trigger is a
preference too"** *applied to my own work, minutes after reading it.*

**Verification.** All four changes mutation-verified with the mutation **confirmed applied** before the
result was believed (§4.2a): reverting `owner == inst` to `true` reproduces the original failure in both
reproducers (2 failures each, while the abstract `isFunc` assertion correctly still passes) and makes
`regression_wast` fail. Spec suite **byte-identical** at 62,113 / 385 / 2,163 — the correct outcome, and
the same reason as the T9 `InstanceId` fix: this is a cross-module path the suite cannot reach.
**457 tests** (was 456), clippy clean.

---

## 🔴 REPORTED FROM wazmrt, 2026-08-14 — GC reference identity across a link

wazmrt hit a cross-module GC-reference defect, fixed it, and swept for the same class here. Three
findings, worst first. **The first is verified by EXECUTION against `target/release/wasmrt` 0.9.0,
not by reading** — reproducer committed as `tests/gc_cross_module_type_index.wast`.

### 1. 🔴 REAL BUG — a GC object's TYPE INDEX is read against the READER's module

`crates/wasmrt-core/src/interp.rs`, `ref_matches` → `RefHeap::Any` arm:

```rust
let Some(obj) = store.gc_heap.get(idx) else { return false; };
let kind = match module.comp_types.get(obj.type_index as usize)...;
head_matches(module, kind, Some(obj.type_index), rt.heap)
```

`obj.type_index` was written by whichever module ALLOCATED the object; `module` is the module doing
the testing. `head_matches` then evaluates `HeapType::Concrete(t) => module.is_subtype(ti, t)` —
raw indices from two different modules. Since `gc_heap` lives on `Pools` (shared per linking group),
objects really do cross modules, so this is reachable today.

**Measured, wrong in BOTH directions.** A's only type is index 0, a `(struct i32)`; B's index 0 is a
`(struct i64)` and B's index 1 is the structurally matching `(struct i32)`:

| test | expected | wasmrt 0.9.0 |
| --- | --- | --- |
| `ref.test (ref $b0)` — `(struct i64)`, structurally WRONG | 0 | **1** |
| `ref.test (ref $b1)` — `(struct i32)`, structurally right | 1 | **0** |

⚠️ **The accept side is the dangerous one** — wazmrt's R1 said exactly this about the same class. A
`ref.cast (ref $b0)` succeeds, and a following `struct.get $b0 0` reads a field written as i32 at
i64 width. Values are `u128` slots, so this is a wrong VALUE rather than an out-of-bounds read — but
it is a silent wrong answer produced by a check that believes it verified the type.

⚠️ **wasmrt ALREADY LEARNED THIS, for funcrefs.** `ref_matches`'s own doc comment says so: *"the
value carries its owning instance, and a funcref's TYPE lives in that instance's module, not the
testing one. Reading it from `module` gave the wrong answer for any reference that arrived through a
shared table."* The `RefHeap::Func` arm was fixed; the `RefHeap::Any` arm was not. **wazmrt made the
mirror-image mistake in the same week** — R2 established "a reference value names an ENTITY, not an
index" and converted funcrefs only, leaving GC references broken for a day. *When an invariant is
written down, enumerate every value kind it governs in the same pass.*

⚠️ **CORRECTION (wasmrt team → wazmrt, 2026-08-14): the paragraph below names the WRONG mechanism,
and the advice was stale on arrival** — wasmrt had already landed
`fix(gc): a reference's TYPE lives in its owning module, not the reader's`.
**There is no `Pools.type_canon`.** The store-wide registry is **`Pools.types: TypeRegistry`**;
`Module::type_canon` is a per-`Module` `Vec<u32>` (`module.rs:278`) and **cannot answer a
cross-module question at all**, which is the very thing the bug is about. The wazmrt author misread
`TypeRegistry`'s doc comment, whose first line begins *"`Module::type_canon` decides identity within
a module…"* — that line is the comment contrasting itself against `type_canon`, and its SUBJECT was
taken for the field's NAME. **Read the struct's fields, not the prose above them.** Corrected
advice: route the concrete case through **`Pools.types`**, the store-wide registry already there for
cross-instance `call_indirect`. Left in place rather than silently edited, so the wrong lesson does
not propagate — the same reason this project keeps retracted findings. *(Original, wrong:)*

**You are better placed to fix this than wazmrt was.** `Pools.type_canon` is a store-wide type
registry, so the correct answer is computable on this path. wazmrt has no equivalent available in
`refMatches` and had to settle for refusing the comparison (a deliberate loud false negative).
Route the concrete case through the registry rather than copying wazmrt's limitation.

### 2. ✅ IMMUNITY WORTH PROTECTING — `gc_heap` on `Pools` is the right call; do not "simplify" it

wazmrt put `gc_heap` on the **instance**, and a GC reference was a bare index — so a `structref`
crossing a module boundary indexed the READER's heap and silently selected a **different object**
(`ref.cast` succeeding, `struct.get` returning another object's value). Fixed 2026-08-14 by making a
GC reference carry `(owner_slot, index)` and resolving through the store.

**wasmrt cannot have that bug**, because `Pools` is *"the resource pools shared by every instance in
a linking group"*, so a heap index is store-wide meaningful. That is a load-bearing consequence of
what reads like a mere allocation detail — recorded so it is not undone later by someone
consolidating heaps per instance.

### 3. ⚠️ LATENT — the host-`externref` / GC-index overlap, unreachable only because a feature is missing

`WASMRT_EXTERNREF` is passed through raw (`Value::from(v.of.ref_)`; header comment *"opaque to the
host: pass it back unchanged"*), and the `RefHeap::Any` arm turns any non-i31, non-null value into a
`gc_heap` index. A host handle of `2` would be read as GC object #2.

**Not reachable today only because `any.convert_extern` / `extern.convert_any` are not implemented**
(empty grep across `wasmrt-core`) — nothing moves a host `externref` into the `any` hierarchy.
wazmrt implemented those two ops and thereby made the shared design flaw live. **wasmrt's scope
requires them**, so fix the representation *before* implementing them, not after.

⚠️ **This one is a TWO-PROJECT decision.** The `wasmrt_*` alias header exists so a consumer can A/B
the engines by swapping a DLL; boxing host externrefs in one and not the other makes the surfaces
differ semantically. wazmrt has tagged host references internally (`interp.host_tag`, bit 62) but
has deliberately NOT changed its C-ABI value model, pending an owner decision. Nothing forces the
issue: **wasmtk does not use `externref` at all** (its single mention is a binaryen type-ID table
entry that `getTypeName` does not even map), and the `universalWasmLoader-*` family reads guest
memory through a caller handle — i32 offsets, not externrefs.

---

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

## ✅ Fixed 2026-08-10 — `wasmrt run` executed WITHOUT VALIDATING (and so did the oracle)

**Found by the owner questioning a claim I made**, not by a test. Resolving T9a#9 I wrote that "the
oracle's execution path skips validation" as though it were wazmrt's peculiarity. Asked to justify it,
the check that should have come first showed **wasmrt had the same hole**:

| entry point | before | now |
| --- | --- | --- |
| `wasmrt <file>` (summarize) | ✅ validates | ✅ |
| `wasmrt wasi` | ✅ validates | ✅ |
| **`wasmrt run`** | ❌ **executed unvalidated** | ✅ |
| C ABI `wasmrt_module_new` | ✅ validates | ✅ |
| Rust `Instance::new` / `Store::instantiate` | ⚠️ documented precondition | ⚠️ (unchanged, now documented properly) |

`wasmrt run ill-typed.wasm f` printed `1` and exited **0**. §4.5.1 defines instantiation only for a
*valid* module, so this was outside the spec — and `wasmrt wasi`, one function away, refused the same
bytes. ⚠️ **The asymmetry was the bug**, exactly as with the T9a#3 `InstanceId` (where the C ABI held
the defence core lacked). `best-practices.md` §3.4 now carries both instances.

**Severity, honestly:** low for wasmrt. `forbid(unsafe_code)` means the failure mode is a wrong answer
or a panic, never memory unsafety, and a type-confusion probe (`i32` into a `funcref` table, then
`call_indirect`) trapped cleanly. It is the **wrong-answer** class that this project ranks worst, not a
memory-safety one.

**`Instance::new` still does not validate, deliberately.** That split matches wasmtime's
compile/instantiate separation and keeps validation off the path for callers who already did it. It is
now documented as a precondition in the strongest terms rather than a passing remark. ✅ **RESOLVED (owner, 2026-08-10) by measuring wasmtime rather than arguing the API.** wasmtime 47 on an
ill-typed module: `Invalid input WebAssembly code at offset 33: type mismatch: expected i32, found i64`.
**wasmrt matches that action** — same byte offset (module-relative, verified byte-identical at 33 and 61
against the live tool and pinned as tests), same wording, plus the function index wasmtime omits.
`Instance::new` keeps the compile/instantiate split: it is safe in practice because every shipped entry
point validates, and making it unrepresentable would cost a breaking API change for a case no shipped
surface can reach. Revisit at T12 if the security review disagrees. See `design-decisions.md`.

### The same defect in the oracle — fixed concurrently, and it was worse there

wazmrt's summarize and `.wast` paths validated; **both** execute paths and **`wasm_module_new`** did
not. Its own source already recorded part of the cost: an export index the decoder never cross-checks
reached a `.?` and was *undefined data in ReleaseFast — a segfault from a 31-byte module*, patched
defensively at that one site with the root cause left in place. Zig's ReleaseFast/ReleaseSmall remove
the safety checks that make such a slip survivable, and the C ABI is the embedding surface, so that is
where it mattered most.

Fixed in `wazmrt@baf0a38`: one guard in `run()` on the existing **`will_execute`** predicate (so a
future execute path inherits it), plus validation in `moduleFromBytes` — `wasm_module_new` is
specified to return NULL for an invalid module.

**Measured, both repos:** wazmrt `zig build test` **489/493 in all four modes** — Debug, ReleaseSafe,
**ReleaseFast and ReleaseSmall** (the freeze record only ever claimed the first two; that gap in the
record is now closed). wasmtk WASI corpus **376/376 unaffected**; wasmrt's `.wat` corpus is a **clean
532/532 at every stage**; wasmrt 446 tests, clippy, four surfaces, C-ABI and Miri gates all green. The
`39_JstyperMixed` fixture — the one that started this — now produces **byte-identical stdout from both
runtimes**, on a module both validate.

🔒 **Deliberate oracle drift, re-baselined in the same breath** to `wazmrt@baf0a38`
(`scripts/wazmrt-baseline.txt`); `check-wazmrt.sh` reports NO DRIFT against the new baseline. This is
the first oracle movement since the 2026-07-27 freeze, and the port matched it immediately.

## ✅ T9a#9 RESOLVED 2026-08-08 — **NOT a defect.** wasmrt is correct; the fixture is invalid

**Suite unchanged, correctly.** The right outcome for this item was to change nothing about the
validator. 4 tests added, all pinning the conclusion so it is not re-opened.

The entry read: *"`39_JstyperMixed.wasm.{rt,roundtrip}.wat` → `TypeMismatch`, oracle assembles **and
runs** them — so this is our type-checker being wrong, not the input."* Every observation in it was
true. The conclusion was wrong in three independent ways.

**1. The module is genuinely ill-typed.** Defined function #6:

```wat
(func (param f64 f64 f64) (result i32)
  local.get 0  local.get 1  f64.ge
  if (result f64)                 ;; declared result: f64
    local.get 0  local.get 2  f64.le   ;; pushes i32
  else
    i32.const 0                        ;; pushes i32
  end
  return)
```

Both arms produce `i32` against a declared `f64` — §3.3.5. The spec suite makes exactly this shape an
`assert_invalid` ("type mismatch") at `if.wast`'s `type-then-value-num-vs-num`, and our `if.wast` is at
**0 failures**, so wasmrt already enforced the rule correctly.

**2. ⚠️⚠️ "The oracle runs them" was never evidence of validity.** `wazmrt <module> <export>` decodes
and executes **without validating**; `wazmrt <module>` summarizes **and validates**. Run the same
construct through the second path and the oracle says:

```
validation: FAILED — TypeMismatch
```

**The oracle agrees with wasmrt.** Confirmed with a blatant control — `(func (result i32) i64.const 1)`
printed a result on the run path and was caught on the summarize path. So the premise recorded here
since T7 was an artefact of which subcommand had been used.

**3. The fixture is stale and double-counted.** The two files are **byte-identical**, so one fixture was
being reported as two failures. It declares **8 functions / 10 types**; the source
`39_JstyperMixed.wasm` has **14 / 13** — it is not a round trip of that binary at all, but of some
older build. The real binary **and** the hand-written `39_JstyperMixed.wat` both validate **OK** in
wasmrt.

**What landed instead — the diagnostic whose absence is what this item actually cost.** `TypeMismatch`
named no location, so pinning the failure to one of nineteen bodies needed a temporary probe.
Validation failures now carry the function index:

```
validation FAILED in function 8: invalid module: TypeMismatch
```

Implemented as a thread-local (`validate::last_failure_func_index`) rather than by widening
`ValidateError` — that type is `Copy`, is matched exhaustively in several places, and crosses the C
ABI, so growing it for a diagnostic would be a breaking change. Cleared **on entry**, so a
module-level failure reports *no* location rather than inheriting the previous module's. `no_std`
returns `None`: it costs a thread-local, and a freestanding embedder has nowhere to print it.

⚠️ **The lesson: cite the subcommand, not the tool.** A runtime that *executes* an invalid module is
over-permissive, not authoritative. Recorded as `best-practices.md` §2.3a — the sibling of §1.5a,
which is the same mistake made about one of our own gates.

## ✅ T9a#8 DONE 2026-08-08 — the assembler did not know four instructions took immediates

**Suite 62,037 / 393 / 2,245 → 62,113 / 385 / 2,163 — 99.4%.** +76 passes, −8 failures, **−82 skips**.
`call_ref.wast` 4/4/27 → **31/0/0**, `br_on_null.wast` 1/3/6 → **7/0/0**, `ref_as_non_null.wast`
1/1/4 → **5/0/0**, `unreached-valid.wast` 1/2/9 → **10/0/0**, `return_call_ref.wast` 10/5/36 → 40/7/0.
No file lost a pass. 🆕 **The `.wat` corpus assembles 534/534 for the first time**, 0 decode failures.

`immediate_arity` ended in `_ => 0` and the emitter's match in `_ => {}`. So four instructions —
`call_ref` and `return_call_ref` (a **type** index each), `br_on_null` and `br_on_non_null` (a
**label** each) — were emitted as a **bare opcode with the operand left in the token stream**. All
four are in the opcode table, decode correctly and execute correctly; only the assembler was wrong.

⚠️ **The three symptoms looked like three unrelated bugs, and none of them named the assembler:**

| form | what happened | what it looked like |
| --- | --- | --- |
| `(call_ref $t …)` folded | the missing typeidx shifted every following byte | **decode** failure: "missing END" |
| `call_ref $t` flat | `$t` was read as the next instruction | `UnknownInstr` — about `$t`, not `call_ref` |
| `br_on_null $l` | same | `UnknownInstr`, filed as an unrelated "1 of 534" |

That last one is the sharpest part: `gc-linked-list.wat` had been logged as a **separate** long-standing
corpus gap. One cause, two punch-list items.

⚠️ **Logged as "1 wasmtk file, undiagnosed"; worth 76 spec assertions** — `best-practices.md` §1.3
again. The entry counted the one file someone had noticed; the cost was in the 82 skips of five spec
files whose modules could not build.

🆕 **The gate had been measuring the wrong thing.** The `.wat` corpus check ran `wat -o` and nothing
more, reading 533/534 while the bytes `call_ref` produced were **undecodable**. **"The assembler
returned Ok" is evidence about the parser, not the emitter.** The corpus gate is now
assemble → decode → validate; see `testing.md`.

**The generalization — T10a's field-coverage sweep, landed for opcodes.** `Op::from_u8` makes the
opcode space enumerable, so a test walks all 256 single-byte opcodes and asserts that **if the decoder
reads an immediate for an op, the assembler writes one** (and the converse). The decoder is the right
oracle: it is the half that defines the binary format. Ops with bespoke emitters — block types,
`br_table`, `call_indirect`, `select_t`, memargs — are listed explicitly, so a **new** op is neither
generic nor special until someone classifies it. This is the **fourth** instance of the emitter
mechanism T10a names, which predicted "expect more than three".

⚠️ **Mutation-verified, and the first attempt lied.** A `perl` substitution silently failed to match,
the test passed, and the natural reading was "the sweep is decoration" — the §4.1 finding, apparently
confirmed. Re-doing it with `sed` **and grepping to confirm the line was gone** made it fail and name
all four ops exactly. **A no-op mutation and a worthless check produce the same observation** (§4.2a).

## ✅ Fixed 2026-08-08 — the text format's source character set was not enforced (§6.2/§6.3)

**Suite 61,987 / 441 / 2,247 → 62,037 / 393 / 2,245 — 99.4%.** +50 passes, −48 failures.
**`id.wast` 0/5/2 → 6/0/1 — a file at zero failures**; `annotations.wast` 12/51 → **56/8**. No file
lost a pass, and the `.wat` corpus held at **533/534** — the check that matters when tightening a
parser, because the suite alone rewards a rule that rejects valid input.

The lexer's `parse_atom` consumed **any** byte that was not a delimiter, and `parse_string` took any
byte raw. So all of this assembled:

```wat
(module (func $a\x01b))    ;; control character in an identifier -- not an idchar
(module (func $a\xffb))    ;; invalid UTF-8 -- source text is Unicode
(module (data "\x01"))     ;; raw control byte -- stringchar requires c >= U+20
(module (func $))          ;; empty identifier -- id ::= '$' idchar+
```

🆕 **And `from_utf8_lossy` did not merely over-accept, it silently RENAMED.** `$a\xffb` and `$a\xfeb`
both became `$a\u{FFFD}b`, so two distinct (malformed) identifiers collided on a single name. The
quoted form `$"…"` carried the same conversion, so `$"\ef"` was accepted *and* renamed. Restricting
atoms to `idchar` makes the slice ASCII by construction, so the conversion cannot lose anything —
the fix removes the lossy call rather than checking around it.

What is enforced now, all in `sexpr.rs`: `idchar` in atoms; `stringchar` (`c >= U+20 && c != U+7F`)
in string literals, with escapes still delivering arbitrary bytes; UTF-8 validity for raw non-ASCII
in strings; `id ::= '$' idchar+` for all three spellings of an empty identifier; and the character
rule **inside a skipped annotation**, since ignoring what an annotation *says* is what the proposal
requires, not exempting its bytes from being source.

⚠️ **A fourth probe looked identical and was NOT a defect.** `linechar ::= c:char (if c ≠ U+0A)` admits
any character but a newline, so a control byte inside a `;;` comment is **legal**; a test now pins that
it stays accepted. Fixing "all four" would have rejected valid `.wat`, and the only thing separating
them was reading the production instead of pattern-matching on "control character = bad".

⚠️⚠️ **THE FINDING IS ABOUT A SCOPE NOTE.** T9g had these 51 assertions under *scope confirmations —
NOT bugs, record, do not "fix"*, as `annotations`, an untargeted proposal. That was true of the **file**
and false of **44 of its assertions**. A logged *cost* gets re-measured because `best-practices.md`
§1.1 says so; a *scope* note reads as settled, is filed under "not a bug", and so nobody returns to it
— which makes it the more dangerous of the two. **The proof it was generic is `id.wast`**, which has
nothing to do with annotations and went from 5 failures to zero on the same fix. Recorded as §1.1a.

What genuinely remains annotation-scoped, and stays out of scope: 7 × "empty annotation id" and one
annotation-id UTF-8 case — 8 assertions, down from 51.

## ✅ Fixed 2026-08-08 — THE START FUNCTION NEVER RAN (silent wrong output)

**Suite 61,975 / 453 / 2,247 → 61,987 / 441 / 2,247 — 99.3%.** +12 passes, −12 failures, nothing
regressed. `start.wast` **8/7 → 15/0**, `start0.wast` 5/3 → **8/0**, `linking3.wast` 11/1 → **12/0**,
`linking.wast` +1.

`Module::start` was decoded by `module.rs`, checked by `validate.rs` (twice — §3 and the instantiation
pass), assembled by `wat.rs`, and printed by `wasmrt <file>`. **No code path ever called it.** §4.5.5
step 11 requires the start function to run as the last step of instantiation; `Store::instantiate`
built the instance and returned.

```wat
(module
  (global $g (mut i32) (i32.const 0))
  (func $init (global.set $g (i32.const 42)))
  (start $init)
  (func (export "get") (result i32) (global.get $g)))
```

`wasmrt run` printed **`0`**. No warning, no error, no unsupported-construct message — a module whose
whole initialization lives in its start function ran with every global at its declared default and
returned a plausible answer. **This is the worst instance of the silent-wrong-output class the port has
produced.**

**The fix** is five lines in `instantiate`, placed after the element and data segments so the start
function can observe them (§4.5.5 orders it last, and a test pins that ordering — running it earlier
still satisfies the naive "did it run?" test). A trap in it fails the instantiation; the caller never
receives the `InstanceId`, so it can neither call into the instance nor name it as an import. The
half-built slot is **not** reclaimed, for the same reason orphaned pool slots are not: index stability
is what makes every other `InstanceId` in the store keep meaning what it meant.

⚠️⚠️ **THE LESSON — and it is a triage lesson, not a coding one.** Ten of these assertions had been
failing for **five releases** in files literally named `start.wast` and `start0.wast`. They survived
every triage pass because triage read failures looking for a *diagnosis* — what does the message say,
which construct is unsupported — and these messages said nothing useful. The question never asked was
the cruder one: **does this file's NAME describe a feature, and does that feature work at all?**

A feature can be fully decoded, fully validated, and reported by the CLI while never executing. Every
stage that *inspects* it passes, so every stage that could have raised an alarm is satisfied. **Grep
for the field's reader, not its writer.** Recorded as `best-practices.md` §3.1a.

**How it was found:** not by triage at all — by asking, while wiring T9a#7's backtraces, where an
*instantiation* trap would get its frames. That question has no answer unless something runs during
instantiation, which led to asking what does. Nothing did.

## ✅ T9a#5 DONE 2026-08-08 — GC constant expressions

**Suite 61,887 / 457 / 2,339 → 61,975 / 453 / 2,247 — 99.3%.** +88 passes, −4 failures, **−92 skips**.
`i31.wast` **0/6/66 → 61/2/5**, `array.wast` **6/2/43 → 18/2/29**, `struct.wast` **6/3/17 → 21/3/0**.
No file lost a pass, none gained a failure. 411 workspace tests.

Six forms, in both the validator and the interpreter: `struct.new`, `struct.new_default`, `array.new`,
`array.new_default`, `array.new_fixed`, `ref.i31`. **The same six on both sides**, deliberately — a
validator that accepts what the evaluator rejects (or the reverse) is the disagreement class a previous
defect had, where `v128.const` evaluated correctly in a const-expr but validated as invalid.

`eval_const_expr` now takes an `Option<(&Module, &mut Pools)>`: the GC forms need the field layouts and
the heap. Passed as `None` at the one site that cannot produce a reference — a segment *offset* is an
integer — so that site keeps rejecting `struct.new` rather than being handed a heap it has no use for.

### ⚠️ The measurement lesson, in a NEW direction: a cost counted in FAILURES understates a defect that stops modules BUILDING

**T9a#5's logged cost was 6.** The real value was **88** — and the discrepancy is structural, not an
estimating error. `ConstantExpressionRequired` on a global initializer makes the whole *module* fail to
validate, and every later assertion in that file then has no target and is **skipped**. `i31.wast` was
0 passed / 6 failed / **66 skipped**: the 6 was the visible cost, the 66 was the actual one.

**Apply: when triaging, read the SKIP column too.** A defect that appears as a handful of failures but
sits in a module-level position (a global initializer, a type definition, a section) is worth its file's
skips, not its failures. The three GC files carried 126 skips between them.

## ✅ T9a#4 COMPLETE 2026-08-08 — the funcref encoding, and imported tables

**Suite 61,802 / 472 / 2,466 → 61,887 / 457 / 2,339 — 99.3%.** +85 passes, −15 failures, **−127 skips**.
`elem.wast` **63/13/17 → 75/6/0**, `imports.wast` **196/13/95 → 230/13/26**, `linking.wast`
**107/11/28 → 131/4/8**, `table_grow.wast` → **50/0/0**, `imports4.wast` → **11/0/0**; `imports0`,
`linking0`, `linking3` to zero skips. No file lost a pass. 408 workspace tests.

**Option 1 of the T9a#4 gate, implemented: a `funcref` now carries the instance that produced it.**

```text
  bit 63      bits 62..32           bits 31..0
  0           instance index        function index
  ^ MUST be 0 — it is I31_TAG
```

⚠️ **The obvious layout (instance in bits 32..63) collides with `I31_TAG`** — recorded before the work
started, and the reason the field is 31 bits. `NULL_REF` is all 64 bits set, so it stays distinct too.

**The property that made this safe to introduce: instance 0 packs to the bare index.** Every
single-instance program keeps bit-identical values, so landing the encoding alone moved the suite by
exactly **+1/−1** — one genuine cross-instance case in `elem.wast` that had been wrong. A value-model
change that can be verified to be a no-op on the existing corpus is worth arranging deliberately.

Sites: producers (`ref.func`, element segments, const-exprs — all stamped with the defining instance)
and consumers (`call_ref`, `call_indirect`, `ref_matches`). `call_indirect` and `call_ref` now dispatch
into the funcref's **owner**, so the callee runs against its own memory and globals. `ref_matches` gained
the instance table because a funcref's *type* lives in the owner's module, not the testing one.

**Then imported tables**, with §4.5.9 matching (element type **equal** — a table is mutable, so a
narrower actual type would let the importer write what the exporter's type forbids) and the spectest
`table`. `Linker::define_table` publishes one by name.

### 🆕 It caught a defect in the memory work from earlier the same day

**A table or memory *instance*'s type has `min = its CURRENT size`, and `grow` updates it (§4.5.9).**
The memory pass stored the **declared** minimum and asserted in a test that growth could not change what
links. No memory case in the suite contradicted it. The table case did, and says so in its own comment:

> `;; imported table limits should match, because external table size is 2 now`

Both now read the current size, which equals the declared minimum until something grows. The wrong test
was rewritten as the positive statement of the real rule. ⚠️ **A test can encode a misreading of the
spec and pass forever if no case exercises it** — the sibling feature is what found this, not review.

### ⚠️ Unattributed: ~5% steady-state regression, handed to T11

Cold start is unchanged (~4.46 vs ~4.50 ms A/B/A). **Steady state measured ~5% slower** — ~55.7 ms vs
~52.7 ms on `sum(1000000)`, consistent across A/B/A — **and I could not attribute it.** The steady loop
is `loop`/`br_if` with i32 adds; it touches no funcref, no table and no type. Two hypotheses were tested
and **both rejected**: moving the new `TypeRegistry` to the end of `Pools` (so the hot fields keep their
offsets) did not recover it, and boxing it so `Pools` grows by 8 bytes rather than ~56 did not either.

The remaining likely cause is **code layout** — the interpreter's `run` is one enormous match, and adding
instructions to two arms shifts register allocation and I-cache alignment for the whole function. That is
a known effect and exactly what **T11** is scoped to examine ("dispatch shape in `interp.rs`"). Recording
it as measured-but-unexplained rather than asserting the cause.

## ✅ Fixed 2026-08-08 — the same type-use rules at their other two sites (`call_indirect`, functions)

**Suite 61,778 / 496 / 2,466 → 61,802 / 472 / 2,466 — 99.2%.** +24 passes, −24 failures.
**`call_indirect.wast` 158/11 → 169/0/0** and **`func.wast` 147/21 → 160/8**. No file lost a pass, none
gained a failure, `.wat` corpus held at 533/534. 405 workspace tests.

**Block types, `call_indirect` and function definitions each had their own copy of the type-use loop** —
and each therefore shipped its own copy of all three defects (clause order, named parameter, inline
signature silently overridden by the `(type x)` beside it). `parse_type_use` is now the **single authority**
for block types and `call_indirect`; the function-definition path enforces the same order inline, because
its loop also owns `import`/`export`/`local` and the body and so cannot delegate wholesale.

**Two copies of a grammar drift. Three copies drifted identically** — which is a stronger argument for one
authority than any amount of reasoning about it.

### ⚠️ A rule attempted, measured, and WITHDRAWN — and it looked obviously right

**"No declaration may appear after the body begins"** (`(func (nop) (local i32))` is malformed). Implemented
by scanning the items after the header loop for a `type`/`param`/`result`/`local` keyword. It broke
**`select.wast`, `stack.wast` and `call_indirect.wast`**: in **flat** instruction form each immediate is its
own top-level item, so `select (result i32)` and `call_indirect (type $t)` put a `result`/`type` form exactly
where a misplaced declaration would sit. **Keyword scanning cannot tell an immediate from a declaration at
that layer** — deciding it needs the body's instruction structure, which the header loop does not have.

Withdrawn rather than forced. `(func (nop) (local i32))` and its three siblings still assemble: **~4
assertions, open**, and worth revisiting only alongside a body-structure-aware pass. **A rule that is
obviously right is still a hypothesis until it is measured.**

**Still open in this cluster:** `func.wast` 8 — the withdrawn body-order rule (~4), duplicate **identifiers**
(3 "duplicate local", 1 "duplicate func" — a distinct rule, untouched), 2 wrong results, and 2 malformed
import names the decoder accepts.

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

- ~~**Trap backtraces are still empty, and the C ABI now says so out loud.**~~ ✅ **RESOLVED at T9a#7
  (2026-08-08).** The T8 bet — freeze the frame API's shape now, fill it in later — paid off: real
  frames landed with **no ABI change at all**. See defect 4 below.
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

### 4. Trap diagnostics are one line — no backtrace ✅ FIXED 2026-08-08 (T9a#7)

Both runtimes now report the same shape:

```
wasmrt  stderr: wasmrt: trap: unreachable executed
                  at fn[0] +0x22
                  by fn[1] +0x26
                  by fn[2] +0x2b
                  (no name section: rebuild the guest unstripped for symbols)
```

The missing piece had been the **byte-offset work deferred at T2**. `Instr` now carries an
`offset: u32` that is **free** — it lands in padding that `Imm`'s 16-byte alignment already forced, so
`Instr` is still 80 bytes, and a `size_of` test pins that so a future `Imm` shrink fails the build
rather than silently starting to pay 16 bytes per instruction. Offsets are reported **absolute from
the start of the module**, the form `wasm-objdump` prints, so nothing needs rebasing.

Frames are built **on the way out** — pushed as the error passes back through `call_function`, not
maintained as a shadow stack — so the machinery costs nothing until something actually traps. They are
cleared per invocation and when EH catches, so a caught exception never leaves frames behind for the
next real trap to inherit.

The C ABI needed **no change**: `wasmrt_trap_frame_count` / `wasmrt_trap_frame` were frozen in their
final shape at T8 and simply went live — the T8 bet paid off exactly as designed. A `wasmrt_trap_t`
**copies** its frames, since it outlives the single backtrace the engine keeps.

⚠️ The plumbing was not free by default: threading `pc: &mut usize` through `run` **measured 3.6%
slower** on the steady-state loop. A one-shot closure recovered it. See `best-practices.md` §1.7.

*(Diagnostics still go to **stderr** — a deliberate divergence, not a bug:
`wasmrt wasi prog.wasm > out.txt` then captures only guest output, whereas the oracle would mix its
trap report into that file.)*

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
