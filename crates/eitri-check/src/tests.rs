use eitri_base::{Interner, SourceId};
use eitri_diag::Diagnostics;
use eitri_syntax::{LexScratch, ModuleKind, ParseScratch, lex, parse};

use crate::{CheckScratch, Hosts, Input, Res, Ty, check, int_value};

const TEST_MODULE: &str = "pub fn emit(value: i64) -> void\npub fn check(value: bool) -> bool\n";

/// Check `text` with the `test` host module registered; diagnostics as
/// `code start..end message`. Panics on lexical or syntax errors.
fn errors(text: &str) -> Vec<String> {
    run(text, |_, _| {})
}

fn run(text: &str, inspect: impl FnOnce(&crate::Checked, &eitri_syntax::ast::Ast)) -> Vec<String> {
    let mut interner = Interner::new();
    let mut hosts = Hosts::new();
    hosts.register("test", TEST_MODULE, &mut interner).unwrap();
    interner.reset();
    let mut diagnostics = Diagnostics::new(1000);
    let mut lex_scratch = LexScratch::new();
    let mut parse_scratch = ParseScratch::new();
    let mut check_scratch = CheckScratch::new();
    let source = SourceId::from_u32(0);
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
        &mut parse_scratch,
    );
    assert!(diagnostics.is_empty(), "syntax errors in a checker test");
    let input = Input {
        text,
        source,
        tokens,
        ast,
        interner: &interner,
        hosts: &hosts,
    };
    let checked = check(&input, &mut diagnostics, &mut check_scratch);
    inspect(checked, ast);
    diagnostics
        .iter()
        .map(|d| {
            let s = d.primary().unwrap().span;
            format!("{} {}..{} {}", d.code.unwrap(), s.start, s.end, d.message)
        })
        .collect()
}

#[test]
fn integer_literal_boundaries() {
    assert_eq!(
        int_value("9_223_372_036_854_775_807", false),
        Some(i64::MAX)
    );
    assert_eq!(int_value("9223372036854775808", false), None);
    assert_eq!(int_value("9223372036854775808", true), Some(i64::MIN));
    assert_eq!(int_value("9223372036854775809", true), None);
    assert_eq!(int_value("0x7fff_ffff_ffff_ffff", false), Some(i64::MAX));
    assert_eq!(int_value("0x8000_0000_0000_0000", true), Some(i64::MIN));
    assert_eq!(int_value("0b1010_0001", false), Some(161));
    assert_eq!(int_value("0o755", false), Some(493));
    assert_eq!(int_value("0", true), Some(0));
    assert_eq!(
        int_value(&"9".repeat(400), false),
        None,
        "no overflow on long spellings"
    );
}

#[test]
fn long_chains_do_not_recurse() {
    // 100,000 left-associated operators: deeper than any Rust stack.
    let sum = "let x = 1".to_owned() + &" + 1".repeat(100_000);
    assert!(errors(&sum).is_empty());
    let compare = "let b = 1".to_owned() + &" + 1".repeat(50_000) + " == 2";
    assert!(errors(&compare).is_empty());
    let logic = "let b = true".to_owned() + &" && false".repeat(50_000);
    assert!(errors(&logic).is_empty());
    // A mistake deep in a long chain is still found, once.
    let bad = "let x = 1".to_owned() + &" + 1".repeat(1000) + " + true" + &" + 1".repeat(1000);
    assert_eq!(errors(&bad).len(), 1);
    // 100,000 `else if` links, as a statement and as a value.
    let links = |body: &str| format!(" else if x == 1 {{\n    {body}\n}}").repeat(100_000);
    let statement = format!("var x = 0\nif x == 0 {{\n    x = 1\n}}{}", links("x = 2"));
    assert!(errors(&statement).is_empty());
    let value = format!(
        "let x = 0\nlet y = if x == 0 {{\n    1\n}}{} else {{\n    3\n}}",
        links("2")
    );
    assert!(errors(&value).is_empty());
    // A mistake deep in a value chain is reported once, at the mistake.
    let bad = value.replacen("    2\n", "    true\n", 1);
    assert_eq!(errors(&bad).len(), 1);
}

#[test]
fn return_paths() {
    let ok = [
        "fn f(n: i64) -> i64 {\n    return n\n}",
        "fn f(n: i64) -> i64 {\n    if n > 0 {\n        return 1\n    } else {\n        return 2\n    }\n}",
        "fn f(n: i64) -> i64 {\n    if n > 0 {\n        return 1\n    } else if n < 0 {\n        return 2\n    } else {\n        return 3\n    }\n}",
        "fn f(n: i64) -> i64 {\n    while n > 0 {\n    }\n    return 0\n}",
        "fn f() -> void {\n}",
    ];
    for text in ok {
        assert!(errors(text).is_empty(), "{text}");
    }
    assert_eq!(
        errors(
            "fn f(n: i64) -> i64 {\n    if n > 0 {\n        return 1\n    } else if n < 0 {\n        return 2\n    }\n}"
        ),
        ["E0402 3..4 function `f` may finish without returning a value"]
    );
    assert_eq!(
        errors("fn f(n: i64) -> i64 {\n    while n > 0 {\n        return 1\n    }\n}"),
        ["E0402 3..4 function `f` may finish without returning a value"]
    );
    // A tail value is not a return in the first slice: say so, once.
    assert_eq!(
        errors("fn square(n: i64) -> i64 {\n    n * n\n}"),
        ["E0001 31..36 not supported yet: function results without `return`"]
    );
}

#[test]
fn host_calls_and_resolution() {
    let text = "import test from \"test\"\nfn twice(n: i64) -> i64 {\n    return n * 2\n}\nlet x = twice(2)\ntest.emit(x)\nlet ok = test.check(x == 4)";
    let errors = run(text, |checked, ast| {
        let calls: Vec<Res> = ast
            .exprs
            .iter_enumerated()
            .filter(|(_, e)| matches!(e, eitri_syntax::ast::Expr::Call { .. }))
            .map(|(id, _)| checked.resolutions[id])
            .collect();
        assert!(
            matches!(calls[..], [Res::Function(_), Res::Host(_), Res::Host(_)]),
            "{calls:?}"
        );
        assert_eq!(checked.bindings.len(), 3, "n, x, ok");
        let ok = checked.bindings.iter().last().unwrap();
        assert_eq!(ok.ty, Ty::Bool);
        assert_eq!(checked.imports.len(), 1);
    });
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        self::errors("import test from \"test\"\ntest.missing(1)"),
        ["E0201 29..36 unknown name `test.missing`"]
    );
    assert_eq!(
        self::errors("import test from \"test\"\ntest.emit(true)"),
        ["E0301 34..38 type mismatch: expected `i64`, found `bool`"]
    );
    assert_eq!(
        self::errors("import test from \"test\"\nlet t = test"),
        ["E0001 32..36 not supported yet: modules as values"]
    );
}

#[test]
fn shadowing_gets_new_bindings_with_new_types() {
    let text =
        "let x = 1\nlet x = x == 1\nfn f(x: i64) -> bool {\n    let x = x > 0\n    return x\n}";
    let errors = run(text, |checked, _| {
        let types: Vec<Ty> = checked.bindings.iter().map(|b| b.ty).collect();
        assert_eq!(types, [Ty::I64, Ty::Bool, Ty::I64, Ty::Bool]);
    });
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn host_registration_is_checked() {
    let mut interner = Interner::new();
    let mut hosts = Hosts::new();
    let error = |text: &str, hosts: &mut Hosts, interner: &mut Interner| -> Vec<String> {
        hosts
            .register("m", text, interner)
            .unwrap_err()
            .iter()
            .map(|d| d.message.clone())
            .collect()
    };
    assert_eq!(
        error("pub fn f(x: bytes) -> void", &mut hosts, &mut interner),
        ["not supported yet: type `bytes`"]
    );
    assert_eq!(
        error("pub fn f(x: void) -> void", &mut hosts, &mut interner),
        ["`void` can only be a return type"]
    );
    assert_eq!(
        error(
            "pub fn f() -> void\npub fn f() -> i64",
            &mut hosts,
            &mut interner
        ),
        ["duplicate function `f`"]
    );
    hosts
        .register("m", "pub fn f() -> void", &mut interner)
        .unwrap();
    assert_eq!(
        error("pub fn g() -> void", &mut hosts, &mut interner),
        ["host module `m` is already registered"]
    );
    // Registered names survive the per-run reset.
    interner.reset();
    assert!(interner.get("f").is_some());
}

#[test]
fn scratch_is_reused_across_runs() {
    // The same text twice through the same scratch gives the same results.
    for _ in 0..2 {
        assert_eq!(
            errors("let a = b\nlet c = a + 1"),
            ["E0201 8..9 unknown name `b`"]
        );
    }
}

/// Everything observable about checking one program, through the optimized
/// checker or the frozen reference: diagnostics, every expression's type and
/// resolution, every statement's binding, and every binding. `None` when the
/// program does not lex and parse cleanly.
fn observe(text: &str, reference: bool) -> Option<String> {
    let mut interner = Interner::new();
    let mut hosts = Hosts::new();
    hosts.register("test", TEST_MODULE, &mut interner).unwrap();
    interner.reset();
    let mut diagnostics = Diagnostics::new(usize::MAX);
    let mut lex_scratch = LexScratch::new();
    let mut parse_scratch = ParseScratch::new();
    let source = SourceId::from_u32(0);
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
        &mut parse_scratch,
    );
    if diagnostics.has_errors() {
        return None;
    }
    // E2's frozen checker is an M0 regression baseline, not a T013 oracle.
    if ast.exprs.iter().any(|e| {
        matches!(
            e,
            eitri_syntax::ast::Expr::Float { .. }
                | eitri_syntax::ast::Expr::Char(_)
                | eitri_syntax::ast::Expr::String(_)
                | eitri_syntax::ast::Expr::StringPart(_)
                | eitri_syntax::ast::Expr::Interpolation(_)
                | eitri_syntax::ast::Expr::Convert { .. }
                | eitri_syntax::ast::Expr::Binary {
                    op: eitri_syntax::ast::BinaryOp::BitAnd
                        | eitri_syntax::ast::BinaryOp::BitOr
                        | eitri_syntax::ast::BinaryOp::BitXor
                        | eitri_syntax::ast::BinaryOp::Shl
                        | eitri_syntax::ast::BinaryOp::Shr,
                    ..
                }
                | eitri_syntax::ast::Expr::Unary {
                    op: eitri_syntax::ast::UnaryOp::BitNot,
                    ..
                }
        )
    }) || ["i8", "u8", "u64", "f32", "f64", "char"]
        .iter()
        .any(|name| {
            text.split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|part| part == *name)
        })
    {
        return None;
    }
    let input = Input {
        text,
        source,
        tokens,
        ast,
        interner: &interner,
        hosts: &hosts,
    };
    let mut out = String::new();
    let mut scratch = CheckScratch::new();
    let mut reference_scratch = crate::check::reference::CheckScratch::new();
    let (types, resolutions, stmt_bindings, bindings, int_values) = if reference {
        let c = crate::check::reference::check(&input, &mut diagnostics, &mut reference_scratch);
        let ints: Vec<_> = ast
            .exprs
            .indices()
            .iter()
            .map(|e| c.int_values.get(&e).copied())
            .collect();
        (
            format!("{:?}", c.types),
            format!("{:?}", c.resolutions),
            format!("{:?}", c.stmt_bindings),
            format!("{:?}", c.bindings),
            format!("{ints:?}"),
        )
    } else {
        let c = check(&input, &mut diagnostics, &mut scratch);
        let ints: Vec<_> = ast.exprs.indices().iter().map(|e| c.int_value(e)).collect();
        // Literal values are compared below; where they are stored is a
        // representation choice, so `Res::Int` reads as the reference's `None`.
        let resolutions: Vec<Res> = c
            .resolutions
            .iter()
            .map(|&r| {
                if matches!(r, Res::Int(_)) {
                    Res::None
                } else {
                    r
                }
            })
            .collect();
        (
            format!("{:?}", c.types),
            format!(
                "{:?}",
                eitri_base::IndexVec::<eitri_syntax::ast::ExprId, Res>::from_raw(resolutions)
            ),
            format!("{:?}", c.stmt_bindings),
            format!("{:?}", c.bindings),
            format!("{ints:?}"),
        )
    };
    for line in [types, resolutions, stmt_bindings, bindings, int_values] {
        out += &line;
        out.push('\n');
    }
    for d in diagnostics.iter() {
        let s = d.primary().unwrap().span;
        out += &format!("{} {}..{} {}\n", d.code.unwrap(), s.start, s.end, d.message);
    }
    Some(out)
}

/// Programs that check: every fixture, the profiling workload, and seeded
/// token-level edits of them that still lex and parse.
fn checker_corpus() -> Vec<String> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../eitri/tests/fixtures");
    let mut files = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(root)];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "eit") {
                files.push(std::fs::read_to_string(path).unwrap());
            }
        }
    }
    files.sort();
    files
}

#[test]
fn differential_against_the_reference_checker() {
    // Replacements that keep programs parseable but change their meaning.
    const SWAPS: &[(&str, &[&str])] = &[
        ("i64", &["bool", "void", "u8", "i46"]),
        ("bool", &["i64", "void"]),
        ("true", &["1", "x", "false"]),
        (
            "1",
            &[
                "true",
                "0x7fff_ffff_ffff_ffff",
                "-9_223_372_036_854_775_808",
                "y",
            ],
        ),
        ("let", &["var"]),
        ("var", &["let"]),
        ("+", &["==", "&&", "<", "*"]),
        ("==", &["+", "!=", "<="]),
        ("return", &["test.emit"]),
        ("x", &["y", "count", "test"]),
        ("emit", &["check", "missing"]),
    ];
    let corpus = checker_corpus();
    let mut programs = corpus.clone();
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        seed.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    for _ in 0..7000 {
        let mut text = corpus[(next() % corpus.len() as u64) as usize].clone();
        for _ in 0..1 + next() % 3 {
            let (from, tos) = SWAPS[(next() % SWAPS.len() as u64) as usize];
            let to = tos[(next() % tos.len() as u64) as usize];
            let hits: Vec<usize> = text.match_indices(from).map(|(i, _)| i).collect();
            if let Some(&at) = hits.get((next() % hits.len().max(1) as u64) as usize) {
                text.replace_range(at..at + from.len(), to);
            }
        }
        programs.push(text);
    }
    let mut compared = 0;
    for (i, text) in programs.iter().enumerate() {
        let expected = observe(text, true);
        let actual = observe(text, false);
        assert!(
            expected == actual,
            "program {i}: optimized checker differs from the reference on {text:?}\n\
             --- reference\n{expected:?}\n--- optimized\n{actual:?}"
        );
        compared += usize::from(expected.is_some());
    }
    assert!(
        compared > 3000,
        "only {compared} programs reached the checker"
    );
}
