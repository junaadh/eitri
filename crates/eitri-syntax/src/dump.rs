//! Text dumps of the token stream and the syntax tree, for inspection
//! (`eitri dump tokens|ast`). Not a stable format.
//!
//! The tree is walked with an explicit stack: a left-associative chain of
//! operators is as deep as it is long, and the dump must not be limited by
//! the Rust stack.

use std::fmt::Write;

use eitri_base::{Idx, Span};

use crate::ast::{
    AssignOp, Ast, BinaryOp, Block, Else, Expr, ExprId, Item, Stmt, StmtId, TypeExpr, UnaryOp,
};
use crate::token::{TokenId, Tokens};

/// Width of the text column before byte ranges.
const COLUMN: usize = 48;

/// One token per line: byte range, kind, and text.
pub fn tokens(text: &str, tokens: &Tokens) -> String {
    let mut out = String::new();
    for i in 0..tokens.len() {
        let id = TokenId::new(i);
        let span = tokens.span(id);
        let range = format!("{}..{}", span.start, span.end);
        let kind = format!("{:?}", tokens.kind(id));
        let shown = slice(text, span).replace('\n', "\\n").replace('\t', "\\t");
        writeln!(out, "{range:<12}{kind:<16}{shown}").unwrap();
    }
    out
}

/// The syntax tree, one node per line, indented, with byte ranges.
pub fn ast(text: &str, tokens: &Tokens, ast: &Ast) -> String {
    let mut dumper = Dumper {
        text,
        tokens,
        ast,
        out: String::new(),
        stack: Vec::new(),
    };
    for import in &ast.imports {
        let name = dumper.token(import.name);
        let path = dumper.token(import.path);
        let span = tokens.span(import.name).to(tokens.span(import.path));
        dumper.line(0, &format!("import {name} from {path}"), Some(span));
    }
    for &item in &ast.items {
        match item {
            Item::Fn(f) => {
                let decl = ast.fns[f];
                let params: Vec<String> = ast
                    .params(decl.params)
                    .iter()
                    .map(|p| format!("{}: {}", dumper.token(p.name), dumper.ty(p.ty)))
                    .collect();
                let header = format!(
                    "fn {}({}) -> {}",
                    dumper.token(decl.name),
                    params.join(", "),
                    dumper.ty(decl.ret)
                );
                let span = match decl.body {
                    Some(body) => tokens.span(decl.name).to(body.span),
                    None => tokens.span(decl.name),
                };
                dumper.line(0, &header, Some(span));
                if let Some(body) = decl.body {
                    dumper.walk(Node::Stmts(body), 1);
                }
            }
            Item::Stmt(stmt) => dumper.walk(Node::Stmt(stmt), 0),
        }
    }
    dumper.out
}

fn slice(text: &str, span: Span) -> &str {
    text.get(span.start as usize..span.end as usize)
        .unwrap_or("")
}

/// Pending work: a node and its depth.
#[derive(Clone, Copy)]
enum Node {
    Expr(ExprId),
    Stmt(StmtId),
    /// A block's statements, without a line of its own.
    Stmts(Block),
    /// A label line (`then`, `else`, `do`) over a block.
    Labeled(&'static str, Block),
    /// A plain line, such as `else` before a nested `if`.
    Text(&'static str),
}

struct Dumper<'a> {
    text: &'a str,
    tokens: &'a Tokens,
    ast: &'a Ast,
    out: String,
    stack: Vec<(Node, usize)>,
}

impl Dumper<'_> {
    fn token(&self, id: TokenId) -> &str {
        slice(self.text, self.tokens.span(id))
    }

    fn ty(&self, ty: TypeExpr) -> String {
        match ty {
            TypeExpr::Named(token) => self.token(token).to_owned(),
            TypeExpr::Error => "<error>".to_owned(),
        }
    }

    fn line(&mut self, depth: usize, text: &str, span: Option<Span>) {
        let indented = format!("{:width$}{text}", "", width = depth * 2);
        match span {
            Some(span) => {
                writeln!(self.out, "{indented:<COLUMN$} {}..{}", span.start, span.end).unwrap()
            }
            None => writeln!(self.out, "{indented}").unwrap(),
        }
    }

    /// Dump `node` and everything under it.
    fn walk(&mut self, node: Node, depth: usize) {
        self.stack.push((node, depth));
        while let Some((node, depth)) = self.stack.pop() {
            match node {
                Node::Expr(e) => self.expr(e, depth),
                Node::Stmt(s) => self.stmt(s, depth),
                Node::Stmts(block) => self.children(depth, block),
                Node::Labeled(label, block) => {
                    self.line(depth, label, Some(block.span));
                    self.children(depth + 1, block);
                }
                Node::Text(text) => self.line(depth, text, None),
            }
        }
    }

    /// Push a block's statements, so they pop in order.
    fn children(&mut self, depth: usize, block: Block) {
        for &stmt in self.ast.block(block).iter().rev() {
            self.stack.push((Node::Stmt(stmt), depth));
        }
    }

    fn stmt(&mut self, id: StmtId, depth: usize) {
        let tokens = self.tokens;
        match self.ast.stmts[id] {
            Stmt::Let {
                mutable,
                name,
                ty,
                init,
            } => {
                let keyword = if mutable { "var" } else { "let" };
                let mut text = format!("{keyword} {}", self.token(name));
                if let Some(ty) = ty {
                    write!(text, ": {}", self.ty(ty)).unwrap();
                }
                let span = tokens.span(name).to(self.ast.span(init));
                self.line(depth, &text, Some(span));
                self.stack.push((Node::Expr(init), depth + 1));
            }
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                let op = match op {
                    AssignOp::Set => "=".to_owned(),
                    AssignOp::Compound(op) => format!("{}=", binary(op)),
                };
                self.line(
                    depth,
                    &format!("assign {} {op}", self.token(target)),
                    Some(span),
                );
                self.stack.push((Node::Expr(value), depth + 1));
            }
            Stmt::Expr(e) => self.stack.push((Node::Expr(e), depth)),
            Stmt::While {
                keyword,
                condition,
                body,
            } => {
                let span = tokens.span(keyword).to(body.span);
                self.line(depth, "while", Some(span));
                self.stack.push((Node::Labeled("do", body), depth + 1));
                self.stack.push((Node::Expr(condition), depth + 1));
            }
            Stmt::Return { keyword, value } => {
                let span = match value {
                    Some(value) => tokens.span(keyword).to(self.ast.span(value)),
                    None => tokens.span(keyword),
                };
                self.line(depth, "return", Some(span));
                if let Some(value) = value {
                    self.stack.push((Node::Expr(value), depth + 1));
                }
            }
        }
    }

    fn expr(&mut self, e: ExprId, depth: usize) {
        let span = Some(self.ast.span(e));
        let child = depth + 1;
        match self.ast.exprs[e] {
            Expr::Int { token, negative } => {
                let sign = if negative { "-" } else { "" };
                let text = format!("int {sign}{}", self.token(token));
                self.line(depth, &text, span);
            }
            Expr::Bool(value) => self.line(depth, &format!("bool {value}"), span),
            Expr::Name(token) => {
                let text = format!("name {}", self.token(token));
                self.line(depth, &text, span);
            }
            Expr::Member { base, name } => {
                let text = format!("member .{}", self.token(name));
                self.line(depth, &text, span);
                self.stack.push((Node::Expr(base), child));
            }
            Expr::Unary { op, operand } => {
                let op = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "!",
                };
                self.line(depth, &format!("unary {op}"), span);
                self.stack.push((Node::Expr(operand), child));
            }
            Expr::Binary { op, lhs, rhs } => {
                self.line(depth, &format!("binary {}", binary(op)), span);
                self.stack.push((Node::Expr(rhs), child));
                self.stack.push((Node::Expr(lhs), child));
            }
            Expr::Call { callee, args } => {
                self.line(depth, "call", span);
                for &arg in self.ast.args(args).iter().rev() {
                    self.stack.push((Node::Expr(arg), child));
                }
                self.stack.push((Node::Expr(callee), child));
            }
            Expr::If(id) => {
                let node = self.ast.ifs[id];
                self.line(depth, "if", span);
                match node.otherwise {
                    Else::None => {}
                    Else::Block(block) => self.stack.push((Node::Labeled("else", block), child)),
                    Else::If(nested) => {
                        self.stack.push((Node::Expr(nested), child + 1));
                        self.stack.push((Node::Text("else"), child));
                    }
                }
                self.stack.push((Node::Labeled("then", node.then), child));
                self.stack.push((Node::Expr(node.condition), child));
            }
            Expr::Error => self.line(depth, "error", span),
        }
    }
}

fn binary(op: BinaryOp) -> &'static str {
    match op {
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
    }
}
