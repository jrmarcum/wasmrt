# cmem — Portable Project Memory for wasmrt

This folder is the **authoritative, portable project memory** for `wasmrt`. It lives inside the
project tree, so it travels with the project (USB drive, clones, GitHub) and is **committed to git** —
unlike a machine-local `CLAUDE.md`. Mirrors the `cmem/` convention of the source project `wazmrt`.

**Format:** plain Markdown — one focused topic file per domain, so any single concern can be reviewed
and revised without wading through one giant file. Keep files small and single-topic.

**What `wasmrt` is:** an idiomatic-**Rust** port of the Zig WebAssembly runtime **`wazmrt`** (sibling
repo `../wazmrt`), built to **replace wasmtime** as the engine beneath the owner's
`universalWasmLoader-*` projects. See [overview.md](overview.md).

## ✅ GATE — OPEN (2026-07-27). Port in progress — decode → validate → run all working. (read first)

The gate is **open**: `wazmrt` reached **full parity** and **`zig build test` passes** (489/493, 4 skip;
Debug + ReleaseSafe green), so the passing Zig build is now a **frozen** reference oracle at
`wazmrt@dadc727` (`scripts/wazmrt-baseline.txt`). **Runtime port code is permitted** — follow the phased
conversion task list in [roadmap.md](roadmap.md), parity-gated at each step. `scripts/check-wazmrt.sh`
now watches for **oracle drift** (a change since the freeze), not for freeze-readiness. Scope note: the
oracle covers every wasmrt-target feature **except the tail-call proposal** (`return_call`/
`return_call_indirect`) — oracle those against **wasmtime + the spec testsuite**. memory64 **is** in
scope (owner, 2026-07-27). See [design-decisions.md](design-decisions.md).

**Progress (2026-08-03): T0–T3 + T4-core + T5 slices 1–9 DONE, released v0.1.0 → v0.6.8 (v0.6.7 + v0.6.8
prepped, awaiting owner publish).** `wasmrt-core` has `types` + `reader` + `opcode` (shared IR +
`decode_body`) + `module` (decode) + `validate` (spec §3 type-checker, core language; SIMD/atomics/GC/EH
typing deferred to 0.5.x) + `interp` (switch interpreter; integer + float compute + linear memory incl.
multi-memory and memory64 + tables/`call_indirect`/reference types + WasmGC structs/arrays/`i31`/casts +
the full `v128` SIMD set incl. relaxed + threads/atomics `0xFE` family single-threaded). **`wasmrt <file>`
summarizes + validates; `wasmrt run <file> <fn> [args]` runs compute + multi-memory + memory64 +
indirect-call + GC + SIMD + atomics functions** (incl. recursion). The interp value slot is 128-bit
(`Value = u128`) so a `v128` is one slot; a memory carries its own index type (`i64` addresses on a 64-bit
memory) while **tables stay 32-bit**. 112 core tests green, clippy clean, all four build surfaces.
**Next: 0.6.x slice 10 — exception handling.** Each task ships a crates.io
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
| [vision.md](vision.md) | The goal — fast + smallest-binary + wasm-compilable runtime that **replaces wasmtime** under the `universalWasmLoader-*` projects. The three success axes: **canonical / fast / small** |
| [architecture.md](architecture.md) | Planned Rust architecture — `wasmrt-core` (no_std-friendly) + `wasmrt-capi` (cdylib/staticlib, the `wasmrt.h` surface) + `wasmrt` CLI; decode→validate→instantiate→execute; the dual-target contract; the shared opcode IR seam |
| [design-decisions.md](design-decisions.md) | Load-bearing decisions + invariants NOT to drift — boundary-faithful/idiomatic-Rust; **public API = own `wasmrt.h`** (clean `wasmrt_*` + native Rust crate, NOT wasm-c-api/wasmtime symbols); **feature scope = full wasmtime browser-standard parity + memory64, WASI p1 only**; Option-A interpreter; the ValType/slot/opcode/trap invariants; size levers; the collapsed oracle split (tail-calls only); the 4 open decisions now as task-list gates |
| [loaders.md](loaders.md) | **The consumers.** How `universalWasmLoader-*` work (hand-rolled Canonical ABI over core modules + WIT sidecar), the ~38-fn engine surface that drives `wasmrt.h`, the caller-based host-callback requirement, the 3 substrates + 10 targets (phased), the `wasmrt.h` v0 draft (held for review) |
| [testing.md](testing.md) | Parity/oracle strategy — Rust↔wazmrt golden vectors for shared features; **wasmtime + official spec testsuite** for extended proposals; `wasmrt-capi` under Miri + a lifecycle fuzz; a wasi-gate compiling real guests; bench cold vs steady; DoD = full parity on both targets |
| [security-model.md](security-model.md) | Carry wazmrt's design to replicate — sandbox **secure by construction** (`walkFull` handle-stack; Rust may use cap-std/openat2 to close the #17 residual TOCTOU); rights-narrowing preopens; pin verify (hash the in-memory bytes you run; root-owned DB; enforce denies before opt-out); authenticity vs authority |
| [licensing.md](licensing.md) | **License = `MIT OR Apache-2.0`** (dual). Carry `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/`third_party/LICENSES.md` verbatim, name wazmrt→wasmrt, "Jon Marcum" 2026. Vendored-`wasm.h` attribution only if that code is reused (it is NOT — we ship our own `wasmrt.h`) |
| [reference-projects.md](reference-projects.md) | The runtimes evaluated (same set as wazmrt) + **wasmtime is the feature-parity target** ("run what it runs") and the thing being replaced under the loaders. 100% original |
| [roadmap.md](roadmap.md) | Current status (**PORT phase; gate OPEN, oracle frozen @dadc727**) + the **conversion task list** T0–T9 (scaffold → types/reader → opcode → decode → validate → interp slices → text → wasi → C-ABI → licensing/size), parity-gated, with the 4 decision-gates inlined. Loader phases 1–4 |
| [known-issues.md](known-issues.md) | Issue tracker — the wazmrt residuals relevant to the port (#17/#18/#23 Zig-std-specific → Rust does the clean version), the **collapsed scope gap (only tail-calls lack a wazmrt oracle)**, and the open decisions (now task-list gates) |
| [releasing.md](releasing.md) | **Versioning + publishing.** Port-progress ladder (`0.x` → `1.0` = full oracle parity); the 3 crates share one version, CLI is published as `wasmrt`; per-task manual releases; the per-release checklist + the 🔒 binding **pre-publish doc-sync gate** (full `cmem/` + ROADMAP/CHANGELOG/README committed BEFORE the owner is notified to publish — owner directive 2026-07-31); crates.io names reserved |

## Related files outside cmem

- `README.md` — the public, user-facing doc. NOT project memory.
- `ROADMAP.md` + `CHANGELOG.md` — the **public progress trackers** (stage ladder + use-case matrix +
  release notes). Kept in lockstep with each release per the `releasing.md` trigger. NOT project memory.
- `docs/port/*.md` — the detailed wazmrt deep-read maps (6 subsystems) + the loader survey + `wasmrt.h.draft`.
- `scripts/check-wazmrt.sh` + `scripts/wazmrt-baseline.txt` — the oracle monitor. Baseline frozen at
  `wazmrt@dadc727` (2026-07-27); the script now watches for **drift** from the freeze, not readiness.
- `LICENSE` — current; `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/`third_party/LICENSES.md` to be added at scaffold time.
