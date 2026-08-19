# Security Model

🔒 **THIS FILE IS THE AUTHORITATIVE SOURCE (2026-08-11).** It previously deferred to
`../../wazmrt/cmem/security-model.md`; that pointer is **retired** along with the oracle
([design-decisions.md](design-decisions.md)) — wazmrt is a competitor now, and its security posture is
neither binding on wasmrt nor a safe thing to inherit unread. Port detail remains in
`docs/port/05-wasi-cli-pin.md` + `docs/port/06-build-docs-licensing.md`.

Two orthogonal properties, both required: **Authority** (what a running guest may touch — BUILT) and
**Authenticity** (is this the code I approved — **`pin` is still a STUB**; a build performs *no*
authenticity check, signatures design-only). ⚠️ Do not read "Authenticity" as implemented anywhere in
this file.

⚠️ **The record below already shows why inheriting was the wrong posture:** on 2026-08-10 wasmrt gated
symlink *creation* correctly and wazmrt had **no `path_symlink` right at all**, so its `--ro-dir` never
stripped it — a guest could plant links in a read-only preopen. Replicating "the authoritative source"
faithfully would have replicated the hole. **T12x** exists to diff the two runtimes' security tables
*as independent implementations*, which is the useful form of the comparison.

## 🔎 A dedicated adversarial review is scheduled: **T12 (`1.0.4`)** — owner, 2026-08-06

This file records the security **design** — what the model is and why. **T12 is where it gets attacked
on purpose**: a review-and-recommend phase over the *final* code (hence after the bug hunt and the
optimization pass), framed around three distinct adversaries — a hostile **guest**, a malformed
**input** to the tooling that may never execute, and a careless or hostile **embedder** misusing the C
ABI. Full scope in [roadmap.md](roadmap.md) (T12a–f).

**Two findings are already on its list, both from the release profile rather than from any code:**

- **`panic = "abort"` makes every reachable panic a host-process kill.** For a library whose purpose is
  to contain untrusted code, a panic reachable from hostile input is a denial of service against the
  embedder — no unwind, no error, no recovery. The spec suite already caught one (`v128.const i64x2`
  hit an `unreachable!()` and aborted the run); the rule from that fix stands: **a library must reject
  a module, never abort the embedder.**
- **No `overflow-checks` in release** — arithmetic **wraps in release and panics in debug**. Both are
  bad on hostile input, in opposite directions: debug gives the abort above, release gives a *wrong
  value* that may then be used as an index, length or offset.

Neither is a vulnerability on its own; both are surfaces, and T12 decides what to do about them.

## ✅ wasmrt gates symlink CREATION correctly — the oracle did not (2026-08-10)

The owner's requirement: *"we do not want the wasm runner creating symlinks at runtime; links should
exist before the run; and a bad actor must not swap one mid-run."* Where each part stands:

| requirement | wasmrt |
| --- | --- |
| a guest cannot plant an **escaping** link | ✅ absolute and lexically-climbing targets refused **at creation** (`NOTCAPABLE`), independently of the follow-time check |
| symlink creation is a **write** right | ✅ `PATH_SYMLINK` is in `WRITE_MASK`, so **`--ro-dir` strips it** |
| pre-existing in-sandbox links still work | ✅ followed normally; the escape check is on the target, not on links as such |
| no mid-run swap by a third party | ⚠️ **the known TOCTOU residual** — see below |

⚠️ **The oracle had a hole here and wasmrt did not.** wazmrt had **no `path_symlink` right at all**
(bit 24 missing), so `--ro-dir` never stripped it and its handler demanded `path_open` — a guest could
plant links in a read-only preopen. Fixed 2026-08-10 in `wazmrt@4a6d745`. Recorded here because the
**asymmetry** is the reusable finding, not the bug: see roadmap **T12x**, diff the two runtimes'
security tables against each other.

✅ **DECIDED + SHIPPED (owner, 2026-08-10): symlink creation is DENIED BY DEFAULT.** *"Symlink creation
during runtime for the purpose of running processes shall be illegal; creating symlinks for a program
install situation should be legal."* So `--dir` now grants `READ_WRITE = ALL & !PATH_SYMLINK`, and
`--allow-symlink` opts back in for installer-shaped work. Same in the oracle (`wazmrt@1a88b84`).

The reasoning is worth keeping: **composing modules over shared linear memory is the STORE's job** —
imported memories are genuinely shared since T9a#4 — so nothing in a normal run needs the filesystem to
grow new links. Denying creation removes a guest-controlled primitive a **second process** could later
repoint, which is exactly what makes the accepted TOCTOU residual survivable.

⚠️ **Enforced at COMPILE TIME**, not by a test: every operand is a constant, so `const _: () =
assert!(…)` makes a violation fail the **build** — including for any crate depending on this one.
Mutation-verified: restoring `PATH_SYMLINK` to the default grant produces
*"evaluation panicked: assertion failed"*. ⚠️ The policy governs **creation**; following a pre-existing
link needs `PATH_OPEN`, which both grants keep, and that too is asserted — otherwise this change would
silently become "break every guest that reads through a link the operator placed".

## 🔒 Validation is a SECURITY boundary, and every entry point must cross it (2026-08-10)

**Validation is what establishes the invariants the interpreter assumes** — stack heights, operand
types, index ranges. §4.5.1 defines instantiation only for a *valid* module, so running an unvalidated
one is outside the spec, and every defensive check downstream is being asked to hold a line validation
was supposed to have held already.

⚠️ **`wasmrt run` did not validate.** It decoded and executed, so an ill-typed module ran and printed a
plausible answer with exit 0 — while `wasmrt wasi`, one function away, refused the same bytes. The
oracle was worse: **both** of wazmrt's execute paths *and* its C ABI `wasm_module_new` skipped it. Fixed
in both repos 2026-08-10.

**Current state — the property, checked at every entry point** (which is the method, not the
afterthought; see roadmap **T12z**):

| entry point | validates before executing? |
| --- | --- |
| `wasmrt <file>` (summarize — never executes) | ✅ reports a verdict |
| `wasmrt run` | ✅ *(was ❌)* |
| `wasmrt wasi` | ✅ |
| C ABI `wasmrt_module_new` | ✅ |
| Rust `Instance::new` / `Store::instantiate` | ⚠️ **documented precondition, by design** |

**Why the Rust API is the deliberate exception.** It mirrors wasmtime's compile/instantiate split and
keeps validation off the path for callers who already did it. That is safe *in practice* because every
**shipped** entry point validates, so reaching an unvalidated instantiation requires an embedder to skip
`validate::validate` on purpose. 🚦 **Whether a `Module` should be able to exist unvalidated at all is
recorded as an open question for T12** (`design-decisions.md`): making it unrepresentable means a
validated-`Module` type — a breaking API change, and a real cost, for a case no shipped surface reaches.

**Severity bound, stated honestly.** `forbid(unsafe_code)` means the worst case here is a wrong answer or
a panic, never memory corruption — a type-confusion probe (an `i32` into a `funcref` table, then
`call_indirect`) trapped cleanly. wazmrt has no such bound: Zig's ReleaseFast/ReleaseSmall remove the
safety checks, and its own source records this class reaching *a segfault from a 31-byte module*. **The
same missing check is a different severity in the two languages**, which is worth remembering whenever a
finding is carried across the port boundary.

## Two new authority controls at T8 / v0.9.0 (2026-08-06) — reachable from C

Both are **new capability wazmrt does not have**, and both are enforced where they cannot be bypassed:

- **Proposal gating** (`Features`, `wasmrt_config_set_feature`) — an embedder can refuse SIMD, GC,
  threads, memory64 and 10 more, shrinking the language a guest may use and with it the interpreter
  surface an attacker can reach. **Enforced at VALIDATION, never at execution**: a module naming a
  disabled proposal is simply *invalid*, so nothing half-checked ever reaches the interpreter. Types are
  gated as well as opcodes — a `(local v128)` or a `(struct …)` type is refused even with no instruction
  in sight, which is the hole a naive opcode-only gate would leave.
  - **The honesty rule that keeps this a real control:** a flag exists only for a proposal wasmrt
    actually implements. **There is no `tail_call` flag**, because those opcodes are unimplemented — a
    toggle for them would gate nothing while *reading to an operator as a security control*, which is
    strictly worse than its absence.
  - Incoherent sets (`gc` without `function_references`) are **rejected, not repaired**: silently
    enabling a dependency would accept modules the embedder meant to refuse.
- **Resource ceilings** (`ResourceLimits`) — memory bytes, table elements, call depth, GC objects and
  boxed exceptions are now per-store rather than compile-time, so a host can bound a guest's appetite
  instead of relying on constants it cannot see. Each is a ceiling, not a reservation.

**Unchanged:** the FS sandbox is still the entire authority story for files, and every property in the
section below holds exactly as before — the C ABI adds no path a guest can take.

## Sandbox / Authority — reproduce EXACTLY, keep the guarantee

- Guest can execute nothing (no `proc_exec`/spawn in WASI p1; `proc_exit` self-terminates); cannot
  reach a non-WASI import (unknown → `NOTSUP` stub, any other module import → trap); the preopen is the
  **entire** FS authority (no `--dir` ⇒ zero files reachable).
- **Rights only ever narrow** — `path_open` intersects the new fd's rights with the parent dir fd's
  inheriting rights, so a `--ro-dir` read-only preopen propagates read-only to the whole subtree by
  construction (no per-syscall write check).
- **The resolver is RESOLVE_BENEATH in userspace** (`walkFull`): a stack of open dir handles (bottom =
  the preopen, never popped), resolving **one component at a time, no-follow, through the held handle**
  (TOCTOU-safe against intermediate-symlink redirection). Symlinks are *followed* but expanded through
  the same loop; `..` can never rise above the preopen (there is no handle above it → up-escape
  impossible); absolute symlink targets **re-base to the preopen root**; a `symlink_max` budget → ELOOP.
  **Security is a property of the construction, not of checking target strings.** Reproduce this — do
  NOT hand a full guest path to a directory API.
- **Mandated adversarial test:** the guest authors the symlink topology; a canary file *outside* the
  preopen is the oracle and must never be read (`examples/wasi_symlink_traversal.zig`).

### ⚠️ Rust divergence — CORRECTED 2026-08-05, and it is a real one

**The earlier claim in this file was wrong and is retracted.** It read: *"Rust's std / `cap-std` /
`openat2(RESOLVE_BENEATH)` can do the atomic no-follow open and close that residual for free."* Grouping
`std` with the other two was the error, and it was load-bearing — the whole T7c-2 plan rested on it.

**Rust's `std` has no dir-relative open.** No `openat`, no `O_PATH` handle to re-open through, on any
platform (verified against the 1.99-nightly sysroot, 2026-08-05). `std` can do a no-follow *stat*
(`symlink_metadata`), which is a different primitive: it tells you what a name pointed at a moment ago,
it does not pin the inode. Inode pinning is the whole of what makes the oracle's walk TOCTOU-safe.

That leaves three project constraints that **cannot all hold at once**:

1. **zero dependency** (owner, 2026-08-04 — rejected `cap-std`/`openat2`);
2. **no `unsafe`** (owner's safety directive — a `libc::openat` decl is `unsafe` by construction);
3. **resolve through held handles, never a full path string**.

**Shipped, pending the owner's call:** 1 and 2 held, 3 relaxed to a **component-accumulated path**
(`wasi/fs.rs::walk`). What this costs is precisely bounded, and is much less than dropping 3 sounds:

- **Unaffected — every escape property still holds by construction**, because they are *lexical* and
  hold on an accumulated path exactly as on a handle stack: `..` cannot pop below the bottom; an
  absolute symlink target re-bases to the preopen root; a symlink's target is expanded through the same
  loop; device/NT-namespace components and embedded NULs are refused (now **up front**, before touching
  the filesystem, so a refusal never depends on filesystem state or leaks whether a prefix exists);
  `SYMLINK_MAX` bounds cycles.
- **Lost — inode pinning only.** A *concurrent writer inside the preopen* could swap a component
  between the walk and the syscall. The guest cannot be that writer (preview 1 here is single-threaded),
  so this needs a second process holding write access to the sandbox — which is the embedder's choice of
  what to preopen.
- **Compensating control:** `verify_beneath` canonicalizes the resolved directory after the walk and
  re-checks it against the canonical preopen root, so a swap that *did* land still cannot yield a path
  outside the sandbox. **It closes the escape, not the race.**

### ✅ DECIDED 2026-08-05 (owner): **accept the residual and document it.** Do not re-litigate.

`cap-std` was rejected again (first runtime dependency, a large tree, against the smallest-binary goal),
and an `unsafe` FFI shim was rejected (it would punch the first hole in `#![forbid(unsafe_code)]`, which
was made mechanical the same day). **wasmrt keeps zero dependencies and zero `unsafe` in the engine.**

**The deployment assumption this creates, stated plainly so an embedder can check it:** wasmrt's sandbox
guarantees a guest cannot *name* anything outside its preopen. It does **not** guarantee that a path
resolved inside the preopen still refers to the same inode by the time the syscall runs. Closing that
gap requires the embedder to ensure **no untrusted second process holds write access to a preopened
directory while a guest is running**. The guest itself cannot create the race (preview 1 is
single-threaded here), so this only matters for multi-tenant or shared-scratch layouts.

Context for the choice: wazmrt itself still carries a narrower **final-component** residual on
`path_open` (`#17/#18/#23`, a Windows Zig-std workaround), so the port is not far behind the oracle — it
trades a narrow residual for a wider one, in exchange for holding both the zero-dep and no-`unsafe`
lines. Revisit only if a consumer's threat model actually includes a hostile co-tenant.

**Mandated adversarial test — DONE and it bites:** `wasi/fs.rs` keeps a canary outside the preopen and
asserts no walk can ever produce a path that reads it, over absolute / relative / chained / symlinked-
directory escapes. It asserts the **outcome** (never reads `SECRET`) rather than a particular errno, so
it survives a change of mechanism. Verified by mutation: deleting the `..` guard makes it fail.

## Authenticity — pin verification (reproduce; signatures still design-only)

> **⚠️ STILL NOT BUILT — re-verified 2026-08-07 after the T9 first pass.** T9's first pass covered
> T9a's top defects plus T9b/T9c/T9d (size, performance, licensing); **`pin` (T9e) was not part of it
> and remains untouched.** `crates/wasmrt-core/src/pin.rs` is a
> **doc-comment stub** — no `decide()`, no DB parsing, no hashing. Everything below describes the
> **oracle's** implementation and the mechanism decisions to reproduce, not current wasmrt behaviour.
> **A wasmrt build today performs no authenticity check of any kind**, and the CLI exposes none.
> Ported at **T9** (it was previously slated for T7, then T8, and slipped both times). The *authority*
> half of the model — the WASI sandbox — **is** fully built; do not confuse the two.

## 🔎 REVIEWED 2026-08-19 — "load once into memory" vs. the verification track (owner's question)

**The owner's proposal:** *"instead of verifying the wasm files, the wazmrt team loads the files into
memory and does not re-access them from the drive after load. This way the file can't be swapped out
mid-run. I think this is better than the verification track."*

**Reviewed rather than adopted, per §2.1/§2.2a — a stated benefit is a hypothesis about someone else's
code, so their code got read.** Three findings, and the first one changes the question.

### 1. ⚠️ The premise does not hold: wazmrt does BOTH, and load-once is what makes verification sound

wazmrt has **pin verification built and armed** — `src/pin.zig` (root-owned SHA-256 allow-list) plus
`src/sign.zig` (Ed25519), and per its own `security-model.md` it **denies unsigned modules by default
when armed** (a root key is embedded *or* a pin DB is present). It is not running load-once *instead of*
verifying. **Load-once is the discipline that makes its `verifyGate` honest**: the gate hashes the
in-memory bytes rather than re-reading the path, so `bytes-hashed == bytes-run` by construction.

**That is the same sentence this file has carried since the mechanism was decided** — see *"TOCTOU
closed by construction"* above, which pre-dates the question. So the owner's mechanism is not an
alternative to the recorded design; **it is the recorded design's load-bearing half.**

### 2. ✅ wasmrt ALREADY HAS the property — it just is not gated

Every CLI path that *executes* a module goes through one loader, `read_module_bytes`
(`crates/wasmrt/src/main.rs:200`), used by summarize (`:104`), `run` (`:424`) and `wasi` (`:532`). It
performs **exactly one `std::fs::read`** into a `Vec<u8>`; `.wat` is assembled from that buffer, and
assemble → decode → validate → instantiate → execute all run on owned memory. **The path is never
reopened, so the file cannot be swapped mid-run.** The C ABI never touches the filesystem at all — the
embedder hands over bytes. The only other `fs::read`s in the binary are `wasmrt wat` (an assembler that
does not execute) and the `.wast` runner (a test harness).

The loader's own doc comment already says why: *"What executes is the assembled bytes, not the file on
disk. That matters for anything that hashes what it runs."*

⚠️ **But it is a comment, not a check** — and *a goal with no gate is a preference* (§4.1). A future
fourth loader, or a streaming/`mmap` "optimization" at T11, could take the property away and nothing
would fail. **Action: make load-once an explicit tested invariant** — one test asserting the executing
paths take `&[u8]` rather than a path, and a `T12z`-style row in the entry-point table. Cheap, and it is
the half of the owner's proposal that is actually missing.

### 3. 🚦 They answer DIFFERENT questions, so this is a scope decision, not a substitution

| property | answered by | attack it stops |
| --- | --- | --- |
| **integrity over time** — the bytes cannot change under me between check and run | **load-once** (already held) | swap the file mid-run; win a race against the loader |
| **authenticity** — these are the bytes I authorized *in the first place* | **pin / signature** (not built) | replace the file **before** the load, with any other valid module |

⚠️ **Load-once alone gives zero protection against the strictly easier attack.** An adversary who can
write the file does not need to win a race — they overwrite it while nothing is running, and a
load-once runtime loads the replacement perfectly and executes it. **Dropping verification in favour of
load-once trades away defence against the easy attack in order to keep defence against the hard one.**

**The honest counter-argument, which is real:** for wasmrt's actual consumers (`rsxtk`, `wasmtk`,
`universalWasmLoader-*`), the module usually ships in the same package as the host binary — and an
attacker who can rewrite the `.wasm` can generally rewrite the host too, against which no runtime check
helps. That is exactly why wazmrt's pin is **inert unless armed**: it costs nothing when no root key and
no DB are present, and the *deployment* decides whether it applies.

### 📌 Recommendation

1. **Keep load-once and GATE it** — it is already true, it is free, and today nothing defends it.
2. **Do not delete the authenticity track; re-scope it.** `pin` stops being a `1.0.1` headline and
   becomes a small, default-`off` mechanism whose cost is zero when unarmed. It is ~a hash, a plaintext
   root-owned DB read, and the `decide()` matrix — all of it already specified above, so the expensive
   part (deciding the mechanism) is done.
3. **If the owner still wants it dropped, record the decision with its threat model** — *"wasmrt does
   not answer authenticity; the embedder must"* — in `wasmrt.h` and the README, not just here.
   ⚠️ An undocumented gap gets found twice: once by us, and again by T12. *Say what you did not do*
   (§6.3).

*(Owner decision pending. Nothing in the code changed for this entry — it is a review.)*

- **Pin verify is BUILT in wazmrt (Phase 5):** SHA-256, plaintext **content-addressed** DB,
  `# mode: off|warn|enforce`, and a pure `decide(policy, pinned, opt_out, tty)` matrix.
- **Decided mechanism (do NOT re-derive):** the pin DB is **root-owned, read-only to the user,
  plaintext** — integrity from **ownership, not secrecy**; pinning is done at install time with
  privilege (**verified install, NOT TOFU**); **no encryption** (category error — gives confidentiality,
  we need integrity); **no machine-binding** (the attacker *is* the user).
- **TOCTOU closed by construction:** hash the **in-memory bytes about to run**, never re-read by path
  (`bytes-hashed == bytes-run`). The opt-out (`--no-verify`/`--yes`) can only *raise* strictness and is
  **refused under `enforce`** (denied before it is consulted); DB parse **fails closed**. Default `off`.
- **Unifying invariant:** integrity is anchored by **ownership or a signature — never by secrecy**.
- **Still design-only (do not block the port):** the signature path — trust anchor (embed the root
  **public** key in the binary), signature format (a wasm custom section signed over every other byte),
  revocation, and the deny-unsigned default. Honest threat model: a root attacker defeats it in software
  → the real answer is *detection* (measured boot / TPM), out of scope for the runtime.
