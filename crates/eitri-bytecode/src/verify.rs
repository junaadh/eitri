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
//! O(instructions × frame slots). Root structural/coverage checks add work
//! proportional to root metadata and visited facts, plus a linear revisit of
//! reachable instructions, with O(root metadata + visited facts) scratch. No
//! additional instruction-by-frame matrix is kept.

use std::fmt;

use eitri_base::IndexVec;

use crate::encoding::{DecodeError, Decoded, decode};
use crate::function::{CallTarget, Function, FunctionId, Signature, ValueType};
use crate::spec::{AllocationClass, BRANCH, Cond, END, Op, POLL, Role, Sem, Test};

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
    OutOfMemory,
    Unimplemented(Op),
    SlotOutsideFrame(u32),
    NoConstant(u32),
    NoSite(u32),
    NoCallee,
    CallShape,
    /// `CALL.SCRIPT` at a host site, or `CALL.HOST` at a script site.
    CallKind,
    /// The frame cannot hold the signature's results and parameters.
    FrameTooSmall,
    /// A branch to a word that does not start an instruction.
    BadTarget(i64),
    FallsOffEnd,
    StartMapMismatch,
    RootMapMissing(crate::RootMapId),
    SafepointNotInstructionStart(u32),
    SafepointsNotSorted,
    RootOutsideFrame(u32),
    DynamicRootsOverlap(u32),
    InitializedSpanOutsideFrame,
    RootCallCountMismatch,
    UnsupportedGuardedRoots,
    InvalidRootGuard,
    MissingSafepoint,
    RootNotInitialized(u32),
    RootWrongShape(u32),
    RootMissing(u32),
    InvalidDynamicPair(u32, u32),
    UntypedLiteral,
    /// A `bool` literal that is not 0 or 1.
    BadBool(u64),
    InvalidScalar(&'static str),
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
            ErrorKind::OutOfMemory => write!(f, "runtime memory limit exceeded"),
            ErrorKind::Unimplemented(op) => write!(f, "`{}` is not executable yet", op.name()),
            ErrorKind::SlotOutsideFrame(s) => write!(f, "slot r{s} is outside the frame"),
            ErrorKind::NoConstant(i) => write!(f, "no constant k{i}"),
            ErrorKind::NoSite(i) => write!(f, "no call site {i}"),
            ErrorKind::NoCallee => write!(f, "the call target does not exist"),
            ErrorKind::CallShape => write!(f, "the call window does not match the callee or frame"),
            ErrorKind::CallKind => write!(f, "the call opcode does not match its target"),
            ErrorKind::FrameTooSmall => {
                write!(f, "the frame is smaller than the results and parameters")
            }
            ErrorKind::BadTarget(t) => {
                write!(f, "branch to {t}, which does not start an instruction")
            }
            ErrorKind::FallsOffEnd => write!(f, "execution can fall off the end of the code"),
            ErrorKind::StartMapMismatch => write!(f, "the instruction-start map is wrong"),
            ErrorKind::RootMapMissing(id) => write!(f, "root map {} does not exist", id.0),
            ErrorKind::SafepointNotInstructionStart(pc) => {
                write!(f, "safepoint {pc} is not an instruction start")
            }
            ErrorKind::SafepointsNotSorted => write!(f, "safepoints are not sorted and unique"),
            ErrorKind::RootOutsideFrame(slot) => {
                write!(f, "root slot r{slot} is outside the frame")
            }
            ErrorKind::DynamicRootsOverlap(slot) => {
                write!(f, "dynamic root slot r{slot} overlaps another root")
            }
            ErrorKind::InitializedSpanOutsideFrame => {
                write!(f, "initialized span is outside the frame")
            }
            ErrorKind::RootCallCountMismatch => {
                write!(f, "root call state count differs from call sites")
            }
            ErrorKind::UnsupportedGuardedRoots => {
                write!(f, "guarded roots require managed verifier facts")
            }
            ErrorKind::InvalidRootGuard => write!(f, "invalid guarded root recipe"),
            ErrorKind::MissingSafepoint => write!(f, "missing safepoint root state"),
            ErrorKind::RootNotInitialized(slot) => {
                write!(f, "root slot r{slot} is not definitely initialized")
            }
            ErrorKind::RootWrongShape(slot) => write!(f, "root slot r{slot} has the wrong shape"),
            ErrorKind::RootMissing(slot) => write!(f, "live root slot r{slot} is missing"),
            ErrorKind::InvalidDynamicPair(a, b) => {
                write!(f, "invalid dynamic root pair r{a}, r{b}")
            }
            ErrorKind::UntypedLiteral => write!(f, "a literal load without a type"),
            ErrorKind::InvalidScalar(message) => f.write_str(message),
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
    verify_budgeted(function, callees, &mut None)
}

/// Verify with fallible, context-budgeted scratch allocations.
pub fn verify_budgeted(
    function: &Function,
    callees: &Callees<'_>,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<(), VerifyError> {
    let insns = structure(function, callees, memory)?;
    root_structure(function, memory)?;
    required_roots(function, &insns)?;
    types(function, callees, &insns, memory)?;
    let managed = !function.strings.is_empty()
        || function
            .signature
            .params
            .iter()
            .chain(&function.signature.results)
            .any(|t| *t == ValueType::String)
        || function.sites.iter().any(|site| {
            callee(callees, site.target).is_some_and(|sig| {
                sig.params
                    .iter()
                    .chain(&sig.results)
                    .any(|t| *t == ValueType::String)
            })
        });
    if managed {
        let expected = string_roots_budgeted(function, callees, memory)?;
        let check = |actual: crate::RootMapId,
                     needed: crate::RootMapId,
                     pc|
         -> Result<(), VerifyError> {
            let a = &function.roots.maps[actual.0 as usize].ordinary;
            for (i, &word) in expected.maps[needed.0 as usize].ordinary.iter().enumerate() {
                let missing = word & !a.get(i).copied().unwrap_or(0);
                if missing != 0 {
                    return Err(error(
                        pc,
                        ErrorKind::RootMissing((i * 64 + missing.trailing_zeros() as usize) as u32),
                    ));
                }
            }
            Ok(())
        };
        check(function.roots.entry, expected.entry, 0)?;
        for (pc, needed) in &expected.safepoints {
            let i = function
                .roots
                .safepoints
                .binary_search_by_key(pc, |(pc, _)| *pc)
                .map_err(|_| error(pc.0, ErrorKind::MissingSafepoint))?;
            check(function.roots.safepoints[i].1.map, needed.map, pc.0)?;
        }
        for (actual, needed) in function.roots.calls.iter().zip(&expected.calls) {
            check(actual.pre.map, needed.pre.map, 0)?;
            check(actual.suspended.map, needed.suspended.map, 0)?;
            check(actual.post.map, needed.post.map, 0)?;
        }
        if let Some(budget) = memory {
            budget.release(expected.heap_bytes());
        }
    }
    release_memory(&insns, memory);
    Ok(())
}

/// Structural metadata checks are O(bitmap words + dynamic slots + states +
/// spans), expected time using a hash set, and O(dynamic slots) scratch per
/// map. No instruction-by-frame matrix is built. Facts and coverage are not
/// checked here; that is a separate verifier obligation.
fn root_structure(
    function: &Function,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<(), VerifyError> {
    let roots = &function.roots;
    let frame = function.frame_size;
    let check_id = |id: crate::RootMapId, pc| {
        if roots.maps.get(id.0 as usize).is_none() {
            Err(error(pc, ErrorKind::RootMapMissing(id)))
        } else {
            Ok(())
        }
    };
    let check_state = |state: &crate::RootState, pc| {
        check_id(state.map, pc)?;
        for span in &state.initialized {
            if span
                .first
                .checked_add(span.count)
                .is_none_or(|end| end > frame)
            {
                return Err(error(pc, ErrorKind::InitializedSpanOutsideFrame));
            }
        }
        Ok(())
    };
    check_id(roots.entry, 0)?;
    for map in &roots.maps {
        for (word_index, &word) in map.ordinary.iter().enumerate() {
            if word == 0 {
                continue;
            }
            let start = word_index.saturating_mul(64);
            let valid = (frame as usize).saturating_sub(start).min(64);
            let invalid = if valid == 64 {
                0
            } else {
                word & (u64::MAX << valid)
            };
            if invalid != 0 {
                let slot = start.saturating_add(invalid.trailing_zeros() as usize);
                return Err(error(
                    0,
                    ErrorKind::RootOutsideFrame(u32::try_from(slot).unwrap_or(u32::MAX)),
                ));
            }
        }
        let mut used = if map.dynamic.is_empty() {
            Vec::new()
        } else {
            filled_memory(frame as usize, false, memory)?
        };
        for &(type_slot, payload_slot) in &map.dynamic {
            for slot in [type_slot, payload_slot] {
                if slot >= frame {
                    return Err(error(0, ErrorKind::RootOutsideFrame(slot)));
                }
                let ordinary = map
                    .ordinary
                    .get(slot as usize / 64)
                    .is_some_and(|word| word & (1u64 << (slot % 64)) != 0);
                if ordinary || used[slot as usize] {
                    return Err(error(0, ErrorKind::DynamicRootsOverlap(slot)));
                }
                used[slot as usize] = true;
            }
        }
        release_memory(&used, memory);
    }
    for map in &roots.maps {
        for recipe in &map.guarded {
            if recipe.guards.is_empty() || recipe.type_slot == recipe.payload_slot {
                return Err(error(0, ErrorKind::InvalidRootGuard));
            }
            for slot in [recipe.type_slot, recipe.payload_slot]
                .into_iter()
                .chain(recipe.guards.iter().map(|&(slot, _)| slot))
            {
                if slot >= frame {
                    return Err(error(0, ErrorKind::RootOutsideFrame(slot)));
                }
            }
            if recipe
                .guards
                .iter()
                .any(|&(slot, _)| slot == recipe.type_slot || slot == recipe.payload_slot)
            {
                return Err(error(0, ErrorKind::InvalidRootGuard));
            }
        }
    }
    if roots.calls.len() != function.sites.len() {
        return Err(error(0, ErrorKind::RootCallCountMismatch));
    }
    for call in &roots.calls {
        for state in [&call.pre, &call.suspended, &call.post] {
            check_state(state, 0)?;
        }
    }
    let mut previous = None;
    for (pc, state) in &roots.safepoints {
        if previous.is_some_and(|old| old >= *pc) {
            return Err(error(pc.0, ErrorKind::SafepointsNotSorted));
        }
        if !function.is_start(pc.0 as usize) {
            return Err(error(pc.0, ErrorKind::SafepointNotInstructionStart(pc.0)));
        }
        check_state(state, pc.0)?;
        previous = Some(*pc);
    }
    Ok(())
}

/// Check a root map against independent per-slot initialization, shape and
/// liveness facts. All live initialized references must be present; marked
/// slots must be initialized and have the correct shape even when dead.
/// Dynamic recipes must name reciprocal, distinct, initialized slot pairs.
/// Work and scratch are O(facts + bitmap words + dynamic recipes), expected
/// hash-set time; collisions can increase that bound.
pub fn verify_root_state(facts: &[crate::RootFact], map: &crate::RootMap) -> Result<(), ErrorKind> {
    use crate::RootShape;
    // Flat scalar facts have no conditional case information; never certify
    // guarded pairs as unconditional or read an inactive payload.
    if !map.guarded.is_empty() {
        return Err(ErrorKind::UnsupportedGuardedRoots);
    }
    let mut covered = eitri_base::FxHashSet::default();
    for (word_index, &word) in map.ordinary.iter().enumerate() {
        let mut bits = word;
        while bits != 0 {
            let index = word_index
                .checked_mul(64)
                .and_then(|n| n.checked_add(bits.trailing_zeros() as usize))
                .unwrap_or(usize::MAX);
            let slot = u32::try_from(index).unwrap_or(u32::MAX);
            let fact = facts.get(index).ok_or(ErrorKind::RootOutsideFrame(slot))?;
            if !fact.initialized {
                return Err(ErrorKind::RootNotInitialized(slot));
            }
            if fact.shape != RootShape::Reference {
                return Err(ErrorKind::RootWrongShape(slot));
            }
            covered.insert(slot);
            bits &= bits - 1;
        }
    }
    for &(ty, payload) in &map.dynamic {
        let a = facts
            .get(ty as usize)
            .ok_or(ErrorKind::RootOutsideFrame(ty))?;
        let b = facts
            .get(payload as usize)
            .ok_or(ErrorKind::RootOutsideFrame(payload))?;
        if !a.initialized {
            return Err(ErrorKind::RootNotInitialized(ty));
        }
        if !b.initialized {
            return Err(ErrorKind::RootNotInitialized(payload));
        }
        if ty == payload
            || a.shape != (RootShape::DynamicType { payload })
            || b.shape != (RootShape::DynamicPayload { type_slot: ty })
            || !covered.insert(ty)
            || !covered.insert(payload)
        {
            return Err(ErrorKind::InvalidDynamicPair(ty, payload));
        }
    }
    for (slot, fact) in facts.iter().enumerate() {
        if fact.initialized && fact.live && !matches!(fact.shape, RootShape::Scalar) {
            let slot = u32::try_from(slot).map_err(|_| ErrorKind::RootOutsideFrame(u32::MAX))?;
            if !covered.contains(&slot) {
                return Err(ErrorKind::RootMissing(slot));
            }
        }
    }
    Ok(())
}

/// Scalar-only production facts need no frame-wide scan at each safepoint:
/// inspect the sparse marked roots and declared initialized spans directly.
fn scalar_root_state(
    slots: &[Slot],
    map: &crate::RootMap,
    spans: &[crate::SlotRange],
    _managed: bool,
) -> Result<(), ErrorKind> {
    if !map.guarded.is_empty() {
        return Err(ErrorKind::UnsupportedGuardedRoots);
    }
    for span in spans {
        for slot in span.slots() {
            if !matches!(slots[slot as usize], Slot::Ty(_)) {
                return Err(ErrorKind::RootNotInitialized(slot));
            }
        }
    }
    for (word_index, &word) in map.ordinary.iter().enumerate() {
        let mut bits = word;
        while bits != 0 {
            let slot = (word_index * 64 + bits.trailing_zeros() as usize) as u32;
            match slots[slot as usize] {
                Slot::Ty(ValueType::String) => {}
                Slot::Ty(_) => return Err(ErrorKind::RootWrongShape(slot)),
                _ => return Err(ErrorKind::RootNotInitialized(slot)),
            }
            bits &= bits - 1;
        }
    }
    if let Some(&(ty, payload)) = map.dynamic.first() {
        for slot in [ty, payload] {
            if !matches!(slots[slot as usize], Slot::Ty(_)) {
                return Err(ErrorKind::RootNotInitialized(slot));
            }
        }
        return Err(ErrorKind::InvalidDynamicPair(ty, payload));
    }
    Ok(())
}

fn required_roots(function: &Function, insns: &[Insn]) -> Result<(), VerifyError> {
    let mut points = function.roots.safepoints.iter().peekable();
    for insn in insns {
        while points.peek().is_some_and(|(pc, _)| pc.0 < insn.pc) {
            points.next();
        }
        if needs_safepoint(insn.pc, &insn.decoded)
            && !points.peek().is_some_and(|(pc, _)| pc.0 == insn.pc)
        {
            return Err(error(insn.pc, ErrorKind::MissingSafepoint));
        }
    }
    Ok(())
}

/// Calls use CallRoots; branch polls occur only on taken backward edges.
pub(crate) fn needs_safepoint(pc: u32, decoded: &Decoded) -> bool {
    if matches!(decoded.op.info().sem, Sem::Call(_)) {
        return false;
    }
    if decoded.op.allocation_class() != AllocationClass::None {
        return true;
    }
    decoded.op.info().effects.contains(POLL)
        && branch_target(&Insn {
            pc,
            decoded: *decoded,
        })
        .is_some_and(|target| target <= pc as i64)
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

/// Build managed roots with fallible temporary and output allocations.
pub fn string_roots(
    function: &Function,
    callees: &Callees<'_>,
) -> Result<crate::FunctionRoots, VerifyError> {
    string_roots_budgeted(function, callees, &mut None)
}

fn reserve_memory<T>(
    values: &mut Vec<T>,
    additional: usize,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<(), VerifyError> {
    if let Some(budget) = memory {
        budget
            .reserve_vec(values, additional)
            .map_err(|_| error(0, ErrorKind::OutOfMemory))
    } else {
        values
            .try_reserve_exact(additional)
            .map_err(|_| error(0, ErrorKind::OutOfMemory))
    }
}

fn filled_memory<T: Clone>(
    length: usize,
    value: T,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<Vec<T>, VerifyError> {
    let mut values = Vec::new();
    reserve_memory(&mut values, length, memory)?;
    values.resize(length, value);
    Ok(values)
}

fn release_memory<T>(values: &Vec<T>, memory: &mut Option<eitri_base::MemoryBudget>) {
    if let Some(budget) = memory {
        budget.release(values.capacity() * size_of::<T>());
    }
}

fn push_memory<T>(
    values: &mut Vec<T>,
    value: T,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<(), VerifyError> {
    if values.len() == values.capacity() {
        let target = values.capacity().saturating_mul(2).max(4);
        reserve_memory(values, target.saturating_sub(values.len()), memory)?;
    }
    values.push(value);
    Ok(())
}

fn intern_live_state(
    roots: &mut crate::FunctionRoots,
    state: &[Slot],
    live: &[bool],
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<crate::RootState, VerifyError> {
    let last = state
        .iter()
        .zip(live)
        .rposition(|(&ty, &live)| live && ty == Slot::Ty(ValueType::String));
    let words = last.map_or(0, |slot| slot / 64 + 1);
    let mut ordinary = filled_memory(words, 0u64, memory)?;
    for (slot, (&ty, &live)) in state.iter().zip(live).enumerate() {
        if live && ty == Slot::Ty(ValueType::String) {
            ordinary[slot / 64] |= 1 << (slot % 64);
        }
    }
    let map = crate::RootMap {
        ordinary,
        dynamic: Vec::new(),
        guarded: Vec::new(),
    };
    let id = if let Some(id) = roots.maps.iter().position(|old| *old == map) {
        release_memory(&map.ordinary, memory);
        id
    } else {
        push_memory(&mut roots.maps, map, memory)?;
        roots.maps.len() - 1
    };
    Ok(crate::RootState {
        map: crate::RootMapId(id as u32),
        initialized: Vec::new(),
    })
}

/// CFG liveness uses O(instructions × frame) scratch; each allocation is
/// reserved before growth. Root output remains charged to `memory` on success.
pub fn string_roots_budgeted(
    function: &Function,
    callees: &Callees<'_>,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<crate::FunctionRoots, VerifyError> {
    let insns = structure(function, callees, memory)?;
    let n = insns.len();
    let frame = function.frame_size as usize;
    let cells = n
        .checked_mul(frame)
        .ok_or_else(|| error(0, ErrorKind::OutOfMemory))?;
    let mut index = filled_memory(function.code.len(), 0usize, memory)?;
    for (i, insn) in insns.iter().enumerate() {
        index[insn.pc as usize] = i;
    }
    let successors = |i: usize| {
        let insn = &insns[i];
        [
            (!insn.decoded.op.info().effects.contains(END)).then_some(i + 1),
            branch_target(insn).map(|pc| index[pc as usize]),
        ]
    };
    let mut states = filled_memory(cells, Slot::Uninit, memory)?;
    let mut reachable = filled_memory(n, false, memory)?;
    let mut queued = filled_memory(n, false, memory)?;
    let mut state = filled_memory(frame, Slot::Uninit, memory)?;
    let mut work = Vec::new();
    reserve_memory(&mut work, n, memory)?;
    for (i, &ty) in function.signature.params.iter().enumerate() {
        states[function.signature.param_base() as usize + i] = Slot::Ty(ty);
    }
    reachable[0] = true;
    queued[0] = true;
    work.push(0usize);
    while let Some(i) = work.pop() {
        queued[i] = false;
        state.copy_from_slice(&states[i * frame..(i + 1) * frame]);
        transfer(function, callees, &insns[i], &mut state)?;
        for j in successors(i).into_iter().flatten() {
            let row = &mut states[j * frame..(j + 1) * frame];
            let mut changed = !reachable[j];
            if reachable[j] {
                for (old, &new) in row.iter_mut().zip(&state) {
                    let joined = old.join(new);
                    changed |= joined != *old;
                    *old = joined;
                }
            } else {
                row.copy_from_slice(&state);
                reachable[j] = true;
            }
            if changed && !queued[j] {
                queued[j] = true;
                work.push(j);
            }
        }
    }
    let mut uses = filled_memory(cells, false, memory)?;
    let mut defs = filled_memory(cells, false, memory)?;
    for (i, insn) in insns.iter().enumerate() {
        for (role, value) in insn
            .decoded
            .op
            .info()
            .roles
            .iter()
            .zip(columns(&insn.decoded))
        {
            match role {
                Role::Src => uses[i * frame + value as usize] = true,
                Role::Dst => defs[i * frame + value as usize] = true,
                Role::SrcDst => {
                    uses[i * frame + value as usize] = true;
                    defs[i * frame + value as usize] = true;
                }
                _ => {}
            }
        }
        match insn.decoded.op.info().sem {
            Sem::StringJoin => {
                let first = insn.decoded.operands.b as usize;
                let last = insn.decoded.operands.c as usize;
                uses[i * frame + first..=i * frame + last].fill(true);
            }
            Sem::Call(_) => {
                let site = function.sites[insn.decoded.operands.b as usize];
                for slot in site.args.slots() {
                    uses[i * frame + slot as usize] = true;
                }
                for slot in site.result.slots() {
                    defs[i * frame + slot as usize] = true;
                }
            }
            Sem::Return => uses[i * frame..i * frame + function.signature.results.len()].fill(true),
            _ => {}
        }
    }
    let mut live_in = filled_memory(cells, false, memory)?;
    let mut live_out = filled_memory(cells, false, memory)?;
    loop {
        let mut changed = false;
        for i in (0..n).rev() {
            if !reachable[i] {
                continue;
            }
            for slot in 0..frame {
                let at = i * frame + slot;
                let out = successors(i)
                    .into_iter()
                    .flatten()
                    .any(|j| live_in[j * frame + slot]);
                let input = uses[at] || (out && !defs[at]);
                changed |= live_out[at] != out || live_in[at] != input;
                live_out[at] = out;
                live_in[at] = input;
            }
        }
        if !changed {
            break;
        }
    }
    let empty = crate::RootState {
        map: crate::RootMapId(0),
        initialized: Vec::new(),
    };
    let mut maps = Vec::new();
    push_memory(&mut maps, crate::RootMap::default(), memory)?;
    let mut calls = Vec::new();
    reserve_memory(&mut calls, function.sites.len(), memory)?;
    calls.resize(
        function.sites.len(),
        crate::CallRoots {
            pre: empty.clone(),
            suspended: empty.clone(),
            post: empty.clone(),
        },
    );
    let mut safepoints = Vec::new();
    let count = insns
        .iter()
        .filter(|insn| needs_safepoint(insn.pc, &insn.decoded))
        .count();
    reserve_memory(&mut safepoints, count, memory)?;
    let mut roots = crate::FunctionRoots {
        entry: crate::RootMapId(0),
        maps,
        safepoints,
        calls,
    };
    roots.entry = intern_live_state(&mut roots, &states[..frame], &live_in[..frame], memory)?.map;
    let mut suspended = filled_memory(frame, false, memory)?;
    let mut after = filled_memory(frame, Slot::Uninit, memory)?;
    for (i, insn) in insns.iter().enumerate() {
        let row = i * frame..(i + 1) * frame;
        if needs_safepoint(insn.pc, &insn.decoded) {
            let point = if reachable[i] {
                intern_live_state(
                    &mut roots,
                    &states[row.clone()],
                    &live_in[row.clone()],
                    memory,
                )?
            } else {
                empty.clone()
            };
            roots.safepoints.push((crate::SafepointPc(insn.pc), point));
        }
        if reachable[i] && matches!(insn.decoded.op.info().sem, Sem::Call(_)) {
            for slot in 0..frame {
                suspended[slot] = live_out[i * frame + slot] && !defs[i * frame + slot];
            }
            let site = function.sites[insn.decoded.operands.b as usize];
            if matches!(site.target, CallTarget::Host(_)) {
                for slot in site.args.slots() {
                    suspended[slot as usize] = true;
                }
            }
            after.copy_from_slice(&states[row.clone()]);
            transfer(function, callees, insn, &mut after)?;
            let pre = intern_live_state(
                &mut roots,
                &states[row.clone()],
                &live_in[row.clone()],
                memory,
            )?;
            let during = intern_live_state(&mut roots, &states[row.clone()], &suspended, memory)?;
            let post = intern_live_state(&mut roots, &after, &live_out[row], memory)?;
            roots.calls[insn.decoded.operands.b as usize] = crate::CallRoots {
                pre,
                suspended: during,
                post,
            };
        }
    }
    release_memory(&suspended, memory);
    release_memory(&after, memory);
    release_memory(&live_in, memory);
    release_memory(&live_out, memory);
    release_memory(&uses, memory);
    release_memory(&defs, memory);
    release_memory(&work, memory);
    release_memory(&state, memory);
    release_memory(&reachable, memory);
    release_memory(&queued, memory);
    release_memory(&states, memory);
    release_memory(&index, memory);
    release_memory(&insns, memory);
    Ok(roots)
}

fn structure(
    function: &Function,
    callees: &Callees<'_>,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<Vec<Insn>, VerifyError> {
    let code = &function.code;
    let signature = &function.signature;
    if (function.frame_size as usize) < signature.results.len() + signature.params.len() {
        return Err(error(0, ErrorKind::FrameTooSmall));
    }
    let mut insns = Vec::new();
    reserve_memory(&mut insns, code.len(), memory)?;
    let mut starts = filled_memory(code.len().div_ceil(64), 0u64, memory)?;
    let mut pc = 0usize;
    while pc < code.len() {
        let at = pc as u32;
        let decoded = decode(code, pc).map_err(|e| error(at, ErrorKind::Decode(e)))?;
        let info = decoded.op.info();
        if decoded.op == Op::StrNew && decoded.operands.b as usize >= function.strings.len() {
            return Err(error(at, ErrorKind::NoConstant(decoded.operands.b as u32)));
        }
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
                check_site(function, callees, at, decoded.op, value as u32)?;
            }
        }
        if decoded.op == Op::StrJoin && decoded.operands.b > decoded.operands.c {
            return Err(error(
                at,
                ErrorKind::InvalidScalar("invalid string join range"),
            ));
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
    release_memory(&starts, memory);
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
    op: Op,
    index: u32,
) -> Result<(), VerifyError> {
    let site = function
        .sites
        .get(index as usize)
        .ok_or_else(|| error(pc, ErrorKind::NoSite(index)))?;
    // Handlers are specialized by opcode and trust the target's kind.
    let kind_matches = match site.target {
        CallTarget::Script(_) => op == Op::CallScript,
        CallTarget::Host(_) => op == Op::CallHost,
    };
    if !kind_matches {
        return Err(error(pc, ErrorKind::CallKind));
    }
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

/// The typed pass: a forward dataflow over basic blocks. A state (one
/// [`Slot`] per frame slot) is kept only at each block's entry and carried
/// through the block, so memory is O(blocks × frame) rather than
/// O(instructions × frame): a function naming a high slot costs one frame
/// per block, not one per instruction.
fn types(
    function: &Function,
    callees: &Callees<'_>,
    insns: &[Insn],
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<(), VerifyError> {
    let frame = function.frame_size as usize;
    let successors = |insn: &Insn| {
        let info = insn.decoded.op.info();
        let fallthrough =
            (!info.effects.contains(END)).then(|| insn.pc as usize + insn.decoded.words as usize);
        [fallthrough, branch_target(insn).map(|t| t as usize)]
    };
    // Blocks start at the entry, at branch targets, and after a branch or
    // an instruction that ends execution.
    let mut leader = filled_memory(insns.len(), false, memory)?;
    let mut index_of = filled_memory(function.code.len(), u32::MAX, memory)?;
    for (i, insn) in insns.iter().enumerate() {
        index_of[insn.pc as usize] = i as u32;
    }
    leader[0] = true;
    for (i, insn) in insns.iter().enumerate() {
        let info = insn.decoded.op.info();
        if let Some(target) = branch_target(insn) {
            leader[index_of[target as usize] as usize] = true;
        }
        if (info.effects.contains(BRANCH) || info.effects.contains(END)) && i + 1 < insns.len() {
            leader[i + 1] = true;
        }
    }
    let mut block_of = filled_memory(insns.len(), 0u32, memory)?;
    let mut starts = Vec::new();
    reserve_memory(
        &mut starts,
        leader.iter().filter(|&&leader| leader).count(),
        memory,
    )?;
    for (i, &is_leader) in leader.iter().enumerate() {
        if is_leader {
            starts.push(i);
        }
        block_of[i] = starts.len() as u32 - 1;
    }
    let cells = starts
        .len()
        .checked_mul(frame)
        .ok_or_else(|| error(0, ErrorKind::OutOfMemory))?;
    let mut states = filled_memory(cells, Slot::Uninit, memory)?;
    let mut reachable = filled_memory(starts.len(), false, memory)?;
    let base = function.signature.param_base() as usize;
    for (i, &ty) in function.signature.params.iter().enumerate() {
        states[base + i] = Slot::Ty(ty);
    }
    reachable[0] = true;
    let mut work = Vec::new();
    reserve_memory(&mut work, starts.len(), memory)?;
    work.push(0usize);
    let mut queued = filled_memory(starts.len(), false, memory)?;
    queued[0] = true;
    let mut state = filled_memory(frame, Slot::Uninit, memory)?;
    while let Some(b) = work.pop() {
        queued[b] = false;
        state.copy_from_slice(&states[b * frame..(b + 1) * frame]);
        let end = starts.get(b + 1).copied().unwrap_or(insns.len());
        for insn in &insns[starts[b]..end] {
            transfer(function, callees, insn, &mut state)?;
        }
        for next in successors(&insns[end - 1]).into_iter().flatten() {
            let j = block_of[index_of[next] as usize] as usize;
            let row = &mut states[j * frame..(j + 1) * frame];
            let mut changed = !reachable[j];
            if reachable[j] {
                for (old, &new) in row.iter_mut().zip(&state) {
                    let joined = old.join(new);
                    changed |= joined != *old;
                    *old = joined;
                }
            } else {
                row.copy_from_slice(&state);
                reachable[j] = true;
            }
            if changed && !queued[j] {
                queued[j] = true;
                work.push(j);
            }
        }
    }
    // Revisit only reachable blocks after convergence, using the same block
    // states; no per-instruction frame snapshots or root-fact matrix.
    let roots = &function.roots;
    let map = |id: crate::RootMapId| &roots.maps[id.0 as usize];
    let check = |slots: &[Slot], root: &crate::RootState, pc| {
        scalar_root_state(
            slots,
            map(root.map),
            &root.initialized,
            !function.strings.is_empty(),
        )
        .map_err(|kind| error(pc, kind))
    };
    scalar_root_state(
        &states[..frame],
        map(roots.entry),
        &[],
        !function.strings.is_empty(),
    )
    .map_err(|kind| error(0, kind))?;
    let mut points = roots.safepoints.iter().peekable();
    for b in 0..starts.len() {
        if !reachable[b] {
            continue;
        }
        state.copy_from_slice(&states[b * frame..(b + 1) * frame]);
        let end = starts.get(b + 1).copied().unwrap_or(insns.len());
        for insn in &insns[starts[b]..end] {
            while points.peek().is_some_and(|(pc, _)| pc.0 < insn.pc) {
                points.next();
            }
            if points.peek().is_some_and(|(pc, _)| pc.0 == insn.pc) {
                let (_, root) = points.next().expect("peeked");
                check(&state, root, insn.pc)?;
            }
            let call = if matches!(insn.decoded.op.info().sem, Sem::Call(_)) {
                Some(&roots.calls[insn.decoded.operands.b as usize])
            } else {
                None
            };
            if let Some(call) = call {
                check(&state, &call.pre, insn.pc)?;
                check(&state, &call.suspended, insn.pc)?;
            }
            transfer(function, callees, insn, &mut state)?;
            if let Some(call) = call {
                check(&state, &call.post, insn.pc)?;
            }
        }
    }
    release_memory(&leader, memory);
    release_memory(&index_of, memory);
    release_memory(&block_of, memory);
    release_memory(&starts, memory);
    release_memory(&states, memory);
    release_memory(&reachable, memory);
    release_memory(&work, memory);
    release_memory(&queued, memory);
    release_memory(&state, memory);
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
        Sem::StringAlloc => write(state, a, ValueType::String),
        Sem::StringLen => {
            read(state, pc, b, Some(ValueType::String))?;
            write(state, a, I64);
        }
        Sem::StringConcat => {
            read(state, pc, b, Some(ValueType::String))?;
            read(state, pc, c, Some(ValueType::String))?;
            write(state, a, ValueType::String);
        }
        Sem::StringCompare => {
            read(state, pc, b, Some(ValueType::String))?;
            read(state, pc, c, Some(ValueType::String))?;
            write(state, a, I64);
        }
        Sem::StringJoin => {
            for slot in b..=c {
                read(state, pc, slot, Some(ValueType::String))?;
            }
            write(state, a, ValueType::String);
        }
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
            if !ty.canonical(value) {
                return Err(error(
                    pc,
                    ErrorKind::InvalidScalar("noncanonical scalar literal"),
                ));
            }
            write(state, a, ty);
        }
        Sem::Int { op, mode, imm } => {
            use crate::spec::{IntOp, Mode};
            let ty = read(state, pc, b, None)?;
            let invalid = || {
                error(
                    pc,
                    ErrorKind::InvalidScalar("incompatible integer operation or width"),
                )
            };
            let unary = matches!(op, IntOp::Neg | IntOp::Not);
            let bool_xor = op == IntOp::Xor && ty == Bool;
            if !ty.integer() && !bool_xor {
                read(state, pc, b, Some(I64))?;
                return Err(invalid());
            }
            if !imm && !unary {
                if matches!(op, IntOp::Shl(_) | IntOp::Shr(_)) {
                    let rhs = read(state, pc, c, None)?;
                    if !rhs.integer() {
                        return Err(invalid());
                    }
                } else {
                    read(state, pc, c, Some(ty))?;
                }
            }
            let result = match op {
                IntOp::Shl(width) | IntOp::Shr(width) => {
                    if ty.bits() != width.bits()
                        || (mode == Mode::Signed && !ty.signed())
                        || (mode == Mode::Unsigned && ty.signed())
                    {
                        return Err(invalid());
                    }
                    if imm && c >= width.bits() as i64 {
                        return Err(invalid());
                    }
                    ty
                }
                IntOp::And | IntOp::Or | IntOp::Xor => ty,
                IntOp::Not => {
                    if ty.signed() {
                        I64
                    } else {
                        ValueType::U64
                    }
                }
                _ => {
                    if matches!(mode, Mode::Signed | Mode::CheckedSigned) && ty == ValueType::U64 {
                        return Err(invalid());
                    }
                    if matches!(mode, Mode::Unsigned | Mode::CheckedUnsigned) && ty.signed() {
                        return Err(invalid());
                    }
                    if matches!(mode, Mode::Unsigned | Mode::CheckedUnsigned) {
                        ValueType::U64
                    } else {
                        I64
                    }
                }
            };
            write(state, a, result);
        }
        Sem::Convert { .. } => {
            let source = read(state, pc, b, None)?;
            let descriptor = c as u8;
            let dest = crate::scalar::format_type(descriptor & 7);
            if descriptor & !15 != 0
                || dest.is_none()
                || !source.integer()
                || (descriptor & 8 != 0) != source.signed()
            {
                return Err(error(
                    pc,
                    ErrorKind::InvalidScalar("invalid integer conversion descriptor"),
                ));
            }
            write(state, a, dest.unwrap());
        }
        Sem::Float { op, precision } => {
            let ty = if precision == crate::spec::Precision::F32 {
                ValueType::F32
            } else {
                ValueType::F64
            };
            read(state, pc, b, Some(ty))?;
            if op != crate::spec::FloatOp::Neg {
                read(state, pc, c, Some(ty))?;
            }
            write(state, a, ty);
        }
        Sem::FloatSet { precision, .. } => {
            let ty = if precision == crate::spec::Precision::F32 {
                ValueType::F32
            } else {
                ValueType::F64
            };
            read(state, pc, b, Some(ty))?;
            read(state, pc, c, Some(ty))?;
            write(state, a, Bool);
        }
        Sem::FloatConvert(kind) => {
            use crate::spec::FloatConversion::*;
            let descriptor = c as u8;
            let int = crate::scalar::format_type(descriptor).ok_or_else(|| {
                error(
                    pc,
                    ErrorKind::InvalidScalar("invalid float conversion descriptor"),
                )
            })?;
            if matches!(kind, F32ToF64 | F64ToF32) && c != 0 {
                return Err(error(
                    pc,
                    ErrorKind::InvalidScalar("reserved float conversion field"),
                ));
            }
            let (from, to) = match kind {
                IntToF32 => (int, ValueType::F32),
                IntToF64 => (int, ValueType::F64),
                F32ToInt => (ValueType::F32, int),
                F64ToInt => (ValueType::F64, int),
                F32ToF64 => (ValueType::F32, ValueType::F64),
                F64ToF32 => (ValueType::F64, ValueType::F32),
            };
            read(state, pc, b, Some(from))?;
            write(state, a, to);
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
    let ty = read(state, pc, a, None)?;
    if ty == ValueType::String {
        return Err(error(
            pc,
            ErrorKind::InvalidScalar("string comparisons are not supported yet"),
        ));
    }
    read(state, pc, b, Some(ty))?;
    let equal = matches!(cond, Cond::Eq | Cond::Ne);
    if !(ty.integer() || ty == ValueType::Char || (equal && ty == ValueType::Bool)) {
        return Err(error(
            pc,
            ErrorKind::InvalidScalar("invalid integer comparison type"),
        ));
    }
    if !equal && matches!(cond, Cond::Lt | Cond::Le | Cond::Gt) && ty == ValueType::U64 {
        return Err(error(
            pc,
            ErrorKind::InvalidScalar("signed comparison of u64"),
        ));
    }
    Ok(())
}
