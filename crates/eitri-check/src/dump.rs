//! The checked syntax tree as text, in the style of `clang -ast-dump`
//! (`eitri dump ast`): one node per line under Unicode tree guides, its
//! kind, its source range as `<line:L:C, col:C>`, and its type. Not a
//! stable format.
//!
//! Locations print `line:L:C` only when the line differs from the last one
//! printed, as clang does. Types come from the checker; an expression it
//! could not type shows `'<error>'`, so a tree with errors still dumps.
//!
//! The tree is walked with an explicit stack, and guides deeper than
//! [`GUIDES`] levels are elided: a left-associative chain of operators is as
//! deep as it is long, and neither the walk nor the output may grow with the
//! square of its length.

use std::fmt::Write;

use eitri_base::{Idx, Interner, Span};
use eitri_syntax::ast::{
    AssignOp, Ast, BinaryOp, Block, Else, Expr, ExprId, FnId, Item, Stmt, StmtId, TypeExpr, UnaryOp,
};
use eitri_syntax::{TokenId, Tokens};

use crate::check::{BindingId, Checked, Res};
use crate::host::{HostFnId, Hosts};
use crate::ty::Ty;

/// Guide columns drawn in full; deeper levels are summarized.
const GUIDES: usize = 48;

/// Everything the dump reads.
pub struct Input<'a> {
    pub text: &'a str,
    pub tokens: &'a Tokens,
    pub ast: &'a Ast,
    pub checked: &'a Checked,
    pub interner: &'a Interner,
    pub hosts: &'a Hosts,
}

/// Dump the module. `color` adds clang's terminal colors.
pub fn ast(input: &Input<'_>, color: bool) -> String {
    let mut lines = vec![0u32];
    lines.extend(
        input
            .text
            .bytes()
            .enumerate()
            .filter(|&(_, b)| b == b'\n')
            .map(|(i, _)| i as u32 + 1),
    );
    let mut dumper = Dumper {
        input,
        color,
        lines,
        last_line: 0,
        out: String::new(),
        stack: vec![(Node::Module, 0, true)],
        lasts: Vec::new(),
    };
    dumper.walk();
    dumper.out
}

#[derive(Clone, Copy)]
enum Node {
    Module,
    Import(usize),
    Fn(FnId),
    Param(FnId, usize),
    Block(Block),
    Stmt(StmtId),
    Expr(ExprId),
    /// A call's callee, which takes its meaning from the call.
    Callee(ExprId, ExprId),
    /// An assignment's target.
    Ref(TokenId, Option<BindingId>),
    /// The module of a host function call.
    ModuleRef(TokenId),
}

// Clang's TextNodeDumper colors.
const DECL: &str = "\x1b[1;32m";
const STMT: &str = "\x1b[1;35m";
const TYPE: &str = "\x1b[32m";
const LOCATION: &str = "\x1b[33m";
const NAME: &str = "\x1b[1;36m";
const VALUE: &str = "\x1b[1;36m";
const GUIDE: &str = "\x1b[34m";
const RESET: &str = "\x1b[0m";

struct Dumper<'a> {
    input: &'a Input<'a>,
    color: bool,
    /// Byte offset of each line's start.
    lines: Vec<u32>,
    /// The line of the last location printed (1-based; 0 before any).
    last_line: usize,
    out: String,
    /// Pending nodes: the node, its depth, whether it is its parent's last
    /// child.
    stack: Vec<(Node, usize, bool)>,
    /// For each ancestor depth, whether that ancestor was a last child.
    lasts: Vec<bool>,
}

impl Dumper<'_> {
    fn paint(&self, color: &str, text: &str) -> String {
        if self.color {
            format!("{color}{text}{RESET}")
        } else {
            text.to_owned()
        }
    }

    fn text(&self, span: Span) -> &str {
        self.input
            .text
            .get(span.start as usize..span.end as usize)
            .unwrap_or("")
    }

    fn token(&self, token: TokenId) -> &str {
        self.text(self.input.tokens.span(token))
    }

    /// Where a declaration starts: the keyword before its name (`fn`,
    /// `import`, `let`, `var`), as clang starts a declaration's range.
    fn decl_start(&self, name: TokenId) -> u32 {
        let tokens = self.input.tokens;
        let keyword = name.index().checked_sub(1).map(TokenId::new);
        match keyword {
            Some(k) if tokens.kind(k).is_keyword() => tokens.span(k).start,
            _ => tokens.span(name).start,
        }
    }

    /// `line:L:C`, or `col:C` on the line last printed.
    fn location(&mut self, offset: u32) -> String {
        let line = self.lines.partition_point(|&start| start <= offset);
        let column = offset - self.lines[line - 1] + 1;
        let text = if line == self.last_line {
            format!("col:{column}")
        } else {
            format!("line:{line}:{column}")
        };
        self.last_line = line;
        text
    }

    /// `<begin, end>`, the end being the range's last byte, as clang points
    /// at the last token.
    fn range(&mut self, span: Span) -> String {
        let begin = self.location(span.start);
        let last = span.end.saturating_sub(1).max(span.start);
        let text = if last == span.start {
            format!("<{begin}>")
        } else {
            format!("<{begin}, {}>", self.location(last))
        };
        self.paint(LOCATION, &text)
    }

    fn ty(&self, ty: Ty) -> String {
        let name = match ty {
            Ty::I8 => "'i8'",
            Ty::U8 => "'u8'",
            Ty::U64 => "'u64'",
            Ty::F32 => "'f32'",
            Ty::F64 => "'f64'",
            Ty::Char => "'char'",
            Ty::String => "'string'",
            Ty::OptionalString => "'?string'",
            Ty::OptionalClass(_) => "'?class'",
            Ty::OptionalI64 => "'?i64'",
            Ty::OptionalBool => "'?bool'",
            Ty::OptionalI8 => "'?i8'",
            Ty::OptionalU8 => "'?u8'",
            Ty::OptionalU64 => "'?u64'",
            Ty::OptionalF32 => "'?f32'",
            Ty::OptionalF64 => "'?f64'",
            Ty::OptionalChar => "'?char'",
            Ty::OptionalStruct(_) => "'?struct'",
            Ty::OptionalEnum(_) => "'?enum'",
            Ty::Class(_) => "'class'",
            Ty::Struct(_) => "'struct'",
            Ty::Enum(_) => "'enum'",
            Ty::FindOffset => "'?i64'",
            Ty::I64 => "'i64'",
            Ty::FixedI64(_) => "'[N]i64'",
            Ty::FixedString(_) => "'[N]string'",
            Ty::FixedScalar(_, _) => "'[N]scalar'",
            Ty::Result(_) => "'result'",
            Ty::OptionalResult(_) => "'?result'",
            Ty::OptionalFixed(_) => "'?[N]T'",
            Ty::FixedClass(_, _) => "'[N]class'",
            Ty::FixedStruct(_, _) => "'[N]struct'",
            Ty::FixedEnum(_, _) => "'[N]enum'",
            Ty::FixedResult(_, _) => "'[N]result'",
            Ty::FixedOptional(_, _) => "'[N]optional'",
            Ty::FixedNested(_, _) => "'[N][M]T'",
            Ty::Bool => "'bool'",
            Ty::Void => "'void'",
            Ty::Error => "'<error>'",
        };
        self.paint(TYPE, name)
    }

    fn fn_type(&self, params: &[Ty], ret: Ty) -> String {
        let name = |ty: Ty| match ty {
            Ty::I8 => "i8",
            Ty::U8 => "u8",
            Ty::U64 => "u64",
            Ty::F32 => "f32",
            Ty::F64 => "f64",
            Ty::Char => "char",
            Ty::String => "string",
            Ty::OptionalString => "?string",
            Ty::OptionalClass(_) => "?class",
            Ty::OptionalI64 => "?i64",
            Ty::OptionalBool => "?bool",
            Ty::OptionalI8 => "?i8",
            Ty::OptionalU8 => "?u8",
            Ty::OptionalU64 => "?u64",
            Ty::OptionalF32 => "?f32",
            Ty::OptionalF64 => "?f64",
            Ty::OptionalChar => "?char",
            Ty::OptionalStruct(_) => "?struct",
            Ty::OptionalEnum(_) => "?enum",
            Ty::Class(_) => "class",
            Ty::Struct(_) => "struct",
            Ty::Enum(_) => "enum",
            Ty::FindOffset => "?i64",
            Ty::I64 => "i64",
            Ty::FixedI64(_) => "[N]i64",
            Ty::FixedString(_) => "[N]string",
            Ty::FixedScalar(_, _) => "[N]scalar",
            Ty::Result(_) => "result",
            Ty::OptionalResult(_) => "?result",
            Ty::OptionalFixed(_) => "?[N]T",
            Ty::FixedClass(_, _) => "[N]class",
            Ty::FixedStruct(_, _) => "[N]struct",
            Ty::FixedEnum(_, _) => "[N]enum",
            Ty::FixedResult(_, _) => "[N]result",
            Ty::FixedOptional(_, _) => "[N]optional",
            Ty::FixedNested(_, _) => "[N][M]T",
            Ty::Bool => "bool",
            Ty::Void => "void",
            Ty::Error => "<error>",
        };
        let params: Vec<&str> = params.iter().map(|&t| name(t)).collect();
        self.paint(TYPE, &format!("'{} ({})'", name(ret), params.join(", ")))
    }

    fn script_type(&self, f: FnId) -> String {
        let checked = self.input.checked;
        let signature = checked.signatures[f];
        self.fn_type(&checked.param_types[signature.params], signature.ret)
    }

    fn host_type(&self, h: HostFnId) -> String {
        let host = self.input.hosts.function(h);
        self.fn_type(&host.params, host.ret)
    }

    fn written_type(&self, ty: TypeExpr) -> String {
        match ty {
            TypeExpr::ResultFixed(error, size, element) => self.paint(
                TYPE,
                &format!(
                    "'{}![{}]{}'",
                    self.token(error),
                    self.token(size),
                    self.token(element)
                ),
            ),
            TypeExpr::OptionalResultOptional(error, success) => self.paint(
                TYPE,
                &format!("'?<{}!?{}>'", self.token(error), self.token(success)),
            ),
            TypeExpr::OptionalResultNamed(error, success) => self.paint(
                TYPE,
                &format!("'?<{}!{}>'", self.token(error), self.token(success)),
            ),
            TypeExpr::ResultOptional(error, success) => self.paint(
                TYPE,
                &format!("'{}!?{}'", self.token(error), self.token(success)),
            ),
            TypeExpr::ResultNamed(error, success) => self.paint(
                TYPE,
                &format!("'{}!{}'", self.token(error), self.token(success)),
            ),
            TypeExpr::FixedNested(size, _, _)
            | TypeExpr::OptionalFixed(size, _)
            | TypeExpr::FixedOptional(size, _)
            | TypeExpr::FixedResult(size, _, _)
            | TypeExpr::FixedNamed(size, _)
            | TypeExpr::FixedScalar(size, _) => {
                self.paint(TYPE, &format!("'[{}]scalar'", self.token(size)))
            }
            TypeExpr::FixedString(token) => {
                self.paint(TYPE, &format!("'[{}]string'", self.token(token)))
            }
            TypeExpr::FixedI64(token) => self.paint(TYPE, &format!("'[{}]i64'", self.token(token))),
            TypeExpr::Named(token) => self.paint(TYPE, &format!("'{}'", self.token(token))),
            TypeExpr::OptionalString(token)
            | TypeExpr::OptionalI64(token)
            | TypeExpr::OptionalBool(token)
            | TypeExpr::OptionalScalar(token)
            | TypeExpr::OptionalNamed(token) => {
                self.paint(TYPE, &format!("'?{}'", self.token(token)))
            }
            TypeExpr::Error => self.paint(TYPE, "'<error>'"),
        }
    }

    /// A binding's kind as clang names references: a parameter or a local.
    fn binding_kind(&self, binding: BindingId) -> &'static str {
        let checked = self.input.checked;
        match checked.bindings[binding].owner {
            Some(f) if checked.params[f].contains(binding) => "ParmVar",
            _ => "Var",
        }
    }

    fn walk(&mut self) {
        while let Some((node, depth, last)) = self.stack.pop() {
            self.lasts.truncate(depth);
            let guides = self.guides(depth, last);
            self.lasts.push(last);
            let (line, children) = self.node(node);
            writeln!(self.out, "{guides}{line}").unwrap();
            let count = children.len();
            for (i, child) in children.into_iter().enumerate().rev() {
                self.stack.push((child, depth + 1, i + 1 == count));
            }
        }
    }

    /// The guide columns before a node at `depth`.
    fn guides(&self, depth: usize, last: bool) -> String {
        if depth == 0 {
            return String::new();
        }
        let mut text = String::new();
        let ancestors = &self.lasts[1..depth];
        let skip = ancestors.len().saturating_sub(GUIDES);
        if skip > 0 {
            write!(text, "[{skip} levels] ").unwrap();
        }
        for &ancestor_last in &ancestors[skip..] {
            text.push_str(if ancestor_last { "  " } else { "│ " });
        }
        text.push_str(if last { "└─" } else { "├─" });
        self.paint(GUIDE, &text)
    }

    fn node(&mut self, node: Node) -> (String, Vec<Node>) {
        let input = self.input;
        let (ast, tokens, checked) = (input.ast, input.tokens, input.checked);
        match node {
            Node::Module => {
                let end = input.text.len() as u32;
                let range = self.range(Span::new(0, end.max(1)));
                let mut children: Vec<Node> = (0..ast.imports.len()).map(Node::Import).collect();
                children.extend(ast.items.iter().map(|&item| match item {
                    Item::Fn(f) => Node::Fn(f),
                    Item::Stmt(s) => Node::Stmt(s),
                }));
                (
                    format!("{} {range}", self.paint(DECL, "ModuleDecl")),
                    children,
                )
            }
            Node::Import(i) => {
                let import = ast.imports[i];
                let end = tokens.span(import.path).end;
                let span = Span::new(self.decl_start(import.name), end);
                let range = self.range(span);
                let at = self.location(tokens.span(import.name).start);
                let line = format!(
                    "{} {range} {} {} {}",
                    self.paint(DECL, "ImportDecl"),
                    self.paint(LOCATION, &at),
                    self.paint(NAME, self.token(import.name)),
                    self.token(import.path),
                );
                (line, Vec::new())
            }
            Node::Fn(f) => {
                let decl = ast.fns[f];
                let start = self.decl_start(decl.name);
                let span = match decl.body {
                    Some(body) => Span::new(start, body.span.end),
                    None => tokens.span(decl.name).to(Span::empty(start)),
                };
                let range = self.range(span);
                let at = self.location(tokens.span(decl.name).start);
                let line = format!(
                    "{} {range} {} {} {}",
                    self.paint(DECL, "FunctionDecl"),
                    self.paint(LOCATION, &at),
                    self.paint(NAME, self.token(decl.name)),
                    self.script_type(f)
                );
                let mut children: Vec<Node> = (0..ast.params(decl.params).len())
                    .map(|i| Node::Param(f, i))
                    .collect();
                children.extend(decl.body.map(Node::Block));
                (line, children)
            }
            Node::Param(f, i) => {
                let param = ast.params(ast.fns[f].params)[i];
                let end = match param.ty {
                    TypeExpr::ResultFixed(_, _, token)
                    | TypeExpr::OptionalResultOptional(_, token)
                    | TypeExpr::OptionalResultNamed(_, token)
                    | TypeExpr::ResultOptional(_, token)
                    | TypeExpr::ResultNamed(_, token)
                    | TypeExpr::FixedNested(token, _, _)
                    | TypeExpr::OptionalFixed(token, _)
                    | TypeExpr::FixedOptional(token, _)
                    | TypeExpr::FixedResult(token, _, _)
                    | TypeExpr::FixedNamed(token, _)
                    | TypeExpr::FixedScalar(token, _)
                    | TypeExpr::FixedString(token)
                    | TypeExpr::FixedI64(token)
                    | TypeExpr::Named(token)
                    | TypeExpr::OptionalString(token)
                    | TypeExpr::OptionalI64(token)
                    | TypeExpr::OptionalBool(token)
                    | TypeExpr::OptionalScalar(token)
                    | TypeExpr::OptionalNamed(token) => tokens.span(token),
                    TypeExpr::Error => tokens.span(param.name),
                };
                let range = self.range(tokens.span(param.name).to(end));
                let at = self.location(tokens.span(param.name).start);
                let line = format!(
                    "{} {range} {} {} {}",
                    self.paint(DECL, "ParmVarDecl"),
                    self.paint(LOCATION, &at),
                    self.paint(NAME, self.token(param.name)),
                    self.written_type(param.ty)
                );
                (line, Vec::new())
            }
            Node::Block(block) => {
                let range = self.range(block.span);
                let children = ast.block(block).iter().map(|&s| Node::Stmt(s)).collect();
                (
                    format!("{} {range}", self.paint(STMT, "CompoundStmt")),
                    children,
                )
            }
            Node::Stmt(id) => self.stmt(id),
            Node::Expr(e) => self.expr(e, None),
            Node::Callee(e, call) => self.expr(e, Some(call)),
            Node::Ref(token, binding) => {
                let range = self.range(tokens.span(token));
                let (ty, kind) = match binding {
                    Some(b) => (self.ty(checked.bindings[b].ty), self.binding_kind(b)),
                    None => (self.ty(Ty::Error), "Var"),
                };
                let name = self.paint(NAME, &format!("'{}'", self.token(token)));
                (
                    format!(
                        "{} {range} {ty} {kind} {name}",
                        self.paint(STMT, "DeclRefExpr")
                    ),
                    Vec::new(),
                )
            }
            Node::ModuleRef(token) => {
                let range = self.range(tokens.span(token));
                let name = self.paint(NAME, &format!("'{}'", self.token(token)));
                (
                    format!("{} {range} Module {name}", self.paint(STMT, "DeclRefExpr")),
                    Vec::new(),
                )
            }
        }
    }

    fn stmt(&mut self, id: StmtId) -> (String, Vec<Node>) {
        let input = self.input;
        let (ast, tokens, checked) = (input.ast, input.tokens, input.checked);
        let binding = checked.stmt_bindings.get(id).copied().flatten();
        match ast.stmts[id] {
            Stmt::Let {
                mutable,
                name,
                ty: _,
                init,
            } => {
                let range = self.range(Span::new(self.decl_start(name), ast.span(init).end));
                let at = self.location(tokens.span(name).start);
                let ty = self.ty(binding.map_or(Ty::Error, |b| checked.bindings[b].ty));
                let keyword = if mutable { "var" } else { "let" };
                let line = format!(
                    "{} {range} {} {} {ty} {keyword}",
                    self.paint(DECL, "VarDecl"),
                    self.paint(LOCATION, &at),
                    self.paint(NAME, self.token(name)),
                );
                (line, vec![Node::Expr(init)])
            }
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                let range = self.range(span);
                let ty = self.ty(binding.map_or(Ty::Error, |b| checked.bindings[b].ty));
                let (kind, op) = match op {
                    AssignOp::Set => ("BinaryOperator", "=".to_owned()),
                    AssignOp::Compound(op) => {
                        ("CompoundAssignOperator", format!("{}=", binary(op)))
                    }
                };
                let line = format!("{} {range} {ty} '{op}'", self.paint(STMT, kind));
                (line, vec![Node::Ref(target, binding), Node::Expr(value)])
            }
            Stmt::AssignIndex { target, value, .. } => (
                self.paint(STMT, "AssignIndex"),
                vec![Node::Expr(target), Node::Expr(value)],
            ),
            Stmt::AssignField {
                target,
                value,
                span,
                ..
            } => (
                format!("{} {}", self.paint(STMT, "FieldAssign"), self.range(span)),
                vec![Node::Expr(target), Node::Expr(value)],
            ),
            Stmt::Expr(e) => self.expr(e, None),
            Stmt::For(for_id) => {
                let eitri_syntax::ast::ForStmt {
                    keyword,
                    start,
                    end,
                    body,
                    ..
                } = ast.fors[for_id];
                let range = self.range(tokens.span(keyword).to(body.span));
                let mut nodes = vec![Node::Expr(start)];
                nodes.extend(end.map(Node::Expr));
                nodes.push(Node::Block(body));
                (format!("{} {range}", self.paint(STMT, "ForStmt")), nodes)
            }
            Stmt::While {
                keyword,
                condition,
                body,
            } => {
                let range = self.range(tokens.span(keyword).to(body.span));
                (
                    format!("{} {range}", self.paint(STMT, "WhileStmt")),
                    vec![Node::Expr(condition), Node::Block(body)],
                )
            }
            Stmt::Break(keyword) | Stmt::Continue(keyword) => {
                let span = self.range(tokens.span(keyword));
                let name = if matches!(ast.stmts[id], Stmt::Break(_)) {
                    "BreakStmt"
                } else {
                    "ContinueStmt"
                };
                (format!("{} {span}", self.paint(STMT, name)), vec![])
            }
            Stmt::Return { keyword, value } => {
                let span = match value {
                    Some(value) => tokens.span(keyword).to(ast.span(value)),
                    None => tokens.span(keyword),
                };
                let range = self.range(span);
                (
                    format!("{} {range}", self.paint(STMT, "ReturnStmt")),
                    value.map(Node::Expr).into_iter().collect(),
                )
            }
        }
    }

    /// An expression; `call` is set for a call's callee.
    fn expr(&mut self, e: ExprId, call: Option<ExprId>) -> (String, Vec<Node>) {
        let input = self.input;
        let (ast, checked) = (input.ast, input.checked);
        let span = ast.span(e);
        let range = self.range(span);
        let ty = self.ty(checked.types.get(e).copied().unwrap_or(Ty::Error));
        let call_target = call.map(|c| checked.resolutions[c]);
        match ast.exprs[e] {
            Expr::Int { .. } | Expr::Float { .. } | Expr::Char(_) => {
                let value = checked.int_value(e).map_or_else(
                    || self.text(span).to_owned(),
                    |v| match checked.types[e] {
                        Ty::U64 => (v as u64).to_string(),
                        Ty::F32 | Ty::F64 | Ty::Char => self.text(span).to_owned(),
                        _ => v.to_string(),
                    },
                );
                let line = format!(
                    "{} {range} {ty} {}",
                    self.paint(
                        STMT,
                        match ast.exprs[e] {
                            Expr::Float { .. } => "FloatLiteral",
                            Expr::Char(_) => "CharacterLiteral",
                            _ => "IntegerLiteral",
                        }
                    ),
                    self.paint(VALUE, &value)
                );
                (line, Vec::new())
            }
            Expr::Null(_) => (format!("{} {range} {ty}", self.paint(STMT, "Null")), vec![]),
            Expr::String(_) | Expr::StringPart(_) => (
                format!(
                    "{} {range} {ty} {}",
                    self.paint(STMT, "StringLiteral"),
                    self.paint(VALUE, self.text(span))
                ),
                Vec::new(),
            ),
            Expr::Index { base, index } => (
                format!("{} {range} {ty}", self.paint(STMT, "Index")),
                vec![Node::Expr(base), Node::Expr(index)],
            ),
            Expr::FixedArray(parts) => (
                "fixed array".into(),
                ast.args(parts).iter().copied().map(Node::Expr).collect(),
            ),
            Expr::Interpolation(parts) => (
                format!("{} {range} {ty}", self.paint(STMT, "Interpolation")),
                ast.args(parts).iter().copied().map(Node::Expr).collect(),
            ),
            Expr::Bool(value) => {
                let line = format!(
                    "{} {range} {ty} {}",
                    self.paint(STMT, "BoolLiteral"),
                    self.paint(VALUE, &value.to_string())
                );
                (line, Vec::new())
            }
            Expr::Match(id) => {
                let node = ast.matches[id];
                let mut children = vec![Node::Expr(node.subject)];
                for arm in &ast.match_arms[node.arms] {
                    match arm.body {
                        eitri_syntax::ast::MatchBody::Expr(body) => children.push(Node::Expr(body)),
                        eitri_syntax::ast::MatchBody::Block(body) => {
                            children.push(Node::Block(body))
                        }
                    }
                }
                (
                    format!("{} {range} {ty}", self.paint(STMT, "MatchExpr")),
                    children,
                )
            }
            Expr::EnumShorthand(name) => (
                format!(
                    "{} {range} {ty} {}",
                    self.paint(STMT, "EnumCase"),
                    self.token(name)
                ),
                vec![],
            ),
            Expr::NamedCall { callee, args } => (
                format!("{} {range} {ty}", self.paint(STMT, "NamedCall")),
                std::iter::once(Node::Expr(callee))
                    .chain(ast.field_inits[args].iter().map(|a| Node::Expr(a.value)))
                    .collect(),
            ),
            Expr::StructLiteral { name, fields } => (
                format!(
                    "{} {range} {ty} {}",
                    self.paint(STMT, "StructLiteral"),
                    self.token(name)
                ),
                ast.field_inits[fields]
                    .iter()
                    .map(|field| Node::Expr(field.value))
                    .collect(),
            ),
            Expr::SelfValue(token) | Expr::Name(token) => {
                let name = self.paint(NAME, &format!("'{}'", self.token(token)));
                let (ty, kind) = match (checked.resolutions[e], call_target) {
                    (Res::Binding(b), _) => (ty, self.binding_kind(b)),
                    (_, Some(Res::Function(f))) => (self.script_type(f), "Function"),
                    _ => (ty, "Unresolved"),
                };
                (
                    format!(
                        "{} {range} {ty} {kind} {name}",
                        self.paint(STMT, "DeclRefExpr")
                    ),
                    Vec::new(),
                )
            }
            Expr::Member { base, name } => {
                let ty = match call_target {
                    Some(Res::Host(h)) => self.host_type(h),
                    _ => ty,
                };
                let member = self.paint(NAME, &format!(".{}", self.token(name)));
                let base = match ast.exprs[base] {
                    Expr::Name(token) => Node::ModuleRef(token),
                    _ => Node::Expr(base),
                };
                (
                    format!("{} {range} {ty} {member}", self.paint(STMT, "MemberExpr")),
                    vec![base],
                )
            }
            Expr::TryMapBlock(id) | Expr::RecoverBlock(id) => {
                let node = ast.recoveries[id];
                (
                    format!("{} {range} {ty}", self.paint(STMT, "RecoveryBlock")),
                    vec![Node::Expr(node.operand), Node::Block(node.block)],
                )
            }
            Expr::TryMap {
                operand, fallback, ..
            } => (
                format!("{} {range} {ty}", self.paint(STMT, "TryMap")),
                vec![Node::Expr(operand), Node::Expr(fallback)],
            ),
            Expr::Recover {
                operand, fallback, ..
            } => (
                format!("{} {range} {ty}", self.paint(STMT, "Recover")),
                vec![Node::Expr(operand), Node::Expr(fallback)],
            ),
            Expr::TryOptional(operand) => (
                format!("{} {range} {ty}", self.paint(STMT, "TryOptional")),
                vec![Node::Expr(operand)],
            ),
            Expr::Try(operand) => (
                format!("{} {range} {ty}", self.paint(STMT, "Try")),
                vec![Node::Expr(operand)],
            ),
            Expr::Unary { op, operand } => {
                let op = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "!",
                    UnaryOp::BitNot => "~",
                };
                (
                    format!(
                        "{} {range} {ty} prefix '{op}'",
                        self.paint(STMT, "UnaryOperator")
                    ),
                    vec![Node::Expr(operand)],
                )
            }
            Expr::Binary { op, lhs, rhs } => (
                format!(
                    "{} {range} {ty} '{}'",
                    self.paint(STMT, "BinaryOperator"),
                    binary(op)
                ),
                vec![Node::Expr(lhs), Node::Expr(rhs)],
            ),
            Expr::Convert { callee, value } => (
                format!("ConversionExpr {range} {ty} truncating"),
                vec![Node::Callee(callee, e), Node::Expr(value)],
            ),
            Expr::Call { callee, args } => {
                let mut children = vec![Node::Callee(callee, e)];
                children.extend(ast.args(args).iter().map(|&a| Node::Expr(a)));
                (
                    format!("{} {range} {ty}", self.paint(STMT, "CallExpr")),
                    children,
                )
            }
            Expr::If(id) => {
                let node = ast.ifs[id];
                let used = !matches!(checked.types.get(e), Some(Ty::Void) | None);
                let line = if used {
                    format!("{} {range} {ty}", self.paint(STMT, "IfExpr"))
                } else {
                    format!("{} {range}", self.paint(STMT, "IfStmt"))
                };
                let mut children = vec![Node::Expr(node.condition), Node::Block(node.then)];
                match node.otherwise {
                    Else::None => {}
                    Else::Block(block) => children.push(Node::Block(block)),
                    // As clang shows `else if`: the nested `if` is the third child.
                    Else::If(nested) => children.push(Node::Expr(nested)),
                }
                (line, children)
            }
            Expr::Error => (
                format!("{} {range} {ty}", self.paint(STMT, "RecoveryExpr")),
                Vec::new(),
            ),
        }
    }
}

fn binary(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Coalesce => "??",
        BinaryOp::Or => "||",
        BinaryOp::And => "&&",
        BinaryOp::Eq => "==",
        BinaryOp::NotEq => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::LtEq => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::GtEq => ">=",
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Rem => "%",
        BinaryOp::WrapAdd => "&+",
        BinaryOp::WrapSub => "&-",
        BinaryOp::WrapMul => "&*",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::Shl => "<<",
        BinaryOp::Shr => ">>",
    }
}
