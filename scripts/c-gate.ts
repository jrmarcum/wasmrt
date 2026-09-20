#!/usr/bin/env -S deno run --allow-run --allow-env --allow-read --allow-write
// c-gate.ts — build the wasmrt static library and run the two C-ABI gates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jon Marcum
//
//   tests/abi_symbols.c  — link completeness: every function `wasmrt.h` DECLARES must be
//                          EXPORTED. Taking a symbol's address forces the linker to resolve
//                          it, so a header/library mismatch fails the build.
//   tests/c_smoke.c      — behaviour: compile -> link a host import -> instantiate -> call
//                          -> memory -> global -> trap, driven from real C.
//
// Both compile against the SHIPPED header, so they also prove `wasmrt.h` is valid C — which
// no Rust test can.
//
//   deno run -A scripts/c-gate.ts [--release]   ·   bun scripts/c-gate.ts [--release]
//
// ⚠️ This host has no `cc` on PATH: set CC=gcc.
//
// 🔻 Ported from `c-gate.sh` (2026-09-19, the TypeScript scripting rule).
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import process from "node:process";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..");
const release = process.argv.includes("--release");
const profileDir = release ? "release" : "debug";
const cargoFlags = release ? ["--release"] : [];
const CC = process.env.CC ?? "cc";
const include = join(repo, "crates", "wasmrt-capi", "include");
const out = mkdtempSync(join(tmpdir(), "wasmrt-c-gate-"));

const run = (cmd: string, args: string[]) =>
  spawnSync(cmd, args, { cwd: repo, stdio: "inherit" }).status ?? 1;

try {
  console.log("== building the static library ==");
  if (run("cargo", ["build", "-p", "wasmrt-capi", ...cargoFlags]) !== 0) process.exit(1);

  // A Rust staticlib is named differently per toolchain; take whichever is there.
  const lib = [
    join(repo, "target", profileDir, "libwasmrt_capi.a"),
    join(repo, "target", profileDir, "wasmrt_capi.lib"),
  ].find(existsSync);
  if (!lib) {
    console.error(`no static library found under target/${profileDir}`);
    process.exit(1);
  }
  console.log(`   using ${lib}`);

  // A Rust staticlib does not carry its own system dependencies; the host libc and friends
  // must be named explicitly at link time.
  const syslibs = process.platform === "win32"
    ? ["-lws2_32", "-lbcrypt", "-luserenv", "-lntdll", "-lole32", "-loleaut32", "-ladvapi32"]
    : process.platform === "darwin"
    ? ["-framework", "CoreFoundation", "-framework", "Security"]
    : ["-lpthread", "-ldl", "-lm"];

  let failed = false;
  for (const gate of ["abi_symbols", "c_smoke"]) {
    console.log(`== ${gate} ==`);
    const exe = join(out, gate);
    const build = run(CC, [
      "-std=c11", "-Wall", "-Wextra", "-Werror", `-I${include}`,
      join(repo, "tests", `${gate}.c`), lib, ...syslibs, "-o", exe,
    ]);
    if (build !== 0) {
      console.error("   BUILD FAILED");
      failed = true;
      continue;
    }
    if (run(exe, []) !== 0) {
      console.error("   RUN FAILED");
      failed = true;
    }
  }

  if (failed) {
    console.error("C-ABI gate: FAILED");
    process.exit(1);
  }
  console.log("C-ABI gate: PASSED");
} finally {
  rmSync(out, { recursive: true, force: true });
}
