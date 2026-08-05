# Security Model

wasmrt carries over wazmrt's security design **to replicate**, not to reinvent. Authoritative source:
`../../wazmrt/cmem/security-model.md`; port detail in `docs/port/05-wasi-cli-pin.md` +
`docs/port/06-build-docs-licensing.md`. Two orthogonal properties, both required: **Authority** (what a
running guest may touch — BUILT in wazmrt) and **Authenticity** (is this the code I approved — pin BUILT,
signatures design-only).

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

**OPEN DECISION for the owner (blocks nothing; revisit at T8/T9):** spend a dependency (`cap-std`) or a
narrow, audited `unsafe` FFI shim (`openat`/`openat2` on unix, `NtCreateFile` on Windows) to close the
residual — or accept it and document the deployment assumption. Note wazmrt itself still has a narrow
**final-component** residual on `path_open` (`#17/#18/#23`, a Windows Zig-std workaround), so the port is
not obviously behind the oracle here; it trades a narrower residual for a wider one.

**Mandated adversarial test — DONE and it bites:** `wasi/fs.rs` keeps a canary outside the preopen and
asserts no walk can ever produce a path that reads it, over absolute / relative / chained / symlinked-
directory escapes. It asserts the **outcome** (never reads `SECRET`) rather than a particular errno, so
it survives a change of mechanism. Verified by mutation: deleting the `..` guard makes it fail.

## Authenticity — pin verification (reproduce; signatures still design-only)

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
