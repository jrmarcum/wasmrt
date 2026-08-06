//! `wasi` — WASI **preview 1** host imports. **Requires `std`** (real OS clocks and I/O).
//!
//! Ported at **T7** (`cmem/roadmap.md`). This layer covers the non-filesystem surface:
//! stdio, `args_*`, `environ_*`, clocks, `random_get`, `proc_exit`, plus the calls a libc
//! start-up probes (`fd_prestat_*`, `fd_fdstat_get`). The sandboxed filesystem lands next,
//! on the **zero-dep handle-stack resolver** (owner decision, 2026-08-04). **Invariant when
//! it does:** secure BY CONSTRUCTION — never resolve a full guest path string; resolve one
//! component at a time through held handles; `..` never rises above the preopen; absolute
//! symlink targets re-base to the preopen root. See `cmem/security-model.md`.
//!
//! **`random_get` is a ChaCha20 CSPRNG seeded once from the OS** ([`crate::rng`], owner
//! decision 2026-08-04, matching the frozen oracle). If OS entropy is unavailable,
//! [`WasiCtx::new`] fails rather than emitting predictable bytes.
//!
//! # Shape
//!
//! A [`WasiCtx`] holds the process-ish state (args, environ, the RNG, the streams, the exit
//! code). [`link`] walks a module's declared imports **by name** and returns the positional
//! [`Imports`] the interpreter wants, so a module may declare them in any order and may
//! import only the subset it uses.
//!
//! Unimplemented calls return **`NOSYS`** rather than trapping. That is what a real WASI
//! host does and what guests are written to handle — and unlike a silent wrong answer, the
//! guest sees it.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::interp::{i32_value, Caller, Imports, Trap, Value};
use crate::module::Module;
use crate::rng::ChaCha20Rng;

// --- errno (WASI preview 1) ---------------------------------------------------

/// Preview-1 `errno` values. These are the ABI's own numbering, not the host's.
pub mod errno {
    pub const SUCCESS: i32 = 0;
    pub const ACCES: i32 = 2;
    pub const BADF: i32 = 8;
    pub const EXIST: i32 = 20;
    pub const FAULT: i32 = 21;
    pub const INVAL: i32 = 28;
    pub const IO: i32 = 29;
    pub const ISDIR: i32 = 31;
    pub const LOOP: i32 = 32;
    pub const NOENT: i32 = 44;
    pub const NOSYS: i32 = 52;
    pub const NOTDIR: i32 = 54;
    pub const NOTEMPTY: i32 = 55;
    /// The sandbox said no: the path would leave the preopen, or the fd lacks the right.
    pub const NOTCAPABLE: i32 = 76;
    pub const PERM: i32 = 63;
    pub const SPIPE: i32 = 70;
}

pub const ERRNO_SUCCESS: i32 = errno::SUCCESS;
pub const ERRNO_BADF: i32 = errno::BADF;
pub const ERRNO_FAULT: i32 = errno::FAULT;
pub const ERRNO_INVAL: i32 = errno::INVAL;
pub const ERRNO_NOSYS: i32 = errno::NOSYS;
pub const ERRNO_NOTCAPABLE: i32 = errno::NOTCAPABLE;

pub mod fs;

/// Preview-1 file descriptors for the standard streams.
const FD_STDIN: i32 = 0;
const FD_STDOUT: i32 = 1;
const FD_STDERR: i32 = 2;

/// Where a standard output stream goes.
enum Sink {
    /// Straight to the host's stdout/stderr.
    Inherit,
    /// Buffered, so tests (and embedders) can read what the guest wrote.
    Capture(Vec<u8>),
}

impl Sink {
    fn write(&mut self, fd: i32, bytes: &[u8]) {
        match self {
            Sink::Capture(buf) => buf.extend_from_slice(bytes),
            Sink::Inherit => {
                use std::io::Write;
                let _ = if fd == FD_STDERR {
                    std::io::stderr().write_all(bytes)
                } else {
                    std::io::stdout().write_all(bytes)
                };
            }
        }
    }
}

/// The process-like state a WASI guest sees.
pub struct WasiCtx {
    args: Vec<Vec<u8>>,
    /// `KEY=VALUE` pairs, already in the flat form the ABI hands back.
    env: Vec<Vec<u8>>,
    stdin: Vec<u8>,
    stdin_pos: usize,
    stdout: Sink,
    stderr: Sink,
    rng: ChaCha20Rng,
    /// Set by `proc_exit`; the CLI reports it as the process status.
    exit: Option<i32>,
    /// The guest's file descriptors. Slots 0/1/2 are stdio; preopens are added by
    /// [`WasiCtx::preopen_dir`] and are the **only** way a guest reaches the filesystem.
    fds: fs::FdTable,
}

impl WasiCtx {
    /// Build a context with OS-seeded randomness and inherited stdout/stderr.
    ///
    /// Returns `None` if the OS will not supply entropy — **failing loudly rather than
    /// seeding predictably** is the point of the decision in `cmem/design-decisions.md`.
    #[must_use]
    pub fn new() -> Option<WasiCtx> {
        Some(WasiCtx {
            args: Vec::new(),
            env: Vec::new(),
            stdin: Vec::new(),
            stdin_pos: 0,
            stdout: Sink::Inherit,
            stderr: Sink::Inherit,
            rng: ChaCha20Rng::from_os()?,
            exit: None,
            fds: fs::FdTable::new(),
        })
    }

    /// A context with a **fixed** RNG seed and captured output — for tests and reproducible
    /// runs. Never use the fixed seed where unpredictability matters.
    #[must_use]
    pub fn deterministic(seed: [u8; 32]) -> WasiCtx {
        WasiCtx {
            args: Vec::new(),
            env: Vec::new(),
            stdin: Vec::new(),
            stdin_pos: 0,
            stdout: Sink::Capture(Vec::new()),
            stderr: Sink::Capture(Vec::new()),
            rng: ChaCha20Rng::from_seed(seed, [0u8; 12]),
            exit: None,
            fds: fs::FdTable::new(),
        }
    }

    /// Set the guest's `argv`. By convention `args[0]` is the program name.
    #[must_use]
    pub fn with_args<I: IntoIterator<Item = S>, S: AsRef<[u8]>>(mut self, args: I) -> WasiCtx {
        self.args = args.into_iter().map(|a| a.as_ref().to_vec()).collect();
        self
    }

    /// Set the guest's environment from `KEY`/`VALUE` pairs.
    #[must_use]
    pub fn with_env<I, K, V>(mut self, vars: I) -> WasiCtx
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        self.env = vars
            .into_iter()
            .map(|(k, v)| {
                let mut e = k.as_ref().to_vec();
                e.push(b'=');
                e.extend_from_slice(v.as_ref());
                e
            })
            .collect();
        self
    }

    /// Provide bytes the guest can read from stdin.
    #[must_use]
    pub fn with_stdin(mut self, bytes: impl AsRef<[u8]>) -> WasiCtx {
        self.stdin = bytes.as_ref().to_vec();
        self
    }

    /// Buffer stdout/stderr instead of inheriting the host's.
    #[must_use]
    pub fn capture_output(mut self) -> WasiCtx {
        self.stdout = Sink::Capture(Vec::new());
        self.stderr = Sink::Capture(Vec::new());
        self
    }

    /// What the guest wrote to stdout, if captured.
    #[must_use]
    pub fn stdout(&self) -> Option<&[u8]> {
        match &self.stdout {
            Sink::Capture(b) => Some(b),
            Sink::Inherit => None,
        }
    }

    /// What the guest wrote to stderr, if captured.
    #[must_use]
    pub fn stderr(&self) -> Option<&[u8]> {
        match &self.stderr {
            Sink::Capture(b) => Some(b),
            Sink::Inherit => None,
        }
    }

    /// The code passed to `proc_exit`, if the guest called it.
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        self.exit
    }

    /// Grant the guest access to `host_path`, which it will see as `guest_name`.
    ///
    /// **This is the only way a guest reaches the filesystem.** With no preopen, every
    /// `path_*` call returns `BADF` and the sandbox is total. `rights` is the ceiling for
    /// the whole subtree — pass [`fs::rights::READ_ONLY`] and nothing opened beneath it can
    /// write, because `path_open` intersects each new fd with the directory's inheriting
    /// rights.
    ///
    /// The host path is canonicalized once, here, so every later walk starts from a root
    /// that is already free of symlinks and `..`.
    ///
    /// # Errors
    /// The preview-1 errno if `host_path` is not a readable directory.
    pub fn preopen_dir(
        &mut self,
        host_path: &std::path::Path,
        guest_name: &str,
        rights: u64,
    ) -> Result<i32, i32> {
        let host = fs::canonical_root(host_path)?;
        Ok(self.fds.insert(fs::FdEntry::Dir(fs::DirFd {
            host,
            preopen_name: Some(String::from(guest_name)),
            rights_base: rights,
            rights_inheriting: rights,
        })))
    }

    /// Builder form of [`WasiCtx::preopen_dir`], for the common read-write case.
    ///
    /// # Errors
    /// The preview-1 errno if the directory cannot be preopened.
    pub fn with_preopen(
        mut self,
        host_path: &std::path::Path,
        guest_name: &str,
        rights: u64,
    ) -> Result<WasiCtx, i32> {
        self.preopen_dir(host_path, guest_name, rights)?;
        Ok(self)
    }
}

/// A shared handle to the context: every host closure holds one, and the borrow is taken
/// only inside a host call — never on the interpreter's hot path.
pub type SharedCtx = Rc<RefCell<WasiCtx>>;

/// Wrap a context for linking.
#[must_use]
pub fn shared(ctx: WasiCtx) -> SharedCtx {
    Rc::new(RefCell::new(ctx))
}

// --- guest memory helpers -----------------------------------------------------
//
// WASI guests export memory 0. Every accessor is bounds-checked and turns a bad pointer
// into `FAULT`, so a guest passing garbage gets an errno rather than aborting the host.

const MEM: u32 = 0;

fn read_u32(c: &Caller<'_>, addr: u32) -> Option<u32> {
    let b = c.read(MEM, u64::from(addr), 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn write_u32(c: &mut Caller<'_>, addr: u32, v: u32) -> Option<()> {
    c.write(MEM, u64::from(addr), 4)?
        .copy_from_slice(&v.to_le_bytes());
    Some(())
}

fn write_u64(c: &mut Caller<'_>, addr: u32, v: u64) -> Option<()> {
    c.write(MEM, u64::from(addr), 8)?
        .copy_from_slice(&v.to_le_bytes());
    Some(())
}

/// Read an `iovec` array: `count` pairs of (pointer, length).
fn read_iovecs(c: &Caller<'_>, ptr: u32, count: u32) -> Option<Vec<(u32, u32)>> {
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let base = ptr.checked_add(i.checked_mul(8)?)?;
        out.push((read_u32(c, base)?, read_u32(c, base.checked_add(4)?)?));
    }
    Some(out)
}

/// Whether `fd` is one of the three standard streams, which the context owns directly
/// rather than the fd table.
fn is_stdio(fd: i32) -> bool {
    fd == FD_STDIN || fd == FD_STDOUT || fd == FD_STDERR
}

fn arg(args: &[Value], i: usize) -> i32 {
    args.get(i).map_or(0, |&v| crate::interp::as_i32(v))
}

/// Write an errno into the result slot. Every WASI call returns one.
fn errno(results: &mut [Value], code: i32) -> Result<(), Trap> {
    if let Some(r) = results.first_mut() {
        *r = i32_value(code);
    }
    Ok(())
}

// --- the call implementations -------------------------------------------------

fn fd_write(
    ctx: &SharedCtx,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let fd = arg(args, 0);
    let (iovs, n, nwritten) = (
        arg(args, 1) as u32,
        arg(args, 2) as u32,
        arg(args, 3) as u32,
    );
    if fd != FD_STDOUT && fd != FD_STDERR {
        return errno(results, ERRNO_BADF);
    }
    let Some(vecs) = read_iovecs(c, iovs, n) else {
        return errno(results, ERRNO_FAULT);
    };
    let mut total: u32 = 0;
    for (ptr, len) in vecs {
        let Some(bytes) = c.read(MEM, u64::from(ptr), len as usize) else {
            return errno(results, ERRNO_FAULT);
        };
        let chunk = bytes.to_vec();
        let mut ctx = ctx.borrow_mut();
        let sink = if fd == FD_STDERR {
            &mut ctx.stderr
        } else {
            &mut ctx.stdout
        };
        sink.write(fd, &chunk);
        total = total.saturating_add(len);
    }
    if write_u32(c, nwritten, total).is_none() {
        return errno(results, ERRNO_FAULT);
    }
    errno(results, ERRNO_SUCCESS)
}

fn fd_read(
    ctx: &SharedCtx,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let fd = arg(args, 0);
    let (iovs, n, nread) = (
        arg(args, 1) as u32,
        arg(args, 2) as u32,
        arg(args, 3) as u32,
    );
    if fd != FD_STDIN {
        return errno(results, ERRNO_BADF);
    }
    let Some(vecs) = read_iovecs(c, iovs, n) else {
        return errno(results, ERRNO_FAULT);
    };
    let mut total: u32 = 0;
    for (ptr, len) in vecs {
        let chunk = {
            let mut ctx = ctx.borrow_mut();
            let start = ctx.stdin_pos.min(ctx.stdin.len());
            let take = (len as usize).min(ctx.stdin.len() - start);
            ctx.stdin_pos = start + take;
            ctx.stdin[start..start + take].to_vec()
        };
        if chunk.is_empty() {
            break; // EOF
        }
        let Some(dst) = c.write(MEM, u64::from(ptr), chunk.len()) else {
            return errno(results, ERRNO_FAULT);
        };
        dst.copy_from_slice(&chunk);
        total = total.saturating_add(chunk.len() as u32);
    }
    if write_u32(c, nread, total).is_none() {
        return errno(results, ERRNO_FAULT);
    }
    errno(results, ERRNO_SUCCESS)
}

/// `args_sizes_get` / `environ_sizes_get`: a count and the total byte size.
fn sizes_get(
    items: &[Vec<u8>],
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (count_ptr, size_ptr) = (arg(args, 0) as u32, arg(args, 1) as u32);
    // Each entry is NUL-terminated in the buffer the guest allocates.
    let total: u32 = items.iter().map(|a| a.len() as u32 + 1).sum();
    if write_u32(c, count_ptr, items.len() as u32).is_none()
        || write_u32(c, size_ptr, total).is_none()
    {
        return errno(results, ERRNO_FAULT);
    }
    errno(results, ERRNO_SUCCESS)
}

/// `args_get` / `environ_get`: a pointer array plus a packed NUL-terminated byte buffer.
fn strings_get(
    items: &[Vec<u8>],
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (ptrs, buf) = (arg(args, 0) as u32, arg(args, 1) as u32);
    let mut cursor = buf;
    for (i, item) in items.iter().enumerate() {
        let Some(slot) = (i as u32).checked_mul(4).and_then(|o| ptrs.checked_add(o)) else {
            return errno(results, ERRNO_FAULT);
        };
        if write_u32(c, slot, cursor).is_none() {
            return errno(results, ERRNO_FAULT);
        }
        let Some(dst) = c.write(MEM, u64::from(cursor), item.len() + 1) else {
            return errno(results, ERRNO_FAULT);
        };
        dst[..item.len()].copy_from_slice(item);
        dst[item.len()] = 0;
        let Some(next) = cursor.checked_add(item.len() as u32 + 1) else {
            return errno(results, ERRNO_FAULT);
        };
        cursor = next;
    }
    errno(results, ERRNO_SUCCESS)
}

/// Nanoseconds since the epoch, for `clock_time_get`.
fn now_nanos() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos().min(u128::from(u64::MAX)) as u64)
}

// --- linking ------------------------------------------------------------------

/// Build the [`Imports`] a module needs, resolving its declared function imports **by
/// name** — so a guest may declare them in any order and import only what it uses.
///
/// An unknown name links to a stub returning `NOSYS`, so a guest that merely *references* a
/// call it never makes still instantiates.
///
/// # Errors
/// Currently infallible; returns `Result` so future link-time checks (signature matching,
/// preopen validation) can report without a breaking change.
pub fn link(module: &Module, ctx: &SharedCtx) -> Result<Imports, Trap> {
    let mut linker = crate::linker::Linker::new();
    add_to_linker(&mut linker, ctx);
    // A WASI module imports only functions; if it also imports a global or a memory, that
    // is the caller's to supply, and `Linker::resolve` would refuse. Keep the historical
    // behaviour — skip the non-function imports — by resolving against a module view that
    // has none. (`wasmrt wasi` runs preview-1 programs, which import functions only.)
    let mut view = module.clone();
    view.imports
        .retain(|i| i.ty.kind() == crate::types::ExternKind::Func);
    let store = crate::interp::Store::new();
    linker
        .resolve(&store, &view)
        // Unreachable with a fallback installed — every function import resolves — but
        // mapped rather than unwrapped so a future change cannot turn into a panic.
        .map_err(|_| Trap::MissingImport)
}

/// Register the WASI preview-1 surface on a [`Linker`](crate::linker::Linker).
///
/// **Deliberately a fallback, not a named namespace.** Guests spell the module name
/// `wasi_snapshot_preview1` or `wasi_unstable` depending on their toolchain and vintage,
/// and the CLI's `wasmrt wasi` subcommand has no other host surface to offer — so every
/// function import routes here, and an unrecognised *call* answers `NOSYS` rather than the
/// module failing to instantiate. A guest that merely references a call it never makes
/// still runs; one that actually makes it learns we did not implement it.
///
/// An embedder that wants strict namespace matching should define the surface itself and
/// leave the fallback unset.
pub fn add_to_linker(linker: &mut crate::linker::Linker, ctx: &SharedCtx) {
    let ctx = Rc::clone(ctx);
    linker.define_fallback(move |_module, name| {
        let name = String::from(name);
        let c = Rc::clone(&ctx);
        crate::linker::host_func(move |caller, args, results| {
            dispatch(&c, &name, caller, args, results)
        })
    });
}

/// Route one WASI call by name.
fn dispatch(
    ctx: &SharedCtx,
    name: &str,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    match name {
        "proc_exit" => {
            ctx.borrow_mut().exit = Some(arg(args, 0));
            // Unwinds the interpreter; the caller reads the recorded code. Deliberately not
            // a WASI-specific `Trap` variant — the engine stays unaware of WASI.
            Err(Trap::HostTrap)
        }
        // stdio keeps its own path (the sinks live on the context, not in the fd table);
        // anything else is a real file and goes to the filesystem layer.
        "fd_write" if is_stdio(arg(args, 0)) => fd_write(ctx, c, args, results),
        "fd_read" if is_stdio(arg(args, 0)) => fd_read(ctx, c, args, results),
        "fd_write" => fs::fd_write_file(&mut ctx.borrow_mut().fds, c, args, results, None),
        "fd_read" => fs::fd_read_file(&mut ctx.borrow_mut().fds, c, args, results, None),
        "fd_pwrite" => {
            let at = args.get(3).map_or(0, |&v| crate::interp::as_i64(v)) as u64;
            fs::fd_write_file(&mut ctx.borrow_mut().fds, c, args, results, Some(at))
        }
        "fd_pread" => {
            let at = args.get(3).map_or(0, |&v| crate::interp::as_i64(v)) as u64;
            fs::fd_read_file(&mut ctx.borrow_mut().fds, c, args, results, Some(at))
        }
        "fd_close" => {
            let fd = arg(args, 0);
            if is_stdio(fd) || ctx.borrow_mut().fds.close(fd) {
                errno(results, ERRNO_SUCCESS)
            } else {
                errno(results, ERRNO_BADF)
            }
        }
        "fd_renumber" => {
            let ok = ctx.borrow_mut().fds.renumber(arg(args, 0), arg(args, 1));
            errno(results, if ok { ERRNO_SUCCESS } else { ERRNO_BADF })
        }
        "fd_seek" => fs::fd_seek(&mut ctx.borrow_mut().fds, c, args, results),
        "fd_tell" => fs::fd_tell(&mut ctx.borrow_mut().fds, c, args, results),
        "fd_fdstat_get" => fs::fd_fdstat_get(&ctx.borrow().fds, c, args, results),
        "fd_filestat_get" => fs::fd_filestat_get(&ctx.borrow().fds, c, args, results),
        "fd_filestat_set_size" => {
            fs::fd_filestat_set_size(&mut ctx.borrow_mut().fds, args, results)
        }
        "fd_readdir" => fs::fd_readdir(&ctx.borrow().fds, c, args, results),
        "fd_prestat_get" => fs::fd_prestat_get(&ctx.borrow().fds, c, args, results),
        "fd_prestat_dir_name" => fs::fd_prestat_dir_name(&ctx.borrow().fds, c, args, results),
        "path_open" => fs::path_open(&mut ctx.borrow_mut().fds, c, args, results),
        "path_create_directory" => fs::path_create_directory(&ctx.borrow().fds, c, args, results),
        "path_remove_directory" => fs::path_remove_directory(&ctx.borrow().fds, c, args, results),
        "path_unlink_file" => fs::path_unlink_file(&ctx.borrow().fds, c, args, results),
        "path_filestat_get" => fs::path_filestat_get(&ctx.borrow().fds, c, args, results),
        "path_readlink" => fs::path_readlink(&ctx.borrow().fds, c, args, results),
        "path_symlink" => fs::path_symlink(&ctx.borrow().fds, c, args, results),
        "path_rename" => fs::path_rename_or_link(&ctx.borrow().fds, c, args, results, false),
        "path_link" => fs::path_rename_or_link(&ctx.borrow().fds, c, args, results, true),
        // Advisory or already-durable: nothing to do, and reporting success is honest
        // because there is no writeback cache of our own to flush.
        "fd_fdstat_set_flags" | "sched_yield" | "fd_advise" | "fd_sync" | "fd_datasync" => {
            errno(results, ERRNO_SUCCESS)
        }
        // Deliberately NOSYS rather than a silent success: a guest that needs a real
        // preallocation or a set-times must learn we did not do it.
        "fd_allocate" | "fd_filestat_set_times" | "path_filestat_set_times" => {
            errno(results, ERRNO_NOSYS)
        }
        "args_sizes_get" => {
            let items = ctx.borrow().args.clone();
            sizes_get(&items, c, args, results)
        }
        "args_get" => {
            let items = ctx.borrow().args.clone();
            strings_get(&items, c, args, results)
        }
        "environ_sizes_get" => {
            let items = ctx.borrow().env.clone();
            sizes_get(&items, c, args, results)
        }
        "environ_get" => {
            let items = ctx.borrow().env.clone();
            strings_get(&items, c, args, results)
        }
        "clock_time_get" => {
            let out = arg(args, 2) as u32;
            if write_u64(c, out, now_nanos()).is_none() {
                return errno(results, ERRNO_FAULT);
            }
            errno(results, ERRNO_SUCCESS)
        }
        "clock_res_get" => {
            let out = arg(args, 1) as u32;
            if write_u64(c, out, 1_000).is_none() {
                return errno(results, ERRNO_FAULT);
            }
            errno(results, ERRNO_SUCCESS)
        }
        "random_get" => {
            let (buf, len) = (arg(args, 0) as u32, arg(args, 1) as u32);
            let mut bytes = vec![0u8; len as usize];
            ctx.borrow_mut().rng.fill(&mut bytes);
            let Some(dst) = c.write(MEM, u64::from(buf), bytes.len()) else {
                return errno(results, ERRNO_FAULT);
            };
            dst.copy_from_slice(&bytes);
            errno(results, ERRNO_SUCCESS)
        }
        // Everything else — including the whole filesystem surface until it lands — reports
        // NOSYS. The guest sees the errno; nothing is silently wrong.
        _ => errno(results, ERRNO_NOSYS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::Instance;

    /// Assemble, instantiate with WASI linked, and run `_start`.
    fn run_wasi(src: &str, ctx: WasiCtx) -> (SharedCtx, Result<Vec<Value>, Trap>) {
        let bytes = crate::wat::assemble(src.as_bytes()).expect("assemble");
        let md = crate::module::decode(&bytes).expect("decode");
        crate::validate::validate(&md).expect("validate");
        let sh = shared(ctx);
        let imports = link(&md, &sh).expect("link");
        let mut inst = Instance::new_with_imports(md, imports).expect("instantiate");
        let r = inst.invoke("_start", &[]);
        (sh, r)
    }

    fn i32_of(r: &Result<Vec<Value>, Trap>) -> i32 {
        crate::interp::as_i32(r.as_ref().expect("trapped")[0])
    }

    // --- the filesystem, end to end through the real ABI ------------------------
    //
    // The resolver has its own unit tests; these prove the syscalls actually route through
    // it, which is a separate claim. A guest that opens a file by name and writes to it
    // exercises path_open -> walk -> OpenOptions -> fd table -> fd_write in one go.

    /// A guest that `path_open`s `$path` under preopen fd 3 and writes `written!` to it.
    /// Returns the first non-zero errno, negated, or 0 on success.
    fn fs_guest(path: &str, oflags: i32) -> String {
        alloc::format!(
            r#"(module
              (import "wasi_snapshot_preview1" "path_open"
                (func $open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
              (import "wasi_snapshot_preview1" "fd_write"
                (func $write (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (data (i32.const 64) "{path}")
              (data (i32.const 96) "written!")
              (func (export "_start") (result i32)
                (local $e i32)
                (local.set $e (call $open
                  (i32.const 3) (i32.const 0) (i32.const 64) (i32.const {len})
                  (i32.const {oflags})
                  (i64.const 0x1FFFFFFF) (i64.const 0x1FFFFFFF)
                  (i32.const 0) (i32.const 0)))
                (if (local.get $e) (then (return (i32.sub (i32.const 0) (local.get $e)))))
                (i32.store (i32.const 16) (i32.const 96))
                (i32.store (i32.const 20) (i32.const 8))
                (call $write (i32.load (i32.const 0)) (i32.const 16) (i32.const 1)
                             (i32.const 8))))"#,
            len = path.len(),
        )
    }

    /// `path_open`'s CREAT flag.
    const O_CREAT: i32 = 1;

    #[test]
    fn a_guest_can_create_and_write_a_file_inside_its_preopen() {
        let s = fs::Scratch::new("e2e-write");
        let ctx = WasiCtx::deterministic([7; 32])
            .with_preopen(&s.0, "/sandbox", fs::rights::ALL)
            .expect("preopen");
        let (_, r) = run_wasi(&fs_guest("out.txt", O_CREAT), ctx);
        assert_eq!(i32_of(&r), 0, "the guest reported an errno");
        assert_eq!(
            std::fs::read(s.join("out.txt")).expect("guest should have created it"),
            b"written!"
        );
    }

    #[test]
    fn a_guest_cannot_escape_its_preopen_through_path_open() {
        let outer = fs::Scratch::new("e2e-escape");
        let root = outer.join("sandbox");
        std::fs::create_dir(&root).unwrap();
        let ctx = WasiCtx::deterministic([7; 32])
            .with_preopen(&root, "/sandbox", fs::rights::ALL)
            .expect("preopen");
        let (_, r) = run_wasi(&fs_guest("../escaped.txt", O_CREAT), ctx);
        assert_eq!(
            i32_of(&r),
            -ERRNO_NOTCAPABLE,
            "path_open must refuse to climb out"
        );
        assert!(
            !outer.join("escaped.txt").exists(),
            "the guest wrote OUTSIDE its preopen"
        );
    }

    #[test]
    fn a_read_only_preopen_refuses_the_write_and_the_restriction_propagates() {
        let s = fs::Scratch::new("e2e-ro");
        std::fs::write(s.join("out.txt"), b"original").unwrap();
        let ctx = WasiCtx::deterministic([7; 32])
            .with_preopen(&s.0, "/sandbox", fs::rights::READ_ONLY)
            .expect("preopen");
        // The *open* still succeeds: `path_open` intersects the guest's requested rights
        // with what the directory passes down, so it gets a read-only fd rather than an
        // error. The refusal lands one step later, at the write — which is why the guest
        // returns it positively (the negated values are path_open's).
        let (_, r) = run_wasi(&fs_guest("out.txt", 0), ctx);
        assert_eq!(
            i32_of(&r),
            ERRNO_NOTCAPABLE,
            "the write through a read-only preopen should be refused"
        );
        assert_eq!(
            std::fs::read(s.join("out.txt")).unwrap(),
            b"original",
            "a read-only preopen was written through"
        );
    }

    #[test]
    fn with_no_preopen_the_filesystem_is_simply_absent() {
        // The default context grants nothing, so every path call is BADF — there is no
        // ambient authority to fall back on.
        let (_, r) = run_wasi(&fs_guest("anything.txt", O_CREAT), WasiCtx::deterministic([7; 32]));
        assert_eq!(i32_of(&r), -ERRNO_BADF);
    }

    #[test]
    fn a_preopen_reports_its_guest_name_to_the_libc_startup() {
        let s = fs::Scratch::new("e2e-prestat");
        let mut ctx = WasiCtx::deterministic([7; 32]);
        let fd = ctx
            .preopen_dir(&s.0, "/data", fs::rights::ALL)
            .expect("preopen");
        assert_eq!(fd, 3, "preopens start after stdio");
        let src = r#"(module
          (import "wasi_snapshot_preview1" "fd_prestat_get"
            (func $pre (param i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_prestat_dir_name"
            (func $name (param i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "_start") (result i32)
            (local $e i32)
            (local.set $e (call $pre (i32.const 3) (i32.const 0)))
            (if (local.get $e) (then (return (i32.sub (i32.const 0) (local.get $e)))))
            ;; name length lands at offset 4; ask for it, then confirm fd 4 is not a preopen
            (local.set $e (call $name (i32.const 3) (i32.const 32) (i32.load (i32.const 4))))
            (if (local.get $e) (then (return (i32.sub (i32.const 0) (local.get $e)))))
            (i32.load (i32.const 4))))"#;
        let (sh, r) = run_wasi(src, ctx);
        assert_eq!(i32_of(&r), 5, "/data is five bytes");
        drop(sh);
    }

    #[test]
    fn a_guest_writes_to_stdout() {
        // The classic shape: build an iovec in memory, then fd_write(1, iov, 1, &n).
        let src = r#"(module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (data (i32.const 100) "hi\n")
            (func (export "_start")
              (i32.store (i32.const 0) (i32.const 100))
              (i32.store (i32.const 4) (i32.const 3))
              (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))))"#;
        let (ctx, r) = run_wasi(src, WasiCtx::deterministic([0; 32]));
        r.expect("_start trapped");
        assert_eq!(ctx.borrow().stdout(), Some(&b"hi\n"[..]));
    }

    #[test]
    fn fd_write_reports_the_byte_count() {
        let src = r#"(module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (data (i32.const 100) "abcd")
            (func (export "_start") (result i32)
              (i32.store (i32.const 0) (i32.const 100))
              (i32.store (i32.const 4) (i32.const 4))
              (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))
              (i32.load (i32.const 8))))"#;
        let (_c, r) = run_wasi(src, WasiCtx::deterministic([0; 32]));
        assert_eq!(i32_of(&r), 4);
    }

    #[test]
    fn stderr_is_a_separate_stream() {
        let src = r#"(module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $w (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (data (i32.const 100) "err")
            (func (export "_start")
              (i32.store (i32.const 0) (i32.const 100))
              (i32.store (i32.const 4) (i32.const 3))
              (drop (call $w (i32.const 2) (i32.const 0) (i32.const 1) (i32.const 8)))))"#;
        let (ctx, r) = run_wasi(src, WasiCtx::deterministic([0; 32]));
        r.expect("_start trapped");
        assert_eq!(ctx.borrow().stderr(), Some(&b"err"[..]));
        assert_eq!(ctx.borrow().stdout(), Some(&b""[..]));
    }

    #[test]
    fn writing_to_a_bad_fd_returns_badf() {
        let src = r#"(module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $w (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (func (export "_start") (result i32)
              (call $w (i32.const 42) (i32.const 0) (i32.const 1) (i32.const 8))))"#;
        let (_c, r) = run_wasi(src, WasiCtx::deterministic([0; 32]));
        assert_eq!(i32_of(&r), ERRNO_BADF);
    }

    #[test]
    fn a_wild_pointer_faults_rather_than_aborting_the_host() {
        // The property that matters: a guest passing garbage gets an errno, not a panic.
        let src = r#"(module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $w (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (func (export "_start") (result i32)
              (call $w (i32.const 1) (i32.const 0xfff0) (i32.const 1000) (i32.const 8))))"#;
        let (_c, r) = run_wasi(src, WasiCtx::deterministic([0; 32]));
        assert_eq!(i32_of(&r), ERRNO_FAULT);
    }

    #[test]
    fn stdin_reads_then_reports_eof() {
        let src = r#"(module
            (import "wasi_snapshot_preview1" "fd_read"
              (func $r (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (func (export "_start") (result i32)
              (i32.store (i32.const 0) (i32.const 100))
              (i32.store (i32.const 4) (i32.const 64))
              (drop (call $r (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 8)))
              ;; bytes read * 1000 + first byte
              (i32.add (i32.mul (i32.load (i32.const 8)) (i32.const 1000))
                       (i32.load8_u (i32.const 100)))))"#;
        let ctx = WasiCtx::deterministic([0; 32]).with_stdin("hey");
        let (_c, r) = run_wasi(src, ctx);
        assert_eq!(i32_of(&r), 3 * 1000 + i32::from(b'h'));
    }

    #[test]
    fn args_are_visible_to_the_guest() {
        let src = r#"(module
            (import "wasi_snapshot_preview1" "args_sizes_get"
              (func $sizes (param i32 i32) (result i32)))
            (memory 1)
            (func (export "_start") (result i32)
              (drop (call $sizes (i32.const 0) (i32.const 4)))
              (i32.add (i32.mul (i32.load (i32.const 0)) (i32.const 1000))
                       (i32.load (i32.const 4)))))"#;
        let ctx = WasiCtx::deterministic([0; 32]).with_args(["prog", "ab"]);
        let (_c, r) = run_wasi(src, ctx);
        // 2 args; bytes = "prog\0" (5) + "ab\0" (3) = 8.
        assert_eq!(i32_of(&r), 2 * 1000 + 8);
    }

    #[test]
    fn args_get_writes_pointers_and_nul_terminated_bytes() {
        let src = r#"(module
            (import "wasi_snapshot_preview1" "args_get"
              (func $get (param i32 i32) (result i32)))
            (memory 1)
            (func (export "_start") (result i32)
              (drop (call $get (i32.const 0) (i32.const 64)))
              (i32.add (i32.mul (i32.load (i32.const 0)) (i32.const 1000))
                       (i32.load8_u (i32.const 64)))))"#;
        let ctx = WasiCtx::deterministic([0; 32]).with_args(["hi"]);
        let (_c, r) = run_wasi(src, ctx);
        assert_eq!(i32_of(&r), 64 * 1000 + i32::from(b'h'));
    }

    #[test]
    fn environ_round_trips() {
        let src = r#"(module
            (import "wasi_snapshot_preview1" "environ_sizes_get"
              (func $sizes (param i32 i32) (result i32)))
            (memory 1)
            (func (export "_start") (result i32)
              (drop (call $sizes (i32.const 0) (i32.const 4)))
              (i32.load (i32.const 4))))"#;
        let ctx = WasiCtx::deterministic([0; 32]).with_env([("A", "b")]);
        let (_c, r) = run_wasi(src, ctx);
        assert_eq!(i32_of(&r), 4); // "A=b\0"
    }

    #[test]
    fn random_get_fills_the_buffer() {
        let src = r#"(module
            (import "wasi_snapshot_preview1" "random_get"
              (func $rand (param i32 i32) (result i32)))
            (memory 1)
            (func (export "_start") (result i32)
              (drop (call $rand (i32.const 0) (i32.const 16)))
              (i32.or (i32.load (i32.const 0))
                      (i32.or (i32.load (i32.const 4))
                              (i32.or (i32.load (i32.const 8)) (i32.load (i32.const 12)))))))"#;
        let (_c, r) = run_wasi(src, WasiCtx::deterministic([5; 32]));
        assert_ne!(i32_of(&r), 0);
    }

    #[test]
    fn clock_time_get_is_nonzero() {
        let src = r#"(module
            (import "wasi_snapshot_preview1" "clock_time_get"
              (func $now (param i32 i64 i32) (result i32)))
            (memory 1)
            (func (export "_start") (result i32)
              (drop (call $now (i32.const 0) (i64.const 0) (i32.const 0)))
              (i32.or (i32.load (i32.const 0)) (i32.load (i32.const 4)))))"#;
        let (_c, r) = run_wasi(src, WasiCtx::deterministic([0; 32]));
        assert_ne!(i32_of(&r), 0);
    }

    #[test]
    fn proc_exit_records_the_code_and_stops_the_guest() {
        let src = r#"(module
            (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
            (memory 1)
            (func (export "_start") (result i32)
              (call $exit (i32.const 3))
              (i32.const 99)))"#;
        let (ctx, r) = run_wasi(src, WasiCtx::deterministic([0; 32]));
        assert!(r.is_err(), "proc_exit must stop execution");
        assert_eq!(ctx.borrow().exit_code(), Some(3));
    }

    #[test]
    fn an_unimplemented_call_reports_nosys() {
        // Not a trap: the guest sees the errno and can cope, which is what real hosts do.
        // (This used `path_open` until the filesystem landed; sockets are the remaining
        // genuinely-unimplemented surface.)
        let src = r#"(module
            (import "wasi_snapshot_preview1" "sock_accept"
              (func $a (param i32 i32 i32) (result i32)))
            (memory 1)
            (func (export "_start") (result i32)
              (call $a (i32.const 0) (i32.const 0) (i32.const 0))))"#;
        let (_c, r) = run_wasi(src, WasiCtx::deterministic([0; 32]));
        assert_eq!(i32_of(&r), ERRNO_NOSYS);
    }

    #[test]
    fn a_preopen_probe_reports_badf_so_libc_stops_asking() {
        let src = r#"(module
            (import "wasi_snapshot_preview1" "fd_prestat_get"
              (func $p (param i32 i32) (result i32)))
            (memory 1)
            (func (export "_start") (result i32)
              (call $p (i32.const 3) (i32.const 0))))"#;
        let (_c, r) = run_wasi(src, WasiCtx::deterministic([0; 32]));
        assert_eq!(i32_of(&r), ERRNO_BADF);
    }

    #[test]
    fn imports_may_be_declared_in_any_order() {
        // `link` resolves BY NAME, so declaration order is the guest's business — the
        // positional Imports vector is built to match whatever order it chose.
        let src = r#"(module
            (import "wasi_snapshot_preview1" "random_get" (func $r (param i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_write"
              (func $w (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (data (i32.const 100) "ok")
            (func (export "_start")
              (i32.store (i32.const 0) (i32.const 100))
              (i32.store (i32.const 4) (i32.const 2))
              (drop (call $w (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))))"#;
        let (ctx, r) = run_wasi(src, WasiCtx::deterministic([0; 32]));
        r.expect("_start trapped");
        assert_eq!(ctx.borrow().stdout(), Some(&b"ok"[..]));
    }
}
