//! `wat` — the WebAssembly **text format** assembler: `.wat` source → a `.wasm` binary,
//! reusing the [`crate::opcode`] table in reverse.
//!
//! Ported from wazmrt `src/wat.zig` (T6). It consumes the [`crate::sexpr`] tree and emits
//! the binary encoding the rest of the crate already reads, so the round trip
//! text → binary → [`crate::module::decode`] → [`crate::validate`] → [`crate::interp`] is
//! closed and self-checking — the assembler's own tests run what they assemble.
//!
//! Assembly is multi-pass, because the text format lets names point forward:
//!
//! 1. **Type names** are collected before any body is read — a concrete `(ref $t)` in a
//!    param, field, or result may name a type declared later (a `(rec …)` group routinely
//!    does). Type *bodies* are parsed in a second pass, once every name resolves.
//! 2. **Definitions** in source order, filling the per-kind index spaces. Imports must
//!    precede definitions of the same kind (§6.6.13); an import after a definition is
//!    rejected rather than silently mis-indexed.
//! 3. **Module-level `(export …)` forms** last: an export may name something declared
//!    further down the file, which is exactly what binaryen emits.
//!
//! Every index space carries a parallel name table (`Vec<Option<String>>`) so a `$name`
//! resolves to the index the binary uses. Imported entries take the low indices, so those
//! tables span imports *and* definitions.
//!
//! Bar to hold: the frozen oracle's assembler has **no gaps** — every construct across
//! every proposal assembles. Where this port does not yet cover a construct it returns a
//! hard [`Error::Unsupported`]; emitting wrong bytes on a fall-through is the worst
//! possible failure mode, so there is no silent default anywhere.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::sexpr::{self, Annot, Sexpr};
use crate::types::ValType;

mod annot;

/// The custom section a branch hint lands in (the branch-hinting proposal).
pub const BRANCH_HINT_SECTION: &[u8] = b"metadata.code.branch_hint";

type V = ValType;

/// Assembly failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The source parsed but held no `(module …)` form.
    NotAModule,
    Parse(sexpr::ParseError),
    BadModuleField,
    /// An `(import …)` after a definition (§6.6.13).
    ImportAfterDefinition,
    BadValType,
    BadImmediate,
    /// A `$name` no index space defines.
    UnknownIdentifier,
    /// Two entries in one index space share a `$name` (§6.3.5). Checked once over the finished
    /// name vectors, because a space is filled by **both** imports and definitions and the rule
    /// spans them — so no single writer can see the conflict.
    DuplicateName,
    /// A mnemonic that is **not an instruction in any WebAssembly proposal** — so the input is
    /// malformed, and saying so is a verdict wasmrt is entitled to give.
    ///
    /// ⚠️⚠️ **This used to mean two different things, and that cost ~300 conformance
    /// assertions.** It covered both "no such instruction exists" *and* "an instruction from a
    /// proposal wasmrt does not implement" — and because the `.wast` runner cannot tell those
    /// apart, it had to score **every** instance as *our* gap. So `load.wast` asserting that
    /// `i32.load32` is malformed — which it is; that mnemonic exists nowhere — got a **skip**
    /// instead of a pass. *An error name that conflates "your input is bad" with "we are
    /// incomplete" cannot be scored correctly by any caller.* The split is
    /// [`Error::UnimplementedInstr`], and it lives here because this is the only place that
    /// knows which of the two it is.
    UnknownInstr,
    /// A real instruction from a proposal **wasmrt has not implemented**. Our gap, never a
    /// verdict about the input — the `.wast` runner must score it as a skip.
    ///
    /// 🔒 **The list behind this must be OVER-INCLUSIVE.** Misfiling a real instruction as
    /// [`Error::UnknownInstr`] turns an `assert_malformed` into a **false pass**, and a false
    /// pass is the one direction that cannot be noticed afterwards. Misfiling in this direction
    /// only costs a skip.
    UnimplementedInstr,
    /// A **pre-standard type keyword** that a proposal renamed — the payload is the modern
    /// spelling, so the message can name it.
    ///
    /// ⚠️⚠️ Its own variant rather than [`Error::BadValType`], and the reason is a trap the sibling
    /// runtime hit first and recorded (`wazmrt` closed the same deviation on 2026-08-17). Dropping
    /// `anyfunc` from the type table is only half a refusal: the token then falls out of the
    /// *routing* predicate too, and lands wherever the parser looks next. Here that was the
    /// funcidx path — `(elem (i32.const 0) anyfunc …)` and `(ref.null anyfunc)` both came back as
    /// **`BadNumber`**, because `anyfunc` was being read as an INDEX. wazmrt's version of the same
    /// slip produced `UnknownIdentifier`, which its `.wast` runner banks as *its own* limitation,
    /// so the deviation came back disguised as a SKIP and the baseline went green for the wrong
    /// reason.
    ///
    /// 🔒 **So the routing predicate ([`is_type_keyword`]) still answers TRUE for an obsolete
    /// keyword** — the token must reach the code that knows *why* it is refused. A rejection routed
    /// to a parser that does not recognise it lands in the wrong bucket, and the bucket is what the
    /// score reads. The input is legacy, not nonsense, and the message says so.
    ObsoleteKeyword(&'static str),
    /// A form of the wrong shape (an atom where a list was required, etc.).
    BadForm,
    /// A numeric literal that does not parse, or does not fit its type.
    BadNumber,
    /// A branch naming a label that is not in scope.
    UnknownLabel,
    /// Nesting beyond the assembler's control-depth cap.
    NestingTooDeep,
    /// A clause in the wrong place. The text format fixes the order of a **type use**
    /// (§6.4.4) — `(type x)?` then `(param …)*` then `(result …)*` — and any other order is
    /// malformed *text*, not an ill-typed module.
    ///
    /// Its own variant because the stage matters: the spec suite asserts these with
    /// `assert_malformed`, and the assembler accepting them meant the **validator** reported
    /// them instead, as a stack-height mismatch on a module that should never have assembled.
    UnexpectedToken,
    /// A second `(start …)` field. A module has at most one start function (§6.6.12); the
    /// assembler used to keep the LAST one silently, so `(start $a) (start $b)` built a module
    /// that ran only `$b` — a module the text did not describe, and one wasmtime refuses.
    MultipleStart,
    /// A type use's `(type x)` and its explicit `(param …)`/`(result …)` clauses disagree (§6.4.4:
    /// they must describe the same function type) — wasm-tools' *"inline function type doesn't
    /// match type reference"*.
    ///
    /// Its own variant because the old one, [`Error::UnexpectedToken`], named the wrong cause: the
    /// tokens are all in the right place, and it is their MEANING that conflicts. (Before
    /// 2026-09-19 an IMPORT with this defect was not refused at all, and surfaced 32 KB later as a
    /// `StackUnderflow` in an unrelated function.)
    TypeUseMismatch,
    /// `(pagesize N)` with `N` zero or not a power of two — malformed text, "invalid custom page
    /// size" (custom-page-sizes). A power of two the proposal does not allow is the validator's.
    InvalidPageSize,
    /// A malformed or misplaced CUSTOM annotation — `@custom`, `@name`, or a branch hint. The
    /// payload is the reason, worded as the spec suite words it. Its own variant because the
    /// stage is the verdict: `assert_malformed_custom` is satisfied by this and nothing else.
    ///
    /// ⚠️ These annotations were discarded as trivia until 2026-09-19, so every one of them —
    /// malformed or not — was accepted, where wasm-tools refuses the module.
    Annotation(&'static str),
    /// A text construct this release does not assemble yet. Loud by design.
    Unsupported(&'static str),
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
            Error::Annotation(why) => write!(f, "annotation: {why}"),
            Error::InvalidPageSize => write!(f, "invalid custom page size"),
            Error::TypeUseMismatch => {
                write!(f, "inline function type doesn't match type reference")
            }
            Error::Unsupported(what) => write!(f, "unsupported text construct: {what}"),
            // The whole point of a dedicated variant: the input is LEGACY, not nonsense, so the
            // message keeps the compatibility hint even though the acceptance does not.
            Error::ObsoleteKeyword(modern) => {
                write!(f, "obsolete keyword — this type is spelled `{modern}` now")
            }
            other => write!(f, "wat error: {other:?}"),
        }
    }
}

impl core::error::Error for Error {}

type Result<T> = core::result::Result<T, Error>;

// --- LEB128 / primitive writers ----------------------------------------------

fn uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn sleb(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7; // arithmetic shift, so the sign propagates
        let sign_set = b & 0x40 != 0;
        if (v == 0 && !sign_set) || (v == -1 && sign_set) {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn name_bytes(out: &mut Vec<u8>, name: &[u8]) {
    uleb(out, name.len() as u64);
    out.extend_from_slice(name);
}

/// Emit a value type (§5.3.5).
///
/// Three shapes: a concrete `(ref null? $t)` is `0x63`/`0x64` + a signed type index; a **non-null
/// abstract** reference is `0x64` + the head's `absheaptype` byte; everything else — the numeric
/// types and the nullable abstract shorthands — is one byte.
///
/// ⚠️⚠️ **The middle case wrote an INTERNAL TAG into the binary.** [`ValType`] keeps non-null
/// abstract references as synthetic bytes in an unused range (`0x57`–`0x68`), and this function
/// pushed `v.bits()` straight out — so `(ref any)` was emitted as `0x66`, `(ref func)` as `0x68`,
/// `(ref exn)` as `0x57`. Our own decoder read them back, so every round-trip test passed and the
/// spec suite was green, but **wasmtime 47 refuses the output**: `invalid value type (at offset
/// 0xd)`. No module wasmrt assembled with a non-null abstract reference type was WebAssembly.
///
/// 🎓 *An internal representation that resembles the wire format will eventually be mistaken for
/// it.* The nullable shorthands genuinely ARE their valtype bytes, which is what made the shortcut
/// look total; the non-null tags were invented, and nothing in the round trip could tell the two
/// apart. §3.8 again: the only reader that can is one that did not write it.
fn emit_val_type(out: &mut Vec<u8>, v: V) -> Result<()> {
    if v.is_exact() {
        // custom-descriptors: `0x63`/`0x64`, the exact prefix `0x62`, then an UNSIGNED type index.
        out.push(if v.is_non_null_ref() { 0x64 } else { 0x63 });
        emit_heap(out, Heap::Exact(v.concrete_index()));
        return Ok(());
    }
    if v.is_concrete() {
        out.push(if v.is_non_null_ref() { 0x64 } else { 0x63 });
        sleb(out, i64::from(v.concrete_index()));
        return Ok(());
    }
    if v.is_non_null_ref() {
        // `(ref ht)`: the prefix, then the head's own `absheaptype` byte — which is exactly the
        // NULLABLE shorthand's byte, since that shorthand is defined as `(ref null ht)`.
        let head = v.ref_heap().val_type(true).bits();
        if head > 0xff {
            return Err(Error::BadValType);
        }
        out.push(0x64);
        out.push(head as u8);
        return Ok(());
    }
    let bits = v.bits();
    if bits > 0xff {
        return Err(Error::BadValType);
    }
    out.push(bits as u8);
    Ok(())
}

fn val_type_vec(out: &mut Vec<u8>, vts: &[V]) -> Result<()> {
    uleb(out, vts.len() as u64);
    for &v in vts {
        emit_val_type(out, v)?;
    }
    Ok(())
}

/// Emit a `limits` (§5.3.7): a flag byte then `min[, max]`.
/// Flag bits: 0 = has max, 1 = shared (threads), 2 = i64 index (memory64).
/// A memory type's binary form: its limits, and — when the text stated a page size, including
/// the default — flag bit 3 and the exponent after them (custom-page-sizes; wasm-tools keeps an
/// explicit `(pagesize 65536)` as flag `0x08` + `16` too, so dropping it would change the module).
fn emit_memory_type(out: &mut Vec<u8>, m: &MemoryDef) {
    let at = out.len();
    emit_limits(out, m.min, m.max, m.shared, m.is64);
    if let Some(e) = m.page_size_log2 {
        out[at] |= 0x08;
        uleb(out, u64::from(e));
    }
}

fn emit_limits(out: &mut Vec<u8>, min: u64, max: Option<u64>, shared: bool, is64: bool) {
    let flag = u8::from(max.is_some()) | (u8::from(shared) << 1) | (u8::from(is64) << 2);
    out.push(flag);
    uleb(out, min);
    if let Some(mx) = max {
        uleb(out, mx);
    }
}

/// Append `content` as a section (id, byte length, payload). An empty section is omitted.
fn push_section(out: &mut Vec<u8>, id: u8, content: &[u8]) {
    if content.is_empty() {
        return;
    }
    out.push(id);
    uleb(out, content.len() as u64);
    out.extend_from_slice(content);
}

// --- Shape-checked accessors --------------------------------------------------

fn want_list(s: &Sexpr) -> Result<&[Sexpr]> {
    s.as_list().ok_or(Error::BadForm)
}
fn want_atom(s: &Sexpr) -> Result<&str> {
    s.as_atom().ok_or(Error::BadForm)
}
fn want_str(s: &Sexpr) -> Result<&[u8]> {
    s.as_str().ok_or(Error::BadForm)
}
fn nth(items: &[Sexpr], i: usize) -> Result<&Sexpr> {
    items.get(i).ok_or(Error::BadForm)
}

/// Is this an identifier atom (`$name`)?
fn is_id(s: &Sexpr) -> bool {
    s.as_atom().is_some_and(|a| a.starts_with('$'))
}
/// Does this atom equal `kw`?
fn eq_atom(s: &Sexpr, kw: &str) -> bool {
    s.as_atom() == Some(kw)
}
/// Is this a list whose leading keyword is `kw`?
fn eq_kw(s: &Sexpr, kw: &str) -> bool {
    s.keyword() == Some(kw)
}

/// Consume an optional leading `$name`, advancing `j`.
fn opt_name(items: &[Sexpr], j: &mut usize) -> Option<String> {
    if items.get(*j).is_some_and(is_id) {
        let n = items[*j].as_atom().map(ToString::to_string);
        *j += 1;
        return n;
    }
    None
}

// --- Numeric literals ---------------------------------------------------------

/// Strip `_` digit separators.
///
/// The text format allows a separator only **between two digits** — `1_000` is a number,
/// but `_100`, `99_`, `1__000` and `1_.0` are all malformed. Blindly filtering `_` out
/// accepts every one of those, which is how the first conformance run passed a pile of
/// `assert_malformed` literals it should have rejected.
fn strip_seps(s: &str) -> Result<String> {
    let b = s.as_bytes();
    // A separator must sit between two **digits** — not merely two alphanumerics. The looser
    // reading wrongly admits `0x_1`, `1_e1` and `1e_1`, because `x`, `e` and `p` are
    // alphanumeric. Which characters count as digits depends on the radix: in a hex literal
    // `a`–`f` are digits (so `0x1_e2` is legal and its `e` is a digit, not an exponent
    // marker), while in a decimal literal `e` can only be the exponent marker.
    let hex = s.strip_prefix('-').unwrap_or(s);
    let hex = hex.strip_prefix('+').unwrap_or(hex);
    let is_hex = hex.starts_with("0x") || hex.starts_with("0X");
    let digit = |c: u8| {
        if is_hex {
            c.is_ascii_hexdigit()
        } else {
            c.is_ascii_digit()
        }
    };
    for (i, &c) in b.iter().enumerate() {
        if c != b'_' {
            continue;
        }
        let prev_ok = i > 0 && digit(b[i - 1]);
        let next_ok = i + 1 < b.len() && digit(b[i + 1]);
        if !prev_ok || !next_ok {
            return Err(Error::BadNumber);
        }
    }
    Ok(s.chars().filter(|&c| c != '_').collect())
}

/// Parse an unsigned integer literal: decimal, or `0x`-prefixed hex.
fn parse_u64_str(s: &str) -> Result<u64> {
    let t = strip_seps(s)?;
    let (digits, radix) = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(rest) => (rest, 16),
        None => (t.as_str(), 10),
    };
    if digits.is_empty() {
        return Err(Error::BadNumber);
    }
    u64::from_str_radix(digits, radix).map_err(|_| Error::BadNumber)
}

/// Parse a signed integer literal, accepting the unsigned spelling of the same bit
/// pattern (`i32.const 0xffffffff` is `-1`).
fn parse_i64_str(s: &str) -> Result<i64> {
    let t = strip_seps(s)?;
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t.as_str())),
    };
    let (digits, radix) = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        Some(rest) => (rest, 16),
        None => (body, 10),
    };
    if digits.is_empty() {
        return Err(Error::BadNumber);
    }
    let mag = u64::from_str_radix(digits, radix).map_err(|_| Error::BadNumber)?;
    if neg {
        // `-9223372036854775808` is representable even though its magnitude is not.
        if mag > (i64::MAX as u64) + 1 {
            return Err(Error::BadNumber);
        }
        Ok((mag as i64).wrapping_neg())
    } else {
        Ok(mag as i64)
    }
}

// --- Float literals -----------------------------------------------------------
//
// Shared with the `.wast` runner: ONE authority for what a float literal means, so an
// expectation and the module it checks can never disagree.

/// IEEE-754 shape of a target float, so one parser serves `f32` and `f64`.
#[derive(Clone, Copy)]
struct FloatFmt {
    /// Stored mantissa bits (23 / 52).
    mant_bits: i32,
    /// Minimum normal exponent (−126 / −1022).
    exp_min: i32,
    /// Exponent bias (127 / 1023).
    bias: i32,
    /// Maximum biased exponent (0xff / 0x7ff).
    max_biased: i32,
}

const F32_FMT: FloatFmt = FloatFmt {
    mant_bits: 23,
    exp_min: -126,
    bias: 127,
    max_biased: 0xff,
};
const F64_FMT: FloatFmt = FloatFmt {
    mant_bits: 52,
    exp_min: -1022,
    bias: 1023,
    max_biased: 0x7ff,
};

/// Bit position of the sign: mantissa bits + exponent bits.
const fn sign_shift(f: FloatFmt) -> i32 {
    f.mant_bits + (f.max_biased + 1).trailing_zeros() as i32
}

/// Assemble an IEEE bit pattern from a rounded significand `q` and the binary exponent of
/// its least-significant bit. `q` is already rounded to at most `mant_bits + 1` significant
/// bits, so nothing here rounds again.
fn compose_float_bits(mut q: u128, mut ulp_exp: i32, neg: bool, f: FloatFmt) -> u64 {
    let sign = if neg { 1u64 << sign_shift(f) } else { 0 };
    if q == 0 {
        return sign;
    }
    let prec = f.mant_bits + 1;
    // A round-up may have carried into the next binade (q == 2^prec); halving is exact.
    let msb = 128 - q.leading_zeros() as i32;
    if msb > prec {
        q >>= 1;
        ulp_exp += 1;
    }
    let msb = 128 - q.leading_zeros() as i32;
    let e = ulp_exp + msb - 1; // unbiased exponent of the value
    if e < f.exp_min {
        // Subnormal: `ulp_exp` is the smallest subnormal's, so `q` IS the stored mantissa.
        return sign | (q as u64);
    }
    let biased = e + f.bias;
    if biased >= f.max_biased {
        return sign | ((f.max_biased as u64) << f.mant_bits); // overflow → infinity
    }
    let implicit = 1u128 << (prec - 1);
    let mantissa = (q - implicit) as u64;
    sign | ((biased as u64) << f.mant_bits) | mantissa
}

/// Parse a WAT float literal to its bit pattern, **correctly rounded**.
///
/// Decimal literals go through Rust's `from_str` (which is correctly rounded). Hexadecimal
/// ones (`0x1.abcp+3`, and the exponent-less `0xABC` form the text format also allows) are
/// parsed here — Rust has no hex-float parsing at all, and a naive implementation that
/// truncates a long hex mantissa instead of rounding it emits a constant one ULP low. That
/// is a *wrong value*, not a rejected one: the same number written in decimal and in hex
/// would compile to different modules. The oracle hit exactly this on the spec suite's
/// `simd_f64x2_rounding.wast`, whose literals are long enough to cross the threshold.
///
/// Returns `None` on a malformed literal.
/// Check a *numeric* float literal against the wasm text grammar:
///
/// ```text
/// float    ::= num ('.' frac?)? (('e'|'E') sign? num)?
/// hexfloat ::= hexnum ('.' hexfrac?)? (('p'|'P') sign? num)?
/// ```
///
/// This cannot be left to Rust's `FromStr`, which is looser in exactly the places the spec
/// tests probe: it accepts `.5` and `1.5.`-style oddities, and the keywords `inf`/`NaN`. The
/// integer part is mandatory (`.0` is malformed), the fraction may be empty (`1.` is fine),
/// and an exponent marker must be followed by at least one digit (`0e`, `0.0e-` are not).
///
/// `body` has already had its sign and any `0x` prefix removed; separators are permitted and
/// were placement-checked by [`strip_seps`]. An exponent is always spelled in **decimal**,
/// even for a hex literal.
fn check_float_syntax(body: &str, is_hex: bool) -> Result<()> {
    let b = body.as_bytes();
    let digit = |c: u8| {
        if is_hex {
            c.is_ascii_hexdigit()
        } else {
            c.is_ascii_digit()
        }
    };
    let mut i = 0;
    let take = |i: &mut usize, p: &dyn Fn(u8) -> bool| {
        let start = *i;
        while *i < b.len() && (b[*i] == b'_' || p(b[*i])) {
            *i += 1;
        }
        *i > start
    };
    if !take(&mut i, &digit) {
        return Err(Error::BadNumber); // no integer part
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        take(&mut i, &digit); // an empty fraction is legal
    }
    if i < b.len() {
        let marker = if is_hex { b'p' } else { b'e' };
        if b[i] | 0x20 != marker {
            return Err(Error::BadNumber);
        }
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        if !take(&mut i, &|c: u8| c.is_ascii_digit()) {
            return Err(Error::BadNumber); // exponent marker with no digits
        }
    }
    if i == b.len() {
        Ok(())
    } else {
        Err(Error::BadNumber)
    }
}

/// Which grammar a float literal is being read under.
///
/// ⚠️ **`nan:canonical` and `nan:arithmetic` are NOT float literals.** §6.3.2's `fN` admits
/// `nan`, `nan:0x`<hexnum> and the numeric forms — nothing else. The two named spellings belong
/// to the **script** grammar (§7): they are patterns an `assert_return` matches a *result*
/// against, and `f32.wast` asserts that a module containing one is malformed. One parser served
/// both, so the pattern language leaked into the module language.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FloatCtx {
    /// Inside a module: §6.3.2 only.
    Module,
    /// Inside a `.wast` script's expectation: §6.3.2 plus the two NaN patterns.
    Script,
}

fn parse_float_bits(lit: &str, f: FloatFmt, ctx: FloatCtx) -> Option<u64> {
    // The NaN spellings. `nan:0x<payload>` in both grammars; `nan:canonical`/`nan:arithmetic`
    // in scripts only.
    if let Some(colon) = lit.find(':') {
        let canonical: u64 = 1u64 << (f.mant_bits - 1);
        let exp_all = (f.max_biased as u64) << f.mant_bits;
        let mant_mask = (1u64 << f.mant_bits) - 1;
        // Everything before the colon must be exactly `nan`, optionally signed — the head was
        // never checked, so `foo:0x1` parsed as a NaN.
        let head = &lit[..colon];
        if !matches!(head, "nan" | "+nan" | "-nan") {
            return None;
        }
        let tail = &lit[colon + 1..];
        let mut bits = exp_all | canonical;
        if tail == "canonical" || tail == "arithmetic" {
            if ctx == FloatCtx::Module {
                return None;
            }
        } else {
            // §6.3.2: the payload is `nan:0x` hexnum — **hex only**. `nan:1` parsed as decimal
            // 1 and produced a perfectly good NaN, which is accept-invalid (`const.wast`).
            let hex = tail.strip_prefix("0x").or_else(|| tail.strip_prefix("0X"))?;
            if hex.is_empty() {
                return None;
            }
            let payload = parse_u64_str(tail).ok()?;
            // The payload must fit the mantissa and be non-zero — a zero payload is an
            // infinity, not a NaN. Masking instead of checking turned an out-of-range
            // payload into a *different* NaN, which is a wrong value rather than a rejection.
            if payload == 0 || payload > mant_mask {
                return None;
            }
            bits = exp_all | payload;
        }
        if lit.starts_with('-') {
            bits |= 1u64 << sign_shift(f);
        }
        return Some(bits);
    }

    // Validate separator placement for BOTH the decimal and hex paths up front — the hex
    // mantissa loop below skips `_` as it goes, so it would otherwise accept `_1.0`.
    strip_seps(lit).ok()?;

    let mut s = lit;
    let mut neg = false;
    if let Some(rest) = s.strip_prefix('-') {
        neg = true;
        s = rest;
    } else if let Some(rest) = s.strip_prefix('+') {
        s = rest;
    }

    // `inf` and `nan` are the ONLY spellings allowed to denote a non-finite value, and they
    // are matched exactly. Rust's own parser would also accept `infinity`, `Inf`, `NaN` — all
    // malformed in wasm — so they must not be allowed to reach it and come back as infinity.
    let sign_bit = if neg { 1u64 << sign_shift(f) } else { 0 };
    if s == "inf" {
        return Some(sign_bit | ((f.max_biased as u64) << f.mant_bits));
    }
    if s == "nan" {
        return Some(sign_bit | ((f.max_biased as u64) << f.mant_bits) | (1u64 << (f.mant_bits - 1)));
    }

    // Not hex → decimal, which Rust rounds correctly.
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
    let Some(body) = hex else {
        check_float_syntax(s, false).ok()?;
        let cleaned = strip_seps(s).ok()?;
        let v: f64 = if f.mant_bits == 23 {
            f64::from(cleaned.parse::<f32>().ok()?)
        } else {
            cleaned.parse::<f64>().ok()?
        };
        // A *numeric* literal that rounds to infinity is out of range, not infinity: the
        // spec makes it malformed, and silently returning inf is a wrong value rather than a
        // rejected one. `inf`/`nan` already returned above, so this cannot catch them.
        if !v.is_finite() {
            return None;
        }
        let bits = if f.mant_bits == 23 {
            let n = v as f32;
            if !n.is_finite() {
                return None; // in range for f64, overflows f32
            }
            u64::from(n.to_bits())
        } else {
            v.to_bits()
        };
        return Some(bits | sign_bit);
    };

    check_float_syntax(body, true).ok()?;

    // Accumulate the hex significand into a u128. Digits past its capacity cannot change
    // the rounded result except through the sticky bit, so they are folded in rather than
    // dropped silently.
    let mut mant: u128 = 0;
    let mut sticky = false;
    let mut exp: i32 = 0; // binary exponent contributed by digit placement
    let mut seen_digit = false;
    let mut seen_dot = false;
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'.' {
            if seen_dot {
                return None;
            }
            seen_dot = true;
            i += 1;
            continue;
        }
        if c == b'p' || c == b'P' {
            break;
        }
        if c == b'_' {
            i += 1;
            continue; // the text format permits digit separators
        }
        let d: u128 = match c {
            b'0'..=b'9' => u128::from(c - b'0'),
            b'a'..=b'f' => u128::from(c - b'a' + 10),
            b'A'..=b'F' => u128::from(c - b'A' + 10),
            _ => return None,
        };
        seen_digit = true;
        if mant >> 124 != 0 {
            if d != 0 {
                sticky = true;
            }
            if !seen_dot {
                exp += 4; // a dropped integer digit still scales the value
            }
        } else {
            mant = (mant << 4) | d;
            if seen_dot {
                exp -= 4;
            }
        }
        i += 1;
    }
    if !seen_digit {
        return None;
    }
    if i < bytes.len() {
        // A `p` exponent.
        i += 1;
        let mut pneg = false;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            pneg = bytes[i] == b'-';
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let mut pexp: i64 = 0;
        while i < bytes.len() {
            if bytes[i] == b'_' {
                i += 1;
                continue;
            }
            if !bytes[i].is_ascii_digit() {
                return None;
            }
            pexp = pexp * 10 + i64::from(bytes[i] - b'0');
            if pexp > 1 << 30 {
                pexp = 1 << 30; // saturate — it over/underflows either way
            }
            i += 1;
        }
        exp += i32::try_from(if pneg { -pexp } else { pexp }).ok()?;
    }
    if mant == 0 {
        return Some(if neg { 1u64 << sign_shift(f) } else { 0 });
    }

    // value = mant × 2^exp. Round it in ONE step to a multiple of the target's ULP, so the
    // reconstruction below only scales an exact integer and never rounds again.
    //
    // The ULP exponent is the coarser of the normalised one (`e - prec + 1`) and the
    // smallest subnormal's. Taking the max makes normal, subnormal, and
    // below-the-smallest-subnormal one path. Rounding in two stages instead — clamping the
    // kept-bit count and scaling afterwards — throws away the sticky bit, so a value just
    // ABOVE half the smallest subnormal flushes to zero instead of rounding up to it.
    let prec = f.mant_bits + 1;
    let msb = 128 - mant.leading_zeros() as i32;
    let e = exp + msb - 1;
    let ulp_exp = core::cmp::max(f.exp_min - prec + 1, e - prec + 1);

    let k = ulp_exp - exp;
    let q: u128 = if k > 0 {
        // Round to nearest, ties to even. `k` can exceed the width of `mant` for a value
        // far below the smallest subnormal, and a u128 shift is only defined for 0..=127,
        // so the shifted-out-entirely case is handled separately.
        if k > 128 {
            0
        } else {
            let sh = (k - 1) as u32; // 0..=127 here
            let guard = (mant >> sh) & 1;
            if mant & ((1u128 << sh) - 1) != 0 {
                sticky = true;
            }
            let mut q = if k == 128 { 0 } else { mant >> k };
            if guard != 0 && (sticky || (q & 1) != 0) {
                q += 1;
            }
            q
        }
    } else {
        mant << (-k) as u32
    };

    // A hex literal that overflows the format is out of range, exactly as in the decimal
    // path. `compose_float_bits` saturates to infinity, so the check is on its result.
    let bits = compose_float_bits(q, ulp_exp, neg, f);
    let exp_all = (f.max_biased as u64) << f.mant_bits;
    if bits & !(1u64 << sign_shift(f)) == exp_all {
        return None;
    }
    Some(bits)
}

/// Parse a WAT `f32` literal to its bit pattern, under `ctx`'s grammar.
pub(crate) fn parse_f32_bits(lit: &str, ctx: FloatCtx) -> Option<u32> {
    parse_float_bits(lit, F32_FMT, ctx).map(|b| b as u32)
}

/// Parse a WAT `f64` literal to its bit pattern, under `ctx`'s grammar.
pub(crate) fn parse_f64_bits(lit: &str, ctx: FloatCtx) -> Option<u64> {
    parse_float_bits(lit, F64_FMT, ctx)
}

/// Parse an integer literal and check it fits `bits` wide.
///
/// The text format lets a constant be written signed **or** unsigned, so the accepted
/// range is `-2^(bits-1) ..= 2^bits - 1`. Anything outside is "constant out of range" — a
/// rejection, not a silent truncation: `(i32.const 0x100000000)` must not quietly become 0,
/// and `(v128.const i8x16 0x100 …)` must not become all-zero lanes.
fn parse_int_fit(a: &str, bits: u32) -> Result<i64> {
    let v = parse_i64_str(a)?;
    if bits >= 64 {
        return Ok(v);
    }
    let min = -(1i64 << (bits - 1));
    let max = (1i64 << bits) - 1;
    if v < min || v > max {
        return Err(Error::BadNumber);
    }
    Ok(v)
}

fn parse_index(s: &Sexpr) -> Result<u32> {
    let a = want_atom(s)?;
    u32::try_from(parse_u64_str(a)?).map_err(|_| Error::BadImmediate)
}

/// Resolve a `$name` against a name table, or parse a bare numeric index.
fn resolve_by_name(names: &[Option<String>], s: &Sexpr) -> Result<u32> {
    let a = want_atom(s).map_err(|_| Error::BadImmediate)?;
    if a.starts_with('$') {
        for (i, nm) in names.iter().enumerate() {
            if nm.as_deref() == Some(a) {
                return u32::try_from(i).map_err(|_| Error::BadImmediate);
            }
        }
        return Err(Error::UnknownIdentifier);
    }
    parse_index(s)
}

// --- Value types --------------------------------------------------------------

/// A pre-standard type keyword and the modern spelling that replaced it.
///
/// One authority, consulted by both the ROUTING predicate ([`is_type_keyword`], which must still
/// claim it) and every rejecting site. See [`Error::ObsoleteKeyword`] for why those are two jobs.
fn obsolete_type_keyword(atom: &str) -> Option<&'static str> {
    match atom {
        // Renamed by reference-types. `obsolete-keywords.wast` asserts the module is malformed.
        // ⚠️ Not to be confused with the **JS WebAssembly API**'s `element: "anyfunc"`, which is
        // that surface's required spelling — V8 rejects `"funcref"` there. Same word, different
        // language; see `cmem/best-practices.md` §3.11.
        "anyfunc" => Some("funcref"),
        _ => None,
    }
}

/// Does this atom occupy a **type position**? Used to route, not to accept.
///
/// 🔒 True for an obsolete keyword as well as a current one. A parser that stops recognising a
/// token stops sending it to the code that can explain it.
fn is_type_keyword(atom: &str) -> bool {
    string_to_val_type(atom).is_some() || obsolete_type_keyword(atom).is_some()
}

/// A value type spelled as a bare keyword.
fn string_to_val_type(atom: &str) -> Option<V> {
    Some(match atom {
        "i32" => V::I32,
        "i64" => V::I64,
        "f32" => V::F32,
        "f64" => V::F64,
        "v128" => V::V128,
        // ⚠️ `anyfunc` — the pre-standard spelling of `funcref` — was accepted here until
        // 2026-08-20 "because MVP-era tools still emit it". It is **malformed**
        // (`obsolete-keywords.wast`), and wasmtime 47 refuses it with the same reasoning,
        // listing the legal spellings. Accepting it was a deliberate deviation, and T13 has
        // none; the cost is real and is two stale `.wat` files in the ArtOfWebAssembly corpus.
        // 🎓 A leniency with no wrong-value consequence is still a leniency: it is the engine
        // telling a tool its output is fine when the next runtime will reject it.
        "funcref" => V::FUNCREF,
        "externref" => V::EXTERNREF,
        // The hierarchy bottoms are their own types (see `heap_type_to_val_type`).
        "nullfuncref" => V::NULLFUNCREF,
        "nullexternref" => V::NULLEXTERNREF,
        "nullexnref" => V::NULLEXNREF,
        "anyref" => V::ANYREF,
        "eqref" => V::EQREF,
        "i31ref" => V::I31REF,
        "structref" => V::STRUCTREF,
        "arrayref" => V::ARRAYREF,
        "nullref" => V::NULLREF,
        "exnref" => V::EXNREF,
        _ => return None,
    })
}

/// A heap type → a reference value type. A `$name` or numeric index is a **concrete**
/// typed reference carrying that type index; the abstract heads map to their own value
/// types. `nullable` picks the nullable or non-null variant.
fn heap_type_to_val_type(s: &Sexpr, nullable: bool, type_names: &[Option<String>]) -> Result<V> {
    if let Some(ti) = parse_exact(s, type_names)? {
        // The kind bits are a placeholder, as for `concrete_ref` below.
        return Ok(V::exact_ref(nullable, crate::types::RefHeap::Struct, ti));
    }
    let atom = want_atom(s).map_err(|_| Error::BadValType)?;
    let first = atom.chars().next().unwrap_or(' ');
    if first == '$' || first.is_ascii_digit() {
        let ti = resolve_by_name(type_names, s)?;
        // `concrete_ref` masks the index to 28 bits, so a large index would silently
        // truncate — and can land on a small *valid* one, which is type confusion rather
        // than merely a wrong number. The binary decoder is bounded by the declared type
        // count; the text side has no such bound, so check the width here.
        if ti > V::MAX_CONCRETE_INDEX {
            return Err(Error::BadImmediate);
        }
        // The kind bits are a placeholder — only the index is emitted, and the decoder
        // re-derives the family from its type-kind pre-scan.
        return Ok(V::concrete_ref(nullable, crate::types::RefHeap::Struct, ti));
    }
    if let Some(modern) = obsolete_type_keyword(atom) {
        return Err(Error::ObsoleteKeyword(modern));
    }
    let pair = match atom {
        "func" | "funcref" => (V::FUNCREF, V::FUNCREF_NN),
        "extern" | "externref" => (V::EXTERNREF, V::EXTERNREF_NN),
        "exn" | "exnref" => (V::EXNREF, V::EXNREF_NN),
        // ⚠️ The three hierarchy BOTTOMS mapped onto their tops here: `(ref null nofunc)` became
        // `funcref`. Only null inhabits either, so nothing ran wrong — but they are distinct
        // types, and `ref_null.wast`'s second module declares globals of all four.
        "nofunc" | "nullfuncref" => (V::NULLFUNCREF, V::NULLFUNCREF_NN),
        "noextern" | "nullexternref" => (V::NULLEXTERNREF, V::NULLEXTERNREF_NN),
        "noexn" | "nullexnref" => (V::NULLEXNREF, V::NULLEXNREF_NN),
        "any" | "anyref" => (V::ANYREF, V::ANYREF_NN),
        "eq" | "eqref" => (V::EQREF, V::EQREF_NN),
        "i31" | "i31ref" => (V::I31REF, V::I31REF_NN),
        "struct" | "structref" => (V::STRUCTREF, V::STRUCTREF_NN),
        "array" | "arrayref" => (V::ARRAYREF, V::ARRAYREF_NN),
        "none" | "nullref" => (V::NULLREF, V::NULLREF_NN),
        _ => return Err(Error::BadValType),
    };
    Ok(if nullable { pair.0 } else { pair.1 })
}

/// Parse a **reference** type — the grammar's `reftype`, where a numeric or vector type is not
/// merely ill-typed but unspellable (§6.4.2).
///
/// ⚠️⚠️ The table element position used [`parse_val_type`], so `(module (table 1 i64))` assembled
/// to a table whose element type byte is `0x7e`: bytes `wasm-tools` refuses as **malformed
/// reference type**, and which wasmrt itself then decoded, validated and ran. The eighth instance
/// of the emitter mechanism (T10a), and the third where our assembler's output is not
/// WebAssembly — wasm-tools refuses the same SOURCE at its parser.
fn parse_ref_type(s: &Sexpr, type_names: &[Option<String>]) -> Result<V> {
    let t = parse_val_type(s, type_names)?;
    if !t.is_ref() {
        return Err(Error::BadValType);
    }
    Ok(t)
}

/// Parse a value type: a bare keyword, or the list form `(ref null? ht)`.
fn parse_val_type(s: &Sexpr, type_names: &[Option<String>]) -> Result<V> {
    if let Some(l) = s.as_list() {
        if l.len() >= 2 && eq_atom(&l[0], "ref") {
            let nullable = ref_form_nullable(l)?;
            return heap_type_to_val_type(&l[l.len() - 1], nullable, type_names);
        }
        return Err(Error::BadValType);
    }
    let atom = want_atom(s).map_err(|_| Error::BadValType)?;
    if let Some(modern) = obsolete_type_keyword(atom) {
        return Err(Error::ObsoleteKeyword(modern));
    }
    string_to_val_type(atom).ok_or(Error::BadValType)
}

/// Is this form a reference type (`(ref …)`)?
fn is_ref_type_form(s: &Sexpr) -> bool {
    s.as_list()
        .is_some_and(|l| !l.is_empty() && eq_atom(&l[0], "ref"))
}

// --- Module-level definitions -------------------------------------------------

/// A GC field's storage: a value type, or one of the two packed integer widths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Storage {
    Val(V),
    I8,
    I16,
}

/// One GC struct field / array element type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GcField {
    storage: Storage,
    mutable: bool,
}

/// A type-section entry. Function types dominate, so they stay the common case; GC struct
/// and array definitions carry their fields, and any of the three may declare a supertype.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TypeDef {
    Func(Sig),
    Struct(Vec<GcField>),
    Array(GcField),
}

impl Default for TypeDef {
    fn default() -> Self {
        TypeDef::Func(Sig::default())
    }
}

/// A function signature, interned in the type section.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Sig {
    params: Vec<V>,
    results: Vec<V>,
}

/// Intern a signature, returning its type index. An identical existing entry is reused, so
/// inline `(param …)(result …)` annotations don't bloat the type section.
/// Reuse an existing type with this signature, or append one — the implicit type use of
/// `(func …)` / `(call_indirect (param …))` and friends.
///
/// ⚠⚠ **A type inside an explicit `(rec …)` group is NEVER reused.** Rec-group membership is
/// part of a type’s IDENTITY, so a standalone `(func)` and the `(func)` inside
/// `(rec (type $ft (func)) (type (func)))` are **different types** even though they spell the
/// same shape. Reusing `$ft` for an implicit use silently gave a function the rec-group type,
/// which made `(global (ref $ft) (ref.func $f))` — an `assert_invalid` — VALID.
///
/// 🎓 The assembler was changing what the module MEANS, not just how it was spelled: the text
/// says “`$f` has its own implicit type” and the encoder said “reuse `$ft`”. Same mechanism as
/// T10a’s emitter defects, one level up — and the reason `is_subtype` could not catch it is that
/// by the time it ran, the two really were one type.
fn intern_sig_outside_rec(types: &mut Vec<TypeDef>, rec_groups: &[(u32, u32)], sig: Sig) -> u32 {
    let want = TypeDef::Func(sig);
    let in_group =
        |i: u32| rec_groups.iter().any(|&(s, n)| i >= s && i < s.saturating_add(n));
    if let Some(i) = types
        .iter()
        .position(|t| *t == want)
        .filter(|&i| !in_group(i as u32))
    {
        return i as u32;
    }
    types.push(want);
    (types.len() - 1) as u32
}

/// The function signature at a type index, if that type is a function type.
fn func_sig_at(types: &[TypeDef], ti: u32) -> Option<&Sig> {
    match types.get(ti as usize) {
        Some(TypeDef::Func(s)) => Some(s),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct ImportRef {
    module: Vec<u8>,
    name: Vec<u8>,
}

/// A parsed `(func …)` definition.
#[derive(Debug, Clone, Default)]
struct Func {
    type_ref: Option<u32>,
    sig: Sig,
    /// Param names then local names, index-aligned with the local index space.
    local_names: Vec<Option<String>>,
    locals: Vec<V>,
    body: Vec<Sexpr>,
    /// The function list's annotations that stand in its body, indexed into `body`.
    body_annots: Vec<Annot>,
}

#[derive(Debug, Clone)]
struct ExportDef {
    name: Vec<u8>,
    kind: u8,
    index: u32,
}

#[derive(Debug, Clone)]
struct MemoryDef {
    min: u64,
    max: Option<u64>,
    shared: bool,
    is64: bool,
    /// `(pagesize N)` as `log2(N)`; `None` when the text states none (64 KiB, no flag emitted).
    page_size_log2: Option<u32>,
}

#[derive(Debug, Clone)]
struct TableDef {
    min: u64,
    max: Option<u64>,
    /// A 64-bit table (table64). Its limits and every index operand are `i64`.
    is64: bool,
    elem: V,
    /// `(table 3 funcref (ref.func $f))` — the function-references initializer expression
    /// every entry starts as. `None` is the plain form, which starts as null.
    ///
    /// **This was silently dropped until 2026-08-06**, so a table declared with an
    /// initializer assembled to one full of nulls: a *wrong module*, not a rejected one.
    init: Option<Vec<Sexpr>>,
}

#[derive(Debug, Clone)]
struct GlobalDef {
    valtype: V,
    mutable: bool,
    init: Vec<Sexpr>,
}

#[derive(Debug, Clone)]
struct DataSeg {
    mem_index: u32,
    /// `None` for a passive segment.
    offset: Option<Vec<Sexpr>>,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
struct ElemDef {
    table_index: u32,
    /// `None` for a passive or declarative segment.
    offset: Option<Vec<Sexpr>>,
    elem_type: V,
    /// Each entry is a const-expr producing a reference.
    items: Vec<Vec<Sexpr>>,
    declarative: bool,
    /// Does this segment need the **expression** encoding (flags 4–7: a reftype plus
    /// const-exprs) rather than the **index** encoding (flags 0–3: an elemkind byte plus
    /// bare function indices)? True when the source used `(item …)` / folded expressions
    /// or a non-funcref element type, false for the bare `func $a $b` shorthand.
    use_exprs: bool,
}

/// Which kind an import declares. Recorded in source order so the import section can be
/// emitted in declaration order while the per-kind lists still assign the indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportKind {
    Func,
    Table,
    Mem,
    Global,
    Tag,
}

#[derive(Debug, Clone)]
struct ImportedFunc {
    r: ImportRef,
    type_index: u32,
    /// `(func (exact …))` — custom-descriptors' EXACT function import (kind `0x20`).
    exact: bool,
}
#[derive(Debug, Clone)]
struct ImportedTable {
    r: ImportRef,
    t: TableDef,
}
#[derive(Debug, Clone)]
struct ImportedMemory {
    r: ImportRef,
    m: MemoryDef,
}
#[derive(Debug, Clone)]
struct ImportedGlobal {
    r: ImportRef,
    valtype: V,
    mutable: bool,
}
#[derive(Debug, Clone)]
struct ImportedTag {
    r: ImportRef,
    type_index: u32,
}

/// Everything collected from the module's fields, before section emission.
#[derive(Default)]
struct ModuleBuild {
    types: Vec<TypeDef>,
    /// Declared supertype of each type, index-aligned with `types`.
    supers: Vec<Option<u32>>,
    /// `(start, len)` of each explicit `(rec …)` group, in order. Types not covered by one are
    /// singleton groups, which is what the spec says they are — so the emitter needs no special case.
    ///
    /// The group is the unit of **type identity**, so dropping it (as the parser did) changes what
    /// the module means: two members of one rec group are not the same types as two standalone
    /// definitions of the same shape.
    rec_groups: Vec<(u32, u32)>,
    /// Whether each type is **final**, index-aligned with `types`. Final is the default: only a
    /// bare `(sub …)` — without `final` — opens a type to being extended.
    ///
    /// Tracked because the emitter must choose between `0x50` (open), `0x4f` (`sub final`) and the
    /// bare-composite shorthand. Before this it emitted a wrapper only when a supertype was present,
    /// so `(sub (struct …))` with no supertype assembled as **final** — silently turning a valid
    /// hierarchy into an invalid one, the same class as element-segment form 4 rewriting a type.
    type_finals: Vec<bool>,
    /// Each type's custom-descriptors `(describes $x)` / `(descriptor $y)`, index-aligned with `types`.
    type_links: Vec<(Option<u32>, Option<u32>)>,
    /// Field names of each struct type, index-aligned with `types` (empty for non-structs),
    /// so `struct.get $T $field` can resolve a field by name — the form binaryen and
    /// hand-written GC .wat actually emit.
    field_names: Vec<Vec<Option<String>>>,
    type_names: Vec<Option<String>>,

    funcs: Vec<Func>,
    func_names: Vec<Option<String>>,
    func_imports: Vec<ImportedFunc>,

    tables: Vec<TableDef>,
    table_names: Vec<Option<String>>,
    table_imports: Vec<ImportedTable>,

    memories: Vec<MemoryDef>,
    mem_names: Vec<Option<String>>,
    mem_imports: Vec<ImportedMemory>,

    globals: Vec<GlobalDef>,
    global_names: Vec<Option<String>>,
    global_imports: Vec<ImportedGlobal>,

    tags: Vec<u32>,
    tag_names: Vec<Option<String>>,
    tag_imports: Vec<ImportedTag>,

    elems: Vec<ElemDef>,
    elem_names: Vec<Option<String>>,

    datas: Vec<DataSeg>,
    data_names: Vec<Option<String>>,

    exports: Vec<ExportDef>,
    import_order: Vec<ImportKind>,
    start: Option<Sexpr>,

    /// Set while encoding a body that emits `memory.init` or `data.drop`.
    ///
    /// §5.5.13 requires the data-count section **only** when one of those two can appear —
    /// it exists so a decoder knows the segment count before reading the code section.
    /// Emitting it unconditionally cost 3 bytes on every module with data segments, which
    /// is 3 bytes against the "small" axis on modules that never bulk-copy.
    needs_data_count: bool,

    /// Type uses whose `(type x)` named an index that did not exist YET when the use was read,
    /// with the explicit clauses beside it: `(type x)` may name an IMPLICIT type — one the text
    /// format appends for an inline signature elsewhere — so the match can only be checked once
    /// every type exists. Checked at the end of assembly (see `assemble_parts`).
    ///
    /// ⚠️ Checking immediately found index `x` absent and refused with `UnexpectedToken`: right to
    /// refuse, wrong CAUSE — wasm-tools says the inline type does not match, and it is right.
    deferred_type_uses: Vec<(u32, Sig)>,
}

// --- Entry points -------------------------------------------------------------

/// Assemble `.wat` source into a `.wasm` binary.
///
/// # Errors
/// Returns [`Error::NotAModule`] if the source holds no `(module …)` form, or a parse /
/// assembly error describing the first problem found.
pub fn assemble(src: &[u8]) -> Result<Vec<u8>> {
    for form in sexpr::parse_all(src)? {
        if form.keyword() == Some("module") {
            return assemble_form(&form);
        }
    }
    Err(Error::NotAModule)
}

/// Assemble an already-parsed `(module …)` NODE, honouring the annotations written in it.
///
/// # Errors
/// Returns an [`Error`] describing the first problem found.
pub fn assemble_form(module: &Sexpr) -> Result<Vec<u8>> {
    assemble_parts(want_list(module)?, module.annotations())
}

/// Assemble an already-parsed `(module …)` form's ITEMS (`module[0]` is the `module` keyword).
/// A bare item slice carries no annotations — prefer [`assemble_form`].
///
/// # Errors
/// Returns an [`Error`] describing the first problem found.
pub fn assemble_module(module: &[Sexpr]) -> Result<Vec<u8>> {
    assemble_parts(module, &[])
}

fn assemble_parts(module: &[Sexpr], annots: &[Annot]) -> Result<Vec<u8>> {
    // Skip the optional module `$name`. ⚠️ Computed BEFORE the `binary` check, because
    // `(module $M binary "…")` is legal and the name sits between the two — checking
    // `module[1]` for `binary` missed the named form and fell through to the field parser,
    // which reported `BadModuleField` about the word `binary`.
    let start = usize::from(module.len() > 1 && is_id(&module[1])) + 1;

    // `(module $name? binary "…" …)` — the strings ARE the module, verbatim.
    if module.get(start).is_some_and(|s| eq_atom(s, "binary")) {
        let mut out = Vec::new();
        for s in &module[start + 1..] {
            out.extend_from_slice(want_str(s)?);
        }
        return Ok(out);
    }
    let fields = module.get(start..).unwrap_or(&[]);

    // Every annotation is checked BEFORE any field is parsed, so a malformed one is what the
    // module is refused for even when something else is wrong with it too.
    let plan = annot::plan(module, annots)?;

    let mut b = ModuleBuild::default();

    // Pre-pass A: every `(type …)` name, so a concrete `(ref $t)` in a later field can
    // forward-reference a type declared further down (a `(rec …)` group routinely does).
    let mut type_forms: Vec<&[Sexpr]> = Vec::new();
    for field in fields {
        match field.keyword() {
            Some("type") => {
                let l = want_list(field)?;
                b.type_names.push(type_def_name(l));
                type_forms.push(l);
            }
            Some("rec") => {
                // Record the group's EXTENT, not just its members. A rec group is the unit of type
                // identity (§3.1.4): `(rec (type (func)) (type (func)))` is a different type from two
                // separate `(type (func))`s, so flattening the group — which this did — emits a module
                // that is not the module the text describes.
                let start = type_forms.len() as u32;
                for t in &want_list(field)?[1..] {
                    if t.keyword() == Some("type") {
                        let l = want_list(t)?;
                        b.type_names.push(type_def_name(l));
                        type_forms.push(l);
                    }
                }
                b.rec_groups.push((start, type_forms.len() as u32 - start));
            }
            _ => {}
        }
    }
    // Pre-pass B: the bodies, now that every type name resolves.
    for form in &type_forms {
        let links = parse_type_body(form, &b.type_names, &mut b.types, &mut b.supers, &mut b.type_finals, &mut b.field_names)?;
        b.type_links.push(links);
    }

    // Pass 1: the remaining definitions, in source order.
    let mut pending_exports: Vec<&[Sexpr]> = Vec::new();
    let mut seen_definition = false;
    for field in fields {
        let kw = field.keyword().ok_or(Error::BadModuleField)?;
        let items = want_list(field)?;
        if field_is_import(kw, items) {
            if seen_definition {
                return Err(Error::ImportAfterDefinition);
            }
        } else if is_def_kind(kw) {
            seen_definition = true;
        }
        match kw {
            "type" | "rec" => {} // handled in the pre-passes
            "func" => parse_func_field(items, field.annotations(), &mut b)?,
            "memory" => parse_memory_field(items, &mut b)?,
            "global" => parse_global_field(items, &mut b)?,
            "table" => parse_table_field(items, &mut b)?,
            "elem" => parse_elem_field(items, &mut b)?,
            "data" => parse_data_field(items, &mut b)?,
            "tag" => parse_tag_field(items, &mut b)?,
            "import" => parse_import_field(items, &mut b)?,
            "start" => {
                if b.start.is_some() {
                    return Err(Error::MultipleStart);
                }
                b.start = Some(nth(items, 1)?.clone());
            }
            // DEFERRED to pass 2: a module-level export may name something declared later
            // in the file, and binaryen emits exactly that order (all exports, then the
            // funcs). Inline `(export …)` clauses stay immediate — they can only name the
            // item they sit inside.
            "export" => pending_exports.push(items),
            _ => return Err(Error::BadModuleField),
        }
    }

    // Pass 2: module-level exports, now that every index space is complete.
    for items in pending_exports {
        let name = want_str(nth(items, 1)?)?.to_vec();
        let target = want_list(nth(items, 2)?)?;
        let idx_form = nth(target, 1)?;
        let (kind, index) = match want_atom(nth(target, 0)?)? {
            "func" => (0u8, resolve_by_name(&b.func_names, idx_form)?),
            "table" => (1, resolve_by_name(&b.table_names, idx_form)?),
            "memory" => (2, resolve_by_name(&b.mem_names, idx_form)?),
            "global" => (3, resolve_by_name(&b.global_names, idx_form)?),
            "tag" => (4, resolve_by_name(&b.tag_names, idx_form)?),
            _ => return Err(Error::BadModuleField),
        };
        b.exports.push(ExportDef { name, kind, index });
    }

    // Resolve each defined function's type index BEFORE emission: interning an inline
    // signature can append to the type table, and the type section must already contain
    // everything by the time it is written.
    let mut func_sigs: Vec<u32> = Vec::with_capacity(b.funcs.len());
    for i in 0..b.funcs.len() {
        let ti = match b.funcs[i].type_ref {
            Some(ti) => ti,
            None => {
                let sig = b.funcs[i].sig.clone();
                intern_sig_outside_rec(&mut b.types, &b.rec_groups, sig)
            }
        };
        func_sigs.push(ti);
    }

    // Encode every body and const-expr BEFORE any section is written: a multi-value block
    // type or an inline `call_indirect` signature interns into the type table as it is
    // encoded, so the type section is only complete once the last body is done.
    let funcs = core::mem::take(&mut b.funcs);
    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(funcs.len());
    let mut body_hints: Vec<Vec<(u32, u8)>> = Vec::with_capacity(funcs.len());
    let mut body_labels: Vec<NameMap> = Vec::with_capacity(funcs.len());
    for f in &funcs {
        let body = encode_body(f, &mut b)?;
        bodies.push(body.bytes);
        body_hints.push(body.hints);
        body_labels.push(body.labels);
    }
    let globals = core::mem::take(&mut b.globals);
    let mut global_inits: Vec<Vec<u8>> = Vec::with_capacity(globals.len());
    for g in &globals {
        let mut out = Vec::new();
        emit_const_expr(&mut out, &g.init, &mut b)?;
        global_inits.push(out);
    }
    // Table initializers, encoded in this same pre-pass and for the same reason as the
    // global ones: `emit_const_expr` can intern a type, and the type section must be
    // complete before any section is written.
    let table_inits: Vec<Option<Vec<u8>>> = {
        let tables = b.tables.clone();
        let mut v = Vec::with_capacity(tables.len());
        for t in &tables {
            match &t.init {
                Some(expr) => {
                    let mut out = Vec::new();
                    emit_const_expr(&mut out, expr, &mut b)?;
                    v.push(Some(out));
                }
                None => v.push(None),
            }
        }
        v
    };
    let elems = core::mem::take(&mut b.elems);
    let mut elem_bytes: Vec<Vec<u8>> = Vec::with_capacity(elems.len());
    for e in &elems {
        let mut out = Vec::new();
        emit_elem_segment(&mut out, e, &mut b)?;
        elem_bytes.push(out);
    }
    let datas = core::mem::take(&mut b.datas);
    let mut data_offsets: Vec<Option<Vec<u8>>> = Vec::with_capacity(datas.len());
    for d in &datas {
        match &d.offset {
            Some(off) => {
                let mut out = Vec::new();
                emit_const_expr(&mut out, off, &mut b)?;
                data_offsets.push(Some(out));
            }
            None => data_offsets.push(None),
        }
    }

    // The deferred type uses, now that every type — implicit ones included — exists. An index
    // that still names nothing is left for the validator, whose stage "unknown type" is.
    for (ti, sig) in &b.deferred_type_uses {
        if let Some(d) = func_sig_at(&b.types, *ti) {
            if d.params != sig.params || d.results != sig.results {
                return Err(Error::TypeUseMismatch);
            }
        }
    }

    // §6.3.5: within an index space, a `$name` must be UNIQUE.
    //
    // 🔒 **Checked on the NAMESPACE, not at each writer.** Every index space is filled from two
    // places — an import and a definition — and the uniqueness rule spans both, so a
    // per-push-site check structurally cannot see `(import "" "" (memory $foo 1))` beside
    // `(memory $foo 1)`. Running it once over the finished vectors catches all three shapes
    // (def+def, import+def, import+import) with one rule and no duplication.
    //
    // ⚠️ Worth 7 `assert_malformed`s that were **accepting** a malformed module — the dangerous
    // direction — across `memory.wast` and `func.wast`.
    for (space, names) in [
        ("type", &b.type_names),
        ("func", &b.func_names),
        ("table", &b.table_names),
        ("memory", &b.mem_names),
        ("global", &b.global_names),
        ("tag", &b.tag_names),
        ("elem", &b.elem_names),
        ("data", &b.data_names),
    ] {
        let _ = space; // named for the reader; the error carries no payload
        check_unique_names(names)?;
    }

    let bytes = emit_module(
        &b,
        &func_sigs,
        &bodies,
        &globals,
        &global_inits,
        &table_inits,
        &elem_bytes,
        &datas,
        &data_offsets,
    )?;
    let module_id = module.get(1).filter(|s| is_id(s)).and_then(Sexpr::as_atom);
    let first_def = b.func_imports.len() as u32;
    let hints = branch_hint_section(first_def, &body_hints);
    let names = name_section(&b, &funcs, &plan, module_id, &body_labels);
    Ok(annot::lay_out(&bytes, &plan.customs, hints.as_deref(), names.as_deref()))
}

/// The `metadata.code.branch_hint` payload: for each function with hints (by function index,
/// ascending), its hints as `(offset, size = 1, value)` in offset order. `None` when no hint
/// was written, so a module without one is byte-for-byte what it was.
fn branch_hint_section(first_def: u32, per_func: &[Vec<(u32, u8)>]) -> Option<Vec<u8>> {
    let with: Vec<(usize, &Vec<(u32, u8)>)> =
        per_func.iter().enumerate().filter(|(_, h)| !h.is_empty()).collect();
    if with.is_empty() {
        return None;
    }
    let mut c = Vec::new();
    uleb(&mut c, with.len() as u64);
    for (i, hs) in with {
        uleb(&mut c, u64::from(first_def) + i as u64);
        uleb(&mut c, hs.len() as u64);
        for &(off, v) in hs {
            uleb(&mut c, u64::from(off));
            uleb(&mut c, 1);
            c.push(v);
        }
    }
    Some(c)
}

/// A name map: `(index, name)`, ascending by index.
type NameMap = Vec<(u32, Vec<u8>)>;
/// An indirect name map: `(outer index, its name map)` — locals and labels per function.
type IndirectNameMap = Vec<(u32, NameMap)>;

/// A name map from an index space's `$id`s, with `@name` overrides for subsection `sub` laid
/// over them — ascending by index, as the name section requires.
fn name_map(ids: &[Option<String>], sub: u8, over: &[(u8, u32, Vec<u8>)]) -> NameMap {
    let mut m: BTreeMap<u32, Vec<u8>> = ids
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n.as_deref().map(|n| (i as u32, id_text(n))))
        .collect();
    for (s, i, n) in over {
        if *s == sub {
            m.insert(*i, n.clone());
        }
    }
    m.into_iter().collect()
}

fn put_name_map(c: &mut Vec<u8>, m: &[(u32, Vec<u8>)]) {
    uleb(c, m.len() as u64);
    for (i, n) in m {
        uleb(c, u64::from(*i));
        name_bytes(c, n);
    }
}

fn put_subsection(out: &mut Vec<u8>, id: u8, c: &[u8]) {
    out.push(id);
    uleb(out, c.len() as u64);
    out.extend_from_slice(c);
}

/// The `name` section — what wasm-tools writes by default: every `$id` in every index space,
/// with any `@name` taking precedence. Subsections in id order, each omitted when empty; the
/// whole section `None` when there is nothing to name.
///
/// ⚠️ Until 2026-09-19 wasmrt wrote NO name section, so every `$id` in the source was lost at
/// assembly — which is why its own trap backtraces printed `name=(none)` for functions the
/// source had named.
fn name_section(
    b: &ModuleBuild,
    funcs: &[Func],
    plan: &annot::Plan,
    module_id: Option<&str>,
    labels: &[NameMap],
) -> Option<Vec<u8>> {
    use annot::sub;
    let first_def = b.func_imports.len() as u32;
    let mut out = Vec::new();

    if let Some(n) = plan.module_name.clone().or_else(|| module_id.map(id_text)) {
        let mut c = Vec::new();
        name_bytes(&mut c, &n);
        put_subsection(&mut out, sub::MODULE, &c);
    }
    let simple = |out: &mut Vec<u8>, id: u8, ids: &[Option<String>]| {
        let m = name_map(ids, id, &plan.names);
        if !m.is_empty() {
            let mut c = Vec::new();
            put_name_map(&mut c, &m);
            put_subsection(out, id, &c);
        }
    };
    // An indirect map: (outer index, name map) for each outer entry that has one.
    let indirect = |out: &mut Vec<u8>, id: u8, maps: &[(u32, NameMap)]| {
        if maps.is_empty() {
            return;
        }
        let mut c = Vec::new();
        uleb(&mut c, maps.len() as u64);
        for (i, m) in maps {
            uleb(&mut c, u64::from(*i));
            put_name_map(&mut c, m);
        }
        put_subsection(out, id, &c);
    };

    simple(&mut out, sub::FUNC, &b.func_names);

    // Locals. A declaration's `@name` becomes a local index only now: the params of a
    // `(type $t)` with no inline params sit in front of every declaration.
    let mut locals = Vec::new();
    for (i, f) in funcs.iter().enumerate() {
        let fi = first_def + i as u32;
        let mut m: BTreeMap<u32, Vec<u8>> = f
            .local_names
            .iter()
            .enumerate()
            .filter_map(|(j, n)| n.as_deref().map(|n| (j as u32, id_text(n))))
            .collect();
        for (pf, decl, total, n) in &plan.locals {
            if *pf == fi {
                let shift = (f.local_names.len() as u32).saturating_sub(*total);
                m.insert(decl + shift, n.clone());
            }
        }
        if !m.is_empty() {
            locals.push((fi, m.into_iter().collect()));
        }
    }
    indirect(&mut out, sub::LOCAL, &locals);

    let label_maps: IndirectNameMap = labels
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.is_empty())
        .map(|(i, l)| (first_def + i as u32, l.clone()))
        .collect();
    indirect(&mut out, sub::LABEL, &label_maps);

    simple(&mut out, sub::TYPE, &b.type_names);
    simple(&mut out, sub::TABLE, &b.table_names);
    simple(&mut out, sub::MEMORY, &b.mem_names);
    simple(&mut out, sub::GLOBAL, &b.global_names);
    simple(&mut out, sub::ELEM, &b.elem_names);
    simple(&mut out, sub::DATA, &b.data_names);

    let mut fields = Vec::new();
    for (ti, names) in b.field_names.iter().enumerate() {
        // Fields take no `@name` (wasm-tools refuses it), so there is nothing to lay over.
        let m = name_map(names, sub::FIELD, &[]);
        if !m.is_empty() {
            fields.push((ti as u32, m));
        }
    }
    indirect(&mut out, sub::FIELD, &fields);

    simple(&mut out, sub::TAG, &b.tag_names);

    (!out.is_empty()).then_some(out)
}

/// The `$name` of a `(type $n …)` definition, if any.
fn type_def_name(items: &[Sexpr]) -> Option<String> {
    items
        .get(1)
        .filter(|s| is_id(s))
        .and_then(|s| s.as_atom())
        .map(ToString::to_string)
}

/// Kinds whose definition closes the import window (§6.6.13). Tags are included because an
/// imported tag takes a low tag index, so a defined tag before it would mis-align the
/// source-order tag space.
fn is_def_kind(kw: &str) -> bool {
    matches!(kw, "func" | "table" | "memory" | "global" | "tag")
}

/// Does this field declare an import — a top-level `(import …)`, or an inline
/// `(func … (import "m" "n") …)` form?
fn field_is_import(kw: &str, items: &[Sexpr]) -> bool {
    kw == "import" || (is_def_kind(kw) && items.iter().any(|s| eq_kw(s, "import")))
}

/// Parse a `(type …)` body into the type table: a function, struct or array definition,
/// optionally wrapped in `(sub $super …)`.
///
/// A `(type …)` definition occupies its **own slot** even when an identical one already
/// exists, so this pushes rather than interns — the declared index must match its position.
/// custom-descriptors: `(describes $x)? (descriptor $y)?` from `list[*k..]`, in that order and each at
/// most once — whatever follows is the composite type, so a repeated or reordered clause is left where
/// the composite type must be and refused there ("unexpected token", `descriptors.wast`).
///
/// One reader for the two places the clauses stand: at the type level, and inside `(sub …)` after the
/// supertypes.
fn parse_type_links(
    list: &[Sexpr],
    k: &mut usize,
    type_names: &[Option<String>],
) -> Result<(Option<u32>, Option<u32>)> {
    let clause = |kw: &str, k: &mut usize| -> Result<Option<u32>> {
        match list.get(*k).filter(|s| s.keyword() == Some(kw)) {
            Some(c) => {
                let [_, x] = want_list(c)? else {
                    return Err(Error::UnexpectedToken);
                };
                *k += 1;
                Ok(Some(resolve_by_name(type_names, x)?))
            }
            None => Ok(None),
        }
    };
    let describes = clause("describes", k)?;
    let descriptor = clause("descriptor", k)?;
    Ok((describes, descriptor))
}

fn parse_type_body(
    items: &[Sexpr],
    type_names: &[Option<String>],
    types: &mut Vec<TypeDef>,
    supers: &mut Vec<Option<u32>>,
    finals: &mut Vec<bool>,
    field_names: &mut Vec<Vec<Option<String>>>,
) -> Result<(Option<u32>, Option<u32>)> {
    let mut j = 1;
    if items.get(j).is_some_and(is_id) {
        j += 1;
    }
    // The clauses may stand at the type level: `(type $a (descriptor $b) (struct))`.
    let mut links = parse_type_links(items, &mut j, type_names)?;
    let mut l = want_list(nth(items, j)?)?;
    let mut super_ref = None;

    // `(sub final? $super? <comptype>)` — the supertype list, then the real definition.
    // A type with no `(sub …)` wrapper at all is final; `(sub …)` opens it; `(sub final …)` closes
    // it again while still allowing a supertype.
    let mut is_final = true;
    if want_atom(nth(l, 0)?)? == "sub" {
        let mut k = 1;
        is_final = l.get(k).is_some_and(|s| eq_atom(s, "final"));
        if is_final {
            k += 1;
        }
        while let Some(s) = l.get(k) {
            if s.as_list().is_some() {
                break; // the clauses or the composite type begin
            }
            super_ref = Some(resolve_by_name(type_names, s)?);
            k += 1;
        }
        // …or inside `(sub …)`, after the supertypes — but not in both places.
        let inner = parse_type_links(l, &mut k, type_names)?;
        if inner != (None, None) {
            if links != (None, None) {
                return Err(Error::UnexpectedToken);
            }
            links = inner;
        }
        l = want_list(nth(l, k)?)?;
    }

    let mut names: Vec<Option<String>> = Vec::new();
    let def = match want_atom(nth(l, 0)?)? {
        "func" => TypeDef::Func(parse_sig(&l[1..], type_names, None)?),
        "struct" => {
            let mut fields = Vec::new();
            for f in &l[1..] {
                parse_field_group(f, type_names, &mut fields, &mut names)?;
            }
            TypeDef::Struct(fields)
        }
        "array" => {
            let mut fields = Vec::new();
            parse_field_group(nth(l, 1)?, type_names, &mut fields, &mut names)?;
            // An array has exactly one element type.
            TypeDef::Array(*fields.first().ok_or(Error::BadForm)?)
        }
        _ => return Err(Error::BadForm),
    };
    check_unique_names(&names)?;
    types.push(def);
    supers.push(super_ref);
    finals.push(is_final);
    field_names.push(names);
    Ok(links)
}

/// No `$name` may repeat within one namespace (§6.3.5). `None` entries are unnamed and never
/// collide.
///
/// 🔒 **One authority, three namespaces.** It was written for the module's index spaces, and the
/// same rule governs a function's locals (parameters and locals share ONE space, so
/// `(func (param $foo i32) (local $foo i32))` is malformed) and a struct type's field names. Each
/// of those had no check at all, in the **accepting** direction — 4 `assert_malformed`s across
/// `func.wast` and `struct.wast`. A second copy of the loop would have been a third place for the
/// rule to drift.
fn check_unique_names(names: &[Option<String>]) -> Result<()> {
    for i in 0..names.len() {
        let Some(n) = names[i].as_deref() else { continue };
        if names[..i].iter().any(|m| m.as_deref() == Some(n)) {
            return Err(Error::DuplicateName);
        }
    }
    Ok(())
}

/// Parse one `(field …)` group — or a bare storage type, which the array and anonymous
/// struct-field forms allow. `(field $x i32)` names one field; `(field i32 i64)` is an
/// anonymous run.
fn parse_field_group(
    s: &Sexpr,
    type_names: &[Option<String>],
    out: &mut Vec<GcField>,
    names: &mut Vec<Option<String>>,
) -> Result<()> {
    let Some(l) = s.as_list() else {
        // A bare storage type: `(array i8)`.
        out.push(parse_field_elem(s, type_names)?);
        names.push(None);
        return Ok(());
    };
    if l.first().map(|f| eq_atom(f, "field")) != Some(true) {
        // `(array (mut i8))` — a `(mut …)` wrapper with no `field` keyword.
        out.push(parse_field_elem(s, type_names)?);
        names.push(None);
        return Ok(());
    }
    if l.len() >= 3 && is_id(&l[1]) {
        out.push(parse_field_elem(nth(l, 2)?, type_names)?);
        names.push(l[1].as_atom().map(ToString::to_string));
        return Ok(());
    }
    for f in &l[1..] {
        out.push(parse_field_elem(f, type_names)?);
        names.push(None);
    }
    Ok(())
}

/// A field element: an optional `(mut …)` wrapper around a storage type.
fn parse_field_elem(s: &Sexpr, type_names: &[Option<String>]) -> Result<GcField> {
    if eq_kw(s, "mut") {
        return Ok(GcField {
            storage: parse_storage(nth(want_list(s)?, 1)?, type_names)?,
            mutable: true,
        });
    }
    Ok(GcField {
        storage: parse_storage(s, type_names)?,
        mutable: false,
    })
}

/// A storage type: the packed integer widths, else a value type.
fn parse_storage(s: &Sexpr, type_names: &[Option<String>]) -> Result<Storage> {
    if let Some(a) = s.as_atom() {
        match a {
            "i8" => return Ok(Storage::I8),
            "i16" => return Ok(Storage::I16),
            _ => {}
        }
    }
    Ok(Storage::Val(parse_val_type(s, type_names)?))
}

/// Emit a GC field: its storage-type byte (packed `i8` = 0x78 / `i16` = 0x77, else the
/// value type), then a mutability byte.
fn emit_gc_field(out: &mut Vec<u8>, f: GcField) -> Result<()> {
    match f.storage {
        Storage::Val(v) => emit_val_type(out, v)?,
        Storage::I8 => out.push(0x78),
        Storage::I16 => out.push(0x77),
    }
    out.push(u8::from(f.mutable));
    Ok(())
}

/// Parse `(param …)* (result …)*` into a signature. When `names` is given, each param's
/// optional `$id` is recorded there (index-aligned with the local index space).
fn parse_sig(
    items: &[Sexpr],
    type_names: &[Option<String>],
    mut names: Option<&mut Vec<Option<String>>>,
) -> Result<Sig> {
    let mut sig = Sig::default();
    // A **type use** has a fixed clause order (§6.4.4): `(type x)?` then `(param …)*` then
    // `(result …)*`. Anything else is malformed *text*.
    //
    // This used to collect `param`/`result` in whatever order they appeared and ignore `type`
    // entirely, so `(block (result i32) (param i32))` assembled — and the resulting module was
    // then refused by the *validator* as a stack-height mismatch. 41 assertions across
    // `block`/`if`/`loop`/`call_indirect`/`func` were being rejected at the wrong stage for that
    // one reason. Other clause kinds (`export`, `import`, `local`, and the body itself) are still
    // skipped, because `parse_sig` is handed a whole `(func …)` field list as well as a bare
    // block type; only the *relative* order of the type-use clauses is enforced here.
    let (mut seen_type, mut seen_param, mut seen_result) = (false, false, false);
    for item in items {
        match item.keyword() {
            Some("type") if is_type_use_clause(item) => {
                if seen_type || seen_param || seen_result {
                    return Err(Error::UnexpectedToken);
                }
                seen_type = true;
            }
            Some("param") => {
                if seen_result {
                    return Err(Error::UnexpectedToken);
                }
                seen_param = true;
                let l = want_list(item)?;
                // `(param $x i32)` names one; `(param i32 i32)` is an anonymous run.
                if l.len() >= 2 && is_id(&l[1]) {
                    sig.params.push(parse_val_type(nth(l, 2)?, type_names)?);
                    if let Some(n) = names.as_deref_mut() {
                        n.push(l[1].as_atom().map(ToString::to_string));
                    }
                } else {
                    for t in &l[1..] {
                        sig.params.push(parse_val_type(t, type_names)?);
                        if let Some(n) = names.as_deref_mut() {
                            n.push(None);
                        }
                    }
                }
            }
            Some("result") => {
                seen_result = true;
                for t in &want_list(item)?[1..] {
                    sig.results.push(parse_val_type(t, type_names)?);
                }
            }
            _ => {}
        }
    }
    Ok(sig)
}

/// Is this `(type …)` a **type use** — a reference to a declared type inside a signature or block
/// type — rather than a module-level `(type $x (func …))` *definition*?
///
/// The two are told apart by shape: a use is `(type x)` with exactly the index or name, while a
/// definition carries the composite type as a further list. `parse_sig` is handed a `(func …)` field
/// list, which never contains a definition, but it is also handed slices assembled elsewhere — so
/// this is checked rather than assumed.
fn is_type_use_clause(item: &Sexpr) -> bool {
    item.as_list().is_some_and(|l| l.len() == 2 && !l[1].as_list().is_some())
}

/// Read an inline `(import "module" "name")` clause, if present.
fn find_import(items: &[Sexpr]) -> Result<Option<ImportRef>> {
    for s in items {
        if eq_kw(s, "import") {
            let l = want_list(s)?;
            return Ok(Some(ImportRef {
                module: want_str(nth(l, 1)?)?.to_vec(),
                name: want_str(nth(l, 2)?)?.to_vec(),
            }));
        }
    }
    Ok(None)
}

/// Collect inline `(export "name")` clauses.
fn find_exports(items: &[Sexpr]) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for s in items {
        if eq_kw(s, "export") {
            out.push(want_str(nth(want_list(s)?, 1)?)?.to_vec());
        }
    }
    Ok(out)
}

/// A segment's **bare** `memidx` / `tableidx` — the abbreviated `memuse` / `tableuse` of
/// §6.6.11–§6.6.12. Returns the resolved index and advances past it, or leaves `j` alone.
///
/// 🔴 **Both segment parsers read only the parenthesised form** (`(memory 0)`, `(table 0)`)
/// until 2026-09-17, and the bare spelling is equally valid — wasmtime accepts both. The two
/// failures were not the same kind:
///
/// * `(data 0 (i32.const 0) "x")` fell through to the datastring loop and came back `BadForm`.
///   A false rejection: the safe direction, and still wrong.
/// * `(elem 0 (i32.const 1) $f)` **assembled a DIFFERENT SEGMENT.** With nothing consuming the
///   `0`, no offset was recognised either, so an *active* segment at offset 0 holding one
///   funcref became a *passive* segment holding three items — `ref.func 0`, the offset
///   expression itself, and `ref.func $f`. It was caught only because an `i32.const` cannot be
///   a `funcref`, and the validator then reported `TypeMismatch`: an error naming neither the
///   stage nor the thing that was wrong. ⚠️ **A spelling the parser does not know is not
///   automatically refused — it can be silently re-read as something else**, which is the
///   emitter mechanism of T10a arriving from the parser's side.
///
/// ⚠️⚠️ **The ambiguity is real, and a DIGIT is what resolves it.** At this position a bare atom
/// can also be `func`, `funcref`, `declare` or any reftype keyword — every one of which
/// introduces a PASSIVE segment's element list, not a table index. `(elem func $f)` read as
/// "table `func`" would be this same defect pointing the other way. Accepting only an atom that
/// starts with a digit excludes all of them by construction, rather than by a keyword list that
/// a future reftype spelling could outgrow.
///
/// 🔒 **A `$name` here is REFUSED, deliberately, and that is a disagreement traced rather than
/// assumed.** A first cut resolved names too, which made wasmrt accept
/// `(data $seg $m (i32.const 0) "x")` — and **wasmtime refuses it** ("expected `(`"), because
/// its parser takes a bare index in this slot but a named one only parenthesised. Neither the
/// spec's own testsuite nor the wasmtk corpus spells a named bare use-index anywhere, so
/// accepting it would be permissiveness with nothing behind it, and over-acceptance is the
/// direction that cannot be noticed afterwards. `(memory $m)` / `(table $t)` remain the way to
/// name one. The `id?` slot is already taken by the caller's `opt_name`, so `(data $m …)` names
/// the SEGMENT — checked against wasmtime, which agrees.
fn opt_bare_use_index(items: &[Sexpr], j: &mut usize) -> Result<u32> {
    let Some(a) = items.get(*j).and_then(Sexpr::as_atom) else {
        return Ok(0);
    };
    if !a.starts_with(|c: char| c.is_ascii_digit()) {
        return Ok(0);
    }
    let idx = parse_index(&items[*j])?;
    *j += 1;
    Ok(idx)
}

/// `(pagesize N)` — custom-page-sizes. `N` must be a nonzero power of two or the TEXT is malformed
/// ("invalid custom page size"); whether it is one of the two sizes the proposal allows (1, 64 KiB)
/// is the validator's question, so `(pagesize 4)` assembles and is refused there, as the suite has it.
///
/// ⚠️ History: until 2026-09-17 this clause was parsed and SILENTLY THROWN AWAY — `(memory 1
/// (pagesize 1))` assembled byte-identical to `(memory 1)`, a 1-byte-page memory silently given 64 KiB
/// pages. X1 then refused it outright (`Error::Unsupported`) until the feature existed; Track P
/// (2026-09-19) replaced that refusal with this.
fn parse_page_size(clause: &Sexpr) -> Result<u32> {
    let [_, n] = want_list(clause)? else {
        return Err(Error::BadForm);
    };
    let n = parse_u64_str(want_atom(n)?)?;
    if !n.is_power_of_two() {
        return Err(Error::InvalidPageSize);
    }
    Ok(n.trailing_zeros())
}

/// A memory type, read from `items[*j..]` to the END of the form:
/// `addrtype? min max? shared? (pagesize N)?` — or, for a definition, the inline-data abbreviation
/// `addrtype? (pagesize N)? (data "…"*)`, returned with its bytes. Anything left over is malformed.
///
/// 🔒 **The ONE reader of a memory type.** A memory type is spelled in a definition and in an
/// import descriptor, and each used to read it itself — which is how `(pagesize N)` came to be
/// dropped by both (X1), and how `(memory 1 2 3)` still assembled on 2026-09-19 with the `3`
/// ignored. A clause the parser accepts and the module does not carry is the T10a mechanism; one
/// reader that owns every clause, and refuses what it does not own, is the structural answer.
fn parse_memory_type(items: &[Sexpr], j: &mut usize) -> Result<(MemoryDef, Option<Vec<u8>>)> {
    // ⚠️ The index type is read BEFORE the inline-data branch, because `(memory i64 (data …))` is
    // legal (`float_memory64.wast` failed whole on a branch that hardcoded `is64: false`).
    let mut is64 = false;
    if items.get(*j).is_some_and(|s| eq_atom(s, "i64")) {
        is64 = true;
        *j += 1;
    } else if items.get(*j).is_some_and(|s| eq_atom(s, "i32")) {
        *j += 1;
    }
    let is_page = |k: usize| items.get(k).is_some_and(|s| eq_kw(s, "pagesize"));
    let is_data = |k: usize| items.get(k).is_some_and(|s| eq_kw(s, "data"));

    // The inline-data abbreviation: the data sizes the memory, in pages of the stated size.
    if is_data(*j) || (is_page(*j) && is_data(*j + 1)) {
        let mut page_size_log2 = None;
        if is_page(*j) {
            page_size_log2 = Some(parse_page_size(&items[*j])?);
            *j += 1;
        }
        let mut bytes = Vec::new();
        for s in &want_list(&items[*j])?[1..] {
            bytes.extend_from_slice(want_str(s)?);
        }
        *j += 1;
        if *j != items.len() {
            return Err(Error::UnexpectedToken);
        }
        // ⚠️ `div_ceil(65536)` stood here: `(memory (pagesize 1) (data "xyz"))` is THREE pages.
        let e = page_size_log2.unwrap_or(crate::module::DEFAULT_PAGE_SIZE_LOG2);
        let pages = (bytes.len() as u64).div_ceil(1u64 << e);
        let m = MemoryDef { min: pages, max: Some(pages), shared: false, is64, page_size_log2 };
        return Ok((m, Some(bytes)));
    }

    let min = match items.get(*j) {
        Some(s) => parse_u64_str(want_atom(s)?)?,
        None => 0,
    };
    *j += 1;
    let mut max = None;
    if let Some(a) = items.get(*j).and_then(Sexpr::as_atom) {
        if a != "shared" {
            max = Some(parse_u64_str(a)?);
            *j += 1;
        }
    }
    let shared = items.get(*j).is_some_and(|s| eq_atom(s, "shared"));
    if shared {
        *j += 1;
    }
    let mut page_size_log2 = None;
    if is_page(*j) {
        page_size_log2 = Some(parse_page_size(&items[*j])?);
        *j += 1;
    }
    if *j < items.len() {
        return Err(Error::UnexpectedToken);
    }
    Ok((MemoryDef { min, max, shared, is64, page_size_log2 }, None))
}

/// Skip over inline `(import …)` / `(export …)` clauses.
fn skip_inline_clauses(items: &[Sexpr], j: &mut usize) {
    while items
        .get(*j)
        .is_some_and(|s| eq_kw(s, "import") || eq_kw(s, "export"))
    {
        *j += 1;
    }
}

/// A folded `(i32.const 0)` offset expression, for the inline data/elem shorthands.
/// The implied `offset` const-expr of an inline `(data …)` / `(elem …)` shorthand.
///
/// ⚠️ **Takes the index type**, because an offset expression is typed by the memory or table it
/// targets: `i64.const 0` on a 64-bit one, `i32.const 0` otherwise. It hardcoded `i32` until
/// 2026-08-19, so `(memory i64 (data "…"))` assembled a module whose own data segment did not
/// type-check — the shorthand produced an offset the memory could not accept.
fn zero_offset_of(is64: bool) -> Vec<Sexpr> {
    vec![Sexpr::list(vec![
        Sexpr::Atom(if is64 { "i64.const" } else { "i32.const" }.to_string()),
        Sexpr::Atom("0".to_string()),
    ])]
}

fn parse_func_field(items: &[Sexpr], annots: &[Annot], b: &mut ModuleBuild) -> Result<()> {
    let mut j = 1;
    let name = opt_name(items, &mut j);
    let import = find_import(items)?;
    let idx = b.func_names.len() as u32; // func-space index (imports take the low ones)
    for name in find_exports(items)? {
        b.exports.push(ExportDef {
            name,
            kind: 0,
            index: idx,
        });
    }

    let mut type_ref = None;
    let mut local_names: Vec<Option<String>> = Vec::new();
    let mut locals: Vec<V> = Vec::new();
    let mut sig = Sig::default();

    // Header clauses: import/export, then the **type use** `(type x)? (param …)* (result …)*`, then
    // `(local …)*`. The first form that is none of those begins the body.
    //
    // The order is FIXED by the text format (§6.4.4 / §6.6.4) and was not enforced — this is the third
    // copy of the same loop to have that gap, after block types and `call_indirect`. So
    // `(func (result i32) (param i32) …)` assembled and the *validator* reported it, and
    // `(func (nop) (local i32))` — a declaration after the body has started — assembled too.
    // `seen_body` is never set — see the note further down on why that rule had to be withdrawn.
    // It is kept, and kept read, so the shape of the check survives for whoever can finish it.
    let (mut seen_param, mut seen_result, mut seen_local, seen_body) = (false, false, false, false);
    let mut k = j;
    while k < items.len() {
        match items[k].keyword() {
            Some("import" | "export") => {}
            Some("type") => {
                if type_ref.is_some() || seen_param || seen_result || seen_local || seen_body {
                    return Err(Error::UnexpectedToken);
                }
                type_ref = Some(resolve_by_name(
                    &b.type_names,
                    nth(want_list(&items[k])?, 1)?,
                )?);
            }
            Some("param" | "result") => {
                let is_param = items[k].keyword() == Some("param");
                if seen_local || seen_body || (is_param && seen_result) {
                    return Err(Error::UnexpectedToken);
                }
                if is_param {
                    seen_param = true;
                } else {
                    seen_result = true;
                }
                let s = parse_sig(
                    core::slice::from_ref(&items[k]),
                    &b.type_names,
                    Some(&mut local_names),
                )?;
                sig.params.extend(s.params);
                sig.results.extend(s.results);
            }
            Some("local") => {
                seen_local = true;
                let l = want_list(&items[k])?;
                if l.len() >= 2 && is_id(&l[1]) {
                    locals.push(parse_val_type(nth(l, 2)?, &b.type_names)?);
                    local_names.push(l[1].as_atom().map(ToString::to_string));
                } else {
                    for t in &l[1..] {
                        locals.push(parse_val_type(t, &b.type_names)?);
                        local_names.push(None);
                    }
                }
            }
            _ => break,
        }
        k += 1;
    }

    // ⚠️ **"No declaration after the body begins" is NOT checkable by keyword here**, and the attempt
    // is recorded because it looked obviously right. In **flat** instruction form each immediate is its
    // own top-level item, so `select (result i32)` and `call_indirect (type $t)` put a `result`/`type`
    // form directly in `items[k..]` — indistinguishable, by keyword alone, from a misplaced
    // declaration. Scanning for them rejected valid modules in `select.wast`, `stack.wast` and
    // `call_indirect.wast`. Deciding it needs the body's instruction structure, which this layer does
    // not have. `(func (nop) (local i32))` therefore still assembles; logged in `known-issues.md`.
    let _ = seen_body;

    // An inline signature given ALONGSIDE a `(type $t)` must match it — they are not alternatives.
    // Without this the explicit clauses were kept while the type index was also used, so the module
    // could mean something the text did not say. The suite calls it "inline function type".
    if let Some(ti) = type_ref {
        if seen_param || seen_result {
            match b.types.get(ti as usize) {
                Some(TypeDef::Func(d)) => {
                    if d.params != sig.params || d.results != sig.results {
                        return Err(Error::TypeUseMismatch);
                    }
                }
                // Possibly an implicit type — see `ModuleBuild::deferred_type_uses`.
                None => b.deferred_type_uses.push((ti, sig.clone())),
                Some(_) => return Err(Error::TypeUseMismatch),
            }
        }
    }

    // An explicit `(type $t)` with no inline params/results takes its signature from the
    // referenced type, so locals resolve against the right param count.
    if let Some(ti) = type_ref {
        if sig.params.is_empty() && sig.results.is_empty() {
            if let Some(s) = func_sig_at(&b.types, ti) {
                sig = s.clone();
                // The referenced type's params are unnamed, and they occupy the FIRST local
                // indices — so every declared local name shifts up by the param count.
                //
                // ⚠️⚠️ This used to be `if local_names.is_empty() { … }`, which is exactly
                // wrong whenever the function declares a local: the `(local $var …)` clause
                // has ALREADY pushed its name by the time this runs, so the placeholders were
                // never inserted and `$var` resolved to **index 0 — the parameter**.
                //
                // `(func $g (type $sig) (local $var i32) (local.get $var))` therefore returned
                // the ARGUMENT instead of the fresh zero local. **Silent wrong output**: no
                // error, no diagnostic, just a different number — the class this project ranks
                // worst, sitting in `func.wast` behind an "8 failures" line.
                let mut names = alloc::vec![None; sig.params.len()];
                names.append(&mut local_names);
                local_names = names;
            }
        }
    }

    // Parameters and locals share ONE index space, so a name may not repeat across the two.
    check_unique_names(&local_names)?;

    if let Some(r) = import {
        // An imported function is a SIGNATURE and nothing else: no locals, no body. Both used to be
        // accepted and thrown away — `(func (import "a" "b") nop)` assembled — where wasm-tools
        // refuses them; and `(exact (type 0))` was dropped the same way, importing `(func)`.
        if seen_local {
            return Err(Error::UnexpectedToken);
        }
        // `(func (import …) (exact <typeuse>))` — the exact form carries the whole signature, so
        // it cannot ALSO have one outside it.
        if items.get(k).is_some_and(|s| s.keyword() == Some("exact")) {
            if type_ref.is_some() || seen_param || seen_result {
                return Err(Error::UnexpectedToken);
            }
            let ti = exact_import_sig(items, k, b)?.ok_or(Error::UnexpectedToken)?;
            b.func_imports.push(ImportedFunc { r, type_index: ti, exact: true });
            b.import_order.push(ImportKind::Func);
            b.func_names.push(name);
            return Ok(());
        }
        after_import_signature(items.get(k))?;
        let ti = type_ref.unwrap_or_else(|| intern_sig_outside_rec(&mut b.types, &b.rec_groups, sig));
        b.func_imports.push(ImportedFunc { r, type_index: ti, exact: false });
        b.import_order.push(ImportKind::Func);
    } else {
        b.funcs.push(Func {
            type_ref,
            sig,
            local_names,
            locals,
            body: items[k..].to_vec(),
            // Rebased onto `body`. Those before `k` stand in the header, which `annot::plan`
            // has already refused or recorded.
            body_annots: annots
                .iter()
                .filter(|a| a.before >= k)
                .map(|a| Annot { before: a.before - k, ..a.clone() })
                .collect(),
        });
    }
    b.func_names.push(name);
    Ok(())
}

/// Classify a mnemonic `Op::from_text_name` could not resolve.
///
/// Returns [`Error::UnimplementedInstr`] when the name is a **real instruction from a proposal
/// wasmrt does not implement**, and [`Error::UnknownInstr`] — a malformed-input verdict —
/// otherwise. Only the assembler can draw this line, because only here is the full set of
/// mnemonics known; downstream all that survives is an error name.
///
/// ✅ **wasmrt only became able to draw it on 2026-08-14**, when tail calls made every
/// **in-scope** proposal implemented. Before that, “a mnemonic we do not know” could always have
/// been an in-scope gap, and conservatism was the only honest answer.
///
/// 🔒 **OVER-INCLUSIVE BY CONSTRUCTION.** Every rule below errs toward “ours”: a whole-family
/// substring rather than an enumeration, so a member nobody listed still lands on the safe side.
/// Adding a name here costs a skip; omitting one banks a **false pass**, which is unrecoverable.
fn classify_unknown_mnemonic(name: &str) -> Error {
    // custom-descriptors — every instruction it adds carries `desc` in its name
    // (`ref.get_desc`, `ref.cast_desc`, `ref.cast_desc_eq`, `struct.new_desc`,
    // `br_on_cast_desc`/`_fail`/`_eq`/`_eq_fail` …). Matched as a substring on purpose:
    // enumerating them invites exactly the omission this function must not make.
    // ⚠️ Tracks D3/D4 built the current spellings (2026-09-19), which `Op::from_text_name`
    // resolves before this function is reached; the rule stays for the proposal's earlier names
    // (`ref.cast_desc`, `br_on_cast_desc` …), which are real and unbuilt.
    if name.contains("desc") {
        return Error::UnimplementedInstr;
    }
    // stack-switching (phase 3) — continuations. Not in the vendored corpus, so this costs no
    // skip today; it is listed because it is a real proposal wasmrt has not built, and a module
    // using it must be scored as OUR gap rather than as malformed input.
    if name.starts_with("cont.")
        || matches!(name, "resume" | "resume_throw" | "resume_throw_ref" | "suspend" | "switch")
    {
        return Error::UnimplementedInstr;
    }
    // ⚠️ wide-arithmetic's four mnemonics were listed here until 2026-09-17. Track W implemented
    // them, so the entry came out the same day — leaving it would make the assembler refuse four
    // instructions the decoder, validator and interpreter all handle, and score that refusal as
    // OUR gap, which is how a finished feature comes to look unfinished.
    // ⚠️ `any.convert_extern` / `extern.convert_any` were listed here until 2026-08-20. They are
    // implemented now (S1 — the tagged `Value` representation), so leaving them would have made
    // the assembler refuse an instruction the rest of the engine handles, and score it as OUR gap.
    // **An entry here is a claim that the gap is still open; it has to be removed the day it
    // closes**, and `wast::tests::every_skip_records_a_reason` depends on this list too.
    Error::UnknownInstr
}

/// §6.5.2 label repetition: consume an optional `$id` after `else` / `end` that repeats the
/// label of the block being closed. Returns how many items were consumed (0 or 1).
///
/// ⚠️ **The repeat is CHECKED against the enclosing label, not merely skipped.** A repeat naming a
/// different block is malformed, and ignoring it would make the whole annotation decorative — its
/// only purpose is to let a human assert *which* block is being closed. `ctx.labels`' last entry
/// is the innermost block, which is the one both `else` and `end` refer to.
fn consume_matching_label(ctx: &Ctx, items: &[Sexpr], at: usize) -> Result<usize> {
    let Some(id) = items
        .get(at)
        .and_then(Sexpr::as_atom)
        .filter(|a| a.starts_with('$'))
    else {
        return Ok(0);
    };
    match ctx.labels.last() {
        Some(Some(want)) if want == id => Ok(1),
        _ => Err(Error::UnknownLabel),
    }
}

fn parse_memory_field(items: &[Sexpr], b: &mut ModuleBuild) -> Result<()> {
    let mut j = 1;
    let name = opt_name(items, &mut j);
    let import = find_import(items)?;
    let mi = b.mem_names.len() as u32;
    for name in find_exports(items)? {
        b.exports.push(ExportDef {
            name,
            kind: 2,
            index: mi,
        });
    }
    skip_inline_clauses(items, &mut j);
    let (m, data) = parse_memory_type(items, &mut j)?;
    if let Some(bytes) = data {
        // An imported memory has no contents of its own to initialise.
        if import.is_some() {
            return Err(Error::UnexpectedToken);
        }
        let is64 = m.is64;
        b.memories.push(m);
        b.mem_names.push(name);
        b.datas.push(DataSeg {
            mem_index: mi,
            offset: Some(zero_offset_of(is64)),
            bytes,
        });
        b.data_names.push(None);
        return Ok(());
    }
    if let Some(r) = import {
        b.mem_imports.push(ImportedMemory { r, m });
        b.import_order.push(ImportKind::Mem);
    } else {
        b.memories.push(m);
    }
    b.mem_names.push(name);
    Ok(())
}

fn parse_global_field(items: &[Sexpr], b: &mut ModuleBuild) -> Result<()> {
    let mut j = 1;
    let name = opt_name(items, &mut j);
    let import = find_import(items)?;
    for name in find_exports(items)? {
        b.exports.push(ExportDef {
            name,
            kind: 3,
            index: b.global_names.len() as u32,
        });
    }
    skip_inline_clauses(items, &mut j);
    let ty_form = nth(items, j)?;
    j += 1;
    let (valtype, mutable) = if eq_kw(ty_form, "mut") {
        (
            parse_val_type(nth(want_list(ty_form)?, 1)?, &b.type_names)?,
            true,
        )
    } else {
        (parse_val_type(ty_form, &b.type_names)?, false)
    };
    if let Some(r) = import {
        b.global_imports.push(ImportedGlobal {
            r,
            valtype,
            mutable,
        });
        b.import_order.push(ImportKind::Global);
    } else {
        b.globals.push(GlobalDef {
            valtype,
            mutable,
            init: items[j..].to_vec(),
        });
    }
    b.global_names.push(name);
    Ok(())
}

/// Parse `min [max]` table limits starting at `j`.
/// `[i32|i64] <min> [<max>]` — a table type’s index type and limits.
///
/// ⚠️ **The limits are `u64` and the index type is read here, both since table64 landed
/// (T13, 2026-08-19).** They were `u32` with no index keyword, so `(table i64 0 0x1_0000_0000
/// funcref)` failed with `BadNumber` — **63 modules across the corpus, and the single largest
/// cause of the whole skip total.** The error named the symptom (“a number that does not fit”);
/// the cause was one missing feature.
/// Consume a table's optional **index type** (`i64` / `i32`, table64), returning whether it is
/// 64-bit. Defaults to `i32`, which is what a table written without one is.
///
/// ⚠️ Its own function because the index type precedes the element type in **every** table form,
/// and the inline-`(elem …)` form did not consume it: `(table $t i64 funcref (elem $f))` read
/// `i64` AS the element type and emitted `0x7e` where a reftype belongs — a table section no
/// decoder accepts, ours included, which is how `call_indirect64.wast` read "byte is not a
/// defined value type" and never built.
fn parse_table_index_type(items: &[Sexpr], j: &mut usize) -> bool {
    if items.get(*j).is_some_and(|s| eq_atom(s, "i64")) {
        *j += 1;
        return true;
    }
    if items.get(*j).is_some_and(|s| eq_atom(s, "i32")) {
        *j += 1;
    }
    false
}

fn parse_table_limits(items: &[Sexpr], j: &mut usize) -> Result<(u64, Option<u64>, bool)> {
    let is64 = parse_table_index_type(items, j);
    let min = parse_u64_str(want_atom(nth(items, *j)?)?)?;
    *j += 1;
    let mut max = None;
    if let Some(a) = items.get(*j).and_then(Sexpr::as_atom) {
        if a.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            max = Some(parse_u64_str(a)?);
            *j += 1;
        }
    }
    Ok((min, max, is64))
}

fn parse_table_field(items: &[Sexpr], b: &mut ModuleBuild) -> Result<()> {
    let mut j = 1;
    let name = opt_name(items, &mut j);
    let import = find_import(items)?;
    let ti = b.table_names.len() as u32;
    for name in find_exports(items)? {
        b.exports.push(ExportDef {
            name,
            kind: 1,
            index: ti,
        });
    }
    skip_inline_clauses(items, &mut j);

    // `(table $t funcref (elem $f …))` — an inline element segment sizes the table.
    if let Some(pos) = items[j..].iter().position(|s| eq_kw(s, "elem")) {
        // Absolute, because `parse_table_index_type` may advance `j` past an `i64`/`i32`.
        let elem_at = j + pos;
        let is64 = parse_table_index_type(items, &mut j);
        let elem_type = parse_ref_type(nth(items, j)?, &b.type_names)?;
        let entries: Vec<Vec<Sexpr>> = want_list(&items[elem_at])?[1..]
            .iter()
            .map(|s| {
                if s.as_list().is_some() {
                    vec![s.clone()]
                } else {
                    vec![Sexpr::list(vec![
                        Sexpr::Atom("ref.func".to_string()),
                        s.clone(),
                    ])]
                }
            })
            .collect();
        // Whether every entry is expressible as a bare function INDEX, which is all the
        // funcidx shorthand can encode. An entry written as an atom always is; a list is only
        // if it is exactly `(ref.func …)`.
        let all_plain_ref_func = want_list(&items[elem_at])?[1..].iter().all(|s| {
            s.as_atom().is_some()
                || s.as_list()
                    .is_some_and(|l| matches!(l.first().and_then(Sexpr::as_atom), Some("ref.func")))
        });
        let n = entries.len() as u32;
        b.tables.push(TableDef {
            min: u64::from(n),
            max: Some(u64::from(n)),
            is64,
            elem: elem_type,
            init: None, // the inline `(elem …)` fills it instead
        });
        b.table_names.push(name);
        b.elems.push(ElemDef {
            table_index: ti,
            // ⚠️ The offset takes the TABLE's index type. This was hardcoded 32-bit behind a note
            // saying an inline `(elem …)` on a 64-bit table "is not expressible — the shorthand
            // carries no index type". `call_indirect64.wast` opens with
            // `(table $t64 i64 funcref (elem $const-i32))`, so it is expressible and was written;
            // the synthesized `i32.const 0` then failed validation and the file never built.
            // 🎓 A scope note is a hypothesis about a cause, and it is the one nobody re-measures.
            offset: Some(zero_offset_of(is64)),
            elem_type,
            items: entries,
            declarative: false,
            // The entries above are already const-expr forms; this flag only picks the
            // ENCODING. The funcidx shorthand (forms 0–3) denotes `funcref` and nothing
            // else, so a table of `(ref null $t)` — or of any non-`funcref` element type —
            // must use the expression family or `emit_elem_segment` refuses it outright.
            //
            // ⚠️ **…and so must a `funcref` table whose entries are not all `ref.func`.** The
            // shorthand can only encode function INDICES, so `(elem (ref.func $f)
            // (ref.null func) (ref.func $g))` — valid, and in `elem.wast` — died with
            // `BadNumber` trying to read `func` as an index. The element TYPE was the wrong
            // question: what decides the encoding is whether every entry is expressible as an
            // index, and `ref.null` never is.
            use_exprs: elem_type != V::FUNCREF || !all_plain_ref_func,
        });
        b.elem_names.push(None);
        return Ok(());
    }

    let (min, max, is64) = parse_table_limits(items, &mut j)?;
    let elem = parse_ref_type(nth(items, j)?, &b.type_names)?;
    // Anything after the element type is the initializer expression (function-references).
    // Reading it is what stops the assembler emitting a table of nulls for
    // `(table 3 funcref (ref.func $f))`.
    let rest = &items[j + 1..];
    let init = if rest.is_empty() {
        None
    } else {
        Some(rest.to_vec())
    };
    let t = TableDef {
        min,
        max,
        is64,
        elem,
        init,
    };
    if let Some(r) = import {
        // The binary format has no initializer on an *imported* table — the exporter
        // supplies the contents. Refuse rather than drop it, which is the bug this whole
        // change exists to remove.
        if t.init.is_some() {
            return Err(Error::BadModuleField);
        }
        b.table_imports.push(ImportedTable { r, t });
        b.import_order.push(ImportKind::Table);
    } else {
        b.tables.push(t);
    }
    b.table_names.push(name);
    Ok(())
}

/// What may follow an imported function's (or a tag's) signature: nothing. (An EXACT function
/// import is recognised before this is reached — see [`exact_import_sig`] — so `(exact …)` here is
/// on a tag, or mixed with an ordinary type use, and is as malformed as anything else.)
fn after_import_signature(next: Option<&Sexpr>) -> Result<()> {
    match next {
        None => Ok(()),
        Some(_) => Err(Error::UnexpectedToken),
    }
}

/// `(exact <typeuse>)` — custom-descriptors' EXACT function import — as the LAST item of `items`,
/// starting at `j`: the type index it names (interning an inline signature), or `None` if `items[j]`
/// is not an `(exact …)` form. Anything after it, or left inside it, is malformed.
///
/// One reader for both spellings of a function import (`(import … (func (exact …)))` and
/// `(func (import …) (exact …))`), so the two cannot disagree about what exact means.
fn exact_import_sig(items: &[Sexpr], j: usize, b: &mut ModuleBuild) -> Result<Option<u32>> {
    let Some(ex) = items.get(j).filter(|s| s.keyword() == Some("exact")) else {
        return Ok(None);
    };
    let inner = want_list(ex)?;
    let mut k = 1;
    let (type_ref, sig) =
        read_type_use(&b.types, &b.type_names, inner, &mut k, true, &mut b.deferred_type_uses)?;
    if k != inner.len() || j + 1 != items.len() {
        return Err(Error::UnexpectedToken);
    }
    Ok(Some(type_ref.unwrap_or_else(|| intern_sig_outside_rec(&mut b.types, &b.rec_groups, sig))))
}

/// The signature of a function IMPORT or a tag: its inline `(import …)` / `(export …)` clauses
/// (a tag field's; an import descriptor has none), then a type use, then NOTHING — the rules are
/// [`read_type_use`]'s, and anything left over is malformed rather than skipped.
///
/// ⚠️ This was a fourth, rule-free copy of the type-use loop ending in `_ => {}`: it enforced no
/// clause order, never checked a `(type x)` against the clauses beside it (and dropped them),
/// and skipped any other form in silence. wasm-tools refuses all of those.
fn parse_tag_type(items: &[Sexpr], b: &mut ModuleBuild) -> Result<u32> {
    let mut j = 0;
    while items.get(j).is_some_and(|s| matches!(s.keyword(), Some("import" | "export"))) {
        j += 1;
    }
    let (type_ref, sig) =
        read_type_use(&b.types, &b.type_names, items, &mut j, true, &mut b.deferred_type_uses)?;
    after_import_signature(items.get(j))?;
    Ok(type_ref.unwrap_or_else(|| intern_sig_outside_rec(&mut b.types, &b.rec_groups, sig)))
}

fn parse_tag_field(items: &[Sexpr], b: &mut ModuleBuild) -> Result<()> {
    let mut j = 1;
    let name = opt_name(items, &mut j);
    let import = find_import(items)?;
    for name in find_exports(items)? {
        b.exports.push(ExportDef {
            name,
            kind: 4,
            index: b.tag_names.len() as u32,
        });
    }
    let ti = parse_tag_type(&items[j..], b)?;
    if let Some(r) = import {
        b.tag_imports.push(ImportedTag { r, type_index: ti });
        b.import_order.push(ImportKind::Tag);
    } else {
        b.tags.push(ti);
    }
    b.tag_names.push(name);
    Ok(())
}

fn parse_import_field(items: &[Sexpr], b: &mut ModuleBuild) -> Result<()> {
    let r = ImportRef {
        module: want_str(nth(items, 1)?)?.to_vec(),
        name: want_str(nth(items, 2)?)?.to_vec(),
    };
    let desc = want_list(nth(items, 3)?)?;
    let kw = want_atom(nth(desc, 0)?)?;
    let mut j = 1;
    let name = opt_name(desc, &mut j);
    match kw {
        "func" => {
            let (ti, exact) = match exact_import_sig(desc, j, b)? {
                Some(ti) => (ti, true),
                // The same (type $t) | (param…)(result…) shape a tag has.
                None => (parse_tag_type(&desc[j..], b)?, false),
            };
            b.func_imports.push(ImportedFunc { r, type_index: ti, exact });
            b.import_order.push(ImportKind::Func);
            b.func_names.push(name);
        }
        "memory" => {
            // The same reader as a memory definition — see `parse_memory_type`.
            let (m, data) = parse_memory_type(desc, &mut j)?;
            if data.is_some() {
                return Err(Error::UnexpectedToken);
            }
            b.mem_imports.push(ImportedMemory { r, m });
            b.import_order.push(ImportKind::Mem);
            b.mem_names.push(name);
        }
        "global" => {
            let ty_form = nth(desc, j)?;
            let (valtype, mutable) = if eq_kw(ty_form, "mut") {
                (
                    parse_val_type(nth(want_list(ty_form)?, 1)?, &b.type_names)?,
                    true,
                )
            } else {
                (parse_val_type(ty_form, &b.type_names)?, false)
            };
            b.global_imports.push(ImportedGlobal {
                r,
                valtype,
                mutable,
            });
            b.import_order.push(ImportKind::Global);
            b.global_names.push(name);
        }
        "table" => {
            let (min, max, is64) = parse_table_limits(desc, &mut j)?;
            // The FOURTH copy of the table element position, and it had the same defect as the
            // other three — found by the test above rather than by reading, because
            // `(import "m" "t" (table 1 f32))` takes this parser and `(table (import …) 1 f32)`
            // takes `parse_table_field`.
            let elem = parse_ref_type(nth(desc, j)?, &b.type_names)?;
            b.table_imports.push(ImportedTable {
                r,
                t: TableDef {
                    min,
                    max,
                    is64,
                    elem,
                    init: None,
                },
            });
            b.import_order.push(ImportKind::Table);
            b.table_names.push(name);
        }
        "tag" => {
            let ti = parse_tag_type(&desc[j..], b)?;
            b.tag_imports.push(ImportedTag { r, type_index: ti });
            b.import_order.push(ImportKind::Tag);
            b.tag_names.push(name);
        }
        _ => return Err(Error::BadModuleField),
    }
    Ok(())
}

fn parse_elem_field(items: &[Sexpr], b: &mut ModuleBuild) -> Result<()> {
    let mut j = 1;
    let name = opt_name(items, &mut j);
    let table_index;
    let mut offset: Option<Vec<Sexpr>> = None;
    let mut declarative = false;
    let mut elem_type = V::FUNCREF;

    if items.get(j).is_some_and(|s| eq_atom(s, "declare")) {
        declarative = true;
        j += 1;
    }
    if let Some(s) = items.get(j).filter(|s| eq_kw(s, "table")) {
        table_index = resolve_by_name(&b.table_names, nth(want_list(s)?, 1)?)?;
        j += 1;
    } else {
        // The abbreviated `tableuse`: `(elem 0 (i32.const 1) $f)`.
        table_index = opt_bare_use_index(items, &mut j)?;
    }
    if let Some(s) = items.get(j) {
        if eq_kw(s, "offset") {
            offset = Some(want_list(s)?[1..].to_vec());
            j += 1;
        } else if s.as_list().is_some() && !eq_kw(s, "item") && !is_ref_type_form(s) {
            // A bare folded const-expr is the offset: `(elem (i32.const 0) $f)`.
            offset = Some(vec![s.clone()]);
            j += 1;
        }
    }
    // An explicit element type may follow (`funcref`, `(ref …)`).
    if let Some(s) = items.get(j) {
        // ⚠️ `is_type_keyword`, not `string_to_val_type` — an obsolete keyword still occupies a
        // type position, and must reach `parse_val_type` to be refused there. Routing it onward
        // makes it a funcidx, and the error becomes `BadNumber`.
        if s.as_atom().is_some_and(is_type_keyword) || is_ref_type_form(s) {
            elem_type = parse_ref_type(s, &b.type_names)?;
            j += 1;
        }
    }
    // The `func` keyword form: `(elem (i32.const 0) func $a $b)`.
    if items.get(j).is_some_and(|s| eq_atom(s, "func")) {
        j += 1;
    }
    let mut entries: Vec<Vec<Sexpr>> = Vec::new();
    // A bare atom entry is the `func $a $b` index shorthand; anything else is an
    // expression, which forces the expression encoding for the whole segment.
    let mut use_exprs = elem_type != V::FUNCREF;
    for s in &items[j..] {
        if eq_kw(s, "item") {
            entries.push(want_list(s)?[1..].to_vec());
            use_exprs = true;
        } else if s.as_list().is_some() {
            entries.push(vec![s.clone()]);
            use_exprs = true;
        } else {
            entries.push(vec![Sexpr::list(vec![
                Sexpr::Atom("ref.func".to_string()),
                s.clone(),
            ])]);
        }
    }
    b.elems.push(ElemDef {
        table_index,
        offset,
        elem_type,
        items: entries,
        declarative,
        use_exprs,
    });
    b.elem_names.push(name);
    Ok(())
}

fn parse_data_field(items: &[Sexpr], b: &mut ModuleBuild) -> Result<()> {
    let mut j = 1;
    let name = opt_name(items, &mut j);
    let mem_index;
    let mut offset: Option<Vec<Sexpr>> = None;
    if let Some(s) = items.get(j).filter(|s| eq_kw(s, "memory")) {
        mem_index = resolve_by_name(&b.mem_names, nth(want_list(s)?, 1)?)?;
        j += 1;
    } else {
        // The abbreviated `memuse`: `(data 0 (i32.const 10) "")`.
        mem_index = opt_bare_use_index(items, &mut j)?;
    }
    if let Some(s) = items.get(j) {
        if eq_kw(s, "offset") {
            offset = Some(want_list(s)?[1..].to_vec());
            j += 1;
        } else if s.as_list().is_some() {
            offset = Some(vec![s.clone()]);
            j += 1;
        }
    }
    let mut bytes = Vec::new();
    for s in &items[j..] {
        bytes.extend_from_slice(want_str(s)?);
    }
    b.datas.push(DataSeg {
        mem_index,
        offset,
        bytes,
    });
    b.data_names.push(name);
    Ok(())
}

// --- Section emission ---------------------------------------------------------

/// Emit the whole module: the header, then each section in its canonical order.
///
/// Everything the caller pre-encoded, because encoding it can grow the type table.
struct Encoded<'a> {
    func_sigs: &'a [u32],
    bodies: &'a [Vec<u8>],
    globals: &'a [GlobalDef],
    global_inits: &'a [Vec<u8>],
    /// Positional with `b.tables`; `None` where the table has no initializer.
    table_inits: &'a [Option<Vec<u8>>],
    elems: &'a [Vec<u8>],
    datas: &'a [DataSeg],
    data_offsets: &'a [Option<Vec<u8>>],
}

/// `func_sigs` and the pre-encoded bodies/const-exprs are produced by the caller *before*
/// this runs — a multi-value block type or an inline `call_indirect` signature interns into
/// the type table while a body is encoded, so the type section is only complete afterwards.
#[allow(clippy::too_many_arguments)]
fn emit_module(
    b: &ModuleBuild,
    func_sigs: &[u32],
    bodies: &[Vec<u8>],
    globals: &[GlobalDef],
    global_inits: &[Vec<u8>],
    table_inits: &[Option<Vec<u8>>],
    elems: &[Vec<u8>],
    datas: &[DataSeg],
    data_offsets: &[Option<Vec<u8>>],
) -> Result<Vec<u8>> {
    let e = Encoded {
        func_sigs,
        bodies,
        globals,
        global_inits,
        table_inits,
        elems,
        datas,
        data_offsets,
    };
    let mut out = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    // 1 — types. The section is a vector of REC GROUPS, not of types: each entry is either a
    // `0x4e`-wrapped group or a bare subtype (a singleton group). The count is therefore the number
    // of groups, and emitting one entry per type — which this did — silently merges every explicit
    // `(rec …)` into singletons and changes what the types *are*.
    if !b.types.is_empty() {
        let mut c = Vec::new();
        // Walk the type index space, emitting an explicit group where one starts and a singleton
        // everywhere else. Counting first because the vector length precedes the entries.
        let mut entries: Vec<(u32, u32, bool)> = Vec::new(); // (start, len, explicit)
        let mut i = 0u32;
        while (i as usize) < b.types.len() {
            match b.rec_groups.iter().find(|(s, _)| *s == i) {
                Some(&(s, len)) if len > 0 => {
                    entries.push((s, len, true));
                    i += len;
                }
                _ => {
                    entries.push((i, 1, false));
                    i += 1;
                }
            }
        }
        uleb(&mut c, entries.len() as u64);
        for (start, len, explicit) in entries {
            if explicit {
                c.push(0x4e);
                uleb(&mut c, u64::from(len));
            }
            for j in start..start + len {
                let i = j as usize;
                // Choose the wrapper from BOTH facts, because they are independent: `0x50` is `sub`
                // (open), `0x4f` is `sub final`, and a bare composite type is the shorthand for
                // "final, no supertype". Keying only on the supertype — as this did — emitted a bare
                // composite for `(sub (struct …))` and thereby marked an *open* type final.
                let sup = b.supers.get(i).copied().flatten();
                let is_final = b.type_finals.get(i).copied().unwrap_or(true);
                if !is_final || sup.is_some() {
                    c.push(if is_final { 0x4f } else { 0x50 });
                    match sup {
                        Some(s) => {
                            uleb(&mut c, 1);
                            uleb(&mut c, u64::from(s));
                        }
                        // Open with no supertype: an empty supertype vector, not an absent wrapper.
                        None => uleb(&mut c, 0),
                    }
                }
                // custom-descriptors: `0x4c describes` then `0x4d descriptor`, between the wrapper and
                // the composite type — dropping either would change the type's IDENTITY.
                let (describes, descriptor) = b.type_links.get(i).copied().unwrap_or((None, None));
                if let Some(x) = describes {
                    c.push(0x4c);
                    uleb(&mut c, u64::from(x));
                }
                if let Some(y) = descriptor {
                    c.push(0x4d);
                    uleb(&mut c, u64::from(y));
                }
                match &b.types[i] {
                    TypeDef::Func(s) => {
                        c.push(0x60);
                        val_type_vec(&mut c, &s.params)?;
                        val_type_vec(&mut c, &s.results)?;
                    }
                    TypeDef::Struct(fields) => {
                        c.push(0x5f);
                        uleb(&mut c, fields.len() as u64);
                        for f in fields {
                            emit_gc_field(&mut c, *f)?;
                        }
                    }
                    TypeDef::Array(f) => {
                        c.push(0x5e);
                        emit_gc_field(&mut c, *f)?;
                    }
                }
            }
        }
        push_section(&mut out, 1, &c);
    }

    emit_imports(&mut out, b)?;
    emit_rest(&mut out, b, &e)?;
    Ok(out)
}

/// Emit section 2. Imports are listed in **declaration order** — that order is the linking
/// ABI a positional embedder builds its extern vector against — while the per-kind lists
/// are what assigned the indices.
fn emit_imports(out: &mut Vec<u8>, b: &ModuleBuild) -> Result<()> {
    if b.import_order.is_empty() {
        return Ok(());
    }
    let mut c = Vec::new();
    uleb(&mut c, b.import_order.len() as u64);
    let (mut f, mut t, mut m, mut g, mut e) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for kind in &b.import_order {
        match kind {
            ImportKind::Func => {
                let i = &b.func_imports[f];
                f += 1;
                name_bytes(&mut c, &i.r.module);
                name_bytes(&mut c, &i.r.name);
                c.push(if i.exact { 0x20 } else { 0x00 });
                uleb(&mut c, u64::from(i.type_index));
            }
            ImportKind::Table => {
                let i = &b.table_imports[t];
                t += 1;
                name_bytes(&mut c, &i.r.module);
                name_bytes(&mut c, &i.r.name);
                c.push(0x01);
                emit_val_type(&mut c, i.t.elem)?;
                emit_limits(&mut c, i.t.min, i.t.max, false, i.t.is64);
            }
            ImportKind::Mem => {
                let i = &b.mem_imports[m];
                m += 1;
                name_bytes(&mut c, &i.r.module);
                name_bytes(&mut c, &i.r.name);
                c.push(0x02);
                emit_memory_type(&mut c, &i.m);
            }
            ImportKind::Global => {
                let i = &b.global_imports[g];
                g += 1;
                name_bytes(&mut c, &i.r.module);
                name_bytes(&mut c, &i.r.name);
                c.push(0x03);
                emit_val_type(&mut c, i.valtype)?;
                c.push(u8::from(i.mutable));
            }
            ImportKind::Tag => {
                let i = &b.tag_imports[e];
                e += 1;
                name_bytes(&mut c, &i.r.module);
                name_bytes(&mut c, &i.r.name);
                c.push(0x04);
                c.push(0x00);
                uleb(&mut c, u64::from(i.type_index));
            }
        }
    }
    push_section(out, 2, &c);
    Ok(())
}

/// Emit sections 3 onward, from the pre-encoded pieces.
fn emit_rest(out: &mut Vec<u8>, b: &ModuleBuild, e: &Encoded) -> Result<()> {
    // 3 — function type indices.
    if !e.func_sigs.is_empty() {
        let mut c = Vec::new();
        uleb(&mut c, e.func_sigs.len() as u64);
        for &ti in e.func_sigs {
            uleb(&mut c, u64::from(ti));
        }
        push_section(out, 3, &c);
    }

    // 4 — tables.
    if !b.tables.is_empty() {
        let mut c = Vec::new();
        uleb(&mut c, b.tables.len() as u64);
        for (i, t) in b.tables.iter().enumerate() {
            // §5.5.6 with function-references:
            //   table ::= tt:tabletype                    (the plain form)
            //           | 0x40 0x00 tt:tabletype e:expr   (with an initializer)
            // `0x40` is not a valid valtype byte, so the two forms are unambiguous.
            if let Some(Some(init)) = e.table_inits.get(i) {
                c.push(0x40);
                c.push(0x00);
                emit_val_type(&mut c, t.elem)?;
                emit_limits(&mut c, t.min, t.max, false, t.is64);
                c.extend_from_slice(init);
            } else {
                emit_val_type(&mut c, t.elem)?;
                emit_limits(&mut c, t.min, t.max, false, t.is64);
            }
        }
        push_section(out, 4, &c);
    }

    // 5 — memories.
    if !b.memories.is_empty() {
        let mut c = Vec::new();
        uleb(&mut c, b.memories.len() as u64);
        for m in &b.memories {
            emit_memory_type(&mut c, m);
        }
        push_section(out, 5, &c);
    }

    // 13 — tags (before globals, per the EH proposal's section order).
    if !b.tags.is_empty() {
        let mut c = Vec::new();
        uleb(&mut c, b.tags.len() as u64);
        for &ti in &b.tags {
            c.push(0x00); // attribute: exception
            uleb(&mut c, u64::from(ti));
        }
        push_section(out, 13, &c);
    }

    // 6 — globals.
    if !e.globals.is_empty() {
        let mut c = Vec::new();
        uleb(&mut c, e.globals.len() as u64);
        for (g, init) in e.globals.iter().zip(e.global_inits) {
            emit_val_type(&mut c, g.valtype)?;
            c.push(u8::from(g.mutable));
            c.extend_from_slice(init);
        }
        push_section(out, 6, &c);
    }

    // 7 — exports.
    if !b.exports.is_empty() {
        let mut c = Vec::new();
        uleb(&mut c, b.exports.len() as u64);
        for e in &b.exports {
            name_bytes(&mut c, &e.name);
            c.push(e.kind);
            uleb(&mut c, u64::from(e.index));
        }
        push_section(out, 7, &c);
    }

    // 8 — start.
    if let Some(s) = &b.start {
        let mut c = Vec::new();
        uleb(&mut c, u64::from(resolve_by_name(&b.func_names, s)?));
        push_section(out, 8, &c);
    }

    // 9 — element segments.
    if !e.elems.is_empty() {
        let mut c = Vec::new();
        uleb(&mut c, e.elems.len() as u64);
        for seg in e.elems {
            c.extend_from_slice(seg);
        }
        push_section(out, 9, &c);
    }

    // 12 — data count. §5.5.13 requires it ONLY when `memory.init`/`data.drop` appear: it
    // exists so a decoder knows the segment count before the code section. Emitting it for
    // every module with data segments spent 3 bytes to say nothing.
    if !e.datas.is_empty() && b.needs_data_count {
        let mut c = Vec::new();
        uleb(&mut c, e.datas.len() as u64);
        push_section(out, 12, &c);
    }

    // 10 — code.
    if !e.bodies.is_empty() {
        let mut c = Vec::new();
        uleb(&mut c, e.bodies.len() as u64);
        for body in e.bodies {
            uleb(&mut c, body.len() as u64);
            c.extend_from_slice(body);
        }
        push_section(out, 10, &c);
    }

    // 11 — data segments.
    if !e.datas.is_empty() {
        let mut c = Vec::new();
        uleb(&mut c, e.datas.len() as u64);
        for (d, off) in e.datas.iter().zip(e.data_offsets) {
            match off {
                Some(bytes) => {
                    if d.mem_index == 0 {
                        uleb(&mut c, 0);
                    } else {
                        uleb(&mut c, 2);
                        uleb(&mut c, u64::from(d.mem_index));
                    }
                    c.extend_from_slice(bytes);
                }
                None => uleb(&mut c, 1), // passive
            }
            uleb(&mut c, d.bytes.len() as u64);
            c.extend_from_slice(&d.bytes);
        }
        push_section(out, 11, &c);
    }

    Ok(())
}

// --- Instruction encoding -----------------------------------------------------

/// Cap on control nesting while assembling, mirroring the validator's.
const MAX_CTRL_DEPTH: usize = 1024;

/// Everything an instruction needs to resolve a name to an index.
///
/// The name tables are borrowed field-by-field rather than through the whole
/// [`ModuleBuild`], because `sigs` must be **mutable** here: a multi-value block type
/// interns its signature into the type table while a body is being encoded. Disjoint field
/// borrows make that safe, and it is why bodies are encoded before any section is written.
struct Ctx<'a> {
    out: Vec<u8>,
    types: &'a mut Vec<TypeDef>,
    /// The explicit `(rec …)` groups, as (start, len) — needed because an implicit type use
    /// may never reuse a type that lives inside one (see `intern_sig_outside_rec`).
    rec_groups: &'a [(u32, u32)],
    type_names: &'a [Option<String>],
    /// Per-type struct field names, so `struct.get $T $field` resolves by name.
    field_names: &'a [Vec<Option<String>>],
    func_names: &'a [Option<String>],
    global_names: &'a [Option<String>],
    table_names: &'a [Option<String>],
    elem_names: &'a [Option<String>],
    data_names: &'a [Option<String>],
    mem_names: &'a [Option<String>],
    tag_names: &'a [Option<String>],
    local_names: &'a [Option<String>],
    /// Control-label stack, innermost last, for resolving `br $name` to a relative depth.
    labels: Vec<Option<String>>,
    /// The open FLAT legacy `try` frames, innermost last — see [`Ctx::flat_try_clause`].
    flat_tries: Vec<FlatTry>,
    /// This body's branch hints and label `@name`s, keyed by the ADDRESS of the item each applies
    /// to (`annot::body_marks`). Looked up in `emit_one`, the one function every instruction item
    /// passes through, so no emitter path can skip the lookup.
    hint_at: &'a BTreeMap<usize, u8>,
    label_at: &'a BTreeMap<usize, Vec<u8>>,
    /// A hint for the folded form `emit_one` is about to hand to `emit_folded`.
    armed: Option<u8>,
    /// Hints for folded `call_indirect` / SIMD / atomic forms, which emit their own operands and
    /// so record at their own opcode themselves. A stack because such forms nest.
    own_hints: Vec<Option<u8>>,
    /// Recorded hints: (offset of the instruction from the start of the body, value).
    hints: Vec<(u32, u8)>,
    /// Block-like instructions opened so far — the name section's label index space, which
    /// counts EVERY block, named or not (`(block (block $b))` names label 1).
    label_count: u32,
    label_names: Vec<(u32, Vec<u8>)>,
    /// Written through to [`ModuleBuild::needs_data_count`] when this body emits
    /// `memory.init`/`data.drop`. A `&mut` to one field, borrowed disjointly from the name
    /// tables — the same trick that lets a body intern a block signature.
    needs_data_count: &'a mut bool,
    /// [`ModuleBuild::deferred_type_uses`], for the type uses a body reads.
    deferred_type_uses: &'a mut Vec<(u32, Sig)>,
}

impl Ctx<'_> {
    /// Resolve a branch target: a `$label` searched innermost-out, or a literal depth.
    fn resolve_label(&self, s: &Sexpr) -> Result<u32> {
        let a = want_atom(s).map_err(|_| Error::BadImmediate)?;
        if a.starts_with('$') {
            for (i, nm) in self.labels.iter().rev().enumerate() {
                if nm.as_deref() == Some(a) {
                    return u32::try_from(i).map_err(|_| Error::BadImmediate);
                }
            }
            return Err(Error::UnknownLabel);
        }
        parse_index(s)
    }

    fn resolve_local(&self, s: &Sexpr) -> Result<u32> {
        resolve_by_name(self.local_names, s)
    }

    /// Admit a flat `catch` / `catch_all` / `delegate`, which is only text when the INNERMOST
    /// open frame is a legacy `try` whose handlers have got no further than `phase` allows:
    /// `try … (catch x …)* (catch_all …)? end` or `try … delegate l`.
    ///
    /// ⚠️ These used to be emitted wherever they appeared — `(func (catch_all))` assembled, and
    /// the VALIDATOR then refused it as `MismatchedCatch`. The spec suite asserts these with
    /// `assert_malformed`, so the stage is the verdict, and it had been passing only because
    /// the `.wast` runner wrapped whole-module quotes in a second `(module …)` and refused
    /// them all with `BadModuleField` (see `wast::quoted_module_source`).
    fn flat_try_clause(&mut self, next: TryPhase) -> Result<()> {
        let depth = self.labels.len();
        // A frame deeper than the label stack was closed by something other than its own
        // `end` (malformed input the validator will reject); it can never match again.
        while self.flat_tries.last().is_some_and(|t| t.depth > depth) {
            self.flat_tries.pop();
        }
        let Some(t) = self.flat_tries.last_mut().filter(|t| t.depth == depth) else {
            return Err(Error::UnexpectedToken);
        };
        let allowed = match next {
            TryPhase::Catch | TryPhase::CatchAll => t.phase != TryPhase::CatchAll,
            TryPhase::Delegate => t.phase == TryPhase::Body,
            TryPhase::Body => false,
        };
        if !allowed {
            return Err(Error::UnexpectedToken);
        }
        t.phase = next;
        if next == TryPhase::Delegate {
            self.flat_tries.pop();
        }
        Ok(())
    }
}

static NO_HINTS: BTreeMap<usize, u8> = BTreeMap::new();
static NO_LABELS: BTreeMap<usize, Vec<u8>> = BTreeMap::new();

/// A `$id` as the name section spells it — without the `$`.
fn id_text(id: &str) -> Vec<u8> {
    id.strip_prefix('$').unwrap_or(id).as_bytes().to_vec()
}

impl Ctx<'_> {
    fn record_hint(&mut self, hint: Option<u8>) {
        if let Some(v) = hint {
            self.hints.push((self.out.len() as u32, v));
        }
    }

    /// The hint `emit_folded` handed to a form that emits its own operands, recorded now that
    /// its opcode is next. Every folded call of such a form pushes exactly one entry.
    fn record_own_hint(&mut self) {
        let h = self.own_hints.pop().flatten();
        self.record_hint(h);
    }

    /// Open a block-like instruction's label: the depth cap, the label stack, and the name
    /// section's label index — one place, so no block form can count differently from another.
    /// `key` is the form's keyword atom, which is what a label `@name` is keyed on.
    fn open_label(&mut self, label: Option<String>, key: &Sexpr) -> Result<()> {
        if self.labels.len() >= MAX_CTRL_DEPTH {
            return Err(Error::NestingTooDeep);
        }
        let idx = self.label_count;
        self.label_count += 1;
        let name = match self.label_at.get(&annot::addr(key)) {
            Some(n) => Some(n.clone()),
            None => label.as_deref().map(id_text),
        };
        if let Some(n) = name {
            self.label_names.push((idx, n));
        }
        self.labels.push(label);
        Ok(())
    }
}

/// An open flat legacy `try`: the label depth its own label sits at, and how far through the
/// handler grammar it has got.
struct FlatTry {
    depth: usize,
    phase: TryPhase,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TryPhase {
    Body,
    Catch,
    CatchAll,
    Delegate,
}

/// Borrow every name table out of a [`ModuleBuild`] alongside a mutable `sigs`, so a body
/// can intern a block-type signature while still resolving names. The disjoint field
/// borrows are what make this sound.
macro_rules! ctx_for {
    ($b:expr, $locals:expr, $out:expr) => {
        Ctx {
            out: $out,
            types: &mut $b.types,
            rec_groups: &$b.rec_groups,
            type_names: &$b.type_names,
            field_names: &$b.field_names,
            func_names: &$b.func_names,
            global_names: &$b.global_names,
            table_names: &$b.table_names,
            elem_names: &$b.elem_names,
            data_names: &$b.data_names,
            mem_names: &$b.mem_names,
            tag_names: &$b.tag_names,
            local_names: $locals,
            labels: Vec::new(),
            flat_tries: Vec::new(),
            hint_at: &NO_HINTS,
            label_at: &NO_LABELS,
            armed: None,
            own_hints: Vec::new(),
            hints: Vec::new(),
            label_count: 0,
            label_names: Vec::new(),
            needs_data_count: &mut $b.needs_data_count,
            deferred_type_uses: &mut $b.deferred_type_uses,
        }
    };
}

/// Encode one function body: the locals vector, the instruction sequence, then the
/// implicit `end`.
fn encode_body(f: &Func, b: &mut ModuleBuild) -> Result<Body> {
    let mut out = Vec::new();
    // One (count = 1, type) group per declared local — simple and always correct.
    uleb(&mut out, f.locals.len() as u64);
    for &t in &f.locals {
        uleb(&mut out, 1);
        emit_val_type(&mut out, t)?;
    }
    let locals = f.local_names.clone();
    let marks = annot::body_marks(&f.body, &f.body_annots);
    let mut ctx = ctx_for!(b, &locals, out);
    ctx.hint_at = &marks.hints;
    ctx.label_at = &marks.labels;
    emit_seq(&mut ctx, &f.body)?;
    ctx.out.push(0x0b); // implicit function end
    // 🔒 Every hint in the body must have been recorded. One standing where no instruction
    // begins — before a flat immediate, say — is never looked up, and would otherwise vanish
    // from the output without a word: exactly the silent drop these annotations suffered when
    // the lexer threw them away.
    if ctx.hints.len() != marks.expected_hints {
        return Err(Error::Annotation(
            "@metadata.code.branch_hint annotation: must precede an instruction",
        ));
    }
    Ok(Body { bytes: ctx.out, hints: ctx.hints, labels: ctx.label_names })
}

/// One encoded function body, with what it contributes to the custom sections.
struct Body {
    bytes: Vec<u8>,
    hints: Vec<(u32, u8)>,
    labels: NameMap,
}

/// Emit a constant expression (a global initializer, or a data/element offset), terminated
/// by `end`.
fn emit_const_expr(out: &mut Vec<u8>, exprs: &[Sexpr], b: &mut ModuleBuild) -> Result<()> {
    let mut ctx = ctx_for!(b, &[], Vec::new());
    emit_seq(&mut ctx, exprs)?;
    let bytes = ctx.out;
    out.extend_from_slice(&bytes);
    out.push(0x0b);
    Ok(())
}

/// Emit a sequence of instruction forms (folded lists and/or flat atoms).
fn emit_seq(ctx: &mut Ctx, items: &[Sexpr]) -> Result<()> {
    let mut i = 0;
    while i < items.len() {
        i = emit_one(ctx, items, i)?;
    }
    Ok(())
}

/// Emit one instruction (flat or folded) starting at `items[i]`; return the index of the
/// next one.
fn emit_one(ctx: &mut Ctx, items: &[Sexpr], i: usize) -> Result<usize> {
    // A branch hint applies to the instruction this item begins. A flat instruction's opcode is
    // the next byte out; a folded one's comes after its operands, so it is handed on.
    let hint = if ctx.hint_at.is_empty() {
        None
    } else {
        ctx.hint_at.get(&annot::addr(&items[i])).copied()
    };
    match &items[i] {
        Sexpr::List(l, _) => {
            ctx.armed = hint;
            emit_folded(ctx, l)?;
            Ok(i + 1)
        }
        Sexpr::Atom(name) => {
            ctx.record_hint(hint);
            emit_flat(ctx, items, i, &name.clone())
        }
        Sexpr::Str(_) => Err(Error::UnknownInstr),
    }
}

/// A folded instruction: `(op operand* )` — the operands are emitted first, then the op.
fn emit_folded(ctx: &mut Ctx, l: &[Sexpr]) -> Result<()> {
    // A branch hint on a folded form marks the form's OWN opcode — which for a block comes
    // first, for `if` comes after the condition, and for everything else comes after all the
    // operands (wasm-tools: `(@hint) (i32.eqz (if …))` hints the `i32.eqz`, not the `if`).
    // Taken now, before any operand can see it.
    let hint = ctx.armed.take();
    let kw = nth(l, 0)?.as_atom().ok_or(Error::UnknownInstr)?.to_string();
    match kw.as_str() {
        "block" | "loop" | "try_table" | "try" => ctx.record_hint(hint),
        _ => {}
    }
    match kw.as_str() {
        "block" | "loop" => return emit_folded_block(ctx, &kw, l),
        "if" => return emit_folded_if(ctx, l, hint),
        "try_table" => return emit_try_table(ctx, l),
        // The legacy folded `try` is structural — `(do …)` plus clause lists — so it is
        // intercepted here rather than going through the opcode table.
        "try" => return emit_folded_try(ctx, l),
        // Folded, these are only ever CLAUSES of a `(try …)`, which `emit_folded_try` consumes
        // itself — so reaching here means one stands alone, e.g. `(func (catch_all))`.
        // ⚠️ The flat-form guard (`Ctx::flat_try_clause`) does not see this path: folded
        // instructions go through `emit_op_with_immediates`, never `emit_flat`.
        "catch" | "catch_all" | "delegate" => return Err(Error::UnexpectedToken),
        _ => {}
    }
    // The prefixed families are looked up before the single-byte table — their members
    // have no `Op` of their own.
    if let Some((sub, imm)) = lookup_simd(&kw) {
        ctx.own_hints.push(hint);
        emit_simd(ctx, sub, imm, l, 1, true)?;
        return Ok(());
    }
    if let Some(sub) = lookup_atomic(&kw) {
        ctx.own_hints.push(hint);
        emit_atomic(ctx, sub, l, 1, true)?;
        return Ok(());
    }
    let op = crate::opcode::Op::from_text_name(&kw)
        .ok_or_else(|| classify_unknown_mnemonic(&kw))?;
    use crate::opcode::Op as O;
    if op == O::CallIndirect || op == O::ReturnCallIndirect {
        let opcode = if op == O::CallIndirect { 0x11 } else { 0x13 };
        ctx.own_hints.push(hint);
        return emit_call_indirect(ctx, l, 1, true, opcode).map(|_| ());
    }
    // `(select (result t) a b c)` — the reference-types typed form. Its `(result …)` is an
    // IMMEDIATE, not an operand, so it must be lifted out before the operand walk below; the
    // same shape `call_indirect`'s `(type $t)` has, and handled the same way.
    if op == O::Select {
        let mut j = 1;
        let tys = parse_select_result(ctx, l, &mut j)?;
        for k in j..l.len() {
            emit_one(ctx, l, k)?;
        }
        ctx.record_hint(hint);
        emit_select(ctx, &tys);
        return Ok(());
    }
    // The cast ops take a **list** immediate — `(ref null? ht)` — which the atom/list split
    // below would otherwise mistake for an operand and try to emit as an instruction.
    match op {
        // `ref.null`'s heap type may be a LIST — `(ref.null (exact $t))` — which the atom/list split
        // below would take for an operand. Same shape as the cast targets.
        O::RefTest | O::RefCastOp | O::RefNull | O::RefCastDescEq => {
            let imm_end = 2.min(l.len());
            for j in imm_end..l.len() {
                emit_one(ctx, l, j)?;
            }
            ctx.record_hint(hint);
            return emit_op_with_immediates(ctx, op, &l[..imm_end], 1);
        }
        O::BrOnCast | O::BrOnCastFail | O::BrOnCastDescEq | O::BrOnCastDescEqFail => {
            let imm_end = 4.min(l.len());
            for j in imm_end..l.len() {
                emit_one(ctx, l, j)?;
            }
            ctx.record_hint(hint);
            return emit_op_with_immediates(ctx, op, &l[..imm_end], 1);
        }
        _ => {}
    }
    // In a folded instruction the immediates are the **leading atoms** and the operands are
    // the parenthesized sub-expressions that follow — folded operands are always
    // parenthesized, so the atom/list split is exactly the immediate/operand split. That
    // covers fixed-arity immediates and the variable `offset=`/`align=` memarg atoms alike.
    let mut imm_end = 1;
    while imm_end < l.len() && l[imm_end].as_atom().is_some() {
        imm_end += 1;
    }
    for j in imm_end..l.len() {
        emit_one(ctx, l, j)?;
    }
    ctx.record_hint(hint);
    // `br_table`'s label vector is variable-length, so it cannot go through
    // `emit_op_with_immediates` — which is what silently dropped it here before.
    if op == O::BrTable {
        ctx.out.push(0x0e);
        emit_br_table_labels(ctx, &l[..imm_end], 1)?;
        return Ok(());
    }
    emit_op_with_immediates(ctx, op, &l[..imm_end], 1)?;
    Ok(())
}

/// Emit a `br_table`'s label vector — every leading index-or-`$id` atom from `start`, the
/// last of which is the default. Returns the index just past them.
///
/// Shared by the flat and folded emitters **on purpose**. It used to live only in the flat
/// one, so a folded `(br_table $l)` emitted the opcode byte and no vector at all: the
/// assembler reported success for a module no decoder could read. An op whose immediates are
/// variable-length has to be reachable from both spellings or one of them silently truncates.
fn emit_br_table_labels(ctx: &mut Ctx, items: &[Sexpr], start: usize) -> Result<usize> {
    let mut j = start;
    let mut labels: Vec<u32> = Vec::new();
    while let Some(s) = items.get(j) {
        if s.as_atom().is_some_and(is_index_or_id) {
            labels.push(ctx.resolve_label(s)?);
            j += 1;
        } else {
            break;
        }
    }
    // The last label is the default, so at least one is required.
    if labels.is_empty() {
        return Err(Error::BadImmediate);
    }
    let default = labels.pop().unwrap();
    uleb(&mut ctx.out, labels.len() as u64);
    for l in labels {
        uleb(&mut ctx.out, u64::from(l));
    }
    uleb(&mut ctx.out, u64::from(default));
    Ok(j)
}

/// How many leading atoms an op takes as immediates in a **flat** form (where operands are
/// already on the stack, so the count has to be exact).
fn immediate_arity(op: crate::opcode::Op) -> usize {
    use crate::opcode::Op as O;
    match op {
        O::I32Const | O::I64Const | O::F32Const | O::F64Const => 1,
        O::LocalGet | O::LocalSet | O::LocalTee => 1,
        O::GlobalGet | O::GlobalSet => 1,
        O::Call | O::RefFunc | O::Br | O::BrIf | O::Throw | O::Rethrow => 1,
        // `return_call x:funcidx` — the tail-call twin of `call`, same immediate (T9f).
        O::ReturnCall => 1,
        // `call_ref x:typeidx` / `return_call_ref x:typeidx` (function-references). Their absence
        // here made `immediate_arity` return 0, so the assembler emitted the opcode and left `$t`
        // in the token stream — see the emitter arm for what that produced.
        O::CallRef | O::ReturnCallRef => 1,
        // `br_on_null l:labelidx` / `br_on_non_null l:labelidx` — the same omission, same family,
        // found by the same sweep.
        O::BrOnNull | O::BrOnNonNull => 1,
        O::TableGet | O::TableSet | O::TableSize | O::TableGrow | O::TableFill => 1,
        O::ElemDrop | O::DataDrop | O::RefNull => 1,
        O::MemorySize | O::MemoryGrow | O::MemoryFill => 1,
        O::TableInit | O::TableCopy | O::MemoryInit | O::MemoryCopy => 2,
        // GC array bulk ops. ⚠️ Listed EXPLICITLY: `immediate_arity` ends in `_ => 0`, and
        // T9a#8 was four instructions that fell into that arm and shipped as bare opcodes
        // with their operand left in the token stream.
        O::ArrayFill => 1,
        O::ArrayNewData | O::ArrayNewElem | O::ArrayInitData | O::ArrayInitElem
        | O::ArrayCopy => 2,
        // The externref bridge takes no immediate. ⚠️ Listed rather than left to `_ => 0` for
        // the same reason as the array bulk ops above: falling into the catch-all is how four
        // instructions shipped as bare opcodes, and "0" has to be a decision, not a default.
        O::AnyConvertExtern | O::ExternConvertAny => 0,
        // ⚠️⚠️ **Every other GC instruction was missing here, so none of them assembled in FLAT form.**
        // `struct.new $s`, `struct.get $s 0`, `ref.cast (ref null $s)`, `br_on_cast 0 anyref (ref $s)`:
        // each fell into `_ => 0`, left its immediates in the token stream, and was refused as
        // `BadImmediate` — while wasm-tools accepts every one. Invisible for the whole GC track because
        // the spec corpus spells GC instructions FOLDED, and the `.wat` corpus has none flat. Found
        // 2026-09-19 by the D3 byte-comparison against wasm-tools, which tried both spellings.
        O::StructNew | O::StructNewDefault | O::ArrayNew | O::ArrayNewDefault | O::ArrayGet
        | O::ArrayGetS | O::ArrayGetU | O::ArraySet => 1,
        O::StructGet | O::StructGetS | O::StructGetU | O::StructSet | O::ArrayNewFixed => 2,
        // A cast's target is ONE list immediate, `(ref null? ht)`.
        O::RefTest | O::RefCastOp => 1,
        // label, source type, target type.
        O::BrOnCast | O::BrOnCastFail => 3,
        O::ArrayLen | O::RefI31 | O::I31GetS | O::I31GetU | O::RefEq => 0,
        // custom-descriptors: type-index ops, the desc-eq cast target, and the desc-eq branches.
        O::StructNewDesc | O::StructNewDefaultDesc | O::RefGetDesc | O::RefCastDescEq => 1,
        O::BrOnCastDescEq | O::BrOnCastDescEqFail => 3,
        // Loads/stores take optional `offset=`/`align=` atoms, consumed by their emitter.
        _ => 0,
    }
}

/// Whether every index immediate of `op` may be omitted in the text format, defaulting to 0.
/// `memory.init` and `table.init` are here too: their *segment* index is mandatory, but it is
/// also index-like, so the leading-atom count stops in the right place either way.
fn has_optional_indices(op: crate::opcode::Op) -> bool {
    use crate::opcode::Op as O;
    matches!(
        op,
        O::TableGet
            | O::TableSet
            | O::TableSize
            | O::TableGrow
            | O::TableFill
            | O::MemorySize
            | O::MemoryGrow
            | O::MemoryFill
            | O::TableInit
            | O::TableCopy
            | O::MemoryInit
            | O::MemoryCopy
    )
}

/// Emit `call_indirect`, in either form:
/// `call_indirect $table? (type $t)? (param …)* (result …)*` — plus, when `folded`, the
/// operand sub-expressions that follow (emitted before the opcode).
///
/// Returns the index just past the instruction's forms.
/// Emit `call_indirect` or its tail twin `return_call_indirect`.
///
/// ⚠️ `opcode` is a PARAMETER, not a constant, and that is the whole point. This function once
/// pushed a hard-coded `0x11`, so routing `return_call_indirect` through it emitted a plain
/// `call_indirect` — the text said one thing and the module said another. Instance #5 of the
/// mechanism T10a names: an emitter reconstructing a form from a SUBSET of the parser's facts,
/// here dropping which instruction it was even emitting. It surfaced as a `StackHeightMismatch`
/// from the VALIDATOR three stages away (`best-practices.md` §3.7).
/// `select (result t)*` — the reference-types typed form. Consumes the optional `(result …)`
/// clause and returns the value types it names; an absent clause yields an empty vector, which
/// is the untyped `select`.
///
/// ⚠️ **Neither form assembled at all until 2026-08-19** — `select` had no bespoke handling, so
/// the `(result …)` was read as the next instruction and came back `UnknownInstr` naming
/// `result`. That is `select.wast`'s 124 skips, and it is why the withdrawn "no declaration
/// after the body begins" rule broke on this file: in FLAT form the immediate sits exactly
/// where a misplaced declaration would.
fn parse_select_result(ctx: &Ctx, l: &[Sexpr], j: &mut usize) -> Result<Vec<ValType>> {
    let mut tys = Vec::new();
    while let Some(items) = l.get(*j).filter(|s| eq_kw(s, "result")).and_then(Sexpr::as_list) {
        for t in &items[1..] {
            tys.push(parse_val_type(t, ctx.type_names)?);
        }
        *j += 1;
    }
    Ok(tys)
}

/// Emit `select` — bare `0x1b`, or `0x1c` with an explicit result-type vector.
fn emit_select(ctx: &mut Ctx, tys: &[ValType]) {
    if tys.is_empty() {
        ctx.out.push(0x1b);
        return;
    }
    ctx.out.push(0x1c);
    uleb(&mut ctx.out, tys.len() as u64);
    for t in tys {
        // `emit_val_type` is fallible only for forms `select` cannot take; a concrete `(ref $t)`
        // is not a valid `select` result type, and `parse_val_type` has already refused one.
        let _ = emit_val_type(&mut ctx.out, *t);
    }
}

fn emit_call_indirect(ctx: &mut Ctx, l: &[Sexpr], start: usize, folded: bool, opcode: u8) -> Result<usize> {
    let mut j = start;
    // An optional leading table index or `$name` (multi-table).
    let mut table = 0u32;
    if let Some(s) = l.get(j) {
        if s.as_atom().is_some_and(is_index_or_id) {
            table = resolve_by_name(ctx.table_names, s)?;
            j += 1;
        }
    }
    // The type annotation: `(type $t)` and/or an inline signature — the same **type use** grammar a
    // block type reads, through the same function. This had its own copy of the loop and therefore its
    // own copy of all three defects: clause order, named parameters, and an inline signature silently
    // overridden by the `(type x)` beside it.
    let (type_ref, sig) = parse_type_use(ctx, l, &mut j)?;
    // An inline signature interns into the shared type table — bodies are encoded before
    // any section is written, so appending here is safe.
    let ti = type_ref.unwrap_or_else(|| intern_sig_outside_rec(ctx.types, ctx.rec_groups, sig));
    if folded {
        for k in j..l.len() {
            emit_one(ctx, l, k)?;
        }
        ctx.record_own_hint();
        j = l.len();
    }
    ctx.out.push(opcode);
    uleb(&mut ctx.out, u64::from(ti));
    uleb(&mut ctx.out, u64::from(table));
    Ok(j)
}

fn emit_folded_block(ctx: &mut Ctx, kw: &str, l: &[Sexpr]) -> Result<()> {
    let op = if kw == "block" { 0x02u8 } else { 0x03 };
    let mut j = 1;
    let label = opt_name(l, &mut j);
    let bt = parse_block_type(ctx, l, &mut j)?;
    ctx.out.push(op);
    emit_block_type(ctx, bt)?;
    ctx.open_label(label, &l[0])?;
    emit_seq(ctx, &l[j..])?;
    ctx.labels.pop();
    ctx.out.push(0x0b);
    Ok(())
}

/// Emit a `try_table`'s catch vector: consume leading `(catch …)` / `(catch_ref …)` /
/// `(catch_all …)` / `(catch_all_ref …)` clauses, then `count` followed by each clause
/// (kind byte, tag index for the non-`all` kinds, label index). Advances `j` past them.
fn emit_catch_clauses(ctx: &mut Ctx, items: &[Sexpr], j: &mut usize) -> Result<()> {
    let mut clauses: Vec<(u8, Option<u32>, u32)> = Vec::new();
    while let Some(cl) = items.get(*j).and_then(Sexpr::as_list) {
        let Some(kw) = cl.first().and_then(Sexpr::as_atom) else {
            break;
        };
        let kind: u8 = match kw {
            "catch" => 0,
            "catch_ref" => 1,
            "catch_all" => 2,
            "catch_all_ref" => 3,
            _ => break,
        };
        if kind < 2 {
            let tag = resolve_by_name(ctx.tag_names, nth(cl, 1)?)?;
            let label = ctx.resolve_label(nth(cl, 2)?)?;
            clauses.push((kind, Some(tag), label));
        } else {
            let label = ctx.resolve_label(nth(cl, 1)?)?;
            clauses.push((kind, None, label));
        }
        *j += 1;
    }
    uleb(&mut ctx.out, clauses.len() as u64);
    for (kind, tag, label) in clauses {
        ctx.out.push(kind);
        if let Some(t) = tag {
            uleb(&mut ctx.out, u64::from(t));
        }
        uleb(&mut ctx.out, u64::from(label));
    }
    Ok(())
}

/// `(try_table $l? blocktype? (catch …)* instr*)`
fn emit_try_table(ctx: &mut Ctx, l: &[Sexpr]) -> Result<()> {
    ctx.out.push(0x1f);
    let mut j = 1;
    let label = opt_name(l, &mut j);
    let bt = parse_block_type(ctx, l, &mut j)?;
    emit_block_type(ctx, bt)?;
    // ⚠️⚠️ **The catch labels resolve in the ENCLOSING scope** — the try_table's own label is
    // pushed AFTER them. `C ⊢ catch ok` is checked in `C`, before the rule extends it with the
    // block's label, so `(catch $e 0)` names the block *around* the try_table.
    //
    // This pushed first, "so a `(catch … 0)` targeting it resolves to 0". The validator and the
    // interpreter both agreed with it, so wasmrt round-tripped its own output perfectly and the
    // suite was almost entirely green — but the BYTES were off by one, and wasmtime 47 refuses
    // them: `catch_all label must have no result types`, resolving our `1` to the function.
    // 🎓 Three components agreeing is not evidence when all three learned it from each other;
    // only an outside reader can tell a convention from a bug.
    emit_catch_clauses(ctx, l, &mut j)?;
    ctx.open_label(label, &l[0])?;
    emit_seq(ctx, &l[j..])?;
    ctx.out.push(0x0b);
    ctx.labels.pop();
    Ok(())
}

/// The legacy folded `try` (older-LLVM exception handling):
///   `(try $label? blocktype? (do instr*) (catch $tag instr*)* (catch_all instr*)?)`
///
/// Emits the flat legacy encoding the decoder consumes: `try bt … catch tag … catch_all …
/// end`. The try's own label stays on the stack while the body and handlers are emitted, so
/// a `$label` / `rethrow` operand resolves against the same depth model the interpreter
/// uses at run time.
fn emit_folded_try(ctx: &mut Ctx, l: &[Sexpr]) -> Result<()> {
    let mut j = 1;
    let label = opt_name(l, &mut j);
    let bt = parse_block_type(ctx, l, &mut j)?;

    let do_form = want_list(nth(l, j)?)?;
    if do_form.first().map(|s| eq_atom(s, "do")) != Some(true) {
        return Err(Error::BadImmediate);
    }
    j += 1;

    ctx.out.push(0x06); // try
    emit_block_type(ctx, bt)?;
    ctx.open_label(label, &l[0])?;
    emit_seq(ctx, &do_form[1..])?;

    // `(delegate $l)` forwards an exception to an enclosing try instead of running local
    // handlers, and TERMINATES the try in place of its `end`. ⚠️ Its label resolves in the scope
    // OUTSIDE the try, so the try's own label comes off before it is read — the same rule
    // `try_table`'s catch labels follow, and for the same reason: the form closes the block.
    if let Some(d) = l.get(j).and_then(Sexpr::as_list) {
        if d.first().map(|s| eq_atom(s, "delegate")) == Some(true) {
            ctx.labels.pop();
            ctx.out.push(0x18);
            let target = ctx.resolve_label(nth(d, 1)?)?;
            uleb(&mut ctx.out, u64::from(target));
            return Ok(());
        }
    }

    // `(catch_all …)` is the LAST handler if present: `(catch x …)* (catch_all …)?`.
    let mut seen_catch_all = false;
    while j < l.len() {
        let cl = want_list(&l[j])?;
        let kw = cl.first().and_then(Sexpr::as_atom);
        if seen_catch_all && matches!(kw, Some("catch" | "catch_all")) {
            return Err(Error::UnexpectedToken);
        }
        match kw {
            Some("catch") => {
                ctx.out.push(0x07);
                let tag = resolve_by_name(ctx.tag_names, nth(cl, 1)?)?;
                uleb(&mut ctx.out, u64::from(tag));
                emit_seq(ctx, &cl[2..])?;
            }
            Some("catch_all") => {
                seen_catch_all = true;
                ctx.out.push(0x19);
                emit_seq(ctx, &cl[1..])?;
            }
            // Only catch / catch_all / delegate may follow `(do …)`.
            _ => return Err(Error::BadImmediate),
        }
        j += 1;
    }
    ctx.out.push(0x0b);
    ctx.labels.pop();
    Ok(())
}

fn emit_folded_if(ctx: &mut Ctx, l: &[Sexpr], hint: Option<u8>) -> Result<()> {
    let mut j = 1;
    let label = opt_name(l, &mut j);
    let bt = parse_block_type(ctx, l, &mut j)?;
    // Any forms before `(then …)` are the condition operands.
    let then_at = l[j..]
        .iter()
        .position(|s| eq_kw(s, "then"))
        .map(|p| p + j)
        .ok_or(Error::BadForm)?;
    for k in j..then_at {
        emit_one(ctx, l, k)?;
    }
    ctx.record_hint(hint);
    ctx.out.push(0x04);
    emit_block_type(ctx, bt)?;
    ctx.open_label(label, &l[0])?;
    emit_seq(ctx, &want_list(&l[then_at])?[1..])?;
    if let Some(els) = l.get(then_at + 1).filter(|s| eq_kw(s, "else")) {
        ctx.out.push(0x05);
        emit_seq(ctx, &want_list(els)?[1..])?;
    }
    ctx.labels.pop();
    ctx.out.push(0x0b);
    Ok(())
}

/// A flat instruction: `op imm*`, with the operands already on the stack.
fn emit_flat(ctx: &mut Ctx, items: &[Sexpr], i: usize, name: &str) -> Result<usize> {
    use crate::opcode::Op as O;
    if let Some((sub, imm)) = lookup_simd(name) {
        return emit_simd(ctx, sub, imm, items, i + 1, false);
    }
    if let Some(sub) = lookup_atomic(name) {
        return emit_atomic(ctx, sub, items, i + 1, false);
    }
    let op = O::from_text_name(name).ok_or_else(|| classify_unknown_mnemonic(name))?;
    match op {
        O::Block | O::Loop | O::If => {
            let mut j = i + 1;
            let label = opt_name(items, &mut j);
            let bt = parse_block_type(ctx, items, &mut j)?;
            ctx.out.push(op as u8);
            emit_block_type(ctx, bt)?;
            ctx.open_label(label, &items[i])?;
            Ok(j)
        }
        O::TryLegacy => {
            // Flat legacy `try $l? bt?` — the body, handlers and `end` follow as siblings.
            let mut j = i + 1;
            let label = opt_name(items, &mut j);
            let bt = parse_block_type(ctx, items, &mut j)?;
            ctx.out.push(0x06);
            emit_block_type(ctx, bt)?;
            ctx.open_label(label, &items[i])?;
            ctx.flat_tries.push(FlatTry { depth: ctx.labels.len(), phase: TryPhase::Body });
            Ok(j)
        }
        O::CatchLegacy => {
            ctx.flat_try_clause(TryPhase::Catch)?;
            ctx.out.push(0x07);
            let tag = resolve_by_name(ctx.tag_names, nth(items, i + 1)?)?;
            uleb(&mut ctx.out, u64::from(tag));
            Ok(i + 2)
        }
        O::CatchAll => {
            ctx.flat_try_clause(TryPhase::CatchAll)?;
            ctx.out.push(0x19);
            Ok(i + 1)
        }
        // `delegate l` terminates its `try`, so its label resolves OUTSIDE it — the try's own
        // label comes off first. See `emit_folded_try`.
        O::Delegate => {
            ctx.flat_try_clause(TryPhase::Delegate)?;
            ctx.labels.pop();
            ctx.out.push(0x18);
            let target = ctx.resolve_label(nth(items, i + 1)?)?;
            uleb(&mut ctx.out, u64::from(target));
            Ok(i + 2)
        }
        O::TryTable => {
            // Flat: `try_table $l? blocktype? catch* … end`. The catch labels resolve in the
            // ENCLOSING scope, so the try_table's own label goes on only after them — see the
            // note in `emit_try_table`, which had the same off-by-one.
            let mut j = i + 1;
            let label = opt_name(items, &mut j);
            let bt = parse_block_type(ctx, items, &mut j)?;
            ctx.out.push(0x1f);
            emit_block_type(ctx, bt)?;
            emit_catch_clauses(ctx, items, &mut j)?;
            ctx.open_label(label, &items[i])?;
            Ok(j)
        }
        // §6.5.2: `else` and `end` may REPEAT the enclosing block's label — `(block $l … end $l)`.
        //
        // ⚠️ The id was not consumed until 2026-08-19, so it was read as the *next instruction*
        // and came back `UnknownInstr` naming a **label** — which is why `id.wast` and
        // `stack.wast` failed with a message that pointed at nothing. The repeat is CHECKED, not
        // skipped: §6.5.2 requires it to match the block it closes, and checking is the only
        // reason the form exists — a silently-ignored mismatch makes the annotation decorative.
        O::Else => {
            ctx.out.push(0x05);
            Ok(i + 1 + consume_matching_label(ctx, items, i + 1)?)
        }
        O::End => {
            ctx.out.push(0x0b);
            let n = consume_matching_label(ctx, items, i + 1)?;
            ctx.labels.pop();
            if ctx.flat_tries.last().is_some_and(|t| t.depth > ctx.labels.len()) {
                ctx.flat_tries.pop();
            }
            Ok(i + 1 + n)
        }
        // Flat `select (result t)?` — the immediate sits where the next instruction would.
        O::Select => {
            let mut j = i + 1;
            let tys = parse_select_result(ctx, items, &mut j)?;
            emit_select(ctx, &tys);
            Ok(j)
        }
        O::CallIndirect => emit_call_indirect(ctx, items, i + 1, false, 0x11),
        O::ReturnCallIndirect => emit_call_indirect(ctx, items, i + 1, false, 0x13),
        O::BrTable => {
            ctx.out.push(0x0e);
            emit_br_table_labels(ctx, items, i + 1)
        }
        _ => {
            let mut n = immediate_arity(op);
            // For ops whose index immediates may be omitted (defaulting to 0), consume only
            // the index-like atoms actually present — otherwise a bare `table.copy` would
            // swallow the two atoms of whatever instruction follows it.
            if has_optional_indices(op) {
                n = (0..n)
                    .take_while(|k| {
                        items
                            .get(i + 1 + k)
                            .and_then(Sexpr::as_atom)
                            .is_some_and(is_index_or_id)
                    })
                    .count();
            }
            let end = (i + 1 + n).min(items.len());
            // ⚠️ A load/store gets the UNtruncated slice: its memarg atoms sit past the fixed
            // arity, and the emitter is the half that knows how many of them there are. Handing
            // it `&items[..end]` is what silently dropped `offset=`/`align=` in the flat form.
            if takes_memarg(op) {
                emit_op_with_immediates(ctx, op, items, i + 1)?;
                let (j, ..) = parse_memarg_run(ctx, items, i + 1, false, 0)?;
                return Ok(j);
            }
            emit_op_with_immediates(ctx, op, &items[..end], i + 1)?;
            Ok(end)
        }
    }
}

fn is_index_or_id(a: &str) -> bool {
    a.starts_with('$') || a.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn takes_memarg(op: crate::opcode::Op) -> bool {
    use crate::opcode::Op as O;
    matches!(
        op,
        O::I32Load
            | O::I64Load
            | O::F32Load
            | O::F64Load
            | O::I32Load8S
            | O::I32Load8U
            | O::I32Load16S
            | O::I32Load16U
            | O::I64Load8S
            | O::I64Load8U
            | O::I64Load16S
            | O::I64Load16U
            | O::I64Load32S
            | O::I64Load32U
            | O::I32Store
            | O::I64Store
            | O::F32Store
            | O::F64Store
            | O::I32Store8
            | O::I32Store16
            | O::I64Store8
            | O::I64Store16
            | O::I64Store32
    )
}

/// Emit `op` plus its immediates, which start at `items[start]`.
fn emit_op_with_immediates(
    ctx: &mut Ctx,
    op: crate::opcode::Op,
    items: &[Sexpr],
    start: usize,
) -> Result<()> {
    use crate::opcode::Op as O;
    let imm = |k: usize| -> Result<&Sexpr> { items.get(start + k).ok_or(Error::BadImmediate) };

    // The prefixed families need their prefix byte plus a sub-opcode.
    match op {
        O::MemoryInit | O::DataDrop | O::MemoryCopy | O::MemoryFill => {
            ctx.out.push(0xfc);
            let sub: u64 = match op {
                O::MemoryInit => 8,
                O::DataDrop => 9,
                O::MemoryCopy => 10,
                _ => 11,
            };
            uleb(&mut ctx.out, sub);
        }
        // 🔴 **These had NO emitter arm until 2026-09-17, and fell through to the single-byte
        // catch-all — so the assembler wrote the raw tag byte `0xc5`–`0xcc`.** Those bytes are
        // unassigned in the single-byte space, so **every module wasmrt assembled containing a
        // saturating truncation was not WebAssembly**: wasmtime answers *"illegal opcode: 0xc5"*.
        // Our own decoder read them back (it accepted the raw bytes too), so the round trip was
        // green — `best-practices.md` §3.8b, the third wire divergence found this way.
        O::I32TruncSatF32S
        | O::I32TruncSatF32U
        | O::I32TruncSatF64S
        | O::I32TruncSatF64U
        | O::I64TruncSatF32S
        | O::I64TruncSatF32U
        | O::I64TruncSatF64S
        | O::I64TruncSatF64U => {
            ctx.out.push(0xfc);
            let sub: u64 = match op {
                O::I32TruncSatF32S => 0,
                O::I32TruncSatF32U => 1,
                O::I32TruncSatF64S => 2,
                O::I32TruncSatF64U => 3,
                O::I64TruncSatF32S => 4,
                O::I64TruncSatF32U => 5,
                O::I64TruncSatF64S => 6,
                _ => 7,
            };
            uleb(&mut ctx.out, sub);
        }
        O::I64Add128 | O::I64Sub128 | O::I64MulWideS | O::I64MulWideU => {
            ctx.out.push(0xfc);
            let sub: u64 = match op {
                O::I64Add128 => 19,
                O::I64Sub128 => 20,
                O::I64MulWideS => 21,
                _ => 22,
            };
            uleb(&mut ctx.out, sub);
        }
        O::TableInit | O::ElemDrop | O::TableCopy | O::TableGrow | O::TableSize | O::TableFill => {
            ctx.out.push(0xfc);
            let sub: u64 = match op {
                O::TableInit => 12,
                O::ElemDrop => 13,
                O::TableCopy => 14,
                O::TableGrow => 15,
                O::TableSize => 16,
                _ => 17,
            };
            uleb(&mut ctx.out, sub);
        }
        // The `0xFB` GC family. `ref.test`/`ref.cast` have separate non-null and nullable
        // sub-opcodes; the decoder folds each pair into one `Op` and keeps nullability in
        // the immediate, so the arm below re-selects the nullable sub-opcode.
        O::StructNew
        | O::StructNewDefault
        | O::StructGet
        | O::StructGetS
        | O::StructGetU
        | O::StructSet
        | O::ArrayNew
        | O::ArrayNewDefault
        | O::ArrayNewFixed
        | O::ArrayGet
        | O::ArrayGetS
        | O::ArrayGetU
        | O::ArraySet
        | O::ArrayLen
        | O::ArrayNewData
        | O::ArrayNewElem
        | O::ArrayFill
        | O::ArrayCopy
        | O::ArrayInitData
        | O::ArrayInitElem
        | O::RefTest
        | O::RefCastOp
        | O::BrOnCast
        | O::BrOnCastFail
        | O::StructNewDesc
        | O::StructNewDefaultDesc
        | O::RefGetDesc
        | O::RefCastDescEq
        | O::BrOnCastDescEq
        | O::BrOnCastDescEqFail
        | O::RefI31
        | O::I31GetS
        | O::I31GetU
        | O::AnyConvertExtern
        | O::ExternConvertAny => {
            ctx.out.push(0xfb);
            let sub: u64 = match op {
                O::StructNew => 0x00,
                O::StructNewDefault => 0x01,
                O::StructGet => 0x02,
                O::StructGetS => 0x03,
                O::StructGetU => 0x04,
                O::StructSet => 0x05,
                O::ArrayNew => 0x06,
                O::ArrayNewDefault => 0x07,
                O::ArrayNewFixed => 0x08,
                O::ArrayGet => 0x0b,
                O::ArrayGetS => 0x0c,
                O::ArrayGetU => 0x0d,
                O::ArraySet => 0x0e,
                O::ArrayLen => 0x0f,
                O::ArrayNewData => 0x09,
                O::ArrayNewElem => 0x0a,
                O::ArrayFill => 0x10,
                O::ArrayCopy => 0x11,
                O::ArrayInitData => 0x12,
                O::ArrayInitElem => 0x13,
                O::RefTest => 0x14,
                O::RefCastOp => 0x16,
                O::BrOnCast => 0x18,
                O::BrOnCastFail => 0x19,
                O::StructNewDesc => 0x20,
                O::StructNewDefaultDesc => 0x21,
                O::RefGetDesc => 0x22,
                // non-null; the cast arm below re-selects 0x24 for a nullable target.
                O::RefCastDescEq => 0x23,
                O::BrOnCastDescEq => 0x25,
                O::BrOnCastDescEqFail => 0x26,
                O::AnyConvertExtern => 0x1a,
                O::ExternConvertAny => 0x1b,
                O::RefI31 => 0x1c,
                O::I31GetS => 0x1d,
                _ => 0x1e,
            };
            uleb(&mut ctx.out, sub);
        }
        // Every remaining op is a real single-byte opcode. 🔒 **Converted FALLIBLY, because `Op`
        // is `#[repr(u16)]`**: an internal tag reaching here would mean the prefixed-family arms
        // above are missing a case, and `op as u8` would silently emit a TRUNCATED byte — a
        // different instruction. Refusing by name says which op has no emitter instead.
        _ => match u8::try_from(op as u16) {
            Ok(b) => ctx.out.push(b),
            Err(_) => return Err(Error::Unsupported(op.text_name())),
        },
    }

    match op {
        O::I32Const => {
            let v = parse_int_fit(want_atom(imm(0)?)?, 32)?;
            sleb(&mut ctx.out, i64::from(v as i32));
        }
        O::I64Const => sleb(&mut ctx.out, parse_int_fit(want_atom(imm(0)?)?, 64)?),
        O::F32Const => {
            let bits = parse_f32_bits(want_atom(imm(0)?)?, FloatCtx::Module).ok_or(Error::BadNumber)?;
            ctx.out.extend_from_slice(&bits.to_le_bytes());
        }
        O::F64Const => {
            let bits = parse_f64_bits(want_atom(imm(0)?)?, FloatCtx::Module).ok_or(Error::BadNumber)?;
            ctx.out.extend_from_slice(&bits.to_le_bytes());
        }
        O::LocalGet | O::LocalSet | O::LocalTee => {
            // Resolve before the `&mut ctx.out` borrow — `resolve_local` reads `ctx`.
            let idx = ctx.resolve_local(imm(0)?)?;
            uleb(&mut ctx.out, u64::from(idx));
        }
        O::GlobalGet | O::GlobalSet => {
            uleb(
                &mut ctx.out,
                u64::from(resolve_by_name(ctx.global_names, imm(0)?)?),
            );
        }
        O::Call | O::ReturnCall | O::RefFunc => {
            uleb(
                &mut ctx.out,
                u64::from(resolve_by_name(ctx.func_names, imm(0)?)?),
            );
        }
        O::Br | O::BrIf | O::Rethrow | O::BrOnNull | O::BrOnNonNull => {
            let l = ctx.resolve_label(imm(0)?)?;
            uleb(&mut ctx.out, u64::from(l));
        }
        O::Throw => {
            uleb(
                &mut ctx.out,
                u64::from(resolve_by_name(ctx.tag_names, imm(0)?)?),
            );
        }
        O::TableGet | O::TableSet | O::TableSize | O::TableGrow | O::TableFill => {
            let idx = match items.get(start) {
                Some(s) => resolve_by_name(ctx.table_names, s)?,
                None => 0,
            };
            uleb(&mut ctx.out, u64::from(idx));
        }
        O::ElemDrop => {
            uleb(
                &mut ctx.out,
                u64::from(resolve_by_name(ctx.elem_names, imm(0)?)?),
            );
        }
        O::DataDrop => {
            *ctx.needs_data_count = true;
            uleb(
                &mut ctx.out,
                u64::from(resolve_by_name(ctx.data_names, imm(0)?)?),
            );
        }
        O::TableInit => {
            // Two spellings: `table.init $table $elem`, or `table.init $elem` with the
            // table defaulting to 0. The binary order is always elem then table.
            let (table, elem) = match (items.get(start), items.get(start + 1)) {
                (Some(t), Some(e)) => (resolve_by_name(ctx.table_names, t)?, e),
                (Some(e), None) => (0, e),
                _ => return Err(Error::BadImmediate),
            };
            let e = resolve_by_name(ctx.elem_names, elem)?;
            uleb(&mut ctx.out, u64::from(e));
            uleb(&mut ctx.out, u64::from(table));
        }
        O::TableCopy => {
            // `table.copy $dst $src`, or bare with both defaulting to table 0.
            let d = items
                .get(start)
                .map_or(Ok(0), |s| resolve_by_name(ctx.table_names, s))?;
            let s = items
                .get(start + 1)
                .map_or(Ok(0), |s| resolve_by_name(ctx.table_names, s))?;
            uleb(&mut ctx.out, u64::from(d));
            uleb(&mut ctx.out, u64::from(s));
        }
        O::MemoryInit => {
            *ctx.needs_data_count = true;
            // Two spellings: `memory.init $mem $data`, or `memory.init $data` with the memory
            // defaulting to 0. The binary order is always data then memory.
            //
            // ⚠️⚠️ The operands were read in the WRONG ORDER for the two-operand form — first
            // as the data segment, second as the memory — so `(memory.init $mem2 0 …)` wrote
            // into the wrong memory. Silent wrong output where both indices exist, and a
            // spurious out-of-bounds trap where the mistaken memory is smaller.
            //
            // 🎓 `table.init` immediately below has had the correct two-spelling handling all
            // along, comment and all. **The sibling was right and this one was wrong** — a rule
            // applied at one of two sites, which is the shape that keeps recurring here.
            let (mem, data) = match (items.get(start), items.get(start + 1)) {
                (Some(m), Some(d)) => (resolve_by_name(ctx.mem_names, m)?, d),
                (Some(d), None) => (0, d),
                _ => return Err(Error::BadImmediate),
            };
            let d = resolve_by_name(ctx.data_names, data)?;
            uleb(&mut ctx.out, u64::from(d));
            uleb(&mut ctx.out, u64::from(mem));
        }
        O::MemoryCopy => {
            let d = items
                .get(start)
                .map_or(Ok(0), |s| resolve_by_name(ctx.mem_names, s))?;
            let s = items
                .get(start + 1)
                .map_or(Ok(0), |s| resolve_by_name(ctx.mem_names, s))?;
            uleb(&mut ctx.out, u64::from(d));
            uleb(&mut ctx.out, u64::from(s));
        }
        O::MemoryFill | O::MemorySize | O::MemoryGrow => {
            let m = items
                .get(start)
                .map_or(Ok(0), |s| resolve_by_name(ctx.mem_names, s))?;
            uleb(&mut ctx.out, u64::from(m));
        }
        // --- WasmGC ---
        O::StructNew | O::StructNewDefault | O::ArrayNew | O::ArrayNewDefault | O::ArrayGet
        | O::ArrayGetS | O::ArrayGetU | O::ArraySet | O::StructNewDesc | O::StructNewDefaultDesc
        | O::RefGetDesc => {
            let ti = resolve_by_name(ctx.type_names, imm(0)?)?;
            uleb(&mut ctx.out, u64::from(ti));
        }
        O::ArrayFill => {
            let ti = resolve_by_name(ctx.type_names, imm(0)?)?;
            uleb(&mut ctx.out, u64::from(ti));
        }
        O::ArrayNewData | O::ArrayInitData => {
            // ⚠️ These name a data segment exactly as `memory.init` does, so they require the
            // data-count section too — and only `memory.init`/`data.drop` set this flag, because
            // the list was written before GC landed and nothing re-read it. The result was an
            // emitter defect of the T10a kind: `wasmrt wat` produced a module wasmrt itself ran
            // happily and `wasm-tools validate` refused with "data count section required".
            *ctx.needs_data_count = true;
            let ti = resolve_by_name(ctx.type_names, imm(0)?)?;
            let d = resolve_by_name(ctx.data_names, imm(1)?)?;
            uleb(&mut ctx.out, u64::from(ti));
            uleb(&mut ctx.out, u64::from(d));
        }
        O::ArrayNewElem | O::ArrayInitElem => {
            let ti = resolve_by_name(ctx.type_names, imm(0)?)?;
            let e = resolve_by_name(ctx.elem_names, imm(1)?)?;
            uleb(&mut ctx.out, u64::from(ti));
            uleb(&mut ctx.out, u64::from(e));
        }
        O::ArrayCopy => {
            // `array.copy $dst $src` — destination type first, matching the binary order.
            let dst = resolve_by_name(ctx.type_names, imm(0)?)?;
            let src = resolve_by_name(ctx.type_names, imm(1)?)?;
            uleb(&mut ctx.out, u64::from(dst));
            uleb(&mut ctx.out, u64::from(src));
        }
        O::ArrayNewFixed => {
            let ti = resolve_by_name(ctx.type_names, imm(0)?)?;
            uleb(&mut ctx.out, u64::from(ti));
            uleb(&mut ctx.out, u64::from(parse_index(imm(1)?)?));
        }
        O::StructGet | O::StructGetS | O::StructGetU | O::StructSet => {
            let ti = resolve_by_name(ctx.type_names, imm(0)?)?;
            // A field may be named rather than numbered — `struct.get $T $field` is the
            // form binaryen and hand-written GC `.wat` actually emit.
            let names = ctx
                .field_names
                .get(ti as usize)
                .map_or(&[][..], Vec::as_slice);
            let fi = resolve_by_name(names, imm(1)?)?;
            uleb(&mut ctx.out, u64::from(ti));
            uleb(&mut ctx.out, u64::from(fi));
        }
        O::ArrayLen
        | O::RefI31
        | O::I31GetS
        | O::I31GetU
        | O::RefEq
        | O::AnyConvertExtern
        | O::ExternConvertAny => {}
        O::RefTest | O::RefCastOp | O::RefCastDescEq => {
            let (nullable, heap) = parse_ref_type_target(ctx, imm(0)?)?;
            if nullable {
                // Re-select the nullable sub-opcode (0x14→0x15 test, 0x16→0x17 cast, 0x23→0x24
                // desc-eq cast); the prefix arm above wrote the non-null one.
                let last = ctx.out.len() - 1;
                ctx.out[last] = match op {
                    O::RefTest => 0x15,
                    O::RefCastDescEq => 0x24,
                    _ => 0x17,
                };
            }
            emit_heap(&mut ctx.out, heap);
        }
        O::BrOnCast | O::BrOnCastFail | O::BrOnCastDescEq | O::BrOnCastDescEqFail => {
            let label = ctx.resolve_label(imm(0)?)?;
            let (n1, c1) = parse_ref_type_target(ctx, imm(1)?)?;
            let (n2, c2) = parse_ref_type_target(ctx, imm(2)?)?;
            // Flags: bit 0 = source nullable, bit 1 = target nullable.
            ctx.out
                .push(u8::from(n1) | (u8::from(n2) << 1));
            uleb(&mut ctx.out, u64::from(label));
            emit_heap(&mut ctx.out, c1);
            emit_heap(&mut ctx.out, c2);
        }
        O::RefNull => {
            // A heap type: an abstract head, or a **concrete** `$t`/index — legal, and
            // encoded as the same positive s33 type index `(ref $t)` already uses, so an
            // unknown head falls through to type-name resolution rather than being
            // rejected. Abstract codes come from the one `abstract_heap_code` table, so
            // `nofunc`/`noexn` cannot drift into their family heads.
            // ⚠️ `ref.null` was the **fifth** entry point with its own heap-type reader — the one that
            // survived fixing the other four (`(ref.null anyfunc)` came back `BadNumber`). It now reads
            // through `parse_heap`, the one reader, which carries that guard and `(exact $t)` both.
            emit_heap(&mut ctx.out, parse_heap(ctx.type_names, imm(0)?)?);
        }
        // `call_ref`/`return_call_ref` take a **type** index, not a function index — the callee is
        // the `funcref` on the stack, and the immediate is the signature to check it against.
        O::CallRef | O::ReturnCallRef => {
            uleb(
                &mut ctx.out,
                u64::from(resolve_by_name(ctx.type_names, imm(0)?)?),
            );
        }
        // Genuinely no immediates. ⚠️ This arm is why `call_ref` was silently wrong for so long: an
        // op that *does* take an immediate and lacks a case above lands here, emits its opcode
        // alone, and reports nothing. `emitter_covers_every_op_with_an_immediate` in the tests
        // below now makes that a build-time failure instead of a malformed module.
        _ => {}
    }

    // Loads/stores: `memidx? offset=? align=?` follows the opcode.
    //
    // ⚠️⚠️ **This had its own copy of the scan, and in FLAT form it never ran at all.** `emit_flat`
    // truncated `items` to the fixed-arity end before calling here — which for a load/store is
    // `start` itself — so the loop looked one past the end of its own input, found nothing, and
    // emitted the DEFAULT memarg. `i32.const 0 i32.load offset=4` read at offset **0** and
    // returned the wrong number: no error, no diagnostic. A separate loop in `emit_flat` then
    // skipped the atoms so nothing downstream noticed they had been dropped.
    //
    // It now uses the SIMD family's scan, which was already correct because the flat form forced
    // it to be: it stops at the first atom that is neither `offset=`/`align=` nor index-like, so
    // a following mnemonic (`drop`, `i32.const`) is not swallowed as a memory index. That is the
    // whole reason the two copies differed, and it is why there is now one.
    if takes_memarg(op) {
        let (_, align, mem, offset, _) =
            parse_memarg_run(ctx, items, start, false, crate::opcode::natural_align_log2(op))?;
        emit_memarg_bytes(&mut ctx.out, align, mem, offset);
    }
    Ok(())
}

// --- SIMD (`0xFD`) and atomics (`0xFE`) ---------------------------------------

/// The immediate shape of a `0xFD` op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimdImm {
    /// No immediate.
    None,
    /// A single lane index byte.
    Lane,
    /// 16 lane indices (`i8x16.shuffle`).
    Shuffle,
    /// A 16-byte literal (`v128.const`).
    Const,
    /// A memarg.
    Mem,
    /// A memarg followed by a lane index.
    MemLane,
}

/// Map a `0xFD` mnemonic to its sub-opcode and immediate shape.
///
/// Kept as one table so a name and its sub-opcode cannot drift apart; the sub-opcodes are
/// the same ones `decode_simd` reads and `simd_natural_align_log2` sizes.
fn lookup_simd(name: &str) -> Option<(u32, SimdImm)> {
    use SimdImm::{Const, Lane, Mem, MemLane, None as N, Shuffle};
    const TBL: &[(&str, u32, SimdImm)] = &[
        // loads / stores
        ("v128.load", 0x00, Mem),
        ("v128.load8x8_s", 0x01, Mem),
        ("v128.load8x8_u", 0x02, Mem),
        ("v128.load16x4_s", 0x03, Mem),
        ("v128.load16x4_u", 0x04, Mem),
        ("v128.load32x2_s", 0x05, Mem),
        ("v128.load32x2_u", 0x06, Mem),
        ("v128.load8_splat", 0x07, Mem),
        ("v128.load16_splat", 0x08, Mem),
        ("v128.load32_splat", 0x09, Mem),
        ("v128.load64_splat", 0x0a, Mem),
        ("v128.store", 0x0b, Mem),
        ("v128.load32_zero", 0x5c, Mem),
        ("v128.load64_zero", 0x5d, Mem),
        ("v128.load8_lane", 0x54, MemLane),
        ("v128.load16_lane", 0x55, MemLane),
        ("v128.load32_lane", 0x56, MemLane),
        ("v128.load64_lane", 0x57, MemLane),
        ("v128.store8_lane", 0x58, MemLane),
        ("v128.store16_lane", 0x59, MemLane),
        ("v128.store32_lane", 0x5a, MemLane),
        ("v128.store64_lane", 0x5b, MemLane),
        // const / shuffle / swizzle / splat
        ("v128.const", 0x0c, Const),
        ("i8x16.shuffle", 0x0d, Shuffle),
        ("i8x16.swizzle", 0x0e, N),
        ("i8x16.splat", 0x0f, N),
        ("i16x8.splat", 0x10, N),
        ("i32x4.splat", 0x11, N),
        ("i64x2.splat", 0x12, N),
        ("f32x4.splat", 0x13, N),
        ("f64x2.splat", 0x14, N),
        // lane access
        ("i8x16.extract_lane_s", 0x15, Lane),
        ("i8x16.extract_lane_u", 0x16, Lane),
        ("i8x16.replace_lane", 0x17, Lane),
        ("i16x8.extract_lane_s", 0x18, Lane),
        ("i16x8.extract_lane_u", 0x19, Lane),
        ("i16x8.replace_lane", 0x1a, Lane),
        ("i32x4.extract_lane", 0x1b, Lane),
        ("i32x4.replace_lane", 0x1c, Lane),
        ("i64x2.extract_lane", 0x1d, Lane),
        ("i64x2.replace_lane", 0x1e, Lane),
        ("f32x4.extract_lane", 0x1f, Lane),
        ("f32x4.replace_lane", 0x20, Lane),
        ("f64x2.extract_lane", 0x21, Lane),
        ("f64x2.replace_lane", 0x22, Lane),
        // comparisons
        ("i8x16.eq", 0x23, N),
        ("i8x16.ne", 0x24, N),
        ("i8x16.lt_s", 0x25, N),
        ("i8x16.lt_u", 0x26, N),
        ("i8x16.gt_s", 0x27, N),
        ("i8x16.gt_u", 0x28, N),
        ("i8x16.le_s", 0x29, N),
        ("i8x16.le_u", 0x2a, N),
        ("i8x16.ge_s", 0x2b, N),
        ("i8x16.ge_u", 0x2c, N),
        ("i16x8.eq", 0x2d, N),
        ("i16x8.ne", 0x2e, N),
        ("i16x8.lt_s", 0x2f, N),
        ("i16x8.lt_u", 0x30, N),
        ("i16x8.gt_s", 0x31, N),
        ("i16x8.gt_u", 0x32, N),
        ("i16x8.le_s", 0x33, N),
        ("i16x8.le_u", 0x34, N),
        ("i16x8.ge_s", 0x35, N),
        ("i16x8.ge_u", 0x36, N),
        ("i32x4.eq", 0x37, N),
        ("i32x4.ne", 0x38, N),
        ("i32x4.lt_s", 0x39, N),
        ("i32x4.lt_u", 0x3a, N),
        ("i32x4.gt_s", 0x3b, N),
        ("i32x4.gt_u", 0x3c, N),
        ("i32x4.le_s", 0x3d, N),
        ("i32x4.le_u", 0x3e, N),
        ("i32x4.ge_s", 0x3f, N),
        ("i32x4.ge_u", 0x40, N),
        ("f32x4.eq", 0x41, N),
        ("f32x4.ne", 0x42, N),
        ("f32x4.lt", 0x43, N),
        ("f32x4.gt", 0x44, N),
        ("f32x4.le", 0x45, N),
        ("f32x4.ge", 0x46, N),
        ("f64x2.eq", 0x47, N),
        ("f64x2.ne", 0x48, N),
        ("f64x2.lt", 0x49, N),
        ("f64x2.gt", 0x4a, N),
        ("f64x2.le", 0x4b, N),
        ("f64x2.ge", 0x4c, N),
        ("i64x2.eq", 0xd6, N),
        ("i64x2.ne", 0xd7, N),
        ("i64x2.lt_s", 0xd8, N),
        ("i64x2.gt_s", 0xd9, N),
        ("i64x2.le_s", 0xda, N),
        ("i64x2.ge_s", 0xdb, N),
        // bitwise
        ("v128.not", 0x4d, N),
        ("v128.and", 0x4e, N),
        ("v128.andnot", 0x4f, N),
        ("v128.or", 0x50, N),
        ("v128.xor", 0x51, N),
        ("v128.bitselect", 0x52, N),
        ("v128.any_true", 0x53, N),
        // i8x16
        ("i8x16.abs", 0x60, N),
        ("i8x16.neg", 0x61, N),
        ("i8x16.popcnt", 0x62, N),
        ("i8x16.all_true", 0x63, N),
        ("i8x16.bitmask", 0x64, N),
        ("i8x16.narrow_i16x8_s", 0x65, N),
        ("i8x16.narrow_i16x8_u", 0x66, N),
        ("i8x16.shl", 0x6b, N),
        ("i8x16.shr_s", 0x6c, N),
        ("i8x16.shr_u", 0x6d, N),
        ("i8x16.add", 0x6e, N),
        ("i8x16.add_sat_s", 0x6f, N),
        ("i8x16.add_sat_u", 0x70, N),
        ("i8x16.sub", 0x71, N),
        ("i8x16.sub_sat_s", 0x72, N),
        ("i8x16.sub_sat_u", 0x73, N),
        ("i8x16.min_s", 0x76, N),
        ("i8x16.min_u", 0x77, N),
        ("i8x16.max_s", 0x78, N),
        ("i8x16.max_u", 0x79, N),
        ("i8x16.avgr_u", 0x7b, N),
        // extadd / q15 / dot
        ("i16x8.extadd_pairwise_i8x16_s", 0x7c, N),
        ("i16x8.extadd_pairwise_i8x16_u", 0x7d, N),
        ("i32x4.extadd_pairwise_i16x8_s", 0x7e, N),
        ("i32x4.extadd_pairwise_i16x8_u", 0x7f, N),
        ("i16x8.q15mulr_sat_s", 0x82, N),
        ("i32x4.dot_i16x8_s", 0xba, N),
        // extmul
        ("i16x8.extmul_low_i8x16_s", 0x9c, N),
        ("i16x8.extmul_high_i8x16_s", 0x9d, N),
        ("i16x8.extmul_low_i8x16_u", 0x9e, N),
        ("i16x8.extmul_high_i8x16_u", 0x9f, N),
        ("i32x4.extmul_low_i16x8_s", 0xbc, N),
        ("i32x4.extmul_high_i16x8_s", 0xbd, N),
        ("i32x4.extmul_low_i16x8_u", 0xbe, N),
        ("i32x4.extmul_high_i16x8_u", 0xbf, N),
        ("i64x2.extmul_low_i32x4_s", 0xdc, N),
        ("i64x2.extmul_high_i32x4_s", 0xdd, N),
        ("i64x2.extmul_low_i32x4_u", 0xde, N),
        ("i64x2.extmul_high_i32x4_u", 0xdf, N),
        // i16x8
        ("i16x8.abs", 0x80, N),
        ("i16x8.neg", 0x81, N),
        ("i16x8.all_true", 0x83, N),
        ("i16x8.bitmask", 0x84, N),
        ("i16x8.narrow_i32x4_s", 0x85, N),
        ("i16x8.narrow_i32x4_u", 0x86, N),
        ("i16x8.extend_low_i8x16_s", 0x87, N),
        ("i16x8.extend_high_i8x16_s", 0x88, N),
        ("i16x8.extend_low_i8x16_u", 0x89, N),
        ("i16x8.extend_high_i8x16_u", 0x8a, N),
        ("i16x8.shl", 0x8b, N),
        ("i16x8.shr_s", 0x8c, N),
        ("i16x8.shr_u", 0x8d, N),
        ("i16x8.add", 0x8e, N),
        ("i16x8.add_sat_s", 0x8f, N),
        ("i16x8.add_sat_u", 0x90, N),
        ("i16x8.sub", 0x91, N),
        ("i16x8.sub_sat_s", 0x92, N),
        ("i16x8.sub_sat_u", 0x93, N),
        ("i16x8.mul", 0x95, N),
        ("i16x8.min_s", 0x96, N),
        ("i16x8.min_u", 0x97, N),
        ("i16x8.max_s", 0x98, N),
        ("i16x8.max_u", 0x99, N),
        ("i16x8.avgr_u", 0x9b, N),
        // i32x4
        ("i32x4.abs", 0xa0, N),
        ("i32x4.neg", 0xa1, N),
        ("i32x4.all_true", 0xa3, N),
        ("i32x4.bitmask", 0xa4, N),
        ("i32x4.extend_low_i16x8_s", 0xa7, N),
        ("i32x4.extend_high_i16x8_s", 0xa8, N),
        ("i32x4.extend_low_i16x8_u", 0xa9, N),
        ("i32x4.extend_high_i16x8_u", 0xaa, N),
        ("i32x4.shl", 0xab, N),
        ("i32x4.shr_s", 0xac, N),
        ("i32x4.shr_u", 0xad, N),
        ("i32x4.add", 0xae, N),
        ("i32x4.sub", 0xb1, N),
        ("i32x4.mul", 0xb5, N),
        ("i32x4.min_s", 0xb6, N),
        ("i32x4.min_u", 0xb7, N),
        ("i32x4.max_s", 0xb8, N),
        ("i32x4.max_u", 0xb9, N),
        // i64x2
        ("i64x2.abs", 0xc0, N),
        ("i64x2.neg", 0xc1, N),
        ("i64x2.all_true", 0xc3, N),
        ("i64x2.bitmask", 0xc4, N),
        ("i64x2.extend_low_i32x4_s", 0xc7, N),
        ("i64x2.extend_high_i32x4_s", 0xc8, N),
        ("i64x2.extend_low_i32x4_u", 0xc9, N),
        ("i64x2.extend_high_i32x4_u", 0xca, N),
        ("i64x2.shl", 0xcb, N),
        ("i64x2.shr_s", 0xcc, N),
        ("i64x2.shr_u", 0xcd, N),
        ("i64x2.add", 0xce, N),
        ("i64x2.sub", 0xd1, N),
        ("i64x2.mul", 0xd5, N),
        // f32x4
        ("f32x4.ceil", 0x67, N),
        ("f32x4.floor", 0x68, N),
        ("f32x4.trunc", 0x69, N),
        ("f32x4.nearest", 0x6a, N),
        ("f32x4.abs", 0xe0, N),
        ("f32x4.neg", 0xe1, N),
        ("f32x4.sqrt", 0xe3, N),
        ("f32x4.add", 0xe4, N),
        ("f32x4.sub", 0xe5, N),
        ("f32x4.mul", 0xe6, N),
        ("f32x4.div", 0xe7, N),
        ("f32x4.min", 0xe8, N),
        ("f32x4.max", 0xe9, N),
        ("f32x4.pmin", 0xea, N),
        ("f32x4.pmax", 0xeb, N),
        ("f32x4.convert_i32x4_s", 0xfa, N),
        ("f32x4.convert_i32x4_u", 0xfb, N),
        ("f32x4.demote_f64x2_zero", 0x5e, N),
        // f64x2
        ("f64x2.ceil", 0x74, N),
        ("f64x2.floor", 0x75, N),
        ("f64x2.trunc", 0x7a, N),
        ("f64x2.nearest", 0x94, N),
        ("f64x2.abs", 0xec, N),
        ("f64x2.neg", 0xed, N),
        ("f64x2.sqrt", 0xef, N),
        ("f64x2.add", 0xf0, N),
        ("f64x2.sub", 0xf1, N),
        ("f64x2.mul", 0xf2, N),
        ("f64x2.div", 0xf3, N),
        ("f64x2.min", 0xf4, N),
        ("f64x2.max", 0xf5, N),
        ("f64x2.pmin", 0xf6, N),
        ("f64x2.pmax", 0xf7, N),
        ("f64x2.promote_low_f32x4", 0x5f, N),
        ("f64x2.convert_low_i32x4_s", 0xfe, N),
        ("f64x2.convert_low_i32x4_u", 0xff, N),
        // trunc_sat
        ("i32x4.trunc_sat_f32x4_s", 0xf8, N),
        ("i32x4.trunc_sat_f32x4_u", 0xf9, N),
        ("i32x4.trunc_sat_f64x2_s_zero", 0xfc, N),
        ("i32x4.trunc_sat_f64x2_u_zero", 0xfd, N),
        // relaxed SIMD (sub-opcodes >= 0x100)
        ("i8x16.relaxed_swizzle", 0x100, N),
        ("i32x4.relaxed_trunc_f32x4_s", 0x101, N),
        ("i32x4.relaxed_trunc_f32x4_u", 0x102, N),
        ("i32x4.relaxed_trunc_f64x2_s_zero", 0x103, N),
        ("i32x4.relaxed_trunc_f64x2_u_zero", 0x104, N),
        ("f32x4.relaxed_madd", 0x105, N),
        ("f32x4.relaxed_nmadd", 0x106, N),
        ("f64x2.relaxed_madd", 0x107, N),
        ("f64x2.relaxed_nmadd", 0x108, N),
        ("i8x16.relaxed_laneselect", 0x109, N),
        ("i16x8.relaxed_laneselect", 0x10a, N),
        ("i32x4.relaxed_laneselect", 0x10b, N),
        ("i64x2.relaxed_laneselect", 0x10c, N),
        ("f32x4.relaxed_min", 0x10d, N),
        ("f32x4.relaxed_max", 0x10e, N),
        ("f64x2.relaxed_min", 0x10f, N),
        ("f64x2.relaxed_max", 0x110, N),
        ("i16x8.relaxed_q15mulr_s", 0x111, N),
        ("i16x8.relaxed_dot_i8x16_i7x16_s", 0x112, N),
        ("i32x4.relaxed_dot_i8x16_i7x16_add_s", 0x113, N),
    ];
    TBL.iter()
        .find(|(n, _, _)| *n == name)
        .map(|&(_, s, i)| (s, i))
}

/// Map a `0xFE` mnemonic to its sub-opcode.
///
/// The rmw/cmpxchg families are **generated** from their layout rather than transcribed:
/// groups of 7 from `0x1e`, ordered add/sub/and/or/xor/xchg/cmpxchg, each laid out
/// `[i32.full, i64.full, i32.8, i32.16, i64.8, i64.16, i64.32]`. That is the same layout
/// `atomic_natural_align_log2` and `atomic_val_type` encode, so describing it once keeps
/// the three in step.
fn lookup_atomic(name: &str) -> Option<u32> {
    const FIXED: &[(&str, u32)] = &[
        ("memory.atomic.notify", 0x00),
        ("memory.atomic.wait32", 0x01),
        ("memory.atomic.wait64", 0x02),
        ("atomic.fence", 0x03),
        ("i32.atomic.load", 0x10),
        ("i64.atomic.load", 0x11),
        ("i32.atomic.load8_u", 0x12),
        ("i32.atomic.load16_u", 0x13),
        ("i64.atomic.load8_u", 0x14),
        ("i64.atomic.load16_u", 0x15),
        ("i64.atomic.load32_u", 0x16),
        ("i32.atomic.store", 0x17),
        ("i64.atomic.store", 0x18),
        ("i32.atomic.store8", 0x19),
        ("i32.atomic.store16", 0x1a),
        ("i64.atomic.store8", 0x1b),
        ("i64.atomic.store16", 0x1c),
        ("i64.atomic.store32", 0x1d),
    ];
    if let Some(&(_, s)) = FIXED.iter().find(|(n, _)| *n == name) {
        return Some(s);
    }
    // `<ty>.atomic.rmw<width>.<op>[_u]`
    let (ty, rest) = match name.strip_prefix("i32.atomic.rmw") {
        Some(r) => (0u32, r),
        None => (1u32, name.strip_prefix("i64.atomic.rmw")?),
    };
    let (width, rest) = if let Some(r) = rest.strip_prefix('8') {
        (8u32, r)
    } else if let Some(r) = rest.strip_prefix("16") {
        (16, r)
    } else if let Some(r) = rest.strip_prefix("32") {
        (32, r)
    } else {
        (0, rest) // the full-width form
    };
    let op_name = rest.strip_prefix('.')?;
    // A sub-width op is spelled with a `_u` suffix; the full-width one is not.
    let op_name = if width == 0 {
        op_name
    } else {
        op_name.strip_suffix("_u")?
    };
    let group = match op_name {
        "add" => 0u32,
        "sub" => 1,
        "and" => 2,
        "or" => 3,
        "xor" => 4,
        "xchg" => 5,
        "cmpxchg" => 6,
        _ => return None,
    };
    // Position within the group.
    let idx = match (ty, width) {
        (0, 0) => 0u32,
        (1, 0) => 1,
        (0, 8) => 2,
        (0, 16) => 3,
        (1, 8) => 4,
        (1, 16) => 5,
        (1, 32) => 6,
        _ => return None, // e.g. `i32.atomic.rmw32.*` does not exist
    };
    Some(0x1e + group * 7 + idx)
}

/// Parse a lane / shuffle index atom into a byte. `(i32x4.extract_lane 999)` must be a
/// clean `BadImmediate`, not a wrapping cast — the decoder range-checks the lane against
/// the op's lane count, but only if the byte it sees is the one the source wrote.
fn simd_lane_byte(s: &Sexpr) -> Result<u8> {
    let a = want_atom(s)?;
    // §6.3.1: a lane index is an **unsigned** integer literal, and `uN` admits no sign — so
    // `+1` and `-1` are both malformed, whatever their value.
    //
    // ⚠️ `-1` was already refused, but only because `u8::try_from(-1 as u32)` overflows — a
    // RANGE check doing a SIGN check's job by accident. `+1` is in range, so it sailed through
    // and seven `assert_malformed`s in `simd_lane.wast` accepted a malformed module.
    // 🎓 **A check that happens to cover a rule for some inputs makes the rule look
    // implemented** — the negative cases passing is exactly why nobody looked at the positive
    // ones.
    if a.starts_with('+') || a.starts_with('-') {
        return Err(Error::BadImmediate);
    }
    let v = parse_i64_str(a)?;
    u8::try_from(v as u32).map_err(|_| Error::BadImmediate)
}

/// Parse a `v128.const` literal: a shape keyword then its lanes.
fn parse_v128_const(items: &[Sexpr], mut j: usize, out: &mut [u8; 16]) -> Result<usize> {
    let shape = want_atom(nth(items, j)?)?;
    j += 1;
    let lanes: usize = match shape {
        "i8x16" => 16,
        "i16x8" => 8,
        "i32x4" | "f32x4" => 4,
        "i64x2" | "f64x2" => 2,
        _ => return Err(Error::BadImmediate),
    };
    for k in 0..lanes {
        let s = nth(items, j)?;
        let a = want_atom(s)?;
        match shape {
            "f32x4" => {
                let bits = parse_f32_bits(a, FloatCtx::Module).ok_or(Error::BadNumber)?;
                out[k * 4..k * 4 + 4].copy_from_slice(&bits.to_le_bytes());
            }
            "f64x2" => {
                let bits = parse_f64_bits(a, FloatCtx::Module).ok_or(Error::BadNumber)?;
                out[k * 8..k * 8 + 8].copy_from_slice(&bits.to_le_bytes());
            }
            "i8x16" => out[k] = parse_int_fit(a, 8)? as u8,
            "i16x8" => {
                let v = parse_int_fit(a, 16)? as u16;
                out[k * 2..k * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
            "i32x4" => {
                let v = parse_int_fit(a, 32)? as u32;
                out[k * 4..k * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            "i64x2" => {
                let v = parse_int_fit(a, 64)? as u64;
                out[k * 8..k * 8 + 8].copy_from_slice(&v.to_le_bytes());
            }
            // Never `unreachable!()`: a shape the lane-count match accepts but this one
            // forgets would panic the whole embedder rather than reject one module. That
            // is exactly how `v128.const i64x2` aborted the first conformance run.
            _ => return Err(Error::BadImmediate),
        }
        j += 1;
    }
    Ok(j)
}

/// Emit a memarg's alignment + optional memory index + offset.
fn emit_memarg_bytes(out: &mut Vec<u8>, align_log2: u32, mem: u32, offset: u64) {
    if mem == 0 {
        uleb(out, u64::from(align_log2));
    } else {
        uleb(out, u64::from(align_log2 | 0x40));
        uleb(out, u64::from(mem));
    }
    uleb(out, offset);
}

/// Collect the leading memarg-ish atom run: `memidx? offset=? align=?`, plus a trailing
/// lane index for the `*_lane` ops.
///
/// 🔒 **The one authority for a memarg, scalar and SIMD alike.** It lived in the SIMD emitter and
/// the scalar loads/stores had their own copy — which never handled the flat form and dropped
/// `offset=`/`align=` there entirely, a wrong VALUE rather than a rejection.
///
/// Only `offset=`/`align=` atoms and index-like atoms are taken. Stopping at anything else
/// matters in the FLAT form, where `items` is the whole sibling instruction sequence: a
/// following mnemonic (`drop`, `i32.const`) is not index-like and must NOT be swallowed as
/// a memory or lane index.
fn parse_memarg_run(
    ctx: &Ctx,
    items: &[Sexpr],
    mut j: usize,
    want_lane: bool,
    default_align: u32,
) -> Result<(usize, u32, u32, u64, u8)> {
    let mut atoms: Vec<&Sexpr> = Vec::new();
    while let Some(s) = items.get(j) {
        let Some(a) = s.as_atom() else { break };
        let is_memarg = a.starts_with("offset=") || a.starts_with("align=");
        if !is_memarg && !is_index_or_id(a) {
            break;
        }
        atoms.push(s);
        j += 1;
    }
    let mut lane = 0u8;
    if want_lane {
        let last = atoms.pop().ok_or(Error::BadImmediate)?;
        lane = simd_lane_byte(last)?;
    }
    let mut align = default_align;
    let mut offset = 0u64;
    let mut mem = 0u32;
    for (k, s) in atoms.iter().enumerate() {
        let a = s.as_atom().unwrap_or_default();
        if let Some(v) = a.strip_prefix("offset=") {
            offset = parse_u64_str(v)?;
        } else if let Some(v) = a.strip_prefix("align=") {
            let bytes = parse_u64_str(v)?;
            if bytes == 0 || !bytes.is_power_of_two() {
                return Err(Error::BadImmediate);
            }
            align = bytes.trailing_zeros();
        } else if k == 0 {
            mem = resolve_by_name(ctx.mem_names, s)?; // memidx precedes the memarg
        } else {
            return Err(Error::BadImmediate);
        }
    }
    Ok((j, align, mem, offset, lane))
}

/// Emit a `0xFD` SIMD op: parse its immediate, emit operand sub-expressions (folded form
/// only), then `0xFD sub imm`. Returns the index just past the instruction's forms.
fn emit_simd(
    ctx: &mut Ctx,
    sub: u32,
    imm: SimdImm,
    items: &[Sexpr],
    start: usize,
    folded: bool,
) -> Result<usize> {
    let mut j = start;
    let mut lane = 0u8;
    let mut cbytes = [0u8; 16];
    let mut align = crate::opcode::simd_natural_align_log2(sub);
    let mut offset = 0u64;
    let mut mem = 0u32;
    match imm {
        SimdImm::None => {}
        SimdImm::Lane => {
            lane = simd_lane_byte(nth(items, j)?)?;
            j += 1;
        }
        SimdImm::Shuffle => {
            for slot in &mut cbytes {
                *slot = simd_lane_byte(nth(items, j)?)?;
                j += 1;
            }
        }
        SimdImm::Const => j = parse_v128_const(items, j, &mut cbytes)?,
        SimdImm::Mem | SimdImm::MemLane => {
            let want_lane = imm == SimdImm::MemLane;
            let r = parse_memarg_run(ctx, items, j, want_lane, align)?;
            j = r.0;
            align = r.1;
            mem = r.2;
            offset = r.3;
            lane = r.4;
        }
    }
    if folded {
        while j < items.len() {
            j = emit_one(ctx, items, j)?;
        }
        ctx.record_own_hint();
    }
    ctx.out.push(0xfd);
    uleb(&mut ctx.out, u64::from(sub));
    match imm {
        SimdImm::None => {}
        SimdImm::Lane => ctx.out.push(lane),
        SimdImm::Shuffle | SimdImm::Const => ctx.out.extend_from_slice(&cbytes),
        SimdImm::Mem => emit_memarg_bytes(&mut ctx.out, align, mem, offset),
        SimdImm::MemLane => {
            emit_memarg_bytes(&mut ctx.out, align, mem, offset);
            ctx.out.push(lane);
        }
    }
    Ok(j)
}

/// Emit a `0xFE` atomic op. Every member except `atomic.fence` carries a memarg, and its
/// alignment must be **exactly** natural — so an omitted `align=` defaults to that, and a
/// wrong explicit one is the validator's to reject.
fn emit_atomic(
    ctx: &mut Ctx,
    sub: u32,
    items: &[Sexpr],
    start: usize,
    folded: bool,
) -> Result<usize> {
    let mut j = start;
    let natural = crate::opcode::atomic_natural_align_log2(sub);
    if sub == 0x03 {
        // `atomic.fence` carries a reserved zero byte, no memarg.
        if folded {
            while j < items.len() {
                j = emit_one(ctx, items, j)?;
            }
            ctx.record_own_hint();
        }
        ctx.out.push(0xfe);
        uleb(&mut ctx.out, u64::from(sub));
        ctx.out.push(0x00);
        return Ok(j);
    }
    let (mut j, align, mem, offset, _) = parse_memarg_run(ctx, items, j, false, natural)?;
    if folded {
        while j < items.len() {
            j = emit_one(ctx, items, j)?;
        }
        ctx.record_own_hint();
    }
    ctx.out.push(0xfe);
    uleb(&mut ctx.out, u64::from(sub));
    emit_memarg_bytes(&mut ctx.out, align, mem, offset);
    Ok(j)
}

/// The binary code of an abstract heap type, as the `s33` a cast/`ref.null` immediate uses.
fn abstract_heap_code(atom: &str) -> Option<i64> {
    Some(match atom {
        "nofunc" => -0x0d,
        "noextern" => -0x0e,
        "none" => -0x0f,
        "func" => -0x10,
        "extern" => -0x11,
        "any" => -0x12,
        "eq" => -0x13,
        "i31" => -0x14,
        "struct" => -0x15,
        "array" => -0x16,
        "exn" => -0x17,
        "noexn" => -0x0c,
        _ => return None,
    })
}

/// Parse a `ref.test` / `ref.cast` / `br_on_cast` type target: `(ref null? ht)` or a bare
/// heap type. Returns `(nullable, heap-type code)` — a concrete `$t` yields its type index
/// as a non-negative code, an abstract head its negative one.
fn parse_ref_type_target(ctx: &Ctx, s: &Sexpr) -> Result<(bool, Heap)> {
    // The list form `(ref null? ht)`.
    if let Some(l) = s.as_list() {
        if l.len() >= 2 && eq_atom(&l[0], "ref") {
            let nullable = ref_form_nullable(l)?;
            return Ok((nullable, parse_heap(ctx.type_names, &l[l.len() - 1])?));
        }
        return Err(Error::BadImmediate);
    }
    // A bare heap type: the `…ref` shorthands are nullable, bare heads are not.
    let a = want_atom(s)?;
    // Refused HERE rather than falling through to `resolve_by_name`, which would read it as a
    // type index and report `BadNumber` — the wrong bucket.
    if let Some(modern) = obsolete_type_keyword(a) {
        return Err(Error::ObsoleteKeyword(modern));
    }
    let (nullable, head) = match a {
        "funcref" => (true, "func"),
        "externref" => (true, "extern"),
        "anyref" => (true, "any"),
        "eqref" => (true, "eq"),
        "i31ref" => (true, "i31"),
        "structref" => (true, "struct"),
        "arrayref" => (true, "array"),
        "exnref" => (true, "exn"),
        "nullref" => (true, "none"),
        "nullfuncref" => (true, "nofunc"),
        "nullexternref" => (true, "noextern"),
        "nullexnref" => (true, "noexn"),
        other => (false, other),
    };
    if let Some(code) = abstract_heap_code(head) {
        return Ok((nullable, Heap::Code(code)));
    }
    Ok((false, Heap::Code(i64::from(resolve_by_name(ctx.type_names, s)?))))
}

/// A heap type as written: one `s33` (an abstract code, or a concrete type index), or EXACT.
#[derive(Debug, Clone, Copy)]
enum Heap {
    Code(i64),
    /// `(exact $t)` — custom-descriptors; encoded `0x62` + an unsigned type index.
    Exact(u32),
}

/// Write a heap type: the one writer, so `0x62` is emitted in one place.
fn emit_heap(out: &mut Vec<u8>, h: Heap) {
    match h {
        Heap::Code(c) => sleb(out, c),
        Heap::Exact(ti) => {
            out.push(0x62);
            uleb(out, u64::from(ti));
        }
    }
}

/// `(exact $t)` — `Some(index)` for that list, `None` for anything else.
///
/// Only a CONCRETE type may be exact: `(exact any)`, `(exact)` and the bare `exact 0` are all
/// "unexpected token" in `exact.wast`, so the refusal is `UnexpectedToken` (malformed text).
fn parse_exact(s: &Sexpr, type_names: &[Option<String>]) -> Result<Option<u32>> {
    let Some(l) = s.as_list() else {
        return Ok(None);
    };
    if !l.first().is_some_and(|x| eq_atom(x, "exact")) {
        return Ok(None);
    }
    let [_, t] = l else {
        return Err(Error::UnexpectedToken);
    };
    let a = want_atom(t).map_err(|_| Error::UnexpectedToken)?;
    if !(a.starts_with('$') || a.starts_with(|c: char| c.is_ascii_digit())) {
        return Err(Error::UnexpectedToken);
    }
    let ti = resolve_by_name(type_names, t)?;
    // As for `concrete_ref`: the index is packed into 27 bits and would truncate.
    if ti > V::MAX_CONCRETE_INDEX {
        return Err(Error::BadImmediate);
    }
    Ok(Some(ti))
}

/// Any heap type: `(exact $t)`, an abstract head, or a type index.
///
/// 🔒 **The one text reader of a heap type** — cast targets, `br_on_cast`, and `ref.null` (which
/// had its own copy, the "fifth entry point" its comment warned about). Learning `(exact …)` once is
/// the point: a heap type read by two readers is one that one of them will get wrong.
fn parse_heap(type_names: &[Option<String>], s: &Sexpr) -> Result<Heap> {
    if let Some(ti) = parse_exact(s, type_names)? {
        return Ok(Heap::Exact(ti));
    }
    let a = want_atom(s)?;
    if let Some(modern) = obsolete_type_keyword(a) {
        return Err(Error::ObsoleteKeyword(modern));
    }
    if let Some(code) = abstract_heap_code(a) {
        return Ok(Heap::Code(code));
    }
    Ok(Heap::Code(i64::from(resolve_by_name(type_names, s)?)))
}

/// `(ref ht)` or `(ref null ht)` — exactly those two shapes. Anything else is malformed.
///
/// ⚠️ The shape was read as "`null` if the second item is `null`, then the LAST item" — so
/// `(ref exact 0)` (a missing pair of parentheses; malformed in `exact.wast`) quietly read as `(ref 0)`,
/// dropping the word that made it exact. Another clause parsed and not carried.
fn ref_form_nullable(l: &[Sexpr]) -> Result<bool> {
    match l.len() {
        2 => Ok(false),
        3 if eq_atom(&l[1], "null") => Ok(true),
        _ => Err(Error::UnexpectedToken),
    }
}

/// A block signature: either a type index or an inline result list.
#[derive(Debug, Clone)]
enum BlockTy {
    Empty,
    Val(V),
    TypeIndex(u32),
}

/// Parse a block type from `items[*j..]`, advancing `j` past it.
/// Parse a **type use** — `(type x)?` then `(param …)*` then `(result …)*` — advancing `*j` past it,
/// and enforcing the three rules the text format fixes (§6.4.4).
///
/// **One function for every site that reads one**, because the rules are identical and two copies of a
/// grammar drift. They already had: block types and `call_indirect` each had their own loop, and
/// `call_indirect` shipped all three of these defects independently of the block-type path.
///
/// The rules, each bought with measured assertions:
///
/// 1. **Clause order.** Collected in any order before, so `(block (result i32) (param i32))` assembled
///    and the **validator** reported the result as a stack-height mismatch — the wrong stage, 36
///    assertions across `block`/`if`/`loop`.
/// 2. **No named parameter.** Only a *function's* parameters bind identifiers, because only a function
///    has local slots for them to name; a block's or an indirect call's operands are stack values.
/// 3. **An inline signature must MATCH a `(type x)` given alongside it** — the two are not
///    alternatives. This used to return on the type index and silently discard the explicit clauses,
///    so the module meant something the text did not say. The suite calls it "inline function type".
///
/// ⚠️ Enforced **here**, not in [`parse_sig`]: this loop hands `parse_sig` one clause at a time, so
/// `parse_sig`'s own order state resets per call and can never observe a sequence. A first attempt put
/// the rule there and moved exactly one assertion out of forty.
fn parse_type_use(ctx: &mut Ctx, items: &[Sexpr], j: &mut usize) -> Result<(Option<u32>, Sig)> {
    // A block type / `call_indirect` signature cannot name its parameters.
    read_type_use(ctx.types, ctx.type_names, items, j, false, ctx.deferred_type_uses)
}

/// A **type use** (§6.4.4) — `(type x)? (param …)* (result …)*` — read from `items[*j..]`, with
/// its three rules: that clause ORDER, a `(type x)` given with explicit clauses must MATCH them
/// (and name a function type), and — where `named_params` is false — no `$name` on a parameter.
///
/// 🔒 **The one authority for those rules.** Block types, `call_indirect`, and function / tag
/// import signatures all read here. The rules were once copied per site, and every copy that was
/// missed kept all three defects: the IMPORT and TAG copy (`parse_tag_type`, until 2026-09-19)
/// enforced none, and where `(type x)` was present it DISCARDED the explicit clauses, so
/// `(import "a" "b" (func (type 0) (param i64)))` built an import of type 0's signature — a
/// module other than the one the text describes, which wasm-tools refuses. (Function
/// *definitions* keep their own loop because locals interleave with it; they enforce the same
/// three rules there.)
fn read_type_use(
    types: &[TypeDef],
    type_names: &[Option<String>],
    items: &[Sexpr],
    j: &mut usize,
    named_params: bool,
    deferred: &mut Vec<(u32, Sig)>,
) -> Result<(Option<u32>, Sig)> {
    let mut sig = Sig::default();
    let mut type_ref = None;
    let (mut seen_param, mut seen_result) = (false, false);
    while let Some(s) = items.get(*j) {
        match s.keyword() {
            Some("type") => {
                if type_ref.is_some() || seen_param || seen_result {
                    return Err(Error::UnexpectedToken);
                }
                type_ref = Some(resolve_by_name(type_names, nth(want_list(s)?, 1)?)?);
                *j += 1;
            }
            Some("param" | "result") => {
                let is_param = s.keyword() == Some("param");
                if is_param && seen_result {
                    return Err(Error::UnexpectedToken);
                }
                if is_param && !named_params && want_list(s)?.get(1).is_some_and(is_id) {
                    return Err(Error::UnexpectedToken);
                }
                if is_param {
                    seen_param = true;
                } else {
                    seen_result = true;
                }
                let one = parse_sig(core::slice::from_ref(s), type_names, None)?;
                sig.params.extend(one.params);
                sig.results.extend(one.results);
                *j += 1;
            }
            _ => break,
        }
    }
    if let Some(ti) = type_ref {
        if seen_param || seen_result {
            match types.get(ti as usize) {
                Some(TypeDef::Func(d)) => {
                    if d.params != sig.params || d.results != sig.results {
                        return Err(Error::TypeUseMismatch);
                    }
                }
                // Not defined yet — possibly an implicit type. Decided once all types exist.
                None => deferred.push((ti, sig.clone())),
                // Naming a non-function type here is malformed, not merely unmatched.
                Some(_) => return Err(Error::UnexpectedToken),
            }
        }
    }
    Ok((type_ref, sig))
}

fn parse_block_type(ctx: &mut Ctx, items: &[Sexpr], j: &mut usize) -> Result<BlockTy> {
    let (type_ref, sig) = parse_type_use(ctx, items, j)?;
    if let Some(ti) = type_ref {
        return Ok(BlockTy::TypeIndex(ti));
    }
    // The shorthand forms: no params and at most one result.
    if sig.params.is_empty() {
        match sig.results.len() {
            0 => return Ok(BlockTy::Empty),
            1 => return Ok(BlockTy::Val(sig.results[0])),
            _ => {}
        }
    }
    // Anything richer (params, or multiple results) needs a real type index. Interning is
    // safe because bodies are encoded before the type section is written.
    Ok(BlockTy::TypeIndex(intern_sig_outside_rec(ctx.types, ctx.rec_groups, sig)))
}

fn emit_block_type(ctx: &mut Ctx, bt: BlockTy) -> Result<()> {
    match bt {
        BlockTy::Empty => ctx.out.push(0x40),
        BlockTy::Val(v) => emit_val_type(&mut ctx.out, v)?,
        BlockTy::TypeIndex(ti) => sleb(&mut ctx.out, i64::from(ti)),
    }
    Ok(())
}

/// Emit one element segment (§5.5.12). The eight flag forms split into two families, and
/// mixing them produces bytes no decoder can read:
///
/// | flag | mode | selector | entries |
/// | --- | --- | --- | --- |
/// | 0 | active, table 0 | offset | func indices |
/// | 1 | passive | **elemkind byte** | func indices |
/// | 2 | active, explicit table | tableidx, offset, **elemkind byte** | func indices |
/// | 3 | declarative | **elemkind byte** | func indices |
/// | 4 | active, table 0 | offset | const-exprs |
/// | 5 | passive | **reftype** | const-exprs |
/// | 6 | active, explicit table | tableidx, offset, **reftype** | const-exprs |
/// | 7 | declarative | **reftype** | const-exprs |
///
/// The index family carries a one-byte *elemkind* (`0x00` = funcref); the expression family
/// carries a full *reftype*. Emitting flag 2 with a reftype and const-exprs — the bug the
/// first conformance run caught — made every `table_copy`/`table_init` module undecodable.
fn emit_elem_segment(c: &mut Vec<u8>, e: &ElemDef, b: &mut ModuleBuild) -> Result<()> {
    // §5.5.12: of the eight forms, only 2/6 (active with an explicit table index) and
    // 5/7 (passive/declarative) carry a type selector. Forms **0 and 4 hardcode
    // `funcref`** — form 4 has no reftype field at all.
    //
    // So an active segment on table 0 whose element type is anything else — `externref`,
    // or the non-nullable `(ref func)` a table initializer now makes expressible — CANNOT
    // use form 4. Emitting it anyway silently rewrote the segment's type to `funcref`:
    // a wrong module, not a rejected one, and the same class as the dropped table
    // initializer this change was made to fix. Promote to form 6 instead.
    let type_is_implicit_funcref = e.elem_type == V::FUNCREF;
    if !e.use_exprs && !type_is_implicit_funcref {
        // The funcidx shorthand (forms 0–3) always denotes `funcref`; a different type
        // cannot be encoded that way at all.
        return Err(Error::BadModuleField);
    }
    let explicit_table = e.table_index != 0 || (e.use_exprs && !type_is_implicit_funcref);
    let flag: u64 = match (&e.offset, e.declarative, e.use_exprs) {
        (Some(_), _, false) => u64::from(explicit_table) * 2, // 0 or 2
        (Some(_), _, true) => 4 + u64::from(explicit_table) * 2, // 4 or 6
        (None, false, false) => 1,
        (None, true, false) => 3,
        (None, false, true) => 5,
        (None, true, true) => 7,
    };
    uleb(c, flag);
    if let Some(off) = &e.offset {
        if explicit_table {
            uleb(c, u64::from(e.table_index));
        }
        emit_const_expr(c, off, b)?;
        // Forms 0 and 4 (table 0) carry no selector at all.
        if explicit_table {
            if e.use_exprs {
                emit_val_type(c, e.elem_type)?;
            } else {
                c.push(0x00); // elemkind: funcref
            }
        }
    } else if e.use_exprs {
        emit_val_type(c, e.elem_type)?;
    } else {
        c.push(0x00); // elemkind: funcref
    }

    uleb(c, e.items.len() as u64);
    for item in &e.items {
        if e.use_exprs {
            emit_const_expr(c, item, b)?;
        } else {
            // The `ref.func $f` shorthand encodes as just the function index.
            let l = want_list(nth(item, 0)?)?;
            uleb(c, u64::from(resolve_by_name(&b.func_names, nth(l, 1)?)?));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asm(src: &str) -> Result<Vec<u8>> {
        assemble(src.as_bytes())
    }

    /// [`asm`] without the `name` section — for tests comparing a `$named` spelling with a
    /// numbered one, which since 2026-09-19 differ in exactly that section and nowhere else.
    fn asm_unnamed(src: &str) -> Result<Vec<u8>> {
        let m = asm(src)?;
        let mut out = m[..8].to_vec();
        let mut p = 8;
        while p < m.len() {
            let start = p;
            let id = m[p];
            p += 1;
            let (mut len, mut shift) = (0usize, 0);
            loop {
                let b = m[p];
                p += 1;
                len |= usize::from(b & 0x7f) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
            }
            let is_name = id == 0 && m.get(p..p + 5) == Some(&b"\x04name"[..]);
            if !is_name {
                out.extend_from_slice(&m[start..p + len]);
            }
            p += len;
        }
        Ok(out)
    }

    #[test]
    fn assembles_the_module_binary_form() {
        let m = asm(r#"(module binary "\00asm\01\00\00\00")"#).unwrap();
        assert_eq!(m, [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
    }

    /// `table.copy`/`table.init` may omit their table index (it defaults to 0), and in flat
    /// form the omission must not make the assembler eat the next instruction's atoms.
    #[test]
    fn assembles_the_table_index_shorthands() {
        let bare = asm_unnamed(
            r#"(module (table 3 funcref) (elem $e funcref (ref.null func))
                 (func (table.copy (i32.const 0) (i32.const 1) (i32.const 1))
                       (table.init $e (i32.const 0) (i32.const 0) (i32.const 1))))"#,
        )
        .unwrap();
        let explicit = asm_unnamed(
            r#"(module (table $t 3 funcref) (elem $e funcref (ref.null func))
                 (func (table.copy $t $t (i32.const 0) (i32.const 1) (i32.const 1))
                       (table.init $t $e (i32.const 0) (i32.const 0) (i32.const 1))))"#,
        )
        .unwrap();
        assert_eq!(bare, explicit);

        // Flat form: `table.copy` takes no immediates here, so `drop` must survive it.
        asm(r#"(module (table 3 funcref) (func (result i32)
                 i32.const 0 i32.const 1 i32.const 1 table.copy i32.const 7))"#)
        .unwrap();
    }

    // The four T9a findings that `br_table.wast` turned out to need are pinned above — each one
    // alone still left the file unbuildable, so each is pinned separately. Every case round-trips
    // through decode+validate: "assemble returned Ok" is not evidence, as the missing label vector
    // showed.

    // --- type-use well-formedness, §6.4.4 (2026-08-08) ---

    /// A **type use** has a fixed clause order: `(type x)?` then `(param …)*` then `(result …)*`.
    /// Any other order is malformed **text** — and the assembler used to accept all of them, so the
    /// *validator* reported them as stack-height mismatches: the wrong stage, and 36 assertions
    /// across `block`/`if`/`loop` on this one rule.
    #[test]
    fn a_type_use_clause_out_of_order_is_malformed() {
        // `(result)` before `(param)`.
        assert_eq!(
            asm("(module (func (i32.const 0) (block (result i32) (param i32))))").unwrap_err(),
            Error::UnexpectedToken
        );
        // `(type)` after `(param)`/`(result)`.
        for src in [
            r#"(module (type $s (func (param i32) (result i32)))
                (func (i32.const 0) (block (param i32) (type $s) (result i32))))"#,
            r#"(module (type $s (func (param i32) (result i32)))
                (func (i32.const 0) (block (result i32) (type $s) (param i32))))"#,
        ] {
            assert_eq!(assemble(src.as_bytes()).unwrap_err(), Error::UnexpectedToken, "{src}");
        }
        // The canonical order still assembles — the rule is an order check, not a ban.
        assert!(
            asm(r#"(module (func (result i32) (i32.const 0)
                     (block (param i32) (result i32))))"#)
                .is_ok()
        );
    }

    /// A block parameter cannot be **named**: only a function's parameters bind identifiers, because
    /// only a function has local slots for them to name.
    #[test]
    fn a_named_block_parameter_is_malformed() {
        assert_eq!(
            asm("(module (func (param i32) (result i32) (block (param $x i32))))").unwrap_err(),
            Error::UnexpectedToken
        );
        // Unnamed is fine.
        assert!(asm("(module (func (i32.const 0) (block (param i32) (drop))))").is_ok());
    }

    /// Giving BOTH a `(type x)` and explicit `(param …)`/`(result …)` is legal only when they
    /// **match** the referenced type. The assembler used to `return` on the type index and silently
    /// discard the explicit clauses, so the module meant something the text did not say — the same
    /// class as the emitter defects, reached from the parser side.
    #[test]
    fn an_inline_block_type_must_match_the_referenced_type() {
        // `$sig` is `(func)`, but the block also claims a result.
        assert_eq!(
            asm(r#"(module (type $sig (func))
                    (func (block (type $sig) (result i32) (i32.const 0)) (unreachable)))"#)
                .unwrap_err(),
            Error::TypeUseMismatch
        );
        // Params disagree in arity.
        assert_eq!(
            asm(r#"(module (type $sig (func (param i32 i32) (result i32)))
                    (func (i32.const 0) (block (type $sig) (param i32) (result i32))
                     (unreachable)))"#)
                .unwrap_err(),
            Error::TypeUseMismatch
        );
        // An exact restatement is accepted — the check must not reject agreement.
        assert!(
            asm(r#"(module (type $sig (func (param i32) (result i32)))
                    (func (result i32) (i32.const 0)
                      (block (type $sig) (param i32) (result i32))))"#)
                .is_ok()
        );
        // And the bare `(type $sig)` form, with nothing to disagree with, still works.
        assert!(
            asm(r#"(module (type $sig (func (param i32) (result i32)))
                    (func (result i32) (i32.const 0) (block (type $sig))))"#)
                .is_ok()
        );
    }

    /// The same rules on the two sites that had NONE until 2026-09-19 — a function IMPORT's
    /// signature and a TAG's (imported or defined). wasm-tools refuses every case below; wasmrt
    /// accepted all of them, and for a mismatch built the import from `(type x)` alone, silently
    /// discarding the clauses beside it.
    #[test]
    fn the_type_use_rules_apply_to_imports_and_tags() {
        for (src, e) in [
            (r#"(module (type (func (param i32))) (import "a" "b" (func (type 0) (param i64))))"#,
             Error::TypeUseMismatch),
            (r#"(module (import "a" "b" (func (type 0) (param i64))) (type (func (param i32))))"#,
             Error::TypeUseMismatch),
            (r#"(module (type (func (param i32))) (import "a" "b" (tag (type 0) (param i64))))"#,
             Error::TypeUseMismatch),
            (r#"(module (type (func (param i32))) (tag (type 0) (param i64)))"#,
             Error::TypeUseMismatch),
            (r#"(module (type (func (param i32))) (import "a" "b" (func (param i32) (type 0))))"#,
             Error::UnexpectedToken),
            (r#"(module (type (func (param i32))) (import "a" "b" (func (type 0) (local i32))))"#,
             Error::UnexpectedToken),
        ] {
            assert_eq!(asm(src).unwrap_err(), e, "{src}");
        }
        // `(type x)` may name an IMPLICIT type — one appended for an inline signature further
        // down — so the match is decided once every type exists. Checked immediately, the first
        // two were REFUSED (index 0 did not exist yet) and the last two refused for the wrong
        // cause; `bindgen_fixtures/fnany_50.wat` is the corpus instance.
        for src in [
            r#"(module (import "a" "b" (func (type 0) (param i32))) (func (param i32)))"#,
            r#"(module (func (type 0) (param i32)) (func (param i32)))"#,
        ] {
            asm(src).unwrap_or_else(|e| panic!("{src} must assemble: {e:?}"));
        }
        for src in [
            r#"(module (import "a" "b" (func (type 0) (param i64))) (func (param i32)))"#,
            r#"(module (func (type 0) (param i64)) (func (param i32)))"#,
        ] {
            assert_eq!(asm(src).unwrap_err(), Error::TypeUseMismatch, "{src}");
        }
        for src in [
            r#"(module (type (func (param i32))) (import "a" "b" (func (type 0) (param i32))))"#,
            r#"(module (import "a" "b" (func $f (param $x i32) (result i32))))"#,
            r#"(module (type $t (func (param i32))) (tag $e (export "x") (type $t)))"#,
            r#"(module (tag $e (import "a" "b") (param i32)))"#,
        ] {
            asm(src).unwrap_or_else(|e| panic!("{src} must assemble: {e:?}"));
        }
    }


    /// The same type-use rules on a **function definition** and on **`call_indirect`** — each had its
    /// own copy of the loop, and therefore its own copy of all three defects. `parse_type_use` is now
    /// the single authority for block types and `call_indirect`; the function path enforces the same
    /// order inline, because its loop also owns `import`/`export`/`local` and the body.
    #[test]
    fn the_type_use_rules_apply_to_functions_and_call_indirect() {
        // A function's own signature: clause order.
        assert_eq!(
            asm("(module (func (result i32) (param i32) (i32.const 0)))").unwrap_err(),
            Error::UnexpectedToken
        );
        assert_eq!(
            asm(r#"(module (type $s (func (param i32) (result i32)))
                    (func (param i32) (type $s) (result i32) (i32.const 0)))"#)
                .unwrap_err(),
            Error::UnexpectedToken
        );
        // A function's inline signature must match a `(type …)` given beside it.
        assert_eq!(
            asm(r#"(module (type $s (func)) (func (type $s) (result i32) (i32.const 0)))"#)
                .unwrap_err(),
            Error::TypeUseMismatch
        );
        // `call_indirect`'s type use: order, named parameter, and inline mismatch.
        assert_eq!(
            asm(r#"(module (type $s (func (param i32) (result i32))) (table 0 funcref)
                    (func (result i32)
                      (call_indirect (type $s) (result i32) (param i32)
                        (i32.const 0) (i32.const 0))))"#)
                .unwrap_err(),
            Error::UnexpectedToken
        );
        assert_eq!(
            asm(r#"(module (table 0 funcref)
                    (func (call_indirect (param $x i32) (i32.const 0) (i32.const 0))))"#)
                .unwrap_err(),
            Error::UnexpectedToken
        );
        assert_eq!(
            asm(r#"(module (type $s (func)) (table 0 funcref)
                    (func (result i32) (call_indirect (type $s) (result i32) (i32.const 0))))"#)
                .unwrap_err(),
            Error::TypeUseMismatch
        );
        // …and the canonical spellings all still assemble.
        assert!(asm("(module (func (param i32) (result i32) (local.get 0)))").is_ok());
        assert!(
            asm(r#"(module (type $s (func (param i32) (result i32))) (table 0 funcref)
                    (func (result i32)
                      (call_indirect (type $s) (i32.const 0) (i32.const 0))))"#)
                .is_ok()
        );
        // ⚠️ Not asserted here, deliberately: that a flat-form `select (result i32)` still assembles.
        // Its immediate sits at the same level a misplaced declaration would, which is why the
        // over-strict version of this rule broke `select.wast`/`stack.wast` — but the flat `select`
        // spelling is itself an assembler gap, so a unit test would be asserting the wrong thing.
        // **The suite is what pins that regression**, and the per-file diff is where it showed up.
    }

    /// T9b. §5.5.13 requires the data-count section only when `memory.init`/`data.drop`
    /// appear. Emitting it always cost 3 bytes per module with data segments — and the
    /// section must still be there when it IS required, or the module stops decoding.
    #[test]
    fn the_data_count_section_is_emitted_only_when_required() {
        let plain = asm(r#"(module (memory 1) (data (i32.const 0) "hi"))"#).unwrap();
        let bulk = asm(
            r#"(module (memory 1) (data $d "hi")
                 (func (memory.init $d (i32.const 0) (i32.const 0) (i32.const 2))))"#,
        )
        .unwrap();
        // Walk the section list rather than scanning for the byte `0x0c`, which occurs all
        // over a module's payload and would make this pass by accident.
        fn section_ids(m: &[u8]) -> Vec<u8> {
            let mut r = crate::reader::Reader::new(&m[8..]); // past the magic + version
            let mut ids = Vec::new();
            while let Ok(id) = r.read_byte() {
                let len = r.read_var_u32().expect("section length");
                ids.push(id);
                r.read_bytes(len as usize).expect("section payload");
            }
            ids
        }
        assert!(
            !section_ids(&plain).contains(&12),
            "no bulk op: no data-count section"
        );
        assert!(
            section_ids(&bulk).contains(&12),
            "memory.init: the section is required"
        );
        // Both must still decode — the section is what tells the decoder the segment count
        // before it reads the code, so dropping it when it IS needed breaks the module.
        crate::module::decode(&plain).expect("decode plain");
        let md = crate::module::decode(&bulk).expect("decode bulk");
        crate::validate::validate(&md).expect("validate bulk");
    }

    #[test]
    fn assembles_a_concrete_ref_null() {
        // `ref.null $t` — a concrete heap type is legal and encodes as a positive s33.
        let bytes = asm(
            r#"(module (type $t (func))
                 (func (result (ref null $t)) (ref.null $t)))"#,
        )
        .unwrap();
        let md = crate::module::decode(&bytes).expect("decode");
        crate::validate::validate(&md).expect("validate");
    }

    #[test]
    fn ref_null_nofunc_is_not_ref_null_func() {
        // The old hand-rolled table mapped `nofunc`→`func` and `noexn`→`exn`, which is a
        // wrong VALUE, not a rejection: `(ref null func)` is not a subtype of
        // `(ref null nofunc)`, so a valid module became invalid and an invalid one could
        // pass. The two must not assemble to the same bytes.
        let nofunc = asm(r#"(module (func (result nullfuncref) (ref.null nofunc)))"#).unwrap();
        let func = asm(r#"(module (func (result funcref) (ref.null func)))"#).unwrap();
        assert_ne!(nofunc, func);
        crate::validate::validate(&crate::module::decode(&nofunc).expect("decode"))
            .expect("validate");
    }

    #[test]
    fn a_table_of_concrete_ref_type_takes_an_inline_elem() {
        // `(table $x (ref null $t) (elem $f))`. The funcidx shorthand (elem forms 0–3)
        // denotes `funcref` and nothing else, so a non-`funcref` element type must use the
        // expression family — emitting the shorthand anyway was refused outright.
        let bytes = asm(
            r#"(module (type $t (func)) (func $f)
                 (table $x (ref null $t) (elem $f)))"#,
        )
        .unwrap();
        let md = crate::module::decode(&bytes).expect("decode");
        crate::validate::validate(&md).expect("validate");
        // The plain `funcref` spelling keeps the compact encoding it always had.
        let plain = asm(r#"(module (func $f) (table $x funcref (elem $f)))"#).unwrap();
        crate::validate::validate(&crate::module::decode(&plain).expect("decode"))
            .expect("validate");
    }

    #[test]
    fn a_block_result_of_concrete_ref_type_round_trips() {
        // The block type is spelled `0x63 <typeidx>`, which `read_block_type` could not
        // read — it consumed the `0x63` as an s33 and then read the type index as an
        // OPCODE, rejecting a valid module as an unsupported instruction.
        let bytes = asm(
            r#"(module (type $t (func))
                 (func (result (ref null $t))
                   (block (result (ref null $t)) (ref.null $t))))"#,
        )
        .unwrap();
        let md = crate::module::decode(&bytes).expect("decode");
        crate::validate::validate(&md).expect("validate");
    }

    /// A folded `br_table` used to emit its opcode with **no label vector**: the flat and
    /// folded emitters had diverged, and only the flat one knew about the variable-length
    /// immediate. The assembler reported success for bytes no decoder could read, which is
    /// why this asserts a decode+validate round-trip rather than "assemble returned Ok".
    #[test]
    fn a_folded_br_table_emits_its_label_vector() {
        let bytes = asm(r#"(module (func (block $a (block $b
                             (br_table $a $b $a (i32.const 1))))))"#)
        .unwrap();
        let md = crate::module::decode(&bytes).expect("folded br_table must decode");
        crate::validate::validate(&md).expect("and validate");

        // Same module written flat must produce byte-identical output.
        let flat = asm(r#"(module (func (block $a (block $b
                            i32.const 1 br_table $a $b $a))))"#)
        .unwrap();
        assert_eq!(bytes, flat);

        // And an unknown label must be refused in the folded spelling too.
        assert_eq!(
            asm(r#"(module (func (block $l (br_table $nope))))"#),
            Err(Error::UnknownLabel)
        );
    }

    /// Only `inf`/`nan` may denote a non-finite value; a numeric literal that overflows is
    /// out of range. Returning infinity instead is a wrong value, not a rejection.
    #[test]
    fn a_float_literal_that_overflows_is_rejected() {
        for lit in ["1e40", "0x1p128", "0x1p10000", "infinity"] {
            let src = alloc::format!("(module (func (f32.const {lit}) drop))");
            assert!(asm(&src).is_err(), "{lit} overflows f32 and must be refused");
        }
        for lit in ["inf", "-inf", "nan", "nan:0x1", "3.4e38", "0x1p127"] {
            let src = alloc::format!("(module (func (f32.const {lit}) drop))");
            assert!(asm(&src).is_ok(), "{lit} is a legal f32 literal");
        }
        // f64 has room for what overflows f32.
        assert!(asm("(module (func (f64.const 1e40) drop))").is_ok());
        assert!(asm("(module (func (f64.const 1e400) drop))").is_err());
    }

    /// The wasm float grammar is stricter than Rust's `FromStr`: the integer part is
    /// mandatory, and an exponent marker needs at least one digit.
    #[test]
    fn float_literal_syntax_follows_the_wasm_grammar_not_rusts() {
        for bad in [".0", ".0e0", "0e", "0e+", "0.0e", "0.0e-", "0x.5", "0x1p", "1x", "0xg"] {
            let src = alloc::format!("(module (func (f32.const {bad}) drop))");
            assert!(asm(&src).is_err(), "{bad} is malformed");
        }
        // An empty fraction is legal; so is a hex exponent spelled in decimal.
        for ok in ["1.", "0x1.", "1e5", "1E5", "0x1p3", "0x1.5p-3", "0123456789"] {
            let src = alloc::format!("(module (func (f32.const {ok}) drop))");
            assert!(asm(&src).is_ok(), "{ok} is well formed");
        }
    }

    /// A NaN payload must fit the mantissa and be non-zero. Masking an over-wide payload
    /// produced a *different* NaN — a wrong value rather than a rejection.
    #[test]
    fn a_nan_payload_is_range_checked_not_masked() {
        assert!(asm("(module (func (f32.const nan:0x800000) drop))").is_err());
        assert!(asm("(module (func (f32.const nan:0x0) drop))").is_err());
        assert!(asm("(module (func (f32.const nan:0x7fffff) drop))").is_ok());
    }

    #[test]
    fn rejects_source_with_no_module() {
        assert_eq!(asm("(func)"), Err(Error::NotAModule));
    }

    #[test]
    fn parses_numeric_literals() {
        assert_eq!(parse_u64_str("0x1_0000").unwrap(), 0x10000);
        assert_eq!(parse_u64_str("1000").unwrap(), 1000);
        assert_eq!(parse_i64_str("-1").unwrap(), -1);
        assert_eq!(parse_i64_str("0xffff_ffff").unwrap(), 0xffff_ffff);
        assert_eq!(parse_i64_str("-9223372036854775808").unwrap(), i64::MIN);
        assert!(parse_u64_str("12x").is_err());
        assert!(parse_u64_str("").is_err());
    }

    #[test]
    fn leb_writers_round_trip_through_the_reader() {
        for v in [0i64, 1, -1, 63, 64, -64, -65, 8191, -8192, i64::MIN, i64::MAX] {
            let mut out = Vec::new();
            sleb(&mut out, v);
            let mut r = crate::reader::Reader::new(&out);
            assert_eq!(r.read_var_i64().unwrap(), v, "sleb round-trip for {v}");
        }
        for v in [0u64, 1, 127, 128, 16383, 16384, u64::from(u32::MAX)] {
            let mut out = Vec::new();
            uleb(&mut out, v);
            let mut r = crate::reader::Reader::new(&out);
            assert_eq!(r.read_var_u64().unwrap(), v, "uleb round-trip for {v}");
        }
    }

    /// ⚠️⚠️ **Every type position, enumerated — because dropping a keyword from the type table is
    /// only half a refusal.** The token then falls out of the *routing* predicate too and lands
    /// wherever the parser looks next; here that was the funcidx path, so
    /// `(elem (i32.const 0) anyfunc …)` and `(ref.null anyfunc)` came back as **`BadNumber`** —
    /// `anyfunc` read as an INDEX. The sibling runtime hit the same slip and recorded it: its
    /// version produced `UnknownIdentifier`, which its `.wast` runner banks as *its own*
    /// limitation, so the deviation returned disguised as a SKIP and the baseline went green for
    /// the wrong reason.
    ///
    /// 🎓 **A rejection has to be routed to the code that knows WHY, or it lands in the wrong
    /// bucket — and the bucket is what the score reads.** Four positions were fixed first and this
    /// table found the fifth (`ref.null`, which reads a heap type through its own reader). One
    /// entry per position, so a future path cannot be right in four places and wrong in one.
    #[test]
    fn an_obsolete_type_keyword_is_refused_in_every_type_position() {
        for src in [
            r#"(module (table 4 anyfunc))"#,
            r#"(module (table $t (export "e") 4 anyfunc))"#,
            r#"(module (import "m" "t" (table 1 anyfunc)))"#,
            r#"(module (global $g anyfunc (ref.null func)))"#,
            r#"(module (func (param anyfunc)))"#,
            r#"(module (func (result anyfunc) (ref.null func)))"#,
            r#"(module (func (local anyfunc)))"#,
            r#"(module (type (func (param anyfunc))))"#,
            r#"(module (func (param (ref anyfunc))))"#,
            r#"(module (func (block (result anyfunc) (ref.null func) (drop))))"#,
            r#"(module (func $f) (table 1 funcref) (elem (i32.const 0) anyfunc (ref.func $f)))"#,
            r#"(module (func (result funcref) (ref.null anyfunc)))"#,
            r#"(module (func (param funcref) (result i32) (ref.test anyfunc (local.get 0))))"#,
        ] {
            assert_eq!(
                asm(src),
                Err(Error::ObsoleteKeyword("funcref")),
                "wrong bucket for: {src}"
            );
        }
        // …and the modern spellings still assemble, so the guard is not simply refusing the shape.
        for src in [
            r#"(module (table 4 funcref))"#,
            r#"(module (func (result funcref) (ref.null func)))"#,
            r#"(module (type $t (func)) (func (param (ref null $t))))"#,
        ] {
            assert!(asm(src).is_ok(), "must still assemble: {src}");
        }
    }

    /// A table's element type is a REFERENCE type, in the text format too (§6.4.2).
    ///
    /// ⚠️⚠️ The element position was parsed with `parse_val_type`, so `(module (table 1 i64))`
    /// ASSEMBLED — to `04 04 01 7e 00 01`, bytes `wasm-tools` refuses as "malformed reference
    /// type" and its own parser refuses as source. That is the eighth instance of the T10a
    /// emitter mechanism and the third time wasmrt's assembler has emitted something that is not
    /// WebAssembly; the other half of it, the decoder ACCEPTING those bytes, is pinned in
    /// `tests/decoder-strictness.wast`.
    #[test]
    fn a_table_element_type_must_be_a_reference_type() {
        for src in [
            r#"(module (table 1 i64))"#,
            r#"(module (table 1 v128))"#,
            r#"(module (import "m" "t" (table 1 f32)))"#,
            r#"(module (table 1 i32 (elem)))"#,
            r#"(module (func $f) (table 1 funcref) (elem (i32.const 0) i64 (ref.func $f)))"#,
        ] {
            assert_eq!(asm(src), Err(Error::BadValType), "must be refused: {src}");
        }
        for src in [
            r#"(module (table 1 funcref))"#,
            r#"(module (table 1 externref))"#,
            r#"(module (type $t (func)) (table 1 (ref null $t)))"#,
            r#"(module (func $f) (table 1 funcref) (elem (i32.const 0) funcref (ref.func $f)))"#,
        ] {
            assert!(asm(src).is_ok(), "must still assemble: {src}");
        }
    }

    #[test]
    fn parses_value_types() {
        let none: Vec<Option<String>> = Vec::new();
        let p = |s: &str| {
            let forms = sexpr::parse_all(s.as_bytes()).unwrap();
            parse_val_type(&forms[0], &none)
        };
        assert_eq!(p("i32").unwrap(), V::I32);
        assert_eq!(p("f64").unwrap(), V::F64);
        assert_eq!(p("v128").unwrap(), V::V128);
        assert_eq!(p("funcref").unwrap(), V::FUNCREF);
        // `anyfunc` is the PRE-STANDARD spelling and is malformed — the direction is pinned
        // here, because it was accepted for the whole port and the reason given was that real
        // `.wat` still emits it. wasmtime 47 refuses it too.
        assert_eq!(p("anyfunc"), Err(Error::ObsoleteKeyword("funcref")));
        assert_eq!(p("externref").unwrap(), V::EXTERNREF);
        assert_eq!(p("exnref").unwrap(), V::EXNREF);
        assert_eq!(p("(ref null any)").unwrap(), V::ANYREF);
        assert_eq!(p("(ref any)").unwrap(), V::ANYREF_NN);
        assert!(p("nope").is_err());
    }

    #[test]
    fn emits_limits_flags() {
        let mut out = Vec::new();
        emit_limits(&mut out, 1, None, false, false);
        assert_eq!(out, [0x00, 0x01]);
        out.clear();
        emit_limits(&mut out, 1, Some(2), false, false);
        assert_eq!(out, [0x01, 0x01, 0x02]);
        out.clear();
        emit_limits(&mut out, 1, Some(1), true, false);
        assert_eq!(out, [0x03, 0x01, 0x01]); // shared requires a max
        out.clear();
        emit_limits(&mut out, 1, None, false, true);
        assert_eq!(out, [0x04, 0x01]); // memory64
    }

    #[test]
    fn rejects_an_import_after_a_definition() {
        let src = r#"(module (func) (import "m" "f" (func)))"#;
        assert_eq!(asm(src), Err(Error::ImportAfterDefinition));
    }

    // --- the closed loop: assemble -> decode -> validate -> run ---
    //
    // These are the assembler's real gate. Byte-level assertions would only prove the
    // assembler agrees with itself; running what it produced proves it agrees with the
    // decoder, the type-checker, and the interpreter.

    /// Assemble, then type-check — a hard failure if the bytes don't validate.
    fn asm_valid(src: &str) -> Vec<u8> {
        let bytes = asm(src).expect("assembly failed");
        let md = crate::module::decode(&bytes).expect("decode failed");
        crate::validate::validate(&md).expect("validation failed");
        bytes
    }

    /// Assemble, validate, instantiate, and call an export.
    fn run(src: &str, func: &str, args: &[crate::interp::Value]) -> Vec<crate::interp::Value> {
        let bytes = asm_valid(src);
        let md = crate::module::decode(&bytes).unwrap();
        let mut inst = crate::interp::Instance::new(md).expect("instantiation failed");
        inst.invoke(func, args).expect("invoke failed")
    }

    #[test]
    fn round_trips_a_flat_add() {
        let src = r#"(module
            (func (export "add") (param $a i32) (param $b i32) (result i32)
              local.get $a
              local.get $b
              i32.add))"#;
        let r = run(
            src,
            "add",
            &[crate::interp::i32_value(40), crate::interp::i32_value(2)],
        );
        assert_eq!(crate::interp::as_i32(r[0]), 42);
    }

    #[test]
    fn round_trips_the_folded_form() {
        // The same function written folded — must produce a module that runs identically.
        let src = r#"(module
            (func (export "add") (param $a i32) (param $b i32) (result i32)
              (i32.add (local.get $a) (local.get $b))))"#;
        let r = run(
            src,
            "add",
            &[crate::interp::i32_value(7), crate::interp::i32_value(5)],
        );
        assert_eq!(crate::interp::as_i32(r[0]), 12);
    }

    #[test]
    fn resolves_named_locals_and_numeric_indices_alike() {
        let named = r#"(module (func (export "f") (param $x i32) (result i32)
              (i32.mul (local.get $x) (i32.const 3))))"#;
        let numbered = r#"(module (func (export "f") (param i32) (result i32)
              (i32.mul (local.get 0) (i32.const 3))))"#;
        assert_eq!(asm_unnamed(named).unwrap(), asm_unnamed(numbered).unwrap());
        assert_ne!(asm(named).unwrap(), asm(numbered).unwrap(), "only `named` has a name section");
    }

    #[test]
    fn runs_a_recursive_function() {
        let src = r#"(module
            (func $fac (export "fac") (param $n i32) (result i32)
              (if (result i32) (i32.lt_s (local.get $n) (i32.const 1))
                (then (i32.const 1))
                (else (i32.mul (local.get $n)
                               (call $fac (i32.sub (local.get $n) (i32.const 1))))))))"#;
        let r = run(src, "fac", &[crate::interp::i32_value(10)]);
        assert_eq!(crate::interp::as_i32(r[0]), 3_628_800);
    }

    #[test]
    fn runs_a_loop_with_named_labels() {
        let src = r#"(module
            (func (export "sum") (param $n i32) (result i32) (local $acc i32)
              (block $done
                (loop $again
                  (br_if $done (i32.eqz (local.get $n)))
                  (local.set $acc (i32.add (local.get $acc) (local.get $n)))
                  (local.set $n (i32.sub (local.get $n) (i32.const 1)))
                  (br $again)))
              (local.get $acc)))"#;
        let r = run(src, "sum", &[crate::interp::i32_value(100)]);
        assert_eq!(crate::interp::as_i32(r[0]), 5050);
    }

    #[test]
    fn assembles_memory_with_data_and_loads_it_back() {
        let src = r#"(module
            (memory 1)
            (data (i32.const 8) "\2a\00\00\00")
            (func (export "get") (result i32)
              (i32.load (i32.const 8))))"#;
        let r = run(src, "get", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 42);
    }

    #[test]
    fn assembles_globals() {
        let src = r#"(module
            (global $g (mut i32) (i32.const 5))
            (func (export "bump") (result i32)
              (global.set $g (i32.add (global.get $g) (i32.const 1)))
              (global.get $g)))"#;
        let r = run(src, "bump", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 6);
    }

    #[test]
    fn assembles_call_indirect_through_a_table() {
        let src = r#"(module
            (type $bin (func (param i32 i32) (result i32)))
            (table 2 funcref)
            (elem (i32.const 0) $add $sub)
            (func $add (param i32 i32) (result i32) (i32.add (local.get 0) (local.get 1)))
            (func $sub (param i32 i32) (result i32) (i32.sub (local.get 0) (local.get 1)))
            (func (export "pick") (param $which i32) (result i32)
              (call_indirect (type $bin) (i32.const 10) (i32.const 4) (local.get $which))))"#;
        // Slot 0 = add, slot 1 = sub.
        let r = run(src, "pick", &[crate::interp::i32_value(0)]);
        assert_eq!(crate::interp::as_i32(r[0]), 14);
        let r = run(src, "pick", &[crate::interp::i32_value(1)]);
        assert_eq!(crate::interp::as_i32(r[0]), 6);
    }

    #[test]
    fn honours_an_explicit_memarg() {
        // `offset=` shifts the effective address; `align=` is a hint the validator bounds.
        let src = r#"(module
            (memory 1)
            (data (i32.const 4) "\07\00\00\00")
            (func (export "get") (result i32)
              (i32.load offset=4 align=4 (i32.const 0))))"#;
        let r = run(src, "get", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 7);
    }

    #[test]
    fn a_forward_referencing_export_resolves() {
        // The export names a function declared later — the order binaryen emits.
        let src = r#"(module
            (export "late" (func $f))
            (func $f (result i32) (i32.const 99)))"#;
        let r = run(src, "late", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 99);
    }

    #[test]
    fn rejects_an_unknown_identifier() {
        let src = r#"(module (func (result i32) (global.get $nope)))"#;
        assert_eq!(asm(src), Err(Error::UnknownIdentifier));
    }

    #[test]
    fn rejects_an_unknown_instruction() {
        let src = r#"(module (func (i32.frobnicate)))"#;
        assert_eq!(asm(src), Err(Error::UnknownInstr));
    }

    // --- float literals ---

    #[test]
    fn hex_float_literals_are_rounded_not_truncated() {
        // The exact case from the spec suite's `simd_f64x2_rounding.wast`. A parser that
        // truncates a long hex mantissa yields ...cde — one ULP low, a WRONG value rather
        // than a rejected one, so the same number in decimal and in hex would compile to
        // different modules.
        assert_eq!(
            parse_f64_bits("0x0123456789ABCDEFabcdef", FloatCtx::Script).unwrap(),
            0x44f2_3456_789a_bcdf
        );
        assert_eq!(
            parse_f64_bits("0x0123456789ABCDEFa", FloatCtx::Script).unwrap(),
            0x43b2_3456_789a_bcdf
        );
        assert_eq!(
            parse_f64_bits("0x1.23456789abcdep+81", FloatCtx::Script).unwrap(),
            0x4502_3456_789a_bcde
        );
    }

    #[test]
    fn parses_ordinary_float_literals() {
        assert_eq!(parse_f64_bits("1.5", FloatCtx::Script).unwrap(), 1.5f64.to_bits());
        assert_eq!(parse_f64_bits("-0.0", FloatCtx::Script).unwrap(), (-0.0f64).to_bits());
        assert_eq!(parse_f64_bits("0", FloatCtx::Script).unwrap(), 0.0f64.to_bits());
        assert_eq!(parse_f32_bits("1.5", FloatCtx::Script).unwrap(), 1.5f32.to_bits());
        assert_eq!(parse_f32_bits("-2.5e3", FloatCtx::Script).unwrap(), (-2500.0f32).to_bits());
        assert_eq!(parse_f64_bits("inf", FloatCtx::Script).unwrap(), f64::INFINITY.to_bits());
        assert_eq!(
            parse_f64_bits("-inf", FloatCtx::Script).unwrap(),
            f64::NEG_INFINITY.to_bits()
        );
        // The exponent-less hex form the text format also allows.
        assert_eq!(parse_f64_bits("0x10", FloatCtx::Script).unwrap(), 16.0f64.to_bits());
        assert_eq!(parse_f64_bits("0x1.8p+1", FloatCtx::Script).unwrap(), 3.0f64.to_bits());
        assert!(parse_f64_bits("0x", FloatCtx::Script).is_none());
        assert!(parse_f64_bits("1.2.3", FloatCtx::Script).is_none());
    }

    #[test]
    fn parses_the_wasm_nan_spellings() {
        let canonical = parse_f64_bits("nan:canonical", FloatCtx::Script).unwrap();
        assert!(f64::from_bits(canonical).is_nan());
        assert_eq!(canonical, f64::NAN.to_bits() & !(1u64 << 63));
        let arith = parse_f64_bits("nan:arithmetic", FloatCtx::Script).unwrap();
        assert!(f64::from_bits(arith).is_nan());
        // An explicit payload lands in the mantissa.
        let payload = parse_f64_bits("nan:0x4000000000000", FloatCtx::Script).unwrap();
        assert_eq!(payload & 0xf_ffff_ffff_ffff, 0x4000000000000);
        assert!(f64::from_bits(payload).is_nan());
        // The sign is honoured.
        assert_ne!(parse_f64_bits("-nan:canonical", FloatCtx::Script).unwrap() >> 63, 0);
        assert!(f32::from_bits(parse_f32_bits("nan", FloatCtx::Script).unwrap()).is_nan());
    }

    #[test]
    fn subnormal_hex_floats_round_rather_than_flush() {
        // 0.75 ULP — just ABOVE half the smallest f64 subnormal — must round up to it
        // rather than flush to zero. This is the case a two-stage rounding (clamp the
        // kept-bit count, then scale) gets wrong by discarding the sticky bit.
        assert_eq!(parse_f64_bits("0x1.8p-1075", FloatCtx::Script).unwrap(), 1);
        // Exactly half ties to even → zero.
        assert_eq!(parse_f64_bits("0x1p-1075", FloatCtx::Script).unwrap(), 0);
        // The smallest subnormal itself.
        assert_eq!(parse_f64_bits("0x1p-1074", FloatCtx::Script).unwrap(), 1);
        // Exactly halfway BETWEEN subnormals 1 and 2 ties to even → 2.
        assert_eq!(parse_f64_bits("0x1.8p-1074", FloatCtx::Script).unwrap(), 2);
        // Rounding up out of the subnormal range lands on the smallest normal.
        assert_eq!(
            parse_f64_bits("0x1.fffffffffffffp-1023", FloatCtx::Script).unwrap(),
            f64::MIN_POSITIVE.to_bits()
        );
    }

    #[test]
    fn runs_float_arithmetic_from_text() {
        let src = r#"(module
            (func (export "add") (result f64)
              (f64.add (f64.const 1.5) (f64.const 2.25))))"#;
        let r = run(src, "add", &[]);
        assert!((crate::interp::as_f64(r[0]) - 3.75).abs() < 1e-12);
    }

    #[test]
    fn multi_value_block_type_interns_a_new_type() {
        // A block returning two values needs a real type index. No `(type $t)` is written,
        // so the assembler must intern one — which is only possible because bodies are
        // encoded before the type section is emitted.
        let src = r#"(module
            (func (export "f") (result i32)
              (block (result i32 i32)
                (i32.const 20)
                (i32.const 22))
              i32.add))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 42);
    }

    #[test]
    fn block_type_with_params_interns_a_new_type() {
        // A block that consumes operands likewise needs a type index.
        let src = r#"(module
            (func (export "f") (result i32)
              (i32.const 30)
              (block (param i32) (result i32)
                (i32.const 12)
                i32.add)))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 42);
    }

    #[test]
    fn call_indirect_interns_an_inline_signature() {
        // No `(type $t)` on the call_indirect — the inline signature must intern.
        let src = r#"(module
            (table 1 funcref)
            (elem (i32.const 0) $double)
            (func $double (param i32) (result i32)
              (i32.mul (local.get 0) (i32.const 2)))
            (func (export "go") (result i32)
              (call_indirect (param i32) (result i32) (i32.const 21) (i32.const 0))))"#;
        let r = run(src, "go", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 42);
    }

    // --- SIMD / atomics text forms ---

    #[test]
    fn runs_simd_from_text() {
        let src = r#"(module
            (func (export "f") (result i32)
              (i32x4.extract_lane 0
                (i32x4.add (i32x4.splat (i32.const 20)) (i32x4.splat (i32.const 22))))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 42);
    }

    #[test]
    fn runs_a_v128_const() {
        let src = r#"(module
            (func (export "f") (result i32)
              (i32x4.extract_lane 2 (v128.const i32x4 1 2 3 4))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 3);
    }

    #[test]
    fn runs_a_v128_const_of_floats() {
        let src = r#"(module
            (func (export "f") (result f32)
              (f32x4.extract_lane 1 (v128.const f32x4 1.5 2.5 3.5 4.5))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_f32(r[0]), 2.5);
    }

    #[test]
    fn runs_a_simd_shuffle() {
        // Take lane 0 of the second operand (indices 16..31 select from it).
        let src = r#"(module
            (func (export "f") (result i32)
              (i8x16.extract_lane_u 0
                (i8x16.shuffle 16 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15
                  (i8x16.splat (i32.const 1))
                  (i8x16.splat (i32.const 9))))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 9);
    }

    #[test]
    fn runs_simd_load_and_store() {
        let src = r#"(module
            (memory 1)
            (func (export "f") (result i32)
              (v128.store (i32.const 0) (i32x4.splat (i32.const 7)))
              (i32x4.extract_lane 3 (v128.load (i32.const 0)))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 7);
    }

    #[test]
    fn runs_a_simd_load_lane() {
        // `v128.load8_lane` takes a memarg AND a trailing lane index.
        let src = r#"(module
            (memory 1)
            (data (i32.const 0) "\2a")
            (func (export "f") (result i32)
              (i8x16.extract_lane_u 5
                (v128.load8_lane 0 5 (i32.const 0) (i8x16.splat (i32.const 0))))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 42);
    }

    #[test]
    fn runs_atomics_from_text() {
        let src = r#"(module
            (memory 1)
            (func (export "f") (result i32)
              (i32.store (i32.const 0) (i32.const 10))
              (drop (i32.atomic.rmw.add (i32.const 0) (i32.const 5)))
              (i32.atomic.load (i32.const 0))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 15);
    }

    #[test]
    fn atomic_names_cover_the_generated_families() {
        // The rmw/cmpxchg names are generated from the layout, so spot-check the corners
        // of every group against the sub-opcodes the decoder reads.
        assert_eq!(lookup_atomic("i32.atomic.rmw.add"), Some(0x1e));
        assert_eq!(lookup_atomic("i64.atomic.rmw.add"), Some(0x1f));
        assert_eq!(lookup_atomic("i32.atomic.rmw8.add_u"), Some(0x20));
        assert_eq!(lookup_atomic("i64.atomic.rmw32.add_u"), Some(0x24));
        assert_eq!(lookup_atomic("i32.atomic.rmw.sub"), Some(0x25));
        assert_eq!(lookup_atomic("i32.atomic.rmw.and"), Some(0x2c));
        assert_eq!(lookup_atomic("i32.atomic.rmw.or"), Some(0x33));
        assert_eq!(lookup_atomic("i32.atomic.rmw.xor"), Some(0x3a));
        assert_eq!(lookup_atomic("i32.atomic.rmw.xchg"), Some(0x41));
        assert_eq!(lookup_atomic("i32.atomic.rmw.cmpxchg"), Some(0x48));
        assert_eq!(lookup_atomic("i64.atomic.rmw32.cmpxchg_u"), Some(0x4e));
        assert_eq!(lookup_atomic("atomic.fence"), Some(0x03));
        // Forms that do not exist must not be invented.
        assert_eq!(lookup_atomic("i32.atomic.rmw32.add_u"), None);
        assert_eq!(lookup_atomic("i32.atomic.rmw.add_u"), None);
        assert_eq!(lookup_atomic("i32.atomic.rmw8.add"), None);
        assert_eq!(lookup_atomic("i32.atomic.rmw.frob"), None);
    }

    #[test]
    fn simd_names_resolve_to_their_sub_opcodes() {
        assert_eq!(lookup_simd("v128.load").map(|(s, _)| s), Some(0x00));
        assert_eq!(lookup_simd("i32x4.add").map(|(s, _)| s), Some(0xae));
        assert_eq!(lookup_simd("f64x2.pmax").map(|(s, _)| s), Some(0xf7));
        assert_eq!(
            lookup_simd("i32x4.relaxed_dot_i8x16_i7x16_add_s").map(|(s, _)| s),
            Some(0x113)
        );
        assert_eq!(lookup_simd("i8x16.nope"), None);
    }

    // --- exception handling text forms ---

    #[test]
    fn runs_try_table_from_text() {
        // The catch clause branches OUT of the try_table to the enclosing block, carrying
        // the tag's payload. ⚠️ A catch label counts from OUTSIDE the try_table, so the
        // enclosing block is label 0 — `$h` resolves there, and the emitted byte is 0.
        let src = r#"(module
            (tag $e (param i32))
            (func (export "f") (result i32)
              (block $h (result i32)
                (try_table (result i32) (catch $e $h)
                  (i32.const 42)
                  (throw $e)))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 42);
    }

    /// ⚠️⚠️ **The bytes, not the answer — the second time.** A non-null abstract reference type is
    /// `0x64` + the head's `absheaptype` byte (§5.3.5). `ValType` stores those types as synthetic
    /// internal tags (`0x57`–`0x68`) in an unused valtype-byte range, and `emit_val_type` pushed
    /// `v.bits()` straight into the binary — so `(ref any)` was emitted as the single byte `0x66`.
    /// Our decoder read it back, so every round trip passed; **wasmtime 47 refuses it** with
    /// `invalid value type`, which means no module wasmrt assembled containing one was
    /// WebAssembly at all.
    ///
    /// 🎓 The nullable shorthands genuinely ARE their valtype bytes, so the shortcut looked total.
    /// *An internal representation that resembles the wire format will eventually be mistaken for
    /// it*, and only a reader that did not write the bytes can tell the two apart.
    #[test]
    fn a_non_null_abstract_reference_is_encoded_as_0x64_plus_its_heap_type() {
        // The head byte a `(ref X)` must be spelled with — identical to the NULLABLE shorthand's,
        // because that shorthand is defined as `(ref null X)`.
        for (text, head) in [
            ("(ref any)", 0x6e),
            ("(ref func)", 0x70),
            ("(ref extern)", 0x6f),
            ("(ref eq)", 0x6d),
            ("(ref i31)", 0x6c),
            ("(ref struct)", 0x6b),
            ("(ref array)", 0x6a),
            ("(ref exn)", 0x69),
            ("(ref none)", 0x71),
            ("(ref nofunc)", 0x73),
            ("(ref noextern)", 0x72),
            ("(ref noexn)", 0x74),
        ] {
            let src = format!("(module (func (export \"f\") (param {text})))");
            let bytes = asm(&src).unwrap_or_else(|e| panic!("{text}: {e:?}"));
            // Type section: … 0x60 (func) 0x01 (one param) then the param's encoding.
            let at = bytes
                .windows(3)
                .position(|w| w == [0x60, 0x01, 0x64])
                .unwrap_or_else(|| panic!("{text} was not emitted as `0x64 <heaptype>`: {bytes:02x?}"));
            assert_eq!(bytes[at + 3], head, "{text} used the wrong heap-type byte");
        }
    }

    /// ⚠️⚠️ **The bytes, not the answer.** A `try_table` catch label counts from the scope
    /// ENCLOSING the try_table (§: `C ⊢ catch ok` is checked before the rule extends `C` with the
    /// block's label). The assembler pushed the try_table's own label first, the validator pushed
    /// its frame first, and the interpreter added `d` instead of `d + 1` — three components that
    /// agreed with each other, so every behavioural test above passed and the spec suite was
    /// almost entirely green while the emitted module was **rejected by wasmtime 47**
    /// (`catch_all label must have no result types`, resolving our `1` to the function).
    ///
    /// 🎓 *Agreement between components that learned the convention from each other is not
    /// evidence.* Only reading the byte — or handing it to an outside reader — can tell a
    /// convention from a bug, which is why this test asserts the encoding rather than the result.
    #[test]
    fn a_catch_label_is_encoded_relative_to_the_enclosing_scope() {
        let bytes = asm_valid(
            r#"(module
                (tag $e)
                (func (export "f")
                  (block $h
                    (try_table (catch_all $h)))))"#,
        );
        // … 1f 40 01 02 <label> 0b …  — try_table, void blocktype, 1 clause, catch_all, label.
        let at = bytes
            .windows(4)
            .position(|w| w == [0x1f, 0x40, 0x01, 0x02])
            .expect("the try_table's catch vector must be in the emitted body");
        assert_eq!(
            bytes[at + 4],
            0x00,
            "the enclosing block is label 0 from a catch clause, not 1"
        );
    }

    #[test]
    fn runs_try_table_catch_all() {
        // `catch_all` binds nothing, so its target label must be void; a local records
        // which path ran.
        let src = r#"(module
            (tag $e (param i32))
            (func (export "f") (result i32) (local $seen i32)
              (local.set $seen (i32.const 7))
              (block $h
                (try_table (catch_all $h)
                  (i32.const 1)
                  (throw $e))
                (local.set $seen (i32.const 99)))
              (local.get $seen)))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 7);
    }

    #[test]
    fn an_uncaught_throw_traps() {
        let src = r#"(module
            (tag $e (param i32))
            (func (export "f") (result i32)
              (i32.const 5)
              (throw $e)))"#;
        let bytes = asm_valid(src);
        let md = crate::module::decode(&bytes).unwrap();
        let mut inst = crate::interp::Instance::new(md).unwrap();
        assert_eq!(
            inst.invoke("f", &[]),
            Err(crate::interp::Trap::UncaughtException)
        );
    }

    #[test]
    fn runs_the_legacy_folded_try() {
        // `(try (do …) (catch $e …))` — the handler runs INSIDE the try and binds the
        // payload, so the try's result comes from the handler.
        let src = r#"(module
            (tag $e (param i32))
            (func (export "f") (result i32)
              (try (result i32)
                (do (i32.const 3) (throw $e))
                (catch $e))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 3);
    }

    #[test]
    fn runs_the_legacy_folded_catch_all() {
        let src = r#"(module
            (tag $e (param i32))
            (func (export "f") (result i32)
              (try (result i32)
                (do (i32.const 1) (throw $e))
                (catch_all (i32.const 55)))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 55);
    }

    /// `delegate` assembles in both forms and its label resolves OUTSIDE the try it closes.
    ///
    /// ⚠️ This asserted `Error::Unsupported` for both — the assembler refused the construct
    /// "matching the oracle". ⚠️ The two spellings must emit the SAME bytes, which is the
    /// property that catches a scope handled in one path and not the other: the try's label has to
    /// come off before the operand is resolved, in each emitter separately.
    #[test]
    fn delegate_assembles_identically_in_both_forms() {
        let folded = r#"(module
            (tag $e (param i32))
            (func (export "f")
              (block
                (try (do (i32.const 4) (throw $e)) (delegate 0)))))"#;
        let flat = r#"(module
            (tag $e (param i32))
            (func (export "f")
              block
                try
                  i32.const 4
                  throw $e
                delegate 0
              end))"#;
        let a = asm_valid(folded);
        assert_eq!(a, asm_valid(flat), "the folded and flat forms must agree");
        // … 0x18 <labelidx>. `delegate 0` names the enclosing BLOCK, so the byte is 0 — if the
        // try's own label were still in scope it would have had to be 1.
        let at = a
            .windows(2)
            .position(|w| w == [0x08, 0x00])
            .expect("throw is in the body");
        assert_eq!(&a[at + 2..at + 4], &[0x18, 0x00]);
    }

    #[test]
    fn runs_catch_ref_and_throw_ref() {
        // `catch_ref` materializes an exnref alongside the payload; `throw_ref` re-raises
        // it for the outer try_table to catch by value.
        let src = r#"(module
            (type $pair (func (result i32 exnref)))
            (tag $e (param i32))
            (func (export "f") (result i32)
              (block $outer (result i32)
                (try_table (catch $e $outer)
                  (block $inner (type $pair)
                    (try_table (catch_ref $e $inner)
                      (i32.const 17)
                      (throw $e))
                    (unreachable))
                  (throw_ref))
                (i32.const 0))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 17);
    }

    // --- WasmGC text forms ---

    #[test]
    fn runs_a_gc_struct_from_text() {
        let src = r#"(module
            (type $point (struct (field $x (mut i32)) (field $y (mut i32))))
            (func (export "f") (result i32) (local $p (ref null $point))
              (local.set $p (struct.new $point (i32.const 40) (i32.const 2)))
              (i32.add (struct.get $point $x (local.get $p))
                       (struct.get $point $y (local.get $p)))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 42);
    }

    #[test]
    fn resolves_struct_fields_by_name_or_number() {
        let named = r#"(module
            (type $t (struct (field $a i32) (field $b i32)))
            (func (export "f") (result i32)
              (struct.get $t $b (struct.new $t (i32.const 1) (i32.const 2)))))"#;
        let numbered = r#"(module
            (type $t (struct (field $a i32) (field $b i32)))
            (func (export "f") (result i32)
              (struct.get $t 1 (struct.new $t (i32.const 1) (i32.const 2)))))"#;
        assert_eq!(asm(named).unwrap(), asm(numbered).unwrap());
        assert_eq!(crate::interp::as_i32(run(named, "f", &[])[0]), 2);
    }

    #[test]
    fn runs_a_gc_array_from_text() {
        let src = r#"(module
            (type $arr (array (mut i32)))
            (func (export "f") (result i32) (local $a (ref null $arr))
              (local.set $a (array.new $arr (i32.const 7) (i32.const 4)))
              (array.set $arr (local.get $a) (i32.const 2) (i32.const 35))
              (i32.add (array.get $arr (local.get $a) (i32.const 2))
                       (array.len (local.get $a)))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 39); // 35 + length 4
    }

    #[test]
    fn runs_a_packed_gc_array() {
        // A packed `i8` element must be read with the sign-aware accessor.
        let src = r#"(module
            (type $bytes (array (mut i8)))
            (func (export "f") (result i32) (local $a (ref null $bytes))
              (local.set $a (array.new $bytes (i32.const 0) (i32.const 4)))
              (array.set $bytes (local.get $a) (i32.const 0) (i32.const 200))
              (array.get_u $bytes (local.get $a) (i32.const 0))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 200);
    }

    #[test]
    fn runs_array_new_fixed() {
        let src = r#"(module
            (type $arr (array (mut i32)))
            (func (export "f") (result i32)
              (array.get $arr
                (array.new_fixed $arr 3 (i32.const 10) (i32.const 20) (i32.const 30))
                (i32.const 1))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 20);
    }

    /// The GC array **bulk** ops, end to end: assemble → decode → validate → run.
    ///
    /// All six were absent from `Op` entirely until 2026-08-19, and the gap produced only
    /// SKIPS — never a failure — so no conformance number could show it (`testing.md`).
    #[test]
    fn runs_array_fill_copy_and_new_data() {
        let src = r#"(module
            (type $arr (array (mut i32)))
            (data $d "\07\00\00\00\09\00\00\00")
            (func (export "fill") (result i32)
              (local $a (ref null $arr))
              (local.set $a (array.new_default $arr (i32.const 4)))
              (array.fill $arr (local.get $a) (i32.const 1) (i32.const 5) (i32.const 2))
              (i32.add (array.get $arr (local.get $a) (i32.const 1))
                       (array.get $arr (local.get $a) (i32.const 2))))
            (func (export "copy") (result i32)
              (local $a (ref null $arr)) (local $b (ref null $arr))
              (local.set $a (array.new_fixed $arr 2 (i32.const 3) (i32.const 4)))
              (local.set $b (array.new_default $arr (i32.const 2)))
              (array.copy $arr $arr (local.get $b) (i32.const 0)
                                    (local.get $a) (i32.const 0) (i32.const 2))
              (i32.add (array.get $arr (local.get $b) (i32.const 0))
                       (array.get $arr (local.get $b) (i32.const 1))))
            (func (export "newdata") (result i32)
              (array.get $arr (array.new_data $arr $d (i32.const 0) (i32.const 2))
                              (i32.const 1))))"#;
        assert_eq!(crate::interp::as_i32(run(src, "fill", &[])[0]), 10);
        assert_eq!(crate::interp::as_i32(run(src, "copy", &[])[0]), 7);
        assert_eq!(crate::interp::as_i32(run(src, "newdata", &[])[0]), 9);
    }

    /// 🔒 **The WRONG ANSWER, written down.** `array.copy` between an `i8` array and an `i16`
    /// one must be REFUSED.
    ///
    /// ⚠️ This is the defect this pass introduced and `array_copy.wast` caught on the first run:
    /// the arm compared `storage.unpacked()`, which maps **both** `i8` and `i16` onto `i32`, so
    /// the check passed and an invalid module was accepted. *An encoding chosen to make
    /// EXECUTION agree can erase the distinction VALIDATION runs on* — the projection is right
    /// for the operand stack and wrong for a type-identity question.
    #[test]
    fn array_copy_between_differently_packed_arrays_is_refused() {
        let mk = |dst: &str, src: &str| {
            format!(
                r#"(module
                (type $d (array (mut {dst})))
                (type $s (array (mut {src})))
                (func (param $a (ref null $d)) (param $b (ref null $s))
                  (array.copy $d $s (local.get $a) (i32.const 0)
                                    (local.get $b) (i32.const 0) (i32.const 1))))"#
            )
        };
        // The mismatch must be refused in BOTH directions...
        for (d, s) in [("i8", "i16"), ("i16", "i8"), ("i8", "i32"), ("i32", "i8")] {
            let m = crate::module::decode(&asm(&mk(d, s)).expect("assembles"))
                .expect("decodes");
            assert!(
                crate::validate::validate(&m).is_err(),
                "array.copy {d} <- {s} must be refused"
            );
        }
        // ...and the matching cases must still be accepted, or the rule is just a blanket ban.
        for (d, s) in [("i8", "i8"), ("i16", "i16"), ("i32", "i32")] {
            let m = crate::module::decode(&asm(&mk(d, s)).expect("assembles"))
                .expect("decodes");
            assert!(
                crate::validate::validate(&m).is_ok(),
                "array.copy {d} <- {s} must be accepted"
            );
        }
    }

    /// table64: a 64-bit table's index type reaches **every** operand position, including the
    /// ones outside instruction bodies.
    ///
    /// ⚠️ The active element segment's offset is the one that bit: it is a module-level check,
    /// not an instruction, so the instruction-typing work missed it entirely and
    /// `(elem (table $t) (i64.const 1) …)` failed as a `TypeMismatch`. That single line took
    /// **every** assertion in `table_get64.wast` and `table_set64.wast` with it — a module that
    /// fails to build costs its whole file.
    #[test]
    fn a_64bit_table_types_its_operands_and_its_elem_offset_as_i64() {
        let src = r#"(module
            (table $t i64 4 funcref)
            (elem (table $t) (i64.const 1) func $f)
            (func $f (result i32) (i32.const 7))
            (func (export "size") (result i64) (table.size $t))
            (func (export "call") (result i32)
              (call_indirect $t (type $s) (i64.const 1))))
            "#;
        // `(type $s)` needs declaring; assemble the whole thing and check it validates.
        let src = src.replace("(type $s)", "(result i32)");
        let m = crate::module::decode(&asm(&src).expect("assembles")).expect("decodes");
        assert!(crate::validate::validate(&m).is_ok(), "a 64-bit table must validate");
        // `table.size` on a 64-bit table yields i64, so the export's result type proves the
        // index type reached the instruction as well as the segment.
        assert_eq!(crate::interp::as_i32(run(&src, "call", &[])[0]), 7);
    }

    /// `(memory i64 (data "…"))` — the index-type keyword and the inline-data shorthand
    /// together.
    ///
    /// ⚠️ The inline-data branch used to run **before** the keyword was parsed and hardcoded
    /// `is64: false`, so the `i64` sat where that branch expected `data`, neither path matched,
    /// and the whole of `float_memory64.wast` failed to assemble with `BadForm` — 6 failures
    /// and 84 skips on one line. **A fact parsed in one branch and hardcoded in another is the
    /// emitter defect wearing a parser's clothes.**
    #[test]
    fn a_64bit_memory_accepts_the_inline_data_shorthand() {
        let src = r#"(module
            (memory i64 (data "\07\00\00\00"))
            (func (export "f") (result i32) (i32.load (i64.const 0))))"#;
        assert_eq!(crate::interp::as_i32(run(src, "f", &[])[0]), 7);
        // …and the 32-bit spelling still works, so the hoist did not just invert the bug.
        let src32 = r#"(module
            (memory (data "\09\00\00\00"))
            (func (export "f") (result i32) (i32.load (i32.const 0))))"#;
        assert_eq!(crate::interp::as_i32(run(src32, "f", &[])[0]), 9);
    }

    /// §6.5.2 label repetition — `else`/`end` may name the block they close.
    ///
    /// ⚠️ Unconsumed until 2026-08-19, so the id was read as the next instruction and reported
    /// `UnknownInstr` **naming a label** — a message that pointed at nothing, which is why
    /// `id.wast` and `stack.wast` sat unexplained. The repeat is **checked**: the negative case
    /// below is the whole reason the form exists.
    #[test]
    fn else_and_end_may_repeat_the_block_label_and_a_mismatch_is_refused() {
        assert!(asm(r#"(module (func (block $l (br $l) end $l)))"#).is_ok());
        assert!(asm(r#"(module (func (if $c (i32.const 1) (then) else $c end $c)))"#).is_ok());
        // A repeat naming a different block is malformed — not silently ignored.
        assert_eq!(
            asm(r#"(module (func (block $l (br $l) end $nope)))"#),
            Err(Error::UnknownLabel)
        );
        // …and the bare forms still work, so this did not just start demanding the id.
        assert!(asm(r#"(module (func (block $l (br $l) end)))"#).is_ok());
    }

    /// `select` with an explicit result type (reference-types), in **both** spellings.
    ///
    /// ⚠️ Neither assembled until 2026-08-19 — `select` had no bespoke handling, so its
    /// `(result …)` was read as the next instruction and came back `UnknownInstr` naming
    /// `result`. That was `select.wast`'s 124 skips. 🎓 It is also why the "no declaration after
    /// the body begins" rule was withdrawn at T9: in FLAT form this immediate sits exactly where
    /// a misplaced declaration would, so keyword scanning cannot tell them apart.
    #[test]
    fn select_takes_an_explicit_result_type_in_both_forms() {
        // Folded. `externref` cannot use bare `select` — the typed form is the only way — so a
        // pass here proves the type reached the encoding rather than being dropped.
        let folded = r#"(module (func (export "f")
            (param externref externref i32) (result externref)
            (select (result externref) (local.get 0) (local.get 1) (local.get 2))))"#;
        let b = asm(folded).expect("folded typed select assembles");
        assert!(b.contains(&0x1c), "must emit the TYPED select opcode 0x1c, not bare 0x1b");
        assert!(crate::validate::validate(&crate::module::decode(&b).unwrap()).is_ok());

        // Flat.
        let flat = r#"(module (func (export "f") (result i32)
            i32.const 1 i32.const 2 i32.const 0 select (result i32)))"#;
        // ⚠️ Condition 0 selects the SECOND operand — `select(a, b, c)` is `c != 0 ? a : b`. My first
        // expectation here said 1, and the implementation was right: a test can be wrong in the
        // direction of accusing working code, which is the reading that wastes the most time.
        assert_eq!(crate::interp::as_i32(run(flat, "f", &[])[0]), 2);
        // …and with a non-zero condition it takes the first, so the test pins the CHOICE and not
        // just “something came back”.
        let flat_true = flat.replace("i32.const 0 select", "i32.const 1 select");
        assert_eq!(crate::interp::as_i32(run(&flat_true, "f", &[])[0]), 1);

        // Bare `select` still emits 0x1b, so the typed path did not capture the untyped one.
        let bare = asm(r#"(module (func (result i32)
            i32.const 1 i32.const 2 i32.const 0 select))"#)
        .expect("bare select assembles");
        assert!(bare.contains(&0x1b) && !bare.contains(&0x1c));
    }

    /// `br_on_non_null` takes the **nullable** form of its label's last type — which is the
    /// whole point of the instruction.
    ///
    /// ⚠️ The validator popped the label types wholesale until 2026-08-19, demanding
    /// `(ref $t)` from a stack holding `(ref null $t)`, and so **rejected the canonical idiom**:
    /// take a nullable reference, branch with it made non-null, fall through when it was null.
    /// 🎓 The stack *effect* was already correct, so the arm looked right and only the demanded
    /// TYPE was wrong — **a wrong type with a right arity passes every shape check**.
    #[test]
    fn br_on_non_null_accepts_a_nullable_operand_for_a_non_null_label() {
        let ok = |src: &str| {
            let m = crate::module::decode(&asm(src).expect("assembles")).expect("decodes");
            crate::validate::validate(&m)
        };
        // The idiom the instruction exists for: nullable in, non-null at the label.
        assert!(ok(r#"(module (type $t (func (result i32)))
            (func (param $r (ref null $t)) (result i32)
              (call_ref $t (block $l (result (ref $t))
                (br_on_non_null $l (local.get $r))
                (return (i32.const -1))))))"#)
        .is_ok());
        // An already-non-null operand still works — subtyping, not equality.
        assert!(ok(r#"(module (type $t (func (result i32)))
            (func (param $r (ref $t)) (result i32)
              (call_ref $t (block $l (result (ref $t))
                (br_on_non_null $l (local.get $r))
                (return (i32.const -1))))))"#)
        .is_ok());
        // With extra label operands — `[t'* (ref null ht)] -> [t'*]`.
        assert!(ok(r#"(module (type $t (func (param i32) (result i32)))
            (func (param $n i32) (param $r (ref null $t)) (result i32)
              (call_ref $t (block $l (result i32 (ref $t))
                (return (br_on_non_null $l (local.get $n) (local.get $r)))))))"#)
        .is_ok());
        // ⚠️ And the rule still BITES: a label whose last type is not a reference is invalid,
        // so this did not simply stop checking.
        assert!(ok(r#"(module
            (func (param $r i32) (block $l (result i32)
              (br_on_non_null $l (local.get $r)) (unreachable))))"#)
        .is_err());
    }

    /// The funcidx **shorthand** element forms have type `(ref func)` — NON-NULL — because every
    /// element is `(ref.func y)`, which cannot be null.
    ///
    /// ⚠️ The decoder defaulted them to the nullable `funcref`, which under-approximates the type
    /// and so **refused valid modules**: such a segment could not initialise a table declared
    /// `(ref func)`. `elem.wast` ships that module five times. 🎓 *A nullable default is the
    /// safe-looking choice and the wrong one here* — safety in a type default is directional, and
    /// this direction rejects rather than admits.
    #[test]
    fn a_funcidx_shorthand_elem_segment_is_non_null() {
        let ok = |src: &str| {
            let m = crate::module::decode(&asm(src).expect("assembles")).expect("decodes");
            crate::validate::validate(&m)
        };
        // A non-nullable table initialised by the shorthand — valid, and was refused.
        assert!(ok(r#"(module
            (func)
            (table 1 (ref func) (ref.func 0))
            (elem (i32.const 0) func 0))"#)
        .is_ok());
        // The nullable table still accepts it, so this did not just swap one refusal for another.
        assert!(ok(r#"(module
            (func)
            (table 1 funcref)
            (elem (i32.const 0) func 0))"#)
        .is_ok());
    }

    /// An inline `(table … (elem …))` picks its ENCODING from whether every entry is expressible
    /// as a function index — not from the element type.
    ///
    /// ⚠️ It keyed on the type alone, so a `funcref` table whose entries include `(ref.null func)`
    /// chose the funcidx shorthand and died with `BadNumber` trying to read `func` as an index.
    /// **The element type was the wrong question**: `ref.null` is never an index, whatever the
    /// table holds.
    #[test]
    fn an_inline_elem_uses_expressions_when_an_entry_is_not_a_function_index() {
        // Mixed entries — must use the expression encoding.
        assert!(asm(r#"(module (func $f) (func $g)
            (table $t funcref (elem (ref.func $f) (ref.null func) (ref.func $g))))"#)
        .is_ok());
        // All-index entries still take the shorthand, which is the smaller encoding.
        let short = asm(r#"(module (func $f) (table $t funcref (elem $f)))"#).expect("assembles");
        let exprs = asm(r#"(module (func $f)
            (table $t funcref (elem (ref.null func))))"#)
        .expect("assembles");
        assert!(
            short.len() < exprs.len(),
            "the funcidx shorthand must still be chosen when it can be — it is smaller"
        );
    }

    /// `(module $name binary "…")` — a **named** binary module.
    ///
    /// ⚠️ The `binary` keyword was looked for at `module[1]`, which is where the optional `$name`
    /// sits, so the named form fell through to the field parser and reported `BadModuleField`
    /// about the word `binary`. The name is now skipped first, exactly as the text form does.
    #[test]
    fn a_binary_module_may_carry_a_name() {
        let bare = asm(r#"(module binary "\00asm\01\00\00\00")"#).expect("bare binary module");
        let named = asm(r#"(module $M binary "\00asm\01\00\00\00")"#).expect("named binary module");
        assert_eq!(bare, named, "the name is not part of the module bytes");
        // The multi-string spelling too, which is how the suite usually writes it.
        assert_eq!(
            asm(r#"(module $M binary "\00asm" "\01\00\00\00")"#).expect("split strings"),
            bare
        );
    }

    /// NaN propagation through the rounding ops must produce an **arithmetic** NaN — quiet bit
    /// set — so a SIGNALLING input is quieted, not passed through.
    ///
    /// ⚠️ `nearest` did this and `trunc`/`floor`/`ceil` did not, and since the latter two
    /// delegate to `trunc`, **three of the four** returned the input unchanged. 28 assertions
    /// across `f32.wast`, `f64.wast` and both `simd_*_rounding.wast`. 🎓 *The rule was written
    /// once and applied at one of four sites* — and the one site that had it is why the code
    /// read as if the rule were handled.
    #[test]
    fn the_rounding_ops_quiet_a_signalling_nan() {
        // 0x7fa00000 is a NaN with payload 0x200000: exponent all-ones, mantissa non-zero, and
        // the quiet bit (0x400000) CLEAR — signalling.
        for op in ["ceil", "floor", "trunc", "nearest"] {
            let src = format!(
                r#"(module (func (export "f") (result i32)
                     (i32.reinterpret_f32 (f32.{op} (f32.reinterpret_i32 (i32.const 0x7fa00000))))))"#
            );
            let got = crate::interp::as_i32(run(&src, "f", &[])[0]) as u32;
            assert_eq!(
                got & 0x0040_0000,
                0x0040_0000,
                "f32.{op} must quiet the NaN; got {got:#010x}"
            );
            // …and the payload and sign are preserved, so "quiet" did not become "canonicalise".
            assert_eq!(got, 0x7fe0_0000, "f32.{op} must keep sign and payload");
        }
    }

    /// A function whose signature comes from `(type $sig)` alone still has its **parameters at
    /// local indices 0..n**, so a named local declared afterwards starts at `n`.
    ///
    /// ⚠️⚠️ **SILENT WRONG OUTPUT until 2026-08-19.** The parameter placeholders were inserted
    /// only `if local_names.is_empty()`, and a `(local $var …)` clause has already pushed its
    /// name by then — so `$var` resolved to **index 0, the parameter**, and the function
    /// returned its ARGUMENT instead of a fresh zero local. No error, no diagnostic, just a
    /// different number. It sat inside `func.wast`'s "8 failures" line.
    #[test]
    fn a_named_local_starts_after_the_params_of_a_referenced_type() {
        let src = r#"(module
            (type $sig (func (param i32) (result i32)))
            (func (export "f") (type $sig)
              (local $var i32)
              (local.get $var))
            (func (export "g") (type $sig)
              (local $a i32) (local $b i32)
              (local.set $b (i32.const 7))
              (local.get $b)))"#;
        // The argument is 42; `$var` is a FRESH local, so the answer is 0 — not 42.
        assert_eq!(
            crate::interp::as_i32(run(src, "f", &[crate::interp::Value::from(42u32)])[0]),
            0,
            "a named local must not alias the parameter"
        );
        // Two named locals: the second must be index 2, after the param and `$a`.
        assert_eq!(
            crate::interp::as_i32(run(src, "g", &[crate::interp::Value::from(42u32)])[0]),
            7
        );
    }

    /// A SIMD lane index is an **unsigned** literal (§6.3.1), and `uN` admits no sign — so
    /// `+1` is malformed even though its value is in range.
    ///
    /// ⚠️ `-1` was already refused, but only because `u8::try_from(-1 as u32)` overflows: a
    /// RANGE check doing a SIGN check's job by accident. `+1` is in range and sailed through,
    /// so seven `assert_malformed`s in `simd_lane.wast` accepted a malformed module.
    /// 🎓 **A check that happens to cover a rule for some inputs makes the rule look
    /// implemented** — the negative cases passing is precisely why nobody examined the positive
    /// ones.
    #[test]
    fn a_simd_lane_index_admits_no_sign() {
        let m = |lane: &str| {
            format!(
                r#"(module (func (result i32) (i8x16.extract_lane_s {lane}
                     (v128.const i8x16 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0))))"#
            )
        };
        // Signed spellings are malformed whatever the value.
        for lane in ["+1", "+0x0f", "+03", "-1", "-0"] {
            assert_eq!(asm(&m(lane)), Err(Error::BadImmediate), "lane `{lane}` must be refused");
        }
        // Unsigned spellings in range still work — the rule is about the SIGN, not the digits.
        for lane in ["0", "15", "0x0f"] {
            assert!(asm(&m(lane)).is_ok(), "lane `{lane}` must be accepted");
        }
        // …and the range check still bites on its own.
        assert_eq!(asm(&m("999")), Err(Error::BadImmediate));
    }

    /// §6.3.5: a `$name` must be unique **within its index space** — and the space is filled by
    /// both imports and definitions, so the rule spans them.
    ///
    /// ⚠️ Missing until 2026-08-19, and in the **accepting** direction: 7 `assert_malformed`s
    /// across `memory.wast` and `func.wast` were admitting a malformed module. 🔒 The check runs
    /// once over the finished name vectors rather than at each writer, because *a rule about a
    /// NAMESPACE belongs on the namespace* — no single push site can see an import colliding
    /// with a later definition.
    #[test]
    fn a_name_must_be_unique_within_its_index_space() {
        // All three shapes: def+def, import+def, import+import.
        assert_eq!(
            asm(r#"(module (memory $foo 1) (memory $foo 1))"#),
            Err(Error::DuplicateName)
        );
        assert_eq!(
            asm(r#"(module (import "" "" (memory $foo 1)) (memory $foo 1))"#),
            Err(Error::DuplicateName)
        );
        assert_eq!(
            asm(r#"(module (func $f) (func $f))"#),
            Err(Error::DuplicateName)
        );
        // ⚠️ The same name in DIFFERENT spaces is legal and must stay so — this is the case a
        // careless "no duplicate identifiers anywhere" rule would break.
        assert!(asm(r#"(module (memory $x 1) (table $x 1 funcref) (func $x))"#).is_ok());
        // …as is reusing a name for a local, which lives in its own per-function space.
        assert!(asm(r#"(module (global $g i32 (i32.const 0))
            (func (param $g i32) (drop (local.get $g))))"#)
        .is_ok());
    }

    /// An **implicit** type use may never reuse a type that lives inside an explicit
    /// `(rec …)` group — rec-group membership is part of a type's IDENTITY.
    ///
    /// ⚠️ It did until 2026-08-19, so `(func $f)` beside `(rec (type $ft (func)) (type (func)))`
    /// silently got type `$ft`, and `(global (ref $ft) (ref.func $f))` — an `assert_invalid` —
    /// became VALID. 🎓 **The assembler was changing what the module MEANS**: the text says "$f
    /// has its own implicit type", the encoder said "reuse $ft". `is_subtype` could not catch it
    /// because by the time it ran the two really were one type.
    #[test]
    fn an_implicit_type_use_never_reuses_a_rec_group_member() {
        // `$ft` is inside a rec group, so `$f`'s implicit `(func)` must be a NEW type — leaving
        // the module invalid, which is what the suite asserts.
        let src = r#"(module
            (rec (type $ft (func)) (type (func)))
            (func $f)
            (global (ref $ft) (ref.func $f)))"#;
        let m = crate::module::decode(&asm(src).expect("assembles")).expect("decodes");
        assert!(
            crate::validate::validate(&m).is_err(),
            "$f's implicit type must not be $ft — rec-group membership is identity"
        );
        // ⚠️ The mirror: OUTSIDE a rec group, reuse is still correct and still happens, or every
        // implicit use would append a duplicate type and the encoding would grow.
        let plain = r#"(module (type $ft (func)) (func $f) (global (ref $ft) (ref.func $f)))"#;
        let m = crate::module::decode(&asm(plain).expect("assembles")).expect("decodes");
        assert!(
            crate::validate::validate(&m).is_ok(),
            "a standalone type must still be reused by an implicit use"
        );
    }

    /// `memory.init $mem $data` — **memory first, data second**; `memory.init $data` defaults
    /// the memory to 0. The binary order is the reverse (data then memory), which is exactly
    /// where the confusion came from.
    ///
    /// ⚠️⚠️ Read backwards for the two-operand form until 2026-08-19, so `(memory.init $mem2 0 …)`
    /// wrote into the **wrong memory**: silent wrong output where both indices exist, and a
    /// spurious out-of-bounds trap where the mistaken memory is smaller. 🎓 `table.init` next
    /// door has had the correct two-spelling handling all along — *a rule applied at one of two
    /// sites*.
    #[test]
    fn memory_init_takes_the_memory_first_and_the_data_second() {
        // Two memories, so writing into the wrong one is observable rather than harmless.
        let two = r#"(module
            (memory $a 1) (memory $b 1)
            (data $d "\77")
            (func (export "go") (result i32)
              (memory.init $b $d (i32.const 3) (i32.const 0) (i32.const 1))
              (i32.load8_u $b (i32.const 3))))"#;
        assert_eq!(crate::interp::as_i32(run(two, "go", &[])[0]), 0x77);
        // …and the named memory really is `$b`: `$a` must be untouched.
        let untouched = two.replace("(i32.load8_u $b", "(i32.load8_u $a");
        assert_eq!(crate::interp::as_i32(run(&untouched, "go", &[])[0]), 0);
        // The one-operand spelling still means the DATA index, memory 0.
        let one = r#"(module
            (memory 1)
            (data $d "\55")
            (func (export "go") (result i32)
              (memory.init $d (i32.const 0) (i32.const 0) (i32.const 1))
              (i32.load8_u (i32.const 0))))"#;
        assert_eq!(crate::interp::as_i32(run(one, "go", &[])[0]), 0x55);
        // The sibling that was already right, pinned so it stays right.
        let copy = r#"(module
            (memory $a 1) (memory $b 1)
            (func (export "go") (result i32)
              (i32.store8 $a (i32.const 3) (i32.const 88))
              (memory.copy $b $a (i32.const 5) (i32.const 3) (i32.const 1))
              (i32.load8_u $b (i32.const 5))))"#;
        assert_eq!(crate::interp::as_i32(run(copy, "go", &[])[0]), 88);
    }

    #[test]
    fn runs_i31_and_ref_test() {
        let src = r#"(module
            (func (export "f") (result i32)
              (i32.add (i31.get_s (ref.i31 (i32.const 5)))
                       (ref.test i31ref (ref.i31 (i32.const 1))))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 6); // 5 + test-true(1)
    }

    #[test]
    fn runs_a_gc_subtype_declaration() {
        // `(sub $base …)` emits the 0x50 wrapper, and a cast to the subtype succeeds.
        let src = r#"(module
            (type $base (sub (struct (field $x i32))))
            (type $derived (sub $base (struct (field $x i32) (field $y i32))))
            (func (export "f") (result i32)
              (struct.get $derived $y
                (ref.cast (ref $derived)
                  (struct.new $derived (i32.const 1) (i32.const 41))))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 41);
    }

    // --- regressions the first spec-suite run found ---

    #[test]
    fn v128_const_i64x2_does_not_panic() {
        // The lane-count match accepted `i64x2` but the lane-writing match forgot it and
        // fell into `unreachable!()`, aborting the whole conformance run on the first
        // `v128.const i64x2`. A library must reject a module, never panic the embedder.
        let src = r#"(module (func (export "f") (result i64)
              (i64x2.extract_lane 1 (v128.const i64x2 7 42))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i64(r[0]), 42);
        // An unknown shape is a clean rejection, not a panic.
        assert!(asm(r#"(module (func (v128.const i9x9 1) drop))"#).is_err());
    }

    #[test]
    fn active_elem_with_an_explicit_table_uses_the_index_encoding() {
        // Element segments split into two families: flags 0-3 carry an elemkind BYTE and
        // bare function indices, flags 4-7 a reftype and const-exprs. Emitting flag 2 with
        // a reftype and exprs produced bytes the decoder could not read — every
        // table_copy/table_init module in the suite failed to build.
        let src = r#"(module
            (type $v (func (result i32)))
            (table $t0 4 funcref)
            (table $t1 4 funcref)
            (func $a (type $v) (i32.const 11))
            (func $b (type $v) (i32.const 22))
            (elem (table $t1) (i32.const 1) func $a $b)
            (func (export "f") (result i32)
              (call_indirect $t1 (type $v) (i32.const 2))))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_i32(r[0]), 22); // slot 2 holds $b
    }

    #[test]
    fn digit_separators_must_sit_between_digits() {
        // `1_000` is a number; `_100`, `99_`, `1__000` and `1_.0` are malformed. Blindly
        // filtering `_` accepted all of them.
        assert_eq!(parse_u64_str("1_000").unwrap(), 1000);
        for bad in ["_100", "99_", "1__000", "_1"] {
            assert!(parse_u64_str(bad).is_err(), "should reject `{bad}`");
        }
        assert!(parse_f64_bits("1_000.5", FloatCtx::Script).is_some());
        for bad in ["_100", "1.0_", "1_.0", "1__000", "+_100"] {
            assert!(parse_f64_bits(bad, FloatCtx::Script).is_none(), "should reject `{bad}`");
        }

        // "between two digits", not "between two alphanumerics" — the looser reading admits
        // these, because `x`, `e` and `p` are alphanumeric. Which characters count as digits
        // depends on the radix: `e` is a digit in hex but an exponent marker in decimal.
        for bad in ["1_e1", "1e_1", "0x_1", "0x1_", "0x1_p3"] {
            assert!(parse_f64_bits(bad, FloatCtx::Script).is_none(), "should reject `{bad}`");
        }
        for ok in ["0x1_e2", "0x1p1_0", "0xa_b", "1.0e1_0"] {
            assert!(parse_f64_bits(ok, FloatCtx::Script).is_some(), "should accept `{ok}`");
        }
        assert!(parse_u64_str("0x_1").is_err());
        assert_eq!(parse_u64_str("0xa_b").unwrap(), 0xab);
    }

    #[test]
    fn constants_out_of_range_are_rejected_not_truncated() {
        // The text format allows a constant signed OR unsigned, so the range is
        // -2^(n-1) ..= 2^n - 1; outside that is "constant out of range". Truncating
        // instead would quietly turn `(i32.const 0x100000000)` into 0.
        assert!(asm(r#"(module (func (i32.const 0x100000000) drop))"#).is_err());
        assert!(asm(r#"(module (func (i32.const -0x80000001) drop))"#).is_err());
        assert!(asm(r#"(module (func (i32.const 0xffffffff) drop))"#).is_ok()); // unsigned spelling
        assert!(asm(r#"(module (func (v128.const i8x16 256 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0) drop))"#).is_err());
        assert!(asm(r#"(module (func (v128.const i8x16 -129 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0) drop))"#).is_err());
        assert!(asm(r#"(module (func (v128.const i8x16 255 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0) drop))"#).is_ok());
    }

    #[test]
    fn runs_a_hex_float_constant() {
        let src = r#"(module
            (func (export "f") (result f32) (f32.const 0x1.8p+1)))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_f32(r[0]), 3.0);
    }

    // ---- Table initializer expressions (fixed 2026-08-06) ---------------------------
    //
    // Both defects here were **silent-wrong output**, the category `cmem/INDEX.md` calls
    // the worst: the assembler accepted the source and emitted a module that ran and gave
    // the wrong answer, rather than refusing it.

    #[test]
    fn a_table_initializer_actually_fills_the_table() {
        // Was: assembled, dropped the `(ref.func $f)`, and every entry came back null.
        let src = r#"(module
            (func $f (result i32) (i32.const 7))
            (type $s (func (result i32)))
            (table $t 3 funcref (ref.func $f))
            (func (export "call1") (result i32) (call_indirect $t (type $s) (i32.const 1))))"#;
        assert_eq!(crate::interp::as_i32(run(src, "call1", &[])[0]), 7);
    }

    #[test]
    fn a_table_initializer_uses_the_0x40_binary_form() {
        // The encoding matters, not just the behaviour: `0x40 0x00` is what a *different*
        // decoder needs to see. Assemble, then read the table section back.
        let bytes = asm_valid(
            r#"(module (func $f) (table $t 2 funcref (ref.func $f)))"#,
        );
        let md = crate::module::decode(&bytes).expect("decode");
        assert_eq!(md.tables.len(), 1);
        assert!(
            md.tables[0].init.is_some(),
            "the initializer must survive the round trip"
        );
        // And the raw bytes carry the marker.
        let sec = md
            .section(crate::types::SectionId::Table)
            .expect("table section");
        assert_eq!(
            bytes[sec.offset + 1],
            0x40,
            "the table entry must use the 0x40 initializer form"
        );
    }

    #[test]
    fn a_plain_table_may_not_have_a_non_nullable_element_type() {
        // Without an initializer the entries would start null, which the type forbids —
        // so the plain form simply cannot express it.
        let bytes = asm(r#"(module (func $f) (table $t 2 (ref func) (ref.func $f)))"#)
            .expect("with an initializer it is fine");
        assert!(crate::module::decode(&bytes).is_ok());
        // Hand-build the same table WITHOUT the initializer: element type `(ref func)`
        // (0x64 0x70), limits {min 2}. It must be refused at decode.
        let mut m = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        m.extend_from_slice(&[0x04, 0x05, 0x01, 0x64, 0x70, 0x00, 0x02]);
        assert!(
            crate::module::decode(&m).is_err(),
            "a non-nullable element type needs an initializer"
        );
    }

    #[test]
    fn an_active_segment_on_table_zero_keeps_a_non_funcref_type() {
        // Element-segment form 4 has NO reftype field — it hardcodes `funcref`. Emitting
        // it for a `(ref func)` segment silently rewrote the type. It must promote to
        // form 6 (explicit table index + reftype) instead.
        let bytes = asm_valid(
            r#"(module
                 (func $f)
                 (table $t 2 (ref func) (ref.func $f))
                 (elem (i32.const 0) (ref func) (ref.func $f)))"#,
        );
        let md = crate::module::decode(&bytes).expect("decode");
        assert_eq!(
            md.elements[0].elem_type,
            V::FUNCREF_NN,
            "the segment's non-nullable type must survive; form 4 would flatten it to funcref"
        );
    }

    #[test]
    fn an_externref_active_segment_on_table_zero_also_keeps_its_type() {
        // The same trap without any function-references involvement.
        let bytes = asm_valid(
            r#"(module
                 (table $t 2 externref)
                 (elem (i32.const 0) externref (ref.null extern)))"#,
        );
        let md = crate::module::decode(&bytes).expect("decode");
        assert_eq!(md.elements[0].elem_type, V::EXTERNREF);
    }

    #[test]
    fn a_nullable_segment_cannot_initialize_a_non_nullable_table() {
        // §3.5.9 is subtyping, not a family match: `funcref` is not a subtype of
        // `(ref func)`. This is the spec-suite `assert_invalid` that the old
        // nullability-normalizing check wrongly accepted.
        let bytes = asm(
            r#"(module
                 (func $f)
                 (table $t 1 (ref func) (ref.func $f))
                 (elem (i32.const 0) funcref (ref.func $f)))"#,
        )
        .expect("it assembles");
        let md = crate::module::decode(&bytes).expect("and decodes");
        assert_eq!(
            crate::validate::validate(&md),
            Err(crate::validate::ValidateError::TypeMismatch)
        );
    }

    #[test]
    fn an_imported_table_may_not_carry_an_initializer() {
        // The binary format has no place to put one — the exporter owns the contents.
        assert!(asm(
            r#"(module (func $f) (import "m" "t" (table 1 funcref (ref.func $f))))"#
        )
        .is_err());
    }
}

#[cfg(test)]
mod emitter_coverage_tests {
    use super::*;
    use crate::opcode::{decode_body, Imm, Op};

    /// Ops whose immediates the assembler emits through a **dedicated path** rather than the
    /// generic `immediate_arity` + emitter-arm pair: block types, vectors, and memargs all need
    /// bespoke parsing. Listing them here is the point of the sweep — an op is either generic, or
    /// deliberately special, and a new one is neither until someone says so.
    fn has_dedicated_emitter(op: Op) -> bool {
        use Op as O;
        matches!(
            op,
            O::Block
                | O::Loop
                | O::If
                | O::TryLegacy
                | O::TryTable
                | O::CatchLegacy
                | O::Delegate
                | O::BrTable
                | O::CallIndirect
                | O::ReturnCallIndirect
                | O::SelectT
        ) || takes_memarg(op)
    }

    /// How many immediate bytes the **decoder** reads for `b`, or `None` if it will not decode in
    /// isolation. Padding is zeros, which spell a valid index/labelidx/blocktype for every op that
    /// takes one, followed by enough `end` bytes to close whatever it opened.
    fn decoder_immediate(b: u8) -> Option<Imm> {
        let mut body = vec![b];
        body.extend_from_slice(&[0x00; 8]);
        body.extend_from_slice(&[0x0b; 8]);
        decode_body(&body).ok()?.first().map(|i| i.imm.clone())
    }

    /// ⚠️ **The sweep T10a asked for, and the one that would have caught T9a#8.**
    ///
    /// The assembler's `immediate_arity` fell through to `_ => 0` for any op nobody had listed, and
    /// the emitter's match ended in `_ => {}`. So an op that genuinely takes an immediate was
    /// emitted as a **bare opcode**, with its operand left in the token stream — silently. Three
    /// instructions were in that state: `call_ref`, `return_call_ref` (a type index each) and
    /// `br_on_null`/`br_on_non_null` (a label each).
    ///
    /// Neither failure mode announced itself as "the assembler does not know this instruction":
    /// `call_ref` folded produced a module the *decoder* rejected for a missing `end`, `call_ref`
    /// flat reported `UnknownInstr` about the *next* token, and `br_on_null` did the same. The
    /// cause was one line away in all three cases and looked like three unrelated bugs.
    ///
    /// This asserts the invariant directly: **if the decoder reads an immediate for an op, the
    /// assembler must know to write one.** The decoder is the right oracle because it is the half
    /// that defines the binary format.
    #[test]
    fn the_assembler_writes_an_immediate_for_every_op_that_decodes_one() {
        let mut missing = Vec::new();
        for b in 0u8..=0xff {
            let Some(op) = Op::from_u8(b) else { continue };
            if has_dedicated_emitter(op) {
                continue;
            }
            let Some(imm) = decoder_immediate(b) else { continue };
            if imm != Imm::None && immediate_arity(op) == 0 {
                missing.push((op.text_name(), b));
            }
        }
        assert!(
            missing.is_empty(),
            "these ops decode an immediate but the assembler writes none, so it will emit a bare \
             opcode and leave the operand in the token stream: {missing:?}"
        );
    }

    /// The converse: an op the assembler expects an operand for, but the decoder reads none, would
    /// swallow the following instruction's first token. Same class, opposite direction.
    #[test]
    fn the_assembler_expects_no_immediate_the_decoder_does_not_read() {
        let mut extra = Vec::new();
        for b in 0u8..=0xff {
            let Some(op) = Op::from_u8(b) else { continue };
            if has_dedicated_emitter(op) || has_optional_indices(op) {
                continue;
            }
            let Some(imm) = decoder_immediate(b) else { continue };
            if imm == Imm::None && immediate_arity(op) > 0 {
                extra.push((op.text_name(), b));
            }
        }
        assert!(extra.is_empty(), "assembler expects operands the decoder never reads: {extra:?}");
    }

    /// The three instructions the sweep found, end to end: assemble → decode → validate.
    /// "Assembled without error" is not evidence — `call_ref` did that while emitting a body the
    /// decoder could not read.
    #[test]
    fn call_ref_and_br_on_null_round_trip() {
        let m = assemble(
            br#"(module
                  (type $i2i (func (param i32) (result i32)))
                  (elem declare funcref (ref.func $inc))
                  (func $inc (param i32) (result i32) (i32.add (local.get 0) (i32.const 1)))
                  (func $hof (param $f (ref $i2i)) (result i32)
                    (call_ref $i2i (i32.const 42) (local.get $f)))
                  (func (export "caller") (result i32) (call $hof (ref.func $inc)))
                  (func (export "onnull") (param $r (ref null $i2i)) (result i32)
                    (block $b
                      (drop (br_on_null $b (local.get $r)))
                      (return (i32.const 1)))
                    (i32.const 0)))"#,
        )
        .expect("must assemble");
        let md = crate::module::decode(&m).expect("must decode");
        crate::validate::validate(&md).expect("must validate");
    }

    /// **X2 — THE CLAUSE-COVERAGE SWEEP.** For every clause the module grammar admits, a module
    /// that sets it to a NON-DEFAULT value, assembled and decoded, must still carry it.
    ///
    /// 🎯 This is the sweep T10a specified on 2026-08-08, after three "the assembler emits a
    /// different module than the text describes" defects in two passes — **all three found by
    /// accident**, by some unrelated check reading a field the emitter had dropped. The opcode
    /// half shipped that day; this is the module half, and by the time it landed the mechanism
    /// had produced **seven** instances, the last two also found by accident.
    ///
    /// ⚠️⚠️ **T10a called this a `ModuleBuild` FIELD-coverage sweep, and that is the weaker
    /// half — it would not have caught either of the two most recent instances.**
    /// `(pagesize N)` was parsed and never stored, so there was no field to find uncovered; the
    /// bare `tableidx` of `(elem 0 …)` was not stored either, it was silently re-read as an
    /// element ITEM. Both are *clauses the parser accepts and the module does not carry*, which
    /// is what the mechanism actually produces. **A field can only be covered once it exists**,
    /// so this sweep is over the GRAMMAR rather than the struct: every row is a text clause, and
    /// a row fails however the clause is lost — dropped, stored but not emitted, or re-read as
    /// something else.
    ///
    /// 🔒 **Each check reads the DECODED module, never `ModuleBuild`.** Asserting against the
    /// builder would compare the parser with itself, which is the §3.8b error: agreement between
    /// two components that learned the convention from each other is not evidence. The decoder is
    /// a second reader of the same bytes. Where a clause is visible to no reader at all (a struct
    /// field's *name*, which exists only to resolve `$name` and reaches no section) there is
    /// nothing to assert, and that row is absent deliberately rather than forgotten.
    #[test]
    fn every_module_clause_survives_the_round_trip() {
        type Check = fn(&crate::module::Module) -> bool;
        use crate::module::{CompType, ElementMode, Extern};

        let rows: &[(&str, &[u8], Check)] = &[
            // --- types ---
            (
                "(rec …) group boundaries",
                br#"(module (rec (type $a (struct)) (type $b (struct))))"#,
                |m| m.rec_groups == [(0, 2)],
            ),
            (
                "(sub …) leaves a type OPEN",
                br#"(module (type $a (sub (struct))))"#,
                |m| m.type_finals == [false],
            ),
            (
                "(sub final …) is final",
                br#"(module (type $a (sub final (struct))))"#,
                |m| m.type_finals == [true],
            ),
            (
                "a declared supertype",
                br#"(module (type $a (sub (struct))) (type $b (sub $a (struct))))"#,
                |m| m.supertypes == [None, Some(0)],
            ),
            (
                "a mutable array field",
                br#"(module (type (array (mut i32))))"#,
                |m| matches!(&m.comp_types[0], CompType::Array(f) if f.mutable),
            ),
            (
                "an immutable array field",
                br#"(module (type (array i32)))"#,
                |m| matches!(&m.comp_types[0], CompType::Array(f) if !f.mutable),
            ),
            (
                "a struct's fields and their mutability",
                br#"(module (type (struct (field i32) (field (mut i64)))))"#,
                |m| {
                    matches!(&m.comp_types[0], CompType::Struct(f)
                        if f.len() == 2 && !f[0].mutable && f[1].mutable)
                },
            ),
            (
                "params and results",
                br#"(module (func (param i32) (result i64) (i64.const 0)))"#,
                |m| {
                    matches!(&m.comp_types[0], CompType::Func(t)
                        if t.params.len() == 1 && t.results.len() == 1)
                },
            ),
            // --- functions ---
            (
                "(type $t) on a func reuses THAT index",
                br#"(module (type $u (func)) (type $t (func (param i32)))
                      (func (type $t) (param i32)))"#,
                |m| m.functions == [1],
            ),
            (
                "declared locals",
                br#"(module (func (local i64) (local f32)))"#,
                |m| m.code[0].locals.iter().map(|l| l.count).sum::<u32>() == 2,
            ),
            (
                "(start $f)",
                br#"(module (func $f) (start $f))"#,
                |m| m.start == Some(0),
            ),
            // --- imports: the ORDER is part of the module ---
            (
                "imports keep their declaration order across kinds",
                br#"(module (import "a" "g" (global i32)) (import "a" "f" (func)))"#,
                |m| {
                    matches!(m.imports.as_slice(), [i0, i1]
                        if matches!(i0.ty, Extern::Global(_)) && matches!(i1.ty, Extern::Func(_)))
                },
            ),
            (
                "an imported global's mutability",
                br#"(module (import "a" "b" (global (mut i32))))"#,
                |m| m.globals[0].mutable,
            ),
            // --- tables ---
            (
                "table min and max",
                br#"(module (table 2 5 funcref))"#,
                |m| m.tables[0].limits.min == 2 && m.tables[0].limits.max == Some(5),
            ),
            (
                "a table's element type",
                br#"(module (table 1 externref))"#,
                |m| m.tables[0].element == V::EXTERNREF,
            ),
            (
                "an i64 (table64) index type",
                br#"(module (table i64 1 funcref))"#,
                |m| m.tables[0].limits.is64,
            ),
            (
                "a table's initializer expression",
                br#"(module (func $f) (table 1 funcref (ref.func $f)))"#,
                |m| m.tables[0].init.is_some(),
            ),
            // --- memories ---
            (
                "memory min and max",
                br#"(module (memory 2 5))"#,
                |m| m.memories[0].limits.min == 2 && m.memories[0].limits.max == Some(5),
            ),
            (
                "a shared memory",
                br#"(module (memory 1 2 shared))"#,
                |m| m.memories[0].limits.shared,
            ),
            (
                "an i64 (memory64) index type",
                br#"(module (memory i64 1))"#,
                |m| m.memories[0].limits.is64,
            ),
            (
                "an inline (data …) sizes the memory and emits the segment",
                br#"(module (memory (data "xyz")))"#,
                |m| m.data.len() == 1 && m.data[0].bytes == b"xyz" && m.data[0].active,
            ),
            // --- globals ---
            (
                "a mutable global",
                br#"(module (global (mut i32) (i32.const 1)))"#,
                |m| m.globals[0].mutable,
            ),
            (
                "an immutable global",
                br#"(module (global i32 (i32.const 1)))"#,
                |m| !m.globals[0].mutable,
            ),
            (
                "a global's initializer VALUE",
                br#"(module (global i32 (i32.const 7)))"#,
                // `i32.const 7` then `end`. The value has to be in the bytes: an initializer
                // emitted as a default would still decode and still validate.
                |m| m.global_inits[0] == [0x41, 0x07, 0x0b],
            ),
            // --- tags ---
            (
                "a defined tag's type index",
                br#"(module (type $u (func)) (type $t (func (param i32))) (tag (type $t)))"#,
                |m| m.tags == [1],
            ),
            (
                "an imported tag",
                br#"(module (type $t (func)) (import "a" "b" (tag (type $t))))"#,
                |m| matches!(m.imports[0].ty, Extern::Tag(_)),
            ),
            // --- element segments ---
            (
                "an elem segment's table index",
                br#"(module (table 1 funcref) (table 4 funcref) (func $f)
                      (elem (table 1) (i32.const 0) func $f))"#,
                |m| m.elements[0].table_index == 1,
            ),
            (
                "a declarative elem segment",
                br#"(module (func $f) (elem declare func $f))"#,
                |m| m.elements[0].mode == ElementMode::Declarative,
            ),
            (
                "a passive elem segment with an explicit type",
                br#"(module (func $f) (elem funcref (ref.func $f)))"#,
                |m| {
                    m.elements[0].mode == ElementMode::Passive
                        && m.elements[0].elem_type == V::FUNCREF
                },
            ),
            (
                "an (item …) entry forces the expression encoding",
                br#"(module (func $f) (elem funcref (item (ref.func $f))))"#,
                |m| m.elements[0].exprs.len() == 1 && m.elements[0].funcs.is_empty(),
            ),
            (
                "an active elem segment carries its offset",
                br#"(module (table 1 funcref) (func $f) (elem (i32.const 0) $f))"#,
                |m| {
                    m.elements[0].mode == ElementMode::Active
                        && !m.elements[0].offset_expr.is_empty()
                },
            ),
            // --- data segments ---
            (
                "a data segment's memory index",
                br#"(module (memory 1) (memory 1) (data (memory 1) (i32.const 0) "x"))"#,
                |m| m.data[0].mem_index == 1,
            ),
            (
                "a passive data segment",
                br#"(module (memory 1) (data "x"))"#,
                |m| !m.data[0].active,
            ),
            (
                "an active data segment's offset",
                br#"(module (memory 1) (data (i32.const 4) "x"))"#,
                |m| m.data[0].active && !m.data[0].offset_expr.is_empty(),
            ),
            // --- imports, one row per kind whose TYPE travels with it ---
            (
                "an imported table's type",
                br#"(module (import "a" "b" (table 3 7 externref)))"#,
                |m| {
                    m.tables[0].limits.min == 3
                        && m.tables[0].limits.max == Some(7)
                        && m.tables[0].element == V::EXTERNREF
                },
            ),
            (
                "an imported memory's type",
                br#"(module (import "a" "b" (memory 3 7 shared)))"#,
                |m| {
                    m.memories[0].limits.min == 3
                        && m.memories[0].limits.max == Some(7)
                        && m.memories[0].limits.shared
                },
            ),
            // --- the data-count section, which exists ONLY because a body can reference a
            // segment by index (§5.5.13). Emitted conditionally, so both directions are rows.
            (
                "memory.init forces the data-count section",
                br#"(module (memory 1) (data "x")
                      (func (memory.init 0 (i32.const 0) (i32.const 0) (i32.const 0))))"#,
                |m| m.section(crate::types::SectionId::DataCount).is_some(),
            ),
            (
                "and a module that cannot reference one does NOT carry it",
                br#"(module (memory 1) (data (i32.const 0) "x"))"#,
                |m| m.section(crate::types::SectionId::DataCount).is_none(),
            ),
            // --- exports: one row covering every KIND, because the kind byte is per-kind code ---
            (
                "every export kind reaches the export section",
                br#"(module (func (export "f")) (table (export "t") 1 funcref)
                      (memory (export "m") 1) (global (export "g") i32 (i32.const 0)))"#,
                |m| {
                    m.exports.len() == 4
                        && m.exports.iter().any(|e| e.name == "f" && matches!(e.ty, Extern::Func(_)))
                        && m.exports.iter().any(|e| e.name == "t" && matches!(e.ty, Extern::Table(_)))
                        && m.exports.iter().any(|e| e.name == "m" && matches!(e.ty, Extern::Memory(_)))
                        && m.exports.iter().any(|e| e.name == "g" && matches!(e.ty, Extern::Global(_)))
                },
            ),
        ];

        let mut lost = Vec::new();
        for (label, src, check) in rows {
            match assemble(src) {
                Ok(bytes) => match crate::module::decode(&bytes) {
                    Ok(md) => {
                        if !check(&md) {
                            lost.push(format!("{label}: assembled, but the clause is GONE"));
                        }
                    }
                    Err(e) => lost.push(format!("{label}: emitted bytes do not decode: {e:?}")),
                },
                Err(e) => lost.push(format!("{label}: does not assemble: {e}")),
            }
        }
        assert!(
            lost.is_empty(),
            "the emitter reconstructs a module from a SUBSET of the parser's facts — \
             {} clause(s) did not survive the round trip:\n  {}",
            lost.len(),
            lost.join("\n  ")
        );
    }

    /// A segment may name its memory or table with a **bare index**, and both spellings must
    /// mean the same thing.
    ///
    /// ⚠️⚠️ **The `elem` half was not a refusal — it assembled a DIFFERENT SEGMENT.** With
    /// nothing consuming the `0`, `(elem 0 (i32.const 1) $f)` lost its offset too and became a
    /// *passive* segment of three items (`ref.func 0`, the offset expression, `ref.func $f`).
    /// It surfaced as the validator's `TypeMismatch`, an error naming neither the stage nor the
    /// thing that was wrong, and only because an `i32.const` cannot be a `funcref`. So this test
    /// asserts the segment's MEANING, not merely that the module assembles: an active segment
    /// writing `$f` at index 1, checked by calling through the table.
    #[test]
    fn a_segment_may_name_its_target_with_a_bare_index() {
        // Both spellings of the same active elem segment must behave identically.
        for elem in [
            br#"(elem 0 (i32.const 1) $f)"#.as_slice(),
            br#"(elem (table 0) (i32.const 1) func $f)"#.as_slice(),
            br#"(elem 0 (offset (i32.const 1)) func $f)"#.as_slice(),
        ] {
            let src = [
                br#"(module (type $t (func (result i32))) (table 4 funcref)
                      (func $f (result i32) (i32.const 11)) "#
                    .as_slice(),
                elem,
                br#" (func (export "call") (param i32) (result i32)
                       (call_indirect (type $t) (local.get 0))))"#
                    .as_slice(),
            ]
            .concat();
            let m = assemble(&src).unwrap_or_else(|e| panic!("must assemble: {e}"));
            let md = crate::module::decode(&m).expect("must decode");
            crate::validate::validate(&md).expect("must validate");
            // ⚠️ The segment must be ACTIVE and land at index 1 — a passive segment also
            // assembles, decodes and validates, which is exactly how the defect hid.
            assert_eq!(md.elements.len(), 1);
            assert_eq!(
                md.elements[0].mode,
                crate::module::ElementMode::Active,
                "a bare index must not turn an active segment passive"
            );
            assert_eq!(md.elements[0].funcs.len() + md.elements[0].exprs.len(), 1,
                "the bare index must be the TABLE, never an element item");
        }
        // And the data twin, which merely came back `BadForm`.
        for data in [
            br#"(data 0 (i32.const 0) "x")"#.as_slice(),
            br#"(data (memory 0) (i32.const 0) "x")"#.as_slice(),
            br#"(data 0 (offset (i32.const 0)) "x")"#.as_slice(),
        ] {
            let src = [br#"(module (memory 1) "#.as_slice(), data, b")"].concat();
            let m = assemble(&src).unwrap_or_else(|e| panic!("must assemble: {e}"));
            let md = crate::module::decode(&m).expect("must decode");
            crate::validate::validate(&md).expect("must validate");
            assert_eq!(md.data.len(), 1);
        }
    }

    /// The exclusions, which are the whole rule: a keyword in the use-index position introduces
    /// a PASSIVE segment's element list and must not be read as a table index. Reading
    /// `(elem func $f)` as "table `func`" would be the same defect pointing the other way.
    ///
    /// 🔒 A `$name` in the bare position stays REFUSED — wasmtime refuses it, and nothing in the
    /// spec testsuite or the wasmtk corpus spells one.
    #[test]
    fn a_keyword_is_not_a_bare_use_index() {
        for src in [
            br#"(module (func $f) (elem func $f))"#.as_slice(),
            br#"(module (func $f) (elem funcref (ref.func $f)))"#.as_slice(),
            br#"(module (table 1 funcref) (func $f) (elem declare func $f))"#.as_slice(),
            br#"(module (memory 1) (data "abc"))"#.as_slice(),
        ] {
            let m = assemble(src)
                .unwrap_or_else(|e| panic!("{} must assemble: {e}", core::str::from_utf8(src).unwrap()));
            crate::module::decode(&m).expect("and decode");
        }
        assert!(
            assemble(br#"(module (memory $m 1) (data $seg $m (i32.const 0) "x"))"#).is_err(),
            "a NAMED bare use-index must stay refused, as wasmtime refuses it"
        );
    }

    /// Track P — every text spelling of a `(pagesize N)` memory must CARRY its page size, byte for
    /// byte as wasm-tools writes it (each expectation below is wasm-tools 1.259's output, past the
    /// 8-byte header).
    ///
    /// ⚠️⚠️ **A wrong-ANSWER test, not a conformance test** — the history is why. Until 2026-09-17
    /// `(memory 1 (pagesize 1))` assembled byte-identical to `(memory 1)`: a guest asking for 1-byte
    /// pages got 64 KiB ones. X1 then refused every spelling (this test pinned that refusal); Track P
    /// replaced the refusal with the feature, and the test with this one.
    ///
    /// 🔒 The clause travels in two positions (after the limits; before an inline `(data …)`) and
    /// through two parsers (a definition; an import descriptor). All are pinned, because the first
    /// guard ever written covered only one parser.
    #[test]
    fn a_page_size_clause_is_carried_in_every_position() {
        fn hex(s: &str) -> Vec<u8> {
            s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
        }
        for (src, want) in [
            (r#"(module (memory 1 (pagesize 1)))"#, "05 04 01 08 01 00"),
            (r#"(module (memory $m 1 2 (pagesize 65536)))"#,
             "05 05 01 09 01 02 10 00 0b 04 6e 61 6d 65 06 04 01 00 01 6d"),
            (r#"(module (memory (pagesize 1) (data "xyz")))"#,
             "05 05 01 09 03 03 00 0b 09 01 00 41 00 0b 03 78 79 7a"),
            (r#"(module (memory (export "m") 0 (pagesize 1)))"#, "05 04 01 08 00 00 07 05 01 01 6d 02 00"),
            (r#"(module (memory i64 1 (pagesize 65536)))"#, "05 04 01 0c 01 10"),
            (r#"(module (import "a" "b" (memory 1 (pagesize 1))))"#, "02 09 01 01 61 01 62 02 08 01 00"),
            (r#"(module (memory (import "m" "x") 0 (pagesize 65536)))"#, "02 09 01 01 6d 01 78 02 08 00 10"),
            (r#"(module (memory 1 2 shared (pagesize 1)))"#, "05 05 01 0b 01 02 00"),
        ] {
            let m = assemble(src.as_bytes()).unwrap_or_else(|e| panic!("{src} must assemble: {e:?}"));
            assert_eq!(m[8..], hex(want)[..], "{src}");
        }
    }

    /// The memory-type grammar is exact — `addrtype? min max? shared? (pagesize N)?` or
    /// `addrtype? (pagesize N)? (data …)` — and wasm-tools refuses everything else. The last two
    /// used to assemble with a clause silently IGNORED (`(memory 1 2 3)` became `(memory 1 2)`).
    #[test]
    fn a_memory_type_is_read_exactly() {
        for (src, e) in [
            (r#"(module (memory 0 (pagesize 3)))"#, Error::InvalidPageSize),
            (r#"(module (memory 0 (pagesize 0)))"#, Error::InvalidPageSize),
            (r#"(module (memory (pagesize 3) (data "x")))"#, Error::InvalidPageSize),
            (r#"(module (memory 1 2 (pagesize 1) shared))"#, Error::UnexpectedToken),
            (r#"(module (memory 1 (pagesize 1) (pagesize 1)))"#, Error::UnexpectedToken),
            (r#"(module (memory (data "x") (pagesize 1)))"#, Error::UnexpectedToken),
            (r#"(module (memory 1 2 3))"#, Error::UnexpectedToken),
            (r#"(module (import "a" "b" (memory 1 2 3)))"#, Error::UnexpectedToken),
        ] {
            assert_eq!(assemble(src.as_bytes()).unwrap_err(), e, "{src}");
        }
        // A power of two the proposal does not allow is the VALIDATOR's to refuse, not the text's.
        assert!(assemble(br#"(module (memory 0 (pagesize 4)))"#).is_ok());
    }

    /// The other half, and the half a guard gets wrong: a plain memory must still assemble.
    /// A refusal that also refuses MVP memories is not a fix, and X3 demands this test shape for
    /// every gated proposal — the flag refuses its own feature and nothing else.
    #[test]
    fn a_plain_memory_still_assembles() {
        for src in [
            br#"(module (memory 1))"#.as_slice(),
            br#"(module (memory 1 2))"#.as_slice(),
            br#"(module (memory 1 2 shared))"#.as_slice(),
            br#"(module (memory i64 1))"#.as_slice(),
            br#"(module (memory (data "xyz")))"#.as_slice(),
            br#"(module (import "a" "b" (memory 1)))"#.as_slice(),
            br#"(module (memory (import "m" "x") 0))"#.as_slice(),
        ] {
            let m = assemble(src)
                .unwrap_or_else(|e| panic!("{} must assemble: {e}", core::str::from_utf8(src).unwrap()));
            crate::module::decode(&m).expect("and decode");
        }
    }

    /// A module has one start function. The assembler kept the LAST `(start …)` silently, so
    /// `(start $a) (start $b)` built a module that ran only `$b` (`start.wast`, hidden behind
    /// the `.wast` runner's whole-module-quote wrapper until 2026-09-19).
    #[test]
    fn a_second_start_field_is_malformed() {
        assert_eq!(
            assemble(b"(module (func $a) (func $b) (start $a) (start $b))").unwrap_err(),
            Error::MultipleStart
        );
        assemble(b"(module (func $a) (start $a))").unwrap();
    }

    /// `catch` / `catch_all` / `delegate` are TEXT only directly inside a legacy `try`, in
    /// grammar order: `(catch x …)* (catch_all …)?`, or a lone `delegate`. They used to be
    /// emitted anywhere and refused later by the VALIDATOR — the wrong stage for an
    /// `assert_malformed`. Both forms, because they reach the emitter by different routes and
    /// the first fix covered only the flat one.
    #[test]
    fn a_legacy_handler_outside_its_try_is_malformed() {
        for src in [
            "(module (func (catch_all)))",
            "(module (tag $e) (func (catch $e)))",
            "(module (func (delegate 0)))",
            "(module (func catch_all))",
            "(module (func delegate 0))",
            "(module (func block try end catch_all end))",
            "(module (func (try (do catch_all))))",
            "(module (func try catch_all catch_all end))",
            "(module (tag $e) (func try catch_all catch $e end))",
            "(module (tag $e) (func try catch $e delegate 0))",
            "(module (func (try (do) (catch_all) (catch_all))))",
            "(module (tag $e) (func (try (do) (catch_all) (catch $e))))",
        ] {
            assert_eq!(assemble(src.as_bytes()).unwrap_err(), Error::UnexpectedToken, "{src}");
        }
        for src in [
            "(module (tag $e) (func try catch $e catch $e catch_all end))",
            "(module (func try delegate 0))",
            "(module (func block try nop delegate 0 end))",
            "(module (tag $e) (func try block end catch $e try catch_all end end))",
            "(module (tag $e) (func (try (do) (catch $e) (catch $e) (catch_all))))",
        ] {
            let m = assemble(src.as_bytes()).unwrap_or_else(|e| panic!("{src} must assemble: {e:?}"));
            crate::module::decode(&m).unwrap_or_else(|e| panic!("{src} must decode: {e:?}"));
        }
    }

    // --- custom annotations (2026-09-19) ---------------------------------------------------
    //
    // Each expected module below is wasm-tools 1.259's output for the same source, BYTE FOR
    // BYTE — for these three, wasmrt's encoding of everything else coincides with it, so the
    // whole module can be compared rather than a section. The corpus-scale check, which cannot
    // compare whole modules, is `scripts/custom-sections-diff.ts`.

    fn asm(src: &str) -> Result<Vec<u8>> {
        assemble(src.as_bytes())
    }

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    /// `$id`s reach the name section, as wasm-tools writes them by default: module (0),
    /// function (1) and local (2) subsections here.
    #[test]
    fn ids_are_written_to_the_name_section() {
        assert_eq!(
            asm(r"(module $M (func $f (param $p i32)))").unwrap(),
            hex("00 61 73 6d 01 00 00 00 01 05 01 60 01 7f 00 03 02 01 00 0a 04 01 02 00 0b \
                 00 17 04 6e 61 6d 65 00 02 01 4d 01 04 01 00 01 66 02 06 01 00 01 00 01 70")
        );
    }

    /// A branch hint lands in `metadata.code.branch_hint`, just before `code`, at the offset
    /// of its instruction counted from the start of the body (locals included): the `if` at 5
    /// and the flat `br_if` at 10.
    #[test]
    fn branch_hints_are_emitted_at_their_instruction() {
        assert_eq!(
            asm(r#"(module (func (local i32)
                     (@metadata.code.branch_hint "\01") (if (i32.const 1) (then))
                     i32.const 0 (@metadata.code.branch_hint "\00") br_if 0))"#)
            .unwrap(),
            hex("00 61 73 6d 01 00 00 00 01 04 01 60 00 00 03 02 01 00 00 23 19 6d 65 74 61 64 \
                 61 74 61 2e 63 6f 64 65 2e 62 72 61 6e 63 68 5f 68 69 6e 74 01 00 02 05 01 01 \
                 0a 01 00 0a 0f 01 0d 01 01 7f 41 01 04 40 0b 41 00 0d 00 0b")
        );
    }

    /// `@custom` lands at its slot, and the slot — not the source order — decides: `b` is
    /// written first but belongs after `func`, `a` before it.
    #[test]
    fn custom_sections_are_placed_by_slot() {
        assert_eq!(
            asm(r#"(module (@custom "b" (after func) "2") (@custom "a" (before func) "1") (func))"#)
                .unwrap(),
            hex("00 61 73 6d 01 00 00 00 01 04 01 60 00 00 00 03 01 61 31 03 02 01 00 00 03 01 \
                 62 32 0a 04 01 02 00 0b")
        );
    }

    /// Every malformed or misplaced annotation REFUSES the module — what wasm-tools does for
    /// each of these — and names the rule, worded as the spec suite words it. A hint the
    /// emitter never reaches (before an immediate) is refused too rather than dropped.
    #[test]
    fn a_malformed_or_misplaced_annotation_refuses_the_module() {
        for (src, why) in [
            (r"(module (@custom))", "@custom annotation: missing section name"),
            (r#"(module (@custom "\df"))"#, "@custom annotation: malformed UTF-8 encoding"),
            (r#"(module (@custom "a" here))"#, "@custom annotation: unexpected token"),
            (r#"(module (@custom "a" (type)))"#, "@custom annotation: malformed placement"),
            (r#"(module (@custom "a" (before types)))"#, "@custom annotation: malformed section kind"),
            (r#"(module (@custom "a" (before datacount)))"#, "@custom annotation: malformed section kind"),
            (r#"(module (func (@custom "a")))"#, "misplaced @custom annotation"),
            (r#"(module (@name "A") (@name "B"))"#, "@name annotation: multiple module"),
            (r#"(module (func) (@name "M"))"#, "misplaced @name annotation"),
            (r#"(module (type (struct (field $x (@name "X") i32))))"#, "misplaced @name annotation"),
            (r#"(module (@metadata.code.branch_hint "\01") (func))"#,
             "@metadata.code.branch_hint annotation: not in a function"),
            (r#"(module (func (@metadata.code.branch_hint "\02") (if (i32.const 0) (then))))"#,
             "@metadata.code.branch_hint annotation: invalid value"),
            (r#"(module (func nop (@metadata.code.branch_hint "\01")))"#,
             "@metadata.code.branch_hint annotation: must precede an instruction"),
            (r#"(module (func i32.const (@metadata.code.branch_hint "\01") 0 drop))"#,
             "@metadata.code.branch_hint annotation: must precede an instruction"),
        ] {
            assert_eq!(asm(src).unwrap_err(), Error::Annotation(why), "{src}");
        }
        // Accepted, as wasm-tools accepts them: an unknown annotation anywhere, and a hint in a
        // constant expression (dropped there, as wasm-tools drops it).
        for src in [
            r"(module (@foo bar) (func (@baz) nop))",
            r#"(module (global i32 (@metadata.code.branch_hint "\01") (i32.const 0)))"#,
        ] {
            asm(src).unwrap_or_else(|e| panic!("{src} must assemble: {e:?}"));
        }
    }
}
