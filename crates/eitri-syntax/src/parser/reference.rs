//! The parser as of T008, frozen as a test oracle. Optimized versions of
//! `super` must produce the same tree and diagnostics on every input (see
//! the differential test). Never optimize this copy.
#![allow(dead_code)]

use eitri_base::{SourceId, Span, Symbol, sym};
use eitri_diag::{Code, Diagnostic, Diagnostics, Label};

use super::ModuleKind;
use crate::ast::{
    AssignOp, Ast, BinaryOp, Block, Else, Expr, ExprId, FnDecl, IfExpr, Import, Item, Param, Stmt,
    StmtId, TypeExpr, UnaryOp,
};
use crate::lexer::MAX_NESTING;
use crate::token::{TokenId, TokenKind, Tokens};

use TokenKind as T;

/// The parser's reusable working memory: the tree it fills and the stacks
/// it builds child lists on. Kept by the caller's context (D44).
#[derive(Default)]
pub struct ParseScratch {
    ast: Ast,
    exprs: Vec<ExprId>,
    stmts: Vec<StmtId>,
    params: Vec<Param>,
}

impl ParseScratch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ast(&self) -> &Ast {
        &self.ast
    }

    pub fn into_ast(self) -> Ast {
        self.ast
    }
}

/// Parse `tokens` (lexed from `text`) into `scratch`, replacing its previous
/// tree, and return the tree. Errors go to `diagnostics`.
pub fn parse<'s>(
    text: &str,
    source: SourceId,
    tokens: &Tokens,
    kind: ModuleKind,
    diagnostics: &mut Diagnostics,
    scratch: &'s mut ParseScratch,
) -> &'s Ast {
    scratch.ast.clear();
    scratch.exprs.clear();
    scratch.stmts.clear();
    scratch.params.clear();
    let mut parser = Parser {
        text,
        source,
        tokens,
        kind,
        diagnostics,
        ast: &mut scratch.ast,
        exprs: &mut scratch.exprs,
        stmts: &mut scratch.stmts,
        params: &mut scratch.params,
        pos: 0,
        depth: 0,
        nesting_reported: false,
        in_fn: false,
    };
    parser.module();
    &scratch.ast
}

/// A syntax error has been reported; the caller resynchronizes.
struct Reported;

type Parsed<T> = Result<T, Reported>;

struct Parser<'a> {
    text: &'a str,
    source: SourceId,
    tokens: &'a Tokens,
    kind: ModuleKind,
    diagnostics: &'a mut Diagnostics,
    ast: &'a mut Ast,
    exprs: &'a mut Vec<ExprId>,
    stmts: &'a mut Vec<StmtId>,
    params: &'a mut Vec<Param>,
    pos: u32,
    depth: usize,
    nesting_reported: bool,
    /// Inside a function body, where `return` is allowed.
    in_fn: bool,
}

impl Parser<'_> {
    // ---- token access ----------------------------------------------------

    fn token(&self) -> TokenId {
        TokenId::from_u32(self.pos)
    }

    fn kind(&self) -> TokenKind {
        self.tokens.kind(self.token())
    }

    /// The kind `n` tokens ahead; `Eof` past the end.
    fn nth(&self, n: u32) -> TokenKind {
        let index = (self.pos + n).min(self.tokens.len() as u32 - 1);
        self.tokens.kind(TokenId::from_u32(index))
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.kind() == kind
    }

    fn span_of(&self, token: TokenId) -> Span {
        self.tokens.span(token)
    }

    fn current_span(&self) -> Span {
        self.span_of(self.token())
    }

    /// The previous token's end, for spans that end at what was consumed.
    fn last_end(&self) -> u32 {
        self.span_of(TokenId::from_u32(self.pos.saturating_sub(1)))
            .end
    }

    fn bump(&mut self) -> TokenId {
        let token = self.token();
        if self.kind() != T::Eof {
            self.pos += 1;
        }
        token
    }

    fn eat(&mut self, kind: TokenKind) -> Option<TokenId> {
        self.at(kind).then(|| self.bump())
    }

    /// Two tokens with no space between them, such as the `>` `>` of a
    /// right shift.
    fn adjacent(&self, n: u32) -> bool {
        let a = TokenId::from_u32((self.pos + n).min(self.tokens.len() as u32 - 1));
        let b = TokenId::from_u32((self.pos + n + 1).min(self.tokens.len() as u32 - 1));
        self.span_of(a).end == self.span_of(b).start
    }

    fn symbol(&self) -> Option<Symbol> {
        (self.kind() == T::Ident).then(|| self.tokens.symbol(self.token()))
    }

    // ---- diagnostics -----------------------------------------------------

    fn report(&mut self, code: Code, message: impl Into<String>, span: Span) {
        self.diagnostics
            .push(Diagnostic::error(code, message).with_label(Label::primary(self.source, span)));
    }

    fn unsupported(&mut self, span: Span, feature: &str) {
        self.report(
            Code::Unsupported,
            format!("not supported yet: {feature}"),
            span,
        );
    }

    /// Report "expected X, found Y" at the current token.
    fn expected(&mut self, what: &str) -> Reported {
        let found = self.kind();
        // An unclosed or stray delimiter was already reported by the lexer.
        if found != T::Eof || !self.diagnostics.has_errors() {
            self.report(
                Code::Syntax,
                format!("expected {what}, found {}", found.describe()),
                self.current_span(),
            );
        }
        Reported
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Parsed<TokenId> {
        self.eat(kind).ok_or_else(|| self.expected(what))
    }

    /// Enter one nesting level; `Err` past the limit, reported once.
    fn enter(&mut self) -> Parsed<()> {
        self.depth += 1;
        if self.depth <= MAX_NESTING {
            return Ok(());
        }
        // The lexer reports delimiter nesting with the same limit; one
        // report per cause is enough.
        let already = self.diagnostics.iter().any(|d| d.code == Some(Code::Limit));
        if !self.nesting_reported && !already {
            self.nesting_reported = true;
            self.report(
                Code::Limit,
                format!("implementation limit: syntax nested deeper than {MAX_NESTING}"),
                self.current_span(),
            );
        }
        self.depth -= 1;
        Err(Reported)
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    // ---- skipping --------------------------------------------------------

    /// Step over one token, or a whole bracketed group if it opens one.
    fn skip_one(&mut self) {
        let token = self.token();
        if self.kind().is_open_delimiter()
            && let Some(close) = self.tokens.matching(token)
        {
            self.pos = close.as_u32() + 1;
            return;
        }
        self.bump();
    }

    fn at_statement_end(&self) -> bool {
        matches!(self.kind(), T::Newline | T::Semi | T::RBrace | T::Eof)
    }

    /// Skip to the end of the current statement, crossing groups whole.
    fn skip_statement(&mut self) {
        while !self.at_statement_end() {
            self.skip_one();
        }
    }

    /// Skip an unsupported type up to what can follow a type.
    fn skip_type(&mut self) {
        while !matches!(
            self.kind(),
            T::Comma | T::RParen | T::Eq | T::LBrace | T::Newline | T::Semi | T::RBrace | T::Eof
        ) {
            self.skip_one();
        }
    }

    fn skip_separators(&mut self) {
        while matches!(self.kind(), T::Newline | T::Semi) {
            self.bump();
        }
    }

    /// After a statement: a separator or the end of the enclosing block.
    fn end_statement(&mut self) {
        if self.at_statement_end() {
            return;
        }
        self.expected("a line break or `;`");
        self.skip_statement();
    }

    // ---- module and items ------------------------------------------------

    fn module(&mut self) {
        self.skip_separators();
        let mut items_seen = false;
        while !self.at(T::Eof) {
            let start = self.pos;
            match self.item(&mut items_seen) {
                Ok(()) => self.end_statement(),
                Err(Reported) => self.skip_statement(),
            }
            // A `}` with no opener (reported by the lexer) ends nothing here.
            if self.at(T::RBrace) {
                self.bump();
            }
            self.skip_separators();
            if self.pos == start {
                self.bump();
            }
        }
    }

    fn item(&mut self, items_seen: &mut bool) -> Parsed<()> {
        match self.kind() {
            T::Import => {
                let keyword = self.current_span();
                let import = self.import()?;
                if *items_seen {
                    self.report(
                        Code::UnresolvedImport,
                        "imports must come before other items",
                        keyword,
                    );
                } else {
                    self.ast.imports.push(import);
                }
                return Ok(());
            }
            T::Attribute => {
                self.unsupported(self.current_span(), "attributes");
                self.bump();
                while self.eat(T::Newline).is_some() {}
                return self.item(items_seen);
            }
            T::Pub if self.kind == ModuleKind::Script => {
                self.unsupported(self.current_span(), "`pub`");
                self.bump();
                return self.item(items_seen);
            }
            _ => {}
        }
        *items_seen = true;
        match self.kind() {
            T::Pub => {
                // Host declarations are all `pub`.
                self.bump();
                self.item(items_seen)
            }
            T::Struct if self.kind == ModuleKind::Script => {
                self.struct_decl()?;
                Ok(())
            }
            T::Extend if self.kind == ModuleKind::Script => {
                self.extension()?;
                Ok(())
            }
            T::Enum if self.kind == ModuleKind::Script => {
                self.enum_decl()?;
                Ok(())
            }
            T::Struct | T::Class | T::Enum | T::Interface | T::Extend | T::Type => {
                let keyword = self.kind();
                let feature = format!("{} declarations", keyword.describe());
                self.unsupported(self.current_span(), &feature);
                self.skip_statement();
                Ok(())
            }
            T::Async => {
                self.unsupported(self.current_span(), "`async` functions");
                self.bump();
                self.item(items_seen)
            }
            T::Fn => {
                if let Some(decl) = self.fn_decl()? {
                    let id = self.ast.fns.push(decl);
                    self.ast.items.push(Item::Fn(id));
                }
                Ok(())
            }
            _ if self.kind == ModuleKind::Host => Err(self.expected("a `pub fn` declaration")),
            _ => {
                if let Some(stmt) = self.statement()? {
                    self.ast.items.push(Item::Stmt(stmt));
                }
                Ok(())
            }
        }
    }

    fn import(&mut self) -> Parsed<Import> {
        self.bump();
        let name = self.expect(T::Ident, "a module name")?;
        if self.symbol() != Some(sym::FROM) {
            return Err(self.expected("`from`"));
        }
        self.bump();
        let path = self.expect(T::Str, "an import path string")?;
        let text = &self.text[self.span_of(path).range()];
        let plain = text.starts_with('"')
            && !text.starts_with("\"\"\"")
            && !text.contains('\\')
            && !text.contains("${");
        if !plain {
            self.report(
                Code::UnresolvedImport,
                "an import path is a plain string literal",
                self.span_of(path),
            );
        }
        Ok(Import { name, path })
    }

    fn match_payload(&mut self) -> Parsed<eitri_base::List<crate::ast::MatchBinding>> {
        self.enter()?;
        let result = (|| {
            let mut bindings = Vec::new();
            while !self.at(T::RParen) {
                let label = if self.at(T::Ident) && self.nth(1) == T::Colon {
                    let label = self.bump();
                    self.bump();
                    Some(label)
                } else {
                    None
                };
                let literal = if matches!(
                    self.kind(),
                    T::Int | T::Float | T::Str | T::Char | T::True | T::False | T::Minus | T::Null
                ) {
                    let name = self.token();
                    Some((name, self.prefix(true)?))
                } else {
                    None
                };
                let nested_qualifier = if self.at(T::Ident) && self.nth(1) == T::Dot {
                    Some(self.bump())
                } else {
                    None
                };
                let nested_case = if self.eat(T::Dot).is_some() {
                    Some(self.expect(T::Ident, "a case name")?)
                } else {
                    None
                };
                let nested_bindings = if nested_case.is_some() && self.eat(T::LParen).is_some() {
                    Some(self.match_payload()?)
                } else {
                    None
                };
                let name = if let Some(name) = nested_case {
                    name
                } else if let Some((name, _)) = literal {
                    name
                } else if self.at(T::Wildcard) {
                    self.bump()
                } else {
                    self.expect(T::Ident, "a binding")?
                };
                bindings.push(crate::ast::MatchBinding {
                    label,
                    name,
                    literal: literal.map(|(_, expr)| expr),
                    nested_case,
                    nested_qualifier,
                    nested_bindings,
                });
                if self.eat(T::Comma).is_none() {
                    break;
                }
            }
            self.expect(T::RParen, "`)`")?;
            Ok(self.ast.match_bindings.push(&bindings))
        })();
        self.leave();
        result
    }

    fn match_expr(&mut self) -> Parsed<ExprId> {
        let keyword = self.bump();
        let subject = self.expr(true)?;
        self.expect(T::LBrace, "`{`")?;
        let mut arms = Vec::new();
        self.skip_separators();
        while !self.at(T::RBrace) && !self.at(T::Eof) {
            let literal = if matches!(
                self.kind(),
                T::Int | T::Float | T::Str | T::Char | T::True | T::False | T::Minus | T::Null
            ) {
                Some(self.prefix(true)?)
            } else {
                None
            };
            let (case, qualifier, catch_binding) = if literal.is_some() {
                (None, None, None)
            } else if self.eat(T::Wildcard).is_some() {
                (None, None, None)
            } else if self.at(T::Ident) && self.nth(1) == T::Dot {
                let qualifier = self.bump();
                self.bump();
                (
                    Some(self.expect(T::Ident, "a case name")?),
                    Some(qualifier),
                    None,
                )
            } else if self.at(T::Ident) {
                (None, None, Some(self.bump()))
            } else {
                self.expect(T::Dot, "a case pattern")?;
                (Some(self.expect(T::Ident, "a case name")?), None, None)
            };
            let parenthesized = self.eat(T::LParen).is_some();
            let bindings = if parenthesized {
                self.match_payload()?
            } else {
                eitri_base::List::EMPTY
            };
            let guard = if self.eat(T::If).is_some() {
                Some(self.expr(false)?)
            } else {
                None
            };
            self.expect(T::FatArrow, "`=>`")?;
            let body = if self.at(T::LBrace) {
                crate::ast::MatchBody::Block(self.block()?)
            } else {
                crate::ast::MatchBody::Expr(self.expr(false)?)
            };
            arms.push(crate::ast::MatchArm {
                literal,
                case,
                qualifier,
                catch_binding,
                parenthesized,
                bindings,
                guard,
                body,
            });
            if self.eat(T::Comma).is_none() && !self.at_statement_end() {
                return Err(self.expected("a comma or line break"));
            }
            self.skip_separators();
        }
        let close = self.expect(T::RBrace, "`}`")?;
        let arms = self.ast.match_arms.push(&arms);
        let id = self
            .ast
            .matches
            .push(crate::ast::MatchExpr { subject, arms });
        Ok(self.ast.expr(
            Expr::Match(id),
            self.span_of(keyword).to(self.span_of(close)),
        ))
    }

    fn enum_decl(&mut self) -> Parsed<()> {
        self.bump();
        let name = self.expect(T::Ident, "an enum name")?;
        self.expect(T::LBrace, "`{`")?;
        let mut cases = Vec::new();
        self.skip_separators();
        while !self.at(T::RBrace) && !self.at(T::Eof) {
            let case = self.expect(T::Ident, "a case name")?;
            let fields = if self.at(T::LParen) {
                self.params()?
            } else {
                eitri_base::List::EMPTY
            };
            cases.push(crate::ast::EnumCase { name: case, fields });
            if self.eat(T::Comma).is_none() && !self.at_statement_end() {
                return Err(self.expected("a line break or `,`"));
            }
            self.skip_separators();
        }
        self.expect(T::RBrace, "`}`")?;
        let cases = self.ast.enum_cases.push(&cases);
        self.ast.enums.push(crate::ast::EnumDecl { name, cases });
        Ok(())
    }

    fn extension(&mut self) -> Parsed<()> {
        self.bump();
        let receiver = self.expect(T::Ident, "a type name")?;
        self.expect(T::LBrace, "`{`")?;
        self.skip_separators();
        while !self.at(T::RBrace) && !self.at(T::Eof) {
            let is_static = self.eat(T::Static).is_some();
            let mutating = self.eat(T::Mutating).is_some();
            if is_static && mutating {
                self.unsupported(self.current_span(), "static mutating methods");
            }
            if !self.at(T::Fn) {
                return Err(self.expected("a method"));
            }
            if let Some(mut method) = self.fn_decl()? {
                method.receiver = Some(receiver);
                method.mutating = mutating;
                method.is_static = is_static;
                let id = self.ast.fns.push(method);
                self.ast.items.push(Item::Fn(id));
            }
            if !self.at_statement_end() {
                return Err(self.expected("a line break or `}`"));
            }
            self.skip_separators();
        }
        self.expect(T::RBrace, "`}`")?;
        Ok(())
    }

    fn struct_decl(&mut self) -> Parsed<()> {
        self.bump();
        let name = self.expect(T::Ident, "a struct name")?;
        self.expect(T::LBrace, "`{`")?;
        let mut fields = Vec::new();
        self.skip_separators();
        while !self.at(T::RBrace) && !self.at(T::Eof) {
            let readonly = self.eat(T::Let).is_some();
            let field = self.expect(T::Ident, "a field name")?;
            self.expect(T::Colon, "`:`")?;
            let ty = self.type_expr()?;
            let default = if self.eat(T::Eq).is_some() {
                Some(self.expr(false)?)
            } else {
                None
            };
            fields.push(Param {
                name: field,
                ty,
                readonly,
                default,
            });
            if self.eat(T::Comma).is_none() && !self.at_statement_end() {
                return Err(self.expected("a line break or `,`"));
            }
            self.skip_separators();
        }
        self.expect(T::RBrace, "`}`")?;
        let fields = self.ast.params.push(&fields);
        self.ast
            .structs
            .push(crate::ast::StructDecl { name, fields });
        Ok(())
    }

    fn struct_literal(&mut self) -> Parsed<ExprId> {
        let name = self.bump();
        let span = self.span_of(name);
        self.expect(T::LBrace, "`{`")?;
        let mut fields = Vec::new();
        self.skip_separators();
        while !self.at(T::RBrace) && !self.at(T::Eof) {
            if let Some(name) = self.eat(T::DotDot) {
                let value = self.expr(false)?;
                fields.push(crate::ast::FieldInit { name, value });
                self.eat(T::Comma);
                self.skip_separators();
                break;
            }
            let field = self.expect(T::Ident, "a field name")?;
            self.expect(T::Colon, "`:`")?;
            let value = self.expr(false)?;
            fields.push(crate::ast::FieldInit { name: field, value });
            if self.eat(T::Comma).is_none() && !self.at_statement_end() {
                return Err(self.expected("a line break or `,`"));
            }
            self.skip_separators();
        }
        let end = self.expect(T::RBrace, "`}`")?;
        let fields = self.ast.field_inits.push(&fields);
        Ok(self.ast.expr(
            Expr::StructLiteral { name, fields },
            span.to(self.span_of(end)),
        ))
    }

    /// A function declaration. `None` when it is reported and dropped.
    fn fn_decl(&mut self) -> Parsed<Option<FnDecl>> {
        let keyword = self.bump();
        let name = self.expect(T::Ident, "a function name")?;
        if self.at(T::Lt) {
            self.unsupported(self.current_span(), "type parameters");
            while !matches!(self.kind(), T::LParen | T::Newline | T::Eof) {
                self.skip_one();
            }
        }
        let params = self.params()?;
        let ret = if self.eat(T::Arrow).is_some() {
            self.type_expr()?
        } else {
            self.unsupported(self.span_of(name), "return type inference");
            TypeExpr::Error
        };
        let body = if self.at(T::LBrace) {
            let outer = std::mem::replace(&mut self.in_fn, true);
            let body = self.block();
            self.in_fn = outer;
            Some(body?)
        } else if self.kind == ModuleKind::Host {
            None
        } else {
            self.report(
                Code::Syntax,
                format!("expected a function body, found {}", self.kind().describe()),
                self.current_span(),
            );
            return Err(Reported);
        };
        if self.kind == ModuleKind::Host && body.is_some() {
            self.unsupported(self.span_of(keyword), "function bodies in host modules");
            return Ok(None);
        }
        Ok(Some(FnDecl {
            name,
            receiver: None,
            mutating: false,
            is_static: false,
            params,
            ret,
            body,
        }))
    }

    fn params(&mut self) -> Parsed<eitri_base::List<Param>> {
        let open = self.expect(T::LParen, "`(`")?;
        let mark = self.params.len();
        let result = self.param_list();
        if result.is_err() {
            // Resume after the parameter list.
            self.params.truncate(mark);
            if let Some(close) = self.tokens.matching(open) {
                self.pos = close.as_u32() + 1;
                return Ok(eitri_base::List::EMPTY);
            }
            return Err(Reported);
        }
        Ok(self.ast.params.push_drain(self.params, mark))
    }

    fn param_list(&mut self) -> Parsed<()> {
        while !self.at(T::RParen) {
            let name = self.expect(T::Ident, "a parameter name")?;
            self.expect(T::Colon, "`:` and a parameter type")?;
            let ty = self.type_expr()?;
            if self.eat(T::Eq).is_some() {
                let default = self.expr(false)?;
                self.unsupported(self.ast.span(default), "default arguments");
            }
            self.params.push(Param {
                name,
                ty,
                readonly: false,
                default: None,
            });
            if self.eat(T::Comma).is_none() {
                break;
            }
        }
        self.expect(T::RParen, "`,` or `)`")?;
        Ok(())
    }

    fn type_expr(&mut self) -> Parsed<TypeExpr> {
        let mut optional_tokens = 0;
        while matches!(self.nth(optional_tokens), T::Question | T::QuestionQuestion) {
            optional_tokens += 1;
        }
        if optional_tokens != 0 && self.nth(optional_tokens) == T::Ident {
            let symbol = self
                .tokens
                .symbol(TokenId::from_u32(self.pos + optional_tokens));
            if matches!(
                symbol,
                eitri_base::sym::STRING
                    | eitri_base::sym::I64
                    | eitri_base::sym::BOOL
                    | eitri_base::sym::I8
                    | eitri_base::sym::U8
                    | eitri_base::sym::U64
                    | eitri_base::sym::F32
                    | eitri_base::sym::F64
                    | eitri_base::sym::CHAR
            ) {
                for _ in 0..optional_tokens {
                    self.bump();
                }
                let token = self.bump();
                return Ok(if symbol == eitri_base::sym::STRING {
                    TypeExpr::OptionalString(token)
                } else if symbol == eitri_base::sym::I64 {
                    TypeExpr::OptionalI64(token)
                } else if symbol == eitri_base::sym::BOOL {
                    TypeExpr::OptionalBool(token)
                } else {
                    TypeExpr::OptionalScalar(token)
                });
            }
        }
        let span = self.current_span();
        let feature = match self.kind() {
            T::Ident => {
                let symbol = self.symbol();
                let starts_type = matches!(
                    self.nth(1),
                    T::Ident | T::LBracket | T::Question | T::Lt | T::Fn
                );
                match symbol {
                    Some(s) if s == sym::MAP && self.nth(1) == T::LBracket => "map types",
                    Some(s) if s == sym::TASK && starts_type => "task types",
                    Some(s) if s == sym::WEAK && starts_type => "weak references",
                    Some(s) if s == sym::CHAN && starts_type => "channel types",
                    Some(s) if s == sym::ANY && starts_type => "interface types",
                    _ => match self.nth(1) {
                        T::Dot => "qualified type names",
                        T::Lt => "generic types",
                        T::Bang => "result types",
                        _ => return Ok(TypeExpr::Named(self.bump())),
                    },
                }
            }
            T::Question => "optional types",
            T::LBracket => "array types",
            T::Fn | T::Async => "function types",
            T::Lt => "grouped types",
            _ => return Err(self.expected("a type")),
        };
        self.unsupported(span, feature);
        self.skip_type();
        Ok(TypeExpr::Error)
    }

    // ---- statements ------------------------------------------------------

    /// A statement. `None` when it was unsupported and dropped.
    fn statement(&mut self) -> Parsed<Option<StmtId>> {
        let span = self.current_span();
        let stmt = match self.kind() {
            T::Let | T::Var => return self.binding(),
            T::Return => {
                let keyword = self.bump();
                let value = if self.at_statement_end() {
                    None
                } else {
                    Some(self.expr(false)?)
                };
                if !self.in_fn {
                    self.unsupported(span, "top-level `return`");
                    return Ok(None);
                }
                Stmt::Return { keyword, value }
            }
            T::While => {
                let keyword = self.bump();
                if self.at(T::Let) {
                    self.unsupported(self.current_span(), "`while let`");
                    self.skip_statement();
                    return Ok(None);
                }
                let condition = self.expr(true)?;
                let body = self.block()?;
                Stmt::While {
                    keyword,
                    condition,
                    body,
                }
            }
            T::For => {
                let keyword = self.bump();
                let name = self.expect(T::Ident, "a name")?;
                let second = if self.eat(T::Comma).is_some() {
                    Some(self.expect(T::Ident, "a name")?)
                } else {
                    None
                };
                self.expect(T::In, "`in`")?;
                let start = self.bitwise(true)?;
                let inclusive = self.at(T::DotDotEq);
                let end = if self.at(T::DotDot) || inclusive {
                    self.bump();
                    Some(self.bitwise(true)?)
                } else {
                    None
                };
                let body = self.block()?;
                Stmt::For(self.ast.fors.push(crate::ast::ForStmt {
                    keyword,
                    name,
                    second,
                    start,
                    end,
                    inclusive,
                    body,
                }))
            }
            T::Guard | T::Defer => {
                let feature = match self.kind() {
                    T::Guard => "`guard`",
                    _ => "`defer`",
                };
                self.unsupported(span, feature);
                self.skip_statement();
                return Ok(None);
            }
            T::Break | T::Continue => {
                let is_break = self.at(T::Break);
                let keyword = self.bump();
                if !self.at_statement_end() {
                    self.unsupported(self.current_span(), "loop labels");
                    self.skip_statement();
                    return Ok(None);
                }
                if is_break {
                    Stmt::Break(keyword)
                } else {
                    Stmt::Continue(keyword)
                }
            }
            T::Fn => {
                self.unsupported(span, "nested functions");
                // Parse it so its body cannot cascade, then drop it.
                self.fn_decl()?;
                return Ok(None);
            }
            T::Ident if self.nth(1) == T::Colon && matches!(self.nth(2), T::While | T::For) => {
                self.unsupported(span, "loop labels");
                self.skip_statement();
                return Ok(None);
            }
            T::Else => return Err(self.expected("a statement")),
            _ => return self.expr_statement(),
        };
        Ok(Some(self.ast.stmts.push(stmt)))
    }

    fn binding(&mut self) -> Parsed<Option<StmtId>> {
        let mutable = self.bump_is(T::Var);
        if matches!(self.kind(), T::LBrace | T::LBracket) {
            self.unsupported(self.current_span(), "destructuring");
            self.skip_statement();
            return Ok(None);
        }
        let name = self.expect(T::Ident, "a name")?;
        let ty = if self.eat(T::Colon).is_some() {
            Some(self.type_expr()?)
        } else {
            None
        };
        self.expect(T::Eq, "`=` and an initial value")?;
        let init = self.expr(false)?;
        Ok(Some(self.ast.stmts.push(Stmt::Let {
            mutable,
            name,
            ty,
            init,
        })))
    }

    /// Consume the current token, reporting whether it was `kind`.
    fn bump_is(&mut self, kind: TokenKind) -> bool {
        let is = self.at(kind);
        self.bump();
        is
    }

    fn expr_statement(&mut self) -> Parsed<Option<StmtId>> {
        let target = self.expr(false)?;
        let op_span = self.current_span();
        let op = match self.kind() {
            T::Eq => Some(AssignOp::Set),
            T::PlusEq => Some(AssignOp::Compound(BinaryOp::Add)),
            T::MinusEq => Some(AssignOp::Compound(BinaryOp::Sub)),
            T::StarEq => Some(AssignOp::Compound(BinaryOp::Mul)),
            T::SlashEq => Some(AssignOp::Compound(BinaryOp::Div)),
            T::PercentEq => Some(AssignOp::Compound(BinaryOp::Rem)),
            T::WrapAddEq => Some(AssignOp::Compound(BinaryOp::WrapAdd)),
            T::WrapSubEq => Some(AssignOp::Compound(BinaryOp::WrapSub)),
            T::WrapMulEq => Some(AssignOp::Compound(BinaryOp::WrapMul)),
            T::AmpEq | T::PipeEq | T::CaretEq | T::ShlEq => None,
            // `>>=` arrives as `>` and an adjacent `>=`.
            T::Gt if self.nth(1) == T::GtEq && self.adjacent(0) => None,
            _ => return Ok(Some(self.ast.stmts.push(Stmt::Expr(target)))),
        };
        let Some(op) = op else {
            let end = if self.at(T::Gt) {
                self.span_of(TokenId::from_u32(self.pos + 1)).end
            } else {
                op_span.end
            };
            self.unsupported(Span::new(op_span.start, end), "bitwise operators");
            self.skip_statement();
            return Ok(None);
        };
        self.bump();
        let value = self.expr(false)?;
        let span = self.ast.span(target).to(self.ast.span(value));
        match self.ast.exprs[target] {
            Expr::Name(token) | Expr::SelfValue(token) => {
                Ok(Some(self.ast.stmts.push(Stmt::Assign {
                    target: token,
                    op,
                    value,
                    span,
                })))
            }
            Expr::Error => Ok(None),
            Expr::Member { .. } => Ok(Some(self.ast.stmts.push(Stmt::AssignField {
                target,
                op,
                value,
                span,
            }))),
            _ => {
                self.report(
                    Code::AssignToImmutable,
                    "cannot assign to this expression",
                    self.ast.span(target),
                );
                Ok(None)
            }
        }
    }

    fn block(&mut self) -> Parsed<Block> {
        self.enter()?;
        let result = self.block_inner();
        self.leave();
        result
    }

    fn block_inner(&mut self) -> Parsed<Block> {
        let open = self.expect(T::LBrace, "`{`")?;
        let mark = self.stmts.len();
        self.skip_separators();
        while !matches!(self.kind(), T::RBrace | T::Eof) {
            let start = self.pos;
            match self.statement() {
                Ok(stmt) => {
                    self.stmts.extend(stmt);
                    self.end_statement();
                }
                Err(Reported) => self.skip_statement(),
            }
            self.skip_separators();
            if self.pos == start {
                self.bump();
            }
        }
        // A missing `}` was reported by the lexer as an unclosed delimiter.
        let end = match self.eat(T::RBrace) {
            Some(close) => self.span_of(close).end,
            None => self.last_end(),
        };
        let stmts = self.ast.stmt_lists.push_drain(self.stmts, mark);
        Ok(Block {
            stmts,
            span: Span::new(self.span_of(open).start, end),
        })
    }

    // ---- expressions -----------------------------------------------------

    fn error(&mut self, span: Span) -> ExprId {
        self.ast.expr(Expr::Error, span)
    }

    fn binary(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> ExprId {
        let span = self.ast.span(lhs).to(self.ast.span(rhs));
        let expr = if matches!(self.ast.exprs[lhs], Expr::Error)
            || matches!(self.ast.exprs[rhs], Expr::Error)
        {
            Expr::Error
        } else {
            Expr::Binary { op, lhs, rhs }
        };
        self.ast.expr(expr, span)
    }

    /// An expression. `no_struct` in statement headers (`if`, `while`),
    /// where `name {` starts the block rather than a struct literal.
    fn expr(&mut self, no_struct: bool) -> Parsed<ExprId> {
        self.enter()?;
        let result = self.or(no_struct);
        self.leave();
        result
    }

    fn or(&mut self, ns: bool) -> Parsed<ExprId> {
        let mut lhs = self.and(ns)?;
        while self.eat(T::OrOr).is_some() {
            let rhs = self.and(ns)?;
            lhs = self.binary(BinaryOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    fn and(&mut self, ns: bool) -> Parsed<ExprId> {
        let mut lhs = self.comparison(ns)?;
        while self.eat(T::AndAnd).is_some() {
            let rhs = self.comparison(ns)?;
            lhs = self.binary(BinaryOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    /// The comparison operator at the current token, if any. A `>` that
    /// begins `>>` or `>>=` is a shift, not a comparison.
    fn comparison_op(&self) -> Option<BinaryOp> {
        Some(match self.kind() {
            T::EqEq => BinaryOp::Eq,
            T::NotEq => BinaryOp::NotEq,
            T::Lt => BinaryOp::Lt,
            T::LtEq => BinaryOp::LtEq,
            T::Gt if matches!(self.nth(1), T::Gt | T::GtEq) && self.adjacent(0) => return None,
            T::Gt => BinaryOp::Gt,
            T::GtEq => BinaryOp::GtEq,
            _ => return None,
        })
    }

    fn comparison(&mut self, ns: bool) -> Parsed<ExprId> {
        let lhs = self.coalesce(ns)?;
        let Some(op) = self.comparison_op() else {
            return Ok(lhs);
        };
        self.bump();
        let rhs = self.coalesce(ns)?;
        let mut expr = self.binary(op, lhs, rhs);
        if self.comparison_op().is_some() {
            self.report(
                Code::Syntax,
                "comparison operators cannot be chained",
                self.current_span(),
            );
            while self.comparison_op().is_some() {
                self.bump();
                let rhs = self.coalesce(ns)?;
                let span = self.ast.span(expr).to(self.ast.span(rhs));
                expr = self.error(span);
            }
        }
        Ok(expr)
    }

    /// Parse `lhs <op> rhs…` for an unsupported operator: report it once,
    /// parse the right side so it is consumed, and produce `Error`.
    fn unsupported_operator(
        &mut self,
        lhs: ExprId,
        feature: &str,
        width: u32,
        next: fn(&mut Self, bool) -> Parsed<ExprId>,
        ns: bool,
    ) -> Parsed<ExprId> {
        let start = self.current_span().start;
        let end = self.span_of(TokenId::from_u32(self.pos + width - 1)).end;
        self.unsupported(Span::new(start, end), feature);
        for _ in 0..width {
            self.bump();
        }
        let rhs = next(self, ns)?;
        let span = self.ast.span(lhs).to(self.ast.span(rhs));
        Ok(self.error(span))
    }

    fn coalesce(&mut self, ns: bool) -> Parsed<ExprId> {
        let lhs = self.range(ns)?;
        if self.at(T::QuestionQuestion) {
            return self.unsupported_operator(lhs, "`??`", 1, Self::coalesce, ns);
        }
        Ok(lhs)
    }

    fn range(&mut self, ns: bool) -> Parsed<ExprId> {
        let lhs = self.bitwise(ns)?;
        if matches!(self.kind(), T::DotDot | T::DotDotEq) {
            return self.unsupported_operator(lhs, "ranges", 1, Self::bitwise, ns);
        }
        Ok(lhs)
    }

    /// `|`, `^`, `&`, `<<`, and `>>`: all outside the first slice.
    fn bitwise(&mut self, ns: bool) -> Parsed<ExprId> {
        let mut lhs = self.additive(ns)?;
        loop {
            lhs = match self.kind() {
                T::Pipe | T::Caret | T::Amp => {
                    self.unsupported_operator(lhs, "bitwise operators", 1, Self::additive, ns)?
                }
                T::Shl => self.unsupported_operator(lhs, "shifts", 1, Self::additive, ns)?,
                T::Gt if self.nth(1) == T::Gt && self.adjacent(0) => {
                    self.unsupported_operator(lhs, "shifts", 2, Self::additive, ns)?
                }
                _ => return Ok(lhs),
            };
        }
    }

    fn additive(&mut self, ns: bool) -> Parsed<ExprId> {
        let mut lhs = self.multiplicative(ns)?;
        loop {
            let op = match self.kind() {
                T::Plus => BinaryOp::Add,
                T::Minus => BinaryOp::Sub,
                T::WrapAdd => BinaryOp::WrapAdd,
                T::WrapSub => BinaryOp::WrapSub,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.multiplicative(ns)?;
            lhs = self.binary(op, lhs, rhs);
        }
    }

    fn multiplicative(&mut self, ns: bool) -> Parsed<ExprId> {
        let mut lhs = self.prefix(ns)?;
        loop {
            let op = match self.kind() {
                T::Star => BinaryOp::Mul,
                T::Slash => BinaryOp::Div,
                T::Percent => BinaryOp::Rem,
                T::WrapMul => BinaryOp::WrapMul,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.prefix(ns)?;
            lhs = self.binary(op, lhs, rhs);
        }
    }

    fn prefix(&mut self, ns: bool) -> Parsed<ExprId> {
        let start = self.current_span();
        let op = match self.kind() {
            T::Minus => UnaryOp::Neg,
            T::Bang => UnaryOp::Not,
            T::Tilde | T::Try | T::TryQuestion | T::Await | T::Spawn => {
                let feature = match self.kind() {
                    T::Tilde => "bitwise operators",
                    T::Try | T::TryQuestion => "`try`",
                    T::Await => "`await`",
                    _ => "`spawn`",
                };
                self.unsupported(start, feature);
                self.bump();
                self.enter()?;
                let operand = self.prefix(ns);
                self.leave();
                let span = start.to(self.ast.span(operand?));
                return Ok(self.error(span));
            }
            _ => return self.postfix(ns),
        };
        self.bump();
        // `-` directly on a literal is one negative constant, unless a
        // postfix operation binds the literal first.
        if op == UnaryOp::Neg
            && self.at(T::Int)
            && !matches!(
                self.nth(1),
                T::Dot | T::LParen | T::LBracket | T::QuestionDot | T::QuestionBracket
            )
        {
            let token = self.bump();
            let span = start.to(self.span_of(token));
            return Ok(self.ast.expr(
                Expr::Int {
                    token,
                    negative: true,
                },
                span,
            ));
        }
        self.enter()?;
        let operand = self.prefix(ns);
        self.leave();
        let operand = operand?;
        let span = start.to(self.ast.span(operand));
        let expr = if matches!(self.ast.exprs[operand], Expr::Error) {
            Expr::Error
        } else {
            Expr::Unary { op, operand }
        };
        Ok(self.ast.expr(expr, span))
    }

    fn postfix(&mut self, ns: bool) -> Parsed<ExprId> {
        let mut expr = self.primary(ns)?;
        loop {
            let start = self.ast.span(expr);
            match self.kind() {
                T::Dot => {
                    self.bump();
                    let name = self.expect(T::Ident, "a member name")?;
                    let span = start.to(self.span_of(name));
                    let member = if matches!(self.ast.exprs[expr], Expr::Error) {
                        Expr::Error
                    } else {
                        Expr::Member { base: expr, name }
                    };
                    expr = self.ast.expr(member, span);
                }
                T::LParen => expr = self.call(expr)?,
                T::QuestionDot | T::LBracket | T::QuestionBracket => {
                    let feature = if self.at(T::QuestionDot) {
                        "optional chaining"
                    } else {
                        "indexing"
                    };
                    self.unsupported(self.current_span(), feature);
                    self.skip_one();
                    if self.at(T::Ident) {
                        self.bump();
                    }
                    let span = Span::new(start.start, self.last_end());
                    expr = self.error(span);
                }
                // `f() else e => fallback`
                T::Else
                    if matches!(self.nth(1), T::Ident | T::Wildcard)
                        && self.nth(2) == T::FatArrow =>
                {
                    self.unsupported(self.current_span(), "`else` recovery");
                    self.skip_statement();
                    let span = Span::new(start.start, self.last_end());
                    return Ok(self.error(span));
                }
                _ => return Ok(expr),
            }
        }
    }

    fn named_call(&mut self, callee: ExprId) -> Parsed<ExprId> {
        self.bump();
        let mut args = Vec::new();
        loop {
            let name = self.expect(T::Ident, "an argument label")?;
            self.expect(T::Colon, "`:`")?;
            let value = self.expr(false)?;
            args.push(crate::ast::FieldInit { name, value });
            if self.eat(T::Comma).is_none() {
                break;
            }
        }
        let close = self.expect(T::RParen, "`)`")?;
        let span = self.ast.span(callee).to(self.span_of(close));
        let args = self.ast.field_inits.push(&args);
        Ok(self.ast.expr(Expr::NamedCall { callee, args }, span))
    }

    fn enum_call(&mut self, callee: ExprId) -> Parsed<ExprId> {
        let open = self.bump();
        let mut fields = Vec::new();
        let mut values = Vec::new();
        let mut labeled = false;
        while !self.at(T::RParen) {
            let name = if self.at(T::Ident) && self.nth(1) == T::Colon {
                labeled = true;
                let name = self.bump();
                self.bump();
                name
            } else {
                open
            };
            let value = self.expr(false)?;
            values.push(value);
            fields.push(crate::ast::FieldInit { name, value });
            if self.eat(T::Comma).is_none() {
                break;
            }
        }
        let close = self.expect(T::RParen, "`)`")?;
        let span = self.ast.span(callee).to(self.span_of(close));
        if labeled {
            let args = self.ast.field_inits.push(&fields);
            Ok(self.ast.expr(Expr::NamedCall { callee, args }, span))
        } else {
            let args = self.ast.expr_lists.push(&values);
            Ok(self.ast.expr(Expr::Call { callee, args }, span))
        }
    }

    fn call(&mut self, callee: ExprId) -> Parsed<ExprId> {
        if matches!(self.ast.exprs[callee], Expr::EnumShorthand(_))
            || matches!(self.ast.exprs[callee], Expr::Member { name, .. } if self.text[self.span_of(name).range()].chars().next().is_some_and(char::is_uppercase))
        {
            return self.enum_call(callee);
        }
        if self.nth(1) == T::Ident
            && self.nth(2) == T::Colon
            && !matches!(self.ast.exprs[callee], Expr::Name(token) if self.tokens.symbol(token).is_type_name())
        {
            return self.named_call(callee);
        }
        let open = self.bump();
        let mark = self.exprs.len();
        if self.at(T::Ident)
            && self.nth(1) == T::Colon
            && &self.text[self.current_span().range()] == "truncating"
            && matches!(self.ast.exprs[callee], Expr::Name(token) if self.tokens.symbol(token).is_type_name())
        {
            self.bump();
            self.bump();
            let value = self.expr(false)?;
            let close = self.expect(T::RParen, "`)`")?;
            let span = self.ast.span(callee).to(self.span_of(close));
            return Ok(self.ast.expr(Expr::Convert { callee, value }, span));
        }
        let mut labeled = false;
        while !self.at(T::RParen) {
            if self.at(T::Ident) && self.nth(1) == T::Colon {
                if !labeled {
                    self.unsupported(self.current_span(), "labeled arguments");
                }
                labeled = true;
                self.bump();
                self.bump();
            }
            match self.expr(false) {
                Ok(arg) => self.exprs.push(arg),
                Err(Reported) => {
                    self.exprs.truncate(mark);
                    return Err(Reported);
                }
            }
            if self.eat(T::Comma).is_none() {
                break;
            }
        }
        let close = match self.eat(T::RParen) {
            Some(close) => close,
            None => {
                self.exprs.truncate(mark);
                // Resume after the argument list if it is closed later.
                let reported = self.expected("`,` or `)`");
                if let Some(close) = self.tokens.matching(open) {
                    self.pos = close.as_u32();
                    self.bump();
                    let span = self.ast.span(callee).to(self.span_of(close));
                    return Ok(self.error(span));
                }
                return Err(reported);
            }
        };
        let span = self.ast.span(callee).to(self.span_of(close));
        let poisoned = labeled
            || matches!(self.ast.exprs[callee], Expr::Error)
            || self.exprs[mark..]
                .iter()
                .any(|&arg| matches!(self.ast.exprs[arg], Expr::Error));
        if poisoned {
            self.exprs.truncate(mark);
            return Ok(self.error(span));
        }
        let args = self.ast.expr_lists.push_drain(self.exprs, mark);
        Ok(self.ast.expr(Expr::Call { callee, args }, span))
    }

    /// Report an unsupported primary at the current token and skip it,
    /// with its group if it opens one.
    fn unsupported_primary(&mut self, feature: &str) -> ExprId {
        let span = self.current_span();
        self.unsupported(span, feature);
        self.skip_one();
        self.error(Span::new(span.start, self.last_end()))
    }

    /// A lambda starting at the current token: report its `=>`, skip its
    /// parameters, and consume its body.
    fn lambda(&mut self, arrow: u32) -> Parsed<ExprId> {
        let start = self.current_span().start;
        self.unsupported(self.span_of(TokenId::from_u32(arrow)), "lambdas");
        self.pos = arrow + 1;
        if self.at(T::LBrace) {
            self.skip_one();
        } else {
            self.expr(false)?;
        }
        let span = Span::new(start, self.last_end());
        Ok(self.error(span))
    }

    fn primary(&mut self, ns: bool) -> Parsed<ExprId> {
        let token = self.token();
        let span = self.current_span();
        Ok(match self.kind() {
            T::Int => {
                self.bump();
                self.ast.expr(
                    Expr::Int {
                        token,
                        negative: false,
                    },
                    span,
                )
            }
            T::True | T::False => {
                let value = self.at(T::True);
                self.bump();
                self.ast.expr(Expr::Bool(value), span)
            }
            T::Float => self.unsupported_primary("float literals"),
            T::Str => self.unsupported_primary("string literals"),
            T::Char => self.unsupported_primary("char literals"),
            T::Null => {
                self.bump();
                self.ast.expr(Expr::Null(token), span)
            }
            T::Dot => {
                self.bump();
                let case = self.expect(T::Ident, "an enum case")?;
                self.ast
                    .expr(Expr::EnumShorthand(case), span.to(self.span_of(case)))
            }
            T::SelfLower => {
                self.bump();
                self.ast.expr(Expr::SelfValue(token), span)
            }
            T::LBracket => self.unsupported_primary("array literals"),
            // In a statement header, `{` where an operand belongs is a
            // missing operand (`if x > {` mid-edit), not a block expression;
            // leave the block for the statement to skip.
            T::LBrace if ns => return Err(self.expected("an expression")),
            T::LBrace => self.unsupported_primary("block expressions"),
            T::Ident if self.nth(1) == T::FatArrow => self.lambda(self.pos + 1)?,
            T::Ident if !ns && self.nth(1) == T::LBrace => self.struct_literal()?,
            T::Ident => {
                self.bump();
                self.ast.expr(Expr::Name(token), span)
            }
            T::LParen => {
                if let Some(close) = self.tokens.matching(token)
                    && self.tokens.kind(TokenId::from_u32(close.as_u32() + 1)) == T::FatArrow
                {
                    return self.lambda(close.as_u32() + 1);
                }
                self.bump();
                let inner = self.expr(false)?;
                self.expect(T::RParen, "`)`")?;
                inner
            }
            T::Async => {
                self.unsupported(span, "`async` lambdas");
                self.bump();
                self.skip_statement();
                self.error(Span::new(span.start, self.last_end()))
            }
            T::Match => self.match_expr()?,
            T::If => self.if_expr()?,
            _ => return Err(self.expected("an expression")),
        })
    }

    /// An `if`. Its blocks count toward the nesting limit, not the `if`
    /// itself, so nested `if`s nest as deep as their braces.
    fn if_expr(&mut self) -> Parsed<ExprId> {
        let start = self.current_span().start;
        self.bump();
        if self.at(T::Let) {
            self.unsupported(self.current_span(), "`if let`");
            while !matches!(self.kind(), T::LBrace | T::Newline | T::Eof) {
                self.skip_one();
            }
            self.skip_one();
            if self.at(T::Else) {
                self.bump();
                self.skip_one();
            }
            return Ok(self.error(Span::new(start, self.last_end())));
        }
        let condition = self.expr(true)?;
        let then = self.block()?;
        let otherwise = if self.eat(T::Else).is_some() {
            if self.at(T::If) {
                Else::If(self.if_expr()?)
            } else {
                Else::Block(self.block()?)
            }
        } else {
            Else::None
        };
        let span = Span::new(start, self.last_end());
        let id = self.ast.ifs.push(IfExpr {
            condition,
            then,
            otherwise,
        });
        Ok(self.ast.expr(Expr::If(id), span))
    }
}
