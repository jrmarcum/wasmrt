# bench/

Interpreter microbenchmark for wasmrt — the Rust port of wazmrt's `bench/bench.zig`.

**Status: PREP placeholder.** No `bench.rs` yet — the port gate is closed
(see `../cmem/roadmap.md`). This README fixes the intended shape so the bench is
built to the same methodology when the runtime lands.

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
- Later: also measure SHA-256 (pin verify) and per-artifact **binary size**
  (criterion (c) — smaller than wasm3/WAMR).

Reference original: `../../wazmrt/bench/bench.zig`.
