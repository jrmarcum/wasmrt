# cmem — Portable Project Memory for wasmrt

This folder is the **authoritative, portable project memory** for `wasmrt`. It lives inside the
project tree, so it travels with the project (USB drive, clones, GitHub) and is **committed to git** —
unlike a machine-local `CLAUDE.md`. Mirrors the `cmem/` convention of the source project `wazmrt`.

**Format:** plain Markdown — one focused topic file per domain, so any single concern can be reviewed
and revised without wading through one giant file. Keep files small and single-topic.

**What `wasmrt` is:** an idiomatic-**Rust** port of the Zig WebAssembly runtime **`wazmrt`** (sibling
repo `../wazmrt`), built to **replace wasmtime** as the engine beneath the owner's
`universalWasmLoader-*` projects. See [overview.md](overview.md).

## ⛔ GATE (read first)

**Do not write runtime port code until `wazmrt` is complete and `zig build test` passes** — the
passing Zig build is the reference oracle. As of 2026-07-17 wazmrt is **actively changing** (Phase 6
exception-handling core just landed). Run `scripts/check-wazmrt.sh` for freeze-readiness. Until then:
design + prep only. See [roadmap.md](roadmap.md).

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
| [design-decisions.md](design-decisions.md) | Load-bearing decisions + invariants NOT to drift — boundary-faithful/idiomatic-Rust; **public API = own `wasmrt.h`** (clean `wasmrt_*` + native Rust crate, NOT wasm-c-api/wasmtime symbols); **feature scope = full wasmtime browser-standard parity, WASI p1 only**; Option-A interpreter; the ValType/slot/opcode/trap invariants; size levers; the oracle split |
| [loaders.md](loaders.md) | **The consumers.** How `universalWasmLoader-*` work (hand-rolled Canonical ABI over core modules + WIT sidecar), the ~38-fn engine surface that drives `wasmrt.h`, the caller-based host-callback requirement, the 3 substrates + 10 targets (phased), the `wasmrt.h` v0 draft (held for review) |
| [testing.md](testing.md) | Parity/oracle strategy — Rust↔wazmrt golden vectors for shared features; **wasmtime + official spec testsuite** for extended proposals; `wasmrt-capi` under Miri + a lifecycle fuzz; a wasi-gate compiling real guests; bench cold vs steady; DoD = full parity on both targets |
| [security-model.md](security-model.md) | Carry wazmrt's design to replicate — sandbox **secure by construction** (`walkFull` handle-stack; Rust may use cap-std/openat2 to close the #17 residual TOCTOU); rights-narrowing preopens; pin verify (hash the in-memory bytes you run; root-owned DB; enforce denies before opt-out); authenticity vs authority |
| [licensing.md](licensing.md) | **License = `MIT OR Apache-2.0`** (dual). Carry `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/`third_party/LICENSES.md` verbatim, name wazmrt→wasmrt, "Jon Marcum" 2026. Vendored-`wasm.h` attribution only if that code is reused (it is NOT — we ship our own `wasmrt.h`) |
| [reference-projects.md](reference-projects.md) | The runtimes evaluated (same set as wazmrt) + **wasmtime is the feature-parity target** ("run what it runs") and the thing being replaced under the loaders. 100% original |
| [roadmap.md](roadmap.md) | Current status (PREP; gate closed) + the port plan — freeze wait → `wasmrt.h` review → scaffold crates → bottom-up port order → extended proposals oracle'd vs wasmtime → parity DONE. Loader phases 1–4 |
| [known-issues.md](known-issues.md) | Prep-phase issue tracker — the wazmrt residuals relevant to the port (#17/#18/#23 are Zig-std-specific → Rust does the clean version), the oracle-split gap (wazmrt lacks SIMD/multi-mem/threads/tail-calls), and the open decisions |

## Related files outside cmem

- `README.md` — the public, user-facing doc. NOT project memory.
- `docs/port/*.md` — the detailed wazmrt deep-read maps (6 subsystems) + the loader survey + `wasmrt.h.draft`.
- `scripts/check-wazmrt.sh` + `scripts/wazmrt-baseline.txt` — the oracle freeze-readiness monitor.
- `LICENSE` — current; `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/`third_party/LICENSES.md` to be added at scaffold time.
