//! WASI findings from the 2026-09-19 subsystem review, driven through the real CLI and a real
//! guest — the only place the OS's own behaviour (following a symlink, aborting on a huge
//! allocation) is actually observable.
//!
//! Each guest reports an errno as its exit status, so an assertion reads as "what the guest was
//! told". WASI errnos used here: SUCCESS 0 · BADF 8 · FAULT 21 · LOOP 32.

use std::process::{Command, Output};

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join("wasmrt_wasi_hardening").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch dir");
    d
}

fn write(dir: &std::path::Path, name: &str, bytes: &[u8]) -> String {
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("write fixture");
    p.to_str().unwrap().to_string()
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wasmrt"))
        .args(args)
        .output()
        .expect("spawn wasmrt")
}

/// Symlink creation needs privilege on Windows (Developer Mode or admin). Where it is
/// unavailable the escape cannot be staged, so the test says so rather than passing quietly.
fn symlink_file(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

/// 🔒🔒 **The sandbox escape.** `dirflags = 0` means "do not follow a symlink". The walk honoured
/// that and left the last component alone; `path_open` then handed the path to the OS, which
/// followed it anyway, reading a file outside the preopen. Expected now: `LOOP`, the errno
/// `O_NOFOLLOW` reports.
#[test]
fn an_unfollowed_symlink_cannot_reach_its_target_outside_the_preopen() {
    let d = scratch("escape");
    let sandbox = d.join("sandbox");
    let outside = d.join("outside");
    std::fs::create_dir_all(&sandbox).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"TOP SECRET").unwrap();
    if !symlink_file(&outside.join("secret.txt"), &sandbox.join("link")) {
        eprintln!("SKIPPED (symlinks unavailable — needs Developer Mode)");
        return;
    }
    // path_open(fd 3, dirflags 0, "link", …) then read; exits with the errno, or with the first
    // byte it managed to read — 84 is 'T', i.e. the escape succeeded.
    let probe = write(&d, "escape.wat", br#"(module
  (import "wasi_snapshot_preview1" "path_open"
    (func $open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_read" (func $read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
  (memory (export "memory") 1)
  (data (i32.const 100) "link")
  (func (export "_start") (local $e i32) (local $fd i32)
    (local.set $e (call $open (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 4)
                              (i32.const 0) (i64.const 2) (i64.const 2) (i32.const 0) (i32.const 200)))
    (if (i32.ne (local.get $e) (i32.const 0)) (then (call $exit (local.get $e))))
    (local.set $fd (i32.load (i32.const 200)))
    (i32.store (i32.const 300) (i32.const 400))
    (i32.store (i32.const 304) (i32.const 16))
    (drop (call $read (local.get $fd) (i32.const 300) (i32.const 1) (i32.const 308)))
    (call $exit (i32.load8_u (i32.const 400)))))"#);

    let out = run(&["wasi", "--dir", sandbox.to_str().unwrap(), &probe]);
    assert_eq!(
        out.status.code(),
        Some(32),
        "expected LOOP; 84 means the OS followed the link out of the sandbox"
    );
    assert_eq!(
        std::fs::read(outside.join("secret.txt")).unwrap(),
        b"TOP SECRET",
        "and the file outside is untouched"
    );
}

/// ⚠️ `iovs_len` is the guest's number. `Vec::with_capacity(0xFFFF_FFFF)` asked for ~34 GiB and
/// aborted the host — a one-line denial of service from inside the sandbox.
#[test]
fn an_absurd_iovec_count_faults_instead_of_aborting_the_host() {
    let d = scratch("iovec");
    let probe = write(&d, "iovec.wat", br#"(module
  (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
  (memory (export "memory") 1)
  (func (export "_start")
    (call $exit (call $write (i32.const 1) (i32.const 0) (i32.const -1) (i32.const 8)))))"#);
    let out = run(&["wasi", &probe]);
    assert_eq!(out.status.code(), Some(21), "FAULT, and the host is still alive");
}

/// ⚠️ Same shape: the destination was checked only AFTER `len` bytes had been allocated and
/// CSPRNG-filled, so a 4 GiB request cost 4 GiB and seconds of keystream to then answer FAULT.
#[test]
fn an_absurd_random_get_faults_without_generating_the_bytes() {
    let d = scratch("random");
    let probe = write(&d, "random.wat", br#"(module
  (import "wasi_snapshot_preview1" "random_get" (func $rand (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
  (memory (export "memory") 1)
  (func (export "_start")
    (call $exit (call $rand (i32.const 0) (i32.const -1)))))"#);
    let out = run(&["wasi", &probe]);
    assert_eq!(out.status.code(), Some(21), "FAULT");
    // ⚠️ NO WALL-CLOCK ASSERTION HERE, deliberately. The first version asserted "under 10s" as a
    // proxy for "it did not generate 4 GiB first", and it FAILED in the parallel harness while
    // passing alone — it was measuring the machine, not the code. Measured directly instead:
    // this probe returns FAULT in 0.111s, and the ordering it checks (bounds-check the
    // destination BEFORE allocating and filling) is enforced in `random_get` itself.
}

/// ⚠️ The fd table is a dense `Vec`, so the TARGET NUMBER sized the allocation:
/// `fd_renumber(3, 2147483646)` asked for two billion slots.
#[test]
fn renumbering_to_an_absurd_fd_is_badf_not_an_allocation() {
    let d = scratch("renumber");
    let probe = write(&d, "renumber.wat", br#"(module
  (import "wasi_snapshot_preview1" "fd_renumber" (func $ren (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
  (memory (export "memory") 1)
  (func (export "_start")
    (call $exit (call $ren (i32.const 3) (i32.const 2147483646)))))"#);
    let out = run(&["wasi", "--dir", d.to_str().unwrap(), &probe]);
    assert_eq!(out.status.code(), Some(8), "BADF, and the host is still alive");
}

/// ⚠️ `fd_fdstat_set_flags` answered SUCCESS and set nothing, so a guest that asked for `O_APPEND`
/// and then wrote **overwrote the file from offset 0** — and was told the call had worked.
#[test]
fn setting_append_after_open_actually_appends() {
    let d = scratch("append");
    std::fs::write(d.join("f.txt"), b"AB").unwrap();
    let probe = write(&d, "append.wat", br#"(module
  (import "wasi_snapshot_preview1" "path_open"
    (func $open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_fdstat_set_flags" (func $setfl (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
  (memory (export "memory") 1)
  (data (i32.const 100) "f.txt")
  (data (i32.const 120) "C")
  (func (export "_start") (local $e i32) (local $fd i32)
    (local.set $e (call $open (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 5)
                              (i32.const 0) (i64.const 76) (i64.const 76) (i32.const 0) (i32.const 200)))
    (if (i32.ne (local.get $e) (i32.const 0)) (then (call $exit (local.get $e))))
    (local.set $fd (i32.load (i32.const 200)))
    (local.set $e (call $setfl (local.get $fd) (i32.const 1)))
    (if (i32.ne (local.get $e) (i32.const 0)) (then (call $exit (i32.add (i32.const 100) (local.get $e)))))
    (i32.store (i32.const 300) (i32.const 120))
    (i32.store (i32.const 304) (i32.const 1))
    (call $exit (call $write (local.get $fd) (i32.const 300) (i32.const 1) (i32.const 308)))))"#);
    let out = run(&["wasi", "--dir", d.to_str().unwrap(), &probe]);
    assert_eq!(out.status.code(), Some(0), "the write itself succeeded");
    assert_eq!(
        std::fs::read(d.join("f.txt")).unwrap(),
        b"ABC",
        "APPEND must put it at the END; \"CB\" means the flag was accepted and ignored"
    );
}
