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

/// The oracle's host interface: arguments and results as words, as the
/// interpreters pass them, and reentry by function name.
pub trait Host {
    fn call(
        &mut self,
        cx: &mut dyn Reenter,
        index: u32,
        args: &[u64],
        results: &mut [u64],
        string_args: &[Option<String>],
        string_result: &mut Option<String>,
    ) -> Result<(), HostError>;
}

/// Reentry from a host function into the running program.
pub trait Reenter {
    /// Whether the program has a script function `name`.
    fn has_function(&self, name: &str) -> bool;

    /// Call the named string-to-string callback using the oracle's own values.
    fn call_text(&mut self, host: &mut dyn Host, name: &str, arg: &str) -> Result<String, Fault>;

    /// Run script function `name`, which must exist. A fault stops only
    /// the nested run; its trace ends at the nested entry.
    fn call(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        args: &[u64],
        results: &mut [u64],
    ) -> Result<(), Fault>;
}

/// Why a host function failed: its own message, or a nested fault it
/// propagates.
pub enum HostError {
    Message(String),
    Fault(Fault),
}

/// Nested runs active at once (the first-slice chapter, "Limits").
const MAX_NESTING: u32 = 64;

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
        host: &mut dyn Host,
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
            budget: limits.budget,
            depth: limits.depth,
            current: None,
            calls: Vec::new(),
            below: 0,
            nesting: 0,
            locals: vec![None; checked.bindings.len()],
            strings: checked.strings.clone(),
        };
        let mut result = Ok(());
        for &item in &ast.items {
            let Item::Stmt(stmt) = item else { continue };
            if let Err(unwind) = eval.stmt(host, stmt) {
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
    Scalar(Ty, u64),
    Bool(bool),
    String(u32),
    Void,
}

fn format_value(value: Value) -> String {
    let mut text = match value.ty() {
        Ty::I64 => format!("{}", value.word() as i64),
        Ty::I8 => format!("{}", value.word() as i8),
        Ty::U8 => format!("{}", value.word() as u8),
        Ty::U64 => format!("{}", value.word()),
        Ty::F32 => format!("{:?}", f32::from_bits(value.word() as u32)),
        Ty::F64 => format!("{:?}", f64::from_bits(value.word())),
        Ty::Bool => format!("{}", value.bool()),
        Ty::Char => format!("{}", char::from_u32(value.word() as u32).unwrap()),
        _ => unreachable!(),
    };
    if matches!(value.ty(), Ty::F32 | Ty::F64)
        && let Some(at) = text.find('e')
        && text.as_bytes().get(at + 1).is_some_and(u8::is_ascii_digit)
    {
        text.insert(at + 1, '+');
    }
    text
}

impl Value {
    fn from_word(ty: Ty, word: u64) -> Self {
        match ty {
            Ty::I64 => Self::Int(word as i64),
            Ty::Bool => Self::Bool(word != 0),
            Ty::Void => Self::Void,
            _ => Self::Scalar(ty, word),
        }
    }
    fn ty(self) -> Ty {
        match self {
            Self::Int(_) => Ty::I64,
            Self::Bool(_) => Ty::Bool,
            Self::String(_) => Ty::String,
            Self::Scalar(ty, _) => ty,
            Self::Void => Ty::Void,
        }
    }
    fn integer(self) -> i128 {
        if self.ty().signed() {
            self.word() as i64 as i128
        } else {
            self.word() as i128
        }
    }
    fn integer_or_bool(self) -> i128 {
        if let Self::Bool(value) = self {
            i128::from(value)
        } else {
            self.integer()
        }
    }
    fn float(self) -> f64 {
        if self.ty() == Ty::F32 {
            f32::from_bits(self.word() as u32) as f64
        } else {
            f64::from_bits(self.word())
        }
    }
    fn int(self) -> i64 {
        match self {
            Value::Int(value) => value,
            Value::Scalar(_, word) => word as i64,
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
            Value::Scalar(_, word) => word,
            Value::Bool(value) => u64::from(value),
            Value::String(_) | Value::Void => {
                unreachable!("the checker rejects managed or void host arguments")
            }
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
    strings: Vec<String>,
    tokens: &'a Tokens,
    ast: &'a Ast,
    checked: &'a Checked,
    interner: &'a Interner,
    hosts: &'a Hosts,
    budget: u64,
    depth: u32,
    /// The function being evaluated; `None` for top-level code.
    current: Option<FnId>,
    /// Active script calls of this (possibly nested) run, outermost first:
    /// the caller and the call's span.
    calls: Vec<(Option<FnId>, Span)>,
    /// Script frames active in outer runs, below a nested run.
    below: usize,
    /// Nested runs active.
    nesting: u32,
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

    fn block(&mut self, host: &mut dyn Host, block: Block) -> Flow<Value> {
        let mut value = Value::Void;
        for &stmt in self.ast.block(block) {
            value = self.stmt(host, stmt)?;
        }
        Ok(value)
    }

    /// Execute a statement; an expression statement yields its value, so a
    /// block's value is that of its final statement.
    fn stmt(&mut self, host: &mut dyn Host, id: eitri_syntax::ast::StmtId) -> Flow<Value> {
        match self.ast.stmts[id] {
            Stmt::Let { init, .. } => {
                let value = self.expr(host, init)?;
                let binding = self.checked.stmt_bindings[id].expect("checked `let`");
                self.locals[binding.index()] = Some(value);
            }
            Stmt::Assign {
                op, value, span, ..
            } => {
                let binding = self.checked.stmt_bindings[id].expect("checked assignment");
                let rhs = self.expr(host, value)?;
                let new = match op {
                    AssignOp::Set => rhs,
                    AssignOp::Compound(op) => {
                        let lhs = self.read(binding);
                        self.binary_values(op, lhs, rhs, span)?
                    }
                };
                self.locals[binding.index()] = Some(new);
            }
            Stmt::Expr(expr) => return self.expr(host, expr),
            Stmt::While {
                keyword,
                condition,
                body,
            } => {
                while self.expr(host, condition)?.bool() {
                    self.block(host, body)?;
                    // After the body, before the condition again.
                    self.debit(self.tokens.span(keyword))?;
                }
            }
            Stmt::Return { value, .. } => {
                let value = match value {
                    Some(value) => self.expr(host, value)?,
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

    fn expr(&mut self, host: &mut dyn Host, e: ExprId) -> Flow<Value> {
        let span = self.ast.span(e);
        match self.ast.exprs[e] {
            Expr::Int { .. } | Expr::Float { .. } | Expr::Char(_) => Ok(Value::from_word(
                self.checked.types[e],
                self.checked.int_value(e).expect("checked literal fits") as u64,
            )),
            Expr::String(_) | Expr::StringPart(_) => {
                let Res::StringLiteral(index) = self.checked.resolutions[e] else {
                    unreachable!()
                };
                Ok(Value::String(index))
            }
            Expr::Interpolation(parts) => {
                let mut text = String::new();
                for &part in self.ast.args(parts) {
                    let value = self.expr(host, part)?;
                    match value {
                        Value::String(index) => text.push_str(&self.strings[index as usize]),
                        _ => text.push_str(&format_value(value)),
                    }
                }
                let index = self.strings.len() as u32;
                self.strings.push(text);
                Ok(Value::String(index))
            }
            Expr::Convert { value, .. } => {
                let value = self.expr(host, value)?;
                self.convert(e, value)
            }
            Expr::Bool(value) => Ok(Value::Bool(value)),
            Expr::Name(_) => match self.checked.resolutions[e] {
                Res::Binding(binding) => Ok(self.read(binding)),
                other => unreachable!("a name used as a value resolves to a binding: {other:?}"),
            },
            Expr::Unary { op, operand } => {
                let value = self.expr(host, operand)?;
                match op {
                    UnaryOp::Not => Ok(Value::Bool(!value.bool())),
                    UnaryOp::Neg if value.ty().float() => Ok(Value::from_word(
                        value.ty(),
                        oracle::scalar::float(
                            eitri_bytecode::spec::FloatOp::Neg,
                            wire_type(value.ty()),
                            value.word(),
                            0,
                        ),
                    )),
                    UnaryOp::Neg => match oracle::neg(int_type(value.ty()), value.integer()) {
                        Ok(number) => Ok(Value::from_word(value.ty(), number as u64)),
                        Err(fault) => Err(self.fault(message(fault), span)),
                    },
                    UnaryOp::BitNot => Ok(Value::from_word(
                        value.ty(),
                        int_type(value.ty()).wrap(!value.integer()) as u64,
                    )),
                }
            }
            Expr::Binary { op, lhs, rhs } => match op {
                BinaryOp::Coalesce => {
                    let value = self.expr(host, lhs)?;
                    if let Value::Scalar(Ty::FindOffset, word) = value {
                        if word as i64 == -1 {
                            self.expr(host, rhs)
                        } else {
                            Ok(Value::Int(word as i64))
                        }
                    } else {
                        unreachable!("checked optional find")
                    }
                }
                BinaryOp::And => Ok(Value::Bool(
                    self.expr(host, lhs)?.bool() && self.expr(host, rhs)?.bool(),
                )),
                BinaryOp::Or => Ok(Value::Bool(
                    self.expr(host, lhs)?.bool() || self.expr(host, rhs)?.bool(),
                )),
                _ => {
                    let a = self.expr(host, lhs)?;
                    let b = self.expr(host, rhs)?;
                    self.binary_values(op, a, b, span)
                }
            },
            Expr::Call { callee, .. }
                if matches!(
                    self.checked.resolutions[e],
                    Res::StringLen | Res::StringIsEmpty
                ) =>
            {
                let Expr::Member { base, .. } = self.ast.exprs[callee] else {
                    unreachable!()
                };
                let Value::String(index) = self.expr(host, base)? else {
                    unreachable!()
                };
                let value = &self.strings[index as usize];
                Ok(if self.checked.resolutions[e] == Res::StringIsEmpty {
                    Value::Bool(value.is_empty())
                } else {
                    Value::Int(value.len() as i64)
                })
            }
            Expr::Call { callee, args }
                if matches!(
                    self.checked.resolutions[e],
                    Res::StringSlice
                        | Res::StringContains
                        | Res::StringStarts
                        | Res::StringEnds
                        | Res::StringFind
                ) =>
            {
                let Expr::Member { base, .. } = self.ast.exprs[callee] else {
                    unreachable!()
                };
                let Value::String(index) = self.expr(host, base)? else {
                    unreachable!()
                };
                let mut arguments = Vec::new();
                for &argument in self.ast.args(args) {
                    arguments.push(self.expr(host, argument)?);
                }
                if self.checked.resolutions[e] == Res::StringSlice {
                    let first = arguments[0].integer();
                    let last = arguments[1].integer();
                    let slice = usize::try_from(first)
                        .ok()
                        .zip(usize::try_from(last).ok())
                        .and_then(|(first, last)| self.strings[index as usize].get(first..last));
                    let Some(slice) = slice else {
                        return Err(self.fault("invalid string slice boundary", span));
                    };
                    let value = slice.to_owned();
                    let index = self.strings.len() as u32;
                    self.strings.push(value);
                    Ok(Value::String(index))
                } else {
                    let Value::String(other) = arguments[0] else {
                        unreachable!()
                    };
                    let value = &self.strings[index as usize];
                    let other = &self.strings[other as usize];
                    if self.checked.resolutions[e] == Res::StringFind {
                        return Ok(Value::Scalar(
                            Ty::FindOffset,
                            value.find(other).map_or(-1, |n| n as i64) as u64,
                        ));
                    }
                    Ok(Value::Bool(match self.checked.resolutions[e] {
                        Res::StringContains => value.contains(other),
                        Res::StringStarts => value.starts_with(other),
                        Res::StringEnds => value.ends_with(other),
                        _ => unreachable!(),
                    }))
                }
            }
            Expr::Call { args, .. }
                if matches!(self.checked.resolutions[e], Res::StringConvert(_)) =>
            {
                let value = self.expr(host, self.ast.args(args)[0])?;
                if let Value::String(_) = value {
                    return Ok(value);
                }
                let text = format_value(value);
                let index = self.strings.len() as u32;
                self.strings.push(text);
                Ok(Value::String(index))
            }
            Expr::Call { args, .. }
                if matches!(self.checked.resolutions[e], Res::Convert { .. }) =>
            {
                let value = self.expr(host, self.ast.args(args)[0])?;
                self.convert(e, value)
            }
            Expr::Call { args, .. } => {
                let mut values = Vec::with_capacity(self.ast.args(args).len());
                for &arg in self.ast.args(args) {
                    values.push(self.expr(host, arg)?);
                }
                // The callee starts only after the debit.
                self.debit(span)?;
                match self.checked.resolutions[e] {
                    Res::Function(f) => self.call_script(host, f, &values, span),
                    Res::Host(h) => self.call_host(host, h, &values, span),
                    other => unreachable!("a checked call resolves to a function: {other:?}"),
                }
            }
            Expr::If(mut id) => loop {
                let node = self.ast.ifs[id];
                if self.expr(host, node.condition)?.bool() {
                    break self.block(host, node.then);
                }
                match node.otherwise {
                    Else::None => break Ok(Value::Void),
                    Else::Block(block) => break self.block(host, block),
                    Else::If(nested) => match self.ast.exprs[nested] {
                        Expr::If(next) => id = next,
                        _ => break self.expr(host, nested),
                    },
                }
            },
            Expr::Member { .. } | Expr::Error => {
                unreachable!("not a value in checked code: {:?}", self.ast.exprs[e])
            }
        }
    }

    fn convert(&self, e: ExprId, value: Value) -> Flow<Value> {
        let Res::Convert { from, to, checked } = self.checked.resolutions[e] else {
            unreachable!()
        };
        oracle::scalar::convert(value.word(), wire_type(from), wire_type(to), checked)
            .map(|word| Value::from_word(to, word))
            .map_err(|error| {
                self.fault(
                    match error {
                        oracle::scalar::ConversionFault::Overflow => "integer overflow",
                        oracle::scalar::ConversionFault::Conversion => {
                            "numeric conversion out of range"
                        }
                    },
                    self.ast.span(e),
                )
            })
    }

    fn binary_values(&mut self, op: BinaryOp, a: Value, b: Value, span: Span) -> Flow<Value> {
        use BinaryOp::*;
        let ty = a.ty();
        if let (Value::String(left), Value::String(right)) = (a, b) {
            let lhs = &self.strings[left as usize];
            let rhs = &self.strings[right as usize];
            if op != Add {
                return Ok(Value::Bool(match op {
                    Eq => lhs == rhs,
                    NotEq => lhs != rhs,
                    Lt => lhs < rhs,
                    LtEq => lhs <= rhs,
                    Gt => lhs > rhs,
                    GtEq => lhs >= rhs,
                    _ => unreachable!(),
                }));
            }
            let text = format!(
                "{}{}",
                self.strings[left as usize], self.strings[right as usize]
            );
            let index = self.strings.len() as u32;
            self.strings.push(text);
            return Ok(Value::String(index));
        }
        if matches!(op, Eq | NotEq | Lt | LtEq | Gt | GtEq) {
            let order = if ty.float() {
                a.float().partial_cmp(&b.float())
            } else {
                Some(a.integer_or_bool().cmp(&b.integer_or_bool()))
            };
            use std::cmp::Ordering::{Equal, Greater, Less};
            return Ok(Value::Bool(match op {
                Eq => order == Some(Equal),
                NotEq => order != Some(Equal),
                Lt => order == Some(Less),
                LtEq => matches!(order, Some(Less | Equal)),
                Gt => order == Some(Greater),
                GtEq => matches!(order, Some(Greater | Equal)),
                _ => unreachable!(),
            }));
        }
        if ty.float() {
            use eitri_bytecode::spec::FloatOp;
            let operation = match op {
                Add => FloatOp::Add,
                Sub => FloatOp::Sub,
                Mul => FloatOp::Mul,
                Div => FloatOp::Div,
                Rem => FloatOp::Rem,
                _ => unreachable!(),
            };
            return Ok(Value::from_word(
                ty,
                oracle::scalar::float(operation, wire_type(ty), a.word(), b.word()),
            ));
        }
        if matches!(op, BitAnd | BitOr | BitXor) {
            let (x, y) = (a.integer(), b.integer());
            let result = match op {
                BitAnd => x & y,
                BitOr => x | y,
                _ => x ^ y,
            };
            return Ok(Value::from_word(ty, int_type(ty).wrap(result) as u64));
        }
        let arith = match op {
            Add => Arith::Add,
            Sub => Arith::Sub,
            Mul => Arith::Mul,
            Div => Arith::Div,
            Rem => Arith::Rem,
            WrapAdd => Arith::WrapAdd,
            WrapSub => Arith::WrapSub,
            WrapMul => Arith::WrapMul,
            Shl => Arith::Shl,
            Shr => Arith::Shr,
            _ => unreachable!(),
        };
        match oracle::arith(int_type(ty), arith, a.integer(), b.integer()) {
            Ok(value) => Ok(Value::from_word(ty, value as u64)),
            Err(error) => Err(self.fault(message(error), span)),
        }
    }

    fn call_script(
        &mut self,
        host: &mut dyn Host,
        f: FnId,
        args: &[Value],
        span: Span,
    ) -> Flow<Value> {
        // Top-level code is not a counted frame.
        if (self.below + self.calls.len()) as u32 >= self.depth {
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
        let result = match self.block(host, body) {
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

    fn call_host(
        &mut self,
        host: &mut dyn Host,
        h: HostFnId,
        args: &[Value],
        span: Span,
    ) -> Flow<Value> {
        let words: Vec<u64> = args
            .iter()
            .map(|value| {
                if matches!(value, Value::String(_)) {
                    0
                } else {
                    value.word()
                }
            })
            .collect();
        let string_args: Vec<_> = args
            .iter()
            .map(|value| match value {
                Value::String(index) => Some(self.strings[*index as usize].clone()),
                _ => None,
            })
            .collect();
        let function = self.hosts.function(h);
        let mut results = vec![0u64; usize::from(function.ret != Ty::Void)];
        let mut string_result = None;
        if let Err(error) = host.call(
            self,
            h.as_u32(),
            &words,
            &mut results,
            &string_args,
            &mut string_result,
        ) {
            let (message, mut trace) = match error {
                HostError::Message(message) => (message, Vec::new()),
                HostError::Fault(fault) => (fault.message, fault.trace),
            };
            let module = &self.hosts.module(function.module).path;
            trace.push((
                format!("{module}.{}", self.interner.resolve(function.name)),
                None,
            ));
            let Unwind::Fault(outer) = self.fault(message, span) else {
                unreachable!("`fault` makes a fault")
            };
            trace.extend(outer.trace);
            return Err(Unwind::Fault(Fault {
                message: outer.message,
                trace,
            }));
        }
        if function.ret == Ty::String {
            let value = string_result.expect("test host initializes string result");
            let index = self.strings.len() as u32;
            self.strings.push(value);
            Ok(Value::String(index))
        } else {
            Ok(Value::from_word(
                function.ret,
                results.first().copied().unwrap_or(0),
            ))
        }
    }

    fn find(&self, name: &str) -> Option<FnId> {
        self.ast
            .fns
            .iter_enumerated()
            .find(|(_, f)| self.interner.resolve(self.tokens.symbol(f.name)) == name)
            .map(|(id, _)| id)
    }
}

impl Reenter for Eval<'_> {
    fn has_function(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    fn call_text(&mut self, host: &mut dyn Host, name: &str, arg: &str) -> Result<String, Fault> {
        let f = self.find(name).expect("checked with `has_function`");
        let signature = self.checked.signatures[f];
        if self.checked.param_types[signature.params] != [Ty::String] || signature.ret != Ty::String
        {
            return Err(Fault {
                message: format!(
                    "host error: reentrant call to `{name}` does not match its signature"
                ),
                trace: Vec::new(),
            });
        }
        let stop = |message: &str| Fault {
            message: message.to_owned(),
            trace: Vec::new(),
        };
        if self.budget == 0 {
            return Err(stop("execution budget exhausted"));
        }
        self.budget -= 1;
        let active = self.below + self.calls.len();
        if active as u32 >= self.depth || self.nesting >= MAX_NESTING {
            return Err(stop("call depth exceeded"));
        }
        let index = self.strings.len() as u32;
        self.strings.push(arg.to_owned());
        let calls = std::mem::take(&mut self.calls);
        let below = std::mem::replace(&mut self.below, active + 1);
        let current = self.current;
        self.nesting += 1;
        let mut locals = vec![None; self.checked.bindings.len()];
        locals[self.checked.params[f].iter().next().unwrap().index()] = Some(Value::String(index));
        let saved = std::mem::replace(&mut self.locals, locals);
        self.current = Some(f);
        let body = self.ast.fns[f].body.expect("script functions have bodies");
        let outcome = match self.block(host, body) {
            Err(Unwind::Return(Value::String(index))) => Ok(self.strings[index as usize].clone()),
            Err(Unwind::Fault(fault)) => Err(fault),
            _ => unreachable!("checked string result"),
        };
        self.locals = saved;
        self.current = current;
        self.nesting -= 1;
        self.below = below;
        self.calls = calls;
        outcome
    }

    /// The chapter's reentry rules: the arguments must match the
    /// signature; the call debits one step and counts as a frame; nested
    /// runs are limited to [`MAX_NESTING`].
    fn call(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        args: &[u64],
        results: &mut [u64],
    ) -> Result<(), Fault> {
        let f = self.find(name).expect("checked with `has_function`");
        let signature = self.checked.signatures[f];
        let params = &self.checked.param_types[signature.params];
        let returns = usize::from(signature.ret != Ty::Void);
        let fits = |ty: Ty, word: u64| wire_type(ty).canonical(word);
        if args.len() != params.len()
            || results.len() != returns
            || signature.ret == Ty::String
            || params.contains(&Ty::String)
            || !params.iter().zip(args).all(|(&ty, &word)| fits(ty, word))
        {
            return Err(Fault {
                message: format!(
                    "host error: reentrant call to `{name}` does not match its signature"
                ),
                trace: Vec::new(),
            });
        }
        let stop = |message: &str| {
            Err(Fault {
                message: message.to_owned(),
                trace: Vec::new(),
            })
        };
        if self.budget == 0 {
            return stop("execution budget exhausted");
        }
        self.budget -= 1;
        let active = self.below + self.calls.len();
        if active as u32 >= self.depth || self.nesting >= MAX_NESTING {
            return stop("call depth exceeded");
        }
        let values: Vec<Value> = params
            .iter()
            .zip(args)
            .map(|(&ty, &word)| Value::from_word(ty, word))
            .collect();
        // The nested run's trace starts empty; its entry is one more frame.
        let calls = std::mem::take(&mut self.calls);
        let below = std::mem::replace(&mut self.below, active + 1);
        let current = self.current;
        self.nesting += 1;
        let mut locals = vec![None; self.checked.bindings.len()];
        for (binding, &value) in self.checked.params[f].iter().zip(&values) {
            locals[binding.index()] = Some(value);
        }
        let saved = std::mem::replace(&mut self.locals, locals);
        self.current = Some(f);
        let body = self.ast.fns[f].body.expect("script functions have bodies");
        let outcome = match self.block(host, body) {
            Ok(_) => Ok(Value::Void),
            Err(Unwind::Return(value)) => Ok(value),
            Err(Unwind::Fault(fault)) => Err(fault),
        };
        self.locals = saved;
        self.current = current;
        self.nesting -= 1;
        self.below = below;
        self.calls = calls;
        if let Some(slot) = results.first_mut() {
            *slot = outcome?.word();
        } else {
            outcome?;
        }
        Ok(())
    }
}

/// The chapter's fault messages ("Arithmetic").
fn message(fault: oracle::Fault) -> &'static str {
    match fault {
        oracle::Fault::Overflow => "integer overflow",
        oracle::Fault::DivisionByZero => "division by zero",
        oracle::Fault::ShiftAmount => "invalid shift amount",
    }
}

fn wire_type(ty: Ty) -> eitri_bytecode::ValueType {
    use eitri_bytecode::ValueType as V;
    match ty {
        Ty::I64 => V::I64,
        Ty::I8 => V::I8,
        Ty::U8 => V::U8,
        Ty::U64 => V::U64,
        Ty::F32 => V::F32,
        Ty::F64 => V::F64,
        Ty::Char => V::Char,
        Ty::Bool => V::Bool,
        _ => unreachable!(),
    }
}
fn int_type(ty: Ty) -> IntType {
    IntType {
        bits: ty.bits(),
        signed: ty.signed(),
    }
}
