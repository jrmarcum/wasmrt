# interop.md — the **wasmrt ⇄ wazmrt swappability contract**

**CONTRACT VERSION: 1** · opened 2026-08-19 (owner) · **this file is IDENTICAL in both repos**
(`wasmrt/cmem/interop.md` and `wazmrt/cmem/interop.md`).

> *"I think this may require a common memory md file in both projects to confer with each other on from
> this point further so that both projects are on the same end track."* — owner, 2026-08-19

This is the **only** document either project may treat as binding on the other. Everything else in
either `cmem/` is that project's own memory, and 🔒 **the oracle is still retired**: neither runtime is
authoritative over the other's design, and reading a competitor's implementation for guidance remains
off-limits. What lives here is a **contract about observable behaviour**, agreed by the owner, that both
must satisfy so a deployment can swap one binary for the other.

---

## 0. Scope — what "swappable" means

🔒 **Defined by the owner, 2026-08-19:** *"If our CLI options are the same they are swappable. If our
security checks are the same they are also swappable."*

**IN scope — must match:**

1. **CLI options** — run modes, flag names, argument shapes, defaults, exit codes.
2. **Security checks** — the pin/verification mechanism and its on-disk artifacts, the WASI sandbox
   rights model, and the resource ceilings.

**OUT of scope — deliberately NOT aligned:**

- **The C ABI.** `wasmrt_*` and `wazmrt_*` symbol prefixes are a recorded deliberate decision in both
  projects. An **operator** swaps the binary and the pin DB; an **embedder** links one or the other and
  is not expected to relink blind.
- **Internal design** — data models, IR, allocation strategy, crate/module layout.
- **Performance and size.** These are what the two are *competing on*; aligning them would defeat the
  purpose. A contract row must never be justified by "the other one is faster".
- **Conformance internals** — each project's own test harness, scoring and baselines.

---

## 1. The change protocol — read this before editing

1. **Neither project edits this file unilaterally.** A change is *proposed* in one repo and lands in
   **both**, in the same **CONTRACT VERSION** bump, before either ships behaviour that depends on it.
2. **Every row carries a status and a date.** ✅ AGREED (with the evidence that verified it) ·
   ⚠️ DIVERGENT (with the agreed resolution) · ⬜ UNVERIFIED (nobody has checked; **do not quote it**).
3. **Every accepted divergence carries a REOPEN CONDITION.** *An entry that loses its condition has
   become an excuse* — and a condition that is never re-tested is an excuse more slowly, so **re-test it
   whenever the entry is priced.**
4. **A row is verified by RUNNING both, not by reading either.** *When a claim will leave this repo,
   verify it against the artifact, not against something that talks about the artifact.*

⚠️⚠️ **THE DRIFT HAZARD, NAMED UP FRONT.** This file exists **twice**, and *a list written out a second
time is a list that will drift* — the projects have already been bitten by exactly this shape (a feature
list spelled three times; a header advertising a switch that was not there). Two repos cannot share one
file, so the pin is the **CONTRACT VERSION** plus rule 1. **The gate:** when both trees are present, a
check compares the two copies byte-for-byte and fails on any difference. Until that check exists, treat
a version mismatch between the copies as **the contract being unknown**, not as "close enough".

---

## 2. The CLI contract

### 2.1 Run modes

wazmrt dispatches on the file extension and on whether an export was named; wasmrt uses explicit
subcommands. **The agreed target is ADDITIVE: each accepts the other's spelling, and neither loses the
form it already ships.**

| capability | wazmrt spelling | wasmrt spelling | status |
| --- | --- | --- | --- |
| summarize + validate, no execution | `wazmrt <module>` | `wasmrt <file>` | ✅ **AGREED** — already identical |
| call an exported function | `wazmrt <module> <export> [args…]` | `wasmrt run <file> <fn> [args…]` | ⚠️ **each must accept both** |
| run a WASI `_start` command | `wazmrt <module> [flags] [-- argv]` | `wasmrt wasi [flags] <file> […]` | ⚠️ **each must accept both** |
| run a `.wast` spec script | `wazmrt <script.wast>` | `wasmrt wast <file\|dir>… [-v]` | ⚠️ **each must accept both** |
| assemble `.wat` → `.wasm` | *(absent)* | `wasmrt wat <file.wat> [-o out]` | ⚠️ **wazmrt must grow it** |
| pin a module for the DB | `wazmrt pin <file\|dir> [--db <path>]` | *(absent — `pin` is a stub)* | ⚠️ **wasmrt must grow it** |
| keypair / signing tools | `wazmrt keygen`, `wazmrt sign` | *(design-only)* | ⬜ **deferred** — reopen when either ships signatures |
| `-h`/`--help`, `-v`/`--version` | **first argument only** | **first argument only** | ✅ **AGREED** |

⚠️⚠️ **ALIGNING THE RUN MODES CHANGES WHAT A BARE PATH DOES, AND IT IS A SECURITY-POSTURE CHANGE.**
wazmrt's `will_execute` predicate is *"an export was named **or** the module exports `_start`"*, so
`wazmrt prog.wasm` **runs** a WASI command — while `wasmrt prog.wasm` today **summarizes** it. Adopting
the predicate means the most casual invocation there is starts **executing** code where it previously
only inspected it. 🔒 **The verification gate must land before, or with, that change — never after.**

### 2.2 Flags

| flag | wazmrt | wasmrt | status |
| --- | --- | --- | --- |
| `--dir <host>[<sep>guest]` | separator `:` | separator `::` | ⚠️⚠️ **DIVERGENT AND LIVE — see below** |
| `--ro-dir` | same separator issue | same | ⚠️⚠️ as above |
| `--allow-symlink` | ✅ | ✅ | ✅ **AGREED** (both added 2026-08-10) |
| `--env KEY=VALUE` | ✅ | ❌ absent | ⚠️ **wasmrt must add** |
| `--max-memory <size>` | ✅ | ❌ absent at the CLI | ⚠️ **wasmrt must expose** (the ceiling exists) |
| `--max-table-elems <count>` | ✅ | ❌ absent at the CLI | ⚠️ **wasmrt must expose** (the ceiling exists) |
| `--features <list>` | ✅ | ❌ absent at the CLI | ⚠️ **wasmrt must expose** (gating exists in the C ABI) |
| `--` ends host flags, rest is guest argv | ✅ | ❌ (preopens must precede the path) | ⚠️ **wasmrt must add** |
| `--pins <path>` | ✅ | ❌ | ⚠️ **wasmrt must add** (with `pin`) |
| `--verify off\|warn\|enforce` | ✅ | ❌ | ⚠️ **wasmrt must add** (with `pin`) |
| `--no-verify`, `--yes` | ✅ | ❌ | ⚠️ **wasmrt must add** (with `pin`) |

⚠️⚠️ **THE `--dir` SEPARATOR IS A SWAPPABILITY BREAK THAT IS LIVE TODAY**, and it has nothing to do with
verification. `--dir .:/` is a working wazmrt invocation. On wasmrt, `split_once("::")` finds nothing, so
host **and** guest both become the literal string `.:/` — **it does not error; it preopens the wrong
thing.** Each side has a real reason: a single `:` is ambiguous with a Windows drive letter
(`--dir C:\data:/data`), which is why wasmrt chose `::`.

**AGREED RESOLUTION: both accept both.** Prefer `::`; fall back to a single `:` when the spec contains
no `::` **and** the split is not a drive letter. Converging on one spelling would break existing
invocations of the other, which is the opposite of swappable.

### 2.3 Exit codes

| behaviour | status |
| --- | --- |
| a WASI guest's `proc_exit(n)` becomes the process exit status (`n & 0xff`) | ✅ **AGREED** — verified in both, 2026-08-19 |
| success → 0, host-side failure (bad args, unreadable file, invalid module, refused by policy) → non-zero | ✅ **AGREED** |
| **specific non-zero codes per failure kind** | ⬜ **UNVERIFIED** — neither has been compared; a script that branches on a specific code is not yet portable |

### 2.4 Flag-parsing rules that are part of the contract

- **`-h`/`--help` and `-v`/`--version` are recognised as the FIRST argument only**, so a `--help` inside
  a guest's argv is never the host's. ✅ AGREED.
- ⚠️ **Verification flags are recognised only in the LEADING RUN of host flags.** Scanning "everything
  before `--`" is **not** sufficient: the common WASI form has no `--` at all
  (`… prog.wasm install --yes`), so the guest's own arguments get searched and a `--yes` meant for the
  guest **silently disables verification**. wazmrt paid for this; wasmrt must not re-buy it.

---

## 3. The security-check contract

### 3.1 What gets hashed — the TOCTOU rule

| rule | status |
| --- | --- |
| hash the **in-memory bytes about to execute**; never re-read by path | ✅ **AGREED** — `bytes-hashed == bytes-run` by construction |
| a `.wat` input hashes the **assembled** bytes, not the source text | ✅ **AGREED** |
| a `.wast` script hashes the **script bytes** — every module it can run is contained in them | ✅ **AGREED** |
| the file is read **once**; no path is reopened after load | ✅ **AGREED** in behaviour · ⚠️ **wasmrt is making it a compiler-checked type** (`Loaded { bytes, digest }`), wazmrt passes a slice — an implementation difference, not a contract difference |

⚠️⚠️ **`.wast` MUST BE GATED.** A script instantiates and invokes the modules it contains — including
`(module binary "…")` raw payloads. wazmrt shipped this bypass: `wazmrt payload.wast` ran unpinned,
unsigned wasm **even under a root-owned `# mode: enforce`**. **Any wasm can be wrapped in a `.wast`, and
the attacker chooses the extension, so the bypass needs no privilege.**

### 3.2 When the gate runs

| rule | status |
| --- | --- |
| the gate runs on a `will_execute` predicate; a pure **summarize/inspect** path is never gated | ✅ **AGREED** |
| the gate runs **BEFORE validation** — authorization first, so an unauthorized module is refused as *unauthorized* rather than parsed and reported on | ✅ **AGREED** |

### 3.3 The pin DB — a SHARED ON-DISK ARTIFACT

| item | agreed value |
| --- | --- |
| **location** | ⚠️ **DECISION NEEDED — see below.** Today: `/etc/wazmrt/pins` · `C:\ProgramData\wazmrt\pins` |
| ownership | **root-owned, read-only to the user, plaintext.** Integrity from **ownership, not secrecy** |
| format | one lowercase-hex SHA-256 per line; blank lines and `#` lines ignored; whitespace-separated text after the hash is a human label and is ignored |
| addressing | **content-addressed — no paths in the DB**, so moving or renaming an approved file does not re-open a hole |
| policy directive | `# mode: off\|warn\|enforce` — the policy inherits the DB file's **ownership** |
| pinning time | at **install** time, with privilege — a verified install, **not** TOFU |
| no encryption | a category error: encryption gives confidentiality; what is needed is integrity |
| no machine-binding | the attacker **is** the user |

⚠️⚠️ **THE PATH IS THE MOST DANGEROUS UNRESOLVED ROW IN THIS FILE.** If each runtime reads its own path,
**swapping the binary finds no DB, computes `armed = false`, and silently runs everything** — a security
downgrade with **no error message**, which is the worst defect class either project tracks.

**RECOMMENDED (owner decision pending): one shared path named for the deployment** —
`/etc/wasmtk/pins` and `C:\ProgramData\wasmtk\pins`, since `wasmtk` is what both are being included in.
Each may keep its own legacy path as a fallback. 🔒 **Whatever is chosen, a swap must not be able to
disarm silently:** if a runtime finds no DB where its sibling would have found one, that is worth saying
out loud rather than treating as "unarmed".

### 3.4 The `decide()` matrix — this must match exactly

Inputs: `explicit` (the DB's `# mode:`, or none) · `pinned` · `opt_out` (`--no-verify`/`--yes`) ·
`tty` · `armed`.

| # | condition | action |
| --- | --- | --- |
| 1 | `pinned` | **Run** — the DB approved it |
| 2 | `explicit = off` | **Run** |
| 3 | `explicit = enforce` | **Deny — ABSOLUTELY.** `opt_out` and `tty` are ignored: authority comes from the root-owned policy, never from a runtime argument |
| 4 | `explicit = warn`, `opt_out` | **Run** (with a warning printed) |
| 5 | `explicit = warn`, no `opt_out`, `tty` | **Prompt** |
| 6 | `explicit = warn`, no `opt_out`, no `tty` | **Deny** |
| 7 | no `explicit`, **not armed** | **Run** — nothing to verify against |
| 8 | no `explicit`, armed, `opt_out` | **Run** (with a warning printed) |
| 9 | no `explicit`, armed, no `opt_out` | **Deny** |

**Armed** = a root key is embedded **or** a pin DB is present. A bare build with neither runs
everything, so "costs nothing when unarmed" is structural rather than promised.

**`--verify` may only RAISE strictness** above the DB-declared policy, never lower it. **Under a
root-owned `# mode: enforce`, both `--pins` and `--verify` are ignored** — the pin set *and* the policy
come from root.

### 3.5 Fail-closed rules — both bought by defects, both binding

| rule | why |
| --- | --- |
| a present `# mode:` with an **unrecognised value** means **`enforce`**, not "no policy" | a typo (`# mode: enfroce`), odd capitalisation or a trailing comment must not silently degrade to a state `--no-verify` can then override. **A root-intended enforce must never be downgradable by a misspelling.** |
| a DB content line whose first token is **not a valid 64-hex digest** is an **error**, not a skipped line | a truncated or mangled DB must fail **loud**; silently dropping approvals makes a pinned module look "not in the list" — which reads as an attack and hides a corrupt file |
| `--verify <typo>` is an **error**, not a default | same reasoning as the `# mode:` rule, at the other input |
| an override that *would* have blocked **prints a warning** | never silently unverified |

### 3.6 The WASI sandbox rights model

| property | agreed value | status |
| --- | --- | --- |
| `PATH_SYMLINK` exists as a right | **bit 24** | ✅ **AGREED** (wazmrt had **no such right at all** until 2026-08-10 — the gap that started this table) |
| `PATH_SYMLINK` is in the **write mask** | yes, so `--ro-dir` strips it | ✅ **AGREED** |
| `--dir` grants | `ALL & !PATH_SYMLINK` — **symlink CREATION denied by default** | ✅ **AGREED** (owner, 2026-08-10) |
| `--ro-dir` grants | `ALL & !WRITE_MASK` | ✅ **AGREED** |
| `--allow-symlink` | opts creation back in, for installer-shaped work | ✅ **AGREED** |
| following a **pre-existing** link | allowed — the grant governs **creation**, not traversal | ✅ **AGREED** |
| an escaping link target | refused **at creation**, independently of the follow-time check | ✅ **AGREED** |
| no `--dir` at all | every path call is `BADF`; **there is no implicit cwd** | ✅ **AGREED** |
| the full `oflags`/`fdflags` sets, and which right each `path_*` handler demands | ⬜ **UNVERIFIED** — this is the T12x row-by-row diff, still to be run |

### 3.7 Resource ceilings

| ceiling | default | status |
| --- | --- | --- |
| max linear memory | **`1 << 30`** (1 GiB) | ✅ **AGREED** — verified in both, 2026-08-19 |
| max table elements | **`1 << 27`** (128 M) | ✅ **AGREED** — verified in both, 2026-08-19 |
| max call depth | **512** | ✅ **AGREED** — verified in both, 2026-08-19 |
| an execution time / fuel bound | **neither has one** — a hostile guest can hang the host | ⬜ **OPEN in both.** wazmrt has it as a decision gate in its hardening track; wasmrt in T12. **Whatever is decided must be decided together, or the two stop being swappable under load.** |

---

## 4. The differential checks that keep this honest

⚠️ *Two implementations of one spec are a free differential oracle; not using them against each other is
the waste.* These checks are the contract's only real enforcement — a row marked ✅ that nothing re-runs
decays exactly like any other claim.

| # | check | catches |
| --- | --- | --- |
| 1 | **byte-compare the two copies of this file** | the drift hazard in §1 — the one failure that makes every other row meaningless |
| 2 | **assemble the shared `.wat` corpus with both, diff the SHA-256 of the outputs** | ⚠️ a pinned `.wat` that validates under one runtime and is refused by the other. **Not hypothetical** — wasmrt has four recorded defects where its emitter produced a different module than the text described. If the assemblers disagree, **only `.wasm` digests are portable** and that must be documented, not discovered |
| 3 | **run the same pin DB + the same module under both**, across all nine `decide()` rows | a policy that is honoured *differently*, which is worse than not being honoured |
| 4 | **row-by-row diff of the WASI rights tables** (§3.6) | the original finding that opened this file: a right present in one and absent in the other, where the read-only test passes trivially because the right is not in the mask |
| 5 | **the same CLI invocation under both**, for every row of §2 | the `--dir` separator class — a flag that does not error and does the wrong thing |

⚠️ **A disagreement found by any of these is recorded as an OBSERVATION until its cause is traced.**
Neither runtime is the oracle, so "the other one does X" is not a diagnosis.

---

## 5. Owner decisions this file is waiting on

| # | decision | why it blocks |
| --- | --- | --- |
| 1 | **The shared pin DB path** (§3.3) | until it is decided, a swap can silently disarm verification |
| 2 | **Who accepts whose CLI spelling, and by when** (§2.1) | the additive plan needs both halves; `wasmrt wat` and `wazmrt pin` each exist on one side only |
| 3 | **Fuel / execution bound** (§3.7) | if one ships a bound and the other does not, a workload that completes under one hangs under the other |
| 4 | **Exit-code table** (§2.3) | only needed if scripts are expected to branch on specific codes |

---

## 6. Change log

| version | date | change |
| --- | --- | --- |
| **1** | 2026-08-19 | Opened. Scope set by the owner (CLI options + security checks; C ABI explicitly out). Recorded: the `--dir` separator break and the bare-path-executes consequence, both live today; the pin DB path risk; the nine-row `decide()` matrix; the ceiling defaults, verified equal in both; the WASI rights rows already agreed. |
