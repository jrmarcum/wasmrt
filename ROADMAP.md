# wasmrt roadmap

**wasmrt** is a fast, small, idiomatic-**Rust** WebAssembly runtime — a from-scratch port of the
battle-tested Zig runtime [`wazmrt`](https://github.com/jrmarcum/wazmrt), which already runs the full
WebAssembly spec testsuite and a large WASI corpus. wazmrt is the **reference oracle**: wasmrt is
finished when it reproduces the oracle's behavior, feature-for-feature.

This page tracks that journey. It updates on **every release**, so you can follow exactly what works
today — not what's promised.

## How versions map to progress

The destination is fixed and known (the completed wazmrt). The version number measures **how far the
Rust port has climbed toward it**:

- **`0.x`** — in progress. Each release lands a parity-gated stage; the number reflects what actually
  runs and is verified against the oracle.
- **`1.0.0`** — **full parity** with the frozen wazmrt oracle on both targets (native + `wasm32`), with
  every conformance gate green.

Nothing is marked ✅ here until it passes its parity/conformance gate — a checkmark means *verified
against the oracle or the official spec testsuite*, not merely "coded."

Legend: ✅ done & verified · 🚧 in progress · ⬜ planned

## Stage ladder

| Version | Stage | Delivers | Status |
| --- | --- | --- | --- |
| **0.1.0** | Scaffold | 3-crate workspace builds on all four surfaces (CLI, static lib, cdylib, freestanding `wasm32`) | ✅ |
| **0.2.0** | Types + reader | value/reference types, spec-correct LEB128 decode | ✅ |
| **0.3.0** | Opcode IR | the shared pre-decoded instruction table | ✅ |
| **0.4.0** | Decode | full module decoding | ✅ |
| **0.5.0** | Validate | the spec type-checker (core language; SIMD/atomics/GC/EH in 0.5.x) | ✅ |
| 0.6.x | Interpret | execution, landing one feature slice at a time (compute + memory + tables/reftypes + WasmGC + SIMD done) | 🚧 |
| 0.7.0 | Text toolchain | `.wat` assembler + `.wast` spec-script runner | ⬜ |
| 0.8.0 | WASI + CLI | run real compiled programs | ⬜ |
| 0.9.0 | C ABI | the `wasmrt.h` embedding surface | ⬜ |
| 0.10.0 | Hardening | licensing, size minimization, all gates green | ⬜ |
| **1.0.0** | **Parity** | **full parity with the wazmrt oracle** | ⬜ |

## What you can do with wasmrt (use-case matrix)

Checked off as each capability lands and passes its gate.

### Run WebAssembly
| Use case | Status | Lands in |
| --- | --- | --- |
| Decode a `.wasm` module | ✅ | 0.4 |
| Validate a module (reject malformed/invalid) | 🚧 | 0.5 |
| Run a pure-compute export (e.g. `fib`, `factorial`) | ✅ | 0.6 |
| Linear memory + globals | ✅ | 0.6 |
| `call_indirect`, multi-table, reference types | ✅ | 0.6 |
| Bulk memory/table ops | ✅ | 0.6 |
| WasmGC (structs/arrays/i31, casts) | ✅ | 0.6 |
| SIMD (full `v128`, incl. relaxed) | ✅ | 0.6 |
| Multi-memory | ⬜ | 0.6 |
| Threads / atomics | ⬜ | 0.6 |
| memory64 | ⬜ | 0.6 |
| Exception handling (exnref + legacy) | ⬜ | 0.6 |
| Tail calls | ⬜ | 0.6 |

### Text & conformance
| Use case | Status | Lands in |
| --- | --- | --- |
| Assemble `.wat` → `.wasm` | ⬜ | 0.7 |
| Run `.wast` spec scripts | ⬜ | 0.7 |
| Pass the official WebAssembly spec testsuite | ⬜ | 0.7 |

### WASI & the CLI
| Use case | Status | Lands in |
| --- | --- | --- |
| Run a WASI preview-1 program (stdout/args/env/clock) | ⬜ | 0.8 |
| Sandboxed filesystem (`--dir` / `--ro-dir` preopens) | ⬜ | 0.8 |
| Module pin verification / signatures | ⬜ | 0.8 |

### Embed wasmrt
| Use case | Status | Lands in |
| --- | --- | --- |
| Native shared library via the C ABI (`wasmrt.h`) | ⬜ | 0.9 |
| Embed wasmrt itself inside another `wasm32` host | ⬜ | 0.9 |
| Rust embedding via the `wasmrt-core` crate | ⬜ | 0.9 |

## Follow along

- **crates.io:** [`wasmrt`](https://crates.io/crates/wasmrt) (CLI) · `wasmrt-core` (library) · `wasmrt-capi` (C ABI)
- **Releases & tags:** each stage ships a `vX.Y.Z` GitHub release with notes.
- **Changelog:** [CHANGELOG.md](CHANGELOG.md).

Scope note: wasmrt targets **full wasmtime browser-standard parity + memory64**, with **WASI preview 1**.
WASI preview 2/3 and the component model are out of scope.
