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

**Stage → version map** (the ladder lives in `ROADMAP.md`; the port tasks in `roadmap.md`):
0.1 T0 scaffold · 0.2 T1 types+reader · 0.3 T2 opcode · 0.4 T3 decode · 0.5 T4 validate ·
0.6.x T5 interp (patch per feature slice) · 0.7 T6 text · 0.8 T7 wasi+cli · 0.9 T8 C-ABI ·
0.10 T9 hardening · **1.0 = parity**.

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

1. **Task is DONE**: its parity/conformance gate passes, `cargo test`/`clippy` green on all surfaces,
   no regressions vs the frozen oracle.
2. **Update the public trackers:**
   - `ROADMAP.md` — flip the stage row to ✅ and check off every use-case the release makes real.
   - `CHANGELOG.md` — move `Unreleased` items into a new `[x.y.z]` section (Keep a Changelog format).
3. **Update `cmem/roadmap.md`** — mark the task DONE (internal memory).
4. **Bump the version** — one place (`workspace.package.version`) plus the internal dep version in the
   root `[workspace.dependencies] wasmrt-core = { version = "x.y.z" }` (they MUST match or publish fails).
   `cargo-release` does both automatically; if bumping by hand, don't forget the dep pin.
5. **Publish in dependency order** (crates.io requires deps published first):
   `cargo publish -p wasmrt-core` → `-p wasmrt-capi` → `-p wasmrt`. (`cargo release` orders this.)
6. **Tag + GitHub release**: `git tag vX.Y.Z`; `gh release create vX.Y.Z` with the CHANGELOG section as
   notes.
7. **Commit + push** the version/tracker changes to `origin`.

## Tooling

- **`cargo-release`** (recommended): workspace-aware — bumps the shared version, syncs the internal dep
  pin, publishes in order, tags. `cargo install cargo-release`; then `cargo release minor` (or `patch`
  for an interp slice). Not yet installed as of 2026-07-27.
- Alternative: the manual steps above. `release-plz` if/when full CI automation is wanted.

## The "reflect a shipped stage" trigger (binding on every agent)

When a task/stage completes and is released, the public `ROADMAP.md` use-case matrix and `CHANGELOG.md`
are **part of the definition of done** — not optional docs. A release that advances the code without
advancing the matrix misreports progress to the people following along. Keep them in lockstep with the
version.

## Who runs the publish

The **owner** runs `cargo publish` + `git tag` + `gh release` for each version (needs the crates.io
token). The agent implements the task, prepares the release (version bump + tracker updates + commit),
and hands over the publish commands. Established rhythm since v0.1.0.

## Status (2026-07-28)

- **Published through v0.6.1** — `wasmrt`, `wasmrt-core`, `wasmrt-capi` are all live on crates.io at each
  released version (T0 v0.1.0 → T3 v0.4.0 → T4-core v0.5.0 → T5 integer v0.6.0 → T5 float v0.6.1). The
  owner runs each publish; the rhythm (implement → prep → owner publishes) has held for seven releases.
- **Still worth doing (nice-to-have, not blocking):** per-crate crates.io **listing metadata** —
  `keywords` (webassembly/wasm/runtime/interpreter/wasi), `categories` (`wasm`, `development-tools`,
  `no-std`), and a per-crate `readme` (cargo packages the readme, so a `../../README.md` path is
  rejected — give each crate its own short README or set `readme = false`). Verify whether these are on
  the published crates and add them on a future release if not.
- **Next release: v0.6.2 (T5 slice 3, linear memory).** Per-release checklist above.
