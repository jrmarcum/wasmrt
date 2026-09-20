// Line-ending gate: no tracked text file may contain a CR, in the working tree or in the index.
//
// 🔒 Why this exists. `.gitattributes` says `* text=auto eol=lf`, which is a rule git enforces on
// CHECKOUT and on COMMIT — it cannot stop an editor or a generator from writing CRLF into a file
// that then sits in the working tree looking normal. That state is what breaks scripted edits: a
// pattern containing `\n` matches nothing in a CRLF file, the edit reports success, the build
// passes, and a mutation test that never applied is indistinguishable from a gate that caught
// nothing (`cmem/best-practices.md` §8.1b).
//
//   deno run -A scripts/eol-gate.ts [--fix]
//
// `--fix` rewrites the offenders in place (CRLF → LF); without it the script only reports.
// Exits non-zero if anything is wrong.
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const repo = join(import.meta.dirname!, "..");
const fix = process.argv.includes("--fix");

const tracked = spawnSync("git", ["ls-files"], { cwd: repo, encoding: "utf8" });
if (tracked.status !== 0) {
  console.error("eol gate: `git ls-files` failed — run this inside the repository");
  process.exit(2);
}
const files = (tracked.stdout ?? "").split("\n").map((s) => s.trim()).filter(Boolean);

const hasCr = (b: Buffer) => b.includes(0x0d);
const isBinary = (b: Buffer) => b.includes(0);

let worktree: string[] = [];
let index: string[] = [];
let checked = 0;

for (const f of files) {
  let buf: Buffer;
  try {
    buf = readFileSync(join(repo, f));
  } catch {
    continue; // a tracked file that is not checked out is not this gate's business
  }
  if (isBinary(buf)) continue;
  checked++;
  if (hasCr(buf)) {
    worktree.push(f);
    if (fix) {
      writeFileSync(join(repo, f), buf.toString("utf8").split("\r\n").join("\n"));
    }
  }
  // The index is the half a `.gitattributes` is supposed to guarantee; check it too, because a
  // file committed before the rule existed would still be carrying CRLF.
  const blob = spawnSync("git", ["show", `:${f}`], { cwd: repo, encoding: "buffer" });
  const b = blob.stdout as unknown as Buffer;
  if (b && !isBinary(b) && hasCr(b)) index.push(f);
}

for (const f of worktree) console.log(`  CR in the working tree: ${f}${fix ? "  (fixed)" : ""}`);
for (const f of index) console.log(`  CR in the INDEX:        ${f}  (git add it after --fix)`);

const bad = (fix ? 0 : worktree.length) + index.length;
console.log(
  bad === 0
    ? `eol gate: PASSED — ${checked} tracked text files, all LF` +
      (fix && worktree.length ? ` (${worktree.length} rewritten)` : "")
    : `eol gate: FAILED — ${worktree.length} in the working tree, ${index.length} in the index` +
      (fix ? "" : "\n  re-run with --fix to rewrite them"),
);
process.exit(bad === 0 ? 0 : 1);
