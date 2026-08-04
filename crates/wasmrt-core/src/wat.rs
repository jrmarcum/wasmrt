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

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::sexpr::{self, Sexpr};
use crate::types::ValType;

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
    /// An instruction name the assembler does not know.
    UnknownInstr,
    /// A form of the wrong shape (an atom where a list was required, etc.).
    BadForm,
    /// A numeric literal that does not parse, or does not fit its type.
    BadNumber,
    /// A branch naming a label that is not in scope.
    UnknownLabel,
    /// Nesting beyond the assembler's control-depth cap.
    NestingTooDeep,
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
            Error::Unsupported(what) => write!(f, "unsupported text construct: {what}"),
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

/// Emit a value type: a concrete `(ref null? $t)` as `0x63`/`0x64` + a signed type index,
/// anything else as its single valtype byte.
fn emit_val_type(out: &mut Vec<u8>, v: V) -> Result<()> {
    if v.is_concrete() {
        out.push(if v.is_non_null_ref() { 0x64 } else { 0x63 });
        sleb(out, i64::from(v.concrete_index()));
    } else {
        let bits = v.bits();
        if bits > 0xff {
            return Err(Error::BadValType);
        }
        out.push(bits as u8);
    }
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

/// Strip `_` digit separators, which the text format allows inside a number.
fn strip_seps(s: &str) -> String {
    s.chars().filter(|&c| c != '_').collect()
}

/// Parse an unsigned integer literal: decimal, or `0x`-prefixed hex.
fn parse_u64_str(s: &str) -> Result<u64> {
    let t = strip_seps(s);
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
    let t = strip_seps(s);
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
fn parse_float_bits(lit: &str, f: FloatFmt) -> Option<u64> {
    // The wasm-specific NaN spellings: `nan:canonical`, `nan:arithmetic`, `nan:0x<payload>`.
    if let Some(colon) = lit.find(':') {
        let canonical: u64 = 1u64 << (f.mant_bits - 1);
        let exp_all = (f.max_biased as u64) << f.mant_bits;
        let mant_mask = (1u64 << f.mant_bits) - 1;
        let tail = &lit[colon + 1..];
        let mut bits = exp_all | canonical;
        if tail != "canonical" && tail != "arithmetic" {
            let payload = parse_u64_str(tail).ok()?;
            bits = exp_all | (payload & mant_mask);
        }
        if lit.starts_with('-') {
            bits |= 1u64 << sign_shift(f);
        }
        return Some(bits);
    }

    let mut s = lit;
    let mut neg = false;
    if let Some(rest) = s.strip_prefix('-') {
        neg = true;
        s = rest;
    } else if let Some(rest) = s.strip_prefix('+') {
        s = rest;
    }

    // Not hex → decimal (and `inf` / `nan`), which Rust rounds correctly.
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
    let Some(body) = hex else {
        let cleaned = strip_seps(s);
        let v: f64 = if f.mant_bits == 23 {
            f64::from(cleaned.parse::<f32>().ok()?)
        } else {
            cleaned.parse::<f64>().ok()?
        };
        let bits = if f.mant_bits == 23 {
            u64::from((v as f32).to_bits())
        } else {
            v.to_bits()
        };
        return Some(if neg { bits | (1u64 << sign_shift(f)) } else { bits });
    };

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

    Some(compose_float_bits(q, ulp_exp, neg, f))
}

/// Parse a WAT `f32` literal to its bit pattern.
pub(crate) fn parse_f32_bits(lit: &str) -> Option<u32> {
    parse_float_bits(lit, F32_FMT).map(|b| b as u32)
}

/// Parse a WAT `f64` literal to its bit pattern.
pub(crate) fn parse_f64_bits(lit: &str) -> Option<u64> {
    parse_float_bits(lit, F64_FMT)
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

/// A value type spelled as a bare keyword.
fn string_to_val_type(atom: &str) -> Option<V> {
    Some(match atom {
        "i32" => V::I32,
        "i64" => V::I64,
        "f32" => V::F32,
        "f64" => V::F64,
        "v128" => V::V128,
        // `anyfunc` is the pre-standard spelling of `funcref`; MVP-era tools and
        // hand-written `.wat` still emit it (`(table N anyfunc)`).
        "funcref" | "nullfuncref" | "anyfunc" => V::FUNCREF,
        "externref" | "nullexternref" => V::EXTERNREF,
        "anyref" => V::ANYREF,
        "eqref" => V::EQREF,
        "i31ref" => V::I31REF,
        "structref" => V::STRUCTREF,
        "arrayref" => V::ARRAYREF,
        "nullref" => V::NULLREF,
        "exnref" | "nullexnref" => V::EXNREF,
        _ => return None,
    })
}

/// A heap type → a reference value type. A `$name` or numeric index is a **concrete**
/// typed reference carrying that type index; the abstract heads map to their own value
/// types. `nullable` picks the nullable or non-null variant.
fn heap_type_to_val_type(s: &Sexpr, nullable: bool, type_names: &[Option<String>]) -> Result<V> {
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
    let pair = match atom {
        "func" | "funcref" | "nofunc" => (V::FUNCREF, V::FUNCREF_NN),
        "extern" | "externref" | "noextern" => (V::EXTERNREF, V::EXTERNREF_NN),
        "exn" | "exnref" | "noexn" => (V::EXNREF, V::EXNREF_NN),
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

/// Parse a value type: a bare keyword, or the list form `(ref null? ht)`.
fn parse_val_type(s: &Sexpr, type_names: &[Option<String>]) -> Result<V> {
    if let Some(l) = s.as_list() {
        if l.len() >= 2 && eq_atom(&l[0], "ref") {
            let nullable = l.len() >= 3 && eq_atom(&l[1], "null");
            return heap_type_to_val_type(&l[l.len() - 1], nullable, type_names);
        }
        return Err(Error::BadValType);
    }
    string_to_val_type(want_atom(s).map_err(|_| Error::BadValType)?).ok_or(Error::BadValType)
}

/// Is this form a reference type (`(ref …)`)?
fn is_ref_type_form(s: &Sexpr) -> bool {
    s.as_list()
        .is_some_and(|l| !l.is_empty() && eq_atom(&l[0], "ref"))
}

// --- Module-level definitions -------------------------------------------------

/// A function signature, interned in the type section.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Sig {
    params: Vec<V>,
    results: Vec<V>,
}

/// Intern a signature, returning its type index. An identical existing entry is reused, so
/// inline `(param …)(result …)` annotations don't bloat the type section.
fn intern_sig(sigs: &mut Vec<Sig>, sig: Sig) -> u32 {
    if let Some(i) = sigs.iter().position(|s| *s == sig) {
        return i as u32;
    }
    sigs.push(sig);
    (sigs.len() - 1) as u32
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
}

#[derive(Debug, Clone, Copy)]
struct TableDef {
    min: u32,
    max: Option<u32>,
    elem: V,
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
    sigs: Vec<Sig>,
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
            return assemble_module(want_list(&form)?);
        }
    }
    Err(Error::NotAModule)
}

/// Assemble an already-parsed `(module …)` form (`module[0]` is the `module` keyword).
///
/// # Errors
/// Returns an [`Error`] describing the first problem found.
pub fn assemble_module(module: &[Sexpr]) -> Result<Vec<u8>> {
    // `(module binary "…" …)` — the strings ARE the module, verbatim.
    if module.len() >= 2 && eq_atom(&module[1], "binary") {
        let mut out = Vec::new();
        for s in &module[2..] {
            out.extend_from_slice(want_str(s)?);
        }
        return Ok(out);
    }
    // Skip the optional module `$name`.
    let start = usize::from(module.len() > 1 && is_id(&module[1])) + 1;
    let fields = module.get(start..).unwrap_or(&[]);

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
                for t in &want_list(field)?[1..] {
                    if t.keyword() == Some("type") {
                        let l = want_list(t)?;
                        b.type_names.push(type_def_name(l));
                        type_forms.push(l);
                    }
                }
            }
            _ => {}
        }
    }
    // Pre-pass B: the bodies, now that every type name resolves.
    for form in &type_forms {
        parse_type_body(form, &b.type_names, &mut b.sigs)?;
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
            "func" => parse_func_field(items, &mut b)?,
            "memory" => parse_memory_field(items, &mut b)?,
            "global" => parse_global_field(items, &mut b)?,
            "table" => parse_table_field(items, &mut b)?,
            "elem" => parse_elem_field(items, &mut b)?,
            "data" => parse_data_field(items, &mut b)?,
            "tag" => parse_tag_field(items, &mut b)?,
            "import" => parse_import_field(items, &mut b)?,
            "start" => b.start = Some(nth(items, 1)?.clone()),
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
                intern_sig(&mut b.sigs, sig)
            }
        };
        func_sigs.push(ti);
    }

    // Encode every body and const-expr BEFORE any section is written: a multi-value block
    // type or an inline `call_indirect` signature interns into the type table as it is
    // encoded, so the type section is only complete once the last body is done.
    let funcs = core::mem::take(&mut b.funcs);
    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(funcs.len());
    for f in &funcs {
        bodies.push(encode_body(f, &mut b)?);
    }
    let globals = core::mem::take(&mut b.globals);
    let mut global_inits: Vec<Vec<u8>> = Vec::with_capacity(globals.len());
    for g in &globals {
        let mut out = Vec::new();
        emit_const_expr(&mut out, &g.init, &mut b)?;
        global_inits.push(out);
    }
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

    emit_module(
        &b,
        &func_sigs,
        &bodies,
        &globals,
        &global_inits,
        &elem_bytes,
        &datas,
        &data_offsets,
    )
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

/// Parse a `(type …)` body into the shared signature table. Only function types are
/// handled here; GC struct/array definitions land with the GC text forms.
fn parse_type_body(
    items: &[Sexpr],
    type_names: &[Option<String>],
    sigs: &mut Vec<Sig>,
) -> Result<()> {
    let mut j = 1;
    if items.get(j).is_some_and(is_id) {
        j += 1;
    }
    let l = want_list(nth(items, j)?)?;
    match want_atom(nth(l, 0)?)? {
        "func" => {
            // A `(type …)` definition occupies its own slot even when an identical
            // signature already exists, so push rather than intern — the declared index
            // must match its position.
            sigs.push(parse_sig(&l[1..], type_names, None)?);
            Ok(())
        }
        "struct" | "array" | "sub" => Err(Error::Unsupported("GC type definitions")),
        _ => Err(Error::BadForm),
    }
}

/// Parse `(param …)* (result …)*` into a signature. When `names` is given, each param's
/// optional `$id` is recorded there (index-aligned with the local index space).
fn parse_sig(
    items: &[Sexpr],
    type_names: &[Option<String>],
    mut names: Option<&mut Vec<Option<String>>>,
) -> Result<Sig> {
    let mut sig = Sig::default();
    for item in items {
        match item.keyword() {
            Some("param") => {
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
                for t in &want_list(item)?[1..] {
                    sig.results.push(parse_val_type(t, type_names)?);
                }
            }
            _ => {}
        }
    }
    Ok(sig)
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
fn zero_offset() -> Vec<Sexpr> {
    vec![Sexpr::List(vec![
        Sexpr::Atom("i32.const".to_string()),
        Sexpr::Atom("0".to_string()),
    ])]
}

fn parse_func_field(items: &[Sexpr], b: &mut ModuleBuild) -> Result<()> {
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

    // Header clauses, in any order: import/export, `(type $t)`, params/results, locals.
    // The first form that is none of those begins the body.
    let mut k = j;
    while k < items.len() {
        match items[k].keyword() {
            Some("import" | "export") => {}
            Some("type") => {
                type_ref = Some(resolve_by_name(
                    &b.type_names,
                    nth(want_list(&items[k])?, 1)?,
                )?);
            }
            Some("param" | "result") => {
                let s = parse_sig(
                    core::slice::from_ref(&items[k]),
                    &b.type_names,
                    Some(&mut local_names),
                )?;
                sig.params.extend(s.params);
                sig.results.extend(s.results);
            }
            Some("local") => {
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

    // An explicit `(type $t)` with no inline params/results takes its signature from the
    // referenced type, so locals resolve against the right param count.
    if let Some(ti) = type_ref {
        if sig.params.is_empty() && sig.results.is_empty() {
            if let Some(s) = b.sigs.get(ti as usize) {
                sig = s.clone();
                // The referenced type's params are unnamed here.
                if local_names.is_empty() {
                    local_names = vec![None; sig.params.len()];
                }
            }
        }
    }

    if let Some(r) = import {
        let ti = type_ref.unwrap_or_else(|| intern_sig(&mut b.sigs, sig));
        b.func_imports.push(ImportedFunc { r, type_index: ti });
        b.import_order.push(ImportKind::Func);
    } else {
        b.funcs.push(Func {
            type_ref,
            sig,
            local_names,
            locals,
            body: items[k..].to_vec(),
        });
    }
    b.func_names.push(name);
    Ok(())
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

    // `(memory $m (data "…"))` — an inline data segment sizes the memory.
    if let Some(d) = items.get(j).filter(|s| eq_kw(s, "data")) {
        let mut bytes = Vec::new();
        for s in &want_list(d)?[1..] {
            bytes.extend_from_slice(want_str(s)?);
        }
        let pages = bytes.len().div_ceil(65536) as u64;
        b.memories.push(MemoryDef {
            min: pages,
            max: Some(pages),
            shared: false,
            is64: false,
        });
        b.mem_names.push(name);
        b.datas.push(DataSeg {
            mem_index: mi,
            offset: Some(zero_offset()),
            bytes,
        });
        b.data_names.push(None);
        return Ok(());
    }

    let mut is64 = false;
    if items.get(j).is_some_and(|s| eq_atom(s, "i64")) {
        is64 = true;
        j += 1;
    } else if items.get(j).is_some_and(|s| eq_atom(s, "i32")) {
        j += 1;
    }
    let min = match items.get(j) {
        Some(s) => parse_u64_str(want_atom(s)?)?,
        None => 0,
    };
    j += 1;
    let mut max = None;
    if let Some(a) = items.get(j).and_then(Sexpr::as_atom) {
        if a != "shared" {
            max = Some(parse_u64_str(a)?);
            j += 1;
        }
    }
    let shared = items.get(j).is_some_and(|s| eq_atom(s, "shared"));
    let m = MemoryDef {
        min,
        max,
        shared,
        is64,
    };
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
fn parse_table_limits(items: &[Sexpr], j: &mut usize) -> Result<(u32, Option<u32>)> {
    let min = u32::try_from(parse_u64_str(want_atom(nth(items, *j)?)?)?)
        .map_err(|_| Error::BadNumber)?;
    *j += 1;
    let mut max = None;
    if let Some(a) = items.get(*j).and_then(Sexpr::as_atom) {
        if a.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            max = Some(u32::try_from(parse_u64_str(a)?).map_err(|_| Error::BadNumber)?);
            *j += 1;
        }
    }
    Ok((min, max))
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
        let elem_type = parse_val_type(nth(items, j)?, &b.type_names)?;
        let entries: Vec<Vec<Sexpr>> = want_list(&items[j + pos])?[1..]
            .iter()
            .map(|s| {
                if s.as_list().is_some() {
                    vec![s.clone()]
                } else {
                    vec![Sexpr::List(vec![
                        Sexpr::Atom("ref.func".to_string()),
                        s.clone(),
                    ])]
                }
            })
            .collect();
        let n = entries.len() as u32;
        b.tables.push(TableDef {
            min: n,
            max: Some(n),
            elem: elem_type,
        });
        b.table_names.push(name);
        b.elems.push(ElemDef {
            table_index: ti,
            offset: Some(zero_offset()),
            elem_type,
            items: entries,
            declarative: false,
        });
        b.elem_names.push(None);
        return Ok(());
    }

    let (min, max) = parse_table_limits(items, &mut j)?;
    let elem = parse_val_type(nth(items, j)?, &b.type_names)?;
    let t = TableDef { min, max, elem };
    if let Some(r) = import {
        b.table_imports.push(ImportedTable { r, t });
        b.import_order.push(ImportKind::Table);
    } else {
        b.tables.push(t);
    }
    b.table_names.push(name);
    Ok(())
}

/// Parse a tag's `(type $t)` and/or inline `(param …)`, returning its type index.
fn parse_tag_type(items: &[Sexpr], b: &mut ModuleBuild) -> Result<u32> {
    let mut type_ref = None;
    let mut sig = Sig::default();
    for item in items {
        match item.keyword() {
            Some("type") => {
                type_ref = Some(resolve_by_name(&b.type_names, nth(want_list(item)?, 1)?)?);
            }
            Some("param" | "result") => {
                let s = parse_sig(core::slice::from_ref(item), &b.type_names, None)?;
                sig.params.extend(s.params);
                sig.results.extend(s.results);
            }
            _ => {}
        }
    }
    Ok(type_ref.unwrap_or_else(|| intern_sig(&mut b.sigs, sig)))
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
            let ti = parse_tag_type(&desc[j..], b)?; // same (type $t) | (param…)(result…) shape
            b.func_imports.push(ImportedFunc { r, type_index: ti });
            b.import_order.push(ImportKind::Func);
            b.func_names.push(name);
        }
        "memory" => {
            let mut is64 = false;
            if desc.get(j).is_some_and(|s| eq_atom(s, "i64")) {
                is64 = true;
                j += 1;
            }
            let min = parse_u64_str(want_atom(nth(desc, j)?)?)?;
            j += 1;
            let mut max = None;
            if let Some(a) = desc.get(j).and_then(Sexpr::as_atom) {
                if a != "shared" {
                    max = Some(parse_u64_str(a)?);
                    j += 1;
                }
            }
            let shared = desc.get(j).is_some_and(|s| eq_atom(s, "shared"));
            b.mem_imports.push(ImportedMemory {
                r,
                m: MemoryDef {
                    min,
                    max,
                    shared,
                    is64,
                },
            });
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
            let (min, max) = parse_table_limits(desc, &mut j)?;
            let elem = parse_val_type(nth(desc, j)?, &b.type_names)?;
            b.table_imports.push(ImportedTable {
                r,
                t: TableDef { min, max, elem },
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
    let mut table_index = 0u32;
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
        if s.as_atom().is_some_and(|a| string_to_val_type(a).is_some()) || is_ref_type_form(s) {
            elem_type = parse_val_type(s, &b.type_names)?;
            j += 1;
        }
    }
    // The `func` keyword form: `(elem (i32.const 0) func $a $b)`.
    if items.get(j).is_some_and(|s| eq_atom(s, "func")) {
        j += 1;
    }
    let mut entries: Vec<Vec<Sexpr>> = Vec::new();
    for s in &items[j..] {
        if eq_kw(s, "item") {
            entries.push(want_list(s)?[1..].to_vec());
        } else if s.as_list().is_some() {
            entries.push(vec![s.clone()]);
        } else {
            entries.push(vec![Sexpr::List(vec![
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
    });
    b.elem_names.push(name);
    Ok(())
}

fn parse_data_field(items: &[Sexpr], b: &mut ModuleBuild) -> Result<()> {
    let mut j = 1;
    let name = opt_name(items, &mut j);
    let mut mem_index = 0u32;
    let mut offset: Option<Vec<Sexpr>> = None;
    if let Some(s) = items.get(j).filter(|s| eq_kw(s, "memory")) {
        mem_index = resolve_by_name(&b.mem_names, nth(want_list(s)?, 1)?)?;
        j += 1;
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
    elems: &[Vec<u8>],
    datas: &[DataSeg],
    data_offsets: &[Option<Vec<u8>>],
) -> Result<Vec<u8>> {
    let e = Encoded {
        func_sigs,
        bodies,
        globals,
        global_inits,
        elems,
        datas,
        data_offsets,
    };
    let mut out = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    // 1 — types.
    if !b.sigs.is_empty() {
        let mut c = Vec::new();
        uleb(&mut c, b.sigs.len() as u64);
        for s in &b.sigs {
            c.push(0x60);
            val_type_vec(&mut c, &s.params)?;
            val_type_vec(&mut c, &s.results)?;
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
                c.push(0x00);
                uleb(&mut c, u64::from(i.type_index));
            }
            ImportKind::Table => {
                let i = &b.table_imports[t];
                t += 1;
                name_bytes(&mut c, &i.r.module);
                name_bytes(&mut c, &i.r.name);
                c.push(0x01);
                emit_val_type(&mut c, i.t.elem)?;
                emit_limits(&mut c, u64::from(i.t.min), i.t.max.map(u64::from), false, false);
            }
            ImportKind::Mem => {
                let i = &b.mem_imports[m];
                m += 1;
                name_bytes(&mut c, &i.r.module);
                name_bytes(&mut c, &i.r.name);
                c.push(0x02);
                emit_limits(&mut c, i.m.min, i.m.max, i.m.shared, i.m.is64);
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
        for t in &b.tables {
            emit_val_type(&mut c, t.elem)?;
            emit_limits(&mut c, u64::from(t.min), t.max.map(u64::from), false, false);
        }
        push_section(out, 4, &c);
    }

    // 5 — memories.
    if !b.memories.is_empty() {
        let mut c = Vec::new();
        uleb(&mut c, b.memories.len() as u64);
        for m in &b.memories {
            emit_limits(&mut c, m.min, m.max, m.shared, m.is64);
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

    // 12 — data count (required whenever `memory.init`/`data.drop` can appear).
    if !e.datas.is_empty() {
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
    sigs: &'a mut Vec<Sig>,
    type_names: &'a [Option<String>],
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
}

/// Borrow every name table out of a [`ModuleBuild`] alongside a mutable `sigs`, so a body
/// can intern a block-type signature while still resolving names. The disjoint field
/// borrows are what make this sound.
macro_rules! ctx_for {
    ($b:expr, $locals:expr, $out:expr) => {
        Ctx {
            out: $out,
            sigs: &mut $b.sigs,
            type_names: &$b.type_names,
            func_names: &$b.func_names,
            global_names: &$b.global_names,
            table_names: &$b.table_names,
            elem_names: &$b.elem_names,
            data_names: &$b.data_names,
            mem_names: &$b.mem_names,
            tag_names: &$b.tag_names,
            local_names: $locals,
            labels: Vec::new(),
        }
    };
}

/// Encode one function body: the locals vector, the instruction sequence, then the
/// implicit `end`.
fn encode_body(f: &Func, b: &mut ModuleBuild) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    // One (count = 1, type) group per declared local — simple and always correct.
    uleb(&mut out, f.locals.len() as u64);
    for &t in &f.locals {
        uleb(&mut out, 1);
        emit_val_type(&mut out, t)?;
    }
    let locals = f.local_names.clone();
    let mut ctx = ctx_for!(b, &locals, out);
    emit_seq(&mut ctx, &f.body)?;
    ctx.out.push(0x0b); // implicit function end
    Ok(ctx.out)
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
    match &items[i] {
        Sexpr::List(l) => {
            emit_folded(ctx, l)?;
            Ok(i + 1)
        }
        Sexpr::Atom(name) => emit_flat(ctx, items, i, &name.clone()),
        Sexpr::Str(_) => Err(Error::UnknownInstr),
    }
}

/// A folded instruction: `(op operand* )` — the operands are emitted first, then the op.
fn emit_folded(ctx: &mut Ctx, l: &[Sexpr]) -> Result<()> {
    let kw = nth(l, 0)?.as_atom().ok_or(Error::UnknownInstr)?.to_string();
    match kw.as_str() {
        "block" | "loop" => return emit_folded_block(ctx, &kw, l),
        "if" => return emit_folded_if(ctx, l),
        _ => {}
    }
    // The prefixed families are looked up before the single-byte table — their members
    // have no `Op` of their own.
    if let Some((sub, imm)) = lookup_simd(&kw) {
        emit_simd(ctx, sub, imm, l, 1, true)?;
        return Ok(());
    }
    if let Some(sub) = lookup_atomic(&kw) {
        emit_atomic(ctx, sub, l, 1, true)?;
        return Ok(());
    }
    let op = crate::opcode::Op::from_text_name(&kw).ok_or(Error::UnknownInstr)?;
    if op == crate::opcode::Op::CallIndirect {
        return emit_call_indirect(ctx, l, 1, true).map(|_| ());
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
    emit_op_with_immediates(ctx, op, &l[..imm_end], 1)?;
    Ok(())
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
        O::TableGet | O::TableSet | O::TableSize | O::TableGrow | O::TableFill => 1,
        O::ElemDrop | O::DataDrop | O::RefNull => 1,
        O::MemorySize | O::MemoryGrow | O::MemoryFill => 1,
        O::TableInit | O::TableCopy | O::MemoryInit | O::MemoryCopy => 2,
        // Loads/stores take optional `offset=`/`align=` atoms, consumed by their emitter.
        _ => 0,
    }
}

/// Emit `call_indirect`, in either form:
/// `call_indirect $table? (type $t)? (param …)* (result …)*` — plus, when `folded`, the
/// operand sub-expressions that follow (emitted before the opcode).
///
/// Returns the index just past the instruction's forms.
fn emit_call_indirect(ctx: &mut Ctx, l: &[Sexpr], start: usize, folded: bool) -> Result<usize> {
    let mut j = start;
    // An optional leading table index or `$name` (multi-table).
    let mut table = 0u32;
    if let Some(s) = l.get(j) {
        if s.as_atom().is_some_and(is_index_or_id) {
            table = resolve_by_name(ctx.table_names, s)?;
            j += 1;
        }
    }
    // The type annotation: `(type $t)` and/or an inline signature.
    let mut type_ref = None;
    let mut sig = Sig::default();
    while let Some(s) = l.get(j) {
        match s.keyword() {
            Some("type") => {
                type_ref = Some(resolve_by_name(ctx.type_names, nth(want_list(s)?, 1)?)?);
                j += 1;
            }
            Some("param" | "result") => {
                let one = parse_sig(core::slice::from_ref(s), ctx.type_names, None)?;
                sig.params.extend(one.params);
                sig.results.extend(one.results);
                j += 1;
            }
            _ => break,
        }
    }
    // An inline signature interns into the shared type table — bodies are encoded before
    // any section is written, so appending here is safe.
    let ti = type_ref.unwrap_or_else(|| intern_sig(ctx.sigs, sig));
    if folded {
        for k in j..l.len() {
            emit_one(ctx, l, k)?;
        }
        j = l.len();
    }
    ctx.out.push(0x11);
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
    if ctx.labels.len() >= MAX_CTRL_DEPTH {
        return Err(Error::NestingTooDeep);
    }
    ctx.labels.push(label);
    emit_seq(ctx, &l[j..])?;
    ctx.labels.pop();
    ctx.out.push(0x0b);
    Ok(())
}

fn emit_folded_if(ctx: &mut Ctx, l: &[Sexpr]) -> Result<()> {
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
    ctx.out.push(0x04);
    emit_block_type(ctx, bt)?;
    if ctx.labels.len() >= MAX_CTRL_DEPTH {
        return Err(Error::NestingTooDeep);
    }
    ctx.labels.push(label);
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
    let op = O::from_text_name(name).ok_or(Error::UnknownInstr)?;
    match op {
        O::Block | O::Loop | O::If => {
            let mut j = i + 1;
            let label = opt_name(items, &mut j);
            let bt = parse_block_type(ctx, items, &mut j)?;
            ctx.out.push(op as u8);
            emit_block_type(ctx, bt)?;
            if ctx.labels.len() >= MAX_CTRL_DEPTH {
                return Err(Error::NestingTooDeep);
            }
            ctx.labels.push(label);
            Ok(j)
        }
        O::Else => {
            ctx.out.push(0x05);
            Ok(i + 1)
        }
        O::End => {
            ctx.out.push(0x0b);
            ctx.labels.pop();
            Ok(i + 1)
        }
        O::CallIndirect => emit_call_indirect(ctx, items, i + 1, false),
        O::BrTable => {
            ctx.out.push(0x0e);
            let mut j = i + 1;
            let mut labels: Vec<u32> = Vec::new();
            while let Some(s) = items.get(j) {
                if s.as_atom().is_some_and(is_index_or_id) {
                    labels.push(ctx.resolve_label(s)?);
                    j += 1;
                } else {
                    break;
                }
            }
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
        _ => {
            let n = immediate_arity(op);
            let end = (i + 1 + n).min(items.len());
            emit_op_with_immediates(ctx, op, &items[..end], i + 1)?;
            // Loads/stores may carry `offset=`/`align=` atoms beyond the fixed arity.
            let mut j = end;
            if takes_memarg(op) {
                while items
                    .get(j)
                    .and_then(Sexpr::as_atom)
                    .is_some_and(|a| a.starts_with("offset=") || a.starts_with("align="))
                {
                    j += 1;
                }
            }
            Ok(j)
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
        _ => ctx.out.push(op as u8),
    }

    match op {
        O::I32Const => {
            let v = parse_i64_str(want_atom(imm(0)?)?)?;
            sleb(&mut ctx.out, i64::from(v as i32));
        }
        O::I64Const => sleb(&mut ctx.out, parse_i64_str(want_atom(imm(0)?)?)?),
        O::F32Const => {
            let bits = parse_f32_bits(want_atom(imm(0)?)?).ok_or(Error::BadNumber)?;
            ctx.out.extend_from_slice(&bits.to_le_bytes());
        }
        O::F64Const => {
            let bits = parse_f64_bits(want_atom(imm(0)?)?).ok_or(Error::BadNumber)?;
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
        O::Call | O::RefFunc => {
            uleb(
                &mut ctx.out,
                u64::from(resolve_by_name(ctx.func_names, imm(0)?)?),
            );
        }
        O::Br | O::BrIf | O::Rethrow => {
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
            uleb(
                &mut ctx.out,
                u64::from(resolve_by_name(ctx.data_names, imm(0)?)?),
            );
        }
        O::TableInit => {
            // `table.init $table $elem` — the binary order is elem then table.
            let a = resolve_by_name(ctx.table_names, imm(0)?)?;
            let e = resolve_by_name(ctx.elem_names, imm(1)?)?;
            uleb(&mut ctx.out, u64::from(e));
            uleb(&mut ctx.out, u64::from(a));
        }
        O::TableCopy => {
            let d = resolve_by_name(ctx.table_names, imm(0)?)?;
            let s = resolve_by_name(ctx.table_names, imm(1)?)?;
            uleb(&mut ctx.out, u64::from(d));
            uleb(&mut ctx.out, u64::from(s));
        }
        O::MemoryInit => {
            let d = resolve_by_name(ctx.data_names, imm(0)?)?;
            uleb(&mut ctx.out, u64::from(d));
            let m = match items.get(start + 1) {
                Some(s) => resolve_by_name(ctx.mem_names, s)?,
                None => 0,
            };
            uleb(&mut ctx.out, u64::from(m));
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
        O::RefNull => {
            let ht = want_atom(imm(0)?)?;
            let code: i64 = match ht {
                "func" | "nofunc" => -0x10,
                "extern" | "noextern" => -0x11,
                "any" => -0x12,
                "eq" => -0x13,
                "i31" => -0x14,
                "struct" => -0x15,
                "array" => -0x16,
                "exn" | "noexn" => -0x17,
                "none" => -0x0f,
                _ => return Err(Error::BadImmediate),
            };
            sleb(&mut ctx.out, code);
        }
        _ => {} // no immediates
    }

    // Loads/stores: an `align=`/`offset=` pair follows the opcode.
    if takes_memarg(op) {
        let mut align: Option<u32> = None;
        let mut offset: u64 = 0;
        let mut mem: u32 = 0;
        let mut k = start;
        // An optional leading memory index (multi-memory).
        if let Some(a) = items.get(k).and_then(Sexpr::as_atom) {
            if !a.starts_with("offset=") && !a.starts_with("align=") {
                mem = resolve_by_name(ctx.mem_names, &items[k])?;
                k += 1;
            }
        }
        while let Some(a) = items.get(k).and_then(Sexpr::as_atom) {
            if let Some(v) = a.strip_prefix("offset=") {
                offset = parse_u64_str(v)?;
            } else if let Some(v) = a.strip_prefix("align=") {
                let bytes = parse_u64_str(v)?;
                if !bytes.is_power_of_two() {
                    return Err(Error::BadImmediate);
                }
                align = Some(bytes.trailing_zeros());
            } else {
                break;
            }
            k += 1;
        }
        let a = align.unwrap_or_else(|| crate::opcode::natural_align_log2(op));
        if mem == 0 {
            uleb(&mut ctx.out, u64::from(a));
        } else {
            uleb(&mut ctx.out, u64::from(a | 0x40));
            uleb(&mut ctx.out, u64::from(mem));
        }
        uleb(&mut ctx.out, offset);
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
    let v = parse_i64_str(want_atom(s)?)?;
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
                let bits = parse_f32_bits(a).ok_or(Error::BadNumber)?;
                out[k * 4..k * 4 + 4].copy_from_slice(&bits.to_le_bytes());
            }
            "f64x2" => {
                let bits = parse_f64_bits(a).ok_or(Error::BadNumber)?;
                out[k * 8..k * 8 + 8].copy_from_slice(&bits.to_le_bytes());
            }
            "i8x16" => out[k] = parse_i64_str(a)? as u8,
            "i16x8" => {
                let v = parse_i64_str(a)? as u16;
                out[k * 2..k * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
            "i32x4" => {
                let v = parse_i64_str(a)? as u32;
                out[k * 4..k * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            _ => unreachable!(),
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
/// Only `offset=`/`align=` atoms and index-like atoms are taken. Stopping at anything else
/// matters in the FLAT form, where `items` is the whole sibling instruction sequence: a
/// following mnemonic (`drop`, `i32.const`) is not index-like and must NOT be swallowed as
/// a memory or lane index.
fn parse_simd_memarg(
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
            let r = parse_simd_memarg(ctx, items, j, want_lane, align)?;
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
        }
        ctx.out.push(0xfe);
        uleb(&mut ctx.out, u64::from(sub));
        ctx.out.push(0x00);
        return Ok(j);
    }
    let (mut j, align, mem, offset, _) = parse_simd_memarg(ctx, items, j, false, natural)?;
    if folded {
        while j < items.len() {
            j = emit_one(ctx, items, j)?;
        }
    }
    ctx.out.push(0xfe);
    uleb(&mut ctx.out, u64::from(sub));
    emit_memarg_bytes(&mut ctx.out, align, mem, offset);
    Ok(j)
}

/// A block signature: either a type index or an inline result list.
#[derive(Debug, Clone)]
enum BlockTy {
    Empty,
    Val(V),
    TypeIndex(u32),
}

/// Parse a block type from `items[*j..]`, advancing `j` past it.
fn parse_block_type(ctx: &mut Ctx, items: &[Sexpr], j: &mut usize) -> Result<BlockTy> {
    let mut sig = Sig::default();
    let mut type_ref = None;
    while let Some(s) = items.get(*j) {
        match s.keyword() {
            Some("type") => {
                type_ref = Some(resolve_by_name(ctx.type_names, nth(want_list(s)?, 1)?)?);
                *j += 1;
            }
            Some("param" | "result") => {
                let one = parse_sig(core::slice::from_ref(s), ctx.type_names, None)?;
                sig.params.extend(one.params);
                sig.results.extend(one.results);
                *j += 1;
            }
            _ => break,
        }
    }
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
    Ok(BlockTy::TypeIndex(intern_sig(ctx.sigs, sig)))
}

fn emit_block_type(ctx: &mut Ctx, bt: BlockTy) -> Result<()> {
    match bt {
        BlockTy::Empty => ctx.out.push(0x40),
        BlockTy::Val(v) => emit_val_type(&mut ctx.out, v)?,
        BlockTy::TypeIndex(ti) => sleb(&mut ctx.out, i64::from(ti)),
    }
    Ok(())
}

fn emit_elem_segment(c: &mut Vec<u8>, e: &ElemDef, b: &mut ModuleBuild) -> Result<()> {
    let uses_exprs = e.elem_type != V::FUNCREF;
    match (&e.offset, e.declarative) {
        (Some(off), _) => {
            if e.table_index == 0 && !uses_exprs {
                uleb(c, 0); // active, table 0, funcref, ref.func entries
                emit_const_expr(c, off, b)?;
            } else {
                uleb(c, 2); // active with an explicit table index
                uleb(c, u64::from(e.table_index));
                emit_const_expr(c, off, b)?;
                emit_val_type(c, e.elem_type)?;
            }
        }
        (None, true) => {
            uleb(c, 3); // declarative
            c.push(0x00); // elemkind: funcref
        }
        (None, false) => {
            uleb(c, 1); // passive
            c.push(0x00); // elemkind: funcref
        }
    }
    uleb(c, e.items.len() as u64);
    for item in &e.items {
        if e.offset.is_some() && e.table_index == 0 && !uses_exprs {
            // The `ref.func $f` shorthand encodes as a bare function index.
            let l = want_list(&item[0])?;
            uleb(c, u64::from(resolve_by_name(&b.func_names, nth(l, 1)?)?));
        } else {
            let l = want_list(&item[0])?;
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

    #[test]
    fn assembles_the_module_binary_form() {
        let m = asm(r#"(module binary "\00asm\01\00\00\00")"#).unwrap();
        assert_eq!(m, [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
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
        assert_eq!(p("anyfunc").unwrap(), V::FUNCREF); // pre-standard spelling
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
        assert_eq!(asm(named).unwrap(), asm(numbered).unwrap());
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
            parse_f64_bits("0x0123456789ABCDEFabcdef").unwrap(),
            0x44f2_3456_789a_bcdf
        );
        assert_eq!(
            parse_f64_bits("0x0123456789ABCDEFa").unwrap(),
            0x43b2_3456_789a_bcdf
        );
        assert_eq!(
            parse_f64_bits("0x1.23456789abcdep+81").unwrap(),
            0x4502_3456_789a_bcde
        );
    }

    #[test]
    fn parses_ordinary_float_literals() {
        assert_eq!(parse_f64_bits("1.5").unwrap(), 1.5f64.to_bits());
        assert_eq!(parse_f64_bits("-0.0").unwrap(), (-0.0f64).to_bits());
        assert_eq!(parse_f64_bits("0").unwrap(), 0.0f64.to_bits());
        assert_eq!(parse_f32_bits("1.5").unwrap(), 1.5f32.to_bits());
        assert_eq!(parse_f32_bits("-2.5e3").unwrap(), (-2500.0f32).to_bits());
        assert_eq!(parse_f64_bits("inf").unwrap(), f64::INFINITY.to_bits());
        assert_eq!(
            parse_f64_bits("-inf").unwrap(),
            f64::NEG_INFINITY.to_bits()
        );
        // The exponent-less hex form the text format also allows.
        assert_eq!(parse_f64_bits("0x10").unwrap(), 16.0f64.to_bits());
        assert_eq!(parse_f64_bits("0x1.8p+1").unwrap(), 3.0f64.to_bits());
        assert!(parse_f64_bits("0x").is_none());
        assert!(parse_f64_bits("1.2.3").is_none());
    }

    #[test]
    fn parses_the_wasm_nan_spellings() {
        let canonical = parse_f64_bits("nan:canonical").unwrap();
        assert!(f64::from_bits(canonical).is_nan());
        assert_eq!(canonical, f64::NAN.to_bits() & !(1u64 << 63));
        let arith = parse_f64_bits("nan:arithmetic").unwrap();
        assert!(f64::from_bits(arith).is_nan());
        // An explicit payload lands in the mantissa.
        let payload = parse_f64_bits("nan:0x4000000000000").unwrap();
        assert_eq!(payload & 0xf_ffff_ffff_ffff, 0x4000000000000);
        assert!(f64::from_bits(payload).is_nan());
        // The sign is honoured.
        assert_ne!(parse_f64_bits("-nan:canonical").unwrap() >> 63, 0);
        assert!(f32::from_bits(parse_f32_bits("nan").unwrap()).is_nan());
    }

    #[test]
    fn subnormal_hex_floats_round_rather_than_flush() {
        // 0.75 ULP — just ABOVE half the smallest f64 subnormal — must round up to it
        // rather than flush to zero. This is the case a two-stage rounding (clamp the
        // kept-bit count, then scale) gets wrong by discarding the sticky bit.
        assert_eq!(parse_f64_bits("0x1.8p-1075").unwrap(), 1);
        // Exactly half ties to even → zero.
        assert_eq!(parse_f64_bits("0x1p-1075").unwrap(), 0);
        // The smallest subnormal itself.
        assert_eq!(parse_f64_bits("0x1p-1074").unwrap(), 1);
        // Exactly halfway BETWEEN subnormals 1 and 2 ties to even → 2.
        assert_eq!(parse_f64_bits("0x1.8p-1074").unwrap(), 2);
        // Rounding up out of the subnormal range lands on the smallest normal.
        assert_eq!(
            parse_f64_bits("0x1.fffffffffffffp-1023").unwrap(),
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

    #[test]
    fn runs_a_hex_float_constant() {
        let src = r#"(module
            (func (export "f") (result f32) (f32.const 0x1.8p+1)))"#;
        let r = run(src, "f", &[]);
        assert_eq!(crate::interp::as_f32(r[0]), 3.0);
    }
}
