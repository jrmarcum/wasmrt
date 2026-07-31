//! Execution: instantiate a validated module and run its functions with a switch-dispatched
//! interpreter over the `opcode` IR (Option A — see `cmem/architecture.md`).
//!
//! Ported from wazmrt `src/interp.zig` (T5). Values are untyped `u64` slots — validation has
//! proven the types, so the stack carries raw bits and each opcode reinterprets. Control flow
//! uses a per-call label stack + a branch-target table precomputed once per function (matching
//! `end`/`else` for every `block`/`loop`/`if`).
//!
//! **Scope this release (v0.6.0): integer compute.** i32/i64 arithmetic/comparison/bitwise,
//! structured control flow, direct `call` (incl. recursion), locals, globals, and constants —
//! enough to run a compute module (`fib`, `factorial`, `add`). Float arithmetic and linear
//! memory land in 0.6.1; tables, reference types, GC, SIMD, threads, and exception handling in
//! later 0.6.x slices. **Anything not yet executed traps loudly** ([`Trap::UnsupportedInstruction`]),
//! never silent-wrong. Modules with imports are rejected for now ([`Trap::ImportsUnsupported`]).

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::module::{CompKind, CompType, FuncType, Module, StorageType};
use crate::opcode::{decode_body, BlockType, HeapType, Imm, Instr, Op, RefType};
use crate::reader::Reader;
use crate::types::{DecodeError, RefHeap};

/// A runtime value: a raw 128-bit slot reinterpreted per the (validated) type.
///
/// The slot is 128 bits wide so a `v128` (SIMD) fits in a **single** stack/local/table/global
/// entry — keeping the whole interpreter on the "one slot per value" model (arity, `select`,
/// `drop`, branch copies, locals, and call marshaling never have to reason about slot width).
/// This is an idiomatic-Rust divergence from wazmrt, which stores a `v128` as two `u64` slots
/// and carries width tables to size `drop`/`select`; observable behavior is identical. Scalars
/// (`i32`/`i64`/`f32`/`f64`) and references live in the low bits, high 64 zero.
pub type Value = u128;

#[must_use]
pub fn i32_value(x: i32) -> Value {
    Value::from(x as u32)
}
#[must_use]
pub fn as_i32(v: Value) -> i32 {
    v as u32 as i32
}
#[must_use]
pub fn i64_value(x: i64) -> Value {
    Value::from(x as u64)
}
#[must_use]
pub fn as_i64(v: Value) -> i64 {
    v as u64 as i64
}
#[must_use]
pub fn f32_value(x: f32) -> Value {
    Value::from(x.to_bits())
}
#[must_use]
pub fn as_f32(v: Value) -> f32 {
    f32::from_bits(v as u32)
}
#[must_use]
pub fn f64_value(x: f64) -> Value {
    Value::from(x.to_bits())
}
#[must_use]
pub fn as_f64(v: Value) -> f64 {
    f64::from_bits(v as u64)
}

/// Cap on guest call depth (a `call` recurses natively, so this bounds host stack use).
const MAX_CALL_DEPTH: usize = 512;

/// WebAssembly linear-memory page size (64 KiB).
pub const PAGE_SIZE: usize = 64 * 1024;

/// Default ceiling on total linear memory per instance (summed across memories), applied at
/// instantiation and at `memory.grow`. A tiny module can declare gigabytes, so this bounds a
/// hostile input. 1 GiB is far above any realistic guest.
const DEFAULT_MAX_MEMORY_BYTES: usize = 1 << 30;

/// Default ceiling on total defined-table entries per instance. Each entry is an eagerly
/// allocated `Value`, so a tiny module's unvalidated `min` (up to 2^32-1) could otherwise
/// demand tens of GiB. 2^27 (~1 GiB of slots) is far above any realistic guest.
const DEFAULT_MAX_TABLE_ELEMS: usize = 1 << 27;

/// The null-reference sentinel — a value-stack `ref.null`, and an uninitialized table entry.
/// A funcref value is a (small) function index, so it never collides.
///
/// **Invariant (do not drift):** `null_ref` (all bits set) is checked *before* [`I31_TAG`]
/// (bit 63), so the two never confuse (`cmem/design-decisions.md`).
pub const NULL_REF: Value = u64::MAX as Value;

/// Tag bit marking a value slot as an unboxed i31 (WasmGC). Set on `ref.i31` results so
/// `ref.test`/`ref.cast` can tell an i31 from a heap-object index (bit 63 clear) within the
/// `any` hierarchy. Checked *after* [`NULL_REF`] (all bits set), so the two never confuse.
const I31_TAG: Value = 1 << 63;
const _: () = assert!(I31_TAG == (1u128 << 63)); // i31 tag lives in the low 64 bits

/// Cap on live GC objects per instance. There is no collector (a proposal-scope decision), so
/// this backstop keeps a guest allocation loop from exhausting host memory.
const MAX_GC_OBJECTS: usize = 1 << 24;

/// A heap-allocated GC object: its declared type index (RTT) + its struct fields / array
/// elements. One `Value` (128-bit) per field — enough for every field type incl. `v128`.
struct HeapObject {
    type_index: u32,
    fields: Vec<Value>,
}

/// Shared linear memory. `bytes` is `alloc_zeroed`-backed (demand-zero pages on mainstream
/// OSes), so a large declared minimum costs address space, not resident memory.
pub struct Memory {
    pub bytes: Vec<u8>,
    pub max: Option<u64>,
    pub is64: bool,
    /// A `shared` memory (threads proposal) — required by `memory.atomic.wait*`.
    pub shared: bool,
}

/// A reference table: `Value` slots (`NULL_REF` = uninitialized; a funcref is its function
/// index) so funcref and externref tables share one representation.
pub struct Table {
    pub entries: Vec<Value>,
    pub max: Option<u32>,
}

/// The mutable runtime state of an instance, threaded as `&mut` through execution so a
/// recursive `call` reborrows it cleanly.
struct Store {
    globals: Vec<Value>,
    memories: Vec<Memory>,
    tables: Vec<Table>,
    /// `data_dropped[i]` marks a data segment consumed — active segments are dropped once
    /// instantiation copies them in (§4.5.4); `data.drop` marks a passive one.
    data_dropped: Vec<bool>,
    /// Evaluated reference values of each element segment (for `table.init`).
    elem_values: Vec<Vec<Value>>,
    /// `elem_dropped[i]` marks a segment consumed (active/declarative at init, or `elem.drop`).
    elem_dropped: Vec<bool>,
    /// GC heap: one entry per allocated struct/array; a GC reference value is its index here.
    /// No collector — objects live for the instance's lifetime, bounded by `MAX_GC_OBJECTS`.
    gc_heap: Vec<HeapObject>,
}

/// A runtime trap or an execution-setup error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trap {
    Unreachable,
    DivByZero,
    IntOverflow,
    /// A trapping float→int conversion of NaN, infinity, or an out-of-range value.
    InvalidConversionToInt,
    CallStackExhausted,
    UndefinedExport,
    BadArgCount,
    UndefinedFunc,
    UndefinedGlobal,
    UndefinedLabel,
    StackUnderflow,
    UnbalancedControl,
    /// A memory instruction in a module with no linear memory (or a bad memory index).
    NoMemory,
    /// A memory access (or data-segment init) outside the memory bounds.
    MemoryOutOfBounds,
    /// The declared/grown linear memory exceeds this instance's budget.
    MemoryLimitExceeded,
    /// An atomic access whose effective address is not naturally aligned to its width.
    UnalignedAtomic,
    /// `memory.atomic.wait*` on a non-shared memory.
    ExpectedSharedMemory,
    /// A `memory.init` / `data.drop` data-segment index out of range.
    UndefinedData,
    /// `call_indirect` in a module with no table (or a bad table index).
    NoTable,
    /// A table access (or element-segment init) outside the table bounds.
    TableOutOfBounds,
    /// The declared/grown table exceeds this instance's budget.
    TableLimitExceeded,
    /// `call_indirect` hit an uninitialized (null) table element.
    UninitializedElement,
    /// `call_indirect`'s declared type did not match the callee's signature.
    IndirectTypeMismatch,
    /// A type index out of range.
    UndefinedType,
    /// A `table.init` / `elem.drop` element-segment index out of range.
    UndefinedElement,
    /// A null reference where a non-null one is required (`call_ref` / `ref.as_non_null`).
    NullReference,
    /// A GC struct field / array element access outside the object's bounds.
    GcOutOfBounds,
    /// The instance allocated more than `MAX_GC_OBJECTS` GC objects (no collector).
    GcHeapExhausted,
    /// `ref.cast` to a type the value is not an instance of.
    CastFailure,
    /// A constant expression used an opcode this slice doesn't evaluate.
    ConstantExpr,
    /// An opcode this interpreter slice does not execute yet (float/memory/tables/GC/SIMD/EH).
    UnsupportedInstruction,
    /// This release runs only import-free modules.
    ImportsUnsupported,
    /// A body failed to decode at instantiation.
    Decode(DecodeError),
}

impl From<DecodeError> for Trap {
    fn from(e: DecodeError) -> Self {
        Trap::Decode(e)
    }
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Trap::Unreachable => f.write_str("unreachable executed"),
            Trap::DivByZero => f.write_str("integer divide by zero"),
            Trap::IntOverflow => f.write_str("integer overflow"),
            Trap::InvalidConversionToInt => f.write_str("invalid conversion to integer"),
            Trap::CallStackExhausted => f.write_str("call stack exhausted"),
            Trap::UndefinedExport => f.write_str("no such exported function"),
            Trap::BadArgCount => f.write_str("wrong number of arguments"),
            Trap::UndefinedFunc => f.write_str("function index out of range"),
            Trap::UndefinedGlobal => f.write_str("global index out of range"),
            Trap::UndefinedLabel => f.write_str("branch label out of range"),
            Trap::StackUnderflow => f.write_str("operand stack underflow"),
            Trap::UnbalancedControl => f.write_str("unbalanced control instruction"),
            Trap::NoMemory => f.write_str("no linear memory"),
            Trap::MemoryOutOfBounds => f.write_str("out of bounds memory access"),
            Trap::MemoryLimitExceeded => f.write_str("memory exceeds the instance budget"),
            Trap::UnalignedAtomic => f.write_str("unaligned atomic access"),
            Trap::ExpectedSharedMemory => f.write_str("atomic wait on non-shared memory"),
            Trap::UndefinedData => f.write_str("data segment index out of range"),
            Trap::NoTable => f.write_str("no table"),
            Trap::TableOutOfBounds => f.write_str("out of bounds table access"),
            Trap::TableLimitExceeded => f.write_str("table exceeds the instance budget"),
            Trap::UninitializedElement => f.write_str("uninitialized table element"),
            Trap::IndirectTypeMismatch => f.write_str("indirect call type mismatch"),
            Trap::UndefinedType => f.write_str("type index out of range"),
            Trap::UndefinedElement => f.write_str("element segment index out of range"),
            Trap::NullReference => f.write_str("null reference"),
            Trap::GcOutOfBounds => f.write_str("out of bounds GC access"),
            Trap::GcHeapExhausted => f.write_str("GC heap exhausted"),
            Trap::CastFailure => f.write_str("cast failure"),
            Trap::ConstantExpr => f.write_str("unsupported constant expression"),
            Trap::UnsupportedInstruction => {
                f.write_str("instruction not executed in this release (float/memory/tables/GC/SIMD/EH)")
            }
            Trap::ImportsUnsupported => f.write_str("modules with imports are not runnable yet"),
            Trap::Decode(e) => write!(f, "decode error at instantiation: {e}"),
        }
    }
}

impl core::error::Error for Trap {}

/// A trap or setup result.
pub type Result<T> = core::result::Result<T, Trap>;

/// A defined function prepared for execution (compute slice: one slot per local/value).
struct FuncBody {
    ty: FuncType,
    /// params + declared locals (one `u64` slot each — no v128 in this slice).
    num_locals: usize,
    ir: Vec<Instr>,
    /// For each `block`/`loop`/`if`/`else` index: the matching `end` index.
    end_of: Vec<usize>,
    /// For each `if` index: the `else` index, or `ir.len()` if none.
    else_of: Vec<usize>,
}

/// An instantiated module ready to run. Owns its `Module` (so it outlives the input and the
/// "instance retains its module" invariant holds by construction).
pub struct Instance {
    module: Module,
    func_bodies: Vec<FuncBody>,
    store: Store,
}

/// Immutable execution context (read-only during a call); the mutable [`Store`] is threaded
/// separately as `&mut` so a recursive `call` reborrows it cleanly.
struct Ctx<'a> {
    module: &'a Module,
    func_bodies: &'a [FuncBody],
}

impl Instance {
    /// Instantiate a decoded module. This slice runs import-free modules only.
    pub fn new(module: Module) -> Result<Instance> {
        if !module.imports.is_empty() {
            return Err(Trap::ImportsUnsupported);
        }
        if module.functions.len() != module.code.len() {
            return Err(Trap::UndefinedFunc);
        }

        // Evaluate defined-global initializers (imported globals are rejected above).
        let mut globals: Vec<Value> = Vec::with_capacity(module.global_inits.len());
        for init in &module.global_inits {
            let v = eval_const_expr(init, &globals)?;
            globals.push(v);
        }

        // Linear memories: allocate each defined memory sized to its declared minimum
        // (demand-zero via `vec![0; n]`), bounded by the per-instance budget.
        let mut memories: Vec<Memory> = Vec::with_capacity(module.memories.len());
        let mut total_bytes: usize = 0;
        for mt in &module.memories {
            let min_pages = usize::try_from(mt.limits.min).map_err(|_| Trap::MemoryLimitExceeded)?;
            let nbytes = min_pages
                .checked_mul(PAGE_SIZE)
                .ok_or(Trap::MemoryLimitExceeded)?;
            total_bytes = total_bytes
                .checked_add(nbytes)
                .filter(|&t| t <= DEFAULT_MAX_MEMORY_BYTES)
                .ok_or(Trap::MemoryLimitExceeded)?;
            memories.push(Memory {
                bytes: vec![0u8; nbytes],
                max: mt.limits.max,
                is64: mt.limits.is64,
                shared: mt.limits.shared,
            });
        }

        // Apply active data segments, then mark them (and only them) dropped (§4.5.4).
        for seg in &module.data {
            if !seg.active {
                continue;
            }
            let mem = memories
                .get_mut(seg.mem_index as usize)
                .ok_or(Trap::NoMemory)?;
            let offset = eval_const_offset(&seg.offset_expr, &globals, mem.is64)?;
            let start = usize::try_from(offset).map_err(|_| Trap::MemoryOutOfBounds)?;
            let end = start
                .checked_add(seg.bytes.len())
                .filter(|&e| e <= mem.bytes.len())
                .ok_or(Trap::MemoryOutOfBounds)?;
            mem.bytes[start..end].copy_from_slice(&seg.bytes);
        }
        let data_dropped: Vec<bool> = module.data.iter().map(|s| s.active).collect();

        // Tables: allocate each defined table sized to its minimum, filled with `NULL_REF`,
        // bounded by the per-instance entry budget.
        let mut tables: Vec<Table> = Vec::with_capacity(module.tables.len());
        let mut total_elems: usize = 0;
        for tt in &module.tables {
            let min = usize::try_from(tt.limits.min).map_err(|_| Trap::TableLimitExceeded)?;
            total_elems = total_elems
                .checked_add(min)
                .filter(|&t| t <= DEFAULT_MAX_TABLE_ELEMS)
                .ok_or(Trap::TableLimitExceeded)?;
            tables.push(Table {
                entries: vec![NULL_REF; min],
                max: tt.limits.max.and_then(|m| u32::try_from(m).ok()),
            });
        }

        // Evaluate element segments to reference values; apply the active ones to their table
        // (then drop them and the declarative ones; passive stay for `table.init`).
        let mut elem_values: Vec<Vec<Value>> = Vec::with_capacity(module.elements.len());
        let mut elem_dropped: Vec<bool> = Vec::with_capacity(module.elements.len());
        for elem in &module.elements {
            let mut vals: Vec<Value> = Vec::with_capacity(elem.funcs.len() + elem.exprs.len());
            vals.extend(elem.funcs.iter().map(|&f| Value::from(f)));
            for ex in &elem.exprs {
                vals.push(eval_const_expr(ex, &globals)?);
            }
            if elem.mode == crate::module::ElementMode::Active {
                let tbl = tables
                    .get_mut(elem.table_index as usize)
                    .ok_or(Trap::NoTable)?;
                let offset = eval_const_offset(&elem.offset_expr, &globals, false)?; // tables are 32-bit
                let start = usize::try_from(offset).map_err(|_| Trap::TableOutOfBounds)?;
                let end = start
                    .checked_add(vals.len())
                    .filter(|&e| e <= tbl.entries.len())
                    .ok_or(Trap::TableOutOfBounds)?;
                tbl.entries[start..end].copy_from_slice(&vals);
            }
            elem_dropped.push(elem.mode != crate::module::ElementMode::Passive);
            elem_values.push(vals);
        }

        // Prepare each defined function: decode its body + precompute control flow.
        let mut func_bodies = Vec::with_capacity(module.functions.len());
        for (&type_index, code) in module.functions.iter().zip(&module.code) {
            let ty = module.func_sig(type_index).ok_or(Trap::UndefinedFunc)?;
            let num_locals = ty.params.len() + code.local_count() as usize;
            let ir = decode_body(&code.body)?;
            let (end_of, else_of) = precompute_control_flow(&ir)?;
            func_bodies.push(FuncBody {
                ty,
                num_locals,
                ir,
                end_of,
                else_of,
            });
        }

        Ok(Instance {
            module,
            func_bodies,
            store: Store {
                globals,
                memories,
                tables,
                data_dropped,
                elem_values,
                elem_dropped,
                gc_heap: Vec::new(),
            },
        })
    }

    /// The wrapped module.
    #[must_use]
    pub fn module(&self) -> &Module {
        &self.module
    }

    fn find_exported_func(&self, name: &str) -> Option<u32> {
        self.module.exports.iter().find_map(|e| {
            if e.name == name && e.ty.kind() == crate::types::ExternKind::Func {
                Some(e.index)
            } else {
                None
            }
        })
    }

    /// Invoke an exported function by name.
    pub fn invoke(&mut self, name: &str, args: &[Value]) -> Result<Vec<Value>> {
        let func_index = self.find_exported_func(name).ok_or(Trap::UndefinedExport)?;
        self.invoke_index(func_index, args)
    }

    /// Invoke a function by its index in the function index space.
    pub fn invoke_index(&mut self, func_index: u32, args: &[Value]) -> Result<Vec<Value>> {
        let ft = self.module.func_type(func_index).ok_or(Trap::UndefinedFunc)?;
        if args.len() != ft.params.len() {
            return Err(Trap::BadArgCount);
        }
        let ctx = Ctx {
            module: &self.module,
            func_bodies: &self.func_bodies,
        };
        call_function(&ctx, &mut self.store, func_index, args, 1)
    }
}

/// Match every `block`/`loop`/`if` with its `end`, and every `if` with its `else`.
fn precompute_control_flow(ir: &[Instr]) -> Result<(Vec<usize>, Vec<usize>)> {
    let mut end_of = vec![0usize; ir.len()];
    let mut else_of = vec![ir.len(); ir.len()]; // sentinel = "no else"
    let mut stack: Vec<usize> = Vec::new();
    for (i, instr) in ir.iter().enumerate() {
        match instr.op {
            // try/try_table also open a block-shaped construct; push so nesting stays balanced
            // even though their handlers aren't executed in this slice.
            Op::Block | Op::Loop | Op::If | Op::TryTable | Op::TryLegacy => stack.push(i),
            Op::Else => {
                let &opener = stack.last().ok_or(Trap::UnbalancedControl)?;
                else_of[opener] = i;
            }
            Op::End => {
                if let Some(opener) = stack.pop() {
                    end_of[opener] = i;
                    if else_of[opener] != ir.len() {
                        end_of[else_of[opener]] = i;
                    }
                }
                // else: the function's implicit final `end`.
            }
            _ => {}
        }
    }
    Ok((end_of, else_of))
}

/// A control label on a frame's label stack.
#[derive(Clone, Copy)]
struct Label {
    is_loop: bool,
    /// Slots carried on a branch (results for block/if, params for loop).
    arity: u32,
    /// pc to jump to on a branch to this label.
    target: usize,
    /// Value-stack height below this construct's operands.
    stack_base: usize,
}

struct Frame<'a> {
    body: &'a FuncBody,
    locals: Vec<Value>,
    vstack: Vec<Value>,
    labels: Vec<Label>,
}

impl Frame<'_> {
    fn push(&mut self, v: Value) {
        self.vstack.push(v);
    }
    /// Pop one slot; a short stack (only reachable from an unvalidated body) yields a defined
    /// 0 and is turned into a trap by `run` before the next instruction consumes it unsafely.
    fn pop(&mut self) -> Value {
        self.vstack.pop().unwrap_or(0)
    }
    fn push_i32(&mut self, v: i32) {
        self.push(i32_value(v));
    }
    fn pop_i32(&mut self) -> i32 {
        as_i32(self.pop())
    }
    fn push_i64(&mut self, v: i64) {
        self.push(i64_value(v));
    }
    fn pop_i64(&mut self) -> i64 {
        as_i64(self.pop())
    }
    fn push_f32(&mut self, v: f32) {
        self.push(f32_value(v));
    }
    fn pop_f32(&mut self) -> f32 {
        as_f32(self.pop())
    }
    fn push_f64(&mut self, v: f64) {
        self.push(f64_value(v));
    }
    fn pop_f64(&mut self) -> f64 {
        as_f64(self.pop())
    }
    /// Pop a memory address/count: i64 for a memory64 memory, else i32 (zero-extended).
    fn pop_mem(&mut self, is64: bool) -> u64 {
        if is64 {
            self.pop_i64() as u64
        } else {
            u64::from(self.pop_i32() as u32)
        }
    }

    fn stack_base(&self, n: usize) -> Result<usize> {
        self.vstack.len().checked_sub(n).ok_or(Trap::StackUnderflow)
    }

    fn branch(&mut self, n: u32) -> Result<usize> {
        if n as usize >= self.labels.len() {
            return Err(Trap::UndefinedLabel);
        }
        let label = self.labels[self.labels.len() - 1 - n as usize];
        let arity = label.arity as usize;
        let from = self.stack_base(arity)?;
        if from < label.stack_base {
            return Err(Trap::StackUnderflow);
        }
        self.vstack.copy_within(from..from + arity, label.stack_base);
        self.vstack.truncate(label.stack_base + arity);
        // A loop-continue keeps the loop's own label; a forward exit pops it too.
        let keep = if label.is_loop {
            self.labels.len() - n as usize
        } else {
            self.labels.len() - (n as usize + 1)
        };
        self.labels.truncate(keep);
        Ok(label.target)
    }
}

/// Branch/label arity in slots (compute slice: one slot per value).
fn block_arity(ctx: &Ctx, bt: BlockType, want_params: bool) -> u32 {
    match bt {
        BlockType::Empty => 0,
        BlockType::Value(_) => u32::from(!want_params),
        BlockType::TypeIndex(i) => ctx.module.func_sig(i).map_or(0, |ft| {
            (if want_params {
                ft.params.len()
            } else {
                ft.results.len()
            }) as u32
        }),
    }
}

fn call_function(
    ctx: &Ctx,
    store: &mut Store,
    func_index: u32,
    args: &[Value],
    depth: usize,
) -> Result<Vec<Value>> {
    if depth > MAX_CALL_DEPTH {
        return Err(Trap::CallStackExhausted);
    }
    // Import-free (checked at instantiation), so every function is defined.
    let defined = func_index as usize;
    let body = ctx.func_bodies.get(defined).ok_or(Trap::UndefinedFunc)?;

    let mut locals = vec![0 as Value; body.num_locals];
    let n_args = args.len().min(locals.len());
    locals[..n_args].copy_from_slice(&args[..n_args]);

    let mut frame = Frame {
        body,
        locals,
        vstack: Vec::new(),
        labels: vec![Label {
            is_loop: false,
            arity: body.ty.results.len() as u32,
            target: body.ir.len(),
            stack_base: 0,
        }],
    };
    run(&mut frame, ctx, store, depth)?;

    let n = body.ty.results.len();
    let base = frame.stack_base(n)?;
    Ok(frame.vstack[base..].to_vec())
}

fn run(frame: &mut Frame, ctx: &Ctx, store: &mut Store, depth: usize) -> Result<()> {
    let body = frame.body;
    let ir = &body.ir;
    let mut pc = 0usize;
    while pc < ir.len() {
        let instr = &ir[pc];
        match instr.op {
            Op::Nop => pc += 1,
            Op::Unreachable => return Err(Trap::Unreachable),
            Op::Drop => {
                frame.pop();
                pc += 1;
            }
            Op::Select | Op::SelectT => {
                let c = frame.pop_i32();
                let b = frame.pop();
                let a = frame.pop();
                frame.push(if c != 0 { a } else { b });
                pc += 1;
            }

            // --- Constants ---
            Op::I32Const => {
                let Imm::I32(x) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                frame.push_i32(x);
                pc += 1;
            }
            Op::I64Const => {
                let Imm::I64(x) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                frame.push_i64(x);
                pc += 1;
            }
            Op::F32Const => {
                let Imm::F32(bits) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                frame.push(Value::from(bits));
                pc += 1;
            }
            Op::F64Const => {
                let Imm::F64(bits) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                frame.push(Value::from(bits));
                pc += 1;
            }

            // --- Variables ---
            Op::LocalGet => {
                let i = local_index(instr)?;
                let v = *frame.locals.get(i).ok_or(Trap::StackUnderflow)?;
                frame.push(v);
                pc += 1;
            }
            Op::LocalSet => {
                let i = local_index(instr)?;
                let v = frame.pop();
                *frame.locals.get_mut(i).ok_or(Trap::StackUnderflow)? = v;
                pc += 1;
            }
            Op::LocalTee => {
                let i = local_index(instr)?;
                let v = *frame.vstack.last().ok_or(Trap::StackUnderflow)?;
                *frame.locals.get_mut(i).ok_or(Trap::StackUnderflow)? = v;
                pc += 1;
            }
            Op::GlobalGet => {
                let Imm::Global(gi) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let v = *store.globals.get(gi as usize).ok_or(Trap::UndefinedGlobal)?;
                frame.push(v);
                pc += 1;
            }
            Op::GlobalSet => {
                let Imm::Global(gi) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let v = frame.pop();
                *store.globals.get_mut(gi as usize).ok_or(Trap::UndefinedGlobal)? = v;
                pc += 1;
            }

            // --- Structured control flow ---
            Op::Block => {
                let bt = block_type(instr)?;
                let params = block_arity(ctx, bt, true);
                let arity = block_arity(ctx, bt, false);
                let stack_base = frame.stack_base(params as usize)?;
                frame.labels.push(Label {
                    is_loop: false,
                    arity,
                    target: body.end_of[pc] + 1,
                    stack_base,
                });
                pc += 1;
            }
            Op::Loop => {
                let bt = block_type(instr)?;
                let params = block_arity(ctx, bt, true);
                let stack_base = frame.stack_base(params as usize)?;
                frame.labels.push(Label {
                    is_loop: true,
                    arity: params,
                    target: pc + 1,
                    stack_base,
                });
                pc += 1;
            }
            Op::If => {
                let c = frame.pop_i32();
                let bt = block_type(instr)?;
                let params = block_arity(ctx, bt, true);
                let arity = block_arity(ctx, bt, false);
                let stack_base = frame.stack_base(params as usize)?;
                frame.labels.push(Label {
                    is_loop: false,
                    arity,
                    target: body.end_of[pc] + 1,
                    stack_base,
                });
                if c != 0 {
                    pc += 1;
                } else {
                    let else_idx = body.else_of[pc];
                    pc = if else_idx != ir.len() {
                        else_idx + 1
                    } else {
                        body.end_of[pc]
                    };
                }
            }
            Op::Else => pc = body.end_of[pc], // end of then-branch: skip to matching end
            Op::End => {
                frame.labels.pop();
                pc += 1;
            }
            Op::Br => pc = frame.branch(label_imm(instr)?)?,
            Op::BrIf => {
                if frame.pop_i32() != 0 {
                    pc = frame.branch(label_imm(instr)?)?;
                } else {
                    pc += 1;
                }
            }
            Op::BrTable => {
                let Imm::BrTable(bt) = &instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let i = frame.pop_i32();
                let idx = if i >= 0 && (i as usize) < bt.labels.len() {
                    bt.labels[i as usize]
                } else {
                    bt.default
                };
                pc = frame.branch(idx)?;
            }
            Op::Return => pc = ir.len(),

            Op::Call => {
                let Imm::Func(f) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let ft = ctx.module.func_type(f).ok_or(Trap::UndefinedFunc)?;
                let np = ft.params.len();
                let base = frame.stack_base(np)?;
                let args = frame.vstack[base..].to_vec();
                let results = call_function(ctx, store, f, &args, depth + 1)?;
                frame.vstack.truncate(base);
                frame.vstack.extend_from_slice(&results);
                pc += 1;
            }

            // --- Linear memory (loads/stores, size/grow) ---
            Op::I32Load
            | Op::I64Load
            | Op::F32Load
            | Op::F64Load
            | Op::I32Load8S
            | Op::I32Load8U
            | Op::I32Load16S
            | Op::I32Load16U
            | Op::I64Load8S
            | Op::I64Load8U
            | Op::I64Load16S
            | Op::I64Load16U
            | Op::I64Load32S
            | Op::I64Load32U
            | Op::I32Store
            | Op::I64Store
            | Op::F32Store
            | Op::F64Store
            | Op::I32Store8
            | Op::I32Store16
            | Op::I64Store8
            | Op::I64Store16
            | Op::I64Store32
            | Op::MemorySize
            | Op::MemoryGrow => {
                exec_memory(frame, store, instr)?;
                pc += 1;
            }
            Op::MemoryCopy => {
                exec_memory_copy(frame, store, instr)?;
                pc += 1;
            }
            Op::MemoryFill => {
                exec_memory_fill(frame, store, instr)?;
                pc += 1;
            }
            Op::MemoryInit => {
                exec_memory_init(frame, ctx, store, instr)?;
                pc += 1;
            }
            Op::DataDrop => {
                let Imm::Data(d) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                *store
                    .data_dropped
                    .get_mut(d as usize)
                    .ok_or(Trap::UndefinedData)? = true;
                pc += 1;
            }

            // --- Reference types --- (funcref = function index; NULL_REF = null)
            Op::RefNull => {
                frame.push(NULL_REF);
                pc += 1;
            }
            Op::RefIsNull => {
                let r = frame.pop();
                frame.push_i32(i32::from(r == NULL_REF));
                pc += 1;
            }
            Op::RefFunc => {
                let Imm::Func(f) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                frame.push(Value::from(f));
                pc += 1;
            }
            Op::RefAsNonNull => {
                let r = frame.pop();
                if r == NULL_REF {
                    return Err(Trap::NullReference);
                }
                frame.push(r);
                pc += 1;
            }
            Op::BrOnNull => {
                let r = frame.pop();
                if r == NULL_REF {
                    pc = frame.branch(label_imm(instr)?)?; // null → branch (ref dropped)
                } else {
                    frame.push(r); // non-null → keep the ref, fall through
                    pc += 1;
                }
            }
            Op::BrOnNonNull => {
                let r = frame.pop();
                if r == NULL_REF {
                    pc += 1; // null → ref consumed, fall through
                } else {
                    frame.push(r); // non-null → keep the ref for the label
                    pc = frame.branch(label_imm(instr)?)?;
                }
            }
            Op::CallRef | Op::ReturnCallRef => {
                let f_ref = frame.pop();
                if f_ref == NULL_REF {
                    return Err(Trap::NullReference);
                }
                let f = f_ref as u32;
                let ft = ctx.module.func_type(f).ok_or(Trap::UndefinedFunc)?;
                let base = frame.stack_base(ft.params.len())?;
                let args = frame.vstack[base..].to_vec();
                let results = call_function(ctx, store, f, &args, depth + 1)?;
                frame.vstack.truncate(base);
                frame.vstack.extend_from_slice(&results);
                pc = if instr.op == Op::ReturnCallRef {
                    ir.len()
                } else {
                    pc + 1
                };
            }

            // --- call_indirect: table lookup + runtime type check ---
            Op::CallIndirect => {
                let Imm::CallIndirect(ci) = &instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let slot = frame.pop_i32() as u32 as usize;
                let entry = *store
                    .tables
                    .get(ci.table as usize)
                    .ok_or(Trap::NoTable)?
                    .entries
                    .get(slot)
                    .ok_or(Trap::TableOutOfBounds)?;
                if entry == NULL_REF {
                    return Err(Trap::UninitializedElement);
                }
                let f = entry as u32;
                let want = ctx.module.func_sig(ci.type_index).ok_or(Trap::UndefinedType)?;
                let got = ctx.module.func_type(f).ok_or(Trap::UndefinedFunc)?;
                if want.params != got.params || want.results != got.results {
                    return Err(Trap::IndirectTypeMismatch);
                }
                let base = frame.stack_base(got.params.len())?;
                let args = frame.vstack[base..].to_vec();
                let results = call_function(ctx, store, f, &args, depth + 1)?;
                frame.vstack.truncate(base);
                frame.vstack.extend_from_slice(&results);
                pc += 1;
            }

            // --- Table access ---
            Op::TableGet => {
                let ti = table_imm(instr)? as usize;
                let i = frame.pop_i32() as u32 as usize;
                let v = *store
                    .tables
                    .get(ti)
                    .ok_or(Trap::NoTable)?
                    .entries
                    .get(i)
                    .ok_or(Trap::TableOutOfBounds)?;
                frame.push(v);
                pc += 1;
            }
            Op::TableSet => {
                let ti = table_imm(instr)? as usize;
                let v = frame.pop();
                let i = frame.pop_i32() as u32 as usize;
                let slot = store
                    .tables
                    .get_mut(ti)
                    .ok_or(Trap::NoTable)?
                    .entries
                    .get_mut(i)
                    .ok_or(Trap::TableOutOfBounds)?;
                *slot = v;
                pc += 1;
            }
            Op::TableSize => {
                let ti = table_imm(instr)? as usize;
                let len = store.tables.get(ti).ok_or(Trap::NoTable)?.entries.len();
                frame.push_i32(len as i32);
                pc += 1;
            }
            Op::TableGrow => {
                let ti = table_imm(instr)? as usize;
                let delta = frame.pop_i32() as u32 as usize;
                let init = frame.pop();
                let table = store.tables.get_mut(ti).ok_or(Trap::NoTable)?;
                let old = table.entries.len();
                let limit = table
                    .max
                    .map_or(DEFAULT_MAX_TABLE_ELEMS, |m| m as usize)
                    .min(DEFAULT_MAX_TABLE_ELEMS);
                match old.checked_add(delta).filter(|&n| n <= limit) {
                    Some(new_len) => {
                        table.entries.resize(new_len, init);
                        frame.push_i32(old as i32);
                    }
                    None => frame.push_i32(-1), // growth refused
                }
                pc += 1;
            }
            Op::TableFill => {
                let ti = table_imm(instr)? as usize;
                let n = frame.pop_i32() as u32 as usize;
                let val = frame.pop();
                let dst = frame.pop_i32() as u32 as usize;
                let table = store.tables.get_mut(ti).ok_or(Trap::NoTable)?;
                let end = dst.checked_add(n).filter(|&e| e <= table.entries.len());
                let end = end.ok_or(Trap::TableOutOfBounds)?;
                table.entries[dst..end].fill(val);
                pc += 1;
            }
            Op::TableInit => {
                let Imm::TableInit { elem, table } = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let (ei, ti) = (elem as usize, table as usize);
                let dropped = *store.elem_dropped.get(ei).ok_or(Trap::UndefinedElement)?;
                let n = frame.pop_i32() as u32 as usize;
                let src = frame.pop_i32() as u32 as usize;
                let dst = frame.pop_i32() as u32 as usize;
                let seg_len = if dropped { 0 } else { store.elem_values[ei].len() };
                let tbl_len = store.tables.get(ti).ok_or(Trap::NoTable)?.entries.len();
                if src.checked_add(n).is_none_or(|e| e > seg_len)
                    || dst.checked_add(n).is_none_or(|e| e > tbl_len)
                {
                    return Err(Trap::TableOutOfBounds);
                }
                for k in 0..n {
                    let v = if dropped {
                        NULL_REF
                    } else {
                        store.elem_values[ei][src + k]
                    };
                    store.tables[ti].entries[dst + k] = v;
                }
                pc += 1;
            }
            Op::ElemDrop => {
                let Imm::Elem(e) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                *store
                    .elem_dropped
                    .get_mut(e as usize)
                    .ok_or(Trap::UndefinedElement)? = true;
                pc += 1;
            }
            Op::TableCopy => {
                let Imm::TableCopy { dst, src } = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let (di, si) = (dst as usize, src as usize);
                let n = frame.pop_i32() as u32 as usize;
                let s = frame.pop_i32() as u32 as usize;
                let d = frame.pop_i32() as u32 as usize;
                let src_len = store.tables.get(si).ok_or(Trap::NoTable)?.entries.len();
                let dst_len = store.tables.get(di).ok_or(Trap::NoTable)?.entries.len();
                if s.checked_add(n).is_none_or(|e| e > src_len)
                    || d.checked_add(n).is_none_or(|e| e > dst_len)
                {
                    return Err(Trap::TableOutOfBounds);
                }
                if di == si {
                    store.tables[di].entries.copy_within(s..s + n, d);
                } else {
                    let tmp = store.tables[si].entries[s..s + n].to_vec();
                    store.tables[di].entries[d..d + n].copy_from_slice(&tmp);
                }
                pc += 1;
            }

            // --- WasmGC: i31 (unboxed; i31_tag checked AFTER null_ref) ---
            Op::RefI31 => {
                let x = frame.pop_i32() as u32;
                frame.push(I31_TAG | Value::from(x & 0x7fff_ffff)); // wrap to 31 bits, non-null
                pc += 1;
            }
            Op::I31GetS => {
                let r = frame.pop();
                if r == NULL_REF {
                    return Err(Trap::NullReference);
                }
                let n = r as u32;
                frame.push_i32(((n << 1) as i32) >> 1); // sign-extend the 31-bit payload
                pc += 1;
            }
            Op::I31GetU => {
                let r = frame.pop();
                if r == NULL_REF {
                    return Err(Trap::NullReference);
                }
                frame.push_i32((r as u32 & 0x7fff_ffff) as i32);
                pc += 1;
            }
            Op::RefEq => {
                let b = frame.pop();
                let a = frame.pop();
                frame.push_i32(i32::from(a == b));
                pc += 1;
            }

            // --- WasmGC: struct objects ---
            Op::StructNew => {
                let Imm::GcType(ti) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let sf = ctx.module.struct_fields(ti).ok_or(Trap::UndefinedType)?;
                let base = frame.stack_base(sf.len())?;
                let obj: Vec<Value> = sf
                    .iter()
                    .enumerate()
                    .map(|(k, f)| pack_field(f.storage, frame.vstack[base + k]))
                    .collect();
                frame.vstack.truncate(base);
                let r = alloc_object(store, ti, obj)?;
                frame.push(r);
                pc += 1;
            }
            Op::StructNewDefault => {
                let Imm::GcType(ti) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let sf = ctx.module.struct_fields(ti).ok_or(Trap::UndefinedType)?;
                let obj: Vec<Value> = sf.iter().map(|f| default_field(f.storage)).collect();
                let r = alloc_object(store, ti, obj)?;
                frame.push(r);
                pc += 1;
            }
            Op::StructGet | Op::StructGetS | Op::StructGetU => {
                let Imm::GcField { type_index, field } = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let idx = gc_object_index(store, frame.pop())?;
                let storage = ctx
                    .module
                    .struct_fields(type_index)
                    .ok_or(Trap::UndefinedType)?
                    .get(field as usize)
                    .ok_or(Trap::GcOutOfBounds)?
                    .storage;
                let v = *store.gc_heap[idx]
                    .fields
                    .get(field as usize)
                    .ok_or(Trap::GcOutOfBounds)?;
                frame.push(unpack_field(storage, v, instr.op == Op::StructGetS));
                pc += 1;
            }
            Op::StructSet => {
                let Imm::GcField { type_index, field } = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let v = frame.pop();
                let idx = gc_object_index(store, frame.pop())?;
                let storage = ctx
                    .module
                    .struct_fields(type_index)
                    .ok_or(Trap::UndefinedType)?
                    .get(field as usize)
                    .ok_or(Trap::GcOutOfBounds)?
                    .storage;
                let slot = store.gc_heap[idx]
                    .fields
                    .get_mut(field as usize)
                    .ok_or(Trap::GcOutOfBounds)?;
                *slot = pack_field(storage, v);
                pc += 1;
            }

            // --- WasmGC: array objects ---
            Op::ArrayNew => {
                let Imm::GcType(ti) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let f = ctx.module.array_field(ti).ok_or(Trap::UndefinedType)?;
                let len = frame.pop_i32() as u32 as usize;
                let init = pack_field(f.storage, frame.pop());
                let r = alloc_object(store, ti, vec![init; len])?;
                frame.push(r);
                pc += 1;
            }
            Op::ArrayNewDefault => {
                let Imm::GcType(ti) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let f = ctx.module.array_field(ti).ok_or(Trap::UndefinedType)?;
                let len = frame.pop_i32() as u32 as usize;
                let r = alloc_object(store, ti, vec![default_field(f.storage); len])?;
                frame.push(r);
                pc += 1;
            }
            Op::ArrayNewFixed => {
                let Imm::GcTypeN { type_index, n } = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let f = ctx.module.array_field(type_index).ok_or(Trap::UndefinedType)?;
                let base = frame.stack_base(n as usize)?;
                let obj: Vec<Value> = (0..n as usize)
                    .map(|k| pack_field(f.storage, frame.vstack[base + k]))
                    .collect();
                frame.vstack.truncate(base);
                let r = alloc_object(store, type_index, obj)?;
                frame.push(r);
                pc += 1;
            }
            Op::ArrayGet | Op::ArrayGetS | Op::ArrayGetU => {
                let Imm::GcType(ti) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let f = ctx.module.array_field(ti).ok_or(Trap::UndefinedType)?;
                let index = frame.pop_i32() as u32 as usize;
                let idx = gc_object_index(store, frame.pop())?;
                let v = *store.gc_heap[idx]
                    .fields
                    .get(index)
                    .ok_or(Trap::GcOutOfBounds)?;
                frame.push(unpack_field(f.storage, v, instr.op == Op::ArrayGetS));
                pc += 1;
            }
            Op::ArraySet => {
                let Imm::GcType(ti) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let f = ctx.module.array_field(ti).ok_or(Trap::UndefinedType)?;
                let v = frame.pop();
                let index = frame.pop_i32() as u32 as usize;
                let idx = gc_object_index(store, frame.pop())?;
                let slot = store.gc_heap[idx]
                    .fields
                    .get_mut(index)
                    .ok_or(Trap::GcOutOfBounds)?;
                *slot = pack_field(f.storage, v);
                pc += 1;
            }
            Op::ArrayLen => {
                let idx = gc_object_index(store, frame.pop())?;
                frame.push_i32(store.gc_heap[idx].fields.len() as i32);
                pc += 1;
            }

            // --- WasmGC: casts ---
            Op::RefTest => {
                let Imm::RefCast(rt) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let v = frame.pop();
                frame.push_i32(i32::from(ref_matches(ctx.module, store, v, rt)));
                pc += 1;
            }
            Op::RefCastOp => {
                let Imm::RefCast(rt) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let v = *frame.vstack.last().ok_or(Trap::StackUnderflow)?; // peek — value stays
                if !ref_matches(ctx.module, store, v, rt) {
                    return Err(Trap::CastFailure);
                }
                pc += 1;
            }
            Op::BrOnCast => {
                let Imm::BrCast { label, dst, .. } = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let v = *frame.vstack.last().ok_or(Trap::StackUnderflow)?;
                pc = if ref_matches(ctx.module, store, v, dst) {
                    frame.branch(label)?
                } else {
                    pc + 1
                };
            }
            Op::BrOnCastFail => {
                let Imm::BrCast { label, dst, .. } = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let v = *frame.vstack.last().ok_or(Trap::StackUnderflow)?;
                pc = if ref_matches(ctx.module, store, v, dst) {
                    pc + 1
                } else {
                    frame.branch(label)?
                };
            }

            // --- SIMD (v128, 0xFD family) ---
            Op::Simd => {
                let Imm::Simd(s) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                exec_simd(frame, store, s)?;
                pc += 1;
            }

            // --- Threads / atomics (0xFE family) ---
            Op::Atomic => {
                let Imm::Atomic(at) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                exec_atomic(frame, store, at)?;
                pc += 1;
            }

            // Integer arithmetic / comparison / bitwise / conversion.
            _ => {
                exec_numeric(frame, instr.op)?;
                pc += 1;
            }
        }
    }
    Ok(())
}

fn local_index(instr: &Instr) -> Result<usize> {
    if let Imm::Local(i) = instr.imm {
        Ok(i as usize)
    } else {
        Err(Trap::UnsupportedInstruction)
    }
}
fn label_imm(instr: &Instr) -> Result<u32> {
    if let Imm::Label(l) = instr.imm {
        Ok(l)
    } else {
        Err(Trap::UnsupportedInstruction)
    }
}
fn block_type(instr: &Instr) -> Result<BlockType> {
    if let Imm::BlockType(bt) = instr.imm {
        Ok(bt)
    } else {
        Err(Trap::UnsupportedInstruction)
    }
}
fn table_imm(instr: &Instr) -> Result<u32> {
    if let Imm::Table(t) = instr.imm {
        Ok(t)
    } else {
        Err(Trap::UnsupportedInstruction)
    }
}

/// Overflow-safe range check: is `base + n <= len`? Returns `base` as `usize` (then `< len`,
/// so it fits) or `None` for out-of-bounds.
fn mem_range(base: u64, n: u64, len: usize) -> Option<usize> {
    if base.checked_add(n)? > len as u64 {
        return None;
    }
    usize::try_from(base).ok()
}

/// Read `n` (1..=8) bytes little-endian from memory `ma.memory` at (popped address + offset),
/// zero-extended into a `u64`. The caller sign/zero-extends into the target slot.
fn load_bytes(frame: &mut Frame, store: &Store, ma: crate::opcode::MemArg, n: usize) -> Result<u64> {
    let mem = store.memories.get(ma.memory as usize).ok_or(Trap::NoMemory)?;
    let addr = frame.pop_mem(mem.is64);
    let ea = addr.checked_add(ma.offset).ok_or(Trap::MemoryOutOfBounds)?;
    let end = ea.checked_add(n as u64).ok_or(Trap::MemoryOutOfBounds)?;
    if end > mem.bytes.len() as u64 {
        return Err(Trap::MemoryOutOfBounds);
    }
    let start = ea as usize;
    let mut v = 0u64;
    for (i, &b) in mem.bytes[start..start + n].iter().enumerate() {
        v |= u64::from(b) << (8 * i);
    }
    Ok(v)
}

/// Write the low `n` bytes of `val` little-endian to memory `ma.memory`. The value was already
/// popped by the caller; this pops the address.
fn store_bytes(
    frame: &mut Frame,
    store: &mut Store,
    ma: crate::opcode::MemArg,
    n: usize,
    val: u64,
) -> Result<()> {
    let mem = store.memories.get_mut(ma.memory as usize).ok_or(Trap::NoMemory)?;
    let addr = frame.pop_mem(mem.is64);
    let ea = addr.checked_add(ma.offset).ok_or(Trap::MemoryOutOfBounds)?;
    let end = ea.checked_add(n as u64).ok_or(Trap::MemoryOutOfBounds)?;
    if end > mem.bytes.len() as u64 {
        return Err(Trap::MemoryOutOfBounds);
    }
    let start = ea as usize;
    for (i, b) in mem.bytes[start..start + n].iter_mut().enumerate() {
        *b = (val >> (8 * i)) as u8;
    }
    Ok(())
}

/// Loads/stores + `memory.size`/`grow`.
fn exec_memory(frame: &mut Frame, store: &mut Store, instr: &Instr) -> Result<()> {
    match instr.op {
        Op::MemorySize => {
            let Imm::MemIndex(mi) = instr.imm else {
                return Err(Trap::UnsupportedInstruction);
            };
            let mem = store.memories.get(mi as usize).ok_or(Trap::NoMemory)?;
            let pages = (mem.bytes.len() / PAGE_SIZE) as u64;
            if mem.is64 {
                frame.push_i64(pages as i64);
            } else {
                frame.push_i32(pages as i32);
            }
            return Ok(());
        }
        Op::MemoryGrow => return memory_grow(frame, store, instr),
        _ => {}
    }
    let Imm::Mem(ma) = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    match instr.op {
        Op::I32Load => {
            let v = load_bytes(frame, store, ma, 4)?;
            frame.push_i32(v as u32 as i32);
        }
        Op::I64Load => {
            let v = load_bytes(frame, store, ma, 8)?;
            frame.push_i64(v as i64);
        }
        Op::F32Load => {
            let v = load_bytes(frame, store, ma, 4)?;
            frame.push(Value::from(v));
        }
        Op::F64Load => {
            let v = load_bytes(frame, store, ma, 8)?;
            frame.push(Value::from(v));
        }
        Op::I32Load8S => {
            let v = load_bytes(frame, store, ma, 1)?;
            frame.push_i32(i32::from(v as u8 as i8));
        }
        Op::I32Load8U => {
            let v = load_bytes(frame, store, ma, 1)?;
            frame.push_i32(i32::from(v as u8));
        }
        Op::I32Load16S => {
            let v = load_bytes(frame, store, ma, 2)?;
            frame.push_i32(i32::from(v as u16 as i16));
        }
        Op::I32Load16U => {
            let v = load_bytes(frame, store, ma, 2)?;
            frame.push_i32(i32::from(v as u16));
        }
        Op::I64Load8S => {
            let v = load_bytes(frame, store, ma, 1)?;
            frame.push_i64(i64::from(v as u8 as i8));
        }
        Op::I64Load8U => {
            let v = load_bytes(frame, store, ma, 1)?;
            frame.push_i64(i64::from(v as u8));
        }
        Op::I64Load16S => {
            let v = load_bytes(frame, store, ma, 2)?;
            frame.push_i64(i64::from(v as u16 as i16));
        }
        Op::I64Load16U => {
            let v = load_bytes(frame, store, ma, 2)?;
            frame.push_i64(i64::from(v as u16));
        }
        Op::I64Load32S => {
            let v = load_bytes(frame, store, ma, 4)?;
            frame.push_i64(i64::from(v as u32 as i32));
        }
        Op::I64Load32U => {
            let v = load_bytes(frame, store, ma, 4)?;
            frame.push_i64(i64::from(v as u32));
        }
        Op::I32Store => {
            let val = u64::from(frame.pop_i32() as u32);
            store_bytes(frame, store, ma, 4, val)?;
        }
        Op::I64Store => {
            let val = frame.pop_i64() as u64;
            store_bytes(frame, store, ma, 8, val)?;
        }
        Op::F32Store => {
            let val = frame.pop() as u64 & 0xffff_ffff;
            store_bytes(frame, store, ma, 4, val)?;
        }
        Op::F64Store => {
            let val = frame.pop() as u64;
            store_bytes(frame, store, ma, 8, val)?;
        }
        Op::I32Store8 => {
            let val = u64::from(frame.pop_i32() as u32);
            store_bytes(frame, store, ma, 1, val)?;
        }
        Op::I32Store16 => {
            let val = u64::from(frame.pop_i32() as u32);
            store_bytes(frame, store, ma, 2, val)?;
        }
        Op::I64Store8 => {
            let val = frame.pop_i64() as u64;
            store_bytes(frame, store, ma, 1, val)?;
        }
        Op::I64Store16 => {
            let val = frame.pop_i64() as u64;
            store_bytes(frame, store, ma, 2, val)?;
        }
        Op::I64Store32 => {
            let val = frame.pop_i64() as u64;
            store_bytes(frame, store, ma, 4, val)?;
        }
        _ => return Err(Trap::UnsupportedInstruction),
    }
    Ok(())
}

fn memory_grow(frame: &mut Frame, store: &mut Store, instr: &Instr) -> Result<()> {
    let Imm::MemIndex(mi) = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    let mi = mi as usize;
    let is64 = store.memories.get(mi).ok_or(Trap::NoMemory)?.is64;
    let delta = frame.pop_mem(is64);
    let mem = &mut store.memories[mi];
    let old_pages = (mem.bytes.len() / PAGE_SIZE) as u64;
    let cap: u64 = if is64 { 0x1_0000_0000_0000 } else { 65536 };
    let limit = mem.max.unwrap_or(cap).min(cap);
    let target = old_pages
        .checked_add(delta)
        .filter(|&p| p <= limit)
        .and_then(|p| usize::try_from(p).ok())
        .and_then(|p| p.checked_mul(PAGE_SIZE))
        .filter(|&n| n <= DEFAULT_MAX_MEMORY_BYTES);
    match target {
        Some(nbytes) => {
            mem.bytes.resize(nbytes, 0);
            if is64 {
                frame.push_i64(old_pages as i64);
            } else {
                frame.push_i32(old_pages as i32);
            }
        }
        None if is64 => frame.push_i64(-1), // growth refused
        None => frame.push_i32(-1),
    }
    Ok(())
}

fn exec_memory_copy(frame: &mut Frame, store: &mut Store, instr: &Instr) -> Result<()> {
    let Imm::MemCopy { dst, src } = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    let (dst, src) = (dst as usize, src as usize);
    let dst64 = store.memories.get(dst).ok_or(Trap::NoMemory)?.is64;
    let src64 = store.memories.get(src).ok_or(Trap::NoMemory)?.is64;
    let n = frame.pop_mem(dst64 && src64);
    let srca = frame.pop_mem(src64);
    let dsta = frame.pop_mem(dst64);
    let si = mem_range(srca, n, store.memories[src].bytes.len()).ok_or(Trap::MemoryOutOfBounds)?;
    let di = mem_range(dsta, n, store.memories[dst].bytes.len()).ok_or(Trap::MemoryOutOfBounds)?;
    let ni = n as usize;
    if dst == src {
        store.memories[dst].bytes.copy_within(si..si + ni, di);
    } else {
        let tmp = store.memories[src].bytes[si..si + ni].to_vec();
        store.memories[dst].bytes[di..di + ni].copy_from_slice(&tmp);
    }
    Ok(())
}

fn exec_memory_fill(frame: &mut Frame, store: &mut Store, instr: &Instr) -> Result<()> {
    let Imm::MemIndex(mi) = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    let mi = mi as usize;
    let is64 = store.memories.get(mi).ok_or(Trap::NoMemory)?.is64;
    let n = frame.pop_mem(is64);
    let byte = frame.pop_i32() as u8;
    let dst = frame.pop_mem(is64);
    let mem = &mut store.memories[mi];
    let di = mem_range(dst, n, mem.bytes.len()).ok_or(Trap::MemoryOutOfBounds)?;
    mem.bytes[di..di + n as usize].fill(byte);
    Ok(())
}

fn exec_memory_init(frame: &mut Frame, ctx: &Ctx, store: &mut Store, instr: &Instr) -> Result<()> {
    let Imm::MemInit { data, mem } = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    let (mi, di) = (mem as usize, data as usize);
    let is64 = store.memories.get(mi).ok_or(Trap::NoMemory)?.is64;
    let dropped = *store.data_dropped.get(di).ok_or(Trap::UndefinedData)?;
    let empty: &[u8] = &[];
    let seg: &[u8] = if dropped {
        empty
    } else {
        &ctx.module.data.get(di).ok_or(Trap::UndefinedData)?.bytes
    };
    // n/src index the data segment (always i32); dst is a memory address.
    let n = u64::from(frame.pop_i32() as u32);
    let src = u64::from(frame.pop_i32() as u32);
    let dst = frame.pop_mem(is64);
    let si = mem_range(src, n, seg.len()).ok_or(Trap::MemoryOutOfBounds)?;
    let mem = &mut store.memories[mi];
    let di_addr = mem_range(dst, n, mem.bytes.len()).ok_or(Trap::MemoryOutOfBounds)?;
    let ni = n as usize;
    mem.bytes[di_addr..di_addr + ni].copy_from_slice(&seg[si..si + ni]);
    Ok(())
}

/// Interpret a data/element-segment offset constant expression as an address.
fn eval_const_offset(expr: &[u8], globals: &[Value], is64: bool) -> Result<u64> {
    let v = eval_const_expr(expr, globals)?;
    Ok(if is64 {
        v as u64
    } else {
        u64::from(as_i32(v) as u32)
    })
}

// --- WasmGC helpers ----------------------------------------------------------

/// Narrow a value to a field's storage width before storing (packed i8/i16 keep low bits;
/// an unpacked field — including a `v128`, which fits one 128-bit `Value` — is verbatim).
fn pack_field(storage: StorageType, v: Value) -> Value {
    match storage {
        StorageType::Val(_) => v,
        StorageType::I8 => v & 0xff,
        StorageType::I16 => v & 0xffff,
    }
}

/// Widen a stored field value back to an i32 slot: `_s` sign-extends a packed field, `_u`
/// zero-extends; an unpacked field is verbatim.
fn unpack_field(storage: StorageType, v: Value, signed: bool) -> Value {
    match storage {
        StorageType::Val(_) => v,
        StorageType::I8 if signed => i32_value(i32::from(v as u8 as i8)),
        StorageType::I8 => i32_value(i32::from(v as u8)),
        StorageType::I16 if signed => i32_value(i32::from(v as u16 as i16)),
        StorageType::I16 => i32_value(i32::from(v as u16)),
    }
}

/// The default slot for a field/element of `storage` at `*.new_default` (null for a ref, else 0).
fn default_field(storage: StorageType) -> Value {
    if storage.unpacked().is_ref() {
        NULL_REF
    } else {
        0
    }
}

/// Validate a non-null GC reference and return its heap index, or trap.
fn gc_object_index(store: &Store, r: Value) -> Result<usize> {
    if r == NULL_REF {
        return Err(Trap::NullReference);
    }
    let idx = usize::try_from(r).map_err(|_| Trap::GcOutOfBounds)?;
    if idx >= store.gc_heap.len() {
        return Err(Trap::GcOutOfBounds);
    }
    Ok(idx)
}

/// Allocate a GC object, returning its reference value (its heap index).
fn alloc_object(store: &mut Store, type_index: u32, fields: Vec<Value>) -> Result<Value> {
    let idx = store.gc_heap.len();
    if idx >= MAX_GC_OBJECTS {
        return Err(Trap::GcHeapExhausted);
    }
    store.gc_heap.push(HeapObject { type_index, fields });
    Ok(idx as Value)
}

/// The type index of a *defined* function (for a funcref `ref.cast` to a concrete func type);
/// `None` for an imported function.
fn defined_func_type(module: &Module, v: Value) -> Option<u32> {
    let fi = u32::try_from(v).ok()?;
    let imported = module.imported_func_count();
    if fi < imported {
        return None;
    }
    module.functions.get((fi - imported) as usize).copied()
}

/// Match a value's actual heap head against a target heap type — abstract targets use the
/// hierarchy relation, concrete targets the declared subtype chain.
fn head_matches(module: &Module, actual: RefHeap, actual_ti: Option<u32>, target: HeapType) -> bool {
    match target {
        HeapType::Concrete(t) => actual_ti.is_some_and(|ti| module.is_subtype(ti, t)),
        // The uninhabited bottoms: only a null ref has these, already handled by `ref_matches`.
        HeapType::NoFunc | HeapType::NoExtern => false,
        _ => module
            .ref_head(target)
            .is_ok_and(|th| actual.is_subtype_of(th)),
    }
}

/// Does GC reference value `v` match target reference type `rt` (`ref.test`/`ref.cast`)?
fn ref_matches(module: &Module, store: &Store, v: Value, rt: RefType) -> bool {
    if v == NULL_REF {
        return rt.nullable;
    }
    let Ok(target_head) = module.ref_head(rt.heap) else {
        return false;
    };
    match target_head.top() {
        RefHeap::Any => {
            if v & I31_TAG != 0 {
                return head_matches(module, RefHeap::I31, None, rt.heap);
            }
            let Ok(idx) = usize::try_from(v) else {
                return false;
            };
            let Some(obj) = store.gc_heap.get(idx) else {
                return false;
            };
            let kind = match module.comp_types.get(obj.type_index as usize).map(CompType::kind) {
                Some(CompKind::Struct) => RefHeap::Struct,
                Some(CompKind::Array) => RefHeap::Array,
                _ => RefHeap::Func,
            };
            head_matches(module, kind, Some(obj.type_index), rt.heap)
        }
        RefHeap::Func => head_matches(module, RefHeap::Func, defined_func_type(module, v), rt.heap),
        _ => head_matches(module, RefHeap::Extern, None, rt.heap),
    }
}

/// Integer arithmetic / comparison / bitwise / conversion opcodes. Anything else (float,
/// memory, SIMD, …) traps `UnsupportedInstruction` in this slice.
fn exec_numeric(frame: &mut Frame, op: Op) -> Result<()> {
    match op as u8 {
        // i32 unary
        0x45 => {
            let v = frame.pop_i32();
            frame.push_i32(i32::from(v == 0)); // i32.eqz
        }
        0x67 => {
            let v = frame.pop_i32() as u32;
            frame.push_i32(v.leading_zeros() as i32);
        }
        0x68 => {
            let v = frame.pop_i32() as u32;
            frame.push_i32(v.trailing_zeros() as i32);
        }
        0x69 => {
            let v = frame.pop_i32() as u32;
            frame.push_i32(v.count_ones() as i32);
        }
        // i32 comparison
        0x46..=0x4f => {
            let b = frame.pop_i32();
            let a = frame.pop_i32();
            frame.push_i32(i32::from(cmp_i32(op as u8, a, b)));
        }
        // i32 binary
        0x6a..=0x78 => {
            let b = frame.pop_i32();
            let a = frame.pop_i32();
            frame.push_i32(bin_i32(op as u8, a, b)?);
        }
        0xc0 => {
            let v = frame.pop_i32();
            frame.push_i32(i32::from(v as i8)); // i32.extend8_s
        }
        0xc1 => {
            let v = frame.pop_i32();
            frame.push_i32(i32::from(v as i16)); // i32.extend16_s
        }
        0xa7 => {
            let v = frame.pop_i64();
            frame.push_i32(v as i32); // i32.wrap_i64
        }

        // i64 unary
        0x50 => {
            let v = frame.pop_i64();
            frame.push_i32(i32::from(v == 0)); // i64.eqz (result i32)
        }
        0x79 => {
            let v = frame.pop_i64() as u64;
            frame.push_i64(i64::from(v.leading_zeros()));
        }
        0x7a => {
            let v = frame.pop_i64() as u64;
            frame.push_i64(i64::from(v.trailing_zeros()));
        }
        0x7b => {
            let v = frame.pop_i64() as u64;
            frame.push_i64(i64::from(v.count_ones()));
        }
        // i64 comparison (result i32)
        0x51..=0x5a => {
            let b = frame.pop_i64();
            let a = frame.pop_i64();
            frame.push_i32(i32::from(cmp_i64(op as u8, a, b)));
        }
        // i64 binary
        0x7c..=0x8a => {
            let b = frame.pop_i64();
            let a = frame.pop_i64();
            frame.push_i64(bin_i64(op as u8, a, b)?);
        }
        0xc2 => {
            let v = frame.pop_i64();
            frame.push_i64(i64::from(v as i8)); // i64.extend8_s
        }
        0xc3 => {
            let v = frame.pop_i64();
            frame.push_i64(i64::from(v as i16)); // i64.extend16_s
        }
        0xc4 => {
            let v = frame.pop_i64();
            frame.push_i64(i64::from(v as i32)); // i64.extend32_s
        }
        0xac => {
            let v = frame.pop_i32();
            frame.push_i64(i64::from(v)); // i64.extend_i32_s
        }
        0xad => {
            let v = frame.pop_i32();
            frame.push_i64(i64::from(v as u32)); // i64.extend_i32_u
        }

        _ => return exec_float(frame, op),
    }
    Ok(())
}

/// Float arithmetic / comparison / conversion opcodes (IEEE 754). Rounding ops use bit
/// manipulation (no_std-clean); `sqrt` is gated behind `std`. Saturating float→int uses
/// Rust's `as` cast, which matches wasm exactly (NaN→0, saturates to min/max).
fn exec_float(frame: &mut Frame, op: Op) -> Result<()> {
    match op as u8 {
        // f32 / f64 comparison (result i32) — IEEE ordering; NaN compares false (ne true).
        0x5b..=0x60 => {
            let b = frame.pop_f32();
            let a = frame.pop_f32();
            frame.push_i32(i32::from(fcmp(op as u8 - 0x5b, f64::from(a), f64::from(b))));
        }
        0x61..=0x66 => {
            let b = frame.pop_f64();
            let a = frame.pop_f64();
            frame.push_i32(i32::from(fcmp(op as u8 - 0x61, a, b)));
        }

        // f32 unary
        0x8b => {
            let v = frame.pop_f32();
            frame.push_f32(fabs_f32(v));
        }
        0x8c => {
            let v = frame.pop_f32();
            frame.push_f32(-v);
        }
        0x8d => {
            let v = frame.pop_f32();
            frame.push_f32(ceil_f32(v));
        }
        0x8e => {
            let v = frame.pop_f32();
            frame.push_f32(floor_f32(v));
        }
        0x8f => {
            let v = frame.pop_f32();
            frame.push_f32(trunc_f32(v));
        }
        0x90 => {
            let v = frame.pop_f32();
            frame.push_f32(nearest_f32(v));
        }
        0x91 => {
            let v = frame.pop_f32();
            frame.push_f32(sqrt_f32(v)?);
        }
        // f32 binary
        0x92..=0x98 => {
            let b = frame.pop_f32();
            let a = frame.pop_f32();
            frame.push_f32(fbin_f32(op as u8, a, b));
        }

        // f64 unary
        0x99 => {
            let v = frame.pop_f64();
            frame.push_f64(fabs_f64(v));
        }
        0x9a => {
            let v = frame.pop_f64();
            frame.push_f64(-v);
        }
        0x9b => {
            let v = frame.pop_f64();
            frame.push_f64(ceil_f64(v));
        }
        0x9c => {
            let v = frame.pop_f64();
            frame.push_f64(floor_f64(v));
        }
        0x9d => {
            let v = frame.pop_f64();
            frame.push_f64(trunc_f64(v));
        }
        0x9e => {
            let v = frame.pop_f64();
            frame.push_f64(nearest_f64(v));
        }
        0x9f => {
            let v = frame.pop_f64();
            frame.push_f64(sqrt_f64(v)?);
        }
        // f64 binary
        0xa0..=0xa6 => {
            let b = frame.pop_f64();
            let a = frame.pop_f64();
            frame.push_f64(fbin_f64(op as u8, a, b));
        }

        // Float → int, trapping.
        0xa8 => {
            let t = trap_trunc_f32(frame.pop_f32(), -2_147_483_648.0, 2_147_483_648.0)?;
            frame.push_i32(t as i32);
        }
        0xa9 => {
            let t = trap_trunc_f32(frame.pop_f32(), 0.0, 4_294_967_296.0)?;
            frame.push_i32(t as u32 as i32);
        }
        0xaa => {
            let t = trap_trunc_f64(frame.pop_f64(), -2_147_483_648.0, 2_147_483_648.0)?;
            frame.push_i32(t as i32);
        }
        0xab => {
            let t = trap_trunc_f64(frame.pop_f64(), 0.0, 4_294_967_296.0)?;
            frame.push_i32(t as u32 as i32);
        }
        0xae => {
            let t = trap_trunc_f32(frame.pop_f32(), -9_223_372_036_854_775_808.0, 9_223_372_036_854_775_808.0)?;
            frame.push_i64(t as i64);
        }
        0xaf => {
            let t = trap_trunc_f32(frame.pop_f32(), 0.0, 18_446_744_073_709_551_616.0)?;
            frame.push_i64(t as u64 as i64);
        }
        0xb0 => {
            let t = trap_trunc_f64(frame.pop_f64(), -9_223_372_036_854_775_808.0, 9_223_372_036_854_775_808.0)?;
            frame.push_i64(t as i64);
        }
        0xb1 => {
            let t = trap_trunc_f64(frame.pop_f64(), 0.0, 18_446_744_073_709_551_616.0)?;
            frame.push_i64(t as u64 as i64);
        }

        // Int → float
        0xb2 => {
            let v = frame.pop_i32();
            frame.push_f32(v as f32);
        }
        0xb3 => {
            let v = frame.pop_i32();
            frame.push_f32(v as u32 as f32);
        }
        0xb4 => {
            let v = frame.pop_i64();
            frame.push_f32(v as f32);
        }
        0xb5 => {
            let v = frame.pop_i64();
            frame.push_f32(v as u64 as f32);
        }
        0xb6 => {
            let v = frame.pop_f64();
            frame.push_f32(v as f32); // f32.demote_f64
        }
        0xb7 => {
            let v = frame.pop_i32();
            frame.push_f64(f64::from(v));
        }
        0xb8 => {
            let v = frame.pop_i32();
            frame.push_f64(f64::from(v as u32));
        }
        0xb9 => {
            let v = frame.pop_i64();
            frame.push_f64(v as f64);
        }
        0xba => {
            let v = frame.pop_i64();
            frame.push_f64(v as u64 as f64);
        }
        0xbb => {
            let v = frame.pop_f32();
            frame.push_f64(f64::from(v)); // f64.promote_f32
        }

        // Reinterpret: the u64 slot already holds the bit pattern, so these are identity.
        0xbc..=0xbf => {}

        // Float → int, saturating (Rust's `as` matches wasm: NaN→0, saturates).
        0xc5 => {
            let v = frame.pop_f32();
            frame.push_i32(v as i32);
        }
        0xc6 => {
            let v = frame.pop_f32();
            frame.push_i32(v as u32 as i32);
        }
        0xc7 => {
            let v = frame.pop_f64();
            frame.push_i32(v as i32);
        }
        0xc8 => {
            let v = frame.pop_f64();
            frame.push_i32(v as u32 as i32);
        }
        0xc9 => {
            let v = frame.pop_f32();
            frame.push_i64(v as i64);
        }
        0xca => {
            let v = frame.pop_f32();
            frame.push_i64(v as u64 as i64);
        }
        0xcb => {
            let v = frame.pop_f64();
            frame.push_i64(v as i64);
        }
        0xcc => {
            let v = frame.pop_f64();
            frame.push_i64(v as u64 as i64);
        }

        _ => return Err(Trap::UnsupportedInstruction),
    }
    Ok(())
}

/// Float comparison, keyed by the offset within its 6-op group (eq/ne/lt/gt/le/ge). Done in
/// f64 (an f32 comparison widens losslessly and orders identically).
fn fcmp(rel: u8, a: f64, b: f64) -> bool {
    match rel {
        0 => a == b,
        1 => a != b,
        2 => a < b,
        3 => a > b,
        4 => a <= b,
        _ => a >= b, // ge
    }
}

fn fbin_f32(op: u8, a: f32, b: f32) -> f32 {
    match op {
        0x92 => a + b,
        0x93 => a - b,
        0x94 => a * b,
        0x95 => a / b,
        0x96 => fmin_f32(a, b),
        0x97 => fmax_f32(a, b),
        _ => fcopysign_f32(a, b), // 0x98 copysign
    }
}
fn fbin_f64(op: u8, a: f64, b: f64) -> f64 {
    match op {
        0xa0 => a + b,
        0xa1 => a - b,
        0xa2 => a * b,
        0xa3 => a / b,
        0xa4 => fmin_f64(a, b),
        0xa5 => fmax_f64(a, b),
        _ => fcopysign_f64(a, b), // 0xa6 copysign
    }
}

// --- Float helpers (bit-manipulation; no_std-clean) --------------------------

fn fabs_f32(x: f32) -> f32 {
    f32::from_bits(x.to_bits() & 0x7fff_ffff)
}
fn fabs_f64(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & 0x7fff_ffff_ffff_ffff)
}
fn fcopysign_f32(x: f32, y: f32) -> f32 {
    f32::from_bits((x.to_bits() & 0x7fff_ffff) | (y.to_bits() & 0x8000_0000))
}
fn fcopysign_f64(x: f64, y: f64) -> f64 {
    f64::from_bits((x.to_bits() & 0x7fff_ffff_ffff_ffff) | (y.to_bits() & 0x8000_0000_0000_0000))
}

/// wasm `fmin`: NaN-propagating, and `min(+0,-0) == -0` (sign-bit OR).
fn fmin_f32(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        return f32::NAN;
    }
    if a < b {
        a
    } else if b < a {
        b
    } else {
        f32::from_bits(a.to_bits() | b.to_bits())
    }
}
fn fmin_f64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a < b {
        a
    } else if b < a {
        b
    } else {
        f64::from_bits(a.to_bits() | b.to_bits())
    }
}
/// wasm `fmax`: NaN-propagating, and `max(+0,-0) == +0` (sign-bit AND).
fn fmax_f32(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        return f32::NAN;
    }
    if a > b {
        a
    } else if b > a {
        b
    } else {
        f32::from_bits(a.to_bits() & b.to_bits())
    }
}
fn fmax_f64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a > b {
        a
    } else if b > a {
        b
    } else {
        f64::from_bits(a.to_bits() & b.to_bits())
    }
}

fn trunc_f32(x: f32) -> f32 {
    let bits = x.to_bits();
    let exp = ((bits >> 23) & 0xff) as i32 - 127;
    if exp < 0 {
        return f32::from_bits(bits & 0x8000_0000); // |x| < 1 → ±0
    }
    if exp >= 23 {
        return x; // already integer, or inf/nan
    }
    let mask = (1u32 << (23 - exp)) - 1;
    f32::from_bits(bits & !mask)
}
fn trunc_f64(x: f64) -> f64 {
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64 - 1023;
    if exp < 0 {
        return f64::from_bits(bits & 0x8000_0000_0000_0000);
    }
    if exp >= 52 {
        return x;
    }
    let mask = (1u64 << (52 - exp)) - 1;
    f64::from_bits(bits & !mask)
}
fn floor_f32(x: f32) -> f32 {
    let t = trunc_f32(x);
    if x < 0.0 && x != t {
        t - 1.0
    } else {
        t
    }
}
fn floor_f64(x: f64) -> f64 {
    let t = trunc_f64(x);
    if x < 0.0 && x != t {
        t - 1.0
    } else {
        t
    }
}
fn ceil_f32(x: f32) -> f32 {
    let t = trunc_f32(x);
    if x > 0.0 && x != t {
        t + 1.0
    } else {
        t
    }
}
fn ceil_f64(x: f64) -> f64 {
    let t = trunc_f64(x);
    if x > 0.0 && x != t {
        t + 1.0
    } else {
        t
    }
}
/// wasm `nearest`: round to nearest, ties to even, preserving the sign of zero and quieting
/// a NaN operand.
fn nearest_f32(x: f32) -> f32 {
    if x.is_nan() {
        return f32::from_bits(x.to_bits() | 0x0040_0000); // set the quiet bit
    }
    if !x.is_finite() {
        return x;
    }
    let f = floor_f32(x);
    let diff = x - f;
    // Round up when past the halfway point, or exactly halfway with an odd floor (ties→even).
    let r = if diff > 0.5 || (diff == 0.5 && f % 2.0 != 0.0) {
        f + 1.0
    } else {
        f
    };
    if r == 0.0 {
        return fcopysign_f32(0.0, x); // a rounded-to-zero result keeps x's sign
    }
    r
}
fn nearest_f64(x: f64) -> f64 {
    if x.is_nan() {
        return f64::from_bits(x.to_bits() | 0x0008_0000_0000_0000);
    }
    if !x.is_finite() {
        return x;
    }
    let f = floor_f64(x);
    let diff = x - f;
    let r = if diff > 0.5 || (diff == 0.5 && f % 2.0 != 0.0) {
        f + 1.0
    } else {
        f
    };
    if r == 0.0 {
        return fcopysign_f64(0.0, x); // a rounded-to-zero result keeps x's sign
    }
    r
}

/// Trapping float→int range check: NaN or `trunc(x) ∉ [lo, hi)` traps; else returns
/// `trunc(x)` for the caller to cast (in range, the cast is exact).
fn trap_trunc_f32(x: f32, lo: f32, hi: f32) -> Result<f32> {
    if x.is_nan() {
        return Err(Trap::InvalidConversionToInt);
    }
    let t = trunc_f32(x);
    if t < lo || t >= hi {
        return Err(Trap::InvalidConversionToInt);
    }
    Ok(t)
}
fn trap_trunc_f64(x: f64, lo: f64, hi: f64) -> Result<f64> {
    if x.is_nan() {
        return Err(Trap::InvalidConversionToInt);
    }
    let t = trunc_f64(x);
    if t < lo || t >= hi {
        return Err(Trap::InvalidConversionToInt);
    }
    Ok(t)
}

/// `f32.sqrt` / `f64.sqrt`. Correctly-rounded sqrt needs the platform math library, so it is
/// available with the `std` feature (the default); a freestanding `no_std` build traps until
/// a software sqrt lands.
#[cfg(feature = "std")]
fn sqrt_f32(x: f32) -> Result<f32> {
    Ok(x.sqrt())
}
#[cfg(feature = "std")]
fn sqrt_f64(x: f64) -> Result<f64> {
    Ok(x.sqrt())
}
#[cfg(not(feature = "std"))]
fn sqrt_f32(_x: f32) -> Result<f32> {
    Err(Trap::UnsupportedInstruction)
}
#[cfg(not(feature = "std"))]
fn sqrt_f64(_x: f64) -> Result<f64> {
    Err(Trap::UnsupportedInstruction)
}

// ============================ SIMD (v128, 0xFD) =============================
//
// A `v128` is a single 128-bit `Value` slot, so `frame.push`/`frame.pop` move it
// with no special-casing. These `v_*`/`p_*` helpers view a `Value` as a
// little-endian lane array and back — endian-explicit (`to_le_bytes`), so the
// wasm SIMD little-endian lane order holds on any host. Ported opcode-for-opcode
// from wazmrt `interp.zig` `execSimd` (frozen oracle @dadc727).

macro_rules! lane_views {
    ($unpack:ident, $pack:ident, $t:ty, $n:expr, $sz:expr) => {
        #[inline]
        fn $unpack(v: Value) -> [$t; $n] {
            let b = v.to_le_bytes();
            core::array::from_fn(|i| <$t>::from_le_bytes(b[i * $sz..i * $sz + $sz].try_into().unwrap()))
        }
        #[inline]
        fn $pack(a: [$t; $n]) -> Value {
            let mut b = [0u8; 16];
            for (i, x) in a.iter().enumerate() {
                b[i * $sz..i * $sz + $sz].copy_from_slice(&x.to_le_bytes());
            }
            Value::from_le_bytes(b)
        }
    };
}
lane_views!(v_u8x16, p_u8x16, u8, 16, 1);
lane_views!(v_i8x16, p_i8x16, i8, 16, 1);
lane_views!(v_u16x8, p_u16x8, u16, 8, 2);
lane_views!(v_i16x8, p_i16x8, i16, 8, 2);
lane_views!(v_u32x4, p_u32x4, u32, 4, 4);
lane_views!(v_i32x4, p_i32x4, i32, 4, 4);
lane_views!(v_u64x2, p_u64x2, u64, 2, 8);
lane_views!(v_i64x2, p_i64x2, i64, 2, 8);
lane_views!(v_f32x4, p_f32x4, f32, 4, 4);
lane_views!(v_f64x2, p_f64x2, f64, 2, 8);

/// Saturating float→int truncation for one lane (NaN→0), bounds compared in f64.
fn sat_trunc_i32(x: f64) -> i32 {
    if x.is_nan() {
        return 0;
    }
    let t = trunc_f64(x);
    if t <= i32::MIN as f64 {
        return i32::MIN;
    }
    if t >= i32::MAX as f64 {
        return i32::MAX;
    }
    t as i32
}
fn sat_trunc_u32(x: f64) -> u32 {
    if x.is_nan() {
        return 0;
    }
    let t = trunc_f64(x);
    if t <= 0.0 {
        return 0;
    }
    if t >= u32::MAX as f64 {
        return u32::MAX;
    }
    t as u32
}
#[inline]
fn fneg_f32(x: f32) -> f32 {
    f32::from_bits(x.to_bits() ^ 0x8000_0000)
}
#[inline]
fn fneg_f64(x: f64) -> f64 {
    f64::from_bits(x.to_bits() ^ 0x8000_0000_0000_0000)
}

/// Pop an address and bounds-check `n` bytes for a SIMD memory op; returns the
/// effective byte offset into memory `ma.memory` (memory64-aware, overflow-safe).
fn simd_mem_ea(frame: &mut Frame, store: &Store, ma: crate::opcode::MemArg, n: u64) -> Result<usize> {
    let mem = store.memories.get(ma.memory as usize).ok_or(Trap::NoMemory)?;
    let addr = frame.pop_mem(mem.is64);
    let ea = addr.checked_add(ma.offset).ok_or(Trap::MemoryOutOfBounds)?;
    let end = ea.checked_add(n).ok_or(Trap::MemoryOutOfBounds)?;
    if end > mem.bytes.len() as u64 {
        return Err(Trap::MemoryOutOfBounds);
    }
    Ok(ea as usize)
}

// Lane-wise op macros (the comptime-helper analogs). `$f` is the frame.
macro_rules! simd_bin {
    ($f:expr, $up:ident, $pk:ident, |$a:ident, $b:ident| $body:expr) => {{
        let bb = $up($f.pop());
        let aa = $up($f.pop());
        let mut r = aa;
        for (i, slot) in r.iter_mut().enumerate() {
            let $a = aa[i];
            let $b = bb[i];
            *slot = $body;
        }
        $f.push($pk(r));
    }};
}
macro_rules! simd_un {
    ($f:expr, $up:ident, $pk:ident, |$x:ident| $body:expr) => {{
        let aa = $up($f.pop());
        let mut r = aa;
        for (i, slot) in r.iter_mut().enumerate() {
            let $x = aa[i];
            *slot = $body;
        }
        $f.push($pk(r));
    }};
}
macro_rules! simd_cmp {
    ($f:expr, $up:ident, $pk:ident, $ures:ty, |$a:ident, $b:ident| $body:expr) => {{
        let bb = $up($f.pop());
        let aa = $up($f.pop());
        $f.push($pk(core::array::from_fn(|i| {
            let $a = aa[i];
            let $b = bb[i];
            if $body { <$ures>::MAX } else { 0 }
        })));
    }};
}
macro_rules! simd_shift {
    ($f:expr, $up:ident, $pk:ident, $bits:expr, $op:tt) => {{
        let amt = ($f.pop_i32() as u32) % $bits;
        let aa = $up($f.pop());
        $f.push($pk(aa.map(|x| x $op amt)));
    }};
}
macro_rules! simd_extend {
    ($f:expr, $up:ident, $pk:ident, $dst:ty, $half:expr, $high:expr) => {{
        let src = $up($f.pop());
        let base: usize = if $high { $half } else { 0 };
        $f.push($pk(core::array::from_fn(|i| src[base + i] as $dst)));
    }};
}
macro_rules! simd_narrow {
    ($f:expr, $up:ident, $pk:ident, $src:ty, $dst:ty) => {{
        let b = $up($f.pop());
        let a = $up($f.pop());
        let half = a.len();
        $f.push($pk(core::array::from_fn(|i| {
            let x: $src = if i < half { a[i] } else { b[i - half] };
            x.clamp(<$dst>::MIN as $src, <$dst>::MAX as $src) as $dst
        })));
    }};
}
macro_rules! simd_convert {
    ($f:expr, $up:ident, $pk:ident, $dst:ty) => {{
        let src = $up($f.pop());
        $f.push($pk(core::array::from_fn(|i| src[i] as $dst)));
    }};
}
macro_rules! simd_extmul {
    ($f:expr, $up:ident, $pk:ident, $dst:ty, $n:expr, $high:expr) => {{
        let b = $up($f.pop());
        let a = $up($f.pop());
        let base: usize = if $high { $n } else { 0 };
        $f.push($pk(core::array::from_fn(|i| (a[base + i] as $dst) * (b[base + i] as $dst))));
    }};
}
macro_rules! simd_extadd {
    ($f:expr, $up:ident, $pk:ident, $dst:ty) => {{
        let src = $up($f.pop());
        $f.push($pk(core::array::from_fn(|i| src[2 * i] as $dst + src[2 * i + 1] as $dst)));
    }};
}
macro_rules! simd_load_extend {
    ($f:expr, $store:expr, $mem:expr, $srcty:ty, $srcsz:expr, $n:expr, $pk:ident, $dst:ty) => {{
        let ea = simd_mem_ea($f, $store, $mem, 8)?;
        let m = &$store.memories[$mem.memory as usize];
        let src: [$srcty; $n] = core::array::from_fn(|i| {
            <$srcty>::from_le_bytes(m.bytes[ea + i * $srcsz..ea + i * $srcsz + $srcsz].try_into().unwrap())
        });
        $f.push($pk(core::array::from_fn(|i| src[i] as $dst)));
    }};
}

/// Execute a `0xFD` SIMD instruction. Covers the entire fixed-width + relaxed SIMD
/// set; an unknown sub-opcode traps `UnsupportedInstruction`.
#[allow(clippy::too_many_lines)]
fn exec_simd(frame: &mut Frame, store: &mut Store, s: crate::opcode::Simd) -> Result<()> {
    let lane = s.lane as usize;
    match s.sub {
        // --- const / load / store ---
        0x0c => frame.push(s.bytes), // v128.const
        0x00 => {
            let ea = simd_mem_ea(frame, store, s.mem, 16)?;
            let m = &store.memories[s.mem.memory as usize];
            frame.push(u128::from_le_bytes(m.bytes[ea..ea + 16].try_into().unwrap()));
        }
        0x0b => {
            let v = frame.pop();
            let ea = simd_mem_ea(frame, store, s.mem, 16)?;
            store.memories[s.mem.memory as usize].bytes[ea..ea + 16].copy_from_slice(&v.to_le_bytes());
        }
        // --- shuffle / swizzle ---
        0x0d => {
            let b = v_u8x16(frame.pop());
            let a = v_u8x16(frame.pop());
            let idx = s.bytes.to_le_bytes();
            frame.push(p_u8x16(core::array::from_fn(|i| {
                let j = idx[i] as usize;
                if j < 16 {
                    a[j]
                } else if j < 32 {
                    b[j - 16]
                } else {
                    0
                }
            })));
        }
        0x0e | 0x100 => {
            let idx = v_u8x16(frame.pop());
            let a = v_u8x16(frame.pop());
            frame.push(p_u8x16(core::array::from_fn(|i| {
                let j = idx[i] as usize;
                if j < 16 { a[j] } else { 0 }
            })));
        }
        // --- splat ---
        0x0f => {
            let x = frame.pop_i32() as u8;
            frame.push(p_u8x16([x; 16]));
        }
        0x10 => {
            let x = frame.pop_i32() as u16;
            frame.push(p_u16x8([x; 8]));
        }
        0x11 => {
            let x = frame.pop_i32() as u32;
            frame.push(p_u32x4([x; 4]));
        }
        0x12 => {
            let x = frame.pop_i64() as u64;
            frame.push(p_u64x2([x; 2]));
        }
        0x13 => {
            let x = frame.pop() as u32;
            frame.push(p_u32x4([x; 4]));
        }
        0x14 => {
            let x = frame.pop() as u64;
            frame.push(p_u64x2([x; 2]));
        }
        // --- extract_lane ---
        0x15 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_i8x16(v)[lane]));
        }
        0x16 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u8x16(v)[lane]));
        }
        0x18 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_i16x8(v)[lane]));
        }
        0x19 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u16x8(v)[lane]));
        }
        0x1b => {
            let v = frame.pop();
            frame.push_i32(v_i32x4(v)[lane]);
        }
        0x1d => {
            let v = frame.pop();
            frame.push(Value::from(v_u64x2(v)[lane]));
        }
        0x1f => {
            let v = frame.pop();
            frame.push(Value::from(v_u32x4(v)[lane]));
        }
        0x21 => {
            let v = frame.pop();
            frame.push(Value::from(v_u64x2(v)[lane]));
        }
        // --- replace_lane ---
        0x17 => {
            let x = frame.pop_i32() as u8;
            let mut a = v_u8x16(frame.pop());
            a[lane] = x;
            frame.push(p_u8x16(a));
        }
        0x1a => {
            let x = frame.pop_i32() as u16;
            let mut a = v_u16x8(frame.pop());
            a[lane] = x;
            frame.push(p_u16x8(a));
        }
        0x1c => {
            let x = frame.pop_i32() as u32;
            let mut a = v_u32x4(frame.pop());
            a[lane] = x;
            frame.push(p_u32x4(a));
        }
        0x1e => {
            let x = frame.pop_i64() as u64;
            let mut a = v_u64x2(frame.pop());
            a[lane] = x;
            frame.push(p_u64x2(a));
        }
        0x20 => {
            let x = frame.pop() as u32;
            let mut a = v_u32x4(frame.pop());
            a[lane] = x;
            frame.push(p_u32x4(a));
        }
        0x22 => {
            let x = frame.pop() as u64;
            let mut a = v_u64x2(frame.pop());
            a[lane] = x;
            frame.push(p_u64x2(a));
        }
        // --- comparisons ---
        0x23 => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a == b),
        0x24 => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a != b),
        0x25 => simd_cmp!(frame, v_i8x16, p_u8x16, u8, |a, b| a < b),
        0x26 => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a < b),
        0x27 => simd_cmp!(frame, v_i8x16, p_u8x16, u8, |a, b| a > b),
        0x28 => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a > b),
        0x29 => simd_cmp!(frame, v_i8x16, p_u8x16, u8, |a, b| a <= b),
        0x2a => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a <= b),
        0x2b => simd_cmp!(frame, v_i8x16, p_u8x16, u8, |a, b| a >= b),
        0x2c => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a >= b),
        0x2d => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a == b),
        0x2e => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a != b),
        0x2f => simd_cmp!(frame, v_i16x8, p_u16x8, u16, |a, b| a < b),
        0x30 => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a < b),
        0x31 => simd_cmp!(frame, v_i16x8, p_u16x8, u16, |a, b| a > b),
        0x32 => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a > b),
        0x33 => simd_cmp!(frame, v_i16x8, p_u16x8, u16, |a, b| a <= b),
        0x34 => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a <= b),
        0x35 => simd_cmp!(frame, v_i16x8, p_u16x8, u16, |a, b| a >= b),
        0x36 => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a >= b),
        0x37 => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a == b),
        0x38 => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a != b),
        0x39 => simd_cmp!(frame, v_i32x4, p_u32x4, u32, |a, b| a < b),
        0x3a => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a < b),
        0x3b => simd_cmp!(frame, v_i32x4, p_u32x4, u32, |a, b| a > b),
        0x3c => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a > b),
        0x3d => simd_cmp!(frame, v_i32x4, p_u32x4, u32, |a, b| a <= b),
        0x3e => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a <= b),
        0x3f => simd_cmp!(frame, v_i32x4, p_u32x4, u32, |a, b| a >= b),
        0x40 => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a >= b),
        0x41 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a == b),
        0x42 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a != b),
        0x43 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a < b),
        0x44 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a > b),
        0x45 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a <= b),
        0x46 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a >= b),
        0x47 => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a == b),
        0x48 => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a != b),
        0x49 => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a < b),
        0x4a => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a > b),
        0x4b => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a <= b),
        0x4c => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a >= b),
        0xd6 => simd_cmp!(frame, v_u64x2, p_u64x2, u64, |a, b| a == b),
        0xd7 => simd_cmp!(frame, v_u64x2, p_u64x2, u64, |a, b| a != b),
        0xd8 => simd_cmp!(frame, v_i64x2, p_u64x2, u64, |a, b| a < b),
        0xd9 => simd_cmp!(frame, v_i64x2, p_u64x2, u64, |a, b| a > b),
        0xda => simd_cmp!(frame, v_i64x2, p_u64x2, u64, |a, b| a <= b),
        0xdb => simd_cmp!(frame, v_i64x2, p_u64x2, u64, |a, b| a >= b),
        // --- bitwise (whole register) ---
        0x4d => {
            let v = frame.pop();
            frame.push(!v);
        }
        0x4e => {
            let b = frame.pop();
            let a = frame.pop();
            frame.push(a & b);
        }
        0x4f => {
            let b = frame.pop();
            let a = frame.pop();
            frame.push(a & !b);
        }
        0x50 => {
            let b = frame.pop();
            let a = frame.pop();
            frame.push(a | b);
        }
        0x51 => {
            let b = frame.pop();
            let a = frame.pop();
            frame.push(a ^ b);
        }
        0x52 => {
            let c = frame.pop();
            let b = frame.pop();
            let a = frame.pop();
            frame.push((a & c) | (b & !c));
        }
        0x53 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v != 0)); // v128.any_true
        }
        // --- i8x16 ---
        0x60 => simd_un!(frame, v_i8x16, p_i8x16, |x| x.wrapping_abs()),
        0x61 => simd_un!(frame, v_i8x16, p_i8x16, |x| x.wrapping_neg()),
        0x62 => simd_un!(frame, v_u8x16, p_u8x16, |x| x.count_ones() as u8),
        0x63 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u8x16(v).iter().all(|&x| x != 0)));
        }
        0x64 => {
            let v = frame.pop();
            frame.push_i32(simd_bitmask(&v_u8x16(v), 8));
        }
        0x6b => simd_shift!(frame, v_u8x16, p_u8x16, 8, <<),
        0x6c => simd_shift!(frame, v_i8x16, p_i8x16, 8, >>),
        0x6d => simd_shift!(frame, v_u8x16, p_u8x16, 8, >>),
        0x6e => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.wrapping_add(b)),
        0x6f => simd_bin!(frame, v_i8x16, p_i8x16, |a, b| a.saturating_add(b)),
        0x70 => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.saturating_add(b)),
        0x71 => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.wrapping_sub(b)),
        0x72 => simd_bin!(frame, v_i8x16, p_i8x16, |a, b| a.saturating_sub(b)),
        0x73 => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.saturating_sub(b)),
        0x76 => simd_bin!(frame, v_i8x16, p_i8x16, |a, b| a.min(b)),
        0x77 => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.min(b)),
        0x78 => simd_bin!(frame, v_i8x16, p_i8x16, |a, b| a.max(b)),
        0x79 => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.max(b)),
        0x7b => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| ((u16::from(a) + u16::from(b) + 1) >> 1) as u8),
        0x7c => simd_extadd!(frame, v_i8x16, p_i16x8, i16),
        0x7d => simd_extadd!(frame, v_u8x16, p_u16x8, u16),
        0x7e => simd_extadd!(frame, v_i16x8, p_i32x4, i32),
        0x7f => simd_extadd!(frame, v_u16x8, p_u32x4, u32),
        // --- i16x8 ---
        0x80 => simd_un!(frame, v_i16x8, p_i16x8, |x| x.wrapping_abs()),
        0x81 => simd_un!(frame, v_i16x8, p_i16x8, |x| x.wrapping_neg()),
        0x82 => {
            let b = v_i16x8(frame.pop());
            let a = v_i16x8(frame.pop());
            frame.push(p_i16x8(core::array::from_fn(|i| {
                let p = (i32::from(a[i]) * i32::from(b[i]) + 0x4000) >> 15;
                p.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
            })));
        }
        0x83 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u16x8(v).iter().all(|&x| x != 0)));
        }
        0x84 => {
            let v = frame.pop();
            frame.push_i32(simd_bitmask(&v_u16x8(v), 16));
        }
        0x85 => simd_narrow!(frame, v_i32x4, p_i16x8, i32, i16),
        0x86 => simd_narrow!(frame, v_i32x4, p_u16x8, i32, u16),
        0x87 => simd_extend!(frame, v_i8x16, p_i16x8, i16, 8, false),
        0x88 => simd_extend!(frame, v_i8x16, p_i16x8, i16, 8, true),
        0x89 => simd_extend!(frame, v_u8x16, p_u16x8, u16, 8, false),
        0x8a => simd_extend!(frame, v_u8x16, p_u16x8, u16, 8, true),
        0x8b => simd_shift!(frame, v_u16x8, p_u16x8, 16, <<),
        0x8c => simd_shift!(frame, v_i16x8, p_i16x8, 16, >>),
        0x8d => simd_shift!(frame, v_u16x8, p_u16x8, 16, >>),
        0x8e => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.wrapping_add(b)),
        0x8f => simd_bin!(frame, v_i16x8, p_i16x8, |a, b| a.saturating_add(b)),
        0x90 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.saturating_add(b)),
        0x91 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.wrapping_sub(b)),
        0x92 => simd_bin!(frame, v_i16x8, p_i16x8, |a, b| a.saturating_sub(b)),
        0x93 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.saturating_sub(b)),
        0x95 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.wrapping_mul(b)),
        0x96 => simd_bin!(frame, v_i16x8, p_i16x8, |a, b| a.min(b)),
        0x97 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.min(b)),
        0x98 => simd_bin!(frame, v_i16x8, p_i16x8, |a, b| a.max(b)),
        0x99 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.max(b)),
        0x9b => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| ((u32::from(a) + u32::from(b) + 1) >> 1) as u16),
        0x9c => simd_extmul!(frame, v_i8x16, p_i16x8, i16, 8, false),
        0x9d => simd_extmul!(frame, v_i8x16, p_i16x8, i16, 8, true),
        0x9e => simd_extmul!(frame, v_u8x16, p_u16x8, u16, 8, false),
        0x9f => simd_extmul!(frame, v_u8x16, p_u16x8, u16, 8, true),
        // --- i32x4 ---
        0xa0 => simd_un!(frame, v_i32x4, p_i32x4, |x| x.wrapping_abs()),
        0xa1 => simd_un!(frame, v_i32x4, p_i32x4, |x| x.wrapping_neg()),
        0xa3 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u32x4(v).iter().all(|&x| x != 0)));
        }
        0xa4 => {
            let v = frame.pop();
            frame.push_i32(simd_bitmask(&v_u32x4(v), 32));
        }
        0xa7 => simd_extend!(frame, v_i16x8, p_i32x4, i32, 4, false),
        0xa8 => simd_extend!(frame, v_i16x8, p_i32x4, i32, 4, true),
        0xa9 => simd_extend!(frame, v_u16x8, p_u32x4, u32, 4, false),
        0xaa => simd_extend!(frame, v_u16x8, p_u32x4, u32, 4, true),
        0xab => simd_shift!(frame, v_u32x4, p_u32x4, 32, <<),
        0xac => simd_shift!(frame, v_i32x4, p_i32x4, 32, >>),
        0xad => simd_shift!(frame, v_u32x4, p_u32x4, 32, >>),
        0xae => simd_bin!(frame, v_u32x4, p_u32x4, |a, b| a.wrapping_add(b)),
        0xb1 => simd_bin!(frame, v_u32x4, p_u32x4, |a, b| a.wrapping_sub(b)),
        0xb5 => simd_bin!(frame, v_u32x4, p_u32x4, |a, b| a.wrapping_mul(b)),
        0xb6 => simd_bin!(frame, v_i32x4, p_i32x4, |a, b| a.min(b)),
        0xb7 => simd_bin!(frame, v_u32x4, p_u32x4, |a, b| a.min(b)),
        0xb8 => simd_bin!(frame, v_i32x4, p_i32x4, |a, b| a.max(b)),
        0xb9 => simd_bin!(frame, v_u32x4, p_u32x4, |a, b| a.max(b)),
        0xba => {
            let b = v_i16x8(frame.pop());
            let a = v_i16x8(frame.pop());
            frame.push(p_i32x4(core::array::from_fn(|i| {
                (i32::from(a[2 * i]) * i32::from(b[2 * i]))
                    .wrapping_add(i32::from(a[2 * i + 1]) * i32::from(b[2 * i + 1]))
            })));
        }
        0xbc => simd_extmul!(frame, v_i16x8, p_i32x4, i32, 4, false),
        0xbd => simd_extmul!(frame, v_i16x8, p_i32x4, i32, 4, true),
        0xbe => simd_extmul!(frame, v_u16x8, p_u32x4, u32, 4, false),
        0xbf => simd_extmul!(frame, v_u16x8, p_u32x4, u32, 4, true),
        // --- i64x2 ---
        0xc0 => simd_un!(frame, v_i64x2, p_i64x2, |x| x.wrapping_abs()),
        0xc1 => simd_un!(frame, v_i64x2, p_i64x2, |x| x.wrapping_neg()),
        0xc3 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u64x2(v).iter().all(|&x| x != 0)));
        }
        0xc4 => {
            let v = frame.pop();
            frame.push_i32(simd_bitmask(&v_u64x2(v), 64));
        }
        0xc7 => simd_extend!(frame, v_i32x4, p_i64x2, i64, 2, false),
        0xc8 => simd_extend!(frame, v_i32x4, p_i64x2, i64, 2, true),
        0xc9 => simd_extend!(frame, v_u32x4, p_u64x2, u64, 2, false),
        0xca => simd_extend!(frame, v_u32x4, p_u64x2, u64, 2, true),
        0xcb => simd_shift!(frame, v_u64x2, p_u64x2, 64, <<),
        0xcc => simd_shift!(frame, v_i64x2, p_i64x2, 64, >>),
        0xcd => simd_shift!(frame, v_u64x2, p_u64x2, 64, >>),
        0xce => simd_bin!(frame, v_u64x2, p_u64x2, |a, b| a.wrapping_add(b)),
        0xd1 => simd_bin!(frame, v_u64x2, p_u64x2, |a, b| a.wrapping_sub(b)),
        0xd5 => simd_bin!(frame, v_u64x2, p_u64x2, |a, b| a.wrapping_mul(b)),
        0xdc => simd_extmul!(frame, v_i32x4, p_i64x2, i64, 2, false),
        0xdd => simd_extmul!(frame, v_i32x4, p_i64x2, i64, 2, true),
        0xde => simd_extmul!(frame, v_u32x4, p_u64x2, u64, 2, false),
        0xdf => simd_extmul!(frame, v_u32x4, p_u64x2, u64, 2, true),
        // --- f32x4 ---
        0x67 => simd_un!(frame, v_f32x4, p_f32x4, |x| ceil_f32(x)),
        0x68 => simd_un!(frame, v_f32x4, p_f32x4, |x| floor_f32(x)),
        0x69 => simd_un!(frame, v_f32x4, p_f32x4, |x| trunc_f32(x)),
        0x6a => simd_un!(frame, v_f32x4, p_f32x4, |x| nearest_f32(x)),
        0xe0 => simd_un!(frame, v_f32x4, p_f32x4, |x| fabs_f32(x)),
        0xe1 => simd_un!(frame, v_f32x4, p_f32x4, |x| fneg_f32(x)),
        0xe3 => {
            let a = v_f32x4(frame.pop());
            let mut r = a;
            for (i, slot) in r.iter_mut().enumerate() {
                *slot = sqrt_f32(a[i])?;
            }
            frame.push(p_f32x4(r));
        }
        0xe4 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| a + b),
        0xe5 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| a - b),
        0xe6 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| a * b),
        0xe7 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| a / b),
        0xe8 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| fmin_f32(a, b)),
        0xe9 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| fmax_f32(a, b)),
        0xea => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| if b < a { b } else { a }),
        0xeb => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| if a < b { b } else { a }),
        // --- f64x2 ---
        0x74 => simd_un!(frame, v_f64x2, p_f64x2, |x| ceil_f64(x)),
        0x75 => simd_un!(frame, v_f64x2, p_f64x2, |x| floor_f64(x)),
        0x7a => simd_un!(frame, v_f64x2, p_f64x2, |x| trunc_f64(x)),
        0x94 => simd_un!(frame, v_f64x2, p_f64x2, |x| nearest_f64(x)),
        0xec => simd_un!(frame, v_f64x2, p_f64x2, |x| fabs_f64(x)),
        0xed => simd_un!(frame, v_f64x2, p_f64x2, |x| fneg_f64(x)),
        0xef => {
            let a = v_f64x2(frame.pop());
            let mut r = a;
            for (i, slot) in r.iter_mut().enumerate() {
                *slot = sqrt_f64(a[i])?;
            }
            frame.push(p_f64x2(r));
        }
        0xf0 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| a + b),
        0xf1 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| a - b),
        0xf2 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| a * b),
        0xf3 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| a / b),
        0xf4 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| fmin_f64(a, b)),
        0xf5 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| fmax_f64(a, b)),
        0xf6 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| if b < a { b } else { a }),
        0xf7 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| if a < b { b } else { a }),
        // --- saturating add/sub group members already handled above; narrow group ---
        0x65 => simd_narrow!(frame, v_i16x8, p_i8x16, i16, i8),
        0x66 => simd_narrow!(frame, v_i16x8, p_u8x16, i16, u8),
        // --- int<->float conversions ---
        0xfa => simd_convert!(frame, v_i32x4, p_f32x4, f32),
        0xfb => simd_convert!(frame, v_u32x4, p_f32x4, f32),
        0xfe => simd_convert!(frame, v_i32x4, p_f64x2, f64),
        0xff => simd_convert!(frame, v_u32x4, p_f64x2, f64),
        0xf8 | 0x101 => {
            let s4 = v_f32x4(frame.pop());
            frame.push(p_i32x4(core::array::from_fn(|i| sat_trunc_i32(f64::from(s4[i])))));
        }
        0xf9 | 0x102 => {
            let s4 = v_f32x4(frame.pop());
            frame.push(p_u32x4(core::array::from_fn(|i| sat_trunc_u32(f64::from(s4[i])))));
        }
        0xfc | 0x103 => {
            let s2 = v_f64x2(frame.pop());
            frame.push(p_i32x4(core::array::from_fn(|i| if i < 2 { sat_trunc_i32(s2[i]) } else { 0 })));
        }
        0xfd | 0x104 => {
            let s2 = v_f64x2(frame.pop());
            frame.push(p_u32x4(core::array::from_fn(|i| if i < 2 { sat_trunc_u32(s2[i]) } else { 0 })));
        }
        0x5e => {
            let s2 = v_f64x2(frame.pop());
            frame.push(p_f32x4([s2[0] as f32, s2[1] as f32, 0.0, 0.0]));
        }
        0x5f => {
            let s4 = v_f32x4(frame.pop());
            frame.push(p_f64x2([f64::from(s4[0]), f64::from(s4[1])]));
        }
        // --- widening loads / splat / zero ---
        0x01 => simd_load_extend!(frame, store, s.mem, i8, 1, 8, p_i16x8, i16),
        0x02 => simd_load_extend!(frame, store, s.mem, u8, 1, 8, p_u16x8, u16),
        0x03 => simd_load_extend!(frame, store, s.mem, i16, 2, 4, p_i32x4, i32),
        0x04 => simd_load_extend!(frame, store, s.mem, u16, 2, 4, p_u32x4, u32),
        0x05 => simd_load_extend!(frame, store, s.mem, i32, 4, 2, p_i64x2, i64),
        0x06 => simd_load_extend!(frame, store, s.mem, u32, 4, 2, p_u64x2, u64),
        0x07 => {
            let ea = simd_mem_ea(frame, store, s.mem, 1)?;
            let x = store.memories[s.mem.memory as usize].bytes[ea];
            frame.push(p_u8x16([x; 16]));
        }
        0x08 => {
            let ea = simd_mem_ea(frame, store, s.mem, 2)?;
            let m = &store.memories[s.mem.memory as usize];
            let x = u16::from_le_bytes(m.bytes[ea..ea + 2].try_into().unwrap());
            frame.push(p_u16x8([x; 8]));
        }
        0x09 => {
            let ea = simd_mem_ea(frame, store, s.mem, 4)?;
            let m = &store.memories[s.mem.memory as usize];
            let x = u32::from_le_bytes(m.bytes[ea..ea + 4].try_into().unwrap());
            frame.push(p_u32x4([x; 4]));
        }
        0x0a => {
            let ea = simd_mem_ea(frame, store, s.mem, 8)?;
            let m = &store.memories[s.mem.memory as usize];
            let x = u64::from_le_bytes(m.bytes[ea..ea + 8].try_into().unwrap());
            frame.push(p_u64x2([x; 2]));
        }
        0x5c => {
            let ea = simd_mem_ea(frame, store, s.mem, 4)?;
            let m = &store.memories[s.mem.memory as usize];
            let mut b = [0u8; 16];
            b[0..4].copy_from_slice(&m.bytes[ea..ea + 4]);
            frame.push(Value::from_le_bytes(b));
        }
        0x5d => {
            let ea = simd_mem_ea(frame, store, s.mem, 8)?;
            let m = &store.memories[s.mem.memory as usize];
            let mut b = [0u8; 16];
            b[0..8].copy_from_slice(&m.bytes[ea..ea + 8]);
            frame.push(Value::from_le_bytes(b));
        }
        // --- load_lane / store_lane ---
        0x54 => {
            let mut a = v_u8x16(frame.pop());
            let ea = simd_mem_ea(frame, store, s.mem, 1)?;
            a[lane] = store.memories[s.mem.memory as usize].bytes[ea];
            frame.push(p_u8x16(a));
        }
        0x55 => {
            let mut a = v_u16x8(frame.pop());
            let ea = simd_mem_ea(frame, store, s.mem, 2)?;
            let m = &store.memories[s.mem.memory as usize];
            a[lane] = u16::from_le_bytes(m.bytes[ea..ea + 2].try_into().unwrap());
            frame.push(p_u16x8(a));
        }
        0x56 => {
            let mut a = v_u32x4(frame.pop());
            let ea = simd_mem_ea(frame, store, s.mem, 4)?;
            let m = &store.memories[s.mem.memory as usize];
            a[lane] = u32::from_le_bytes(m.bytes[ea..ea + 4].try_into().unwrap());
            frame.push(p_u32x4(a));
        }
        0x57 => {
            let mut a = v_u64x2(frame.pop());
            let ea = simd_mem_ea(frame, store, s.mem, 8)?;
            let m = &store.memories[s.mem.memory as usize];
            a[lane] = u64::from_le_bytes(m.bytes[ea..ea + 8].try_into().unwrap());
            frame.push(p_u64x2(a));
        }
        0x58 => {
            let a = v_u8x16(frame.pop());
            let ea = simd_mem_ea(frame, store, s.mem, 1)?;
            store.memories[s.mem.memory as usize].bytes[ea] = a[lane];
        }
        0x59 => {
            let a = v_u16x8(frame.pop());
            let ea = simd_mem_ea(frame, store, s.mem, 2)?;
            store.memories[s.mem.memory as usize].bytes[ea..ea + 2].copy_from_slice(&a[lane].to_le_bytes());
        }
        0x5a => {
            let a = v_u32x4(frame.pop());
            let ea = simd_mem_ea(frame, store, s.mem, 4)?;
            store.memories[s.mem.memory as usize].bytes[ea..ea + 4].copy_from_slice(&a[lane].to_le_bytes());
        }
        0x5b => {
            let a = v_u64x2(frame.pop());
            let ea = simd_mem_ea(frame, store, s.mem, 8)?;
            store.memories[s.mem.memory as usize].bytes[ea..ea + 8].copy_from_slice(&a[lane].to_le_bytes());
        }
        // --- relaxed SIMD (deterministic choices per wazmrt) ---
        0x105 => {
            let c = v_f32x4(frame.pop());
            let b = v_f32x4(frame.pop());
            let a = v_f32x4(frame.pop());
            frame.push(p_f32x4(core::array::from_fn(|i| a[i] * b[i] + c[i])));
        }
        0x106 => {
            let c = v_f32x4(frame.pop());
            let b = v_f32x4(frame.pop());
            let a = v_f32x4(frame.pop());
            frame.push(p_f32x4(core::array::from_fn(|i| c[i] - a[i] * b[i])));
        }
        0x107 => {
            let c = v_f64x2(frame.pop());
            let b = v_f64x2(frame.pop());
            let a = v_f64x2(frame.pop());
            frame.push(p_f64x2(core::array::from_fn(|i| a[i] * b[i] + c[i])));
        }
        0x108 => {
            let c = v_f64x2(frame.pop());
            let b = v_f64x2(frame.pop());
            let a = v_f64x2(frame.pop());
            frame.push(p_f64x2(core::array::from_fn(|i| c[i] - a[i] * b[i])));
        }
        0x109..=0x10c => {
            let m = frame.pop();
            let b = frame.pop();
            let a = frame.pop();
            frame.push((a & m) | (b & !m));
        }
        0x10d => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| fmin_f32(a, b)),
        0x10e => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| fmax_f32(a, b)),
        0x10f => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| fmin_f64(a, b)),
        0x110 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| fmax_f64(a, b)),
        0x111 => {
            let b = v_i16x8(frame.pop());
            let a = v_i16x8(frame.pop());
            frame.push(p_i16x8(core::array::from_fn(|i| {
                let p = (i32::from(a[i]) * i32::from(b[i]) + 0x4000) >> 15;
                p.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
            })));
        }
        0x112 => {
            let b = v_i8x16(frame.pop());
            let a = v_i8x16(frame.pop());
            frame.push(p_i16x8(core::array::from_fn(|i| {
                let s = i32::from(a[2 * i]) * i32::from(b[2 * i])
                    + i32::from(a[2 * i + 1]) * i32::from(b[2 * i + 1]);
                s.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
            })));
        }
        0x113 => {
            let c = v_i32x4(frame.pop());
            let b = v_i8x16(frame.pop());
            let a = v_i8x16(frame.pop());
            frame.push(p_i32x4(core::array::from_fn(|i| {
                let p0 = i32::from(a[4 * i]) * i32::from(b[4 * i])
                    + i32::from(a[4 * i + 1]) * i32::from(b[4 * i + 1]);
                let p1 = i32::from(a[4 * i + 2]) * i32::from(b[4 * i + 2])
                    + i32::from(a[4 * i + 3]) * i32::from(b[4 * i + 3]);
                p0.wrapping_add(p1).wrapping_add(c[i])
            })));
        }
        _ => return Err(Trap::UnsupportedInstruction),
    }
    Ok(())
}

/// Pack each lane's high (sign) bit into the low bits of an i32 (`iNxM.bitmask`).
fn simd_bitmask<T: Copy + Into<u128>>(lanes: &[T], bits: u32) -> i32 {
    let mut m: u32 = 0;
    for (i, &x) in lanes.iter().enumerate() {
        if (x.into() >> (bits - 1)) & 1 != 0 {
            m |= 1 << i;
        }
    }
    m as i32
}

// ========================= Threads / atomics (0xFE) ========================
//
// Single-threaded: every atomic access is trivially atomic and `atomic.fence` is
// a no-op. Atomic accesses trap on a misaligned effective address (unlike ordinary
// loads/stores), and `wait*` requires a shared memory. `wait*` never blocks — a
// value mismatch returns 1 ("not equal"), a match returns 2 ("timed out", since no
// thread can notify); `notify` wakes 0. Ported from wazmrt `interp.zig execAtomic`.

/// Read `width` (1/2/4/8) little-endian bytes at `mem[ea..]`, zero-extended to u64.
fn atomic_read(mem: &[u8], ea: usize, width: u64) -> u64 {
    match width {
        1 => u64::from(mem[ea]),
        2 => u64::from(u16::from_le_bytes(mem[ea..ea + 2].try_into().unwrap())),
        4 => u64::from(u32::from_le_bytes(mem[ea..ea + 4].try_into().unwrap())),
        _ => u64::from_le_bytes(mem[ea..ea + 8].try_into().unwrap()),
    }
}
/// Write the low `width` bytes of `v` little-endian at `mem[ea..]`.
fn atomic_write(mem: &mut [u8], ea: usize, width: u64, v: u64) {
    match width {
        1 => mem[ea] = v as u8,
        2 => mem[ea..ea + 2].copy_from_slice(&(v as u16).to_le_bytes()),
        4 => mem[ea..ea + 4].copy_from_slice(&(v as u32).to_le_bytes()),
        _ => mem[ea..ea + 8].copy_from_slice(&v.to_le_bytes()),
    }
}
/// Keep only the low `width` bytes of `v` (a sub-width atomic op's access width).
fn mask_width(v: u64, width: u64) -> u64 {
    if width >= 8 {
        v
    } else {
        v & ((1u64 << (width * 8)) - 1)
    }
}
/// True if the atomic op at sub-opcode `sub` operates on an `i64` (vs `i32`).
fn atomic_is64(sub: u32) -> bool {
    match sub {
        0x11 | 0x14 | 0x15 | 0x16 | 0x18 | 0x1b | 0x1c | 0x1d => true,
        0x10 | 0x12 | 0x13 | 0x17 | 0x19 | 0x1a => false,
        // rmw/cmpxchg 7-op group [i32.full, i64.full, i32.8, i32.16, i64.8, i64.16, i64.32]
        _ => matches!((sub - 0x1e) % 7, 1 | 4 | 5 | 6),
    }
}
/// log2 of an atomic op's natural access width in bytes (its required alignment).
fn atomic_align_log2(sub: u32) -> u32 {
    match sub {
        0x00 | 0x01 => 2, // notify, wait32
        0x02 => 3,        // wait64
        0x03 => 0,        // fence (no memarg)
        0x10 | 0x17 => 2, // i32 load/store
        0x11 | 0x18 => 3, // i64 load/store
        0x12 | 0x19 => 0, // i32 …8
        0x13 | 0x1a => 1, // i32 …16
        0x14 | 0x1b => 0, // i64 …8
        0x15 | 0x1c => 1, // i64 …16
        0x16 | 0x1d => 2, // i64 …32
        _ => match (sub.wrapping_sub(0x1e)) % 7 {
            0 => 2, // i32 full
            1 => 3, // i64 full
            2 => 0, // i32.8
            3 => 1, // i32.16
            4 => 0, // i64.8
            5 => 1, // i64.16
            _ => 2, // i64.32
        },
    }
}

/// Pop an atomic op's address, bounds- + **alignment**-check it, require a shared
/// memory when `need_shared`. Returns the effective byte offset into `at.mem.memory`.
fn atomic_ea(
    frame: &mut Frame,
    store: &Store,
    at: crate::opcode::Atomic,
    width: u64,
    need_shared: bool,
) -> Result<usize> {
    let mem = store.memories.get(at.mem.memory as usize).ok_or(Trap::NoMemory)?;
    let base = frame.pop_mem(mem.is64);
    if need_shared && !mem.shared {
        return Err(Trap::ExpectedSharedMemory);
    }
    let ea = base.checked_add(at.mem.offset).ok_or(Trap::MemoryOutOfBounds)?;
    let end = ea.checked_add(width).ok_or(Trap::MemoryOutOfBounds)?;
    if end > mem.bytes.len() as u64 {
        return Err(Trap::MemoryOutOfBounds);
    }
    if ea % width != 0 {
        return Err(Trap::UnalignedAtomic);
    }
    Ok(ea as usize)
}

/// Execute a `0xFE` atomic instruction (single-threaded semantics).
fn exec_atomic(frame: &mut Frame, store: &mut Store, at: crate::opcode::Atomic) -> Result<()> {
    let sub = at.sub;
    if sub == 0x03 {
        return Ok(()); // atomic.fence — nothing to order single-threaded
    }
    let w = 1u64 << atomic_align_log2(sub);
    let mi = at.mem.memory as usize;
    match sub {
        0x00 => {
            // memory.atomic.notify [addr count] -> [woken] (always 0 single-threaded)
            let _count = frame.pop_i32();
            let _ea = atomic_ea(frame, store, at, w, false)?;
            frame.push_i32(0);
        }
        0x01 | 0x02 => {
            // memory.atomic.wait32 / wait64 [addr expected timeout] -> [i32]
            let _timeout = frame.pop_i64();
            let expected: u64 = if sub == 0x01 {
                u64::from(frame.pop_i32() as u32)
            } else {
                frame.pop_i64() as u64
            };
            let ea = atomic_ea(frame, store, at, w, true)?;
            let cur = atomic_read(&store.memories[mi].bytes, ea, w);
            frame.push_i32(if cur != expected { 1 } else { 2 });
        }
        0x10..=0x16 => {
            // atomic load [addr] -> [T]
            let ea = atomic_ea(frame, store, at, w, false)?;
            let v = atomic_read(&store.memories[mi].bytes, ea, w);
            if atomic_is64(sub) {
                frame.push_i64(v as i64);
            } else {
                frame.push_i32(v as u32 as i32);
            }
        }
        0x17..=0x1d => {
            // atomic store [addr T] -> []
            let val: u64 = if atomic_is64(sub) {
                frame.pop_i64() as u64
            } else {
                u64::from(frame.pop_i32() as u32)
            };
            let ea = atomic_ea(frame, store, at, w, false)?;
            atomic_write(&mut store.memories[mi].bytes, ea, w, val);
        }
        0x1e..=0x4e => {
            let is64 = atomic_is64(sub);
            let group = (sub - 0x1e) / 7; // 0 add,1 sub,2 and,3 or,4 xor,5 xchg,6 cmpxchg
            if group == 6 {
                // cmpxchg [addr expected replacement] -> [old]
                let repl: u64 = if is64 {
                    frame.pop_i64() as u64
                } else {
                    u64::from(frame.pop_i32() as u32)
                };
                let expected: u64 = if is64 {
                    frame.pop_i64() as u64
                } else {
                    u64::from(frame.pop_i32() as u32)
                };
                let ea = atomic_ea(frame, store, at, w, false)?;
                let old = atomic_read(&store.memories[mi].bytes, ea, w);
                if old == mask_width(expected, w) {
                    atomic_write(&mut store.memories[mi].bytes, ea, w, repl);
                }
                if is64 {
                    frame.push_i64(old as i64);
                } else {
                    frame.push_i32(old as u32 as i32);
                }
            } else {
                // add/sub/and/or/xor/xchg [addr val] -> [old]
                let val: u64 = if is64 {
                    frame.pop_i64() as u64
                } else {
                    u64::from(frame.pop_i32() as u32)
                };
                let ea = atomic_ea(frame, store, at, w, false)?;
                let old = atomic_read(&store.memories[mi].bytes, ea, w);
                let new = match group {
                    0 => old.wrapping_add(val),
                    1 => old.wrapping_sub(val),
                    2 => old & val,
                    3 => old | val,
                    4 => old ^ val,
                    _ => val, // xchg
                };
                atomic_write(&mut store.memories[mi].bytes, ea, w, new);
                if is64 {
                    frame.push_i64(old as i64);
                } else {
                    frame.push_i32(old as u32 as i32);
                }
            }
        }
        _ => return Err(Trap::UnsupportedInstruction),
    }
    Ok(())
}

fn cmp_i32(op: u8, a: i32, b: i32) -> bool {
    let (ua, ub) = (a as u32, b as u32);
    match op {
        0x46 => a == b,
        0x47 => a != b,
        0x48 => a < b,
        0x49 => ua < ub,
        0x4a => a > b,
        0x4b => ua > ub,
        0x4c => a <= b,
        0x4d => ua <= ub,
        0x4e => a >= b,
        _ => ua >= ub, // 0x4f i32.ge_u
    }
}

fn cmp_i64(op: u8, a: i64, b: i64) -> bool {
    let (ua, ub) = (a as u64, b as u64);
    match op {
        0x51 => a == b,
        0x52 => a != b,
        0x53 => a < b,
        0x54 => ua < ub,
        0x55 => a > b,
        0x56 => ua > ub,
        0x57 => a <= b,
        0x58 => ua <= ub,
        0x59 => a >= b,
        _ => ua >= ub, // 0x5a i64.ge_u
    }
}

fn bin_i32(op: u8, a: i32, b: i32) -> Result<i32> {
    let (ua, ub) = (a as u32, b as u32);
    Ok(match op {
        0x6a => a.wrapping_add(b),
        0x6b => a.wrapping_sub(b),
        0x6c => a.wrapping_mul(b),
        0x6d => {
            // div_s
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            if a == i32::MIN && b == -1 {
                return Err(Trap::IntOverflow);
            }
            a / b
        }
        0x6e => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            (ua / ub) as i32
        }
        0x6f => {
            // rem_s
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            if b == -1 {
                0
            } else {
                a % b
            }
        }
        0x70 => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            (ua % ub) as i32
        }
        0x71 => a & b,
        0x72 => a | b,
        0x73 => a ^ b,
        0x74 => (ua << (ub % 32)) as i32,
        0x75 => a >> (ub % 32),
        0x76 => (ua >> (ub % 32)) as i32,
        0x77 => ua.rotate_left(ub % 32) as i32,
        _ => ua.rotate_right(ub % 32) as i32, // 0x78 i32.rotr
    })
}

fn bin_i64(op: u8, a: i64, b: i64) -> Result<i64> {
    let (ua, ub) = (a as u64, b as u64);
    Ok(match op {
        0x7c => a.wrapping_add(b),
        0x7d => a.wrapping_sub(b),
        0x7e => a.wrapping_mul(b),
        0x7f => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            if a == i64::MIN && b == -1 {
                return Err(Trap::IntOverflow);
            }
            a / b
        }
        0x80 => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            (ua / ub) as i64
        }
        0x81 => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            if b == -1 {
                0
            } else {
                a % b
            }
        }
        0x82 => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            (ua % ub) as i64
        }
        0x83 => a & b,
        0x84 => a | b,
        0x85 => a ^ b,
        0x86 => (ua << (ub % 64)) as i64,
        0x87 => a >> (ub % 64),
        0x88 => (ua >> (ub % 64)) as i64,
        0x89 => ua.rotate_left((ub % 64) as u32) as i64,
        _ => ua.rotate_right((ub % 64) as u32) as i64, // 0x8a i64.rotr
    })
}

/// Evaluate a defined global's constant initializer (integer/float const, `global.get` of a
/// prior global, and extended-const i32/i64 add/sub/mul). Reference and GC const-exprs are
/// deferred with the reference-type execution slice.
fn eval_const_expr(expr: &[u8], globals: &[Value]) -> Result<Value> {
    let mut r = Reader::new(expr);
    let mut stack: Vec<Value> = Vec::new();
    loop {
        match r.read_byte()? {
            0x0b => break,
            0x41 => stack.push(i32_value(r.read_var_i32()?)),
            0x42 => stack.push(i64_value(r.read_var_i64()?)),
            0x43 => stack.push(Value::from(r.read_f32_bits()?)),
            0x44 => stack.push(Value::from(r.read_f64_bits()?)),
            0x23 => {
                let gi = r.read_var_u32()? as usize;
                stack.push(*globals.get(gi).ok_or(Trap::UndefinedGlobal)?);
            }
            0xd0 => {
                // ref.null <heaptype> — the heap type is consumed; the value is the sentinel.
                crate::opcode::read_heap_type(&mut r).map_err(|_| Trap::ConstantExpr)?;
                stack.push(NULL_REF);
            }
            0xd2 => {
                // ref.func x — a funcref value is its function index.
                stack.push(Value::from(r.read_var_u32()?));
            }
            byte @ 0x6a..=0x6c => {
                let b = as_i32(stack.pop().ok_or(Trap::ConstantExpr)?);
                let a = as_i32(stack.pop().ok_or(Trap::ConstantExpr)?);
                stack.push(i32_value(bin_i32(byte, a, b)?));
            }
            byte @ 0x7c..=0x7e => {
                let b = as_i64(stack.pop().ok_or(Trap::ConstantExpr)?);
                let a = as_i64(stack.pop().ok_or(Trap::ConstantExpr)?);
                stack.push(i64_value(bin_i64(byte, a, b)?));
            }
            0xfd => {
                // v128.const — the only 0xFD op valid in a constant expression.
                if r.read_var_u32()? != 0x0c {
                    return Err(Trap::ConstantExpr);
                }
                let mut b = [0u8; 16];
                for slot in &mut b {
                    *slot = r.read_byte()?;
                }
                stack.push(Value::from_le_bytes(b));
            }
            _ => return Err(Trap::ConstantExpr),
        }
    }
    stack.pop().ok_or(Trap::ConstantExpr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::decode;

    /// Build a single-function module: one type, one function, one export, one code entry.
    /// `code_entry` is the locals-vector + body bytes (including the trailing `end`).
    fn single_func(params: &[u8], results: &[u8], code_entry: &[u8], export: &str) -> Vec<u8> {
        fn section(id: u8, content: &[u8]) -> Vec<u8> {
            let mut s = vec![id];
            write_uleb(&mut s, content.len() as u32);
            s.extend_from_slice(content);
            s
        }
        let mut type_content = vec![0x01u8, 0x60];
        write_uleb(&mut type_content, params.len() as u32);
        type_content.extend_from_slice(params);
        write_uleb(&mut type_content, results.len() as u32);
        type_content.extend_from_slice(results);

        let mut export_content = vec![0x01u8];
        write_uleb(&mut export_content, export.len() as u32);
        export_content.extend_from_slice(export.as_bytes());
        export_content.extend_from_slice(&[0x00, 0x00]); // kind func, index 0

        let mut code_content = vec![0x01u8];
        write_uleb(&mut code_content, code_entry.len() as u32);
        code_content.extend_from_slice(code_entry);

        let mut m = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        m.extend(section(0x01, &type_content));
        m.extend(section(0x03, &[0x01, 0x00])); // function: 1 func of type 0
        m.extend(section(0x07, &export_content));
        m.extend(section(0x0a, &code_content));
        m
    }

    fn run1(bytes: &[u8], export: &str, args: &[Value]) -> Result<Vec<Value>> {
        let md = decode(bytes).unwrap();
        let mut inst = Instance::new(md)?;
        inst.invoke(export, args)
    }

    #[test]
    fn runs_add() {
        // (func (param i32 i32) (result i32) local.get 0  local.get 1  i32.add)
        let entry = [0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b];
        let m = single_func(&[0x7f, 0x7f], &[0x7f], &entry, "add");
        let r = run1(&m, "add", &[i32_value(40), i32_value(2)]).unwrap();
        assert_eq!(as_i32(r[0]), 42);
    }

    #[test]
    fn runs_recursive_factorial() {
        // (func $fac (param i32) (result i32)
        //   (if (result i32) (i32.lt_s (local.get 0) (i32.const 1))
        //     (then (i32.const 1))
        //     (else (i32.mul (local.get 0) (call 0 (i32.sub (local.get 0) (i32.const 1)))))))
        let entry = [
            0x00, // no locals
            0x20, 0x00, // local.get 0
            0x41, 0x01, // i32.const 1
            0x48, // i32.lt_s
            0x04, 0x7f, // if (result i32)
            0x41, 0x01, // i32.const 1
            0x05, // else
            0x20, 0x00, // local.get 0
            0x20, 0x00, // local.get 0
            0x41, 0x01, // i32.const 1
            0x6b, // i32.sub
            0x10, 0x00, // call 0
            0x6c, // i32.mul
            0x0b, // end (if)
            0x0b, // end (func)
        ];
        let m = single_func(&[0x7f], &[0x7f], &entry, "fac");
        assert_eq!(as_i32(run1(&m, "fac", &[i32_value(5)]).unwrap()[0]), 120);
        assert_eq!(as_i32(run1(&m, "fac", &[i32_value(0)]).unwrap()[0]), 1);
        assert_eq!(as_i32(run1(&m, "fac", &[i32_value(10)]).unwrap()[0]), 3_628_800);
    }

    #[test]
    fn runs_loop_sum() {
        // sum(n) = n + (n-1) + ... + 1, via a block/loop with br_if/br.
        let entry = [
            0x01, 0x01, 0x7f, // one i32 local (the accumulator, local 1)
            0x02, 0x40, // block
            0x03, 0x40, // loop
            0x20, 0x00, // local.get 0
            0x45, // i32.eqz
            0x0d, 0x01, // br_if 1  (exit block when n == 0)
            0x20, 0x01, // local.get 1  (acc)
            0x20, 0x00, // local.get 0  (n)
            0x6a, // i32.add
            0x21, 0x01, // local.set 1  (acc += n)
            0x20, 0x00, // local.get 0
            0x41, 0x01, // i32.const 1
            0x6b, // i32.sub
            0x21, 0x00, // local.set 0  (n -= 1)
            0x0c, 0x00, // br 0  (continue loop)
            0x0b, // end (loop)
            0x0b, // end (block)
            0x20, 0x01, // local.get 1  (return acc)
            0x0b, // end (func)
        ];
        let m = single_func(&[0x7f], &[0x7f], &entry, "sum");
        assert_eq!(as_i32(run1(&m, "sum", &[i32_value(5)]).unwrap()[0]), 15);
        assert_eq!(as_i32(run1(&m, "sum", &[i32_value(100)]).unwrap()[0]), 5050);
    }

    #[test]
    fn traps_divide_by_zero() {
        // (func (param i32) (result i32) local.get 0  i32.const 0  i32.div_s)
        let entry = [0x00, 0x20, 0x00, 0x41, 0x00, 0x6d, 0x0b];
        let m = single_func(&[0x7f], &[0x7f], &entry, "d");
        assert_eq!(run1(&m, "d", &[i32_value(1)]), Err(Trap::DivByZero));
    }

    #[test]
    fn traps_unreachable() {
        let entry = [0x00, 0x00, 0x0b]; // unreachable ; end
        let m = single_func(&[], &[], &entry, "u");
        assert_eq!(run1(&m, "u", &[]), Err(Trap::Unreachable));
    }

    #[test]
    fn i64_arithmetic() {
        // (func (param i64 i64) (result i64) local.get 0  local.get 1  i64.mul)
        let entry = [0x00, 0x20, 0x00, 0x20, 0x01, 0x7e, 0x0b];
        let m = single_func(&[0x7e, 0x7e], &[0x7e], &entry, "mul");
        let r = run1(&m, "mul", &[i64_value(1_000_000), i64_value(1_000_000)]).unwrap();
        assert_eq!(as_i64(r[0]), 1_000_000_000_000);
    }

    #[test]
    fn runs_f64_add() {
        // (func (param f64 f64) (result f64) local.get 0  local.get 1  f64.add)
        let entry = [0x00, 0x20, 0x00, 0x20, 0x01, 0xa0, 0x0b];
        let m = single_func(&[0x7c, 0x7c], &[0x7c], &entry, "add");
        let r = run1(&m, "add", &[f64_value(1.5), f64_value(2.25)]).unwrap();
        assert_eq!(as_f64(r[0]), 3.75);
    }

    #[test]
    fn nearest_rounds_ties_to_even() {
        assert_eq!(nearest_f64(2.5), 2.0);
        assert_eq!(nearest_f64(3.5), 4.0);
        assert_eq!(nearest_f64(-2.5), -2.0);
        assert_eq!(nearest_f64(0.4), 0.0);
        // -0.5 rounds to -0.0 (sign of zero preserved).
        assert_eq!(nearest_f64(-0.5).to_bits(), (-0.0f64).to_bits());
        assert_eq!(nearest_f32(2.5), 2.0);
        assert_eq!(nearest_f32(3.5), 4.0);
    }

    #[test]
    fn min_max_nan_and_signed_zero() {
        assert!(fmin_f32(f32::NAN, 1.0).is_nan());
        assert!(fmax_f32(1.0, f32::NAN).is_nan());
        // min(+0,-0) = -0 ; max(+0,-0) = +0.
        assert_eq!(fmin_f32(0.0, -0.0).to_bits(), (-0.0f32).to_bits());
        assert_eq!(fmax_f32(0.0, -0.0).to_bits(), (0.0f32).to_bits());
        assert_eq!(fmin_f64(2.0, 3.0), 2.0);
        assert_eq!(fmax_f64(2.0, 3.0), 3.0);
    }

    #[test]
    fn trunc_floor_ceil() {
        assert_eq!(trunc_f64(2.7), 2.0);
        assert_eq!(trunc_f64(-2.7), -2.0);
        assert_eq!(floor_f64(-2.1), -3.0);
        assert_eq!(ceil_f64(2.1), 3.0);
        assert_eq!(trunc_f32(-0.9).to_bits(), (-0.0f32).to_bits());
    }

    #[test]
    fn float_to_int_traps_and_saturates() {
        // i32.trunc_f64_s traps on an out-of-range value; i32.trunc_sat clamps.
        let trap_entry = [0x00, 0x20, 0x00, 0xaa, 0x0b]; // local.get 0 ; i32.trunc_f64_s
        let mt = single_func(&[0x7c], &[0x7f], &trap_entry, "t");
        assert_eq!(run1(&mt, "t", &[f64_value(1e300)]), Err(Trap::InvalidConversionToInt));
        assert_eq!(as_i32(run1(&mt, "t", &[f64_value(42.9)]).unwrap()[0]), 42);

        // i32.trunc_sat_f64_s (0xfc 0x02) saturates instead of trapping.
        let sat_entry = [0x00, 0x20, 0x00, 0xfc, 0x02, 0x0b];
        let ms = single_func(&[0x7c], &[0x7f], &sat_entry, "s");
        assert_eq!(as_i32(run1(&ms, "s", &[f64_value(1e300)]).unwrap()[0]), i32::MAX);
        assert_eq!(as_i32(run1(&ms, "s", &[f64_value(f64::NAN)]).unwrap()[0]), 0);
    }

    /// Assemble a module from `(section-id, content)` pairs (magic + version prepended).
    fn asm(sections: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut m = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        for (id, content) in sections {
            m.push(*id);
            write_uleb(&mut m, content.len() as u32);
            m.extend_from_slice(content);
        }
        m
    }
    /// Wrap a code body (locals-vec + instrs incl. `end`) as a 1-entry code section content.
    fn code1(entry: &[u8]) -> Vec<u8> {
        code_n(&[entry])
    }
    /// Wrap N code bodies as a code section content.
    fn code_n(entries: &[&[u8]]) -> Vec<u8> {
        let mut c = vec![entries.len() as u8];
        for e in entries {
            write_uleb(&mut c, e.len() as u32);
            c.extend_from_slice(e);
        }
        c
    }

    #[test]
    fn call_indirect_dispatch() {
        // table [add, sub]; dispatch(op, a, b) = table[op](a, b).
        let add = [0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b];
        let sub = [0x00, 0x20, 0x00, 0x20, 0x01, 0x6b, 0x0b];
        let disp = [0x00, 0x20, 0x01, 0x20, 0x02, 0x20, 0x00, 0x11, 0x00, 0x00, 0x0b];
        let m = asm(&[
            // type 0: (i32,i32)->i32 ; type 1: (i32,i32,i32)->i32
            (1, vec![
                0x02, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, 0x60, 0x03, 0x7f, 0x7f, 0x7f, 0x01, 0x7f,
            ]),
            (3, vec![0x03, 0x00, 0x00, 0x01]), // funcs of type 0, 0, 1
            (4, vec![0x01, 0x70, 0x00, 0x02]), // table: funcref, min 2
            (7, {
                let mut e = vec![0x01, 0x08];
                e.extend_from_slice(b"dispatch");
                e.extend_from_slice(&[0x00, 0x02]); // func index 2
                e
            }),
            (9, vec![0x01, 0x00, 0x41, 0x00, 0x0b, 0x02, 0x00, 0x01]), // active elem @0: [func 0, func 1]
            (10, code_n(&[&add, &sub, &disp])),
        ]);
        let d = |op, a, b| as_i32(run1(&m, "dispatch", &[i32_value(op), i32_value(a), i32_value(b)]).unwrap()[0]);
        assert_eq!(d(0, 40, 2), 42); // add
        assert_eq!(d(1, 40, 2), 38); // sub
        assert_eq!(
            run1(&m, "dispatch", &[i32_value(9), i32_value(1), i32_value(1)]),
            Err(Trap::TableOutOfBounds)
        );
    }

    #[test]
    fn ref_null_is_null() {
        // (func (result i32) ref.null func; ref.is_null)  -> 1
        let entry = [0x00, 0xd0, 0x70, 0xd1, 0x0b];
        let m = single_func(&[], &[0x7f], &entry, "n");
        assert_eq!(as_i32(run1(&m, "n", &[]).unwrap()[0]), 1);
    }

    #[test]
    fn table_set_get_ref_func() {
        // funcs: $g (empty) ; test: table.set[1]=ref.func $g ; ref.is_null(table.get[1]) -> 0
        let g = [0x00, 0x0b];
        let test = [
            0x00, 0x41, 0x01, 0xd2, 0x00, 0x26, 0x00, // (slot 1) (ref.func 0) table.set table 0
            0x41, 0x01, 0x25, 0x00, // (slot 1) table.get table 0
            0xd1, 0x0b, // ref.is_null
        ];
        let m = asm(&[
            (1, vec![0x02, 0x60, 0x00, 0x00, 0x60, 0x00, 0x01, 0x7f]), // ()->() ; ()->(i32)
            (3, vec![0x02, 0x00, 0x01]),
            (4, vec![0x01, 0x70, 0x00, 0x03]), // table funcref min 3
            (7, vec![0x01, 0x01, b't', 0x00, 0x01]),
            (10, code_n(&[&g, &test])),
        ]);
        assert_eq!(as_i32(run1(&m, "t", &[]).unwrap()[0]), 0); // slot 1 is non-null after set
    }

    #[test]
    fn memory_store_load_roundtrip() {
        // (memory 1) (func (param i32) (result i32) i32.store[0] (get 0); i32.load[0])
        let entry = [
            0x00, 0x41, 0x00, 0x20, 0x00, 0x36, 0x02, 0x00, // i32.store (addr 0) (local 0)
            0x41, 0x00, 0x28, 0x02, 0x00, // i32.load (addr 0)
            0x0b,
        ];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x01, 0x00, 0x01]), // memory: min 1
            (7, vec![0x01, 0x02, b'r', b't', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "rt", &[i32_value(0x1234_5678)]).unwrap()[0]), 0x1234_5678);
    }

    #[test]
    fn memory_size_and_grow() {
        // (memory 1) (func (result i32) i32.const 2; memory.grow; drop; memory.size)
        let entry = [0x00, 0x41, 0x02, 0x40, 0x00, 0x1a, 0x3f, 0x00, 0x0b];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'g', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "g", &[]).unwrap()[0]), 3); // 1 + 2 pages
    }

    #[test]
    fn active_data_segment_initializes_memory() {
        // (memory 1) (data (i32.const 0) "\2a") (func (result i32) i32.load8_u 0)
        let entry = [0x00, 0x41, 0x00, 0x2d, 0x00, 0x00, 0x0b];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'd', 0x00, 0x00]),
            (10, code1(&entry)),
            (11, vec![0x01, 0x00, 0x41, 0x00, 0x0b, 0x01, 0x2a]), // active seg @0, bytes [0x2a]
        ]);
        assert_eq!(as_i32(run1(&m, "d", &[]).unwrap()[0]), 42);
    }

    #[test]
    fn memory_out_of_bounds_traps() {
        // (memory 1) (func (result i32) i32.load (i32.const 100000))  — past 1 page (65536)
        let entry = [0x00, 0x41, 0xa0, 0x8d, 0x06, 0x28, 0x02, 0x00, 0x0b];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'o', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(run1(&m, "o", &[]), Err(Trap::MemoryOutOfBounds));
    }

    #[test]
    fn gc_struct_new_set_get() {
        // type 0 = ()->i32 ; type 1 = (struct (field (mut i32)))
        // new_default, set field0=42, get field0 -> 42
        let entry = [
            0x01, 0x01, 0x63, 0x01, // 1 local: (ref null 1)
            0xfb, 0x01, 0x01, // struct.new_default 1
            0x21, 0x00, // local.set 0
            0x20, 0x00, 0x41, 0x2a, 0xfb, 0x05, 0x01, 0x00, // struct.set 1 0 = 42
            0x20, 0x00, 0xfb, 0x02, 0x01, 0x00, // struct.get 1 0
            0x0b,
        ];
        let m = asm(&[
            (
                1,
                vec![
                    0x02, // 2 types
                    0x60, 0x00, 0x01, 0x7f, // type 0: ()->i32
                    0x5f, 0x01, 0x7f, 0x01, // type 1: struct { (mut i32) }
                ],
            ),
            (3, vec![0x01, 0x00]),
            (7, vec![0x01, 0x02, b's', b'm', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "sm", &[]).unwrap()[0]), 42);
    }

    #[test]
    fn gc_array_new_set_get_len() {
        // type 0 = ()->i32 ; type 1 = (array (mut i32))
        // new 7×3, a[1]=50, a[1]+len -> 53
        let entry = [
            0x01, 0x01, 0x63, 0x01, // 1 local: (ref null 1)
            0x41, 0x07, 0x41, 0x03, 0xfb, 0x06, 0x01, // array.new 1 (init 7, len 3)
            0x21, 0x00, // local.set 0
            0x20, 0x00, 0x41, 0x01, 0x41, 0x32, 0xfb, 0x0e, 0x01, // array.set 1 (idx 1) = 50
            0x20, 0x00, 0x41, 0x01, 0xfb, 0x0b, 0x01, // array.get 1 (idx 1)
            0x20, 0x00, 0xfb, 0x0f, // array.len
            0x6a, 0x0b, // i32.add
        ];
        let m = asm(&[
            (
                1,
                vec![
                    0x02, // 2 types
                    0x60, 0x00, 0x01, 0x7f, // type 0: ()->i32
                    0x5e, 0x7f, 0x01, // type 1: array (mut i32)
                ],
            ),
            (3, vec![0x01, 0x00]),
            (7, vec![0x01, 0x02, b'a', b'x', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "ax", &[]).unwrap()[0]), 53);
    }

    #[test]
    fn gc_i31() {
        // i31.get_s(ref.i31(-5)) -> -5
        let s = single_func(
            &[],
            &[0x7f],
            &[0x00, 0x41, 0x7b, 0xfb, 0x1c, 0xfb, 0x1d, 0x0b],
            "i",
        );
        assert_eq!(as_i32(run1(&s, "i", &[]).unwrap()[0]), -5);
        // i31.get_u(ref.i31(5)) -> 5
        let u = single_func(
            &[],
            &[0x7f],
            &[0x00, 0x41, 0x05, 0xfb, 0x1c, 0xfb, 0x1e, 0x0b],
            "u",
        );
        assert_eq!(as_i32(run1(&u, "u", &[]).unwrap()[0]), 5);
    }

    #[test]
    fn gc_ref_test_i31() {
        // ref.test (ref i31) (ref.i31 1) -> 1
        let t = single_func(
            &[],
            &[0x7f],
            &[0x00, 0x41, 0x01, 0xfb, 0x1c, 0xfb, 0x14, 0x6c, 0x0b],
            "t",
        );
        assert_eq!(as_i32(run1(&t, "t", &[]).unwrap()[0]), 1);
    }

    // --- SIMD (v128) ---

    #[test]
    fn simd_splat_extract() {
        // i32x4.extract_lane 2 (i32x4.splat 7) -> 7
        let s = single_func(
            &[],
            &[0x7f],
            &[0x00, 0x41, 0x07, 0xfd, 0x11, 0xfd, 0x1b, 0x02, 0x0b],
            "s",
        );
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 7);
    }

    #[test]
    fn simd_const_extract_u() {
        // v128.const [0,1,..,15]; i8x16.extract_lane_u 3 -> 3
        let mut e = vec![0x00, 0xfd, 0x0c];
        e.extend(0u8..16);
        e.extend_from_slice(&[0xfd, 0x16, 0x03, 0x0b]);
        let s = single_func(&[], &[0x7f], &e, "s");
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 3);
    }

    #[test]
    fn simd_i32x4_add() {
        // extract_lane 0 (i32x4.add (splat 3) (splat 4)) -> 7
        let s = single_func(
            &[],
            &[0x7f],
            &[
                0x00, 0x41, 0x03, 0xfd, 0x11, 0x41, 0x04, 0xfd, 0x11, 0xfd, 0xae, 0x01, 0xfd, 0x1b,
                0x00, 0x0b,
            ],
            "s",
        );
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 7);
    }

    #[test]
    fn simd_eq_bitmask() {
        // i32x4.bitmask (i32x4.eq (splat 5) (splat 5)) -> 0b1111 = 15
        let s = single_func(
            &[],
            &[0x7f],
            &[
                0x00, 0x41, 0x05, 0xfd, 0x11, 0x41, 0x05, 0xfd, 0x11, 0xfd, 0x37, 0xfd, 0xa4, 0x01,
                0x0b,
            ],
            "s",
        );
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 15);
    }

    #[test]
    fn simd_shift() {
        // i32x4.extract_lane 0 (i32x4.shl (splat 1) 4) -> 16
        let s = single_func(
            &[],
            &[0x7f],
            &[
                0x00, 0x41, 0x01, 0xfd, 0x11, 0x41, 0x04, 0xfd, 0xab, 0x01, 0xfd, 0x1b, 0x00, 0x0b,
            ],
            "s",
        );
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 16);
    }

    #[test]
    fn simd_f32x4_add() {
        // f32x4.extract_lane 0 (f32x4.add (splat 2.5) (splat 1.5)) -> 4.0
        let s = single_func(
            &[],
            &[0x7d],
            &[
                0x00, //
                0x43, 0x00, 0x00, 0x20, 0x40, 0xfd, 0x13, // f32.const 2.5; f32x4.splat
                0x43, 0x00, 0x00, 0xc0, 0x3f, 0xfd, 0x13, // f32.const 1.5; f32x4.splat
                0xfd, 0xe4, 0x01, // f32x4.add
                0xfd, 0x1f, 0x00, // f32x4.extract_lane 0
                0x0b,
            ],
            "s",
        );
        assert_eq!(as_f32(run1(&s, "s", &[]).unwrap()[0]), 4.0);
    }

    #[test]
    fn simd_add_sat_s() {
        // i8x16.extract_lane_s 0 (i8x16.add_sat_s (splat 100) (splat 100)) -> 127 (saturated)
        let s = single_func(
            &[],
            &[0x7f],
            &[
                0x00, 0x41, 0xe4, 0x00, 0xfd, 0x0f, // i32.const 100; i8x16.splat
                0x41, 0xe4, 0x00, 0xfd, 0x0f, // i32.const 100; i8x16.splat
                0xfd, 0x6f, // i8x16.add_sat_s
                0xfd, 0x15, 0x00, // i8x16.extract_lane_s 0
                0x0b,
            ],
            "s",
        );
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 127);
    }

    #[test]
    fn simd_load_store_roundtrip() {
        // store a v128 [10,20,30,40] (i32 lanes) at addr 0, load it back, extract lane 2 -> 30
        let entry = [
            0x00, 0x41, 0x00, // i32.const 0 (store addr)
            0xfd, 0x0c, 10, 0, 0, 0, 20, 0, 0, 0, 30, 0, 0, 0, 40, 0, 0, 0, // v128.const
            0xfd, 0x0b, 0x00, 0x00, // v128.store align0 off0
            0x41, 0x00, // i32.const 0 (load addr)
            0xfd, 0x00, 0x00, 0x00, // v128.load align0 off0
            0xfd, 0x1b, 0x02, // i32x4.extract_lane 2
            0x0b,
        ];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x01, 0x00, 0x01]), // memory: min 1
            (7, vec![0x01, 0x01, b's', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "s", &[]).unwrap()[0]), 30);
    }

    #[test]
    fn simd_v128_struct_field() {
        // type 1 = (struct (field (mut v128))); store a v128 (i32x4 lane0=7), read it, extract -> 7
        let mut entry = vec![
            0x01, 0x01, 0x63, 0x01, // 1 local (ref null 1)
            0xfb, 0x01, 0x01, // struct.new_default 1
            0x21, 0x00, // local.set 0
            0x20, 0x00, // local.get 0
            0xfd, 0x0c, // v128.const
        ];
        entry.extend_from_slice(&[7, 0, 0, 0]);
        entry.extend_from_slice(&[0; 12]);
        entry.extend_from_slice(&[
            0xfb, 0x05, 0x01, 0x00, // struct.set type1 field0
            0x20, 0x00, // local.get 0
            0xfb, 0x02, 0x01, 0x00, // struct.get type1 field0
            0xfd, 0x1b, 0x00, // i32x4.extract_lane 0
            0x0b,
        ]);
        let m = asm(&[
            (
                1,
                vec![
                    0x02, // 2 types
                    0x60, 0x00, 0x01, 0x7f, // type 0: ()->i32
                    0x5f, 0x01, 0x7b, 0x01, // type 1: struct { (mut v128) }
                ],
            ),
            (3, vec![0x01, 0x00]),
            (7, vec![0x01, 0x01, b'v', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "v", &[]).unwrap()[0]), 7);
    }

    // --- multi-memory ---

    #[test]
    fn multimem_distinct_routing() {
        // 2 memories; store 7->mem0[0], 9->mem1[0]; (load mem0)*10 + (load mem1) = 79
        let entry = [
            0x00, //
            0x41, 0x00, 0x41, 0x07, 0x36, 0x00, 0x00, // i32.store mem0[0] = 7
            0x41, 0x00, 0x41, 0x09, 0x36, 0x40, 0x01, 0x00, // i32.store mem1[0] = 9 (memidx flag)
            0x41, 0x00, 0x28, 0x00, 0x00, // i32.load mem0[0] -> 7
            0x41, 0x0a, 0x6c, // * 10 -> 70
            0x41, 0x00, 0x28, 0x40, 0x01, 0x00, // i32.load mem1[0] -> 9
            0x6a, // + -> 79
            0x0b,
        ];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x02, 0x00, 0x01, 0x00, 0x01]), // 2 memories, each min 1
            (7, vec![0x01, 0x01, b'm', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "m", &[]).unwrap()[0]), 79);
    }

    #[test]
    fn multimem_active_data_to_mem1() {
        // active data segment (flag 2) targets memory 1; load mem1[0] -> 42
        let entry = [0x00, 0x41, 0x00, 0x28, 0x40, 0x01, 0x00, 0x0b]; // i32.load8-less: i32.load mem1[0]
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x02, 0x00, 0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'm', 0x00, 0x00]),
            (10, code1(&entry)),
            // data: 1 segment, flag 2 (active + explicit memidx), memidx 1, (i32.const 0), bytes [42,0,0,0]
            (11, vec![0x01, 0x02, 0x01, 0x41, 0x00, 0x0b, 0x04, 0x2a, 0x00, 0x00, 0x00]),
        ]);
        assert_eq!(as_i32(run1(&m, "m", &[]).unwrap()[0]), 42);
    }

    #[test]
    fn multimem_cross_copy() {
        // store 55 -> mem0[0]; memory.copy dst=mem1 src=mem0 (4 bytes); load mem1[0] -> 55
        let entry = [
            0x00, //
            0x41, 0x00, 0x41, 0x37, 0x36, 0x00, 0x00, // i32.store mem0[0] = 55
            0x41, 0x00, 0x41, 0x00, 0x41, 0x04, 0xfc, 0x0a, 0x01, 0x00, // memory.copy dst=1 src=0
            0x41, 0x00, 0x28, 0x40, 0x01, 0x00, // i32.load mem1[0]
            0x0b,
        ];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x02, 0x00, 0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'm', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "m", &[]).unwrap()[0]), 55);
    }

    // --- threads / atomics (0xFE) ---

    /// (memory 1) with a single-memory helper.
    fn mem_func(entry: &[u8], mem_section: Vec<u8>, results: &[u8]) -> Vec<u8> {
        let mut ty = vec![0x01u8, 0x60, 0x00];
        ty.push(results.len() as u8);
        ty.extend_from_slice(results);
        asm(&[
            (1, ty),
            (3, vec![0x01, 0x00]),
            (5, mem_section),
            (7, vec![0x01, 0x01, b'a', 0x00, 0x00]),
            (10, code1(entry)),
        ])
    }

    #[test]
    fn atomic_rmw_add() {
        // store 10 @0; (rmw.add @0, 5) -> old 10; (atomic.load @0) -> 15; 10+15 = 25
        let entry = [
            0x00, //
            0x41, 0x00, 0x41, 0x0a, 0x36, 0x02, 0x00, // i32.store [0] = 10
            0x41, 0x00, 0x41, 0x05, 0xfe, 0x1e, 0x02, 0x00, // i32.atomic.rmw.add [0] 5 -> 10
            0x41, 0x00, 0xfe, 0x10, 0x02, 0x00, // i32.atomic.load [0] -> 15
            0x6a, // + -> 25
            0x0b,
        ];
        let m = mem_func(&entry, vec![0x01, 0x00, 0x01], &[0x7f]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 25);
    }

    #[test]
    fn atomic_cmpxchg() {
        // store 7 @0; cmpxchg(@0, expect 7, repl 42) -> old 7; load @0 -> 42; 7+42 = 49
        let entry = [
            0x00, //
            0x41, 0x00, 0x41, 0x07, 0x36, 0x02, 0x00, // i32.store [0] = 7
            0x41, 0x00, 0x41, 0x07, 0x41, 0x2a, 0xfe, 0x48, 0x02, 0x00, // cmpxchg(7 -> 42) -> 7
            0x41, 0x00, 0xfe, 0x10, 0x02, 0x00, // i32.atomic.load [0] -> 42
            0x6a, // + -> 49
            0x0b,
        ];
        let m = mem_func(&entry, vec![0x01, 0x00, 0x01], &[0x7f]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 49);
    }

    #[test]
    fn atomic_unaligned_traps() {
        // i32.atomic.load at addr 1 (not 4-aligned) -> UnalignedAtomic
        let entry = [0x00, 0x41, 0x01, 0xfe, 0x10, 0x02, 0x00, 0x0b];
        let m = mem_func(&entry, vec![0x01, 0x00, 0x01], &[0x7f]);
        assert_eq!(run1(&m, "a", &[]), Err(Trap::UnalignedAtomic));
    }

    #[test]
    fn atomic_wait_nonshared_traps() {
        // memory.atomic.wait32 on a non-shared memory -> ExpectedSharedMemory
        let entry = [
            0x00, 0x41, 0x00, 0x41, 0x00, 0x42, 0x00, 0xfe, 0x01, 0x02, 0x00, 0x0b,
        ];
        let m = mem_func(&entry, vec![0x01, 0x00, 0x01], &[0x7f]);
        assert_eq!(run1(&m, "a", &[]), Err(Trap::ExpectedSharedMemory));
    }

    #[test]
    fn atomic_wait_shared_mismatch() {
        // shared memory (flags 0x03: shared+max); wait32(@0, expect 5, timeout 0);
        // mem[0]=0 != 5 -> returns 1 ("not equal")
        let entry = [
            0x00, 0x41, 0x00, 0x41, 0x05, 0x42, 0x00, 0xfe, 0x01, 0x02, 0x00, 0x0b,
        ];
        let m = mem_func(&entry, vec![0x01, 0x03, 0x01, 0x01], &[0x7f]); // shared, min 1, max 1
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 1);
    }

    #[test]
    fn simd_v128_global() {
        // (global v128 (v128.const i32x4 9 0 0 0)); extract_lane 0 (global.get 0) -> 9
        let mut global = vec![0x01, 0x7b, 0x00, 0xfd, 0x0c]; // 1 global: v128, immutable, v128.const
        global.extend_from_slice(&[9, 0, 0, 0]);
        global.extend_from_slice(&[0; 12]);
        global.push(0x0b); // end of const-expr
        let entry = [0x00, 0x23, 0x00, 0xfd, 0x1b, 0x00, 0x0b]; // global.get 0; i32x4.extract_lane 0
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (6, global),
            (7, vec![0x01, 0x01, b'g', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "g", &[]).unwrap()[0]), 9);
    }

    fn write_uleb(out: &mut Vec<u8>, mut v: u32) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                out.push(b | 0x80);
            } else {
                out.push(b);
                break;
            }
        }
    }
}
