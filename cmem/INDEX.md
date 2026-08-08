# cmem — Portable Project Memory for wasmrt

This folder is the **authoritative, portable project memory** for `wasmrt`. It lives inside the
project tree, so it travels with the project (USB drive, clones, GitHub) and is **committed to git** —
unlike a machine-local `CLAUDE.md`. Mirrors the `cmem/` convention of the source project `wazmrt`.

**Format:** plain Markdown — one focused topic file per domain, so any single concern can be reviewed
and revised without wading through one giant file. Keep files small and single-topic.

**What `wasmrt` is:** an idiomatic-**Rust** port of the Zig WebAssembly runtime **`wazmrt`** (sibling
repo `../wazmrt`), built to **replace wasmtime** as the engine beneath the owner's
`universalWasmLoader-*` projects. See [overview.md](overview.md).

## ✅ GATE — OPEN (2026-07-27). Port in progress — assemble → decode → validate → run → WASI → embed-from-C all working. (read first)

The gate is **open**: `wazmrt` reached **full parity** and **`zig build test` passes** (489/493, 4 skip;
Debug + ReleaseSafe green), so the passing Zig build is now a **frozen** reference oracle at
`wazmrt@dadc727` (`scripts/wazmrt-baseline.txt`). **Runtime port code is permitted** — follow the phased
conversion task list in [roadmap.md](roadmap.md), parity-gated at each step. `scripts/check-wazmrt.sh`
now watches for **oracle drift** (a change since the freeze), not for freeze-readiness. Scope note: the
oracle covers every wasmrt-target feature **except the tail-call proposal** (`return_call`/
`return_call_indirect`) — oracle those against **wasmtime + the spec testsuite**. memory64 **is** in
scope (owner, 2026-07-27). See [design-decisions.md](design-decisions.md).

## ◐ T9 IN PROGRESS (2026-08-08) — second pass landed, unreleased

Working tree is **ahead of the published v0.9.0**. Suite **61,593 / 697 / 2,469 — 98.9%** of 62,290
adjudicated; **375 tests** (was 363); clippy, all four surfaces, the C-ABI gate (74/74 + `c_smoke`) and
Miri (26/26) green. **No file lost a single pass** — the check that matters when skips convert to verdicts.

### 2026-08-08 — T9a#4: imported **memories** ship; **tables** stay refused (owner chose option 2)

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

**Still open in T9:** T9a #4's **table** half (gated), #5, #6, #7, #8, #9, #11, #12 · **T9e `pin`** (still
a stub — a build performs **no** authenticity check) · **T9f tail calls** (1.0 cannot be claimed without
them).

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
| [overview.md](overview.md) | What wasmrt is, the oracle, planned repo/crate layout, mental model, current status |
| [vision.md](vision.md) | The goal — **plus where the three axes actually stand, now that ALL THREE are measured (2026-08-07)**: *canonical* 98.9%; *fast* **4.48 ms cold at 48 KB / ~237 Mops/s steady**; *small* **CLI 621 KiB, cdylib 493.5 KiB, freestanding wasm32 engine 158.1 KiB → 137.5 KiB with `wasm-opt -Oz`**. What is still missing on fast/small is the **comparison to another runtime** (wasm3/WAMR/Deno), which needs those binaries present. Still: do not quote wazmrt's benchmarks as wasmrt's. fast + smallest-binary + wasm-compilable runtime that **replaces wasmtime** under the `universalWasmLoader-*` projects |
| [architecture.md](architecture.md) | Rust architecture — **the T8 C ABI** (two handle kinds: opaque pointers you own vs. **checked value handles** that carry their issuing store, so a foreign/stale one is refused not followed; `ffi.rs` as the crate's ONLY raw-pointer boundary; the env finalizer behind an `Rc` so teardown order cannot matter); `wasmrt-core` (no_std-friendly) + `wasmrt-capi` (cdylib/staticlib, the `wasmrt.h` surface) + `wasmrt` CLI; decode→validate→instantiate→execute; the dual-target contract; the shared opcode IR seam; **the T7 shared store** (`code` + `pools` as separate fields so cross-instance calls borrow disjointly — no `Rc`/`RefCell`/`unsafe`) |
| [design-decisions.md](design-decisions.md) | Load-bearing decisions + invariants NOT to drift — the **4 T8 header answers (owner, 2026-08-06)**: real proposal gating (not limits-only), the linker in **core**, raw memory pointer + checked copies, trap-frame shape now / backtraces at T9 — and why there is **no tail-call flag**; the 🔒 **safety directive** (no unsafe constructs migrated from Zig; prove the concept BEFORE hardening; **ENFORCED at v0.8.0**: `#![forbid(unsafe_code)]` in core + CLI, `deny` + one justified `allow` in capi, mutation-verified); boundary-faithful/idiomatic-Rust; **public API = own `wasmrt.h`** (clean `wasmrt_*` + native Rust crate, NOT wasm-c-api/wasmtime symbols); **feature scope = full wasmtime browser-standard parity + memory64, WASI p1 only**; Option-A interpreter; the ValType/slot/opcode/trap invariants; size levers; the collapsed oracle split (tail-calls only); **all 4 deferred decisions are now RESOLVED** (crate split T0, `random_get` + resolver T7, header shape T8) — the gate queue is empty |
| [loaders.md](loaders.md) | **The consumers.** How `universalWasmLoader-*` work (hand-rolled Canonical ABI over core modules + WIT sidecar), the ~38-fn engine surface that drives `wasmrt.h`, the caller-based host-callback requirement, the 3 substrates + 10 targets (phased), and **what the shipped `wasmrt.h` actually gives them** (T8 / v0.9.0) — including the one caveat to carry into the loader ports: `wasmrt_caller_get_memory` always returns false, so callbacks use `wasmrt_caller_read`/`_write`. The v0 draft is **historical**; four of its shapes never matched the code |
| [testing.md](testing.md) | Parity/oracle strategy — **live conformance 98.9% (61,593 / 697 / 2,469 of 62,290) and 375 tests as of 2026-08-08**; the 08-08 column (+346 passes / −463 skips, **no file losing a pass**) and why **a blanket skip is not a neutral placeholder** — `assert_unlinkable` was skipped for a reason obsolete since T7b and was insuring a real engine defect; the runner now separates *"nothing defines this import"* (a real unlinkable verdict) from *"wasmrt cannot back this kind"* (a gap → skip), which is exactly what had made the assertion unimplementable; the 08-07 column split into *real capability* vs *honest re-accounting* (the runner's failed-build redirect was inflating failures); the **T9c benchmark** and why it asserts its own result before timing; the methodological finding that **a cost logged beside a defect is a hypothesis about its cause**; **the three C-ABI gates and what each catches that the others cannot** (link completeness / header-is-valid-C / Miri sees a UAF a normal allocator cannot); the mutation check on handle tagging; **351 tests**; Rust↔wazmrt golden vectors for shared features; **the vendored spec testsuite + its live conformance numbers (98.8% — 61,033/738/3,075 at v0.9.0; all 284 files parse) and where it lives**; **why the total adjudicated count moves when modules start building** (a module that fails to build costs exactly ONE skip, not one per assertion); `wasmrt-capi` under Miri + a lifecycle fuzz; a wasi-gate compiling real guests; bench cold vs steady; DoD = full parity on both targets |
| [security-model.md](security-model.md) | 🔎 **A dedicated adversarial review is scheduled — T12 (0.13.0)**, framed on three adversaries (hostile guest / malformed input / misusing embedder); two surfaces already listed from the release profile alone: `panic = "abort"` makes any reachable panic a **host-process kill**, and no `overflow-checks` in release means arithmetic **wraps there and panics in debug**. ⚠️ **`pin` is a STUB in wasmrt — a build today performs NO authenticity check**; that section describes the ORACLE, not current behaviour (T9). The *authority* half (the WASI sandbox) IS built. **Two new authority controls at T8:** proposal gating + resource ceilings, both reachable from C. Sandbox **secure by construction** + ⚠️ the **CORRECTED resolver note (2026-08-05)**: Rust's `std` has **no** dir-relative open, so zero-dep + no-`unsafe` + hold-real-handles cannot all hold. Shipped a component-accumulated path — every **escape** property intact, **inode pinning** lost, `verify_beneath` compensating; closing the residual was **DECIDED 2026-08-05: accept + document** (zero-dep and zero-`unsafe` both held). Also: rights-narrowing preopens; the mandated canary test (mutation-verified); pin verify (hash the in-memory bytes you run; root-owned DB; enforce denies before opt-out); authenticity vs authority |
| [licensing.md](licensing.md) | **Verified 2026-08-06: all four license files exist since T0, the Component Ledger is EMPTY and there are ZERO third-party dependencies.** The `cap-std` question is **CLOSED** (rejected 2026-08-05), not open. Where SPDX actually is, per file — **the `README.md` gap was closed 2026-08-07 (T9d), so the convention is now fully satisfied with no remaining gaps.** **License = `MIT OR Apache-2.0`** (dual). Carry `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/`third_party/LICENSES.md` verbatim, name wazmrt→wasmrt, "Jon Marcum" 2026. Vendored-`wasm.h` attribution only if that code is reused (it is NOT — we ship our own `wasmrt.h`) |
| [reference-projects.md](reference-projects.md) | **wasmtime SHAPED `wasmrt.h` — shape only, no code/symbols/headers taken**; the wasm-c-api `wasm.h` was NOT vendored even at the C ABI, the moment that shortcut was most tempting. The runtimes evaluated (same set as wazmrt) + **wasmtime is the feature-parity target** ("run what it runs") and the thing being replaced under the loaders. 100% original |
| [roadmap.md](roadmap.md) | **T9 IS IN PROGRESS — second pass landed 2026-08-08**: T9a#4's **memory half ✅** + `assert_unlinkable` + link-time import type checking. First pass (08-07): T9a #1/#2/#3 (+3 unlisted defects), **T9b size ✅ measured**, **T9c perf ✅ measured** (`examples/bench.rs`), **T9d licensing/docs ✅ done**. Open: T9a#4's **table** half (🚦 decision-gate), #5–#9, #11, #12, T9e `pin`, T9f tail calls. Original scoping (2026-08-06) — T9a correctness punch-list (12 items, each with its measured cost, ordered by value), T9b size, T9c perf, T9d licensing/docs, T9e `pin`, T9f tail calls (the last unimplemented IN-SCOPE proposal — 1.0 cannot be claimed without them), T9g scope confirmations (what to leave failing, and why the in-scope residual is far smaller than 738/3,075 suggests). **T10 (0.11) = bug hunt + code hygiene**, **T11 (0.12) = optimization review** and **T12 (0.13) = security review** were added by the owner 2026-08-06; the ordering **measure → find → optimize → attack** is deliberate, and T12 records that `panic = "abort"` makes any reachable panic a host-process kill. Current status (**PORT phase; T0–T8 done, published through v0.9.0, oracle frozen @dadc727**) + the **conversion task list** T0–T12 (scaffold → types/reader → opcode → decode → validate → interp slices → text → wasi → C-ABI → hardening → bug hunt → optimization review → security review), parity-gated, with the 4 decision-gates inlined (**all now resolved**) and T9 scoped to what actually remains. Loader phases 1–4 |
| [known-issues.md](known-issues.md) | Issue tracker — ✅ **T9a#4's memory half DONE 2026-08-08** (owner chose option 2: memories ship, tables stay refused), with the three non-obvious details — `module.memories` is the whole index space so only its tail is allocated; active data segments fork on the index so a later failure leaves no orphaned slots; limits matching compares **declared** types, so `Memory` carries its `min`. **Plus what unskipping `assert_unlinkable` found: imports were NEVER type-checked at link time** (silent-wrong-call), now fixed in the two places the type is known, with the equality-vs-subtyping trade **measured** rather than argued. ⚠️ **The obvious funcref packing is wrong — bits 32–63 collide with `I31_TAG`.** New open items: no cross-module type canonicalisation (1 assertion), host imports unchecked by construction, and malformed modules reaching the *link* stage. Also: ✅ **T9a first pass FIXED 6 defects (2026-08-07)**, three of which were not on any list: `Op::MemorySize` reading another instance's memory (mutation-verified fix), the `.wast` runner's failed-build redirect, `ref.null $t` (**plus a silent-wrong-value bug in the same arm — `nofunc` assembled as `func`**), the inline table-elem shorthand, an **undecodable concrete-ref block type**, and **`br_table` label typing wrong in two directions**. ⚠️ **`ref.null`'s logged cause was WRONG** — it did not move `br_table.wast`, which needed all four assembler/decoder/validator fixes; treat every remaining cost figure as a hypothesis. 🚦 **NEW DECISION-GATE: T9a#4 is not plumbing** — a `funcref` carries no instance identity, so an imported *table* would dispatch to the wrong function; imported *memories* are safe, imported tables need the value encoding decided. Plus: the **2 silent-wrong-output defects fixed at T8** (dropped table initializers; element-segment form 4 rewriting a type) **and the third-order lesson**: a check that is harmless "because the case cannot occur" becomes a bug the moment it can; what T8 closed and what it left open; the **ranked pre-T8 punch list** (2026-08-05), the **5 open defects the wasmtk corpus surfaced** (`ref.null $T` unassemblable · 2 validator gaps the oracle passes · no trap backtrace · an unconditional `data_count` section), the fixed shared-store and literal/text defect write-ups, the port's intentional divergences (single-threaded atomics, `delegate` rejected everywhere, the debug-build stack-depth finding), the wazmrt residuals (#17/#18/#23), and the open decisions |
| [releasing.md](releasing.md) | **Versioning + publishing.** Port-progress ladder (`0.x` → `1.0` = full oracle parity); the 3 crates share one version, CLI is published as `wasmrt`; per-task manual releases; the per-release checklist + the 🔒 binding **pre-publish doc-sync gate** (full `cmem/` + ROADMAP/CHANGELOG/README committed BEFORE the owner is notified to publish — owner directive 2026-07-31); crates.io names reserved |

## Related files outside cmem

- `README.md` — the public, user-facing doc. NOT project memory.
- `ROADMAP.md` + `CHANGELOG.md` — the **public progress trackers** (stage ladder + use-case matrix +
  release notes). Kept in lockstep with each release per the `releasing.md` trigger. NOT project memory.
- `docs/port/*.md` — the detailed wazmrt deep-read maps (6 subsystems) + the loader survey + `wasmrt.h.draft`.
- `scripts/check-wazmrt.sh` + `scripts/wazmrt-baseline.txt` — the oracle monitor. Baseline frozen at
  `wazmrt@dadc727` (2026-07-27); the script now watches for **drift** from the freeze, not readiness.
- `LICENSE` — current; `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/`third_party/LICENSES.md` to be added at scaffold time.
