# wasmrt roadmap

**wasmrt** is a fast, small, idiomatic-**Rust** WebAssembly runtime — a from-scratch port of the
battle-tested Zig runtime [`wazmrt`](https://github.com/jrmarcum/wazmrt). **That oracle relationship is
retired** (see the anchor change below) — wasmrt is finished on its own terms, measured against the
official spec testsuite, wasmtime's observable behaviour, and the wasmtk WASI corpus.

This page tracks that journey. It updates on **every release**, so you can follow exactly what works
today — not what's promised.

## How versions map to progress

The version number measures **what genuinely runs and passes its gate** — never inflated to signal
maturity:

- **`0.x`** — in progress. Each release lands a gated stage; the number reflects what actually runs and
  is verified against the official spec testsuite.
- **`1.0.0`** — **the conformance clear-out**: the official spec testsuite driven to **zero failures,
  zero skips and zero unrun files**, with an empty baseline and no deliberate spec deviations. Every
  in-scope proposal is already implemented (tail calls landed 2026-08-14), so what remains is the
  residual itself — the proposals the corpus contains that wasmrt had not targeted, and the blocks that
  never ran at all.
- **`1.0.x`** — the four review phases behind it, in a deliberate order: **`1.0.1`** hardening (incl.
  module authenticity), **`1.0.2`** bug hunt + code hygiene, **`1.0.3`** optimization review, **`1.0.4`**
  security review. The last two are **review-and-recommend**: they produce costed findings for a
  decision, not a unilateral pass.

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
| **1.0.0** | **Conformance clear-out** | the spec testsuite to **zero failures / zero skips / zero unrun**, empty baseline, zero deliberate deviations — which brings the proposals the corpus contains into scope | ⬜ |
| 1.0.1 | Hardening | the correctness punch-list, **module authenticity (`pin`)**, licensing, docs, size minimization, all gates green | ⬜ |
| 1.0.2 | Bug hunt + code hygiene | a comprehensive audit across tested **and** untested paths — bugs, fall-throughs, stale workarounds, dead code, missing documentation | ⬜ |
| 1.0.3 | Optimization review | measured options for making the shipped artifacts faster and smaller — judged at the binary and C-ABI boundary, not by micro-benchmarks | ⬜ |
| 1.0.4 | Security review | an adversarial review of the penetration surfaces — hostile guests, malformed input, and C-ABI misuse — with recommended mitigations | ⬜ |

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
| Tail calls (`return_call`/`return_call_indirect`/`return_call_ref`) | ✅ | 0.10 |

### Text & conformance
| Use case | Status | Lands in |
| --- | --- | --- |
| Assemble `.wat` → `.wasm` | ✅ | 0.7 |
| Run `.wast` spec scripts | ✅ | 0.7 |
| Pass the official WebAssembly spec testsuite | 🚧 99.4% — **zero failures and zero skips is the `1.0.0` gate** | 0.7 → 1.0.0 |

### WASI & the CLI
| Use case | Status | Lands in |
| --- | --- | --- |
| Run a WASI preview-1 program (stdout/args/env/clock) | ✅ | 0.8 |
| Sandboxed filesystem (`--dir` / `--ro-dir` preopens) | ✅ | 0.8 |
| Module pin verification / signatures | ⬜ | 1.0.1 |

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
