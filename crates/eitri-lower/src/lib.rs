//! Lowering (T009): checked syntax to typed bytecode functions.
//!
//! One bytecode function per script `fn`, plus `<module>` for the top-level
//! statements (function 0, the entry). Frames follow the bytecode
//! convention: results, then parameters, then bindings and temporaries.
//!
//! - Bindings get a slot when declared and keep it to the end of their
//!   block; temporaries come from a stack-ordered counter released after
//!   every statement, so slots stay low.
//! - A name used as an operand is read from its binding's slot directly,
//!   without a copy. Sound because nothing in the first slice can change a
//!   local in the middle of an expression; closures (T019) must revisit this.
//! - Operands are evaluated left to right and nothing is constant-folded,
//!   so faults happen at run time after every earlier effect.
//! - Operator chains, `&&`/`||` chains, and `else if` chains are walked
//!   iteratively; recursion only follows parenthesized nesting, which the
//!   parser bounds.
//! - Conditions branch directly (compare-and-branch on the complement), so
//!   `if a < b` is one instruction, not a `CSET` plus a test.
//! - A `while` loop is top-tested and its back edge is the `J` after the
//!   body, so the budget is polled after the body and before the condition,
//!   as the first slice specifies.
//!
//! `lower_simple` retains the comparison baseline: binding operands are
//! copied, arithmetic uses registers, and comparisons materialize before
//! branching. The choice is a const parameter, so the default lowering has
//! no runtime mode test. Both modes retain destination passing and reuse.
//!
//! Time and memory are linear in the module's syntax.

use eitri_base::{Idx, IndexVec, Span};
use eitri_bytecode::ir::Body;
use eitri_bytecode::spec::{Cond, Test};
use eitri_bytecode::{
    AsmError, Assembler, CallSite, CallTarget, Function, FunctionId, Label, Op, Operands,
    Signature, SlotRange, ValueType, imm_complement,
};
use eitri_check::{BindingId, Checked, HostFnId, Hosts, Input, Res, Ty};
use eitri_diag::{Code, Diagnostic, Label as DiagLabel};
use eitri_syntax::ast::{
    AssignOp, BinaryOp, Block, Else, Expr, ExprId, FnId, IfExpr, IfId, Item, Stmt, StmtId, UnaryOp,
};

/// The top-level code: always function 0.
pub const ENTRY: FunctionId = FunctionId::from_u32(0);

/// One compiled module. Owned by the run that compiled it.
pub struct Program {
    pub functions: IndexVec<FunctionId, Function>,
    pub entry: FunctionId,
    /// Owned layouts. Scalar-only programs need no layout table. Managed
    /// bytecode must check its required storage domain before using this table;
    /// IDs are local to it and must never be compared across programs.
    pub layouts: Option<eitri_types::LayoutStore>,
    /// Each function's logical instructions (`.eits` in memory), when
    /// lowering was asked to keep them.
    pub bodies: Option<IndexVec<FunctionId, Body>>,
}

impl Program {
    /// Every function's signature, for the verifier.
    pub fn signatures(&self) -> IndexVec<FunctionId, Signature> {
        Self::signatures_of(&self.functions)
    }

    pub fn signatures_of(
        functions: &IndexVec<FunctionId, Function>,
    ) -> IndexVec<FunctionId, Signature> {
        IndexVec::from_raw(functions.iter().map(|f| f.signature.clone()).collect())
    }
}

/// Lowering's reusable working memory.
#[derive(Default)]
pub struct LowerScratch {
    asm: Assembler,
    /// Used instead of `asm` when the IR is kept: a separate instantiation,
    /// so first-run lowering contains no recording code.
    recording: Assembler<true>,
    slots: IndexVec<BindingId, u32>,
    stack: Vec<ExprId>,
}

impl LowerScratch {
    pub fn new() -> Self {
        Self::default()
    }
}

pub fn value_type(ty: Ty) -> ValueType {
    match ty {
        Ty::I64 => ValueType::I64,
        Ty::I8 => ValueType::I8,
        Ty::U8 => ValueType::U8,
        Ty::U64 => ValueType::U64,
        Ty::F32 => ValueType::F32,
        Ty::F64 => ValueType::F64,
        Ty::Char => ValueType::Char,
        Ty::Bool => ValueType::Bool,
        Ty::Void | Ty::Error => unreachable!("checked code has only value types here"),
    }
}

fn signature(params: &[Ty], ret: Ty) -> Signature {
    Signature {
        params: params.iter().map(|&ty| value_type(ty)).collect(),
        results: if ret == Ty::Void {
            Vec::new()
        } else {
            vec![value_type(ret)]
        },
    }
}

/// Bytecode signatures of every registered host function, by host index.
pub fn host_signatures(hosts: &Hosts) -> Vec<Signature> {
    (0..hosts.function_count())
        .map(|index| {
            let host = hosts.function(HostFnId::new(index));
            signature(&host.params, host.ret)
        })
        .collect()
}

/// Lower a checked module. The only error is an implementation limit
/// (`E0002`): a function needing more than 65,536 slots or too much code.
/// With `keep_ir`, the assembler also records each function's logical
/// instructions for printing or re-optimizing; a first run passes `false`.
pub fn lower(
    input: &Input<'_>,
    checked: &Checked,
    scratch: &mut LowerScratch,
    keep_ir: bool,
) -> Result<Program, Diagnostic> {
    lower_mode::<false>(input, checked, scratch, keep_ir)
}

/// Retained simple lowering baseline (T009): copy binding operands, use
/// register arithmetic, and materialize comparisons before testing them.
/// Keeps destination passing and stack-ordered temporary reuse. It has the
/// same source semantics and limits as `lower`, but may need more slots.
pub fn lower_simple(
    input: &Input<'_>,
    checked: &Checked,
    scratch: &mut LowerScratch,
    keep_ir: bool,
) -> Result<Program, Diagnostic> {
    lower_mode::<true>(input, checked, scratch, keep_ir)
}

fn lower_mode<const SIMPLE: bool>(
    input: &Input<'_>,
    checked: &Checked,
    scratch: &mut LowerScratch,
    keep_ir: bool,
) -> Result<Program, Diagnostic> {
    scratch.slots.clear();
    scratch.slots.resize(checked.bindings.len(), u32::MAX);
    scratch.stack.clear();
    if keep_ir {
        let lowerer = Lowerer::<_, SIMPLE> {
            input,
            checked,
            asm: &mut scratch.recording,
            slots: &mut scratch.slots,
            stack: &mut scratch.stack,
            top: 0,
        };
        lower_with(lowerer)
    } else {
        let lowerer = Lowerer::<_, SIMPLE> {
            input,
            checked,
            asm: &mut scratch.asm,
            slots: &mut scratch.slots,
            stack: &mut scratch.stack,
            top: 0,
        };
        lower_with(lowerer)
    }
}

/// Lower every function; with a recording assembler, keep each body.
fn lower_with<const R: bool, const SIMPLE: bool>(
    mut lowerer: Lowerer<'_, '_, R, SIMPLE>,
) -> Result<Program, Diagnostic> {
    let (input, ast) = (lowerer.input, lowerer.input.ast);
    let mut bodies = R.then(|| IndexVec::with_capacity(ast.fns.len() + 1));
    let mut functions = IndexVec::with_capacity(ast.fns.len() + 1);
    let module = lowerer
        .module()
        .map_err(|_| too_large(input, "<module>", Span::default()))?;
    functions.push(module);
    if let Some(bodies) = &mut bodies {
        bodies.push(
            lowerer
                .asm
                .take_recorded()
                .expect("recorded while lowering"),
        );
    }
    for (id, decl) in ast.fns.iter_enumerated() {
        let function = lowerer.function(id).map_err(|_| {
            let name = input.interner.resolve(input.tokens.symbol(decl.name));
            too_large(input, name, input.tokens.span(decl.name))
        })?;
        functions.push(function);
        if let Some(bodies) = &mut bodies {
            bodies.push(
                lowerer
                    .asm
                    .take_recorded()
                    .expect("recorded while lowering"),
            );
        }
    }
    Ok(Program {
        functions,
        entry: ENTRY,
        layouts: None,
        bodies,
    })
}

fn too_large(input: &Input<'_>, name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::Limit,
        format!("implementation limit: `{name}` needs more than 65,536 slots or too much code"),
    )
    .with_label(DiagLabel::primary(input.source, span))
}

/// Convert root-builder failures into a source-located implementation-limit
/// diagnostic. Callers must propagate every builder operation through this
/// boundary rather than discarding a failed snapshot.
pub fn root_build_diagnostic(
    error: eitri_bytecode::RootBuildError,
    source: eitri_base::SourceId,
    span: Span,
) -> Diagnostic {
    Diagnostic::error(
        Code::Limit,
        format!("implementation limit: root metadata could not be constructed ({error:?})"),
    )
    .with_label(DiagLabel::primary(source, span))
}

/// Finish and verify root metadata before publishing it to a function. A
/// failed build or verification leaves the original metadata intact. The
/// present verifier supports scalar executable facts; managed fact derivation
/// remains a prerequisite for managed lowering.
pub fn install_verified_roots(
    function: &mut Function,
    builder: eitri_bytecode::RootBuilder,
    pc_map: &[(u32, Vec<eitri_bytecode::SafepointPc>)],
    callees: &eitri_bytecode::Callees<'_>,
    source: eitri_base::SourceId,
    span: Span,
) -> Result<(), Diagnostic> {
    let roots = builder
        .finish(pc_map)
        .map_err(|e| root_build_diagnostic(e, source, span))?;
    let old = std::mem::replace(&mut function.roots, roots);
    if let Err(error) = eitri_bytecode::verify(function, callees) {
        function.roots = old;
        return Err(
            Diagnostic::bug(format!("invalid generated root metadata: {error}"))
                .with_label(DiagLabel::primary(source, span)),
        );
    }
    Ok(())
}

type Lowered<T> = Result<T, AsmError>;

struct Lowerer<'a, 'i, const R: bool, const SIMPLE: bool> {
    input: &'a Input<'i>,
    checked: &'a Checked,
    asm: &'a mut Assembler<R>,
    slots: &'a mut IndexVec<BindingId, u32>,
    stack: &'a mut Vec<ExprId>,
    /// The next free slot.
    top: u32,
}

impl<const R: bool, const SIMPLE: bool> Lowerer<'_, '_, R, SIMPLE> {
    // ---- functions -------------------------------------------------------

    fn module(&mut self) -> Lowered<Function> {
        self.top = 0;
        let built = self.module_body();
        self.recover(built)
    }

    fn module_body(&mut self) -> Lowered<Function> {
        let ast = self.input.ast;
        for item in &ast.items {
            if let Item::Stmt(stmt) = *item {
                self.stmt(stmt)?;
            }
        }
        self.asm.ret(Span::default())?;
        self.asm.finish("<module>", Signature::default())
    }

    fn function(&mut self, id: FnId) -> Lowered<Function> {
        let built = self.function_body(id);
        self.recover(built)
    }

    fn function_body(&mut self, id: FnId) -> Lowered<Function> {
        let (ast, checked) = (self.input.ast, self.checked);
        let decl = ast.fns[id];
        let types = checked.signatures[id];
        let signature = signature(&checked.param_types[types.params], types.ret);
        let base = signature.param_base();
        for (index, binding) in checked.params[id].iter().enumerate() {
            self.slots[binding] = base + index as u32;
        }
        self.top = base + signature.params.len() as u32;
        let body = decl.body.expect("script functions have bodies");
        self.statements(body)?;
        if types.ret == Ty::Void {
            self.asm.ret(body.span.shrink_to_end())?;
        }
        let name = self
            .input
            .interner
            .resolve(self.input.tokens.symbol(decl.name));
        self.asm.finish(name, signature)
    }

    /// After a failure, empty the assembler so the next run starts clean.
    fn recover(&mut self, built: Lowered<Function>) -> Lowered<Function> {
        if built.is_err() {
            let _ = self.asm.finish("", Signature::default());
        }
        built
    }

    fn temp(&mut self) -> u32 {
        let slot = self.top;
        self.top += 1;
        slot
    }

    fn dst(&mut self, dst: Option<u32>) -> u32 {
        match dst {
            Some(slot) => slot,
            None => self.temp(),
        }
    }

    // ---- statements ------------------------------------------------------

    /// A block in its own scope. Statements after one that always returns
    /// cannot run, and are not emitted.
    fn statements(&mut self, block: Block) -> Lowered<()> {
        let ast = self.input.ast;
        let mark = self.top;
        for &stmt in ast.block(block) {
            self.stmt(stmt)?;
            if self.stmt_ends(stmt) {
                break;
            }
        }
        self.top = mark;
        Ok(())
    }

    /// Whether `stmt` always returns: `return`, or an `if` statement whose
    /// every branch, including an `else`, does.
    fn stmt_ends(&self, stmt: StmtId) -> bool {
        let ast = self.input.ast;
        match ast.stmts[stmt] {
            Stmt::Return { .. } => true,
            Stmt::Expr(e) => match ast.exprs[e] {
                Expr::If(id) => self.checked.types[e] == Ty::Void && self.if_ends(id),
                _ => false,
            },
            _ => false,
        }
    }

    fn ends(&self, block: Block) -> bool {
        self.input
            .ast
            .block(block)
            .iter()
            .any(|&stmt| self.stmt_ends(stmt))
    }

    fn if_ends(&self, mut id: IfId) -> bool {
        let ast = self.input.ast;
        loop {
            let IfExpr {
                then, otherwise, ..
            } = ast.ifs[id];
            if !self.ends(then) {
                return false;
            }
            match otherwise {
                Else::None => return false,
                Else::Block(block) => return self.ends(block),
                Else::If(e) => {
                    let Expr::If(next) = ast.exprs[e] else {
                        return false;
                    };
                    id = next;
                }
            }
        }
    }

    fn stmt(&mut self, id: StmtId) -> Lowered<()> {
        let (ast, checked) = (self.input.ast, self.checked);
        let mark = self.top;
        match ast.stmts[id] {
            Stmt::Let { init, .. } => {
                let binding =
                    checked.stmt_bindings[id].expect("a checked `let` declares a binding");
                // The new slot is distinct from any binding it shadows, so
                // `let x = x + 1` reads the old `x` while writing the new.
                let slot = self.temp();
                self.slots[binding] = slot;
                self.expr(init, Some(slot))?;
                self.top = slot + 1;
                return Ok(());
            }
            Stmt::Assign {
                op, value, span, ..
            } => {
                let binding = checked.stmt_bindings[id].expect("a checked assignment has a target");
                let slot = self.slots[binding];
                match op {
                    AssignOp::Set => {
                        self.expr(value, Some(slot))?;
                    }
                    AssignOp::Compound(op) => {
                        // A fault here points at the whole statement.
                        if let (Some(k), Some(imm_op)) = (
                            self.imm8(value)
                                .filter(|_| checked.bindings[binding].ty == Ty::I64),
                            imm_op(op),
                        ) {
                            self.asm.emit(
                                imm_op,
                                Operands::abc(slot.into(), slot.into(), k),
                                span,
                            )?;
                        } else {
                            let rhs = self.expr(value, None)?;
                            self.operate(op, checked.bindings[binding].ty, slot, slot, rhs, span)?;
                        }
                    }
                }
            }
            Stmt::Expr(e) => self.discard(e)?,
            Stmt::While {
                keyword,
                condition,
                body,
            } => {
                let top = self.asm.label();
                let exit = self.asm.label();
                self.asm.bind(top)?;
                self.branch(condition, false, exit)?;
                self.statements(body)?;
                // The back edge polls the budget; a fault there points at
                // `while`.
                self.asm.jump(top, self.input.tokens.span(keyword));
                self.asm.bind(exit)?;
            }
            Stmt::Return { keyword, value } => {
                if let Some(value) = value {
                    self.expr(value, Some(0))?;
                }
                self.asm.ret(self.input.tokens.span(keyword))?;
            }
        }
        self.top = mark;
        Ok(())
    }

    /// An expression statement: its value, if any, is dropped.
    fn discard(&mut self, e: ExprId) -> Lowered<()> {
        if let Expr::If(id) = self.input.ast.exprs[e]
            && self.checked.types[e] == Ty::Void
        {
            return self.if_stmt(id);
        }
        self.expr(e, None).map(drop)
    }

    /// An `if` statement, iterating along its `else if` chain.
    fn if_stmt(&mut self, id: IfId) -> Lowered<()> {
        let ast = self.input.ast;
        let end = self.asm.label();
        let mut current = id;
        loop {
            let IfExpr {
                condition,
                then,
                otherwise,
            } = ast.ifs[current];
            let next = self.asm.label();
            self.branch(condition, false, next)?;
            self.statements(then)?;
            if !matches!(otherwise, Else::None) && !self.ends(then) {
                self.asm.jump(end, then.span);
            }
            self.asm.bind(next)?;
            match otherwise {
                Else::None => break,
                Else::Block(block) => {
                    self.statements(block)?;
                    break;
                }
                Else::If(e) => {
                    let Expr::If(nested) = ast.exprs[e] else {
                        unreachable!("`else if` holds an `if`");
                    };
                    current = nested;
                }
            }
        }
        self.asm.bind(end)
    }

    // ---- conditions ------------------------------------------------------

    /// Jump to `target` when `e` evaluates to `when`; fall through otherwise.
    fn branch(&mut self, e: ExprId, when: bool, target: Label) -> Lowered<()> {
        let ast = self.input.ast;
        let span = ast.span(e);
        let mark = self.top;
        match ast.exprs[e] {
            Expr::Binary { op, lhs, rhs }
                if !SIMPLE
                    && compare(op).is_some()
                    && matches!(self.checked.types[lhs], Ty::I64 | Ty::Bool) =>
            {
                // Against a small literal: one `CBI`, no load (E5, C2).
                if let Some((x, cond, k)) = self.compare_imm(op, lhs, rhs) {
                    let x = self.expr(x, None)?;
                    let (cond, k) = if when {
                        (cond, k)
                    } else {
                        imm_complement(cond, k).expect("checked by compare_imm")
                    };
                    self.asm.branch_imm(cond, x, k, target, span)?;
                    self.top = mark;
                    return Ok(());
                }
                let a = self.expr(lhs, None)?;
                let b = self.expr(rhs, None)?;
                let (cond, swap) = compare(op).expect("matched above");
                let (a, b) = if swap { (b, a) } else { (a, b) };
                let (cond, a, b) = if when {
                    (cond, a, b)
                } else {
                    let (complement, swap) = cond
                        .complement()
                        .expect("register-form conditions have complements");
                    if swap {
                        (complement, b, a)
                    } else {
                        (complement, a, b)
                    }
                };
                self.asm.branch_if(cond, a, b, target, span)?;
            }
            Expr::Binary {
                op: BinaryOp::And | BinaryOp::Or,
                ..
            } => self.logic_branch(e, when, target)?,
            Expr::Unary {
                op: UnaryOp::Not,
                operand,
            } => self.branch(operand, !when, target)?,
            Expr::Bool(value) => {
                if value == when {
                    self.asm.jump(target, span);
                }
            }
            _ => {
                let value = self.expr(e, None)?;
                let test = if when { Test::True } else { Test::False };
                self.asm.branch_test(test, value, target, span);
            }
        }
        self.top = mark;
        Ok(())
    }

    /// `a && b && …` or `a || b || …`, short-circuiting, over the chain's
    /// operands in source order without recursion.
    fn logic_branch(&mut self, e: ExprId, when: bool, target: Label) -> Lowered<()> {
        let Expr::Binary { op, .. } = self.input.ast.exprs[e] else {
            unreachable!("called on `&&` or `||`");
        };
        let mark = self.stack.len();
        self.operands(e, op);
        let count = self.stack.len() - mark;
        // The value that decides the chain early: false for `&&`, true for `||`.
        let decides = op == BinaryOp::Or;
        let skip = self.asm.label();
        for index in 0..count {
            let operand = self.stack[mark + index];
            if when == decides || index + 1 == count {
                self.branch(operand, when, target)?;
            } else {
                self.branch(operand, decides, skip)?;
            }
        }
        self.asm.bind(skip)?;
        self.stack.truncate(mark);
        Ok(())
    }

    /// Push the operands of a homogeneous `op` chain rooted at `root`, in
    /// source order.
    fn operands(&mut self, root: ExprId, op: BinaryOp) {
        let ast = self.input.ast;
        let mark = self.stack.len();
        let mut node = root;
        while let Expr::Binary { op: o, lhs, rhs } = ast.exprs[node]
            && o == op
        {
            self.stack.push(rhs);
            node = lhs;
        }
        self.stack.push(node);
        self.stack[mark..].reverse();
    }

    // ---- values ----------------------------------------------------------

    /// Lower `e` into `dst` when given, or anywhere convenient; return the
    /// slot holding its value.
    fn expr(&mut self, e: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let (ast, checked) = (self.input.ast, self.checked);
        let span = ast.span(e);
        match ast.exprs[e] {
            Expr::Int { .. } | Expr::Float { .. } | Expr::Char(_) => {
                let slot = self.dst(dst);
                let value = checked.int_value(e).expect("checked literals have values");
                self.asm
                    .load(slot, value as u64, value_type(checked.types[e]), span)?;
                Ok(slot)
            }
            Expr::Bool(value) => {
                let slot = self.dst(dst);
                self.asm
                    .load(slot, u64::from(value), ValueType::Bool, span)?;
                Ok(slot)
            }
            Expr::Name(_) => {
                let Res::Binding(binding) = checked.resolutions[e] else {
                    unreachable!("checked value names are bindings");
                };
                let slot = self.slots[binding];
                match dst {
                    Some(d) if d != slot => {
                        self.asm
                            .emit(Op::Mov, Operands::abc(d.into(), slot.into(), 0), span)?;
                        Ok(d)
                    }
                    None if SIMPLE => {
                        let copy = self.temp();
                        self.asm
                            .emit(Op::Mov, Operands::abc(copy.into(), slot.into(), 0), span)?;
                        Ok(copy)
                    }
                    _ => Ok(slot),
                }
            }
            Expr::Unary {
                op: UnaryOp::Neg | UnaryOp::BitNot,
                operand,
            } => {
                let x = self.expr(operand, None)?;
                let slot = self.dst(dst);
                let ty = checked.types[e];
                let Expr::Unary { op, .. } = ast.exprs[e] else {
                    unreachable!()
                };
                let opcode = if op == UnaryOp::BitNot {
                    Op::Not
                } else if ty == Ty::F32 {
                    Op::FnegF32
                } else if ty == Ty::F64 {
                    Op::FnegF64
                } else if !ty.signed() {
                    Op::NegChkU
                } else if ty == Ty::I8 {
                    Op::NegWrap
                } else {
                    Op::NegChkS
                };
                self.asm
                    .emit(opcode, Operands::abc(slot.into(), x.into(), 0), span)?;
                if ty == Ty::I8 || ty == Ty::U8 {
                    let from = if ty.signed() { Ty::I64 } else { Ty::U64 };
                    self.convert(slot, slot, from, ty, op != UnaryOp::BitNot, span)?;
                }
                Ok(slot)
            }
            Expr::Unary {
                op: UnaryOp::Not,
                operand,
            } => {
                // `!b` is `b ^ true`; the 1 is typed `bool` for the verifier.
                let x = self.expr(operand, None)?;
                let one = self.temp();
                self.asm.load(one, 1, ValueType::Bool, span)?;
                let slot = self.dst(dst);
                self.asm.emit(
                    Op::Xor,
                    Operands::abc(slot.into(), x.into(), one.into()),
                    span,
                )?;
                Ok(slot)
            }
            Expr::Binary {
                op: BinaryOp::And | BinaryOp::Or,
                ..
            } => self.logic_value(e, dst),
            Expr::Binary { .. } => self.chain(e, dst),
            Expr::Call { args, .. } if matches!(checked.resolutions[e], Res::Convert { .. }) => {
                self.conversion(e, ast.args(args)[0], dst)
            }
            Expr::Convert { value, .. } => self.conversion(e, value, dst),
            Expr::Call { .. } => self.call(e, dst),
            Expr::If(id) => self.if_value(id, dst, span),
            Expr::Member { .. } | Expr::Error => {
                unreachable!("checked code has no bare members or errors")
            }
        }
    }

    /// `&&`/`||` as a value: branch, then write `true` or `false`.
    fn logic_value(&mut self, e: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let span = self.input.ast.span(e);
        let no = self.asm.label();
        let end = self.asm.label();
        self.branch(e, false, no)?;
        let slot = self.dst(dst);
        self.asm.load(slot, 1, ValueType::Bool, span)?;
        self.asm.jump(end, span);
        self.asm.bind(no)?;
        self.asm.load(slot, 0, ValueType::Bool, span)?;
        self.asm.bind(end)?;
        Ok(slot)
    }

    /// Arithmetic and comparisons along a left spine, iteratively: the
    /// left-most operand first, then each operator from the inside out,
    /// with one accumulator for intermediate results and each right
    /// operand's temporaries released after use.
    fn chain(&mut self, root: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let ast = self.input.ast;
        let mark = self.stack.len();
        let mut node = root;
        while let Expr::Binary { op, lhs, .. } = ast.exprs[node]
            && !matches!(op, BinaryOp::And | BinaryOp::Or)
        {
            self.stack.push(node);
            node = lhs;
        }
        let mut acc = self.expr(node, None)?;
        // Intermediate results never land in `dst`: it may be a binding a
        // later operand still reads.
        let accumulator = (self.stack.len() - mark > 1).then(|| self.temp());
        while self.stack.len() > mark {
            let n = self.stack.pop().expect("above the mark");
            let Expr::Binary { op, rhs, .. } = ast.exprs[n] else {
                unreachable!("only binary nodes are pushed");
            };
            // A small literal right operand becomes an immediate (E5, C2).
            let Expr::Binary { lhs, .. } = ast.exprs[n] else {
                unreachable!()
            };
            let operand_ty = self.checked.types[lhs];
            let imm = self
                .imm8(rhs)
                .filter(|_| operand_ty == Ty::I64)
                .zip(imm_op(op));
            let save = self.top;
            let rhs = match imm {
                Some(_) => 0,
                None => self.expr(rhs, None)?,
            };
            // The right operand is read by this one instruction, so its
            // temporaries are free again before the result is placed.
            self.top = save;
            let out = if n == root {
                self.dst(dst)
            } else {
                accumulator.expect("intermediate results need an accumulator")
            };
            match imm {
                Some((k, imm_op)) => {
                    let operands = Operands::abc(out.into(), acc.into(), k);
                    self.asm.emit(imm_op, operands, ast.span(n))?;
                }
                None => self.operate(op, operand_ty, out, acc, rhs, ast.span(n))?,
            }
            acc = out;
        }
        Ok(acc)
    }

    /// An integer literal that fits an eight-bit immediate.
    fn imm8(&self, e: ExprId) -> Option<i64> {
        if SIMPLE {
            return None;
        }
        match self.input.ast.exprs[e] {
            Expr::Int { .. } => self
                .checked
                .int_value(e)
                .filter(|k| (i8::MIN as i64..=i8::MAX as i64).contains(k)),
            _ => None,
        }
    }

    /// A comparison with a small literal on either side as a `CBI`
    /// condition on the other operand, when the condition and its
    /// complement both fit (`x <= k` is `x < k + 1`). The literal has no
    /// effects, so evaluating only the other operand keeps source order.
    fn compare_imm(&self, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Option<(ExprId, Cond, i64)> {
        let (x, k, op) = match self.imm8(rhs) {
            Some(k) => (lhs, k, op),
            None => {
                let mirrored = match op {
                    BinaryOp::Lt => BinaryOp::Gt,
                    BinaryOp::LtEq => BinaryOp::GtEq,
                    BinaryOp::Gt => BinaryOp::Lt,
                    BinaryOp::GtEq => BinaryOp::LtEq,
                    other => other,
                };
                (rhs, self.imm8(lhs)?, mirrored)
            }
        };
        let (cond, k) = match op {
            BinaryOp::Eq => (Cond::Eq, k),
            BinaryOp::NotEq => (Cond::Ne, k),
            BinaryOp::Lt => (Cond::Lt, k),
            BinaryOp::LtEq => (Cond::Lt, k + 1),
            BinaryOp::Gt => (Cond::Gt, k),
            BinaryOp::GtEq => (Cond::Gt, k - 1),
            _ => return None,
        };
        imm_complement(cond, k)?;
        Some((x, cond, k))
    }

    fn conversion(&mut self, e: ExprId, value: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let Res::Convert { from, to, checked } = self.checked.resolutions[e] else {
            unreachable!()
        };
        let source = self.expr(value, None)?;
        let out = self.dst(dst);
        self.convert(out, source, from, to, checked, self.input.ast.span(e))?;
        Ok(out)
    }

    fn convert(
        &mut self,
        out: u32,
        source: u32,
        from: Ty,
        to: Ty,
        checked: bool,
        span: Span,
    ) -> Lowered<()> {
        use eitri_bytecode::scalar::integer_format;
        if from == to {
            if out != source {
                self.asm
                    .emit(Op::Mov, Operands::abc(out.into(), source.into(), 0), span)?;
            }
            return Ok(());
        }
        let (op, format) = if from.integer() && to.integer() {
            (
                if checked { Op::CvtChk } else { Op::CvtWrap },
                integer_format(to.bits(), to.signed()) | (u8::from(from.signed()) << 3),
            )
        } else if from.integer() {
            (
                if to == Ty::F32 {
                    Op::FcvtI2f32
                } else {
                    Op::FcvtI2f64
                },
                integer_format(from.bits(), from.signed()),
            )
        } else if to.integer() {
            (
                if from == Ty::F32 {
                    Op::FcvtF32i
                } else {
                    Op::FcvtF64i
                },
                integer_format(to.bits(), to.signed()),
            )
        } else {
            (
                if to == Ty::F32 {
                    Op::FcvtF64f32
                } else {
                    Op::FcvtF32f64
                },
                0,
            )
        };
        self.asm.emit(
            op,
            Operands::abc(out.into(), source.into(), format.into()),
            span,
        )
    }

    fn operate(
        &mut self,
        op: BinaryOp,
        ty: Ty,
        out: u32,
        a: u32,
        b: u32,
        span: Span,
    ) -> Lowered<()> {
        use BinaryOp::*;
        let (opcode, a, b) = if ty.float() {
            let f32 = ty == Ty::F32;
            let (opcode, swap) = match op {
                Add => (if f32 { Op::FaddF32 } else { Op::FaddF64 }, false),
                Sub => (if f32 { Op::FsubF32 } else { Op::FsubF64 }, false),
                Mul => (if f32 { Op::FmulF32 } else { Op::FmulF64 }, false),
                Div => (if f32 { Op::FdivF32 } else { Op::FdivF64 }, false),
                Rem => (if f32 { Op::FremF32 } else { Op::FremF64 }, false),
                Eq => (
                    if f32 {
                        Op::FcsetFeqF32
                    } else {
                        Op::FcsetFeqF64
                    },
                    false,
                ),
                NotEq => (
                    if f32 {
                        Op::FcsetFneF32
                    } else {
                        Op::FcsetFneF64
                    },
                    false,
                ),
                Lt | Gt => (
                    if f32 {
                        Op::FcsetFltF32
                    } else {
                        Op::FcsetFltF64
                    },
                    op == Gt,
                ),
                LtEq | GtEq => (
                    if f32 {
                        Op::FcsetFleF32
                    } else {
                        Op::FcsetFleF64
                    },
                    op == GtEq,
                ),
                _ => unreachable!(),
            };
            (opcode, if swap { b } else { a }, if swap { a } else { b })
        } else {
            let narrow = ty == Ty::I8 || ty == Ty::U8;
            let unsigned = ty == Ty::U64 || ty == Ty::U8 || ty == Ty::Char;
            let opcode = match op {
                Add => {
                    if narrow {
                        Op::AddWrap
                    } else if unsigned {
                        Op::AddChkU
                    } else {
                        Op::AddChkS
                    }
                }
                Sub => {
                    if narrow {
                        Op::SubWrap
                    } else if unsigned {
                        Op::SubChkU
                    } else {
                        Op::SubChkS
                    }
                }
                Mul => {
                    if narrow {
                        Op::MulWrap
                    } else if unsigned {
                        Op::MulChkU
                    } else {
                        Op::MulChkS
                    }
                }
                WrapAdd => Op::AddWrap,
                WrapSub => Op::SubWrap,
                WrapMul => Op::MulWrap,
                Div => {
                    if unsigned {
                        Op::DivU
                    } else {
                        Op::DivS
                    }
                }
                Rem => {
                    if unsigned {
                        Op::RemU
                    } else {
                        Op::RemS
                    }
                }
                BitAnd => Op::And,
                BitOr => Op::Or,
                BitXor => Op::Xor,
                Shl => match ty {
                    Ty::I8 => Op::ShlS8,
                    Ty::U8 => Op::ShlU8,
                    _ => Op::Shl64,
                },
                Shr => match ty {
                    Ty::I8 => Op::ShrS8,
                    Ty::U8 => Op::ShrU8,
                    Ty::U64 => Op::ShrU64,
                    _ => Op::ShrS64,
                },
                Eq => Op::CsetEq,
                NotEq => Op::CsetNe,
                Lt | Gt => {
                    if unsigned {
                        Op::CsetLo
                    } else {
                        Op::CsetLt
                    }
                }
                LtEq | GtEq => {
                    if unsigned {
                        Op::CsetLs
                    } else {
                        Op::CsetLe
                    }
                }
                And | Or => unreachable!(),
            };
            let swap = matches!(op, Gt | GtEq);
            (opcode, if swap { b } else { a }, if swap { a } else { b })
        };
        self.asm
            .emit(opcode, Operands::abc(out.into(), a.into(), b.into()), span)?;
        if matches!(
            op,
            Add | Sub | Mul | Div | Rem | WrapAdd | WrapSub | WrapMul
        ) && matches!(ty, Ty::I8 | Ty::U8 | Ty::U64)
        {
            let from = if (matches!(op, Div | Rem) && !ty.signed())
                || (ty == Ty::U64 && matches!(op, Add | Sub | Mul))
            {
                Ty::U64
            } else {
                Ty::I64
            };
            self.convert(
                out,
                out,
                from,
                ty,
                !matches!(op, WrapAdd | WrapSub | WrapMul),
                span,
            )?;
        }
        Ok(())
    }

    /// Arguments in order into a fresh window; the result after it, so a
    /// host's argument and result windows never overlap.
    fn call(&mut self, e: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let (ast, checked) = (self.input.ast, self.checked);
        let Expr::Call { args, .. } = ast.exprs[e] else {
            unreachable!("called on a call");
        };
        let (target, returns) = match checked.resolutions[e] {
            Res::Function(f) => (
                CallTarget::Script(FunctionId::from_u32(f.as_u32() + 1)),
                checked.signatures[f].ret != Ty::Void,
            ),
            Res::Host(h) => (
                CallTarget::Host(h.as_u32()),
                self.input.hosts.function(h).ret != Ty::Void,
            ),
            _ => unreachable!("checked calls resolve to functions"),
        };
        let args = ast.args(args);
        let first = self.top;
        self.top += args.len() as u32;
        for (index, &arg) in args.iter().enumerate() {
            let save = self.top;
            self.expr(arg, Some(first + index as u32))?;
            self.top = save;
        }
        let result = if returns {
            SlotRange {
                first: self.dst(dst),
                count: 1,
            }
        } else {
            SlotRange { first: 0, count: 0 }
        };
        let site = CallSite {
            target,
            args: SlotRange {
                first,
                count: args.len() as u32,
            },
            result,
        };
        self.asm.call(site, ast.span(e))?;
        Ok(result.first)
    }

    /// An `if` used as a value: every branch writes its final expression
    /// into the same slot. Iterates along the `else if` chain.
    fn if_value(&mut self, id: IfId, dst: Option<u32>, span: Span) -> Lowered<u32> {
        let ast = self.input.ast;
        let slot = self.dst(dst);
        let end = self.asm.label();
        let mut current = id;
        loop {
            let IfExpr {
                condition,
                then,
                otherwise,
            } = ast.ifs[current];
            let next = self.asm.label();
            self.branch(condition, false, next)?;
            self.block_value(then, slot)?;
            self.asm.jump(end, span);
            self.asm.bind(next)?;
            match otherwise {
                Else::Block(block) => {
                    self.block_value(block, slot)?;
                    break;
                }
                Else::If(e) => {
                    let Expr::If(nested) = ast.exprs[e] else {
                        unreachable!("`else if` holds an `if`");
                    };
                    current = nested;
                }
                Else::None => unreachable!("the checker requires an `else` on a value `if`"),
            }
        }
        self.asm.bind(end)?;
        Ok(slot)
    }

    /// A block used as a value: its statements, then its final expression
    /// into `slot`.
    fn block_value(&mut self, block: Block, slot: u32) -> Lowered<()> {
        let ast = self.input.ast;
        let mark = self.top;
        let stmts = ast.block(block);
        let (&last, rest) = stmts
            .split_last()
            .expect("the checker requires a value block to end in an expression");
        for &stmt in rest {
            self.stmt(stmt)?;
        }
        let Stmt::Expr(value) = ast.stmts[last] else {
            unreachable!("the checker requires a value block to end in an expression");
        };
        self.expr(value, Some(slot))?;
        self.top = mark;
        Ok(())
    }
}

/// The `ALUI` form of an arithmetic operator, if it has one.
fn imm_op(op: BinaryOp) -> Option<Op> {
    Some(match op {
        BinaryOp::Add => Op::AddiChkS,
        BinaryOp::Sub => Op::SubiChkS,
        BinaryOp::Mul => Op::MuliChkS,
        BinaryOp::WrapAdd => Op::AddiWrap,
        BinaryOp::WrapSub => Op::SubiWrap,
        _ => return None,
    })
}

/// The register-form condition for a comparison, and whether its operands
/// swap (`a > b` is `b < a`).
fn compare(op: BinaryOp) -> Option<(Cond, bool)> {
    Some(match op {
        BinaryOp::Eq => (Cond::Eq, false),
        BinaryOp::NotEq => (Cond::Ne, false),
        BinaryOp::Lt => (Cond::Lt, false),
        BinaryOp::LtEq => (Cond::Le, false),
        BinaryOp::Gt => (Cond::Lt, true),
        BinaryOp::GtEq => (Cond::Le, true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod synthetic_roots;
