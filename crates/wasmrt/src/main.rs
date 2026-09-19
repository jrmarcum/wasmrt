//! `wasmrt` — the command-line interface.
//!
//! Grows across the roadmap (`cmem/roadmap.md`). Today it summarizes + type-checks a module,
//! calls an export, runs a WASI preview-1 program, assembles `.wat`, and runs `.wast` scripts.

// The CLI carries no `unsafe` either; `forbid` keeps it that way (`cmem/design-decisions.md`).
#![forbid(unsafe_code)]

use std::process::ExitCode;

use wasmrt_core::interp::{self, Instance, Value};
use wasmrt_core::module::{self, Extern, Module};
use wasmrt_core::types::ValType;
use wasmrt_core::features::{Feature, Features};
use wasmrt_core::interp::ResourceLimits;
use wasmrt_core::pin::{self, Action, Mode};
use wasmrt_core::validate::{validate, ValidateError};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        // No arguments is a usage error, not a request for the version: `interop.md` §2.3 makes
        // bad arguments non-zero, and wazmrt exits 1 here (measured 2026-09-19). This printed the
        // version and exited 0.
        None => {
            print_help();
            ExitCode::FAILURE
        }
        Some("-v" | "--version") => {
            println!(
                "wasmrt {} (abi {})",
                wasmrt_core::VERSION,
                wasmrt_core::abi_version()
            );
            ExitCode::SUCCESS
        }
        Some("-h" | "--help") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some("run") => run_export(&args[2..]),
        Some("wasi") => run_wasi_module(&args[2..]),
        Some("wat") => assemble_wat(&args[2..]),
        Some("wast") => run_wast(&args[2..]),
        Some("pin") => pin_modules(&args[2..]),
        // A bare module path, optionally preceded by host flags (`wasmrt --features mvp m.wasm f`).
        // An unrecognised flag here is reported by the flag parser itself, as `unknown flag`.
        Some(_) => run_bare(&args[1..]),
    }
}

/// Does `a` have the SHAPE of a flag? `-` alone (stdin by convention) and `--` (end of host flags)
/// are not flags.
fn is_flag(a: &str) -> bool {
    a.starts_with('-') && a != "-" && a != "--"
}

/// 🔒 **`interop.md` §2.4a — the UNKNOWN-FLAG rule (owner, 2026-09-19).** A flag-shaped argument in a
/// HOST-FLAG position that this command does not recognise stops the run: stderr says `unknown flag`
/// and names it, and the exit status is 1. Host-flag positions are everything before the module path,
/// the leading run of flags immediately after it, and — for a command with no guest argv (`wat`,
/// `wast`, summarize) — every argument. Guest positions (after `--`, or after the first non-flag
/// argument that follows the path) are never examined.
///
/// ⚠️ One exception, decided with the rule: **a single-dash token immediately after the module path is
/// the GUEST's**, because every host flag that can appear there is double-dash. No heuristic goes with
/// it — a `-dir` typo of `--dir` reaches the guest silently, which the owner chose over a "looks like"
/// warning that would guess.
///
/// Measured before this existed: an unknown flag was SILENTLY IGNORED after the path on summarize, in
/// `wast` and in `wat`, and misreported as "cannot read `--x`" in front of a path. Both runtimes.
fn unknown_flag(arg: &str, guest_hint: bool) -> ExitCode {
    if guest_hint {
        eprintln!("wasmrt: unknown flag `{arg}` (use `--` to pass it to the guest)");
    } else {
        eprintln!("wasmrt: unknown flag `{arg}`");
    }
    ExitCode::FAILURE
}

/// Bytes that entered this process **once**, with the digest of exactly those bytes.
///
/// 🔒 **Load-once is a TYPE here, not a discipline** (`cmem/security-model.md` §4). Every path that
/// EXECUTES demands a `Loaded`, and the only way to make one is [`load_module`] — so re-reading a
/// path cannot produce the value the execute path requires, and the digest the gate checks is taken
/// at the single point where the bytes enter. A digest re-derived by reopening the file is a digest
/// of whatever the file says *now*, which is the TOCTOU hole this closes by construction
/// (`interop.md` §3.1).
///
/// ⚠️ For `.wat` the bytes are the **ASSEMBLED** module, not the source text — that is what runs, so
/// that is what is hashed, and `wasmrt pin` must hash the same thing or a pinned `.wat` never matches.
struct Loaded {
    path: String,
    bytes: Vec<u8>,
    digest: pin::Digest,
}

/// Read a module and hash it, once. `.wat` is assembled first (see [`Loaded`]).
fn load_module(path: &str) -> Result<Loaded, String> {
    let bytes = read_module_bytes(path)?;
    Ok(Loaded { digest: pin::hash(&bytes), path: path.to_string(), bytes })
}

/// Read a `.wast` script and hash **the script bytes**.
///
/// ⚠️⚠️ **A `.wast` MUST be gated, and this is the bypass that shipped in the sibling runtime**
/// (`interop.md` §3.1): a script instantiates and invokes the modules it contains, including
/// `(module binary "…")` raw payloads, so an unpinned script ran arbitrary wasm **even under a
/// root-owned `# mode: enforce`**. Any wasm can be wrapped in a `.wast` and the attacker picks the
/// extension, so the bypass needs no privilege. Hashing the script authorizes exactly what it can run.
fn load_script(path: &str) -> Result<Loaded, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    Ok(Loaded { digest: pin::hash(&bytes), path: path.to_string(), bytes })
}

/// Every host flag one run of arguments carried. Host flags are recognised **only** in a host-flag
/// position (`interop.md` §2.4a); everything else belongs to the guest.
#[derive(Debug, Default, Clone)]
struct HostFlags {
    preopens: Vec<Preopen>,
    allow_symlink: bool,
    env: Vec<(String, String)>,
    max_memory: Option<usize>,
    max_table_elems: Option<usize>,
    /// `--max-iterations <count>`; `0` means unlimited at the CLI.
    max_iterations: Option<u64>,
    features: Option<Features>,
    verify: VerifyFlags,
}

impl HostFlags {
    /// Combine the run before the module path with the run immediately after it.
    fn merge(mut self, other: HostFlags) -> HostFlags {
        self.preopens.extend(other.preopens);
        self.env.extend(other.env);
        self.allow_symlink |= other.allow_symlink;
        self.max_memory = self.max_memory.or(other.max_memory);
        self.max_table_elems = self.max_table_elems.or(other.max_table_elems);
        self.max_iterations = self.max_iterations.or(other.max_iterations);
        // `--features` is refused after the path, so at most one run can carry it.
        self.features = self.features.or(other.features);
        self.verify = VerifyFlags {
            pins: self.verify.pins.or(other.verify.pins),
            // Two spellings of a policy RAISE, never lower.
            verify: match (self.verify.verify, other.verify.verify) {
                (Some(x), Some(y)) => Some(pin::stricter(x, y)),
                (x, y) => x.or(y),
            },
            opt_out: self.verify.opt_out || other.verify.opt_out,
        };
        self
    }

    /// The instantiation ceilings this command line asks for.
    fn limits(&self) -> ResourceLimits {
        let mut l = ResourceLimits::defaults();
        if let Some(m) = self.max_memory {
            l.max_memory_bytes = m;
        }
        if let Some(t) = self.max_table_elems {
            l.max_table_elems = t;
        }
        if let Some(n) = self.max_iterations {
            l.max_iterations = n;
        }
        l
    }
}

/// The verification flags, recognised **only in a leading run of host flags** — never in guest argv.
#[derive(Debug, Default, Clone)]
struct VerifyFlags {
    /// `--pins <path>`: a different pin set. Ignored under a root `enforce`.
    pins: Option<String>,
    /// `--verify off|warn|enforce`: may only RAISE the DB's policy.
    verify: Option<Mode>,
    /// `--no-verify` / `--yes`.
    opt_out: bool,
}

/// A byte/count size: a plain number, or one with a `K`/`M`/`G` suffix (`512M`, `2G`).
fn parse_size(s: &str) -> Option<usize> {
    let t = s.trim();
    let (digits, mult) = match t.as_bytes().last()? {
        b'k' | b'K' => (&t[..t.len() - 1], 1024usize),
        b'm' | b'M' => (&t[..t.len() - 1], 1024 * 1024),
        b'g' | b'G' => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        _ => (t, 1),
    };
    digits.trim().parse::<usize>().ok()?.checked_mul(mult)
}

/// Split a `--dir` spec into (host, guest).
///
/// 🔒 **Both separators, because this is a live swappability break** (`interop.md` §2.2): wazmrt
/// spells it `<host>:<guest>`, wasmrt `<host>::<guest>`. `::` is preferred and tried first; a single
/// `:` is honoured when it is not a Windows drive letter, which is why wasmrt chose the doubled form
/// in the first place (`--dir C:\data:/data`). Converging on one spelling would break working
/// command lines of the other runtime, so both accept both.
fn split_preopen(spec: &str) -> (String, String) {
    if let Some((h, g)) = spec.split_once("::") {
        return (h.to_string(), g.to_string());
    }
    if let Some(idx) = spec.rfind(':') {
        let (host, rest) = spec.split_at(idx);
        let guest = &rest[1..];
        let is_drive_letter =
            host.len() == 1 && host.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
        if !host.is_empty() && !guest.is_empty() && !is_drive_letter {
            return (host.to_string(), guest.to_string());
        }
    }
    (spec.to_string(), spec.to_string())
}

/// Parse a `--features` list: items separated by commas, `name` to enable and `-name` to disable.
///
/// The seed follows the sibling runtime so one list means one thing under both: a list containing
/// any **signed** item starts from `all`, a list of bare names starts from `mvp`, and an explicit
/// `all` / `mvp` item sets it outright. Layering is then checked once
/// (`Features::check_coherent`) — the proposals' own dependencies, not a house rule.
fn parse_features(list: &str) -> Result<Features, String> {
    let items: Vec<&str> = list.split(',').map(str::trim).collect();
    if items.iter().any(|i| i.is_empty()) {
        return Err(String::from("--features: empty item"));
    }
    let mut f = if items.iter().any(|i| i.starts_with('-')) {
        Features::all()
    } else {
        Features::mvp()
    };
    for item in items {
        let (on, name) = match item.strip_prefix('-') {
            Some(rest) => (false, rest),
            None => (true, item),
        };
        match name.to_ascii_lowercase().replace('-', "_").as_str() {
            "all" => f = Features::all(),
            "mvp" => f = Features::mvp(),
            // ⚠️ The sibling gates multiple TABLES separately; wasmrt does not model it as its own
            // proposal (reference-types brings it). Recognised so a wazmrt command line still runs,
            // and said out loud rather than silently ignored — `interop.md` records the divergence.
            "multi_table" => eprintln!(
                "wasmrt: warning: `multi_table` is not a separate proposal here (reference-types \
                 carries it); the item had no effect"
            ),
            other => match Feature::from_name(other) {
                Some(feat) => f.set(feat, on),
                None => {
                    let known: Vec<&str> = Feature::ALL.iter().map(|x| x.name()).collect();
                    return Err(format!(
                        "--features: unknown proposal '{name}'\n  known: all, mvp, {}",
                        known.join(", ")
                    ));
                }
            },
        }
    }
    // The layering is the proposals' own: enabling `gc` without `function-references` is refused
    // here rather than producing a set the validator would have to reason about.
    f.check_coherent()
        .map_err(|e| format!("--features: {e} — it is layered on it and cannot be enabled alone"))?;
    Ok(f)
}

/// Where a pin DB lives. 🔒 **One SHARED path, named for the deployment both runtimes ship inside**
/// (owner, 2026-09-19; `interop.md` §3.3): if each runtime read its own path, swapping the binary
/// would find no DB, compute `armed = false` and **silently run everything** — a security downgrade
/// with no error, the worst class this project tracks. The per-runtime paths stay readable as a
/// fallback, and the SIBLING's path is checked purely so a swap cannot disarm in silence.
fn db_paths() -> (Vec<String>, String) {
    #[cfg(windows)]
    {
        let base = std::env::var("ProgramData").unwrap_or_else(|_| String::from("C:\\ProgramData"));
        (
            std::vec![format!("{base}\\wasmtk\\pins"), format!("{base}\\wasmrt\\pins")],
            format!("{base}\\wazmrt\\pins"),
        )
    }
    #[cfg(not(windows))]
    {
        (
            std::vec![String::from("/etc/wasmtk/pins"), String::from("/etc/wasmrt/pins")],
            String::from("/etc/wazmrt/pins"),
        )
    }
}

/// The DB this run is governed by: its path and its text, or `None` when nothing is installed.
///
/// ⚠️ **A swap must never disarm SILENTLY.** When none of our paths has a DB but the sibling's does,
/// that is said out loud — the operator pinned modules for a runtime they have just swapped out, and
/// treating that as "unarmed" is exactly the failure §3.3 calls the most dangerous row in the file.
fn resolve_default_db() -> Option<(String, String)> {
    let (ours, sibling) = db_paths();
    match choose_db(&ours, &sibling, |p| std::path::Path::new(p).exists()) {
        DbChoice::Use(path) => std::fs::read_to_string(&path).ok().map(|t| (path, t)),
        DbChoice::NoneButSiblingHasOne => {
            eprintln!(
                "wasmrt: warning: a pin DB exists at {sibling} but not at {} — \
                 verification is NOT armed (move or copy it to the shared path)",
                ours[0]
            );
            None
        }
        DbChoice::None => None,
    }
}

/// Which DB a run is governed by — the decision alone, with no I/O, so the one case that matters
/// can be tested.
#[derive(Debug, PartialEq, Eq)]
enum DbChoice {
    /// Read this one.
    Use(String),
    /// Nothing of ours, but the SIBLING runtime has one. ⚠️ **This is the case the owner's decision
    /// exists for**: an operator pinned modules for the runtime they just swapped out, and treating
    /// that as "unarmed" would disable verification with no message at all.
    NoneButSiblingHasOne,
    /// Nothing anywhere: an unarmed build, which runs everything by design.
    None,
}

fn choose_db(ours: &[String], sibling: &str, exists: impl Fn(&str) -> bool) -> DbChoice {
    if let Some(p) = ours.iter().find(|p| exists(p)) {
        return DbChoice::Use(p.clone());
    }
    if exists(sibling) {
        return DbChoice::NoneButSiblingHasOne;
    }
    DbChoice::None
}

/// Authorize the bytes that are about to run. `true` to proceed.
///
/// 🔒 **Runs BEFORE validation** (`interop.md` §3.2): authorization first, so an unauthorized module
/// is refused as *unauthorized* rather than parsed, type-checked and reported on.
/// 🔒 **Never gates a pure inspect path** — `wasmrt <file>` summarizes and is not gated.
fn verify_gate(loaded: &Loaded, flags: &VerifyFlags) -> bool {
    // The root-owned DB is read FIRST, because it declares the policy that decides whether the
    // user's own flags are allowed to matter at all.
    let default_db = resolve_default_db();
    let root_mode = default_db.as_ref().and_then(|(_, t)| pin::mode_from_db(t));
    let root_enforces = root_mode == Some(Mode::Enforce);

    // Under a root `enforce`, BOTH the pin set and the policy come from root: redirecting with
    // `--pins` or lowering with `--verify` is ignored (§3.4).
    let (db_path, db_text) = match (&flags.pins, root_enforces, &default_db) {
        (Some(p), false, _) => match std::fs::read_to_string(p) {
            Ok(t) => (p.clone(), Some(t)),
            Err(e) => {
                eprintln!("wasmrt: cannot read pin DB {p}: {e}");
                return false;
            }
        },
        (_, _, Some((p, t))) => (p.clone(), Some(t.clone())),
        _ => (db_paths().0[0].clone(), None),
    };

    let mut explicit = root_mode;
    if !root_enforces {
        // A user-supplied source may only RAISE: an `off` from a redirected DB or from `--verify`
        // must not turn an armed default-deny into a run.
        for m in [db_text.as_deref().and_then(pin::mode_from_db), flags.verify]
            .into_iter()
            .flatten()
            .filter(|m| *m != Mode::Off)
        {
            explicit = Some(explicit.map_or(m, |e| pin::stricter(e, m)));
        }
    }

    // Armed = a pin DB is present. (A root signing key would also arm it; signatures stay
    // design-only, so presence of a DB is the whole test today — and an unarmed build runs
    // everything, which makes "costs nothing when unarmed" structural.)
    let armed = db_text.is_some();
    let pinned = match &db_text {
        Some(t) => match pin::Db::parse(t) {
            Ok(db) => db.contains(&loaded.digest),
            // ⚠️ FAIL LOUD: a mangled DB is never treated as an empty allow-list, because that
            // makes a pinned module look "not in the list" — which reads as an attack.
            Err(e) => {
                eprintln!("wasmrt: pin DB {db_path} is malformed: {e}");
                return false;
            }
        },
        None => false,
    };

    let tty = {
        use std::io::IsTerminal;
        std::io::stdin().is_terminal()
    };
    match pin::decide(explicit, pinned, flags.opt_out, tty, armed) {
        Action::Run => {
            // An override that WOULD have blocked is never silent.
            if armed && !pinned && flags.opt_out && explicit != Some(Mode::Off) {
                eprintln!(
                    "wasmrt: warning: {} is NOT pinned and is running UNVERIFIED (sha256 {})",
                    loaded.path,
                    pin::to_hex(&loaded.digest)
                );
            }
            true
        }
        Action::Prompt => {
            eprint!(
                "wasmrt: {} is not pinned (sha256 {}, db {db_path}). Run it anyway? [y/N] ",
                loaded.path,
                pin::to_hex(&loaded.digest)
            );
            use std::io::Write;
            let _ = std::io::stderr().flush();
            let mut answer = String::new();
            if std::io::stdin().read_line(&mut answer).is_err() {
                return false;
            }
            matches!(answer.trim(), "y" | "Y" | "yes" | "YES")
        }
        Action::Deny => {
            eprintln!(
                "wasmrt: refusing to run {}: not in the pin DB\n  sha256 {}\n  db     {db_path}\n  policy {}",
                loaded.path,
                pin::to_hex(&loaded.digest),
                match explicit {
                    Some(Mode::Enforce) => "enforce (root-owned; --no-verify cannot override it)",
                    Some(Mode::Warn) => "warn, and no terminal to ask at",
                    Some(Mode::Off) => "off",
                    None => "armed by the presence of a pin DB",
                }
            );
            false
        }
    }
}

/// `wasmrt pin <file|dir> [--db <path>]` — approve modules by appending their digests.
///
/// Hashes the **assembled** bytes for `.wat`, exactly as the gate does, or a pinned `.wat` would
/// never match. One unreadable file warns and is skipped rather than aborting a whole bundle.
fn pin_modules(rest: &[String]) -> ExitCode {
    let mut targets: Vec<String> = Vec::new();
    let mut db: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--db" => {
                let Some(v) = rest.get(i + 1) else {
                    eprintln!("wasmrt: --db needs a path");
                    return ExitCode::FAILURE;
                };
                db = Some(v.clone());
                i += 2;
            }
            other if is_flag(other) => return unknown_flag(other, false),
            other => {
                targets.push(other.to_string());
                i += 1;
            }
        }
    }
    if targets.is_empty() {
        eprintln!("wasmrt: usage: wasmrt pin <file|dir>... [--db <path>]");
        return ExitCode::FAILURE;
    }


    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for t in &targets {
        let p = std::path::Path::new(t);
        if p.is_dir() {
            collect_modules(p, &mut files);
        } else {
            files.push(p.to_path_buf());
        }
    }
    files.sort();

    let mut lines = String::new();
    let mut n = 0usize;
    for f in &files {
        let name = f.to_string_lossy().to_string();
        match load_module(&name) {
            Ok(l) => {
                lines.push_str(&format!("{}  {name}\n", pin::to_hex(&l.digest)));
                n += 1;
            }
            // One bad file must not abort a bundle.
            Err(e) => eprintln!("wasmrt: warning: skipping {name}: {e}"),
        }
    }
    if n == 0 {
        eprintln!("wasmrt: nothing to pin");
        return ExitCode::FAILURE;
    }
    // Always PRINT the lines; append only when a DB is named. Matches wazmrt, so an installer
    // script written for either runtime does the same thing under both (`interop.md` §2.1).
    print!("{lines}");
    let Some(db_path) = db else {
        return ExitCode::SUCCESS;
    };
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let existing = std::fs::read_to_string(&db_path).unwrap_or_default();
    // Parse what is there before appending: writing into a DB we cannot read would corrupt it
    // further, and the gate fails closed on a malformed DB.
    if !existing.is_empty() {
        if let Err(e) = pin::Db::parse(&existing) {
            eprintln!("wasmrt: pin DB {db_path} is malformed: {e}");
            return ExitCode::FAILURE;
        }
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&lines);
    if let Err(e) = std::fs::write(&db_path, out) {
        eprintln!("wasmrt: cannot write {db_path}: {e}");
        return ExitCode::FAILURE;
    }
    println!("pinned {n} module(s) into {db_path}");
    ExitCode::SUCCESS
}

/// Every `.wasm` / `.wat` under `dir`, for `pin`'s directory form.
fn collect_modules(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("wasmrt: warning: cannot read directory {}", dir.display());
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_modules(&p, out);
        } else if p
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("wasm") || x.eq_ignore_ascii_case("wat"))
        {
            out.push(p);
        }
    }
}

/// One `--dir` / `--ro-dir` grant: the host directory, the name the guest sees, and
/// whether it is read-only.
#[derive(Debug, Clone)]
struct Preopen {
    host: String,
    guest: String,
    read_only: bool,
}

/// Every flag this CLI recognises that takes a following value. Used to tell a **misplaced**
/// host flag from a guest argument that merely looks like one.
const HOST_FLAGS_WITH_VALUE: &[&str] = &["--dir", "--ro-dir"];
/// Every valueless flag this CLI recognises.
const HOST_FLAGS_BARE: &[&str] = &["--allow-symlink"];

/// Warn when a host flag appears where only the **guest** will ever see it.
///
/// ⚠️⚠️ **This is the fail-open half, and it is why the function exists.** Host flags are
/// recognised positionally; anything outside those positions is the guest's argv. For a
/// *preopen* a misplacement fails **closed** — the grant is simply never made, so the guest
/// gets less access than intended. **For a restriction flag it fails OPEN**: `--verify`,
/// `--pins` (T9e) and `--max-iterations` / `--max-*` (T9i) would be silently dropped, and the
/// user who asked for a bound would run without one and see no error.
///
/// **Warn, never refuse:** a guest may legitimately take `--dir` as its own argument, so
/// rejecting would break valid command lines. And **nothing after an explicit `--` is
/// examined** — that marker is the user saying "the rest is the guest's", and second-guessing
/// it would make `--` useless.
///
/// Matches wazmrt's behaviour (their H7); see `cmem/interop.md` §2.2.
fn warn_misplaced_host_flags(guest_argv: &[String]) {
    for a in guest_argv {
        if a == "--" {
            return; // everything past here is deliberately the guest's
        }
        if HOST_FLAGS_WITH_VALUE.contains(&a.as_str()) || HOST_FLAGS_BARE.contains(&a.as_str()) {
            eprintln!(
                "wasmrt: warning: `{a}` here is passed to the GUEST, not applied by wasmrt — \
                 host flags go before the module path or immediately after it \
                 (use `--` to pass it to the guest deliberately)"
            );
        }
    }
}

/// Pull the `--dir` / `--ro-dir` / `--allow-symlink` flags off one run of arguments,
/// returning the preopens plus whatever is left.
///
/// `--dir <host>` maps the directory under its own name; `--dir <host>::<guest>` renames it
/// for the guest. `::` rather than `:` because a Windows host path starts `C:`.
///
/// Parsing stops at the first non-flag. ⚠️ An **unrecognised** `--flag` in a leading position
/// is an **error**, not a module path: treating it as the path is how a typo becomes
/// "cannot read '--dir'" instead of a usable message, and how a misplaced restriction flag
/// disappears silently.
///
/// `guest_follows` says whether the GUEST's argv begins where this run ends — true only for the run
/// immediately after the module path. It decides what a **single-dash** token means there
/// (`interop.md` §2.4a, owner 2026-09-19): every host flag in this position is double-dash, so a
/// single-dash token cannot be one and is simply the guest's (`prog.wasm -la`). Before the path, or
/// in a command with no guest argv, there is no guest to own it, so it is an unknown flag.
fn take_dir_flags(
    args: &[String],
    guest_follows: bool,
) -> Result<(HostFlags, &[String]), String> {
    let mut hf = HostFlags::default();
    let mut vf = VerifyFlags::default();
    let mut i = 0;
    while i < args.len() {
        // A valueless flag, so it is handled before the ones that consume an argument.
        if args[i] == "--allow-symlink" {
            hf.allow_symlink = true;
            i += 1;
            continue;
        }
        // 🔒 **`--features` goes BEFORE the module, and anywhere else is an ERROR** (owner,
        // 2026-09-19; the sibling runtime already refuses it the same way). It selects the language
        // the module is DECODED and VALIDATED against, so by the time the path has been read the
        // decision has been made — accepting it afterwards would silently ignore it, which is the
        // failure mode this rule exists to prevent. Stated in the usage line and in `--help`.
        if args[i] == "--features" {
            if guest_follows {
                return Err(String::from(
                    "`--features` must come BEFORE the module path",
                ));
            }
            let Some(v) = args.get(i + 1) else {
                return Err(String::from("--features needs a list"));
            };
            hf.features = Some(parse_features(v)?);
            i += 2;
            continue;
        }
        if args[i] == "--env" {
            let Some(v) = args.get(i + 1) else {
                return Err(String::from("--env needs KEY=VALUE"));
            };
            let Some((k, val)) = v.split_once('=') else {
                return Err(format!("--env {v}: expected KEY=VALUE"));
            };
            hf.env.push((k.to_string(), val.to_string()));
            i += 2;
            continue;
        }
        if args[i] == "--max-iterations" {
            let Some(v) = args.get(i + 1) else {
                return Err(String::from("--max-iterations needs a count"));
            };
            // `0` is UNLIMITED at the CLI. (The C ABI keeps the default for 0 instead — a library
            // embedder does not get to remove the bound by passing zero.)
            let Some(n) = parse_size(v).map(|n| n as u64) else {
                return Err(format!("--max-iterations {v}: expected a count like 1G or 0"));
            };
            hf.max_iterations = Some(n);
            i += 2;
            continue;
        }
        if args[i] == "--max-memory" || args[i] == "--max-table-elems" {
            let Some(v) = args.get(i + 1) else {
                return Err(format!("{} needs a size", args[i]));
            };
            let Some(n) = parse_size(v) else {
                return Err(format!("{} {v}: expected a size like 512M or 2G", args[i]));
            };
            if args[i] == "--max-memory" {
                hf.max_memory = Some(n);
            } else {
                hf.max_table_elems = Some(n);
            }
            i += 2;
            continue;
        }
        // 🔒 The VERIFICATION flags, and the region rule is the point (`interop.md` §2.4): they are
        // recognised only here, in a run of host flags — never in the guest's argv. Scanning
        // "everything before `--`" is not enough, because the common WASI form has no `--` at all
        // (`prog.wasm install --yes`), so the guest's own arguments would be searched and a `--yes`
        // meant for the guest would **silently disable verification**. wazmrt paid for that one.
        if args[i] == "--no-verify" || args[i] == "--yes" {
            vf.opt_out = true;
            i += 1;
            continue;
        }
        if args[i] == "--verify" {
            let Some(v) = args.get(i + 1) else {
                return Err(String::from("--verify needs off|warn|enforce"));
            };
            // ⚠️ A typo is an ERROR, never a default: `--verify enfroce` must fail rather than
            // silently mean `off` (§3.5, the same reasoning as the `# mode:` rule).
            let Some(m) = pin::parse_mode(&v.to_ascii_lowercase()) else {
                return Err(format!("--verify {v}: expected off, warn or enforce"));
            };
            vf.verify = Some(vf.verify.map_or(m, |old| pin::stricter(old, m)));
            i += 2;
            continue;
        }
        if args[i] == "--pins" {
            let Some(v) = args.get(i + 1) else {
                return Err(String::from("--pins needs a path"));
            };
            vf.pins = Some(v.clone());
            i += 2;
            continue;
        }
        // `--` ends host flags: everything after it belongs to the guest, verbatim.
        if args[i] == "--" {
            hf.verify = vf;
            return Ok((hf, &args[i..]));
        }
        let ro = match args[i].as_str() {
            "--dir" => false,
            "--ro-dir" => true,
            // ⚠️ An unrecognised `--flag` is an ERROR, not the module path. Falling through
            // made `wasmrt wasi --typo x.wasm` report "cannot read '--typo'", and — the half
            // that matters — let a misplaced restriction flag vanish without a word.
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}` (use `--` to pass it to the guest)"));
            }
            // A single-dash token: the guest's where a guest follows, an unknown flag otherwise.
            other if is_flag(other) && !guest_follows => {
                return Err(format!("unknown flag `{other}`"));
            }
            _ => break,
        };
        let Some(spec) = args.get(i + 1) else {
            return Err(format!("{} needs a directory", args[i]));
        };
        let (host, guest) = split_preopen(spec);
        hf.preopens.push(Preopen { host, guest, read_only: ro });
        i += 2;
    }
    hf.verify = vf;
    Ok((hf, &args[i..]))
}

/// `wasmrt wasi <file.wasm> [args...]` — run a WASI preview-1 program's `_start`.
///
/// Exits with the guest's `proc_exit` code when it calls one, so shell pipelines see the
/// status the program intended.
fn run_wasi_module(rest: &[String]) -> ExitCode {
    run_wasi_argv(rest)
}

/// `wasmrt wasi …` and the bare-path WASI form share this.
fn run_wasi_argv(rest: &[String]) -> ExitCode {
    // Host flags are accepted in BOTH positions — before the module path (wasmrt's spelling)
    // and immediately after it (wazmrt's). 🔒 `cmem/interop.md` §2.2: a command line must do
    // the same thing under either runtime with only the program name changed, and flag
    // POSITION is part of an argument shape. Everything after the trailing run — or after an
    // explicit `--` — is the guest's argv.
    let (mut flags, rest) = match take_dir_flags(rest, false) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(path) = rest.first() else {
        eprintln!(
            "wasmrt: usage: wasmrt wasi [--dir <host>[::<guest>]] [--ro-dir …] [--allow-symlink] <file> [flags] [--] [args...]"
        );
        return ExitCode::FAILURE;
    };
    let path = path.clone();
    // The TRAILING run of host flags, immediately after the module path (wazmrt's spelling).
    let guest_argv: Vec<String> = match take_dir_flags(&rest[1..], true) {
        Ok((more, tail)) => {
            flags = flags.merge(more);
            // ⚠️ Warn on the tail *including* any `--`, so the marker can stop the scan —
            // stripping it first made `wasmrt wasi m.wasm -- --dir X` warn about a flag the
            // user had explicitly handed to the guest, which is the one case that must stay
            // quiet. Caught by the case-5 probe, not by reading this.
            warn_misplaced_host_flags(tail);
            // Then drop the marker itself: the guest sees its arguments, not our separator.
            let tail = if tail.first().is_some_and(|a| a == "--") { &tail[1..] } else { tail };
            tail.to_vec()
        }
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    let path = path.as_str();
    let loaded = match load_module(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    run_wasi_loaded(&loaded, &flags, &guest_argv)
}

/// Run a loaded WASI command module under `flags`.
fn run_wasi_loaded(loaded: &Loaded, flags: &HostFlags, guest_argv: &[String]) -> ExitCode {
    let path = loaded.path.as_str();
    // 🔒 Authorization BEFORE validation, and before anything is decoded (`interop.md` §3.2).
    if !verify_gate(loaded, &flags.verify) {
        return ExitCode::FAILURE;
    }
    let md = match module::decode(&loaded.bytes) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("wasmrt: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // `--features` narrows the language this module is judged against.
    let features = flags.features.unwrap_or_else(Features::all);
    if let Err(e) = wasmrt_core::validate::validate_with_features(&md, &features) {
        eprintln!("wasmrt: {path}: {}", invalidity_report(&e));
        return ExitCode::FAILURE;
    }

    // A CSPRNG that cannot be seeded must fail loudly rather than run predictably.
    let Some(ctx) = wasmrt_core::wasi::WasiCtx::new() else {
        eprintln!("wasmrt: no OS entropy available; refusing to run with a predictable RNG");
        return ExitCode::FAILURE;
    };
    // argv[0] is the module path, then whatever follows on our command line.
    let mut ctx = ctx
        .with_args(std::iter::once(path.to_string()).chain(guest_argv.iter().cloned()))
        .with_env(std::env::vars())
        // `--env KEY=VALUE` is applied after the inherited environment, so it wins.
        .with_env(flags.env.iter().cloned());
    // **The guest reaches nothing it was not explicitly granted.** With no `--dir`, every
    // path call returns BADF; there is no implicit cwd preopen.
    for p in &flags.preopens {
        // 🔒 Read-write does NOT include planting symlinks unless `--allow-symlink` asked for it:
        // a workload run has no need to create links, and denying it removes a guest-controlled
        // primitive a second process could later repoint. Following an EXISTING link is unaffected.
        let rights = if p.read_only {
            wasmrt_core::wasi::fs::rights::READ_ONLY
        } else if flags.allow_symlink {
            wasmrt_core::wasi::fs::rights::ALL
        } else {
            wasmrt_core::wasi::fs::rights::READ_WRITE
        };
        if let Err(e) = ctx.preopen_dir(std::path::Path::new(&p.host), &p.guest, rights) {
            eprintln!("wasmrt: cannot preopen {}: errno {e}", p.host);
            return ExitCode::FAILURE;
        }
    }
    let shared = wasmrt_core::wasi::shared(ctx);

    let imports = match wasmrt_core::wasi::link(&md, &shared) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("wasmrt: {path}: cannot link WASI: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut inst = match Instance::new_with(md, imports, flags.limits()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("wasmrt: {path}: instantiation failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let r = inst.invoke("_start", &[]);
    // `proc_exit` unwinds as a host trap, so consult the recorded code before treating the
    // error as a failure — an exit is a normal way for a WASI program to finish.
    if let Some(code) = shared.borrow().exit_code() {
        return ExitCode::from(u8::try_from(code & 0xff).unwrap_or(1));
    }
    match r {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wasmrt: {path}: trap: {e}");
            print_backtrace(&inst);
            ExitCode::FAILURE
        }
    }
}

/// The **bare-path run modes** — `wasmrt <module> …`, the sibling runtime's shape
/// (`interop.md` §2.1, the ADDITIVE target: each runtime accepts the other's spelling and neither
/// loses its own, so a command line written for either drives both).
///
/// | form | what happens |
/// | --- | --- |
/// | `wasmrt m.wasm f 2 3` | calls the export `f` |
/// | `wasmrt prog.wasm [flags] [-- argv]` | runs `_start` as a WASI command, when one is exported |
/// | `wasmrt script.wast` | runs the spec script |
/// | `wasmrt m.wasm` | summarizes + validates, as before |
///
/// ⚠️⚠️ **This changes what the most casual invocation does: a bare path now EXECUTES** a WASI
/// command that previously would only have been summarized. That is a security-posture change, and
/// the rule it was gated on is `cmem/security-model.md` §3b: **the verification gate lands before,
/// or with, this — never after.** It ships in the same commit as `verify_gate` for that reason.
///
/// 🔒 **Z1: a named export that does not exist is a FAILURE**, never a silent fall-back to
/// summarizing (`interop.md` §2.3). Falling back is how `wasmrt m.wasm add 2 3` used to print a
/// summary and exit 0 while ignoring both the export and its arguments — the silent-wrong class.
fn run_bare(args: &[String]) -> ExitCode {
    // Host flags may PRECEDE the path (wasmrt's own position) as well as follow it (wazmrt's).
    let (lead, rest) = match take_dir_flags(args, false) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(path) = rest.first().cloned() else {
        eprintln!("wasmrt: usage: wasmrt [--features <list>] <file> [<export> args... | wasi-flags [-- argv]]");
        return ExitCode::FAILURE;
    };
    run_bare_path(&path, &rest[1..], lead)
}

fn run_bare_path(path: &str, rest: &[String], lead: HostFlags) -> ExitCode {
    // A `.wast` script, by extension — the sibling dispatches on it and so do we now.
    if std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wast"))
    {
        let mut argv = std::vec![String::from(path)];
        argv.extend(rest.iter().cloned());
        return run_wast(&argv);
    }
    // The trailing run of host flags — wazmrt's position. A single-dash token here is the guest's
    // (§2.4a), and everything from the first non-flag on is the export name or the guest's argv.
    let (trailing, tail) = match take_dir_flags(rest, true) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    let flags = lead.merge(trailing);
    warn_misplaced_host_flags(tail);
    let tail: Vec<String> = if tail.first().is_some_and(|a| a == "--") {
        tail[1..].to_vec()
    } else {
        tail.to_vec()
    };

    let loaded = match load_module(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Decoded here only to ask WHICH MODE this is — the module's exports decide. The gate still
    // runs before validation, and before anything executes; a summarize that never executes is
    // never gated at all (§3.2).
    let md = match module::decode(&loaded.bytes) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("wasmrt: {path}: decode failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let exports_func = |n: &str| {
        md.exports
            .iter()
            .any(|e| e.name == n && matches!(e.ty, Extern::Func(_)))
    };
    let has_start = exports_func("_start");

    match tail.first() {
        // A word follows the path: an export name, or — when the module is a WASI command — the
        // guest's own argv (`prog.wasm install --yes`).
        Some(first) if exports_func(first) => call_export(&loaded, &flags, first, &tail[1..]),
        Some(_) if has_start => run_wasi_loaded(&loaded, &flags, &tail),
        Some(first) => {
            eprintln!(
                "wasmrt: no exported function `{first}` in {path} (and it exports no `_start`)"
            );
            ExitCode::FAILURE
        }
        None if has_start => run_wasi_loaded(&loaded, &flags, &[]),
        // Nothing to execute: inspect it, exactly as before, and do not gate.
        None => summarize_loaded(&loaded, &md),
    }
}

/// Read a module file, **assembling it first when it is `.wat` text**.
///
/// `wasmrt run prog.wat` used to fail with *"not a WebAssembly binary (bad magic)"* — the assembler
/// was right there in the same binary, but only reachable as a separate `wasmrt wat` step. The oracle
/// accepted `.wat` on its run path all along, so this was a port/oracle divergence of exactly the
/// shape today keeps producing: a capability present in one and absent in the other, with nothing
/// comparing them.
///
/// **One helper, used by every path that loads a module** (`run`, `wasi`, summarize) rather than three
/// copies of the sniff — the same reasoning as wazmrt hanging its validation guard off the existing
/// `will_execute` predicate. A fourth loader added later inherits this instead of having to remember.
///
/// Dispatch is on the **extension**, matching the oracle: predictable, and it keeps a malformed
/// *binary* reporting a decode error rather than being fed to the assembler and blamed for bad syntax.
///
/// ⚠️ What executes is the **assembled bytes**, not the file on disk. That matters for anything that
/// hashes what it runs — `pin` is still a stub (T9e), but when it lands it must hash the assembled
/// module, exactly as wazmrt's `verifyGate` hashes the in-memory bytes rather than re-reading the path.
fn read_module_bytes(path: &str) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    if std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wat"))
    {
        return wasmrt_core::wat::assemble(&bytes)
            .map_err(|e| format!("{path}: cannot assemble: {e}"));
    }
    Ok(bytes)
}

/// Format an invalid-module report, **shaped to match wasmtime**.
///
/// wasmtime 47 on `(func (result i32) i64.const 1)`:
///
/// ```text
/// Invalid input WebAssembly code at offset 33: type mismatch: expected i32, found i64
/// ```
///
/// So: the byte offset **in decimal**, from the start of the module — the same origin wasmtime uses,
/// so the two tools' numbers are directly comparable on the same file — then the two types. The
/// function index is ours to add: wasmtime does not print it, and it is what makes a twenty-body
/// module tractable. Anything the validator did not record is simply omitted rather than guessed.
fn invalidity_report(e: &ValidateError) -> String {
    let site = wasmrt_core::validate::last_failure_site();
    let mut s = String::from("invalid module");
    if let Some(off) = site.offset {
        s.push_str(&format!(" at offset {off}"));
    }
    if let Some(i) = site.func_index {
        s.push_str(&format!(" (function {i})"));
    }
    match (site.expected, site.found) {
        // Match wasmtime's wording exactly where we have the same facts.
        (Some(exp), Some(found)) => s.push_str(&format!(": type mismatch: expected {exp:?}, found {found:?}")),
        _ => s.push_str(&format!(": {e}")),
    }
    s
}

/// Print the trap's call stack, innermost first, to **stderr**.
///
/// Stderr deliberately, like the trap line itself: `wasmrt wasi prog.wasm > out.txt` must capture
/// only the guest's output, not our diagnostics — the oracle mixes them into stdout and that is a
/// divergence we keep.
///
/// `+N` is the byte offset from the start of the module, which is what `wasm-objdump` prints.
fn print_backtrace(inst: &interp::Instance) {
    let frames = inst.backtrace();
    if frames.is_empty() {
        return;
    }
    let mut unnamed = false;
    for (i, f) in frames.iter().enumerate() {
        let lead = if i == 0 { "at" } else { "by" };
        match inst.frame_name(f).and_then(|n| core::str::from_utf8(n).ok()) {
            Some(name) => eprintln!("  {lead} {name} (fn[{}]) +{:#x}", f.func_index, f.offset),
            None => {
                unnamed = true;
                eprintln!("  {lead} fn[{}] +{:#x}", f.func_index, f.offset);
            }
        }
    }
    if unnamed {
        eprintln!("  (no name section: rebuild the guest unstripped for symbols)");
    }
}

/// `wasmrt wat <file.wat> [-o out.wasm]` — assemble text to a binary.
fn assemble_wat(rest: &[String]) -> ExitCode {
    // §2.4a: `wat` has no guest argv, so EVERY argument is in a host-flag position.
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "-o" {
            i += 2;
            continue;
        }
        if is_flag(&rest[i]) {
            return unknown_flag(&rest[i], false);
        }
        i += 1;
    }
    let Some(path) = rest.first() else {
        eprintln!("wasmrt: usage: wasmrt wat <file.wat> [-o <out.wasm>]");
        return ExitCode::FAILURE;
    };
    let src = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("wasmrt: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let bytes = match wasmrt_core::wat::assemble(&src) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("wasmrt: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let out = rest
        .iter()
        .position(|a| a == "-o")
        .and_then(|i| rest.get(i + 1));
    match out {
        Some(o) => {
            if let Err(e) = std::fs::write(o, &bytes) {
                eprintln!("wasmrt: cannot write {o}: {e}");
                return ExitCode::FAILURE;
            }
            println!("{o}: {} bytes", bytes.len());
        }
        None => println!("{path}: assembled {} bytes", bytes.len()),
    }
    ExitCode::SUCCESS
}

/// `wasmrt wast <file.wast | dir>...` — run spec scripts and report the pass profile.
fn run_wast(rest: &[String]) -> ExitCode {
    if rest.is_empty() {
        eprintln!("wasmrt: usage: wasmrt wast <file.wast | directory>...");
        return ExitCode::FAILURE;
    }
    // The verification flags are host flags here too; `wast` has no guest argv, so they may appear
    // anywhere. ⚠️⚠️ A `.wast` EXECUTES the modules it contains, so it is gated like any other
    // execute path — see `load_script`.
    let (gate_flags, _) = match take_dir_flags(rest, false) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    // ⚠️ EVERY value-taking host flag, or its VALUE is mistaken for a script path — which is how
    // `wast --max-iterations 1000 <dir>` first tried to run a script named "1000". One list,
    // used both to validate the flags and to collect the files.
    let known_value_flag = |a: &String| {
        matches!(
            a.as_str(),
            "--pins" | "--verify" | "--features" | "--env" | "--dir" | "--ro-dir"
                | "--max-memory" | "--max-table-elems" | "--max-iterations"
        )
    };
    let mut skip_next = false;
    for a in rest {
        if skip_next {
            skip_next = false;
            continue;
        }
        if known_value_flag(a) {
            skip_next = true;
            continue;
        }
        // §2.4a: `-v` and the verification flags are the only ones `wast` knows.
        if is_flag(a) && a != "-v" && a != "--no-verify" && a != "--yes" {
            return unknown_flag(a, false);
        }
    }
    let verbose = rest.iter().any(|a| a == "-v");
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut skip_next = false;
    for a in rest.iter().filter(|a| {
        if skip_next {
            skip_next = false;
            return false;
        }
        if known_value_flag(a) {
            skip_next = true;
            return false;
        }
        !a.starts_with('-')
    }) {
        let p = std::path::Path::new(a);
        if p.is_dir() {
            collect_wast(p, &mut files);
        } else {
            files.push(p.to_path_buf());
        }
    }
    files.sort();

    let (mut passed, mut failed, mut skipped, mut errored) = (0usize, 0usize, 0usize, 0usize);
    let mut worst: Vec<(String, usize)> = Vec::new();
    // Corpus-wide skip census, keyed by the reason with its variable tail stripped. A skip
    // total nobody can attribute is a number, not a measurement — this is what makes the
    // remaining skip work scopeable.
    let mut skip_census: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for f in &files {
        let loaded = match load_script(&f.to_string_lossy()) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("wasmrt: {e}");
                errored += 1;
                continue;
            }
        };
        // 🔒 Gated like every other execute path: a script runs the modules it carries.
        if !verify_gate(&loaded, &gate_flags.verify) {
            errored += 1;
            continue;
        }
        let src = loaded.bytes;
        // ⚠️⚠️ The **path**, not the basename. Seven basenames occur twice in the spec corpus
        // — `binary`, `br_on_cast`, `br_on_cast_fail`, `exports`, `imports`, `memory`, `throw`
        // — once at the top level and once under `proposals/`. Printing only the basename made
        // two different files indistinguishable in the report, and any analysis keyed on that
        // name silently merged them: a regression in one could be netted out by a gain in the
        // other and the per-file "no file lost a pass" check would report NONE.
        //
        // 🎓 **A gate whose identifier is not unique is not a gate.** Found 2026-08-19 when the
        // same file read 1 failure standalone and 12 in the corpus walk — which looked like a
        // harness state leak and was two files wearing one name.
        let name = f
            .strip_prefix(std::env::current_dir().unwrap_or_default())
            .unwrap_or(f)
            .to_string_lossy();
        // Each file under the features it is written against (`features_for_script`): a proposal can
        // change what is valid, so a core file must not run with one it does not expect.
        match wasmrt_core::wast::run_script_with(&src, wasmrt_core::wast::features_for_script(&name), gate_flags.limits()) {
            Ok(s) => {
                passed += s.passed;
                failed += s.failed;
                skipped += s.skipped;
                if s.failed > 0 {
                    worst.push((name.to_string(), s.failed));
                }
                // ⚠⚠ Report a file that only SKIPS, too. Listing on `failed > 0` alone made
                // every skips-only file invisible — so the skip total could be read but never
                // ATTRIBUTED, and scoping the remaining skip work from this report was
                // impossible: 234 of 1,024 skips lived in files that printed nothing.
                // A report that cannot account for a number it prints is not a report.
                for r in &s.skips {
                    // Group on the reason's stable head: everything before the parenthesised
                    // detail, which carries file-specific text (a type index, a name) and
                    // would otherwise make every skip its own category.
                    let key = r.split_once(" (").map_or(r.as_str(), |(h, _)| h).to_string();
                    *skip_census.entry(key).or_default() += 1;
                }
                // ⚠️⚠️ **EVERY file prints a row, including a clean one.** The condition here
                // was `verbose || s.failed > 0 || s.skipped > 0`, and that left the per-file gate
                // (`scripts/conformance-diff.sh`) structurally blind in one direction: a clean
                // file is absent from the baseline, so it has no recorded pass count, so passes
                // it later loses to SKIPS cannot be detected. The gate's own header records two
                // earlier holes of this family; this is the third, and X1 landed in it —
                // `custom-page-sizes/memory_max.wast` went 2 passed -> 0 passed / 6 skipped and
                // the gate printed "no file lost a pass".
                //
                // 🎓 The rule the project already had: *a gate that cannot fail is decoration.*
                // Here it could fail, but only for files that were already failing. 288 rows is
                // a cheap price for a join with no missing side.
                {
                    println!("{name}: {s}");
                    if verbose {
                        // All recorded failures, not a sample: triaging a file means seeing
                        // the distinct reasons, and three of forty-six is not a diagnosis.
                        for m in &s.failures {
                            println!("    {m}");
                        }
                        for m in &s.skips {
                            println!("    SKIP {m}");
                        }
                    }
                }
            }
            Err(e) => {
                // The file did not even parse — a runner-level problem, kept separate from
                // assertion failures so it cannot hide in the totals.
                errored += 1;
                println!("{name}: PARSE ERROR: {e}");
            }
        }
    }

    worst.sort_by_key(|(_, c)| core::cmp::Reverse(*c));
    println!("\n=== conformance summary ===");
    println!("files      {} ({errored} unparseable)", files.len());
    println!("passed     {passed}");
    println!("failed     {failed}");
    println!("skipped    {skipped}  (constructs this build cannot put to the test)");
    let adjudicated = passed + failed;
    if adjudicated > 0 {
        let pct = (passed as f64) * 100.0 / (adjudicated as f64);
        println!("pass rate  {pct:.1}% of {adjudicated} adjudicated assertions");
    }
    if !worst.is_empty() {
        println!("\nworst files:");
        for (n, c) in worst.iter().take(15) {
            println!("  {c:>6}  {n}");
        }
    }
    if !skip_census.is_empty() {
        let mut rows: Vec<(&String, &usize)> = skip_census.iter().collect();
        rows.sort_by_key(|(n, c)| (core::cmp::Reverse(**c), (*n).clone()));
        println!("\nskip reasons:");
        for (n, c) in rows.iter().take(20) {
            println!("  {c:>6}  {n}");
        }
    }
    // ⚠️⚠️ A test runner's exit status must say whether the tests passed. This was
    // unconditionally SUCCESS — a failed assertion, an unparseable script and an unreadable file
    // all exited 0 — so no CI job could gate on `wasmrt wast`. wazmrt and `wasmtime wast` both
    // exit non-zero (measured 2026-09-19). SKIPS do not fail the run: they are reported
    // separately, and the per-file gate (`scripts/conformance-diff.sh`) is what judges them.
    if failed > 0 || errored > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn collect_wast(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_wast(&p, out);
        } else if p.extension().is_some_and(|x| x == "wast") {
            out.push(p);
        }
    }
}

fn print_help() {
    println!(
        "wasmrt {} — a fast, small WebAssembly runtime\n\n\
         USAGE — both spellings work, so a command line written for either runtime runs here:\n    \
         wasmrt <module> <export> [args...]     call an exported function\n    \
         wasmrt <module> [wasi-flags] [-- argv] run a WASI `_start` command\n    \
         wasmrt <module>                        summarize + type-check (executes nothing)\n    \
         wasmrt <script.wast>                   run a .wast spec script\n    \
         wasmrt run <file> <fn> [args...]       call an exported function\n    \
         wasmrt wasi [flags] <file> [-- argv]   run a WASI preview-1 program (_start)\n    \
         wasmrt wat <file.wat> [-o out.wasm]    assemble the text format to a binary\n    \
         wasmrt wast <file|dir>... [-v]         run .wast spec scripts\n    \
         wasmrt pin <file|dir> [--db <path>]    print (and record) pin digests\n    \
         wasmrt -h | --help                     show this help\n    \
         wasmrt -v | --version                  show the version\n\n\
         LANGUAGE (must come BEFORE the module path — it selects what is accepted, and\n\
         after the path it would already be too late, so it is an error there):\n    \
           --features <list>           e.g. `mvp`, `all,-simd`, `gc` — bare names imply\n    \
                                       `mvp`, a signed item implies `all`; comma-separated\n\n\
         WASI — a guest reaches ONLY what you preopen:\n    \
           --dir <host>[:<guest>]      grant read-write access to a directory (`::` also\n    \
                                       accepted, and required when <host> is a drive letter)\n    \
           --ro-dir <host>[:<guest>]   grant read-only access (propagates to the subtree)\n    \
           --allow-symlink             let the guest CREATE symlinks (off by default)\n    \
           --env KEY=VALUE             set one variable for the guest (repeatable)\n    \
           --max-memory <size>         linear-memory ceiling (e.g. 512M, 2G)\n    \
           --max-table-elems <count>   table-entry ceiling\n    \
           --max-iterations <count>    stop a guest that never returns (default 1G, 0 = off;\n    \
                                       one iteration = one loop back-edge or one tail call)\n\n\
         With no --dir, every path call returns BADF — there is no implicit cwd.\n\n\
         VERIFICATION — off unless a pin DB is installed:\n    \
           --pins <path>               use this pin DB instead of the default\n    \
           --verify off|warn|enforce   raise strictness (it can never be lowered)\n    \
           --no-verify, --yes          run an unpinned module anyway (refused under enforce)\n\n\
         <file> is a `.wasm` binary or `.wat` text; text is assembled first, then validated\n\
         and run exactly like a binary.\n\n\
         An unrecognised flag is an error (`unknown flag`, exit 1). After the module path a\n\
         single-dash argument is the GUEST's (wasmrt prog.wasm -la); use `--` to hand it\n\
         a `--flag` of its own: wasmrt prog.wasm -- --help",
        wasmrt_core::VERSION
    );
}

fn run_export(rest: &[String]) -> ExitCode {
    // The leading run of host flags — a preopen is meaningless here because a bare export call
    // wires no imports at all, but the verification, ceiling and language flags all apply.
    let (flags, rest) = match take_dir_flags(rest, false) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    // §2.4a: a flag where the path or the function name goes is unknown. The function's ARGUMENTS
    // are not checked — `-1` is a value there.
    if let Some(f) = rest.iter().take(2).find(|a| is_flag(a)) {
        return unknown_flag(f, false);
    }
    let (path, func) = match (rest.first(), rest.get(1)) {
        (Some(p), Some(f)) => (p.as_str(), f.as_str()),
        _ => {
            eprintln!("wasmrt: usage: wasmrt run [--features <list>] <file> <function> [args...]");
            return ExitCode::FAILURE;
        }
    };
    let loaded = match load_module(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    call_export(&loaded, &flags, func, &rest[2..])
}

/// Call `func` on a loaded module. Shared by `wasmrt run` and the bare-path form.
fn call_export(loaded: &Loaded, flags: &HostFlags, func: &str, args: &[String]) -> ExitCode {
    let path = loaded.path.as_str();
    // 🔒 Authorization first (`interop.md` §3.2).
    if !verify_gate(loaded, &flags.verify) {
        return ExitCode::FAILURE;
    }
    let bytes = loaded.bytes.clone();
    let module = match module::decode(&bytes) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("wasmrt: {path}: decode failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    // §4.5.1: instantiation is defined only for a **valid** module, so nothing executes before
    // validation. This path used to skip it — `wasmrt run` would happily execute an ill-typed
    // module and print a plausible answer, while `wasmrt wasi` next door refused the same bytes.
    // An asymmetry between two entry points of one binary is a bug, not a style difference.
    if let Err(e) = validate(&module) {
        eprintln!("wasmrt: {path}: {}", invalidity_report(&e));
        return ExitCode::FAILURE;
    }
    // Resolve the export's signature so args/results can be typed.
    let Some(sig) = module.exports.iter().find_map(|e| match &e.ty {
        Extern::Func(ft) if e.name == func => Some((e.index, ft.clone())),
        _ => None,
    }) else {
        eprintln!("wasmrt: no exported function `{func}` in {path}");
        return ExitCode::FAILURE;
    };
    let (index, ft) = sig;

    let arg_strs = args;
    if arg_strs.len() != ft.params.len() {
        eprintln!(
            "wasmrt: `{func}` takes {} argument(s), got {}",
            ft.params.len(),
            arg_strs.len()
        );
        return ExitCode::FAILURE;
    }
    let mut args: Vec<Value> = Vec::with_capacity(arg_strs.len());
    for (s, &pt) in arg_strs.iter().zip(&ft.params) {
        match parse_arg(s, pt) {
            Ok(v) => args.push(v),
            Err(()) => {
                eprintln!("wasmrt: cannot parse `{s}` as {}", type_name(pt));
                return ExitCode::FAILURE;
            }
        }
    }

    // The ceilings apply to a plain export call too — an unbounded loop is unbounded whichever
    // entry point reached it.
    let mut inst = match Instance::new_with(module, wasmrt_core::interp::Imports::new(), flags.limits()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("wasmrt: cannot instantiate {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match inst.invoke_index(index, &args) {
        Ok(results) => {
            let printed: Vec<String> = results
                .iter()
                .zip(&ft.results)
                .map(|(&v, &rt)| format_result(v, rt))
                .collect();
            println!("{}", printed.join(" "));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("wasmrt: trap: {e}");
            print_backtrace(&inst);
            ExitCode::FAILURE
        }
    }
}

fn parse_arg(s: &str, ty: ValType) -> Result<Value, ()> {
    match ty {
        ValType::I32 => s.parse::<i32>().map(interp::i32_value).map_err(|_| ()),
        ValType::I64 => s.parse::<i64>().map(interp::i64_value).map_err(|_| ()),
        ValType::F32 => s.parse::<f32>().map(interp::f32_value).map_err(|_| ()),
        ValType::F64 => s.parse::<f64>().map(interp::f64_value).map_err(|_| ()),
        _ => Err(()),
    }
}

fn format_result(v: Value, ty: ValType) -> String {
    match ty {
        ValType::I32 => interp::as_i32(v).to_string(),
        ValType::I64 => interp::as_i64(v).to_string(),
        ValType::F32 => interp::as_f32(v).to_string(),
        ValType::F64 => interp::as_f64(v).to_string(),
        _ => format!("0x{v:x}"),
    }
}

fn type_name(ty: ValType) -> &'static str {
    match ty {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F32 => "f32",
        ValType::F64 => "f64",
        _ => "a non-numeric type",
    }
}

/// Summarize an already-loaded, already-decoded module — the bare-path form has both in hand.
fn summarize_loaded(loaded: &Loaded, md: &Module) -> ExitCode {
    if print_summary(&loaded.path, md) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}


/// Print the summary and the validation verdict; `true` iff the module VALIDATED.
fn print_summary(path: &str, m: &Module) -> bool {
    let defined_funcs = m.functions.len();
    let imported_funcs = m.imported_func_count() as usize;
    println!("{path}: WebAssembly module (version {})", m.version);
    println!("  sections   {}", m.sections.len());
    println!("  types      {}", m.comp_types.len());
    println!(
        "  functions  {} ({imported_funcs} imported + {defined_funcs} defined)",
        imported_funcs + defined_funcs
    );
    println!("  memories   {}", m.memories.len());
    println!("  tables     {}", m.tables.len());
    println!("  globals    {}", m.globals.len());
    println!("  imports    {}", m.imports.len());
    println!("  exports    {}", m.exports.len());
    println!("  data segs  {}", m.data.len());
    println!("  elem segs  {}", m.elements.len());
    if let Some(s) = m.start {
        println!("  start      func {s}");
    }
    if !m.exports.is_empty() {
        println!("  exported:");
        for e in &m.exports {
            let kind = match e.ty {
                Extern::Func(_) => "func",
                Extern::Table(_) => "table",
                Extern::Memory(_) => "memory",
                Extern::Global(_) => "global",
                Extern::Tag(_) => "tag",
            };
            println!("    {kind:<7} {}", e.name);
        }
    }
    match validate(m) {
        Ok(()) => {
            println!("  validation OK");
            true
        }
        // Deferred typing arm — not a verdict on the module, a gap in the validator. Still
        // NON-ZERO: a caller using this command as a validity gate must not pass a module
        // nobody checked (that direction fails open).
        Err(ValidateError::UnsupportedValidation) => {
            println!("  validation SKIPPED (uses a construct the validator can't check yet)");
            false
        }
        // Name the function when the failure was inside one. A bare `TypeMismatch` for a module
        // with twenty bodies is a verdict without a diagnosis — localizing one by hand is what
        // T9a#9 cost before this existed.
        Err(e) => {
            println!("  validation FAILED: {}", invalidity_report(&e));
            false
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// 🔒 The owner's anti-silent-disarm rule (`interop.md` §3.3): a swap must never be able to turn
    /// verification off without saying so. The shared path wins, our legacy path is a fallback, and
    /// finding only the SIBLING's DB is a case of its own rather than "unarmed".
    #[test]
    fn a_swap_cannot_disarm_verification_in_silence() {
        let ours = std::vec![String::from("/etc/wasmtk/pins"), String::from("/etc/wasmrt/pins")];
        let sibling = "/etc/wazmrt/pins";
        let only = |want: &'static str| move |p: &str| p == want;

        assert_eq!(
            choose_db(&ours, sibling, only("/etc/wasmtk/pins")),
            DbChoice::Use(String::from("/etc/wasmtk/pins")),
            "the shared path is preferred"
        );
        assert_eq!(
            choose_db(&ours, sibling, only("/etc/wasmrt/pins")),
            DbChoice::Use(String::from("/etc/wasmrt/pins")),
            "our own path still works as a fallback"
        );
        assert_eq!(
            choose_db(&ours, sibling, only("/etc/wazmrt/pins")),
            DbChoice::NoneButSiblingHasOne,
            "ONLY the sibling has one — this must be said out loud, not silently unarmed"
        );
        assert_eq!(choose_db(&ours, sibling, |_| false), DbChoice::None);
        // The shared path wins even when every path has a DB, so both runtimes read the same one.
        assert_eq!(
            choose_db(&ours, sibling, |_| true),
            DbChoice::Use(String::from("/etc/wasmtk/pins"))
        );
    }

    /// Both separators, and a Windows drive letter is not a separator (the reason `::` exists).
    #[test]
    fn preopen_specs_split_the_way_both_runtimes_spell_them() {
        assert_eq!(split_preopen("."), (String::from("."), String::from(".")));
        assert_eq!(split_preopen(".:/g"), (String::from("."), String::from("/g")));
        assert_eq!(split_preopen(".::/g"), (String::from("."), String::from("/g")));
        assert_eq!(
            split_preopen("C:\\data"),
            (String::from("C:\\data"), String::from("C:\\data")),
            "a drive letter is not a separator"
        );
        assert_eq!(
            split_preopen("C:\\data:/d"),
            (String::from("C:\\data"), String::from("/d"))
        );
    }

    #[test]
    fn sizes_accept_the_suffixes_the_sibling_documents() {
        assert_eq!(parse_size("512M"), Some(512 * 1024 * 1024));
        assert_eq!(parse_size("2G"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("100000"), Some(100_000));
        assert_eq!(parse_size("0"), Some(0));
        assert_eq!(parse_size("lots"), None);
        assert_eq!(parse_size(""), None);
    }
}
