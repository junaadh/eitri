use eitri_diag::{
    Chars, Code, Diagnostic, Diagnostics, DisplayStyle, Files, Label, Location, RenderOptions,
    Renderer, Span,
};

fn span_of(text: &str, needle: &str) -> Span {
    let start = text.find(needle).unwrap() as u32;
    Span::new(start, start + needle.len() as u32)
}

fn render(files: &Files, diagnostic: &Diagnostic) -> String {
    Renderer::new(files, RenderOptions::default()).render(diagnostic)
}

#[test]
fn single_primary_label() {
    let text = "let x: i64 = true\n";
    let mut files = Files::new();
    let id = files.add("init.eit", text);
    let d = Diagnostic::error(
        Code::TypeMismatch,
        "type mismatch: expected `i64`, found `bool`",
    )
    .with_label(Label::primary(id, span_of(text, "true")).with_message("expected `i64`"));
    assert_eq!(
        render(&files, &d),
        "\
error[E0301]: type mismatch: expected `i64`, found `bool`
  ╭─ init.eit:1:14
  │
1 │ let x: i64 = true
  │              ^^^^ expected `i64`
"
    );
}

#[test]
fn primary_and_secondary_on_one_line_with_notes() {
    let text = "let x: i64 = true\n";
    let mut files = Files::new();
    let id = files.add("init.eit", text);
    let d = Diagnostic::error(Code::TypeMismatch, "type mismatch")
        .with_label(Label::primary(id, span_of(text, "true")).with_message("found `bool`"))
        .with_label(
            Label::secondary(id, span_of(text, "i64")).with_message("expected because of this"),
        )
        .with_note("conditions are never converted")
        .with_help("write `1`\nor `0`");
    assert_eq!(
        render(&files, &d),
        "\
error[E0301]: type mismatch
  ╭─ init.eit:1:14
  │
1 │ let x: i64 = true
  │        ---   ^^^^ found `bool`
  │        │
  │        expected because of this
  │
  = note: conditions are never converted
  = help: write `1`
          or `0`
"
    );
}

#[test]
fn several_stacked_messages() {
    let text = "f(a, b, c)\n";
    let mut files = Files::new();
    let id = files.add("m.eit", text);
    let d = Diagnostic::error(Code::ArgumentCount, "bad call")
        .with_label(Label::secondary(id, span_of(text, "a")).with_message("one"))
        .with_label(Label::secondary(id, span_of(text, "b")).with_message("two"))
        .with_label(Label::primary(id, span_of(text, "f(a, b, c)")).with_message("whole"));
    assert_eq!(
        render(&files, &d),
        "\
error[E0304]: bad call
  ╭─ m.eit:1:1
  │
1 │ f(a, b, c)
  │ ^^^^^^^^^^ whole
  │   │  │
  │   │  two
  │   │
  │   one
"
    );
}

#[test]
fn lines_far_apart_are_elided_and_near_lines_are_shown() {
    let text = "fn f() -> void {\n}\n\n\n\nfn f() -> void {\n    1\n}\n";
    let mut files = Files::new();
    let id = files.add("dup.eit", text);
    let second = text.rfind("fn f").unwrap() as u32 + 3;
    let d = Diagnostic::error(Code::DuplicateName, "duplicate function `f`")
        .with_label(Label::primary(id, Span::new(second, second + 1)).with_message("redefined"))
        .with_label(Label::secondary(id, Span::new(3, 4)).with_message("first defined here"))
        .with_label(Label::secondary(id, span_of(text, "1")));
    assert_eq!(
        render(&files, &d),
        "\
error[E0202]: duplicate function `f`
  ╭─ dup.eit:6:4
  │
1 │ fn f() -> void {
  │    - first defined here
  ·
6 │ fn f() -> void {
  │    ^ redefined
7 │     1
  │     -
"
    );
}

#[test]
fn gap_of_one_line_is_printed() {
    let text = "a\nb\nc\n";
    let mut files = Files::new();
    let id = files.add("g.eit", text);
    let d = Diagnostic::error(Code::Unsupported, "gap")
        .with_label(Label::primary(id, Span::new(0, 1)))
        .with_label(Label::secondary(id, Span::new(4, 5)));
    assert_eq!(
        render(&files, &d),
        "\
error[E0001]: gap
  ╭─ g.eit:1:1
  │
1 │ a
  │ ^
2 │ b
3 │ c
  │ -
"
    );
}

#[test]
fn multi_line_span_draws_a_bracket() {
    let text = "let x = if c {\n    1\n} else {\n    2\n}\n";
    let mut files = Files::new();
    let id = files.add("if.eit", text);
    let start = text.find("if").unwrap() as u32;
    let d = Diagnostic::error(Code::IfWithoutElse, "branches differ").with_label(
        Label::primary(id, Span::new(start, text.len() as u32 - 1)).with_message("this `if`"),
    );
    assert_eq!(
        render(&files, &d),
        "\
error[E0401]: branches differ
  ╭─ if.eit:1:9
  │
1 │   let x = if c {
  │ ╭─────────^
2 │ │     1
3 │ │ } else {
4 │ │     2
5 │ │ }
  │ ╰─^ this `if`
"
    );
}

#[test]
fn wide_and_zero_width_characters_align() {
    let text = "\"ދިވެހި\" + 💥 + x\n";
    let mut files = Files::new();
    let id = files.add("u.eit", text);
    let d = Diagnostic::error(Code::UnknownName, "unknown name `x`")
        .with_label(Label::primary(id, span_of(text, "x")))
        .with_label(Label::secondary(id, span_of(text, "💥")));
    // Six Thaana letters with three zero-width vowel signs take 3 columns.
    assert_eq!(
        render(&files, &d),
        "\
error[E0201]: unknown name `x`
  ╭─ u.eit:1:16
  │
1 │ \"ދިވެހި\" + 💥 + x
  │         --   ^
"
    );
}

#[test]
fn tabs_expand_consistently() {
    let text = "\tlet y = z\n";
    let mut files = Files::new();
    let id = files.add("t.eit", text);
    let d = Diagnostic::error(Code::UnknownName, "unknown name `z`")
        .with_label(Label::primary(id, span_of(text, "z")));
    assert_eq!(
        render(&files, &d),
        "\
error[E0201]: unknown name `z`
  ╭─ t.eit:1:10
  │
1 │     let y = z
  │             ^
"
    );
}

#[test]
fn empty_span_at_end_of_input() {
    let text = "f(1";
    let mut files = Files::new();
    let id = files.add("eof.eit", text);
    let d = Diagnostic::error(Code::Syntax, "expected `)`, found end of input")
        .with_label(Label::primary(id, Span::empty(3)).with_message("expected `)`"));
    assert_eq!(
        render(&files, &d),
        "\
error[E0102]: expected `)`, found end of input
  ╭─ eof.eit:1:4
  │
1 │ f(1
  │    ^ expected `)`
"
    );
}

#[test]
fn labels_in_other_files_get_their_own_section() {
    let mut files = Files::new();
    let main = files.add("init.eit", "import ui from \"ui\"\nui.open(1)\n");
    let decl = files.add("ui.eit", "pub fn open() -> void\n");
    let d = Diagnostic::error(Code::ArgumentCount, "`open` takes 0 arguments, found 1")
        .with_label(Label::secondary(decl, Span::new(7, 11)).with_message("declared here"))
        .with_label(Label::primary(main, Span::new(20, 30)));
    assert_eq!(
        render(&files, &d),
        "\
error[E0304]: `open` takes 0 arguments, found 1
  ╭─ init.eit:2:1
  │
2 │ ui.open(1)
  │ ^^^^^^^^^^
  ├─ ui.eit:1:8
  │
1 │ pub fn open() -> void
  │        ---- declared here
"
    );
}

#[test]
fn fault_with_call_trace() {
    let text = "fn bump(n: i64) -> i64 {\n    return n + 1\n}\nbump(9_223_372_036_854_775_807)\n";
    let mut files = Files::new();
    let id = files.add("init.eit", text);
    let d = Diagnostic::fault("integer overflow")
        .with_label(Label::primary(id, span_of(text, "n + 1")).with_message("overflowed here"))
        .with_label(
            Label::secondary(id, span_of(text, "bump(9").to(span_of(text, "807)")))
                .with_message("called from <module>"),
        );
    assert_eq!(
        render(&files, &d),
        "\
fault: integer overflow
  ╭─ init.eit:2:12
  │
2 │     return n + 1
  │            ^^^^^ overflowed here
3 │ }
4 │ bump(9_223_372_036_854_775_807)
  │ ------------------------------- called from <module>
"
    );
}

#[test]
fn no_labels_and_unknown_sources() {
    let files = Files::new();
    let d = Diagnostic::error(Code::UnresolvedImport, "unresolved import `x`")
        .with_label(Label::primary(
            eitri_diag::SourceId::from_u32(9),
            Span::new(0, 1),
        ))
        .with_note("the host registers importable modules");
    assert_eq!(
        render(&files, &d),
        "error[E0205]: unresolved import `x`\n  = note: the host registers importable modules\n"
    );
}

#[test]
fn bad_spans_are_clamped() {
    let text = "💥x";
    let mut files = Files::new();
    let id = files.add("c.eit", text);
    // Starts inside the emoji and ends past the input.
    let d = Diagnostic::error(Code::Lexical, "clamped")
        .with_label(Label::primary(id, Span::new(2, 99)));
    assert_eq!(
        render(&files, &d),
        "error[E0101]: clamped\n  ╭─ c.eit:1:1\n  │\n1 │ 💥x\n  │ ^^^\n"
    );
}

#[test]
fn colors_are_opt_in() {
    let text = "x\n";
    let mut files = Files::new();
    let id = files.add("c.eit", text);
    let d = Diagnostic::error(Code::UnknownName, "unknown name `x`")
        .with_label(Label::primary(id, Span::new(0, 1)));
    let options = RenderOptions {
        color: true,
        ..RenderOptions::default()
    };
    let colored = Renderer::new(&files, options).render(&d);
    assert!(colored.starts_with("\x1b[1;31merror[E0201]\x1b[0m\x1b[1m: unknown name `x`\x1b[0m\n"));
    assert!(colored.contains("\x1b[1;31m^\x1b[0m"));
    assert!(!render(&files, &d).contains('\x1b'));
}

#[test]
fn locations_count_characters() {
    let text = "ab\n💥c\r\nd";
    let mut files = Files::new();
    let id = files.add("l.eit", text);
    let mut renderer = Renderer::new(&files, RenderOptions::default());
    let at = |line, column| Some(Location { line, column });
    assert_eq!(renderer.location(id, 0), at(1, 1));
    assert_eq!(renderer.location(id, 2), at(1, 3));
    assert_eq!(renderer.location(id, 7), at(2, 2));
    assert_eq!(renderer.location(id, 10), at(3, 1));
    assert_eq!(renderer.location(id, 1000), at(3, 2));
    assert_eq!(
        renderer.location(eitri_diag::SourceId::from_u32(1), 0),
        None
    );
}

#[test]
fn diagnostics_cap_counts_overflow() {
    let mut all = Diagnostics::new(2);
    for _ in 0..5 {
        all.push(Diagnostic::error(Code::Lexical, "e"));
    }
    assert_eq!(all.len(), 2);
    assert_eq!(all.dropped(), 3);
    assert!(all.is_full() && all.has_errors());
    all.clear();
    assert!(all.is_empty() && !all.has_errors() && all.dropped() == 0);
}

const PICK: &str = "fn pick(ready: bool) -> i64 {\n    let x = if ready {\n        1\n    } else {\n        false\n    }\n    return x\n}\n";

/// Branch mismatch with nested multi-line labels and a label in another file.
fn pick() -> (Files<'static>, Diagnostic) {
    let mut files = Files::new();
    let id = files.add("init.eit", PICK);
    let other = files.add("ui.eit", "pub fn open() -> void\n");
    let if_start = PICK.find("if ready").unwrap() as u32;
    let if_end = PICK.find("    }\n    return").unwrap() as u32 + 5;
    let d = Diagnostic::error(Code::TypeMismatch, "`if` branches have different types")
        .with_label(
            Label::primary(id, span_of(PICK, "false")).with_message("expected `i64`, found `bool`"),
        )
        .with_label(Label::secondary(id, span_of(PICK, "1")).with_message("this is `i64`"))
        .with_label(Label::secondary(id, Span::new(if_start, if_end)).with_message("in this `if`"))
        .with_label(
            Label::secondary(id, Span::new(0, PICK.len() as u32 - 1)).with_message("in `pick`"),
        )
        .with_label(Label::secondary(other, Span::new(7, 11)).with_message("declared here"))
        .with_help("make both branches the same type");
    (files, d)
}

#[test]
fn nested_multi_line_labels_take_separate_slots() {
    let (files, d) = pick();
    assert_eq!(
        render(&files, &d),
        "\
error[E0301]: `if` branches have different types
  ╭─ init.eit:5:9
  │
1 │ ╭   fn pick(ready: bool) -> i64 {
2 │ │       let x = if ready {
  │ │ ╭─────────────'
3 │ │ │         1
  │ │ │         - this is `i64`
4 │ │ │     } else {
5 │ │ │         false
  │ │ │         ^^^^^ expected `i64`, found `bool`
6 │ │ │     }
  │ │ ╰─────' in this `if`
7 │ │       return x
8 │ │   }
  │ ╰───' in `pick`
  ├─ ui.eit:1:8
  │
1 │ pub fn open() -> void
  │        ---- declared here
  │
  = help: make both branches the same type
"
    );
}

#[test]
fn ascii_characters() {
    let (files, d) = pick();
    let options = RenderOptions {
        chars: Chars::ascii(),
        ..RenderOptions::default()
    };
    assert_eq!(
        Renderer::new(&files, options).render(&d),
        "\
error[E0301]: `if` branches have different types
  --> init.eit:5:9
  |
1 | /   fn pick(ready: bool) -> i64 {
2 | |       let x = if ready {
  | | /-------------'
3 | | |         1
  | | |         - this is `i64`
4 | | |     } else {
5 | | |         false
  | | |         ^^^^^ expected `i64`, found `bool`
6 | | |     }
  | | \\-----' in this `if`
7 | |       return x
8 | |   }
  | \\---' in `pick`
  ::: ui.eit:1:8
  |
1 | pub fn open() -> void
  |        ---- declared here
  |
  = help: make both branches the same type
"
    );
}

#[test]
fn medium_and_short_styles() {
    let (files, d) = pick();
    let render_as = |style| {
        let options = RenderOptions {
            style,
            ..RenderOptions::default()
        };
        Renderer::new(&files, options).render(&d)
    };
    assert_eq!(
        render_as(DisplayStyle::Medium),
        "init.eit:5:9: error[E0301]: `if` branches have different types\n\
         = help: make both branches the same type\n"
    );
    assert_eq!(
        render_as(DisplayStyle::Short),
        "init.eit:5:9: error[E0301]: `if` branches have different types\n"
    );
    let bare = Diagnostic::bug("slot table corrupted");
    let options = RenderOptions {
        style: DisplayStyle::Short,
        ..RenderOptions::default()
    };
    assert_eq!(
        Renderer::new(&files, options).render(&bare),
        "bug: slot table corrupted\n"
    );
}

#[test]
fn long_multi_line_labels_break_with_open_slots() {
    let text = (1..=12).map(|i| format!("line {i}\n")).collect::<String>();
    let mut files = Files::new();
    let id = files.add("long.eit", &text);
    let d = Diagnostic::fault("call depth exceeded")
        .with_label(
            Label::secondary(id, span_of(&text, "line 1\n").to(span_of(&text, "line 12")))
                .with_message("in `deep`"),
        )
        .with_label(Label::primary(id, span_of(&text, "line 7")).with_message("here"));
    assert_eq!(
        render(&files, &d),
        "\
fault: call depth exceeded
   ╭─ long.eit:7:1
   │
 1 │ ╭ line 1
 2 │ │ line 2
 3 │ │ line 3
 4 │ │ line 4
   · │
 7 │ │ line 7
   │ │ ^^^^^^ here
   · │
11 │ │ line 11
12 │ │ line 12
   │ ╰───────' in `deep`
"
    );
}

#[test]
fn context_lines_around_labels() {
    let text = "a\nb\nc\nd\ne\n";
    let mut files = Files::new();
    let id = files.add("ctx.eit", text);
    let d = Diagnostic::warning(Code::Limit, "unused")
        .with_label(Label::primary(id, span_of(text, "c")));
    let options = RenderOptions {
        before_label_lines: 1,
        after_label_lines: 5,
        ..RenderOptions::default()
    };
    assert_eq!(
        Renderer::new(&files, options).render(&d),
        "\
warning[E0002]: unused
  ╭─ ctx.eit:3:1
  │
2 │ b
3 │ c
  │ ^
4 │ d
5 │ e
"
    );
}
