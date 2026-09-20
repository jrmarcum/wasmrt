#!/usr/bin/env -S deno run --allow-read
// conformance-diff.ts — the per-file conformance gate, with ONE definition.
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jon Marcum
//
//   deno run --allow-read scripts/conformance-diff.ts <baseline.txt> <current.txt>
//   bun scripts/conformance-diff.ts <baseline.txt> <current.txt>
//
// Both arguments are saved `wasmrt wast <dir>` output. Exits non-zero if any file
// regressed. Prints nothing but the verdict when clean.
//
// ─────────────────────────────────────────────────────────────────────────────
// WHY THIS EXISTS AS A FILE, rather than being retyped per session.
//
// This project's central conformance rule is "no file lost a pass" — the only check
// that stays meaningful while skips convert into verdicts, because the totals move
// for honest reasons in both directions. It was being re-implemented ad hoc each
// time it was needed, and each re-implementation was free to be subtly different.
// Two holes got in that way, on the same day:
//
//   1. Keyed on the file's BASENAME. Seven basenames occur twice in the spec corpus
//      (binary, br_on_cast, br_on_cast_fail, exports, imports, memory, throw — once
//      at the top level, once under proposals/), so two files merged into one row
//      and a regression in either could be netted out by a gain in the other.
//      Fixed by making the runner print the path.
//
//   2. Iterated only the files present in the BASELINE. A file that was clean —
//      and therefore absent from the report — and then regressed was invisible.
//      That hole hid three regressions (instance, throw, throw_ref) behind a
//      "NO FILES LOST A PASS" verdict, and would have shipped them.
//
//   3. 🆕 The MIRROR of (2), found by review on 2026-09-19 and present in the shell
//      version this file replaced: a file listed in the BASELINE and ABSENT from the
//      current report was never checked. A script the runner failed to read or parse
//      simply stops being listed, so every pass it used to have vanishes under a
//      "no file lost a pass" verdict — the loudest possible regression, silently.
//
// ⚠️ A gate you retype is a gate that drifts. Both directions are checked here, once.
//
// 🔻 Ported from `conformance-diff.sh` (2026-09-19, the TypeScript scripting rule). The
// port is behaviour-for-behaviour: same row parsing, same join over the UNION of both
// sides, same messages, same exit status — verified by running both against the same
// inputs, including inputs that must FAIL.
import { readFileSync } from "node:fs";
import process from "node:process";

/** `<path>: N passed, N failed, N skipped` → one row, keyed by the PATH (never the basename). */
type Row = { passed: number; failed: number };

function extract(file: string): Map<string, Row> {
  const rows = new Map<string, Row>();
  const re = /^(\S+\.wast): (\d+) passed, (\d+) failed, (\d+) skipped/;
  for (const line of readFileSync(file, "utf8").split(/\r?\n/)) {
    const m = re.exec(line);
    if (m) rows.set(m[1], { passed: Number(m[2]), failed: Number(m[3]) });
  }
  return rows;
}

const [baseline, current] = process.argv.slice(2);
if (!baseline || !current) {
  console.error("usage: conformance-diff.ts <baseline.txt> <current.txt>");
  process.exit(2);
}

const a = extract(baseline);
const b = extract(current);

// The UNION of both sides (the `join -a1 -a2` of the shell version), so a newly-listed
// file — one that was clean and now is not — cannot slip through.
const regressions: string[] = [];
for (const path of [...new Set([...a.keys(), ...b.keys()])].sort()) {
  const was = a.get(path);
  const now = b.get(path);
  if (!was) {
    // Was clean (absent), now listed.
    if (now && now.failed > 0) {
      regressions.push(`  NEWLY FAILING  ${path}  (now ${now.failed} failed)`);
    }
  } else if (!now) {
    // In the baseline, gone from the current report: every pass it had is unaccounted for.
    // (Comparing a SUBSET run against a full-corpus baseline trips this by construction — that
    // is the honest answer, because the two reports are not about the same thing.)
    if (was.passed > 0) {
      regressions.push(
        `  MISSING        ${path}  (was ${was.passed} passed; absent from the current report)`,
      );
    }
  } else {
    if (now.passed < was.passed) {
      regressions.push(`  LOST PASSES    ${path}  ${was.passed} -> ${now.passed}`);
    } else if (now.failed > was.failed) {
      regressions.push(`  MORE FAILURES  ${path}  ${was.failed} -> ${now.failed}`);
    }
  }
}

if (regressions.length > 0) {
  console.log("CONFORMANCE REGRESSION:");
  for (const r of regressions) console.log(r);
  process.exit(1);
}
console.log("conformance: no file lost a pass, and no clean file started failing");
