//! **The bare-path run modes and the flags that go with them** — `interop.md` §2.1/§2.2, the
//! ADDITIVE target: wasmrt accepts the sibling runtime's spelling as well as its own, so a command
//! line written for either drives both. Measured against wazmrt 1.0.1 before each was written.

use std::process::{Command, Output};

fn write(name: &str, bytes: &[u8]) -> String {
    let d = std::env::temp_dir().join("wasmrt_run_modes");
    std::fs::create_dir_all(&d).expect("scratch dir");
    let p = d.join(name);
    std::fs::write(&p, bytes).expect("write fixture");
    p.to_str().unwrap().to_string()
}

fn wasmrt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wasmrt"))
        .args(args)
        .output()
        .expect("spawn wasmrt")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

const ADD: &[u8] = br#"(module (func (export "add") (param i32 i32) (result i32)
                        (i32.add (local.get 0) (local.get 1))))"#;
const COMMAND: &[u8] = br#"(module (memory (export "memory") 1) (func (export "_start")))"#;

#[test]
fn a_bare_path_calls_a_named_export() {
    let m = write("bare_add.wat", ADD);
    let out = wasmrt(&[&m, "add", "2", "3"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&out), "5");
    // …and the subcommand spelling keeps working, because the target is ADDITIVE.
    assert_eq!(stdout(&wasmrt(&["run", &m, "add", "2", "3"])), "5");
}

/// 🔒 **Z1** (`interop.md` §2.3): naming an export the module does not have is a FAILURE. It used
/// to print a summary and exit 0, ignoring both the export and its arguments.
#[test]
fn naming_an_export_that_does_not_exist_fails_loudly() {
    let m = write("bare_missing.wat", ADD);
    let out = wasmrt(&[&m, "nosuch", "1"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no exported function `nosuch`"), "{err}");
}

/// ⚠️⚠️ A bare path now EXECUTES a WASI command — the posture change the pin gate had to precede.
#[test]
fn a_bare_path_runs_a_wasi_command_and_a_plain_module_is_only_summarized() {
    let cmd = write("bare_start.wat", COMMAND);
    assert_eq!(wasmrt(&[&cmd]).status.code(), Some(0));

    // No `_start`, no export named: inspection only, and the summary says so.
    let m = write("bare_plain.wat", ADD);
    let out = wasmrt(&[&m]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("validation OK"), "{}", stdout(&out));
}

#[test]
fn a_bare_wast_script_runs() {
    let s = write(
        "bare_script.wast",
        br#"(module (func (export "f") (result i32) (i32.const 1)))
            (assert_return (invoke "f") (i32.const 1))"#,
    );
    let out = wasmrt(&[&s]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("1 passed"), "{}", stdout(&out));
}

/// 🔒 **`--features` goes BEFORE the module, and anywhere else is an error** (owner, 2026-09-19).
/// It selects the language the module is decoded and validated against, so after the path the
/// decision has already been made — accepting it there would silently ignore it.
#[test]
fn features_must_precede_the_module_and_says_so_when_it_does_not() {
    let m = write("feat.wat", ADD);
    assert_eq!(stdout(&wasmrt(&["--features", "mvp", &m, "add", "2", "3"])), "5");

    let out = wasmrt(&[&m, "--features", "mvp", "add", "2", "3"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("must come BEFORE the module path"), "{err}");
}

#[test]
fn features_refuses_an_unknown_proposal_and_an_incoherent_set() {
    let m = write("feat2.wat", ADD);
    let unknown = wasmrt(&["--features", "bogus", &m, "add", "1", "1"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown proposal 'bogus'"));

    // Layering is the proposals' own: gc is defined on top of function-references.
    let incoherent = wasmrt(&["--features", "gc", &m, "add", "1", "1"]);
    assert_eq!(incoherent.status.code(), Some(1));
    let err = String::from_utf8_lossy(&incoherent.stderr);
    assert!(err.contains("gc") && err.contains("function-references"), "{err}");

    // The sibling runtime's vocabulary resolves too, or its command lines would not run here.
    assert_eq!(
        wasmrt(&["--features", "all,-wide_arithmetic", &m, "add", "2", "3"])
            .status
            .code(),
        Some(0)
    );
}

/// ⚠️ The `--dir` separator was a live swappability break: wazmrt spells it `:`, wasmrt `::`.
/// Both are accepted, and a Windows drive letter is not mistaken for a separator.
#[test]
fn both_preopen_separators_are_accepted() {
    let cmd = write("sep.wat", COMMAND);
    let d = std::env::temp_dir().join("wasmrt_run_modes");
    let host = d.to_str().unwrap().to_string();
    for spec in [format!("{host}:/guest"), format!("{host}::/guest"), host.clone()] {
        let out = wasmrt(&["wasi", "--dir", &spec, &cmd]);
        assert_eq!(out.status.code(), Some(0), "{spec}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn the_ceiling_and_env_flags_are_accepted_in_both_positions() {
    let cmd = write("ceil.wat", COMMAND);
    for args in [
        std::vec!["wasi", "--max-memory", "512M", "--env", "K=V", &cmd],
        std::vec!["wasi", &cmd, "--max-memory", "512M", "--env", "K=V"],
    ] {
        let out = wasmrt(&args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    // A size that is not a size is an error, not a default.
    let bad = wasmrt(&["wasi", "--max-memory", "lots", &cmd]);
    assert_eq!(bad.status.code(), Some(1));
}
