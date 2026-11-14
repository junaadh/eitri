//! Host reentry and tier-up. A `ScriptFn` is valid only during the host
//! call that looked it up (a host keeping one does not compile; see the
//! `ScriptFn` doctest in `eitri-vm`), so a handle can never outlive its
//! program or reach another program's function.

use eitri::{Call, Context, Host, HostError};

const DECLARATIONS: &str = "pub fn reenter(value: i64) -> i64\npub fn emit(value: i64) -> void\n";

/// Looks `callback` up on every call, as the lifetime of `ScriptFn`
/// requires.
#[derive(Default)]
struct Reentering {
    emitted: Vec<i64>,
}

impl Host for Reentering {
    fn call(&mut self, cx: &mut Call<'_, '_>, index: u32) -> Result<(), HostError> {
        if index == 1 {
            self.emitted.push(cx.arg(0) as i64);
            return Ok(());
        }
        let callback = cx.function("callback").ok_or("no callback")?;
        let mut result = [0];
        cx.call(self, callback, &[cx.arg(0)], &mut result)?;
        cx.set_result(0, result[0]);
        Ok(())
    }
}

fn context() -> Context {
    let mut context = Context::new();
    context.register_host_module("h", DECLARATIONS).unwrap();
    context
}

/// Found by an audit: a handle kept from one program was accepted by the
/// next program's `callback` at the same position. Handles no longer
/// outlive their call; each program's own `callback` runs.
#[test]
fn each_program_reenters_its_own_callback() {
    let mut context = context();
    let mut host = Reentering::default();
    for (body, expected) in [("n + 1", 2), ("99", 99), ("n * 10", 10)] {
        let source = format!(
            "import h from \"h\"\nfn callback(n: i64) -> i64 {{\n    return {body}\n}}\nh.emit(h.reenter(1))\n"
        );
        let compiled = context.compile(&source).unwrap();
        context.execute(&compiled, &mut host).unwrap();
        drop(compiled);
        assert_eq!(host.emitted.last(), Some(&expected));
    }
}

/// Reentry works the same after tier-up replaces the code.
#[test]
fn reentry_works_after_tier_up() {
    let mut context = context();
    let mut host = Reentering::default();
    let source = "import h from \"h\"\nfn callback(n: i64) -> i64 {\n    var i = 0\n    while i < 1000 {\n        i += 1\n    }\n    return n + i\n}\nh.emit(h.reenter(1))\n";
    let mut compiled = context.compile(source).unwrap();
    context.execute(&compiled, &mut host).unwrap();
    context.optimize(&mut compiled, source).unwrap();
    context.execute(&compiled, &mut host).unwrap();
    assert_eq!(host.emitted, [1001, 1001]);
}

/// Found by an audit: `return 1` and `return true` compile to the same
/// words, constants, and sites, and optimizing one with the other's source
/// was accepted, then failed verification. The signature differs.
#[test]
fn optimizing_with_another_source_is_rejected() {
    let mut context = Context::new();
    let mut compiled = context
        .compile("fn f() -> i64 {\n    return 1\n}\n")
        .unwrap();
    let other = "fn f() -> bool {\n    return true\n}\n";
    assert!(matches!(
        context.optimize(&mut compiled, other),
        Err(eitri::OptimizeError::SourceMismatch)
    ));
    assert!(!compiled.is_optimized());
}

#[test]
fn aggregate_signature_cannot_be_called_as_flat_scalars() {
    struct Probe;
    impl Host for Probe {
        fn call(&mut self, cx: &mut Call<'_, '_>, _: u32) -> Result<(), HostError> {
            let callback = cx.function("callback").unwrap();
            let mut result = [0];
            assert!(cx.call(self, callback, &[7], &mut result).is_err());
            assert!(
                cx.call_values(self, callback, &[eitri::ReentryArg::Scalar(7)])
                    .is_err()
            );
            Ok(())
        }
    }
    let source = "import h from \"h\"\nstruct Boxed { value: i64 }\nfn callback(p: Boxed) -> i64 { return p.value }\nh.probe()\n";
    for threaded in [false, true] {
        let mut cx = Context::new();
        cx.set_threaded(threaded);
        cx.register_host_module("h", "pub fn probe() -> void\n")
            .unwrap();
        let mut compiled = cx.compile(source).unwrap();
        cx.execute(&compiled, &mut Probe).unwrap();
        cx.optimize(&mut compiled, source).unwrap();
        cx.execute(&compiled, &mut Probe).unwrap();
    }
}

#[test]
fn optional_signature_cannot_be_called_as_scalar() {
    struct Probe;
    impl Host for Probe {
        fn call(&mut self, cx: &mut Call<'_, '_>, _: u32) -> Result<(), HostError> {
            let callback = cx.function("callback").unwrap();
            let mut result = [0];
            assert!(cx.call(self, callback, &[0], &mut result).is_err());
            assert!(
                cx.call_values(self, callback, &[eitri::ReentryArg::Scalar(0)])
                    .is_err()
            );
            Ok(())
        }
    }
    for ty in ["?string", "?i64"] {
        let source = format!(
            "import h from \"h\"\nfn callback(value: {ty}) -> i64 {{ return 1 }}\nh.probe()\n"
        );
        for threaded in [false, true] {
            let mut cx = Context::new();
            cx.set_threaded(threaded);
            cx.register_host_module("h", "pub fn probe() -> void\n")
                .unwrap();
            let compiled = cx.compile(&source).unwrap();
            cx.execute(&compiled, &mut Probe).unwrap();
        }
    }
}

#[test]
fn optional_host_signatures_are_rejected() {
    let mut context = Context::new();
    assert!(
        context
            .register_host_module("h", "pub fn f(value: ?string) -> void\n")
            .is_err()
    );
    assert!(
        context
            .register_host_module("h", "pub fn f() -> ?string\n")
            .is_err()
    );
    assert!(
        context
            .register_host_module("h", "pub fn f(value: ?i64) -> void\n")
            .is_err()
    );
    assert!(
        context
            .register_host_module("h", "pub fn f() -> ?i64\n")
            .is_err()
    );
}
