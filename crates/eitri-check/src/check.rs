//! Name resolution and type checking for the first slice, in one pass over
//! each body after a pass that collects every function signature (so calls
//! may precede declarations, D4).
//!
//! Names resolve through a head per spelling plus an undo log: declaring a
//! name records the head it hides, and leaving a scope restores heads back
//! to a mark. Lookup is O(1); leaving a scope is O(names declared in it).
//!
//! Expressions are checked against an expected type where the context
//! gives one. Long left-associative chains (`a + b + … + z`) are walked
//! along their left spine with an explicit stack, so their length is not
//! limited by the Rust stack; only nesting (bounded by the parser) recurses.
//! The whole check is linear in the size of the syntax tree.

use eitri_base::{IdRange, Idx, IndexVec, Interner, List, ListPool, SourceId, Span, Symbol};
use eitri_diag::{Code, Diagnostic, Diagnostics, Label};
use eitri_syntax::ast::{
    AssignOp, Ast, BinaryOp, Block, Else, Expr, ExprId, FnId, IfExpr, Item, Stmt, StmtId, TypeExpr,
    UnaryOp,
};
use eitri_syntax::{TokenId, Tokens};

use crate::host::{HostFnId, HostModuleId, Hosts};
use crate::ty::{Ty, TypeName, type_name};

eitri_base::newtype_index! {
    /// A declared variable or parameter. Distinct from its spelling: two
    /// shadowed `x` share a symbol and have different binding ids.
    pub struct BindingId;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub name: TokenId,
    pub mutable: bool,
    pub ty: Ty,
    /// The function declaring it, or `None` at module level.
    pub owner: Option<FnId>,
}

/// What a name expression, call target, or assignment refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Res {
    #[default]
    None,
    Binding(BindingId),
    Function(FnId),
    Host(HostFnId),
    StringLen,
    StringIsEmpty,
    StringLiteral(u32),
    /// An integer literal: its value is `Checked::ints[index]`.
    Int(u32),
    Convert {
        from: Ty,
        to: Ty,
        checked: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature {
    pub params: List<Ty>,
    pub ret: Ty,
}

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
    /// Integer literal values (sign folded in), referenced by `Res::Int`:
    /// a plain vector, so recording one is a push, not a hash insert.
    pub ints: Vec<i64>,
    pub strings: Vec<String>,
    /// The host module of each import, in `Ast::imports` order.
    pub imports: Vec<Option<HostModuleId>>,
}

impl Checked {
    /// The value of an integer literal expression, if it is one that fits.
    pub fn int_value(&self, expr: ExprId) -> Option<i64> {
        match self.resolutions[expr] {
            Res::Int(index) => Some(self.ints[index as usize]),
            _ => None,
        }
    }

    fn clear(&mut self) {
        self.bindings.clear();
        self.types.clear();
        self.resolutions.clear();
        self.stmt_bindings.clear();
        self.signatures.clear();
        self.param_types.clear();
        self.params.clear();
        self.ints.clear();
        self.strings.clear();
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

/// A context-local set indexed by the interner's dense symbol IDs. Its
/// backing capacity can be measured and reserved before checking.
#[derive(Default)]
struct SymbolSet {
    present: Vec<bool>,
}

impl SymbolSet {
    fn clear(&mut self) {
        self.present.fill(false);
    }
    fn contains(&self, symbol: &Symbol) -> bool {
        self.present.get(symbol.index()).copied().unwrap_or(false)
    }
    fn insert(&mut self, symbol: Symbol) -> bool {
        let index = symbol.index();
        if self.present.len() <= index {
            self.present.resize(index + 1, false);
        }
        let old = self.present[index];
        self.present[index] = true;
        !old
    }
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
    /// Pending outer links of `else if` chains used as values.
    links: Vec<IfLink>,
    module_bindings: SymbolSet,
    unknown: SymbolSet,
}

/// An `else if` link whose type waits on the rest of its chain: the nested
/// `if` expression and its parent's `then` type.
#[derive(Clone, Copy)]
struct IfLink {
    nested: ExprId,
    then: Ty,
}

impl CheckScratch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retained checker vectors, excluding allocator bookkeeping.
    pub fn vector_bytes(&self) -> usize {
        let c = &self.checked;
        c.bindings.capacity() * size_of::<Binding>()
            + c.types.capacity() * size_of::<Ty>()
            + c.resolutions.capacity() * size_of::<Res>()
            + c.stmt_bindings.capacity() * size_of::<Option<BindingId>>()
            + c.signatures.capacity() * size_of::<Signature>()
            + c.param_types.heap_bytes()
            + c.params.capacity() * size_of::<IdRange<BindingId>>()
            + c.ints.capacity() * size_of::<i64>()
            + c.strings.capacity() * size_of::<String>()
            + c.strings.iter().map(String::capacity).sum::<usize>()
            + c.imports.capacity() * size_of::<Option<HostModuleId>>()
            + self.heads.capacity() * size_of::<Option<Name>>()
            + self.undo.capacity() * size_of::<(Symbol, Option<Name>)>()
            + self.spine.capacity() * size_of::<ExprId>()
            + self.links.capacity() * size_of::<IfLink>()
            + self.module_bindings.present.capacity() * size_of::<bool>()
            + self.unknown.present.capacity() * size_of::<bool>()
    }

    /// Payload bytes in currently retained checked string literals.
    pub fn string_payload_bytes(&self) -> usize {
        self.checked.strings.iter().map(String::capacity).sum()
    }

    /// Pre-reserve vector storage for checking a parsed source. String
    /// payloads, temporary values, diagnostics and later phases must also be
    /// budgeted separately before this can enforce a whole-run limit.
    pub fn reserve_budgeted(
        &mut self,
        token_count: usize,
        symbol_count: usize,
        budget: &mut eitri_base::MemoryBudget,
    ) -> Result<(), eitri_base::OutOfMemory> {
        let count = token_count
            .checked_mul(2)
            .and_then(|n| n.checked_add(1))
            .ok_or(eitri_base::OutOfMemory)?;
        let c = &mut self.checked;
        c.bindings
            .reserve_budgeted(count.saturating_sub(c.bindings.len()), budget)?;
        c.types
            .reserve_budgeted(count.saturating_sub(c.types.len()), budget)?;
        c.resolutions
            .reserve_budgeted(count.saturating_sub(c.resolutions.len()), budget)?;
        c.stmt_bindings
            .reserve_budgeted(count.saturating_sub(c.stmt_bindings.len()), budget)?;
        c.signatures
            .reserve_budgeted(count.saturating_sub(c.signatures.len()), budget)?;
        c.param_types.reserve_budgeted(count, budget)?;
        c.params
            .reserve_budgeted(count.saturating_sub(c.params.len()), budget)?;
        macro_rules! reserve_total {
            ($values:expr, $total:expr) => {{
                let additional = $total.saturating_sub($values.len());
                budget.reserve_vec($values, additional)?;
            }};
        }
        reserve_total!(&mut c.ints, count);
        reserve_total!(&mut c.strings, count);
        reserve_total!(&mut c.imports, count);
        reserve_total!(&mut self.heads, symbol_count);
        reserve_total!(&mut self.undo, count);
        reserve_total!(&mut self.spine, count);
        reserve_total!(&mut self.links, count);
        reserve_total!(&mut self.module_bindings.present, symbol_count);
        reserve_total!(&mut self.unknown.present, symbol_count);
        Ok(())
    }

    pub fn checked(&self) -> &Checked {
        &self.checked
    }
}

/// The parts of a compilation the checker reads.
pub struct Input<'a> {
    pub text: &'a str,
    pub source: SourceId,
    pub tokens: &'a Tokens,
    pub ast: &'a Ast,
    pub interner: &'a Interner,
    pub hosts: &'a Hosts,
}

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
    scratch.links.clear();
    scratch.module_bindings.clear();
    scratch.unknown.clear();
    let ast = input.ast;
    scratch.checked.types.resize(ast.exprs.len(), Ty::Error);
    scratch
        .checked
        .resolutions
        .resize(ast.exprs.len(), Res::None);
    scratch.checked.stmt_bindings.resize(ast.stmts.len(), None);
    let mut checker = Checker {
        input,
        diagnostics,
        out: &mut scratch.checked,
        heads: &mut scratch.heads,
        undo: &mut scratch.undo,
        spine: &mut scratch.spine,
        links: &mut scratch.links,
        module_bindings: &mut scratch.module_bindings,
        unknown: &mut scratch.unknown,
        current: None,
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
    links: &'a mut Vec<IfLink>,
    module_bindings: &'a mut SymbolSet,
    /// Spellings already reported as unknown: a misspelled name is one
    /// mistake however often it is used (as while typing), so later uses
    /// are silent.
    unknown: &'a mut SymbolSet,
    current: Option<FnContext>,
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
    #[cold]
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
            let mut params = Vec::new();
            if params
                .try_reserve_exact(ast.params(decl.params).len())
                .is_err()
            {
                self.report(
                    Code::OutOfMemory,
                    "runtime memory limit exceeded",
                    self.token_span(decl.name),
                );
                return;
            }
            for param in ast.params(decl.params) {
                params.push(self.ty(param.ty, false));
            }
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

    /// Whether `expr` is an `if` every branch of which returns, following
    /// an `else if` chain in a loop.
    fn if_returns(&self, mut expr: ExprId) -> bool {
        let ast = self.input.ast;
        loop {
            let Expr::If(id) = ast.exprs[expr] else {
                return false;
            };
            let IfExpr {
                then, otherwise, ..
            } = ast.ifs[id];
            if !self.returns(then) {
                return false;
            }
            match otherwise {
                Else::None => return false,
                Else::Block(block) => return self.returns(block),
                Else::If(nested) => expr = nested,
            }
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
            Stmt::While {
                condition, body, ..
            } => {
                self.value(condition, Some(Ty::Bool));
                self.statements(body);
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
        let shifts = matches!(op, AssignOp::Compound(BinaryOp::Shl | BinaryOp::Shr));
        if let AssignOp::Compound(binary) = op
            && !valid_operand(binary, ty)
        {
            self.mismatch(ty, Ty::I64, self.token_span(target));
        }
        let operand_ty = if let AssignOp::Compound(binary) = op {
            if !valid_operand(binary, ty) || ty == Ty::Error {
                Ty::I64
            } else {
                ty
            }
        } else {
            ty
        };
        let rhs = self.value(value, if shifts { None } else { Some(operand_ty) });
        if shifts && rhs != Ty::Error && !rhs.integer() {
            self.mismatch(rhs, Ty::I64, self.span(value));
        }
        self.out.stmt_bindings[stmt] = binding;
    }

    // ---- expressions -----------------------------------------------------

    /// Check `expr` as a value against `expected`, reporting a mismatch at
    /// its span. Returns its own type.
    fn value(&mut self, expr: ExprId, expected: Option<Ty>) -> Ty {
        let found = self.expr(expr, expected, true);
        // Compare first: the span is only needed for the rare mismatch. An
        // `if`'s branches were held to `expected` and reported their own
        // mismatch; reporting the whole `if` again would be a cascade.
        if let Some(expected) = expected
            && found != expected
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
            Expr::Int { token, negative } => self.int(expr, token, negative, expected),
            Expr::Float { token, negative } => self.float(expr, token, negative, expected),
            Expr::Char(token) => {
                let spelling = &self.input.text[self.token_span(token).range()];
                if let Some(value) = char_value(spelling) {
                    self.literal(expr, value as u64);
                } else {
                    self.report(
                        Code::Lexical,
                        "invalid character literal or escape",
                        self.token_span(token),
                    );
                }
                Ty::Char
            }
            Expr::String(token) => {
                let spelling = &self.input.text[self.token_span(token).range()];
                match string_value(spelling) {
                    Ok(Some(value)) => {
                        self.out.resolutions[expr] =
                            Res::StringLiteral(self.out.strings.len() as u32);
                        self.out.strings.push(value);
                        Ty::String
                    }
                    Ok(None) => {
                        self.report(
                            Code::Unsupported,
                            "not supported yet: interpolated or multiline string literals",
                            self.token_span(token),
                        );
                        Ty::Error
                    }
                    Err(_) => {
                        self.report(
                            Code::OutOfMemory,
                            "runtime memory limit exceeded",
                            self.token_span(token),
                        );
                        Ty::Error
                    }
                }
            }
            Expr::Convert { callee, value } => self.conversion(expr, callee, &[value], false),
            Expr::Bool(_) => Ty::Bool,
            Expr::Name(token) => self.name(expr, token),
            Expr::Member { base, name } => self.member_value(base, name),
            Expr::Unary { op, operand } => {
                let operand_ty = if op == UnaryOp::Not {
                    Some(Ty::Bool)
                } else {
                    expected.filter(|t| t.numeric())
                };
                let found = self.value(operand, operand_ty);
                let valid = match op {
                    UnaryOp::Not => found == Ty::Bool,
                    UnaryOp::Neg => found.numeric(),
                    UnaryOp::BitNot => found.integer(),
                };
                if !valid && (operand_ty.is_none() || operand_ty == Some(found)) {
                    self.mismatch(found, Ty::I64, self.span(operand));
                }
                if op == UnaryOp::Not {
                    Ty::Bool
                } else if !valid {
                    if op == UnaryOp::BitNot {
                        Ty::Error
                    } else {
                        operand_ty.unwrap_or(Ty::I64)
                    }
                } else {
                    found
                }
            }
            Expr::Binary { .. } => self.binary_chain(expr, expected),
            Expr::Call { callee, args } => self.call(expr, callee, args),
            Expr::If(id) => self.if_expr(expr, id, expected, used),
            Expr::Error => Ty::Error,
        };
        self.out.types[expr] = ty;
        ty
    }

    fn literal(&mut self, expr: ExprId, word: u64) {
        self.out.resolutions[expr] = Res::Int(self.out.ints.len() as u32);
        self.out.ints.push(word as i64);
    }

    fn int(&mut self, expr: ExprId, token: TokenId, negative: bool, expected: Option<Ty>) -> Ty {
        let spelling = &self.input.text[self.token_span(token).range()];
        let ty = expected.filter(|t| t.numeric()).unwrap_or(Ty::I64);
        let value = if ty == Ty::I64 {
            int_value(spelling, negative).map(|v| v as u64)
        } else {
            integer_literal(spelling, negative, ty)
        };
        match value {
            Some(value) => self.literal(expr, value),
            None => self.report(
                Code::LiteralOutOfRange,
                format!("integer literal does not fit {}", ty.quoted()),
                self.span(expr),
            ),
        }
        ty
    }

    fn float(&mut self, expr: ExprId, token: TokenId, negative: bool, expected: Option<Ty>) -> Ty {
        let ty = expected.filter(|t| t.float()).unwrap_or(Ty::F64);
        let text = self.input.text[self.token_span(token).range()].replace('_', "");
        let value = if ty == Ty::F32 {
            text.parse::<f32>()
                .ok()
                .map(|x| if negative { -x } else { x })
                .filter(|x| x.is_finite())
                .map(|x| x.to_bits() as u64)
        } else {
            text.parse::<f64>()
                .ok()
                .map(|x| if negative { -x } else { x })
                .filter(|x| x.is_finite())
                .map(f64::to_bits)
        };
        if let Some(value) = value {
            self.literal(expr, value);
        } else {
            self.report(
                Code::LiteralOutOfRange,
                format!("float literal does not fit {}", ty.quoted()),
                self.span(expr),
            );
        }
        ty
    }

    fn conversion(&mut self, expr: ExprId, callee: ExprId, args: &[ExprId], checked: bool) -> Ty {
        let Expr::Name(token) = self.ast().exprs[callee] else {
            unreachable!("conversion target is a type name")
        };
        let to = match type_name(self.symbol(token)) {
            TypeName::Supported(t) if t.numeric() => t,
            _ => {
                self.report(
                    Code::TypeMismatch,
                    "numeric conversion requires a numeric destination",
                    self.span(callee),
                );
                for &arg in args {
                    self.value(arg, None);
                }
                return Ty::Error;
            }
        };
        if args.len() != 1 {
            self.report(
                Code::ArgumentCount,
                format!(
                    "{} conversion takes 1 argument, found {}",
                    to.quoted(),
                    args.len()
                ),
                self.span(expr),
            );
            for &arg in args {
                self.value(arg, None);
            }
            return Ty::Error;
        }
        let from = self.value(args[0], None);
        if from != Ty::Error
            && (!from.numeric() || (!checked && (!from.integer() || !to.integer())))
        {
            self.report(
                Code::TypeMismatch,
                if checked {
                    "numeric conversion requires a numeric source"
                } else {
                    "`truncating:` requires integer source and destination types"
                },
                self.span(expr),
            );
        }
        self.out.resolutions[expr] = Res::Convert { from, to, checked };
        to
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
        if matches!(ast.exprs[callee], Expr::Name(token) if self.symbol(token).is_type_name()) {
            return self.conversion(expr, callee, ast.args(args), true);
        }
        if let Expr::Member { base, name } = ast.exprs[callee]
            && matches!(self.spelling(name), "len" | "is_empty")
            && !matches!(ast.exprs[base], Expr::Name(token) if matches!(self.heads.get(self.symbol(token).as_u32() as usize).copied().flatten(), Some(Name::Module(_))))
        {
            self.value(base, Some(Ty::String));
            for &arg in ast.args(args) {
                self.value(arg, None);
            }
            if !ast.args(args).is_empty() {
                self.report(
                    Code::ArgumentCount,
                    format!("`{}` takes 0 arguments", self.spelling(name)),
                    self.span(expr),
                );
            }
            return if self.spelling(name) == "len" {
                self.out.resolutions[expr] = Res::StringLen;
                Ty::I64
            } else {
                self.out.resolutions[expr] = Res::StringIsEmpty;
                Ty::Bool
            };
        }
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

    /// An `if`, following its `else if` chain in a loop so the chain's
    /// length is not bounded by the stack. Diagnostics and recorded types
    /// match checking each nested `if` as its own expression: a used chain
    /// is unwound innermost first, as recursion would. Kept out of line:
    /// inlined, the loop made checking about 3% slower (E2, 2026-11-07).
    #[inline(never)]
    fn if_expr(
        &mut self,
        expr: ExprId,
        id: eitri_syntax::ast::IfId,
        expected: Option<Ty>,
        used: bool,
    ) -> Ty {
        let ast = self.input.ast;
        let mark = self.links.len();
        let (mut expr, mut id, mut expected) = (expr, id, expected);
        let mut ty = loop {
            let IfExpr {
                condition,
                then,
                otherwise,
            } = ast.ifs[id];
            self.value(condition, Some(Ty::Bool));
            if !used {
                self.statements(then);
                match otherwise {
                    Else::None => {}
                    Else::Block(block) => self.statements(block),
                    Else::If(nested) => {
                        if let Expr::If(next) = ast.exprs[nested] {
                            self.out.types[nested] = Ty::Void;
                            (expr, id) = (nested, next);
                            continue;
                        }
                        self.expr(nested, None, false);
                    }
                }
                break Ty::Void;
            }
            let then_ty = self.block_value(then, expected);
            let link_expected = expected.or(Some(then_ty)).filter(|&t| t != Ty::Error);
            match otherwise {
                Else::None => {
                    let start = self.span(expr).start;
                    self.report(
                        Code::IfWithoutElse,
                        "`if` used as a value needs an `else`",
                        Span::new(start, start + 2),
                    );
                    break Ty::Error;
                }
                Else::Block(block) => {
                    let else_ty = self.block_value(block, link_expected);
                    break if then_ty == Ty::Error {
                        else_ty
                    } else {
                        then_ty
                    };
                }
                Else::If(nested) => {
                    if let Expr::If(next) = ast.exprs[nested] {
                        self.links.push(IfLink {
                            nested,
                            then: then_ty,
                        });
                        (expr, id, expected) = (nested, next, link_expected);
                        continue;
                    }
                    let else_ty = self.value(nested, link_expected);
                    break if then_ty == Ty::Error {
                        else_ty
                    } else {
                        then_ty
                    };
                }
            }
        };
        while self.links.len() > mark {
            let link = self.links.pop().expect("above the mark");
            // What `value(nested, expected)` does after checking it; as an
            // `if`, the nested link reports no mismatch of its own.
            self.out.types[link.nested] = ty;
            if link.then != Ty::Error {
                ty = link.then;
            }
        }
        ty
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
    fn hint(&self, expr: ExprId) -> Option<Ty> {
        if let Expr::Name(token) = self.ast().exprs[expr]
            && let Some(Some(Name::Binding(binding))) = self.heads.get(self.symbol(token).index())
        {
            return Some(self.out.bindings[*binding].ty);
        }
        None
    }

    fn binary_chain(&mut self, root: ExprId, expected: Option<Ty>) -> Ty {
        let ast = self.input.ast;
        let mark = self.spine.len();
        let mut leftmost = root;
        while let Expr::Binary { lhs, .. } = ast.exprs[leftmost] {
            self.spine.push(leftmost);
            leftmost = lhs;
        }
        let Expr::Binary {
            op: first_op,
            rhs: first_rhs,
            ..
        } = ast.exprs[*self.spine.last().unwrap()]
        else {
            unreachable!()
        };
        let guidance = if matches!(first_op, BinaryOp::And | BinaryOp::Or) {
            Some(Ty::Bool)
        } else {
            expected.filter(|t| t.numeric()).or_else(|| {
                if !matches!(first_op, BinaryOp::Shl | BinaryOp::Shr)
                    && matches!(ast.exprs[leftmost], Expr::Int { .. } | Expr::Float { .. })
                {
                    self.hint(first_rhs)
                } else {
                    None
                }
            })
        };
        let mut lhs_ty = self.expr(leftmost, guidance, true);
        let mut lhs_span = self.span(leftmost);
        while self.spine.len() > mark {
            let node = self.spine.pop().unwrap();
            let Expr::Binary { op, rhs, .. } = ast.exprs[node] else {
                unreachable!()
            };
            let valid = valid_operand(op, lhs_ty);
            if !valid && lhs_ty == Ty::Void && matches!(op, BinaryOp::Eq | BinaryOp::NotEq) {
                self.report(
                    Code::TypeMismatch,
                    "type mismatch: expected a value, found `void`",
                    lhs_span,
                );
            } else if !valid {
                self.mismatch(
                    lhs_ty,
                    if matches!(op, BinaryOp::And | BinaryOp::Or) {
                        Ty::Bool
                    } else {
                        Ty::I64
                    },
                    lhs_span,
                );
            }
            let shifts = matches!(op, BinaryOp::Shl | BinaryOp::Shr);
            let rhs_expected = if valid && !shifts {
                if lhs_ty == Ty::Error {
                    match op {
                        BinaryOp::Eq | BinaryOp::NotEq => None,
                        BinaryOp::And | BinaryOp::Or => Some(Ty::Bool),
                        _ => Some(Ty::I64),
                    }
                } else {
                    Some(lhs_ty)
                }
            } else {
                None
            };
            let rhs_ty = self.value(rhs, rhs_expected);
            if shifts && rhs_ty != Ty::Error && !rhs_ty.integer() {
                self.mismatch(rhs_ty, Ty::I64, self.span(rhs));
            }
            let ty = if comparison(op) || matches!(op, BinaryOp::And | BinaryOp::Or) {
                Ty::Bool
            } else if valid && lhs_ty != Ty::Error {
                lhs_ty
            } else {
                Ty::I64
            };
            self.out.types[node] = ty;
            lhs_ty = ty;
            lhs_span = self.span(node);
        }
        lhs_ty
    }
}

fn comparison(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::Lt
            | BinaryOp::LtEq
            | BinaryOp::Gt
            | BinaryOp::GtEq
    )
}
fn valid_operand(op: BinaryOp, ty: Ty) -> bool {
    use BinaryOp::*;
    if ty == Ty::Error {
        return true;
    }
    match op {
        And | Or => ty == Ty::Bool,
        Eq | NotEq => ty != Ty::Void,
        Lt | LtEq | Gt | GtEq => ty.numeric() || matches!(ty, Ty::Char | Ty::String),
        Add => ty.numeric() || ty == Ty::String,
        Sub | Mul | Div | Rem => ty.numeric(),
        WrapAdd | WrapSub | WrapMul | BitAnd | BitOr | BitXor | Shl | Shr => ty.integer(),
    }
}

/// Exact integer-literal contextualization, without rounding large integers
/// through f64 on the way to f32.
fn integer_literal(text: &str, negative: bool, ty: Ty) -> Option<u64> {
    let (digits, radix) = match text.get(..2) {
        Some("0x") => (&text[2..], 16),
        Some("0o") => (&text[2..], 8),
        Some("0b") => (&text[2..], 2),
        _ => (text, 10),
    };
    if ty.float() {
        return exact_integer_float(digits, radix as u32, negative, ty);
    }
    let mut magnitude = 0u128;
    for c in digits.chars().filter(|&c| c != '_') {
        magnitude = magnitude
            .checked_mul(radix)?
            .checked_add(c.to_digit(radix as u32)? as u128)?;
    }
    let bits = ty.bits();
    let max = if ty.signed() {
        (1u128 << (bits - 1)) - 1
    } else {
        (1u128 << bits) - 1
    };
    if negative {
        if !ty.signed() {
            return (magnitude == 0).then_some(0);
        }
        if magnitude > max + 1 {
            return None;
        }
        Some((-(magnitude as i128)) as i64 as u64)
    } else {
        (magnitude <= max).then_some(magnitude as u64)
    }
}

/// A fixed 1024-bit accumulator covers every finite f64 integer. Long
/// spellings cost O(bytes), bounded 16-word arithmetic, and no heap buffer.
fn exact_integer_float(digits: &str, radix: u32, negative: bool, ty: Ty) -> Option<u64> {
    let mut words = [0u64; 16];
    for c in digits.chars().filter(|&c| c != '_') {
        let mut carry = c.to_digit(radix)? as u128;
        for word in &mut words {
            let value = *word as u128 * radix as u128 + carry;
            *word = value as u64;
            carry = value >> 64;
        }
        if carry != 0 {
            return None;
        }
    }
    let sign = u64::from(negative) << if ty == Ty::F32 { 31 } else { 63 };
    let Some(high) = words.iter().rposition(|&w| w != 0) else {
        return Some(sign);
    };
    let bits = high * 64 + 64 - words[high].leading_zeros() as usize;
    let low = words.iter().position(|&w| w != 0).unwrap();
    let trailing = low * 64 + words[low].trailing_zeros() as usize;
    let precision = if ty == Ty::F32 { 24 } else { 53 };
    let max_exp = if ty == Ty::F32 { 127 } else { 1023 };
    let significant = bits - trailing;
    if significant > precision || bits - 1 > max_exp {
        return None;
    }
    let mut magnitude = 0u64;
    for bit in trailing..bits {
        magnitude |= ((words[bit / 64] >> (bit % 64)) & 1) << (bit - trailing);
    }
    let fraction_bits = precision - 1;
    let bias = if ty == Ty::F32 { 127 } else { 1023 };
    let fraction = (magnitude << (precision - significant)) & ((1u64 << fraction_bits) - 1);
    Some(sign | (((bits - 1 + bias) as u64) << fraction_bits) | fraction)
}

fn string_value(text: &str) -> Result<Option<String>, eitri_base::OutOfMemory> {
    let (raw, text) = text.strip_prefix('r').map_or((false, text), |s| (true, s));
    if text.starts_with("\"\"\"") {
        return Ok(None);
    }
    let Some(inner) = text.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return Ok(None);
    };
    let mut out = String::new();
    out.try_reserve_exact(inner.len())
        .map_err(|_| eitri_base::OutOfMemory)?;
    if raw {
        out.push_str(inner);
        return Ok(Some(out));
    }
    let mut chars = inner.chars().peekable();
    let valid = (|| -> Option<()> {
        while let Some(c) = chars.next() {
            if c == '$' && chars.peek() == Some(&'{') {
                return None;
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            out.push(match chars.next()? {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '0' => '\0',
                '\\' => '\\',
                '"' => '"',
                '$' => '$',
                'u' => {
                    if chars.next()? != '{' {
                        return None;
                    }
                    let mut value = 0u32;
                    let mut count = 0;
                    loop {
                        let c = chars.next()?;
                        if c == '}' {
                            break;
                        }
                        value = value.checked_mul(16)?.checked_add(c.to_digit(16)?)?;
                        count += 1;
                    }
                    if count == 0 {
                        return None;
                    }
                    char::from_u32(value)?
                }
                _ => return None,
            });
        }
        Some(())
    })();
    Ok(valid.map(|()| out))
}

fn char_value(text: &str) -> Option<char> {
    let inner = text.strip_prefix('\'')?.strip_suffix('\'')?;
    if !inner.starts_with('\\') {
        return inner.chars().next();
    }
    Some(match &inner[1..] {
        "n" => '\n',
        "r" => '\r',
        "t" => '\t',
        "0" => '\0',
        "$" => '$',
        "\\" => '\\',
        "\'" => '\'',
        "\"" => '\"',
        escape if escape.starts_with("u{") => char::from_u32(
            u32::from_str_radix(escape.strip_prefix("u{")?.strip_suffix('}')?, 16).ok()?,
        )?,
        _ => return None,
    })
}

/// The value of an integer literal's spelling as an `i64`, with the sign of
/// a folded prefix `-`; `None` if it does not fit. The lexer has already
/// validated the spelling.
pub fn int_value(spelling: &str, negative: bool) -> Option<i64> {
    // Fast path: plain decimal of at most 18 digits cannot overflow `u64`
    // arithmetic or `i64`, so no wide math or per-digit checks are needed.
    let bytes = spelling.as_bytes();
    if bytes.len() <= 18 && bytes.iter().all(u8::is_ascii_digit) {
        let mut value: i64 = 0;
        for &b in bytes {
            value = value * 10 + i64::from(b - b'0');
        }
        return Some(if negative { -value } else { value });
    }
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

#[cfg(test)]
pub(crate) mod reference;

#[cfg(test)]
mod string_literal_tests {
    use super::string_value;

    #[test]
    fn decodes_single_line_strings_and_rejects_interpolation() {
        assert_eq!(
            string_value(r#""hello\n\u{2603}""#).unwrap().as_deref(),
            Some("hello\n☃")
        );
        assert_eq!(
            string_value(r#"r"\n${raw}""#).unwrap().as_deref(),
            Some(r"\n${raw}")
        );
        assert_eq!(string_value(r#""${value}""#).unwrap(), None);
        assert_eq!(
            string_value(r#""\${value}""#).unwrap().as_deref(),
            Some("${value}")
        );
    }
}
