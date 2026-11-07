//! The program-level semantic oracle (T009, T045): a tree-walking evaluator
//! over the checked syntax tree, written from the first-slice chapter
//! rather than from lowering or the interpreters, so the fixture corpus can
//! compare the two.
//!
//! It shares the frontend (lexing, parsing, checking) with the compiler and
//! nothing after it: no slots, no bytecode, no handlers. Integer arithmetic
//! and comparisons come from the T004 operation oracle
//! (`eitri_bytecode::oracle`), which computes exact `i128` results instead
//! of using the handlers' `checked_*`/`wrapping_*`. Budget debits and the
//! depth limit follow the chapter's rules ("Limits"), so a lowering that
//! polls in the wrong place disagrees with it.
//!
//! Correctness only: script recursion recurses in Rust, so callers run it
//! on a thread with a large stack ([`STACK`]).

use eitri_base::{Idx, Interner, SourceId, Span};
use eitri_bytecode::oracle::{self, Arith, IntType};
use eitri_bytecode::spec::Cond;
use eitri_check::{BindingId, CheckScratch, Checked, HostFnId, Hosts, Input, Res, Ty};
use eitri_diag::Diagnostics;
use eitri_syntax::ast::{
    AssignOp, Ast, BinaryOp, Block, Else, Expr, ExprId, FnId, Item, Stmt, UnaryOp,
};
use eitri_syntax::{LexScratch, ModuleKind, ParseScratch, Tokens};

/// Stack for a thread running the oracle: enough for the default depth
/// limit of 4,096 script frames, each a handful of Rust frames.
pub const STACK: usize = 256 << 20;

const SOURCE: SourceId = SourceId::from_u32(0);

/// A fault: its message and trace, innermost first. Entries are a function
/// name and the faulting operation or active call; a host function has no
/// span.
#[derive(Debug, PartialEq, Eq)]
pub struct Fault {
    pub message: String,
    pub trace: Vec<(String, Option<Span>)>,
}

/// The limits of one run, as the context applies them.
#[derive(Clone, Copy)]
pub struct Limits {
    pub budget: u64,
    pub depth: u32,
}

/// The oracle's own context: a host registry built the same way as the
/// compiler's, so host function indices agree with `Context::host_index`.
pub struct Oracle {
    interner: Interner,
    hosts: Hosts,
}

impl Oracle {
    pub fn new() -> Self {
        Self {
            interner: Interner::new(),
            hosts: Hosts::new(),
        }
    }

    pub fn register_host_module(&mut self, path: &str, declarations: &str) {
        self.hosts
            .register(path, declarations, &mut self.interner)
            .expect("host declarations check");
    }

    pub fn host_index(&self, path: &str, name: &str) -> Option<u32> {
        let module = self.hosts.find(path)?;
        self.hosts
            .module(module)
            .fns
            .iter()
            .copied()
            .find(|&f| self.interner.resolve(self.hosts.function(f).name) == name)
            .map(HostFnId::as_u32)
    }

    /// Evaluate `source`. `None` if the frontend rejects it: the oracle
    /// says nothing about rejected programs.
    pub fn run(
        &mut self,
        source: &str,
        limits: Limits,
        host: &mut dyn eitri::Host,
    ) -> Option<Result<(), Fault>> {
        self.interner.reset();
        let mut diagnostics = Diagnostics::new(1);
        let mut lex = LexScratch::default();
        let mut parse = ParseScratch::default();
        let mut check = CheckScratch::default();
        let tokens = eitri_syntax::lex(
            source,
            SOURCE,
            &mut self.interner,
            &mut diagnostics,
            &mut lex,
        );
        if diagnostics.has_errors() {
            return None;
        }
        let ast = eitri_syntax::parse(
            source,
            SOURCE,
            tokens,
            ModuleKind::Script,
            &mut diagnostics,
            &mut parse,
        );
        if diagnostics.has_errors() {
            return None;
        }
        let input = Input {
            text: source,
            source: SOURCE,
            tokens,
            ast,
            interner: &self.interner,
            hosts: &self.hosts,
        };
        let checked = eitri_check::check(&input, &mut diagnostics, &mut check);
        if diagnostics.has_errors() {
            return None;
        }
        let mut eval = Eval {
            tokens,
            ast,
            checked,
            interner: &self.interner,
            hosts: &self.hosts,
            host,
            budget: limits.budget,
            depth: limits.depth,
            current: None,
            calls: Vec::new(),
            locals: vec![None; checked.bindings.len()],
        };
        let mut result = Ok(());
        for &item in &ast.items {
            let Item::Stmt(stmt) = item else { continue };
            if let Err(unwind) = eval.stmt(stmt) {
                result = Err(match unwind {
                    Unwind::Fault(fault) => fault,
                    Unwind::Return(_) => unreachable!("the checker rejects a top-level return"),
                });
                break;
            }
        }
        Some(result)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Value {
    Int(i64),
    Bool(bool),
    Void,
}

impl Value {
    fn int(self) -> i64 {
        match self {
            Value::Int(value) => value,
            other => unreachable!("checked as `i64`, found {other:?}"),
        }
    }

    fn bool(self) -> bool {
        match self {
            Value::Bool(value) => value,
            other => unreachable!("checked as `bool`, found {other:?}"),
        }
    }

    /// The value as a host argument word.
    fn word(self) -> u64 {
        match self {
            Value::Int(value) => value as u64,
            Value::Bool(value) => u64::from(value),
            Value::Void => unreachable!("the checker rejects `void` arguments"),
        }
    }
}

/// Leaving evaluation early: a `return`, or a fault on its way out.
enum Unwind {
    Return(Value),
    Fault(Fault),
}

type Flow<T> = Result<T, Unwind>;

struct Eval<'a> {
    tokens: &'a Tokens,
    ast: &'a Ast,
    checked: &'a Checked,
    interner: &'a Interner,
    hosts: &'a Hosts,
    host: &'a mut dyn eitri::Host,
    budget: u64,
    depth: u32,
    /// The function being evaluated; `None` for top-level code.
    current: Option<FnId>,
    /// Active script calls, outermost first: the caller and the call's span.
    calls: Vec<(Option<FnId>, Span)>,
    /// The current frame's bindings. A call saves and replaces it.
    locals: Vec<Option<Value>>,
}

impl Eval<'_> {
    fn name(&self, function: Option<FnId>) -> String {
        match function {
            None => "<module>".to_owned(),
            Some(f) => {
                let token = self.ast.fns[f].name;
                self.interner.resolve(self.tokens.symbol(token)).to_owned()
            }
        }
    }

    /// A fault raised at `span` in the current function, with the trace of
    /// every active call.
    fn fault(&self, message: impl Into<String>, span: Span) -> Unwind {
        let mut trace = vec![(self.name(self.current), Some(span))];
        for &(caller, call) in self.calls.iter().rev() {
            trace.push((self.name(caller), Some(call)));
        }
        Unwind::Fault(Fault {
            message: message.into(),
            trace,
        })
    }

    /// One step of the budget, debited at `span`.
    fn debit(&mut self, span: Span) -> Flow<()> {
        if self.budget == 0 {
            return Err(self.fault("execution budget exhausted", span));
        }
        self.budget -= 1;
        Ok(())
    }

    fn arith(&self, op: Arith, a: i64, b: i64, span: Span) -> Flow<Value> {
        match oracle::arith(IntType::I64, op, a.into(), b.into()) {
            Ok(value) => Ok(Value::Int(i64::try_from(value).expect("in range"))),
            Err(fault) => Err(self.fault(message(fault), span)),
        }
    }

    fn block(&mut self, block: Block) -> Flow<Value> {
        let mut value = Value::Void;
        for &stmt in self.ast.block(block) {
            value = self.stmt(stmt)?;
        }
        Ok(value)
    }

    /// Execute a statement; an expression statement yields its value, so a
    /// block's value is that of its final statement.
    fn stmt(&mut self, id: eitri_syntax::ast::StmtId) -> Flow<Value> {
        match self.ast.stmts[id] {
            Stmt::Let { init, .. } => {
                let value = self.expr(init)?;
                let binding = self.checked.stmt_bindings[id].expect("checked `let`");
                self.locals[binding.index()] = Some(value);
            }
            Stmt::Assign {
                op, value, span, ..
            } => {
                let binding = self.checked.stmt_bindings[id].expect("checked assignment");
                let rhs = self.expr(value)?;
                let new = match op {
                    AssignOp::Set => rhs,
                    AssignOp::Compound(op) => {
                        let lhs = self.read(binding);
                        self.binary_values(op, lhs, rhs, span)?
                    }
                };
                self.locals[binding.index()] = Some(new);
            }
            Stmt::Expr(expr) => return self.expr(expr),
            Stmt::While {
                keyword,
                condition,
                body,
            } => {
                while self.expr(condition)?.bool() {
                    self.block(body)?;
                    // After the body, before the condition again.
                    self.debit(self.tokens.span(keyword))?;
                }
            }
            Stmt::Return { value, .. } => {
                let value = match value {
                    Some(value) => self.expr(value)?,
                    None => Value::Void,
                };
                return Err(Unwind::Return(value));
            }
        }
        Ok(Value::Void)
    }

    fn read(&self, binding: BindingId) -> Value {
        self.locals[binding.index()].expect("the checker rejects use before initialization")
    }

    fn expr(&mut self, e: ExprId) -> Flow<Value> {
        let span = self.ast.span(e);
        match self.ast.exprs[e] {
            Expr::Int { .. } => Ok(Value::Int(
                self.checked.int_value(e).expect("checked literal fits"),
            )),
            Expr::Bool(value) => Ok(Value::Bool(value)),
            Expr::Name(_) => match self.checked.resolutions[e] {
                Res::Binding(binding) => Ok(self.read(binding)),
                other => unreachable!("a name used as a value resolves to a binding: {other:?}"),
            },
            Expr::Unary { op, operand } => {
                let value = self.expr(operand)?;
                match op {
                    UnaryOp::Not => Ok(Value::Bool(!value.bool())),
                    UnaryOp::Neg => match oracle::neg(IntType::I64, value.int().into()) {
                        Ok(value) => Ok(Value::Int(i64::try_from(value).expect("in range"))),
                        Err(fault) => Err(self.fault(message(fault), span)),
                    },
                }
            }
            Expr::Binary { op, lhs, rhs } => match op {
                BinaryOp::And => Ok(Value::Bool(
                    self.expr(lhs)?.bool() && self.expr(rhs)?.bool(),
                )),
                BinaryOp::Or => Ok(Value::Bool(
                    self.expr(lhs)?.bool() || self.expr(rhs)?.bool(),
                )),
                _ => {
                    let a = self.expr(lhs)?;
                    let b = self.expr(rhs)?;
                    self.binary_values(op, a, b, span)
                }
            },
            Expr::Call { args, .. } => {
                let mut values = Vec::with_capacity(self.ast.args(args).len());
                for &arg in self.ast.args(args) {
                    values.push(self.expr(arg)?);
                }
                // The callee starts only after the debit.
                self.debit(span)?;
                match self.checked.resolutions[e] {
                    Res::Function(f) => self.call_script(f, &values, span),
                    Res::Host(h) => self.call_host(h, &values, span),
                    other => unreachable!("a checked call resolves to a function: {other:?}"),
                }
            }
            Expr::If(mut id) => loop {
                let node = self.ast.ifs[id];
                if self.expr(node.condition)?.bool() {
                    break self.block(node.then);
                }
                match node.otherwise {
                    Else::None => break Ok(Value::Void),
                    Else::Block(block) => break self.block(block),
                    Else::If(nested) => match self.ast.exprs[nested] {
                        Expr::If(next) => id = next,
                        _ => break self.expr(nested),
                    },
                }
            },
            Expr::Member { .. } | Expr::Error => {
                unreachable!("not a value in checked code: {:?}", self.ast.exprs[e])
            }
        }
    }

    fn binary_values(&self, op: BinaryOp, a: Value, b: Value, span: Span) -> Flow<Value> {
        let cond = |cond, x: i64, y: i64| Ok(Value::Bool(oracle::int_cond(cond, x, y)));
        let arith = match op {
            // Equality applies to both types; the checker made them agree.
            BinaryOp::Eq => return Ok(Value::Bool(a == b)),
            BinaryOp::NotEq => return Ok(Value::Bool(a != b)),
            BinaryOp::Lt => return cond(Cond::Lt, a.int(), b.int()),
            BinaryOp::LtEq => return cond(Cond::Le, a.int(), b.int()),
            BinaryOp::Gt => return cond(Cond::Gt, a.int(), b.int()),
            BinaryOp::GtEq => return cond(Cond::Le, b.int(), a.int()),
            BinaryOp::Add => Arith::Add,
            BinaryOp::Sub => Arith::Sub,
            BinaryOp::Mul => Arith::Mul,
            BinaryOp::Div => Arith::Div,
            BinaryOp::Rem => Arith::Rem,
            BinaryOp::WrapAdd => Arith::WrapAdd,
            BinaryOp::WrapSub => Arith::WrapSub,
            BinaryOp::WrapMul => Arith::WrapMul,
            BinaryOp::And | BinaryOp::Or => unreachable!("short-circuit operators"),
        };
        self.arith(arith, a.int(), b.int(), span)
    }

    fn call_script(&mut self, f: FnId, args: &[Value], span: Span) -> Flow<Value> {
        // Top-level code is not a counted frame.
        if self.calls.len() as u32 >= self.depth {
            return Err(self.fault("call depth exceeded", span));
        }
        let mut locals = vec![None; self.checked.bindings.len()];
        for (binding, &value) in self.checked.params[f].iter().zip(args) {
            locals[binding.index()] = Some(value);
        }
        let saved = std::mem::replace(&mut self.locals, locals);
        self.calls.push((self.current, span));
        let caller = self.current.replace(f);
        let body = self.ast.fns[f].body.expect("script functions have bodies");
        let result = match self.block(body) {
            // Falling off the end is only accepted for `void`.
            Ok(_) => Ok(Value::Void),
            Err(Unwind::Return(value)) => Ok(value),
            Err(fault) => Err(fault),
        };
        self.current = caller;
        self.calls.pop();
        self.locals = saved;
        result
    }

    fn call_host(&mut self, h: HostFnId, args: &[Value], span: Span) -> Flow<Value> {
        let words: Vec<u64> = args.iter().map(|value| value.word()).collect();
        let function = self.hosts.function(h);
        let mut results = vec![0u64; usize::from(function.ret != Ty::Void)];
        if let Err(message) = self.host.call(h.as_u32(), &words, &mut results) {
            let Unwind::Fault(mut fault) = self.fault(message, span) else {
                unreachable!()
            };
            let module = &self.hosts.module(function.module).path;
            let name = format!("{module}.{}", self.interner.resolve(function.name));
            fault.trace.insert(0, (name, None));
            return Err(Unwind::Fault(fault));
        }
        Ok(match function.ret {
            Ty::I64 => Value::Int(results[0] as i64),
            Ty::Bool => Value::Bool(results[0] != 0),
            _ => Value::Void,
        })
    }
}

/// The chapter's fault messages ("Arithmetic").
fn message(fault: oracle::Fault) -> &'static str {
    match fault {
        oracle::Fault::Overflow => "integer overflow",
        oracle::Fault::DivisionByZero => "division by zero",
        oracle::Fault::ShiftAmount => unreachable!("no shifts in the first slice"),
    }
}
