//! The assembler: logical instructions with labels and source spans to an
//! executable [`Function`].
//!
//! Plain instructions are encoded when emitted (their encoding does not
//! depend on layout). Branches are laid out in exactly two passes, with no
//! relaxation loop ([Branch emission](../../../docs/implementation/bytecode.md#branch-emission-without-repeated-relaxation)):
//!
//! 1. Upper-bound positions, assuming every branch takes its long form.
//! 2. A branch is short when its worst-case displacement fits. Final sizes
//!    only shrink, so final displacements are never larger than the bound.
//!    A long conditional branch is its complement, skipping a `J_X`.
//!
//! Linear in the number of instructions.

use eitri_base::{FxHashMap, IndexVec, Span};

use crate::encoding::{EncodeError, Operands, encode};
use crate::function::{CallSite, CallTarget, Function, Signature, ValueType};
use crate::ir::{self, Body, Insn};
use crate::spec::{Cond, Op, Role, Sem, Test};

eitri_base::newtype_index! {
    /// A position in the code being assembled.
    pub struct Label;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsmError {
    Encode(EncodeError),
    /// A branch targets a label that was never bound.
    UnboundLabel(Label),
    /// A label was bound twice.
    Rebound(Label),
    /// `emit` was given a branch or call; use the dedicated method.
    NotPlain(Op),
    /// A condition with no register-form opcode (`GT`, `HI`: swap the
    /// operands instead).
    UnsupportedCondition(Cond),
    TooManyConstants,
    TooManySites,
    CodeTooLarge,
    OutOfMemory,
}

impl From<EncodeError> for AsmError {
    fn from(error: EncodeError) -> Self {
        AsmError::Encode(error)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    /// Already encoded: `words[start..start + len]`.
    Plain {
        start: u32,
        len: u32,
    },
    Branch {
        branch: Branch,
        label: Label,
    },
}

#[derive(Clone, Copy)]
enum Branch {
    Compare {
        cond: Cond,
        a: u32,
        b: u32,
    },
    /// `CBI`: compare a slot with an eight-bit immediate.
    CompareImm {
        cond: Cond,
        a: u32,
        imm: i64,
    },
    Test {
        test: Test,
        a: u32,
    },
    Jump,
}

/// Builds one function. Reusable: `finish` leaves it empty.
///
/// `RECORD` also records the logical instructions as they are emitted
/// ([`ir`]). It is a compile-time choice, so the assembler a first run
/// compiles with carries no recording code at all.
#[derive(Default)]
pub struct Assembler<const RECORD: bool = false> {
    insns: Vec<Kind>,
    spans: Vec<Span>,
    words: Vec<u32>,
    labels: IndexVec<Label, Option<u32>>,
    consts: Vec<u64>,
    const_index: FxHashMap<u64, u32>,
    sites: Vec<CallSite>,
    /// Literal loads: logical instruction and the type they produce.
    literals: Vec<(u32, ValueType)>,
    /// One past the highest slot named so far.
    frame: u32,
    /// Layout working memory, kept between functions (E5/L1).
    max_pos: Vec<u64>,
    short: Vec<bool>,
    final_pos: Vec<u32>,
    /// The logical instructions emitted so far, when recording.
    items: Vec<ir::Item>,
    /// The last finished function's recorded body.
    recorded: Option<Body>,
    memory: Option<eitri_base::MemoryBudget>,
    artifact_charged: usize,
}

impl Assembler {
    /// An assembler that records nothing: the one lowering uses for a
    /// first run.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Assembler<true> {
    /// An assembler that records each function's logical instructions, as
    /// `.eits` in memory; [`Assembler::take_recorded`] returns them after
    /// [`Assembler::finish`].
    pub fn recording() -> Self {
        Self::default()
    }
}

impl<const RECORD: bool> Assembler<RECORD> {
    /// Move the active compilation budget into this assembler.
    pub fn set_memory_budget(&mut self, memory: Option<eitri_base::MemoryBudget>) {
        self.memory = memory;
        self.artifact_charged = 0;
    }

    /// Recover the active compilation budget after lowering.
    pub fn take_memory_budget(&mut self) -> (Option<eitri_base::MemoryBudget>, usize) {
        (
            self.memory.take(),
            std::mem::take(&mut self.artifact_charged),
        )
    }

    /// Allocate temporary storage under the active compilation budget.
    pub fn temporary_vec<T>(&mut self, capacity: usize) -> Result<Vec<T>, AsmError> {
        output_vec(capacity, &mut self.memory)
    }

    /// Release temporary storage after its owner has been dropped.
    pub fn release_temporary_bytes(&mut self, bytes: usize) {
        if let Some(budget) = &mut self.memory {
            budget.release(bytes);
        }
    }

    /// Allocate artifact vector storage and charge it before growing.
    pub fn artifact_vec<T>(&mut self, capacity: usize) -> Result<Vec<T>, AsmError> {
        let before = self.memory.as_ref().map(eitri_base::MemoryBudget::used);
        let values = output_vec(capacity, &mut self.memory)?;
        if let (Some(before), Some(budget)) = (before, &self.memory) {
            self.artifact_charged = self
                .artifact_charged
                .saturating_add(budget.used().saturating_sub(before));
        }
        Ok(values)
    }

    /// Copy a UTF-8 artifact string, charging before allocation.
    pub fn artifact_string(&mut self, text: &str) -> Result<Box<str>, AsmError> {
        let before = self.memory.as_ref().map(eitri_base::MemoryBudget::used);
        let mut value = String::new();
        if let Some(budget) = &mut self.memory {
            budget
                .reserve_string(&mut value, text.len())
                .map_err(|_| AsmError::OutOfMemory)?;
        } else {
            value
                .try_reserve_exact(text.len())
                .map_err(|_| AsmError::OutOfMemory)?;
        }
        value.push_str(text);
        if let (Some(before), Some(budget)) = (before, &self.memory) {
            self.artifact_charged = self
                .artifact_charged
                .saturating_add(budget.used().saturating_sub(before));
        }
        Ok(value.into_boxed_str())
    }

    /// Build managed root metadata under the active compilation budget.
    pub fn string_roots(
        &mut self,
        function: &Function,
        callees: &crate::Callees<'_>,
    ) -> Result<crate::FunctionRoots, crate::VerifyError> {
        let before = self.memory.as_ref().map(eitri_base::MemoryBudget::used);
        let roots = crate::string_roots_budgeted(function, callees, &mut self.memory)?;
        if let (Some(before), Some(budget)) = (before, &self.memory) {
            self.artifact_charged = self
                .artifact_charged
                .saturating_add(budget.used().saturating_sub(before));
        }
        Ok(roots)
    }

    /// Release artifact storage replaced during lowering.
    pub fn release_artifact_bytes(&mut self, bytes: usize) {
        if let Some(budget) = &mut self.memory {
            budget.release(bytes);
            self.artifact_charged = self.artifact_charged.saturating_sub(bytes);
        }
    }

    /// Pre-reserve assembler vectors and its constant table using a
    /// caller-owned budget. `count` is a conservative instruction bound.
    pub fn reserve_budgeted(
        &mut self,
        count: usize,
        budget: &mut eitri_base::MemoryBudget,
    ) -> Result<(), eitri_base::OutOfMemory> {
        macro_rules! reserve_total {
            ($values:expr) => {{
                let additional = count.saturating_sub($values.len());
                budget.reserve_vec($values, additional)?;
            }};
        }
        reserve_total!(&mut self.insns);
        reserve_total!(&mut self.spans);
        reserve_total!(&mut self.words);
        self.labels
            .reserve_budgeted(count.saturating_sub(self.labels.len()), budget)?;
        reserve_total!(&mut self.consts);
        reserve_total!(&mut self.sites);
        reserve_total!(&mut self.literals);
        reserve_total!(&mut self.max_pos);
        reserve_total!(&mut self.short);
        reserve_total!(&mut self.final_pos);
        if RECORD {
            reserve_total!(&mut self.items);
        }
        let additional = count.saturating_sub(self.const_index.len());
        budget.reserve_map(&mut self.const_index, additional)?;
        Ok(())
    }

    /// Retained assembler storage with a conservative constant-table charge.
    pub fn budget_bytes(&self) -> Result<usize, eitri_base::OutOfMemory> {
        self.vector_bytes()
            .checked_add(eitri_base::MemoryBudget::hash_table_bytes::<(u64, u32)>(
                self.const_index.capacity(),
            )?)
            .ok_or(eitri_base::OutOfMemory)
    }

    /// Retained vector allocations. The constant hash table and allocator
    /// bookkeeping are excluded.
    pub fn vector_bytes(&self) -> usize {
        self.insns.capacity() * size_of::<Kind>()
            + self.spans.capacity() * size_of::<Span>()
            + self.words.capacity() * size_of::<u32>()
            + self.labels.capacity() * size_of::<Option<u32>>()
            + self.consts.capacity() * size_of::<u64>()
            + self.sites.capacity() * size_of::<CallSite>()
            + self.literals.capacity() * size_of::<(u32, ValueType)>()
            + self.max_pos.capacity() * size_of::<u64>()
            + self.short.capacity() * size_of::<bool>()
            + self.final_pos.capacity() * size_of::<u32>()
            + self.items.capacity() * size_of::<ir::Item>()
            + self
                .recorded
                .as_ref()
                .map_or(0, |body| body.items.capacity() * size_of::<ir::Item>())
    }

    /// The body recorded for the function last finished, with each
    /// instruction's final position; `None` when not recording.
    pub fn take_recorded(&mut self) -> Option<Body> {
        self.recorded.take()
    }

    #[inline(always)]
    fn note(&mut self, insn: Insn, span: Span) {
        if RECORD {
            self.record_item(insn, span);
        }
    }

    /// Out of line: compiling for a first run never records.
    #[cold]
    #[inline(never)]
    fn record_item(&mut self, insn: Insn, span: Span) {
        self.items.push(ir::Item::new(insn, span));
    }

    pub fn label(&mut self) -> Label {
        self.labels.push(None)
    }

    /// Bind `label` to the next instruction emitted.
    pub fn bind(&mut self, label: Label) -> Result<(), AsmError> {
        let slot = &mut self.labels[label];
        if slot.is_some() {
            return Err(AsmError::Rebound(label));
        }
        *slot = Some(self.insns.len() as u32);
        self.note(Insn::Label(label.as_u32()), Span::default());
        Ok(())
    }

    fn uses(&mut self, slot: u32) {
        // Saturating: an out-of-range slot is rejected when it is encoded.
        self.frame = self.frame.max(slot.saturating_add(1));
    }

    /// Emit an instruction that is not a branch, call, or literal load.
    pub fn emit(&mut self, op: Op, operands: Operands, span: Span) -> Result<(), AsmError> {
        let info = op.info();
        if matches!(
            info.sem,
            Sem::Branch { .. }
                | Sem::FloatBranch { .. }
                | Sem::TestBranch(_)
                | Sem::Jump
                | Sem::Loop { .. }
                | Sem::Call(_)
                | Sem::Wide
        ) {
            return Err(AsmError::NotPlain(op));
        }
        self.plain(op, operands, span)?;
        self.note(Insn::Plain { op, operands }, span);
        Ok(())
    }

    fn plain(&mut self, op: Op, operands: Operands, span: Span) -> Result<(), AsmError> {
        let start = self.words.len() as u32;
        encode(op, operands, &mut self.words)?;
        let columns = [operands.a, operands.b, operands.c];
        for (role, value) in op.info().roles.iter().zip(columns) {
            if role.is_slot() {
                self.uses(value as u32);
            }
        }
        self.insns.push(Kind::Plain {
            start,
            len: self.words.len() as u32 - start,
        });
        self.spans.push(span);
        Ok(())
    }

    /// Load the constant `value` of type `ty` into `dst`: `LDI` when it fits
    /// sixteen bits, otherwise a pooled `LDK` or `LDK_X`.
    pub fn load(
        &mut self,
        dst: u32,
        value: u64,
        ty: ValueType,
        span: Span,
    ) -> Result<(), AsmError> {
        let at = self.insns.len() as u32;
        let signed = value as i64;
        let (imm_min, imm_max) = Role::Imm16.range();
        if (imm_min..=imm_max).contains(&signed) {
            self.plain(Op::Ldi, Operands::abc(dst as i64, signed, 0), span)?;
        } else {
            let index = self.constant(value)?;
            if index <= u16::MAX as u32 {
                self.plain(Op::Ldk, Operands::abc(dst as i64, index as i64, 0), span)?;
            } else {
                let operands = Operands {
                    a: dst as i64,
                    ext: index as i64,
                    ..Operands::default()
                };
                self.plain(Op::LdkX, operands, span)?;
            }
        }
        self.literals.push((at, ty));
        self.note(Insn::Load { dst, value, ty }, span);
        Ok(())
    }

    fn constant(&mut self, value: u64) -> Result<u32, AsmError> {
        if let Some(&index) = self.const_index.get(&value) {
            return Ok(index);
        }
        let index = u32::try_from(self.consts.len()).map_err(|_| AsmError::TooManyConstants)?;
        self.consts.push(value);
        self.const_index.insert(value, index);
        Ok(index)
    }

    /// Branch to `label` when `a cond b`.
    pub fn branch_if(
        &mut self,
        cond: Cond,
        a: u32,
        b: u32,
        label: Label,
        span: Span,
    ) -> Result<(), AsmError> {
        if cond.complement().is_none() {
            return Err(AsmError::UnsupportedCondition(cond));
        }
        self.uses(a);
        self.uses(b);
        self.branch(Branch::Compare { cond, a, b }, label, span);
        self.note_branch(ir::Branch::Compare { cond, a, b }, label, span);
        Ok(())
    }

    /// Branch to `label` when the integer in `a` compares `cond` with `imm`
    /// (`CBI`). Supports `Eq`, `Ne`, `Lt`, and `Gt` where both `imm` and
    /// the complement's immediate fit eight bits, so the long form exists;
    /// see [`imm_complement`].
    pub fn branch_imm(
        &mut self,
        cond: Cond,
        a: u32,
        imm: i64,
        label: Label,
        span: Span,
    ) -> Result<(), AsmError> {
        if imm_complement(cond, imm).is_none() {
            return Err(AsmError::UnsupportedCondition(cond));
        }
        self.uses(a);
        self.branch(Branch::CompareImm { cond, a, imm }, label, span);
        self.note_branch(ir::Branch::CompareImm { cond, a, imm }, label, span);
        Ok(())
    }

    /// Branch to `label` when the bool in `a` passes `test`.
    pub fn branch_test(&mut self, test: Test, a: u32, label: Label, span: Span) {
        self.uses(a);
        self.branch(Branch::Test { test, a }, label, span);
        self.note_branch(ir::Branch::Test { test, a }, label, span);
    }

    pub fn jump(&mut self, label: Label, span: Span) {
        self.branch(Branch::Jump, label, span);
        self.note_branch(ir::Branch::Jump, label, span);
    }

    #[inline(always)]
    fn note_branch(&mut self, branch: ir::Branch, label: Label, span: Span) {
        let target = label.as_u32();
        self.note(Insn::Branch { branch, target }, span);
    }

    fn branch(&mut self, branch: Branch, label: Label, span: Span) {
        self.insns.push(Kind::Branch { branch, label });
        self.spans.push(span);
    }

    pub fn call(&mut self, site: CallSite, span: Span) -> Result<(), AsmError> {
        let index = self.sites.len();
        if index > u16::MAX as usize {
            return Err(AsmError::TooManySites);
        }
        for range in [site.args, site.result] {
            if range.count > 0 {
                self.uses(range.end() - 1);
            }
        }
        let op = match site.target {
            CallTarget::Script(_) => Op::CallScript,
            CallTarget::Host(_) => Op::CallHost,
        };
        self.sites.push(site);
        self.plain(op, Operands::abc(0, index as i64, 0), span)?;
        self.note(Insn::Call(site), span);
        Ok(())
    }

    pub fn ret(&mut self, span: Span) -> Result<(), AsmError> {
        self.plain(Op::Ret, Operands::default(), span)?;
        self.note(Insn::Ret, span);
        Ok(())
    }

    /// Lay out and encode everything emitted, and reset for the next
    /// function. The frame covers every slot named and the signature.
    pub fn finish(&mut self, name: &str, signature: Signature) -> Result<Function, AsmError> {
        let before = self.memory.as_ref().map(eitri_base::MemoryBudget::used);
        let mut result = self.layout(name, signature);
        if RECORD {
            self.recorded = None;
            if result.is_ok() {
                if self.memory.is_some() {
                    match output_vec(self.items.len(), &mut self.memory) {
                        Ok(mut items) => {
                            items.extend_from_slice(&self.items);
                            self.recorded = Some(Body {
                                items,
                                labels: self.labels.len() as u32,
                            });
                        }
                        Err(error) => result = Err(error),
                    }
                } else {
                    self.recorded = Some(Body {
                        items: std::mem::take(&mut self.items),
                        labels: self.labels.len() as u32,
                    });
                }
            }
            self.items.clear();
        }
        if let (Some(before), Some(budget)) = (before, &self.memory)
            && result.is_ok()
        {
            self.artifact_charged = self
                .artifact_charged
                .saturating_add(budget.used().saturating_sub(before));
        }
        self.insns.clear();
        self.spans.clear();
        self.words.clear();
        self.labels.clear();
        self.consts.clear();
        self.const_index.clear();
        self.sites.clear();
        self.literals.clear();
        self.frame = 0;
        result
    }

    fn target(&self, label: Label) -> Result<usize, AsmError> {
        self.labels[label]
            .map(|at| at as usize)
            .ok_or(AsmError::UnboundLabel(label))
    }

    fn layout(&mut self, name: &str, signature: Signature) -> Result<Function, AsmError> {
        let n = self.insns.len();
        // Pass 1: upper-bound positions, every branch long.
        let mut max_pos = std::mem::take(&mut self.max_pos);
        let mut short = std::mem::take(&mut self.short);
        let mut final_pos = std::mem::take(&mut self.final_pos);
        max_pos.clear();
        short.clear();
        final_pos.clear();
        let result = self.place(name, signature, n, &mut max_pos, &mut short, &mut final_pos);
        self.max_pos = max_pos;
        self.short = short;
        self.final_pos = final_pos;
        result
    }

    fn place(
        &mut self,
        name: &str,
        signature: Signature,
        n: usize,
        max_pos: &mut Vec<u64>,
        short: &mut Vec<bool>,
        final_pos: &mut Vec<u32>,
    ) -> Result<Function, AsmError> {
        let mut pos = 0u64;
        for insn in &self.insns {
            max_pos.push(pos);
            pos += match *insn {
                Kind::Plain { len, .. } => len as u64,
                Kind::Branch { branch, .. } => far_size(branch) as u64,
            };
        }
        max_pos.push(pos);
        if pos > i32::MAX as u64 {
            return Err(AsmError::CodeTooLarge);
        }

        // Decide each branch from its worst-case displacement.
        short.resize(n, false);
        for (i, insn) in self.insns.iter().enumerate() {
            let Kind::Branch { branch, label } = *insn else {
                continue;
            };
            let target = self.target(label)?;
            let (min, max) = short_role(branch).range();
            short[i] = if target > i {
                (max_pos[target] - max_pos[i + 1]) as i64 <= max
            } else {
                let bound = (max_pos[i] - max_pos[target]) as i64 + short_size(branch) as i64;
                -bound >= min
            };
        }

        // Pass 2: final positions and encoding.
        let mut pos = 0u32;
        for (i, insn) in self.insns.iter().enumerate() {
            final_pos.push(pos);
            pos += match *insn {
                Kind::Plain { len, .. } => len,
                Kind::Branch { branch, .. } if short[i] => short_size(branch),
                Kind::Branch { branch, .. } => far_size(branch),
            };
        }
        final_pos.push(pos);
        // Recorded instructions are the logical ones, in order, plus labels.
        let mut logical = 0;
        for item in self.items.iter_mut().filter(|_| RECORD) {
            if !matches!(item.insn, Insn::Label(_)) {
                item.pc = final_pos[logical];
                item.words = final_pos[logical + 1] - final_pos[logical];
                logical += 1;
            }
        }

        let mut code = output_vec(pos as usize, &mut self.memory)?;
        let start_words = (pos as usize).div_ceil(64);
        let mut starts = output_vec(start_words, &mut self.memory)?;
        starts.resize(start_words, 0u64);
        let extra_spans = self.insns.iter().zip(short.iter()).filter(|(insn, short)| {
            !**short && matches!(insn, Kind::Branch { branch, .. } if !matches!(branch, Branch::Jump))
        }).count();
        let span_count = n.checked_add(extra_spans).ok_or(AsmError::OutOfMemory)?;
        let mut spans = output_vec(span_count, &mut self.memory)?;
        let mark = |starts: &mut Vec<u64>, at: usize| starts[at / 64] |= 1 << (at % 64);
        for (i, insn) in self.insns.iter().enumerate() {
            let at = code.len();
            debug_assert_eq!(at, final_pos[i] as usize);
            mark(&mut starts, at);
            spans.push((at as u32, self.spans[i]));
            match *insn {
                Kind::Plain { start, len } => {
                    // One to three words: a direct copy, not a `memmove`
                    // call per instruction (E5/L3).
                    for &word in &self.words[start as usize..(start + len) as usize] {
                        code.push(word);
                    }
                }
                Kind::Branch { branch, label } => {
                    let target = final_pos[self.target(label)?] as i64;
                    let end = final_pos[i + 1] as i64;
                    if short[i] {
                        encode_branch(branch, false, target - end, &mut code)?;
                    } else {
                        // The complement skips the two-word `J_X` that follows.
                        if !matches!(branch, Branch::Jump) {
                            encode_branch(branch, true, 2, &mut code)?;
                            mark(&mut starts, code.len());
                            spans.push((code.len() as u32, self.spans[i]));
                        }
                        let jump = Operands {
                            ext: target - end,
                            ..Operands::default()
                        };
                        encode(Op::JX, jump, &mut code)?;
                    }
                }
            }
        }

        let mut literal_types = output_vec(self.literals.len(), &mut self.memory)?;
        literal_types.extend(
            self.literals
                .iter()
                .map(|&(i, ty)| (final_pos[i as usize], ty)),
        );
        let frame_size = self
            .frame
            .max(signature.param_base() + signature.params.len() as u32);
        let mut maps = output_vec(1, &mut self.memory)?;
        maps.push(crate::RootMap::default());
        let mut calls = output_vec(self.sites.len(), &mut self.memory)?;
        let state = crate::RootState {
            map: crate::RootMapId(0),
            initialized: Vec::new(),
        };
        for _ in &self.sites {
            calls.push(crate::CallRoots {
                pre: state.clone(),
                suspended: state.clone(),
                post: state.clone(),
            });
        }
        let mut point_count = 0usize;
        let mut scan = 0usize;
        while scan < code.len() {
            let decoded = crate::decode(&code, scan).expect("assembler emitted valid code");
            point_count += usize::from(crate::verify::needs_safepoint(scan as u32, &decoded));
            scan += decoded.words as usize;
        }
        let safepoints = output_vec(point_count, &mut self.memory)?;
        let mut roots = crate::FunctionRoots {
            entry: crate::RootMapId(0),
            maps,
            safepoints,
            calls,
        };
        let mut pc = 0usize;
        while pc < code.len() {
            let decoded = crate::decode(&code, pc).expect("assembler emitted valid code");
            if crate::verify::needs_safepoint(pc as u32, &decoded) {
                roots.safepoints.push((
                    crate::SafepointPc(pc as u32),
                    crate::RootState {
                        map: crate::RootMapId(0),
                        initialized: Vec::new(),
                    },
                ));
            }
            pc += decoded.words as usize;
        }
        let mut function_name = String::new();
        if let Some(budget) = &mut self.memory {
            budget
                .reserve_string(&mut function_name, name.len())
                .map_err(|_| AsmError::OutOfMemory)?;
        } else {
            function_name
                .try_reserve_exact(name.len())
                .map_err(|_| AsmError::OutOfMemory)?;
        }
        function_name.push_str(name);
        let mut consts = output_vec(self.consts.len(), &mut self.memory)?;
        consts.extend_from_slice(&self.consts);
        let mut sites = output_vec(self.sites.len(), &mut self.memory)?;
        sites.extend_from_slice(&self.sites);
        Ok(Function {
            name: function_name.into_boxed_str(),
            signature,
            reentry_safe: true,
            frame_size,
            code: code.into_boxed_slice(),
            // Copied at their exact size, so the assembler keeps its
            // capacity for the next function.
            consts: consts.into_boxed_slice(),
            strings: Box::default(),
            sites: sites.into_boxed_slice(),
            roots,
            literal_types: literal_types.into_boxed_slice(),
            starts: starts.into_boxed_slice(),
            spans: spans.into_boxed_slice(),
        })
    }
}

fn output_vec<T>(
    capacity: usize,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<Vec<T>, AsmError> {
    let mut values = Vec::new();
    if let Some(budget) = memory {
        budget
            .reserve_vec(&mut values, capacity)
            .map_err(|_| AsmError::OutOfMemory)?;
    } else {
        values
            .try_reserve_exact(capacity)
            .map_err(|_| AsmError::OutOfMemory)?;
    }
    Ok(values)
}

fn wide(slots: &[u32]) -> u32 {
    u32::from(slots.iter().any(|&s| s > u8::MAX as u32))
}

fn short_size(branch: Branch) -> u32 {
    match branch {
        Branch::Compare { a, b, .. } => 1 + wide(&[a, b]),
        Branch::CompareImm { a, .. } => 1 + wide(&[a]),
        Branch::Test { a, .. } => 1 + wide(&[a]),
        Branch::Jump => 1,
    }
}

/// The long form: the complement over a `J_X`, or a `J_X` alone.
fn far_size(branch: Branch) -> u32 {
    match branch {
        Branch::Jump => 2,
        _ => short_size(branch) + 2,
    }
}

fn short_role(branch: Branch) -> Role {
    match branch {
        Branch::Compare { .. } | Branch::CompareImm { .. } => Role::Disp8,
        Branch::Test { .. } => Role::Disp16,
        Branch::Jump => Role::Disp24,
    }
}

pub(crate) fn compare_op(cond: Cond) -> Op {
    match cond {
        Cond::Eq => Op::CbEq,
        Cond::Ne => Op::CbNe,
        Cond::Lt => Op::CbLt,
        Cond::Le => Op::CbLe,
        Cond::Lo => Op::CbLo,
        Cond::Ls => Op::CbLs,
        Cond::Gt | Cond::Hi => unreachable!("rejected by branch_if"),
    }
}

/// The negation of `x cond imm` as another `CBI` condition and immediate,
/// if both immediates fit eight bits: `x < k` is false exactly when
/// `x > k - 1`. `None` for conditions `CBI` cannot express.
pub fn imm_complement(cond: Cond, imm: i64) -> Option<(Cond, i64)> {
    let (complement, other) = match cond {
        Cond::Eq => (Cond::Ne, imm),
        Cond::Ne => (Cond::Eq, imm),
        Cond::Lt => (Cond::Gt, imm - 1),
        Cond::Gt => (Cond::Lt, imm + 1),
        _ => return None,
    };
    let fits = |k: i64| (i8::MIN as i64..=i8::MAX as i64).contains(&k);
    (fits(imm) && fits(other)).then_some((complement, other))
}

pub(crate) fn compare_imm_op(cond: Cond) -> Op {
    match cond {
        Cond::Eq => Op::CbiEq,
        Cond::Ne => Op::CbiNe,
        Cond::Lt => Op::CbiLt,
        Cond::Gt => Op::CbiGt,
        _ => unreachable!("rejected by branch_imm"),
    }
}

pub(crate) fn test_op(test: Test) -> Op {
    match test {
        Test::True => Op::TbTrue,
        Test::False => Op::TbFalse,
        Test::None => Op::TbNone,
        Test::Some => Op::TbSome,
    }
}

/// Encode a short branch with displacement `disp`, or its complement.
fn encode_branch(
    branch: Branch,
    complement: bool,
    disp: i64,
    code: &mut Vec<u32>,
) -> Result<(), AsmError> {
    match branch {
        Branch::Compare { cond, a, b } => {
            let (cond, a, b) = if complement {
                let (cond, swap) = cond.complement().expect("checked by branch_if");
                if swap { (cond, b, a) } else { (cond, a, b) }
            } else {
                (cond, a, b)
            };
            encode(
                compare_op(cond),
                Operands::abc(a as i64, b as i64, disp),
                code,
            )?;
        }
        Branch::CompareImm { cond, a, imm } => {
            let (cond, imm) = if complement {
                imm_complement(cond, imm).expect("checked by branch_imm")
            } else {
                (cond, imm)
            };
            encode(
                compare_imm_op(cond),
                Operands::abc(a as i64, imm, disp),
                code,
            )?;
        }
        Branch::Test { test, a } => {
            let test = if complement { test.complement() } else { test };
            encode(test_op(test), Operands::abc(a as i64, disp, 0), code)?;
        }
        Branch::Jump => encode(Op::J, Operands::abc(disp, 0, 0), code)?,
    }
    Ok(())
}
