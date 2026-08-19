#!/usr/bin/env bash
# conformance-diff.sh — the per-file conformance gate, with ONE definition.
#
#   scripts/conformance-diff.sh <baseline.txt> <current.txt>
#
# Both arguments are saved `wasmrt wast <dir>` output. Exits non-zero if any file
# regressed. Prints nothing but the verdict when clean.
#
# ─────────────────────────────────────────────────────────────────────────────
# WHY THIS EXISTS AS A FILE, rather than being retyped per session.
#
# This project's central conformance rule is "no file lost a pass" — the only check
# that stays meaningful while skips convert into verdicts, because the totals move
# for honest reasons in both directions. It was being re-implemented ad hoc each
# time it was needed, and each re-implementation was free to be subtly different.
# Two holes got in that way, on the same day:
#
#   1. Keyed on the file's BASENAME. Seven basenames occur twice in the spec corpus
#      (binary, br_on_cast, br_on_cast_fail, exports, imports, memory, throw — once
#      at the top level, once under proposals/), so two files merged into one row
#      and a regression in either could be netted out by a gain in the other.
#      Fixed by making the runner print the path.
#
#   2. Iterated only the files present in the BASELINE. A file that was clean —
#      and therefore absent from the report — and then regressed was invisible.
#      That hole hid three regressions (instance, throw, throw_ref) behind a
#      "NO FILES LOST A PASS" verdict, and would have shipped them.
#
# ⚠️ A gate you retype is a gate that drifts. Both directions are checked here, once.
set -u

baseline="${1:?usage: conformance-diff.sh <baseline.txt> <current.txt>}"
current="${2:?usage: conformance-diff.sh <baseline.txt> <current.txt>}"

# `<path>: N passed, N failed, N skipped` → `path N N N`
extract() {
  sed -nE 's/^([^ ]+\.wast): ([0-9]+) passed, ([0-9]+) failed, ([0-9]+) skipped.*/\1 \2 \3 \4/p' "$1" \
    | sort
}

a=$(mktemp) ; b=$(mktemp)
trap 'rm -f "$a" "$b"' EXIT
extract "$baseline" > "$a"
extract "$current"  > "$b"

# join on the path, keeping rows present in EITHER side (-a1 -a2) so a newly-listed
# file — one that was clean and now is not — cannot slip through.
regressions=$(
  join -a1 -a2 -e MISSING -o 0,1.2,1.3,2.2,2.3 "$a" "$b" 2>/dev/null | awk '
    {
      path=$1; op=$2; of=$3; np=$4; nf=$5;
      if (op == "MISSING") {                       # was clean, now listed
        if (nf+0 > 0) printf "  NEWLY FAILING  %s  (now %s failed)\n", path, nf;
      } else if (np != "MISSING") {                 # listed both times
        if (np+0 < op+0) printf "  LOST PASSES    %s  %s -> %s\n", path, op, np;
        else if (nf+0 > of+0) printf "  MORE FAILURES  %s  %s -> %s\n", path, of, nf;
      }
    }'
)

if [ -n "$regressions" ]; then
  echo "CONFORMANCE REGRESSION:"
  echo "$regressions"
  exit 1
fi
echo "conformance: no file lost a pass, and no clean file started failing"
