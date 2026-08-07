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

**Progress (2026-08-06): T0–T8 DONE; ✅ PUBLISHED through v0.9.0** (release commit `a7abd83`, tag
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
**Next: T9 — licensing, docs, size, all gates green.**

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
| [vision.md](vision.md) | The goal — **plus where the three axes actually stand at v0.9.0**: only *canonical* is measured (98.8%); *fast* and *small* are architecturally set up but **unproven, with numbers due at T9** — do not quote wazmrt's benchmarks as wasmrt's. fast + smallest-binary + wasm-compilable runtime that **replaces wasmtime** under the `universalWasmLoader-*` projects. The three success axes: **canonical / fast / small** |
| [architecture.md](architecture.md) | Rust architecture — **the T8 C ABI** (two handle kinds: opaque pointers you own vs. **checked value handles** that carry their issuing store, so a foreign/stale one is refused not followed; `ffi.rs` as the crate's ONLY raw-pointer boundary; the env finalizer behind an `Rc` so teardown order cannot matter); `wasmrt-core` (no_std-friendly) + `wasmrt-capi` (cdylib/staticlib, the `wasmrt.h` surface) + `wasmrt` CLI; decode→validate→instantiate→execute; the dual-target contract; the shared opcode IR seam; **the T7 shared store** (`code` + `pools` as separate fields so cross-instance calls borrow disjointly — no `Rc`/`RefCell`/`unsafe`) |
| [design-decisions.md](design-decisions.md) | Load-bearing decisions + invariants NOT to drift — the **4 T8 header answers (owner, 2026-08-06)**: real proposal gating (not limits-only), the linker in **core**, raw memory pointer + checked copies, trap-frame shape now / backtraces at T9 — and why there is **no tail-call flag**; the 🔒 **safety directive** (no unsafe constructs migrated from Zig; prove the concept BEFORE hardening; **ENFORCED at v0.8.0**: `#![forbid(unsafe_code)]` in core + CLI, `deny` + one justified `allow` in capi, mutation-verified); boundary-faithful/idiomatic-Rust; **public API = own `wasmrt.h`** (clean `wasmrt_*` + native Rust crate, NOT wasm-c-api/wasmtime symbols); **feature scope = full wasmtime browser-standard parity + memory64, WASI p1 only**; Option-A interpreter; the ValType/slot/opcode/trap invariants; size levers; the collapsed oracle split (tail-calls only); **all 4 deferred decisions are now RESOLVED** (crate split T0, `random_get` + resolver T7, header shape T8) — the gate queue is empty |
| [loaders.md](loaders.md) | **The consumers.** How `universalWasmLoader-*` work (hand-rolled Canonical ABI over core modules + WIT sidecar), the ~38-fn engine surface that drives `wasmrt.h`, the caller-based host-callback requirement, the 3 substrates + 10 targets (phased), the `wasmrt.h` v0 draft (held for review) |
| [testing.md](testing.md) | Parity/oracle strategy — **the three C-ABI gates and what each catches that the others cannot** (link completeness / header-is-valid-C / Miri sees a UAF a normal allocator cannot); the mutation check on handle tagging; **351 tests**; Rust↔wazmrt golden vectors for shared features; **the vendored spec testsuite + its live conformance numbers (98.8% — 61,033/738/3,075 at v0.9.0; all 284 files parse) and where it lives**; **why the total adjudicated count moves when modules start building** (a module that fails to build costs exactly ONE skip, not one per assertion); `wasmrt-capi` under Miri + a lifecycle fuzz; a wasi-gate compiling real guests; bench cold vs steady; DoD = full parity on both targets |
| [security-model.md](security-model.md) | ⚠️ **`pin` is a STUB in wasmrt — a build today performs NO authenticity check**; that section describes the ORACLE, not current behaviour (T9). The *authority* half (the WASI sandbox) IS built. **Two new authority controls at T8:** proposal gating + resource ceilings, both reachable from C. Sandbox **secure by construction** + ⚠️ the **CORRECTED resolver note (2026-08-05)**: Rust's `std` has **no** dir-relative open, so zero-dep + no-`unsafe` + hold-real-handles cannot all hold. Shipped a component-accumulated path — every **escape** property intact, **inode pinning** lost, `verify_beneath` compensating; closing the residual was **DECIDED 2026-08-05: accept + document** (zero-dep and zero-`unsafe` both held). Also: rights-narrowing preopens; the mandated canary test (mutation-verified); pin verify (hash the in-memory bytes you run; root-owned DB; enforce denies before opt-out); authenticity vs authority |
| [licensing.md](licensing.md) | **Verified 2026-08-06: all four license files exist since T0, the Component Ledger is EMPTY and there are ZERO third-party dependencies.** The `cap-std` question is **CLOSED** (rejected 2026-08-05), not open. Where SPDX actually is, per file — the one gap is `README.md`, due at T9. **License = `MIT OR Apache-2.0`** (dual). Carry `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/`third_party/LICENSES.md` verbatim, name wazmrt→wasmrt, "Jon Marcum" 2026. Vendored-`wasm.h` attribution only if that code is reused (it is NOT — we ship our own `wasmrt.h`) |
| [reference-projects.md](reference-projects.md) | **wasmtime SHAPED `wasmrt.h` — shape only, no code/symbols/headers taken**; the wasm-c-api `wasm.h` was NOT vendored even at the C ABI, the moment that shortcut was most tempting. The runtimes evaluated (same set as wazmrt) + **wasmtime is the feature-parity target** ("run what it runs") and the thing being replaced under the loaders. 100% original |
| [roadmap.md](roadmap.md) | Current status (**PORT phase; T0–T8 done, published through v0.9.0, oracle frozen @dadc727**) + the **conversion task list** T0–T9 (scaffold → types/reader → opcode → decode → validate → interp slices → text → wasi → C-ABI → licensing/size), parity-gated, with the 4 decision-gates inlined (**all now resolved**) and T9 scoped to what actually remains. Loader phases 1–4 |
| [known-issues.md](known-issues.md) | Issue tracker — the **2 silent-wrong-output defects fixed at T8** (dropped table initializers; element-segment form 4 rewriting a type) **and the third-order lesson**: a check that is harmless "because the case cannot occur" becomes a bug the moment it can; what T8 closed and what it left open; the **ranked pre-T8 punch list** (2026-08-05), the **5 open defects the wasmtk corpus surfaced** (`ref.null $T` unassemblable · 2 validator gaps the oracle passes · no trap backtrace · an unconditional `data_count` section), the fixed shared-store and literal/text defect write-ups, the port's intentional divergences (single-threaded atomics, `delegate` rejected everywhere, the debug-build stack-depth finding), the wazmrt residuals (#17/#18/#23), and the open decisions |
| [releasing.md](releasing.md) | **Versioning + publishing.** Port-progress ladder (`0.x` → `1.0` = full oracle parity); the 3 crates share one version, CLI is published as `wasmrt`; per-task manual releases; the per-release checklist + the 🔒 binding **pre-publish doc-sync gate** (full `cmem/` + ROADMAP/CHANGELOG/README committed BEFORE the owner is notified to publish — owner directive 2026-07-31); crates.io names reserved |

## Related files outside cmem

- `README.md` — the public, user-facing doc. NOT project memory.
- `ROADMAP.md` + `CHANGELOG.md` — the **public progress trackers** (stage ladder + use-case matrix +
  release notes). Kept in lockstep with each release per the `releasing.md` trigger. NOT project memory.
- `docs/port/*.md` — the detailed wazmrt deep-read maps (6 subsystems) + the loader survey + `wasmrt.h.draft`.
- `scripts/check-wazmrt.sh` + `scripts/wazmrt-baseline.txt` — the oracle monitor. Baseline frozen at
  `wazmrt@dadc727` (2026-07-27); the script now watches for **drift** from the freeze, not readiness.
- `LICENSE` — current; `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/`third_party/LICENSES.md` to be added at scaffold time.
