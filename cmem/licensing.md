# Licensing

**License = `MIT OR Apache-2.0`** (dual, at the consumer's option) — the WebAssembly/Rust ecosystem
standard, and what the source project `wazmrt` uses. Carry it over verbatim. The compliance ledger is
`third_party/LICENSES.md`; this file records the *why* + strategy.

## Files (mimic wazmrt)

- **`LICENSE-MIT`** — copied verbatim from wazmrt (Copyright (c) 2026 Jon Marcum).
- **`LICENSE-APACHE`** — canonical Apache-2.0 text, copied verbatim.
- **`NOTICE`** — dual-license notice + third-party framework. **Adapted:** wasmrt lists **no
  incorporated components** (it ships its own `wasmrt.h` and does NOT vendor the wasm-c-api `wasm.h`).
- **`third_party/LICENSES.md`** — obligations table + Adoption Checklist + (empty) Component Ledger +
  reference-project inventory. Adapted: nothing adopted; `wasm-c-api` marked "Not adopted".

*(The single bare `LICENSE` that the initial repo had — plain Apache text — was replaced by the
`LICENSE-MIT` + `LICENSE-APACHE` pair to match wazmrt.)*

## SPDX convention (accurate — do not overgeneralize)

SPDX identifier, verbatim: `SPDX-License-Identifier: MIT OR Apache-2.0`.
Per wazmrt's convention it is **not** on every source file. Put it on: `NOTICE`, `README.md`, the C
header(s) (`wasmrt.h`), and `Cargo.toml` (`license = "MIT OR Apache-2.0"`). A per-file SPDX tag is
required **only on files that contain third-party code** (none yet). Per-file SPDX on Rust sources is
optional/harmless and common in Rust — you may add `//! SPDX-License-Identifier: MIT OR Apache-2.0`, but
the original did not require it.

## Why dual, and compatibility

Dual `MIT OR Apache-2.0` lets any-language consumers pick the license that fits, and it is compatible
with incorporating code from every candidate reference runtime (all permissive: MIT, ISC, Apache-2.0,
Apache-2.0 WITH LLVM-exception). One-way rule: MIT/ISC flows into an Apache-2.0 distribution; adapted
Apache-2.0 code keeps its own terms for those files. Inbound = outbound (contributions dual-licensed
unless stated). **Verify any upstream license against its actual `LICENSE` file, not the GitHub badge**
(wasmtime's badge omits `WITH LLVM-exception`).

## The "update the project memory" trigger touches this

If a Rust dependency or any adopted code is added, run the **Adoption Checklist** in
`third_party/LICENSES.md`, add a Component Ledger entry, add change-notes + SPDX headers to adapting
files, and update `NOTICE`. wasmrt aims to stay **100% original + dependency-light** (wazmrt has zero
deps); the one open question is whether to add `cap-std`/`openat2` for the sandbox (see
`design-decisions.md`) — if so, record its license here and in the ledger.
