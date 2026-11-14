//! Encoding limits (T012): programs large enough to need `WIDE` slot
//! operands, far forward branches (a complement branch over `J_X`), and
//! `LDK_X` constant
//! indices, run by both interpreters and the program-level oracle, plus the
//! locals limit of the first-slice chapter ("more than 65,535 locals in one
//! function" is `E0002`). Sources are built here because they are too large
//! to keep as fixtures. `eitri-lower`'s tests confirm the same shapes emit
//! the long forms.

mod support;

use std::fmt::Write;

use support::Setup;

fn run(source: &str) -> String {
    let setup = Setup {
        test_host: true,
        ..Setup::default()
    };
    let outcome =
        support::outcome(source, &setup).unwrap_or_else(|disagreement| panic!("{disagreement}"));
    // The long encodings must survive `.eits` too.
    if let Err(failure) = support::eits::round_trip(source) {
        panic!("{failure}");
    }
    outcome
}

fn effects(values: impl IntoIterator<Item = i64>) -> String {
    let mut out = String::new();
    for value in values {
        writeln!(out, "effect: test.emit({value})").unwrap();
    }
    out
}

const IMPORT: &str = "import test from \"test\"\n";

/// 300 live locals: operands above slot 255 need `WIDE`.
#[test]
fn wide_slots() {
    let mut source = IMPORT.to_owned();
    for i in 0..300 {
        writeln!(source, "let v{i} = {i}").unwrap();
    }
    // Reads, arithmetic, and a branch on the highest slots.
    source +=
        "var sum = 0\nvar i = 0\nwhile i < 3 {\n    sum += v299 * v298 - v257\n    i += 1\n}\n";
    source += "test.emit(sum)\nif v299 > v256 {\n    test.emit(v299 &+ v1)\n}\n";
    // The same inside a function, whose frame is separate.
    source += "fn wide(a: i64) -> i64 {\n";
    for i in 0..300 {
        writeln!(source, "    let w{i} = a + {i}").unwrap();
    }
    source += "    return w299 - w0\n}\ntest.emit(wide(1000))\n";
    let sum = 3 * (299 * 298 - 257);
    assert_eq!(run(&source), effects([sum, 300, 299]) + "ok\n");
}

/// Conditional bodies beyond the short displacements, so each forward
/// branch is a complement over `J_X`: comparisons (8-bit displacement) past
/// a few hundred statements, `bool` tests (16-bit) past 32,767 words, and a
/// loop exit. An `else` is skipped by `J`, as is a loop's back edge (loops
/// test at the top); its 24-bit displacement is not approached here.
#[test]
fn far_branches() {
    let body = |count: i64| {
        let mut text = String::new();
        for k in 0..count {
            writeln!(text, "    x = x &+ {k}").unwrap();
        }
        text
    };
    let step = |count: i64| (0..count).sum::<i64>();
    let mut source = IMPORT.to_owned();
    source += "var x = 0\nlet one = test.id(1)\nlet two = test.id(2)\n";
    source += "let yes = test.check(true)\nlet no = test.check(false)\n";
    let mut expected = "effect: test.id(1)\neffect: test.id(2)\n".to_owned();
    expected += "effect: test.check(true)\neffect: test.check(false)\n";
    let mut x = 0i64;
    // Taken and not taken, each with an `else` as large.
    let cases = [
        ("one == 1", true, 400),
        ("one > two", false, 400),
        ("yes", true, 20_000),
        ("no", false, 20_000),
    ];
    for (condition, taken, count) in cases {
        writeln!(source, "if {condition} {{\n    test.emit(1)").unwrap();
        source += &body(count);
        source += "} else {\n    test.emit(2)\n";
        source += &body(count);
        source += "}\ntest.emit(x)\n";
        x = x.wrapping_add(step(count));
        expected += &effects([if taken { 1 } else { 2 }, x]);
    }
    source += "var i = 0\nwhile i < 3 {\n    i += 1\n";
    source += &body(400);
    source += "}\ntest.emit(x)\ntest.emit(i)\n";
    x = x.wrapping_add(3 * step(400));
    expected += &effects([x, 3]);
    assert_eq!(run(&source), expected + "ok\n");
}

/// More distinct constants than a 16-bit index reaches: the later ones are
/// loaded with `LDK_X`.
#[test]
fn far_constants() {
    const COUNT: i64 = 70_000;
    let mut source = IMPORT.to_owned();
    source += "var x = 0\nvar sum = 0\n";
    // Values outside the 16-bit immediate, all distinct.
    for i in 0..COUNT {
        writeln!(source, "x = {}", 1_000_000 + i).unwrap();
        if i % 10_000 == 0 || i == COUNT - 1 {
            source += "test.emit(x)\n";
        }
    }
    let mut expected: Vec<i64> = (0..COUNT).step_by(10_000).map(|i| 1_000_000 + i).collect();
    expected.push(1_000_000 + COUNT - 1);
    assert_eq!(run(&source), effects(expected) + "ok\n");
}

/// `count` module-level locals.
fn locals(count: usize) -> String {
    let mut source = IMPORT.to_owned();
    for i in 0..count {
        writeln!(source, "let v{i} = {i}").unwrap();
    }
    writeln!(source, "test.emit(v{})", count - 1).unwrap();
    source
}

#[test]
fn locals_limit() {
    // At the limit: runs. The emit's argument may reuse a local's slot or
    // need a temporary; either way the program is within the limit.
    let at = locals(65_535);
    assert_eq!(run(&at), effects([65_534]) + "ok\n");
    // One more: rejected before anything runs, at the function.
    let over = locals(65_536);
    let outcome = run(&over);
    assert!(
        outcome.starts_with("error[E0002]"),
        "expected E0002, got:\n{outcome}"
    );
}

/// At most 64 nested runs: `test.reenter(n)` from top-level code starts
/// `n + 1` of them, each `callback` reentering with one less.
#[test]
fn nesting_limit() {
    let source = |n: i64| {
        format!(
            "{IMPORT}fn callback(n: i64) -> i64 {{\n    if n == 0 {{\n        return 0\n    }}\n    return test.reenter(n - 1) + n\n}}\nlet r = test.reenter({n})\ntest.emit(r)\n"
        )
    };
    let reentries = |n: i64| {
        (0..=n)
            .rev()
            .map(|k| format!("effect: test.reenter({k})\n"))
            .collect::<String>()
    };
    assert_eq!(
        run(&source(63)),
        reentries(63) + &effects([(0..=63).sum()]) + "ok\n"
    );
    let over = run(&source(64));
    // Each `test.reenter` records its effect before reentering; the 65th
    // nested run is refused.
    let expected = reentries(64) + "fault: call depth exceeded\n";
    assert!(over.starts_with(&expected), "{over}");
}

#[test]
fn struct_field_limit_rejects_before_execution() {
    let mut source = String::from("struct Wide {\n");
    for index in 0..257 {
        writeln!(source, "f{index}: i64").unwrap();
    }
    source += "}\nlet value: Wide = Wide {}\nvalue.f0\n";
    assert!(run(&source).starts_with("error[E0002]"));
}

#[test]
fn deep_struct_layout_and_cycles_are_bounded() {
    let mut source = String::new();
    for index in 0..2000 {
        writeln!(source, "struct S{index} {{ value: S{} }}", index + 1).unwrap();
    }
    source += "struct S2000 { value: i64 }\n";
    assert_eq!(run(&source), "ok\n");
    let recursive = "struct A { b: B }\nstruct B { a: A }\n";
    assert!(run(recursive).starts_with("error[E0301]"));
}

#[test]
fn method_count_limit_rejects_before_execution() {
    let mut source = String::from("struct T { x: i64 }\nextend T {\n");
    for index in 0..257 {
        writeln!(source, "fn m{index}() -> void {{}}").unwrap();
    }
    source += "}\n";
    assert!(run(&source).starts_with("error[E0002]"));
}

#[test]
fn enum_struct_inline_cycles_are_rejected() {
    let source = "struct A { value: B }\nenum B { Next(value: A), End }\n";
    assert!(run(source).starts_with("error[E0301]"));
}

#[test]
fn finite_pattern_groups_are_bounded() {
    let mut source = String::from("enum Many {\n");
    for i in 0..33 {
        writeln!(source, "C{i}(value: bool),").unwrap();
    }
    source.push_str("}\nfn inspect(value: Many) -> i64 { return match value {\n");
    for i in 0..33 {
        writeln!(source, ".C{i}(true) => 1, .C{i}(false) => 0,").unwrap();
    }
    source.push_str("} }\n");
    let mut context = eitri::Context::new();
    let diagnostics = match context.compile(&source) {
        Err(diagnostics) => diagnostics,
        Ok(_) => panic!("group limit must reject before execution"),
    };
    assert!(
        diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("more than 32 finite pattern groups")),
        "{diagnostics:?}"
    );
}
