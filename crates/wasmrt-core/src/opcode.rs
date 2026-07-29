//! The shared WebAssembly instruction table and the byte-code → IR decoder.
//!
//! Ported from wazmrt `src/opcode.zig` (T2). This is the single opcode authority the
//! runtime is built around (Option A — see `cmem/architecture.md`): the same [`Op`] enum
//! and [`Instr`] IR feed validation and the switch interpreter, and (in reverse) the
//! assembler. [`decode_body`] turns a function body's raw bytes into a flat `Vec<Instr>`
//! with pre-parsed immediates.
//!
//! **Invariant (do not drift):** [`Op`] values `0xD7`–`0xFA` are **internal tags**, not
//! wire bytes — they name ops whose real encoding is `0xFB`/`0xFC` + a LEB sub-opcode.
//! [`decode_body`] rejects a raw byte in that range (accepting one would execute a
//! non-standard encoding as a real instruction). [`Op::from_u8`] maps only real
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
macro_rules! define_ops {
    (
        wire { $($w:ident = $wv:literal),* $(,)? }
        internal { $($i:ident = $iv:literal),* $(,)? }
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
            /// range `0xD7`–`0xFA` return `None`).
            #[must_use]
            pub const fn from_u8(b: u8) -> Option<Op> {
                match b {
                    $($wv => Some(Op::$w),)*
                    _ => None,
                }
            }
        }
    };
}

define_ops! {
    wire {
        // Control + exception handling (both encodings) + typed-ref calls.
        Unreachable = 0x00, Nop = 0x01, Block = 0x02, Loop = 0x03, If = 0x04, Else = 0x05,
        TryLegacy = 0x06, CatchLegacy = 0x07, Throw = 0x08, Rethrow = 0x09, ThrowRef = 0x0a,
        End = 0x0b, Br = 0x0c, BrIf = 0x0d, BrTable = 0x0e, Return = 0x0f, Call = 0x10,
        CallIndirect = 0x11, CallRef = 0x14, ReturnCallRef = 0x15, Delegate = 0x18,
        CatchAll = 0x19, TryTable = 0x1f,
        // Parametric.
        Drop = 0x1a, Select = 0x1b, SelectT = 0x1c,
        // Variable.
        LocalGet = 0x20, LocalSet = 0x21, LocalTee = 0x22, GlobalGet = 0x23, GlobalSet = 0x24,
        // Table access.
        TableGet = 0x25, TableSet = 0x26,
        // Memory.
        I32Load = 0x28, I64Load = 0x29, F32Load = 0x2a, F64Load = 0x2b,
        I32Load8S = 0x2c, I32Load8U = 0x2d, I32Load16S = 0x2e, I32Load16U = 0x2f,
        I64Load8S = 0x30, I64Load8U = 0x31, I64Load16S = 0x32, I64Load16U = 0x33,
        I64Load32S = 0x34, I64Load32U = 0x35, I32Store = 0x36, I64Store = 0x37,
        F32Store = 0x38, F64Store = 0x39, I32Store8 = 0x3a, I32Store16 = 0x3b,
        I64Store8 = 0x3c, I64Store16 = 0x3d, I64Store32 = 0x3e, MemorySize = 0x3f, MemoryGrow = 0x40,
        // Numeric constants.
        I32Const = 0x41, I64Const = 0x42, F32Const = 0x43, F64Const = 0x44,
        // Comparison — i32.
        I32Eqz = 0x45, I32Eq = 0x46, I32Ne = 0x47, I32LtS = 0x48, I32LtU = 0x49, I32GtS = 0x4a,
        I32GtU = 0x4b, I32LeS = 0x4c, I32LeU = 0x4d, I32GeS = 0x4e, I32GeU = 0x4f,
        // Comparison — i64.
        I64Eqz = 0x50, I64Eq = 0x51, I64Ne = 0x52, I64LtS = 0x53, I64LtU = 0x54, I64GtS = 0x55,
        I64GtU = 0x56, I64LeS = 0x57, I64LeU = 0x58, I64GeS = 0x59, I64GeU = 0x5a,
        // Comparison — f32 / f64.
        F32Eq = 0x5b, F32Ne = 0x5c, F32Lt = 0x5d, F32Gt = 0x5e, F32Le = 0x5f, F32Ge = 0x60,
        F64Eq = 0x61, F64Ne = 0x62, F64Lt = 0x63, F64Gt = 0x64, F64Le = 0x65, F64Ge = 0x66,
        // Numeric — i32.
        I32Clz = 0x67, I32Ctz = 0x68, I32Popcnt = 0x69, I32Add = 0x6a, I32Sub = 0x6b, I32Mul = 0x6c,
        I32DivS = 0x6d, I32DivU = 0x6e, I32RemS = 0x6f, I32RemU = 0x70, I32And = 0x71, I32Or = 0x72,
        I32Xor = 0x73, I32Shl = 0x74, I32ShrS = 0x75, I32ShrU = 0x76, I32Rotl = 0x77, I32Rotr = 0x78,
        // Numeric — i64.
        I64Clz = 0x79, I64Ctz = 0x7a, I64Popcnt = 0x7b, I64Add = 0x7c, I64Sub = 0x7d, I64Mul = 0x7e,
        I64DivS = 0x7f, I64DivU = 0x80, I64RemS = 0x81, I64RemU = 0x82, I64And = 0x83, I64Or = 0x84,
        I64Xor = 0x85, I64Shl = 0x86, I64ShrS = 0x87, I64ShrU = 0x88, I64Rotl = 0x89, I64Rotr = 0x8a,
        // Numeric — f32.
        F32Abs = 0x8b, F32Neg = 0x8c, F32Ceil = 0x8d, F32Floor = 0x8e, F32Trunc = 0x8f,
        F32Nearest = 0x90, F32Sqrt = 0x91, F32Add = 0x92, F32Sub = 0x93, F32Mul = 0x94, F32Div = 0x95,
        F32Min = 0x96, F32Max = 0x97, F32Copysign = 0x98,
        // Numeric — f64.
        F64Abs = 0x99, F64Neg = 0x9a, F64Ceil = 0x9b, F64Floor = 0x9c, F64Trunc = 0x9d,
        F64Nearest = 0x9e, F64Sqrt = 0x9f, F64Add = 0xa0, F64Sub = 0xa1, F64Mul = 0xa2, F64Div = 0xa3,
        F64Min = 0xa4, F64Max = 0xa5, F64Copysign = 0xa6,
        // Conversions.
        I32WrapI64 = 0xa7, I32TruncF32S = 0xa8, I32TruncF32U = 0xa9, I32TruncF64S = 0xaa,
        I32TruncF64U = 0xab, I64ExtendI32S = 0xac, I64ExtendI32U = 0xad, I64TruncF32S = 0xae,
        I64TruncF32U = 0xaf, I64TruncF64S = 0xb0, I64TruncF64U = 0xb1, F32ConvertI32S = 0xb2,
        F32ConvertI32U = 0xb3, F32ConvertI64S = 0xb4, F32ConvertI64U = 0xb5, F32DemoteF64 = 0xb6,
        F64ConvertI32S = 0xb7, F64ConvertI32U = 0xb8, F64ConvertI64S = 0xb9, F64ConvertI64U = 0xba,
        F64PromoteF32 = 0xbb, I32ReinterpretF32 = 0xbc, I64ReinterpretF64 = 0xbd,
        F32ReinterpretI32 = 0xbe, F64ReinterpretI64 = 0xbf,
        // Sign extension.
        I32Extend8S = 0xc0, I32Extend16S = 0xc1, I64Extend8S = 0xc2, I64Extend16S = 0xc3,
        I64Extend32S = 0xc4,
        // Saturating (non-trapping) float→int truncation. Real wire form is `0xFC 0x00..0x07`;
        // these bytes are also accepted as raw single-byte forms, mirroring the wazmrt oracle.
        I32TruncSatF32S = 0xc5, I32TruncSatF32U = 0xc6, I32TruncSatF64S = 0xc7, I32TruncSatF64U = 0xc8,
        I64TruncSatF32S = 0xc9, I64TruncSatF32U = 0xca, I64TruncSatF64S = 0xcb, I64TruncSatF64U = 0xcc,
        // Reference.
        RefNull = 0xd0, RefIsNull = 0xd1, RefFunc = 0xd2, RefEq = 0xd3, RefAsNonNull = 0xd4,
        BrOnNull = 0xd5, BrOnNonNull = 0xd6,
    }
    internal {
        // Bulk memory (`0xFC 0x08..0x0b`) + the SIMD / atomic family tags.
        MemoryInit = 0xd7, DataDrop = 0xd8, MemoryCopy = 0xd9, MemoryFill = 0xda,
        Simd = 0xdb, Atomic = 0xdc,
        // Table ops (`0xFC 0x0c..0x11`).
        TableInit = 0xe0, ElemDrop = 0xe1, TableCopy = 0xe2, TableGrow = 0xe3, TableSize = 0xe4,
        TableFill = 0xe5,
        // GC array ops (`0xFB` prefix).
        ArrayNew = 0xe6, ArrayNewDefault = 0xe7, ArrayNewFixed = 0xe8, ArrayGet = 0xe9,
        ArrayGetS = 0xea, ArrayGetU = 0xeb, ArraySet = 0xec, ArrayLen = 0xed,
        // GC casts (`0xFB` prefix).
        RefTest = 0xee, RefCastOp = 0xef,
        // GC i31 / struct ops (`0xFB` prefix) + cast branches.
        RefI31 = 0xf0, I31GetS = 0xf1, I31GetU = 0xf2, StructNew = 0xf3, StructNewDefault = 0xf4,
        StructGet = 0xf5, StructGetS = 0xf6, StructGetU = 0xf7, StructSet = 0xf8,
        BrOnCast = 0xf9, BrOnCastFail = 0xfa,
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

/// A decoded instruction: an opcode and its immediate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instr {
    pub op: Op,
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
        0x10 => ImmKind::Func,
        0x11 => ImmKind::CallIndirect,
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
        // GC ops with no immediate: ref.i31 / i31.get_s / i31.get_u, array.len.
        0xf0 | 0xf1 | 0xf2 | 0xed => ImmKind::None,
        // GC ops with a single type index.
        0xe6 | 0xe7 | 0xe9 | 0xea | 0xeb | 0xec | 0xf3 | 0xf4 => ImmKind::GcType,
        // GC struct ops with a type index + field index.
        0xf5..=0xf8 => ImmKind::GcField,
        // array.new_fixed: type index + element count.
        0xe8 => ImmKind::GcTypeN,
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
        _ => return Err(DecodeError::UnsupportedOpcode),
    })
}

/// Read a heap type (§ GC binary format): a non-negative `s33` is a concrete type index;
/// negative values are the abstract heap-type codes.
fn read_heap_type(r: &mut Reader) -> DecodeResult<HeapType> {
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
    Ok(Instr {
        op: Op::Simd,
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
        let b0 = r.read_byte()?;

        if b0 == 0xfb {
            // 0xFB-prefixed GC op: a LEB sub-opcode picks the internal Op tag.
            let instr = match r.read_var_u32()? {
                0x00 => Instr { op: Op::StructNew, imm: Imm::GcType(r.read_var_u32()?) },
                0x01 => Instr { op: Op::StructNewDefault, imm: Imm::GcType(r.read_var_u32()?) },
                0x02 => Instr { op: Op::StructGet, imm: read_gc_field(&mut r)? },
                0x03 => Instr { op: Op::StructGetS, imm: read_gc_field(&mut r)? },
                0x04 => Instr { op: Op::StructGetU, imm: read_gc_field(&mut r)? },
                0x05 => Instr { op: Op::StructSet, imm: read_gc_field(&mut r)? },
                0x06 => Instr { op: Op::ArrayNew, imm: Imm::GcType(r.read_var_u32()?) },
                0x07 => Instr { op: Op::ArrayNewDefault, imm: Imm::GcType(r.read_var_u32()?) },
                0x08 => Instr {
                    op: Op::ArrayNewFixed,
                    imm: Imm::GcTypeN { type_index: r.read_var_u32()?, n: r.read_var_u32()? },
                },
                0x0b => Instr { op: Op::ArrayGet, imm: Imm::GcType(r.read_var_u32()?) },
                0x0c => Instr { op: Op::ArrayGetS, imm: Imm::GcType(r.read_var_u32()?) },
                0x0d => Instr { op: Op::ArrayGetU, imm: Imm::GcType(r.read_var_u32()?) },
                0x0e => Instr { op: Op::ArraySet, imm: Imm::GcType(r.read_var_u32()?) },
                0x0f => Instr { op: Op::ArrayLen, imm: Imm::None },
                0x14 => Instr {
                    op: Op::RefTest,
                    imm: Imm::RefCast(RefType { nullable: false, heap: read_heap_type(&mut r)? }),
                },
                0x15 => Instr {
                    op: Op::RefTest,
                    imm: Imm::RefCast(RefType { nullable: true, heap: read_heap_type(&mut r)? }),
                },
                0x16 => Instr {
                    op: Op::RefCastOp,
                    imm: Imm::RefCast(RefType { nullable: false, heap: read_heap_type(&mut r)? }),
                },
                0x17 => Instr {
                    op: Op::RefCastOp,
                    imm: Imm::RefCast(RefType { nullable: true, heap: read_heap_type(&mut r)? }),
                },
                0x18 => Instr { op: Op::BrOnCast, imm: read_br_cast(&mut r)? },
                0x19 => Instr { op: Op::BrOnCastFail, imm: read_br_cast(&mut r)? },
                0x1c => Instr { op: Op::RefI31, imm: Imm::None },
                0x1d => Instr { op: Op::I31GetS, imm: Imm::None },
                0x1e => Instr { op: Op::I31GetU, imm: Imm::None },
                _ => return Err(DecodeError::UnsupportedOpcode),
            };
            list.push(instr);
            continue;
        }

        if b0 == 0xfc {
            // 0xFC-prefixed op: a LEB sub-opcode picks the internal Op tag.
            let instr = match r.read_var_u32()? {
                0x00 => Instr { op: Op::I32TruncSatF32S, imm: Imm::None },
                0x01 => Instr { op: Op::I32TruncSatF32U, imm: Imm::None },
                0x02 => Instr { op: Op::I32TruncSatF64S, imm: Imm::None },
                0x03 => Instr { op: Op::I32TruncSatF64U, imm: Imm::None },
                0x04 => Instr { op: Op::I64TruncSatF32S, imm: Imm::None },
                0x05 => Instr { op: Op::I64TruncSatF32U, imm: Imm::None },
                0x06 => Instr { op: Op::I64TruncSatF64S, imm: Imm::None },
                0x07 => Instr { op: Op::I64TruncSatF64U, imm: Imm::None },
                0x08 => {
                    let data = r.read_var_u32()?;
                    let mem = r.read_var_u32()?;
                    Instr { op: Op::MemoryInit, imm: Imm::MemInit { data, mem } }
                }
                0x09 => Instr { op: Op::DataDrop, imm: Imm::Data(r.read_var_u32()?) },
                0x0a => {
                    let dst = r.read_var_u32()?;
                    let src = r.read_var_u32()?;
                    Instr { op: Op::MemoryCopy, imm: Imm::MemCopy { dst, src } }
                }
                0x0b => Instr { op: Op::MemoryFill, imm: Imm::MemIndex(r.read_var_u32()?) },
                0x0c => {
                    let elem = r.read_var_u32()?;
                    let table = r.read_var_u32()?;
                    Instr { op: Op::TableInit, imm: Imm::TableInit { elem, table } }
                }
                0x0d => Instr { op: Op::ElemDrop, imm: Imm::Elem(r.read_var_u32()?) },
                0x0e => {
                    let dst = r.read_var_u32()?;
                    let src = r.read_var_u32()?;
                    Instr { op: Op::TableCopy, imm: Imm::TableCopy { dst, src } }
                }
                0x0f => Instr { op: Op::TableGrow, imm: Imm::Table(r.read_var_u32()?) },
                0x10 => Instr { op: Op::TableSize, imm: Imm::Table(r.read_var_u32()?) },
                0x11 => Instr { op: Op::TableFill, imm: Imm::Table(r.read_var_u32()?) },
                _ => return Err(DecodeError::UnsupportedOpcode),
            };
            list.push(instr);
            continue;
        }

        if b0 == 0xfd {
            let sub = r.read_var_u32()?;
            list.push(decode_simd(&mut r, sub)?);
            continue;
        }

        if b0 == 0xfe {
            let sub = r.read_var_u32()?;
            list.push(decode_atomic(&mut r, sub)?);
            continue;
        }

        // `0xd7..=0xfa` are internal tags whose real wire form is a `0xFB`/`0xFC`
        // prefix + sub-opcode (handled above). A raw byte in that range is not a valid
        // single-byte opcode. (`0xd0..=0xd6` are real ops; `0xfb..=0xfe` are prefixes.)
        if (0xd7..=0xfa).contains(&b0) {
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
            | ImmKind::RefCast
            | ImmKind::BrCast
            | ImmKind::Unsupported => return Err(DecodeError::UnsupportedOpcode),
        };

        let op = Op::from_u8(b0).ok_or(DecodeError::UnsupportedOpcode)?;
        list.push(Instr { op, imm });
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
        assert_eq!(instrs[0], Instr { op: Op::LocalGet, imm: Imm::Local(0) });
        assert_eq!(instrs[1], Instr { op: Op::LocalGet, imm: Imm::Local(1) });
        assert_eq!(instrs[2], Instr { op: Op::I32Add, imm: Imm::None });
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
        let body = [0x0e, 0x02, 0x00, 0x01, 0x02];
        let instrs = decode_body(&body).unwrap();
        assert_eq!(instrs.len(), 1);
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
        let body = [0xfd, 0x0c, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0];
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
        let instrs = decode_body(&[0xfc, 0x0a, 0x00, 0x00]).unwrap();
        assert_eq!(
            instrs[0],
            Instr { op: Op::MemoryCopy, imm: Imm::MemCopy { dst: 0, src: 0 } }
        );
    }

    #[test]
    fn rejects_unknown_opcode() {
        assert_eq!(decode_body(&[0xff]), Err(DecodeError::UnsupportedOpcode));
    }

    #[test]
    fn rejects_raw_internal_tag_bytes() {
        // `0xd7..=0xfa` are internal Op tags whose real wire form is a prefix + sub-opcode.
        for b in [0xe3u8, 0xe4, 0xe5, 0xed, 0xf0, 0xf1, 0xf2, 0xd7, 0xdb, 0xfa] {
            assert_eq!(decode_body(&[b]), Err(DecodeError::UnsupportedOpcode));
        }
        // The real single-byte ops just below the range must still decode.
        assert!(decode_body(&[0xd1, 0x0b]).is_ok()); // ref.is_null
        assert!(decode_body(&[0xd4, 0x0b]).is_ok()); // ref.as_non_null
        assert!(decode_body(&[0xd6, 0x00]).is_ok()); // br_on_non_null <label>
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
    }
}
