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
//! Time and memory are linear in the module's syntax.

use eitri_base::{Idx, IndexVec, Span};
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
}

impl Program {
    /// Every function's signature, for the verifier.
    pub fn signatures(&self) -> IndexVec<FunctionId, Signature> {
        IndexVec::from_raw(self.functions.iter().map(|f| f.signature.clone()).collect())
    }
}

/// Lowering's reusable working memory.
#[derive(Default)]
pub struct LowerScratch {
    asm: Assembler,
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
pub fn lower(
    input: &Input<'_>,
    checked: &Checked,
    scratch: &mut LowerScratch,
) -> Result<Program, Diagnostic> {
    let ast = input.ast;
    scratch.slots.clear();
    scratch.slots.resize(checked.bindings.len(), u32::MAX);
    scratch.stack.clear();
    let mut lowerer = Lowerer {
        input,
        checked,
        asm: &mut scratch.asm,
        slots: &mut scratch.slots,
        stack: &mut scratch.stack,
        top: 0,
    };
    let mut functions = IndexVec::with_capacity(ast.fns.len() + 1);
    let module = lowerer
        .module()
        .map_err(|_| too_large(input, "<module>", Span::default()))?;
    functions.push(module);
    for (id, decl) in ast.fns.iter_enumerated() {
        let function = lowerer.function(id).map_err(|_| {
            let name = input.interner.resolve(input.tokens.symbol(decl.name));
            too_large(input, name, input.tokens.span(decl.name))
        })?;
        functions.push(function);
    }
    Ok(Program {
        functions,
        entry: ENTRY,
    })
}

fn too_large(input: &Input<'_>, name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::Limit,
        format!("implementation limit: `{name}` needs more than 65,536 slots or too much code"),
    )
    .with_label(DiagLabel::primary(input.source, span))
}

type Lowered<T> = Result<T, AsmError>;

struct Lowerer<'a, 'i> {
    input: &'a Input<'i>,
    checked: &'a Checked,
    asm: &'a mut Assembler,
    slots: &'a mut IndexVec<BindingId, u32>,
    stack: &'a mut Vec<ExprId>,
    /// The next free slot.
    top: u32,
}

impl Lowerer<'_, '_> {
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
                        if let (Some(k), Some(imm_op)) = (self.imm8(value), imm_op(op)) {
                            self.asm.emit(
                                imm_op,
                                Operands::abc(slot.into(), slot.into(), k),
                                span,
                            )?;
                        } else {
                            let rhs = self.expr(value, None)?;
                            self.operate(op, slot, slot, rhs, span)?;
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
            Expr::Binary { op, lhs, rhs } if compare(op).is_some() => {
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
            Expr::Int { .. } => {
                let slot = self.dst(dst);
                let value = checked.int_value(e).expect("checked literals have values");
                self.asm.load(slot, value as u64, ValueType::I64, span)?;
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
                    _ => Ok(slot),
                }
            }
            Expr::Unary {
                op: UnaryOp::Neg,
                operand,
            } => {
                let x = self.expr(operand, None)?;
                let slot = self.dst(dst);
                self.asm
                    .emit(Op::NegChkS, Operands::abc(slot.into(), x.into(), 0), span)?;
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
            let imm = self.imm8(rhs).zip(imm_op(op));
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
                None => self.operate(op, out, acc, rhs, ast.span(n))?,
            }
            acc = out;
        }
        Ok(acc)
    }

    /// An integer literal that fits an eight-bit immediate.
    fn imm8(&self, e: ExprId) -> Option<i64> {
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

    fn operate(&mut self, op: BinaryOp, out: u32, a: u32, b: u32, span: Span) -> Lowered<()> {
        let (op, a, b) = match op {
            BinaryOp::Add => (Op::AddChkS, a, b),
            BinaryOp::Sub => (Op::SubChkS, a, b),
            BinaryOp::Mul => (Op::MulChkS, a, b),
            BinaryOp::WrapAdd => (Op::AddWrap, a, b),
            BinaryOp::WrapSub => (Op::SubWrap, a, b),
            BinaryOp::WrapMul => (Op::MulWrap, a, b),
            BinaryOp::Div => (Op::DivS, a, b),
            BinaryOp::Rem => (Op::RemS, a, b),
            BinaryOp::Eq => (Op::CsetEq, a, b),
            BinaryOp::NotEq => (Op::CsetNe, a, b),
            BinaryOp::Lt => (Op::CsetLt, a, b),
            BinaryOp::LtEq => (Op::CsetLe, a, b),
            BinaryOp::Gt => (Op::CsetLt, b, a),
            BinaryOp::GtEq => (Op::CsetLe, b, a),
            BinaryOp::And | BinaryOp::Or => unreachable!("lowered by branching"),
        };
        self.asm
            .emit(op, Operands::abc(out.into(), a.into(), b.into()), span)
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
