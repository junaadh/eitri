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
//! - A host call gives the host a [`Call`] over the caller's argument and
//!   result windows, in place. Through it the host may reenter: run a
//!   script function of the same program on top of the current frames,
//!   sharing the slot and frame stacks, the budget, and the depth limit.
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
use eitri_bytecode::{CallTarget, Function, FunctionId, Op, SlotRange, ValueType};

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

/// One trace entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// A script function at the instruction that faulted, or at a call
    /// still active in an outer frame.
    Script { function: FunctionId, pc: u32 },
    /// A host function: one that raised the fault, or that reentered and
    /// propagated a fault from the nested run.
    Host(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    pub kind: FaultKind,
    /// Innermost first.
    pub trace: Vec<Location>,
}

/// The host functions a program calls, by index.
pub trait Host {
    /// Call host function `index`. Arguments and results are reached
    /// through `cx`, which also reenters the runtime. An error is a fault
    /// at the call.
    fn call(&mut self, cx: &mut Call<'_, '_>, index: u32) -> Result<(), HostError>;
}

/// Why a host function failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostError {
    /// A fault with this message, raised by the host function.
    Message(String),
    /// A fault from a nested run, propagated: the run stops with it, its
    /// trace continued through the host function.
    Fault(Fault),
}

impl From<String> for HostError {
    fn from(message: String) -> Self {
        HostError::Message(message)
    }
}

impl From<&str> for HostError {
    fn from(message: &str) -> Self {
        HostError::Message(message.to_owned())
    }
}

impl From<Fault> for HostError {
    fn from(fault: Fault) -> Self {
        HostError::Fault(fault)
    }
}

/// A script function a host may reenter, from [`Call::function`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptFn(FunctionId);

/// Nested runs active at once. Each costs Rust stack (a host frame and an
/// interpreter entry), so their nesting is limited separately from script
/// frames; exceeding it is `call depth exceeded`, like the frame limit.
/// Measured per nested run, excluding the host's own frame: 688 B
/// (`match` loop) and 896 B (threaded) in release, 14.9 KB and 3.5 KB in
/// debug builds, so 64 fit a default 2 MiB thread in every build.
pub const MAX_NESTING: u32 = 64;

/// The state a run shares with the runs its host functions start.
struct Machine<'a> {
    functions: &'a IndexVec<FunctionId, Function>,
    slots: &'a mut Vec<u64>,
    frames: &'a mut Vec<Frame>,
    budget: u64,
    depth: u32,
    /// The run's entry function, which a host may not reenter.
    entry: FunctionId,
    /// Nested runs active.
    nesting: u32,
    threaded: bool,
}

/// A host function's view of its call: the argument and result windows in
/// the caller's frame, and reentry. Reentering takes `&mut self`, so no
/// argument or result borrow can be held across it: a nested run may grow
/// and move the slot stack.
///
/// Reading arguments before reentering, and results after, compiles:
///
/// ```
/// use eitri_vm::{Call, Host, HostError};
/// struct Twice;
/// impl Host for Twice {
///     fn call(&mut self, cx: &mut Call<'_, '_>, _: u32) -> Result<(), HostError> {
///         let arg = cx.arg(0);
///         let f = cx.function("callback").ok_or("no callback")?;
///         let mut result = [0];
///         cx.call(self, f, &[arg], &mut result)?;
///         cx.set_result(0, result[0] * 2);
///         Ok(())
///     }
/// }
/// ```
///
/// Holding the argument view across reentry does not:
///
/// ```compile_fail,E0502
/// use eitri_vm::{Call, Host, HostError};
/// struct Stale;
/// impl Host for Stale {
///     fn call(&mut self, cx: &mut Call<'_, '_>, _: u32) -> Result<(), HostError> {
///         let args = cx.args();
///         let f = cx.function("callback").ok_or("no callback")?;
///         cx.call(self, f, &[], &mut [])?;
///         let _stale = args[0];
///         Ok(())
///     }
/// }
/// ```
pub struct Call<'c, 'a> {
    machine: &'c mut Machine<'a>,
    /// The calling script frame, pushed below a nested run for its return
    /// and for traces.
    caller: Frame,
    /// The first slot after the caller's frame: where a nested run starts.
    top: usize,
    /// Absolute slot ranges.
    args: SlotRange,
    results: SlotRange,
}

impl Call<'_, '_> {
    #[inline]
    pub fn args(&self) -> &[u64] {
        &self.machine.slots[self.args.slots_usize()]
    }

    #[inline]
    pub fn arg(&self, index: usize) -> u64 {
        self.args()[index]
    }

    #[inline]
    pub fn results(&mut self) -> &mut [u64] {
        &mut self.machine.slots[self.results.slots_usize()]
    }

    #[inline]
    pub fn set_result(&mut self, index: usize, value: u64) {
        self.results()[index] = value;
    }

    /// The script function named `name`, if the program has one. A linear
    /// search: look a function up once and keep it.
    pub fn function(&self, name: &str) -> Option<ScriptFn> {
        let m = &*self.machine;
        m.functions
            .iter_enumerated()
            .find(|&(id, f)| id != m.entry && &*f.name == name)
            .map(|(id, _)| ScriptFn(id))
    }

    /// Run script function `function` with `args`, writing `results`. The
    /// call debits one step and counts as a frame, as a script call does;
    /// host calls inside it go to `host`, normally the caller's `self`. A
    /// fault stops only the nested run: the host may handle it or return it
    /// (`?`) to stop the outer run too. Its trace ends at the nested entry.
    pub fn call(
        &mut self,
        host: &mut dyn Host,
        function: ScriptFn,
        args: &[u64],
        results: &mut [u64],
    ) -> Result<(), Fault> {
        let m = &mut *self.machine;
        let callee = &m.functions[function.0];
        let signature = &callee.signature;
        let mismatch = args.len() != signature.params.len()
            || results.len() != signature.results.len()
            || args
                .iter()
                .zip(&signature.params)
                .any(|(&arg, &ty)| ty == ValueType::Bool && arg > 1);
        if mismatch {
            let message = format!(
                "host error: reentrant call to `{}` does not match its signature",
                callee.name
            );
            return Err(Fault {
                kind: FaultKind::Host(message),
                trace: Vec::new(),
            });
        }
        let stop = |kind| {
            Err(Fault {
                kind,
                trace: Vec::new(),
            })
        };
        if m.budget == 0 {
            return stop(FaultKind::Budget);
        }
        m.budget -= 1;
        if m.frames.len() as u32 >= m.depth || m.nesting >= MAX_NESTING {
            return stop(FaultKind::Depth);
        }
        let base = self.top;
        let end = base + callee.frame_size as usize;
        if m.slots.len() < end {
            m.slots.resize(end, 0);
        }
        let first = base + signature.param_base() as usize;
        m.slots[first..first + args.len()].copy_from_slice(args);
        m.frames.push(self.caller);
        let floor = m.frames.len();
        m.nesting += 1;
        let outcome = if m.threaded {
            threaded::nested(m, host, function.0, base, floor)
        } else {
            execute(m, host, function.0, base, floor)
        };
        m.nesting -= 1;
        m.frames.truncate(floor - 1);
        outcome?;
        results.copy_from_slice(&m.slots[base..base + results.len()]);
        Ok(())
    }
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
    let mut m = machine(functions, entry, args, limits, scratch, false);
    execute(&mut m, host, entry, 0, 0)?;
    Ok(m.slots[..functions[entry].signature.results.len()].to_vec())
}

/// The shared state of a fresh run, with `entry`'s frame at slot 0.
fn machine<'a>(
    functions: &'a IndexVec<FunctionId, Function>,
    entry: FunctionId,
    args: &[u64],
    limits: Limits,
    scratch: &'a mut VmScratch,
    threaded: bool,
) -> Machine<'a> {
    scratch.slots.clear();
    scratch.frames.clear();
    let function = &functions[entry];
    scratch.slots.resize(function.frame_size as usize, 0);
    let first = function.signature.param_base() as usize;
    scratch.slots[first..first + args.len()].copy_from_slice(args);
    Machine {
        functions,
        slots: &mut scratch.slots,
        frames: &mut scratch.frames,
        budget: limits.budget,
        depth: limits.depth,
        entry,
        nesting: 0,
        threaded,
    }
}

/// `range`, a window of the frame at `base`, as absolute slots.
#[inline(always)]
fn absolute(range: SlotRange, base: usize) -> SlotRange {
    SlotRange {
        first: base as u32 + range.first,
        count: range.count,
    }
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

/// A fault at `pc` in `function`, with the active calls of this run
/// (`frames`, above its floor) below it.
#[cold]
fn fault(frames: &[Frame], kind: FaultKind, function: FunctionId, pc: usize) -> Fault {
    raised(Vec::new(), frames, kind, function, pc)
}

/// A fault whose innermost entries are `inner` (a host function, or a
/// nested run and the host function that propagated it), raised at `pc`.
#[cold]
fn raised(
    mut trace: Vec<Location>,
    frames: &[Frame],
    kind: FaultKind,
    function: FunctionId,
    pc: usize,
) -> Fault {
    trace.push(Location::Script {
        function,
        pc: pc as u32,
    });
    trace.extend(frames.iter().rev().map(|frame| Location::Script {
        function: frame.function,
        pc: frame.call_pc,
    }));
    Fault { kind, trace }
}

/// The fault a failed host function `index` raises at its call.
#[cold]
fn host_fault(
    error: HostError,
    index: u32,
    frames: &[Frame],
    function: FunctionId,
    pc: usize,
) -> Fault {
    let (kind, mut inner) = match error {
        HostError::Message(message) => (FaultKind::Host(message), Vec::new()),
        HostError::Fault(fault) => (fault.kind, fault.trace),
    };
    inner.push(Location::Host(index));
    raised(inner, frames, kind, function, pc)
}

/// The interpreter loop. The current function's code and frame are held
/// as local slices and re-derived only by `CALL` and `RET`, so the
/// instruction head does not reload them (E5, V2).
///
/// Runs `entry`, whose frame is in place at `base`, until it returns (its
/// results are then at `base`) or faults. Frames below `floor` belong to
/// outer runs: a nested run returns to its host when its entry returns,
/// and its faults trace only its own frames.
fn execute(
    m: &mut Machine<'_>,
    host: &mut dyn Host,
    entry: FunctionId,
    base: usize,
    floor: usize,
) -> Result<(), Fault> {
    let functions = m.functions;
    let depth = m.depth;
    let mut budget = m.budget;
    let mut id = entry;
    let mut function = &functions[id];
    let mut base = base;
    let mut code: &[u32] = &function.code;
    let mut s: &mut [u64] = &mut m.slots[base..base + function.frame_size as usize];
    let mut pc = 0usize;

    loop {
        let start = pc;
        let word = code[pc];
        pc += 1;

        macro_rules! checked {
            ($slot:expr, $result:expr, $kind:expr) => {
                match $result {
                    Some(value) => s[$slot] = value as u64,
                    None => return Err(fault(&m.frames[floor..], $kind, id, start)),
                }
            };
        }
        macro_rules! poll {
            () => {
                if budget == 0 {
                    return Err(fault(&m.frames[floor..], FaultKind::Budget, id, start));
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
                            return Err(fault(
                                &m.frames[floor..],
                                FaultKind::DivisionByZero,
                                id,
                                start,
                            ));
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
                                // Publish the budget: the host may reenter
                                // and spend from it.
                                m.budget = budget;
                                let frame_size = function.frame_size as usize;
                                let mut cx = Call {
                                    machine: &mut *m,
                                    caller: Frame {
                                        function: id,
                                        base,
                                        call_pc: start as u32,
                                        resume: pc,
                                        result: 0,
                                    },
                                    top: base + frame_size,
                                    args: absolute(site.args, base),
                                    results: absolute(site.result, base),
                                };
                                let outcome = host.call(&mut cx, index);
                                budget = m.budget;
                                // A nested run may have moved the slot stack.
                                s = &mut m.slots[base..base + frame_size];
                                if let Err(error) = outcome {
                                    return Err(host_fault(
                                        error,
                                        index,
                                        &m.frames[floor..],
                                        id,
                                        start,
                                    ));
                                }
                            }
                            CallTarget::Script(callee) => {
                                if m.frames.len() as u32 >= depth {
                                    return Err(fault(
                                        &m.frames[floor..],
                                        FaultKind::Depth,
                                        id,
                                        start,
                                    ));
                                }
                                let callee_fn = &functions[callee];
                                let callee_base = base + function.frame_size as usize;
                                let end = callee_base + callee_fn.frame_size as usize;
                                if m.slots.len() < end {
                                    m.slots.resize(end, 0);
                                }
                                let params =
                                    callee_base + callee_fn.signature.param_base() as usize;
                                let from = base + site.args.first as usize;
                                copy_slots(m.slots, from, params, site.args.count as usize);
                                m.frames.push(Frame {
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
                                s = &mut m.slots[base..end];
                                pc = 0;
                            }
                        }
                    }
                    Op::Ret => {
                        let count = function.signature.results.len();
                        if m.frames.len() == floor {
                            m.budget = budget;
                            return Ok(());
                        }
                        let caller = m.frames.pop().expect("above the floor");
                        let caller_fn = &functions[caller.function];
                        let to = caller.base + caller.result as usize;
                        copy_slots(m.slots, base, to, count);
                        id = caller.function;
                        function = caller_fn;
                        base = caller.base;
                        code = &function.code;
                        s = &mut m.slots[base..base + function.frame_size as usize];
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
