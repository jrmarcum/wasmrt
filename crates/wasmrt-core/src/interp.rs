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

use crate::module::{FuncType, Module};
use crate::opcode::{decode_body, BlockType, Imm, Instr, Op};
use crate::reader::Reader;
use crate::types::DecodeError;

/// A runtime value: a raw 64-bit slot reinterpreted per the (validated) type.
pub type Value = u64;

#[must_use]
pub fn i32_value(x: i32) -> Value {
    u64::from(x as u32)
}
#[must_use]
pub fn as_i32(v: Value) -> i32 {
    v as u32 as i32
}
#[must_use]
pub fn i64_value(x: i64) -> Value {
    x as u64
}
#[must_use]
pub fn as_i64(v: Value) -> i64 {
    v as i64
}
#[must_use]
pub fn f32_value(x: f32) -> Value {
    u64::from(x.to_bits())
}
#[must_use]
pub fn as_f32(v: Value) -> f32 {
    f32::from_bits(v as u32)
}
#[must_use]
pub fn f64_value(x: f64) -> Value {
    x.to_bits()
}
#[must_use]
pub fn as_f64(v: Value) -> f64 {
    f64::from_bits(v)
}

/// Cap on guest call depth (a `call` recurses natively, so this bounds host stack use).
const MAX_CALL_DEPTH: usize = 512;

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
    globals: Vec<Value>,
}

/// Immutable execution context (read-only during a call); `globals` is threaded separately as
/// `&mut` so a recursive `call` reborrows it cleanly.
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
            globals,
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
        call_function(&ctx, &mut self.globals, func_index, args, 1)
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
    globals: &mut [Value],
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

    let mut locals = vec![0u64; body.num_locals];
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
    run(&mut frame, ctx, globals, depth)?;

    let n = body.ty.results.len();
    let base = frame.stack_base(n)?;
    Ok(frame.vstack[base..].to_vec())
}

fn run(frame: &mut Frame, ctx: &Ctx, globals: &mut [Value], depth: usize) -> Result<()> {
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
                frame.push(u64::from(bits));
                pc += 1;
            }
            Op::F64Const => {
                let Imm::F64(bits) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                frame.push(bits);
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
                let v = *globals.get(gi as usize).ok_or(Trap::UndefinedGlobal)?;
                frame.push(v);
                pc += 1;
            }
            Op::GlobalSet => {
                let Imm::Global(gi) = instr.imm else {
                    return Err(Trap::UnsupportedInstruction);
                };
                let v = frame.pop();
                *globals.get_mut(gi as usize).ok_or(Trap::UndefinedGlobal)? = v;
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
                let results = call_function(ctx, globals, f, &args, depth + 1)?;
                frame.vstack.truncate(base);
                frame.vstack.extend_from_slice(&results);
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
            0x43 => stack.push(u64::from(r.read_f32_bits()?)),
            0x44 => stack.push(r.read_f64_bits()?),
            0x23 => {
                let gi = r.read_var_u32()? as usize;
                stack.push(*globals.get(gi).ok_or(Trap::UndefinedGlobal)?);
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
