//! The scalar register interpreter (T006).
//!
//! Portable Rust `match` dispatch over verified code (D9): the baseline that
//! threaded or generated dispatch must beat. Slots are untagged 64-bit words
//! on one growable stack; control frames are a separate stack, so script
//! recursion never recurses in Rust and its depth is a checked limit.
//!
//! - A script call copies arguments from the caller's window into the
//!   callee's frame (results, then parameters, then locals) and `RET`
//!   copies the result span back (D8, non-overlapping frames).
//! - A host call gives the host the caller's argument window directly.
//! - Every call and every taken backward branch debits one step of the
//!   budget before it happens; a budget of N permits exactly N debits.
//! - A fault stops the run with its kind and a trace of (function, PC),
//!   innermost first; a fault inside `WIDE` reports the prefix's PC.
//!
//! The loop keeps the current code and frame as local slices, decodes
//! operands inside the handlers that use them, and expands the handlers
//! twice so the common path has no `WIDE` merge: measured in the research
//! ledger (E5/E4 first pass).
//!
//! [`run_threaded`] is the tail-call threaded interpreter with the same
//! contract, and the default since 2026-11-07; this `match` loop remains
//! the portable reference. Both must agree on every test and fixture (E5
//! second pass in the research ledger).
//!
//! The interpreter is safe Rust with bounds-checked slots: even unverified
//! code cannot cause undefined behavior, only a panic. The verifier is what
//! guarantees well-typed code; the interpreter does not re-check types.

#![feature(
    explicit_tail_calls,
    rust_preserve_none_cc,
    core_intrinsics,
    vec_push_within_capacity
)]
#![allow(incomplete_features, internal_features)]

use eitri_base::IndexVec;
use eitri_bytecode::{CallTarget, Function, FunctionId, Op};

mod threaded;

pub use threaded::run_threaded;

/// Why a run stopped early.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaultKind {
    Overflow,
    DivisionByZero,
    Budget,
    Depth,
    /// Raised by a host function, with its message.
    Host(String),
}

impl FaultKind {
    /// The message the first slice specifies.
    pub fn message(&self) -> &str {
        match self {
            FaultKind::Overflow => "integer overflow",
            FaultKind::DivisionByZero => "division by zero",
            FaultKind::Budget => "execution budget exhausted",
            FaultKind::Depth => "call depth exceeded",
            FaultKind::Host(message) => message,
        }
    }
}

/// A position in the code: the instruction that faulted, or a call still
/// active in an outer frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    pub function: FunctionId,
    pub pc: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    pub kind: FaultKind,
    /// The host function that raised it, for `FaultKind::Host`.
    pub host: Option<u32>,
    /// Innermost first.
    pub trace: Vec<Location>,
}

/// The host functions a program calls, by index.
pub trait Host {
    /// Call host function `index` with `args`, writing `results`. An error
    /// is a fault with that message at the call.
    fn call(&mut self, index: u32, args: &[u64], results: &mut [u64]) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Steps: each call and each taken backward branch costs one.
    pub budget: u64,
    /// Active script frames below the entry function.
    pub depth: u32,
}

impl Default for Limits {
    /// The first slice's defaults.
    fn default() -> Self {
        Limits {
            budget: 1_000_000,
            depth: 4096,
        }
    }
}

/// A suspended caller.
#[derive(Clone, Copy)]
struct Frame {
    function: FunctionId,
    base: usize,
    /// The caller's `CALL`, for traces.
    call_pc: u32,
    /// Where the caller resumes.
    resume: usize,
    /// The caller's slot that receives the callee's result span.
    result: u32,
}

/// The interpreter's reusable working memory (D44).
#[derive(Default)]
pub struct VmScratch {
    slots: Vec<u64>,
    frames: Vec<Frame>,
}

impl VmScratch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Heap bytes retained, for scratch high-water accounting.
    pub fn heap_bytes(&self) -> usize {
        self.slots.capacity() * 8 + self.frames.capacity() * size_of::<Frame>()
    }
}

/// Run `entry` with `args` and return its results. `functions` must be
/// verified.
pub fn run(
    functions: &IndexVec<FunctionId, Function>,
    entry: FunctionId,
    args: &[u64],
    host: &mut dyn Host,
    limits: Limits,
    scratch: &mut VmScratch,
) -> Result<Vec<u64>, Fault> {
    scratch.slots.clear();
    scratch.frames.clear();
    execute(
        functions,
        host,
        limits,
        &mut scratch.slots,
        &mut scratch.frames,
        entry,
        args,
    )
}

/// Field extraction for short forms; `hi` is a `WIDE` prefix word or 0.
#[inline(always)]
fn slot_field(word: u32, hi: u32, shift: u32) -> usize {
    (((word >> shift) & 0xff) | ((hi >> shift) & 0xff) << 8) as usize
}

/// Copy `count` slots from `from` to `to`. Calls usually move one slot,
/// which is copied inline instead of through a call to `memmove` (E4, V4).
#[inline(always)]
fn copy_slots(slots: &mut [u64], from: usize, to: usize, count: usize) {
    if count == 1 {
        slots[to] = slots[from];
    } else {
        slots.copy_within(from..from + count, to);
    }
}

/// A fault at `pc` in `function`, with the active calls below it.
#[cold]
fn fault(
    frames: &[Frame],
    kind: FaultKind,
    function: FunctionId,
    pc: usize,
    host: Option<u32>,
) -> Fault {
    let mut trace = vec![Location {
        function,
        pc: pc as u32,
    }];
    trace.extend(frames.iter().rev().map(|frame| Location {
        function: frame.function,
        pc: frame.call_pc,
    }));
    Fault { kind, host, trace }
}

/// The interpreter loop. The current function's code and frame are held
/// as local slices and re-derived only by `CALL` and `RET`, so the
/// instruction head does not reload them (E5, V2).
fn execute(
    functions: &IndexVec<FunctionId, Function>,
    host: &mut dyn Host,
    limits: Limits,
    slots: &mut Vec<u64>,
    frames: &mut Vec<Frame>,
    entry: FunctionId,
    args: &[u64],
) -> Result<Vec<u64>, Fault> {
    let mut budget = limits.budget;
    let mut id = entry;
    let mut function = &functions[id];
    let mut base = 0usize;
    slots.resize(function.frame_size as usize, 0);
    let first = function.signature.param_base() as usize;
    slots[first..first + args.len()].copy_from_slice(args);
    let mut code: &[u32] = &function.code;
    let mut s: &mut [u64] = &mut slots[..function.frame_size as usize];
    let mut pc = 0usize;

    loop {
        let start = pc;
        let word = code[pc];
        pc += 1;

        macro_rules! checked {
            ($slot:expr, $result:expr, $kind:expr) => {
                match $result {
                    Some(value) => s[$slot] = value as u64,
                    None => return Err(fault(frames, $kind, id, start, None)),
                }
            };
        }
        macro_rules! poll {
            () => {
                if budget == 0 {
                    return Err(fault(frames, FaultKind::Budget, id, start, None));
                }
                budget -= 1;
            };
        }
        macro_rules! branch {
            ($taken:expr, $disp:expr) => {
                if $taken {
                    let target = (pc as isize + $disp as isize) as usize;
                    // A taken backward branch polls the budget.
                    if target <= start {
                        poll!();
                    }
                    pc = target;
                }
            };
        }
        let int = |slot: usize, s: &[u64]| s[slot] as i64;

        // The handlers, expanded twice: with `hi` a constant 0 on the
        // common path, so operand decoding drops the `WIDE` merge, and with
        // the prefix word after `WIDE` (E5, V5).
        macro_rules! dispatch {
            ($word:expr, $hi:expr) => {{
                let word: u32 = $word;
                let hi: u32 = $hi;
                let op = Op::from_byte(word as u8).expect("verified code");
                macro_rules! a {
                    () => {
                        slot_field(word, hi, 8)
                    };
                }
                macro_rules! b {
                    () => {
                        slot_field(word, hi, 16)
                    };
                }
                macro_rules! c {
                    () => {
                        slot_field(word, hi, 24)
                    };
                }
                // Immediates are never widened: the `Imm8` of `ALUI` (third
                // field) and of `CBI` (second field).
                macro_rules! imm {
                    () => {
                        (word >> 24) as i8 as i64
                    };
                }
                macro_rules! cbi_imm {
                    () => {
                        (word >> 16) as u8 as i8 as i64
                    };
                }
                match op {
                    Op::Mov => s[a!()] = s[b!()],
                    Op::Ldi => s[a!()] = (word >> 16) as i16 as i64 as u64,
                    Op::Ldk => s[a!()] = function.consts[(word >> 16) as usize],
                    Op::LdkX => {
                        s[a!()] = function.consts[code[pc] as usize];
                        pc += 1;
                    }
                    Op::AddWrap => s[a!()] = int(b!(), s).wrapping_add(int(c!(), s)) as u64,
                    Op::SubWrap => s[a!()] = int(b!(), s).wrapping_sub(int(c!(), s)) as u64,
                    Op::MulWrap => s[a!()] = int(b!(), s).wrapping_mul(int(c!(), s)) as u64,
                    Op::AddChkS => checked!(
                        a!(),
                        int(b!(), s).checked_add(int(c!(), s)),
                        FaultKind::Overflow
                    ),
                    Op::SubChkS => checked!(
                        a!(),
                        int(b!(), s).checked_sub(int(c!(), s)),
                        FaultKind::Overflow
                    ),
                    Op::MulChkS => checked!(
                        a!(),
                        int(b!(), s).checked_mul(int(c!(), s)),
                        FaultKind::Overflow
                    ),
                    Op::DivS | Op::RemS => {
                        let (x, y) = (int(b!(), s), int(c!(), s));
                        if y == 0 {
                            return Err(fault(frames, FaultKind::DivisionByZero, id, start, None));
                        }
                        if op == Op::DivS {
                            checked!(a!(), x.checked_div(y), FaultKind::Overflow);
                        } else {
                            // `MIN % -1` is 0, which fits.
                            s[a!()] = x.wrapping_rem(y) as u64;
                        }
                    }
                    Op::Xor => s[a!()] = s[b!()] ^ s[c!()],
                    Op::AddiWrap => s[a!()] = int(b!(), s).wrapping_add(imm!()) as u64,
                    Op::SubiWrap => s[a!()] = int(b!(), s).wrapping_sub(imm!()) as u64,
                    Op::AddiChkS => {
                        checked!(a!(), int(b!(), s).checked_add(imm!()), FaultKind::Overflow)
                    }
                    Op::SubiChkS => {
                        checked!(a!(), int(b!(), s).checked_sub(imm!()), FaultKind::Overflow)
                    }
                    Op::MuliChkS => {
                        checked!(a!(), int(b!(), s).checked_mul(imm!()), FaultKind::Overflow)
                    }
                    Op::CbiEq => branch!(int(a!(), s) == cbi_imm!(), (word >> 24) as i8),
                    Op::CbiNe => branch!(int(a!(), s) != cbi_imm!(), (word >> 24) as i8),
                    Op::CbiLt => branch!(int(a!(), s) < cbi_imm!(), (word >> 24) as i8),
                    Op::CbiGt => branch!(int(a!(), s) > cbi_imm!(), (word >> 24) as i8),
                    Op::NegChkS => checked!(a!(), int(b!(), s).checked_neg(), FaultKind::Overflow),
                    Op::CbEq => branch!(s[a!()] == s[b!()], (word >> 24) as i8),
                    Op::CbNe => branch!(s[a!()] != s[b!()], (word >> 24) as i8),
                    Op::CbLt => branch!(int(a!(), s) < int(b!(), s), (word >> 24) as i8),
                    Op::CbLe => branch!(int(a!(), s) <= int(b!(), s), (word >> 24) as i8),
                    Op::TbTrue => branch!(s[a!()] != 0, (word >> 16) as i16),
                    Op::TbFalse => branch!(s[a!()] == 0, (word >> 16) as i16),
                    Op::CsetEq => s[a!()] = u64::from(s[b!()] == s[c!()]),
                    Op::CsetNe => s[a!()] = u64::from(s[b!()] != s[c!()]),
                    Op::CsetLt => s[a!()] = u64::from(int(b!(), s) < int(c!(), s)),
                    Op::CsetLe => s[a!()] = u64::from(int(b!(), s) <= int(c!(), s)),
                    Op::J => branch!(true, (word as i32) >> 8),
                    Op::JX => {
                        let disp = code[pc] as i32;
                        pc += 1;
                        branch!(true, disp);
                    }
                    Op::CallScript | Op::CallHost => {
                        let site = function.sites[(word >> 16) as usize];
                        poll!();
                        match site.target {
                            CallTarget::Host(index) => {
                                let (args, results) = (site.args, site.result);
                                // The verifier guarantees the windows are
                                // disjoint, so the host borrows both in place.
                                let (inputs, outputs) = if results.count == 0 {
                                    (&s[args.slots_usize()], &mut [][..])
                                } else if args.end() <= results.first {
                                    let (low, high) = s.split_at_mut(results.first as usize);
                                    (
                                        &low[args.slots_usize()],
                                        &mut high[..results.count as usize],
                                    )
                                } else {
                                    let (low, high) = s.split_at_mut(args.first as usize);
                                    (
                                        &high[..args.count as usize],
                                        &mut low[results.slots_usize()],
                                    )
                                };
                                if let Err(message) = host.call(index, inputs, outputs) {
                                    return Err(fault(
                                        frames,
                                        FaultKind::Host(message),
                                        id,
                                        start,
                                        Some(index),
                                    ));
                                }
                            }
                            CallTarget::Script(callee) => {
                                if frames.len() as u32 >= limits.depth {
                                    return Err(fault(frames, FaultKind::Depth, id, start, None));
                                }
                                let callee_fn = &functions[callee];
                                let callee_base = base + function.frame_size as usize;
                                let end = callee_base + callee_fn.frame_size as usize;
                                if slots.len() < end {
                                    slots.resize(end, 0);
                                }
                                let params =
                                    callee_base + callee_fn.signature.param_base() as usize;
                                let from = base + site.args.first as usize;
                                copy_slots(slots, from, params, site.args.count as usize);
                                frames.push(Frame {
                                    function: id,
                                    base,
                                    call_pc: start as u32,
                                    resume: pc,
                                    result: site.result.first,
                                });
                                id = callee;
                                function = callee_fn;
                                base = callee_base;
                                code = &function.code;
                                s = &mut slots[base..end];
                                pc = 0;
                            }
                        }
                    }
                    Op::Ret => {
                        let count = function.signature.results.len();
                        let Some(caller) = frames.pop() else {
                            return Ok(s[..count].to_vec());
                        };
                        let caller_fn = &functions[caller.function];
                        let to = caller.base + caller.result as usize;
                        copy_slots(slots, base, to, count);
                        id = caller.function;
                        function = caller_fn;
                        base = caller.base;
                        code = &function.code;
                        s = &mut slots[base..base + function.frame_size as usize];
                        pc = caller.resume;
                    }
                    other => unreachable!("`{}` is rejected by the verifier", other.name()),
                }
            }};
        }
        if word as u8 == Op::Wide as u8 {
            let next = code[pc];
            pc += 1;
            dispatch!(next, word);
        } else {
            dispatch!(word, 0);
        }
    }
}

#[cfg(test)]
mod tests;
