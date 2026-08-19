# Design Decisions & Invariants

Load-bearing decisions for the `wasmrt` port. **Do not silently revert these.** Detail + rationale:
`docs/port/` (esp. `00-synthesis.md`, `06-build-docs-licensing.md`).

## 🔒 The oracle is RETIRED — wasmrt stands alone (owner, 2026-08-11)

**wasmrt no longer refers back to the `wazmrt` repo.** Through T9 wazmrt was a frozen oracle and
`scripts/check-wazmrt.sh` watched it for drift; that script is **deleted** and its baseline is kept only
as `scripts/wazmrt-provenance.txt`, a historical record nothing reads.

Why, precisely: **the two runtimes are now independent entrants** competing for inclusion in **wasmtk**
and the **universalWasmLoader-\*** runtimes, decided on **the smallest and fastest binary**. `rsxtk`
takes wasmrt by default through the native Rust interface. wazmrt is simultaneously running its own
size/self-ownership program for the same contest — so its head is a **competitor's** design, and
"following the oracle" would now mean adopting the choices of the thing wasmrt must beat.

What replaces it, and why this is a **stronger** anchor rather than a weaker one:

- **Correctness** → the official spec testsuite (62,616 adjudicated assertions), **wasmtime's observable
  behaviour** (already the reference for invalid-module diagnostics, matched byte-for-byte), and the
  wasmtk WASI corpus. The oracle was never the hardest of these; it was the most convenient.
- **Completion** → `1.0.0` no longer means "parity with wazmrt". It means every in-scope proposal
  implemented, conformance at its achievable ceiling, the C ABI stable, and the size/speed numbers
  measured and defended (`releasing.md`, `ROADMAP.md`).
- **Selection** → the three axes in `vision.md` are no longer aspirations with a footnote; **fast and
  small are now the gate.** See the ⚠️ there: three gaps that were acceptable footnotes are now the
  critical path, chief among them that **nobody has ever measured what rsxtk actually links.**

⚠️ **The one thing NOT retired is provenance.** wasmrt is derived from wazmrt in design and both are the
owner's under the same dual MIT/Apache licence, so there is no third-party obligation — but the history
is real and `licensing.md`'s attribution stays. *Retiring a gate is not rewriting where the work came
from.*

⚠️ **A caveat this project has paid for twice:** wazmrt's punch-list entries are no longer evidence about
wasmrt. Two T9 items ("the oracle runs this, so our type-checker is wrong") turned out to rest on a
misread of what the oracle's subcommand did — `best-practices.md` §2.3a. With the oracle gone, that
whole class of false lead goes with it; do not reintroduce it by reasoning about what wazmrt "would do".

## Port-level decisions (owner, 2026-07-17)

*Historical framing — these were the decisions that governed the port. The oracle-parity clauses are
superseded by the section above; the engineering choices they produced stand.*

- **Boundary-faithful, idiomatic Rust.** The observable behavior/outputs and the `wasmrt.h` contract
  are fixed (verified by Rust↔oracle parity **through T9; since 2026-08-11 by the spec testsuite and
  wasmtime**); internals are idiomatic Rust (ownership, enums, `Result`), not a Zig transliteration.
  File/module structure mirrors wazmrt for reviewability.
- 🔒 **"wasmtime's SHAPE, our code" — the standing rule, reaffirmed by the owner 2026-08-08.** Where
  wasmtime has already solved a structural problem, wasmrt adopts the **architecture** and writes its own
  implementation: no code, no symbols, no headers, no data structures transcribed. Three applications so
  far — the `wasmrt.h` surface (T8), the **shared store** (T7b), and the **engine-level type registry**
  for cross-module type identity (T9h, approach approved 2026-08-08). **The Component Ledger stays
  empty** and wasmrt remains 100% original Rust: `INDEX.md`'s "evaluate a reference project" trigger
  requires a ledger entry for *copying or porting code*, and reading an architecture is explicitly free.
  The test of compliance is that our version differs where our constraints differ — the C ABI's checked
  value handles instead of refcounted objects, the store's `code`/`pools` split for disjoint borrows,
  zero dependencies, `forbid(unsafe_code)`. If a design cannot be re-derived under those constraints,
  that is the signal it was being copied rather than understood.
- **Public API = wasmrt's own `wasmrt.h`** — NOT the standard wasm-c-api, NOT wasmtime's exact symbols.
  Strategy: **clean lean `wasmrt_*` C ABI (wasmtime-*shaped*) + a native `wasmrt` Rust crate.** No
  exact-wasmtime compat shim. This drops the entire wasm-c-api refcount object model (wazmrt's
  highest-risk file) — big win for size and safety. See [loaders.md](loaders.md).
- **Feature scope = full wasmtime browser-standard parity + memory64, WASI preview 1 only.** In: MVP,
  reference types, multi-table, bulk memory/table, extended-const, function-references, full WasmGC,
  sat-trunc, **SIMD, multi-memory, threads/atomics, tail calls, exception handling (both exnref and
  legacy), and memory64**. Out: WASI p2/p3 and the **component model** (non-browser-standard).
  **memory64 added to scope 2026-07-27** (owner) — the frozen oracle implements it, so "match the
  oracle" now includes it. This **overrides** wazmrt's earlier narrower "browser-standard bar defers
  SIMD/etc." rule.
- **Size is a first-class goal:** minimize every artifact. Levers — `opt-level="z"` + LTO +
  `codegen-units=1` + strip + `wasm-opt -Oz` for the shipped wasm; pick per artifact.
- **Oracle split — re-checked at the 2026-07-27 freeze; it has all but collapsed.** Features wazmrt
  implements → parity Rust↔wazmrt (golden vectors); at the freeze that is **everything wasmrt targets
  except the tail-call proposal** (`return_call`/`return_call_indirect` — wazmrt has `return_call_ref`
  via function-references but not base tail calls). **Tail calls alone** → oracle against **wasmtime +
  the official spec testsuite**. SIMD, multi-memory, threads/atomics, memory64, and EH (both encodings)
  are now on the wazmrt side of the split (they were on the wasmtime side pre-freeze). See
  [testing.md](testing.md).

## Interpreter = Option A + the perf ladder (from wazmrt)

Switch over a pre-decoded IR; untyped `u64` slots; one shared opcode table for validate + interp +
assembler-in-reverse. Interpreter, **not** JIT/AOT (smallest-binary + wasm-freestanding self-embed).
Perf ladder: A → A.5 (superinstructions / partial-eval) → B (register machine) → JIT (native-only,
later). Decide A→B with benchmarks, not upfront.

## 🔒 Safety directive — no unsafe constructs migrated from Zig (owner, 2026-08-04)

**The rule.** The Rust codebase shall carry **no unsafe constructs inherited from the Zig oracle.** A
boundary-faithful port must reproduce wazmrt's *observable behavior*, never its *memory-safety posture* —
Zig's manual allocation, raw pointers, arena lifetimes and `@intCast` (UB in ReleaseFast) have no
business surviving translation.

**The sequencing constraint, equally binding: prove the concept first.** Do **not** refactor for safety
on unproven code. A hardening pass over behavior that isn't yet demonstrated correct trades one unknown
for two — you can no longer tell a safety regression from a behavior bug. Establish provability
(conformance + parity green), *then* harden. Hence the order: finish **T7**, review the known issues,
*then* take this on before/with **T8**.

**✅ ENFORCED as of 2026-08-05 — the rule is now mechanical, not aspirational.**

- `wasmrt-core`: **`#![forbid(unsafe_code)]`**. `forbid`, not `deny`, deliberately — `deny` can be
  switched off by an `#[allow]` further down, which is exactly how such a rule erodes.
- `wasmrt` (CLI): **`#![forbid(unsafe_code)]`** — it had none either.
- `wasmrt-capi`: **`#![deny(unsafe_code)]`** plus one site-local `#[allow]` on `#[unsafe(no_mangle)]`,
  carrying a written justification (the obligation is symbol-name uniqueness, discharged by the
  `wasmrt_` prefix; it dereferences nothing). `deny` rather than `forbid` precisely so each future
  exception is written down at its site and shows up in review.
- **Verified by mutation:** adding `unsafe { … }` to `rng.rs` fails the build with *"usage of an
  `unsafe` block"*. A lint nobody has watched fail is not enforcement.

The lints cost nothing today — the measured baseline was already **zero `unsafe` blocks or `unsafe fn`
workspace-wide** — which is the whole reason to add them *now*: the next one becomes a compile error
rather than a review comment. **If a future need looks genuinely unavoidable** (the `openat` question in
`security-model.md` is the live one), relaxing the line is an **owner decision**, never a local
work-around.

**What "unsafe constructs from Zig" actually means in a Rust port** — the literal `unsafe` keyword is the
*least* of it, and today's spec-suite run proved the point. The real inherited-hazard surface is:

1. **Panic on guest-controlled input.** Rust turns Zig's UB into a panic, which for an embedder is a
   process abort — a denial-of-service, not a safety win. The suite found exactly this: `unreachable!()`
   on `v128.const i64x2`, and an `item[0]` index on an empty `(item)`. **A library must reject a module,
   never abort its host.** `unwrap`/`expect`/`unreachable!`/direct indexing on any guest-derived path are
   all in scope.
2. **Silent truncation where Zig used `@intCast`.** `as u8` / `as u32` quietly discard bits where the
   oracle would have hit UB. Also found by the suite: out-of-range `i32.const` becoming 0, and
   `v128.const i8x16 0x100` becoming all-zero lanes. Wrong values are worse than rejections.
3. **Unchecked index/length arithmetic** that happens to be in-bounds today.

4. **Fall-through arms that fail to compile off the dev platform.** The `#![forbid]` work surfaced one:
   `path_symlink` defined its result only under `#[cfg(unix)]`/`#[cfg(windows)]`, so the `wasm32` no_std
   build broke. Fixed with an explicit third arm returning `NOSYS`. The dual-target build is what caught
   it — **run it, not just the native one**, before calling a change done.

**The T8 nuance to settle before starting.** The C ABI *cannot* be zero-unsafe: `extern "C"` entry points
receive raw pointers from C callers and must dereference them. So the realistic target is
**`unsafe_code = "forbid"` in `wasmrt-core` (achievable today) and `deny` + narrowly-scoped, individually
justified `allow` in `wasmrt-capi`**, with every such site documented as to what the caller must
guarantee. "Zero unsafe everywhere" would be a promise the FFI boundary makes impossible to keep;
"zero unsafe in the engine, confined and audited at the boundary" is both achievable and honest.

## Invariants that must survive the port (easy to get wrong)

- **`ValType` = a `u32` newtype**, concrete refs bit-packed (bit31 concrete, bit30 nullable, bits28-29
  family func/struct/array, bits0-27 index). NOT a plain enum. All accessors are pure bit ops.
- **Slot encoding:** `null_ref = u64::MAX` checked **before** `i31_tag = 1<<63`; heap/func/extern are
  small indices. Three-way discrimination depends on that order.
- **`Op` discriminants are internal tags ≠ wire bytes** for 0xFC/0xFB-prefixed ops; the `fc/gcSubOpcode`
  reverse maps are the emit-side truth. One shared opcode table; keep values stable or the assembler
  breaks.
- **Two-pass type-section decode** (pre-scan kinds) for rec-group forward references.
- **LEB128 over-long/too-large rejection** transcribed exactly (5th-byte `>>4` + sign bits; 10th-byte
  `v ∈ {0,0x7f}`) — conformance suites probe this.
- **`Instance` retains its `Module`** (UAF fix). Tie module lifetime to the instance (Arc/borrow).
- **Trap-record path `#[inline(never)]`/`#[cold]`** — inlining it into the ~200-arm dispatch loop is a
  measured ~14% i-cache regression. Trap byte offsets resolved **lazily** by re-decoding one body; never
  stored per-instruction.
- **Shared `Memory`/`Table`** so `grow` is visible to importers (growth reassigns the backing slice in
  place). Rust: `Rc<RefCell<…>>` / raw shared ownership; refcount `Cell<u32>`, NOT `Arc` (single-thread
  ABI assumption).
- **C-ABI (capi):** lightweight `{id}` handles into a store (no wasm-c-api refcount model);
  **caller-based host callbacks** so a host import can read/write guest memory and return a value (the
  one capability wazmrt's wasm-c-api callback lacks); `#[repr(C)]` for boundary structs; real pointers
  (`*mut c_void`), never hardcoded `i32`.
- **WASI sandbox secure BY CONSTRUCTION** — never resolve a full guest path string against a dir API;
  resolve one component at a time through held handles (`walkFull`); `..` never rises above the preopen;
  absolute symlink targets re-base to the preopen root; `symlink_max`→ELOOP. Rust may use
  `cap-std`/`openat2(RESOLVE_BENEATH)` to close wazmrt's final-component TOCTOU residual (#17). See
  [security-model.md](security-model.md).
- **Pin verify hashes the in-memory bytes it runs** (bytes-hashed == bytes-run); `enforce` denies
  before consulting the opt-out; DB parse fails closed.

## Port-implementation decisions (as built, T0–T5)

Choices made while porting; consistent with "boundary-faithful behavior, idiomatic-Rust internals."

- **Workspace of 3 crates** (`wasmrt-core` / `wasmrt-capi` / `wasmrt`), **edition 2024**
  (`#[unsafe(no_mangle)]` on the C ABI), size-first `[profile.release]` (`opt-level="z"` + LTO +
  `codegen-units=1` + strip + `panic="abort"`). **Windows build host = `x86_64-pc-windows-gnullvm`**
  (see [architecture.md](architecture.md)).
- **Owned data model over an arena.** The decoder returns owned `Vec`/`String`; a `Module` frees on drop
  (no `deinit`, no allocator-error threading). Idiomatic-Rust divergence, behavior identical.
- **`Op` = a macro-defined `#[repr(u8)]` enum** (PascalCase variants) with `from_u8` generated from one
  wire/internal list; **immediates that own data hold a `Vec`**, so dropping the IR frees them (replaces
  wazmrt's manual `freeBody`). The internal-tag-vs-wire-byte invariant is preserved (raw `0xD7`–`0xFA`
  rejected). Raw `0xC5`–`0xCC` accepted as sat-trunc to mirror the oracle (see [known-issues.md](known-issues.md)).
- **CLI grows with the pipeline** — `wasmrt <file>` summarizes + validates (T3/T4); `wasmrt run <file>
  <fn> [args]` executes (T5), parsing args to the export's param types. Faithful to wazmrt's CLI role.
  🔒 **Every path that loads a module goes through ONE helper** (`read_module_bytes`, T9): it assembles
  `.wat` text before decoding, so `run`, `wasi` and summarize accept the same file types and validate on
  the same terms — three copies of the sniff is how they drifted apart in the first place (`best-practices.md`
  §3.8, §3.4). Dispatch is on the **extension, not the content**, so a malformed `.wat` is reported as an
  assemble failure and a malformed `.wasm` as a decode failure; sniffing content would blame the assembler
  for a corrupt binary. The `wasmrt wat` subcommand keeps its own raw read — it assembles by definition.
- **Sliced the two correctness-critical, hard-to-test-early modules** (owner-approved): **T4 validate**
  and **T5 interp** land core-first with the exotic proposal arms deferred to `.x` patch releases, because
  each is a trustworthiness promise and most of their tests need the WAT assembler (T6). Deferred ops
  **reject loudly**, never silent-accept. See [known-issues.md](known-issues.md).
- **Interpreter internals** (T5): untyped value slots — `u64` through v0.6.4, **widened to `u128` at the
  SIMD slice (v0.6.5)** so a `v128` occupies ONE slot and the engine stays "one slot per value"
  (select/drop/branch-arity/locals/call-marshaling never reason about slot width). wazmrt instead uses two
  `u64` slots + width tables (`slotWidth`/`local_map`/`drop_select_w`); wasmrt's wider slot trades runtime
  memory (16 B/slot) for eliminating the slot-desync hazard class — an idiomatic divergence, behavior
  identical, scalars/refs in the low 64 so the sentinel invariants hold. `Instance` **owns** its `Module`;
  the immutable `module`/`func_bodies` are threaded separately from `&mut Store` so a recursive `call`
  reborrows cleanly (no `RefCell`); control flow via a precomputed `end_of`/`else_of` table + a label
  stack. Float rounding is bit-manipulation (no_std); **`sqrt` is `std`-gated** (the one no_std float gap).
- **Versioning = port-progress ladder**, per-task release to crates.io (see [releasing.md](releasing.md)).

## ✅ RESOLVED (owner, 2026-08-10) — an invalid module is REFUSED, with a wasmtime-shaped diagnostic

**The question:** should a `Module` be able to exist **unvalidated** at all? Raised when `wasmrt run`
turned out to execute without validating (`known-issues.md`).

**The owner's rule for answering it: base it on what wasmtime actually does.** So it was measured
against the real tool — wasmtime **47.0.2**, installed on this machine — rather than argued from the
API surface:

```
$ wasmtime run --invoke f ill-typed.wasm
  Invalid input WebAssembly code at offset 33: type mismatch: expected i32, found i64
```

**The decision: match that action.** Refuse, and be that precise. Three properties, all now
reproduced:

| property | wasmtime 47 | wasmrt |
| --- | --- | --- |
| refuses before executing | yes | yes — every CLI path + the C ABI |
| byte offset, from the start of the module | `at offset 33` | `at offset 33` — **byte-identical** |
| expected vs found | `expected i32, found i64` | same wording |
| which function | *(not printed)* | `(function 0)` — a deliberate superset |

Verified on two modules against the live tool (offsets **33** and **61**), both pinned as tests, so a
future refactor that changes the origin fails rather than silently diverging. Sharing wasmtime's origin
is the point: the two tools' numbers are directly comparable on the same file, which they would not be
if we counted from the body.

**What was NOT changed, and why.** `Instance::new` still accepts a decoded-but-unvalidated `Module` —
the compile/instantiate split. That is now safe in practice because **every shipped entry point
validates** (all CLI paths, `wasmrt_module_new`), so the precondition is only reachable by a Rust
embedder who skips `validate` deliberately, and it is documented as a precondition in the strongest
terms. Making it *unrepresentable* would mean a validated-`Module` type — a breaking API change
carrying real cost (double validation, or a second type through every signature) for a case no shipped
surface can reach. Revisit at **T12** if the security review disagrees.

⚠️ **Method note worth keeping:** the useful comparison was the tool's *observable behaviour*, not its
API docs. Asking "what does wasmtime's `Module::new` do" invites answering from memory; running the
binary on a three-line module answered it in seconds, gave the exact wording to match, and produced two
numbers to assert against. **When a decision is to be based on a reference implementation, run it.**

## ✅ The four deferred decisions — ALL RESOLVED as of 2026-08-06 (kept for the *why*)

These four were "raise before/at scaffolding." At the freeze the owner chose to **defer them as explicit
decision-gates at the relevant task-list step** (see [roadmap.md](roadmap.md)) rather than resolve them
all now — each decided when the port reached the task that needed it. **That queue is now empty**: the
crate split went at T0, `random_get` and the sandbox resolver at T7, and the `wasmrt.h` shape at T8.
Each entry below records the answer and the reasoning, so none of them gets re-derived.

- ~~`random_get`: non-crypto PRNG vs. an OS CSPRNG?~~ **✅ RESOLVED (owner, 2026-08-04): a ChaCha20
  CSPRNG seeded once from the OS**, matching the oracle (wazmrt moved to ChaCha on 2026-07-20, so parity
  *means* a CSPRNG). Zero dependencies, no `unsafe`, auditable, and it still works on the freestanding
  `wasm32` self-embed target where a syscall has nothing to call. **If OS entropy is unavailable, fail
  loudly — never emit predictable bytes**, the one failure mode that turns a CSPRNG into a security hole.
- ~~Zero-dep vs. `cap-std`/`openat2` for the sandbox path resolver?~~ **✅ RESOLVED (owner, 2026-08-04):
  the zero-dep handle-stack walker** (wazmrt's `walkFull`). Resolve a path **component-by-component
  against open directory handles, never re-opening by full path** — that removes the TOCTOU window
  rather than checking for it, which is how wazmrt's **#17** gets closed *by construction*. Rejected
  `cap-std`/`openat2`: a large dependency tree against the smallest-binary goal, strongest on Linux and
  uneven elsewhere, and it would be wasmrt's first runtime dependency.
- ~~`wasmrt.h` review (naming, the store simplification, the `{id}`-handle model).~~ **✅ RESOLVED
  (owner, 2026-08-06) — four answers, two of which changed the plan:**
  1. **Config gates proposals FOR REAL**, not just resource limits. The draft's per-proposal toggles
     would have been silent no-ops (core had no feature gating at all), so the owner chose to build the
     gating rather than drop the toggles — new work threaded through the validator. Limits are
     configurable too. **Corollary the owner should not be surprised by later: there is no `tail_call`
     flag**, because `return_call`/`return_call_indirect` are not implemented; a toggle for them would
     gate nothing while reading as a security control.
  2. **The linker lives in `wasmrt-core`**, not in the C-ABI crate — so the C ABI, the native Rust
     crate, WASI and the `.wast` runner share **one** name-resolution authority. Binding two same-kind
     imports in the wrong order links fine and misroutes every call, so it is written once.
  3. **Memory: raw `uint8_t*` primary + bounds-checked `read`/`write`.** Zero-copy is what the loaders'
     hand-rolled Canonical ABI marshalling needs; the checked pair is there for embedders who would
     rather not reason about the invalidation rule.

  4. **Trap frames: ship the API shape now, real backtraces at T9.** `wasmrt_trap_frame_count` returns
     0 until byte offsets are recorded. Committing the shape now avoids a breaking ABI change later,
     and reporting nothing is better than reporting a plausible-looking wrong frame.
     ✅ **VINDICATED at T9a#7 (2026-08-08): real frames landed with NO ABI change** — not one signature
     moved. Worth generalizing: **when a feature's data is missing but its shape is knowable, freezing
     the shape early and returning honest emptiness costs nothing and buys a non-breaking fill-in.** It
     works because the *shape* question (what does a frame consist of?) was answerable at T8 while the
     *data* question (what are the byte offsets?) was not — separate them and only the second waits.

  Delivered 2026-08-06 as v0.9.0 (T8); the resulting invariants are in [architecture.md](architecture.md).
- ~~core+capi crate split vs. a single multi-target crate.~~ **✅ RESOLVED (owner, 2026-07-27, at the T0
  scaffold): a workspace of THREE** — `wasmrt-core` (`no_std`-friendly, `default=["std"]`),
  `wasmrt-capi` (`staticlib`+`cdylib`+`rlib`, ships `include/wasmrt.h`) and `wasmrt` (the CLI bin). It
  earned its keep at T8: because capi is a separate crate, `wasmrt-core` can stay
  `#![forbid(unsafe_code)]` while the C boundary — which cannot be — is `deny` in a crate of its own.
  A single multi-target crate would have forced one lint posture on both.
