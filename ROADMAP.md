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

- **`0.x`** — in progress. Each release lands a gated stage; the number reflects what actually runs and
  is verified against the official spec testsuite.
- **`1.0.0`** — **complete on wasmrt's own terms** on both targets (native + `wasm32`): every in-scope
  proposal implemented (tail calls are the last one outstanding), the spec testsuite at its achievable
  ceiling, the WASI corpus green, the C ABI stable — and the size and speed numbers that decide
  inclusion, measured and defended.

Nothing is marked ✅ here until it passes its conformance gate — a checkmark means *verified against the
official spec testsuite*, not merely "coded."

> **Anchor change (owner, 2026-08-11).** Through T9, wasmrt was gated against the `wazmrt` Zig runtime as
> a frozen oracle. **That is retired.** The two runtimes are now independent entrants competing for
> inclusion in **wasmtk** and the **universalWasmLoader-\*** runtimes, decided on the smallest and fastest
> binary; **rsxtk takes wasmrt by default** through the native Rust interface. Correctness anchors on the
> external references that were always the harder test — the official spec testsuite, wasmtime's
> observable behaviour, and the wasmtk WASI corpus.

Legend: ✅ done & verified · 🚧 in progress · ⬜ planned

## Stage ladder

| Version | Stage | Delivers | Status |
| --- | --- | --- | --- |
| **0.1.0** | Scaffold | 3-crate workspace builds on all four surfaces (CLI, static lib, cdylib, freestanding `wasm32`) | ✅ |
| **0.2.0** | Types + reader | value/reference types, spec-correct LEB128 decode | ✅ |
| **0.3.0** | Opcode IR | the shared pre-decoded instruction table | ✅ |
| **0.4.0** | Decode | full module decoding | ✅ |
| **0.5.0** | Validate (core) | the spec type-checker over the core language | ✅ |
| **0.6.x** | Interpret | execution, one feature slice at a time — compute, memory, tables/reftypes, WasmGC, SIMD, multi-memory, threads/atomics, memory64, exception handling | ✅ |
| **0.7.0** | Text toolchain + Validate (complete) | `.wat` assembler + `.wast` runner, bringing the official spec testsuite online; plus the SIMD / atomics / GC / EH typing arms, so the type-checker covers everything the interpreter runs | ✅ |
| **0.8.0** | WASI + CLI | host imports + module linking on a shared store + WASI preview 1 **including the sandboxed filesystem** (`--dir` / `--ro-dir` preopens); plus `#![forbid(unsafe_code)]` in the engine | ✅ |
| **0.9.0** | C ABI | the `wasmrt.h` embedding surface — checked handles, caller-based host callbacks, configurable proposals + resource ceilings; gated by c-smoke, link-completeness and Miri | ✅ |
| 0.10.0 | Hardening | the correctness punch-list, licensing, docs, size minimization, all gates green | ⬜ |
| 0.11.0 | Bug hunt + code hygiene | a comprehensive audit across tested **and** untested paths — bugs, fall-throughs, stale workarounds, dead code, missing documentation | ⬜ |
| 0.12.0 | Optimization review | measured options for making the shipped artifacts faster and smaller — judged at the binary and C-ABI boundary, not by micro-benchmarks | ⬜ |
| 0.13.0 | Security review | an adversarial review of the penetration surfaces — hostile guests, malformed input, and C-ABI misuse — with recommended mitigations | ⬜ |
| **1.0.0** | **Complete** | **every in-scope proposal implemented, conformance at its ceiling, C ABI stable, and the size/speed numbers that decide inclusion measured and defended** | ⬜ |

## What you can do with wasmrt (use-case matrix)

Checked off as each capability lands and passes its gate.

### Run WebAssembly
| Use case | Status | Lands in |
| --- | --- | --- |
| Decode a `.wasm` module | ✅ | 0.4 |
| Validate a module (reject malformed/invalid) | ✅ | 0.5 |
| Run a pure-compute export (e.g. `fib`, `factorial`) | ✅ | 0.6 |
| Linear memory + globals | ✅ | 0.6 |
| `call_indirect`, multi-table, reference types | ✅ | 0.6 |
| Bulk memory/table ops | ✅ | 0.6 |
| WasmGC (structs/arrays/i31, casts) | ✅ | 0.6 |
| SIMD (full `v128`, incl. relaxed) | ✅ | 0.6 |
| Multi-memory | ✅ | 0.6 |
| Threads / atomics | ✅ | 0.6 |
| memory64 | ✅ | 0.6 |
| Exception handling (exnref + legacy) | ✅ | 0.6 |
| Type-check every construct above | ✅ | 0.7 |
| Host imports (link a module against another) | ✅ | 0.8 |
| Restrict which proposals a guest may use | ✅ | 0.9 |
| Configurable resource ceilings (memory, call depth, GC) | ✅ | 0.9 |
| Tail calls (`return_call`/`return_call_indirect`) | ⬜ | 0.10 |

### Text & conformance
| Use case | Status | Lands in |
| --- | --- | --- |
| Assemble `.wat` → `.wasm` | ✅ | 0.7 |
| Run `.wast` spec scripts | ✅ | 0.7 |
| Pass the official WebAssembly spec testsuite | 🚧 99.4% | 0.7 |

### WASI & the CLI
| Use case | Status | Lands in |
| --- | --- | --- |
| Run a WASI preview-1 program (stdout/args/env/clock) | ✅ | 0.8 |
| Sandboxed filesystem (`--dir` / `--ro-dir` preopens) | ✅ | 0.8 |
| Module pin verification / signatures | ⬜ | 0.10 |

### Embed wasmrt
| Use case | Status | Lands in |
| --- | --- | --- |
| Native shared library via the C ABI (`wasmrt.h`) | ✅ | 0.9 |
| Embed wasmrt itself inside another `wasm32` host | ⬜ | loader Phase 4 |
| Rust embedding via the `wasmrt-core` crate | ✅ | 0.9 |

## Follow along

- **crates.io:** [`wasmrt`](https://crates.io/crates/wasmrt) (CLI) · `wasmrt-core` (library) · `wasmrt-capi` (C ABI)
- **Releases & tags:** each stage ships a `vX.Y.Z` GitHub release with notes.
- **Changelog:** [CHANGELOG.md](CHANGELOG.md).

Scope note: wasmrt targets **full wasmtime browser-standard parity + memory64**, with **WASI preview 1**.
WASI preview 2/3 and the component model are out of scope.
