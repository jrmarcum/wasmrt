# Licensing

**License = `MIT OR Apache-2.0`** (dual, at the consumer's option) — the WebAssembly/Rust ecosystem
standard, and what the source project `wazmrt` uses. Carry it over verbatim. The compliance ledger is
`third_party/LICENSES.md`; this file records the *why* + strategy.

## Files (mimic wazmrt) — **all four exist since the T0 scaffold**, verified 2026-08-06

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
Per wazmrt's convention it is **not** on every source file. A per-file SPDX tag is required **only on
files that contain third-party code** (none yet). Per-file SPDX on Rust sources is optional/harmless and
common in Rust — you may add `//! SPDX-License-Identifier: MIT OR Apache-2.0`, but the original did not
require it.

**Where it actually is, verified 2026-08-06 at v0.9.0** (this list used to state an intent; it now
states the fact):

| File | SPDX tag | Note |
| --- | --- | --- |
| `NOTICE` | ✅ | |
| `crates/wasmrt-capi/include/wasmrt.h` | ✅ | the shipped C header |
| `tests/c_smoke.c`, `tests/abi_symbols.c` | ✅ | added at T8 |
| `scripts/c-gate.sh`, `scripts/miri-gate.sh` | ✅ | added at T8 |
| `Cargo.toml` | ✅ *(as `license = "MIT OR Apache-2.0"`)* | the field **is** the SPDX expression; no separate comment needed |
| `README.md` | ❌ **missing** | the only gap — add at T9 |

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
files, and update `NOTICE`.

**Status at v0.9.0 (verified 2026-08-06): still ZERO third-party dependencies and an EMPTY Component
Ledger.** Every crate's `[dependencies]` contains only the internal `wasmrt-core` path dep; nothing is
vendored or adapted. The whole runtime — decoder, validator, interpreter, text toolchain, WASI, sandbox
and C ABI — is original Rust.

**The `cap-std`/`openat2` question is CLOSED, not open** (owner, 2026-08-05): **rejected**, and the
sandbox resolver's TOCTOU residual is accepted + documented instead. It would have been wasmrt's first
runtime dependency, against both the smallest-binary axis and the zero-dep posture. Recorded here
because this file previously carried it as "the one open question" — see `security-model.md` for the
argument and `known-issues.md` for the decision. **Do not re-litigate.**
