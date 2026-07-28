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

## Project memory lives in `cmem/` — read it first

Portable project memory (committed, travels with the repo) is in **[`cmem/`](cmem/)**, one topic file
per domain — same convention as wazmrt. **Start with [`cmem/INDEX.md`](cmem/INDEX.md).**

- [`cmem/overview.md`](cmem/overview.md) · [`cmem/vision.md`](cmem/vision.md) — what/why (canonical / fast / small)
- [`cmem/architecture.md`](cmem/architecture.md) · [`cmem/design-decisions.md`](cmem/design-decisions.md) — crates, dual-target, invariants NOT to drift
- [`cmem/loaders.md`](cmem/loaders.md) — the `universalWasmLoader-*` consumers + the `wasmrt.h` surface
- [`cmem/testing.md`](cmem/testing.md) · [`cmem/security-model.md`](cmem/security-model.md) · [`cmem/licensing.md`](cmem/licensing.md)
- [`cmem/roadmap.md`](cmem/roadmap.md) · [`cmem/known-issues.md`](cmem/known-issues.md) · [`cmem/reference-projects.md`](cmem/reference-projects.md)

Detailed engineering reference (the wazmrt deep-read maps + `wasmrt.h` draft) is in
[`docs/port/`](docs/port/).

## "Update the project memory" = update `cmem/`

When asked to record/remember anything for the project, fold it into the matching `cmem/` file(s) and
refresh the `cmem/INDEX.md` Files table (see the binding triggers in `cmem/INDEX.md`), then commit +
push (`origin` = github.com/jrmarcum/wasmrt). `README.md` is public-facing docs, NOT project memory.
