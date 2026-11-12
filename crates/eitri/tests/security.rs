//! T064: generated adversarial inputs complement the discovered fixtures.
//! No timing assertions: these check rejection, bounded execution and reuse.
mod support;

use eitri::{Call, Context, Host, HostError};
use eitri_diag::{Code, Severity};

#[derive(Default)]
struct Calls(usize);

impl Host for Calls {
    fn call(&mut self, _: &mut Call<'_, '_>, _: u32) -> Result<(), HostError> {
        self.0 += 1;
        Ok(())
    }
}

#[test]
fn hostile_source_rejects_before_effects_and_context_recovers() {
    let mut context = Context::new();
    context
        .register_host_module("h", "pub fn effect() -> void\n")
        .unwrap();
    let prefix = "import h from \"h\"\nh.effect()\n";
    let cases = [
        ("(".repeat(257) + "1" + &")".repeat(257), Code::Limit),
        ("!".repeat(10_000) + "true", Code::Limit),
        ("if true {\n".repeat(257) + &"}\n".repeat(257), Code::Limit),
        (
            "let x = ".to_owned() + &"9".repeat(131_072),
            Code::LiteralOutOfRange,
        ),
        (
            "let x = \"".to_owned() + &"x".repeat(131_072),
            Code::Lexical,
        ),
        ("spawn f()".to_owned(), Code::Unsupported),
        (
            "import x from \"/etc/passwd\"".to_owned(),
            Code::UnresolvedImport,
        ),
    ];
    let mut host = Calls::default();
    for (suffix, expected) in cases {
        let source = prefix.to_owned() + &suffix;
        let Err(eitri::RunError::Compile(errors)) = context.run(&source, &mut host) else {
            panic!("hostile source must be rejected");
        };
        assert!(
            errors.iter().any(|d| d.code == Some(expected)),
            "{errors:?}"
        );
        assert!(errors.len() <= 100);
        for error in errors {
            assert_ne!(error.severity, Severity::Bug);
            for label in error.labels {
                assert!(label.span.range().end <= source.len());
            }
        }
        assert_eq!(host.0, 0);
        context.run("let recovered = 1\n", &mut host).unwrap();
    }
}

#[test]
fn large_tokens_and_flat_expressions_return_to_host() {
    let name = "x".repeat(131_072);
    let cases = [
        format!("let {name} = 7\nlet result = {name}\n"),
        "/*".to_owned() + &"x".repeat(131_072) + "*/" + "\n",
        "let x = 1".to_owned() + &" + 1".repeat(10_000) + "\n",
    ];
    for source in cases {
        assert_eq!(
            support::outcome(&source, &support::Setup::default()).unwrap(),
            "ok\n"
        );
    }
}

#[test]
fn expression_nesting_limit_accepts_the_boundary() {
    let mut context = Context::new();
    // The leaf expression itself counts as one syntax level.
    let at = "(".repeat(255) + "1" + &")".repeat(255);
    context.run(&at, &mut Calls::default()).unwrap();
    let over = "(".repeat(256) + "1" + &")".repeat(256);
    let errors = context
        .compile(&over)
        .err()
        .expect("over the nesting limit");
    assert!(errors.iter().any(|d| d.code == Some(Code::Limit)));
}

#[test]
fn assembly_cycles_exhaust_budget_in_every_tier() {
    for (init, branch) in [
        ("", "j label %loop"),
        ("%r0 = ldi bool true\n", "tb.true %r0, label %loop"),
        ("%r0 = ldi i64 0\n", "cbi.eq %r0, 0, label %loop"),
        ("%r0 = ldi i64 0\n", "cb.eq %r0, %r0, label %loop"),
    ] {
        let frame = usize::from(!init.is_empty());
        let text =
            format!("define void @<module>() frame({frame}) {{\n{init}loop:\n{branch}\nret\n}}\n");
        let mut context = Context::new();
        context.set_budget(3);
        let mut compiled = context.assemble(&text).unwrap();
        for optimized in [false, true] {
            if optimized {
                context.optimize_kept(&mut compiled).unwrap();
            }
            for threaded in [false, true] {
                context.set_threaded(threaded);
                let fault = context
                    .execute(&compiled, &mut Calls::default())
                    .unwrap_err();
                assert_eq!(fault.message, "execution budget exhausted");
                assert_eq!(fault.trace.len(), 1);
            }
        }
    }
}

#[test]
fn assembly_cannot_create_host_capabilities_or_skip_verification() {
    let mut context = Context::new();
    for text in [
        "declare void @os.exit()\ndefine void @<module>() frame(0) { ret }",
        "define void @<module>() frame(4294967295) { ret }",
        "define void @<module>() frame(1) { %r0 = mov %r0\nret }",
        "define void @<module>() frame(0) { j label %missing }",
        "define void @<module>() frame(0) { %r65536 = ldi i64 0\nret }",
    ] {
        assert!(context.assemble(text).is_err(), "{text}");
        let valid = context
            .assemble("define void @<module>() frame(0) { ret }")
            .unwrap();
        context.execute(&valid, &mut Calls::default()).unwrap();
    }
}

/// A host may catch a nested fault, but cannot replenish the active budget.
struct Catching {
    attempts: usize,
}

impl Host for Catching {
    fn call(&mut self, cx: &mut Call<'_, '_>, _: u32) -> Result<(), HostError> {
        let callback = cx.function("callback").unwrap();
        for _ in 0..2 {
            let fault = cx.call(self, callback, &[], &mut []).unwrap_err();
            assert_eq!(fault.kind, eitri_vm::FaultKind::Budget);
            self.attempts += 1;
        }
        Ok(())
    }
}

#[test]
fn caught_reentry_fault_does_not_reset_budget() {
    let source =
        "import h from \"h\"\nfn callback() -> void {\nwhile true {}\n}\nh.catch()\nh.catch()\n";
    for threaded in [false, true] {
        let mut context = Context::new();
        context.set_threaded(threaded);
        context.set_budget(3);
        context
            .register_host_module("h", "pub fn catch() -> void\n")
            .unwrap();
        let mut host = Catching { attempts: 0 };
        let Err(eitri::RunError::Fault(fault)) = context.run(source, &mut host) else {
            panic!("the next outer host call must exhaust the shared budget");
        };
        assert_eq!(host.attempts, 2);
        assert_eq!(fault.message, "execution budget exhausted");
    }
}

#[test]
fn foreign_artifact_is_rejected_before_host_dispatch() {
    let mut a = Context::new();
    a.register_host_module("a", "pub fn effect() -> void\n")
        .unwrap();
    let compiled = a.compile("import a from \"a\"\na.effect()\n").unwrap();
    let mut b = Context::new();
    b.register_host_module("b", "pub fn unrelated() -> void\n")
        .unwrap();
    assert_eq!(a.host_index("a", "effect"), b.host_index("b", "unrelated"));
    let mut host = Calls::default();
    let error = b.execute(&compiled, &mut host).unwrap_err();
    assert_eq!(error.code, Some(Code::HostBindingMismatch));
    assert_eq!(error.diagnostic().code, Some(Code::HostBindingMismatch));
    assert!(error.trace.is_empty());
    assert_eq!(host.0, 0);
    assert_eq!(compiled.runs(), 0);
    a.execute(&compiled, &mut host).unwrap();
    assert_eq!(host.0, 1);
}

#[test]
fn artifacts_require_same_used_bindings_not_same_context() {
    let source = "import a from \"a\"\na.effect()\na.effect()\n";
    let mut a = Context::new();
    a.set_keep_ir(true);
    a.register_host_module("a", "pub fn effect() -> void\n")
        .unwrap();
    a.register_host_module("unused", "pub fn other() -> void\n")
        .unwrap();
    let normal = a.compile(source).unwrap();
    let simple = a.compile_simple(source).unwrap();
    let text = a
        .disassemble(&normal, eitri::PrintOptions::default())
        .unwrap();
    let assembled = a.assemble(&text).unwrap();
    let mut b = Context::new();
    b.register_host_module("a", "pub fn effect() -> void\n")
        .unwrap();
    let mut wrong = Context::new();
    wrong
        .register_host_module("a", "pub fn effect(x: i64) -> void\n")
        .unwrap();
    let mut wrong_result = Context::new();
    wrong_result
        .register_host_module("a", "pub fn effect() -> i64\n")
        .unwrap();
    let mut wrong_name = Context::new();
    wrong_name
        .register_host_module("a", "pub fn other() -> void\n")
        .unwrap();
    let mut empty = Context::new();
    let mut host = Calls::default();
    // Metadata is owned: it remains valid after the compiling context dies.
    drop(a);
    for mut compiled in [normal, simple, assembled] {
        for optimized in [false, true] {
            if optimized {
                b.optimize_kept(&mut compiled).unwrap();
            }
            for threaded in [false, true] {
                b.set_threaded(threaded);
                let before = host.0;
                b.execute(&compiled, &mut host).unwrap();
                assert_eq!(host.0, before + 2);
                let runs = compiled.runs();
                for context in [&mut wrong, &mut wrong_result, &mut wrong_name, &mut empty] {
                    context.set_threaded(threaded);
                    let error = context.execute(&compiled, &mut host).unwrap_err();
                    assert_eq!(error.code, Some(Code::HostBindingMismatch));
                    assert_eq!(compiled.runs(), runs);
                    assert_eq!(host.0, before + 2);
                }
            }
        }
    }
    // Code without host calls imposes no registry requirement.
    let plain = b.compile("let x = 1\n").unwrap();
    empty.execute(&plain, &mut host).unwrap();
}

#[test]
fn canonical_descriptors_do_not_enable_compound_registration() {
    let mut context = Context::new();
    for declarations in [
        "pub fn f(x: ?i64) -> void\n",
        "pub fn f(x: [2]i64) -> void\n",
        "pub fn f() -> i64!i64\n",
        "type Alias = i64\npub fn f(x: Alias) -> void\n",
    ] {
        assert!(
            context
                .register_host_module("compound", declarations)
                .is_err()
        );
        assert!(!context.has_host_module("compound"));
    }
    context
        .register_host_module("compound", "pub fn f(x: i64) -> void\n")
        .unwrap();
}
