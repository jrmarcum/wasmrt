# bench/

Interpreter microbenchmark for wasmrt — the Rust port of wazmrt's `bench/bench.zig`.

**Status (2026-08-06): still no `bench.rs` — and the runtime has been ready for one
since v0.6.x.** The port gate opened 2026-07-27 and T0–T8 are done, so this is now
a real gap rather than a placeholder: **the "fast" axis is one of wasmrt's three
stated success criteria (`../cmem/vision.md`) and it has never been measured.**

Writing it is **T9c** (`../cmem/roadmap.md`). Until then:

> ⚠️ **Do not quote wazmrt's numbers as wasmrt's.** The figures below are the
> *oracle's*, kept here as the methodology and the target to beat. Inheriting the
> interpreter's shape does not inherit its measurements.

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
- Later: also measure SHA-256 (pin verify — `pin` is still a stub, T9e) and
  per-artifact **binary size** (criterion (c) — smaller than wasm3/WAMR). Size is
  **T9b** and is likewise unmeasured; both feed **T11**, the optimization review,
  which cannot start without these baselines.

Reference original: `../../wazmrt/bench/bench.zig`.
