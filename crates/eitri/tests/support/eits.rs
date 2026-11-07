//! The `.eits` round trip, shared by the test targets: printing compiled
//! code and assembling the text back gives the same text, the same words,
//! and the same behavior on both interpreters.

use eitri::{Context, PrintOptions};

use super::{TEST_MODULE, TestHost};

const BENCH_MODULE: &str = "pub fn emit(value: i64) -> void\npub fn id(value: i64) -> i64\n";

pub fn context() -> Context {
    let mut context = Context::new();
    context.register_host_module("test", TEST_MODULE).unwrap();
    context.register_host_module("bench", BENCH_MODULE).unwrap();
    context.set_budget(100_000);
    context
}

/// The annotated listing without its source spans.
fn words(text: &str) -> String {
    text.lines()
        .map(|line| match line.find("  @") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The harness `test` module, plus `bench` for the benchmark workloads.
struct Hosts {
    test: TestHost,
    emit: u32,
    id: u32,
}

impl eitri::Host for Hosts {
    fn call(&mut self, cx: &mut eitri::Call<'_, '_>, index: u32) -> Result<(), eitri::HostError> {
        if index == self.emit {
            self.test.effects += &format!("effect: bench.emit({})\n", cx.arg(0) as i64);
        } else if index == self.id {
            cx.set_result(0, cx.arg(0));
        } else {
            return eitri::Host::call(&mut self.test, cx, index);
        }
        Ok(())
    }
}

/// Effects and the outcome, without trace spans (they refer to different
/// texts).
fn behavior(context: &mut Context, compiled: &eitri::Compiled, threaded: bool) -> String {
    context.set_threaded(threaded);
    let mut host = Hosts {
        test: TestHost::new(|name| context.host_index("test", name)),
        emit: context.host_index("bench", "emit").unwrap(),
        id: context.host_index("bench", "id").unwrap(),
    };
    let outcome = match context.execute(compiled, &mut host) {
        Ok(()) => "ok".to_owned(),
        Err(fault) => format!("fault: {}", fault.message),
    };
    host.test.effects + &outcome
}

/// Check the round trip of `source`; `Ok(false)` if it does not compile.
pub fn round_trip(source: &str) -> Result<bool, String> {
    let mut context = context();
    let Ok(compiled) = context.compile(source) else {
        return Ok(false);
    };
    let plain = PrintOptions::default();
    let annotated = PrintOptions { annotate: true };
    let text = context.disassemble(&compiled, plain);
    let assembled = context
        .assemble(&text)
        .map_err(|d| format!("the printed text does not assemble: {d:?}\n{text}"))?;
    let again = context.disassemble(&assembled, plain);
    if again != text {
        return Err(format!(
            "the text changed\n--- printed\n{text}--- reprinted\n{again}"
        ));
    }
    let (before, after) = (
        words(&context.disassemble(&compiled, annotated)),
        words(&context.disassemble(&assembled, annotated)),
    );
    if before != after {
        return Err(format!(
            "the words changed\n--- compiled\n{before}\n--- assembled\n{after}"
        ));
    }
    for threaded in [false, true] {
        let (a, b) = (
            behavior(&mut context, &compiled, threaded),
            behavior(&mut context, &assembled, threaded),
        );
        if a != b {
            return Err(format!(
                "behavior changed\n--- compiled\n{a}\n--- assembled\n{b}"
            ));
        }
    }
    Ok(true)
}
