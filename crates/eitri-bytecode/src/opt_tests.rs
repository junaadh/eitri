//! The optimizer's passes on hand-written `.eits`: each rewrite where its
//! conditions hold, and none where one fails. Behavior is checked end to
//! end in `crates/eitri` (every fixture, random program, and mutated
//! `.eits` runs before and after optimizing); these pin the conditions
//! compiled code alone never exercises.

use crate::Callees;
use crate::function::{FunctionId, Signature};
use crate::opt::{Report, optimize};
use crate::text::{PrintOptions, parse, print};
use eitri_base::IndexVec;

/// Optimize every function of `text`, verify the result, and print it.
fn optimized(text: &str) -> (String, Report) {
    let parsed = parse(text).unwrap_or_else(|e| panic!("{e:?}\n{text}"));
    let mut report = Report::default();
    let mut asm = crate::Assembler::recording();
    let mut functions = Vec::new();
    let mut bodies = Vec::new();
    for (function, body) in parsed.functions.iter().zip(&parsed.bodies) {
        let body = optimize(body, function, &mut report);
        let (code, body) =
            crate::ir::assemble(&body, &function.name, function.signature.clone(), &mut asm)
                .unwrap();
        functions.push(code);
        bodies.push(body);
    }
    let signatures: IndexVec<FunctionId, Signature> =
        IndexVec::from_raw(functions.iter().map(|f| f.signature.clone()).collect());
    let hosts: Vec<Signature> = parsed.hosts.iter().map(|h| h.signature.clone()).collect();
    let callees = Callees {
        functions: &signatures,
        hosts: &hosts,
        record_shapes: &[],
    };
    for f in &functions {
        crate::verify(f, &callees).unwrap_or_else(|e| panic!("{}: {e}", f.name));
    }
    (
        print(&functions, &bodies, &parsed.hosts, PrintOptions::default()),
        report,
    )
}

const HOSTS: &str = "declare void @t.emit(i64)\ndeclare i64 @t.id(i64)\n";

/// `body` as top-level code, its frame covering every slot it names, with
/// two script functions to call.
fn module(body: &str) -> String {
    let frame = body
        .match_indices("%r")
        .filter_map(|(at, _)| {
            let digits: String = body[at + 2..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse::<u32>().ok()
        })
        .max()
        .map_or(0, |slot| slot + 1);
    format!(
        "{HOSTS}define void @<module>() frame({frame}) {{\n{body}\n    ret\n}}\ndefine void @f(i64 %r0) frame(1) {{\n    ret\n}}\ndefine void @g(i64 %r0, i64 %r1) frame(2) {{\n    ret\n}}\n"
    )
}

#[test]
fn argument_copies_are_coalesced() {
    let (text, report) = optimized(&module(
        "    %r1 = ldi i64 5\n    %r2 = mov %r1\n    call.script void @f(i64 %r2)\n    %r2 = mov %r1\n    call.host void @t.emit(i64 %r2)",
    ));
    assert_eq!(report.coalesced_moves, 2, "{text}");
    assert!(text.contains("call.script void @f(i64 %r1)"), "{text}");
    assert!(text.contains("call.host void @t.emit(i64 %r1)"), "{text}");
    assert!(!text.contains("mov"), "{text}");
}

#[test]
fn argument_copies_stay_when_a_condition_fails() {
    let cases = [
        // The temporary is read after the call.
        "    %r1 = ldi i64 5\n    %r2 = mov %r1\n    call.script void @f(i64 %r2)\n    %r3 = mov %r2",
        // The sources are not consecutive.
        "    %r1 = ldi i64 5\n    %r3 = ldi i64 6\n    %r4 = mov %r1\n    %r5 = mov %r3\n    call.script void @g(i64 %r4, i64 %r5)",
        // The sources are in the wrong order.
        "    %r1 = ldi i64 5\n    %r2 = ldi i64 6\n    %r4 = mov %r2\n    %r5 = mov %r1\n    call.script void @g(i64 %r4, i64 %r5)",
        // A host call's argument window would overlap its result.
        "    %r0 = ldi i64 5\n    %r2 = mov %r0\n    %r0 = call.host i64 @t.id(i64 %r2)",
        // Not every argument is a copy.
        "    %r1 = ldi i64 5\n    %r4 = mov %r1\n    %r5 = ldi i64 6\n    call.script void @g(i64 %r4, i64 %r5)",
    ];
    for body in cases {
        let (text, report) = optimized(&module(body));
        assert_eq!(report.coalesced_moves, 0, "{body}\n{text}");
    }
}

/// A counted loop whose condition compares with a literal outside the
/// eight-bit immediate range.
fn counted(extra: &str) -> String {
    module(&format!(
        "    %r0 = ldi i64 0\ntop:\n    %r1 = ldi i64 1000\n    cb.le %r1, %r0, label %done\n    %r0 = addi.chk.s %r0, 1\n{extra}    j label %top\ndone:\n    call.host void @t.emit(i64 %r0)"
    ))
}

#[test]
fn loop_invariant_literals_move_before_the_loop() {
    let (text, report) = optimized(&counted(""));
    assert_eq!(report.hoisted_loads, 1, "{text}");
    // Loaded once, into a new slot, before the loop's first block.
    assert!(text.contains("define void @<module>() frame(3)"), "{text}");
    let load = text.find("%r2 = ldi i64 1000").expect(&text);
    let compare = text.find("cb.le %r2, %r0").expect(&text);
    let back = text.find("j label").expect(&text);
    assert!(load < compare && compare < back, "{text}");
}

/// Found by an audit: a frame already using every encodable slot left
/// hoisting nowhere to put the literal, and it allocated `%r65536`.
#[test]
fn literals_stay_when_no_slot_is_left() {
    let text = module(
        "    %r65535 = ldi i64 7\n    %r0 = ldi i64 0\ntop:\n    %r1 = ldi i64 1000\n    cb.le %r1, %r0, label %done\n    %r0 = addi.chk.s %r0, 1\n    j label %top\ndone:",
    );
    let (out, report) = optimized(&text);
    assert_eq!(report.hoisted_loads, 0, "{out}");
    assert!(out.contains("frame(65536)"), "{out}");
}

#[test]
fn literals_stay_in_the_loop_when_a_condition_fails() {
    let cases = [
        // The loop is also entered in the middle.
        module(
            "    %r0 = ldi i64 0\n    j label %inside\ntop:\n    %r1 = ldi i64 1000\n    cb.le %r1, %r0, label %done\ninside:\n    %r0 = addi.chk.s %r0, 1\n    j label %top\ndone:",
        ),
        // The literal slot is read again after its user.
        counted("    %r2 = mov %r1\n"),
        // The literal feeds a call: a call's window is fixed.
        module(
            "    %r0 = ldi i64 0\ntop:\n    cbi.gt %r0, 2, label %done\n    %r1 = ldi i64 1000\n    call.script void @f(i64 %r1)\n    %r0 = addi.chk.s %r0, 1\n    j label %top\ndone:",
        ),
    ];
    for text in &cases {
        let (out, report) = optimized(text);
        assert_eq!(report.hoisted_loads, 0, "{text}\n{out}");
    }
}
