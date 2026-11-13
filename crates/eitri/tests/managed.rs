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
