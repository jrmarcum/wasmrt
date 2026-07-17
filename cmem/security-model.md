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

### Rust divergence (allowed — same behavior, better mechanism)

wazmrt's `#17/#18/#23` are **Zig-0.16-std-specific** workarounds (a Windows `openFile(.follow=false)`
host-crash forces a stat-then-open-with-follow, leaving a narrow **final-component** TOCTOU residual on
`path_open`). Rust's std / `cap-std` / `openat2(RESOLVE_BENEATH)` can do the atomic no-follow open and
**close that residual for free.** The invariant "resolve through held handles, never a full path string"
is what carries over — not the Zig workarounds. (Open decision: zero-dep vs. adding `cap-std` — see
`design-decisions.md`.)

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
