//! Regressions found by code review on 2026-09-19, the day the code was written — each was
//! **silent**: the command succeeded while doing something other than what was asked.
//!
//! 🎓 Three of the four are one root cause: **flags parsed in one place and used in another**.
//! Parsing a flag proves it was ACCEPTED; only the code path that honours it proves it was
//! APPLIED, and a security flag that is accepted and dropped is worse than one that is rejected.

use std::process::{Command, Output};

fn dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join("wasmrt_review_regressions");
    std::fs::create_dir_all(&d).expect("scratch dir");
    d
}

fn write(name: &str, bytes: &[u8]) -> String {
    let p = dir().join(name);
    std::fs::write(&p, bytes).expect("write fixture");
    p.to_str().unwrap().to_string()
}

fn wasmrt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wasmrt"))
        .args(args)
        .output()
        .expect("spawn wasmrt")
}

/// Reports the number of environment variables the guest can see, as its exit status.
const ENV_PROBE: &[u8] = br#"(module
  (import "wasi_snapshot_preview1" "environ_sizes_get" (func $sizes (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
  (memory (export "memory") 1)
  (func (export "_start")
    (drop (call $sizes (i32.const 0) (i32.const 8)))
    (call $exit (i32.load (i32.const 0)))))"#;

const SCRIPT: &[u8] = br#"(module (func (export "f") (result i32) (i32.const 1)))
                          (assert_return (invoke "f") (i32.const 1))"#;

/// BOTH a WASI command and the owner of an export called `status`, so the two run modes are
/// distinguishable — and `_start` exits with its own ARGC, so the guest argv is observable. Without
/// that, a stray `--` left in the argv is invisible: the module still runs and still prints nothing.
const BOTH: &[u8] = br#"(module
  (import "wasi_snapshot_preview1" "args_sizes_get" (func $argc (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
  (memory (export "memory") 1)
  (func (export "_start")
    (drop (call $argc (i32.const 0) (i32.const 8)))
    (call $exit (i32.load (i32.const 0))))
  (func (export "status") (result i32) (i32.const 42)))"#;

/// The same two exports with NO imports — because calling an export wires none, so a module that
/// imports WASI cannot be instantiated down that path. (Found by this very test: the argc probe
/// above could not also serve as the export-call fixture.)
const BOTH_NO_IMPORTS: &[u8] = br#"(module (memory (export "memory") 1)
  (func (export "_start"))
  (func (export "status") (result i32) (i32.const 42)))"#;

/// ⚠️⚠️ `WasiCtx::with_env` ASSIGNS. Calling it twice — inherited environment, then `--env` —
/// read as layering and was replacement: the guest saw **zero** variables.
#[test]
fn env_flags_add_to_the_inherited_environment_rather_than_replacing_it() {
    let m = write("env_probe.wat", ENV_PROBE);
    let inherited = wasmrt(&["wasi", &m]).status.code().expect("exit code");
    assert!(inherited > 0, "the guest must inherit the host environment, saw {inherited}");

    let with_new = wasmrt(&["wasi", "--env", "WASMRT_TEST_ONLY=1", &m]).status.code().unwrap();
    assert_eq!(with_new, inherited + 1, "a new variable is added");

    // An override replaces in place: the count does not grow, and nothing is duplicated (two
    // entries for one key is a shape no libc expects).
    let overridden = wasmrt(&["wasi", "--env", "PATH=x", &m]).status.code().unwrap();
    assert_eq!(overridden, inherited, "an existing variable is replaced, not duplicated");
}

/// ⚠️ `wast` has no guest argv, so a host flag is a host flag wherever it appears. Parsing only a
/// LEADING run meant `wast s.wast --verify enforce` accepted the flag and dropped it.
#[test]
fn wast_honours_host_flags_after_the_script_path_too() {
    let s = write("after.wast", SCRIPT);
    let db = write("after_db.txt", b"# mode: warn\n");
    for args in [
        std::vec!["wast", "--pins", &db, "--verify", "enforce", &s],
        std::vec!["wast", &s, "--pins", &db, "--verify", "enforce"],
    ] {
        let out = wasmrt(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?} must refuse to run unpinned");
        assert!(String::from_utf8_lossy(&out.stderr).contains("refusing to run"));
    }
    // Unflagged, the same script runs — otherwise the assertions above pass for the wrong reason.
    assert_eq!(wasmrt(&["wast", &s]).status.code(), Some(0));
}

/// ⚠️ The bare-path `.wast` form rebuilt an argv and re-parsed it, **discarding every flag that
/// preceded the path** — so `wasmrt --verify enforce s.wast` ran the script.
#[test]
fn a_bare_path_script_keeps_the_flags_written_before_it() {
    let s = write("bare.wast", SCRIPT);
    let db = write("bare_db.txt", b"# mode: warn\n");
    let out = wasmrt(&["--pins", &db, "--verify", "enforce", &s]);
    assert_eq!(out.status.code(), Some(1), "an unpinned script must be refused");
    assert_eq!(wasmrt(&[&s]).status.code(), Some(0), "and it runs without the flags");
}

/// ⚠️ `--` means "the rest is the GUEST's". Without remembering it, the first word after the
/// marker was still matched against export names: `prog.wasm -- status` CALLED `status`.
#[test]
fn the_end_of_flags_marker_forces_guest_argv_instead_of_an_export_name() {
    let m = write("marker.wat", BOTH);
    // argv is [<path>, "status"] — TWO. The marker itself must not reach the guest, which is what
    // makes this stronger than "nothing was printed": leaving `--` in argv still runs _start.
    let after_marker = wasmrt(&[&m, "--", "status"]);
    assert_eq!(
        after_marker.status.code(),
        Some(2),
        "`-- status` must run _start with argv [path, status]; stdout: {}",
        String::from_utf8_lossy(&after_marker.stdout)
    );
    // Without the marker the same word IS an export name, so the test distinguishes the modes.
    // A separate, import-free fixture: the export path wires no imports at all.
    let plain = write("marker_plain.wat", BOTH_NO_IMPORTS);
    let as_export = wasmrt(&[&plain, "status"]);
    assert_eq!(String::from_utf8_lossy(&as_export.stdout).trim(), "42");
    assert_eq!(wasmrt(&[&plain, "--", "status"]).status.code(), Some(0), "and with the marker it runs _start");

    // Arguments after `--` with nothing to give them to is an error, not a silent summary.
    let no_start = write("marker_nostart.wat", br#"(module (func (export "f")))"#);
    assert_eq!(wasmrt(&[&no_start, "--", "x"]).status.code(), Some(1));
}
