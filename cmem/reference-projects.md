# Reference Projects

The candidate runtimes studied for wasmrt — the same set wazmrt evaluated. The **verified license
inventory + the Adoption Checklist** live in `third_party/LICENSES.md` (the compliance source of truth);
this file records evaluation intent. **wasmrt is 100% original Rust** (the wazmrt port is the owner's own
code) — nothing is adopted; adopting any of these triggers the checklist + a ledger entry.

## 🔒 `wazmrt` is now a COMPETITOR, not the oracle (owner, 2026-08-11)

Through T9 the Zig runtime `wazmrt` was wasmrt's frozen reference oracle. **That is retired**
([design-decisions.md](design-decisions.md)). The two runtimes are now **independent entrants** for
inclusion in **wasmtk** and the **universalWasmLoader-\*** runtimes, decided on the smallest and fastest
binary; **`rsxtk` takes wasmrt by default** through the native Rust interface.

Its place in *this* file therefore changes: wazmrt is a **runtime to benchmark against**, in the same
column as wasm3 and WAMR, and no longer a source of answers. ⚠️ **Reading its design for ideas is now the
thing the "wasmtime's SHAPE, our code" rule was written to constrain, aimed at a rival** — and it is
running its own size/self-ownership program for the same contest, so its current head reflects *its*
constraints, not ours. **Benchmark it; do not follow it.**

⚠️ Provenance is untouched: wasmrt is derived from wazmrt in design, both are the owner's under the same
dual MIT/Apache licence, and [licensing.md](licensing.md)'s attribution stays. `scripts/wazmrt-provenance.txt`
records which commits the port was developed against.

| Runtime to benchmark against | Why | Status |
|---|---|---|
| **wazmrt** (Zig) | the direct rival for the same two inclusion slots | ⬜ **never compared on this machine** |
| **wasm3**, **WAMR** | the small-interpreter bar wasmrt aims to beat on size | ⬜ **never compared** — needs their binaries, not estimates |
| **wasmtime** | the feature-parity target and the runtime being replaced | partially: used as the correctness reference; **not** size/speed compared |

| Project | License (SPDX) | Why it's interesting for wasmrt |
|---|---|---|
| **wasmtime** | Apache-2.0 WITH LLVM-exception | **The feature-parity target** — "run what wasmtime runs" (criterion (a)) — and the runtime wasmrt **replaces** under the loaders. Its `wasmtime_*` C API **shaped** `wasmrt.h` (shipped at T8 / v0.9.0): store/context/linker/typed-val model, caller-based host callbacks. **Shape only — no code, no symbols, no headers were taken**; the names are ours and the handle model deliberately differs (checked value handles, not refcounted objects). **Three shape-borrows to date** — the header (T8), the **shared store** (T7b), and the **engine-level type registry** for cross-module type identity (T9h, approved 2026-08-08). See the 🔒 *"wasmtime's SHAPE, our code"* rule in [design-decisions.md](design-decisions.md); the **Component Ledger stays empty** because no code is adopted, and each borrow visibly diverges where wasmrt's constraints differ. |
| **wasm3** | MIT | The canonical smallest-interpreter size benchmark (criterion (c)). |
| **WAMR** | Apache-2.0 WITH LLVM-exception | "Production small runtime" size/perf comparison. |
| **wasmi** | Apache-2.0 OR MIT | A Rust register-machine interpreter — reference for the Option A→B ladder. |
| **wasmer** | MIT | Another wasm-c-api implementer. |
| **wazero** | Apache-2.0 | The Go loader's current runtime (replaced in Phase 3). |
| **wain** | MIT | Small Rust interpreter — implementation reference. |
| **wai** | MIT | Interpreter reference. |
| **rust-wasm** | ISC | Interpreter reference. |
| **wasm-c-api** | Apache-2.0 | The standard `wasm.h`. **Not adopted** — wasmrt ships its own `wasmrt.h` (see `loaders.md`). |
| **Chicory** | Apache-2.0 | The JVM loader's current runtime (replaced in Phase 3). |

**Two distinct relationships to keep straight:**
- **Behavioral target / replace:** wasmtime (parity + drop-in under the loaders); wazero, Chicory, host
  `WebAssembly` (the other substrates being replaced per loader). These are *goals*, not code adoptions.
- **Code adoption:** none. If it ever happens, `third_party/LICENSES.md` governs.

**Verified 2026-08-06 at v0.9.0: still true.** The Component Ledger in `third_party/LICENSES.md` is
**empty** and every crate has **zero third-party dependencies** (only the internal `wasmrt-core` path
dep). The C ABI was the moment this was most likely to slip — the obvious shortcut would have been to
vendor the wasm-c-api `wasm.h` — and it did not: wasmrt ships its own header, written from the loaders'
requirements. See `licensing.md`.

> **Trademarks:** permissive licenses grant no trademark rights. Don't brand wasmrt with "Wasmtime",
> "Wasmer", "wazero", etc., or imply endorsement.
