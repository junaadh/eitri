//! T013 source/host-boundary coverage in addition to reusable fixtures.
mod support;

use eitri::{Call, Context, Host, HostError};

#[derive(Default)]
struct Echo {
    values: Vec<u64>,
}
impl Host for Echo {
    fn call(&mut self, cx: &mut Call<'_, '_>, index: u32) -> Result<(), HostError> {
        if index == 7 {
            assert_eq!(cx.arg(0), 1);
            return Ok(());
        }
        let word = cx.arg(0);
        self.values.push(word);
        if index == 6 {
            let callback = cx.function("callback").unwrap();
            let mut result = [0];
            cx.call(self, callback, &[word], &mut result)?;
            cx.set_result(0, result[0]);
        } else {
            cx.set_result(0, word);
        }
        Ok(())
    }
}
const DECLARATIONS: &str = "pub fn echo_i8(v: i8) -> i8\npub fn echo_u8(v: u8) -> u8\npub fn echo_u64(v: u64) -> u64\npub fn echo_f32(v: f32) -> f32\npub fn echo_f64(v: f64) -> f64\npub fn echo_char(v: char) -> char\npub fn reenter_i8(v: i8) -> i8\npub fn check(v: bool) -> void\n";
#[test]
fn canonical_scalars_cross_host_and_reentry_boundaries() {
    let source = "import m from \"m\"\nfn callback(n: i8) -> i8 { return n &+ 1 }\nm.check(m.echo_i8(-128) == -128)\nm.check(m.echo_u8(255) == 255)\nm.check(m.echo_u64(18446744073709551615) == 18446744073709551615)\nm.check(m.echo_f32(-0.0) == 0.0)\nm.check(m.echo_f64(-0.0) == 0.0)\nm.check(m.echo_char('💥') == '💥')\nm.check(m.reenter_i8(-128) == -127)\n";
    for simple in [false, true] {
        for threaded in [false, true] {
            let mut context = Context::new();
            context.set_threaded(threaded);
            context.register_host_module("m", DECLARATIONS).unwrap();
            let code = if simple {
                context.compile_simple(source)
            } else {
                context.compile(source)
            }
            .unwrap();
            let mut host = Echo::default();
            context.execute(&code, &mut host).unwrap();
            assert_eq!(
                host.values,
                [
                    i8::MIN as i64 as u64,
                    255,
                    u64::MAX,
                    (-0.0f32).to_bits() as u64,
                    (-0.0f64).to_bits(),
                    '💥' as u64,
                    i8::MIN as i64 as u64
                ]
            );
        }
    }
}

#[test]
fn scalar_source_combinations_agree_with_oracle_and_eits() {
    let setup = support::Setup {
        test_host: true,
        ..support::Setup::default()
    };
    for ty in ["i8", "u8", "i64", "u64"] {
        let source = format!(
            "import test from \"test\"\nvar a: {ty} = 1\nvar n: u8 = 0\nwhile n < 6 {{\n a = a << 1\n n += 1\n}}\ntest.check(a == 64)\ntest.check((a | 3) ^ 3 == 64)\n"
        );
        assert_eq!(
            support::outcome(&source, &setup).unwrap(),
            "effect: test.check(true)\neffect: test.check(true)\nok\n"
        );
        support::eits::round_trip(&source).unwrap();
    }
    for ty in ["f32", "f64"] {
        let source = format!(
            "import test from \"test\"\nfn f(x: {ty}) -> {ty} {{return (x * 2.0 + 1.0) / 2.0}}\nlet a: {ty} = 0.25\ntest.check(f(a) == 0.75)\nlet nan: {ty} = 0.0 / 0.0\nif nan >= 0.0 {{test.check(false)}} else {{test.check(true)}}\n"
        );
        assert_eq!(
            support::outcome(&source, &setup).unwrap(),
            "effect: test.check(true)\neffect: test.check(true)\nok\n"
        );
        support::eits::round_trip(&source).unwrap();
    }
}

#[test]
fn integer_source_boundaries_match_oracle_at_every_tier() {
    let setup = support::Setup {
        test_host: true,
        ..support::Setup::default()
    };
    for (ty, values) in [
        ("i8", ["-128", "-1", "0", "127"]),
        ("u8", ["0", "1", "128", "255"]),
        (
            "i64",
            ["-9223372036854775808", "-1", "0", "9223372036854775807"],
        ),
        (
            "u64",
            ["0", "1", "9223372036854775808", "18446744073709551615"],
        ),
    ] {
        for a in values {
            for b in values {
                for op in ["+", "-", "*", "/", "%", "&+", "&-", "&*", "&", "|", "^"] {
                    let source = format!(
                        "import test from \"test\"\nlet a: {ty} = {a}\nlet b: {ty} = {b}\nlet result = a {op} b\ntest.emit(i64(truncating: result))\n"
                    );
                    let outcome = support::outcome(&source, &setup).unwrap();
                    assert!(
                        outcome.ends_with("ok\n") || outcome.starts_with("fault:"),
                        "{source}\n{outcome}"
                    );
                }
            }
        }
    }
}

#[test]
fn storage_widths_and_nonnumeric_conversions_remain_rejected() {
    for ty in ["i16", "i32", "u16", "u32", "isize", "usize", "ptr", "cstr"] {
        let source = format!("let value: {ty} = 0\n");
        let result = support::outcome(&source, &support::Setup::default()).unwrap();
        assert!(result.starts_with("error[E0201]"), "{source}\n{result}");
    }
    for source in [
        "let x = f32(truncating: 1)\n",
        "let x = i8(true)\n",
        "let x = i64('a')\n",
        "let x = char(65)\n",
        "let x = 1 << 1.0\n",
        "let x = 1.0 &+ 2.0\n",
    ] {
        let result = support::outcome(source, &support::Setup::default()).unwrap();
        assert!(result.starts_with("error[E0301]"), "{source}\n{result}");
    }
}
