// The 64-bit axis, swept instruction by instruction — at validation AND at run time.
//
// Every instruction that addresses a table or a memory takes THAT table's/memory's index type.
// Part 1 checks the typing: the i64 spelling must be accepted by both engines and the i32
// spelling refused by both. Part 2 checks execution: an index of 2^32 on a two-slot 64-bit table
// (or a one-page 64-bit memory) is out of bounds and must TRAP — truncated to 32 bits it becomes
// slot 0, which exists, so the engine returns an answer instead of trapping.
//
// 🔒 Why this exists (`best-practices.md` §1.11). table64 landed as a type-level change and the
// operand rule was assumed to follow it. It did not: `return_call_indirect` typed its index `i32`
// (part 1), and SIX table instructions truncated theirs at run time (part 2) — `call_indirect` at
// 2^32 called the function in slot 0 and returned its answer. The spec suite was at 64,603 / 0 / 0
// throughout, because no suite file tail-calls or indexes past 2^32 through a 64-bit table.
//
//   deno run -A scripts/index-type-sweep.ts [path/to/wasmrt]
//
// Needs `wasm-tools` and `wasmtime` on PATH. Exits non-zero on any disagreement.
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const wasmrt = resolve(
  process.argv[2] ?? join(import.meta.dirname!, "..", "target", "release", "wasmrt.exe"),
);
const FEATURES =
  "all,-compact-imports,-stack-switching,-shared-everything-threads,-memory-control";
const WASMTIME_W = "memory64=y,tail-call=y,threads=y,gc=y,function-references=y";

function run(cmd: string, args: string[]) {
  const r = spawnSync(cmd, args, { encoding: "utf8" });
  return { code: r.status ?? -1, out: ((r.stdout ?? "") + (r.stderr ?? "")).trim() };
}
const first = (s: string) => s.split("\n").map((l) => l.trim()).filter(Boolean)[0] ?? "";

const dir = mkdtempSync(join(tmpdir(), "wasmrt-index-sweep-"));
const build = (src: string, name: string) => {
  const wat = join(dir, `${name}.wat`);
  const wasm = join(dir, `${name}.wasm`);
  writeFileSync(wat, src + "\n");
  const r = run("wasm-tools", ["parse", wat, "-o", wasm]);
  return r.code === 0 ? wasm : null;
};

// --- part 1: typing. `%` becomes i64 (must be accepted) then i32 (must be refused). ----------
const TBL = `(table $t i64 2 funcref) (elem declare func $g) (func $g)`;
const MEM = `(memory $m i64 1)`;
const SHARED = `(memory $m i64 1 1 shared)`;
const TYPED: Array<[string, string]> = [
  ["table.get", `${TBL} (func (result funcref) %.const 0 table.get $t)`],
  ["table.set", `${TBL} (func %.const 0 ref.null func table.set $t)`],
  ["table.grow", `${TBL} (func (result i64) ref.null func %.const 1 table.grow $t)`],
  ["table.fill", `${TBL} (func %.const 0 ref.null func %.const 1 table.fill $t)`],
  ["table.init", `${TBL} (elem $e func $g) (func %.const 0 i32.const 0 i32.const 0 table.init $t $e)`],
  ["table.copy", `${TBL} (func %.const 0 %.const 0 %.const 0 table.copy $t $t)`],
  ["call_indirect", `${TBL} (type $ft (func)) (func %.const 0 call_indirect $t (type $ft))`],
  ["return_call_indirect", `${TBL} (type $ft (func)) (func %.const 0 return_call_indirect $t (type $ft))`],
  ["i32.load", `${MEM} (func (result i32) %.const 0 i32.load)`],
  ["i64.store", `${MEM} (func %.const 0 i64.const 0 i64.store)`],
  ["memory.grow", `${MEM} (func (result i64) %.const 1 memory.grow)`],
  ["memory.fill", `${MEM} (func %.const 0 i32.const 0 %.const 0 memory.fill)`],
  ["memory.copy", `${MEM} (func %.const 0 %.const 0 %.const 0 memory.copy)`],
  ["memory.init", `${MEM} (data $d "x") (func %.const 0 i32.const 0 i32.const 0 memory.init $d)`],
  ["v128.load", `${MEM} (func (result v128) %.const 0 v128.load)`],
  ["v128.store", `${MEM} (func %.const 0 v128.const i32x4 0 0 0 0 v128.store)`],
  ["v128.load8_lane", `${MEM} (func (result v128) %.const 0 v128.const i32x4 0 0 0 0 v128.load8_lane 0)`],
  ["i32.atomic.load", `${SHARED} (func (result i32) %.const 0 i32.atomic.load)`],
  ["i32.atomic.store", `${SHARED} (func %.const 0 i32.const 0 i32.atomic.store)`],
  ["i32.atomic.rmw.add", `${SHARED} (func (result i32) %.const 0 i32.const 0 i32.atomic.rmw.add)`],
  ["i32.atomic.rmw.cmpxchg", `${SHARED} (func (result i32) %.const 0 i32.const 0 i32.const 0 i32.atomic.rmw.cmpxchg)`],
  ["memory.atomic.notify", `${SHARED} (func (result i32) %.const 0 i32.const 0 memory.atomic.notify)`],
  ["memory.atomic.wait32", `${SHARED} (func (result i32) %.const 0 i32.const 0 i64.const 0 memory.atomic.wait32)`],
];

// --- part 2: execution past 2^32, which must trap on both engines. ---------------------------
const HUGE = "0x1_0000_0000";
const RT_TBL = `
  (type $t (func (result i32)))
  (table $tb i64 2 funcref)
  (elem (table $tb) (i64.const 0) func $zero $one)
  (func $zero (type $t) i32.const 100)
  (func $one (type $t) i32.const 101)`;
const RUNTIME: Array<[string, string]> = [
  ["call_indirect", `${RT_TBL} (func (export "f") (result i32) i64.const ${HUGE} call_indirect $tb (type $t))`],
  ["return_call_indirect", `${RT_TBL} (func (export "f") (result i32) i64.const ${HUGE} return_call_indirect $tb (type $t))`],
  ["table.get", `${RT_TBL} (func (export "f") (result i32) i64.const ${HUGE} table.get $tb ref.is_null)`],
  ["table.set", `${RT_TBL} (func (export "f") (result i32) i64.const ${HUGE} ref.null func table.set $tb i32.const 1)`],
  ["table.fill", `${RT_TBL} (func (export "f") (result i32) i64.const ${HUGE} ref.null func i64.const 1 table.fill $tb i32.const 1)`],
  ["table.copy", `${RT_TBL} (func (export "f") (result i32) i64.const ${HUGE} i64.const 0 i64.const 1 table.copy $tb $tb i32.const 1)`],
  ["table.init", `${RT_TBL} (elem $e func $zero) (func (export "f") (result i32) i64.const ${HUGE} i32.const 0 i32.const 1 table.init $tb $e i32.const 1)`],
  ["i32.load", `(memory $m i64 1) (func (export "f") (result i32) i64.const ${HUGE} i32.load)`],
  ["memory.fill", `(memory $m i64 1) (func (export "f") (result i32) i64.const ${HUGE} i32.const 7 i64.const 1 memory.fill i32.const 1)`],
  ["i32.atomic.load", `(memory $m i64 1 1 shared) (func (export "f") (result i32) i64.const ${HUGE} i32.atomic.load)`],
];

let bad = 0;
try {
  for (const [id, body] of TYPED) {
    for (const [ty, mustAccept] of [["i64", true], ["i32", false]] as const) {
      const wasm = build(`(module ${body.replaceAll("%", ty)})`, `${id}-${ty}`.replace(/\W/g, "_"));
      if (!wasm) {
        bad++;
        console.log(`typing ${id.padEnd(24)} ${ty}  FIXTURE DOES NOT ASSEMBLE`);
        continue;
      }
      const t = run("wasm-tools", ["validate", "-f", FEATURES, wasm]).code === 0;
      const o = run(wasmrt, [wasm]);
      const ov = o.code === 0;
      if (t === mustAccept && ov === mustAccept) continue;
      bad++;
      console.log(
        `typing ${id.padEnd(24)} ${ty}  wasm-tools=${t ? "ACCEPT" : "refuse"} ` +
          `wasmrt=${ov ? "ACCEPT" : "refuse"}  (expected ${mustAccept ? "ACCEPT" : "refuse"})`,
      );
      if (!ov) console.log(`      wasmrt: ${first(o.out.split("elem segs")[1] ?? o.out)}`);
    }
  }
  for (const [id, body] of RUNTIME) {
    const wasm = build(`(module ${body})`, `rt-${id}`.replace(/\W/g, "_"));
    if (!wasm) {
      bad++;
      console.log(`runtime ${id.padEnd(23)} FIXTURE DOES NOT ASSEMBLE`);
      continue;
    }
    const t = run("wasmtime", ["run", "--invoke", "f", "-W", WASMTIME_W, wasm]);
    const o = run(wasmrt, ["run", wasm, "f"]);
    // Both must refuse to produce a value: an index past 2^32 is out of bounds either way.
    const tTrapped = t.code !== 0;
    const oTrapped = o.code !== 0;
    if (tTrapped && oTrapped) continue;
    bad++;
    console.log(
      `runtime ${id.padEnd(23)} wasmtime=${tTrapped ? "trap" : "RETURNED " + first(t.out)} ` +
        `wasmrt=${oTrapped ? "trap" : "RETURNED " + first(o.out)}`,
    );
  }
} finally {
  rmSync(dir, { recursive: true, force: true });
}

console.log(
  bad === 0
    ? `index-type sweep: PASSED (${TYPED.length} instructions × 2 spellings, ${RUNTIME.length} past 2^32)`
    : `index-type sweep: FAILED — ${bad} row(s)`,
);
process.exit(bad === 0 ? 0 : 1);
