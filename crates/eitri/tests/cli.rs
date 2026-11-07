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
    let invalid: [&[&str]; 6] = [
        &[],
        &["one.eit", "two.eit"],
        &["asm"],
        &["dump", "bytes", "x.eit"],
        &["--host", "missing-equals", "x.eit"],
        &["asm", "--bogus", "x.eit"],
    ];
    for args in invalid {
        let output = cli().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.starts_with("usage: eitri <command>"),
            "{args:?}: {stderr}"
        );
    }
    let help = cli().arg("help").output().unwrap();
    assert_eq!(help.status.code(), Some(0));
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .starts_with("usage: eitri <command>")
    );
}

/// A temporary path in the target directory, unique to the test.
fn scratch(name: &str) -> String {
    let dir = concat!(env!("CARGO_TARGET_TMPDIR"), "/cli");
    std::fs::create_dir_all(dir).unwrap();
    format!("{dir}/{name}")
}

const TEST_DECLARATIONS: &str = "\
pub fn emit(value: i64) -> void
pub fn check(value: bool) -> bool
pub fn id(value: i64) -> i64
pub fn fail() -> void
pub fn reenter(value: i64) -> i64
";

#[test]
fn asm_prints_eits_that_verify_runs_and_check_accept() {
    let source = fixture("ok/functions.eit");
    let host = scratch("test.eith");
    std::fs::write(&host, TEST_DECLARATIONS).unwrap();
    let host = format!("test={host}");
    let out = scratch("functions.eits");

    let printed = cli()
        .args(["--host", &host, "asm", &source])
        .output()
        .unwrap();
    assert_eq!(printed.status.code(), Some(0));
    let text = String::from_utf8(printed.stdout).unwrap();
    assert!(text.starts_with("; functions.eit\n\n"), "{text}");
    assert!(text.contains("define void @<module>() frame("), "{text}");

    let written = cli()
        .args(["--host", &host, "asm", "-o", &out, &source])
        .output()
        .unwrap();
    assert_eq!(written.status.code(), Some(0));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), text);

    let verified = cli()
        .args(["--host", &host, "verify", &out])
        .output()
        .unwrap();
    assert_eq!(verified.status.code(), Some(0));
    assert!(
        String::from_utf8(verified.stdout)
            .unwrap()
            .ends_with("functions verified\n")
    );
    let checked = cli()
        .args(["--host", &host, "check", &out])
        .output()
        .unwrap();
    assert_eq!(checked.status.code(), Some(0));
    assert!(checked.stdout.is_empty() && checked.stderr.is_empty());

    // The CLI implements no host functions: the first call faults, at its
    // line in the `.eits` file.
    let run = cli()
        .args(["--host", &host, "run", &out])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(3));
    let stderr = String::from_utf8(run.stderr).unwrap();
    assert!(
        stderr.starts_with("fault: `eitri run` implements no host functions"),
        "{stderr}"
    );
    assert!(stderr.contains("functions.eits:"), "{stderr}");

    let annotated = cli()
        .args(["--host", &host, "asm", "--annotate", &source])
        .output()
        .unwrap();
    assert!(
        String::from_utf8(annotated.stdout)
            .unwrap()
            .contains("; 0000  ")
    );
}

#[test]
fn rejected_eits_reports_its_line() {
    let path = scratch("bad.eits");
    std::fs::write(
        &path,
        "define void @<module>() frame(1) {\n    %r0 = ldi i64 100000\n    ret\n}\n",
    )
    .unwrap();
    let output = cli()
        .args(["verify", &path])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("error[E0902]: this value loads with `ldk`, not `ldi`"),
        "{stderr}"
    );
    assert!(stderr.contains("bad.eits:2:"), "{stderr}");
    // `verify` is for `.eits`; `check` compiles `.eit`.
    let wrong = cli()
        .args(["verify", &fixture("ok/scalar.eit")])
        .output()
        .unwrap();
    assert_eq!(wrong.status.code(), Some(2));
}

#[test]
fn dump_prints_tokens_and_the_tree() {
    let source = fixture("ok/scalar.eit");
    let tokens = cli().args(["dump", "tokens", &source]).output().unwrap();
    assert_eq!(tokens.status.code(), Some(0));
    let tokens = String::from_utf8(tokens.stdout).unwrap();
    assert!(
        tokens
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("0..")),
        "{tokens}"
    );

    let ast = cli().args(["dump", "ast", &source]).output().unwrap();
    assert_eq!(ast.status.code(), Some(0));
    let ast = String::from_utf8(ast.stdout).unwrap();
    assert!(ast.contains("let ") || ast.contains("var "), "{ast}");

    // A syntax error is reported like any compile error.
    let broken = cli()
        .args(["dump", "ast", &fixture("reject/unknown_name.eit")])
        .output()
        .unwrap();
    assert_eq!(
        broken.status.code(),
        Some(0),
        "names are checked later than the tree"
    );
    let lexical = cli()
        .args([
            "dump",
            "tokens",
            &fixture("reject/unexpected_character.eit"),
        ])
        .output()
        .unwrap();
    assert_eq!(lexical.status.code(), Some(1));
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
