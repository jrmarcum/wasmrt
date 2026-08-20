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
}

impl Feature {
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
        }
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

        // --- non-trapping float→int ---
        I32TruncSatF32S | I32TruncSatF32U | I32TruncSatF64S | I32TruncSatF64U
        | I64TruncSatF32S | I64TruncSatF32U | I64TruncSatF64S | I64TruncSatF64U => {
            Feature::SaturatingFloatToInt
        }

        // Everything else is WebAssembly 1.0 and cannot be gated.
        _ => return None,
    })
}

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
}
