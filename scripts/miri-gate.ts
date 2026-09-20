#!/usr/bin/env -S deno run --allow-run --allow-env --allow-read
// miri-gate.ts — run the C-ABI surface under Miri.
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jon Marcum
//
//   deno run -A scripts/miri-gate.ts     ·     bun scripts/miri-gate.ts
//
// WHY. `wasmrt-capi` is the one crate that cannot be `#![forbid(unsafe_code)]`, because a C
// ABI is an unsafe boundary. A normal allocator will happily hand back freed memory that
// still looks right, so an ordinary test run cannot tell a use-after-free from a pass. Miri
// interprets the program and reports it as an error.
//
// What it covers: every `wasmrt-capi` test, including `lifecycle_fuzz`, which drives objects
// through randomized creation, use and destruction orders — including the orders the header
// tells embedders not to use — and touches handles whose store is already gone.
//
// The Miri run is DELIBERATELY not part of `cargo test`: it is ~100x slower, and the fuzz
// scales itself down under `cfg!(miri)` so this finishes in seconds rather than minutes.
//
// Requires: rustup component add miri
//
// 🔻 Ported from `miri-gate.sh` (2026-09-19, the TypeScript scripting rule).
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import process from "node:process";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..");

if (spawnSync("cargo", ["miri", "--version"], { cwd: repo }).status !== 0) {
  console.error("miri is not installed; run: rustup component add miri");
  process.exit(1);
}

// `-Zmiri-disable-isolation` lets the WASI-touching paths read the real clock/entropy if a
// test reaches them; without it Miri blocks those syscalls and the run dies for a reason
// that has nothing to do with memory safety.
const env = { ...process.env, MIRIFLAGS: process.env.MIRIFLAGS ?? "-Zmiri-disable-isolation" };

console.log("== miri: wasmrt-capi ==");
const r = spawnSync("cargo", ["miri", "test", "-p", "wasmrt-capi"], {
  cwd: repo,
  env,
  stdio: "inherit"
});
if (r.status !== 0) {
  console.error("Miri gate: FAILED");
  process.exit(r.status ?? 1);
}
console.log("Miri gate: PASSED");
