//! The tail-call threaded interpreter (E5, D9), the default since
//! 2026-11-07. [`run_threaded`] must match [`crate::run`] on every test
//! and fixture; the portable `match` loop stays as the reference.
//!
//! Each opcode has its own handler, and every handler ends by tail-calling
//! (`become`) the next instruction's handler through a 256-entry table, so
//! there is no shared dispatch loop and each handler has its own indirect
//! branch (R15, R19). Handlers use the `rust-preserve-none` convention,
//! which keeps the interpreter state (code, PC, the instruction word, and
//! the frame) in argument registers across the jumps (R5, R18, R19).
//!
//! The frame is passed as a raw pointer and length: a `&mut [u64]` could
//! not coexist with the `&mut` state that `CALL` and `RET` use to grow and
//! re-slice the slot stack. Every slot access is still bounds-checked
//! against the frame length, so unverified code can only panic, as with
//! [`crate::run`]; the pointer is re-derived after anything that may move
//! or reborrow the slot stack.

use eitri_base::IndexVec;
use eitri_bytecode::{CallTarget, Function, FunctionId, Op};

use crate::{
    Call, Fault, FaultKind, Frame, Host, Limits, Location, Machine, VmScratch, absolute,
    host_fault, machine, raised, slot_field,
};

/// Everything a handler needs that is not hot enough for a register.
struct State<'h> {
    /// Shared with the runs host functions start: embedded, so a host
    /// call lends it to its [`Call`] without copying fields.
    m: Machine<'h>,
    host: &'h mut dyn Host,
    id: FunctionId,
    function: &'h Function,
    base: usize,
    /// Frames below it belong to outer runs (see [`crate::Call::call`]).
    floor: usize,
    outcome: Option<Result<(), Fault>>,
    /// A fault on its way to `raise`, with its innermost trace entries
    /// when a host function raised it.
    pending: Option<(FaultKind, Vec<Location>)>,
}

/// A handler: the code, the PC of this instruction (or its `WIDE` prefix),
/// its first word, the frame, and the state.
type Handler = for<'c, 'h, 's> extern "rust-preserve-none" fn(
    &'c [u32],
    usize,
    u32,
    *mut u64,
    usize,
    &'s mut State<'h>,
);

/// Run `entry` with `args`, as [`crate::run`] does.
pub fn run_threaded(
    functions: &IndexVec<FunctionId, Function>,
    entry: FunctionId,
    args: &[u64],
    host: &mut dyn Host,
    limits: Limits,
    scratch: &mut VmScratch,
) -> Result<Vec<u64>, Fault> {
    let mut m = machine(functions, entry, args, limits, scratch, true);
    nested(&mut m, host, entry, 0, 0)?;
    Ok(m.slots[..functions[entry].signature.results.len()].to_vec())
}

/// Run `entry`, whose frame is in place at `base`, as [`crate::execute`]
/// does: the outermost run, or one a host started.
pub(crate) fn nested(
    m: &mut Machine<'_>,
    host: &mut dyn Host,
    entry: FunctionId,
    base: usize,
    floor: usize,
) -> Result<(), Fault> {
    let function = &m.functions[entry];
    let size = function.frame_size as usize;
    // SAFETY: the caller put `entry`'s frame at `base..base + size`, inside
    // the slot stack.
    let s = unsafe { m.slots.as_mut_ptr().add(base) };
    let mut state = State {
        m: Machine {
            functions: m.functions,
            slots: &mut *m.slots,
            frames: &mut *m.frames,
            budget: m.budget,
            depth: m.depth,
            entry: m.entry,
            nesting: m.nesting,
            threaded: true,
        },
        host,
        id: entry,
        function,
        base,
        floor,
        outcome: None,
        pending: None,
    };
    let code: &[u32] = &function.code;
    TABLE[(code[0] & 0xff) as usize](code, 0, code[0], s, size, &mut state);
    let outcome = state.outcome;
    m.budget = state.m.budget;
    outcome.expect("execution ends by returning or faulting")
}

/// A slot outside the frame: reachable only from unverified code. Traps
/// (one instruction) rather than panicking, so handlers make no call on
/// their fast path and need no stack frame (E5, T2).
#[inline(always)]
fn out_of_frame() -> ! {
    core::intrinsics::abort_immediate()
}

#[inline(always)]
fn get(s: *mut u64, len: usize, slot: usize) -> u64 {
    if slot >= len {
        out_of_frame();
    }
    // SAFETY: `s` points at a live frame of `len` slots inside the slot
    // stack (re-derived after every growth or reborrow), and `slot < len`.
    unsafe { *s.add(slot) }
}

#[inline(always)]
fn set(s: *mut u64, len: usize, slot: usize, value: u64) {
    if slot >= len {
        out_of_frame();
    }
    // SAFETY: as in `get`.
    unsafe { *s.add(slot) = value }
}

/// Continue at `pc`.
macro_rules! next {
    ($code:expr, $pc:expr, $s:expr, $len:expr, $st:expr) => {{
        let pc: usize = $pc;
        let word = $code[pc];
        become TABLE[(word & 0xff) as usize]($code, pc, word, $s, $len, $st)
    }};
}

/// Stop with a fault at `pc`: record it and tail-call the cold `raise`
/// handler, so the fault path makes no call either.
macro_rules! fail {
    ($code:expr, $pc:expr, $word:expr, $s:expr, $len:expr, $st:expr, $kind:expr) => {{
        $st.pending = Some(($kind, Vec::new()));
        become raise($code, $pc, $word, $s, $len, $st)
    }};
}

#[cold]
extern "rust-preserve-none" fn raise(
    _: &[u32],
    pc: usize,
    _: u32,
    _: *mut u64,
    _: usize,
    st: &mut State<'_>,
) {
    let (kind, inner) = st.pending.take().expect("set before `raise`");
    let frames = &st.m.frames[st.floor..];
    st.outcome = Some(Err(raised(inner, frames, kind, st.id, pc)));
}

/// The effect of a non-control instruction with decoded operands; shared
/// by the narrow handlers (with a constant `op`) and `WIDE`.
#[inline(always)]
fn slot_op(
    op: u8,
    (a, b, c): (usize, usize, usize),
    word: u32,
    s: *mut u64,
    len: usize,
    consts: &[u64],
) -> Result<(), FaultKind> {
    let int = |slot| get(s, len, slot) as i64;
    let value = match op {
        x if x == Op::Mov as u8 => get(s, len, b),
        x if x == Op::Ldi as u8 => (word >> 16) as i16 as i64 as u64,
        x if x == Op::Ldk as u8 => consts[(word >> 16) as usize],
        x if x == Op::AddWrap as u8 => int(b).wrapping_add(int(c)) as u64,
        x if x == Op::SubWrap as u8 => int(b).wrapping_sub(int(c)) as u64,
        x if x == Op::MulWrap as u8 => int(b).wrapping_mul(int(c)) as u64,
        x if x == Op::AddChkS as u8 => {
            int(b).checked_add(int(c)).ok_or(FaultKind::Overflow)? as u64
        }
        x if x == Op::SubChkS as u8 => {
            int(b).checked_sub(int(c)).ok_or(FaultKind::Overflow)? as u64
        }
        x if x == Op::MulChkS as u8 => {
            int(b).checked_mul(int(c)).ok_or(FaultKind::Overflow)? as u64
        }
        x if x == Op::DivS as u8 || x == Op::RemS as u8 => {
            let (x_, y) = (int(b), int(c));
            if y == 0 {
                return Err(FaultKind::DivisionByZero);
            }
            if op == Op::DivS as u8 {
                x_.checked_div(y).ok_or(FaultKind::Overflow)? as u64
            } else {
                // `MIN % -1` is 0, which fits.
                x_.wrapping_rem(y) as u64
            }
        }
        x if x == Op::Xor as u8 => get(s, len, b) ^ get(s, len, c),
        // `ALUI`: the immediate is the third field, never widened.
        x if x == Op::AddiWrap as u8 => int(b).wrapping_add(imm8(word)) as u64,
        x if x == Op::SubiWrap as u8 => int(b).wrapping_sub(imm8(word)) as u64,
        x if x == Op::AddiChkS as u8 => {
            int(b).checked_add(imm8(word)).ok_or(FaultKind::Overflow)? as u64
        }
        x if x == Op::SubiChkS as u8 => {
            int(b).checked_sub(imm8(word)).ok_or(FaultKind::Overflow)? as u64
        }
        x if x == Op::MuliChkS as u8 => {
            int(b).checked_mul(imm8(word)).ok_or(FaultKind::Overflow)? as u64
        }
        x if x == Op::NegChkS as u8 => int(b).checked_neg().ok_or(FaultKind::Overflow)? as u64,
        x if x == Op::CsetEq as u8 => u64::from(get(s, len, b) == get(s, len, c)),
        x if x == Op::CsetNe as u8 => u64::from(get(s, len, b) != get(s, len, c)),
        x if x == Op::CsetLt as u8 => u64::from(int(b) < int(c)),
        x if x == Op::CsetLe as u8 => u64::from(int(b) <= int(c)),
        other => {
            let info = Op::from_byte(other).expect("verified opcode").info();
            let x = if info.roles[1].is_slot() {
                get(s, len, b)
            } else {
                0
            };
            let y = if info.roles[2].is_slot() {
                get(s, len, c)
            } else {
                0
            };
            eitri_bytecode::scalar::eval(info.sem, x, y, (word >> 24) as u8)
                .map_err(FaultKind::from)?
        }
    };
    set(s, len, a, value);
    Ok(())
}

#[inline(always)]
fn imm8(word: u32) -> i64 {
    (word >> 24) as i8 as i64
}

#[inline(always)]
fn cbi_imm(word: u32) -> i64 {
    (word >> 16) as u8 as i8 as i64
}

/// Whether a conditional branch is taken, and its displacement.
#[inline(always)]
fn branch_op(op: u8, a: usize, b: usize, word: u32, s: *mut u64, len: usize) -> (bool, isize) {
    let int = |slot| get(s, len, slot) as i64;
    let d8 = (word >> 24) as i8 as isize;
    let d16 = (word >> 16) as i16 as isize;
    match op {
        x if x == Op::CbEq as u8 => (get(s, len, a) == get(s, len, b), d8),
        x if x == Op::CbNe as u8 => (get(s, len, a) != get(s, len, b), d8),
        x if x == Op::CbLt as u8 => (int(a) < int(b), d8),
        x if x == Op::CbLe as u8 => (int(a) <= int(b), d8),
        // `CBI`: the immediate is the second field, never widened.
        x if x == Op::CbiEq as u8 => (int(a) == cbi_imm(word), d8),
        x if x == Op::CbiNe as u8 => (int(a) != cbi_imm(word), d8),
        x if x == Op::CbiLt as u8 => (int(a) < cbi_imm(word), d8),
        x if x == Op::CbiGt as u8 => (int(a) > cbi_imm(word), d8),
        x if x == Op::TbTrue as u8 => (get(s, len, a) != 0, d16),
        x if x == Op::TbFalse as u8 => (get(s, len, a) == 0, d16),
        other => {
            let eitri_bytecode::spec::Sem::Branch { cond, imm } =
                Op::from_byte(other).expect("verified opcode").info().sem
            else {
                unreachable!("verified branch")
            };
            (
                cond.eval(
                    get(s, len, a) as i64,
                    if imm {
                        cbi_imm(word)
                    } else {
                        get(s, len, b) as i64
                    },
                ),
                d8,
            )
        }
    }
}

/// Debit the budget for a branch from `start` to `target` if it goes
/// backward. False when the budget is exhausted.
#[inline(always)]
fn take(st: &mut State<'_>, start: usize, target: usize) -> bool {
    if target <= start {
        if st.m.budget == 0 {
            return false;
        }
        st.m.budget -= 1;
    }
    true
}

macro_rules! slot_handler {
    ($name:ident, $op:expr) => {
        extern "rust-preserve-none" fn $name(
            code: &[u32],
            pc: usize,
            word: u32,
            s: *mut u64,
            len: usize,
            st: &mut State<'_>,
        ) {
            let (a, b, c) = (
                slot_field(word, 0, 8),
                slot_field(word, 0, 16),
                slot_field(word, 0, 24),
            );
            if let Err(kind) = slot_op($op as u8, (a, b, c), word, s, len, &st.function.consts) {
                fail!(code, pc, word, s, len, st, kind)
            }
            next!(code, pc + 1, s, len, st)
        }
    };
}

macro_rules! branch_handler {
    ($name:ident, $op:expr) => {
        extern "rust-preserve-none" fn $name(
            code: &[u32],
            pc: usize,
            word: u32,
            s: *mut u64,
            len: usize,
            st: &mut State<'_>,
        ) {
            let (a, b) = (slot_field(word, 0, 8), slot_field(word, 0, 16));
            let (taken, disp) = branch_op($op as u8, a, b, word, s, len);
            let mut to = pc + 1;
            if taken {
                to = (to as isize + disp) as usize;
                if !take(st, pc, to) {
                    fail!(code, pc, word, s, len, st, FaultKind::Budget)
                }
            }
            next!(code, to, s, len, st)
        }
    };
}

slot_handler!(mov, Op::Mov);
slot_handler!(ldi, Op::Ldi);
slot_handler!(ldk, Op::Ldk);
slot_handler!(add_wrap, Op::AddWrap);
slot_handler!(sub_wrap, Op::SubWrap);
slot_handler!(mul_wrap, Op::MulWrap);
slot_handler!(add_chk, Op::AddChkS);
slot_handler!(sub_chk, Op::SubChkS);
slot_handler!(mul_chk, Op::MulChkS);
slot_handler!(div, Op::DivS);
slot_handler!(rem, Op::RemS);
slot_handler!(xor, Op::Xor);
slot_handler!(neg_chk, Op::NegChkS);
slot_handler!(addi_wrap, Op::AddiWrap);
slot_handler!(subi_wrap, Op::SubiWrap);
slot_handler!(addi_chk, Op::AddiChkS);
slot_handler!(subi_chk, Op::SubiChkS);
slot_handler!(muli_chk, Op::MuliChkS);
slot_handler!(cset_eq, Op::CsetEq);
slot_handler!(cset_ne, Op::CsetNe);
slot_handler!(cset_lt, Op::CsetLt);
slot_handler!(cset_le, Op::CsetLe);
branch_handler!(cb_eq, Op::CbEq);
branch_handler!(cb_ne, Op::CbNe);
branch_handler!(cb_lt, Op::CbLt);
branch_handler!(cb_le, Op::CbLe);
branch_handler!(cbi_eq, Op::CbiEq);
branch_handler!(cbi_ne, Op::CbiNe);
branch_handler!(cbi_lt, Op::CbiLt);
branch_handler!(cbi_gt, Op::CbiGt);
branch_handler!(tb_true, Op::TbTrue);
branch_handler!(tb_false, Op::TbFalse);

extern "rust-preserve-none" fn ldk_x(
    code: &[u32],
    pc: usize,
    word: u32,
    s: *mut u64,
    len: usize,
    st: &mut State<'_>,
) {
    set(
        s,
        len,
        slot_field(word, 0, 8),
        st.function.consts[code[pc + 1] as usize],
    );
    next!(code, pc + 2, s, len, st)
}

extern "rust-preserve-none" fn j(
    code: &[u32],
    pc: usize,
    word: u32,
    s: *mut u64,
    len: usize,
    st: &mut State<'_>,
) {
    let to = (pc as isize + 1 + ((word as i32) >> 8) as isize) as usize;
    if !take(st, pc, to) {
        fail!(code, pc, word, s, len, st, FaultKind::Budget)
    }
    next!(code, to, s, len, st)
}

extern "rust-preserve-none" fn j_x(
    code: &[u32],
    pc: usize,
    word: u32,
    s: *mut u64,
    len: usize,
    st: &mut State<'_>,
) {
    let to = (pc as isize + 2 + code[pc + 1] as i32 as isize) as usize;
    if !take(st, pc, to) {
        fail!(code, pc, word, s, len, st, FaultKind::Budget)
    }
    next!(code, to, s, len, st)
}

/// A `WIDE`-prefixed instruction: operands get the prefix's high bytes.
/// Rare, so one handler serves every widened opcode.
#[inline(never)]
extern "rust-preserve-none" fn wide(
    code: &[u32],
    pc: usize,
    hi: u32,
    s: *mut u64,
    len: usize,
    st: &mut State<'_>,
) {
    let word = code[pc + 1];
    let op = word as u8;
    let (a, b, c) = (
        slot_field(word, hi, 8),
        slot_field(word, hi, 16),
        slot_field(word, hi, 24),
    );
    let after = pc + 2;
    if op == Op::LdkX as u8 {
        set(s, len, a, st.function.consts[code[after] as usize]);
        next!(code, after + 1, s, len, st)
    }
    if matches!(
        Op::from_byte(op).expect("verified opcode").info().sem,
        eitri_bytecode::spec::Sem::Branch { .. } | eitri_bytecode::spec::Sem::TestBranch(_)
    ) {
        let (taken, disp) = branch_op(op, a, b, word, s, len);
        let mut to = after;
        if taken {
            to = (to as isize + disp) as usize;
            if !take(st, pc, to) {
                fail!(code, pc, word, s, len, st, FaultKind::Budget)
            }
        }
        next!(code, to, s, len, st)
    }
    if let Err(kind) = slot_op(op, (a, b, c), word, s, len, &st.function.consts) {
        fail!(code, pc, word, s, len, st, kind)
    }
    next!(code, after, s, len, st)
}

extern "rust-preserve-none" fn call_host(
    code: &[u32],
    pc: usize,
    word: u32,
    s: *mut u64,
    len: usize,
    st: &mut State<'_>,
) {
    let site = st.function.sites[(word >> 16) as usize];
    if st.m.budget == 0 {
        fail!(code, pc, word, s, len, st, FaultKind::Budget)
    }
    st.m.budget -= 1;
    let CallTarget::Host(index) = site.target else {
        unreachable!("the verifier matches the opcode to the target");
    };
    // The host reaches its windows through the shared state, by index, so
    // a nested run may grow the slot stack under it.
    let mut cx = Call {
        machine: &mut st.m,
        caller: Frame {
            function: st.id,
            base: st.base,
            call_pc: pc as u32,
            resume: pc + 1,
            result: 0,
        },
        top: st.base + len,
        args: absolute(site.args, st.base),
        results: absolute(site.result, st.base),
    };
    let outcome = st.host.call(&mut cx, index);
    // SAFETY: the current frame, `base..base + len`, is still inside the
    // slot stack (nested runs only grow it); the stack may have moved.
    let s = unsafe { st.m.slots.as_mut_ptr().add(st.base) };
    if let Err(error) = outcome {
        let fault = host_fault(error, index, &[], st.id, pc);
        // `raise` adds this frame and the frames below it.
        let mut inner = fault.trace;
        inner.pop();
        st.pending = Some((fault.kind, inner));
        become raise(code, pc, word, s, len, st)
    }
    next!(code, pc + 1, s, len, st)
}

/// `CALL.SCRIPT`. The fast path makes no call: growth of the slot or
/// frame stack is handed to the cold `grow` handler, which re-executes this
/// instruction; nothing observable (the budget) has changed by then.
extern "rust-preserve-none" fn call_script(
    code: &[u32],
    pc: usize,
    word: u32,
    s: *mut u64,
    len: usize,
    st: &mut State<'_>,
) {
    let site = st.function.sites[(word >> 16) as usize];
    let CallTarget::Script(callee) = site.target else {
        unreachable!("the verifier matches the opcode to the target");
    };
    let callee_fn = &st.m.functions[callee];
    let size = callee_fn.frame_size as usize;
    // The callee's frame starts right after this one (`len` slots).
    let base = st.base + len;
    if st.m.slots.len() < base + size || st.m.frames.len() == st.m.frames.capacity() {
        become grow(code, pc, word, s, len, st)
    }
    if st.m.budget == 0 {
        fail!(code, pc, word, s, len, st, FaultKind::Budget)
    }
    st.m.budget -= 1;
    if st.m.frames.len() as u32 >= st.m.depth {
        fail!(code, pc, word, s, len, st, FaultKind::Depth)
    }
    // Parameters follow the callee's results, whose count the site carries
    // (checked against the callee's signature by the verifier).
    let (args, params) = (site.args, site.result.count as usize);
    if args.end() as usize > len || params + args.count as usize > size {
        out_of_frame();
    }
    // SAFETY: `s` covers this frame and the slot stack holds at least
    // `base + size` slots (checked above), so the callee frame lies inside
    // the same allocation, after this frame.
    let callee_s = unsafe { s.add(len) };
    // One argument is the common case; a fixed copy beats the loop (T4).
    if args.count == 1 {
        set(callee_s, size, params, get(s, len, args.first as usize));
    } else {
        for i in 0..args.count as usize {
            let value = get(s, len, args.first as usize + i);
            set(callee_s, size, params + i, value);
        }
    }
    let pushed = st.m.frames.push_within_capacity(Frame {
        function: st.id,
        base: st.base,
        call_pc: pc as u32,
        resume: pc + 1,
        result: site.result.first,
    });
    debug_assert!(pushed.is_ok(), "capacity checked above");
    st.id = callee;
    st.function = callee_fn;
    st.base = base;
    let code: &[u32] = &callee_fn.code;
    next!(code, 0, callee_s, size, st)
}

/// Cold: make room for a call, then execute it again.
#[cold]
extern "rust-preserve-none" fn grow(
    code: &[u32],
    pc: usize,
    word: u32,
    _: *mut u64,
    len: usize,
    st: &mut State<'_>,
) {
    let site = st.function.sites[(word >> 16) as usize];
    let CallTarget::Script(callee) = site.target else {
        unreachable!("the verifier matches the opcode to the target");
    };
    let end = st.base + len + st.m.functions[callee].frame_size as usize;
    if st.m.slots.len() < end {
        // Doubling keeps growth amortized for deep recursion.
        st.m.slots.resize(end.max(st.m.slots.len() * 2), 0);
    }
    if st.m.frames.len() == st.m.frames.capacity() {
        st.m.frames.reserve(st.m.frames.len().max(16));
    }
    // The slot stack may have moved.
    // SAFETY: `base + len <= slots.len()`: the current frame is in bounds.
    let s = unsafe { st.m.slots.as_mut_ptr().add(st.base) };
    next!(code, pc, s, len, st)
}

extern "rust-preserve-none" fn ret(
    code: &[u32],
    pc: usize,
    word: u32,
    s: *mut u64,
    len: usize,
    st: &mut State<'_>,
) {
    if st.m.frames.len() == st.floor {
        become finish(code, pc, word, s, len, st)
    }
    let Some(caller) = st.m.frames.pop() else {
        out_of_frame()
    };
    let caller_fn = &st.m.functions[caller.function];
    let caller_len = caller_fn.frame_size as usize;
    // SAFETY: frames are laid out back to back in one allocation; the
    // caller's starts `st.base - caller.base` slots before this one.
    let caller_s = unsafe { s.sub(st.base - caller.base) };
    let results = st.function.signature.results.len();
    if results == 1 {
        set(caller_s, caller_len, caller.result as usize, get(s, len, 0));
    } else {
        for i in 0..results {
            let value = get(s, len, i);
            set(caller_s, caller_len, caller.result as usize + i, value);
        }
    }
    st.id = caller.function;
    st.function = caller_fn;
    st.base = caller.base;
    let code: &[u32] = &caller_fn.code;
    next!(code, caller.resume, caller_s, caller_len, st)
}

/// Cold: the entry function returned; its results are at the start of
/// its frame.
#[cold]
extern "rust-preserve-none" fn finish(
    _: &[u32],
    _: usize,
    _: u32,
    _: *mut u64,
    _: usize,
    st: &mut State<'_>,
) {
    st.outcome = Some(Ok(()));
}

extern "rust-preserve-none" fn invalid(
    _: &[u32],
    _: usize,
    word: u32,
    _: *mut u64,
    _: usize,
    _: &mut State<'_>,
) {
    unreachable!("opcode {:#04x} is rejected by the verifier", word as u8)
}

slot_handler!(t013_add_chk_u, Op::AddChkU);
slot_handler!(t013_sub_chk_u, Op::SubChkU);
slot_handler!(t013_mul_chk_u, Op::MulChkU);
slot_handler!(t013_div_u, Op::DivU);
slot_handler!(t013_rem_u, Op::RemU);
slot_handler!(t013_and, Op::And);
slot_handler!(t013_or, Op::Or);
slot_handler!(t013_shl_s8, Op::ShlS8);
slot_handler!(t013_shl_u8, Op::ShlU8);
slot_handler!(t013_shl64, Op::Shl64);
slot_handler!(t013_shr_s8, Op::ShrS8);
slot_handler!(t013_shr_s64, Op::ShrS64);
slot_handler!(t013_shr_u8, Op::ShrU8);
slot_handler!(t013_shr_u64, Op::ShrU64);
slot_handler!(t013_addi_chk_u, Op::AddiChkU);
slot_handler!(t013_andi, Op::Andi);
slot_handler!(t013_shli_s8, Op::ShliS8);
slot_handler!(t013_shli_u8, Op::ShliU8);
slot_handler!(t013_shli64, Op::Shli64);
slot_handler!(t013_shri_s, Op::ShriS);
slot_handler!(t013_shri_u, Op::ShriU);
slot_handler!(t013_neg_wrap, Op::NegWrap);
slot_handler!(t013_neg_chk_u, Op::NegChkU);
slot_handler!(t013_not, Op::Not);
slot_handler!(t013_cvt_wrap, Op::CvtWrap);
slot_handler!(t013_cvt_chk, Op::CvtChk);
slot_handler!(t013_fadd_f32, Op::FaddF32);
slot_handler!(t013_fadd_f64, Op::FaddF64);
slot_handler!(t013_fsub_f32, Op::FsubF32);
slot_handler!(t013_fsub_f64, Op::FsubF64);
slot_handler!(t013_fmul_f32, Op::FmulF32);
slot_handler!(t013_fmul_f64, Op::FmulF64);
slot_handler!(t013_fdiv_f32, Op::FdivF32);
slot_handler!(t013_fdiv_f64, Op::FdivF64);
slot_handler!(t013_fneg_f32, Op::FnegF32);
slot_handler!(t013_fneg_f64, Op::FnegF64);
slot_handler!(t013_fcvt_i2f32, Op::FcvtI2f32);
slot_handler!(t013_fcvt_i2f64, Op::FcvtI2f64);
slot_handler!(t013_fcvt_f32i, Op::FcvtF32i);
slot_handler!(t013_fcvt_f64i, Op::FcvtF64i);
slot_handler!(t013_fcvt_f32f64, Op::FcvtF32f64);
slot_handler!(t013_fcvt_f64f32, Op::FcvtF64f32);
branch_handler!(t013_cb_lo, Op::CbLo);
branch_handler!(t013_cb_ls, Op::CbLs);
branch_handler!(t013_cbi_lo, Op::CbiLo);
branch_handler!(t013_cbi_hi, Op::CbiHi);
slot_handler!(t013_cset_lo, Op::CsetLo);
slot_handler!(t013_cset_ls, Op::CsetLs);
slot_handler!(t013_fcset_feq_f32, Op::FcsetFeqF32);
slot_handler!(t013_fcset_fne_f32, Op::FcsetFneF32);
slot_handler!(t013_fcset_flt_f32, Op::FcsetFltF32);
slot_handler!(t013_fcset_fle_f32, Op::FcsetFleF32);
slot_handler!(t013_fcset_fult_f32, Op::FcsetFultF32);
slot_handler!(t013_fcset_fule_f32, Op::FcsetFuleF32);
slot_handler!(t013_fcset_feq_f64, Op::FcsetFeqF64);
slot_handler!(t013_fcset_fne_f64, Op::FcsetFneF64);
slot_handler!(t013_fcset_flt_f64, Op::FcsetFltF64);
slot_handler!(t013_fcset_fle_f64, Op::FcsetFleF64);
slot_handler!(t013_fcset_fult_f64, Op::FcsetFultF64);
slot_handler!(t013_fcset_fule_f64, Op::FcsetFuleF64);
slot_handler!(t013_frem_f32, Op::FremF32);
slot_handler!(t013_frem_f64, Op::FremF64);

static TABLE: [Handler; 256] = {
    let mut table = [invalid as Handler; 256];
    table[Op::AddChkU as usize] = t013_add_chk_u;
    table[Op::SubChkU as usize] = t013_sub_chk_u;
    table[Op::MulChkU as usize] = t013_mul_chk_u;
    table[Op::DivU as usize] = t013_div_u;
    table[Op::RemU as usize] = t013_rem_u;
    table[Op::And as usize] = t013_and;
    table[Op::Or as usize] = t013_or;
    table[Op::ShlS8 as usize] = t013_shl_s8;
    table[Op::ShlU8 as usize] = t013_shl_u8;
    table[Op::Shl64 as usize] = t013_shl64;
    table[Op::ShrS8 as usize] = t013_shr_s8;
    table[Op::ShrS64 as usize] = t013_shr_s64;
    table[Op::ShrU8 as usize] = t013_shr_u8;
    table[Op::ShrU64 as usize] = t013_shr_u64;
    table[Op::AddiChkU as usize] = t013_addi_chk_u;
    table[Op::Andi as usize] = t013_andi;
    table[Op::ShliS8 as usize] = t013_shli_s8;
    table[Op::ShliU8 as usize] = t013_shli_u8;
    table[Op::Shli64 as usize] = t013_shli64;
    table[Op::ShriS as usize] = t013_shri_s;
    table[Op::ShriU as usize] = t013_shri_u;
    table[Op::NegWrap as usize] = t013_neg_wrap;
    table[Op::NegChkU as usize] = t013_neg_chk_u;
    table[Op::Not as usize] = t013_not;
    table[Op::CvtWrap as usize] = t013_cvt_wrap;
    table[Op::CvtChk as usize] = t013_cvt_chk;
    table[Op::FaddF32 as usize] = t013_fadd_f32;
    table[Op::FaddF64 as usize] = t013_fadd_f64;
    table[Op::FsubF32 as usize] = t013_fsub_f32;
    table[Op::FsubF64 as usize] = t013_fsub_f64;
    table[Op::FmulF32 as usize] = t013_fmul_f32;
    table[Op::FmulF64 as usize] = t013_fmul_f64;
    table[Op::FdivF32 as usize] = t013_fdiv_f32;
    table[Op::FdivF64 as usize] = t013_fdiv_f64;
    table[Op::FnegF32 as usize] = t013_fneg_f32;
    table[Op::FnegF64 as usize] = t013_fneg_f64;
    table[Op::FcvtI2f32 as usize] = t013_fcvt_i2f32;
    table[Op::FcvtI2f64 as usize] = t013_fcvt_i2f64;
    table[Op::FcvtF32i as usize] = t013_fcvt_f32i;
    table[Op::FcvtF64i as usize] = t013_fcvt_f64i;
    table[Op::FcvtF32f64 as usize] = t013_fcvt_f32f64;
    table[Op::FcvtF64f32 as usize] = t013_fcvt_f64f32;
    table[Op::CbLo as usize] = t013_cb_lo;
    table[Op::CbLs as usize] = t013_cb_ls;
    table[Op::CbiLo as usize] = t013_cbi_lo;
    table[Op::CbiHi as usize] = t013_cbi_hi;
    table[Op::CsetLo as usize] = t013_cset_lo;
    table[Op::CsetLs as usize] = t013_cset_ls;
    table[Op::FcsetFeqF32 as usize] = t013_fcset_feq_f32;
    table[Op::FcsetFneF32 as usize] = t013_fcset_fne_f32;
    table[Op::FcsetFltF32 as usize] = t013_fcset_flt_f32;
    table[Op::FcsetFleF32 as usize] = t013_fcset_fle_f32;
    table[Op::FcsetFultF32 as usize] = t013_fcset_fult_f32;
    table[Op::FcsetFuleF32 as usize] = t013_fcset_fule_f32;
    table[Op::FcsetFeqF64 as usize] = t013_fcset_feq_f64;
    table[Op::FcsetFneF64 as usize] = t013_fcset_fne_f64;
    table[Op::FcsetFltF64 as usize] = t013_fcset_flt_f64;
    table[Op::FcsetFleF64 as usize] = t013_fcset_fle_f64;
    table[Op::FcsetFultF64 as usize] = t013_fcset_fult_f64;
    table[Op::FcsetFuleF64 as usize] = t013_fcset_fule_f64;
    table[Op::FremF32 as usize] = t013_frem_f32;
    table[Op::FremF64 as usize] = t013_frem_f64;
    table[Op::Mov as usize] = mov;
    table[Op::Ldi as usize] = ldi;
    table[Op::Ldk as usize] = ldk;
    table[Op::LdkX as usize] = ldk_x;
    table[Op::AddWrap as usize] = add_wrap;
    table[Op::SubWrap as usize] = sub_wrap;
    table[Op::MulWrap as usize] = mul_wrap;
    table[Op::AddChkS as usize] = add_chk;
    table[Op::SubChkS as usize] = sub_chk;
    table[Op::MulChkS as usize] = mul_chk;
    table[Op::DivS as usize] = div;
    table[Op::RemS as usize] = rem;
    table[Op::Xor as usize] = xor;
    table[Op::NegChkS as usize] = neg_chk;
    table[Op::AddiWrap as usize] = addi_wrap;
    table[Op::SubiWrap as usize] = subi_wrap;
    table[Op::AddiChkS as usize] = addi_chk;
    table[Op::SubiChkS as usize] = subi_chk;
    table[Op::MuliChkS as usize] = muli_chk;
    table[Op::CbiEq as usize] = cbi_eq;
    table[Op::CbiNe as usize] = cbi_ne;
    table[Op::CbiLt as usize] = cbi_lt;
    table[Op::CbiGt as usize] = cbi_gt;
    table[Op::CsetEq as usize] = cset_eq;
    table[Op::CsetNe as usize] = cset_ne;
    table[Op::CsetLt as usize] = cset_lt;
    table[Op::CsetLe as usize] = cset_le;
    table[Op::CbEq as usize] = cb_eq;
    table[Op::CbNe as usize] = cb_ne;
    table[Op::CbLt as usize] = cb_lt;
    table[Op::CbLe as usize] = cb_le;
    table[Op::TbTrue as usize] = tb_true;
    table[Op::TbFalse as usize] = tb_false;
    table[Op::J as usize] = j;
    table[Op::JX as usize] = j_x;
    table[Op::CallScript as usize] = call_script;
    table[Op::CallHost as usize] = call_host;
    table[Op::Ret as usize] = ret;
    table[Op::Wide as usize] = wide;
    table
};
