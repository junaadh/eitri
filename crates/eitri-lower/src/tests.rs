use eitri_base::{Interner, SourceId};
use eitri_bytecode::{Callees, disassemble, verify};
use eitri_check::{CheckScratch, Hosts, Input, check};
use eitri_diag::Diagnostics;
use eitri_syntax::{LexScratch, ModuleKind, ParseScratch, lex, parse};

use crate::{LowerScratch, Program, host_signatures, lower};

const TEST_MODULE: &str = "\
pub fn emit(value: i64) -> void
pub fn check(value: bool) -> bool
pub fn id(value: i64) -> i64
pub fn fail() -> void
";

/// Compile `text` with the `test` host module, lower it, and verify every
/// function; panics with the disassembly on any problem.
fn compile(text: &str) -> Program {
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
    let program = lower(&input, checked, &mut lower_scratch).unwrap();
    let signatures = program.signatures();
    let hosts = host_signatures(&hosts);
    let callees = Callees {
        functions: &signatures,
        hosts: &hosts,
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
                compile(&std::fs::read_to_string(&path).unwrap());
                count += 1;
            }
        }
    }
    assert!(count >= 31);
}

#[test]
fn long_chains_lower_iteratively() {
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
