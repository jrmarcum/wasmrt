//! **A host flag must never be silently donated to the guest.**
//!
//! An integration test because the defect is in argument handling, which no core-level test can
//! reach. `wasmrt wasi <module> --dir X` used to pass `--dir X` straight to the guest: the preopen
//! was **never granted**, every path call returned `BADF`, and **nothing said so** — it read as a
//! guest bug rather than a missing grant.
//!
//! It surfaced as a swappability break (`cmem/interop.md` §2.2 F3): wazmrt takes host flags AFTER
//! the module path and wasmrt took them only before, so a wazmrt-shaped command line ran here
//! unsandboxed.
//!
//! ⚠️⚠️ **Why it is a blocker and not a papercut.** Today the only such flags are preopens, so a
//! misplacement fails **closed** — the guest gets *less* access. The moment T9e adds
//! `--verify`/`--pins` or T9i adds `--max-iterations`, the same slip fails **OPEN**: the user asks
//! for a restriction, sees no error, and runs without it. This test exists so the parser is fixed
//! *before* the flags that make it dangerous arrive.

use std::process::Command;

fn wasmrt() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wasmrt"))
}

fn fixture() -> std::path::PathBuf {
    let p = std::env::temp_dir().join("wasmrt_flagpos.wat");
    std::fs::write(&p, br#"(module (func (export "f")))"#).expect("write fixture");
    p
}

/// A directory that does not exist, so a *granted* preopen fails loudly and an *ignored* one
/// says nothing. The distinction is the whole point: it makes "was the flag applied?" observable
/// without needing a guest that touches the filesystem.
fn missing_dir() -> String {
    std::env::temp_dir()
        .join("wasmrt_no_such_dir_flagpos")
        .to_string_lossy()
        .into_owned()
}

fn run(args: &[&str]) -> (bool, String) {
    let out = wasmrt().args(args).output().expect("run wasmrt");
    let text = String::from_utf8_lossy(&out.stderr).into_owned()
        + &String::from_utf8_lossy(&out.stdout);
    (out.status.success(), text)
}

/// ⚠️ The regression: this used to succeed in ignoring the flag entirely.
#[test]
fn a_trailing_preopen_flag_is_applied_not_donated_to_the_guest() {
    let m = fixture();
    let (_, after) = run(&["wasi", &m.to_string_lossy(), "--dir", &missing_dir()]);
    assert!(
        after.contains("cannot preopen"),
        "a trailing --dir must be APPLIED (wazmrt's spelling); got: {after}"
    );
}

/// The two spellings must be indistinguishable — that is what "swappable" means for a flag.
#[test]
fn both_flag_positions_behave_identically() {
    let m = fixture();
    let d = missing_dir();
    let (_, before) = run(&["wasi", "--dir", &d, &m.to_string_lossy()]);
    let (_, after) = run(&["wasi", &m.to_string_lossy(), "--dir", &d]);
    assert_eq!(
        before.contains("cannot preopen"),
        after.contains("cannot preopen"),
        "leading and trailing --dir must do the same thing\nbefore: {before}\nafter: {after}"
    );
}

/// ⚠️ An unrecognised leading `--flag` must be an ERROR, not the module path. It used to be read
/// as a filename, producing `cannot read '--typo'` — and, worse, letting a misplaced restriction
/// flag disappear without a word.
#[test]
fn an_unknown_leading_option_is_refused_not_treated_as_a_path() {
    let (ok, text) = run(&["wasi", "--typo", "x.wasm"]);
    assert!(!ok, "an unknown option must fail");
    assert!(
        text.contains("unknown option"),
        "must name it as an unknown OPTION, not as an unreadable file; got: {text}"
    );
}

/// A host flag stranded in the guest's argv **warns** — and does **not** refuse, because a guest
/// may legitimately take `--dir` as its own argument. Matches wazmrt's H7.
#[test]
fn a_host_flag_stranded_in_guest_argv_warns() {
    let m = fixture();
    let (_, text) = run(&["wasi", &m.to_string_lossy(), "somearg", "--dir", &missing_dir()]);
    assert!(
        text.contains("passed to the GUEST"),
        "a stranded host flag must warn; got: {text}"
    );
}

/// 🔒 …but an **explicit `--`** silences it. That marker is the user saying "the rest is the
/// guest's", and second-guessing it would make `--` useless.
///
/// ⚠️ This case failed on the first implementation: the `--` was stripped *before* the warning
/// scan, so the scan never saw its own stop marker. Found by probing the five cases, not by
/// reading the code — which is why the inverse is pinned here beside the positive.
#[test]
fn an_explicit_double_dash_silences_the_warning() {
    let m = fixture();
    let (_, text) = run(&["wasi", &m.to_string_lossy(), "--", "--dir", &missing_dir()]);
    assert!(
        !text.contains("passed to the GUEST"),
        "after `--` the flag is deliberately the guest's; must not warn. got: {text}"
    );
}
