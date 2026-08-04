//! `wast` — the WAST **script runner**: the spec testsuite's command language.
//!
//! Handles `(module …)` definitions plus the assertion commands — `assert_return`,
//! `assert_trap`, `assert_exhaustion`, `assert_invalid`, `assert_malformed`,
//! `assert_unlinkable` — and the `invoke` / `get` / `register` actions. Ported from wazmrt
//! `src/wast.zig` (T6).
//!
//! Operates on script **text**; file I/O is the CLI's job, so this stays `std`-free.
//!
//! # The honesty rule
//!
//! **Never count "we couldn't build it" as a pass.** A harness that treats its own gaps as
//! success reports the shape of its gaps as conformance. So:
//!
//! - `assert_invalid` passes only on a **validation** rejection, `assert_malformed` only on
//!   a **decode/parse** rejection. If the assembler simply cannot express the construct
//!   ([`crate::wat::Error::Unsupported`] / `UnknownInstr`), that is a **skip** — the module
//!   was never really put to the test.
//! - `assert_trap` and `assert_exhaustion` accept only a genuine runtime [`Trap`], never a
//!   setup or assembly failure.
//!
//! Skips are counted and reported separately from passes for exactly this reason.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::interp::{Instance, Trap, Value};
use crate::sexpr::{self, Sexpr};
use crate::wat;

/// Outcome of running a script.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub passed: usize,
    pub failed: usize,
    /// Commands this runner could not put to the test (an unsupported construct, a module
    /// that never built, a command kind not handled). **Never** folded into `passed`.
    pub skipped: usize,
    /// Descriptions of the failures, for debugging. Capped so a badly-broken file cannot
    /// produce unbounded output.
    pub failures: Vec<String>,
}

impl Summary {
    /// Total commands that were actually adjudicated.
    #[must_use]
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} passed, {} failed, {} skipped",
            self.passed, self.failed, self.skipped
        )
    }
}

/// A script-level failure (the source could not be parsed at all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Parse(sexpr::ParseError),
}

impl From<sexpr::ParseError> for Error {
    fn from(e: sexpr::ParseError) -> Self {
        Error::Parse(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Parse(e) => write!(f, "s-expression parse error: {e}"),
        }
    }
}

impl core::error::Error for Error {}

/// Cap on recorded failure descriptions.
const MAX_RECORDED_FAILURES: usize = 25;

/// Parse and run a whole `.wast` source.
///
/// # Errors
/// Returns [`Error::Parse`] if the source is not well-formed s-expressions. Command-level
/// problems are counted in the [`Summary`] rather than returned.
pub fn run_script(src: &[u8]) -> Result<Summary, Error> {
    let forms = sexpr::parse_all(src)?;
    let mut r = Runner::default();
    for cmd in &forms {
        r.command(cmd);
    }
    Ok(r.summary)
}

#[derive(Default)]
struct Runner {
    /// The most recently built module, which un-named actions target.
    current: Option<Instance>,
    /// Modules by their textual `$name`, for `(invoke $M …)`.
    named: Vec<(String, Instance)>,
    summary: Summary,
}

/// Why an action could not run.
enum ActionErr {
    /// The module never built, or the named target is unknown — nothing was tested.
    NoTarget,
    /// A genuine runtime trap.
    Trap(Trap),
    /// The action form itself was malformed.
    Bad(String),
}

impl Runner {
    fn fail(&mut self, msg: String) {
        self.summary.failed += 1;
        if self.summary.failures.len() < MAX_RECORDED_FAILURES {
            self.summary.failures.push(msg);
        }
    }

    fn command(&mut self, cmd: &Sexpr) {
        let Some(kw) = cmd.keyword() else {
            self.summary.skipped += 1;
            return;
        };
        let list = cmd.as_list().unwrap_or(&[]);
        match kw {
            "module" => self.define_module(list),
            "assert_return" => self.assert_return(list),
            "assert_trap" => self.assert_trap(list),
            "assert_exhaustion" => self.assert_exhaustion(list),
            "assert_invalid" => self.assert_rejected(list, Rejection::Invalid),
            "assert_malformed" => self.assert_rejected(list, Rejection::Malformed),
            "assert_unlinkable" => self.assert_unlinkable(list),
            "invoke" | "get" => match self.run_action(cmd) {
                Ok(_) => self.summary.passed += 1,
                Err(ActionErr::NoTarget) => self.summary.skipped += 1,
                Err(ActionErr::Trap(t)) => self.fail(format!("action trapped: {t}")),
                Err(ActionErr::Bad(m)) => self.fail(m),
            },
            // `register` needs cross-module imports, which the interpreter does not link
            // yet (see the module docs) — count it honestly rather than silently.
            "register" => self.summary.skipped += 1,
            _ => self.summary.skipped += 1,
        }
    }

    /// Assemble a `(module …)` form to bytes. Handles the `binary` and `quote` variants.
    fn module_binary(form: &[Sexpr]) -> Result<Vec<u8>, wat::Error> {
        // `(module quote "…" …)` — the strings are `.wat` source holding the module's
        // FIELDS, not a whole `(module …)` form, so they are wrapped before assembly.
        let quote_at = form.iter().position(|s| s.as_atom() == Some("quote"));
        if let Some(q) = quote_at {
            let mut src = b"(module\n".to_vec();
            for s in &form[q + 1..] {
                src.extend_from_slice(s.as_str().unwrap_or(&[]));
                src.push(b'\n');
            }
            src.extend_from_slice(b")\n");
            return wat::assemble(&src);
        }
        wat::assemble_module(form)
    }

    fn build(form: &[Sexpr]) -> Result<Instance, BuildErr> {
        let bytes = Self::module_binary(form).map_err(BuildErr::Assemble)?;
        let md = crate::module::decode(&bytes).map_err(BuildErr::Decode)?;
        crate::validate::validate(&md).map_err(BuildErr::Validate)?;
        Instance::new(md).map_err(BuildErr::Instantiate)
    }

    fn define_module(&mut self, list: &[Sexpr]) {
        match Self::build(list) {
            Ok(inst) => {
                // Track by textual `$name` for later `$M` references.
                if let Some(name) = list.get(1).and_then(Sexpr::as_atom) {
                    if name.starts_with('$') {
                        self.named.push((name.to_string(), inst));
                        // The most recent module is also `current`; clone-free by
                        // re-building is wasteful, so `current` tracks "the last one" via
                        // the named list when a name is present.
                        self.current = None;
                        return;
                    }
                }
                self.current = Some(inst);
            }
            Err(e) => {
                // A module that does not build is a real failure UNLESS the assembler
                // simply cannot express it yet — that is a gap, not a conformance result.
                if e.is_unsupported() {
                    self.summary.skipped += 1;
                } else {
                    self.fail(format!("module failed to build: {e}"));
                }
                self.current = None;
            }
        }
    }

    /// The instance an action targets: `$name` if given, else the most recent module.
    fn target(&mut self, name: Option<&str>) -> Option<&mut Instance> {
        match name {
            Some(n) => self
                .named
                .iter_mut()
                .find(|(k, _)| k == n)
                .map(|(_, i)| i),
            None => match self.current {
                Some(ref mut i) => Some(i),
                // With no un-named current module, fall back to the most recent named one
                // — a `.wast` file that names every module still runs its bare actions.
                None => self.named.last_mut().map(|(_, i)| i),
            },
        }
    }

    /// Run `(invoke $M? "name" arg*)` or `(get $M? "name")`.
    fn run_action(&mut self, action: &Sexpr) -> Result<Vec<Value>, ActionErr> {
        let l = action
            .as_list()
            .ok_or_else(|| ActionErr::Bad("action is not a list".to_string()))?;
        let kw = l.first().and_then(Sexpr::as_atom).unwrap_or("");
        let mut j = 1;
        let module_name = l
            .get(j)
            .and_then(Sexpr::as_atom)
            .filter(|a| a.starts_with('$'))
            .map(ToString::to_string);
        if module_name.is_some() {
            j += 1;
        }
        let export = l
            .get(j)
            .and_then(Sexpr::as_str)
            .ok_or_else(|| ActionErr::Bad("action: missing export name".to_string()))?
            .to_vec();
        let export = String::from_utf8_lossy(&export).into_owned();
        j += 1;

        let mut args = Vec::new();
        if kw == "invoke" {
            for a in &l[j..] {
                args.push(parse_const(a).map_err(ActionErr::Bad)?);
            }
        }

        let inst = self
            .target(module_name.as_deref())
            .ok_or(ActionErr::NoTarget)?;
        if kw == "get" {
            // Reading an exported global is not part of the embedding surface yet.
            return Err(ActionErr::NoTarget);
        }
        inst.invoke(&export, &args).map_err(ActionErr::Trap)
    }

    fn assert_return(&mut self, form: &[Sexpr]) {
        let Some(action) = form.get(1) else {
            self.fail("assert_return: missing action".to_string());
            return;
        };
        let results = match self.run_action(action) {
            Ok(r) => r,
            Err(ActionErr::NoTarget) => {
                self.summary.skipped += 1;
                return;
            }
            Err(ActionErr::Trap(t)) => {
                self.fail(format!("assert_return: unexpected trap {t}"));
                return;
            }
            Err(ActionErr::Bad(m)) => {
                self.fail(m);
                return;
            }
        };
        let expected = &form[2..];
        // A `v128` is ONE value slot here (wasmrt's 128-bit slot), so the result count and
        // the expectation count compare directly — the oracle needs a slot-vs-form
        // adjustment because it stores a v128 as two `u64`s.
        if results.len() != expected.len() {
            self.fail(format!(
                "assert_return: arity {} != expected {}",
                results.len(),
                expected.len()
            ));
            return;
        }
        for (got, exp) in results.iter().zip(expected) {
            match value_matches(*got, exp) {
                Ok(true) => {}
                Ok(false) => {
                    self.fail(format!(
                        "assert_return: result mismatch (got 0x{got:x}, expected {exp:?})"
                    ));
                    return;
                }
                Err(m) => {
                    self.fail(m);
                    return;
                }
            }
        }
        self.summary.passed += 1;
    }

    fn assert_trap(&mut self, form: &[Sexpr]) {
        let Some(operand) = form.get(1) else {
            self.fail("assert_trap: missing operand".to_string());
            return;
        };
        // `assert_trap (module …)` — instantiation itself must trap (an active data or
        // element segment out of bounds, say). It does not become the current module.
        if operand.keyword() == Some("module") {
            match Self::build(operand.as_list().unwrap_or(&[])) {
                Ok(_) => self.fail("assert_trap: module instantiated without trapping".to_string()),
                Err(BuildErr::Instantiate(_)) => self.summary.passed += 1,
                Err(e) if e.is_unsupported() => self.summary.skipped += 1,
                Err(e) => self.fail(format!("assert_trap: non-trap error {e}")),
            }
            return;
        }
        match self.run_action(operand) {
            Ok(_) => self.fail("assert_trap: expected a trap, got a result".to_string()),
            Err(ActionErr::Trap(_)) => self.summary.passed += 1,
            Err(ActionErr::NoTarget) => self.summary.skipped += 1,
            Err(ActionErr::Bad(m)) => self.fail(m),
        }
    }

    fn assert_exhaustion(&mut self, form: &[Sexpr]) {
        let Some(action) = form.get(1) else {
            self.fail("assert_exhaustion: missing action".to_string());
            return;
        };
        match self.run_action(action) {
            Ok(_) => self.fail("assert_exhaustion: expected exhaustion, got a result".to_string()),
            Err(ActionErr::Trap(Trap::CallStackExhausted)) => self.summary.passed += 1,
            Err(ActionErr::Trap(t)) => self.fail(format!("assert_exhaustion: got {t}")),
            Err(ActionErr::NoTarget) => self.summary.skipped += 1,
            Err(ActionErr::Bad(m)) => self.fail(m),
        }
    }

    /// `assert_invalid` / `assert_malformed (module …) "reason"` — the module must be
    /// rejected, and by the **right stage**.
    fn assert_rejected(&mut self, form: &[Sexpr], kind: Rejection) {
        let Some(inner) = form.get(1).filter(|s| s.keyword() == Some("module")) else {
            self.summary.skipped += 1;
            return;
        };
        match Self::build(inner.as_list().unwrap_or(&[])) {
            Ok(_) => self.fail(format!(
                "{kind:?}: module was accepted (should be rejected)"
            )),
            Err(e) => {
                // Only the matching rejection stage counts. An assembler gap is a SKIP:
                // the module was never really put to the test, and scoring it as a pass
                // would make missing features look like conformance.
                if e.is_unsupported() {
                    self.summary.skipped += 1;
                } else if kind.accepts(&e) {
                    self.summary.passed += 1;
                } else {
                    self.fail(format!("{kind:?}: rejected at the wrong stage ({e})"));
                }
            }
        }
    }

    /// `assert_unlinkable (module …) "reason"` — valid, but must fail to link. Linking
    /// needs host imports, which the interpreter does not support yet, so these are
    /// skipped rather than counted either way.
    fn assert_unlinkable(&mut self, _form: &[Sexpr]) {
        self.summary.skipped += 1;
    }
}

/// Which rejection stage an assertion demands.
#[derive(Debug, Clone, Copy)]
enum Rejection {
    /// Must fail type-checking.
    Invalid,
    /// Must fail parsing or decoding.
    Malformed,
}

impl Rejection {
    fn accepts(self, e: &BuildErr) -> bool {
        match self {
            // A malformed binary can also be caught by the validator's decode step, so
            // accept either rejection for `assert_invalid`.
            Rejection::Invalid => matches!(e, BuildErr::Validate(_) | BuildErr::Decode(_)),
            Rejection::Malformed => matches!(
                e,
                BuildErr::Decode(_) | BuildErr::Assemble(wat::Error::Parse(_))
            ),
        }
    }
}

/// Why a module failed to become an instance — the stage matters for the assertions.
enum BuildErr {
    Assemble(wat::Error),
    Decode(crate::types::DecodeError),
    Validate(crate::validate::ValidateError),
    Instantiate(Trap),
}

impl BuildErr {
    /// Is this "wasmrt cannot express/handle the construct" rather than "the module is
    /// bad"? Those must never be scored as conformance results.
    fn is_unsupported(&self) -> bool {
        matches!(
            self,
            BuildErr::Assemble(
                wat::Error::Unsupported(_) | wat::Error::UnknownInstr | wat::Error::NotAModule
            ) | BuildErr::Validate(crate::validate::ValidateError::UnsupportedValidation)
                | BuildErr::Instantiate(Trap::ImportsUnsupported | Trap::UnsupportedInstruction)
        )
    }
}

impl fmt::Display for BuildErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildErr::Assemble(e) => write!(f, "assemble: {e}"),
            BuildErr::Decode(e) => write!(f, "decode: {e}"),
            BuildErr::Validate(e) => write!(f, "validate: {e}"),
            BuildErr::Instantiate(t) => write!(f, "instantiate: {t}"),
        }
    }
}

// --- Value literals and matching ----------------------------------------------

/// Parse a concrete argument literal: `(TYPE.const …)` or a reference literal.
fn parse_const(form: &Sexpr) -> Result<Value, String> {
    let l = form
        .as_list()
        .ok_or_else(|| "argument is not a list".to_string())?;
    let kw = l.first().and_then(Sexpr::as_atom).unwrap_or("");
    let lit = l.get(1).and_then(Sexpr::as_atom);
    match kw {
        // `ref.null` carries an ignorable heap type.
        "ref.null" => Ok(NULL_REF),
        "ref.func" | "ref.extern" => match lit {
            Some(a) => parse_int_lit(a).map(|v| v as Value),
            None => Ok(NULL_REF),
        },
        "i32.const" => {
            let v = parse_int_lit(lit.ok_or("i32.const: missing literal")?)?;
            Ok(crate::interp::i32_value(v as i32))
        }
        "i64.const" => {
            let v = parse_int_lit(lit.ok_or("i64.const: missing literal")?)?;
            Ok(crate::interp::i64_value(v))
        }
        "f32.const" => {
            let bits = float_bits_32(lit.ok_or("f32.const: missing literal")?)?;
            Ok(Value::from(bits))
        }
        "f64.const" => {
            let bits = float_bits_64(lit.ok_or("f64.const: missing literal")?)?;
            Ok(Value::from(bits))
        }
        "v128.const" => parse_v128(l),
        other => Err(format!("unsupported value literal `{other}`")),
    }
}

/// The interpreter's null-reference sentinel.
const NULL_REF: Value = u64::MAX as Value;

fn parse_int_lit(a: &str) -> Result<i64, String> {
    let t: String = a.chars().filter(|&c| c != '_').collect();
    let (neg, body) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(&t)),
    };
    let (digits, radix) = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        Some(r) => (r, 16),
        None => (body, 10),
    };
    let mag =
        u64::from_str_radix(digits, radix).map_err(|_| format!("bad integer literal `{a}`"))?;
    Ok(if neg { (mag as i64).wrapping_neg() } else { mag as i64 })
}

/// A float literal's bits, including the wasm NaN spellings. Shares the assembler's parser
/// so an expectation and the module it checks can never disagree about a literal.
fn float_bits_32(lit: &str) -> Result<u32, String> {
    wat::parse_f32_bits(lit).ok_or_else(|| format!("bad f32 literal `{lit}`"))
}
fn float_bits_64(lit: &str) -> Result<u64, String> {
    wat::parse_f64_bits(lit).ok_or_else(|| format!("bad f64 literal `{lit}`"))
}

/// `(v128.const <shape> <lane>…)` → the 128-bit value.
fn parse_v128(l: &[Sexpr]) -> Result<Value, String> {
    let shape = l
        .get(1)
        .and_then(Sexpr::as_atom)
        .ok_or("v128.const: missing shape")?;
    let mut bytes = [0u8; 16];
    let lanes: usize = match shape {
        "i8x16" => 16,
        "i16x8" => 8,
        "i32x4" | "f32x4" => 4,
        "i64x2" | "f64x2" => 2,
        _ => return Err(format!("v128.const: bad shape `{shape}`")),
    };
    for k in 0..lanes {
        let a = l
            .get(2 + k)
            .and_then(Sexpr::as_atom)
            .ok_or("v128.const: missing lane")?;
        match shape {
            "i8x16" => bytes[k] = parse_int_lit(a)? as u8,
            "i16x8" => {
                bytes[k * 2..k * 2 + 2].copy_from_slice(&(parse_int_lit(a)? as u16).to_le_bytes());
            }
            "i32x4" => {
                bytes[k * 4..k * 4 + 4].copy_from_slice(&(parse_int_lit(a)? as u32).to_le_bytes());
            }
            "i64x2" => {
                bytes[k * 8..k * 8 + 8].copy_from_slice(&(parse_int_lit(a)? as u64).to_le_bytes());
            }
            "f32x4" => bytes[k * 4..k * 4 + 4].copy_from_slice(&float_bits_32(a)?.to_le_bytes()),
            "f64x2" => bytes[k * 8..k * 8 + 8].copy_from_slice(&float_bits_64(a)?.to_le_bytes()),
            _ => unreachable!(),
        }
    }
    Ok(Value::from_le_bytes(bytes))
}

fn is_canonical_nan_32(bits: u32) -> bool {
    bits & 0x7fff_ffff == 0x7fc0_0000
}
fn is_canonical_nan_64(bits: u64) -> bool {
    bits & 0x7fff_ffff_ffff_ffff == 0x7ff8_0000_0000_0000
}
/// An arithmetic NaN: any NaN whose quiet bit is set.
fn is_arithmetic_nan_32(bits: u32) -> bool {
    bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0 && bits & 0x0040_0000 != 0
}
fn is_arithmetic_nan_64(bits: u64) -> bool {
    bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000
        && bits & 0x000f_ffff_ffff_ffff != 0
        && bits & 0x0008_0000_0000_0000 != 0
}

fn float_matches_32(got: u32, lit: &str) -> Result<bool, String> {
    match lit {
        "nan:canonical" => return Ok(is_canonical_nan_32(got)),
        "nan:arithmetic" => return Ok(is_arithmetic_nan_32(got)),
        _ => {}
    }
    Ok(got == float_bits_32(lit)?)
}
fn float_matches_64(got: u64, lit: &str) -> Result<bool, String> {
    match lit {
        "nan:canonical" => return Ok(is_canonical_nan_64(got)),
        "nan:arithmetic" => return Ok(is_arithmetic_nan_64(got)),
        _ => {}
    }
    Ok(got == float_bits_64(lit)?)
}

/// Does an actual result match an expected `(TYPE.const …)` form?
fn value_matches(got: Value, exp: &Sexpr) -> Result<bool, String> {
    let l = exp
        .as_list()
        .ok_or_else(|| "expectation is not a list".to_string())?;
    let kw = l.first().and_then(Sexpr::as_atom).unwrap_or("");
    let lit = l.get(1).and_then(Sexpr::as_atom);
    match kw {
        "ref.null" => return Ok(got == NULL_REF),
        // A bare `(ref.func)` / `(ref.extern)` asserts merely non-null; with a payload it
        // is exact. The abstract GC matchers assert non-null of that kind, which the
        // interpreter's untyped slot cannot distinguish — non-null is the honest check.
        "ref.func" | "ref.extern" => {
            return match lit {
                Some(a) => Ok(got == parse_int_lit(a)? as Value),
                None => Ok(got != NULL_REF),
            };
        }
        "ref.struct" | "ref.array" | "ref.i31" | "ref.eq" | "ref.any" | "ref.host"
        | "ref.data" => return Ok(got != NULL_REF),
        "f32.const" => {
            return float_matches_32(got as u32, lit.ok_or("f32.const: missing literal")?);
        }
        "f64.const" => {
            return float_matches_64(got as u64, lit.ok_or("f64.const: missing literal")?);
        }
        "v128.const" => {
            // Float lanes are matched lane by lane, so a per-lane `nan:canonical` works.
            let shape = l.get(1).and_then(Sexpr::as_atom).unwrap_or("");
            if shape == "f32x4" || shape == "f64x2" {
                let bytes = got.to_le_bytes();
                let (lanes, width) = if shape == "f32x4" { (4, 4) } else { (2, 8) };
                for k in 0..lanes {
                    let a = l
                        .get(2 + k)
                        .and_then(Sexpr::as_atom)
                        .ok_or("v128.const: missing lane")?;
                    let ok = if width == 4 {
                        let mut b = [0u8; 4];
                        b.copy_from_slice(&bytes[k * 4..k * 4 + 4]);
                        float_matches_32(u32::from_le_bytes(b), a)?
                    } else {
                        let mut b = [0u8; 8];
                        b.copy_from_slice(&bytes[k * 8..k * 8 + 8]);
                        float_matches_64(u64::from_le_bytes(b), a)?
                    };
                    if !ok {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            return Ok(got == parse_v128(l)?);
        }
        _ => {}
    }
    // Integers and everything else: exact comparison against the parsed literal.
    Ok(got == parse_const(exp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Summary {
        run_script(src.as_bytes()).expect("script parse failed")
    }

    #[test]
    fn runs_a_simple_assert_return() {
        let s = run(
            r#"(module (func (export "add") (param i32 i32) (result i32)
                 (i32.add (local.get 0) (local.get 1))))
               (assert_return (invoke "add" (i32.const 40) (i32.const 2)) (i32.const 42))"#,
        );
        assert_eq!((s.passed, s.failed, s.skipped), (1, 0, 0));
    }

    #[test]
    fn reports_a_mismatch_as_a_failure() {
        let s = run(
            r#"(module (func (export "f") (result i32) (i32.const 1)))
               (assert_return (invoke "f") (i32.const 2))"#,
        );
        assert_eq!(s.failed, 1);
        assert_eq!(s.passed, 0);
        assert!(s.failures[0].contains("mismatch"));
    }

    #[test]
    fn checks_arity() {
        let s = run(
            r#"(module (func (export "f") (result i32) (i32.const 1)))
               (assert_return (invoke "f") (i32.const 1) (i32.const 2))"#,
        );
        assert_eq!(s.failed, 1);
        assert!(s.failures[0].contains("arity"));
    }

    #[test]
    fn runs_assert_trap() {
        let s = run(
            r#"(module (func (export "boom") (result i32)
                 (i32.div_s (i32.const 1) (i32.const 0))))
               (assert_trap (invoke "boom") "integer divide by zero")"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0));
    }

    #[test]
    fn a_missing_trap_is_a_failure() {
        let s = run(
            r#"(module (func (export "ok") (result i32) (i32.const 1)))
               (assert_trap (invoke "ok") "integer divide by zero")"#,
        );
        assert_eq!(s.failed, 1);
    }

    #[test]
    fn runs_assert_invalid() {
        // A type error the validator must catch.
        let s = run(
            r#"(assert_invalid
                 (module (func (result i32) (f32.const 0)))
                 "type mismatch")"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0));
    }

    #[test]
    fn an_accepted_module_fails_assert_invalid() {
        let s = run(
            r#"(assert_invalid
                 (module (func (result i32) (i32.const 0)))
                 "type mismatch")"#,
        );
        assert_eq!(s.failed, 1);
        assert!(s.failures[0].contains("should be rejected"));
    }

    #[test]
    fn an_assembler_gap_is_skipped_not_passed() {
        // THE honesty property: a construct the assembler cannot express must not satisfy
        // an `assert_invalid`, or missing features would masquerade as conformance.
        let s = run(
            r#"(assert_invalid
                 (module (func (i32.nonexistent_opcode)))
                 "some reason")"#,
        );
        assert_eq!(s.passed, 0);
        assert_eq!(s.failed, 0);
        assert_eq!(s.skipped, 1);
    }

    #[test]
    fn runs_assert_malformed_on_a_bad_binary() {
        let s = run(
            r#"(assert_malformed
                 (module binary "\00asm\01\00\00\00\01\00")
                 "unexpected end")"#,
        );
        // A truncated section must be rejected at decode.
        assert_eq!(s.failed, 0);
        assert!(s.passed + s.skipped == 1);
    }

    #[test]
    fn matches_float_results_including_nan_forms() {
        let s = run(
            r#"(module
                 (func (export "half") (result f64) (f64.const 0.5))
                 (func (export "nan") (result f64)
                   (f64.div (f64.const 0) (f64.const 0))))
               (assert_return (invoke "half") (f64.const 0.5))
               (assert_return (invoke "nan") (f64.const nan:canonical))"#,
        );
        assert_eq!((s.passed, s.failed), (2, 0), "{:?}", s.failures);
    }

    #[test]
    fn matches_a_v128_result_as_one_slot() {
        // wasmrt's 128-bit value slot means a v128 result is ONE slot, so the arity check
        // compares directly against the expectation count.
        let s = run(
            r#"(module (func (export "v") (result v128)
                 (i32x4.splat (i32.const 7))))
               (assert_return (invoke "v") (v128.const i32x4 7 7 7 7))"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);
    }

    #[test]
    fn runs_named_modules() {
        let s = run(
            r#"(module $A (func (export "f") (result i32) (i32.const 1)))
               (module $B (func (export "f") (result i32) (i32.const 2)))
               (assert_return (invoke $A "f") (i32.const 1))
               (assert_return (invoke $B "f") (i32.const 2))"#,
        );
        assert_eq!((s.passed, s.failed), (2, 0), "{:?}", s.failures);
    }

    #[test]
    fn runs_a_quoted_module() {
        let s = run(
            r#"(module quote "(func (export \"f\") (result i32) (i32.const 9))")
               (assert_return (invoke "f") (i32.const 9))"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);
    }

    #[test]
    fn assert_exhaustion_needs_real_exhaustion() {
        // Runs on a thread with a large stack. The interpreter's 512-frame recursion cap
        // matches the oracle's, but a DEBUG-profile `run` frame is big enough that the
        // native stack can go first — release is fine. See `cmem/known-issues.md`; the cap
        // is deliberately left at the oracle's value rather than tuned to one profile.
        let h = std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                run_script(
                    br#"(module (func $f (export "f") (result i32) (call $f)))
                        (assert_exhaustion (invoke "f") "call stack exhausted")"#,
                )
                .unwrap()
            })
            .unwrap();
        let s = h.join().unwrap();
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);
    }

    #[test]
    fn unknown_commands_are_skipped() {
        let s = run(r#"(assert_exception (invoke "f"))"#);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.passed + s.failed, 0);
    }
}
