//! Core WebAssembly binary-format constants, section identifiers, value types, and
//! the decoder error set. Dependency-free (`core` only) so it compiles for every
//! target, including `wasm32`-freestanding.
//!
//! Ported from wazmrt `src/types.zig` (T1). **Invariant (do not drift):** [`ValType`]
//! is a `u32` **newtype** with concrete typed refs bit-packed in the high bits — NOT a
//! plain enum; every accessor is a pure bit op. See `cmem/design-decisions.md`.

use core::fmt;

/// The 4-byte magic that opens every WebAssembly binary: `\0asm`.
pub const MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6d];

/// The only binary-format version wasmrt currently decodes.
pub const SUPPORTED_VERSION: u32 = 1;

/// A convenience alias for decode results.
pub type DecodeResult<T> = core::result::Result<T, DecodeError>;

/// Section identifiers as defined by the core WebAssembly spec (§5.5). Unknown ids
/// decode to `None` via [`SectionId::from_u8`] rather than crashing; callers validate
/// the range where it matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SectionId {
    Custom = 0,
    Type = 1,
    Import = 2,
    Function = 3,
    Table = 4,
    Memory = 5,
    Global = 6,
    Export = 7,
    Start = 8,
    Element = 9,
    Code = 10,
    Data = 11,
    DataCount = 12,
    /// Exception tags (EH proposal).
    Tag = 13,
}

impl SectionId {
    /// Highest identifier defined by the current spec.
    pub const MAX: u8 = 13;

    /// The known section id for `b`, or `None` if outside the defined range.
    #[must_use]
    pub const fn from_u8(b: u8) -> Option<SectionId> {
        Some(match b {
            0 => SectionId::Custom,
            1 => SectionId::Type,
            2 => SectionId::Import,
            3 => SectionId::Function,
            4 => SectionId::Table,
            5 => SectionId::Memory,
            6 => SectionId::Global,
            7 => SectionId::Export,
            8 => SectionId::Start,
            9 => SectionId::Element,
            10 => SectionId::Code,
            11 => SectionId::Data,
            12 => SectionId::DataCount,
            13 => SectionId::Tag,
            _ => return None,
        })
    }

    /// This section's position in the fixed order a module's sections must appear in (§5.5.2),
    /// with `Custom` reported as `None` because a custom section may appear anywhere, any number
    /// of times.
    ///
    /// **The order is NOT the id order**, which is the whole reason this is a table rather than a
    /// comparison. `DataCount` is id **12** but must appear *before* `Code` (id 10) — its entire
    /// purpose is to let `memory.init` decode without having read the data section — and `Tag`
    /// (id 13) belongs between `Memory` and `Global`, because it was added to the middle of the
    /// list by the exception-handling proposal. Comparing raw ids would accept both in the wrong
    /// place and reject them in the right one.
    #[must_use]
    pub const fn order(self) -> Option<u8> {
        Some(match self {
            SectionId::Custom => return None,
            SectionId::Type => 0,
            SectionId::Import => 1,
            SectionId::Function => 2,
            SectionId::Table => 3,
            SectionId::Memory => 4,
            SectionId::Tag => 5,
            SectionId::Global => 6,
            SectionId::Export => 7,
            SectionId::Start => 8,
            SectionId::Element => 9,
            SectionId::DataCount => 10,
            SectionId::Code => 11,
            SectionId::Data => 12,
        })
    }
}

/// The kind of an import or export, from the binary import/export descriptor byte
/// (§5.5.10 / §5.5.5). NOTE: this is the *binary* ordering (func=0, table=1, mem=2,
/// global=3), which differs from the wasm-c-api `wasm_externkind_t` ordering — the C
/// ABI layer maps between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExternKind {
    Func = 0x00,
    Table = 0x01,
    Memory = 0x02,
    Global = 0x03,
    /// Exception tag (EH proposal).
    Tag = 0x04,
}

impl ExternKind {
    /// The extern kind for `b`, or `None` for an unknown kind byte.
    #[must_use]
    pub const fn from_u8(b: u8) -> Option<ExternKind> {
        Some(match b {
            0x00 => ExternKind::Func,
            0x01 => ExternKind::Table,
            0x02 => ExternKind::Memory,
            0x03 => ExternKind::Global,
            0x04 => ExternKind::Tag,
            _ => return None,
        })
    }
}

/// The heap type a reference points at, ignoring nullability — the axis reference
/// subtyping is decided on ([`RefHeap::is_subtype_of`]). Non-reference value types have
/// no heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefHeap {
    Func,
    Extern,
    Any,
    Eq,
    I31,
    Struct,
    Array,
    None,
    /// Exception references — its own hierarchy (EH proposal).
    Exn,
}

impl RefHeap {
    /// The value type for this heap head at the given nullability (the collapsed
    /// reference representation — concrete refs share their head).
    #[must_use]
    pub const fn val_type(self, is_nullable: bool) -> ValType {
        match self {
            RefHeap::Func => bool_pick(is_nullable, ValType::FUNCREF, ValType::FUNCREF_NN),
            RefHeap::Extern => bool_pick(is_nullable, ValType::EXTERNREF, ValType::EXTERNREF_NN),
            RefHeap::Any => bool_pick(is_nullable, ValType::ANYREF, ValType::ANYREF_NN),
            RefHeap::Eq => bool_pick(is_nullable, ValType::EQREF, ValType::EQREF_NN),
            RefHeap::I31 => bool_pick(is_nullable, ValType::I31REF, ValType::I31REF_NN),
            RefHeap::Struct => bool_pick(is_nullable, ValType::STRUCTREF, ValType::STRUCTREF_NN),
            RefHeap::Array => bool_pick(is_nullable, ValType::ARRAYREF, ValType::ARRAYREF_NN),
            RefHeap::None => bool_pick(is_nullable, ValType::NULLREF, ValType::NULLREF_NN),
            RefHeap::Exn => bool_pick(is_nullable, ValType::EXNREF, ValType::EXNREF_NN),
        }
    }

    /// The top of this head's hierarchy: `Any` for the internal GC family, else
    /// `Func` / `Extern` / `Exn`.
    #[must_use]
    pub const fn top(self) -> RefHeap {
        match self {
            RefHeap::Func => RefHeap::Func,
            RefHeap::Extern => RefHeap::Extern,
            RefHeap::Exn => RefHeap::Exn,
            _ => RefHeap::Any,
        }
    }

    /// Is heap `self` a subtype of heap `other` in the WasmGC hierarchy? (wazmrt
    /// `RefHeap.sub`.) The `func` and `extern` hierarchies are disjoint from the `any`
    /// family; within `any`, i31/struct/array <: eq <: any, and `none` is the bottom.
    #[must_use]
    pub fn is_subtype_of(self, other: RefHeap) -> bool {
        use RefHeap::*;
        if self == other {
            return true;
        }
        match self {
            None => matches!(other, I31 | Struct | Array | Eq | Any),
            I31 | Struct | Array => matches!(other, Eq | Any),
            Eq => other == Any,
            _ => false, // func/extern/exn/any have no proper supertype here
        }
    }
}

/// `const fn`-friendly ternary (there is no `if`-expression in older const contexts we
/// want to stay compatible with, and it keeps [`RefHeap::val_type`] a pure table).
const fn bool_pick(cond: bool, when_true: ValType, when_false: ValType) -> ValType {
    if cond { when_true } else { when_false }
}

/// A WebAssembly value type (§5.3.1).
///
/// Numeric and abstract-reference types keep their single binary byte (`< 0x100`). A
/// **concrete typed reference** `(ref null? $t)` (GC) is encoded in the high bits — bit
/// 31 marks concrete, bit 30 nullable, bits 28–29 the family (func/struct/array), bits
/// 0–27 the type index — so `ValType` stays a single comparable scalar. This lets
/// `(ref $t)` flow through params/fields/locals with its exact type instead of
/// collapsing to a family head.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValType(u32);

// --- Concrete typed-reference encoding (high bits of the u32) --------------------
const CONCRETE_BIT: u32 = 0x8000_0000;
const NULLABLE_BIT: u32 = 0x4000_0000;
const KIND_SHIFT: u32 = 28;
const KIND_MASK: u32 = 0x3 << KIND_SHIFT;
const INDEX_MASK: u32 = 0x0fff_ffff; // 28 bits — up to ~268M types

impl ValType {
    // Numeric value types.
    pub const I32: ValType = ValType(0x7f);
    pub const I64: ValType = ValType(0x7e);
    pub const F32: ValType = ValType(0x7d);
    pub const F64: ValType = ValType(0x7c);
    pub const V128: ValType = ValType(0x7b);

    // Abstract nullable heap-type shorthands, encoded by their real valtype bytes.
    pub const FUNCREF: ValType = ValType(0x70);
    pub const EXTERNREF: ValType = ValType(0x6f);
    pub const ANYREF: ValType = ValType(0x6e);
    pub const EQREF: ValType = ValType(0x6d);
    pub const I31REF: ValType = ValType(0x6c);
    pub const STRUCTREF: ValType = ValType(0x6b);
    pub const ARRAYREF: ValType = ValType(0x6a);
    /// `(ref null exn)` — exception references (EH proposal).
    pub const EXNREF: ValType = ValType(0x69);
    /// `(ref null none)` — bottom of the `any` hierarchy.
    pub const NULLREF: ValType = ValType(0x71);

    // Non-nullable reference types (function-references + GC proposals). Synthetic
    // internal tags in an otherwise-unused valtype-byte range — our assembler/decoder
    // round-trip them, and an external binary's `0x64 ht` maps here.
    pub const FUNCREF_NN: ValType = ValType(0x68);
    pub const EXTERNREF_NN: ValType = ValType(0x67);
    pub const ANYREF_NN: ValType = ValType(0x66);
    pub const EQREF_NN: ValType = ValType(0x65);
    pub const I31REF_NN: ValType = ValType(0x62);
    pub const STRUCTREF_NN: ValType = ValType(0x61);
    pub const ARRAYREF_NN: ValType = ValType(0x59);
    /// `(ref none)` — uninhabited but syntactically valid.
    pub const NULLREF_NN: ValType = ValType(0x58);
    /// `(ref exn)` — non-null exception reference.
    pub const EXNREF_NN: ValType = ValType(0x57);

    /// Largest type index a concrete `(ref $t)` can carry. [`ValType::concrete_ref`]
    /// masks with the 28-bit index, so anything above this **silently truncates** — and
    /// a large index can truncate to a small *valid* one, which is type confusion, not
    /// merely a wrong number. Callers must reject above this before constructing.
    pub const MAX_CONCRETE_INDEX: u32 = INDEX_MASK;

    /// Wrap a raw `u32` bit pattern (the decode representation).
    #[must_use]
    pub const fn from_bits(bits: u32) -> ValType {
        ValType(bits)
    }

    /// The raw `u32` bit pattern.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Build a concrete typed reference `(ref null? $ti)` for family `kind` (must be
    /// `Func`/`Struct`/`Array`).
    ///
    /// # Panics
    /// Panics if `kind` is not one of the three concrete families.
    #[must_use]
    pub fn concrete_ref(is_nullable: bool, kind: RefHeap, ti: u32) -> ValType {
        let k: u32 = match kind {
            RefHeap::Func => 0,
            RefHeap::Struct => 1,
            RefHeap::Array => 2,
            _ => unreachable!("concrete_ref: family must be func/struct/array"),
        };
        ValType(
            CONCRETE_BIT
                | (if is_nullable { NULLABLE_BIT } else { 0 })
                | (k << KIND_SHIFT)
                | (ti & INDEX_MASK),
        )
    }

    /// True if this is a concrete typed reference (carries a type index).
    #[must_use]
    pub const fn is_concrete(self) -> bool {
        self.0 & CONCRETE_BIT != 0
    }

    /// The type index of a concrete reference (only meaningful when [`is_concrete`]).
    ///
    /// [`is_concrete`]: ValType::is_concrete
    #[must_use]
    pub const fn concrete_index(self) -> u32 {
        self.0 & INDEX_MASK
    }

    /// True only for the defined value types (rejects garbage bit patterns).
    #[must_use]
    pub fn is_valid(self) -> bool {
        if self.is_concrete() {
            return true;
        }
        matches!(
            self,
            ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64 | ValType::V128
        ) || self.is_ref()
    }

    /// True for any reference type (nullable or not).
    #[must_use]
    pub fn is_ref(self) -> bool {
        if self.is_concrete() {
            return true;
        }
        matches!(
            self.0,
            // nullable
            0x70 | 0x6f | 0x6e | 0x6d | 0x6c | 0x6b | 0x6a | 0x69 | 0x71
            // non-null
            | 0x68 | 0x67 | 0x66 | 0x65 | 0x62 | 0x61 | 0x59 | 0x58 | 0x57
        )
    }

    /// True for a non-nullable reference (a non-defaultable local type).
    #[must_use]
    pub fn is_non_null_ref(self) -> bool {
        if self.is_concrete() {
            return self.0 & NULLABLE_BIT == 0;
        }
        matches!(
            self.0,
            0x68 | 0x67 | 0x66 | 0x65 | 0x62 | 0x61 | 0x59 | 0x58 | 0x57
        )
    }

    /// The NON-nullable form of a reference type (nullable → non-null; others as-is).
    /// The inverse of [`nullable`](ValType::nullable); needed by `ref.as_non_null` and
    /// `br_on_null`.
    #[must_use]
    pub fn non_null(self) -> ValType {
        if self.is_concrete() {
            return ValType(self.0 & !NULLABLE_BIT);
        }
        match self.0 {
            0x70 => ValType::FUNCREF_NN,
            0x6f => ValType::EXTERNREF_NN,
            0x6e => ValType::ANYREF_NN,
            0x6d => ValType::EQREF_NN,
            0x6c => ValType::I31REF_NN,
            0x6b => ValType::STRUCTREF_NN,
            0x6a => ValType::ARRAYREF_NN,
            0x69 => ValType::EXNREF_NN,
            0x71 => ValType::NULLREF_NN,
            _ => self,
        }
    }

    /// The nullable form of a reference type (non-null → nullable; others as-is).
    #[must_use]
    pub fn nullable(self) -> ValType {
        if self.is_concrete() {
            return ValType(self.0 | NULLABLE_BIT);
        }
        match self.0 {
            0x68 => ValType::FUNCREF,
            0x67 => ValType::EXTERNREF,
            0x66 => ValType::ANYREF,
            0x65 => ValType::EQREF,
            0x62 => ValType::I31REF,
            0x61 => ValType::STRUCTREF,
            0x59 => ValType::ARRAYREF,
            0x57 => ValType::EXNREF,
            0x58 => ValType::NULLREF,
            _ => self,
        }
    }

    /// The heap type of a reference value type (only meaningful when [`is_ref`]). A
    /// concrete ref reads its family from the kind bits.
    ///
    /// [`is_ref`]: ValType::is_ref
    ///
    /// # Panics
    /// Panics if called on a non-reference value type.
    #[must_use]
    pub fn ref_heap(self) -> RefHeap {
        if self.is_concrete() {
            return match (self.0 & KIND_MASK) >> KIND_SHIFT {
                0 => RefHeap::Func,
                1 => RefHeap::Struct,
                2 => RefHeap::Array,
                _ => unreachable!("concrete ref kind bits out of range"),
            };
        }
        match self.0 {
            0x70 | 0x68 => RefHeap::Func,
            0x6f | 0x67 => RefHeap::Extern,
            0x6e | 0x66 => RefHeap::Any,
            0x6d | 0x65 => RefHeap::Eq,
            0x6c | 0x62 => RefHeap::I31,
            0x6b | 0x61 => RefHeap::Struct,
            0x6a | 0x59 => RefHeap::Array,
            0x69 | 0x57 => RefHeap::Exn,
            0x71 | 0x58 => RefHeap::None,
            _ => unreachable!("ref_heap on a non-reference value type"),
        }
    }
}

impl fmt::Debug for ValType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_concrete() {
            let nn = if self.0 & NULLABLE_BIT == 0 { " nn" } else { "" };
            return write!(f, "(ref{nn} {:?} #{})", self.ref_heap(), self.concrete_index());
        }
        let name = match self.0 {
            0x7f => "i32",
            0x7e => "i64",
            0x7d => "f32",
            0x7c => "f64",
            0x7b => "v128",
            0x70 => "funcref",
            0x6f => "externref",
            0x6e => "anyref",
            0x6d => "eqref",
            0x6c => "i31ref",
            0x6b => "structref",
            0x6a => "arrayref",
            0x69 => "exnref",
            0x71 => "nullref",
            0x68 => "funcref_nn",
            0x67 => "externref_nn",
            0x66 => "anyref_nn",
            0x65 => "eqref_nn",
            0x62 => "i31ref_nn",
            0x61 => "structref_nn",
            0x59 => "arrayref_nn",
            0x58 => "nullref_nn",
            0x57 => "exnref_nn",
            _ => return write!(f, "ValType(0x{:x})", self.0),
        };
        f.write_str(name)
    }
}

/// Errors that can arise while decoding a WebAssembly binary. Ported from wazmrt
/// `types.DecodeError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Ran out of input before a structure was complete.
    UnexpectedEof,
    /// The leading 4 bytes were not the WebAssembly magic.
    BadMagic,
    /// The binary declares a version wasmrt does not support.
    UnsupportedVersion,
    /// A LEB128-encoded integer did not fit in its target type (over-long or too-large).
    LebOverflow,
    /// A section declared an identifier outside the defined range.
    InvalidSectionId,
    /// A function type did not begin with the `0x60` form byte.
    BadFuncType,
    /// A type-section entry was not a valid composite type, or a GC sub type declared
    /// more than one supertype.
    BadType,
    /// An import/export descriptor used an unknown kind byte.
    UnknownExternKind,
    /// A type/function/extern index referred outside the decoded space.
    IndexOutOfRange,
    /// A single-byte flag (global mutability, limits flag) held a reserved value.
    MalformedFlag,
    /// A value-type byte was not one of the defined value types.
    BadValType,
    /// A name (import module/field, export, custom-section id) was not valid UTF-8.
    InvalidUtf8,
    /// The data-count section disagreed with the number of data segments.
    DataCountMismatch,
    /// An instruction opcode wasmrt does not decode (or a raw internal-tag byte).
    UnsupportedOpcode,
    /// A non-custom section appeared twice, or out of the order §5.5.2 fixes. Both read the same
    /// way to a decoder: it has already finished with that section, so those bytes are unexpected
    /// content — which is exactly how the spec suite words it. Silently taking the *second*
    /// occurrence, as a decoder without this check does, is the worse outcome: a repeated function
    /// section quietly changes what the module is.
    SectionOrder,
    /// A section's declared size does not match the bytes its contents occupy. Leftover bytes
    /// inside a section are never harmless — they mean the producer and the decoder disagree about
    /// where the section ends.
    SectionSizeMismatch,
    /// The function and code sections declare different numbers of functions (§5.5.13). A
    /// structural disagreement between two sections, so it is *malformed*, not invalid.
    FuncCodeCountMismatch,
    /// A body uses `memory.init` or `data.drop` but the module has no data-count section. The
    /// count is what makes those instructions decodable without reading the data section, so its
    /// absence is a decode-stage failure (bulk-memory, §5.5.16).
    DataCountRequired,
    /// An expression's bytes ran out before its terminating `end` (§5.4.9).
    MissingEnd,
    /// A function's declared locals sum to more than 2^32−1, so the count cannot be represented.
    /// A *malformed* encoding, distinct from the validator's `TooManyLocals` resource ceiling:
    /// this says "these bytes cannot mean anything", that one says "we decline to allocate it".
    TooManyLocals,
    /// An `end` (or a legacy `delegate`) closed an expression that was already complete, or
    /// instructions followed the expression's terminating `end` (§5.4.9: `expr ::= instr* 0x0B`).
    ///
    /// The mirror of [`DecodeError::MissingEnd`]. Both are *malformed*: an expression whose
    /// control structures do not balance is not a program the validator gets to have an opinion
    /// about, and reporting it as the validator's `ControlUnderflow` is the right verdict at the
    /// wrong stage (`binary.wast`).
    UnbalancedEnd,
    /// A memarg's flags field has a bit set above the multi-memory flag (`0x40`).
    ///
    /// The field holds an alignment **exponent** in the low bits plus `0x40` to say a memory index
    /// follows; nothing else is defined, so `align="2**128"` is not a large alignment — it is a
    /// byte sequence that means nothing. ⚠️ Reading it as an alignment and letting the *validator*
    /// say "larger than natural" is the right verdict at the wrong STAGE, which
    /// `align.wast` distinguishes: those two cases are `assert_malformed`, not `assert_invalid`.
    MalformedMemopFlags,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DecodeError::UnexpectedEof => "unexpected end of input",
            DecodeError::BadMagic => "not a WebAssembly binary (bad magic)",
            DecodeError::UnsupportedVersion => "unsupported binary format version",
            DecodeError::LebOverflow => "malformed LEB128 (over-long or out of range)",
            DecodeError::InvalidSectionId => "section id outside the defined range",
            DecodeError::BadFuncType => "function type missing the 0x60 form byte",
            DecodeError::BadType => "invalid composite type entry",
            DecodeError::UnknownExternKind => "unknown import/export kind byte",
            DecodeError::IndexOutOfRange => "index outside the decoded space",
            DecodeError::MalformedFlag => "reserved value in a single-byte flag",
            DecodeError::BadValType => "byte is not a defined value type",
            DecodeError::InvalidUtf8 => "name is not valid UTF-8",
            DecodeError::DataCountMismatch => "data count disagrees with data segments",
            DecodeError::UnsupportedOpcode => "unsupported instruction opcode",
            DecodeError::SectionOrder => "section repeated or out of order",
            DecodeError::SectionSizeMismatch => "section size mismatch",
            DecodeError::FuncCodeCountMismatch => {
                "function and code section have inconsistent lengths"
            }
            DecodeError::DataCountRequired => "data count section required",
            DecodeError::TooManyLocals => "too many locals",
            DecodeError::MissingEnd => "unexpected end of section or function (missing END)",
            DecodeError::MalformedMemopFlags => "malformed memop flags",
            DecodeError::UnbalancedEnd => "END opcode outside a matching control structure",
        };
        f.write_str(s)
    }
}

impl core::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_id_range() {
        assert_eq!(SectionId::from_u8(0), Some(SectionId::Custom));
        assert_eq!(SectionId::from_u8(13), Some(SectionId::Tag));
        assert_eq!(SectionId::from_u8(14), None);
        assert_eq!(SectionId::MAX, 13);
    }

    #[test]
    fn extern_kind_binary_order() {
        assert_eq!(ExternKind::from_u8(0), Some(ExternKind::Func));
        assert_eq!(ExternKind::from_u8(3), Some(ExternKind::Global));
        assert_eq!(ExternKind::from_u8(4), Some(ExternKind::Tag));
        assert_eq!(ExternKind::from_u8(5), None);
    }

    #[test]
    fn numeric_types_are_valid_non_ref() {
        for t in [ValType::I32, ValType::I64, ValType::F32, ValType::F64, ValType::V128] {
            assert!(t.is_valid());
            assert!(!t.is_ref());
            assert!(!t.is_non_null_ref());
        }
    }

    #[test]
    fn abstract_ref_nullability_roundtrip() {
        // nullable → non-null → nullable is identity, and heads are preserved.
        let pairs = [
            (ValType::FUNCREF, ValType::FUNCREF_NN, RefHeap::Func),
            (ValType::EXTERNREF, ValType::EXTERNREF_NN, RefHeap::Extern),
            (ValType::ANYREF, ValType::ANYREF_NN, RefHeap::Any),
            (ValType::EQREF, ValType::EQREF_NN, RefHeap::Eq),
            (ValType::I31REF, ValType::I31REF_NN, RefHeap::I31),
            (ValType::STRUCTREF, ValType::STRUCTREF_NN, RefHeap::Struct),
            (ValType::ARRAYREF, ValType::ARRAYREF_NN, RefHeap::Array),
            (ValType::EXNREF, ValType::EXNREF_NN, RefHeap::Exn),
            (ValType::NULLREF, ValType::NULLREF_NN, RefHeap::None),
        ];
        for (nullable, nn, heap) in pairs {
            assert!(nullable.is_ref() && nn.is_ref());
            assert!(!nullable.is_non_null_ref());
            assert!(nn.is_non_null_ref());
            assert_eq!(nullable.non_null(), nn);
            assert_eq!(nn.nullable(), nullable);
            assert_eq!(nullable.non_null().nullable(), nullable);
            assert_eq!(nullable.ref_heap(), heap);
            assert_eq!(nn.ref_heap(), heap);
            assert_eq!(heap.val_type(true), nullable);
            assert_eq!(heap.val_type(false), nn);
        }
    }

    #[test]
    fn concrete_ref_bit_packing() {
        let t = ValType::concrete_ref(true, RefHeap::Struct, 42);
        assert!(t.is_concrete());
        assert!(t.is_ref());
        assert!(t.is_valid());
        assert!(!t.is_non_null_ref());
        assert_eq!(t.concrete_index(), 42);
        assert_eq!(t.ref_heap(), RefHeap::Struct);

        let nn = t.non_null();
        assert!(nn.is_concrete() && nn.is_non_null_ref());
        assert_eq!(nn.concrete_index(), 42);
        assert_eq!(nn.ref_heap(), RefHeap::Struct);
        assert_eq!(nn.nullable(), t);

        // The three families read back correctly.
        assert_eq!(ValType::concrete_ref(false, RefHeap::Func, 1).ref_heap(), RefHeap::Func);
        assert_eq!(ValType::concrete_ref(false, RefHeap::Array, 7).ref_heap(), RefHeap::Array);
    }

    #[test]
    fn concrete_index_truncates_at_28_bits() {
        // Above MAX_CONCRETE_INDEX the index masks down — callers must reject first.
        let t = ValType::concrete_ref(false, RefHeap::Func, ValType::MAX_CONCRETE_INDEX);
        assert_eq!(t.concrete_index(), ValType::MAX_CONCRETE_INDEX);
        let over = ValType::concrete_ref(false, RefHeap::Func, INDEX_MASK + 1);
        assert_eq!(over.concrete_index(), 0); // 2^28 masks to 0
    }

    #[test]
    fn gc_heap_subtyping() {
        use RefHeap::*;
        // i31/struct/array <: eq <: any; none is the bottom.
        assert!(I31.is_subtype_of(Eq) && I31.is_subtype_of(Any));
        assert!(Struct.is_subtype_of(Eq) && Array.is_subtype_of(Any));
        assert!(Eq.is_subtype_of(Any));
        assert!(
            None.is_subtype_of(I31)
                && None.is_subtype_of(Struct)
                && None.is_subtype_of(Array)
                && None.is_subtype_of(Eq)
                && None.is_subtype_of(Any)
        );
        assert!(Func.is_subtype_of(Func)); // reflexive
        // func/extern are disjoint from the any family.
        assert!(!Func.is_subtype_of(Any));
        assert!(!Extern.is_subtype_of(Any));
        assert!(!Any.is_subtype_of(Eq)); // no downward subtyping
        assert_eq!(I31.top(), Any);
        assert_eq!(Func.top(), Func);
        assert_eq!(Exn.top(), Exn);
    }

    #[test]
    fn is_valid_rejects_garbage() {
        assert!(!ValType::from_bits(0x00).is_valid());
        assert!(!ValType::from_bits(0x50).is_valid());
    }
}
