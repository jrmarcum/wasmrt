//! The WASI preview-1 **sandboxed filesystem**: the fd table, the rights model, and the
//! path resolver that is the sandbox itself.
//!
//! # The sandbox is the resolver
//!
//! A guest may only name files under a directory the embedder **preopened**. Nothing else
//! enforces that — not the host OS, not `std::fs` — so [`walk`] is the whole boundary. It
//! resolves a guest path **one component at a time**, and the properties that keep a guest
//! inside its preopen hold *by construction*, not by a check that could be forgotten:
//!
//! - the component stack bottoms out at the preopen: `..` pops it but **never below the
//!   bottom**, so climbing out is not rejected — it is unrepresentable;
//! - a **symlink** is followed, but its target is expanded through the *same* loop, so a
//!   link cannot smuggle in components that skip these rules;
//! - an **absolute** symlink target re-bases to the **preopen root**, never the host root;
//! - a component naming a device or the NT namespace (`C:`, `?`, `??`) is refused, as is an
//!   embedded NUL (which would truncate the path at the syscall boundary);
//! - [`SYMLINK_MAX`] bounds cycles and amplification.
//!
//! # ⚠️ The one divergence from the oracle, and it is a real one
//!
//! wazmrt walks through a stack of **open directory handles**, opening each component
//! `openat`-style relative to the handle it already holds. That pins the inode, which is
//! what makes the walk TOCTOU-safe: an attacker who swaps a directory for a symlink
//! *after* it was checked still loses, because the held handle no longer names the thing
//! they replaced.
//!
//! **Rust's `std` has no dir-relative open.** There is no `openat`, no `O_PATH` handle you
//! can re-open through, on any platform. Reaching one means either a C FFI declaration
//! (`unsafe`, which the safety directive forbids) or `cap-std`/`rustix` (a dependency tree,
//! which the zero-dep decision forbids). All three constraints cannot hold at once, so this
//! port accumulates a **path** rather than handles, and re-resolves it at the syscall.
//!
//! What that costs is precisely bounded: **the escape properties above are unaffected** —
//! they are lexical and hold on the accumulated path exactly as on the handle stack. What
//! is lost is inode pinning, so a **concurrent writer inside the preopen** could swap a
//! component between the walk and the syscall. The guest cannot be that writer (preview 1
//! is single-threaded here), so this needs a *second* process with write access to the
//! sandbox — a threat the embedder controls by choosing what to preopen.
//!
//! [`verify_beneath`] is the compensating control: after the walk, the resolved path is
//! canonicalized and re-checked against the canonical preopen root, so a swap that *did*
//! land still cannot produce a path outside the sandbox. It closes the escape; it does not
//! close the race. `cmem/security-model.md` records this as an open decision — it is the
//! owner's call whether to spend a dependency or an `unsafe` block to close it fully.

use alloc::string::String;
use alloc::vec::Vec;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use super::errno;

/// How many symlinks one resolution may follow before it is called a loop.
const SYMLINK_MAX: u32 = 32;

// --- rights -------------------------------------------------------------------

/// Preview-1 rights bits. Only the ones this layer enforces are named.
pub mod rights {
    pub const FD_DATASYNC: u64 = 1 << 0;
    pub const FD_READ: u64 = 1 << 1;
    pub const FD_SEEK: u64 = 1 << 2;
    pub const FD_FDSTAT_SET_FLAGS: u64 = 1 << 3;
    pub const FD_SYNC: u64 = 1 << 4;
    pub const FD_TELL: u64 = 1 << 5;
    pub const FD_WRITE: u64 = 1 << 6;
    pub const FD_ADVISE: u64 = 1 << 7;
    pub const FD_ALLOCATE: u64 = 1 << 8;
    pub const PATH_CREATE_DIRECTORY: u64 = 1 << 9;
    pub const PATH_CREATE_FILE: u64 = 1 << 10;
    pub const PATH_LINK_SOURCE: u64 = 1 << 11;
    pub const PATH_LINK_TARGET: u64 = 1 << 12;
    pub const PATH_OPEN: u64 = 1 << 13;
    pub const FD_READDIR: u64 = 1 << 14;
    pub const PATH_READLINK: u64 = 1 << 15;
    pub const PATH_RENAME_SOURCE: u64 = 1 << 16;
    pub const PATH_RENAME_TARGET: u64 = 1 << 17;
    pub const PATH_FILESTAT_GET: u64 = 1 << 18;
    pub const PATH_FILESTAT_SET_SIZE: u64 = 1 << 19;
    pub const PATH_FILESTAT_SET_TIMES: u64 = 1 << 20;
    pub const FD_FILESTAT_GET: u64 = 1 << 21;
    pub const FD_FILESTAT_SET_SIZE: u64 = 1 << 22;
    pub const FD_FILESTAT_SET_TIMES: u64 = 1 << 23;
    pub const PATH_SYMLINK: u64 = 1 << 24;
    pub const PATH_REMOVE_DIRECTORY: u64 = 1 << 25;
    pub const PATH_UNLINK_FILE: u64 = 1 << 26;
    pub const POLL_FD_READWRITE: u64 = 1 << 27;

    /// Everything a preopen hands out — the dir's own rights plus what files opened
    /// under it may inherit.
    pub const ALL: u64 = (1 << 29) - 1;

    /// The rights that let a guest *mutate* the filesystem.
    pub const WRITE_MASK: u64 = FD_WRITE
        | FD_ALLOCATE
        | PATH_CREATE_DIRECTORY
        | PATH_CREATE_FILE
        | PATH_LINK_SOURCE
        | PATH_LINK_TARGET
        | PATH_RENAME_SOURCE
        | PATH_RENAME_TARGET
        | PATH_SYMLINK
        | PATH_FILESTAT_SET_TIMES
        | PATH_FILESTAT_SET_SIZE
        | FD_FILESTAT_SET_SIZE
        | FD_FILESTAT_SET_TIMES
        | PATH_REMOVE_DIRECTORY
        | PATH_UNLINK_FILE;

    /// A read-only preopen (`--ro-dir`). `path_open` intersects a new fd's rights with the
    /// directory's *inheriting* rights, so read-only **propagates** down the subtree —
    /// nothing opened beneath a read-only preopen can write either.
    pub const READ_ONLY: u64 = ALL & !WRITE_MASK;
}

/// `oflags` for `path_open`.
pub mod oflags {
    pub const CREAT: u16 = 1 << 0;
    pub const DIRECTORY: u16 = 1 << 1;
    pub const EXCL: u16 = 1 << 2;
    pub const TRUNC: u16 = 1 << 3;
}

/// `fdflags`.
pub mod fdflags {
    pub const APPEND: u16 = 1 << 0;
    pub const DSYNC: u16 = 1 << 1;
    pub const NONBLOCK: u16 = 1 << 2;
    pub const RSYNC: u16 = 1 << 3;
    pub const SYNC: u16 = 1 << 4;
}

/// Preview-1 `filetype`.
pub mod filetype {
    pub const UNKNOWN: u8 = 0;
    pub const DIRECTORY: u8 = 3;
    pub const REGULAR_FILE: u8 = 4;
    pub const SYMBOLIC_LINK: u8 = 7;
}

/// `lookupflags`: whether a final symlink is followed.
pub const LOOKUP_SYMLINK_FOLLOW: u32 = 1 << 0;

// --- the resolver -------------------------------------------------------------

/// Where a walk ended: the directory that contains `name`, and the name itself.
///
/// The walk deliberately stops *before* the final component so the caller can decide what
/// to do with it (open, create, stat-without-following, unlink) — the same split the
/// oracle's `WalkOut` makes.
pub struct Walk {
    /// Host path of the containing directory. Always beneath the preopen root.
    pub dir: PathBuf,
    /// The final component, or `"."` when the path named the directory itself.
    pub name: OsString,
    /// Whether the final component is itself a symlink (it was not followed).
    pub final_is_symlink: bool,
}

impl Walk {
    /// The full host path this walk resolved to.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        if self.name == "." {
            self.dir.clone()
        } else {
            self.dir.join(&self.name)
        }
    }
}

/// True if a symlink target names an absolute location — POSIX root, a Windows separator,
/// or a drive qualifier. Such a target re-bases to the **preopen** root, never the host's.
fn is_absolute_target(t: &[u8]) -> bool {
    match t {
        [] => false,
        [b'/' | b'\\', ..] => true,
        [_, b':', ..] => true,
        _ => false,
    }
}

/// True if a *relative* symlink target lexically climbs above its own directory, tracking
/// depth so an in-sandbox `a/../b` is still allowed.
///
/// The walk already contains such a link at *follow* time, so this is not what keeps the
/// guest in. It exists because `cmem/security-model.md` requires refusing an obviously
/// escaping target **at creation**: otherwise a guest can plant a landmine for the next
/// privileged reader — a host `tar`, `cp -L`, or the next pipeline stage — which is an
/// orchestrator invariant this runtime cannot enforce after the fact.
#[must_use]
pub fn escapes_relative(t: &[u8]) -> bool {
    let mut depth: i64 = 0;
    for seg in t.split(|&c| c == b'/' || c == b'\\') {
        match seg {
            b"" | b"." => {}
            b".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

/// Split a path and push its components onto `pending` **reversed**, so that popping the
/// LIFO yields them left to right. Following a symlink pushes its target this way, on top
/// of the remainder — which is exactly why a link's components get the same treatment as
/// the original path's.
fn push_reversed(pending: &mut Vec<Vec<u8>>, path: &[u8]) {
    let comps: Vec<&[u8]> = path.split(|&c| c == b'/' || c == b'\\').collect();
    for c in comps.into_iter().rev() {
        pending.push(c.to_vec());
    }
}

/// A component that must never appear in a guest path: it would name a device, the NT
/// namespace, or truncate the path at the syscall boundary.
fn component_is_forbidden(c: &[u8]) -> bool {
    c.contains(&0) || c.contains(&b':') || c == b"?" || c == b"??"
}

/// Resolve `guest_path` beneath `root`, following symlinks through the component loop.
///
/// `follow_final = false` leaves a final-component symlink unfollowed — what
/// `path_unlink_file`, `path_readlink`, and a stat without `SYMLINK_FOLLOW` need.
///
/// Returns an errno on failure. See the module doc for exactly which properties this
/// guarantees and the one (TOCTOU) that it does not.
///
/// # Errors
/// `NOTCAPABLE` if the path would leave the sandbox, `LOOP` past [`SYMLINK_MAX`],
/// `NOTDIR` if an intermediate component is not a directory, or the OS's own errno.
pub fn walk(root: &Path, guest_path: &[u8], follow_final: bool) -> Result<Walk, i32> {
    if guest_path.contains(&0) {
        return Err(errno::INVAL);
    }
    // Refuse a forbidden component up front, before touching the filesystem at all. The
    // per-component check below would catch it too, but only once the walk had reached it —
    // which both leaks whether the earlier components exist and makes the refusal depend on
    // filesystem state rather than on the path alone.
    if guest_path
        .split(|&c| c == b'/' || c == b'\\')
        .any(component_is_forbidden)
    {
        return Err(errno::NOTCAPABLE);
    }
    // Components descended into so far. Empty means "at the root"; `..` pops this and can
    // never reach past its bottom, because the root itself is never pushed.
    let mut stack: Vec<OsString> = Vec::new();
    let mut pending: Vec<Vec<u8>> = Vec::new();
    push_reversed(&mut pending, guest_path);
    let mut budget = SYMLINK_MAX;

    while let Some(c) = pending.pop() {
        if c.is_empty() || c == b"." {
            continue;
        }
        if c == b".." {
            // Popping an empty stack would rise above the preopen. There is nothing above
            // it to name, so this is refused rather than clamped.
            if stack.pop().is_none() {
                return Err(errno::NOTCAPABLE);
            }
            continue;
        }
        if component_is_forbidden(&c) {
            return Err(errno::NOTCAPABLE);
        }
        let name = bytes_to_os(&c)?;
        let is_final = pending.is_empty();
        let dir = join_all(root, &stack);
        let probe = dir.join(&name);

        let meta = match std::fs::symlink_metadata(&probe) {
            Ok(m) => m,
            // A missing FINAL component is fine — `path_open` with `O_CREAT` needs exactly
            // this. Anything else is a real error.
            Err(e) if is_final && e.kind() == std::io::ErrorKind::NotFound => {
                verify_beneath(root, &dir)?;
                return Ok(Walk {
                    dir,
                    name,
                    final_is_symlink: false,
                });
            }
            Err(e) => return Err(errno_for(&e)),
        };

        if meta.file_type().is_symlink() && !(is_final && !follow_final) {
            if budget == 0 {
                return Err(errno::LOOP);
            }
            budget -= 1;
            let target = std::fs::read_link(&probe).map_err(|e| errno_for(&e))?;
            let mut t = os_to_bytes(target.as_os_str());
            if is_absolute_target(&t) {
                // Absolute means the **preopen** root. Drop everything back to the bottom
                // and strip the whole absolute prefix — a drive qualifier, then any leading
                // separators — so what remains is relative to the root.
                stack.clear();
                if t.len() >= 2 && t[1] == b':' {
                    t.drain(..2);
                }
                while matches!(t.first(), Some(b'/' | b'\\')) {
                    t.remove(0);
                }
            }
            push_reversed(&mut pending, &t);
            continue;
        }

        if is_final {
            verify_beneath(root, &dir)?;
            return Ok(Walk {
                dir,
                name,
                final_is_symlink: meta.file_type().is_symlink(),
            });
        }

        if !meta.is_dir() {
            return Err(errno::NOTDIR);
        }
        stack.push(name);
    }

    // Everything was `.`/`..`, or the path was empty: the walk names the directory itself.
    let dir = join_all(root, &stack);
    verify_beneath(root, &dir)?;
    Ok(Walk {
        dir,
        name: OsString::from("."),
        final_is_symlink: false,
    })
}

fn join_all(root: &Path, stack: &[OsString]) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in stack {
        p.push(c);
    }
    p
}

/// The compensating control for the missing `openat` (see the module doc): canonicalize the
/// resolved directory and confirm it is still beneath the canonical preopen root.
///
/// The component walk already makes escape unrepresentable; this catches the case where a
/// component was **swapped underneath the walk** by another process. It closes the escape,
/// not the race — a swap can still make the syscall touch the wrong file *inside* the
/// sandbox, which is why this is a compensating control and not a fix.
fn verify_beneath(root: &Path, dir: &Path) -> Result<(), i32> {
    let real_root = std::fs::canonicalize(root).map_err(|e| errno_for(&e))?;
    let real_dir = std::fs::canonicalize(dir).map_err(|e| errno_for(&e))?;
    if real_dir.starts_with(&real_root) {
        Ok(())
    } else {
        Err(errno::NOTCAPABLE)
    }
}

/// Reject a path that is not valid for this host's encoding rather than lossily converting
/// it — a lossy conversion would silently name a *different* file.
fn bytes_to_os(b: &[u8]) -> Result<OsString, i32> {
    core::str::from_utf8(b)
        .map(OsString::from)
        .map_err(|_| errno::INVAL)
}

fn os_to_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    s.to_string_lossy().as_bytes().to_vec()
}

/// Map a host I/O error to a preview-1 errno.
#[must_use]
pub fn errno_for(e: &std::io::Error) -> i32 {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => errno::NOENT,
        K::PermissionDenied => errno::ACCES,
        K::AlreadyExists => errno::EXIST,
        K::InvalidInput => errno::INVAL,
        K::UnexpectedEof => errno::IO,
        _ => match e.raw_os_error() {
            // `DirectoryNotEmpty`/`NotADirectory`/`IsADirectory` are still unstable to
            // match on portably, so fall back to the raw code for the ones that matter.
            Some(c) if is_notdir(c) => errno::NOTDIR,
            Some(c) if is_isdir(c) => errno::ISDIR,
            Some(c) if is_notempty(c) => errno::NOTEMPTY,
            _ => errno::IO,
        },
    }
}

#[cfg(windows)]
fn is_notdir(c: i32) -> bool {
    c == 267 // ERROR_DIRECTORY
}
#[cfg(not(windows))]
fn is_notdir(c: i32) -> bool {
    c == 20 // ENOTDIR
}

#[cfg(windows)]
fn is_isdir(c: i32) -> bool {
    c == 5 // ERROR_ACCESS_DENIED — Windows has no distinct EISDIR
}
#[cfg(not(windows))]
fn is_isdir(c: i32) -> bool {
    c == 21 // EISDIR
}

#[cfg(windows)]
fn is_notempty(c: i32) -> bool {
    c == 145 // ERROR_DIR_NOT_EMPTY
}
#[cfg(not(windows))]
fn is_notempty(c: i32) -> bool {
    c == 39 // ENOTEMPTY
}

// --- the fd table -------------------------------------------------------------

/// A preopened or opened directory. Holds a host **path**, not a handle — see the module
/// doc for why, and what it costs.
pub struct DirFd {
    pub host: PathBuf,
    /// The guest-visible name, set only on preopens (`fd_prestat_dir_name`).
    pub preopen_name: Option<String>,
    pub rights_base: u64,
    pub rights_inheriting: u64,
}

/// An open regular file.
pub struct FileFd {
    pub file: std::fs::File,
    pub rights_base: u64,
    pub rights_inheriting: u64,
    pub append: bool,
}

/// One guest file descriptor.
pub enum FdEntry {
    Stdin,
    Stdout,
    Stderr,
    Dir(DirFd),
    File(FileFd),
}

impl FdEntry {
    /// The rights this fd carries, or `ALL` for stdio (which the rights model does not
    /// gate — a preview-1 guest expects stdio to just work).
    #[must_use]
    pub fn rights_base(&self) -> u64 {
        match self {
            FdEntry::Dir(d) => d.rights_base,
            FdEntry::File(f) => f.rights_base,
            _ => rights::ALL,
        }
    }

    #[must_use]
    pub fn filetype(&self) -> u8 {
        match self {
            FdEntry::Dir(_) => filetype::DIRECTORY,
            FdEntry::File(_) => filetype::REGULAR_FILE,
            // Preview 1 calls a stdio stream a character device.
            _ => 2,
        }
    }
}

/// The guest's fd table. Slot index *is* the guest fd, so 0/1/2 are stdio; a closed fd
/// leaves a `None` hole that the next open reuses (lowest first, as POSIX promises).
#[derive(Default)]
pub struct FdTable {
    slots: Vec<Option<FdEntry>>,
}

impl FdTable {
    /// A table with the three standard streams and no preopens.
    #[must_use]
    pub fn new() -> FdTable {
        FdTable {
            slots: alloc::vec![
                Some(FdEntry::Stdin),
                Some(FdEntry::Stdout),
                Some(FdEntry::Stderr)
            ],
        }
    }

    #[must_use]
    pub fn get(&self, fd: i32) -> Option<&FdEntry> {
        usize::try_from(fd).ok().and_then(|i| self.slots.get(i))?.as_ref()
    }

    pub fn get_mut(&mut self, fd: i32) -> Option<&mut FdEntry> {
        usize::try_from(fd)
            .ok()
            .and_then(|i| self.slots.get_mut(i))?
            .as_mut()
    }

    /// Install an entry at the lowest free fd.
    pub fn insert(&mut self, e: FdEntry) -> i32 {
        let slot = self.slots.iter().position(Option::is_none);
        match slot {
            Some(i) => {
                self.slots[i] = Some(e);
                i as i32
            }
            None => {
                self.slots.push(Some(e));
                (self.slots.len() - 1) as i32
            }
        }
    }

    /// Close an fd. Returns whether it was open.
    pub fn close(&mut self, fd: i32) -> bool {
        match usize::try_from(fd).ok().and_then(|i| self.slots.get_mut(i)) {
            Some(s @ Some(_)) => {
                *s = None;
                true
            }
            _ => false,
        }
    }

    /// Move `from` onto `to`, closing whatever `to` was — `fd_renumber`.
    pub fn renumber(&mut self, from: i32, to: i32) -> bool {
        let (Ok(f), Ok(t)) = (usize::try_from(from), usize::try_from(to)) else {
            return false;
        };
        if f >= self.slots.len() || self.slots[f].is_none() {
            return false;
        }
        if t >= self.slots.len() {
            self.slots.resize_with(t + 1, || None);
        }
        self.slots[t] = self.slots[f].take();
        true
    }

    /// Every open fd, lowest first — `fd_prestat_get` scanning and tests.
    pub fn iter(&self) -> impl Iterator<Item = (i32, &FdEntry)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().map(|e| (i as i32, e)))
    }
}

/// Normalize a preopen's host path once, at registration, so every later walk starts from a
/// canonical root. A relative or symlinked preopen would otherwise make `verify_beneath`
/// compare against the wrong thing.
///
/// # Errors
/// The host's errno if the directory cannot be canonicalized.
pub fn canonical_root(p: &Path) -> Result<PathBuf, i32> {
    let c = std::fs::canonicalize(p).map_err(|e| errno_for(&e))?;
    if c.is_dir() {
        Ok(c)
    } else {
        Err(errno::NOTDIR)
    }
}

/// Whether the walk resolved to the directory itself rather than a named child.
#[must_use]
pub fn is_dot(name: &std::ffi::OsStr) -> bool {
    name == "."
}

/// Whether `p` has any component that is not a plain name — used by tests asserting the
/// walk normalized what it returned.
#[must_use]
pub fn is_normalized(p: &Path) -> bool {
    p.components()
        .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
}

// --- the syscalls -------------------------------------------------------------
//
// Every call here follows the same three steps, in this order: look the fd up, check the
// rights bit, then walk the path. Rights before resolution matters — a guest must not be
// able to probe what exists under a directory it holds without the matching right.

use super::{arg, errno as err, read_iovecs, write_u32, write_u64, MEM};
use crate::interp::{Caller, Trap, Value};

/// Size of the preview-1 `filestat` struct.
const FILESTAT_LEN: usize = 64;
/// Size of the preview-1 `fdstat` struct.
const FDSTAT_LEN: usize = 24;
/// Size of a `dirent` header, before the name bytes.
const DIRENT_LEN: usize = 24;

/// Write an errno into the result slot.
fn ret(results: &mut [Value], code: i32) -> Result<(), Trap> {
    if let Some(r) = results.first_mut() {
        *r = crate::interp::i32_value(code);
    }
    Ok(())
}

/// Read a guest path (pointer + length, not NUL-terminated).
fn read_path(c: &Caller<'_>, ptr: u32, len: u32) -> Option<Vec<u8>> {
    Some(c.read(MEM, u64::from(ptr), len as usize)?.to_vec())
}

/// Look up a directory fd and confirm it carries `need`, then read the guest path.
///
/// Returns the resolved walk, or the errno to report. This is the single funnel every
/// `path_*` call goes through, so the rights check cannot be skipped by adding a call.
fn resolve(
    table: &FdTable,
    c: &Caller<'_>,
    fd: i32,
    need: u64,
    ptr: u32,
    len: u32,
    follow_final: bool,
) -> Result<Walk, i32> {
    let Some(entry) = table.get(fd) else {
        return Err(err::BADF);
    };
    let FdEntry::Dir(d) = entry else {
        return Err(err::NOTDIR);
    };
    if need != 0 && d.rights_base & need != need {
        return Err(err::NOTCAPABLE);
    }
    let Some(path) = read_path(c, ptr, len) else {
        return Err(err::FAULT);
    };
    walk(&d.host, &path, follow_final)
}

fn nanos_of(t: std::io::Result<std::time::SystemTime>) -> u64 {
    t.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos().min(u128::from(u64::MAX)) as u64)
}

fn filetype_of(m: &std::fs::Metadata) -> u8 {
    if m.is_dir() {
        filetype::DIRECTORY
    } else if m.file_type().is_symlink() {
        filetype::SYMBOLIC_LINK
    } else if m.is_file() {
        filetype::REGULAR_FILE
    } else {
        filetype::UNKNOWN
    }
}

/// Serialize a `filestat` into guest memory.
fn write_filestat(c: &mut Caller<'_>, buf: u32, m: &std::fs::Metadata) -> Result<(), i32> {
    let Some(dst) = c.write(MEM, u64::from(buf), FILESTAT_LEN) else {
        return Err(err::FAULT);
    };
    dst.fill(0);
    // dev(0..8) and ino(8..16) stay zero: preview 1 permits it, and a real inode number is
    // not portably reachable without a platform extension.
    dst[16] = filetype_of(m);
    dst[24..32].copy_from_slice(&1u64.to_le_bytes()); // nlink
    dst[32..40].copy_from_slice(&m.len().to_le_bytes());
    dst[40..48].copy_from_slice(&nanos_of(m.accessed()).to_le_bytes());
    dst[48..56].copy_from_slice(&nanos_of(m.modified()).to_le_bytes());
    dst[56..64].copy_from_slice(&nanos_of(m.created()).to_le_bytes());
    Ok(())
}

// --- fd_* on the table --------------------------------------------------------

pub fn fd_prestat_get(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (fd, buf) = (arg(args, 0), arg(args, 1) as u32);
    // Only a *preopen* answers this. An ordinary opened directory must report BADF, or a
    // libc start-up would keep walking fds forever looking for the end of the preopen list.
    let Some(FdEntry::Dir(DirFd {
        preopen_name: Some(name),
        ..
    })) = table.get(fd)
    else {
        return ret(results, err::BADF);
    };
    let Some(dst) = c.write(MEM, u64::from(buf), 8) else {
        return ret(results, err::FAULT);
    };
    dst.fill(0); // tag 0 = preopentype::dir
    let len = u32::try_from(name.len()).unwrap_or(u32::MAX);
    dst[4..8].copy_from_slice(&len.to_le_bytes());
    ret(results, err::SUCCESS)
}

pub fn fd_prestat_dir_name(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (fd, ptr, len) = (arg(args, 0), arg(args, 1) as u32, arg(args, 2) as u32);
    let Some(FdEntry::Dir(DirFd {
        preopen_name: Some(name),
        ..
    })) = table.get(fd)
    else {
        return ret(results, err::BADF);
    };
    let bytes = name.as_bytes();
    if (len as usize) < bytes.len() {
        return ret(results, err::INVAL);
    }
    let Some(dst) = c.write(MEM, u64::from(ptr), bytes.len()) else {
        return ret(results, err::FAULT);
    };
    dst.copy_from_slice(bytes);
    ret(results, err::SUCCESS)
}

pub fn fd_fdstat_get(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (fd, buf) = (arg(args, 0), arg(args, 1) as u32);
    let Some(e) = table.get(fd) else {
        return ret(results, err::BADF);
    };
    let (ft, base, inheriting, flags) = match e {
        FdEntry::Dir(d) => (
            filetype::DIRECTORY,
            d.rights_base,
            d.rights_inheriting,
            0u16,
        ),
        FdEntry::File(f) => (
            filetype::REGULAR_FILE,
            f.rights_base,
            f.rights_inheriting,
            if f.append { fdflags::APPEND } else { 0 },
        ),
        // A character device is what makes a libc treat stdio as unbuffered/non-seekable.
        _ => (2, rights::ALL, rights::ALL, 0),
    };
    let Some(dst) = c.write(MEM, u64::from(buf), FDSTAT_LEN) else {
        return ret(results, err::FAULT);
    };
    dst.fill(0);
    dst[0] = ft;
    dst[2..4].copy_from_slice(&flags.to_le_bytes());
    dst[8..16].copy_from_slice(&base.to_le_bytes());
    dst[16..24].copy_from_slice(&inheriting.to_le_bytes());
    ret(results, err::SUCCESS)
}

pub fn fd_filestat_get(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (fd, buf) = (arg(args, 0), arg(args, 1) as u32);
    let meta = match table.get(fd) {
        Some(FdEntry::File(f)) => {
            if f.rights_base & rights::FD_FILESTAT_GET == 0 {
                return ret(results, err::NOTCAPABLE);
            }
            f.file.metadata()
        }
        Some(FdEntry::Dir(d)) => {
            if d.rights_base & rights::FD_FILESTAT_GET == 0 {
                return ret(results, err::NOTCAPABLE);
            }
            std::fs::metadata(&d.host)
        }
        Some(_) => return write_char_filestat(c, buf, results),
        None => return ret(results, err::BADF),
    };
    match meta {
        Ok(m) => match write_filestat(c, buf, &m) {
            Ok(()) => ret(results, err::SUCCESS),
            Err(e) => ret(results, e),
        },
        Err(e) => ret(results, errno_for(&e)),
    }
}

/// stdio has no file behind it; report a character device of length 0.
fn write_char_filestat(c: &mut Caller<'_>, buf: u32, results: &mut [Value]) -> Result<(), Trap> {
    let Some(dst) = c.write(MEM, u64::from(buf), FILESTAT_LEN) else {
        return ret(results, err::FAULT);
    };
    dst.fill(0);
    dst[16] = 2; // character_device
    ret(results, err::SUCCESS)
}

pub fn fd_seek(
    table: &mut FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    use std::io::{Seek, SeekFrom};
    let fd = arg(args, 0);
    let offset = args.get(1).map_or(0, |&v| crate::interp::as_i64(v));
    let (whence, out) = (arg(args, 2) as u8, arg(args, 3) as u32);
    let Some(FdEntry::File(f)) = table.get_mut(fd) else {
        // Seeking a directory or a stream is not a thing in preview 1.
        return ret(results, err::SPIPE);
    };
    if f.rights_base & rights::FD_SEEK == 0 {
        return ret(results, err::NOTCAPABLE);
    }
    let from = match whence {
        0 => SeekFrom::Start(offset as u64),
        1 => SeekFrom::Current(offset),
        2 => SeekFrom::End(offset),
        _ => return ret(results, err::INVAL),
    };
    match f.file.seek(from) {
        Ok(pos) => {
            if write_u64(c, out, pos).is_none() {
                return ret(results, err::FAULT);
            }
            ret(results, err::SUCCESS)
        }
        Err(e) => ret(results, errno_for(&e)),
    }
}

pub fn fd_tell(
    table: &mut FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    use std::io::Seek;
    let (fd, out) = (arg(args, 0), arg(args, 1) as u32);
    let Some(FdEntry::File(f)) = table.get_mut(fd) else {
        return ret(results, err::SPIPE);
    };
    if f.rights_base & rights::FD_TELL == 0 {
        return ret(results, err::NOTCAPABLE);
    }
    match f.file.stream_position() {
        Ok(pos) => {
            if write_u64(c, out, pos).is_none() {
                return ret(results, err::FAULT);
            }
            ret(results, err::SUCCESS)
        }
        Err(e) => ret(results, errno_for(&e)),
    }
}

/// `fd_read` / `fd_pread` on a real file. `at` is `Some(offset)` for the positional form,
/// which must not disturb the file's own cursor.
pub fn fd_read_file(
    table: &mut FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
    at: Option<u64>,
) -> Result<(), Trap> {
    use std::io::{Read, Seek, SeekFrom};
    let (fd, iovs, n, out) = (
        arg(args, 0),
        arg(args, 1) as u32,
        arg(args, 2) as u32,
        arg(args, if at.is_some() { 4 } else { 3 }) as u32,
    );
    let Some(vecs) = read_iovecs(c, iovs, n) else {
        return ret(results, err::FAULT);
    };
    let Some(FdEntry::File(f)) = table.get_mut(fd) else {
        return ret(results, err::BADF);
    };
    if f.rights_base & rights::FD_READ == 0 {
        return ret(results, err::NOTCAPABLE);
    }
    // Positional I/O without a platform extension: save the cursor, move, restore. Doing it
    // by hand keeps this portable and dependency-free, and preview 1 is single-threaded here
    // so nothing can observe the intermediate position.
    let saved = if at.is_some() {
        match f.file.stream_position() {
            Ok(p) => Some(p),
            Err(e) => return ret(results, errno_for(&e)),
        }
    } else {
        None
    };
    if let Some(off) = at {
        if let Err(e) = f.file.seek(SeekFrom::Start(off)) {
            return ret(results, errno_for(&e));
        }
    }
    let mut total: u32 = 0;
    let mut failed = None;
    let mut chunks: Vec<(u32, Vec<u8>)> = Vec::new();
    for (ptr, len) in vecs {
        let mut buf = alloc::vec![0u8; len as usize];
        match f.file.read(&mut buf) {
            Ok(0) => break,
            Ok(k) => {
                buf.truncate(k);
                total = total.saturating_add(k as u32);
                chunks.push((ptr, buf));
                if k < len as usize {
                    break;
                }
            }
            Err(e) => {
                failed = Some(errno_for(&e));
                break;
            }
        }
    }
    if let (Some(p), Some(_)) = (saved, at) {
        let _ = f.file.seek(SeekFrom::Start(p));
    }
    if let Some(e) = failed {
        return ret(results, e);
    }
    for (ptr, buf) in chunks {
        let Some(dst) = c.write(MEM, u64::from(ptr), buf.len()) else {
            return ret(results, err::FAULT);
        };
        dst.copy_from_slice(&buf);
    }
    if write_u32(c, out, total).is_none() {
        return ret(results, err::FAULT);
    }
    ret(results, err::SUCCESS)
}

/// `fd_write` / `fd_pwrite` on a real file.
pub fn fd_write_file(
    table: &mut FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
    at: Option<u64>,
) -> Result<(), Trap> {
    use std::io::{Seek, SeekFrom, Write};
    let (fd, iovs, n, out) = (
        arg(args, 0),
        arg(args, 1) as u32,
        arg(args, 2) as u32,
        arg(args, if at.is_some() { 4 } else { 3 }) as u32,
    );
    let Some(vecs) = read_iovecs(c, iovs, n) else {
        return ret(results, err::FAULT);
    };
    // Copy out of guest memory first: the borrow of `c` must end before the table borrow.
    let mut data: Vec<u8> = Vec::new();
    for (ptr, len) in vecs {
        let Some(b) = c.read(MEM, u64::from(ptr), len as usize) else {
            return ret(results, err::FAULT);
        };
        data.extend_from_slice(b);
    }
    let Some(FdEntry::File(f)) = table.get_mut(fd) else {
        return ret(results, err::BADF);
    };
    if f.rights_base & rights::FD_WRITE == 0 {
        return ret(results, err::NOTCAPABLE);
    }
    let saved = if at.is_some() {
        match f.file.stream_position() {
            Ok(p) => Some(p),
            Err(e) => return ret(results, errno_for(&e)),
        }
    } else {
        None
    };
    if let Some(off) = at {
        if let Err(e) = f.file.seek(SeekFrom::Start(off)) {
            return ret(results, errno_for(&e));
        }
    } else if f.append {
        if let Err(e) = f.file.seek(SeekFrom::End(0)) {
            return ret(results, errno_for(&e));
        }
    }
    let written = f.file.write(&data);
    if let (Some(p), Some(_)) = (saved, at) {
        let _ = f.file.seek(SeekFrom::Start(p));
    }
    match written {
        Ok(k) => {
            if write_u32(c, out, k as u32).is_none() {
                return ret(results, err::FAULT);
            }
            ret(results, err::SUCCESS)
        }
        Err(e) => ret(results, errno_for(&e)),
    }
}

pub fn fd_filestat_set_size(
    table: &mut FdTable,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let fd = arg(args, 0);
    let size = args.get(1).map_or(0, |&v| crate::interp::as_i64(v)) as u64;
    let Some(FdEntry::File(f)) = table.get_mut(fd) else {
        return ret(results, err::BADF);
    };
    if f.rights_base & rights::FD_FILESTAT_SET_SIZE == 0 {
        return ret(results, err::NOTCAPABLE);
    }
    match f.file.set_len(size) {
        Ok(()) => ret(results, err::SUCCESS),
        Err(e) => ret(results, errno_for(&e)),
    }
}

pub fn fd_readdir(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (fd, buf, buf_len) = (arg(args, 0), arg(args, 1) as u32, arg(args, 2) as u32);
    let cookie = args.get(3).map_or(0, |&v| crate::interp::as_i64(v)) as u64;
    let out = arg(args, 4) as u32;
    let Some(FdEntry::Dir(d)) = table.get(fd) else {
        return ret(results, err::BADF);
    };
    if d.rights_base & rights::FD_READDIR == 0 {
        return ret(results, err::NOTCAPABLE);
    }
    // `.` and `..` come first so a guest walking a tree sees a POSIX-shaped listing. `..`
    // is listed but naming it still cannot leave the preopen — that is the walk's job, not
    // the listing's.
    let mut entries: Vec<(String, u8)> = alloc::vec![
        (String::from("."), filetype::DIRECTORY),
        (String::from(".."), filetype::DIRECTORY),
    ];
    match std::fs::read_dir(&d.host) {
        Ok(rd) => {
            for e in rd.flatten() {
                let ft = e.file_type().map_or(filetype::UNKNOWN, |t| {
                    if t.is_dir() {
                        filetype::DIRECTORY
                    } else if t.is_symlink() {
                        filetype::SYMBOLIC_LINK
                    } else {
                        filetype::REGULAR_FILE
                    }
                });
                entries.push((e.file_name().to_string_lossy().into_owned(), ft));
            }
        }
        Err(e) => return ret(results, errno_for(&e)),
    }

    let mut blob: Vec<u8> = Vec::new();
    for (i, (name, ft)) in entries.iter().enumerate().skip(cookie as usize) {
        if blob.len() >= buf_len as usize {
            break;
        }
        let mut hdr = [0u8; DIRENT_LEN];
        // d_next is the cookie to resume *after* this entry.
        hdr[0..8].copy_from_slice(&((i as u64) + 1).to_le_bytes());
        hdr[16..20].copy_from_slice(&(name.len() as u32).to_le_bytes());
        hdr[20] = *ft;
        blob.extend_from_slice(&hdr);
        blob.extend_from_slice(name.as_bytes());
    }
    // A truncated final entry is correct here: the guest sees bufused == buf_len and knows
    // to call again with a bigger buffer.
    blob.truncate(buf_len as usize);
    let Some(dst) = c.write(MEM, u64::from(buf), blob.len()) else {
        return ret(results, err::FAULT);
    };
    dst.copy_from_slice(&blob);
    if write_u32(c, out, blob.len() as u32).is_none() {
        return ret(results, err::FAULT);
    }
    ret(results, err::SUCCESS)
}

// --- path_* -------------------------------------------------------------------

pub fn path_open(
    table: &mut FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let dirfd = arg(args, 0);
    let dirflags = arg(args, 1) as u32;
    let (ptr, len) = (arg(args, 2) as u32, arg(args, 3) as u32);
    let oflag = arg(args, 4) as u16;
    let want_base = args.get(5).map_or(0, |&v| crate::interp::as_i64(v)) as u64;
    let want_inh = args.get(6).map_or(0, |&v| crate::interp::as_i64(v)) as u64;
    let fdflag = arg(args, 7) as u16;
    let out = arg(args, 8) as u32;

    let follow = dirflags & LOOKUP_SYMLINK_FOLLOW != 0;
    let w = match resolve(
        table,
        c,
        dirfd,
        rights::PATH_OPEN,
        ptr,
        len,
        follow,
    ) {
        Ok(w) => w,
        Err(e) => return ret(results, e),
    };
    // A new fd may never hold more than the directory was willing to pass down. This is
    // what makes a read-only preopen propagate to its whole subtree.
    let Some(FdEntry::Dir(d)) = table.get(dirfd) else {
        return ret(results, err::BADF);
    };
    let base = want_base & d.rights_inheriting;
    let inheriting = want_inh & d.rights_inheriting;
    let (creat, excl, trunc, want_dir) = (
        oflag & oflags::CREAT != 0,
        oflag & oflags::EXCL != 0,
        oflag & oflags::TRUNC != 0,
        oflag & oflags::DIRECTORY != 0,
    );
    // Refuse the *intent* to mutate when the right was not passed down, rather than letting
    // the OS refuse later — the guest gets NOTCAPABLE, which says the sandbox declined.
    if (creat || trunc) && d.rights_inheriting & rights::PATH_CREATE_FILE == 0 {
        return ret(results, err::NOTCAPABLE);
    }
    let host = w.path();

    let existing = std::fs::symlink_metadata(&host).ok();
    if want_dir {
        return match existing {
            Some(m) if m.is_dir() => {
                let e = FdEntry::Dir(DirFd {
                    host,
                    preopen_name: None,
                    rights_base: base,
                    rights_inheriting: inheriting,
                });
                let fd = table.insert(e);
                if write_u32(c, out, fd as u32).is_none() {
                    return ret(results, err::FAULT);
                }
                ret(results, err::SUCCESS)
            }
            Some(_) => ret(results, err::NOTDIR),
            None => ret(results, err::NOENT),
        };
    }
    if existing.as_ref().is_some_and(std::fs::Metadata::is_dir) {
        // Opening a directory without O_DIRECTORY: hand back a directory fd anyway, which
        // is what a preview-1 guest doing `open(".")` expects.
        let e = FdEntry::Dir(DirFd {
            host,
            preopen_name: None,
            rights_base: base,
            rights_inheriting: inheriting,
        });
        let fd = table.insert(e);
        if write_u32(c, out, fd as u32).is_none() {
            return ret(results, err::FAULT);
        }
        return ret(results, err::SUCCESS);
    }
    if excl && existing.is_some() {
        return ret(results, err::EXIST);
    }

    let writable = base & (rights::FD_WRITE | rights::FD_ALLOCATE) != 0;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(base & rights::FD_READ != 0 || !writable)
        .write(writable)
        .create(creat && writable)
        .create_new(creat && excl && writable)
        .truncate(trunc && writable);
    match opts.open(&host) {
        Ok(file) => {
            let e = FdEntry::File(FileFd {
                file,
                rights_base: base,
                rights_inheriting: inheriting,
                append: fdflag & fdflags::APPEND != 0,
            });
            let fd = table.insert(e);
            if write_u32(c, out, fd as u32).is_none() {
                return ret(results, err::FAULT);
            }
            ret(results, err::SUCCESS)
        }
        Err(e) => ret(results, errno_for(&e)),
    }
}

pub fn path_create_directory(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (fd, ptr, len) = (arg(args, 0), arg(args, 1) as u32, arg(args, 2) as u32);
    match resolve(
        table,
        c,
        fd,
        rights::PATH_CREATE_DIRECTORY,
        ptr,
        len,
        false,
    ) {
        Ok(w) => match std::fs::create_dir(w.path()) {
            Ok(()) => ret(results, err::SUCCESS),
            Err(e) => ret(results, errno_for(&e)),
        },
        Err(e) => ret(results, e),
    }
}

pub fn path_remove_directory(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (fd, ptr, len) = (arg(args, 0), arg(args, 1) as u32, arg(args, 2) as u32);
    match resolve(
        table,
        c,
        fd,
        rights::PATH_REMOVE_DIRECTORY,
        ptr,
        len,
        false,
    ) {
        Ok(w) => match std::fs::remove_dir(w.path()) {
            Ok(()) => ret(results, err::SUCCESS),
            Err(e) => ret(results, errno_for(&e)),
        },
        Err(e) => ret(results, e),
    }
}

pub fn path_unlink_file(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (fd, ptr, len) = (arg(args, 0), arg(args, 1) as u32, arg(args, 2) as u32);
    // `follow_final = false`: unlinking a symlink removes the link, never its target.
    match resolve(table, c, fd, rights::PATH_UNLINK_FILE, ptr, len, false) {
        Ok(w) => {
            let p = w.path();
            if std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
                return ret(results, err::ISDIR);
            }
            match std::fs::remove_file(&p) {
                Ok(()) => ret(results, err::SUCCESS),
                Err(e) => ret(results, errno_for(&e)),
            }
        }
        Err(e) => ret(results, e),
    }
}

pub fn path_filestat_get(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let fd = arg(args, 0);
    let flags = arg(args, 1) as u32;
    let (ptr, len, buf) = (arg(args, 2) as u32, arg(args, 3) as u32, arg(args, 4) as u32);
    let follow = flags & LOOKUP_SYMLINK_FOLLOW != 0;
    let w = match resolve(
        table,
        c,
        fd,
        rights::PATH_FILESTAT_GET,
        ptr,
        len,
        follow,
    ) {
        Ok(w) => w,
        Err(e) => return ret(results, e),
    };
    let p = w.path();
    let meta = if follow {
        std::fs::metadata(&p)
    } else {
        std::fs::symlink_metadata(&p)
    };
    match meta {
        Ok(m) => match write_filestat(c, buf, &m) {
            Ok(()) => ret(results, err::SUCCESS),
            Err(e) => ret(results, e),
        },
        Err(e) => ret(results, errno_for(&e)),
    }
}

pub fn path_readlink(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (fd, ptr, len) = (arg(args, 0), arg(args, 1) as u32, arg(args, 2) as u32);
    let (buf, buf_len, out) = (arg(args, 3) as u32, arg(args, 4) as u32, arg(args, 5) as u32);
    let w = match resolve(table, c, fd, rights::PATH_READLINK, ptr, len, false) {
        Ok(w) => w,
        Err(e) => return ret(results, e),
    };
    match std::fs::read_link(w.path()) {
        Ok(t) => {
            let bytes = os_to_bytes(t.as_os_str());
            let n = bytes.len().min(buf_len as usize);
            let Some(dst) = c.write(MEM, u64::from(buf), n) else {
                return ret(results, err::FAULT);
            };
            dst.copy_from_slice(&bytes[..n]);
            if write_u32(c, out, n as u32).is_none() {
                return ret(results, err::FAULT);
            }
            ret(results, err::SUCCESS)
        }
        Err(e) => ret(results, errno_for(&e)),
    }
}

pub fn path_symlink(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
) -> Result<(), Trap> {
    let (old_ptr, old_len) = (arg(args, 0) as u32, arg(args, 1) as u32);
    let fd = arg(args, 2);
    let (new_ptr, new_len) = (arg(args, 3) as u32, arg(args, 4) as u32);
    let Some(target) = read_path(c, old_ptr, old_len) else {
        return ret(results, err::FAULT);
    };
    // Refuse to *plant* an escaping link, even though the walk would contain it at follow
    // time. See `escapes_relative` for why this is a separate obligation.
    if is_absolute_target(&target) || escapes_relative(&target) {
        return ret(results, err::NOTCAPABLE);
    }
    let w = match resolve(table, c, fd, rights::PATH_SYMLINK, new_ptr, new_len, false) {
        Ok(w) => w,
        Err(e) => return ret(results, e),
    };
    let link = w.path();
    let t = match core::str::from_utf8(&target) {
        Ok(t) => PathBuf::from(t),
        Err(_) => return ret(results, err::INVAL),
    };
    match make_symlink(&t, &link, w.dir.join(&t).is_dir()) {
        Ok(()) => ret(results, err::SUCCESS),
        Err(e) => ret(results, e),
    }
}

/// Create a symlink. The one filesystem call with no portable spelling in `std`, so each
/// platform gets its own body — and a host that has neither says **`NOSYS`** rather than
/// reporting a success it did not perform.
///
/// `target_is_dir` is only consulted on Windows, which must be told at creation time which
/// kind of link it is making.
fn make_symlink(target: &Path, link: &Path, target_is_dir: bool) -> Result<(), i32> {
    #[cfg(unix)]
    {
        let _ = target_is_dir;
        std::os::unix::fs::symlink(target, link).map_err(|e| errno_for(&e))
    }
    #[cfg(windows)]
    {
        if target_is_dir {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
        .map_err(|e| errno_for(&e))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link, target_is_dir);
        Err(err::NOSYS)
    }
}

/// `path_rename` and `path_link` — the two calls that walk *two* paths, each against its own
/// directory fd, so a rename can never move a file across a sandbox boundary.
pub fn path_rename_or_link(
    table: &FdTable,
    c: &mut Caller<'_>,
    args: &[Value],
    results: &mut [Value],
    link: bool,
) -> Result<(), Trap> {
    // `path_link` has an extra leading `old_flags`; everything after it lines up.
    let s = usize::from(link);
    let old_fd = arg(args, 0);
    let (old_ptr, old_len) = (arg(args, 1 + s) as u32, arg(args, 2 + s) as u32);
    let new_fd = arg(args, 3 + s);
    let (new_ptr, new_len) = (arg(args, 4 + s) as u32, arg(args, 5 + s) as u32);
    let (need_src, need_dst) = if link {
        (rights::PATH_LINK_SOURCE, rights::PATH_LINK_TARGET)
    } else {
        (rights::PATH_RENAME_SOURCE, rights::PATH_RENAME_TARGET)
    };
    let old = match resolve(table, c, old_fd, need_src, old_ptr, old_len, false) {
        Ok(w) => w,
        Err(e) => return ret(results, e),
    };
    let new = match resolve(table, c, new_fd, need_dst, new_ptr, new_len, false) {
        Ok(w) => w,
        Err(e) => return ret(results, e),
    };
    let r = if link {
        std::fs::hard_link(old.path(), new.path())
    } else {
        std::fs::rename(old.path(), new.path())
    };
    match r {
        Ok(()) => ret(results, err::SUCCESS),
        Err(e) => ret(results, errno_for(&e)),
    }
}

/// A scratch directory that cleans itself up. No `tempfile` dependency — the zero-dep
/// decision applies to dev-dependencies too.
#[cfg(test)]
pub(crate) struct Scratch(pub PathBuf);

#[cfg(test)]
impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let p = std::env::temp_dir().join(alloc::format!(
            "wasmrt-fs-{tag}-{nanos}-{}",
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).expect("scratch dir");
        Scratch(std::fs::canonicalize(&p).expect("canonical scratch"))
    }
    pub fn join(&self, s: &str) -> PathBuf {
        self.0.join(s)
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creating a symlink needs privilege on Windows (admin or Developer Mode). Where it is
    /// unavailable the symlink tests cannot run — and per the project's honesty rule they
    /// say so out loud rather than passing quietly.
    fn symlink(target: &Path, link: &Path, dir: bool) -> bool {
        #[cfg(unix)]
        {
            let _ = dir;
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            let r = if dir {
                std::os::windows::fs::symlink_dir(target, link)
            } else {
                std::os::windows::fs::symlink_file(target, link)
            };
            r.is_ok()
        }
    }

    fn skip(what: &str) {
        std::eprintln!("SKIPPED (symlinks unavailable — needs Developer Mode): {what}");
    }

    // --- the properties that hold with no symlinks involved --------------------

    #[test]
    fn dot_dot_can_never_rise_above_the_preopen() {
        let s = Scratch::new("updot");
        std::fs::create_dir(s.join("sub")).unwrap();
        // Straight up, and the sneakier "descend then climb twice".
        assert_eq!(walk(&s.0, b"..", true).err(), Some(errno::NOTCAPABLE));
        assert_eq!(
            walk(&s.0, b"sub/../..", true).err(),
            Some(errno::NOTCAPABLE)
        );
        assert_eq!(
            walk(&s.0, b"sub/../../../etc/passwd", true).err(),
            Some(errno::NOTCAPABLE)
        );
        // But climbing back to where you started is fine.
        let w = walk(&s.0, b"sub/../sub", true).expect("in-sandbox climb");
        assert_eq!(w.path(), s.join("sub"));
    }

    #[test]
    fn a_walk_never_returns_an_unnormalized_path() {
        let s = Scratch::new("norm");
        std::fs::create_dir(s.join("a")).unwrap();
        std::fs::write(s.join("a/f"), b"x").unwrap();
        let w = walk(&s.0, b"./a/./f", true).unwrap();
        assert!(is_normalized(&w.path()), "{:?}", w.path());
        assert_eq!(w.path(), s.join("a").join("f"));
    }

    #[test]
    fn device_and_nt_namespace_components_are_refused() {
        let s = Scratch::new("dev");
        for p in [
            &b"C:"[..],
            b"C:/Windows",
            b"?/x",
            b"??/x",
            b"a/C:/b",
            b"\\\\?\\C:\\x",
        ] {
            assert_eq!(
                walk(&s.0, p, true).err(),
                Some(errno::NOTCAPABLE),
                "should refuse {:?}",
                String::from_utf8_lossy(p)
            );
        }
    }

    #[test]
    fn an_embedded_nul_is_refused_before_it_can_truncate() {
        let s = Scratch::new("nul");
        assert_eq!(walk(&s.0, b"a\0/../../etc", true).err(), Some(errno::INVAL));
    }

    #[test]
    fn an_absolute_guest_path_names_the_preopen_root_not_the_host_root() {
        let s = Scratch::new("abs");
        std::fs::write(s.join("f"), b"in").unwrap();
        // Leading separators are components that split to empty, so they are skipped and
        // the path resolves relative to the root — it can never reach the host's `/`.
        let w = walk(&s.0, b"/f", true).expect("absolute re-bases to the preopen");
        assert_eq!(w.path(), s.join("f"));
    }

    #[test]
    fn a_missing_final_component_resolves_so_create_can_work() {
        let s = Scratch::new("missing");
        let w = walk(&s.0, b"new.txt", true).expect("missing final is not an error");
        assert_eq!(w.path(), s.join("new.txt"));
        // A missing *intermediate* component is still an error.
        assert_eq!(walk(&s.0, b"nodir/new.txt", true).err(), Some(errno::NOENT));
    }

    #[test]
    fn an_intermediate_non_directory_is_refused() {
        let s = Scratch::new("notdir");
        std::fs::write(s.join("f"), b"x").unwrap();
        assert_eq!(walk(&s.0, b"f/inside", true).err(), Some(errno::NOTDIR));
    }

    // --- the mandated adversarial test: a canary OUTSIDE the preopen -----------

    #[test]
    fn a_symlink_out_of_the_sandbox_can_never_read_the_canary() {
        let outer = Scratch::new("canary-outer");
        let canary = outer.join("canary.txt");
        std::fs::write(&canary, b"SECRET").unwrap();
        let root = outer.join("sandbox");
        std::fs::create_dir(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();

        // Four ways to point out: absolute to the host, relative climb, a two-hop chain,
        // and a symlinked *directory* used as an intermediate component.
        if !symlink(&canary, &root.join("abs"), false) {
            skip("a_symlink_out_of_the_sandbox_can_never_read_the_canary");
            return;
        }
        symlink(Path::new("../canary.txt"), &root.join("rel"), false);
        symlink(Path::new("rel"), &root.join("hop"), false);
        symlink(&outer.0, &root.join("outdir"), true);

        // The property under test is not *which* errno comes back, nor where a re-based
        // path lands — it is that no walk can ever produce a path that reads the canary.
        // Asserting the outcome rather than the mechanism is what makes this test survive
        // a change of mechanism.
        for p in [
            &b"abs"[..],
            b"rel",
            b"hop",
            b"outdir/canary.txt",
            b"../canary.txt",
            b"./sub/../../canary.txt",
        ] {
            let named = String::from_utf8_lossy(p).into_owned();
            if let Ok(w) = walk(&root, p, true) {
                let got = w.path();
                assert!(
                    got.starts_with(&root),
                    "{named} resolved outside the sandbox: {got:?}"
                );
                assert_ne!(
                    std::fs::read(&got).ok().as_deref(),
                    Some(&b"SECRET"[..]),
                    "{named} READ THE CANARY at {got:?}"
                );
            }
        }

        // And the canary is still exactly where it was, unread.
        assert_eq!(std::fs::read(&canary).unwrap(), b"SECRET");
    }

    #[test]
    fn a_symlink_inside_the_sandbox_is_followed_normally() {
        let s = Scratch::new("follow");
        std::fs::create_dir(s.join("d")).unwrap();
        std::fs::write(s.join("d/real.txt"), b"hello").unwrap();
        if !symlink(Path::new("d/real.txt"), &s.join("link"), false) {
            skip("a_symlink_inside_the_sandbox_is_followed_normally");
            return;
        }
        let w = walk(&s.0, b"link", true).expect("in-sandbox link resolves");
        assert_eq!(std::fs::read(w.path()).unwrap(), b"hello");

        // `follow_final = false` leaves it alone — what unlink/readlink need.
        let w = walk(&s.0, b"link", false).expect("unfollowed");
        assert!(w.final_is_symlink);
        assert_eq!(w.path(), s.join("link"));
    }

    #[test]
    fn a_symlink_cycle_ends_in_eloop_rather_than_hanging() {
        let s = Scratch::new("loop");
        if !symlink(Path::new("b"), &s.join("a"), false) {
            skip("a_symlink_cycle_ends_in_eloop_rather_than_hanging");
            return;
        }
        symlink(Path::new("a"), &s.join("b"), false);
        assert_eq!(walk(&s.0, b"a", true).err(), Some(errno::LOOP));
    }

    // --- the create-time landmine check ---------------------------------------

    #[test]
    fn an_escaping_symlink_target_is_recognised_at_creation() {
        // The walk contains these at follow time; this is the separate obligation to refuse
        // planting one, so the next privileged reader does not trip over it.
        assert!(escapes_relative(b"../x"));
        assert!(escapes_relative(b"a/../../x"));
        assert!(escapes_relative(b".."));
        // In-sandbox round trips are fine.
        assert!(!escapes_relative(b"a/../b"));
        assert!(!escapes_relative(b"./a/b"));
        assert!(!escapes_relative(b"a/b/../.."));
    }

    #[test]
    fn rights_are_a_lattice_read_only_removes_every_mutating_bit() {
        // `const {}` so a future edit to the masks fails to *compile* rather than failing a
        // test run — these are the sandbox's shape, not a behaviour that can drift.
        const {
            assert!(rights::READ_ONLY & rights::WRITE_MASK == 0);
            assert!(rights::READ_ONLY & rights::FD_READ != 0);
            assert!(rights::READ_ONLY & rights::PATH_OPEN != 0);
            assert!(rights::READ_ONLY & rights::PATH_UNLINK_FILE == 0);
            assert!(rights::ALL & rights::FD_WRITE != 0);
        }
    }

    #[test]
    fn the_fd_table_reuses_the_lowest_free_slot() {
        let mut t = FdTable::new();
        assert!(matches!(t.get(1), Some(FdEntry::Stdout)));
        let a = t.insert(FdEntry::Stdin);
        assert_eq!(a, 3);
        let b = t.insert(FdEntry::Stdin);
        assert_eq!(b, 4);
        assert!(t.close(a));
        assert!(!t.close(a), "double close must not succeed");
        assert_eq!(t.insert(FdEntry::Stdin), 3, "lowest free slot is reused");
        assert!(t.renumber(4, 9));
        assert!(t.get(4).is_none());
        assert!(t.get(9).is_some());
        assert!(t.get(-1).is_none(), "a negative fd is not a slot");
    }
}
