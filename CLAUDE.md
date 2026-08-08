# wasmrt

An idiomatic-**Rust** WebAssembly runtime — a port of the Zig runtime **`wazmrt`** (`../wazmrt`),
built to **replace wasmtime** as the engine beneath the owner's `universalWasmLoader-*` projects.

## ✅ GATE — OPEN (2026-07-27). The port has begun.

The gate condition is **met**: `wazmrt` reached **full parity** (every wasm proposal it targets is
implemented; it runs the whole wasmtk WASI suite; ~60k spec-testsuite assertions pass; the WAT
assembler is gap-free; the C ABI is complete 319/319) and **`zig build test` passes** (489/493, 4 skip;
green under Debug AND ReleaseSafe). The oracle is **frozen** at `wazmrt@dadc727` — recorded in
`scripts/wazmrt-baseline.txt`. **Port code is now permitted.** Follow the phased task list in
[`cmem/roadmap.md`](cmem/roadmap.md), parity-gated against the frozen oracle at each step.

`scripts/check-wazmrt.sh`'s role has **inverted**: it no longer waits for freeze — it detects whether
the frozen oracle has **drifted** under the in-flight port. A "CHANGED since baseline" result means:
review the new wazmrt commits, decide whether the port must follow, then re-baseline deliberately.

## Where the port actually is (keep this line current)

**T0–T8 DONE (published through v0.9.0); T9 IN PROGRESS — four passes landed 2026-08-08, unreleased.**
wasmrt assembles, decodes, validates, runs, does WASI preview 1 with a sandboxed filesystem, and is
**embeddable from C** via `wasmrt.h`. Spec suite **99.1%** (61,712 / 578 / 2,469 of 62,290) — **first time
over 99%** — **395 workspace tests**, no file lost a pass. **T9b/T9c/T9d done, so all three success axes carry a real
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
🔧 **That measurement names the next item: no type canonicalisation** — wasmrt compares concrete types by
**index**, the spec by **structure**, so identical rec groups are two types. **~40 assertions** across
`type-subtyping`/`type-rec`/`type-equivalence`; the largest in-scope cluster left, and a feature not a patch.
**Still open in T9:** T9a#4's **table** half (🚦 decision-gate), **type canonicalisation (~40)**, #5, #7,
#8, #9, #12's **text-parser** remainder (`func.wast` 21, in `wat.rs`), `pin`, **tail calls**.
Then **T10** bug hunt, **T11** optimization review (**no longer blocked — its baselines now exist**),
**T12** security review — the order **measure → find → optimize → attack** is deliberate.
⚠️ **T9a#1 taught that a cost logged beside a defect is a hypothesis about its cause**: the `ref.null`
fix was real but did not move `br_table.wast`, which needed three further unlisted fixes. Detail is in
[`cmem/roadmap.md`](cmem/roadmap.md); [`cmem/INDEX.md`](cmem/INDEX.md) carries the fuller status.

## Project memory lives in `cmem/` — read it first

Portable project memory (committed, travels with the repo) is in **[`cmem/`](cmem/)**, one topic file
per domain — same convention as wazmrt. **Start with [`cmem/INDEX.md`](cmem/INDEX.md).**

- [`cmem/overview.md`](cmem/overview.md) · [`cmem/vision.md`](cmem/vision.md) — what/why (canonical / fast / small)
- [`cmem/architecture.md`](cmem/architecture.md) · [`cmem/design-decisions.md`](cmem/design-decisions.md) — crates, dual-target, invariants NOT to drift
- [`cmem/loaders.md`](cmem/loaders.md) — the `universalWasmLoader-*` consumers + the `wasmrt.h` surface
- [`cmem/testing.md`](cmem/testing.md) · [`cmem/security-model.md`](cmem/security-model.md) · [`cmem/licensing.md`](cmem/licensing.md)
- [`cmem/roadmap.md`](cmem/roadmap.md) · [`cmem/known-issues.md`](cmem/known-issues.md) · [`cmem/reference-projects.md`](cmem/reference-projects.md)
- [`cmem/releasing.md`](cmem/releasing.md) — versioning ladder + the binding per-release checklist

Detailed engineering reference (the wazmrt deep-read maps) is in [`docs/port/`](docs/port/).
⚠️ **`docs/port/wasmrt.h.draft` is HISTORICAL** — four of its shapes never matched the code. The
finalized C ABI is [`crates/wasmrt-capi/include/wasmrt.h`](crates/wasmrt-capi/include/wasmrt.h).

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
