//! 🔒 **The pin gate** (T9e; `cmem/security-model.md`, `cmem/interop.md` §3).
//!
//! The DB format, the mode ladder, the `decide` precedence and the fail-closed rules are a
//! **compatibility surface**: one root-owned DB governs wasmrt and wazmrt alike, so these
//! assertions are contract, not preference.
//!
//! Every test drives the gate through `--pins`, because the default DB lives at a system path
//! (`/etc/wasmtk/pins`, `C:\ProgramData\wasmtk\pins`) that a test must never write to.

use std::process::{Command, Output};

fn dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join("wasmrt_pin_gate");
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

fn digest_of(path: &str) -> String {
    let out = wasmrt(&["pin", path]);
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().next().expect("a digest").to_string()
}

const MODULE: &[u8] = br#"(module (func (export "f") (result i32) (i32.const 7)))"#;
const OTHER: &[u8] = br#"(module (func (export "f") (result i32) (i32.const 8)))"#;

#[test]
fn unarmed_runs_everything_so_the_gate_costs_nothing_when_unused() {
    let m = write("unarmed.wat", MODULE);
    let empty = write("empty_db_absent.txt", b"");
    // No DB at all → nothing to verify against (`decide` row 7).
    let _ = std::fs::remove_file(&empty);
    assert_eq!(wasmrt(&["run", &m, "f"]).status.code(), Some(0));
}

#[test]
fn armed_runs_a_pinned_module_and_refuses_an_unpinned_one() {
    let m = write("pinned.wat", MODULE);
    let other = write("unpinned.wat", OTHER);
    let db = write("db_one.txt", format!("{}  pinned\n", digest_of(&m)).as_bytes());

    assert_eq!(wasmrt(&["run", "--pins", &db, &m, "f"]).status.code(), Some(0));
    let out = wasmrt(&["run", "--pins", &db, &other, "f"]);
    assert_eq!(out.status.code(), Some(1), "an unpinned module must be refused");
    let err = String::from_utf8_lossy(&out.stderr);
    // A denial names what was refused, its digest and the DB — an operator has to be able to act.
    assert!(err.contains("refusing to run"), "{err}");
    assert!(err.contains(&digest_of(&other)), "the digest must be printed: {err}");
    assert!(err.contains(&db), "the DB path must be printed: {err}");
}

/// ⚠️ The `.wat` digest is of the **ASSEMBLED** bytes, or a pinned `.wat` would never match.
#[test]
fn a_wat_is_pinned_by_its_assembled_bytes() {
    let text = write("assembled.wat", MODULE);
    let mut binary = dir().join("assembled.wasm");
    assert!(wasmrt(&["wat", &text, "-o", binary.to_str().unwrap()]).status.success());
    binary = binary.canonicalize().unwrap_or(binary);
    assert_eq!(
        digest_of(&text),
        digest_of(binary.to_str().unwrap()),
        "text and its assembled binary must pin to the same digest"
    );
}

/// 🔒 `enforce` is ABSOLUTE: a runtime argument cannot lower a root-owned policy.
#[test]
fn enforce_cannot_be_overridden_by_no_verify() {
    let m = write("enforced.wat", OTHER);
    let db = write("db_enforce.txt", b"# mode: enforce\n");
    for flag in ["--no-verify", "--yes"] {
        let out = wasmrt(&["run", "--pins", &db, flag, &m, "f"]);
        assert_eq!(out.status.code(), Some(1), "{flag} must not override enforce");
    }
    // …while the same module under a no-policy DB is a plain armed deny, which the opt-out MAY
    // override — otherwise the test above would pass for the wrong reason.
    let plain = write("db_plain.txt", b"# just a comment\n");
    assert_eq!(
        wasmrt(&["run", "--pins", &plain, "--no-verify", &m, "f"]).status.code(),
        Some(0)
    );
}

/// ⚠️ A mistyped mode is `enforce`, never "no policy" — it must not be downgradable by a typo.
#[test]
fn a_mistyped_mode_directive_still_enforces() {
    let m = write("typo_mode.wat", OTHER);
    let db = write("db_typo.txt", b"# mode: enfroce\n");
    assert_eq!(
        wasmrt(&["run", "--pins", &db, "--no-verify", &m, "f"]).status.code(),
        Some(1),
        "a typo in the policy must fail CLOSED"
    );
}

/// ⚠️ A malformed DB fails LOUD: silently dropping approvals makes a pinned module look unpinned.
#[test]
fn a_malformed_db_is_refused_rather_than_read_as_empty() {
    let m = write("malformed_case.wat", MODULE);
    let db = write("db_malformed.txt", b"deadbeef\n");
    let out = wasmrt(&["run", "--pins", &db, &m, "f"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("malformed"));
}

#[test]
fn a_mistyped_verify_flag_is_an_error_not_a_default() {
    let m = write("verify_typo.wat", MODULE);
    let out = wasmrt(&["run", "--verify", "enfroce", &m, "f"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("expected off, warn or enforce"));
}

/// ⚠️⚠️ A `.wast` script EXECUTES the modules it carries — including `(module binary "…")` — so it
/// is gated like any other execute path. This is the bypass that shipped in the sibling runtime.
#[test]
fn a_wast_script_is_gated_and_summarize_is_not() {
    let script = write(
        "gated.wast",
        br#"(module (func (export "f") (result i32) (i32.const 1)))
            (assert_return (invoke "f") (i32.const 1))"#,
    );
    let db = write("db_other.txt", b"# mode: warn\n");
    assert_eq!(
        wasmrt(&["wast", "--pins", &db, &script]).status.code(),
        Some(1),
        "an unpinned script must not run"
    );
    // Pinning the SCRIPT authorizes exactly what it can execute.
    let db_ok = write("db_script.txt", format!("{}\n", digest_of(&script)).as_bytes());
    assert_eq!(wasmrt(&["wast", "--pins", &db_ok, &script]).status.code(), Some(0));

    // 🔒 Inspection is never gated: summarize executes nothing, and an operator needs it to
    // identify (and then pin) an unknown module.
    let m = write("summarize_me.wat", MODULE);
    assert_eq!(wasmrt(&["--pins", &db, &m]).status.code(), Some(0));
}
