mod support;

use eitri::Context;
use support::{TEST_MODULE, TestHost};

const SOURCE: &str = include_str!("fixtures/managed/string_lengths.eit");

#[test]
fn joined_parts_survive_collection_and_keep_source_order() {
    let source = include_str!("fixtures/managed/string_chain.eit");
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
                "effect: test.decorate([a])\neffect: test.decorate([b])\neffect: test.decorate([c])\neffect: test.check(true)\neffect: test.emit(9)\n"
            );
            assert_eq!(context.managed_heap_bytes(), 0);
        }
    }
}

#[test]
fn slicing_checks_utf8_boundaries_and_keeps_the_host_running() {
    let invalid = [
        "\"é\".slice(1, 2)",
        "\"é\".slice(0, 1)",
        "\"é\".slice(-1, 2)",
        "\"é\".slice(0, 3)",
        "\"é\".slice(2, 0)",
    ];
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        let mut host = TestHost::new(|_| None);
        for source in invalid {
            let artifact = context.compile(source).unwrap();
            let error = context.execute(&artifact, &mut host).unwrap_err();
            assert_eq!(error.kind, Some(eitri::FaultKind::InvalidStringSlice));
        }
        let safe = context.compile("\"é\".slice(0, 2).len()").unwrap();
        context.execute(&safe, &mut host).unwrap();
    }
}

#[test]
fn slice_and_search_survive_collection_at_every_safepoint() {
    let source = include_str!("fixtures/managed/string_slice_search.eit");
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
                format!("{}ok\n", host.effects),
                include_str!("fixtures/managed/string_slice_search.expect")
            );
        }
    }
}

#[test]
fn string_transformations_reject_wrong_arguments() {
    let mut context = Context::new();
    for source in [
        "\"x\".trim(1)",
        "\"x\".to_upper(1)",
        "\"x\".replace(\"x\")",
        "\"x\".replace(1, 2)",
    ] {
        assert!(context.compile(source).is_err(), "{source}");
    }
}

#[test]
fn string_iteration_survives_collection_at_every_safepoint() {
    let source = include_str!("fixtures/managed/for_strings.eit");
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
                format!("{}ok\n", host.effects),
                include_str!("fixtures/managed/for_strings.expect")
            );
        }
    }
}

#[test]
fn trimming_survives_collection_at_every_safepoint() {
    let source = include_str!("fixtures/managed/string_trim.eit");
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
                format!("{}ok\n", host.effects),
                include_str!("fixtures/managed/string_trim.expect")
            );
        }
    }
}

#[test]
fn scalar_string_conversions_survive_forced_collection() {
    let source = include_str!("fixtures/managed/string_conversion.eit");
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
                format!("{}ok\n", host.effects),
                include_str!("fixtures/managed/string_conversion.expect")
            );
        }
    }
}

#[test]
fn string_conversion_rejects_optional_values_and_bad_arity() {
    let mut context = Context::new();
    for source in ["string()", "string(1, 2)", "string(\"abc\".find(\"a\"))"] {
        assert!(context.compile(source).is_err(), "{source} was accepted");
    }
}

#[test]
fn string_conversion_reports_allocation_failure_and_host_can_continue() {
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context.set_heap_limit(0);
        let compiled = context.compile("string(42).len()").unwrap();
        let mut host = TestHost::new(|_| None);
        assert_eq!(
            context.execute(&compiled, &mut host).unwrap_err().kind,
            Some(eitri::FaultKind::OutOfMemory)
        );
        context.set_heap_limit(1024);
        context.execute(&compiled, &mut host).unwrap();
    }
}

#[test]
fn interpolation_evaluates_all_values_before_allocating() {
    let source = "import test from \"test\"\nlet value = \"${test.id(1)} ${test.id(2)}\"";
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context.set_heap_limit(0);
        context.register_host_module("test", TEST_MODULE).unwrap();
        let artifact = context.compile(source).unwrap();
        let mut host = TestHost::new(|name| context.host_index("test", name));
        let fault = context.execute(&artifact, &mut host).unwrap_err();
        assert_eq!(fault.kind, Some(eitri::FaultKind::OutOfMemory));
        assert_eq!(host.effects, "effect: test.id(1)\neffect: test.id(2)\n");
    }
}

#[test]
fn interpolation_preserves_live_strings_at_safepoints() {
    let source = include_str!("fixtures/managed/string_interpolation.eit");
    for threaded in [false, true] {
        for simple in [false, true] {
            let mut context = Context::new();
            context.set_threaded(threaded);
            context.set_force_gc(true);
            context.register_host_module("test", TEST_MODULE).unwrap();
            let artifact = if simple {
                context.compile_simple(source)
            } else {
                context.compile(source)
            }
            .unwrap();
            let mut host = TestHost::new(|name| context.host_index("test", name));
            context.execute(&artifact, &mut host).unwrap();
            assert_eq!(
                format!("{}ok\n", host.effects),
                include_str!("fixtures/managed/string_interpolation.expect")
            );
        }
    }
}

#[test]
fn interpolation_works_with_wide_plan_and_frame_indices() {
    let mut source = String::from("import test from \"test\"\n");
    for index in 0..260 {
        source += &format!("let value{index} = \"{index}\"\n");
    }
    source += "test.check(\"${value259}:${1}\" == \"259:1\")\n";
    assert_eq!(support::eits::round_trip(&source), Ok(true));
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context.register_host_module("test", TEST_MODULE).unwrap();
        let artifact = context.compile(&source).unwrap();
        let mut host = TestHost::new(|name| context.host_index("test", name));
        context.execute(&artifact, &mut host).unwrap();
        assert_eq!(host.effects, "effect: test.check(true)\n");
    }
}

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

#[test]
fn retained_fixture_survives_forced_collection() {
    let source = include_str!("fixtures/managed/retained_strings.eit");
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
                "effect: test.retain_text()\neffect: test.retained_text()\neffect: test.release_text()\neffect: test.emit(8)\n"
            );
            assert_eq!(context.managed_heap_bytes(), 0);
        }
    }
}

#[test]
fn retained_strings_survive_runs_and_reject_stale_or_foreign_tokens() {
    #[derive(Default)]
    struct Retainer {
        value: Option<eitri::RetainedString>,
        stale: Option<eitri::RetainedString>,
        store: u32,
        load: u32,
        seen: Option<i64>,
        emit: u32,
    }
    impl eitri::Host for Retainer {
        fn call(&mut self, cx: &mut eitri::Call<'_, '_>, id: u32) -> Result<(), eitri::HostError> {
            if id == self.store {
                self.value = cx.retain_string_arg(0)?;
            } else if id == self.load {
                cx.set_result_retained_string(
                    0,
                    self.value.as_ref().ok_or("missing retained value")?,
                )?;
            } else if id == self.emit {
                self.seen = Some(cx.arg(0) as i64);
            }
            Ok(())
        }
    }
    let declarations = "pub fn store(value: string) -> void\npub fn load() -> string\npub fn emit(value: i64) -> void";
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context.register_host_module("host", declarations).unwrap();
        let mut host = Retainer {
            store: context.host_index("host", "store").unwrap(),
            load: context.host_index("host", "load").unwrap(),
            emit: context.host_index("host", "emit").unwrap(),
            ..Default::default()
        };
        let save = context
            .compile("import host from \"host\"\nhost.store(\"persist\")")
            .unwrap();
        let load = context
            .compile("import host from \"host\"\nhost.emit(host.load().len())")
            .unwrap();
        context.execute(&save, &mut host).unwrap();
        assert!(context.managed_heap_bytes() > 0);
        context.execute(&load, &mut host).unwrap();
        assert_eq!(host.seen, Some(7));
        let handle = host.value.take().unwrap();
        let mut other = Context::new();
        assert!(!other.release_retained_string(&handle));
        assert!(context.release_retained_string(&handle));
        assert!(!context.release_retained_string(&handle));
        host.stale = Some(handle);
        context.execute(&save, &mut host).unwrap();
        assert!(!context.release_retained_string(host.stale.as_ref().unwrap()));
        context.execute(&load, &mut host).unwrap();
        assert!(context.release_retained_string(host.value.as_ref().unwrap()));
        let empty = context.compile("").unwrap();
        context.execute(&empty, &mut host).unwrap();
        assert_eq!(context.managed_heap_bytes(), 0);
    }
}

#[test]
fn released_result_stays_rooted_during_host_reentry() {
    #[derive(Default)]
    struct Host {
        value: Option<eitri::RetainedString>,
        store: u32,
        load: u32,
        emit: u32,
        observed: i64,
    }
    impl eitri::Host for Host {
        fn call(&mut self, cx: &mut eitri::Call<'_, '_>, id: u32) -> Result<(), eitri::HostError> {
            if id == self.store {
                self.value = cx.retain_string_arg(0)?;
            } else if id == self.load {
                let value = self.value.take().ok_or("missing retained string")?;
                cx.set_result_retained_string(0, &value)?;
                assert!(cx.release_retained_string(&value));
                let callback = cx.function("callback").unwrap();
                cx.call(self, callback, &[], &mut [0])?;
            } else if id == self.emit {
                self.observed = cx.arg(0) as i64;
            }
            Ok(())
        }
    }
    let source = r#"
import host from "host"
fn callback() -> i64 { return "garbage".len(); }
host.store("retained")
let result = host.load()
host.emit(result.len())
"#;
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context.register_host_module("host", "pub fn store(value: string) -> void\npub fn load() -> string\npub fn emit(value: i64) -> void").unwrap();
        let mut host = Host {
            store: context.host_index("host", "store").unwrap(),
            load: context.host_index("host", "load").unwrap(),
            emit: context.host_index("host", "emit").unwrap(),
            ..Default::default()
        };
        let compiled = context.compile(source).unwrap();
        context.execute(&compiled, &mut host).unwrap();
        assert_eq!(host.observed, 8);
        assert_eq!(context.managed_heap_bytes(), 0);
    }
}

#[test]
fn string_reentry_fixture_survives_forced_collection() {
    let source = include_str!("fixtures/managed/reentry_strings.eit");
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
                "effect: test.reenter_text()\neffect: test.emit(3)\n"
            );
            assert_eq!(context.managed_heap_bytes(), 0);
        }
    }
}

#[test]
fn typed_reentry_roots_mixed_and_retained_arguments() {
    struct Callback;
    impl eitri::Host for Callback {
        fn call(&mut self, cx: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
            let retained = cx.retain_string_arg(0)?.ok_or("missing argument")?;
            let callback = cx.function("callback").ok_or("missing callback")?;
            let result = cx.call_values(
                self,
                callback,
                &[
                    eitri::ReentryArg::String("new"),
                    eitri::ReentryArg::RetainedString(&retained),
                    eitri::ReentryArg::Scalar(4),
                    eitri::ReentryArg::String("longer"),
                ],
            )?;
            assert_eq!(result, eitri::ReentryValue::String("persist".to_owned()));
            assert!(cx.release_retained_string(&retained));
            cx.set_result(0, 1);
            Ok(())
        }
    }
    let source = r#"
import host from "host"
fn callback(a: string, b: string, n: i64, c: string) -> string {
    "garbage".len()
    if a.len() + b.len() + n + c.len() != 20 { let x = 1 / 0; }
    return b
}
host.trigger("persist")
"#;
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context
            .register_host_module("host", "pub fn trigger(value: string) -> i64")
            .unwrap();
        let compiled = context.compile(source).unwrap();
        context.execute(&compiled, &mut Callback).unwrap();
        assert_eq!(context.managed_heap_bytes(), 0);
    }
}

#[test]
fn typed_reentry_reports_signature_budget_depth_and_allocation_failures() {
    struct Callback(eitri::FaultKind);
    impl eitri::Host for Callback {
        fn call(&mut self, cx: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
            let callback = cx.function("callback").unwrap();
            let wrong = cx
                .call_values(self, callback, &[eitri::ReentryArg::Scalar(1)])
                .unwrap_err();
            assert!(matches!(wrong.kind, eitri::FaultKind::Host(_)));
            let fault = cx
                .call_values(self, callback, &[eitri::ReentryArg::String("input")])
                .unwrap_err();
            assert_eq!(fault.kind, self.0);
            Ok(())
        }
    }
    let source =
        "import host from \"host\"\nfn callback(s: string) -> string { return s; }\nhost.trigger()";
    for threaded in [false, true] {
        for expected in [
            eitri::FaultKind::Budget,
            eitri::FaultKind::Depth,
            eitri::FaultKind::OutOfMemory,
        ] {
            let mut context = Context::new();
            context.set_threaded(threaded);
            context.set_force_gc(true);
            context
                .register_host_module("host", "pub fn trigger() -> void")
                .unwrap();
            match expected {
                eitri::FaultKind::Budget => context.set_budget(1),
                eitri::FaultKind::Depth => context.set_depth(0),
                eitri::FaultKind::OutOfMemory => context.set_heap_limit(0),
                _ => unreachable!(),
            }
            let compiled = context.compile(source).unwrap();
            context.execute(&compiled, &mut Callback(expected)).unwrap();
            assert_eq!(context.managed_heap_bytes(), 0);
        }
    }
}

#[test]
fn typed_reentry_returns_scalar_and_void_values() {
    struct Callback;
    impl eitri::Host for Callback {
        fn call(&mut self, cx: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
            let length = cx.function("length").unwrap();
            assert_eq!(
                cx.call_values(self, length, &[eitri::ReentryArg::String("☃")])?,
                eitri::ReentryValue::Scalar(3)
            );
            let discard = cx.function("discard").unwrap();
            assert_eq!(
                cx.call_values(self, discard, &[eitri::ReentryArg::Scalar(1)])?,
                eitri::ReentryValue::Void
            );
            Ok(())
        }
    }
    let source = "import host from \"host\"\nfn length(s: string) -> i64 { return s.len(); }\nfn discard(n: i64) -> void { let x = n; }\nhost.trigger()";
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context
            .register_host_module("host", "pub fn trigger() -> void")
            .unwrap();
        let compiled = context.compile(source).unwrap();
        context.execute(&compiled, &mut Callback).unwrap();
        assert_eq!(context.managed_heap_bytes(), 0);
    }
}

#[test]
fn growing_string_faults_at_heap_limit_and_host_can_continue() {
    let source = r#"
import test from "test"
var text = "a"
var i = 0
while i < 512 { text = text + "x"; i = i + 1; }
test.emit(text.len())
"#;
    for threaded in [false, true] {
        for simple in [false, true] {
            let mut context = Context::new();
            context.set_threaded(threaded);
            context.set_force_gc(true);
            context.set_heap_limit(300);
            context.register_host_module("test", TEST_MODULE).unwrap();
            let compiled = if simple {
                context.compile_simple(source)
            } else {
                context.compile(source)
            }
            .unwrap();
            let mut host = TestHost::new(|name| context.host_index("test", name));
            let fault = context.execute(&compiled, &mut host).unwrap_err();
            assert_eq!(fault.message, "managed memory limit exceeded");
            assert_eq!(fault.kind, Some(eitri::FaultKind::OutOfMemory));
            assert_eq!(host.effects, "");
            assert_eq!(context.managed_heap_bytes(), 0);
            let safe = context
                .compile("import test from \"test\"\ntest.emit(7)")
                .unwrap();
            context.execute(&safe, &mut host).unwrap();
            assert_eq!(host.effects, "effect: test.emit(7)\n");
        }
    }
}
#[test]
fn is_empty_rejects_non_strings_and_arguments() {
    let mut context = eitri::Context::new();
    for source in ["1.is_empty()", "\"\".is_empty(1)"] {
        assert!(context.compile(source).is_err(), "{source} was accepted");
    }
}
