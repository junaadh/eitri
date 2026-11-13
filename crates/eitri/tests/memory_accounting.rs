//! Retained-size reporting. These figures do not yet implement a memory quota.

use eitri::Context;

#[test]
fn artifact_size_tracks_recorded_bodies_and_outlives_compiler_context() {
    let source = "fn value(n: i64) -> i64 { return n + 1; }\nvalue(41)";
    let mut context = Context::new();
    let plain = context.compile(source).unwrap();
    context.set_keep_ir(true);
    let recorded = context.compile(source).unwrap();
    assert!(plain.retained_bytes() > plain.code_bytes());
    assert!(recorded.retained_bytes() > plain.retained_bytes());
    let before = recorded.retained_bytes();
    drop(context);
    assert_eq!(recorded.retained_bytes(), before);
}

#[test]
fn compiler_vector_measurement_tracks_warm_storage_per_context() {
    let mut first = Context::new();
    let second = Context::new();
    let untouched = second.compiler_vector_bytes();
    let source =
        "fn sum(n: i64) -> i64 { var x = 0; while x < n { x = x + 1; }; return x; }\nsum(4)";
    first.compile(source).unwrap();
    let warm = first.compiler_vector_bytes();
    assert!(warm > untouched);
    for _ in 0..3 {
        first.compile(source).unwrap();
    }
    assert_eq!(first.compiler_vector_bytes(), warm);
    assert_eq!(second.compiler_vector_bytes(), untouched);
}

#[test]
fn owned_artifact_size_is_stable_across_compatible_contexts() {
    struct Host;
    impl eitri::Host for Host {
        fn call(&mut self, _: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
            Ok(())
        }
    }
    let mut origin = Context::new();
    let compiled = origin
        .compile("fn value() -> i64 { return 7; }\nvalue()")
        .unwrap();
    let retained = compiled.retained_bytes();
    drop(origin);
    for threaded in [false, true] {
        let mut destination = Context::new();
        destination.set_threaded(threaded);
        destination.execute(&compiled, &mut Host).unwrap();
        assert_eq!(compiled.retained_bytes(), retained);
    }
}

#[test]
fn runtime_quota_stops_string_growth_and_context_can_continue() {
    struct Host {
        effects: Vec<i64>,
    }
    impl eitri::Host for Host {
        fn call(&mut self, cx: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
            self.effects.push(cx.arg(0) as i64);
            Ok(())
        }
    }
    let source = "import host from \"host\"\nvar text = \"a\"\nvar i = 0\nwhile i < 512 { text = text + \"x\"; i = i + 1; }\nhost.emit(text.len())";
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_force_gc(true);
        context
            .register_host_module("host", "pub fn emit(value: i64) -> void")
            .unwrap();
        let compiled = context.compile(source).unwrap();
        let safe = context
            .compile("import host from \"host\"\nhost.emit(7)")
            .unwrap();
        let limit = context.retained_runtime_bytes() + compiled.retained_bytes() + 800;
        context.set_memory_limit(limit);
        let mut host = Host {
            effects: Vec::new(),
        };
        let fault = context.execute(&compiled, &mut host).unwrap_err();
        assert_eq!(fault.kind, Some(eitri::FaultKind::OutOfMemory));
        assert!(host.effects.is_empty());
        // The smaller artifact can still execute under the same quota.
        context.execute(&safe, &mut host).unwrap();
        assert_eq!(host.effects, [7]);
    }
}

#[test]
fn limited_compilation_rejects_low_quota_and_can_retry() {
    let mut context = Context::new();
    context.set_memory_limit(1);
    let error = match context.compile("let value = 1") {
        Ok(_) => panic!("small quota accepted"),
        Err(error) => error,
    };
    assert_eq!(error[0].code, Some(eitri::diag::Code::OutOfMemory));
    context.set_memory_limit(16 * 1024 * 1024);
    let artifact = context.compile("let value = 1").unwrap();
    struct Host;
    impl eitri::Host for Host {
        fn call(&mut self, _: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
            Ok(())
        }
    }
    context.execute(&artifact, &mut Host).unwrap();
}

#[test]
fn inactive_artifact_is_not_charged_to_an_executing_context() {
    struct Host;
    impl eitri::Host for Host {
        fn call(&mut self, _: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
            Ok(())
        }
    }
    let small_source = "let x = 1";
    let large_source = format!("let x = \"{}\"", "x".repeat(4096));
    for threaded in [false, true] {
        let mut origin = Context::new();
        let small = origin.compile(small_source).unwrap();
        let large = origin.compile(&large_source).unwrap();
        drop(origin);
        let mut destination = Context::new();
        destination.set_threaded(threaded);
        let limit = destination.retained_runtime_bytes() + small.retained_bytes() + 1024;
        assert!(large.retained_bytes() > small.retained_bytes() + 1024);
        destination.set_memory_limit(limit);
        destination.execute(&small, &mut Host).unwrap();
        let fault = destination.execute(&large, &mut Host).unwrap_err();
        assert_eq!(fault.kind, Some(eitri::FaultKind::OutOfMemory));
    }
}

#[test]
fn limited_compilation_handles_managed_roots_and_recorded_code() {
    for recording in [false, true] {
        let mut context = Context::new();
        context.set_keep_ir(recording);
        context.set_memory_limit(128 * 1024 * 1024);
        let source =
            "fn pick(s: string) -> string { return s; }\nlet text = pick(\"a\")\ntext.len()";
        let compiled = context.compile(source).unwrap();
        struct Host;
        impl eitri::Host for Host {
            fn call(
                &mut self,
                _: &mut eitri::Call<'_, '_>,
                _: u32,
            ) -> Result<(), eitri::HostError> {
                Ok(())
            }
        }
        context.set_force_gc(true);
        context.execute(&compiled, &mut Host).unwrap();
    }
}

#[test]
fn compilation_quota_boundaries_leave_context_reusable() {
    let source = "fn pick(s: string) -> string { return s; }\nvar s = pick(\"a\")\nvar i = 0\nwhile i < 4 { s = s + \"x\"; i = i + 1; }\ns.len()";
    for recording in [false, true] {
        for allowance in (0..20).map(|power| 1usize << power) {
            let mut context = Context::new();
            context.set_keep_ir(recording);
            context.set_memory_limit(context.retained_runtime_bytes() + allowance);
            if let Err(errors) = context.compile(source) {
                assert_eq!(errors[0].code, Some(eitri::diag::Code::OutOfMemory));
            }
            context.set_memory_limit(128 * 1024 * 1024);
            context.compile(source).unwrap();
        }
    }
}
