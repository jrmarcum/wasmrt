//! The shared WebAssembly instruction table and the byte-code → IR decoder.
//!
//! Ported from wazmrt `src/opcode.zig` (T2). This is the single opcode authority the
//! runtime is built around (Option A — see `cmem/architecture.md`): the same [`Op`] enum
//! and [`Instr`] IR feed validation and the switch interpreter, and (in reverse) the
//! assembler. [`decode_body`] turns a function body's raw bytes into a flat `Vec<Instr>`
//! with pre-parsed immediates.
//!
//! **Invariant (do not drift):** [`Op`] values `0x16`–`0x17` and `0xD7`–`0xFA` are
//! **internal tags**, not wire bytes — they name ops whose real encoding is `0xFB`/`0xFC` +
//! a LEB sub-opcode. [`decode_body`] rejects a raw byte in either range (accepting one would
//! execute a non-standard encoding as a real instruction). [`Op::from_u8`] maps only real
//! single-byte opcodes; the prefix families construct their `Op` variant directly.
//!
//! Unlike wazmrt, immediates that own data (`br_table` labels, typed-`select` types,
//! `try_table` catches) hold a `Vec`, so dropping the IR frees them — there is no manual
//! `freeBody`. Control-flow nesting and branch-target resolution belong to validation,
//! not here.

use alloc::vec::Vec;

use crate::reader::Reader;
use crate::types::{DecodeError, DecodeResult, ValType};

/// Defines the [`Op`] enum and [`Op::from_u8`] from one list, split into `wire`
/// (real single-byte opcodes, mapped by `from_u8`) and `internal` (tags whose wire form
/// is a `0xFB`/`0xFC` prefix + sub-opcode; enum-only, never produced by `from_u8`).
/// Each entry is `Variant = byte => "text.name"`. Keeping the **text name** in the same
/// table as the byte makes the binary and text spellings one authority, so the assembler's
/// reverse map cannot drift from the decoder (the discipline the oracle applies to its own
/// `decode_simd`-adjacent tables).
///
/// A name of `""` means the op has no text spelling of its own — the `0xFD`/`0xFE` family
/// tags, whose members are named by their sub-opcode tables. Empty names are excluded from
/// [`Op::from_text_name`].
macro_rules! define_ops {
    (
        wire { $($w:ident = $wv:literal => $wn:literal),* $(,)? }
        internal { $($i:ident = $iv:literal => $in:literal),* $(,)? }
    ) => {
        /// Every WebAssembly opcode, keyed by its binary byte (§5.4) for the single-byte
        /// forms and by an internal tag for the prefixed families.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Op {
            $($w = $wv,)*
            $($i = $iv,)*
        }

        impl Op {
            /// The op for a real **single-byte** opcode `b`, or `None` if `b` is not a
            /// defined single-byte op (prefix bytes `0xFB`–`0xFE` and the internal-tag
            /// ranges `0x16`–`0x17` / `0xD7`–`0xFA` return `None`).
            #[must_use]
            pub const fn from_u8(b: u8) -> Option<Op> {
                match b {
                    $($wv => Some(Op::$w),)*
                    _ => None,
                }
            }

            /// This op's WebAssembly **text-format** name (`i32.add`), or `""` for a family
            /// tag that has no text spelling of its own.
            #[must_use]
            pub const fn text_name(self) -> &'static str {
                match self {
                    $(Op::$w => $wn,)*
                    $(Op::$i => $in,)*
                }
            }

            /// The op a text-format instruction name denotes — the assembler's reverse map.
            /// Names that belong to a prefixed family (`0xFD` SIMD, `0xFE` atomics) are not
            /// here; those resolve through their own sub-opcode tables.
            #[must_use]
            pub fn from_text_name(name: &str) -> Option<Op> {
                match name {
                    $($wn if !$wn.is_empty() => Some(Op::$w),)*
                    $($in if !$in.is_empty() => Some(Op::$i),)*
                    _ => None,
                }
            }
        }
    };
}

define_ops! {
    wire {
        // Control + exception handling (both encodings) + typed-ref calls.
        Unreachable = 0x00 => "unreachable", Nop = 0x01 => "nop", Block = 0x02 => "block",
        Loop = 0x03 => "loop", If = 0x04 => "if", Else = 0x05 => "else",
        TryLegacy = 0x06 => "try", CatchLegacy = 0x07 => "catch", Throw = 0x08 => "throw",
        Rethrow = 0x09 => "rethrow", ThrowRef = 0x0a => "throw_ref",
        End = 0x0b => "end", Br = 0x0c => "br", BrIf = 0x0d => "br_if",
        BrTable = 0x0e => "br_table", Return = 0x0f => "return", Call = 0x10 => "call",
        CallIndirect = 0x11 => "call_indirect",
        // Tail calls (§2.4.8). `return_call` takes a funcidx like `call`; `return_call_indirect`
        // takes (typeidx, tableidx) like `call_indirect` — the immediates are identical to their
        // non-tail twins, only the frame discipline differs.
        ReturnCall = 0x12 => "return_call", ReturnCallIndirect = 0x13 => "return_call_indirect",
        CallRef = 0x14 => "call_ref",
        ReturnCallRef = 0x15 => "return_call_ref", Delegate = 0x18 => "delegate",
        CatchAll = 0x19 => "catch_all", TryTable = 0x1f => "try_table",
        // Parametric. `select` with an explicit result type assembles to `SelectT`, so the
        // typed form carries a sentinel name that no source text can spell.
        Drop = 0x1a => "drop", Select = 0x1b => "select", SelectT = 0x1c => "select.t",
        // Variable.
        LocalGet = 0x20 => "local.get", LocalSet = 0x21 => "local.set",
        LocalTee = 0x22 => "local.tee", GlobalGet = 0x23 => "global.get",
        GlobalSet = 0x24 => "global.set",
        // Table access.
        TableGet = 0x25 => "table.get", TableSet = 0x26 => "table.set",
        // Memory.
        I32Load = 0x28 => "i32.load", I64Load = 0x29 => "i64.load",
        F32Load = 0x2a => "f32.load", F64Load = 0x2b => "f64.load",
        I32Load8S = 0x2c => "i32.load8_s", I32Load8U = 0x2d => "i32.load8_u",
        I32Load16S = 0x2e => "i32.load16_s", I32Load16U = 0x2f => "i32.load16_u",
        I64Load8S = 0x30 => "i64.load8_s", I64Load8U = 0x31 => "i64.load8_u",
        I64Load16S = 0x32 => "i64.load16_s", I64Load16U = 0x33 => "i64.load16_u",
        I64Load32S = 0x34 => "i64.load32_s", I64Load32U = 0x35 => "i64.load32_u",
        I32Store = 0x36 => "i32.store", I64Store = 0x37 => "i64.store",
        F32Store = 0x38 => "f32.store", F64Store = 0x39 => "f64.store",
        I32Store8 = 0x3a => "i32.store8", I32Store16 = 0x3b => "i32.store16",
        I64Store8 = 0x3c => "i64.store8", I64Store16 = 0x3d => "i64.store16",
        I64Store32 = 0x3e => "i64.store32", MemorySize = 0x3f => "memory.size",
        MemoryGrow = 0x40 => "memory.grow",
        // Numeric constants.
        I32Const = 0x41 => "i32.const", I64Const = 0x42 => "i64.const",
        F32Const = 0x43 => "f32.const", F64Const = 0x44 => "f64.const",
        // Comparison — i32.
        I32Eqz = 0x45 => "i32.eqz", I32Eq = 0x46 => "i32.eq", I32Ne = 0x47 => "i32.ne",
        I32LtS = 0x48 => "i32.lt_s", I32LtU = 0x49 => "i32.lt_u", I32GtS = 0x4a => "i32.gt_s",
        I32GtU = 0x4b => "i32.gt_u", I32LeS = 0x4c => "i32.le_s", I32LeU = 0x4d => "i32.le_u",
        I32GeS = 0x4e => "i32.ge_s", I32GeU = 0x4f => "i32.ge_u",
        // Comparison — i64.
        I64Eqz = 0x50 => "i64.eqz", I64Eq = 0x51 => "i64.eq", I64Ne = 0x52 => "i64.ne",
        I64LtS = 0x53 => "i64.lt_s", I64LtU = 0x54 => "i64.lt_u", I64GtS = 0x55 => "i64.gt_s",
        I64GtU = 0x56 => "i64.gt_u", I64LeS = 0x57 => "i64.le_s", I64LeU = 0x58 => "i64.le_u",
        I64GeS = 0x59 => "i64.ge_s", I64GeU = 0x5a => "i64.ge_u",
        // Comparison — f32 / f64.
        F32Eq = 0x5b => "f32.eq", F32Ne = 0x5c => "f32.ne", F32Lt = 0x5d => "f32.lt",
        F32Gt = 0x5e => "f32.gt", F32Le = 0x5f => "f32.le", F32Ge = 0x60 => "f32.ge",
        F64Eq = 0x61 => "f64.eq", F64Ne = 0x62 => "f64.ne", F64Lt = 0x63 => "f64.lt",
        F64Gt = 0x64 => "f64.gt", F64Le = 0x65 => "f64.le", F64Ge = 0x66 => "f64.ge",
        // Numeric — i32.
        I32Clz = 0x67 => "i32.clz", I32Ctz = 0x68 => "i32.ctz", I32Popcnt = 0x69 => "i32.popcnt",
        I32Add = 0x6a => "i32.add", I32Sub = 0x6b => "i32.sub", I32Mul = 0x6c => "i32.mul",
        I32DivS = 0x6d => "i32.div_s", I32DivU = 0x6e => "i32.div_u",
        I32RemS = 0x6f => "i32.rem_s", I32RemU = 0x70 => "i32.rem_u",
        I32And = 0x71 => "i32.and", I32Or = 0x72 => "i32.or", I32Xor = 0x73 => "i32.xor",
        I32Shl = 0x74 => "i32.shl", I32ShrS = 0x75 => "i32.shr_s", I32ShrU = 0x76 => "i32.shr_u",
        I32Rotl = 0x77 => "i32.rotl", I32Rotr = 0x78 => "i32.rotr",
        // Numeric — i64.
        I64Clz = 0x79 => "i64.clz", I64Ctz = 0x7a => "i64.ctz", I64Popcnt = 0x7b => "i64.popcnt",
        I64Add = 0x7c => "i64.add", I64Sub = 0x7d => "i64.sub", I64Mul = 0x7e => "i64.mul",
        I64DivS = 0x7f => "i64.div_s", I64DivU = 0x80 => "i64.div_u",
        I64RemS = 0x81 => "i64.rem_s", I64RemU = 0x82 => "i64.rem_u",
        I64And = 0x83 => "i64.and", I64Or = 0x84 => "i64.or", I64Xor = 0x85 => "i64.xor",
        I64Shl = 0x86 => "i64.shl", I64ShrS = 0x87 => "i64.shr_s", I64ShrU = 0x88 => "i64.shr_u",
        I64Rotl = 0x89 => "i64.rotl", I64Rotr = 0x8a => "i64.rotr",
        // Numeric — f32.
        F32Abs = 0x8b => "f32.abs", F32Neg = 0x8c => "f32.neg", F32Ceil = 0x8d => "f32.ceil",
        F32Floor = 0x8e => "f32.floor", F32Trunc = 0x8f => "f32.trunc",
        F32Nearest = 0x90 => "f32.nearest", F32Sqrt = 0x91 => "f32.sqrt",
        F32Add = 0x92 => "f32.add", F32Sub = 0x93 => "f32.sub", F32Mul = 0x94 => "f32.mul",
        F32Div = 0x95 => "f32.div", F32Min = 0x96 => "f32.min", F32Max = 0x97 => "f32.max",
        F32Copysign = 0x98 => "f32.copysign",
        // Numeric — f64.
        F64Abs = 0x99 => "f64.abs", F64Neg = 0x9a => "f64.neg", F64Ceil = 0x9b => "f64.ceil",
        F64Floor = 0x9c => "f64.floor", F64Trunc = 0x9d => "f64.trunc",
        F64Nearest = 0x9e => "f64.nearest", F64Sqrt = 0x9f => "f64.sqrt",
        F64Add = 0xa0 => "f64.add", F64Sub = 0xa1 => "f64.sub", F64Mul = 0xa2 => "f64.mul",
        F64Div = 0xa3 => "f64.div", F64Min = 0xa4 => "f64.min", F64Max = 0xa5 => "f64.max",
        F64Copysign = 0xa6 => "f64.copysign",
        // Conversions.
        I32WrapI64 = 0xa7 => "i32.wrap_i64", I32TruncF32S = 0xa8 => "i32.trunc_f32_s",
        I32TruncF32U = 0xa9 => "i32.trunc_f32_u", I32TruncF64S = 0xaa => "i32.trunc_f64_s",
        I32TruncF64U = 0xab => "i32.trunc_f64_u", I64ExtendI32S = 0xac => "i64.extend_i32_s",
        I64ExtendI32U = 0xad => "i64.extend_i32_u", I64TruncF32S = 0xae => "i64.trunc_f32_s",
        I64TruncF32U = 0xaf => "i64.trunc_f32_u", I64TruncF64S = 0xb0 => "i64.trunc_f64_s",
        I64TruncF64U = 0xb1 => "i64.trunc_f64_u", F32ConvertI32S = 0xb2 => "f32.convert_i32_s",
        F32ConvertI32U = 0xb3 => "f32.convert_i32_u", F32ConvertI64S = 0xb4 => "f32.convert_i64_s",
        F32ConvertI64U = 0xb5 => "f32.convert_i64_u", F32DemoteF64 = 0xb6 => "f32.demote_f64",
        F64ConvertI32S = 0xb7 => "f64.convert_i32_s", F64ConvertI32U = 0xb8 => "f64.convert_i32_u",
        F64ConvertI64S = 0xb9 => "f64.convert_i64_s", F64ConvertI64U = 0xba => "f64.convert_i64_u",
        F64PromoteF32 = 0xbb => "f64.promote_f32", I32ReinterpretF32 = 0xbc => "i32.reinterpret_f32",
        I64ReinterpretF64 = 0xbd => "i64.reinterpret_f64",
        F32ReinterpretI32 = 0xbe => "f32.reinterpret_i32",
        F64ReinterpretI64 = 0xbf => "f64.reinterpret_i64",
        // Sign extension.
        I32Extend8S = 0xc0 => "i32.extend8_s", I32Extend16S = 0xc1 => "i32.extend16_s",
        I64Extend8S = 0xc2 => "i64.extend8_s", I64Extend16S = 0xc3 => "i64.extend16_s",
        I64Extend32S = 0xc4 => "i64.extend32_s",
        // Saturating (non-trapping) float→int truncation. Real wire form is `0xFC 0x00..0x07`;
        // these bytes are also accepted as raw single-byte forms, mirroring the wazmrt oracle.
        I32TruncSatF32S = 0xc5 => "i32.trunc_sat_f32_s", I32TruncSatF32U = 0xc6 => "i32.trunc_sat_f32_u",
        I32TruncSatF64S = 0xc7 => "i32.trunc_sat_f64_s", I32TruncSatF64U = 0xc8 => "i32.trunc_sat_f64_u",
        I64TruncSatF32S = 0xc9 => "i64.trunc_sat_f32_s", I64TruncSatF32U = 0xca => "i64.trunc_sat_f32_u",
        I64TruncSatF64S = 0xcb => "i64.trunc_sat_f64_s", I64TruncSatF64U = 0xcc => "i64.trunc_sat_f64_u",
        // Reference.
        RefNull = 0xd0 => "ref.null", RefIsNull = 0xd1 => "ref.is_null",
        RefFunc = 0xd2 => "ref.func", RefEq = 0xd3 => "ref.eq",
        RefAsNonNull = 0xd4 => "ref.as_non_null", BrOnNull = 0xd5 => "br_on_null",
        BrOnNonNull = 0xd6 => "br_on_non_null",
    }
    internal {
        // Bulk memory (`0xFC 0x08..0x0b`) + the SIMD / atomic family tags. The two family
        // tags have no text name of their own — their members are named per sub-opcode.
        MemoryInit = 0xd7 => "memory.init", DataDrop = 0xd8 => "data.drop",
        MemoryCopy = 0xd9 => "memory.copy", MemoryFill = 0xda => "memory.fill",
        Simd = 0xdb => "", Atomic = 0xdc => "",
        // Table ops (`0xFC 0x0c..0x11`).
        TableInit = 0xe0 => "table.init", ElemDrop = 0xe1 => "elem.drop",
        TableCopy = 0xe2 => "table.copy", TableGrow = 0xe3 => "table.grow",
        TableSize = 0xe4 => "table.size", TableFill = 0xe5 => "table.fill",
        // GC array ops (`0xFB` prefix).
        ArrayNew = 0xe6 => "array.new", ArrayNewDefault = 0xe7 => "array.new_default",
        ArrayNewFixed = 0xe8 => "array.new_fixed", ArrayGet = 0xe9 => "array.get",
        ArrayGetS = 0xea => "array.get_s", ArrayGetU = 0xeb => "array.get_u",
        ArraySet = 0xec => "array.set", ArrayLen = 0xed => "array.len",
        // GC array bulk ops (`0xFB 0x09/0x0a/0x10..0x13`). Added 2026-08-19: they were absent
        // from the table entirely, so `.wat` using them would not assemble and the binary form
        // had no opcode to decode — a gap that produced only SKIPS, never failures.
        ArrayNewData = 0xdd => "array.new_data", ArrayNewElem = 0xde => "array.new_elem",
        ArrayFill = 0xdf => "array.fill", ArrayCopy = 0xcd => "array.copy",
        ArrayInitData = 0xce => "array.init_data", ArrayInitElem = 0xcf => "array.init_elem",
        // GC casts (`0xFB` prefix).
        RefTest = 0xee => "ref.test", RefCastOp = 0xef => "ref.cast",
        // GC i31 / struct ops (`0xFB` prefix) + cast branches.
        RefI31 = 0xf0 => "ref.i31", I31GetS = 0xf1 => "i31.get_s", I31GetU = 0xf2 => "i31.get_u",
        StructNew = 0xf3 => "struct.new", StructNewDefault = 0xf4 => "struct.new_default",
        StructGet = 0xf5 => "struct.get", StructGetS = 0xf6 => "struct.get_s",
        StructGetU = 0xf7 => "struct.get_u", StructSet = 0xf8 => "struct.set",
        BrOnCast = 0xf9 => "br_on_cast", BrOnCastFail = 0xfa => "br_on_cast_fail",
        // The externref bridge (`0xFB 0x1a/0x1b`). ⚠️ Their internal tags are `0x16`/`0x17`
        // rather than the usual `0xd7..` block because that block is full and the two bytes
        // either side of it (`0xfb`, `0xfc`) are *prefix* bytes — tagging with one of those
        // would force `decode_body`'s internal-tag guard to reject the prefix it must accept.
        // `0x16`/`0x17` are unassigned in the single-byte space, and the guard covers them.
        AnyConvertExtern = 0x16 => "any.convert_extern",
        ExternConvertAny = 0x17 => "extern.convert_any",
    }
}

/// A GC heap type: an abstract head or a concrete type index (the operand of
/// `ref.null` / `ref.test` / `ref.cast` / `br_on_cast`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapType {
    Func,
    Extern,
    Any,
    Eq,
    I31,
    Struct,
    Array,
    None,
    NoFunc,
    NoExtern,
    Exn,
    /// A concrete type index.
    Concrete(u32),
}

/// A reference type: a heap type plus nullability (`(ref null? ht)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefType {
    pub nullable: bool,
    pub heap: HeapType,
}

/// A block signature (§5.3.6): empty, a single value type, or a type index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    Empty,
    Value(ValType),
    TypeIndex(u32),
    /// A single result of **concrete** reference type — `(ref null? $t)`, spelled long-form
    /// as `0x63`/`0x64` followed by a heap type.
    ///
    /// It cannot collapse into [`BlockType::Value`] here: a concrete `ValType` carries its
    /// family head (func/struct/array), and only the module's type section says which one
    /// `$t` is. `decode_body` has no module context by design, so the index travels
    /// unresolved and the validator — which does hold the module — maps it.
    ConcreteRef { nullable: bool, type_index: u32 },
}

/// A load/store memory-immediate. `memory` is the target memory index (multi-memory):
/// the alignment's bit 6 flags an explicit index that follows. `offset` is `u64` — the
/// memory64 proposal widens the static offset for a 64-bit memory (a 32-bit memory still
/// requires it to fit in `u32`, enforced by the validator).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemArg {
    pub alignment: u32,
    pub offset: u64,
    pub memory: u32,
}

/// A `br_table` target list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrTable {
    pub labels: Vec<u32>,
    pub default: u32,
}

/// A `call_indirect` immediate: a type index and a table index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallIndirect {
    pub type_index: u32,
    pub table: u32,
}

/// The four `try_table` catch-clause kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchKind {
    Catch,
    CatchRef,
    CatchAll,
    CatchAllRef,
}

/// A `try_table` catch clause. On a thrown exception whose tag matches (or `catch_all`),
/// control branches to `label` with the exception's values pushed — plus the `exnref`
/// itself for the `_ref` variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Catch {
    pub kind: CatchKind,
    pub tag: u32,
    pub label: u32,
}

/// A `try_table` immediate: a block type plus its catch clauses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TryTable {
    pub block_type: BlockType,
    pub catches: Vec<Catch>,
}

/// A decoded `0xFD` (v128 SIMD) instruction. `sub` is the 0xFD sub-opcode; `mem` is set
/// for loads/stores, `lane` for lane ops, `bytes` for `v128.const` (and the 16 lane
/// indices of `i8x16.shuffle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Simd {
    pub sub: u32,
    pub mem: MemArg,
    pub lane: u8,
    pub bytes: u128,
}

/// A decoded `0xFE` atomic instruction. `sub` is the 0xFE sub-opcode; `mem` is the
/// memarg every atomic memory op carries (`atomic.fence` has none, so `mem` is ignored).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Atomic {
    pub sub: u32,
    pub mem: MemArg,
}

/// A decoded instruction immediate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Imm {
    None,
    BlockType(BlockType),
    Label(u32),
    BrTable(BrTable),
    Func(u32),
    CallIndirect(CallIndirect),
    Local(u32),
    Global(u32),
    Table(u32),
    /// `elem.drop` — a passive element-segment index.
    Elem(u32),
    /// `data.drop` — a data-segment index.
    Data(u32),
    /// `table.init` — element-segment index + destination table index.
    TableInit { elem: u32, table: u32 },
    /// `table.copy` — destination + source table indices.
    TableCopy { dst: u32, src: u32 },
    Mem(MemArg),
    /// Reserved byte of `memory.size` / `memory.grow` (the memory index, 0).
    MemReserved(u8),
    /// Memory index of `memory.size` / `memory.grow` / `memory.fill` (multi-memory).
    MemIndex(u32),
    /// `memory.copy` — destination + source memory indices (multi-memory).
    MemCopy { dst: u32, src: u32 },
    /// `memory.init` — a data-segment index + the target memory index.
    MemInit { data: u32, mem: u32 },
    I32(i32),
    I64(i64),
    /// Raw little-endian bit pattern (`f32.const`).
    F32(u32),
    /// Raw little-endian bit pattern (`f64.const`).
    F64(u64),
    /// Result types of a typed `select` (`0x1c`).
    SelectTypes(Vec<ValType>),
    /// Heap type of `ref.null` (`0xd0`).
    RefType(HeapType),
    /// A GC type index (`struct.new` / `array.new` / `array.get` / …).
    GcType(u32),
    /// A GC struct type index + field index.
    GcField { type_index: u32, field: u32 },
    /// A GC array type index + element count (`array.new_fixed`).
    GcTypeN { type_index: u32, n: u32 },
    /// A GC array type index + a data/element **segment** index
    /// (`array.new_data` / `array.new_elem` / `array.init_data` / `array.init_elem`).
    ///
    /// Deliberately its own variant rather than reusing [`Imm::GcTypeN`]: that one carries a
    /// *count* and this one a *segment index*, and two fields of the same width meaning
    /// different things is how a shared shape hides a distinction.
    GcTypeSeg { type_index: u32, seg: u32 },
    /// `array.copy` — destination and source array type indices.
    GcArrayCopy { dst: u32, src: u32 },
    /// A GC cast target reference type (`ref.test` / `ref.cast`).
    RefCast(RefType),
    /// A GC cast-branch (`br_on_cast` / `br_on_cast_fail`): a label + source & dest types.
    BrCast {
        label: u32,
        src: RefType,
        dst: RefType,
    },
    /// `throw` / legacy `catch` — an exception tag index.
    Tag(u32),
    /// `try_table` — a block type + its catch clauses.
    TryTable(TryTable),
    /// A `0xFD` SIMD op.
    Simd(Simd),
    /// A `0xFE` atomic op.
    Atomic(Atomic),
}

/// A decoded instruction: an opcode, its immediate, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instr {
    pub op: Op,
    /// Byte offset of this instruction **within its function body**, for trap backtraces.
    ///
    /// **Free.** `Imm` is 64 bytes with 16-byte alignment, so `Instr` was already 80 bytes with 15
    /// of them padding after the one-byte opcode; this `u32` lands in that padding and `Instr` is
    /// still 80. A test pins that, because the moment `Imm` shrinks this stops being free and the
    /// trade should be re-decided rather than silently paid.
    ///
    /// Relative to the body, not absolute: `Code::body_offset` carries the body's own position, and
    /// keeping this relative means it cannot overflow on a module whose sections are large.
    pub offset: u32,
    pub imm: Imm,
}

/// The immediate shape an opcode carries, keyed by its byte. Mirrors wazmrt
/// `immediateKind`. The internal-tag kinds (`0xd7`+) are unreachable in [`decode_body`]
/// (rejected by the range guard first) but kept for completeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImmKind {
    None,
    BlockType,
    Label,
    BrTable,
    Func,
    CallIndirect,
    Local,
    Global,
    Table,
    Elem,
    Data,
    DataInit,
    MemCopy,
    TableInit,
    TableCopy,
    Mem,
    MemReserved,
    MemIndex,
    I32c,
    I64c,
    F32c,
    F64c,
    SelectTypes,
    RefType,
    GcType,
    GcField,
    GcTypeN,
    GcTypeSeg,
    GcArrayCopy,
    RefCast,
    BrCast,
    Tag,
    TryTable,
    Unsupported,
}

/// Classify an opcode's immediate by its binary byte (§5.4). Mirrors wazmrt
/// `immediateKind` exactly.
fn immediate_kind(b: u8) -> ImmKind {
    match b {
        0x02 | 0x03 | 0x04 | 0x06 => ImmKind::BlockType, // block/loop/if + legacy `try`
        0x08 | 0x07 => ImmKind::Tag,                     // throw / legacy `catch`
        0x0a => ImmKind::None,                           // throw_ref
        0x19 => ImmKind::None,                           // legacy `catch_all`
        0x1f => ImmKind::TryTable,                       // try_table
        0x0c | 0x0d | 0x09 | 0x18 => ImmKind::Label,     // br/br_if + legacy rethrow/delegate
        0x0e => ImmKind::BrTable,
        0x10 | 0x12 => ImmKind::Func, // call / return_call (imm = func index)
        0x11 | 0x13 => ImmKind::CallIndirect, // call_indirect / return_call_indirect
        0x14 | 0x15 => ImmKind::Func, // call_ref / return_call_ref (imm = type index)
        0xd5 | 0xd6 => ImmKind::Label, // br_on_null / br_on_non_null
        0x20..=0x22 => ImmKind::Local,
        0x23 | 0x24 => ImmKind::Global,
        0x25 | 0x26 | 0xe3 | 0xe4 | 0xe5 => ImmKind::Table, // table.get/set + grow/size/fill
        0xe0 => ImmKind::TableInit,
        0xe1 => ImmKind::Elem, // elem.drop
        0xe2 => ImmKind::TableCopy,
        0xd7 => ImmKind::DataInit,   // memory.init
        0xd8 => ImmKind::Data,       // data.drop
        0xd9 => ImmKind::MemCopy,    // memory.copy
        0xda => ImmKind::MemReserved, // memory.fill (raw tag byte, rejected at decode)
        0x28..=0x3e => ImmKind::Mem,
        0x3f | 0x40 => ImmKind::MemIndex, // memory.size / memory.grow
        0x41 => ImmKind::I32c,
        0x42 => ImmKind::I64c,
        0x43 => ImmKind::F32c,
        0x44 => ImmKind::F64c,
        0x1c => ImmKind::SelectTypes,
        0xd0 => ImmKind::RefType, // ref.null <heaptype>
        0xd2 => ImmKind::Func,    // ref.func <funcidx>
        // Core-MVP range with no immediate (`0xc5..=0xcc` are the sat-trunc tags).
        0x00 | 0x01 | 0x05 | 0x0b | 0x0f | 0x1a | 0x1b | 0xd1 | 0xd3 | 0xd4 | 0x45..=0xcc => {
            ImmKind::None
        }
        // GC ops with no immediate: ref.i31 / i31.get_s / i31.get_u, array.len, and the
        // externref bridge (internal tags `0x16`/`0x17`).
        0xf0 | 0xf1 | 0xf2 | 0xed | 0x16 | 0x17 => ImmKind::None,
        // GC ops with a single type index.
        0xe6 | 0xe7 | 0xe9 | 0xea | 0xeb | 0xec | 0xf3 | 0xf4 => ImmKind::GcType,
        // GC struct ops with a type index + field index.
        0xf5..=0xf8 => ImmKind::GcField,
        // array.new_fixed: type index + element count.
        0xe8 => ImmKind::GcTypeN,
        // array.fill: a single array type index.
        0xdf => ImmKind::GcType,
        // array.new_data / new_elem / init_data / init_elem: type index + segment index.
        0xdd | 0xde | 0xce | 0xcf => ImmKind::GcTypeSeg,
        // array.copy: destination + source array type indices.
        0xcd => ImmKind::GcArrayCopy,
        // ref.test / ref.cast: a target reference type.
        0xee | 0xef => ImmKind::RefCast,
        // br_on_cast / br_on_cast_fail: a label + source & destination ref types.
        0xf9 | 0xfa => ImmKind::BrCast,
        _ => ImmKind::Unsupported,
    }
}

/// Lanes addressable by an extract/replace/lane-load-store lane immediate. An
/// out-of-range lane must be rejected **at decode**. Not a lane op → 255 (never rejects).
fn simd_lane_count(sub: u32) -> u8 {
    match sub {
        0x15 | 0x16 | 0x17 | 0x54 | 0x58 => 16,
        0x18 | 0x19 | 0x1a | 0x55 | 0x59 => 8,
        0x1b | 0x1c | 0x1f | 0x20 | 0x56 | 0x5a => 4,
        0x1d | 0x1e | 0x21 | 0x22 | 0x57 | 0x5b => 2,
        _ => 255,
    }
}

/// The natural alignment of a scalar memory access, **as a log2 exponent** (the form the
/// memarg carries): 0 for 8-bit, 1 for 16-bit, 2 for 32-bit, 3 for 64-bit. The validator
/// rejects a memarg whose alignment exceeds this (§6.5.8); the assembler defaults a missing
/// `align=` to it. See [`simd_natural_align_log2`] / [`atomic_natural_align_log2`] for the
/// `0xFD` / `0xFE` families.
#[must_use]
pub fn natural_align_log2(op: Op) -> u32 {
    match op {
        Op::I32Load8S
        | Op::I32Load8U
        | Op::I64Load8S
        | Op::I64Load8U
        | Op::I32Store8
        | Op::I64Store8 => 0,
        Op::I32Load16S
        | Op::I32Load16U
        | Op::I64Load16S
        | Op::I64Load16U
        | Op::I32Store16
        | Op::I64Store16 => 1,
        Op::I32Load
        | Op::F32Load
        | Op::I32Store
        | Op::F32Store
        | Op::I64Load32S
        | Op::I64Load32U
        | Op::I64Store32 => 2,
        Op::I64Load | Op::F64Load | Op::I64Store | Op::F64Store => 3,
        _ => 0,
    }
}

/// The natural alignment (log2 bytes) of a `0xFD` SIMD memory access. As with the scalar
/// table this is a **maximum** the validator enforces (§6.5.8), and the assembler's default.
#[must_use]
pub fn simd_natural_align_log2(sub: u32) -> u32 {
    match sub {
        0x07 | 0x54 | 0x58 => 0, // 1-byte: load8_splat, load8_lane, store8_lane
        0x08 | 0x55 | 0x59 => 1, // 2-byte
        0x09 | 0x5c | 0x56 | 0x5a => 2, // 4-byte: load32_splat/zero/lane, store32_lane
        0x01..=0x06 | 0x0a | 0x5d | 0x57 | 0x5b => 3, // 8-byte: loadMxN, load64_splat/zero/lane
        _ => 4,                  // 16-byte: v128.load / v128.store
    }
}

/// Does this `0xFD` sub-opcode carry a memarg (i.e. touch linear memory)? The `Simd`
/// immediate always has a `mem` field (defaulted), so its presence cannot distinguish these.
/// Kept beside `decode_simd`, whose match is the authority, so the two can't drift.
#[must_use]
pub fn simd_is_memory_op(sub: u32) -> bool {
    matches!(sub, 0x00..=0x0b | 0x5c | 0x5d | 0x54..=0x5b)
}

/// The **required** alignment (log2 bytes) of a `0xFE` atomic op. Atomics must be naturally
/// aligned, so unlike the scalar/SIMD tables this is the exact value the validator enforces
/// — any other alignment is invalid, not merely over-aligned. The access width rides in the
/// sub-opcode (`…8`→1 byte, `…16`→2, `…32`→4, else the full type width). `atomic.fence`
/// (0x03) has no memarg and returns 0.
#[must_use]
pub fn atomic_natural_align_log2(sub: u32) -> u32 {
    match sub {
        0x00 | 0x01 => 2, // notify, wait32
        0x02 => 3,        // wait64
        0x03 => 0,        // fence (no memarg)
        0x10 | 0x17 => 2, // i32.atomic.load / store
        0x11 | 0x18 => 3, // i64.atomic.load / store
        0x12 | 0x19 => 0, // i32 …8
        0x13 | 0x1a => 1, // i32 …16
        0x14 | 0x1b => 0, // i64 …8
        0x15 | 0x1c => 1, // i64 …16
        0x16 | 0x1d => 2, // i64 …32
        // rmw + cmpxchg: groups of 7, laid out
        // [i32.full, i64.full, i32.8, i32.16, i64.8, i64.16, i64.32] from 0x1e.
        0x1e..=0x4e => match (sub - 0x1e) % 7 {
            0 => 2, // i32 full (4 bytes)
            1 => 3, // i64 full (8)
            2 => 0, // i32.8    (1)
            3 => 1, // i32.16   (2)
            4 => 0, // i64.8    (1)
            5 => 1, // i64.16   (2)
            _ => 2, // i64.32   (4)
        },
        _ => 0,
    }
}

/// Highest `0xFD` sub-opcode wasmrt decodes — the tail of the relaxed-SIMD range.
const MAX_SIMD_SUB: u32 = 0x113;

/// Highest `0xFE` atomic sub-opcode wasmrt decodes (`i64.atomic.rmw32.cmpxchg_u`).
const MAX_ATOMIC_SUB: u32 = 0x4e;

/// Decode a block type (§5.3.6): an s33 — negative values encode empty/valtype, a
/// non-negative value is a type index.
fn read_block_type(r: &mut Reader) -> DecodeResult<BlockType> {
    let v = r.read_var_s33()?;
    if v >= 0 {
        if v > u32::MAX as i64 {
            return Err(DecodeError::UnsupportedOpcode);
        }
        return Ok(BlockType::TypeIndex(v as u32));
    }
    Ok(match v {
        -64 => BlockType::Empty,
        -1 => BlockType::Value(ValType::I32),
        -2 => BlockType::Value(ValType::I64),
        -3 => BlockType::Value(ValType::F32),
        -4 => BlockType::Value(ValType::F64),
        -5 => BlockType::Value(ValType::V128),
        -16 => BlockType::Value(ValType::FUNCREF),
        -17 => BlockType::Value(ValType::EXTERNREF),
        -18 => BlockType::Value(ValType::ANYREF),
        -19 => BlockType::Value(ValType::EQREF),
        -20 => BlockType::Value(ValType::I31REF),
        -21 => BlockType::Value(ValType::STRUCTREF),
        -22 => BlockType::Value(ValType::ARRAYREF),
        -23 => BlockType::Value(ValType::EXNREF),
        -15 => BlockType::Value(ValType::NULLREF),
        -24 => BlockType::Value(ValType::FUNCREF_NN),
        -25 => BlockType::Value(ValType::EXTERNREF_NN),
        -26 => BlockType::Value(ValType::ANYREF_NN),
        -27 => BlockType::Value(ValType::EQREF_NN),
        -30 => BlockType::Value(ValType::I31REF_NN),
        -31 => BlockType::Value(ValType::STRUCTREF_NN),
        -39 => BlockType::Value(ValType::ARRAYREF_NN),
        -40 => BlockType::Value(ValType::NULLREF_NN),
        -41 => BlockType::Value(ValType::EXNREF_NN),
        // The long forms `0x63 ht` / `0x64 ht` — as s33, `0x63` reads as -29 and `0x64` as
        // -28. A block result of concrete reference type has no other encoding, so without
        // these two arms the heap-type byte was re-read as an opcode and a perfectly valid
        // module was rejected as an unsupported instruction.
        -29 | -28 => {
            let nullable = v == -29;
            match read_heap_type(r)? {
                HeapType::Concrete(type_index) => BlockType::ConcreteRef {
                    nullable,
                    type_index,
                },
                ht => BlockType::Value(abstract_heap_val_type(ht, nullable)),
            }
        }
        _ => return Err(DecodeError::UnsupportedOpcode),
    })
}

/// The value type of an abstract heap type at a given nullability — the long-form
/// `0x63`/`0x64` spelling of what the one-byte shorthands already encode.
fn abstract_heap_val_type(ht: HeapType, nullable: bool) -> ValType {
    let (n, nn) = match ht {
        // `nofunc`/`noextern`/`none` are bottoms of their families; the port models each
        // as its family head, exactly as `module::read_heap_type_ref` does.
        HeapType::Func | HeapType::NoFunc => (ValType::FUNCREF, ValType::FUNCREF_NN),
        HeapType::Extern | HeapType::NoExtern => (ValType::EXTERNREF, ValType::EXTERNREF_NN),
        HeapType::Any => (ValType::ANYREF, ValType::ANYREF_NN),
        HeapType::Eq => (ValType::EQREF, ValType::EQREF_NN),
        HeapType::I31 => (ValType::I31REF, ValType::I31REF_NN),
        HeapType::Struct => (ValType::STRUCTREF, ValType::STRUCTREF_NN),
        HeapType::Array => (ValType::ARRAYREF, ValType::ARRAYREF_NN),
        HeapType::Exn => (ValType::EXNREF, ValType::EXNREF_NN),
        HeapType::None => (ValType::NULLREF, ValType::NULLREF_NN),
        // Unreachable: `read_heap_type` returns `Concrete` only for a non-negative s33,
        // which the caller has already split off.
        HeapType::Concrete(_) => (ValType::ANYREF, ValType::ANYREF_NN),
    };
    if nullable { n } else { nn }
}

/// Read a heap type (§ GC binary format): a non-negative `s33` is a concrete type index;
/// negative values are the abstract heap-type codes. Public because the validator reads it
/// from a `ref.null` constant expression.
pub fn read_heap_type(r: &mut Reader) -> DecodeResult<HeapType> {
    let v = r.read_var_s33()?;
    if v >= 0 {
        if v > u32::MAX as i64 {
            return Err(DecodeError::UnsupportedOpcode);
        }
        return Ok(HeapType::Concrete(v as u32));
    }
    Ok(match v {
        -0x10 => HeapType::Func,
        -0x11 => HeapType::Extern,
        -0x12 => HeapType::Any,
        -0x13 => HeapType::Eq,
        -0x14 => HeapType::I31,
        -0x15 => HeapType::Struct,
        -0x16 => HeapType::Array,
        -0x0f => HeapType::None,
        -0x0d => HeapType::NoFunc,
        -0x0e => HeapType::NoExtern,
        -0x17 => HeapType::Exn,
        _ => return Err(DecodeError::UnsupportedOpcode),
    })
}

/// Read a GC struct-op immediate: a struct type index followed by a field index.
fn read_gc_field(r: &mut Reader) -> DecodeResult<Imm> {
    let type_index = r.read_var_u32()?;
    let field = r.read_var_u32()?;
    Ok(Imm::GcField { type_index, field })
}

/// Read a `br_on_cast` / `br_on_cast_fail` immediate: a flags byte (bit 0 = src nullable,
/// bit 1 = dst nullable), a label index, then the src & dst heap types.
fn read_br_cast(r: &mut Reader) -> DecodeResult<Imm> {
    let flags = r.read_byte()?;
    let label = r.read_var_u32()?;
    let src_ht = read_heap_type(r)?;
    let dst_ht = read_heap_type(r)?;
    Ok(Imm::BrCast {
        label,
        src: RefType {
            nullable: flags & 0b01 != 0,
            heap: src_ht,
        },
        dst: RefType {
            nullable: flags & 0b10 != 0,
            heap: dst_ht,
        },
    })
}

/// Read a load/store memarg. Multi-memory: bit 6 of the alignment flags an explicit
/// memory index that follows (before the offset); else memory 0. The offset is a full
/// `u64` (the 32-bit-memory ceiling is a validation rule, not a decode one).
fn read_mem_arg(r: &mut Reader) -> DecodeResult<MemArg> {
    let mut alignment = r.read_var_u32()?;
    let mut memory = 0u32;
    if alignment & 0x40 != 0 {
        alignment &= !0x40u32;
        memory = r.read_var_u32()?;
    }
    let offset = r.read_var_u64()?;
    Ok(MemArg {
        alignment,
        offset,
        memory,
    })
}

/// Decode a `0xFD` SIMD op given its sub-opcode. Decoding is complete for the whole
/// family; execution supports a subset (added at T5).
fn decode_simd(r: &mut Reader, sub: u32) -> DecodeResult<Instr> {
    let mut s = Simd {
        sub,
        mem: MemArg {
            alignment: 0,
            offset: 0,
            memory: 0,
        },
        lane: 0,
        bytes: 0,
    };
    match sub {
        0x00..=0x0b | 0x5c | 0x5d => s.mem = read_mem_arg(r)?, // v128.load* / store / load{32,64}_zero
        0x54..=0x5b => {
            // v128.load/store lane: memarg + a lane index.
            s.mem = read_mem_arg(r)?;
            s.lane = r.read_byte()?;
            if s.lane >= simd_lane_count(sub) {
                return Err(DecodeError::UnsupportedOpcode);
            }
        }
        0x0c | 0x0d => {
            // v128.const / i8x16.shuffle: 16 immediate bytes (little-endian).
            let mut v: u128 = 0;
            for i in 0..16u32 {
                let b = r.read_byte()?;
                // shuffle's bytes are lane indices selecting from two 16-byte operands,
                // so each must be < 32; v128.const bytes are literal data.
                if sub == 0x0d && b >= 32 {
                    return Err(DecodeError::UnsupportedOpcode);
                }
                v |= (b as u128) << (i * 8);
            }
            s.bytes = v;
        }
        0x15..=0x22 => {
            // extract_lane / replace_lane.
            s.lane = r.read_byte()?;
            if s.lane >= simd_lane_count(sub) {
                return Err(DecodeError::UnsupportedOpcode);
            }
        }
        _ => {
            if sub > MAX_SIMD_SUB {
                return Err(DecodeError::UnsupportedOpcode);
            }
        }
    }
    // `offset` is filled in by the caller at push time, which is the only place the instruction's
    // start is known — a decoder for one family cannot see it.
    Ok(Instr {
        op: Op::Simd,
        offset: 0,
        imm: Imm::Simd(s),
    })
}

/// Decode a `0xFE` atomic op. `atomic.fence` (0x03) carries a reserved byte; every other
/// op carries a memarg. Sub-opcodes outside the defined set are rejected at decode.
fn decode_atomic(r: &mut Reader, sub: u32) -> DecodeResult<Instr> {
    let mut at = Atomic {
        sub,
        mem: MemArg {
            alignment: 0,
            offset: 0,
            memory: 0,
        },
    };
    match sub {
        0x03 => {
            r.read_byte()?; // atomic.fence: a reserved 0x00
        }
        0x00 | 0x01 | 0x02 | 0x10..=MAX_ATOMIC_SUB => at.mem = read_mem_arg(r)?,
        _ => return Err(DecodeError::UnsupportedOpcode),
    }
    Ok(Instr {
        op: Op::Atomic,
        offset: 0,
        imm: Imm::Atomic(at),
    })
}

/// Read a `try_table` immediate: a block type followed by a vector of catch clauses.
fn read_try_table(r: &mut Reader) -> DecodeResult<Imm> {
    let block_type = read_block_type(r)?;
    let n = r.read_vec_len()?;
    let mut catches = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let kind = match r.read_byte()? {
            0x00 => CatchKind::Catch,
            0x01 => CatchKind::CatchRef,
            0x02 => CatchKind::CatchAll,
            0x03 => CatchKind::CatchAllRef,
            _ => return Err(DecodeError::UnsupportedOpcode),
        };
        let tag = match kind {
            CatchKind::Catch | CatchKind::CatchRef => r.read_var_u32()?,
            CatchKind::CatchAll | CatchKind::CatchAllRef => 0,
        };
        let label = r.read_var_u32()?;
        catches.push(Catch { kind, tag, label });
    }
    Ok(Imm::TryTable(TryTable {
        block_type,
        catches,
    }))
}

/// Decode a function body's raw bytes into a flat instruction list. Nesting and branch
/// targets are left to validation. Owned immediates are freed when the `Vec` drops.
pub fn decode_body(body: &[u8]) -> DecodeResult<Vec<Instr>> {
    let mut r = Reader::new(body);
    let mut list: Vec<Instr> = Vec::new();

    while !r.at_end() {
        // Where this instruction starts, for trap backtraces. Captured before the opcode byte is
        // consumed, so it points at the instruction rather than its first immediate.
        let at = u32::try_from(r.pos()).unwrap_or(u32::MAX);
        let b0 = r.read_byte()?;

        if b0 == 0xfb {
            // 0xFB-prefixed GC op: a LEB sub-opcode picks the internal Op tag.
            let instr = match r.read_var_u32()? {
                0x00 => Instr { offset: 0, op: Op::StructNew, imm: Imm::GcType(r.read_var_u32()?) },
                0x01 => Instr { offset: 0, op: Op::StructNewDefault, imm: Imm::GcType(r.read_var_u32()?) },
                0x02 => Instr { offset: 0, op: Op::StructGet, imm: read_gc_field(&mut r)? },
                0x03 => Instr { offset: 0, op: Op::StructGetS, imm: read_gc_field(&mut r)? },
                0x04 => Instr { offset: 0, op: Op::StructGetU, imm: read_gc_field(&mut r)? },
                0x05 => Instr { offset: 0, op: Op::StructSet, imm: read_gc_field(&mut r)? },
                0x06 => Instr { offset: 0, op: Op::ArrayNew, imm: Imm::GcType(r.read_var_u32()?) },
                0x07 => Instr { offset: 0, op: Op::ArrayNewDefault, imm: Imm::GcType(r.read_var_u32()?) },
                0x08 => Instr {
                    offset: 0,
                    op: Op::ArrayNewFixed,
                    imm: Imm::GcTypeN { type_index: r.read_var_u32()?, n: r.read_var_u32()? },
                },
                0x09 => Instr {
                    offset: 0,
                    op: Op::ArrayNewData,
                    imm: Imm::GcTypeSeg { type_index: r.read_var_u32()?, seg: r.read_var_u32()? },
                },
                0x0a => Instr {
                    offset: 0,
                    op: Op::ArrayNewElem,
                    imm: Imm::GcTypeSeg { type_index: r.read_var_u32()?, seg: r.read_var_u32()? },
                },
                0x0b => Instr { offset: 0, op: Op::ArrayGet, imm: Imm::GcType(r.read_var_u32()?) },
                0x0c => Instr { offset: 0, op: Op::ArrayGetS, imm: Imm::GcType(r.read_var_u32()?) },
                0x0d => Instr { offset: 0, op: Op::ArrayGetU, imm: Imm::GcType(r.read_var_u32()?) },
                0x0e => Instr { offset: 0, op: Op::ArraySet, imm: Imm::GcType(r.read_var_u32()?) },
                0x0f => Instr { offset: 0, op: Op::ArrayLen, imm: Imm::None },
                0x10 => Instr { offset: 0, op: Op::ArrayFill, imm: Imm::GcType(r.read_var_u32()?) },
                0x11 => Instr {
                    offset: 0,
                    op: Op::ArrayCopy,
                    imm: Imm::GcArrayCopy { dst: r.read_var_u32()?, src: r.read_var_u32()? },
                },
                0x12 => Instr {
                    offset: 0,
                    op: Op::ArrayInitData,
                    imm: Imm::GcTypeSeg { type_index: r.read_var_u32()?, seg: r.read_var_u32()? },
                },
                0x13 => Instr {
                    offset: 0,
                    op: Op::ArrayInitElem,
                    imm: Imm::GcTypeSeg { type_index: r.read_var_u32()?, seg: r.read_var_u32()? },
                },
                0x14 => Instr {
                    offset: 0,
                    op: Op::RefTest,
                    imm: Imm::RefCast(RefType { nullable: false, heap: read_heap_type(&mut r)? }),
                },
                0x15 => Instr {
                    offset: 0,
                    op: Op::RefTest,
                    imm: Imm::RefCast(RefType { nullable: true, heap: read_heap_type(&mut r)? }),
                },
                0x16 => Instr {
                    offset: 0,
                    op: Op::RefCastOp,
                    imm: Imm::RefCast(RefType { nullable: false, heap: read_heap_type(&mut r)? }),
                },
                0x17 => Instr {
                    offset: 0,
                    op: Op::RefCastOp,
                    imm: Imm::RefCast(RefType { nullable: true, heap: read_heap_type(&mut r)? }),
                },
                0x18 => Instr { offset: 0, op: Op::BrOnCast, imm: read_br_cast(&mut r)? },
                0x19 => Instr { offset: 0, op: Op::BrOnCastFail, imm: read_br_cast(&mut r)? },
                // The externref bridge. An `externref` is a WRAPPER: `extern.convert_any` boxes
                // an internal reference, `any.convert_extern` unboxes one, and null maps to null
                // both ways (§4.4.7.3). The wrapper bit lives in `Value`'s high half, so the two
                // numeric spaces the earlier note warned about — a host handle and a GC heap index
                // — are no longer one space. See `interp::EXTERN_TAG` / `interp::HOST_TAG`.
                0x1a => Instr { offset: 0, op: Op::AnyConvertExtern, imm: Imm::None },
                0x1b => Instr { offset: 0, op: Op::ExternConvertAny, imm: Imm::None },
                0x1c => Instr { offset: 0, op: Op::RefI31, imm: Imm::None },
                0x1d => Instr { offset: 0, op: Op::I31GetS, imm: Imm::None },
                0x1e => Instr { offset: 0, op: Op::I31GetU, imm: Imm::None },
                _ => return Err(DecodeError::UnsupportedOpcode),
            };
            list.push(Instr { offset: at, ..instr });
            continue;
        }

        if b0 == 0xfc {
            // 0xFC-prefixed op: a LEB sub-opcode picks the internal Op tag.
            let instr = match r.read_var_u32()? {
                0x00 => Instr { offset: 0, op: Op::I32TruncSatF32S, imm: Imm::None },
                0x01 => Instr { offset: 0, op: Op::I32TruncSatF32U, imm: Imm::None },
                0x02 => Instr { offset: 0, op: Op::I32TruncSatF64S, imm: Imm::None },
                0x03 => Instr { offset: 0, op: Op::I32TruncSatF64U, imm: Imm::None },
                0x04 => Instr { offset: 0, op: Op::I64TruncSatF32S, imm: Imm::None },
                0x05 => Instr { offset: 0, op: Op::I64TruncSatF32U, imm: Imm::None },
                0x06 => Instr { offset: 0, op: Op::I64TruncSatF64S, imm: Imm::None },
                0x07 => Instr { offset: 0, op: Op::I64TruncSatF64U, imm: Imm::None },
                0x08 => {
                    let data = r.read_var_u32()?;
                    let mem = r.read_var_u32()?;
                    Instr { offset: 0, op: Op::MemoryInit, imm: Imm::MemInit { data, mem } }
                }
                0x09 => Instr { offset: 0, op: Op::DataDrop, imm: Imm::Data(r.read_var_u32()?) },
                0x0a => {
                    let dst = r.read_var_u32()?;
                    let src = r.read_var_u32()?;
                    Instr { offset: 0, op: Op::MemoryCopy, imm: Imm::MemCopy { dst, src } }
                }
                0x0b => Instr { offset: 0, op: Op::MemoryFill, imm: Imm::MemIndex(r.read_var_u32()?) },
                0x0c => {
                    let elem = r.read_var_u32()?;
                    let table = r.read_var_u32()?;
                    Instr { offset: 0, op: Op::TableInit, imm: Imm::TableInit { elem, table } }
                }
                0x0d => Instr { offset: 0, op: Op::ElemDrop, imm: Imm::Elem(r.read_var_u32()?) },
                0x0e => {
                    let dst = r.read_var_u32()?;
                    let src = r.read_var_u32()?;
                    Instr { offset: 0, op: Op::TableCopy, imm: Imm::TableCopy { dst, src } }
                }
                0x0f => Instr { offset: 0, op: Op::TableGrow, imm: Imm::Table(r.read_var_u32()?) },
                0x10 => Instr { offset: 0, op: Op::TableSize, imm: Imm::Table(r.read_var_u32()?) },
                0x11 => Instr { offset: 0, op: Op::TableFill, imm: Imm::Table(r.read_var_u32()?) },
                _ => return Err(DecodeError::UnsupportedOpcode),
            };
            list.push(Instr { offset: at, ..instr });
            continue;
        }

        if b0 == 0xfd {
            let sub = r.read_var_u32()?;
            list.push(Instr { offset: at, ..decode_simd(&mut r, sub)? });
            continue;
        }

        if b0 == 0xfe {
            let sub = r.read_var_u32()?;
            list.push(Instr { offset: at, ..decode_atomic(&mut r, sub)? });
            continue;
        }

        // `0xcd..=0xcf` and `0xd7..=0xfa` are internal tags whose real wire form is a
        // `0xFB`/`0xFC` prefix + sub-opcode (handled above). A raw byte in either range is not a
        // valid single-byte opcode. (`0xd0..=0xd6` are real ops; `0xfb..=0xfe` are prefixes.)
        //
        // `0x16..=0x17` joined them with the externref bridge (`any.convert_extern` /
        // `extern.convert_any`); both bytes are unassigned in the single-byte space.
        //
        // ⚠️⚠️ **The `0xcd..=0xcf` half was added with the array bulk ops on 2026-08-19, and
        // extending this guard is not optional.** Those three tags name `array.copy` /
        // `array.init_data` / `array.init_elem` internally; without the guard a raw `0xcd` byte in
        // a function body falls through to `immediate_kind` and **decodes as `array.copy`** — an
        // accept-invalid, and precisely the "a synthetic internal tag placed in a real encoding
        // space eventually means something else" defect recorded in `best-practices.md` §3A.2 the
        // same morning. **Whenever an internal tag is added, this range moves with it**; the test
        // `raw_internal_tag_bytes_are_refused` pins every one of them.
        if (0x16..=0x17).contains(&b0) || (0xcd..=0xcf).contains(&b0) || (0xd7..=0xfa).contains(&b0) {
            return Err(DecodeError::UnsupportedOpcode);
        }

        let imm = match immediate_kind(b0) {
            ImmKind::None => Imm::None,
            ImmKind::BlockType => Imm::BlockType(read_block_type(&mut r)?),
            ImmKind::Label => Imm::Label(r.read_var_u32()?),
            ImmKind::BrTable => {
                let n = r.read_vec_len()?;
                let mut labels = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    labels.push(r.read_var_u32()?);
                }
                Imm::BrTable(BrTable { labels, default: r.read_var_u32()? })
            }
            ImmKind::Func => Imm::Func(r.read_var_u32()?),
            ImmKind::CallIndirect => {
                let type_index = r.read_var_u32()?;
                let table = r.read_var_u32()?;
                Imm::CallIndirect(CallIndirect { type_index, table })
            }
            ImmKind::Local => Imm::Local(r.read_var_u32()?),
            ImmKind::Global => Imm::Global(r.read_var_u32()?),
            ImmKind::Table => Imm::Table(r.read_var_u32()?),
            ImmKind::Mem => Imm::Mem(read_mem_arg(&mut r)?),
            ImmKind::MemIndex => Imm::MemIndex(r.read_var_u32()?),
            ImmKind::I32c => Imm::I32(r.read_var_i32()?),
            ImmKind::I64c => Imm::I64(r.read_var_i64()?),
            ImmKind::F32c => Imm::F32(r.read_f32_bits()?),
            ImmKind::F64c => Imm::F64(r.read_f64_bits()?),
            ImmKind::SelectTypes => {
                let n = r.read_vec_len()?;
                let mut tys = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let t = ValType::from_bits(r.read_byte()? as u32);
                    // A single byte can't encode a concrete `(ref $t)` (bit 31 clear),
                    // so `is_valid` is exactly the abstract/numeric set here.
                    if !t.is_valid() {
                        return Err(DecodeError::UnsupportedOpcode);
                    }
                    tys.push(t);
                }
                Imm::SelectTypes(tys)
            }
            ImmKind::RefType => Imm::RefType(read_heap_type(&mut r)?),
            ImmKind::Tag => Imm::Tag(r.read_var_u32()?),
            ImmKind::TryTable => read_try_table(&mut r)?,
            // These kinds belong to `0xFB`/`0xFC`-prefixed ops decoded above; reaching
            // here means a raw synthetic-tag byte, which is malformed.
            ImmKind::Elem
            | ImmKind::Data
            | ImmKind::DataInit
            | ImmKind::MemCopy
            | ImmKind::MemReserved
            | ImmKind::TableInit
            | ImmKind::TableCopy
            | ImmKind::GcType
            | ImmKind::GcField
            | ImmKind::GcTypeN
            | ImmKind::GcTypeSeg
            | ImmKind::GcArrayCopy
            | ImmKind::RefCast
            | ImmKind::BrCast
            | ImmKind::Unsupported => return Err(DecodeError::UnsupportedOpcode),
        };

        let op = Op::from_u8(b0).ok_or(DecodeError::UnsupportedOpcode)?;
        list.push(Instr { op, offset: at, imm });
    }

    // An `expr` is terminated by its matching `end` (§5.4.9), so the last instruction must be one.
    // Both callers decode a complete expression — a function body or a constant expression — and a
    // body that simply runs out of bytes was previously accepted here and only refused later, by
    // the validator's control-stack underflow.
    //
    // Deliberately a **terminator** check, not a nesting one: full balance (which `end` closes
    // which opener, and the legacy-EH `delegate` that terminates a `try` without one) is
    // `precompute_control_flow`'s job, and duplicating that here would be two authorities on the
    // same rule. Every well-formed expression ends in `end` regardless of what it contains, so this
    // much is provable in one line.
    if list.last().map(|i| i.op) != Some(Op::End) {
        return Err(DecodeError::MissingEnd);
    }

    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn simple_body_local_add_end() {
        // local.get 0 ; local.get 1 ; i32.add ; end
        let body = [0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b];
        let instrs = decode_body(&body).unwrap();
        assert_eq!(instrs.len(), 4);
        assert_eq!(instrs[0], Instr { offset: 0, op: Op::LocalGet, imm: Imm::Local(0) });
        assert_eq!(instrs[1], Instr { offset: 2, op: Op::LocalGet, imm: Imm::Local(1) });
        assert_eq!(instrs[2], Instr { offset: 4, op: Op::I32Add, imm: Imm::None });
        assert_eq!(instrs[3].op, Op::End);
    }

    #[test]
    fn immediates_block_const_load() {
        // block (result i32) ; i32.const -3 ; i32.load align=2 offset=8 ; end
        let body = [0x02, 0x7f, 0x41, 0x7d, 0x28, 0x02, 0x08, 0x0b];
        let instrs = decode_body(&body).unwrap();
        assert_eq!(instrs.len(), 4);
        assert_eq!(instrs[0].imm, Imm::BlockType(BlockType::Value(ValType::I32)));
        assert_eq!(instrs[1].imm, Imm::I32(-3));
        assert_eq!(
            instrs[2].imm,
            Imm::Mem(MemArg { alignment: 2, offset: 8, memory: 0 })
        );
        assert_eq!(instrs[3].op, Op::End);
    }

    #[test]
    fn decodes_br_table() {
        // br_table 0 1 (default 2)
        let body = [0x0e, 0x02, 0x00, 0x01, 0x02, 0x0b];
        let instrs = decode_body(&body).unwrap();
        assert_eq!(instrs.len(), 2); // br_table + the terminating `end`
        assert_eq!(
            instrs[0].imm,
            Imm::BrTable(BrTable { labels: vec![0, 1], default: 2 })
        );
    }

    #[test]
    fn rejects_block_type_index_out_of_s33_range() {
        // block with an s33 whose bit 32 is set but higher bits don't sign-extend it —
        // 2^32 is out of s33 range, so readVarS33 rejects it as malformed.
        let body = [0x02, 0x80, 0x80, 0x80, 0x80, 0x10];
        assert_eq!(decode_body(&body), Err(DecodeError::LebOverflow));
        // An over-long (>5-byte) s33 encoding of a small index is also rejected.
        let overlong = [0x02, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00];
        assert_eq!(decode_body(&overlong), Err(DecodeError::LebOverflow));
    }

    #[test]
    fn select_t_rejects_invalid_valtype() {
        // select_t (0x1c), 1 result type, 0x50 — not a value type → rejected.
        assert_eq!(
            decode_body(&[0x1c, 0x01, 0x50]),
            Err(DecodeError::UnsupportedOpcode)
        );
        // A valid typed select (i32 = 0x7f) still decodes.
        let ok = decode_body(&[0x1c, 0x01, 0x7f, 0x0b]).unwrap();
        assert_eq!(ok[0].op, Op::SelectT);
        assert_eq!(ok[0].imm, Imm::SelectTypes(vec![ValType::I32]));
    }

    #[test]
    fn decodes_simd_v128_const() {
        // 0xfd 0x0c <16 bytes> — v128.const with a 1,2,3,4 (i32x4) little-endian payload.
        let body = [0xfd, 0x0c, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 0x0b];
        let instrs = decode_body(&body).unwrap();
        assert_eq!(instrs[0].op, Op::Simd);
        let Imm::Simd(s) = &instrs[0].imm else {
            panic!("expected a SIMD immediate");
        };
        assert_eq!(s.sub, 0x0c);
        assert_eq!(s.bytes, 1 | (2u128 << 32) | (3u128 << 64) | (4u128 << 96));
    }

    #[test]
    fn decodes_fc_memory_copy() {
        // 0xfc 0x0a 0x00 0x00 — memory.copy dst-mem 0, src-mem 0.
        let instrs = decode_body(&[0xfc, 0x0a, 0x00, 0x00, 0x0b]).unwrap();
        assert_eq!(
            instrs[0],
            Instr { offset: 0, op: Op::MemoryCopy, imm: Imm::MemCopy { dst: 0, src: 0 } }
        );
    }

    /// **The assumption the backtrace design rests on: `Instr::offset` is free.** `Imm` is 64 bytes at
    /// 16-byte alignment, so `Instr` was already 80 with 15 bytes of padding after the one-byte
    /// opcode, and the `u32` lands in that padding.
    ///
    /// Pinned because it stops being true the moment `Imm` shrinks — at which point the offset starts
    /// costing 16 bytes per instruction and the trade should be **re-decided**, not silently paid.
    /// An optimization pass that shrinks `Imm` (a live T11 candidate) will fail here and be told why.
    #[test]
    fn the_instruction_offset_costs_nothing() {
        assert_eq!(
            core::mem::size_of::<Instr>(),
            80,
            "Instr grew — if Imm shrank, `offset` is no longer free; see the doc on Instr::offset"
        );
    }

    /// The offset points at the instruction's own first byte, not at its immediates, and not at the
    /// following instruction — which is what a backtrace consumer expects to map to source.
    #[test]
    fn offsets_point_at_the_start_of_each_instruction() {
        // i32.const 1 (2 bytes) ; i32.const 300 (3 bytes: 0x41 + 2-byte LEB) ; drop ; end
        let body = [0x41, 0x01, 0x41, 0xac, 0x02, 0x1a, 0x0b];
        let instrs = decode_body(&body).unwrap();
        assert_eq!(
            instrs.iter().map(|i| i.offset).collect::<Vec<_>>(),
            vec![0, 2, 5, 6]
        );
    }

    #[test]
    fn rejects_unknown_opcode() {
        assert_eq!(decode_body(&[0xff]), Err(DecodeError::UnsupportedOpcode));
    }

    /// An expression must be terminated by its matching `end` (§5.4.9). Without this, a body that
    /// simply ran out of bytes decoded fine and was only refused later by the validator's control
    /// stack — the wrong stage for a malformed encoding.
    #[test]
    fn rejects_an_expression_with_no_terminating_end() {
        // i32.const 1 ; drop — and then nothing.
        assert_eq!(
            decode_body(&[0x41, 0x01, 0x1a]),
            Err(DecodeError::MissingEnd)
        );
        // An empty expression has no `end` either.
        assert_eq!(decode_body(&[]), Err(DecodeError::MissingEnd));
        // The same bytes terminated properly decode.
        assert!(decode_body(&[0x41, 0x01, 0x1a, 0x0b]).is_ok());
        assert!(decode_body(&[0x02, 0x40, 0x0b, 0x0b]).is_ok()); // block … end, then the body's end

        // **The boundary, asserted rather than assumed.** `block … end` with the function's own
        // `end` missing still *ends* in an `end`, so this check passes it — the check is a
        // terminator check, not a nesting one. That imbalance is caught later, by
        // `precompute_control_flow` at instantiation and by the validator's control stack. Pinned
        // here so the limitation is visible in the tests instead of being discovered by someone
        // trusting the name of the error.
        assert!(decode_body(&[0x02, 0x40, 0x0b]).is_ok());
    }

    #[test]
    fn rejects_raw_internal_tag_bytes() {
        // `0x16..=0x17` and `0xd7..=0xfa` are internal Op tags whose real wire form is a
        // prefix + sub-opcode.
        for b in [0xe3u8, 0xe4, 0xe5, 0xed, 0xf0, 0xf1, 0xf2, 0xd7, 0xdb, 0xfa, 0x16, 0x17] {
            assert_eq!(decode_body(&[b]), Err(DecodeError::UnsupportedOpcode));
        }
        // The real single-byte ops just below the range must still decode.
        assert!(decode_body(&[0xd1, 0x0b]).is_ok()); // ref.is_null
        assert!(decode_body(&[0xd4, 0x0b]).is_ok()); // ref.as_non_null
        assert!(decode_body(&[0xd6, 0x00, 0x0b]).is_ok()); // br_on_non_null <label>
    }

    #[test]
    fn from_u8_maps_wire_ops_only() {
        assert_eq!(Op::from_u8(0x6a), Some(Op::I32Add));
        assert_eq!(Op::from_u8(0x00), Some(Op::Unreachable));
        assert_eq!(Op::from_u8(0xd6), Some(Op::BrOnNonNull));
        // Prefix bytes and internal tags are not single-byte ops.
        assert_eq!(Op::from_u8(0xfb), None);
        assert_eq!(Op::from_u8(0xdb), None); // the SIMD internal tag
        assert_eq!(Op::from_u8(0xe3), None); // table.grow internal tag
        assert_eq!(Op::from_u8(0x16), None); // any.convert_extern internal tag
        assert_eq!(Op::from_u8(0x17), None); // extern.convert_any internal tag
    }

    #[test]
    fn text_names_round_trip() {
        // Every op with a text name must resolve back to itself — the property that keeps
        // the assembler's reverse map from drifting off the decoder's table.
        for b in 0u16..=0xff {
            if let Some(op) = Op::from_u8(b as u8) {
                let n = op.text_name();
                assert!(!n.is_empty(), "{op:?} has no text name");
                assert_eq!(Op::from_text_name(n), Some(op), "round-trip failed for {n}");
            }
        }
        // Spot-check the internal (prefixed-family) tags too.
        for op in [
            Op::MemoryInit,
            Op::DataDrop,
            Op::MemoryCopy,
            Op::MemoryFill,
            Op::TableInit,
            Op::ElemDrop,
            Op::TableCopy,
            Op::TableGrow,
            Op::TableSize,
            Op::TableFill,
            Op::ArrayNew,
            Op::ArrayNewDefault,
            Op::ArrayNewFixed,
            Op::ArrayGet,
            Op::ArrayGetS,
            Op::ArrayGetU,
            Op::ArraySet,
            Op::ArrayLen,
            Op::RefTest,
            Op::RefCastOp,
            Op::RefI31,
            Op::I31GetS,
            Op::I31GetU,
            Op::StructNew,
            Op::StructNewDefault,
            Op::StructGet,
            Op::StructGetS,
            Op::StructGetU,
            Op::StructSet,
            Op::BrOnCast,
            Op::BrOnCastFail,
            Op::AnyConvertExtern,
            Op::ExternConvertAny,
        ] {
            assert_eq!(Op::from_text_name(op.text_name()), Some(op));
        }
    }

    #[test]
    fn family_tags_have_no_text_name() {
        // `0xFD`/`0xFE` members are named by their sub-opcode tables, not by the tag.
        assert_eq!(Op::Simd.text_name(), "");
        assert_eq!(Op::Atomic.text_name(), "");
        assert_eq!(Op::from_text_name(""), None);
    }

    #[test]
    fn spot_check_text_names() {
        assert_eq!(Op::from_text_name("i32.add"), Some(Op::I32Add));
        assert_eq!(Op::from_text_name("local.get"), Some(Op::LocalGet));
        assert_eq!(Op::from_text_name("i64.trunc_sat_f64_u"), Some(Op::I64TruncSatF64U));
        assert_eq!(Op::from_text_name("memory.copy"), Some(Op::MemoryCopy));
        assert_eq!(Op::from_text_name("try_table"), Some(Op::TryTable));
        assert_eq!(Op::from_text_name("throw_ref"), Some(Op::ThrowRef));
        assert_eq!(Op::from_text_name("struct.get_u"), Some(Op::StructGetU));
        assert_eq!(Op::from_text_name("ref.cast"), Some(Op::RefCastOp));
        // `select` resolves to the untyped form; the typed one carries a sentinel name
        // that source text cannot spell, so it never shadows it.
        assert_eq!(Op::from_text_name("select"), Some(Op::Select));
        assert_eq!(Op::from_text_name("nope.nope"), None);
    }
}
