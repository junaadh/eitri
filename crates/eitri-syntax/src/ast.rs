//! The syntax tree of one module: compact, index-addressed, and reusable.
//!
//! Nodes live in per-kind [`IndexVec`]s and refer to each other by id;
//! variable-length children live in [`ListPool`]s. Names refer to their
//! tokens, so a node stores four bytes for a name and the [`Tokens`] buffer
//! supplies its symbol and span. Every expression also has a span for
//! diagnostics, in a parallel column.
//!
//! A construct the first slice does not support is reported by the parser
//! and replaced by [`Expr::Error`] (or dropped, for statements and items),
//! with all of its subexpressions. Later stages treat `Error` as already
//! reported and say nothing more about it, so one unsupported feature
//! produces one diagnostic.
//!
//! [`Tokens`]: crate::Tokens

use eitri_base::{IndexVec, List, ListPool, Span};

use crate::token::TokenId;

eitri_base::newtype_index! {
    pub struct ExprId;
    pub struct StmtId;
    pub struct FnId;
    pub struct IfId;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    And,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    WrapAdd,
    WrapSub,
    WrapMul,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    /// Checked negation.
    Neg,
    Not,
}

/// `=`, or the operator of a compound assignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Compound(BinaryOp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expr {
    /// An integer literal. `negative` when a prefix `-` was folded into it,
    /// so `-9223372036854775808` is one constant.
    Int {
        token: TokenId,
        negative: bool,
    },
    Bool(bool),
    /// A name, resolved by the checker.
    Name(TokenId),
    /// `base.name`. Only a host function call target in the first slice.
    Member {
        base: ExprId,
        name: TokenId,
    },
    Unary {
        op: UnaryOp,
        operand: ExprId,
    },
    Binary {
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    Call {
        callee: ExprId,
        args: List<ExprId>,
    },
    /// Stored aside: its two blocks would double every node's size.
    If(IfId),
    /// An unsupported or malformed construct, already reported.
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IfExpr {
    pub condition: ExprId,
    pub then: Block,
    pub otherwise: Else,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Else {
    None,
    Block(Block),
    /// `else if …`: the nested `If` expression.
    If(ExprId),
}

/// A `{ … }` block: its statements and its span (braces included).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub stmts: List<StmtId>,
    pub span: Span,
}

/// A written type. The first slice only has named types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeExpr {
    Named(TokenId),
    /// Unsupported, already reported.
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stmt {
    Let {
        mutable: bool,
        name: TokenId,
        ty: Option<TypeExpr>,
        init: ExprId,
    },
    /// `name op= value`. The span covers the whole statement.
    Assign {
        target: TokenId,
        op: AssignOp,
        value: ExprId,
        span: Span,
    },
    Expr(ExprId),
    While {
        keyword: TokenId,
        condition: ExprId,
        body: Block,
    },
    Return {
        keyword: TokenId,
        value: Option<ExprId>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: TokenId,
    pub ty: TypeExpr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FnDecl {
    pub name: TokenId,
    pub params: List<Param>,
    pub ret: TypeExpr,
    /// `None` for a bodiless (host) declaration.
    pub body: Option<Block>,
}

/// `import name from "path"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Import {
    pub name: TokenId,
    /// The string literal token; its contents, without quotes, are the path.
    pub path: TokenId,
}

/// A module-level item, in source order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Fn(FnId),
    Stmt(StmtId),
}

/// One module's syntax. Cleared and refilled by each parse, keeping its
/// allocations (it lives in the caller's scratch).
#[derive(Default)]
pub struct Ast {
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
    pub fns: IndexVec<FnId, FnDecl>,
    pub stmts: IndexVec<StmtId, Stmt>,
    pub exprs: IndexVec<ExprId, Expr>,
    pub ifs: IndexVec<IfId, IfExpr>,
    pub expr_spans: IndexVec<ExprId, Span>,
    pub expr_lists: ListPool<ExprId>,
    pub stmt_lists: ListPool<StmtId>,
    pub params: ListPool<Param>,
}

const _: () = assert!(size_of::<Expr>() <= 16 && size_of::<Stmt>() <= 32);

impl Ast {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.imports.clear();
        self.items.clear();
        self.fns.clear();
        self.stmts.clear();
        self.exprs.clear();
        self.ifs.clear();
        self.expr_spans.clear();
        self.expr_lists.clear();
        self.stmt_lists.clear();
        self.params.clear();
    }

    pub(crate) fn expr(&mut self, expr: Expr, span: Span) -> ExprId {
        self.expr_spans.push(span);
        self.exprs.push(expr)
    }

    pub fn span(&self, expr: ExprId) -> Span {
        self.expr_spans[expr]
    }

    pub fn block(&self, block: Block) -> &[StmtId] {
        &self.stmt_lists[block.stmts]
    }

    pub fn args(&self, args: List<ExprId>) -> &[ExprId] {
        &self.expr_lists[args]
    }

    pub fn params(&self, params: List<Param>) -> &[Param] {
        &self.params[params]
    }
}
