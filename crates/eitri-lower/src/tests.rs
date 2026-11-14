use eitri_base::{Interner, SourceId};
use eitri_bytecode::{Callees, disassemble, verify};
use eitri_check::{CheckScratch, Hosts, Input, check};
use eitri_diag::Diagnostics;
use eitri_syntax::{LexScratch, ModuleKind, ParseScratch, lex, parse};

use crate::{LowerScratch, Program, host_signatures, lower, lower_simple};

const TEST_MODULE: &str = "\
pub fn emit(value: i64) -> void
pub fn check(value: bool) -> bool
pub fn id(value: i64) -> i64
pub fn fail() -> void
pub fn reenter(value: i64) -> i64
";

/// Compile `text` with the `test` host module, lower it, and verify every
/// function; panics with the disassembly on any problem.
fn compile(text: &str) -> Program {
    compile_mode(text, false)
}

fn compile_mode(text: &str, simple: bool) -> Program {
    let mut interner = Interner::new();
    let mut hosts = Hosts::new();
    hosts.register("test", TEST_MODULE, &mut interner).unwrap();
    let mut diagnostics = Diagnostics::new(100);
    let mut lex_scratch = LexScratch::new();
    let mut parse_scratch = ParseScratch::new();
    let mut check_scratch = CheckScratch::new();
    let mut lower_scratch = LowerScratch::new();
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
    let input = Input {
        text,
        source,
        tokens,
        ast,
        interner: &interner,
        hosts: &hosts,
    };
    let checked = check(&input, &mut diagnostics, &mut check_scratch);
    assert!(!diagnostics.has_errors(), "{text}");
    let program = if simple {
        lower_simple(&input, checked, &mut lower_scratch, true)
    } else {
        lower(&input, checked, &mut lower_scratch, false)
    }
    .unwrap();
    let signatures = program.signatures();
    let hosts = host_signatures(&hosts);
    let callees = Callees {
        functions: &signatures,
        hosts: &hosts,
        record_shapes: &[],
        record_class_count: 0,
    };
    for f in program.functions.iter() {
        verify(f, &callees)
            .unwrap_or_else(|e| panic!("{}: {e:?}\n{}", f.name, disassemble(&f.code)));
    }
    program
}

fn listing(program: &Program, index: usize) -> String {
    disassemble(&program.functions.as_slice()[index].code)
}

#[test]
fn the_worked_lowering() {
    let program = compile("fn sum(a: i64, b: i64) -> i64 {\n    return a + b\n}");
    assert_eq!(
        listing(&program, 1),
        "0000  ADD.CHK.S    r0, r1, r2\n0001  RET\n"
    );
}

#[test]
fn conditions_branch_directly() {
    // `if a < b` is one compare-and-branch on the complement (b <= a).
    let program = compile(
        "fn f(a: i64, b: i64) -> i64 {\n    if a < b {\n        return 1\n    }\n    return 2\n}",
    );
    let listing = listing(&program, 1);
    assert!(listing.starts_with("0000  CB.LE"), "{listing}");
}

#[test]
fn every_executable_fixture_lowers_and_verifies() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../eitri/tests/fixtures");
    let mut count = 0;
    for group in ["ok", "fault"] {
        for entry in std::fs::read_dir(format!("{root}/{group}")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "eit") {
                let source = std::fs::read_to_string(&path).unwrap();
                compile(&source);
                compile_mode(&source, true);
                count += 1;
            }
        }
    }
    assert!(count >= 31);
}

#[test]
fn long_chains_lower_iteratively() {
    let compile = |text: &str| {
        compile_mode(text, false);
        compile_mode(text, true);
    };
    compile(&format!("let x = 1{}", " + 1".repeat(100_000)));
    compile(&format!("let b = true{}", " && false".repeat(50_000)));
    compile(&format!(
        "var c = 0\nif c == 0{} {{\n    c = 1\n}}",
        " || c == 1".repeat(50_000)
    ));
    let links = " else if c == 1 {\n    c = 2\n}".repeat(100_000);
    compile(&format!("var c = 0\nif c == 0 {{\n    c = 1\n}}{links}"));
}

#[test]
fn statements_after_a_return_are_not_emitted() {
    let program = compile("fn f() -> i64 {\n    return 1\n    let x = 2\n}");
    assert_eq!(
        listing(&program, 1),
        "0000  LDI          r0, #1\n0001  RET\n"
    );
}

/// The shapes `crates/eitri/tests/limits.rs` executes do reach the long
/// encodings, so those tests keep covering them.
#[test]
fn large_functions_use_the_long_forms() {
    let mut wide = "import test from \"test\"\n".to_owned();
    for i in 0..300 {
        wide += &format!("let v{i} = {i}\n");
    }
    wide += "test.emit(v299 * v298 - v257)\n";
    assert!(listing(&compile(&wide), 0).contains("WIDE "));

    // A comparison, a `bool` test past its 16-bit reach, and a loop exit.
    let body = |count: usize| "    x = x &+ 1000\n".repeat(count);
    let far = compile(&format!(
        "import test from \"test\"\nvar x = 0\nif x == 0 {{\n{}}}\nif test.check(true) {{\n{}}}\nwhile x < 3 {{\n{}}}\n",
        body(400),
        body(20_000),
        body(400),
    ));
    let far = listing(&far, 0);
    assert_eq!(far.matches("J_X").count(), 3);
    assert!(far.contains("TB.TRUE") && far.contains("CBI."));

    let mut constants = "var x = 0\n".to_owned();
    for i in 0..70_000 {
        constants += &format!("x = {}\n", 1_000_000 + i);
    }
    assert!(listing(&compile(&constants), 0).contains("LDK_X"));
}

/// The retained baseline must actually omit the local instruction-selection
/// shortcuts; otherwise a comparison would measure the same lowering twice.
#[test]
fn simple_lowering_materializes_operands_and_comparisons() {
    let text =
        "fn f(a: i64) -> i64 {\n    if a < 2 {\n        return a + 1\n    }\n    return a\n}";
    let default = listing(&compile(text), 1);
    let simple = compile_mode(text, true);
    let baseline = listing(&simple, 1);
    assert!(
        default.contains("CBI.") && default.contains("ADDI.CHK.S"),
        "{default}"
    );
    for instruction in ["MOV", "LDI", "CSET.LT", "TB.FALSE", "ADD.CHK.S"] {
        assert!(
            baseline.contains(instruction),
            "missing {instruction}:\n{baseline}"
        );
    }
    assert!(
        !baseline.contains("CBI.") && !baseline.contains("ADDI.CHK.S"),
        "{baseline}"
    );
    assert_eq!(
        simple.bodies.as_ref().unwrap().len(),
        simple.functions.len()
    );
}

#[test]
fn root_builder_diagnostics_and_verification_are_transactional() {
    use crate::{install_verified_roots, root_build_diagnostic};
    use eitri_base::Span;
    use eitri_bytecode::{RootBuildError, RootBuilder, RootLayout, SlotRange};
    use eitri_diag::Code;
    let source = SourceId::from_u32(0);
    let diagnostic = root_build_diagnostic(RootBuildError::Limit, source, Span::default());
    assert_eq!(diagnostic.code, Some(Code::Limit));
    let mut program = compile_mode("fn f(n: i64) -> i64 {\n return n\n}\n", false);
    let signatures = program.signatures();
    let callees = Callees {
        functions: &signatures,
        hosts: &[],
        record_shapes: &[],
        record_class_count: 0,
    };
    let function = program
        .functions
        .as_mut_slice()
        .iter_mut()
        .find(|f| !f.signature.params.is_empty())
        .unwrap();
    let original = function.roots.clone();
    let mut builder = RootBuilder::new(function.frame_size).unwrap();
    builder.at_poll(77).unwrap();
    let error = install_verified_roots(function, builder, &[], &callees, source, Span::default())
        .unwrap_err();
    assert_eq!(error.code, Some(Code::Limit));
    assert_eq!(function.roots, original);
    let mut builder = RootBuilder::new(function.frame_size).unwrap();
    builder
        .note_definition(
            SlotRange { first: 1, count: 1 },
            &RootLayout {
                ordinary: vec![0],
                dynamic: vec![],
            },
        )
        .unwrap();
    builder.at_entry().unwrap();
    let error = install_verified_roots(function, builder, &[], &callees, source, Span::default())
        .unwrap_err();
    assert_eq!(error.severity, eitri_diag::Severity::Bug);
    assert_eq!(function.roots, original);
    let builder = RootBuilder::new(function.frame_size).unwrap();
    install_verified_roots(function, builder, &[], &callees, source, Span::default()).unwrap();
}
