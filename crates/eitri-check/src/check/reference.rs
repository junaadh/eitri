//! The checker as of T008, frozen as a test oracle. Optimized versions of
//! `super` must produce the same diagnostics, types, resolutions, and
//! bindings on every input (see the differential test). Never optimize
//! this copy.
#![allow(dead_code, unused_imports)]

use eitri_base::{
    FxHashMap, FxHashSet, IdRange, Idx, IndexVec, Interner, List, ListPool, SourceId, Span, Symbol,
};
use eitri_diag::{Code, Diagnostic, Diagnostics, Label};
use eitri_syntax::ast::{
    AssignOp, Ast, BinaryOp, Block, Else, Expr, ExprId, FnId, IfExpr, Item, Stmt, StmtId, TypeExpr,
    UnaryOp,
};
use eitri_syntax::{TokenId, Tokens};

use crate::host::{HostFnId, HostModuleId, Hosts};
use crate::ty::{Ty, TypeName, type_name};

pub use super::{Binding, BindingId, Res, Signature};

/// The checker's results, consumed by lowering (T009).
#[derive(Default)]
pub struct Checked {
    pub bindings: IndexVec<BindingId, Binding>,
    /// The type of every expression.
    pub types: IndexVec<ExprId, Ty>,
    /// For names and calls: what they refer to.
    pub resolutions: IndexVec<ExprId, Res>,
    /// For `let`: the declared binding; for assignment: the target.
    pub stmt_bindings: IndexVec<StmtId, Option<BindingId>>,
    pub signatures: IndexVec<FnId, Signature>,
    pub param_types: ListPool<Ty>,
    /// Each function's parameters, in order.
    pub params: IndexVec<FnId, IdRange<BindingId>>,
    /// Integer literal values (sign folded in).
    pub int_values: FxHashMap<ExprId, i64>,
    /// The host module of each import, in `Ast::imports` order.
    pub imports: Vec<Option<HostModuleId>>,
}

impl Checked {
    fn clear(&mut self) {
        self.bindings.clear();
        self.types.clear();
        self.resolutions.clear();
        self.stmt_bindings.clear();
        self.signatures.clear();
        self.param_types.clear();
        self.params.clear();
        self.int_values.clear();
        self.imports.clear();
    }
}

/// What a name means in the scope where it is looked up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Name {
    Binding(BindingId),
    Function(FnId),
    Module(HostModuleId),
    /// Declared, but its declaration had an error: uses are silent.
    Poisoned,
}

/// The checker's reusable working memory (D44).
#[derive(Default)]
pub struct CheckScratch {
    checked: Checked,
    /// The innermost declaration of each spelling, by symbol index.
    heads: Vec<Option<Name>>,
    /// Hidden heads to restore when scopes close.
    undo: Vec<(Symbol, Option<Name>)>,
    /// Pending left spines of binary chains.
    spine: Vec<ExprId>,
    module_bindings: FxHashSet<Symbol>,
    unknown: FxHashSet<Symbol>,
}

impl CheckScratch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn checked(&self) -> &Checked {
        &self.checked
    }
}

pub use super::Input;

/// Check `input`, replacing the results in `scratch`, and return them.
pub fn check<'s>(
    input: &Input<'_>,
    diagnostics: &mut Diagnostics,
    scratch: &'s mut CheckScratch,
) -> &'s Checked {
    scratch.checked.clear();
    scratch.heads.clear();
    scratch.undo.clear();
    scratch.spine.clear();
    scratch.module_bindings.clear();
    scratch.unknown.clear();
    let ast = input.ast;
    for _ in ast.exprs.iter() {
        scratch.checked.types.push(Ty::Error);
        scratch.checked.resolutions.push(Res::None);
    }
    for _ in ast.stmts.iter() {
        scratch.checked.stmt_bindings.push(None);
    }
    let mut checker = Checker {
        input,
        diagnostics,
        out: &mut scratch.checked,
        heads: &mut scratch.heads,
        undo: &mut scratch.undo,
        spine: &mut scratch.spine,
        module_bindings: &mut scratch.module_bindings,
        unknown: &mut scratch.unknown,
        current: None,
        loop_depth: 0,
    };
    checker.module();
    &scratch.checked
}

/// A callee's parameter types, borrowed: script signatures from the
/// results being built, host signatures from the registry.
#[derive(Clone, Copy)]
enum Params<'i> {
    Script(List<Ty>),
    Host(&'i [Ty]),
}

/// The function whose body is being checked.
#[derive(Clone, Copy)]
struct FnContext {
    id: FnId,
    ret: Ty,
}

struct Checker<'a, 'i> {
    input: &'a Input<'i>,
    diagnostics: &'a mut Diagnostics,
    out: &'a mut Checked,
    heads: &'a mut Vec<Option<Name>>,
    undo: &'a mut Vec<(Symbol, Option<Name>)>,
    spine: &'a mut Vec<ExprId>,
    module_bindings: &'a mut FxHashSet<Symbol>,
    /// Spellings already reported as unknown: a misspelled name is one
    /// mistake however often it is used (as while typing), so later uses
    /// are silent.
    unknown: &'a mut FxHashSet<Symbol>,
    current: Option<FnContext>,
    loop_depth: usize,
}

impl<'i> Checker<'_, 'i> {
    // ---- helpers ---------------------------------------------------------

    // Accessors return borrows of the input (`'i`), not of the checker, so
    // names and lists can be used while reporting without copying them.

    fn ast(&self) -> &'i Ast {
        self.input.ast
    }

    fn text(&self) -> &'i str {
        self.input.text
    }

    fn symbol(&self, token: TokenId) -> Symbol {
        self.input.tokens.symbol(token)
    }

    fn token_span(&self, token: TokenId) -> Span {
        self.input.tokens.span(token)
    }

    fn spelling(&self, token: TokenId) -> &'i str {
        let interner: &'i Interner = self.input.interner;
        interner.resolve(self.symbol(token))
    }

    fn span(&self, expr: ExprId) -> Span {
        self.ast().span(expr)
    }

    fn report(&mut self, code: Code, message: impl Into<String>, span: Span) {
        self.diagnostics.push(
            Diagnostic::error(code, message).with_label(Label::primary(self.input.source, span)),
        );
    }

    /// Report a mismatch unless either side is already an error.
    fn mismatch(&mut self, found: Ty, expected: Ty, span: Span) {
        if found == expected || found == Ty::Error || expected == Ty::Error {
            return;
        }
        self.report(
            Code::TypeMismatch,
            format!(
                "type mismatch: expected {}, found {}",
                expected.quoted(),
                found.quoted()
            ),
            span,
        );
    }

    // ---- scopes ----------------------------------------------------------

    fn lookup(&self, symbol: Symbol) -> Option<Name> {
        self.heads.get(symbol.index()).copied().flatten()
    }

    fn declare(&mut self, symbol: Symbol, name: Name) {
        if self.heads.len() <= symbol.index() {
            self.heads.resize(symbol.index() + 1, None);
        }
        let hidden = self.heads[symbol.index()].replace(name);
        self.undo.push((symbol, hidden));
    }

    fn mark(&self) -> usize {
        self.undo.len()
    }

    fn restore(&mut self, mark: usize) {
        while self.undo.len() > mark {
            let (symbol, hidden) = self.undo.pop().unwrap();
            self.heads[symbol.index()] = hidden;
        }
    }

    /// A predeclared type name used as a name: reported, `false`.
    fn check_name(&mut self, token: TokenId) -> bool {
        if !self.symbol(token).is_type_name() {
            return true;
        }
        let spelling = self.spelling(token);
        self.report(
            Code::TypeNameAsName,
            format!("`{spelling}` is a type name and cannot be used as a name"),
            self.token_span(token),
        );
        false
    }

    fn new_binding(&mut self, name: TokenId, mutable: bool, ty: Ty) -> BindingId {
        let owner = self.current.map(|f| f.id);
        let id = self.out.bindings.push(Binding {
            name,
            mutable,
            ty,
            owner,
        });
        if self.check_name(name) {
            self.declare(self.symbol(name), Name::Binding(id));
        }
        id
    }

    /// Resolve a name for a use. `None` after reporting.
    fn resolve(&mut self, token: TokenId) -> Option<Name> {
        let symbol = self.symbol(token);
        let span = self.token_span(token);
        let found = self.lookup(symbol);
        let module_binding = match found {
            Some(Name::Binding(b)) => self.out.bindings[b].owner.is_none(),
            None => self.module_bindings.contains(&symbol),
            _ => false,
        };
        if self.current.is_some() && module_binding {
            self.report(
                Code::Unsupported,
                "not supported yet: module bindings inside functions (T022)",
                span,
            );
            return None;
        }
        match found {
            Some(Name::Poisoned) => None,
            Some(name) => Some(name),
            None => {
                if self.unknown.insert(symbol) {
                    let spelling = self.spelling(token);
                    self.report(
                        Code::UnknownName,
                        format!("unknown name `{spelling}`"),
                        span,
                    );
                }
                None
            }
        }
    }

    // ---- types -----------------------------------------------------------

    /// A written type; `void` only where `allow_void`.
    fn ty(&mut self, ty: TypeExpr, allow_void: bool) -> Ty {
        let TypeExpr::Named(token) = ty else {
            return Ty::Error;
        };
        let span = self.token_span(token);
        let spelling = self.spelling(token);
        match type_name(self.symbol(token)) {
            TypeName::Supported(Ty::Void) if !allow_void => {
                self.report(Code::TypeMismatch, "`void` can only be a return type", span);
                Ty::Error
            }
            TypeName::Supported(ty) => ty,
            TypeName::Unsupported => {
                self.report(
                    Code::Unsupported,
                    format!("not supported yet: type `{spelling}`"),
                    span,
                );
                Ty::Error
            }
            TypeName::Unknown => {
                self.report(
                    Code::UnknownName,
                    format!("unknown type `{spelling}`"),
                    span,
                );
                Ty::Error
            }
        }
    }

    // ---- module ----------------------------------------------------------

    fn module(&mut self) {
        let ast = self.input.ast;
        for import in &ast.imports {
            let path_span = self.token_span(import.path);
            let literal = &self.input.text[path_span.range()];
            let path = literal.trim_matches('"');
            let module = self.input.hosts.find(path);
            if module.is_none() {
                self.report(
                    Code::UnresolvedImport,
                    format!("unresolved import `{path}`"),
                    path_span,
                );
            }
            self.out.imports.push(module);
            if !self.check_name(import.name) {
                continue;
            }
            let symbol = self.symbol(import.name);
            if self.lookup(symbol).is_some() {
                let spelling = self.spelling(import.name);
                self.report(
                    Code::DuplicateName,
                    format!("duplicate import `{spelling}`"),
                    self.token_span(import.name),
                );
                continue;
            }
            self.declare(symbol, module.map_or(Name::Poisoned, Name::Module));
        }

        // Every signature before any body, so calls may come first.
        for (id, decl) in ast.fns.iter_enumerated() {
            let params: Vec<Ty> = ast
                .params(decl.params)
                .iter()
                .map(|p| self.ty(p.ty, false))
                .collect();
            let ret = self.ty(decl.ret, true);
            let params = self.out.param_types.push(&params);
            self.out.signatures.push(Signature { params, ret });
            if !self.check_name(decl.name) {
                continue;
            }
            let symbol = self.symbol(decl.name);
            if self.lookup(symbol).is_some() {
                let spelling = self.spelling(decl.name);
                self.report(
                    Code::DuplicateName,
                    format!("duplicate function `{spelling}`"),
                    self.token_span(decl.name),
                );
                continue;
            }
            self.declare(symbol, Name::Function(id));
        }

        for item in &ast.items {
            if let Item::Stmt(stmt) = *item
                && let Stmt::Let { name, .. } = ast.stmts[stmt]
            {
                self.module_bindings.insert(self.symbol(name));
            }
        }

        for item in &ast.items {
            match *item {
                Item::Fn(id) => self.function(id),
                Item::Stmt(stmt) => self.stmt(stmt),
            }
        }
    }

    fn function(&mut self, id: FnId) {
        let ast = self.input.ast;
        let decl = ast.fns[id];
        let signature = self.out.signatures[id];
        self.current = Some(FnContext {
            id,
            ret: signature.ret,
        });
        let mark = self.mark();
        let first = self.out.bindings.next_index();
        for (index, param) in ast.params(decl.params).iter().enumerate() {
            let ty = self.out.param_types[signature.params][index];
            let duplicate = ast.params(decl.params)[..index]
                .iter()
                .any(|earlier| self.symbol(earlier.name) == self.symbol(param.name));
            if duplicate {
                let spelling = self.spelling(param.name);
                self.report(
                    Code::DuplicateName,
                    format!("duplicate parameter `{spelling}`"),
                    self.token_span(param.name),
                );
            }
            self.new_binding(param.name, false, ty);
        }
        self.out
            .params
            .push(IdRange::new(first, self.out.bindings.next_index()));

        if let Some(body) = decl.body {
            self.statements(body);
            let ret = signature.ret;
            if ret != Ty::Void && ret != Ty::Error && !self.returns(body) {
                if let Some(&last) = ast.block(body).last()
                    && let Stmt::Expr(tail) = ast.stmts[last]
                    && !matches!(self.out.types[tail], Ty::Void | Ty::Error)
                {
                    self.report(
                        Code::Unsupported,
                        "not supported yet: function results without `return`",
                        self.span(tail),
                    );
                } else {
                    let spelling = self.spelling(decl.name);
                    self.report(
                        Code::MissingReturn,
                        format!("function `{spelling}` may finish without returning a value"),
                        self.token_span(decl.name),
                    );
                }
            }
        }
        self.restore(mark);
        self.current = None;
    }

    /// Whether every path through `block` ends in `return`.
    fn returns(&self, block: Block) -> bool {
        let ast = self.input.ast;
        ast.block(block).iter().any(|&stmt| match ast.stmts[stmt] {
            Stmt::Return { .. } => true,
            Stmt::Expr(expr) => self.if_returns(expr),
            _ => false,
        })
    }

    fn if_returns(&self, expr: ExprId) -> bool {
        let ast = self.input.ast;
        let Expr::If(id) = ast.exprs[expr] else {
            return false;
        };
        let IfExpr {
            then, otherwise, ..
        } = ast.ifs[id];
        self.returns(then)
            && match otherwise {
                Else::None => false,
                Else::Block(block) => self.returns(block),
                Else::If(nested) => self.if_returns(nested),
            }
    }

    // ---- statements ------------------------------------------------------

    /// A block in its own scope, values discarded.
    fn statements(&mut self, block: Block) {
        let mark = self.mark();
        for &stmt in self.input.ast.block(block) {
            self.stmt(stmt);
        }
        self.restore(mark);
    }

    fn stmt(&mut self, id: StmtId) {
        match self.input.ast.stmts[id] {
            Stmt::Let {
                mutable,
                name,
                ty,
                init,
            } => {
                let annotated = ty.map(|ty| self.ty(ty, false));
                let found = self.value(init, annotated);
                let ty = match annotated {
                    Some(ty) => ty,
                    None if found == Ty::Void => {
                        self.report(
                            Code::TypeMismatch,
                            "type mismatch: expected a value, found `void`",
                            self.span(init),
                        );
                        Ty::Error
                    }
                    None => found,
                };
                let binding = self.new_binding(name, mutable, ty);
                self.out.stmt_bindings[id] = Some(binding);
            }
            Stmt::Assign {
                target, op, value, ..
            } => self.assign(id, target, op, value),
            Stmt::Expr(expr) => {
                self.expr(expr, None, false);
            }
            Stmt::AssignIndex { target, .. } | Stmt::AssignField { target, .. } => {
                self.report(
                    Code::Unsupported,
                    "not supported yet: assignment to fields",
                    self.span(target),
                );
            }
            Stmt::For(for_id) => {
                let eitri_syntax::ast::ForStmt {
                    name,
                    second,
                    start,
                    end,
                    body,
                    ..
                } = self.input.ast.fors[for_id];
                if let Some(end) = end {
                    let range_ty = self.value(start, None);
                    if range_ty != Ty::Error && !range_ty.integer() {
                        self.mismatch(range_ty, Ty::I64, self.span(start));
                    }
                    self.value(end, Some(range_ty));
                    if second.is_some() {
                        self.report(
                            Code::Unsupported,
                            "not supported yet: two names for a range",
                            self.span(start),
                        );
                    }
                    let mark = self.mark();
                    let binding = self.new_binding(name, false, range_ty);
                    self.out.stmt_bindings[id] = Some(binding);
                    self.loop_depth += 1;
                    self.statements(body);
                    self.loop_depth -= 1;
                    self.restore(mark);
                } else {
                    let iterator = match self.input.ast.exprs[start] {
                        Expr::Call { callee, args } if self.input.ast.args(args).is_empty() => {
                            match self.input.ast.exprs[callee] {
                                Expr::Member { base, name } => match self.spelling(name) {
                                    "chars" => Some((base, Res::ForChars, Ty::Char)),
                                    "lines" => Some((base, Res::ForLines, Ty::String)),
                                    _ => None,
                                },
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    let iterator = if let Some((base, kind, element)) = iterator {
                        self.value(base, Some(Ty::String));
                        self.out.resolutions[start] = kind;
                        Some(element)
                    } else if self.value(start, None) == Ty::String {
                        Some(Ty::Char)
                    } else {
                        None
                    };
                    if let Some(element) = iterator {
                        let mark = self.mark();
                        let binding = self.new_binding(
                            name,
                            false,
                            if second.is_some() { Ty::I64 } else { element },
                        );
                        self.out.stmt_bindings[id] = Some(binding);
                        if let Some(second) = second {
                            self.new_binding(second, false, element);
                        }
                        self.loop_depth += 1;
                        self.statements(body);
                        self.loop_depth -= 1;
                        self.restore(mark);
                    } else {
                        self.report(
                            Code::Unsupported,
                            "not supported yet: this for iterator",
                            self.span(start),
                        );
                    }
                }
            }
            Stmt::While {
                condition, body, ..
            } => {
                self.value(condition, Some(Ty::Bool));
                self.loop_depth += 1;
                self.statements(body);
                self.loop_depth -= 1;
            }
            Stmt::Break(keyword) | Stmt::Continue(keyword) => {
                if self.loop_depth == 0 {
                    self.report(
                        Code::Syntax,
                        "loop control outside a loop",
                        self.token_span(keyword),
                    );
                }
            }
            Stmt::Return { keyword, value } => {
                let ret = self.current.map_or(Ty::Error, |f| f.ret);
                match value {
                    Some(value) => {
                        self.value(value, Some(ret));
                    }
                    None => self.mismatch(Ty::Void, ret, self.token_span(keyword)),
                }
            }
        }
    }

    fn assign(&mut self, stmt: StmtId, target: TokenId, op: AssignOp, value: ExprId) {
        let binding = match self.resolve(target) {
            Some(Name::Binding(b)) => Some(b),
            Some(Name::Function(_) | Name::Module(_)) => None,
            Some(Name::Poisoned) | None => {
                self.expr(value, None, false);
                return;
            }
        };
        let mutable = binding.is_some_and(|b| self.out.bindings[b].mutable);
        if !mutable {
            let spelling = self.spelling(target);
            self.report(
                Code::AssignToImmutable,
                format!("cannot assign to `{spelling}`: it is not a `var` binding"),
                self.token_span(target),
            );
        }
        let ty = binding.map_or(Ty::Error, |b| self.out.bindings[b].ty);
        if matches!(op, AssignOp::Compound(_)) {
            self.mismatch(ty, Ty::I64, self.token_span(target));
        }
        let expected = if matches!(op, AssignOp::Compound(_)) {
            Ty::I64
        } else {
            ty
        };
        self.value(value, Some(expected));
        self.out.stmt_bindings[stmt] = binding;
    }

    // ---- expressions -----------------------------------------------------

    /// Check `expr` as a value against `expected`, reporting a mismatch at
    /// its span. Returns its own type.
    fn value(&mut self, expr: ExprId, expected: Option<Ty>) -> Ty {
        let found = self.expr(expr, expected, true);
        // An `if`'s branches were held to `expected` and reported their own
        // mismatch; reporting the whole `if` again would be a cascade.
        if let Some(expected) = expected
            && !matches!(self.input.ast.exprs[expr], Expr::If(_))
        {
            self.mismatch(found, expected, self.span(expr));
        }
        found
    }

    /// Check `expr`; `used` when its value is needed (an `if` then needs
    /// an `else`). Returns its type without comparing it to `expected`,
    /// which only guides literals and branches.
    fn expr(&mut self, expr: ExprId, expected: Option<Ty>, used: bool) -> Ty {
        let ty = match self.input.ast.exprs[expr] {
            Expr::Int { token, negative } => self.int(expr, token, negative),
            Expr::Bool(_) => Ty::Bool,
            Expr::Match(_) | Expr::EnumShorthand(_) | Expr::NamedCall { .. } => {
                unreachable!("outside frozen M0 reference")
            }
            Expr::SelfValue(token) => {
                self.report(
                    Code::Unsupported,
                    "not supported yet: `self`",
                    self.token_span(token),
                );
                Ty::Error
            }
            Expr::StructLiteral { name, .. } => {
                self.report(
                    Code::Unsupported,
                    "not supported yet: struct literals",
                    self.token_span(name),
                );
                Ty::Error
            }
            Expr::Name(token) => self.name(expr, token),
            Expr::Member { base, name } => self.member_value(base, name),
            Expr::Unary { op, operand } => {
                let operand_ty = match op {
                    UnaryOp::Neg => Ty::I64,
                    UnaryOp::Not => Ty::Bool,
                    UnaryOp::BitNot => unreachable!("outside frozen M0 reference"),
                };
                self.value(operand, Some(operand_ty));
                operand_ty
            }
            Expr::Binary { .. } => self.binary_chain(expr),
            Expr::Call { callee, args } => self.call(expr, callee, args),
            Expr::If(id) => self.if_expr(expr, id, expected, used),
            Expr::Float { .. }
            | Expr::Char(_)
            | Expr::String(_)
            | Expr::Null(_)
            | Expr::StringPart(_)
            | Expr::Interpolation(_)
            | Expr::Convert { .. } => {
                unreachable!("outside frozen M0 reference")
            }
            Expr::TryOptional(operand) => {
                let source = self.value(operand, None);
                if source != Ty::Error {
                    self.report(
                        Code::TypeMismatch,
                        "try? requires a result",
                        self.span(expr),
                    );
                }
                Ty::Error
            }
            Expr::Index { .. } | Expr::FixedArray(_) | Expr::Try(_) | Expr::Error => Ty::Error,
        };
        self.out.types[expr] = ty;
        ty
    }

    fn int(&mut self, expr: ExprId, token: TokenId, negative: bool) -> Ty {
        let spelling = &self.input.text[self.token_span(token).range()];
        match int_value(spelling, negative) {
            Some(value) => {
                self.out.int_values.insert(expr, value);
            }
            None => self.report(
                Code::LiteralOutOfRange,
                "integer literal does not fit `i64`",
                self.span(expr),
            ),
        }
        Ty::I64
    }

    fn name(&mut self, expr: ExprId, token: TokenId) -> Ty {
        match self.resolve(token) {
            Some(Name::Binding(b)) => {
                self.out.resolutions[expr] = Res::Binding(b);
                self.out.bindings[b].ty
            }
            Some(Name::Function(_)) => {
                self.report(
                    Code::Unsupported,
                    "not supported yet: function values",
                    self.token_span(token),
                );
                Ty::Error
            }
            Some(Name::Module(_)) => {
                self.report(
                    Code::Unsupported,
                    "not supported yet: modules as values",
                    self.token_span(token),
                );
                Ty::Error
            }
            Some(Name::Poisoned) | None => Ty::Error,
        }
    }

    /// The host function `base.name`, if `base` names a module.
    fn host_function(&mut self, base: ExprId, name: TokenId) -> Result<HostFnId, ()> {
        let Expr::Name(module_token) = self.input.ast.exprs[base] else {
            self.expr(base, None, true);
            self.report(
                Code::Unsupported,
                "not supported yet: member access",
                self.token_span(name),
            );
            return Err(());
        };
        let module = match self.resolve(module_token) {
            Some(Name::Module(m)) => m,
            Some(Name::Poisoned) | None => return Err(()),
            Some(_) => {
                self.report(
                    Code::Unsupported,
                    "not supported yet: member access",
                    self.token_span(name),
                );
                return Err(());
            }
        };
        match self.input.hosts.lookup(module, self.symbol(name)) {
            Some(f) => Ok(f),
            None => {
                let message = format!(
                    "unknown name `{}.{}`",
                    self.spelling(module_token),
                    self.spelling(name)
                );
                self.report(Code::UnknownName, message, self.token_span(name));
                Err(())
            }
        }
    }

    fn member_value(&mut self, base: ExprId, name: TokenId) -> Ty {
        if self.host_function(base, name).is_ok() {
            self.report(
                Code::Unsupported,
                "not supported yet: function values",
                self.token_span(name),
            );
        }
        Ty::Error
    }

    fn call(&mut self, expr: ExprId, callee: ExprId, args: List<ExprId>) -> Ty {
        let ast = self.input.ast;
        let call_span = self.span(expr);
        let target = match ast.exprs[callee] {
            Expr::Name(token) => match self.resolve(token) {
                Some(Name::Function(f)) => {
                    let s = self.out.signatures[f];
                    Some((Res::Function(f), Params::Script(s.params), s.ret))
                }
                Some(Name::Binding(_) | Name::Module(_)) => {
                    let spelling = self.spelling(token);
                    self.report(
                        Code::ArgumentCount,
                        format!("`{spelling}` is not a function"),
                        call_span,
                    );
                    None
                }
                Some(Name::Poisoned) | None => None,
            },
            Expr::Member { base, name } => match self.host_function(base, name) {
                Ok(f) => {
                    let hosts: &'i Hosts = self.input.hosts;
                    let host = hosts.function(f);
                    Some((Res::Host(f), Params::Host(&host.params), host.ret))
                }
                Err(()) => None,
            },
            Expr::Error => None,
            _ => {
                self.expr(callee, None, true);
                self.report(
                    Code::ArgumentCount,
                    "this expression is not a function",
                    self.span(callee),
                );
                None
            }
        };
        let args = ast.args(args);
        let Some((res, params, ret)) = target else {
            for &arg in args {
                self.value(arg, None);
            }
            return Ty::Error;
        };
        self.out.resolutions[expr] = res;
        let count = match params {
            Params::Script(list) => list.len(),
            Params::Host(types) => types.len(),
        };
        if args.len() != count {
            let callee_text = &self.text()[self.span(callee).range()];
            let plural = if count == 1 { "" } else { "s" };
            self.report(
                Code::ArgumentCount,
                format!(
                    "`{callee_text}` takes {} argument{plural}, found {}",
                    count,
                    args.len()
                ),
                call_span,
            );
        }
        for (index, &arg) in args.iter().enumerate() {
            let expected = match params {
                Params::Script(list) => self.out.param_types[list].get(index).copied(),
                Params::Host(types) => types.get(index).copied(),
            };
            self.value(arg, expected);
        }
        ret
    }

    fn if_expr(
        &mut self,
        expr: ExprId,
        id: eitri_syntax::ast::IfId,
        expected: Option<Ty>,
        used: bool,
    ) -> Ty {
        let IfExpr {
            condition,
            then,
            otherwise,
        } = self.input.ast.ifs[id];
        self.value(condition, Some(Ty::Bool));
        if !used {
            self.statements(then);
            match otherwise {
                Else::None => {}
                Else::Block(block) => self.statements(block),
                Else::If(nested) => {
                    self.expr(nested, None, false);
                }
            }
            return Ty::Void;
        }
        let then_ty = self.block_value(then, expected);
        let expected = expected.or(Some(then_ty)).filter(|&t| t != Ty::Error);
        match otherwise {
            Else::None => {
                let start = self.span(expr).start;
                self.report(
                    Code::IfWithoutElse,
                    "`if` used as a value needs an `else`",
                    Span::new(start, start + 2),
                );
                Ty::Error
            }
            Else::Block(block) => {
                let else_ty = self.block_value(block, expected);
                if then_ty == Ty::Error {
                    else_ty
                } else {
                    then_ty
                }
            }
            Else::If(nested) => {
                let else_ty = self.value(nested, expected);
                if then_ty == Ty::Error {
                    else_ty
                } else {
                    then_ty
                }
            }
        }
    }

    /// A block used as a value: its final expression statement.
    fn block_value(&mut self, block: Block, expected: Option<Ty>) -> Ty {
        let mark = self.mark();
        let stmts = self.input.ast.block(block);
        let (last, rest) = match stmts.split_last() {
            Some((&last, rest)) => (Some(last), rest),
            None => (None, stmts),
        };
        for &stmt in rest {
            self.stmt(stmt);
        }
        let ty = match last.map(|s| (s, self.input.ast.stmts[s])) {
            Some((_, Stmt::Expr(value))) => self.value(value, expected),
            other => {
                if let Some((stmt, _)) = other {
                    self.stmt(stmt);
                }
                self.report(
                    Code::IfWithoutElse,
                    "this branch has no value: it does not end in an expression",
                    block.span,
                );
                Ty::Error
            }
        };
        self.restore(mark);
        ty
    }

    /// A binary expression, walking its left spine iteratively: the
    /// left-most operand is checked first, then each operator from the
    /// inside out, so evaluation order and diagnostics follow the source.
    fn binary_chain(&mut self, root: ExprId) -> Ty {
        let ast = self.input.ast;
        let mark = self.spine.len();
        let mut leftmost = root;
        while let Expr::Binary { lhs, .. } = ast.exprs[leftmost] {
            self.spine.push(leftmost);
            leftmost = lhs;
        }
        let first_op = match ast.exprs[*self.spine.last().unwrap()] {
            Expr::Binary { op, .. } => op,
            _ => unreachable!(),
        };
        let mut lhs_ty = self.value(leftmost, operand_type(first_op));
        let mut lhs_span = self.span(leftmost);
        // The left-most operand was just checked against its operator; every
        // later operator's left side is the previous operator's result.
        let mut first = true;
        while self.spine.len() > mark {
            let node = self.spine.pop().unwrap();
            let Expr::Binary { op, rhs, .. } = ast.exprs[node] else {
                unreachable!()
            };
            if !first && let Some(required) = operand_type(op) {
                self.mismatch(lhs_ty, required, lhs_span);
            }
            first = false;
            // One mistake, one diagnostic: once the left operand is wrong for
            // the operator, the right one is checked only for its own errors.
            let lhs_wrong = operand_type(op).is_some_and(|t| lhs_ty != t && lhs_ty != Ty::Error);
            let rhs_expected = match operand_type(op) {
                Some(_) if lhs_wrong => None,
                Some(t) => Some(t),
                // `==` and `!=` compare two values of the same type.
                None if lhs_ty == Ty::Void => {
                    self.report(
                        Code::TypeMismatch,
                        "type mismatch: expected a value, found `void`",
                        lhs_span,
                    );
                    None
                }
                None => Some(lhs_ty).filter(|&t| t != Ty::Error),
            };
            self.value(rhs, rhs_expected);
            let ty = result_type(op);
            self.out.types[node] = ty;
            lhs_ty = ty;
            lhs_span = self.span(node);
        }
        lhs_ty
    }
}

/// The operand type an operator requires, or `None` for `==`/`!=`.
fn operand_type(op: BinaryOp) -> Option<Ty> {
    use BinaryOp::*;
    match op {
        Or | And => Some(Ty::Bool),
        Eq | NotEq => None,
        BitAnd | BitOr | BitXor | Shl | Shr | Coalesce => {
            unreachable!("outside frozen M0 reference")
        }
        Lt | LtEq | Gt | GtEq | Add | Sub | Mul | Div | Rem | WrapAdd | WrapSub | WrapMul => {
            Some(Ty::I64)
        }
    }
}

fn result_type(op: BinaryOp) -> Ty {
    use BinaryOp::*;
    match op {
        Or | And | Eq | NotEq | Lt | LtEq | Gt | GtEq => Ty::Bool,
        Add | Sub | Mul | Div | Rem | WrapAdd | WrapSub | WrapMul => Ty::I64,
        BitAnd | BitOr | BitXor | Shl | Shr | Coalesce => {
            unreachable!("outside frozen M0 reference")
        }
    }
}

/// The value of an integer literal's spelling as an `i64`, with the sign of
/// a folded prefix `-`; `None` if it does not fit. The lexer has already
/// validated the spelling.
fn int_value(spelling: &str, negative: bool) -> Option<i64> {
    let (digits, radix) = match spelling.get(..2) {
        Some("0x") => (&spelling[2..], 16),
        Some("0o") => (&spelling[2..], 8),
        Some("0b") => (&spelling[2..], 2),
        _ => (spelling, 10),
    };
    let limit = 1u128 << 63;
    let mut value: u128 = 0;
    for c in digits.chars().filter(|&c| c != '_') {
        value = value * radix as u128 + c.to_digit(radix)? as u128;
        if value > limit {
            return None;
        }
    }
    if negative {
        (value <= limit).then(|| (value as i128).wrapping_neg() as i64)
    } else {
        i64::try_from(value).ok()
    }
}
