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
    SelfField(BindingId, u32),
    Function(FnId),
    Method(FnId),
    MutMethod(FnId, BindingId),
    BareMethod(FnId, BindingId),
    BareMutMethod(FnId, BindingId),
    Host(HostFnId),
    StringLen,
    StringIsEmpty,
    StringSlice,
    StringContains,
    StringStarts,
    StringEnds,
    StringFind,
    StringReplace,
    StringUpper,
    StringLower,
    ForChars,
    ForLines,
    StringTrim,
    StringTrimStart,
    StringTrimEnd,
    StringConvert(Ty),
    StringLiteral(u32),
    Struct(eitri_syntax::ast::StructId),
    EnumCase(eitri_syntax::ast::EnumId, u32),
    Field(u32),
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
    pub optional_wrap: IndexVec<ExprId, bool>,
    /// For `let`: the declared binding; for assignment: the target.
    pub stmt_bindings: IndexVec<StmtId, Option<BindingId>>,
    /// Field index for a bare receiver-field assignment.
    pub stmt_fields: IndexVec<StmtId, Option<u32>>,
    pub signatures: IndexVec<FnId, Signature>,
    pub struct_fields: IndexVec<eitri_syntax::ast::StructId, List<Ty>>,
    pub struct_widths: IndexVec<eitri_syntax::ast::StructId, u32>,
    pub enum_starts: IndexVec<eitri_syntax::ast::EnumId, u32>,
    pub enum_widths: IndexVec<eitri_syntax::ast::EnumId, u32>,
    pub enum_case_types: Vec<List<Ty>>,
    pub pattern_bindings: IndexVec<TokenId, Option<BindingId>>,
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
        self.optional_wrap.clear();
        self.stmt_bindings.clear();
        self.stmt_fields.clear();
        self.signatures.clear();
        self.struct_fields.clear();
        self.struct_widths.clear();
        self.enum_starts.clear();
        self.enum_widths.clear();
        self.enum_case_types.clear();
        self.pattern_bindings.clear();
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
    struct_names: Vec<Option<eitri_syntax::ast::StructId>>,
    enum_names: Vec<Option<eitri_syntax::ast::EnumId>>,
    struct_states: Vec<u8>,
    struct_stack: Vec<(Ty, usize, usize, u32)>,
    methods: Vec<FnId>,
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
            + c.optional_wrap.capacity() * size_of::<bool>()
            + c.stmt_bindings.capacity() * size_of::<Option<BindingId>>()
            + c.stmt_fields.capacity() * size_of::<Option<u32>>()
            + c.signatures.capacity() * size_of::<Signature>()
            + c.struct_fields.capacity() * size_of::<List<Ty>>()
            + c.struct_widths.capacity() * size_of::<u32>()
            + c.enum_starts.capacity() * size_of::<u32>()
            + c.enum_widths.capacity() * size_of::<u32>()
            + c.enum_case_types.capacity() * size_of::<List<Ty>>()
            + c.pattern_bindings.capacity() * size_of::<Option<BindingId>>()
            + c.param_types.heap_bytes()
            + c.params.capacity() * size_of::<IdRange<BindingId>>()
            + c.ints.capacity() * size_of::<i64>()
            + c.strings.capacity() * size_of::<String>()
            + c.strings.iter().map(String::capacity).sum::<usize>()
            + c.imports.capacity() * size_of::<Option<HostModuleId>>()
            + self.heads.capacity() * size_of::<Option<Name>>()
            + self.struct_names.capacity() * size_of::<Option<eitri_syntax::ast::StructId>>()
            + self.enum_names.capacity() * size_of::<Option<eitri_syntax::ast::EnumId>>()
            + self.struct_states.capacity() * size_of::<u8>()
            + self.struct_stack.capacity() * size_of::<(Ty, usize, usize, u32)>()
            + self.methods.capacity() * size_of::<FnId>()
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
        c.optional_wrap
            .reserve_budgeted(count.saturating_sub(c.optional_wrap.len()), budget)?;
        c.stmt_bindings
            .reserve_budgeted(count.saturating_sub(c.stmt_bindings.len()), budget)?;
        c.stmt_fields
            .reserve_budgeted(count.saturating_sub(c.stmt_fields.len()), budget)?;
        c.signatures
            .reserve_budgeted(count.saturating_sub(c.signatures.len()), budget)?;
        c.struct_fields
            .reserve_budgeted(count.saturating_sub(c.struct_fields.len()), budget)?;
        c.struct_widths
            .reserve_budgeted(count.saturating_sub(c.struct_widths.len()), budget)?;
        c.enum_starts
            .reserve_budgeted(count.saturating_sub(c.enum_starts.len()), budget)?;
        c.enum_widths
            .reserve_budgeted(count.saturating_sub(c.enum_widths.len()), budget)?;
        let enum_cases = count.saturating_sub(c.enum_case_types.len());
        budget.reserve_vec(&mut c.enum_case_types, enum_cases)?;
        c.pattern_bindings
            .reserve_budgeted(count.saturating_sub(c.pattern_bindings.len()), budget)?;
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
        reserve_total!(&mut self.struct_names, symbol_count);
        reserve_total!(&mut self.enum_names, symbol_count);
        reserve_total!(&mut self.struct_states, count);
        reserve_total!(&mut self.struct_stack, count);
        reserve_total!(&mut self.methods, count);
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
    scratch.struct_names.clear();
    scratch.enum_names.clear();
    scratch.enum_names.resize(input.interner.len(), None);
    scratch.struct_states.clear();
    scratch.struct_stack.clear();
    scratch.methods.clear();
    scratch.struct_names.resize(input.interner.len(), None);
    scratch.undo.clear();
    scratch.spine.clear();
    scratch.links.clear();
    scratch.module_bindings.clear();
    scratch.unknown.clear();
    let ast = input.ast;
    scratch.checked.types.resize(ast.exprs.len(), Ty::Error);
    scratch.checked.optional_wrap.resize(ast.exprs.len(), false);
    scratch
        .checked
        .resolutions
        .resize(ast.exprs.len(), Res::None);
    scratch.checked.stmt_bindings.resize(ast.stmts.len(), None);
    scratch.checked.stmt_fields.resize(ast.stmts.len(), None);
    scratch
        .checked
        .pattern_bindings
        .resize(input.tokens.len(), None);
    let mut checker = Checker {
        input,
        diagnostics,
        out: &mut scratch.checked,
        heads: &mut scratch.heads,
        struct_names: &mut scratch.struct_names,
        enum_names: &mut scratch.enum_names,
        struct_states: &mut scratch.struct_states,
        struct_stack: &mut scratch.struct_stack,
        methods: &mut scratch.methods,
        undo: &mut scratch.undo,
        spine: &mut scratch.spine,
        links: &mut scratch.links,
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
    self_binding: Option<BindingId>,
}

struct Checker<'a, 'i> {
    input: &'a Input<'i>,
    diagnostics: &'a mut Diagnostics,
    out: &'a mut Checked,
    heads: &'a mut Vec<Option<Name>>,
    struct_names: &'a mut Vec<Option<eitri_syntax::ast::StructId>>,
    enum_names: &'a mut Vec<Option<eitri_syntax::ast::EnumId>>,
    struct_states: &'a mut Vec<u8>,
    struct_stack: &'a mut Vec<(Ty, usize, usize, u32)>,
    methods: &'a mut Vec<FnId>,
    undo: &'a mut Vec<(Symbol, Option<Name>)>,
    spine: &'a mut Vec<ExprId>,
    links: &'a mut Vec<IfLink>,
    module_bindings: &'a mut SymbolSet,
    /// Spellings already reported as unknown: a misspelled name is one
    /// mistake however often it is used (as while typing), so later uses
    /// are silent.
    unknown: &'a mut SymbolSet,
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
        match ty {
            TypeExpr::OptionalString(_) => return Ty::OptionalString,
            TypeExpr::OptionalI64(_) => return Ty::OptionalI64,
            TypeExpr::OptionalBool(_) => return Ty::OptionalBool,
            TypeExpr::OptionalNamed(token) => {
                let inner = self.ty(TypeExpr::Named(token), false);
                return inner.optional_of().unwrap_or(Ty::Error);
            }
            TypeExpr::OptionalScalar(token) => {
                return match type_name(self.symbol(token)) {
                    TypeName::Supported(inner) => inner.optional_of().unwrap_or(Ty::Error),
                    _ => Ty::Error,
                };
            }
            _ => {}
        }
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
            TypeName::Unknown
                if self
                    .enum_names
                    .get(self.symbol(token).index())
                    .copied()
                    .flatten()
                    .is_some() =>
            {
                Ty::Enum(self.enum_names[self.symbol(token).index()].unwrap())
            }
            TypeName::Unknown
                if self
                    .struct_names
                    .get(self.symbol(token).index())
                    .copied()
                    .flatten()
                    .is_some() =>
            {
                Ty::Struct(self.struct_names[self.symbol(token).index()].unwrap())
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

        for (id, decl) in ast.enums.iter_enumerated() {
            let symbol = self.symbol(decl.name);
            if symbol.index() >= self.enum_names.len() {
                self.enum_names.resize(symbol.index() + 1, None);
            }
            if self.enum_names[symbol.index()].replace(id).is_some() {
                self.report(
                    Code::DuplicateName,
                    "duplicate enum name",
                    self.token_span(decl.name),
                );
            }
        }
        // Struct names are indexed by interned symbol, so type lookup is O(1).
        for (id, decl) in ast.structs.iter_enumerated() {
            let symbol = self.symbol(decl.name);
            if symbol.index() >= self.struct_names.len() {
                self.struct_names.resize(symbol.index() + 1, None);
            }
            if self.struct_names[symbol.index()].replace(id).is_some()
                || self
                    .enum_names
                    .get(symbol.index())
                    .copied()
                    .flatten()
                    .is_some()
            {
                self.report(
                    Code::DuplicateName,
                    "duplicate struct name",
                    self.token_span(decl.name),
                );
            }
        }
        for (_, decl) in ast.enums.iter_enumerated() {
            let cases = &ast.enum_cases[decl.cases];
            self.out
                .enum_starts
                .push(self.out.enum_case_types.len() as u32);
            if cases.is_empty() || cases.len() > 256 {
                self.report(
                    Code::Limit,
                    "enum must have between 1 and 256 cases",
                    self.token_span(decl.name),
                );
                self.out
                    .enum_case_types
                    .extend(std::iter::repeat_n(List::EMPTY, cases.len()));
                self.out.enum_widths.push(1);
                continue;
            }
            let mut width = 1u32;
            for case in cases {
                if cases
                    .iter()
                    .filter(|other| self.symbol(other.name) == self.symbol(case.name))
                    .count()
                    != 1
                {
                    self.report(
                        Code::DuplicateName,
                        "duplicate enum case",
                        self.token_span(case.name),
                    );
                }
                let fields = ast.params(case.fields);
                if fields.len() > 256 {
                    self.report(
                        Code::Limit,
                        "implementation limit: more than 256 payload fields",
                        self.token_span(case.name),
                    );
                    self.out.enum_case_types.push(List::EMPTY);
                    continue;
                }
                let mut types = [Ty::Error; 256];
                for (index, field) in fields.iter().enumerate() {
                    let ty = self.ty(field.ty, false);
                    if fields
                        .iter()
                        .filter(|other| self.symbol(other.name) == self.symbol(field.name))
                        .count()
                        != 1
                    {
                        self.report(
                            Code::DuplicateName,
                            "duplicate payload field",
                            self.token_span(field.name),
                        );
                    }
                    types[index] = ty;
                }
                width = width.saturating_add(fields.len() as u32);
                self.out
                    .enum_case_types
                    .push(self.out.param_types.push(&types[..fields.len()]));
            }
            if width > 65_536 {
                self.report(
                    Code::Limit,
                    "implementation limit: enum exceeds 65536 slots",
                    self.token_span(decl.name),
                );
            }
            self.out.enum_widths.push(width);
        }
        // Field lookup is linear; cap fields so duplicate and initializer checking
        // remain O(256 * source fields), even for adversarial declarations.
        for (_, decl) in ast.structs.iter_enumerated() {
            if ast.params(decl.fields).len() > 256 {
                self.report(
                    Code::Limit,
                    "implementation limit: more than 256 struct fields",
                    self.token_span(decl.name),
                );
                self.out.struct_fields.push(List::EMPTY);
                continue;
            }
            let mut fields = [Ty::Error; 256];
            let mut field_count = 0;
            for field in ast.params(decl.fields) {
                let ty = self.ty(field.ty, false);
                if ast
                    .params(decl.fields)
                    .iter()
                    .filter(|other| self.symbol(other.name) == self.symbol(field.name))
                    .count()
                    != 1
                {
                    self.report(
                        Code::DuplicateName,
                        "duplicate struct field",
                        self.token_span(field.name),
                    );
                }
                fields[field_count] = ty;
                field_count += 1;
            }
            let list = self.out.param_types.push(&fields[..field_count]);
            self.out.struct_fields.push(list);
        }

        self.struct_layouts();
        for (id, decl) in ast.fns.iter_enumerated() {
            if decl.receiver.is_some() {
                self.methods.push(id);
            }
        }
        if self.methods.len() > 256 {
            self.report(
                Code::Limit,
                "implementation limit: more than 256 methods",
                Span::default(),
            );
            return;
        }

        // Every signature before any body, so calls may come first.
        for (id, decl) in ast.fns.iter_enumerated() {
            let mut params = Vec::new();
            if params
                .try_reserve_exact(
                    ast.params(decl.params).len() + usize::from(decl.receiver.is_some()),
                )
                .is_err()
            {
                self.report(
                    Code::OutOfMemory,
                    "runtime memory limit exceeded",
                    self.token_span(decl.name),
                );
                return;
            }
            if let Some(receiver) = decl.receiver {
                let ty = self.ty(TypeExpr::Named(receiver), false);
                if !matches!(ty, Ty::Struct(_) | Ty::Enum(_) | Ty::Error) {
                    self.report(
                        Code::TypeMismatch,
                        "extension requires a struct or enum",
                        self.token_span(receiver),
                    );
                }
                if matches!(ty, Ty::Enum(_)) && decl.mutating {
                    self.report(
                        Code::TypeMismatch,
                        "enum methods cannot be mutating",
                        self.token_span(decl.name),
                    );
                }
                if !decl.is_static {
                    params.push(ty);
                }
                let earlier = self
                    .methods
                    .iter()
                    .take_while(|&&method| method != id)
                    .map(|&method| &ast.fns[method])
                    .filter(|other| {
                        other
                            .receiver
                            .is_some_and(|r| self.symbol(r) == self.symbol(receiver))
                            && self.symbol(other.name) == self.symbol(decl.name)
                    })
                    .count();
                if earlier != 0 {
                    self.report(
                        Code::DuplicateName,
                        "duplicate method",
                        self.token_span(decl.name),
                    );
                }
            }
            for param in ast.params(decl.params) {
                let ty = self.ty(param.ty, false);
                params.push(ty);
            }
            let ret = self.ty(decl.ret, true);
            let slots = params
                .iter()
                .copied()
                .chain(std::iter::once(ret))
                .map(|ty| match ty {
                    Ty::Void => 0,
                    Ty::Struct(id) => self.out.struct_widths[id],
                    Ty::Enum(id) => self.out.enum_widths[id],
                    Ty::OptionalStruct(id) => 1 + self.out.struct_widths[id],
                    Ty::OptionalEnum(id) => 1 + self.out.enum_widths[id],
                    ty if ty.tagged_optional_inner().is_some() => 2,
                    _ => 1,
                })
                .fold(0u32, u32::saturating_add);
            let slots = slots.saturating_add(if decl.mutating {
                params.first().map_or(0, |ty| match ty {
                    Ty::Struct(id) => self.out.struct_widths[*id],
                    _ => 0,
                })
            } else {
                0
            });
            if slots > 65_536 {
                self.report(
                    Code::Limit,
                    "implementation limit: function signature exceeds 65536 slots",
                    self.token_span(decl.name),
                );
            }
            let params = self.out.param_types.push(&params);
            self.out.signatures.push(Signature { params, ret });
            if decl.receiver.is_some() {
                continue;
            }
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

        for (id, decl) in ast.structs.iter_enumerated() {
            for (index, field) in ast.params(decl.fields).iter().enumerate() {
                if let Some(default) = field.default {
                    if self.default_expression_supported(default) {
                        if let Some(&ty) =
                            self.out.param_types[self.out.struct_fields[id]].get(index)
                        {
                            self.value(default, Some(ty));
                        }
                    } else {
                        self.report(
                            Code::Unsupported,
                            "not supported yet: this field default expression",
                            self.span(default),
                        );
                    }
                }
            }
        }

        for item in &ast.items {
            match *item {
                Item::Fn(id) => self.function(id),
                Item::Stmt(stmt) => self.stmt(stmt),
            }
        }
    }

    /// Visit each default-expression node once using reusable compiler scratch.
    /// Struct and enum constructors are allowed; lowering bounds recursive
    /// default expansion. Names remain excluded until lexical captures are
    /// implemented.
    fn default_expression_supported(&mut self, root: ExprId) -> bool {
        let ast = self.input.ast;
        self.spine.clear();
        self.spine.push(root);
        while let Some(expr) = self.spine.pop() {
            match ast.exprs[expr] {
                Expr::Int { .. }
                | Expr::Float { .. }
                | Expr::Char(_)
                | Expr::String(_)
                | Expr::Bool(_)
                | Expr::Null(_) => {}
                Expr::Unary { operand, .. } => self.spine.push(operand),
                Expr::Binary { lhs, rhs, .. } => {
                    self.spine.push(lhs);
                    self.spine.push(rhs);
                }
                Expr::Call { callee, args }
                    if matches!(
                        ast.exprs[callee],
                        Expr::Name(_) | Expr::Member { .. } | Expr::EnumShorthand(_)
                    ) =>
                {
                    self.spine.extend_from_slice(ast.args(args))
                }
                Expr::Convert { value, .. } => self.spine.push(value),
                Expr::EnumShorthand(_) => {}
                Expr::Member { base, .. } if matches!(ast.exprs[base], Expr::Name(token) if self.enum_names.get(self.symbol(token).index()).copied().flatten().is_some()) =>
                    {}
                Expr::NamedCall { callee, args }
                    if matches!(
                        ast.exprs[callee],
                        Expr::EnumShorthand(_) | Expr::Member { .. }
                    ) =>
                {
                    self.spine
                        .extend(ast.field_inits[args].iter().map(|field| field.value));
                }
                Expr::StructLiteral { name, fields } => {
                    let Some(id) = self
                        .struct_names
                        .get(self.symbol(name).index())
                        .copied()
                        .flatten()
                    else {
                        self.spine.clear();
                        return false;
                    };
                    let entries = &ast.field_inits[fields];
                    if ast.params(ast.structs[id].fields).len() > 256 {
                        self.spine.clear();
                        return false;
                    }
                    self.spine.extend(entries.iter().map(|field| field.value));
                }
                _ => {
                    self.spine.clear();
                    return false;
                }
            }
        }
        true
    }

    /// Iterative DFS traverses each field once, O(declarations + fields)
    /// time and O(declarations) scratch; no source depth consumes Rust stack.
    fn struct_layouts(&mut self) {
        let structs = self.input.ast.structs.len();
        let enums = self.input.ast.enums.len();
        self.struct_states.resize(structs + enums, 0);
        self.out.struct_widths.resize(structs, 1);
        for root in 0..structs + enums {
            if self.struct_states[root] != 0 {
                continue;
            }
            let ty = if root < structs {
                Ty::Struct(eitri_syntax::ast::StructId::new(root))
            } else {
                Ty::Enum(eitri_syntax::ast::EnumId::new(root - structs))
            };
            self.struct_states[root] = 1;
            self.struct_stack
                .push((ty, 0, 0, u32::from(matches!(ty, Ty::Enum(_)))));
            while let Some(&(node, case, field, width)) = self.struct_stack.last() {
                let (index, name, list) = match node {
                    Ty::Struct(id) => (
                        id.index(),
                        self.input.ast.structs[id].name,
                        Some(self.out.struct_fields[id]),
                    ),
                    Ty::Enum(id) => {
                        let cases = &self.input.ast.enum_cases[self.input.ast.enums[id].cases];
                        (
                            structs + id.index(),
                            self.input.ast.enums[id].name,
                            (case < cases.len()).then(|| {
                                self.out.enum_case_types[self.out.enum_starts[id] as usize + case]
                            }),
                        )
                    }
                    _ => unreachable!(),
                };
                let Some(list) = list else {
                    if let Ty::Enum(id) = node {
                        self.out.enum_widths[id] = width;
                    }
                    self.struct_states[index] = 2;
                    self.struct_stack.pop();
                    continue;
                };
                let fields = &self.out.param_types[list];
                if field == fields.len() {
                    if matches!(node, Ty::Enum(_)) {
                        *self.struct_stack.last_mut().unwrap() = (node, case + 1, 0, width);
                        continue;
                    }
                    if let Ty::Struct(id) = node {
                        self.out.struct_widths[id] = width.max(1);
                    }
                    self.struct_states[index] = 2;
                    self.struct_stack.pop();
                    continue;
                }
                let child = fields[field];
                let child_node = child
                    .tagged_optional_inner()
                    .filter(|ty| matches!(ty, Ty::Struct(_) | Ty::Enum(_)))
                    .unwrap_or(child);
                let child_index = match child_node {
                    Ty::Struct(id) => Some(id.index()),
                    Ty::Enum(id) => Some(structs + id.index()),
                    _ => None,
                };
                if let Some(child_index) = child_index
                    && self.struct_states[child_index] == 0
                {
                    self.struct_states[child_index] = 1;
                    self.struct_stack.push((
                        child_node,
                        0,
                        0,
                        u32::from(matches!(child_node, Ty::Enum(_))),
                    ));
                    continue;
                }
                let field_width = if let Some(child_index) = child_index
                    && self.struct_states[child_index] == 1
                {
                    self.report(
                        Code::TypeMismatch,
                        "recursive inline struct type",
                        self.token_span(name),
                    );
                    1
                } else {
                    match child {
                        Ty::Struct(id) => self.out.struct_widths[id],
                        Ty::Enum(id) => self.out.enum_widths[id],
                        Ty::OptionalStruct(id) => 1 + self.out.struct_widths[id],
                        Ty::OptionalEnum(id) => 1 + self.out.enum_widths[id],
                        ty if ty.tagged_optional_inner().is_some() => 2,
                        _ => 1,
                    }
                };
                let total = width.saturating_add(field_width);
                if total > 65_536 {
                    self.report(
                        Code::Limit,
                        "implementation limit: aggregate exceeds 65536 slots",
                        self.token_span(name),
                    );
                }
                *self.struct_stack.last_mut().unwrap() = (node, case, field + 1, total.min(65_537));
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
            self_binding: None,
        });
        let mark = self.mark();
        let first = self.out.bindings.next_index();
        let offset = usize::from(decl.receiver.is_some() && !decl.is_static);
        if let Some(receiver) = decl.receiver.filter(|_| !decl.is_static) {
            let binding = self.out.bindings.push(Binding {
                name: receiver,
                mutable: decl.mutating,
                ty: self.out.param_types[signature.params][0],
                owner: Some(id),
            });
            self.current.as_mut().unwrap().self_binding = Some(binding);
        }
        for (index, param) in ast.params(decl.params).iter().enumerate() {
            let ty = self.out.param_types[signature.params][index + offset];
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
            Stmt::Expr(expr) => match ast.exprs[expr] {
                Expr::Match(id) => {
                    let arms = &ast.match_arms[ast.matches[id].arms];
                    !arms.is_empty()
                        && arms.iter().all(|arm| match arm.body {
                            eitri_syntax::ast::MatchBody::Block(block) => self.returns(block),
                            eitri_syntax::ast::MatchBody::Expr(_) => false,
                        })
                }
                _ => self.if_returns(expr),
            },
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
            Stmt::AssignField {
                target, op, value, ..
            } => {
                let ty = self.value(target, None);
                let Expr::Member { base, .. } = self.ast().exprs[target] else {
                    unreachable!()
                };
                let mut root = base;
                while let Expr::Member { base, .. } = self.ast().exprs[root] {
                    root = base;
                }
                let binding = match self.ast().exprs[root] {
                    Expr::Name(_) | Expr::SelfValue(_) => match self.out.resolutions[root] {
                        Res::Binding(b) | Res::SelfField(b, _) => Some(b),
                        _ => None,
                    },
                    _ => None,
                };
                if !binding.is_some_and(|b| self.out.bindings[b].mutable) {
                    self.report(
                        Code::AssignToImmutable,
                        "cannot assign through an immutable value",
                        self.span(target),
                    );
                }
                self.check_readonly_path(target);
                if let AssignOp::Compound(binary) = op
                    && !valid_operand(binary, ty)
                {
                    self.mismatch(ty, Ty::I64, self.span(target));
                }
                if matches!(op, AssignOp::Compound(_)) && ty == Ty::String {
                    self.report(
                        Code::Unsupported,
                        "not supported yet: compound assignment to string fields",
                        self.span(target),
                    );
                }
                let shifts = matches!(op, AssignOp::Compound(BinaryOp::Shl | BinaryOp::Shr));
                let rhs = self.value(value, if shifts { None } else { Some(ty) });
                if shifts && rhs != Ty::Error && !rhs.integer() {
                    self.mismatch(rhs, Ty::I64, self.span(value));
                }
                self.out.stmt_bindings[id] = binding;
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
        let field = self.receiver_field(target);
        let resolved = if let Some((binding, _, _)) = field {
            Some(Name::Binding(binding))
        } else if self.input.tokens.kind(target) == eitri_syntax::TokenKind::SelfLower {
            self.current.and_then(|f| f.self_binding).map(Name::Binding)
        } else {
            self.resolve(target)
        };
        let binding = match resolved {
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
        if let Some((binding, index, _)) = field {
            let Ty::Struct(owner) = self.out.bindings[binding].ty else {
                unreachable!()
            };
            if self.input.ast.params(self.input.ast.structs[owner].fields)[index as usize].readonly
            {
                self.report(
                    Code::AssignToImmutable,
                    "cannot assign through a read-only field",
                    self.token_span(target),
                );
            }
            self.out.stmt_fields[stmt] = Some(index);
        }
        let ty = field.map_or_else(
            || binding.map_or(Ty::Error, |b| self.out.bindings[b].ty),
            |(_, _, ty)| ty,
        );
        let shifts = matches!(op, AssignOp::Compound(BinaryOp::Shl | BinaryOp::Shr));
        if let AssignOp::Compound(binary) = op
            && !valid_operand(binary, ty)
        {
            self.mismatch(ty, Ty::I64, self.token_span(target));
        }
        if field.is_some() && matches!(op, AssignOp::Compound(_)) && ty == Ty::String {
            self.report(
                Code::Unsupported,
                "not supported yet: compound assignment to string fields",
                self.token_span(target),
            );
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
        let guidance = if matches!(
            self.input.ast.exprs[expr],
            Expr::Int { .. }
                | Expr::Float { .. }
                | Expr::StructLiteral { .. }
                | Expr::EnumShorthand(_)
                | Expr::Call { .. }
                | Expr::NamedCall { .. }
        ) {
            expected.and_then(Ty::tagged_optional_inner).or(expected)
        } else {
            expected
        };
        let found = self.expr(expr, guidance, true);
        if let Some(expected) = expected
            && (found.optional_of() == Some(expected)
                || (expected == Ty::OptionalI64 && found == Ty::FindOffset))
        {
            self.out.optional_wrap[expr] = true;
            return expected;
        }
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
            Expr::StringPart(span) => {
                match string_fragment(&self.input.text[span.range()], false) {
                    Ok(Some(value)) => {
                        self.out.resolutions[expr] =
                            Res::StringLiteral(self.out.strings.len() as u32);
                        self.out.strings.push(value);
                        Ty::String
                    }
                    Ok(None) => {
                        self.report(Code::Lexical, "invalid string escape", span);
                        Ty::Error
                    }
                    Err(_) => {
                        self.report(Code::OutOfMemory, "runtime memory limit exceeded", span);
                        Ty::Error
                    }
                }
            }
            Expr::Interpolation(parts) => {
                for &part in self.input.ast.args(parts) {
                    let ty = self.value(part, None);
                    if !(ty.numeric() || matches!(ty, Ty::String | Ty::Bool | Ty::Char | Ty::Error))
                    {
                        self.report(
                            Code::TypeMismatch,
                            "interpolated value is not formattable",
                            self.span(part),
                        );
                    }
                }
                Ty::String
            }
            Expr::Convert { callee, value } => self.conversion(expr, callee, &[value], false),
            Expr::Bool(_) => Ty::Bool,
            Expr::Null(token) => {
                if expected.is_some_and(|ty| {
                    ty == Ty::OptionalString || ty.tagged_optional_inner().is_some()
                }) {
                    expected.unwrap()
                } else {
                    self.report(
                        Code::Unsupported,
                        "not supported yet: `null`",
                        self.token_span(token),
                    );
                    Ty::Error
                }
            }
            Expr::SelfValue(token) => {
                if let Some(binding) = self.current.and_then(|f| f.self_binding) {
                    self.out.resolutions[expr] = Res::Binding(binding);
                    self.out.bindings[binding].ty
                } else {
                    self.report(
                        Code::Unsupported,
                        "not supported yet: `self` outside methods",
                        self.token_span(token),
                    );
                    Ty::Error
                }
            }
            Expr::Name(token) => self.name(expr, token),
            Expr::StructLiteral { name, fields } => self.struct_literal(expr, name, fields),
            Expr::EnumShorthand(name) => self.enum_value(expr, None, name, expected, None, None),
            Expr::NamedCall { callee, args } => self.enum_named_call(expr, callee, args, expected),
            Expr::Member { base, name } => {
                if let Expr::Name(token) = self.ast().exprs[base]
                    && let Some(id) = self
                        .enum_names
                        .get(self.symbol(token).index())
                        .copied()
                        .flatten()
                {
                    self.enum_value(expr, Some(id), name, expected, None, None)
                } else {
                    self.member_value(expr, base, name)
                }
            }
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
            Expr::Binary {
                op: BinaryOp::Coalesce,
                ..
            } => self.coalesce(expr, expected),
            Expr::Binary { .. } => self.binary_chain(expr, expected),
            Expr::Call { callee, args } => self.call(expr, callee, args, expected),
            Expr::If(id) => self.if_expr(expr, id, expected, used),
            Expr::Match(id) => self.match_expr(expr, id, expected, used),
            Expr::Error => Ty::Error,
        };
        self.out.types[expr] = ty;
        ty
    }

    fn literal(&mut self, expr: ExprId, word: u64) {
        self.out.resolutions[expr] = Res::Int(self.out.ints.len() as u32);
        self.out.ints.push(word as i64);
    }

    fn coalesce(&mut self, root: ExprId, expected: Option<Ty>) -> Ty {
        let mark = self.spine.len();
        let mut tail = root;
        while let Expr::Binary {
            op: BinaryOp::Coalesce,
            rhs,
            ..
        } = self.ast().exprs[tail]
        {
            tail = rhs;
        }
        let context = expected
            .and_then(|ty| {
                if ty == Ty::OptionalString || ty.tagged_optional_inner().is_some() {
                    Some(ty)
                } else {
                    ty.optional_of()
                }
            })
            .or_else(|| match self.ast().exprs[tail] {
                Expr::String(_) => Some(Ty::OptionalString),
                Expr::Int { .. } => Some(Ty::OptionalI64),
                Expr::Float { .. } => Some(Ty::OptionalF64),
                Expr::Bool(_) => Some(Ty::OptionalBool),
                Expr::Char(_) => Some(Ty::OptionalChar),
                _ => self.hint(tail).and_then(|ty| {
                    if ty == Ty::OptionalString || ty.tagged_optional_inner().is_some() {
                        Some(ty)
                    } else {
                        ty.optional_of()
                    }
                }),
            });
        let mut node = root;
        let mut source_kind = None;
        while let Expr::Binary {
            op: BinaryOp::Coalesce,
            lhs,
            rhs,
        } = self.ast().exprs[node]
        {
            self.spine.push(node);
            let hint = if matches!(self.ast().exprs[lhs], Expr::Null(_)) {
                context
            } else {
                None
            };
            let mut found = self.value(lhs, hint);
            if found == Ty::FindOffset && source_kind == Some(Ty::OptionalI64) {
                self.out.optional_wrap[lhs] = true;
                found = Ty::OptionalI64;
            } else if found == Ty::OptionalI64 && source_kind == Some(Ty::FindOffset) {
                for &node in &self.spine[mark..] {
                    if let Expr::Binary { lhs, .. } = self.input.ast.exprs[node]
                        && self.out.types[lhs] == Ty::FindOffset
                    {
                        self.out.optional_wrap[lhs] = true;
                    }
                }
                source_kind = Some(Ty::OptionalI64);
            }
            if !matches!(found, Ty::FindOffset | Ty::OptionalString | Ty::Error)
                && found.tagged_optional_inner().is_none()
            {
                self.report(
                    Code::TypeMismatch,
                    "left operand of `??` must be optional",
                    self.span(lhs),
                );
            }
            if let Some(previous) = source_kind
                && previous != found
                && found != Ty::Error
                && previous != Ty::Error
            {
                self.mismatch(found, previous, self.span(lhs));
            }
            source_kind = Some(found);
            node = rhs;
        }
        let source_kind = source_kind.unwrap_or(Ty::Error);
        let result = match source_kind {
            kind if kind == Ty::OptionalString || kind.tagged_optional_inner().is_some() => {
                let hint = match self.ast().exprs[node] {
                    Expr::Null(_) => Some(source_kind),
                    Expr::Int { .. } | Expr::Float { .. } => source_kind.tagged_optional_inner(),
                    _ => None,
                };
                let mut found = self.value(node, hint);
                if source_kind == Ty::OptionalI64 && found == Ty::FindOffset {
                    self.out.optional_wrap[node] = true;
                    found = Ty::OptionalI64;
                }
                let inner = if source_kind == Ty::OptionalString {
                    Ty::String
                } else {
                    source_kind.tagged_optional_inner().unwrap()
                };
                if found != inner && found != source_kind && found != Ty::Error {
                    self.mismatch(found, inner, self.span(node));
                }
                found
            }
            Ty::FindOffset => {
                let hint = if matches!(self.ast().exprs[node], Expr::Null(_)) {
                    Some(Ty::OptionalI64)
                } else {
                    None
                };
                let found = self.value(node, hint);
                if matches!(found, Ty::OptionalI64 | Ty::FindOffset) {
                    if found == Ty::FindOffset {
                        self.out.optional_wrap[node] = true;
                    }
                    for &node in &self.spine[mark..] {
                        if let Expr::Binary { lhs, .. } = self.input.ast.exprs[node]
                            && self.out.types[lhs] == Ty::FindOffset
                        {
                            self.out.optional_wrap[lhs] = true;
                        }
                    }
                    Ty::OptionalI64
                } else {
                    if found != Ty::I64 && found != Ty::Error {
                        self.mismatch(found, Ty::I64, self.span(node));
                    }
                    Ty::I64
                }
            }
            _ => {
                self.value(node, None);
                Ty::Error
            }
        };
        while self.spine.len() > mark {
            let node = self.spine.pop().expect("above mark");
            self.out.types[node] = result;
        }
        result
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
        if matches!(
            type_name(self.symbol(token)),
            TypeName::Supported(Ty::String)
        ) && checked
        {
            if args.len() != 1 {
                self.report(
                    Code::ArgumentCount,
                    format!("`string` conversion takes 1 argument, found {}", args.len()),
                    self.span(expr),
                );
                for &arg in args {
                    self.value(arg, None);
                }
                return Ty::Error;
            }
            let from = self.value(args[0], None);
            if !(from.numeric() || matches!(from, Ty::Bool | Ty::Char | Ty::String | Ty::Error)) {
                self.report(
                    Code::TypeMismatch,
                    "string conversion requires a formattable value",
                    self.span(args[0]),
                );
            }
            self.out.resolutions[expr] = Res::StringConvert(from);
            return Ty::String;
        }
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

    fn receiver_field(&self, token: TokenId) -> Option<(BindingId, u32, Ty)> {
        if matches!(self.lookup(self.symbol(token)), Some(Name::Binding(binding)) if self.out.bindings[binding].owner.is_some())
        {
            return None;
        }
        let binding = self.current?.self_binding?;
        let Ty::Struct(id) = self.out.bindings[binding].ty else {
            return None;
        };
        let fields = self.input.ast.params(self.input.ast.structs[id].fields);
        let index = fields
            .iter()
            .position(|field| self.symbol(field.name) == self.symbol(token))?;
        let ty = *self.out.param_types[self.out.struct_fields[id]].get(index)?;
        Some((binding, index as u32, ty))
    }

    fn name(&mut self, expr: ExprId, token: TokenId) -> Ty {
        if let Some((binding, index, ty)) = self.receiver_field(token) {
            self.out.resolutions[expr] = Res::SelfField(binding, index);
            return ty;
        }
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

    /// Pattern nesting is bounded by the parser's nesting limit. Each level
    /// scans at most 256 declared fields for each source pattern.
    fn check_match_payload(
        &mut self,
        enum_id: eitri_syntax::ast::EnumId,
        case_index: usize,
        case: TokenId,
        bindings_list: List<eitri_syntax::ast::MatchBinding>,
        parenthesized: bool,
    ) {
        let fields = self
            .ast()
            .params(self.ast().enum_cases[self.ast().enums[enum_id].cases][case_index].fields);
        let bindings = &self.ast().match_bindings[bindings_list];
        if parenthesized && bindings.len() != fields.len() {
            self.report(
                Code::ArgumentCount,
                "pattern payload arity mismatch",
                self.token_span(case),
            );
        }
        let types = self.out.enum_case_types[self.out.enum_starts[enum_id] as usize + case_index];
        let mut seen = [false; 256];
        let mut labeled = false;
        for (i, pattern) in bindings.iter().enumerate() {
            let index = if let Some(label) = pattern.label {
                labeled = true;
                match fields
                    .iter()
                    .take(256)
                    .position(|field| self.symbol(field.name) == self.symbol(label))
                {
                    Some(index) => index,
                    None => {
                        self.report(
                            Code::UnknownName,
                            "unknown enum payload field",
                            self.token_span(label),
                        );
                        continue;
                    }
                }
            } else {
                if labeled {
                    self.report(
                        Code::TypeMismatch,
                        "positional pattern follows a labeled pattern",
                        self.token_span(pattern.name),
                    );
                }
                i
            };
            if index < fields.len()
                && index < seen.len()
                && std::mem::replace(&mut seen[index], true)
            {
                self.report(
                    Code::DuplicateName,
                    "duplicate enum payload field pattern",
                    self.token_span(pattern.label.unwrap_or(pattern.name)),
                );
            }
            let ty = self.out.param_types[types]
                .get(index)
                .copied()
                .unwrap_or(Ty::Error);
            if let Some(case) = pattern.nested_case {
                if let Ty::Enum(nested) | Ty::OptionalEnum(nested) = ty {
                    if let Some(qualifier) = pattern.nested_qualifier
                        && self.symbol(qualifier) != self.symbol(self.ast().enums[nested].name)
                    {
                        self.report(
                            Code::TypeMismatch,
                            "nested enum pattern qualifier does not match payload",
                            self.token_span(qualifier),
                        );
                    }
                    let cases = &self.ast().enum_cases[self.ast().enums[nested].cases];
                    if let Some(nested_index) = cases
                        .iter()
                        .take(256)
                        .position(|candidate| self.symbol(candidate.name) == self.symbol(case))
                    {
                        if let Some(children) = pattern.nested_bindings {
                            self.check_match_payload(nested, nested_index, case, children, true);
                        }
                    } else {
                        self.report(
                            Code::UnknownName,
                            "unknown nested enum case",
                            self.token_span(case),
                        );
                    }
                } else if ty != Ty::Error {
                    self.report(
                        Code::TypeMismatch,
                        "nested case pattern requires an enum payload",
                        self.token_span(case),
                    );
                }
                continue;
            }
            let narrowed = if ty == Ty::OptionalString {
                Ty::String
            } else {
                ty.tagged_optional_inner().unwrap_or(ty)
            };
            if let Some(literal) = pattern.literal {
                if matches!(self.ast().exprs[literal], Expr::Null(_)) {
                    self.value(literal, Some(ty));
                    continue;
                }
                if !matches!(
                    self.ast().exprs[literal],
                    Expr::Int { .. }
                        | Expr::Float { .. }
                        | Expr::Bool(_)
                        | Expr::Char(_)
                        | Expr::String(_)
                ) {
                    self.report(
                        Code::Unsupported,
                        "not supported yet: this literal pattern",
                        self.span(literal),
                    );
                } else {
                    self.value(literal, Some(narrowed));
                }
                continue;
            }
            if self.input.tokens.kind(pattern.name) == eitri_syntax::TokenKind::Wildcard {
                continue;
            }
            let binding = self.new_binding(pattern.name, false, narrowed);
            self.out.pattern_bindings[pattern.name] = Some(binding);
        }
    }

    fn payload_patterns_irrefutable(
        &self,
        enum_id: eitri_syntax::ast::EnumId,
        case_index: usize,
        bindings: List<eitri_syntax::ast::MatchBinding>,
    ) -> bool {
        let ast = self.input.ast;
        let fields = ast.params(ast.enum_cases[ast.enums[enum_id].cases][case_index].fields);
        let types = self.out.enum_case_types[self.out.enum_starts[enum_id] as usize + case_index];
        for (position, pattern) in ast.match_bindings[bindings].iter().enumerate() {
            if pattern.literal.is_some() || pattern.nested_case.is_some() {
                return false;
            }
            if self.input.tokens.kind(pattern.name) == eitri_syntax::TokenKind::Wildcard {
                continue;
            }
            let index = match pattern.label {
                Some(label) => match fields
                    .iter()
                    .take(256)
                    .position(|field| self.symbol(field.name) == self.symbol(label))
                {
                    Some(index) => index,
                    None => continue,
                },
                None => position,
            };
            if self.out.param_types[types]
                .get(index)
                .is_some_and(|ty| *ty == Ty::OptionalString || ty.tagged_optional_inner().is_some())
            {
                return false;
            }
        }
        true
    }

    /// Recognize a single finite restriction, keeping coverage storage bounded
    /// to 256 cases and 256 values per case. Other shapes remain conservative.
    fn finite_case_pattern(
        &self,
        enum_id: eitri_syntax::ast::EnumId,
        case_index: usize,
        bindings: List<eitri_syntax::ast::MatchBinding>,
    ) -> Option<(usize, [u64; 4], usize)> {
        let ast = self.input.ast;
        let case = ast.enum_cases[ast.enums[enum_id].cases][case_index];
        let fields = ast.params(case.fields);
        let types = self.out.enum_case_types[self.out.enum_starts[enum_id] as usize + case_index];
        let mut restriction = None;
        for (position, pattern) in ast.match_bindings[bindings].iter().enumerate() {
            let field = match pattern.label {
                Some(label) => fields
                    .iter()
                    .take(256)
                    .position(|field| self.symbol(field.name) == self.symbol(label))?,
                None => position,
            };
            let ty = *self.out.param_types[types].get(field)?;
            let mut mask = [0u64; 4];
            let count = if let Some(literal) = pattern.literal {
                match (ty, ast.exprs[literal]) {
                    (Ty::Bool, Expr::Bool(value)) => {
                        mask[0] = 1 << usize::from(value);
                        Some(2)
                    }
                    (Ty::OptionalBool, Expr::Bool(value)) => {
                        mask[0] = 1 << (1 + usize::from(value));
                        Some(3)
                    }
                    (Ty::OptionalBool, Expr::Null(_)) => {
                        mask[0] = 1;
                        Some(3)
                    }
                    (Ty::OptionalEnum(id), Expr::Null(_)) => {
                        let count = ast.enum_cases[ast.enums[id].cases].len() + 1;
                        if count > 256 {
                            return None;
                        }
                        mask[0] = 1;
                        Some(count)
                    }
                    (ty, Expr::Null(_))
                        if ty == Ty::OptionalString || ty.tagged_optional_inner().is_some() =>
                    {
                        mask[0] = 1;
                        Some(2)
                    }
                    _ => return None,
                }
            } else if let Some(name) = pattern.nested_case {
                let (nested, extra) = match ty {
                    Ty::Enum(id) => (id, 0),
                    Ty::OptionalEnum(id) => (id, 1),
                    _ => return None,
                };
                let cases = &ast.enum_cases[ast.enums[nested].cases];
                if cases.is_empty() || cases.len() + extra > 256 {
                    return None;
                }
                let nested_index = cases
                    .iter()
                    .position(|case| self.symbol(case.name) == self.symbol(name))?;
                if let Some(children) = pattern.nested_bindings
                    && !self.payload_patterns_irrefutable(nested, nested_index, children)
                {
                    return None;
                }
                let bit = nested_index + extra;
                mask[bit / 64] = 1u64 << (bit % 64);
                Some(cases.len() + extra)
            } else if self.input.tokens.kind(pattern.name) != eitri_syntax::TokenKind::Wildcard {
                match ty {
                    Ty::OptionalBool => {
                        mask[0] = 0b110;
                        Some(3)
                    }
                    Ty::OptionalEnum(id) => {
                        let count = ast.enum_cases[ast.enums[id].cases].len() + 1;
                        if count > 256 {
                            return None;
                        }
                        for bit in 1..count {
                            mask[bit / 64] |= 1u64 << (bit % 64);
                        }
                        Some(count)
                    }
                    ty if ty == Ty::OptionalString || ty.tagged_optional_inner().is_some() => {
                        mask[0] = 0b10;
                        Some(2)
                    }
                    _ => None,
                }
            } else {
                None
            };
            if let Some(count) = count {
                if restriction.is_some() {
                    return None;
                }
                restriction = Some((field, mask, count));
            }
        }
        restriction
    }

    fn match_expr(
        &mut self,
        expr: ExprId,
        id: eitri_syntax::ast::MatchId,
        expected: Option<Ty>,
        used: bool,
    ) -> Ty {
        let node = self.ast().matches[id];
        let subject = self.value(node.subject, None);
        let (enum_id, optional) = match subject {
            Ty::Enum(id) => (id, false),
            Ty::OptionalEnum(id) => (id, true),
            _ => return self.scalar_match(expr, id, subject, expected, used),
        };
        let cases = &self.ast().enum_cases[self.ast().enums[enum_id].cases];
        if cases.len() > 256 {
            return Ty::Error;
        }
        let mut covered = [false; 256];
        // Sparse, stack-bounded tracking avoids 8 KiB for every nested match.
        // At most 32 case/field groups are tracked in one match.
        let mut finite = [(usize::MAX, usize::MAX, [0u64; 4]); 32];
        let mut finite_limit = false;
        let mut fallback = false;
        let mut null_covered = false;
        let mut result = expected;
        for arm in &self.ast().match_arms[node.arms] {
            let mark = self.mark();
            if let Some(literal) = arm.literal {
                if optional && matches!(self.ast().exprs[literal], Expr::Null(_)) {
                    self.value(literal, Some(subject));
                    if arm.guard.is_none() {
                        null_covered = true;
                    }
                } else {
                    self.report(
                        Code::TypeMismatch,
                        "literal pattern does not match enum",
                        self.span(literal),
                    );
                }
            } else if let Some(case) = arm.case {
                if let Some(qualifier) = arm.qualifier
                    && self.symbol(qualifier) != self.symbol(self.ast().enums[enum_id].name)
                {
                    self.report(
                        Code::TypeMismatch,
                        "enum pattern qualifier does not match subject",
                        self.token_span(qualifier),
                    );
                }
                if let Some(index) = cases
                    .iter()
                    .position(|item| self.symbol(item.name) == self.symbol(case))
                {
                    if arm.guard.is_none()
                        && self.payload_patterns_irrefutable(enum_id, index, arm.bindings)
                    {
                        covered[index] = true;
                    }
                    self.check_match_payload(enum_id, index, case, arm.bindings, arm.parenthesized);
                    if arm.guard.is_none()
                        && let Some((field, mask, count)) =
                            self.finite_case_pattern(enum_id, index, arm.bindings)
                    {
                        let group = finite
                            .iter()
                            .position(|(case, tracked, _)| *case == index && *tracked == field)
                            .or_else(|| finite.iter().position(|(case, _, _)| *case == usize::MAX));
                        if let Some(group) = group {
                            let (case, tracked, bits) = &mut finite[group];
                            *case = index;
                            *tracked = field;
                            for (bits, mask) in bits.iter_mut().zip(mask) {
                                *bits |= mask;
                            }
                            if (0..count).all(|n| bits[n / 64] & (1u64 << (n % 64)) != 0) {
                                covered[index] = true;
                            }
                        } else {
                            finite_limit = true;
                        }
                    }
                } else {
                    self.report(
                        Code::UnknownName,
                        "unknown enum case",
                        self.token_span(case),
                    );
                }
            } else {
                if arm.parenthesized {
                    self.report(
                        Code::TypeMismatch,
                        "wildcard pattern cannot have payload bindings",
                        self.span(expr),
                    );
                }
                if let Some(token) = arm.catch_binding {
                    let binding = self.new_binding(token, false, Ty::Enum(enum_id));
                    self.out.pattern_bindings[token] = Some(binding);
                }
                if arm.guard.is_none() {
                    if optional && arm.catch_binding.is_some() {
                        covered[..cases.len()].fill(true);
                    } else {
                        fallback = true;
                    }
                }
            }
            if let Some(guard) = arm.guard {
                self.value(guard, Some(Ty::Bool));
            }
            let ty = match arm.body {
                eitri_syntax::ast::MatchBody::Expr(body) => {
                    if used {
                        self.value(body, result)
                    } else {
                        self.expr(body, None, false);
                        Ty::Void
                    }
                }
                eitri_syntax::ast::MatchBody::Block(body) => {
                    if used {
                        self.block_value(body, result)
                    } else {
                        self.statements(body);
                        Ty::Void
                    }
                }
            };
            if used && result.is_none() && ty != Ty::Error {
                result = Some(ty);
            }
            self.restore(mark);
        }
        if cases.len() > 256
            || (!fallback
                && ((optional && !null_covered) || covered[..cases.len()].iter().any(|seen| !seen)))
        {
            if finite_limit {
                self.report(
                    Code::Limit,
                    "implementation limit: more than 32 finite pattern groups",
                    self.span(expr),
                );
            } else {
                self.report(Code::TypeMismatch, "non-exhaustive match", self.span(expr));
            }
        }
        if used {
            result.unwrap_or(Ty::Error)
        } else {
            Ty::Void
        }
    }

    fn scalar_match(
        &mut self,
        expr: ExprId,
        id: eitri_syntax::ast::MatchId,
        subject: Ty,
        expected: Option<Ty>,
        used: bool,
    ) -> Ty {
        let node = self.ast().matches[id];
        if matches!(subject, Ty::Void | Ty::FindOffset) {
            self.report(
                Code::TypeMismatch,
                "match subject type is not supported",
                self.span(node.subject),
            );
            return Ty::Error;
        }
        let inner = if subject == Ty::OptionalString {
            Some(Ty::String)
        } else {
            subject.tagged_optional_inner()
        };
        let mut fallback = false;
        let mut present = false;
        let mut null = false;
        let mut booleans = [false; 2];
        let mut result = expected;
        for arm in &self.ast().match_arms[node.arms] {
            let mark = self.mark();
            if let Some(literal) = arm.literal {
                let is_null = matches!(self.ast().exprs[literal], Expr::Null(_));
                self.value(
                    literal,
                    Some(if is_null {
                        subject
                    } else {
                        inner.unwrap_or(subject)
                    }),
                );
                if arm.guard.is_none() && is_null && inner.is_some() {
                    null = true;
                }
                if arm.guard.is_none()
                    && let Expr::Bool(value) = self.ast().exprs[literal]
                    && inner.unwrap_or(subject) == Ty::Bool
                {
                    booleans[usize::from(value)] = true;
                }
            } else if let Some(case) = arm.case {
                self.report(
                    Code::TypeMismatch,
                    "enum case pattern requires an enum",
                    self.token_span(case),
                );
            } else {
                if arm.parenthesized {
                    self.report(
                        Code::TypeMismatch,
                        "wildcard pattern cannot have payload bindings",
                        self.span(expr),
                    );
                }
                if let Some(token) = arm.catch_binding {
                    let binding = self.new_binding(token, false, inner.unwrap_or(subject));
                    self.out.pattern_bindings[token] = Some(binding);
                }
                if arm.guard.is_none() {
                    if inner.is_some() && arm.catch_binding.is_some() {
                        present = true;
                    } else {
                        fallback = true;
                    }
                }
            }
            if let Some(guard) = arm.guard {
                self.value(guard, Some(Ty::Bool));
            }
            let ty = match arm.body {
                eitri_syntax::ast::MatchBody::Expr(body) => {
                    if used {
                        self.value(body, result)
                    } else {
                        self.expr(body, None, false);
                        Ty::Void
                    }
                }
                eitri_syntax::ast::MatchBody::Block(body) => {
                    if used {
                        self.block_value(body, result)
                    } else {
                        self.statements(body);
                        Ty::Void
                    }
                }
            };
            if used && result.is_none() && ty != Ty::Error {
                result = Some(ty);
            }
            self.restore(mark);
        }
        let bool_covered = inner.unwrap_or(subject) == Ty::Bool && booleans.iter().all(|x| *x);
        if !fallback
            && !(if inner.is_some() {
                null && (present || bool_covered)
            } else {
                bool_covered
            })
            && subject != Ty::Error
        {
            self.report(Code::TypeMismatch, "non-exhaustive match", self.span(expr));
        }
        if used {
            result.unwrap_or(Ty::Error)
        } else {
            Ty::Void
        }
    }

    fn enum_value(
        &mut self,
        expr: ExprId,
        owner: Option<eitri_syntax::ast::EnumId>,
        name: TokenId,
        expected: Option<Ty>,
        args: Option<List<eitri_syntax::ast::FieldInit>>,
        positional: Option<List<ExprId>>,
    ) -> Ty {
        let id = owner.or(match expected {
            Some(Ty::Enum(id)) => Some(id),
            _ => None,
        });
        let Some(id) = id else {
            self.report(
                Code::TypeMismatch,
                "enum case requires an expected type",
                self.span(expr),
            );
            return Ty::Error;
        };
        let cases = &self.ast().enum_cases[self.ast().enums[id].cases];
        let Some(index) = cases
            .iter()
            .position(|case| self.symbol(case.name) == self.symbol(name))
        else {
            self.report(
                Code::UnknownName,
                "unknown enum case",
                self.token_span(name),
            );
            return Ty::Error;
        };
        let fields = self.ast().params(cases[index].fields);
        let types = self.out.enum_case_types[self.out.enum_starts[id] as usize + index];
        if fields.len() > 256 {
            return Ty::Error;
        }
        let mut seen = [false; 256];
        if let Some(args) = args {
            let mut labeled = false;
            for (position, arg) in self.ast().field_inits[args].iter().enumerate() {
                let index = if self.input.tokens.kind(arg.name) == eitri_syntax::TokenKind::LParen {
                    if labeled {
                        self.report(
                            Code::TypeMismatch,
                            "positional payload argument follows a label",
                            self.span(arg.value),
                        );
                    }
                    (position < fields.len()).then_some(position)
                } else {
                    labeled = true;
                    fields
                        .iter()
                        .position(|field| self.symbol(field.name) == self.symbol(arg.name))
                };
                if let Some(index) = index {
                    if std::mem::replace(&mut seen[index], true) {
                        self.report(
                            Code::DuplicateName,
                            "duplicate payload initializer",
                            self.span(arg.value),
                        );
                    }
                    self.value(arg.value, Some(self.out.param_types[types][index]));
                } else {
                    self.report(
                        Code::UnknownName,
                        "unknown or excess payload argument",
                        self.span(arg.value),
                    );
                    self.value(arg.value, None);
                }
            }
        }
        if let Some(positional) = positional {
            let args = self.ast().args(positional);
            if args.len() > fields.len() {
                self.report(
                    Code::ArgumentCount,
                    "too many enum payload arguments",
                    self.span(expr),
                );
            }
            for (index, &arg) in args.iter().enumerate() {
                let expected = self.out.param_types[types].get(index).copied();
                self.value(arg, expected);
                if index < fields.len() {
                    seen[index] = true;
                }
            }
        }
        if fields.len() > 256 || seen[..fields.len()].iter().any(|seen| !seen) {
            self.report(
                Code::ArgumentCount,
                "missing enum payload field",
                self.span(expr),
            );
        }
        self.out.resolutions[expr] = Res::EnumCase(id, index as u32);
        Ty::Enum(id)
    }

    fn enum_named_call(
        &mut self,
        expr: ExprId,
        callee: ExprId,
        args: List<eitri_syntax::ast::FieldInit>,
        expected: Option<Ty>,
    ) -> Ty {
        match self.ast().exprs[callee] {
            Expr::EnumShorthand(name) => {
                self.enum_value(expr, None, name, expected, Some(args), None)
            }
            Expr::Member { base, name } => {
                if let Expr::Name(token) = self.ast().exprs[base]
                    && let Some(id) = self
                        .enum_names
                        .get(self.symbol(token).index())
                        .copied()
                        .flatten()
                {
                    self.enum_value(expr, Some(id), name, expected, Some(args), None)
                } else {
                    self.report(
                        Code::Unsupported,
                        "not supported yet: labeled arguments",
                        self.span(expr),
                    );
                    Ty::Error
                }
            }
            _ => {
                self.report(
                    Code::Unsupported,
                    "not supported yet: labeled arguments",
                    self.span(expr),
                );
                Ty::Error
            }
        }
    }

    fn struct_literal(
        &mut self,
        expr: ExprId,
        name: TokenId,
        fields: List<eitri_syntax::ast::FieldInit>,
    ) -> Ty {
        let Some(id) = self
            .struct_names
            .get(self.symbol(name).index())
            .copied()
            .flatten()
        else {
            self.report(
                Code::UnknownName,
                "unknown struct type",
                self.token_span(name),
            );
            return Ty::Error;
        };
        let decl = self.input.ast.structs[id];
        let declared = self.input.ast.params(decl.fields);
        let types = self.out.struct_fields[id];
        if declared.len() > 256 {
            return Ty::Error;
        }
        let mut seen = [false; 256];
        let entries = &self.input.ast.field_inits[fields];
        let base = entries
            .last()
            .filter(|field| self.input.tokens.kind(field.name) == eitri_syntax::TokenKind::DotDot)
            .map(|field| field.value);
        for field in entries
            .iter()
            .filter(|field| self.input.tokens.kind(field.name) != eitri_syntax::TokenKind::DotDot)
        {
            let index = declared
                .iter()
                .position(|f| self.symbol(f.name) == self.symbol(field.name));
            if let Some(index) = index {
                if std::mem::replace(&mut seen[index], true) {
                    self.report(
                        Code::DuplicateName,
                        "duplicate field initializer",
                        self.token_span(field.name),
                    );
                }
                self.value(field.value, Some(self.out.param_types[types][index]));
            } else {
                self.report(
                    Code::UnknownName,
                    "unknown struct field",
                    self.token_span(field.name),
                );
                self.value(field.value, None);
            }
        }
        if let Some(base) = base {
            self.value(base, Some(Ty::Struct(id)));
        }
        if base.is_none()
            && declared
                .iter()
                .enumerate()
                .any(|(index, field)| !seen[index] && field.default.is_none())
        {
            self.report(Code::TypeMismatch, "missing struct field", self.span(expr));
        }
        self.out.resolutions[expr] = Res::Struct(id);
        Ty::Struct(id)
    }

    fn check_readonly_path(&mut self, mut expr: ExprId) {
        while let Expr::Member { base, name } = self.input.ast.exprs[expr] {
            if let (Ty::Struct(id), Res::Field(index)) =
                (self.out.types[base], self.out.resolutions[expr])
                && self.input.ast.params(self.input.ast.structs[id].fields)[index as usize].readonly
            {
                self.report(
                    Code::AssignToImmutable,
                    "cannot assign through a read-only field",
                    self.token_span(name),
                );
                return;
            }
            expr = base;
        }
        if let Res::SelfField(binding, index) = self.out.resolutions[expr] {
            let Ty::Struct(id) = self.out.bindings[binding].ty else {
                unreachable!()
            };
            if self.input.ast.params(self.input.ast.structs[id].fields)[index as usize].readonly {
                self.report(
                    Code::AssignToImmutable,
                    "cannot assign through a read-only field",
                    self.span(expr),
                );
            }
        }
    }

    fn member_value(&mut self, expr: ExprId, base: ExprId, name: TokenId) -> Ty {
        let may_be_struct = match self.input.ast.exprs[base] {
            Expr::Name(token) => match self.lookup(self.symbol(token)) {
                Some(Name::Binding(binding)) => self.receiver_field(token).map_or(
                    matches!(self.out.bindings[binding].ty, Ty::Struct(_)),
                    |(_, _, ty)| matches!(ty, Ty::Struct(_)),
                ),
                _ => self
                    .receiver_field(token)
                    .is_some_and(|(_, _, ty)| matches!(ty, Ty::Struct(_))),
            },
            _ => true,
        };
        if may_be_struct {
            if let Ty::Struct(id) = self.value(base, None) {
                let fields = self.input.ast.params(self.input.ast.structs[id].fields);
                if let Some(index) = fields
                    .iter()
                    .position(|f| self.symbol(f.name) == self.symbol(name))
                {
                    let fields = &self.out.param_types[self.out.struct_fields[id]];
                    if let Some(&ty) = fields.get(index) {
                        self.out.resolutions[expr] = Res::Field(index as u32);
                        return ty;
                    }
                    return Ty::Error;
                }
                self.report(
                    Code::UnknownName,
                    "unknown struct field",
                    self.token_span(name),
                );
                return Ty::Error;
            }
            self.report(
                Code::Unsupported,
                "not supported yet: member access",
                self.token_span(name),
            );
            return Ty::Error;
        }
        if self.host_function(base, name).is_ok() {
            self.report(
                Code::Unsupported,
                "not supported yet: function values",
                self.token_span(name),
            );
        }
        Ty::Error
    }

    fn call(
        &mut self,
        expr: ExprId,
        callee: ExprId,
        args: List<ExprId>,
        expected: Option<Ty>,
    ) -> Ty {
        let ast = self.input.ast;
        match ast.exprs[callee] {
            Expr::EnumShorthand(name) => {
                return self.enum_value(expr, None, name, expected, None, Some(args));
            }
            Expr::Member { base, name } => {
                if let Expr::Name(owner) = ast.exprs[base]
                    && let Some(id) = self
                        .enum_names
                        .get(self.symbol(owner).index())
                        .copied()
                        .flatten()
                    && ast.enum_cases[ast.enums[id].cases]
                        .iter()
                        .take(256)
                        .any(|case| self.symbol(case.name) == self.symbol(name))
                {
                    return self.enum_value(expr, Some(id), name, expected, None, Some(args));
                }
            }
            _ => {}
        }
        if matches!(ast.exprs[callee], Expr::Name(token) if self.symbol(token).is_type_name()) {
            return self.conversion(expr, callee, ast.args(args), true);
        }
        if let Expr::Name(name) = ast.exprs[callee]
            && self.receiver_field(name).is_none()
            && !matches!(self.lookup(self.symbol(name)), Some(Name::Binding(binding)) if self.out.bindings[binding].owner.is_some())
            && let Some(binding) = self.current.and_then(|current| current.self_binding)
            && let Some(method) = self.methods.iter().copied().find(|&id| {
                let decl = ast.fns[id];
                !decl.is_static
                    && decl.receiver.is_some()
                    && self.symbol(decl.name) == self.symbol(name)
                    && self.out.param_types[self.out.signatures[id].params].first()
                        == Some(&self.out.bindings[binding].ty)
            })
        {
            let signature = self.out.signatures[method];
            let params = signature.params;
            if ast.args(args).len() != params.len() - 1 {
                self.report(
                    Code::ArgumentCount,
                    format!("method takes {} arguments", params.len() - 1),
                    self.span(expr),
                );
            }
            for (index, &arg) in ast.args(args).iter().enumerate() {
                self.value(arg, self.out.param_types[params].get(index + 1).copied());
            }
            if ast.fns[method].mutating {
                if !self.out.bindings[binding].mutable {
                    self.report(
                        Code::AssignToImmutable,
                        "mutating method requires a mutable receiver",
                        self.token_span(name),
                    );
                }
                self.out.resolutions[expr] = Res::BareMutMethod(method, binding);
            } else {
                self.out.resolutions[expr] = Res::BareMethod(method, binding);
            }
            return signature.ret;
        }
        if let Expr::Member { base, name } = ast.exprs[callee]
            && let Expr::Name(owner) = ast.exprs[base]
            && self.lookup(self.symbol(owner)).is_none()
            && let Some(method) = self.methods.iter().copied().find(|&id| {
                let decl = ast.fns[id];
                decl.is_static
                    && decl
                        .receiver
                        .is_some_and(|receiver| self.symbol(receiver) == self.symbol(owner))
                    && self.symbol(decl.name) == self.symbol(name)
            })
        {
            let signature = self.out.signatures[method];
            let params = signature.params;
            if ast.args(args).len() != params.len() {
                self.report(
                    Code::ArgumentCount,
                    format!("static function takes {} arguments", params.len()),
                    self.span(expr),
                );
            }
            for (index, &arg) in ast.args(args).iter().enumerate() {
                self.value(arg, self.out.param_types[params].get(index).copied());
            }
            self.out.resolutions[expr] = Res::Function(method);
            return signature.ret;
        }
        if let Expr::Member { base, name } = ast.exprs[callee]
            && self
                .methods
                .iter()
                .any(|&id| self.symbol(ast.fns[id].name) == self.symbol(name))
            && !matches!(ast.exprs[base], Expr::Name(token) if matches!(self.lookup(self.symbol(token)), Some(Name::Module(_))))
        {
            let receiver = self.value(base, None);
            if matches!(receiver, Ty::Struct(_) | Ty::Enum(_)) {
                let method = self
                    .methods
                    .iter()
                    .copied()
                    .map(|id| (id, &ast.fns[id]))
                    .find(|(id, method)| {
                        method.receiver.is_some()
                            && !method.is_static
                            && self.symbol(method.name) == self.symbol(name)
                            && self.out.param_types[self.out.signatures[*id].params].first()
                                == Some(&receiver)
                    })
                    .map(|(id, _)| id);
                if let Some(method) = method {
                    let signature = self.out.signatures[method];
                    let types = signature.params;
                    let expected_count = types.len() - 1;
                    if ast.args(args).len() != expected_count {
                        self.report(
                            Code::ArgumentCount,
                            format!("method takes {expected_count} arguments"),
                            self.span(expr),
                        );
                    }
                    for (index, &arg) in ast.args(args).iter().enumerate() {
                        let expected = self.out.param_types[types].get(index + 1).copied();
                        self.value(arg, expected);
                    }
                    if ast.fns[method].mutating {
                        self.check_readonly_path(base);
                        let mut root = base;
                        while let Expr::Member { base, .. } = ast.exprs[root] {
                            root = base;
                        }
                        let binding = match self.out.resolutions[root] {
                            Res::Binding(binding) | Res::SelfField(binding, _) => Some(binding),
                            _ => None,
                        };
                        if !binding.is_some_and(|binding| self.out.bindings[binding].mutable) {
                            self.report(
                                Code::AssignToImmutable,
                                "mutating method requires a mutable receiver",
                                self.span(base),
                            );
                        }
                        if let Some(binding) = binding {
                            self.out.resolutions[expr] = Res::MutMethod(method, binding);
                        }
                    } else {
                        self.out.resolutions[expr] = Res::Method(method);
                    }
                    return signature.ret;
                }
                self.report(
                    Code::UnknownName,
                    "unknown aggregate method",
                    self.token_span(name),
                );
                for &arg in ast.args(args) {
                    self.value(arg, None);
                }
                return Ty::Error;
            }
        }
        if let Expr::Member { base, name } = ast.exprs[callee]
            && matches!(
                self.spelling(name),
                "slice"
                    | "contains"
                    | "starts_with"
                    | "ends_with"
                    | "find"
                    | "trim"
                    | "trim_start"
                    | "trim_end"
                    | "replace"
                    | "to_upper"
                    | "to_lower"
            )
            && !matches!(ast.exprs[base], Expr::Name(token) if matches!(self.heads.get(self.symbol(token).as_u32() as usize).copied().flatten(), Some(Name::Module(_))))
        {
            self.value(base, Some(Ty::String));
            let method = self.spelling(name);
            let (count, argument, resolution, result) = match method {
                "to_upper" => (0, Ty::String, Res::StringUpper, Ty::String),
                "to_lower" => (0, Ty::String, Res::StringLower, Ty::String),
                "replace" => (2, Ty::String, Res::StringReplace, Ty::String),
                "trim" => (0, Ty::String, Res::StringTrim, Ty::String),
                "trim_start" => (0, Ty::String, Res::StringTrimStart, Ty::String),
                "trim_end" => (0, Ty::String, Res::StringTrimEnd, Ty::String),
                "slice" => (2, Ty::I64, Res::StringSlice, Ty::String),
                "contains" => (1, Ty::String, Res::StringContains, Ty::Bool),
                "starts_with" => (1, Ty::String, Res::StringStarts, Ty::Bool),
                "ends_with" => (1, Ty::String, Res::StringEnds, Ty::Bool),
                "find" => (1, Ty::String, Res::StringFind, Ty::FindOffset),
                _ => unreachable!(),
            };
            for &arg in ast.args(args) {
                self.value(arg, Some(argument));
            }
            if ast.args(args).len() != count {
                self.report(
                    Code::ArgumentCount,
                    format!("`{method}` takes {count} arguments"),
                    self.span(expr),
                );
            }
            self.out.resolutions[expr] = resolution;
            return result;
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
            Expr::Name(token) if self.receiver_field(token).is_some() => {
                let spelling = self.spelling(token);
                self.report(
                    Code::ArgumentCount,
                    format!("`{spelling}` is not a function"),
                    call_span,
                );
                None
            }
            Expr::Name(token)
                if self
                    .current
                    .and_then(|current| ast.fns[current.id].receiver)
                    .is_some()
                    && !matches!(self.lookup(self.symbol(token)), Some(Name::Binding(binding)) if self.out.bindings[binding].owner.is_some())
                    && self.methods.iter().any(|&id| {
                        let candidate = ast.fns[id];
                        candidate.is_static
                            && candidate.receiver.is_some_and(|owner| {
                                Some(self.symbol(owner))
                                    == self
                                        .current
                                        .and_then(|current| ast.fns[current.id].receiver)
                                        .map(|owner| self.symbol(owner))
                            })
                            && self.symbol(candidate.name) == self.symbol(token)
                    }) =>
            {
                let owner = ast.fns[self.current.unwrap().id].receiver.unwrap();
                let method = self
                    .methods
                    .iter()
                    .copied()
                    .find(|&id| {
                        let candidate = ast.fns[id];
                        candidate.is_static
                            && candidate
                                .receiver
                                .is_some_and(|receiver| self.symbol(receiver) == self.symbol(owner))
                            && self.symbol(candidate.name) == self.symbol(token)
                    })
                    .unwrap();
                let signature = self.out.signatures[method];
                Some((
                    Res::Function(method),
                    Params::Script(signature.params),
                    signature.ret,
                ))
            }
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
        self.hint_at(expr, 0)
    }

    fn hint_at(&self, expr: ExprId, depth: u8) -> Option<Ty> {
        if depth >= 64 {
            return None;
        }
        if matches!(self.ast().exprs[expr], Expr::String(_)) {
            return Some(Ty::String);
        }
        if let Expr::Binary {
            op: BinaryOp::Coalesce,
            lhs,
            ..
        } = self.ast().exprs[expr]
        {
            let ty = self.hint_at(lhs, depth + 1)?;
            return if ty == Ty::OptionalString {
                Some(Ty::String)
            } else {
                ty.tagged_optional_inner()
            };
        }
        if let Expr::Call { callee, .. } = self.ast().exprs[expr]
            && let Expr::Name(name) = self.ast().exprs[callee]
            && let Some(Name::Function(function)) = self.lookup(self.symbol(name))
        {
            return Some(self.out.signatures[function].ret);
        }
        if let Expr::Call { callee, .. } = self.ast().exprs[expr]
            && let Expr::Member { base, name } = self.ast().exprs[callee]
            && self.spelling(name) == "find"
            && self.hint_at(base, depth + 1) == Some(Ty::String)
        {
            return Some(Ty::FindOffset);
        }
        if let Expr::Member { base, name } = self.ast().exprs[expr]
            && let Some(Ty::Struct(id)) = self.hint_at(base, depth + 1)
        {
            let fields = self.ast().params(self.ast().structs[id].fields);
            let index = fields
                .iter()
                .position(|field| self.symbol(field.name) == self.symbol(name))?;
            return self.out.param_types[self.out.struct_fields[id]]
                .get(index)
                .copied();
        }
        if let Expr::Name(token) = self.ast().exprs[expr]
            && let Some(Some(Name::Binding(binding))) = self.heads.get(self.symbol(token).index())
        {
            return Some(self.out.bindings[*binding].ty);
        }
        if let Expr::Name(token) = self.ast().exprs[expr] {
            return self.receiver_field(token).map(|(_, _, ty)| ty);
        }
        None
    }

    fn binary_chain(&mut self, root: ExprId, expected: Option<Ty>) -> Ty {
        let ast = self.input.ast;
        let mark = self.spine.len();
        let mut leftmost = root;
        while let Expr::Binary { op, lhs, .. } = ast.exprs[leftmost]
            && op != BinaryOp::Coalesce
        {
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
        } else if matches!(first_op, BinaryOp::Eq | BinaryOp::NotEq)
            && self
                .hint(first_rhs)
                .is_some_and(|ty| ty == Ty::OptionalString || ty.tagged_optional_inner().is_some())
        {
            self.hint(first_rhs)
        } else if matches!(first_op, BinaryOp::Eq | BinaryOp::NotEq)
            && ((matches!(ast.exprs[first_rhs], Expr::Null(_))
                && (matches!(ast.exprs[leftmost], Expr::Bool(_))
                    || self.hint(leftmost) == Some(Ty::Bool)))
                || (matches!(ast.exprs[leftmost], Expr::Null(_))
                    && (matches!(ast.exprs[first_rhs], Expr::Bool(_))
                        || self.hint(first_rhs) == Some(Ty::Bool))))
        {
            Some(Ty::OptionalBool)
        } else if matches!(first_op, BinaryOp::Eq | BinaryOp::NotEq)
            && matches!(ast.exprs[first_rhs], Expr::Null(_))
            && matches!(self.hint(leftmost), Some(Ty::I64 | Ty::FindOffset))
        {
            Some(Ty::OptionalI64)
        } else if matches!(first_op, BinaryOp::Eq | BinaryOp::NotEq)
            && matches!(ast.exprs[first_rhs], Expr::Null(_))
            && (self.hint(leftmost) == Some(Ty::String)
                || matches!(ast.exprs[leftmost], Expr::Null(_)))
        {
            Some(Ty::OptionalString)
        } else if matches!(first_op, BinaryOp::Eq | BinaryOp::NotEq)
            && ((matches!(ast.exprs[leftmost], Expr::Null(_))
                && matches!(ast.exprs[first_rhs], Expr::Int { .. }))
                || (matches!(ast.exprs[first_rhs], Expr::Null(_))
                    && matches!(ast.exprs[leftmost], Expr::Int { .. })))
        {
            Some(Ty::OptionalI64)
        } else if matches!(first_op, BinaryOp::Eq | BinaryOp::NotEq)
            && ((matches!(ast.exprs[leftmost], Expr::Null(_))
                && matches!(ast.exprs[first_rhs], Expr::String(_)))
                || (matches!(ast.exprs[first_rhs], Expr::Null(_))
                    && matches!(ast.exprs[leftmost], Expr::String(_))))
        {
            Some(Ty::OptionalString)
        } else {
            expected.filter(|t| t.numeric()).or_else(|| {
                if !matches!(first_op, BinaryOp::Shl | BinaryOp::Shr)
                    && matches!(
                        ast.exprs[leftmost],
                        Expr::Int { .. } | Expr::Float { .. } | Expr::Null(_) | Expr::String(_)
                    )
                {
                    self.hint(first_rhs)
                } else {
                    None
                }
            })
        };
        let guidance = if matches!(first_op, BinaryOp::Eq | BinaryOp::NotEq)
            && guidance == Some(Ty::FindOffset)
        {
            Some(Ty::OptionalI64)
        } else {
            guidance
        };
        let mut lhs_ty = if guidance
            .is_some_and(|ty| ty == Ty::OptionalString || ty.tagged_optional_inner().is_some())
        {
            self.value(leftmost, guidance)
        } else {
            self.expr(leftmost, guidance, true)
        };
        if matches!(first_op, BinaryOp::Eq | BinaryOp::NotEq) && lhs_ty == Ty::FindOffset {
            self.out.optional_wrap[leftmost] = true;
            lhs_ty = Ty::OptionalI64;
        }
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
        Coalesce => false,
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
    string_fragment(inner, raw)
}

fn string_fragment(inner: &str, raw: bool) -> Result<Option<String>, eitri_base::OutOfMemory> {
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
