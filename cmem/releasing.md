# Releasing & Versioning

How wasmrt is versioned and published so others can follow progress. Decisions locked by the owner
**2026-07-27**. Public-facing trackers: `ROADMAP.md` + `CHANGELOG.md` (repo root — NOT project memory,
but this file defines the process that keeps them current).

## Versioning = port-progress ladder (owner decision)

The destination is fixed and known (the completed, frozen `wazmrt` oracle). The version number measures
**how far the Rust port has climbed toward it**, and reflects what genuinely runs and passes its parity
gate — never inflated to signal the effort's maturity (that story is told by `ROADMAP.md`'s use-case
matrix + a fast cadence).

- **`0.x`** — in progress; each release lands one parity-gated stage.
- **`1.0.0`** — **full parity with the frozen wazmrt oracle** on both targets, all conformance gates green.
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
0.8 T7 wasi+cli · 0.9 T8 C-ABI · 0.10 T9 hardening · **1.0 = parity**.

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

1. **Task is DONE**: its parity/conformance gate passes, `cargo test`/`clippy` green on all surfaces,
   no regressions vs the frozen oracle.
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

## Status (2026-08-03)

- **Published through v0.7.0 — no release is pending.** 0.7.0 shipped the validator completion **and all
  of T6** (the text toolchain) together, as the owner decided on 2026-08-03. Spec-suite conformance at
  release: **98.4%** (54,509 passing); the remaining punch-list is deliberately deferred to after T7 —
  see `known-issues.md`.
  `wasmrt`, `wasmrt-core`, `wasmrt-capi` are live on
  crates.io through v0.7.0 (T0 v0.1.0 → T3 v0.4.0 →
  T4-core v0.5.0 → T5 integer v0.6.0 → float v0.6.1 → linear memory v0.6.2 → tables/reftypes v0.6.3 →
  WasmGC v0.6.4 → SIMD v0.6.5 → multi-memory v0.6.6 → threads/atomics v0.6.7 → memory64 v0.6.8 →
  exception handling v0.6.9 → **text toolchain + validator completion v0.7.0**). The owner
  runs each publish; the rhythm (implement →
  **full-doc prep** → owner publishes) holds. **v0.6.4 onward run under the pre-publish doc-sync gate, and
  each v0.6.4+ GitHub release was created automatically by the tag-triggered `.github/workflows/release.yml`**
  (push the `v*` tag → the release + notes publish themselves).
- **Still worth doing (nice-to-have, not blocking):** per-crate crates.io **listing metadata** —
  `keywords` (webassembly/wasm/runtime/interpreter/wasi), `categories` (`wasm`, `development-tools`,
  `no-std`), and a per-crate `readme` (cargo packages the readme, so a `../../README.md` path is
  rejected — give each crate its own short README or set `readme = false`). Verify whether these are on
  the published crates and add them on a future release if not.
- **Next release after v0.7.0: T7 — host imports + WASI preview 1** (0.8.0). Per-release checklist above
  — full doc sync before the publish handoff.
- **PowerShell note (owner's shell):** Windows PowerShell 5.1 has no `&&`. Chain the publishes with
  `cargo publish -p wasmrt-core; if ($LASTEXITCODE -eq 0) { cargo publish -p wasmrt-capi }; …` — not
  `; if ($?) {…}`, which reports the *`if` statement's* success after a skipped block and would run
  step 3 even when step 1 failed.
