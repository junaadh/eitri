use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_eitri"))
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn a_valid_program_runs_silently() {
    let output = cli().arg(fixture("ok/scalar.eit")).output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
}

#[test]
fn a_compile_error_is_rendered_and_exits_1() {
    let output = cli()
        .arg(fixture("reject/unknown_name.eit"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("error[E0201]: unknown name `y`"),
        "{stderr}"
    );
}

#[test]
fn a_fault_is_rendered_with_its_trace_and_exits_3() {
    let output = cli()
        .arg(fixture("fault/sub_overflow.eit"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("fault: integer overflow"), "{stderr}");
    assert!(stderr.contains("in `<module>`"), "{stderr}");
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
