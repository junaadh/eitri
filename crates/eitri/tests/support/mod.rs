//! Shared by the test targets: the harness `test` host module, outcome
//! rendering in the fixture expectation format, and the comparison of the
//! interpreters with the program-level oracle.

// Each test target uses a different part of this module.
#![allow(dead_code)]

pub mod eits;
pub mod fuzz;
pub mod oracle;

use std::thread;

/// The harness host module `test`, as documented in the root README.
pub const TEST_MODULE: &str = include_str!("../fixtures/test.eith");

/// The `test` module's behavior. Effects are recorded in call order.
pub struct TestHost {
    emit: u32,
    check: u32,
    id: u32,
    fail: u32,
    reenter: u32,
    pub effects: String,
}

/// What a `test` function does after recording its effect.
enum Action {
    Return(Option<u64>),
    Fail,
    /// Call the script's `callback` with this argument and return its
    /// result, propagating a fault.
    Reenter(u64),
}

const FAILED: &str = "requested by fixture";
const NO_CALLBACK: &str = "no `callback` function to reenter";

impl TestHost {
    /// `index` maps a `test` function name to its host index, if registered.
    pub fn new(index: impl Fn(&str) -> Option<u32>) -> Self {
        let index = |name| index(name).unwrap_or(u32::MAX);
        Self {
            emit: index("emit"),
            check: index("check"),
            id: index("id"),
            fail: index("fail"),
            reenter: index("reenter"),
            effects: String::new(),
        }
    }

    /// Record the call's effect and say what it does next. Shared by the
    /// interpreters' and the oracle's host interfaces.
    fn begin(&mut self, index: u32, args: &[u64]) -> Action {
        let name = if index == self.emit {
            "emit"
        } else if index == self.check {
            "check"
        } else if index == self.id {
            "id"
        } else if index == self.fail {
            return Action::Fail;
        } else if index == self.reenter {
            "reenter"
        } else {
            unreachable!("unknown host function {index}")
        };
        let shown = if name == "check" {
            (args[0] != 0).to_string()
        } else {
            (args[0] as i64).to_string()
        };
        self.effects += &format!("effect: test.{name}({shown})\n");
        match name {
            "emit" => Action::Return(None),
            "reenter" => Action::Reenter(args[0]),
            _ => Action::Return(Some(args[0])),
        }
    }
}

impl eitri::Host for TestHost {
    fn call(&mut self, cx: &mut eitri::Call<'_, '_>, index: u32) -> Result<(), eitri::HostError> {
        let value = match self.begin(index, cx.args()) {
            Action::Return(value) => value,
            Action::Fail => return Err(FAILED.into()),
            Action::Reenter(arg) => {
                let callback = cx.function("callback").ok_or(NO_CALLBACK)?;
                let mut result = [0];
                cx.call(self, callback, &[arg], &mut result)?;
                Some(result[0])
            }
        };
        if let Some(value) = value {
            cx.set_result(0, value);
        }
        Ok(())
    }
}

impl oracle::Host for TestHost {
    fn call(
        &mut self,
        cx: &mut dyn oracle::Reenter,
        index: u32,
        args: &[u64],
        results: &mut [u64],
    ) -> Result<(), oracle::HostError> {
        let value = match self.begin(index, args) {
            Action::Return(value) => value,
            Action::Fail => return Err(oracle::HostError::Message(FAILED.into())),
            Action::Reenter(arg) => {
                if !cx.has_function("callback") {
                    return Err(oracle::HostError::Message(NO_CALLBACK.into()));
                }
                let mut result = [0];
                cx.call(self, "callback", &[arg], &mut result)
                    .map_err(oracle::HostError::Fault)?;
                Some(result[0])
            }
        };
        if let Some(value) = value {
            results[0] = value;
        }
        Ok(())
    }
}

/// How a program runs: whether the `test` module is registered, and the
/// limits (the context's defaults when `None`).
#[derive(Default)]
pub struct Setup {
    pub test_host: bool,
    pub budget: Option<u64>,
    pub depth: Option<u32>,
}

/// The outcome in the expectation format. Both interpreters and the
/// program-level oracle run the program and must agree; later tiers join
/// here. `Err` describes a disagreement.
pub fn outcome(source: &str, setup: &Setup) -> Result<String, String> {
    let outcome = run_with(source, setup, false);
    let threaded = run_with(source, setup, true);
    if threaded != outcome {
        return Err(format!(
            "threaded interpreter disagrees\n--- match loop\n{outcome}--- threaded\n{threaded}"
        ));
    }
    // The hot tier (`Context::optimize`) must not change anything visible.
    for threaded in [false, true] {
        if let Some(optimized) = run_optimized(source, setup, threaded)
            && optimized != outcome
        {
            return Err(format!(
                "optimized code disagrees (threaded: {threaded})\n--- compiled\n{outcome}--- optimized\n{optimized}"
            ));
        }
    }
    // The oracle models the language, not implementation limits (`E0002`),
    // which lowering reports after the checker accepts.
    let limited = outcome.starts_with("error[E0002]");
    if !limited
        && let Some(expected) = run_oracle(source, setup)
        && expected != outcome
    {
        return Err(format!(
            "interpreter disagrees with the oracle\n--- oracle\n{expected}--- interpreter\n{outcome}"
        ));
    }
    Ok(outcome)
}

/// The oracle's outcome, or `None` for a program the frontend rejects.
pub fn run_oracle(source: &str, setup: &Setup) -> Option<String> {
    let defaults = eitri::Limits::default();
    let limits = oracle::Limits {
        budget: setup.budget.unwrap_or(defaults.budget),
        depth: setup.depth.unwrap_or(defaults.depth),
    };
    let run = || {
        let mut oracle = oracle::Oracle::new();
        if setup.test_host {
            oracle.register_host_module("test", TEST_MODULE);
        }
        let mut host = TestHost::new(|name| oracle.host_index("test", name));
        let outcome = oracle.run(source, limits, &mut host)?;
        Some(match outcome {
            Ok(()) => host.effects + "ok\n",
            Err(fault) => {
                let trace = fault
                    .trace
                    .into_iter()
                    .map(|(function, span)| eitri::TraceFrame { function, span });
                host.effects + &render_fault(&fault.message, trace)
            }
        })
    };
    thread::scope(|scope| {
        thread::Builder::new()
            .stack_size(oracle::STACK)
            .spawn_scoped(scope, run)
            .expect("spawn the oracle thread")
            .join()
            .expect("the oracle does not panic")
    })
}

fn render_fault(message: &str, trace: impl IntoIterator<Item = eitri::TraceFrame>) -> String {
    let mut out = format!("fault: {message}\n");
    for frame in trace {
        out += &match frame.span {
            Some(span) => format!("  at {} {}..{}\n", frame.function, span.start, span.end),
            None => format!("  at {}\n", frame.function),
        };
    }
    out
}

/// One interpreter's outcome, rejections included.
/// A context set up as `setup` says.
fn context(setup: &Setup, threaded: bool) -> eitri::Context {
    let mut context = eitri::Context::new();
    context.set_threaded(threaded);
    if setup.test_host {
        context
            .register_host_module("test", TEST_MODULE)
            .expect("the harness module declares cleanly");
    }
    if let Some(budget) = setup.budget {
        context.set_budget(budget);
    }
    if let Some(depth) = setup.depth {
        context.set_depth(depth);
    }
    context
}

/// The outcome of the code `Context::optimize` produces, or `None` if the
/// source does not compile.
pub fn run_optimized(source: &str, setup: &Setup, threaded: bool) -> Option<String> {
    let mut context = context(setup, threaded);
    let mut compiled = context.compile(source).ok()?;
    context
        .optimize(&mut compiled, source)
        .unwrap_or_else(|error| panic!("optimizing compiled source: {error:?}"));
    assert!(compiled.is_optimized());
    let mut host = TestHost::new(|name| context.host_index("test", name));
    Some(match context.execute(&compiled, &mut host) {
        Ok(()) => host.effects + "ok\n",
        Err(report) => host.effects + &render_fault(&report.message, report.trace),
    })
}

pub fn run_with(source: &str, setup: &Setup, threaded: bool) -> String {
    let mut context = eitri::Context::new();
    context.set_threaded(threaded);
    if setup.test_host {
        context
            .register_host_module("test", TEST_MODULE)
            .expect("the harness module declares cleanly");
    }
    if let Some(budget) = setup.budget {
        context.set_budget(budget);
    }
    if let Some(depth) = setup.depth {
        context.set_depth(depth);
    }
    let mut host = TestHost::new(|name| context.host_index("test", name));
    match context.run(source, &mut host) {
        Ok(()) => host.effects + "ok\n",
        Err(eitri::RunError::Fault(report)) => {
            host.effects + &render_fault(&report.message, report.trace)
        }
        Err(eitri::RunError::Compile(diagnostics)) => {
            assert!(host.effects.is_empty(), "a rejected program has no effects");
            assert!(!diagnostics.is_empty(), "a rejection reports a cause");
            let mut out = String::new();
            for diagnostic in &diagnostics {
                let Some(label) = diagnostic.primary() else {
                    // A `bug:` from the debug-build verifier has no label.
                    return format!("{}: {}\n", diagnostic.severity, diagnostic.message);
                };
                assert_eq!(label.source, eitri::Context::SOURCE);
                let span = label.span.range();
                assert!(source.is_char_boundary(span.start));
                assert!(source.is_char_boundary(span.end));
                assert!(span.start <= span.end);
                let code = diagnostic.code.expect("compile diagnostics have codes");
                out += &format!(
                    "error[{code}] {}..{}: {}\n",
                    span.start, span.end, diagnostic.message
                );
            }
            out
        }
    }
}
