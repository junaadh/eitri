//! The verifier: checks that a function's code is well formed and well
//! typed before anything runs.
//!
//! Structural pass: every word decodes to an implemented operation; slots
//! lie inside the frame; constants and call sites exist and call windows
//! fit both the frame and the callee's signature; branches land on
//! instruction starts (never on an operand word or a `WIDE` follower); no
//! instruction falls off the end; the start map is exact.
//!
//! Typed pass: a dataflow over the control-flow graph to a fixpoint. Each
//! slot is `i64`, `bool`, uninitialized, or in conflict (different types on
//! paths that meet). Reading an uninitialized or conflicting slot, an
//! operand of the wrong type, or a `bool` literal other than 0 or 1 is
//! rejected. The lattice has height two per slot, so the pass is
//! O(instructions × frame slots).

use std::fmt;

use eitri_base::IndexVec;

use crate::encoding::{DecodeError, Decoded, decode};
use crate::function::{CallTarget, Function, FunctionId, Signature, ValueType};
use crate::spec::{BRANCH, Cond, END, Op, Role, Sem, Test};

/// The signatures calls are checked against.
pub struct Callees<'a> {
    pub functions: &'a IndexVec<FunctionId, Signature>,
    pub hosts: &'a [Signature],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyError {
    pub pc: u32,
    pub kind: ErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Decode(DecodeError),
    Unimplemented(Op),
    SlotOutsideFrame(u32),
    NoConstant(u32),
    NoSite(u32),
    NoCallee,
    CallShape,
    /// A branch to a word that does not start an instruction.
    BadTarget(i64),
    FallsOffEnd,
    StartMapMismatch,
    UntypedLiteral,
    /// A `bool` literal that is not 0 or 1.
    BadBool(u64),
    Uninitialized(u32),
    /// Paths that meet here leave different types in the slot.
    Conflict(u32),
    WrongType {
        slot: u32,
        expected: ValueType,
        found: ValueType,
    },
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pc {}: ", self.pc)?;
        match &self.kind {
            ErrorKind::Decode(e) => write!(f, "malformed instruction: {e:?}"),
            ErrorKind::Unimplemented(op) => write!(f, "`{}` is not executable yet", op.name()),
            ErrorKind::SlotOutsideFrame(s) => write!(f, "slot r{s} is outside the frame"),
            ErrorKind::NoConstant(i) => write!(f, "no constant k{i}"),
            ErrorKind::NoSite(i) => write!(f, "no call site {i}"),
            ErrorKind::NoCallee => write!(f, "the call target does not exist"),
            ErrorKind::CallShape => write!(f, "the call window does not match the callee or frame"),
            ErrorKind::BadTarget(t) => {
                write!(f, "branch to {t}, which does not start an instruction")
            }
            ErrorKind::FallsOffEnd => write!(f, "execution can fall off the end of the code"),
            ErrorKind::StartMapMismatch => write!(f, "the instruction-start map is wrong"),
            ErrorKind::UntypedLiteral => write!(f, "a literal load without a type"),
            ErrorKind::BadBool(v) => write!(f, "`bool` literal {v} is neither 0 nor 1"),
            ErrorKind::Uninitialized(s) => write!(f, "slot r{s} may be read before it is written"),
            ErrorKind::Conflict(s) => {
                write!(f, "slot r{s} holds different types on paths that meet")
            }
            ErrorKind::WrongType {
                slot,
                expected,
                found,
            } => write!(
                f,
                "slot r{slot} holds {found:?} where {expected:?} is required"
            ),
        }
    }
}

impl std::error::Error for VerifyError {}

/// What the typed pass knows about one slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Ty(ValueType),
    Conflict,
    Uninit,
}

impl Slot {
    fn join(self, other: Slot) -> Slot {
        match (self, other) {
            (a, b) if a == b => a,
            (Slot::Uninit, _) | (_, Slot::Uninit) => Slot::Uninit,
            _ => Slot::Conflict,
        }
    }
}

pub fn verify(function: &Function, callees: &Callees<'_>) -> Result<(), VerifyError> {
    let insns = structure(function, callees)?;
    types(function, callees, &insns)
}

/// A decoded instruction and its PC.
struct Insn {
    pc: u32,
    decoded: Decoded,
}

fn error(pc: u32, kind: ErrorKind) -> VerifyError {
    VerifyError { pc, kind }
}

fn columns(d: &Decoded) -> [i64; 3] {
    [d.operands.a, d.operands.b, d.operands.c]
}

/// Where a branch goes, if the instruction branches.
fn branch_target(insn: &Insn) -> Option<i64> {
    let info = insn.decoded.op.info();
    if !info.effects.contains(BRANCH) {
        return None;
    }
    let end = insn.pc as i64 + insn.decoded.words as i64;
    let disp = info
        .roles
        .iter()
        .zip(columns(&insn.decoded))
        .find(|(role, _)| role.is_displacement())
        .map_or(insn.decoded.operands.ext, |(_, value)| value);
    Some(end + disp)
}

fn structure(function: &Function, callees: &Callees<'_>) -> Result<Vec<Insn>, VerifyError> {
    let code = &function.code;
    let mut insns = Vec::new();
    let mut starts = vec![0u64; code.len().div_ceil(64)];
    let mut pc = 0usize;
    while pc < code.len() {
        let at = pc as u32;
        let decoded = decode(code, pc).map_err(|e| error(at, ErrorKind::Decode(e)))?;
        let info = decoded.op.info();
        if !info.implemented {
            return Err(error(at, ErrorKind::Unimplemented(decoded.op)));
        }
        for (role, value) in info.roles.iter().zip(columns(&decoded)) {
            if role.is_slot() && value as u32 >= function.frame_size {
                return Err(error(at, ErrorKind::SlotOutsideFrame(value as u32)));
            }
            if matches!(role, Role::Const16) && value as usize >= function.consts.len() {
                return Err(error(at, ErrorKind::NoConstant(value as u32)));
            }
            if matches!(role, Role::Site16) {
                check_site(function, callees, at, value as u32)?;
            }
        }
        if info.format.ext_role() == Role::Const32
            && decoded.operands.ext as usize >= function.consts.len()
        {
            return Err(error(
                at,
                ErrorKind::NoConstant(decoded.operands.ext as u32),
            ));
        }
        starts[pc / 64] |= 1 << (pc % 64);
        pc += decoded.words as usize;
        if pc >= code.len() && !info.effects.contains(END) {
            return Err(error(at, ErrorKind::FallsOffEnd));
        }
        insns.push(Insn { pc: at, decoded });
    }
    if insns.is_empty() {
        return Err(error(0, ErrorKind::FallsOffEnd));
    }
    if starts[..] != function.starts[..] {
        return Err(error(0, ErrorKind::StartMapMismatch));
    }
    for insn in &insns {
        if let Some(target) = branch_target(insn) {
            let valid = (0..code.len() as i64).contains(&target)
                && starts[target as usize / 64] >> (target % 64) & 1 == 1;
            if !valid {
                return Err(error(insn.pc, ErrorKind::BadTarget(target)));
            }
        }
    }
    Ok(insns)
}

fn callee<'a>(callees: &'a Callees<'_>, target: CallTarget) -> Option<&'a Signature> {
    match target {
        CallTarget::Script(f) => callees.functions.get(f),
        CallTarget::Host(h) => callees.hosts.get(h as usize),
    }
}

fn check_site(
    function: &Function,
    callees: &Callees<'_>,
    pc: u32,
    index: u32,
) -> Result<(), VerifyError> {
    let site = function
        .sites
        .get(index as usize)
        .ok_or_else(|| error(pc, ErrorKind::NoSite(index)))?;
    let signature = callee(callees, site.target).ok_or_else(|| error(pc, ErrorKind::NoCallee))?;
    // A host borrows both windows at once, so they must be disjoint.
    let disjoint = matches!(site.target, CallTarget::Script(_)) || !site.args.overlaps(site.result);
    let shape = site.args.count as usize == signature.params.len()
        && site.result.count as usize == signature.results.len()
        && site.args.end() <= function.frame_size
        && site.result.end() <= function.frame_size
        && disjoint;
    if !shape {
        return Err(error(pc, ErrorKind::CallShape));
    }
    Ok(())
}

fn types(function: &Function, callees: &Callees<'_>, insns: &[Insn]) -> Result<(), VerifyError> {
    let frame = function.frame_size as usize;
    // Instruction index by PC, for successors.
    let mut index_of = vec![u32::MAX; function.code.len()];
    for (i, insn) in insns.iter().enumerate() {
        index_of[insn.pc as usize] = i as u32;
    }
    let mut entry = vec![Slot::Uninit; frame];
    let base = function.signature.param_base() as usize;
    for (i, &ty) in function.signature.params.iter().enumerate() {
        entry[base + i] = Slot::Ty(ty);
    }
    let mut states: Vec<Option<Vec<Slot>>> = vec![None; insns.len()];
    states[0] = Some(entry);
    let mut work = vec![0usize];
    let mut queued = vec![false; insns.len()];
    queued[0] = true;
    while let Some(i) = work.pop() {
        queued[i] = false;
        let mut state = states[i].clone().expect("queued states exist");
        let insn = &insns[i];
        transfer(function, callees, insn, &mut state)?;
        let info = insn.decoded.op.info();
        let mut successors = [None, None];
        if !info.effects.contains(END) {
            successors[0] = Some(insn.pc as usize + insn.decoded.words as usize);
        }
        successors[1] = branch_target(insn).map(|t| t as usize);
        for next in successors.into_iter().flatten() {
            let j = index_of[next] as usize;
            let changed = match &mut states[j] {
                Some(existing) => {
                    let mut changed = false;
                    for (old, &new) in existing.iter_mut().zip(&state) {
                        let joined = old.join(new);
                        changed |= joined != *old;
                        *old = joined;
                    }
                    changed
                }
                empty => {
                    *empty = Some(state.clone());
                    true
                }
            };
            if changed && !queued[j] {
                queued[j] = true;
                work.push(j);
            }
        }
    }
    Ok(())
}

fn read(
    state: &[Slot],
    pc: u32,
    slot: i64,
    expected: Option<ValueType>,
) -> Result<ValueType, VerifyError> {
    let slot = slot as u32;
    match state[slot as usize] {
        Slot::Uninit => Err(error(pc, ErrorKind::Uninitialized(slot))),
        Slot::Conflict => Err(error(pc, ErrorKind::Conflict(slot))),
        Slot::Ty(found) => match expected {
            Some(expected) if expected != found => Err(error(
                pc,
                ErrorKind::WrongType {
                    slot,
                    expected,
                    found,
                },
            )),
            _ => Ok(found),
        },
    }
}

fn transfer(
    function: &Function,
    callees: &Callees<'_>,
    insn: &Insn,
    state: &mut [Slot],
) -> Result<(), VerifyError> {
    use ValueType::{Bool, I64};
    let pc = insn.pc;
    let d = &insn.decoded;
    let [a, b, c] = columns(d);
    let write = |state: &mut [Slot], slot: i64, ty| state[slot as usize] = Slot::Ty(ty);
    match d.op.info().sem {
        Sem::Move => {
            let ty = read(state, pc, b, None)?;
            write(state, a, ty);
        }
        Sem::LoadImm | Sem::LoadConst => {
            let ty = function
                .literal_type(pc)
                .ok_or_else(|| error(pc, ErrorKind::UntypedLiteral))?;
            let value = match d.op {
                Op::Ldi => b as u64,
                Op::Ldk => function.consts[b as usize],
                _ => function.consts[d.operands.ext as usize],
            };
            if ty == Bool && value > 1 {
                return Err(error(pc, ErrorKind::BadBool(value)));
            }
            write(state, a, ty);
        }
        // An immediate form: one integer operand and an `Imm8`.
        Sem::Int { imm: true, .. } => {
            read(state, pc, b, Some(I64))?;
            write(state, a, I64);
        }
        Sem::Int { op, .. } => {
            use crate::spec::IntOp;
            match op {
                IntOp::Xor => {
                    let ty = read(state, pc, b, None)?;
                    read(state, pc, c, Some(ty))?;
                    write(state, a, ty);
                }
                IntOp::Neg => {
                    read(state, pc, b, Some(I64))?;
                    write(state, a, I64);
                }
                _ => {
                    read(state, pc, b, Some(I64))?;
                    read(state, pc, c, Some(I64))?;
                    write(state, a, I64);
                }
            }
        }
        Sem::Branch { imm: true, .. } => {
            read(state, pc, a, Some(I64))?;
        }
        Sem::Branch { cond, .. } => compare(state, pc, cond, a, b)?,
        Sem::Set(cond) => {
            compare(state, pc, cond, b, c)?;
            write(state, a, Bool);
        }
        Sem::TestBranch(Test::True | Test::False) => {
            read(state, pc, a, Some(Bool))?;
        }
        Sem::Jump | Sem::Wide => {}
        Sem::Call(_) => {
            let site = function.sites[b as usize];
            let signature = callee(callees, site.target).expect("checked structurally");
            for (slot, &ty) in site.args.slots().zip(&signature.params) {
                read(state, pc, slot as i64, Some(ty))?;
            }
            for (slot, &ty) in site.result.slots().zip(&signature.results) {
                write(state, slot as i64, ty);
            }
        }
        Sem::Return => {
            for (slot, &ty) in function.signature.results.iter().enumerate() {
                read(state, pc, slot as i64, Some(ty))?;
            }
        }
        // Rejected structurally: not implemented.
        _ => return Err(error(pc, ErrorKind::Unimplemented(d.op))),
    }
    Ok(())
}

/// Operands of a comparison: `EQ`/`NE` take two values of one type,
/// ordering takes integers.
fn compare(state: &[Slot], pc: u32, cond: Cond, a: i64, b: i64) -> Result<(), VerifyError> {
    if matches!(cond, Cond::Eq | Cond::Ne) {
        let ty = read(state, pc, a, None)?;
        read(state, pc, b, Some(ty))?;
    } else {
        read(state, pc, a, Some(ValueType::I64))?;
        read(state, pc, b, Some(ValueType::I64))?;
    }
    Ok(())
}
