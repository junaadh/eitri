mod support;

use eitri::Context;
use support::{TEST_MODULE, TestHost};

const SOURCE: &str = include_str!("fixtures/managed/string_lengths.eit");

#[test]
fn strings_survive_every_safepoint_and_are_reclaimed_after_run() {
    for threaded in [false, true] {
        for simple in [false, true] {
            let mut context = Context::new();
            context.set_threaded(threaded);
            context.set_force_gc(true);
            context.register_host_module("test", TEST_MODULE).unwrap();
            let compiled = if simple {
                context.compile_simple(SOURCE)
            } else {
                context.compile(SOURCE)
            }
            .unwrap();
            let mut host = TestHost::new(|name| context.host_index("test", name));
            let baseline = context.managed_heap_bytes();
            for _ in 0..3 {
                context.execute(&compiled, &mut host).unwrap();
                assert_eq!(host.effects, "effect: test.emit(10)\n");
                assert_eq!(context.managed_heap_bytes(), baseline);
                host.effects.clear();
            }
        }
    }
}

#[test]
fn allocation_limit_faults_before_host_effects() {
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context.set_heap_limit(0);
        context.register_host_module("test", TEST_MODULE).unwrap();
        let compiled = context.compile(SOURCE).unwrap();
        let mut host = TestHost::new(|name| context.host_index("test", name));
        let fault = context.execute(&compiled, &mut host).unwrap_err();
        assert_eq!(fault.message, "managed memory limit exceeded");
        assert_eq!(host.effects, "");
        assert_eq!(context.managed_heap_bytes(), 0);
    }
}

#[test]
fn unreachable_strings_are_reclaimed_at_run_end() {
    let source =
        "fn f() -> i64 { \"discard\".len(); let s = \"☃\"; let t = s; return t.len(); }; f()";
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        let compiled = context.compile(source).unwrap();
        let baseline = context.managed_heap_bytes();
        let mut host = TestHost::new(|_| None);
        context.execute(&compiled, &mut host).unwrap();
        assert_eq!(context.managed_heap_bytes(), baseline);
    }
}

#[test]
fn forced_collection_also_visits_backward_branches_and_calls() {
    let source = r#"
import test from "test"
fn main() -> i64 { let s = "hello"; let t = "world"; return s.len() + t.len(); }
var total = 0
while total < 20 { total += main(); }
test.emit(total)
"#;
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context.register_host_module("test", TEST_MODULE).unwrap();
        let compiled = context.compile(source).unwrap();
        let mut host = TestHost::new(|name| context.host_index("test", name));
        context.execute(&compiled, &mut host).unwrap();
        assert_eq!(host.effects, "effect: test.emit(20)\n");
    }
}

#[test]
fn calls_and_loop_roots_survive_forced_collection() {
    let source = include_str!("fixtures/managed/string_control_calls.eit");
    for threaded in [false, true] {
        for simple in [false, true] {
            let mut context = Context::new();
            context.set_threaded(threaded);
            context.set_force_gc(true);
            context.register_host_module("test", TEST_MODULE).unwrap();
            let compiled = if simple {
                context.compile_simple(source)
            } else {
                context.compile(source)
            }
            .unwrap();
            let mut host = TestHost::new(|name| context.host_index("test", name));
            context.execute(&compiled, &mut host).unwrap();
            assert_eq!(host.effects, "effect: test.emit(6)\n");
        }
    }
}

#[test]
fn loop_reclaims_dead_strings_before_next_allocation() {
    let source = r#"
import test from "test"
fn count() -> i64 {
    let live = "persistent"
    var i = 0
    while i < 100 {
        let s = "iteration"
        i = i + s.len() - 8
    }
    return live.len() + i
}
test.emit(count())
"#;
    for threaded in [false, true] {
        for simple in [false, true] {
            let mut context = Context::new();
            context.set_threaded(threaded);
            context.set_force_gc(true);
            context.set_heap_limit(256);
            context.register_host_module("test", TEST_MODULE).unwrap();
            let compiled = if simple {
                context.compile_simple(source)
            } else {
                context.compile(source)
            }
            .unwrap();
            let mut host = TestHost::new(|name| context.host_index("test", name));
            context.execute(&compiled, &mut host).unwrap();
            assert_eq!(host.effects, "effect: test.emit(110)\n");
            assert_eq!(context.managed_heap_bytes(), 0);
        }
    }
}

#[test]
fn host_reentry_rejects_string_results_before_running_callback() {
    struct Reenter;
    impl eitri::Host for Reenter {
        fn call(&mut self, cx: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
            let callback = cx.function("callback").unwrap();
            let error = cx.call(self, callback, &[], &mut [0]).unwrap_err();
            assert!(
                error
                    .kind
                    .message()
                    .contains("does not match its signature")
            );
            Ok(())
        }
    }
    let source =
        "import test from \"test\"\nfn callback() -> string { return \"no\"; }\ntest.emit(1)";
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.register_host_module("test", TEST_MODULE).unwrap();
        let compiled = context.compile(source).unwrap();
        context.execute(&compiled, &mut Reenter).unwrap();
    }
}

#[test]
fn suspended_caller_keeps_live_string_through_callee_allocations() {
    let source = r#"
import test from "test"
fn choose(flag: bool, value: string) -> string {
    let unused = "callee garbage"
    if flag { return value; }; return "alternative"
}
fn main() -> i64 {
    let held = "caller alive"
    let value = choose(true, "argument")
    return held.len() + value.len()
}
test.emit(main())
"#;
    for threaded in [false, true] {
        for simple in [false, true] {
            let mut context = Context::new();
            context.set_threaded(threaded);
            context.set_force_gc(true);
            context.register_host_module("test", TEST_MODULE).unwrap();
            let compiled = if simple {
                context.compile_simple(source)
            } else {
                context.compile(source)
            }
            .unwrap();
            let mut host = TestHost::new(|name| context.host_index("test", name));
            context.execute(&compiled, &mut host).unwrap();
            assert_eq!(host.effects, "effect: test.emit(20)\n");
        }
    }
}

#[test]
fn host_string_fixture_survives_forced_collection() {
    let source = include_str!("fixtures/managed/host_strings.eit");
    for threaded in [false, true] {
        for simple in [false, true] {
            let mut context = Context::new();
            context.set_threaded(threaded);
            context.set_force_gc(true);
            context.register_host_module("test", TEST_MODULE).unwrap();
            let compiled = if simple {
                context.compile_simple(source)
            } else {
                context.compile(source)
            }
            .unwrap();
            let mut host = TestHost::new(|name| context.host_index("test", name));
            context.execute(&compiled, &mut host).unwrap();
            assert_eq!(
                host.effects,
                "effect: test.decorate([☃])\neffect: test.text_len(5)\neffect: test.emit(10)\n"
            );
            assert_eq!(context.managed_heap_bytes(), 0);
        }
    }
}

#[test]
fn host_string_argument_and_result_survive_reentry() {
    struct Reentrant;
    impl eitri::Host for Reentrant {
        fn call(&mut self, cx: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
            assert_eq!(cx.string_arg(0), Some("argument"));
            cx.set_result_string(0, "result")?;
            let callback = cx.function("callback").unwrap();
            cx.call(self, callback, &[], &mut [0])?;
            assert_eq!(cx.string_arg(0), Some("argument"));
            Ok(())
        }
    }
    let source = r#"
import test from "test"
fn callback() -> i64 { return "garbage".len(); }
let result = test.decorate("argument")
if result.len() != 6 { let failure = 1 / 0; }
"#;
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context.register_host_module("test", TEST_MODULE).unwrap();
        let compiled = context.compile(source).unwrap();
        context.execute(&compiled, &mut Reentrant).unwrap();
        assert_eq!(context.managed_heap_bytes(), 0);
    }
}

#[test]
fn host_string_result_requires_initialization_and_obeys_heap_limit() {
    struct ResultHost(bool);
    impl eitri::Host for ResultHost {
        fn call(&mut self, cx: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
            if self.0 {
                cx.set_result_string(0, "created by host")?;
            }
            Ok(())
        }
    }
    let source = "import host from \"host\"\nlet s = host.get()\ns.len()";
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context
            .register_host_module("host", "pub fn get() -> string")
            .unwrap();
        let compiled = context.compile(source).unwrap();
        let err = context
            .execute(&compiled, &mut ResultHost(false))
            .unwrap_err();
        assert!(err.message.contains("did not initialize"));
        context.set_heap_limit(0);
        let err = context
            .execute(&compiled, &mut ResultHost(true))
            .unwrap_err();
        assert_eq!(err.message, "managed memory limit exceeded");
        assert_eq!(context.managed_heap_bytes(), 0);
    }
}
