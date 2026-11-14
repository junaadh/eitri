use eitri_base::{Interner, SourceId};
use eitri_diag::Diagnostics;

use super::*;
use crate::ast::{Ast, Else, Expr, ExprId, Item, Stmt, TypeExpr};
use crate::lexer::{LexScratch, lex};

/// The tree as an S-expression, and the diagnostics as `code start..end message`.
fn parse_text(text: &str, kind: ModuleKind) -> (String, Vec<String>) {
    parse_with(text, kind, false)
}

/// Like `parse_text`, through the frozen reference parser when `reference`.
fn parse_with(text: &str, kind: ModuleKind, reference: bool) -> (String, Vec<String>) {
    let mut interner = Interner::new();
    let mut diagnostics = Diagnostics::new(usize::MAX);
    let mut lex_scratch = LexScratch::new();
    let mut scratch = ParseScratch::new();
    let mut reference_scratch = super::reference::ParseScratch::new();
    let source = SourceId::from_u32(0);
    let tokens = lex(
        text,
        source,
        &mut interner,
        &mut diagnostics,
        &mut lex_scratch,
    );
    let ast = if reference {
        super::reference::parse(
            text,
            source,
            tokens,
            kind,
            &mut diagnostics,
            &mut reference_scratch,
        )
    } else {
        parse(text, source, tokens, kind, &mut diagnostics, &mut scratch)
    };
    let printer = Printer { text, tokens, ast };
    let mut out = Vec::new();
    for import in &ast.imports {
        out.push(format!(
            "(import {} {})",
            printer.token(import.name),
            printer.token(import.path)
        ));
    }
    for item in &ast.items {
        out.push(match *item {
            Item::Fn(id) => printer.function(id),
            Item::Stmt(id) => printer.stmt(id),
        });
    }
    let errors = diagnostics
        .iter()
        .map(|d| {
            let s = d.primary().unwrap().span;
            format!("{} {}..{} {}", d.code.unwrap(), s.start, s.end, d.message)
        })
        .collect();
    (out.join("\n"), errors)
}

/// Parse a valid script: no diagnostics allowed.
fn tree(text: &str) -> String {
    let (tree, errors) = parse_text(text, ModuleKind::Script);
    assert!(errors.is_empty(), "{text:?}: {errors:?}");
    tree
}

fn errors(text: &str) -> Vec<String> {
    parse_text(text, ModuleKind::Script).1
}

/// Diagnostics only, without printing the tree (which recurses, so it
/// would overflow on deliberately deep trees).
fn errors_only(text: &str) -> Vec<String> {
    let mut interner = Interner::new();
    let mut diagnostics = Diagnostics::new(100);
    let mut lex_scratch = LexScratch::new();
    let mut scratch = ParseScratch::new();
    let source = SourceId::from_u32(0);
    let tokens = lex(
        text,
        source,
        &mut interner,
        &mut diagnostics,
        &mut lex_scratch,
    );
    parse(
        text,
        source,
        tokens,
        ModuleKind::Script,
        &mut diagnostics,
        &mut scratch,
    );
    diagnostics
        .iter()
        .map(|d| format!("{} {}", d.code.unwrap(), d.message))
        .collect()
}

struct Printer<'a> {
    text: &'a str,
    tokens: &'a Tokens,
    ast: &'a Ast,
}

impl Printer<'_> {
    fn token(&self, token: TokenId) -> &str {
        &self.text[self.tokens.span(token).range()]
    }

    fn ty(&self, ty: TypeExpr) -> String {
        match ty {
            TypeExpr::Named(token) => self.token(token).to_owned(),
            TypeExpr::OptionalString(token)
            | TypeExpr::OptionalI64(token)
            | TypeExpr::OptionalBool(token)
            | TypeExpr::OptionalScalar(token)
            | TypeExpr::OptionalNamed(token) => {
                format!("?{}", self.token(token))
            }
            TypeExpr::Error => "<error>".into(),
        }
    }

    fn function(&self, id: crate::ast::FnId) -> String {
        let f = &self.ast.fns[id];
        let params: Vec<String> = self
            .ast
            .params(f.params)
            .iter()
            .map(|p| format!("{}: {}", self.token(p.name), self.ty(p.ty)))
            .collect();
        let body = f.body.map_or("<none>".into(), |b| self.block(b));
        format!(
            "(fn {} ({}) {} {})",
            self.token(f.name),
            params.join(", "),
            self.ty(f.ret),
            body
        )
    }

    fn block(&self, block: Block) -> String {
        let stmts: Vec<String> = self
            .ast
            .block(block)
            .iter()
            .map(|&s| self.stmt(s))
            .collect();
        format!("{{{}}}", stmts.join(" "))
    }

    fn stmt(&self, id: StmtId) -> String {
        match self.ast.stmts[id] {
            Stmt::Let {
                mutable,
                name,
                ty,
                init,
            } => format!(
                "({} {}{} {})",
                if mutable { "var" } else { "let" },
                self.token(name),
                ty.map_or(String::new(), |t| format!(": {}", self.ty(t))),
                self.expr(init)
            ),
            Stmt::Assign {
                target, op, value, ..
            } => {
                let op = match op {
                    AssignOp::Set => "=".to_owned(),
                    AssignOp::Compound(op) => format!("{op:?}="),
                };
                format!("({op} {} {})", self.token(target), self.expr(value))
            }
            Stmt::AssignField { target, value, .. } => {
                format!("(assign-field {} {})", self.expr(target), self.expr(value))
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::For(for_id) => {
                let crate::ast::ForStmt {
                    start, end, body, ..
                } = self.ast.fors[for_id];
                format!(
                    "(for {} {:?} {})",
                    self.expr(start),
                    end.map(|e| self.expr(e)),
                    self.block(body)
                )
            }
            Stmt::While {
                condition, body, ..
            } => format!("(while {} {})", self.expr(condition), self.block(body)),
            Stmt::Break(_) => "(break)".into(),
            Stmt::Continue(_) => "(continue)".into(),
            Stmt::Return { value, .. } => match value {
                Some(v) => format!("(return {})", self.expr(v)),
                None => "(return)".into(),
            },
        }
    }

    fn expr(&self, id: ExprId) -> String {
        match self.ast.exprs[id] {
            Expr::Int { token, negative } | Expr::Float { token, negative } => {
                format!("{}{}", if negative { "-" } else { "" }, self.token(token))
            }
            Expr::Char(token) | Expr::String(token) | Expr::Null(token) => {
                self.token(token).to_owned()
            }
            Expr::StringPart(span) => self.text[span.range()].to_owned(),
            Expr::Interpolation(parts) => format!(
                "(interpolation {})",
                self.ast
                    .args(parts)
                    .iter()
                    .map(|&e| self.expr(e))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            Expr::Convert { callee, value } => {
                format!("(truncating {} {})", self.expr(callee), self.expr(value))
            }
            Expr::Bool(b) => b.to_string(),
            Expr::StructLiteral { name, fields } => format!(
                "(struct {} {})",
                self.token(name),
                self.ast.field_inits[fields]
                    .iter()
                    .map(|f| format!("{}:{}", self.token(f.name), self.expr(f.value)))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            Expr::Match(id) => format!("(match {})", self.expr(self.ast.matches[id].subject)),
            Expr::EnumShorthand(token) => format!(".{}", self.token(token)),
            Expr::NamedCall { callee, args } => format!(
                "(named {} {})",
                self.expr(callee),
                self.ast.field_inits[args]
                    .iter()
                    .map(|a| format!("{}:{}", self.token(a.name), self.expr(a.value)))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            Expr::SelfValue(token) | Expr::Name(token) => self.token(token).to_owned(),
            Expr::Member { base, name } => format!("{}.{}", self.expr(base), self.token(name)),
            Expr::Unary { op, operand } => format!("({op:?} {})", self.expr(operand)),
            Expr::Binary { op, lhs, rhs } => {
                format!("({op:?} {} {})", self.expr(lhs), self.expr(rhs))
            }
            Expr::Call { callee, args } => {
                let args: Vec<String> = self.ast.args(args).iter().map(|&a| self.expr(a)).collect();
                format!("(call {} {})", self.expr(callee), args.join(" "))
            }
            Expr::If(id) => {
                let crate::ast::IfExpr {
                    condition,
                    then,
                    otherwise,
                } = self.ast.ifs[id];
                let otherwise = match otherwise {
                    Else::None => String::new(),
                    Else::Block(b) => format!(" {}", self.block(b)),
                    Else::If(e) => format!(" {}", self.expr(e)),
                };
                format!(
                    "(if {} {}{otherwise})",
                    self.expr(condition),
                    self.block(then)
                )
            }
            Expr::Error => "<error>".into(),
        }
    }
}

#[test]
fn precedence_and_associativity() {
    assert_eq!(tree("1 + 2 * 3 - 4"), "(Sub (Add 1 (Mul 2 3)) 4)");
    assert_eq!(tree("a || b && c == d"), "(Or a (And b (Eq c d)))");
    assert_eq!(tree("a &+ b &* c"), "(WrapAdd a (WrapMul b c))");
    assert_eq!(tree("100 / 10 / 5 % 3"), "(Rem (Div (Div 100 10) 5) 3)");
    assert_eq!(tree("(1 + 2) * 3"), "(Mul (Add 1 2) 3)");
    assert_eq!(tree("!a == b"), "(Eq (Not a) b)");
    assert_eq!(tree("a + 1 < b * 2"), "(Lt (Add a 1) (Mul b 2))");
    assert_eq!(tree("x >= y"), "(GtEq x y)");
}

#[test]
fn negative_literals_fold_only_directly() {
    assert_eq!(
        tree("-9_223_372_036_854_775_808"),
        "-9_223_372_036_854_775_808"
    );
    assert_eq!(tree("-7 / 2"), "(Div -7 2)");
    assert_eq!(tree("-x"), "(Neg x)");
    assert_eq!(tree("- -1"), "(Neg -1)");
    assert_eq!(tree("-(1)"), "(Neg 1)");
}

#[test]
fn statements_and_functions() {
    assert_eq!(
        tree("import test from \"test\"\nlet a: i64 = 1; var b = a\nb += 2\nb &*= 3\nb = b"),
        "(import test \"test\")\n(let a: i64 1)\n(var b a)\n(Add= b 2)\n(WrapMul= b 3)\n(= b b)"
    );
    assert_eq!(
        tree("fn add(a: i64, b: i64,) -> i64 {\n    return a + b\n}\nfn f() -> void { return }"),
        "(fn add (a: i64, b: i64) i64 {(return (Add a b))})\n(fn f () void {(return)})"
    );
    assert_eq!(
        tree("while i < 10 {\n  i += 1\n}"),
        "(while (Lt i 10) {(Add= i 1)})"
    );
    assert_eq!(
        tree("test.emit(f(1, 2), x)"),
        "(call test.emit (call f 1 2) x)"
    );
    assert_eq!(
        tree("let s = if n < 0 { -1 } else if n == 0 { 0 } else { 1 }"),
        "(let s (if (Lt n 0) {-1} (if (Eq n 0) {0} {1})))"
    );
    assert_eq!(tree("if ready {\n  go()\n}"), "(if ready {(call go )})");
}

#[test]
fn newlines_and_separators() {
    assert_eq!(
        tree("\n\nlet a = 1\n\n\nlet b = 2;;\n"),
        "(let a 1)\n(let b 2)"
    );
    assert_eq!(tree("let c = a +\n    b"), "(let c (Add a b))");
    assert_eq!(tree("f(a,\n  b)"), "(call f a b)");
}

#[test]
fn syntax_errors_resynchronize() {
    assert_eq!(
        errors("let x = 1 < 2 < 3"),
        ["E0102 14..15 comparison operators cannot be chained"]
    );
    assert_eq!(
        errors("if true {\n}\nelse {\n}"),
        ["E0102 12..16 expected a statement, found `else`"]
    );
    // Two independent errors, each reported once.
    assert_eq!(
        errors("let = 1\nlet ok = 2\nlet y 3"),
        [
            "E0102 4..5 expected a name, found `=`",
            "E0102 25..26 expected `=` and an initial value, found integer literal",
        ]
    );
    assert_eq!(
        errors("let a = 1 2"),
        ["E0102 10..11 expected a line break or `;`, found integer literal"]
    );
    assert_eq!(
        errors("f(1, ,)"),
        ["E0102 5..6 expected an expression, found `,`"]
    );
    assert_eq!(
        errors("fn f(->) -> void {}"),
        ["E0102 5..7 expected a parameter name, found `->`"]
    );
    assert_eq!(
        errors("fn f() -> void"),
        ["E0102 14..14 expected a function body, found end of input"]
    );
    assert_eq!(
        errors("1 = 2"),
        ["E0303 0..1 cannot assign to this expression"]
    );
}

#[test]
fn unsupported_features_are_reported_once_and_dropped() {
    let cases: &[(&str, &str)] = &[
        (
            "fn f(x: ?i32) -> void {\n}",
            "E0001 8..9 not supported yet: optional types",
        ),
        (
            "fn f(x: []i64) -> void {\n}",
            "E0001 8..9 not supported yet: array types",
        ),
        (
            "fn f(x: map[string]i64) -> void {\n}",
            "E0001 8..11 not supported yet: map types",
        ),
        (
            "fn f(x: fn(i64) -> i64) -> void {\n}",
            "E0001 8..10 not supported yet: function types",
        ),
        (
            "fn f(x: E!i64) -> void {\n}",
            "E0001 8..9 not supported yet: result types",
        ),
        (
            "pub fn f() -> void {\n}",
            "E0001 0..3 not supported yet: `pub`",
        ),
        (
            "@extern\nfn f() -> void {\n}",
            "E0001 0..7 not supported yet: attributes",
        ),
        (
            "let x = {\n    1\n}",
            "E0001 8..9 not supported yet: block expressions",
        ),
        (
            "let f = (n: i64) => n",
            "E0001 17..19 not supported yet: lambdas",
        ),
        (
            "let f = n => n + 1",
            "E0001 10..12 not supported yet: lambdas",
        ),
        ("let r = 0..3", "E0001 9..11 not supported yet: ranges"),
        (
            "fn outer() -> void {\n    fn inner() -> void {\n    }\n}",
            "E0001 25..27 not supported yet: nested functions",
        ),
        (
            "fn f() {\n}",
            "E0001 3..4 not supported yet: return type inference",
        ),
        (
            "fn f(n: i64 = 1) -> void {\n}",
            "E0001 14..15 not supported yet: default arguments",
        ),
        ("return", "E0001 0..6 not supported yet: top-level `return`"),
        (
            "let { x } = p",
            "E0001 4..5 not supported yet: destructuring",
        ),
        ("let x = try f()", "E0001 8..11 not supported yet: `try`"),
        ("let x = a[0]", "E0001 9..10 not supported yet: indexing"),
        (
            "let x = a?.b",
            "E0001 9..11 not supported yet: optional chaining",
        ),
        (
            "let a = [1, 2]",
            "E0001 8..9 not supported yet: array literals",
        ),
        (
            "let x = f() else e => 0",
            "E0001 12..16 not supported yet: `else` recovery",
        ),
        (
            "if let x = y {\n}",
            "E0001 3..6 not supported yet: `if let`",
        ),
        (
            "outer: while true {\n}",
            "E0001 0..5 not supported yet: loop labels",
        ),
        ("guard x else {\n}", "E0001 0..5 not supported yet: `guard`"),
        ("defer {\n}", "E0001 0..5 not supported yet: `defer`"),
    ];
    for &(text, expected) in cases {
        assert_eq!(errors(text), [expected], "{text:?}");
    }
    // The rest of the program still parses normally.
    let (tree, errors) = parse_text("let x = try y\nlet z = 2", ModuleKind::Script);
    assert_eq!(errors.len(), 1);
    assert_eq!(tree, "(let x <error>)\n(let z 2)");
}

#[test]
fn imports_come_first() {
    assert_eq!(
        errors("let x = 1\nimport test from \"test\""),
        ["E0205 10..16 imports must come before other items"]
    );
    assert_eq!(
        errors("import t from \"a${b}\""),
        ["E0205 14..21 an import path is a plain string literal"]
    );
    assert_eq!(
        errors("import t \"x\""),
        ["E0102 9..12 expected `from`, found string literal"]
    );
}

#[test]
fn host_declarations() {
    let (tree, errors) = parse_text(
        "pub fn emit(value: i64) -> void\npub fn check(value: bool) -> bool\n",
        ModuleKind::Host,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        tree,
        "(fn emit (value: i64) void <none>)\n(fn check (value: bool) bool <none>)"
    );
    assert_eq!(
        parse_text("pub fn f() -> void { }", ModuleKind::Host).1,
        ["E0001 4..6 not supported yet: function bodies in host modules"]
    );
    assert_eq!(
        parse_text("let x = 1", ModuleKind::Host).1,
        ["E0102 0..3 expected a `pub fn` declaration, found `let`"]
    );
}

#[test]
fn nesting_limit_protects_the_stack() {
    for text in [
        "-".repeat(10_000) + "1",
        "!".repeat(10_000) + "x",
        "(".repeat(300) + "1" + &")".repeat(300),
        "if a { ".repeat(300) + &"}".repeat(300),
    ] {
        let errors = errors_only(&text);
        let limits: Vec<_> = errors.iter().filter(|e| e.starts_with("E0002")).collect();
        assert_eq!(limits.len(), 1, "{:?}", &errors[..errors.len().min(3)]);
    }
    // At the limit there is no error.
    assert!(errors_only(&("(".repeat(200) + "1" + &")".repeat(200))).is_empty());
}

#[test]
fn hostile_inputs_stay_linear() {
    let n = 100_000;
    let long_sum = "1".to_owned() + &" + 1".repeat(n);
    assert!(errors_only(&long_sum).is_empty());
    let many_statements = "let x = 1\n".repeat(n / 10);
    assert!(errors_only(&many_statements).is_empty());
    // Garbage tokens: every one produces at most a bounded number of diagnostics.
    let garbage = ") ] , = => . ".repeat(n / 10);
    assert!(!errors_only(&garbage).is_empty());
}

#[test]
fn scratch_is_reused() {
    let mut interner = Interner::new();
    let mut diagnostics = Diagnostics::new(100);
    let mut lex_scratch = LexScratch::new();
    let mut scratch = ParseScratch::new();
    let source = SourceId::from_u32(0);
    for text in [
        "fn f(a: i64) -> i64 { return a + 1 }\nf(2)",
        "x",
        "fn f(a: i64) -> i64 { return a + 1 }\nf(2)",
    ] {
        interner.reset();
        let tokens = lex(
            text,
            source,
            &mut interner,
            &mut diagnostics,
            &mut lex_scratch,
        );
        let ast = parse(
            text,
            source,
            tokens,
            ModuleKind::Script,
            &mut diagnostics,
            &mut scratch,
        );
        assert_eq!(ast.items.len(), if text == "x" { 1 } else { 2 });
    }
    assert!(diagnostics.is_empty());
}

#[test]
fn differential_against_the_reference_parser() {
    use crate::test_support::{FRAGMENTS, Rng, corpus, mutate};
    let check = |text: &str, case: &str| {
        if !m0_reference_input(text) {
            return;
        }
        for kind in [ModuleKind::Script, ModuleKind::Host] {
            let expected = parse_with(text, kind, true);
            let actual = parse_with(text, kind, false);
            assert!(
                expected == actual,
                "{case}: optimized parser differs from the reference on {text:?}\n\
                 --- reference\n{expected:?}\n--- optimized\n{actual:?}"
            );
        }
    };
    let corpus = corpus();
    for (i, text) in corpus.iter().enumerate() {
        check(text, &format!("corpus {i}"));
    }
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for case in 0..10_000 {
        let text = if case % 2 == 0 {
            let pick = rng.below(corpus.len());
            mutate(&mut rng, &corpus[pick])
        } else {
            let mut text = String::new();
            for _ in 0..rng.below(24) {
                text += FRAGMENTS[rng.below(FRAGMENTS.len())];
            }
            text
        };
        check(&text, &format!("case {case}"));
    }
}

#[test]
fn operator_edge_cases_match_the_reference() {
    let cases = [
        "let x = a < b < c && d < e < f",
        "let x = a == b..c..d",
        "let x = a..b..c",
        "let x = a..b + c",
        "let x = a + b..c * d",
        "let x = a ?? b..c ?? d",
        "let x = a ?? b == c",
        "let x = a == b ?? c",
        "let x = a..b ?? c",
        "let x = a & b & c + d",
        "let x = a + b & c",
        "let x = a > > b",
        "let x = a >> b >> c",
        "let x = a > b",
        "var x = 1\nx >>= 2",
        "let x = a >= b",
        "let x = !a == -b * c",
        "let x = a < b == c != d",
        "let x = a || b && c || d",
        "let x = (a < b) < c",
        "let x = a < (b < c)",
        "if a < b < c {\n}",
        "while a == b..c {\n}",
        "let x = - - a ?? b",
        "let x = a & b ?? c..d < e && f",
    ];
    for text in cases {
        if !m0_reference_input(text) {
            continue;
        }
        for kind in [ModuleKind::Script, ModuleKind::Host] {
            assert_eq!(
                parse_with(text, kind, true),
                parse_with(text, kind, false),
                "{text:?}"
            );
        }
    }
}

#[test]
fn long_coalesce_chains_do_not_recurse() {
    // The right-associative tree is built without recursive descent.
    let text = "let x = a".to_owned() + &" ?? a".repeat(100_000);
    let errors = errors_only(&text);
    assert!(errors.is_empty(), "{errors:?}");
    let (tree, errors) = parse_text("let x = a ?? b ?? c", ModuleKind::Script);
    assert!(errors.is_empty());
    assert_eq!(tree, "(let x (Coalesce a (Coalesce b c)))");
}

/// E2's frozen parser predates T013. Preserve it for the original syntax;
/// scalar extensions are covered by semantic fixtures and reference models.
fn m0_reference_input(text: &str) -> bool {
    let mut interner = Interner::new();
    let mut diagnostics = Diagnostics::new(100);
    let mut scratch = LexScratch::new();
    let tokens = lex(
        text,
        SourceId::from_u32(0),
        &mut interner,
        &mut diagnostics,
        &mut scratch,
    );
    !tokens.kinds().iter().any(|kind| {
        matches!(
            kind,
            crate::TokenKind::Float
                | crate::TokenKind::Str
                | crate::TokenKind::Char
                | crate::TokenKind::Tilde
                | crate::TokenKind::Amp
                | crate::TokenKind::Pipe
                | crate::TokenKind::Caret
                | crate::TokenKind::Shl
                | crate::TokenKind::AmpEq
                | crate::TokenKind::PipeEq
                | crate::TokenKind::CaretEq
                | crate::TokenKind::ShlEq
                | crate::TokenKind::QuestionQuestion
        )
    }) && !text.contains(">>")
        && !text.contains("truncating:")
}

#[test]
fn scalar_operators_follow_the_language_precedence() {
    let (tree, errors) = parse_text("let x = a | b ^ c & d << e + f", ModuleKind::Script);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        tree,
        "(let x (BitOr a (BitXor b (BitAnd c (Shl d (Add e f))))))"
    );
    for text in [
        "let c = 'a'",
        "let f = -1.5",
        "let x = 8 >> 1",
        "var x = 8\nx >>= 1",
        "let x = u8(truncating: 257)",
    ] {
        assert!(errors_only(text).is_empty(), "{text}");
    }
}

#[test]
fn budgeted_parse_rejects_small_quota_and_can_retry() {
    let text = "fn f(n: i64) -> i64 { return n + 1; }\nf(41)";
    let mut interner = Interner::new();
    let mut diagnostics = Diagnostics::new(100);
    let mut lex_scratch = LexScratch::new();
    let tokens = lex(
        text,
        SourceId::from_u32(0),
        &mut interner,
        &mut diagnostics,
        &mut lex_scratch,
    );
    let mut scratch = ParseScratch::new();
    let mut small = eitri_base::MemoryBudget::new(1, 0).unwrap();
    assert!(
        parse_budgeted(
            text,
            SourceId::from_u32(0),
            tokens,
            ModuleKind::Script,
            &mut diagnostics,
            &mut scratch,
            &mut small
        )
        .is_err()
    );
    let mut large = eitri_base::MemoryBudget::new(0, scratch.heap_bytes()).unwrap();
    let ast = parse_budgeted(
        text,
        SourceId::from_u32(0),
        tokens,
        ModuleKind::Script,
        &mut diagnostics,
        &mut scratch,
        &mut large,
    )
    .unwrap();
    assert_eq!(ast.fns.len(), 1);
    assert!(!diagnostics.has_errors());
}
