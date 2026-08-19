# Releasing & Versioning

How wasmrt is versioned and published so others can follow progress. Decisions locked by the owner
**2026-07-27**. Public-facing trackers: `ROADMAP.md` + `CHANGELOG.md` (repo root — NOT project memory,
but this file defines the process that keeps them current).

## Versioning = port-progress ladder (owner decision)

The destination is fixed and known: **a complete runtime, small and fast enough to be chosen.** The
version number measures how far wasmrt has climbed toward it and reflects what genuinely runs and passes
its gate — never inflated to signal the effort's maturity (that story is told by `ROADMAP.md`'s use-case
matrix + a fast cadence).

*Through T9 the destination was phrased as "the completed, frozen `wazmrt` oracle". **Retired 2026-08-11
(owner)** — see the 1.0 entry below.*

- **`0.x`** — in progress; each release lands one gated stage.
- **`1.0.0`** — 🆕 **RE-CUT 2026-08-19 (owner): the CONFORMANCE CLEAR-OUT.** The spec corpus driven to
  **zero failures, zero skips, zero unrun files**, an **empty baseline**, and **zero deliberate spec
  deviations**. This is a specific deliverable, not a summary of the phases before it — and it brings the
  proposals the corpus contains into scope (custom-descriptors/`exact`, table64, wide-arithmetic,
  custom-page-sizes, custom-annotations, memory64-imports, legacy `delegate`). WASI p2/p3 and the
  component model stay out. Task **T13** in `roadmap.md`.
- **`1.0.x`** — the four review phases, behind it and in this order: **`1.0.1`** T9 hardening (incl.
  `pin`), **`1.0.2`** T10 bug hunt + code hygiene, **`1.0.3`** T11 optimization review, **`1.0.4`** T12
  security review. ⚠️ **The API stability promise starts at `1.0.0`**, so the C ABI and `wasmrt-core`
  surfaces freeze when the clear-out ships — a proposal added at 1.0.0 may add a `wasmrt_feature_t`
  value, which is additive and keeps `abi_version() == 1`; anything that is not additive must land
  BEFORE 1.0.0 or wait for 2.0.
- *(Superseded: `1.0.0` previously meant "complete on wasmrt's own terms — every in-scope proposal
  implemented, conformance at its achievable ceiling, C ABI stable, size/speed measured and defended".
  That reading made 1.0 a judgement call; the clear-out makes it a number.)* 🔒 **The wazmrt oracle was retired as the 1.0 criterion on
  2026-08-11 (owner)** — the two runtimes now compete independently for inclusion in wasmtk and the
  universalWasmLoader-\* runtimes, so parity with a competitor is neither the goal nor a meaningful gate.
- Pre-1.0 the public API (both `wasmrt-core` and `wasmrt.h`) is **unstable** — breaking changes bump the
  minor (`0.y`), which is the SemVer compatibility unit below 1.0. Expect frequent `0.y` bumps until T8.

**Cadence rule (owner, 2026-08-03): the patch component stays a single digit.** A `0.y.z` line runs
`z = 0…9`; the release after `0.y.9` is **`0.(y+1).0`**, never `0.y.10`. That is what closed the 0.6
interp line: v0.6.9 (exception handling) was its last slice, so the next release took **0.7.0**. When a
line fills up, the remaining stage→version map shifts down by one rather than growing a two-digit patch.

**Stage → version map** (the ladder lives in `ROADMAP.md`; the port tasks in `roadmap.md`) — *shifted by
one from 0.8 onward when the 0.6 line filled up; T6 and the T4 completion then shipped together in 0.7.0*:
0.1 T0 scaffold · 0.2 T1 types+reader · 0.3 T2 opcode · 0.4 T3 decode · 0.5 T4 validate (core) ·
0.6.x T5 interp (patch per feature slice, 0.6.0–0.6.9) · **0.7 T6 text toolchain + T4 completion (the deferred SIMD/atomics/GC/EH typing arms)** ·
0.8 T7 wasi+cli · 0.9 T8 C-ABI · 🆕 **1.0.0 T13 conformance clear-out** · **1.0.1 T9 hardening** · **1.0.2 T10 bug hunt + code hygiene** · **1.0.3 T11 optimization review** · **1.0.4 T12 security review**.

🆕 **RE-CUT 2026-08-19 (owner).** The `0.10`–`0.13` rungs are gone: T9–T12 became the `1.0.x` line and a
new task, **T13 — the conformance clear-out — takes `1.0.0` and runs FIRST**. ⚠️ **T-numbers are
identifiers, not an order** (they are cited across every `cmem/` file), so the order is **T13 → T9 → T10
→ T11 → T12** while the names stay put. The single-digit-patch rule is satisfied: `1.0.0`…`1.0.4`.

*(T10, T11 and T12 added by the owner 2026-08-06. The single-digit-patch rule constrains the PATCH component
only, so a two-digit MINOR — 0.10 … 0.13 — is correct and not a violation.)*

## Crates & names (owner decision: CLI takes the bare `wasmrt` name)

Three crates, **one shared version** (`workspace.package.version`), released together:

| Published name | Dir | Kind | Install |
| --- | --- | --- | --- |
| `wasmrt` | `crates/wasmrt` | CLI bin | `cargo install wasmrt` |
| `wasmrt-core` | `crates/wasmrt-core` | library | `wasmrt-core = "0.x"` |
| `wasmrt-capi` | `crates/wasmrt-capi` | C ABI (cdylib/staticlib) | build from source |

Names verified **available** on crates.io 2026-07-27 (`wasmrt`, `wasmrt-core`, `wasmrt-capi`,
`wasmrt-cli`, `wazmrt` all unregistered). The **first publish (0.1.0) claims all three names** — do it
early to avoid squatting. Needs the owner's crates.io API token (`cargo login`, or a `CRATES_IO_TOKEN`
repo secret if/when CI publish is added).

## Release cadence (owner decision: per-task, manual now)

Release on **each completed, parity-gated task**. Automate later (a tag-triggered GitHub Action or
`release-plz`) once the manual flow is proven; don't build CI publishing up front.

## Per-release checklist (binding — do all, in order)

**Steps 1–6 happen BEFORE the owner is notified of the publish needs** (the 🔒 rule below): the commit
that gets published must already carry every doc + memory update. The agent does 1–6 and commits; the
owner does 7–9.

1. **Task is DONE**: its conformance gate passes, `cargo test`/`clippy` green on all surfaces, no
   regression in the spec-suite numbers and **no file losing a pass**.
1a. **The size and speed numbers are re-measured and recorded**, because they are what decides inclusion
   (`cmem/vision.md`). A release that grows the cdylib or slows the steady loop must say so in the
   CHANGELOG with the reason — an unexplained regression is the one thing this project cannot ship.
1b. **The shipped binary is STANDALONE.** Copy `wasmrt.exe` to an empty directory and run it with
   `PATH` reduced to the system directories; it must print its version, not exit 127. `objdump -p`
   should list only `KERNEL32`, `ntdll`, `bcryptprimitives` and `api-ms-win-*`. ⚠️ This is a real step,
   not a formality — the CLI silently needed a **toolchain** DLL (`libunwind.dll`) until 2026-08-10,
   and every dev-box test passed the whole time. See the distribution manifest below.
2. **Update the public trackers:**
   - `ROADMAP.md` — flip the stage row to ✅ and check off every use-case the release makes real.
   - `CHANGELOG.md` — move `Unreleased` items into a new `[x.y.z]` section (Keep a Changelog format).
   - `README.md` — refresh the status line to what the release now runs.
3. **Sync ALL project memory (`cmem/`), not just the roadmap** — this is the step that used to lag a
   turn behind and ship stale. Fold the release into **every** affected file, then update
   `cmem/INDEX.md`'s Files table:
   - `cmem/roadmap.md` — mark the task/slice DONE with its per-slice record.
   - `cmem/architecture.md` — advance the "realized so far" snapshot to the new version.
   - `cmem/testing.md` — bump the unit-test count + note what the new slice's tests cover.
   - `cmem/known-issues.md` — log any new intentional divergences / deferrals from the slice.
   - `cmem/releasing.md` (this file) — advance the **Status** section to the new version.
   - any other `cmem/` file the slice touched (overview/design-decisions/security-model/…).
4. **Bump the version** — one place (`workspace.package.version`) plus the internal dep version in the
   root `[workspace.dependencies] wasmrt-core = { version = "x.y.z" }` (they MUST match or publish fails).
   `cargo-release` does both automatically; if bumping by hand, don't forget the dep pin.
5. **Re-verify green** after the doc/version edits: `cargo test --workspace` + `clippy` still pass (a
   version bump or doctest can break the build).
6. **Commit** the code + version + trackers + full `cmem/` sync as ONE release commit, and **notify the
   owner** it's ready to publish, handing over the exact commands for steps 7–9. *(This is the publish
   handoff — everything above is already committed, so the published artifact captures it.)*
7. **(Owner) Publish in dependency order** (crates.io requires deps published first):
   `cargo publish -p wasmrt-core` → `-p wasmrt-capi` → `-p wasmrt`. (`cargo release` orders this.)
8. **(Owner) Tag + push** — `git tag vX.Y.Z` then `git push origin main --tags`. **Pushing the `v*` tag
   fires `.github/workflows/release.yml`, which extracts this version's `CHANGELOG.md` section and creates
   the GitHub Release automatically** — no manual `gh release create`. (The workflow uses the built-in
   `GITHUB_TOKEN`; no secret needed. It **fails loudly** if the CHANGELOG has no `## [X.Y.Z]` section, so
   step 2 must already be committed — which the pre-publish gate guarantees.)

## 📦 Distribution manifest — exactly which files a user needs (2026-08-10)

**Measured on a clean `PATH`**, not read off the build files: each binary was copied to an empty
directory and run with `PATH=C:\Windows\system32;C:\Windows`. That is the only way to tell a real
dependency from one the dev box happens to satisfy.

| you are shipping | files a user needs | not needed |
| --- | --- | --- |
| **the CLI** | `wasmrt.exe` — **one file** | — |
| **the C ABI, dynamic** | `wasmrt_capi.dll` + `include/wasmrt.h` | `libwasmrt_capi.dll.a` (import lib: needed to **link**, not to run) |
| **the C ABI, static** | `libwasmrt_capi.a` + `include/wasmrt.h` | — |
| **the freestanding engine** | the `wasm32-unknown-unknown` `.wasm` | — |

⚠️ **`libwasmrt_capi.a` is ~22 MB and that is NOT shipped size.** A Rust `.a` carries crate metadata;
only the code the consumer actually links lands in their binary. Do not quote it as a size figure — the
honest numbers are the CLI and the cdylib (see `testing.md` / T9b).

### 🔒 The gotcha this manifest exists to prevent

Before 2026-08-10, `wasmrt.exe` was **not standalone**: it imported **`libunwind.dll`**, supplied by the
mingw-llvm toolchain rather than by Windows. On a dev box it resolves off `PATH`; on a user's machine
the process died before `main` with **exit 127, "cannot open shared object file"** — no output, no
diagnostic worth the name.

Fixed by pinning `-C target-feature=+crt-static` for the host triple in **`.cargo/config.toml`**, so it
cannot regress depending on who types the build command. Cost ~33 KiB on the CLI (650.5 → 684.0);
`wasmrt_capi.dll` gets the same treatment. Remaining imports are Windows-provided only: `KERNEL32`,
`ntdll`, `bcryptprimitives`, and the `api-ms-win-crt-*` Universal CRT set (Windows 10+).

**The lesson, worth applying to any artifact:** *"it runs here"* is not evidence that it ships. Copy the
binary somewhere empty, strip `PATH` to the system directories, and run it. `objdump -p <exe>` lists the
imports — anything not `KERNEL32`/`ntdll`/`api-ms-win-*`/`bcrypt*` is a file you are also shipping,
whether you meant to or not.

*(The oracle is clean here by construction: `wazmrt.exe` and `wazmrt.dll` import only `ntdll` and
`KERNEL32`. ⚠️ Do not ship `wazmrt.pdb` — 3.6 MB of debug symbols, and the largest file in its
`zig-out`.)*

## Tooling

- **`.github/workflows/release.yml`** (in place since v0.6.4): a tag-triggered GitHub Action. Push a
  `v*` tag → it extracts the matching `CHANGELOG.md` `## [X.Y.Z]` section (literal-string match, no regex
  escaping) and runs `gh release create` with those notes. Secret-free (built-in `GITHUB_TOKEN`). This is
  the **only** CI automation so far — deliberately scoped to the GitHub-release step; **`cargo publish`
  remains manual** (needs the owner's crates.io token, and the owner chose not to build CI publishing up
  front). To add crate publishing later, extend this workflow with a `CRATES_IO_TOKEN` repo secret.
- **`cargo-release`** (recommended): workspace-aware — bumps the shared version, syncs the internal dep
  pin, publishes in order, tags. `cargo install cargo-release`; then `cargo release minor` (or `patch`
  for an interp slice). Not yet installed as of 2026-07-27.
- Alternative: the manual steps above. `release-plz` if/when full CI automation is wanted.

## 🔒 The "reflect a shipped stage" trigger (binding on every agent)

When a task/stage completes and is released, the public `ROADMAP.md` use-case matrix, `CHANGELOG.md`,
`README.md`, **and the full `cmem/` project memory** are **part of the definition of done** — not
optional docs, and **not a follow-up after publish**. A release that advances the code without advancing
them misreports progress to the people following along. **The owner's 2026-07-31 directive:** memory +
roadmap SHALL be updated and committed *before* the agent notifies the owner of the publish needs, so
the published commit already carries them. Keep them in lockstep with the version — in the *same* commit.

## Who runs the publish

The **owner** runs `cargo publish` + `git tag` + `gh release` + push for each version (needs the
crates.io token). The agent implements the task and does the **entire** release prep first — version
bump + public trackers + **full `cmem/` sync** + release commit (checklist steps 1–6) — and only then
hands over the publish commands. Established rhythm since v0.1.0; the pre-publish doc-sync gate added
2026-07-31 (v0.6.4) after earlier releases shipped with docs lagging a turn behind.

## Status (2026-08-06)

- **✅ v0.9.0 PUBLISHED and pushed (2026-08-06, release commit `a7abd83`, tag `v0.9.0`) — no release is
  pending.** T8 — the **`wasmrt.h` C ABI** — shipped with all three gates green (`abi_symbols.c` 74/74,
  `c_smoke.c` compiled by a C compiler against the shipped header, and Miri over a randomized lifecycle
  fuzz). Also ships **proposal gating** + **configurable resource ceilings**, a **`Linker` in core**, and
  two **silent-wrong-output** fixes (dropped table initializers; element-segment form 4 rewriting a
  segment's type). Conformance **61,033 / 738 / 3,075 — 98.8%**; 351 workspace tests.
  **`abi_version()` is now 1** (was 0) — the C ABI is finalized, and `WASMRT_ABI_VERSION` in the header
  must stay equal to it (a test pins the pair).
- **Published through v0.9.0.** `wasmrt`, `wasmrt-core` and `wasmrt-capi` are live on crates.io through
  v0.9.0. The tag fired `.github/workflows/release.yml` for the GitHub release, as it has since v0.6.4.
- **The pre-publish doc-sync gate held again:** the version bump, `ROADMAP.md`/`CHANGELOG.md`/`README.md`
  and the **full** `cmem/` sync were all in the release commit `a7abd83` itself, so the published
  artifact carries its own docs. Nine `cmem/` files plus `tests/README.md` were touched, and the v0
  header draft in `docs/port/` was marked HISTORICAL rather than left to mislead.
- **Next release: T13 — the conformance clear-out** (`1.0.0`, owner 2026-08-19): the corpus to zero failed / zero skipped / zero unrun, empty baseline, zero deliberate deviations. **Then T9 hardening (`1.0.1`, and `pin` is its headline since the clear-out ships without it), T10 bug hunt + code hygiene (`1.0.2`), T11 optimization review (`1.0.3`), T12 security review (`1.0.4`)**. Order: **clear → measure → find → optimize → attack**. *(Superseded: "Next release: T9 … (0.10.0), then T10 (0.11.0), T11 (0.12.0), T12 (0.13.0)".)*

### Previous

- **✅ v0.8.0 PUBLISHED and pushed (2026-08-05, release commit `81d1c24`, tag `v0.8.0`).** It ships **all of
  T7** — host imports, module linking on a shared store, and WASI preview 1 **including the sandboxed
  filesystem** with `--dir`/`--ro-dir` — plus the **safety pass** (`#![forbid(unsafe_code)]` in core and
  the CLI) and the **literal/text edges**. Spec-suite conformance at release: **98.8%** (61,013 passing /
  751 failing / 3,094 skipped) and **all 284 files parse for the first time**. 281 workspace tests.
- **The full release chain** (T0 v0.1.0 → T3 v0.4.0 →
  T4-core v0.5.0 → T5 integer v0.6.0 → float v0.6.1 → linear memory v0.6.2 → tables/reftypes v0.6.3 →
  WasmGC v0.6.4 → SIMD v0.6.5 → multi-memory v0.6.6 → threads/atomics v0.6.7 → memory64 v0.6.8 →
  exception handling v0.6.9 → text toolchain + validator completion v0.7.0 → WASI p1 + host imports +
  module linking v0.8.0 → **the `wasmrt.h` C ABI v0.9.0**). The owner
  runs each publish; the rhythm (implement →
  **full-doc prep** → owner publishes) holds. **v0.6.4 onward run under the pre-publish doc-sync gate, and
  each v0.6.4+ GitHub release was created automatically by the tag-triggered `.github/workflows/release.yml`**
  (push the `v*` tag → the release + notes publish themselves).
- **✅ DONE 2026-08-07 (T9d) — per-crate crates.io listing metadata.** All three crates now carry
  `keywords`, `categories` and `readme`, and each has **its own short README** (the `../../README.md`
  path is indeed rejected — cargo packages the readme, so it must live inside the package). Categories
  used: core `wasm`/`no-std`/`development-tools`, capi `wasm`/`external-ffi-bindings`/`development-tools`,
  CLI `wasm`/`command-line-utilities`/`development-tools`. Verified with `cargo package --no-verify` on
  each: clean. **These ship with the next publish (`1.0.0`)** — the already-published 0.9.0 crates do not
  have them.
- **Next release after v0.7.0: T7 — host imports + WASI preview 1** (0.8.0). Per-release checklist above
  — full doc sync before the publish handoff.
- **PowerShell note (owner's shell):** Windows PowerShell 5.1 has no `&&`. Chain the publishes with
  `cargo publish -p wasmrt-core; if ($LASTEXITCODE -eq 0) { cargo publish -p wasmrt-capi }; …` — not
  `; if ($?) {…}`, which reports the *`if` statement's* success after a skipped block and would run
  step 3 even when step 1 failed.
