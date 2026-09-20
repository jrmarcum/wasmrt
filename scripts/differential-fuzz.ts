// Differential validation against `wasm-tools`, at scale, in both directions.
//
//   sweep A — REFUSE-VALID: `wasm-tools smith` generates a module that is valid BY CONSTRUCTION,
//             so anything wasmrt refuses is a defect and no oracle beyond an exit status is
//             needed.
//   sweep B — ACCEPT-INVALID: mutate one byte, keep the case only when wasm-tools then calls the
//             module invalid, and report only "wasmrt accepted it" — the silent-wrong direction.
//
// 🔒 Why this exists (`best-practices.md` §1.12). The spec suite is written in the TEXT format, so
// there are invalid modules it structurally cannot express — a table whose element type is `i64`
// is one, and it decoded, validated and RAN here until this fuzz found it in a 300-module run.
// The suite was at 64,603 / 0 / 0 throughout.
//
//   deno run -A scripts/differential-fuzz.ts [iterations] [path/to/wasmrt]
//
// Needs `wasm-tools` on PATH. Exits non-zero if either direction finds anything, and keeps the
// offending module in the scratch directory it names.
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const N = Number(process.argv[2] ?? 200);
const wasmrt = resolve(
  process.argv[3] ?? join(import.meta.dirname!, "..", "target", "release", "wasmrt.exe"),
);

// ⚠️ Judge with the proposal set wasmrt IMPLEMENTS, not with everything wasm-tools knows. `-f all`
// enables compact-imports and stack-switching, which wasmrt does not implement at all, and the
// first run of this script reported seven "defects" that were only that. A differential result is
// a result only when both sides were asked the same question.
const FEATURES =
  "all,-compact-imports,-stack-switching,-shared-everything-threads,-memory-control";

const SMITH = [
  "--bulk-memory-enabled=true",
  "--reference-types-enabled=true",
  "--simd-enabled=true",
  "--relaxed-simd-enabled=true",
  "--multi-value-enabled=true",
  "--sign-extension-ops-enabled=true",
  "--saturating-float-to-int-enabled=true",
  "--tail-call-enabled=true",
  "--exceptions-enabled=true",
  "--gc-enabled=true",
  "--custom-page-sizes-enabled=true",
  "--wide-arithmetic-enabled=true",
  "--memory64-enabled=true",
  "--threads-enabled=true",
  "--extended-const-enabled=true",
  "--compact-imports-enabled=false",
  "--shared-everything-threads-enabled=false",
  "--max-funcs=8",
  "--max-instructions=200",
  "--allow-start-export=false",
];

function run(cmd: string, args: string[]) {
  const r = spawnSync(cmd, args, { encoding: "utf8" });
  return { code: r.status ?? -1, out: ((r.stdout ?? "") + (r.stderr ?? "")).trim() };
}
const why = (s: string) =>
  s.split("\n").map((l) => l.trim()).filter(Boolean).slice(0, 3).join(" | ").slice(0, 180);

if (run("wasm-tools", ["--version"]).code !== 0) {
  console.error("need wasm-tools on PATH");
  process.exit(2);
}

const dir = mkdtempSync(join(tmpdir(), "wasmrt-diff-fuzz-"));
const seedPath = join(dir, "seed.bin");
const modPath = join(dir, "case.wasm");
const mutPath = join(dir, "mut.wasm");
let generated = 0, mutations = 0, refuseValid = 0, acceptInvalid = 0;

for (let i = 0; i < N; i++) {
  const seed = new Uint8Array(400 + (i % 600));
  crypto.getRandomValues(seed);
  writeFileSync(seedPath, seed);
  if (run("wasm-tools", ["smith", ...SMITH, seedPath, "-o", modPath]).code !== 0) continue;
  const bytes = readFileSync(modPath);
  // The generator's own claim is not evidence: confirm the module validates for wasm-tools.
  if (run("wasm-tools", ["validate", "-f", FEATURES, modPath]).code !== 0) continue;
  generated++;

  const ours = run(wasmrt, [modPath]);
  if (ours.code !== 0) {
    refuseValid++;
    const keep = join(dir, `refuse-valid-${i}.wasm`);
    writeFileSync(keep, bytes);
    console.log(`REFUSE-VALID   ${keep}\n    wasmrt: ${why(ours.out.split("elem segs")[1] ?? ours.out)}`);
  }

  for (let k = 0; k < 4; k++) {
    const m = Uint8Array.from(bytes);
    const at = 8 + Math.floor(Math.random() * (m.length - 8)); // never the magic or the version
    m[at] = Math.floor(Math.random() * 256);
    writeFileSync(mutPath, m);
    mutations++;
    if (run("wasm-tools", ["validate", "-f", FEATURES, mutPath]).code === 0) continue; // still valid
    if (run(wasmrt, [mutPath]).code === 0) {
      acceptInvalid++;
      const keep = join(dir, `accept-invalid-${i}-${k}.wasm`);
      writeFileSync(keep, m);
      console.log(
        `ACCEPT-INVALID ${keep}\n    wasm-tools: ${
          why(run("wasm-tools", ["validate", "-f", FEATURES, keep]).out)
        }`,
      );
    }
  }
}

const bad = refuseValid + acceptInvalid;
console.log(
  `differential fuzz: ${bad === 0 ? "PASSED" : "FAILED"} — ${generated} valid modules, ` +
    `${mutations} mutations; refuse-valid=${refuseValid} accept-invalid=${acceptInvalid}` +
    (bad === 0 ? "" : `\n  cases kept in ${dir}`),
);
process.exit(bad === 0 ? 0 : 1);
