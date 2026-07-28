#!/usr/bin/env bash
# check-wazmrt.sh — monitor the wazmrt (Zig) oracle for changes and (now) drift.
#
# GATE STATUS: OPEN as of 2026-07-27 (see CLAUDE.md / cmem/roadmap.md). The freeze
# baseline is wazmrt@dadc727 (full parity; zig build test green). The port has begun,
# so this script's role has INVERTED: it no longer waits for the gate to open — it
# detects whether the oracle has DRIFTED out from under the in-flight port. A
# "CHANGED since baseline" result now means: review the new wazmrt commits, decide
# whether the port must follow, and re-baseline deliberately (--rebaseline) — never
# silently chase HEAD mid-port. Still diffs wazmrt against the baseline and runs its
# test suite.
#
# Usage:
#   scripts/check-wazmrt.sh            # report changes + run `zig build test`
#   scripts/check-wazmrt.sh --no-test  # skip the (slow) build+test, just git/feature diff
#   scripts/check-wazmrt.sh --rebaseline   # record the current wazmrt state as the new baseline
#
# Exit code: 0 = no change since baseline AND tests green (candidate freeze);
#            1 = changed or tests not green (gate stays closed); 2 = setup error.

set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WAZ="$(cd "$HERE/../../wazmrt" 2>/dev/null && pwd)"
BASE="$HERE/wazmrt-baseline.txt"
RUN_TEST=1
[ "${1:-}" = "--no-test" ] && RUN_TEST=0

if [ -z "${WAZ:-}" ] || [ ! -d "$WAZ/src" ]; then
  echo "ERROR: wazmrt not found at ../wazmrt (from $HERE)"; exit 2
fi

cur_head=$(git -C "$WAZ" rev-parse HEAD 2>/dev/null)
cur_dirty=$(git -C "$WAZ" status --porcelain 2>/dev/null | wc -l | tr -d ' ')
cur_subj=$(git -C "$WAZ" log -1 --format='%h %ci %s' 2>/dev/null)

# Feature-implemented markers (grep the sources; a hit ≈ decode/exec support exists).
feat() { grep -rliE "$1" "$WAZ/src" 2>/dev/null | wc -l | tr -d ' '; }
f_eh=$(feat "try_table|exnref|throw_ref"); f_simd=$(feat "\\bv128\\b.*(add|mul|load)|simd_|0xfd.*(0x00|prefix)")
f_mm=$(feat "multi.?memory|memories\\.len.*>.*1")

if [ "${1:-}" = "--rebaseline" ]; then
  { echo "BASELINE_HEAD=$cur_head"
    echo "BASELINE_SUBJ=$cur_subj"
    echo "BASELINE_DATE=$(git -C "$WAZ" log -1 --format=%ci)"
    echo "BASELINE_CAPTURED=recorded-by-rebaseline"
  } > "$BASE"
  echo "Rebaselined to $cur_head"; exit 0
fi

echo "=== wazmrt monitor ==="
echo "current: $cur_subj"
[ "$cur_dirty" -gt 0 ] && echo "working tree: DIRTY ($cur_dirty files) — actively being edited"

if [ -f "$BASE" ]; then
  base_head=$(sed -n 's/^BASELINE_HEAD=//p' "$BASE")
  if [ "$cur_head" = "$base_head" ] && [ "$cur_dirty" -eq 0 ]; then
    echo "git: UNCHANGED since baseline ($base_head)"
    changed=0
  else
    echo "git: CHANGED since baseline"
    echo "  baseline: $(sed -n 's/^BASELINE_SUBJ=//p' "$BASE")"
    echo "  new commits:"; git -C "$WAZ" log --oneline "$base_head..HEAD" 2>/dev/null | sed 's/^/    /'
    [ "$cur_dirty" -gt 0 ] && echo "    (+ uncommitted edits)"
    changed=1
  fi
else
  echo "git: no baseline file — run with --rebaseline to record one"; changed=1
fi

echo "feature markers (files touching each): EH=$f_eh SIMD=$f_simd multi-mem=$f_mm"
echo "  (as of the 2026-07-27 freeze wazmrt has EH/SIMD/multi-mem/threads/memory64 — the ONLY wasmrt-target"
echo "   feature with no wazmrt oracle is the tail-call proposal: return_call/return_call_indirect)"

test_green=0
if [ "$RUN_TEST" -eq 1 ]; then
  echo "=== zig build test (in $WAZ) ==="
  if ( cd "$WAZ" && zig build test ) 2>&1 | tail -20; then
    echo "tests: GREEN"; test_green=1
  else
    echo "tests: FAILED (or build error) — gate stays closed"
  fi
else
  echo "tests: skipped (--no-test)"
fi

echo "=== verdict ==="
if [ "$changed" -eq 0 ] && [ "$test_green" -eq 1 ]; then
  echo "FREEZE CANDIDATE: unchanged since baseline AND tests green. Re-confirm scope, then the gate may open."
  exit 0
else
  echo "GATE STAYS CLOSED: wazmrt still changing and/or tests not confirmed green."
  exit 1
fi
