use eitri_base::{Interner, SourceId, Span, kw, sym};
use eitri_diag::Diagnostics;

use super::*;
use crate::test_support::{FRAGMENTS, Rng, corpus, mutate};
use crate::token::TokenKind::{self, *};

struct Lexed {
    tokens: Tokens,
    interner: Interner,
    diagnostics: Diagnostics,
}

fn lex(text: &str) -> Lexed {
    let mut lexed = Lexed {
        tokens: Tokens::new(),
        interner: Interner::new(),
        diagnostics: Diagnostics::new(100),
    };
    let mut scratch = LexScratch::new();
    super::lex(
        text,
        SourceId::from_u32(0),
        &mut lexed.interner,
        &mut lexed.diagnostics,
        &mut scratch,
    );
    lexed.tokens = scratch.into_tokens();
    lexed
}

/// Kinds, without the final `Eof`; asserts there were no diagnostics.
fn kinds(text: &str) -> Vec<TokenKind> {
    let lexed = lex(text);
    assert!(
        lexed.diagnostics.is_empty(),
        "{text:?}: {:?}",
        errors(&lexed)
    );
    let kinds = lexed.tokens.kinds();
    assert_eq!(kinds.last(), Some(&Eof));
    kinds[..kinds.len() - 1].to_vec()
}

/// Each token's spelling, `\n` for newlines.
fn spellings(text: &str) -> Vec<&str> {
    let lexed = lex(text);
    assert!(lexed.diagnostics.is_empty(), "{:?}", errors(&lexed));
    let tokens = &lexed.tokens;
    (0..tokens.len() - 1)
        .map(|i| &text[tokens.span(TokenId::from_u32(i as u32)).range()])
        .collect()
}

/// `code start..end message` for each diagnostic.
fn errors(lexed: &Lexed) -> Vec<String> {
    lexed
        .diagnostics
        .iter()
        .map(|d| {
            let span = d.primary().unwrap().span;
            format!(
                "{} {}..{} {}",
                d.code.unwrap(),
                span.start,
                span.end,
                d.message
            )
        })
        .collect()
}

fn error_list(text: &str) -> Vec<String> {
    errors(&lex(text))
}

#[test]
fn keywords_identifiers_and_symbols() {
    let lexed = lex("let x = x1 + _ + _y\nwhile i64 Self from");
    let tokens = &lexed.tokens;
    let kinds = tokens.kinds();
    assert_eq!(
        kinds,
        [
            Let, Ident, Eq, Ident, Plus, Wildcard, Plus, Ident, Newline, While, Ident, Ident,
            Ident, Eof
        ]
    );
    let symbol = |i: u32| tokens.symbol(TokenId::from_u32(i));
    assert_eq!(symbol(0), kw::LET);
    assert_eq!(lexed.interner.resolve(symbol(1)), "x");
    assert_eq!(
        symbol(10),
        sym::I64,
        "type names are identifiers with predefined symbols"
    );
    assert_eq!(symbol(11), sym::SELF_UPPER);
    assert_eq!(symbol(12), sym::FROM, "contextual words are identifiers");
}

#[test]
fn shadowed_spellings_share_one_symbol() {
    let lexed = lex("let x = 1\nlet x = x + 1");
    let tokens = &lexed.tokens;
    let xs: Vec<_> = (0..tokens.len() as u32)
        .map(TokenId::from_u32)
        .filter(|&t| tokens.kind(t) == Ident)
        .map(|t| tokens.symbol(t))
        .collect();
    assert_eq!(xs.len(), 3);
    assert!(xs.iter().all(|&s| s == xs[0]));
}

#[test]
fn operators_use_longest_match() {
    assert_eq!(
        kinds("?. ?[ ] ?? ? => -> ..= .. . == != <<= << <= < >= > && || ! ~"),
        [
            QuestionDot,
            QuestionBracket,
            RBracket,
            QuestionQuestion,
            Question,
            FatArrow,
            Arrow,
            DotDotEq,
            DotDot,
            Dot,
            EqEq,
            NotEq,
            ShlEq,
            Shl,
            LtEq,
            Lt,
            GtEq,
            Gt,
            AndAnd,
            OrOr,
            Bang,
            Tilde
        ]
    );
    assert_eq!(
        kinds("&+= &+ &-= &- &*= &* &= & |= | ^= ^ += + -= - *= * /= / %= % ="),
        [
            WrapAddEq, WrapAdd, WrapSubEq, WrapSub, WrapMulEq, WrapMul, AmpEq, Amp, PipeEq, Pipe,
            CaretEq, Caret, PlusEq, Plus, MinusEq, Minus, StarEq, Star, SlashEq, Slash, PercentEq,
            Percent, Eq
        ]
    );
}

#[test]
fn greater_than_is_always_single() {
    assert_eq!(kinds(">>"), [Gt, Gt]);
    assert_eq!(kinds(">>="), [Gt, GtEq]);
    assert_eq!(
        kinds("[]<[]<E!T>>"),
        [
            LBracket, RBracket, Lt, LBracket, RBracket, Lt, Ident, Bang, Ident, Gt, Gt
        ]
    );
}

#[test]
fn try_question_only_when_adjacent() {
    assert_eq!(kinds("try? f()"), [TryQuestion, Ident, LParen, RParen]);
    assert_eq!(kinds("try ?"), [Try, Question]);
    assert_eq!(spellings("x = try?f"), ["x", "=", "try?", "f"]);
}

#[test]
fn ranges_are_not_floats() {
    assert_eq!(kinds("0..n"), [Int, DotDot, Ident]);
    assert_eq!(kinds("0..=10"), [Int, DotDotEq, Int]);
    assert_eq!(kinds("1.x"), [Int, Dot, Ident]);
}

#[test]
fn number_literals() {
    assert_eq!(
        spellings("42 1_000_000 0xFF_FF 0o755 0b1010_0001 1__0 3.14 1.0e-9 6e23 2E+5 1_0.5_0"),
        [
            "42",
            "1_000_000",
            "0xFF_FF",
            "0o755",
            "0b1010_0001",
            "1__0",
            "3.14",
            "1.0e-9",
            "6e23",
            "2E+5",
            "1_0.5_0"
        ]
    );
    assert_eq!(kinds("42 0x1 3.14 6e23"), [Int, Int, Float, Float]);
    // The value is not converted here: the expected type decides it.
    assert_eq!(kinds("99999999999999999999999"), [Int]);
}

#[test]
fn malformed_numbers_span_the_whole_run() {
    assert_eq!(
        error_list("0b102"),
        ["E0101 0..5 malformed integer literal"]
    );
    assert_eq!(error_list("0x"), ["E0101 0..2 malformed integer literal"]);
    assert_eq!(error_list("0x_1"), ["E0101 0..4 malformed integer literal"]);
    assert_eq!(
        error_list("0o8 1"),
        ["E0101 0..3 malformed integer literal"]
    );
    assert_eq!(
        error_list("12abc"),
        ["E0101 0..5 malformed integer literal"]
    );
    assert_eq!(error_list("1e"), ["E0101 0..2 malformed integer literal"]);
    assert_eq!(error_list("1.5e+"), ["E0101 0..4 malformed float literal"]);
    // The literal token is still produced, so the parser sees no gap.
    assert_eq!(lex("0b102 + 1").tokens.kinds(), [Int, Plus, Int, Eof]);
}

#[test]
fn newline_rules() {
    // Statement ends.
    assert_eq!(kinds("a\nb"), [Ident, Newline, Ident]);
    // Consecutive breaks, comments, and blank lines collapse.
    assert_eq!(kinds("a\n\n  // c\n\n b"), [Ident, Newline, Ident]);
    // No newline at the start, nor after a continuation token.
    assert_eq!(kinds("\n\na +\n b"), [Ident, Plus, Ident]);
    assert_eq!(kinds("let x =\n 1"), [Let, Ident, Eq, Int]);
    assert_eq!(
        kinds("f(a,\n b)"),
        [Ident, LParen, Ident, Comma, Ident, RParen]
    );
    // Inside parentheses and brackets, breaks are ignored.
    assert_eq!(kinds("(a\n+ b)"), [LParen, Ident, Plus, Ident, RParen]);
    assert_eq!(kinds("[a\nb]"), [LBracket, Ident, Ident, RBracket]);
    // Inside braces, even within parentheses, they count again.
    assert_eq!(
        kinds("({\na\n})"),
        [LParen, LBrace, Newline, Ident, Newline, RBrace, RParen]
    );
    // A leading `.` does not join the previous line.
    assert_eq!(kinds("a\n.b"), [Ident, Newline, Dot, Ident]);
    // `<` and `>` are not continuation tokens; `>=` is.
    assert_eq!(kinds("a <\nb"), [Ident, Lt, Newline, Ident]);
    assert_eq!(kinds("a >=\nb"), [Ident, GtEq, Ident]);
    // `else` on the next line is a new statement.
    assert_eq!(
        kinds("{\n}\nelse"),
        [LBrace, Newline, RBrace, Newline, Else]
    );
    // Keyword continuations.
    for word in ["try", "try?", "await", "spawn"] {
        assert!(!kinds(&format!("{word}\nf")).contains(&Newline), "{word}");
    }
    // A line break inside a block comment separates statements.
    assert_eq!(kinds("a /* x\ny */ b"), [Ident, Newline, Ident]);
    assert_eq!(kinds("a /* x */ b"), [Ident, Ident]);
    assert_eq!(kinds("a\r\nb"), [Ident, Newline, Ident]);
}

#[test]
fn newline_tokens_span_the_break() {
    let lexed = lex("a\nb");
    assert_eq!(lexed.tokens.span(TokenId::from_u32(1)), Span::new(1, 2));
    assert_eq!(
        lexed.tokens.span(TokenId::from_u32(3)),
        Span::empty(3),
        "eof"
    );
}

#[test]
fn delimiters_are_linked_both_ways() {
    let lexed = lex("f(a[1], { b }) ?[0]");
    let tokens = &lexed.tokens;
    let pair = |open: u32| {
        let open = TokenId::from_u32(open);
        let close = tokens.matching(open).unwrap();
        assert_eq!(tokens.matching(close), Some(open));
        (tokens.kind(open), tokens.kind(close), close.as_u32())
    };
    assert_eq!(pair(1), (LParen, RParen, 10));
    assert_eq!(pair(3), (LBracket, RBracket, 5));
    assert_eq!(pair(7), (LBrace, RBrace, 9));
    assert_eq!(pair(11), (QuestionBracket, RBracket, 13));
    assert_eq!(
        tokens.matching(TokenId::from_u32(0)),
        None,
        "not a delimiter"
    );
}

#[test]
fn unmatched_delimiters() {
    assert_eq!(error_list("f(a"), ["E0102 1..2 unclosed delimiter `(`"]);
    assert_eq!(
        error_list("a)"),
        ["E0102 1..2 unexpected closing delimiter `)`"]
    );
    // A closer that matches a deeper opener closes it; the ones between are unclosed.
    let lexed = lex("( { )");
    assert_eq!(errors(&lexed), ["E0102 2..3 unclosed delimiter `{`"]);
    assert_eq!(
        lexed.tokens.matching(TokenId::from_u32(0)),
        Some(TokenId::from_u32(2))
    );
    assert_eq!(lexed.tokens.matching(TokenId::from_u32(1)), None);
    // Several unclosed openers are reported in source order.
    assert_eq!(
        error_list("{ ( ["),
        [
            "E0102 0..1 unclosed delimiter `{`",
            "E0102 2..3 unclosed delimiter `(`",
            "E0102 4..5 unclosed delimiter `[`",
        ]
    );
}

#[test]
fn nesting_limit_is_reported_once() {
    let depth = MAX_NESTING + 50;
    let text = "(".repeat(depth) + &")".repeat(depth);
    assert_eq!(
        error_list(&text),
        [format!(
            "E0002 {MAX_NESTING}..{} implementation limit: delimiters nested deeper than {MAX_NESTING}",
            MAX_NESTING + 1
        )]
    );
    assert!(
        error_list(&"(".repeat(MAX_NESTING))
            .iter()
            .all(|e| !e.starts_with("E0002"))
    );
}

#[test]
fn strings_are_single_tokens() {
    assert_eq!(
        spellings(r#"a "hi" "tab\t\"q\"" "x ${f("}") + {y}} z" r"\d+" "" b"#),
        [
            "a",
            r#""hi""#,
            r#""tab\t\"q\"""#,
            r#""x ${f("}") + {y}} z""#,
            r#"r"\d+""#,
            r#""""#,
            "b"
        ]
    );
    assert_eq!(spellings("\"\\${not}\""), ["\"\\${not}\""]);
    assert_eq!(spellings("\"ދިވެހި\" + 1"), ["\"ދިވެހި\"", "+", "1"]);
    let multi = "let help = \"\"\"\n    Usage: ${name}\n    \"quoted\"\n    \"\"\"\nx";
    let literal = &multi[multi.find("\"\"\"").unwrap()..multi.rfind("\"\"\"").unwrap() + 3];
    assert_eq!(spellings(multi), ["let", "help", "=", literal, "\n", "x"]);
    let raw = "r\"\"\"\n  no ${interp} \\here\n  \"\"\"";
    assert_eq!(spellings(raw), [raw]);
    // A nested string in an interpolation may itself interpolate.
    assert_eq!(spellings(r#""a${"b${c}d"}e""#).len(), 1);
    assert_eq!(spellings("\"a ${'}'} b\""), ["\"a ${'}'} b\""]);
}

#[test]
fn malformed_strings() {
    assert_eq!(
        error_list("\"open\nx"),
        ["E0101 0..5 unterminated string literal"]
    );
    assert_eq!(
        error_list("\"open"),
        ["E0101 0..5 unterminated string literal"]
    );
    // A line break inside the interpolation of a single-line string ends it;
    // the next line then lexes on its own.
    assert_eq!(
        error_list("\"a ${b\n}\""),
        [
            "E0101 0..6 unterminated string literal",
            "E0102 7..8 unexpected closing delimiter `}`",
            "E0101 8..9 unterminated string literal",
        ]
    );
    assert_eq!(
        error_list("\"a\\\nb\""),
        [
            "E0101 0..3 unterminated string literal",
            "E0101 5..6 unterminated string literal"
        ]
    );
    assert_eq!(
        error_list("r\"x\ny"),
        ["E0101 0..3 unterminated string literal"]
    );
    assert_eq!(
        error_list("\"\"\"text\n\"\"\""),
        ["E0101 0..3 a multi-line string starts on the line after `\"\"\"`"]
    );
    assert_eq!(
        error_list("\"\"\"\nnever closed"),
        ["E0101 0..16 unterminated string literal"]
    );
}

#[test]
fn char_literals() {
    assert_eq!(
        spellings(r"'a' '\n' '\'' '\u{2603}' 'ދ' '💥'"),
        ["'a'", r"'\n'", r"'\''", r"'\u{2603}'", "'ދ'", "'💥'"]
    );
    assert_eq!(error_list("'ab' x"), ["E0101 0..4 malformed char literal"]);
    assert_eq!(error_list("'' x"), ["E0101 0..2 malformed char literal"]);
    assert_eq!(
        error_list("'a\nx"),
        ["E0101 0..2 unterminated char literal"]
    );
    assert_eq!(lex("'ab' x").tokens.kinds(), [Char, Ident, Eof]);
}

#[test]
fn attributes() {
    let lexed = lex("@extern\nstruct");
    assert_eq!(lexed.tokens.kinds(), [Attribute, Newline, Struct, Eof]);
    assert_eq!(
        lexed
            .interner
            .resolve(lexed.tokens.symbol(TokenId::from_u32(0))),
        "extern"
    );
    assert_eq!(error_list("@ x"), ["E0101 0..1 unexpected character `@`"]);
}

#[test]
fn unexpected_characters_and_comments() {
    assert_eq!(
        error_list("💥 fn {"),
        [
            "E0101 0..4 unexpected character `💥`",
            "E0102 8..9 unclosed delimiter `{`"
        ]
    );
    assert_eq!(error_list("a $ b"), ["E0101 2..3 unexpected character `$`"]);
    assert_eq!(
        error_list("a\u{7}b"),
        ["E0101 1..2 unexpected character `\\u{7}`"]
    );
    assert_eq!(error_list("naïve"), ["E0101 2..4 unexpected character `ï`"]);
    assert_eq!(
        error_list("let x = 1 /* never closed\n"),
        ["E0101 10..26 unterminated block comment"]
    );
    // Block comments do not nest.
    assert_eq!(kinds("/* a /* b */ c"), [Ident]);
    assert_eq!(kinds("// only a comment"), []);
    assert_eq!(kinds(""), []);
}

#[test]
fn adversarial_inputs_stay_linear() {
    // Long runs exercise each scanner without quadratic rescans.
    let n = 200_000;
    let idents = "a ".repeat(n);
    assert_eq!(lex(&idents).tokens.len(), n + 1);
    let string = format!("\"{}\"", "x".repeat(n));
    assert_eq!(lex(&string).tokens.len(), 2);
    let interpolations = format!("\"{}{}\"", "${\"".repeat(1000), "\"}".repeat(1000));
    let lexed = lex(&interpolations);
    assert!(lexed.diagnostics.is_empty(), "{:?}", errors(&lexed));
    assert_eq!(lexed.tokens.len(), 2);
    let comment = format!("/*{}", "*".repeat(n));
    assert_eq!(error_list(&comment).len(), 1);
    let newlines = "\n".repeat(n);
    assert_eq!(kinds(&newlines), []);
}

#[test]
fn scratch_is_reused_across_compilations() {
    let mut interner = Interner::new();
    let mut scratch = LexScratch::new();
    let source = SourceId::from_u32(0);
    let text = "let alpha = beta + gamma(1, [2])\nwhile alpha { \"s ${beta}\" }";
    let mut diagnostics = Diagnostics::new(100);

    let first = super::lex(text, source, &mut interner, &mut diagnostics, &mut scratch)
        .kinds()
        .to_vec();
    let alpha = scratch.tokens().symbol(TokenId::from_u32(1));
    let retained = (scratch.heap_bytes(), interner.heap_bytes());

    // A different, shorter compilation in between must leave nothing behind.
    interner.reset();
    super::lex("x", source, &mut interner, &mut diagnostics, &mut scratch);
    assert_eq!(scratch.tokens().kinds(), [Ident, Eof]);

    for _ in 0..3 {
        interner.reset();
        let tokens = super::lex(text, source, &mut interner, &mut diagnostics, &mut scratch);
        assert_eq!(tokens.kinds(), first);
        // Fresh numbering after a reset: the same spelling, the same symbol.
        assert_eq!(tokens.symbol(TokenId::from_u32(1)), alpha);
        assert_eq!(
            (scratch.heap_bytes(), interner.heap_bytes()),
            retained,
            "warm runs must not grow"
        );
    }
    assert!(diagnostics.is_empty());
}

/// Everything observable about one lexing: kinds, spans, identifier
/// spellings, delimiter partners, and diagnostics.
fn observe(text: &str, reference: bool) -> String {
    let mut interner = Interner::new();
    let mut diagnostics = Diagnostics::new(usize::MAX);
    let mut scratch = LexScratch::new();
    let mut reference_scratch = super::reference::LexScratch::new();
    let source = SourceId::from_u32(0);
    let tokens = if reference {
        super::reference::lex(
            text,
            source,
            &mut interner,
            &mut diagnostics,
            &mut reference_scratch,
        )
    } else {
        super::lex(text, source, &mut interner, &mut diagnostics, &mut scratch)
    };
    let mut out = String::new();
    for i in 0..tokens.len() as u32 {
        let t = TokenId::from_u32(i);
        let kind = tokens.kind(t);
        let span = tokens.span(t);
        out += &format!("{kind:?} {}..{}", span.start, span.end);
        if matches!(kind, Ident | Attribute) || kind.is_keyword() {
            out += &format!(" {:?}", interner.resolve(tokens.symbol(t)));
        }
        if let Some(partner) = tokens.matching(t) {
            out += &format!(" ~{}", partner.as_u32());
        }
        out.push('\n');
    }
    for d in diagnostics.iter() {
        let s = d.primary().unwrap().span;
        out += &format!("{} {}..{} {}\n", d.code.unwrap(), s.start, s.end, d.message);
    }
    out
}

fn assert_same(text: &str, case: &str) {
    let expected = observe(text, true);
    let actual = observe(text, false);
    assert!(
        expected == actual,
        "{case}: optimized lexer differs from the reference on {text:?}\n--- reference\n{expected}--- optimized\n{actual}"
    );
}

#[test]
fn differential_against_the_reference_lexer() {
    let corpus = corpus();
    for (i, text) in corpus.iter().enumerate() {
        assert_same(text, &format!("corpus {i}"));
    }
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for case in 0..20_000 {
        let text = if case % 2 == 0 {
            {
                let pick = rng.below(corpus.len());
                mutate(&mut rng, &corpus[pick])
            }
        } else {
            let mut text = String::new();
            for _ in 0..rng.below(24) {
                text += FRAGMENTS[rng.below(FRAGMENTS.len())];
            }
            text
        };
        assert_same(&text, &format!("case {case}"));
    }
}

#[test]
fn one_byte_tokens_ignore_what_follows() {
    for (text, kind) in [
        ("((", LParen),
        ("))", RParen),
        ("[[", LBracket),
        ("]]", RBracket),
        ("{{", LBrace),
        ("}}", RBrace),
        (",,", Comma),
        (";;", Semi),
        ("::", Colon),
        ("~~", Tilde),
    ] {
        assert_eq!(lex(text).tokens.kinds()[..2], [kind, kind], "{text:?}");
    }
}

#[test]
fn budgeted_lexing_rejects_growth_without_executing_and_remains_reusable() {
    let source = "fn f(n: i64) -> i64 { return n + 1; }\nf(41)";
    let mut interner = Interner::new();
    let mut scratch = LexScratch::new();
    let baseline = interner.heap_bytes() + scratch.heap_bytes();
    let mut diagnostics = Diagnostics::new(100);
    let mut small = eitri_base::MemoryBudget::new(baseline + 1, baseline).unwrap();
    assert!(
        super::lex_budgeted(
            source,
            SourceId::from_u32(0),
            &mut interner,
            &mut diagnostics,
            &mut scratch,
            &mut small
        )
        .is_err()
    );
    let baseline = interner.heap_bytes() + scratch.heap_bytes();
    let mut sufficient = eitri_base::MemoryBudget::new(0, baseline).unwrap();
    let tokens = super::lex_budgeted(
        source,
        SourceId::from_u32(0),
        &mut interner,
        &mut diagnostics,
        &mut scratch,
        &mut sufficient,
    )
    .unwrap();
    assert_eq!(tokens.last_kind(), Some(TokenKind::Eof));
    assert!(!diagnostics.has_errors());
}
