# bench/

Interpreter microbenchmark for wasmrt — the Rust port of wazmrt's `bench/bench.zig`.

## ✅ It exists — but it lives at `crates/wasmrt-core/examples/bench.rs`

```sh
cargo run --release -p wasmrt-core --example bench
```

**Why not `bench/bench.rs`:** a cargo target whose `path` points outside its package
directory breaks `cargo package`, and all three crates are published. This directory
keeps the **methodology and the oracle's reference figures**; the code lives in the
package it measures.

### wasmrt's own numbers — first measured 2026-08-07 (T9c)

| Regime | Measurement |
| --- | --- |
| cold — toy module (70 B) | **3.5 µs** |
| cold — real module (48,067 B) | **4.48 ms** ← quote this one |
| steady — `sum(n)` tight `loop`/`br_if` | **~237 Mops/s** (232–240 over 1M/10M/50M) |

Host: `x86_64-pc-windows-gnullvm`, release profile. Against the oracle's figures below
that is **cold parity and ~90% of steady throughput** — measured on a different machine,
so treat it as a sanity check, not a head-to-head.

> ⚠️ **Do not quote wazmrt's numbers as wasmrt's.** The figures in the next section are
> the *oracle's*, kept here as the methodology and the target to beat. Inheriting the
> interpreter's shape does not inherit its measurements. Where the two are named
> together above, both are labelled.

## What it measures (from wazmrt `cmem/vision.md` + `docs/port/06`)

Two regimes, reported separately — never conflated:

- **Cold path** — decode + instantiate + one call: the per-run cost a short-lived
  program pays. This is where wasmrt aims to **beat Deno/V8** (which pay ~110 ms
  of V8 init + JIT + JS marshalling every run). Quote a real-script cold start
  (~4.4 ms for a 46 KB guest), NOT the sub-µs toy-module figure.
- **Steady state** — a hot loop inside one instance: raw dispatch throughput
  (~264 Mops/s for wazmrt; a JIT wins this regime — informs the Option A → B
  decision).

## Methodology (carry over)

- Build in release-fast for the steady numbers; measure in-process.
- Compare against a **same-session baseline** (`git stash` A/B/A) — never compare
  throughput across days (run-to-run spread ~8%).
- The cross-process cold-start-vs-Deno numbers are recorded in `cmem/testing.md`.
- Canonical workload: `sum(n)` over a tight `loop`/`br_if` (see wazmrt bench).
- Later: also measure SHA-256 (pin verify — `pin` is still a stub, T9e).
- **Per-artifact binary size is measured** (T9b, 2026-08-07) — the numbers are in
  `../cmem/roadmap.md` (T9b) and `../cmem/vision.md`. The wasm3/WAMR comparison is
  still outstanding: it needs those binaries present, not an estimate.
- **T11 (the optimization review) is no longer blocked** — both baselines it required
  now exist, so every proposal there can be stated as a delta.

Reference original: `../../wazmrt/bench/bench.zig`.
