//! Fuzz entry points (T012) for the compiler and verifier boundaries. Each
//! takes arbitrary input and panics on a broken invariant, so a coverage
//! fuzzer can call it directly; `tests/fuzz.rs` drives both with seeded
//! mutations of the fixture corpus.

use eitri_base::{IndexVec, Interner, SourceId};
use eitri_bytecode::{Callees, Function, FunctionId};
use eitri_check::{CheckScratch, Hosts, Input};
use eitri_diag::{Diagnostics, Severity};
use eitri_lower::{LowerScratch, Program};
use eitri_syntax::{LexScratch, ModuleKind, ParseScratch};
use eitri_vm::{Limits, VmScratch};

use super::{Setup, TEST_MODULE};

/// Limits small enough that any accepted input finishes quickly.
const LIMITS: Limits = Limits {
    budget: 10_000,
    depth: 64,
};

/// Source text: compiling never panics. A rejection reports at least one
/// error, never a `bug:` (an internal error), with every label a valid
/// range of the source. Accepted programs must give the same outcome on
/// both interpreters and the program-level oracle. Returns whether it
/// compiled.
pub fn source(data: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(data) else {
        return false;
    };
    let mut context = eitri::Context::new();
    context.set_budget(LIMITS.budget);
    context.set_depth(LIMITS.depth);
    context
        .register_host_module("test", TEST_MODULE)
        .expect("the harness module declares cleanly");
    match context.compile(text) {
        Ok(_) => {
            let setup = Setup {
                test_host: true,
                budget: Some(LIMITS.budget),
                depth: Some(LIMITS.depth),
            };
            if let Err(disagreement) = super::outcome(text, &setup) {
                panic!("{disagreement}");
            }
            true
        }
        Err(diagnostics) => {
            assert!(
                diagnostics.iter().any(|d| d.severity == Severity::Error),
                "a rejection reports an error"
            );
            for diagnostic in &diagnostics {
                assert_ne!(
                    diagnostic.severity,
                    Severity::Bug,
                    "internal error: {}",
                    diagnostic.message
                );
                for label in &diagnostic.labels {
                    let range = label.span.range();
                    assert!(
                        range.start <= range.end
                            && text.is_char_boundary(range.start)
                            && text.is_char_boundary(range.end),
                        "label {range:?} is not a source range"
                    );
                }
            }
            false
        }
    }
}

/// `.eits` text: assembling never panics; code that assembles (and so
/// verifies) runs on both interpreters with equal outcomes, and so does the
/// same code after the optimizer. Returns whether it assembled.
pub fn assembly(data: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(data) else {
        return false;
    };
    let mut context = super::eits::context();
    context.set_budget(LIMITS.budget);
    context.set_depth(LIMITS.depth);
    let Ok(mut compiled) = context.assemble(text) else {
        return false;
    };
    let run = |context: &mut eitri::Context, compiled: &eitri::Compiled| {
        [false, true].map(|threaded| {
            context.set_threaded(threaded);
            context.execute(compiled, &mut Echo)
        })
    };
    let before = run(&mut context, &compiled);
    assert_eq!(
        before[0], before[1],
        "the interpreters disagree on assembled code"
    );
    // The hot tier on arbitrary control flow: same outcome, traces included.
    context
        .optimize_kept(&mut compiled)
        .expect("assembled code keeps its logical instructions");
    let after = run(&mut context, &compiled);
    assert_eq!(before, after, "optimized code behaves differently\n{text}");
    true
}

/// The `.eits` text of `source`, if it compiles.
pub fn print(source: &str) -> Option<String> {
    let mut context = super::eits::context();
    let compiled = context.compile(source).ok()?;
    Some(super::eits::listing(
        &context,
        &compiled,
        eitri::PrintOptions::default(),
    ))
}

/// A compiled program and the host signatures it was verified against.
pub struct Compiled {
    pub program: Program,
    pub hosts: Vec<eitri_bytecode::Signature>,
}

/// Compile `text` with the `test` module, or `None` if it is rejected.
pub fn compile(text: &str) -> Option<Compiled> {
    let mut interner = Interner::new();
    let mut hosts = Hosts::new();
    hosts.register("test", TEST_MODULE, &mut interner).ok()?;
    let source = SourceId::from_u32(0);
    let mut diagnostics = Diagnostics::new(1);
    let (mut lex, mut parse) = (LexScratch::new(), ParseScratch::new());
    let (mut check, mut lower) = (CheckScratch::new(), LowerScratch::new());
    let tokens = eitri_syntax::lex(text, source, &mut interner, &mut diagnostics, &mut lex);
    let ast = eitri_syntax::parse(
        text,
        source,
        tokens,
        ModuleKind::Script,
        &mut diagnostics,
        &mut parse,
    );
    if diagnostics.has_errors() {
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
    let checked = eitri_check::check(&input, &mut diagnostics, &mut check);
    if diagnostics.has_errors() {
        return None;
    }
    let program = eitri_lower::lower(&input, checked, &mut lower, false).ok()?;
    Some(Compiled {
        program,
        hosts: eitri_lower::host_signatures(&hosts),
    })
}

/// Bytecode: `functions` with arbitrary code words. Verifying never
/// panics. If every function verifies, both interpreters run the program
/// without panicking and agree on its result or fault: verified code is
/// what the threaded interpreter's `unsafe` relies on. Returns whether it
/// verified and ran.
pub fn bytecode(
    functions: &IndexVec<FunctionId, Function>,
    hosts: &[eitri_bytecode::Signature],
) -> bool {
    let signatures: IndexVec<FunctionId, _> =
        IndexVec::from_raw(functions.iter().map(|f| f.signature.clone()).collect());
    let callees = Callees {
        functions: &signatures,
        hosts,
        record_shapes: &[],
        record_class_count: 0,
    };
    for function in functions.iter() {
        if eitri_bytecode::verify(function, &callees).is_err() {
            return false;
        }
    }
    let entry = FunctionId::from_u32(0);
    let mut scratch = VmScratch::new();
    scratch.set_host_signatures(hosts.to_vec());
    let portable = eitri_vm::run(functions, entry, &[], &mut Echo, LIMITS, &mut scratch);
    let threaded = eitri_vm::run_threaded(functions, entry, &[], &mut Echo, LIMITS, &mut scratch);
    assert_eq!(
        portable, threaded,
        "the interpreters disagree on verified code"
    );
    true
}

/// Accepts every host call and returns the low bit of its first argument:
/// a valid `i64` and a valid `bool` alike.
struct Echo;

impl eitri::Host for Echo {
    fn call(&mut self, cx: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
        let value = cx.args().first().map_or(0, |&arg| arg & 1);
        if let Some(result) = cx.results().first_mut() {
            *result = value;
        }
        Ok(())
    }
}
