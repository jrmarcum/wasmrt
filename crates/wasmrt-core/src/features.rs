//! Proposal gating — which WebAssembly proposals this engine **accepts**.
//!
//! Ported from nothing: the frozen `wazmrt` oracle has no feature flags (it always runs
//! everything it implements). This is a T8 addition, driven by the C ABI's
//! `wasmrt_config_set_*` surface — an embedder restricting what a guest may use.
//!
//! **The rule that makes this honest: a flag exists only for a proposal wasmrt actually
//! implements.** A toggle for something unimplemented would be a no-op that reads as a
//! security control, which is the "fall-through" class `cmem/INDEX.md` forbids.
//!
//! ✅ **[`Feature::TailCall`] is that rule working as intended.** Through v0.9.0 there was
//! deliberately no such flag, because `return_call` / `return_call_indirect` (`0x12` / `0x13`)
//! were not in the opcode table and a toggle for them would have gated nothing. T9f implemented
//! them — as real frame replacement, not "call then return" — so the flag exists now, and not one
//! release earlier. Adding an enum value is additive, so `abi_version()` stays **1**.
//! `return_call_ref` belongs to function-references and is gated there.
//!
//! **Everything defaults ON** ([`Features::all`]), so the default path is byte-identical to
//! pre-T8 behaviour and the spec suite is unaffected. Gating only ever *rejects*; turning a
//! flag off can never make a module validate that would otherwise fail.
//!
//! The gate fires at **validation**, never at execution: a disabled proposal makes the
//! module invalid ([`crate::validate::ValidateError::FeatureDisabled`]), so nothing
//! partially-checked ever reaches the interpreter.

use core::fmt;

use crate::opcode::Op;
use crate::types::{RefHeap, ValType};

/// A WebAssembly proposal that can be individually disabled. Named in the error so an
/// embedder can report *which* proposal a module needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    /// `i32.extend8_s` … `i64.extend32_s`.
    SignExtension,
    /// `i32.trunc_sat_f32_s` … (`0xFC 0x00`–`0x07`).
    SaturatingFloatToInt,
    /// Blocks and functions returning more than one value.
    MultiValue,
    /// `funcref`/`externref` as value types, `ref.null`/`ref.func`/`ref.is_null`,
    /// `select` with an explicit type, the `table.*` accessors, and >1 table.
    ReferenceTypes,
    /// `memory.init`/`copy`/`fill`, `data.drop`, `table.init`/`copy`, `elem.drop`, and
    /// passive/declarative segments.
    BulkMemory,
    /// Arithmetic (`i32.add`/`sub`/`mul`, `i64.…`) inside a constant expression.
    ExtendedConst,
    /// The fixed-width `0xFD` vector family and the `v128` value type.
    Simd,
    /// The relaxed (implementation-defined-result) subset of `0xFD`, sub-opcodes
    /// `0x100`–`0x113`. Requires [`Feature::Simd`].
    RelaxedSimd,
    /// The `0xFE` atomic family and `shared` memories. (wasmrt executes these with
    /// single-threaded semantics — see `cmem/known-issues.md`.)
    Threads,
    /// More than one memory in a module.
    MultiMemory,
    /// 64-bit linear memories (`is64` limits). Tables stay 32-bit by a recorded invariant.
    Memory64,
    /// Typed function references: `call_ref`, `return_call_ref`, `ref.as_non_null`,
    /// `br_on_null`/`br_on_non_null`, concrete `(ref $t)` types and non-nullable refs.
    /// Requires [`Feature::ReferenceTypes`].
    FunctionReferences,
    /// WasmGC: struct/array types and ops, `i31`, `ref.eq`, casts and cast-branches, and
    /// the `any`/`eq`/`i31`/`struct`/`array`/`none` heap hierarchy. Requires
    /// [`Feature::FunctionReferences`].
    Gc,
    /// Exception handling, both encodings (`try_table`/`throw`/`throw_ref` and the legacy
    /// `try`/`catch`/`rethrow`), the tag section, and `exnref`.
    Exceptions,
    /// Tail calls: `return_call` and `return_call_indirect` (`0x12`/`0x13`), which **replace**
    /// the caller's frame rather than stacking on it.
    ///
    /// ⚠️ `return_call_ref` stays under [`Feature::FunctionReferences`], where it has always been:
    /// that proposal defines it, and its typed reference operand is not even expressible without
    /// it. Moving it here would change what an existing embedder's config rejects for no safety
    /// gain — both flags default on, and disabling function-references already removes it.
    TailCall,
    /// Wide arithmetic: `i64.add128`, `i64.sub128`, `i64.mul_wide_s`, `i64.mul_wide_u`
    /// (`0xFC 0x13`–`0x16`). Each carries a 128-bit value as a pair of i64 halves and returns
    /// two results, so it also needs [`Feature::MultiValue`] to be *usable* — but not to be
    /// *defined*, and the two are gated separately: a function type with two results is
    /// already refused by the multi-value gate, and adding a dependency here would refuse the
    /// instruction for a reason that is not about this proposal.
    WideArithmetic,
    /// Custom page sizes: a memory type may state its page size (flag bit 3 + an exponent) —
    /// 1 byte or 64 KiB. Gated on the flag, not the value: stating the default is still using
    /// the proposal, and wasm-tools refuses `(pagesize 65536)` without it too.
    CustomPageSizes,
    /// Custom descriptors: exact reference types `(ref null? (exact $t))`, descriptor/describes
    /// clauses, and the `*_desc` instructions. Defined on top of GC — requires [`Feature::Gc`].
    CustomDescriptors,
}

impl Feature {
    /// Every proposal, in declaration order — which is also `wasmrt_feature_t`'s integer order in
    /// `wasmrt.h` (`Feature::ALL[n]` is C value `n`). Those integers are FROZEN: append, never insert.
    ///
    /// Kept as a written list on purpose, and checked from both sides so it cannot fall behind: the
    /// core test counts `Features`' FIELDS against it, and the C-ABI test parses `wasmrt.h` against
    /// it. (The first version of this check compared a hand-written list with itself, so a new
    /// proposal missing from it — `CustomPageSizes`, 2026-09-19 — passed. See that test.)
    pub const ALL: [Feature; 18] = [
        Feature::SignExtension,
        Feature::SaturatingFloatToInt,
        Feature::MultiValue,
        Feature::ReferenceTypes,
        Feature::BulkMemory,
        Feature::ExtendedConst,
        Feature::Simd,
        Feature::RelaxedSimd,
        Feature::Threads,
        Feature::MultiMemory,
        Feature::Memory64,
        Feature::FunctionReferences,
        Feature::Gc,
        Feature::Exceptions,
        Feature::TailCall,
        Feature::WideArithmetic,
        Feature::CustomPageSizes,
        Feature::CustomDescriptors,
    ];

    /// The stable lower-case name, matching the proposal's repository name. Used by the C
    /// ABI's error text and by the CLI.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Feature::SignExtension => "sign-extension-ops",
            Feature::SaturatingFloatToInt => "nontrapping-float-to-int-conversions",
            Feature::MultiValue => "multi-value",
            Feature::ReferenceTypes => "reference-types",
            Feature::BulkMemory => "bulk-memory-operations",
            Feature::ExtendedConst => "extended-const",
            Feature::Simd => "simd",
            Feature::RelaxedSimd => "relaxed-simd",
            Feature::Threads => "threads",
            Feature::MultiMemory => "multi-memory",
            Feature::Memory64 => "memory64",
            Feature::FunctionReferences => "function-references",
            Feature::Gc => "gc",
            Feature::Exceptions => "exception-handling",
            Feature::TailCall => "tail-call",
            Feature::WideArithmetic => "wide-arithmetic",
            Feature::CustomPageSizes => "custom-page-sizes",
            Feature::CustomDescriptors => "custom-descriptors",
        }
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The set of proposals an engine accepts. **All on by default** — full wasmtime
/// browser-standard parity plus memory64, which is wasmrt's stated scope
/// (`cmem/design-decisions.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Features {
    pub sign_extension: bool,
    pub saturating_float_to_int: bool,
    pub multi_value: bool,
    pub reference_types: bool,
    pub bulk_memory: bool,
    pub extended_const: bool,
    pub simd: bool,
    pub relaxed_simd: bool,
    pub threads: bool,
    pub multi_memory: bool,
    pub memory64: bool,
    pub function_references: bool,
    pub gc: bool,
    pub exceptions: bool,
    pub tail_call: bool,
    pub wide_arithmetic: bool,
    pub custom_page_sizes: bool,
    pub custom_descriptors: bool,
}

impl Default for Features {
    fn default() -> Self {
        Features::all()
    }
}

/// A `Features` set that cannot be satisfied because a proposal is enabled without one it
/// is defined on top of. Reported rather than silently repaired: quietly enabling `simd`
/// because `relaxed_simd` was asked for would accept modules the embedder meant to refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncoherentFeatures {
    /// The proposal that was enabled.
    pub enabled: Feature,
    /// The proposal it depends on, which was disabled.
    pub requires: Feature,
}

impl fmt::Display for IncoherentFeatures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} requires {}", self.enabled, self.requires)
    }
}

impl core::error::Error for IncoherentFeatures {}

impl Features {
    /// Every proposal wasmrt implements, enabled. The default, and what plain
    /// [`crate::validate::validate`] uses.
    #[must_use]
    pub const fn all() -> Features {
        Features {
            sign_extension: true,
            saturating_float_to_int: true,
            multi_value: true,
            reference_types: true,
            bulk_memory: true,
            extended_const: true,
            simd: true,
            relaxed_simd: true,
            threads: true,
            multi_memory: true,
            memory64: true,
            function_references: true,
            gc: true,
            exceptions: true,
            tail_call: true,
            wide_arithmetic: true,
            custom_page_sizes: true,
            custom_descriptors: true,
        }
    }

    /// The STANDARDIZED language — WebAssembly 3.0 — without the proposals that are not part of it
    /// (threads, wide-arithmetic, custom-page-sizes, custom-descriptors). What the spec suite's CORE
    /// files are written against: a proposal can change the validity of existing constructs (custom-
    /// descriptors relaxes `br_on_cast`), so the core files must not see one they do not expect.
    #[must_use]
    pub const fn standard() -> Features {
        let mut f = Features::all();
        f.threads = false;
        f.wide_arithmetic = false;
        f.custom_page_sizes = false;
        f.custom_descriptors = false;
        f
    }

    /// The WebAssembly 1.0 core language: every post-MVP proposal disabled.
    #[must_use]
    pub const fn mvp() -> Features {
        Features {
            sign_extension: false,
            saturating_float_to_int: false,
            multi_value: false,
            reference_types: false,
            bulk_memory: false,
            extended_const: false,
            simd: false,
            relaxed_simd: false,
            threads: false,
            multi_memory: false,
            memory64: false,
            function_references: false,
            gc: false,
            exceptions: false,
            tail_call: false,
            wide_arithmetic: false,
            custom_page_sizes: false,
            custom_descriptors: false,
        }
    }

    /// Is `f` enabled?
    #[must_use]
    pub const fn has(&self, f: Feature) -> bool {
        match f {
            Feature::SignExtension => self.sign_extension,
            Feature::SaturatingFloatToInt => self.saturating_float_to_int,
            Feature::MultiValue => self.multi_value,
            Feature::ReferenceTypes => self.reference_types,
            Feature::BulkMemory => self.bulk_memory,
            Feature::ExtendedConst => self.extended_const,
            Feature::Simd => self.simd,
            Feature::RelaxedSimd => self.relaxed_simd,
            Feature::Threads => self.threads,
            Feature::MultiMemory => self.multi_memory,
            Feature::Memory64 => self.memory64,
            Feature::FunctionReferences => self.function_references,
            Feature::Gc => self.gc,
            Feature::Exceptions => self.exceptions,
            Feature::TailCall => self.tail_call,
            Feature::WideArithmetic => self.wide_arithmetic,
            Feature::CustomPageSizes => self.custom_page_sizes,
            Feature::CustomDescriptors => self.custom_descriptors,
        }
    }

    /// Set `f` on or off by name (the C ABI's setters funnel through here).
    pub const fn set(&mut self, f: Feature, on: bool) {
        match f {
            Feature::SignExtension => self.sign_extension = on,
            Feature::SaturatingFloatToInt => self.saturating_float_to_int = on,
            Feature::MultiValue => self.multi_value = on,
            Feature::ReferenceTypes => self.reference_types = on,
            Feature::BulkMemory => self.bulk_memory = on,
            Feature::ExtendedConst => self.extended_const = on,
            Feature::Simd => self.simd = on,
            Feature::RelaxedSimd => self.relaxed_simd = on,
            Feature::Threads => self.threads = on,
            Feature::MultiMemory => self.multi_memory = on,
            Feature::Memory64 => self.memory64 = on,
            Feature::FunctionReferences => self.function_references = on,
            Feature::Gc => self.gc = on,
            Feature::Exceptions => self.exceptions = on,
            Feature::TailCall => self.tail_call = on,
            Feature::WideArithmetic => self.wide_arithmetic = on,
            Feature::CustomPageSizes => self.custom_page_sizes = on,
            Feature::CustomDescriptors => self.custom_descriptors = on,
        }
    }

    /// Reject a set that enables a proposal without one it is layered on. Checked once,
    /// when the set is handed to the engine, so validation itself never has to reason
    /// about dependencies.
    ///
    /// The layering is the proposals' own: GC is specified on top of function-references,
    /// which is specified on top of reference-types; relaxed SIMD extends SIMD; and the
    /// exception proposal's `exnref` is a reference type.
    pub const fn check_coherent(&self) -> Result<(), IncoherentFeatures> {
        macro_rules! require {
            ($enabled:ident => $dep:ident, $ef:expr, $df:expr) => {
                if self.$enabled && !self.$dep {
                    return Err(IncoherentFeatures {
                        enabled: $ef,
                        requires: $df,
                    });
                }
            };
        }
        require!(gc => function_references, Feature::Gc, Feature::FunctionReferences);
        require!(function_references => reference_types,
                 Feature::FunctionReferences, Feature::ReferenceTypes);
        require!(relaxed_simd => simd, Feature::RelaxedSimd, Feature::Simd);
        require!(custom_descriptors => gc, Feature::CustomDescriptors, Feature::Gc);
        require!(exceptions => reference_types, Feature::Exceptions, Feature::ReferenceTypes);
        Ok(())
    }
}

/// The proposal an opcode belongs to, or `None` for WebAssembly 1.0 core instructions.
///
/// **One authority.** Every gate in the validator reads this table, so an instruction
/// cannot be gated in one place and forgotten in another — and a new opcode added to
/// [`crate::opcode`] shows up here as an explicit decision rather than silently defaulting
/// to "always allowed", because the match below is exhaustive over `Op`.
#[must_use]
pub const fn op_feature(op: Op) -> Option<Feature> {
    use Op::*;
    Some(match op {
        // --- exception handling (both encodings) ---
        TryLegacy | CatchLegacy | Throw | Rethrow | ThrowRef | Delegate | CatchAll
        | TryTable => Feature::Exceptions,

        // --- tail calls ---
        ReturnCall | ReturnCallIndirect => Feature::TailCall,

        // --- typed function references ---
        CallRef | ReturnCallRef | RefAsNonNull | BrOnNull | BrOnNonNull => {
            Feature::FunctionReferences
        }

        // --- reference types ---
        SelectT | TableGet | TableSet | RefNull | RefIsNull | RefFunc | TableGrow
        | TableSize | TableFill => Feature::ReferenceTypes,

        // --- bulk memory + table ---
        MemoryInit | DataDrop | MemoryCopy | MemoryFill | TableInit | ElemDrop
        | TableCopy => Feature::BulkMemory,

        // --- vectors. The relaxed subset is decided by sub-opcode, not by the family
        // tag, so `Simd` here is the floor; `simd_sub_feature` refines it. ---
        Simd => Feature::Simd,

        // --- threads / atomics ---
        Atomic => Feature::Threads,

        // --- WasmGC (the whole 0xFB family, plus `ref.eq`) ---
        RefEq | ArrayNew | ArrayNewDefault | ArrayNewFixed | ArrayGet | ArrayGetS
        | ArrayGetU | ArraySet | ArrayLen | RefTest | RefCastOp | RefI31 | I31GetS
        | I31GetU | StructNew | StructNewDefault | StructGet | StructGetS | StructGetU
        | StructSet | BrOnCast | BrOnCastFail | AnyConvertExtern | ExternConvertAny => {
            Feature::Gc
        }

        // --- sign extension ---
        I32Extend8S | I32Extend16S | I64Extend8S | I64Extend16S | I64Extend32S => {
            Feature::SignExtension
        }

        // --- wide arithmetic ---
        I64Add128 | I64Sub128 | I64MulWideS | I64MulWideU => Feature::WideArithmetic,

        // --- custom-descriptors ---
        StructNewDesc | StructNewDefaultDesc | RefGetDesc | RefCastDescEq | BrOnCastDescEq
        | BrOnCastDescEqFail => Feature::CustomDescriptors,

        // --- non-trapping float→int ---
        I32TruncSatF32S | I32TruncSatF32U | I32TruncSatF64S | I32TruncSatF64U
        | I64TruncSatF32S | I64TruncSatF32U | I64TruncSatF64S | I64TruncSatF64U => {
            Feature::SaturatingFloatToInt
        }

        // Everything else is WebAssembly 1.0 and cannot be gated.
        _ => return None,
    })
}

/// The ops [`op_feature`] deliberately leaves ungated, by internal tag byte — WebAssembly 1.0,
/// which no flag can switch off.
///
/// ⚠️⚠️ **`op_feature`'s doc comment used to claim the match was "exhaustive over `Op`", so that
/// "a new opcode shows up here as an explicit decision rather than silently defaulting to
/// 'always allowed'". IT IS NOT — it ends in `_ => return None`**, and the four wide-arithmetic
/// ops landed ungated because of it: added to the enum, wired through decoder, assembler,
/// validator and interpreter, and refusable by no flag at all. That is X3's failure mode
/// exactly — *a proposal that ships without a gate is not "enabled by default", it is
/// UNREFUSABLE* — and the comment asserting the protection is the reason nobody would look.
///
/// 🔒 So the property is pinned by DATA instead of by a claim. Adding an op now either gives it
/// a feature (this list does not move) or shows up here as a visible, deliberate diff.
/// `best-practices.md`: a gate that cannot fail is decoration — and a gate that exists only in
/// a doc comment is not even that.
#[cfg(test)]
const UNGATED_CORE_OPS: usize = 178;

/// Lowest relaxed-SIMD sub-opcode in the `0xFD` space (`i8x16.relaxed_swizzle`).
pub const RELAXED_SIMD_FIRST: u32 = 0x100;
/// Highest relaxed-SIMD sub-opcode (`i16x8.relaxed_dot_i8x16_i7x16_add_s`).
pub const RELAXED_SIMD_LAST: u32 = 0x113;

/// The proposal a `0xFD` sub-opcode belongs to: relaxed SIMD for `0x100`–`0x113`, plain
/// SIMD otherwise.
#[must_use]
pub const fn simd_sub_feature(sub: u32) -> Feature {
    if sub >= RELAXED_SIMD_FIRST && sub <= RELAXED_SIMD_LAST {
        Feature::RelaxedSimd
    } else {
        Feature::Simd
    }
}

/// The proposal a **value type** belongs to, or `None` for `i32`/`i64`/`f32`/`f64`.
///
/// Used wherever a value type is declared — parameters, results, locals, globals, struct
/// and array fields, `select`'s explicit type — so a disabled proposal cannot slip in
/// through a *type* when its instructions are all rejected.
///
/// Table **element** types are checked separately ([`table_element_feature`]): a `funcref`
/// table is WebAssembly 1.0, whereas a `funcref` *parameter* is reference-types.
#[must_use]
pub fn val_type_feature(v: ValType) -> Option<Feature> {
    if v == ValType::V128 {
        return Some(Feature::Simd);
    }
    if !v.is_ref() {
        return None; // i32 / i64 / f32 / f64
    }
    // An EXACT reference is custom-descriptors (wasm-tools: "custom descriptors required for exact
    // reference types") — checked before the concrete rule, which would otherwise answer first.
    if v.is_exact() {
        return Some(Feature::CustomDescriptors);
    }
    // A concrete `(ref $t)` is function-references regardless of its family head.
    if v.is_concrete() {
        return Some(Feature::FunctionReferences);
    }
    match v.ref_heap() {
        RefHeap::Exn | RefHeap::NoExn => Some(Feature::Exceptions),
        // The bottoms of the func and extern hierarchies are GC additions too — they do not exist
        // in reference-types, which has only `funcref`/`externref`.
        RefHeap::Any | RefHeap::Eq | RefHeap::I31 | RefHeap::Struct | RefHeap::Array
        | RefHeap::None | RefHeap::NoFunc | RefHeap::NoExtern => Some(Feature::Gc),
        // `funcref`/`externref` as a value type is reference-types; the *non-nullable*
        // spellings `(ref func)` / `(ref extern)` need function-references.
        RefHeap::Func | RefHeap::Extern => {
            if v.is_non_null_ref() {
                Some(Feature::FunctionReferences)
            } else {
                Some(Feature::ReferenceTypes)
            }
        }
    }
}

/// The proposal a **table element type** belongs to. `funcref` is WebAssembly 1.0 (the
/// only element type an MVP table may have); anything else follows [`val_type_feature`].
#[must_use]
pub fn table_element_feature(v: ValType) -> Option<Feature> {
    if v == ValType::FUNCREF {
        None
    } else {
        val_type_feature(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_the_default_and_is_coherent() {
        assert_eq!(Features::default(), Features::all());
        assert!(Features::all().check_coherent().is_ok());
        assert!(Features::mvp().check_coherent().is_ok());
    }

    #[test]
    fn all_enables_every_feature_and_mvp_enables_none() {
        for f in EVERY {
            assert!(Features::all().has(f), "all() must enable {f}");
            assert!(!Features::mvp().has(f), "mvp() must disable {f}");
        }
    }

    #[test]
    fn set_round_trips_every_feature_independently() {
        for f in EVERY {
            let mut fs = Features::all();
            fs.set(f, false);
            assert!(!fs.has(f), "{f} did not clear");
            // Exactly one flag moved — `set` must not touch its neighbours.
            let moved = EVERY.iter().filter(|&&g| fs.has(g) != Features::all().has(g)).count();
            assert_eq!(moved, 1, "setting {f} disturbed another flag");
        }
    }

    #[test]
    fn incoherent_sets_are_rejected_not_repaired() {
        let mut fs = Features::all();
        fs.gc = true;
        fs.function_references = false;
        assert_eq!(
            fs.check_coherent(),
            Err(IncoherentFeatures {
                enabled: Feature::Gc,
                requires: Feature::FunctionReferences
            })
        );

        let mut fs = Features::all();
        fs.relaxed_simd = true;
        fs.simd = false;
        assert_eq!(
            fs.check_coherent(),
            Err(IncoherentFeatures {
                enabled: Feature::RelaxedSimd,
                requires: Feature::Simd
            })
        );

        // Turning the dependent OFF is always fine — SIMD without relaxed SIMD is a
        // perfectly ordinary configuration.
        let mut fs = Features::all();
        fs.relaxed_simd = false;
        assert!(fs.check_coherent().is_ok());
    }

    #[test]
    fn core_instructions_are_ungated() {
        for op in [
            Op::I32Add,
            Op::Block,
            Op::Call,
            Op::CallIndirect,
            Op::I32Load,
            Op::MemoryGrow,
            Op::Select,
            Op::Drop,
            Op::End,
            Op::F64Sqrt,
        ] {
            assert_eq!(op_feature(op), None, "{op:?} is WebAssembly 1.0");
        }
    }

    #[test]
    fn each_proposal_family_maps_to_its_proposal() {
        assert_eq!(op_feature(Op::TryTable), Some(Feature::Exceptions));
        assert_eq!(op_feature(Op::Delegate), Some(Feature::Exceptions));
        assert_eq!(op_feature(Op::CallRef), Some(Feature::FunctionReferences));
        assert_eq!(op_feature(Op::ReturnCallRef), Some(Feature::FunctionReferences));
        assert_eq!(op_feature(Op::TableGet), Some(Feature::ReferenceTypes));
        assert_eq!(op_feature(Op::MemoryCopy), Some(Feature::BulkMemory));
        assert_eq!(op_feature(Op::Simd), Some(Feature::Simd));
        assert_eq!(op_feature(Op::Atomic), Some(Feature::Threads));
        assert_eq!(op_feature(Op::StructNew), Some(Feature::Gc));
        assert_eq!(op_feature(Op::RefEq), Some(Feature::Gc));
        assert_eq!(op_feature(Op::I32Extend8S), Some(Feature::SignExtension));
        assert_eq!(
            op_feature(Op::I32TruncSatF32S),
            Some(Feature::SaturatingFloatToInt)
        );
    }

    #[test]
    fn relaxed_simd_is_a_sub_opcode_range_not_a_family() {
        assert_eq!(simd_sub_feature(0x0c), Feature::Simd); // v128.const
        assert_eq!(simd_sub_feature(0x0e), Feature::Simd); // i8x16.swizzle
        assert_eq!(simd_sub_feature(0xff), Feature::Simd);
        assert_eq!(simd_sub_feature(0x100), Feature::RelaxedSimd); // relaxed_swizzle
        assert_eq!(simd_sub_feature(0x113), Feature::RelaxedSimd); // relaxed_dot_add
        assert_eq!(simd_sub_feature(0x114), Feature::Simd); // past the end
    }

    #[test]
    fn value_types_carry_their_proposal() {
        assert_eq!(val_type_feature(ValType::I32), None);
        assert_eq!(val_type_feature(ValType::F64), None);
        assert_eq!(val_type_feature(ValType::V128), Some(Feature::Simd));
        assert_eq!(val_type_feature(ValType::FUNCREF), Some(Feature::ReferenceTypes));
        assert_eq!(
            val_type_feature(ValType::EXTERNREF),
            Some(Feature::ReferenceTypes)
        );
        assert_eq!(
            val_type_feature(ValType::FUNCREF_NN),
            Some(Feature::FunctionReferences)
        );
        assert_eq!(val_type_feature(ValType::ANYREF), Some(Feature::Gc));
        assert_eq!(val_type_feature(ValType::I31REF), Some(Feature::Gc));
        assert_eq!(val_type_feature(ValType::NULLREF), Some(Feature::Gc));
        assert_eq!(val_type_feature(ValType::EXNREF), Some(Feature::Exceptions));
        assert_eq!(
            val_type_feature(ValType::concrete_ref(true, RefHeap::Struct, 3)),
            Some(Feature::FunctionReferences)
        );
    }

    #[test]
    fn a_funcref_table_is_mvp_but_a_funcref_parameter_is_not() {
        assert_eq!(table_element_feature(ValType::FUNCREF), None);
        assert_eq!(val_type_feature(ValType::FUNCREF), Some(Feature::ReferenceTypes));
        assert_eq!(
            table_element_feature(ValType::EXTERNREF),
            Some(Feature::ReferenceTypes)
        );
    }

    const EVERY: [Feature; 15] = [
        Feature::SignExtension,
        Feature::SaturatingFloatToInt,
        Feature::MultiValue,
        Feature::ReferenceTypes,
        Feature::BulkMemory,
        Feature::ExtendedConst,
        Feature::Simd,
        Feature::RelaxedSimd,
        Feature::Threads,
        Feature::MultiMemory,
        Feature::Memory64,
        Feature::FunctionReferences,
        Feature::Gc,
        Feature::Exceptions,
        Feature::TailCall,
    ];

    /// ⚠️ **The flag must GATE something.** This module's own doc says a toggle for an
    /// unimplemented proposal is a no-op that reads as a security control — so the test for a new
    /// flag is not that it exists, it is that turning it off changes the verdict, and that turning
    /// it on leaves the module valid. Both directions, or it is decoration.
    #[test]
    fn the_tail_call_flag_actually_gates_tail_calls() {
        // (module (func $f (return_call $f)))
        let wat = br#"(module (func $f (return_call $f)))"#;
        let bytes = crate::wat::assemble(wat).expect("assemble");
        let module = crate::module::decode(&bytes).expect("decode");

        assert!(
            crate::validate::validate_with_features(&module, &Features::all()).is_ok(),
            "enabled: a tail call must validate"
        );

        let mut off = Features::all();
        off.tail_call = false;
        assert_eq!(
            crate::validate::validate_with_features(&module, &off),
            Err(crate::validate::ValidateError::FeatureDisabled(Feature::TailCall)),
            "disabled: the module must be refused, naming tail-call"
        );

        // And the neighbouring flag must NOT gate it — `return_call` is not a function-references
        // instruction, and a flag that rejects more than it claims is its own kind of wrong.
        let mut no_funcrefs = Features::all();
        no_funcrefs.function_references = false;
        no_funcrefs.gc = false; // gc requires function-references; keep the set coherent
        assert!(
            crate::validate::validate_with_features(&module, &no_funcrefs).is_ok(),
            "return_call must not be gated by function-references"
        );
    }

    /// Every `Op` is either gated by a proposal or deliberately listed as WebAssembly 1.0.
    ///
    /// ⚠️⚠️ **This exists because the property it checks was ASSERTED IN A DOC COMMENT and was
    /// false.** [`op_feature`] said the match was "exhaustive over `Op`", so "a new opcode shows
    /// up here as an explicit decision rather than silently defaulting to 'always allowed'". It
    /// ends in `_ => return None`. The four wide-arithmetic ops were added to the enum, wired
    /// through the decoder, the assembler, the validator and the interpreter — and were
    /// refusable by no flag, because the catch-all quietly classified them as core 1.0. That is
    /// X3's failure mode word for word: *a proposal that ships without a gate is not "enabled by
    /// default", it is UNREFUSABLE.*
    ///
    /// 🔒 The count is the pin. Add an op and either it carries a feature (this number does not
    /// move) or the number changes and the diff asks why.
    #[test]
    fn every_op_is_gated_or_deliberately_core() {
        let ungated: Vec<&str> = Op::ALL
            .iter()
            .filter(|op| op_feature(**op).is_none())
            .map(|op| op.text_name())
            .collect();
        assert_eq!(
            ungated.len(),
            UNGATED_CORE_OPS,
            "the set of UNGATED ops moved. Every entry here is refusable by no flag at all, so \
             a proposal instruction among them is unrefusable. Gate it, or update the count \
             deliberately.\n{ungated:?}"
        );
        // And the four that were the reason for this test, by name, since a count alone would
        // also be satisfied by gating one op and ungating another.
        for op in [Op::I64Add128, Op::I64Sub128, Op::I64MulWideS, Op::I64MulWideU] {
            assert_eq!(
                op_feature(op),
                Some(Feature::WideArithmetic),
                "{} must be gated by wide-arithmetic",
                op.text_name()
            );
        }
    }

    /// X3 for track W: the flag refuses its own proposal, and refuses nothing else.
    #[test]
    fn the_wide_arithmetic_flag_refuses_wide_arithmetic_and_nothing_else() {
        let module = crate::module::decode(
            &crate::wat::assemble(
                br#"(module (func (param i64 i64) (result i64 i64)
                      (i64.mul_wide_u (local.get 0) (local.get 1))))"#,
            )
            .expect("must assemble"),
        )
        .expect("must decode");

        assert!(
            crate::validate::validate_with_features(&module, &Features::all()).is_ok(),
            "enabled: wide arithmetic must validate"
        );

        let mut off = Features::all();
        off.wide_arithmetic = false;
        assert_eq!(
            crate::validate::validate_with_features(&module, &off),
            Err(crate::validate::ValidateError::FeatureDisabled(Feature::WideArithmetic)),
            "disabled: the module must be refused, naming wide-arithmetic"
        );

        // The no-false-positive half: a plain MVP module must not care about this flag.
        let plain = crate::module::decode(
            &crate::wat::assemble(
                br#"(module (func (param i64 i64) (result i64)
                      (i64.add (local.get 0) (local.get 1))))"#,
            )
            .expect("must assemble"),
        )
        .expect("must decode");
        assert!(
            crate::validate::validate_with_features(&plain, &off).is_ok(),
            "a flag that rejects more than it claims is its own kind of wrong"
        );
    }

    /// **T10b — the proposal list has THREE spellings, and nothing made them agree.**
    /// `Feature` (this enum), `Features`' struct fields, and `wasmrt_feature_t` in
    /// `wasmrt.h` + `feature_of` in the C ABI. Adding `WideArithmetic` updated the first two
    /// and the third was missed on the first pass, which would have shipped a proposal an
    /// embedder could not switch off **through the C ABI only** — enabled in-process, gated at
    /// one entry point of two. X3 asks for "gated at BOTH module entry points"; this is the
    /// check that notices when it is one.
    ///
    /// 🔒 The C ABI's integers are FROZEN, so this walks upward from 0 and fails on the first
    /// gap: a new feature must be APPENDED, never inserted, or every embedder's compiled
    /// constant silently changes meaning.
    #[test]
    fn every_feature_is_reachable_through_the_c_abi_by_a_stable_integer() {
        let all = Feature::ALL;
        // ⚠️⚠️ **This test once compared a hand-written list with ITSELF.** Its `all` was a literal
        // array and its size check was `all.len() == 16` — so `CustomPageSizes`, added to the enum,
        // the struct and the C header but not to that array, PASSED. A list can only be checked
        // against something that grows on its own. `Features` does: a proposal cannot be gated
        // without a field, and `Debug` prints every field. The C header is checked in `wasmrt-capi`.
        let fields = alloc::format!("{:?}", Features::all()).matches(": true").count();
        assert_eq!(
            fields,
            all.len(),
            "`Features` has {fields} flags but `Feature::ALL` lists {} — a proposal is missing              from `Feature::ALL` (append it; the C integers follow this order)",
            all.len()
        );
        // Spelling 2: the struct. `set` then `has` must round-trip, or a flag exists in the
        // enum with no field behind it.
        for f in all {
            let mut fs = Features::all();
            fs.set(f, false);
            assert!(!fs.has(f), "{f} has no field behind it: set(false) did not take");
            fs.set(f, true);
            assert!(fs.has(f), "{f}: set(true) did not take");
        }
        // Spelling 3: the C ABI integers, contiguous from 0 and in this same order. The
        // assertion is on the ORDER, not just membership — an inserted feature would shift
        // every later integer and break an already-compiled embedder.
        // The FROZEN prefix: integers an embedder may already have compiled keep their meaning.
        assert_eq!(all[15], Feature::WideArithmetic, "C value 15 is frozen");
        assert_eq!(all[16], Feature::CustomPageSizes, "C value 16 is frozen");
        assert_eq!(all[17], Feature::CustomDescriptors, "C value 17 is frozen");
    }

    /// custom-page-sizes is gated on the FLAG — stating the default `(pagesize 65536)` is still
    /// using the proposal, as wasm-tools has it — and refuses nothing else.
    #[test]
    fn the_custom_page_sizes_flag_refuses_a_stated_page_size_and_nothing_else() {
        let mut off = Features::all();
        off.custom_page_sizes = false;
        for src in [&b"(module (memory 1 (pagesize 1)))"[..], b"(module (memory 1 (pagesize 65536)))"] {
            let m = crate::module::decode(&crate::wat::assemble(src).unwrap()).unwrap();
            assert_eq!(
                crate::validate::validate_with_features(&m, &off),
                Err(crate::validate::ValidateError::FeatureDisabled(Feature::CustomPageSizes))
            );
            assert!(crate::validate::validate_with_features(&m, &Features::all()).is_ok());
        }
        let plain = crate::module::decode(&crate::wat::assemble(b"(module (memory 1))").unwrap()).unwrap();
        assert!(crate::validate::validate_with_features(&plain, &off).is_ok());
    }
}
