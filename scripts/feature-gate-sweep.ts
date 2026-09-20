// The proposal-gate sweep: for EVERY proposal wasmrt gates, a module that uses it, judged with
// that proposal disabled — by wasmrt and by wasm-tools, which must both refuse it.
//
// 🔒 Why this exists (X3, and `best-practices.md` §1.11). A gate is only ever tested where someone
// remembered to test it, and this project has now shipped three proposals gated at ONE entry point
// out of two: wide-arithmetic (missing from `wasmrt_feature_t`), table64 (gated for memories only),
// and the table-initializer form. Each was found by a sweep, never by reading the list.
//
// Every row also runs a CONTROL with every proposal enabled, which both engines must ACCEPT.
// Without it a probe that fails for an unrelated reason reads as a working gate — that is not
// hypothetical: a `(table 1 funcref (ref.func $f))` fixture was refused for its declare segment's
// element type and certified a gate it never exercised.
//
//   deno run -A scripts/feature-gate-sweep.ts [path/to/wasmrt]
//
// Needs `wasm-tools` on PATH. Exits non-zero if any row disagrees or any control fails.
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const wasmrt = resolve(
  process.argv[2] ?? join(import.meta.dirname!, "..", "target", "release", "wasmrt.exe"),
);

function run(cmd: string, args: string[]) {
  const r = spawnSync(cmd, args, { encoding: "utf8" });
  return { code: r.status ?? -1, out: ((r.stdout ?? "") + (r.stderr ?? "")).trim() };
}

// `ours` is wasmrt's spelling of the proposal, `wt` is wasm-tools'. The module must USE the
// proposal and nothing else that the same flag switch would remove.
type Row = { id: string; ours: string; wt: string; wat: string };
const START = `(func (export "_start"))`; // so the module is runnable through the WASI path
const ROWS: Row[] = [
  ["sign-extension", "sign-extension-ops", "sign-extension",
    `(func $f (result i32) i32.const 1 i32.extend8_s)`],
  ["saturating-float-to-int", "nontrapping-float-to-int-conversions", "saturating-float-to-int",
    `(func $f (result i32) f32.const 1 i32.trunc_sat_f32_s)`],
  ["multi-value", "multi-value", "multi-value",
    `(func $f (result i32 i32) i32.const 1 i32.const 2)`],
  ["reference-types", "reference-types", "reference-types", `(table 1 externref)`],
  ["bulk-memory", "bulk-memory-operations", "bulk-memory",
    `(memory 1) (func $f i32.const 0 i32.const 0 i32.const 0 memory.fill)`],
  ["extended-const", "extended-const", "extended-const",
    `(global i32 (i32.add (i32.const 1) (i32.const 2)))`],
  ["simd", "simd", "simd", `(func $f (result v128) v128.const i32x4 1 2 3 4)`],
  ["relaxed-simd", "relaxed-simd", "relaxed-simd",
    `(func $f (result v128) v128.const i32x4 0 0 0 0 v128.const i32x4 0 0 0 0 i8x16.relaxed_swizzle)`],
  ["threads/atomic-op", "threads", "threads",
    `(memory 1) (func $f (result i32) i32.const 0 i32.atomic.load)`],
  ["threads/shared-memory", "threads", "threads", `(memory 1 1 shared)`],
  ["multi-memory", "multi-memory", "multi-memory", `(memory 1) (memory 1)`],
  ["memory64/memory", "memory64", "memory64", `(memory i64 1)`],
  // ⚠️ table64 is the memory64 proposal's other half, and was ungated until 2026-09-20.
  ["memory64/table64", "memory64", "memory64", `(table i64 1 funcref)`],
  ["function-references/call_ref", "function-references", "function-references",
    `(type $t (func)) (func $f (param $r (ref $t)) local.get $r call_ref $t)`],
  // ⚠️ the table-with-initializer form — nothing else in this module needs the proposal.
  ["function-references/table-init", "function-references", "function-references",
    `(table 1 funcref (ref.null func))`],
  ["gc/struct", "gc", "gc",
    `(type $s (struct (field i32))) (func $f (result (ref $s)) i32.const 1 struct.new $s)`],
  ["gc/i31", "gc", "gc", `(func $f (result i31ref) i32.const 1 ref.i31)`],
  ["exceptions", "exception-handling", "exceptions", `(tag $e) (func $f throw $e)`],
  ["tail-call", "tail-call", "tail-call", `(func $g) (func $f return_call $g)`],
  ["wide-arithmetic", "wide-arithmetic", "wide-arithmetic",
    `(func $f (result i64 i64) i64.const 1 i64.const 2 i64.const 3 i64.const 4 i64.add128)`],
  ["custom-page-sizes", "custom-page-sizes", "custom-page-sizes", `(memory 1 (pagesize 1))`],
  ["custom-descriptors", "custom-descriptors", "custom-descriptors",
    `(rec (type $a (descriptor $b) (struct)) (type $b (describes $a) (struct)))
     (func $f (param $r (ref null (exact $a))))`],
].map(([id, ours, wt, wat]) => ({ id, ours, wt, wat }));

const dir = mkdtempSync(join(tmpdir(), "wasmrt-gate-sweep-"));
let bad = 0;
try {
  for (const row of ROWS) {
    const wat = join(dir, "m.wat");
    const wasm = join(dir, "m.wasm");
    writeFileSync(wat, `(module ${START} ${row.wat})\n`);
    const asm = run("wasm-tools", ["parse", wat, "-o", wasm]);
    if (asm.code !== 0) {
      bad++;
      console.log(`${row.id.padEnd(30)} FIXTURE DOES NOT ASSEMBLE: ${asm.out.split("\n")[0]}`);
      continue;
    }
    // Control: every proposal on — both engines must accept, or the row measures nothing.
    const ctlT = run("wasm-tools", ["validate", "-f", "all", wasm]).code === 0;
    // `wasmrt wasi` judges and runs; a module whose only export is `_start` exits 0.
    const ctlO = run(wasmrt, ["wasi", wasm]).code === 0;
    // The gate.
    const t = run("wasm-tools", ["validate", "-f", `all,-${row.wt}`, wasm]).code === 0;
    const o = run(wasmrt, ["wasi", "--features", `all,-${row.ours}`, wasm]);
    const ov = o.code === 0;
    const ok = ctlT && ctlO && !t && !ov;
    if (!ok) {
      bad++;
      console.log(
        `${row.id.padEnd(30)} control(wt=${ctlT} ours=${ctlO}) ` +
          `disabled: wasm-tools ${t ? "ACCEPTS" : "refuses"}, wasmrt ${ov ? "ACCEPTS" : "refuses"}`,
      );
      if (ov) console.log(`    wasmrt accepted a module that names a disabled proposal`);
      else if (!ctlO) console.log(`    wasmrt: ${o.out.split("\n").slice(0, 2).join(" | ")}`);
    }
  }
} finally {
  rmSync(dir, { recursive: true, force: true });
}

console.log(
  bad === 0
    ? `feature-gate sweep: PASSED (${ROWS.length} proposals, each refused by both engines when disabled)`
    : `feature-gate sweep: FAILED — ${bad} row(s)`,
);
process.exit(bad === 0 ? 0 : 1);
