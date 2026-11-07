use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_eitri"))
}

#[test]
fn source_is_rejected_with_a_diagnostic() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ok/scalar.eit");
    let output = cli().arg(path).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "error[E0001]: source execution is not implemented\n  \
             ╭─ {path}:1:1\n  │\n1 │ //! pending: T010\n  │ ^\n"
        )
    );
}

#[test]
fn invalid_invocation_reports_usage() {
    for args in [vec![], vec!["one.eit", "two.eit"]] {
        let output = cli().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"usage: eitri <source.eit>\n");
    }
}

#[test]
fn unreadable_source_reports_io_failure() {
    // A file used as a parent is guaranteed not to contain a readable child.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml/missing.eit");
    let output = cli().arg(path).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .starts_with(&format!("{path}: error[IO]: "))
    );
}
