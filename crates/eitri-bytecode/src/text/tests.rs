use super::*;
use crate::function::{CallTarget, FunctionId};

/// Parse `text`, expecting an error; return whether it is a mismatch and
/// the text its span covers.
fn error(text: &str) -> (bool, String, String) {
    let error = parse(text).expect_err("the text is rejected");
    let covered = text[error.span.start as usize..error.span.end as usize].to_owned();
    (error.mismatch, covered, error.message)
}

const HEADER: &str = "declare void @t.emit(i64)\n";

#[test]
fn hand_written_text_assembles() {
    // Labels named freely, comments, a forward call, and a far-from-text
    // form: `ldk` for a value outside sixteen bits.
    let text = "\
; a comment
declare void @t.emit(i64)
define void @<module>() frame(2) {
start:
    %r1 = ldi i64 10          ; the argument
    %r0 = call.script i64 @down(i64 %r1)
    call.host void @t.emit(i64 %r0)
    ret
}
define i64 @down(i64 %r1) frame(3) {
    cbi.gt %r1, 0, label %again
    %r0 = ldk i64 1000000
    ret
again:
    %r2 = subi.chk.s %r1, 1
    %r0 = call.script i64 @down(i64 %r2)
    ret
}
";
    let parsed = parse(text).unwrap();
    assert_eq!(parsed.functions.len(), 2);
    assert_eq!(&*parsed.functions[0].name, MODULE);
    assert_eq!(
        parsed.functions[0].sites[0].target,
        CallTarget::Script(FunctionId::from_u32(1))
    );
    assert_eq!(parsed.functions[0].sites[1].target, CallTarget::Host(0));
    assert_eq!(parsed.hosts[0].name, "t.emit");
    assert_eq!(&*parsed.functions[1].consts, &[1_000_000]);
    // Printing gives the canonical form, which parses to the same code.
    let printed = print(&parsed.functions, &parsed.hosts, PrintOptions::default());
    let again = parse(&printed).unwrap();
    for (a, b) in parsed.functions.iter().zip(&again.functions) {
        assert_eq!(a.code, b.code);
        assert_eq!(a.consts, b.consts);
        assert_eq!(a.sites, b.sites);
        assert_eq!(a.literal_types, b.literal_types);
        assert_eq!(a.frame_size, b.frame_size);
    }
    assert_eq!(
        print(&again.functions, &again.hosts, PrintOptions::default()),
        printed
    );
}

#[test]
fn malformed_text_is_a_syntax_error() {
    let cases: &[(&str, &str)] = &[
        (
            "define void @<module>() frame(1) {\n    %r0 = frobnicate %r0\n    ret\n}",
            "frobnicate",
        ),
        ("define void @<module>() frame(1) {\n    $\n}", "$"),
        ("declare void @\"t.emit(i64)", "@\"t.emit(i64)"),
        ("define void @<module>() frame(1) {\n    ret\n", "@<module>"),
        ("define void @<module>() frame(1) {\nr3:\n    ret\n}", "r3"),
        (
            "define void @<module>() frame(1) {\n    j_x label %a\na:\n    ret\n}",
            "j_x",
        ),
        (
            "define void @<module>() frame(1) {\n    wide\n    ret\n}",
            "wide",
        ),
        (
            "define void @<module>() frame(1) {\n    %r0 = ldi bool 1\n    ret\n}",
            "1",
        ),
        // `ret` takes nothing, so `%r0` starts the next instruction.
        ("define void @<module>() frame(1) {\n    ret %r0\n}", "}"),
        (
            "define void @<module>() frame(1) {\n    %r0 = ret\n}",
            "ret",
        ),
        (
            "define void @<module>() frame(2) {\n    %r0 = add.chk.s %r1\n    ret\n}",
            "\n",
        ),
        ("bogus", "bogus"),
        // Found by fuzzing: a slot past the encodable range overflowed the
        // assembler's frame count.
        (
            "define void @<module>() frame(1) {\n    cb.lt %r4294967295, %r0, label %a\na:\n    ret\n}",
            "%r4294967295",
        ),
    ];
    for &(text, covered) in cases {
        let (mismatch, got, message) = error(text);
        assert!(!mismatch, "{text:?}: {message}");
        assert!(
            got == covered || covered == "\n",
            "{text:?}: covered {got:?}, {message}"
        );
    }
}

#[test]
fn text_that_contradicts_its_program_is_a_mismatch() {
    let module =
        |body: &str| format!("{HEADER}define void @<module>() frame(3) {{\n{body}\n    ret\n}}\n");
    let cases: Vec<(String, &str)> = vec![
        (module("    call.script void @missing()"), "@missing"),
        (module("    call.host void @t.other()"), "@t.other"),
        (
            module("    %r0 = call.host void @t.emit(i64 %r1)"),
            "call.host",
        ),
        (module("    call.host void @t.emit(bool %r1)"), "@t.emit"),
        (module("    call.host i64 @t.emit(i64 %r1)"), "i64"),
        (module("    %r2 = ldi i64 100000"), "100000"),
        (module("    %r2 = ldk i64 5"), "5"),
        (module("    j label %nowhere"), "%nowhere"),
        (module("a:\n    %r2 = ldi i64 0\na:"), "a"),
        (module("    cbi.lt %r1, -128, label %a\na:"), "cbi.lt"),
        (
            format!("{HEADER}define void @<module>() frame(9) {{\n    ret\n}}\n"),
            "9",
        ),
        (
            "define i64 @f(i64 %r2) frame(3) {\n    ret\n}\n".to_owned(),
            "%r2",
        ),
        (
            "define void @f() frame(0) {\n    ret\n}\ndefine void @f() frame(0) {\n    ret\n}\n".to_owned(),
            "@f",
        ),
        (
            "define void @f(i64 %r0, i64 %r1) frame(4) {\n    call.script void @f(i64 %r1, i64 %r3)\n    ret\n}\n".to_owned(),
            "%r3",
        ),
    ];
    for (text, covered) in &cases {
        let (mismatch, got, message) = error(text);
        assert!(mismatch, "{text}: {message}");
        assert_eq!(&got, covered, "{text}: {message}");
    }
}
