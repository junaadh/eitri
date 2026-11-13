//! T047: snapshots of real diagnostics, plus bounded independent recovery.
//! Source cases reuse the T045 corpus. Snapshots cover structured host data
//! and its default terminal rendering; they are never rewritten implicitly.

use std::{fs, path::Path};

use eitri::Context;
use eitri_diag::{Code, Diagnostic, Files, RenderOptions, Renderer, Severity};

struct NoHost;
impl eitri::Host for NoHost {
    fn call(&mut self, _: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
        panic!("a rejected artifact must never call the host")
    }
}

fn rejected(result: Result<eitri::Compiled, Vec<Diagnostic>>) -> Vec<Diagnostic> {
    match result {
        Err(diagnostics) => diagnostics,
        Ok(_) => panic!("the diagnostic input must be rejected"),
    }
}

/// Every existing supported code must have a real producer case. Adding a
/// code without a snapshot fails this test; E0204 stays reserved for T022.
#[test]
fn every_supported_code_has_a_structured_and_terminal_snapshot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let snapshots = root.join("diagnostics");
    let bless = std::env::var_os("EITRI_BLESS").is_some();
    let mut failures = String::new();
    for &code in Code::ALL {
        let fixture = match code {
            Code::Unsupported => Some("unsupported/struct.eit"),
            Code::Lexical => Some("reject/malformed_literal.eit"),
            Code::Syntax => Some("reject/chained_comparison.eit"),
            Code::UnknownName => Some("reject/unknown_name.eit"),
            Code::DuplicateName => Some("reject/duplicate_function.eit"),
            Code::TypeNameAsName => Some("reject/type_name_as_binding.eit"),
            Code::UnresolvedImport => Some("reject/unregistered_import.eit"),
            Code::TypeMismatch => Some("reject/argument_type.eit"),
            Code::LiteralOutOfRange => Some("reject/literal_too_large.eit"),
            Code::AssignToImmutable => Some("reject/assign_to_let.eit"),
            Code::ArgumentCount => Some("reject/argument_count.eit"),
            Code::IfWithoutElse => Some("reject/if_value_without_else.eit"),
            Code::MissingReturn => Some("reject/missing_return.eit"),
            Code::HostBindingMismatch
            | Code::OutOfMemory
            | Code::Limit
            | Code::AsmSyntax
            | Code::AsmMismatch
            | Code::AsmRejected => None,
        };
        let (name, source) = match fixture {
            Some(path) => (path.to_owned(), fs::read_to_string(root.join("fixtures").join(path)).unwrap()),
            None => match code {
                Code::HostBindingMismatch => ("binding.eit".into(), "import a from \"a\"\na.effect()\n".into()),
                Code::OutOfMemory => ("memory.eit".into(), "let x = 1\n".into()),
                Code::Limit => ("nesting.eit".into(), format!("let x = {}1{}\n", "(".repeat(257), ")".repeat(257))),
                Code::AsmSyntax => ("syntax.eits".into(), "bogus\n".into()),
                Code::AsmMismatch => ("mismatch.eits".into(), "define void @<module>() frame(1) {\n    %r0 = ldi i64 100000\n    ret\n}\n".into()),
                Code::AsmRejected => ("uninitialized.eits".into(), "define void @<module>() frame(1) {\n    %r0 = add.chk.s %r0, %r0\n    ret\n}\n".into()),
                _ => unreachable!(),
            },
        };
        let mut context = Context::new();
        if code == Code::OutOfMemory {
            context.set_memory_limit(1);
        }
        let diagnostics = if code == Code::HostBindingMismatch {
            context
                .register_host_module("a", "pub fn effect() -> void\n")
                .unwrap();
            let compiled = context.compile(&source).unwrap();
            let mut other = Context::new();
            vec![
                other
                    .execute(&compiled, &mut NoHost)
                    .unwrap_err()
                    .diagnostic(),
            ]
        } else if name.ends_with(".eits") {
            rejected(context.assemble(&source))
        } else {
            let diagnostics = rejected(context.compile(&source));
            assert_eq!(diagnostics, rejected(context.compile_simple(&source)));
            diagnostics
        };
        assert!(
            diagnostics.iter().any(|d| d.code == Some(code)),
            "{code}: {diagnostics:?}"
        );
        for diagnostic in &diagnostics {
            assert_eq!(diagnostic.severity, Severity::Error);
            assert!(diagnostic.code.is_some());
            assert!(!diagnostic.message.is_empty());
            for label in &diagnostic.labels {
                assert_eq!(label.source, Context::SOURCE);
                let range = label.span.range();
                assert!(range.start <= range.end);
                assert!(source.is_char_boundary(range.start) && source.is_char_boundary(range.end));
            }
        }
        let mut files = Files::new();
        assert_eq!(files.add(&name, &source), Context::SOURCE);
        let mut renderer = Renderer::new(&files, RenderOptions::default());
        let mut actual = format!("{diagnostics:#?}\n\n");
        for diagnostic in &diagnostics {
            actual += &renderer.render(diagnostic);
        }
        let path = snapshots.join(format!("{code}.snap"));
        if bless {
            fs::write(&path, actual).unwrap();
        } else {
            let expected = fs::read_to_string(&path).unwrap_or_default();
            if actual != expected {
                failures +=
                    &format!("\n{code} differs\n--- expected\n{expected}--- actual\n{actual}");
            }
        }
    }
    // An obsolete or misspelled snapshot must not silently stop being tested.
    for entry in fs::read_dir(&snapshots).unwrap() {
        let path = entry.unwrap().path();
        assert!(
            Code::ALL
                .iter()
                .any(|code| path.file_name().unwrap() == format!("{code}.snap").as_str()),
            "orphan snapshot: {}",
            path.display()
        );
    }
    assert!(
        failures.is_empty(),
        "{failures}\nUpdate explicitly with EITRI_BLESS=1 and review."
    );
    eprintln!(
        "{} supported codes: structured and terminal snapshots agree",
        Code::ALL.len()
    );
}

#[test]
fn recovery_is_capped_and_a_reused_context_starts_clean() {
    for (code, statement) in [
        (Code::Lexical, "$"),
        (Code::Syntax, "let vINDEX = ;"),
        (Code::UnknownName, "let vINDEX = missingINDEX"),
    ] {
        let mut context = Context::new();
        for count in [1, 99, 100, 101, 300] {
            let source: String = (0..count)
                .map(|index| format!("{}\n", statement.replace("INDEX", &index.to_string())))
                .collect();
            let diagnostics = rejected(context.compile(&source));
            assert_eq!(diagnostics.len(), count.min(100), "{code}, {count}");
            assert!(
                diagnostics.iter().all(|d| d.code == Some(code)),
                "{diagnostics:?}"
            );
            let starts: Vec<_> = diagnostics
                .iter()
                .map(|d| d.primary().unwrap().span.start)
                .collect();
            assert!(
                starts.windows(2).all(|pair| pair[0] < pair[1]),
                "each independent mistake appears once, in source order"
            );
            assert!(context.compile("let fine = 1\n").is_ok());
        }
    }
}
