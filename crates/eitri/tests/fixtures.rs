//! Fixture corpus runner. The format is documented in the root README.

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
};

/// Header directives, written as leading `//! key: value` comment lines.
#[derive(Default)]
struct Directives {
    /// Host modules the fixture may import; only the harness `test` module exists.
    hosts: Vec<String>,
    budget: Option<u64>,
    depth: Option<u32>,
    /// Task that will make this fixture executable; until then it must be rejected.
    pending: Option<String>,
}

fn parse_directives(source: &str) -> Result<Directives, String> {
    let mut directives = Directives::default();
    for line in source.lines() {
        let Some(directive) = line.strip_prefix("//! ") else {
            break;
        };
        let (key, value) = directive
            .split_once(": ")
            .ok_or_else(|| format!("malformed directive `{line}`"))?;
        let number = || format!("directive `{key}` needs an integer, found `{value}`");
        match key {
            "host" if value == "test" => directives.hosts.push(value.to_owned()),
            "budget" => directives.budget = Some(value.parse().map_err(|_| number())?),
            "depth" => directives.depth = Some(value.parse().map_err(|_| number())?),
            "pending"
                if value.len() == 4
                    && value.starts_with('T')
                    && value[1..].bytes().all(|b| b.is_ascii_digit()) =>
            {
                directives.pending = Some(value.to_owned());
            }
            _ => return Err(format!("unknown directive `{line}`")),
        }
    }
    Ok(directives)
}

/// Check that an expectation is well formed before comparing it, so a typo
/// cannot hide inside a pending fixture until its implementation lands.
fn validate_expectation(source: &str, expected: &str) -> Result<(), String> {
    let check_span = |text: &str| -> Result<(), String> {
        let (start, end) = text
            .split_once("..")
            .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)))
            .ok_or_else(|| format!("malformed span `{text}`"))?;
        if start > end || !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return Err(format!("span `{text}` is not a source range"));
        }
        Ok(())
    };
    let mut lines = expected.lines().peekable();
    while lines.next_if(|line| line.starts_with("effect: ")).is_some() {}
    match lines.next() {
        Some("ok") => {}
        Some(line) if line.starts_with("error[") => {
            let mut line = line;
            loop {
                let rest = line
                    .strip_prefix("error[")
                    .and_then(|rest| rest.split_once("] "))
                    .and_then(|(code, rest)| {
                        let valid = code.len() == 5
                            && code.starts_with('E')
                            && code[1..].bytes().all(|b| b.is_ascii_digit());
                        valid.then_some(rest)
                    })
                    .and_then(|rest| rest.split_once(": "))
                    .ok_or_else(|| format!("malformed diagnostic `{line}`"))?;
                check_span(rest.0)?;
                match lines.next() {
                    Some(next) => line = next,
                    None => break,
                }
            }
            if expected.starts_with("effect: ") {
                return Err("a rejected program cannot have host effects".into());
            }
        }
        Some(line) if line.starts_with("fault: ") => {
            let mut frames = 0;
            for line in lines.by_ref() {
                let frame = line
                    .strip_prefix("  at ")
                    .ok_or_else(|| format!("expected a trace entry, found `{line}`"))?;
                match frame.split_once(' ') {
                    Some((_, span)) => check_span(span)?,
                    // Host functions appear by qualified name, without a range.
                    None if frame.contains('.') => {}
                    None => return Err(format!("trace entry `{line}` needs a span")),
                }
                frames += 1;
            }
            if frames == 0 {
                return Err("a fault needs at least one trace entry".into());
            }
        }
        line => return Err(format!("expected an outcome, found {line:?}")),
    }
    match lines.next() {
        Some(line) => Err(format!("unexpected line after the outcome: `{line}`")),
        None if !expected.ends_with('\n') => Err("missing final newline".into()),
        None => Ok(()),
    }
}

/// Render the actual outcome in the expectation format. Effects and faults
/// join this adapter when the context can execute (T010); the oracle and
/// later tiers run the same corpus through their own adapters.
fn run_interpreter(source: &str) -> String {
    let mut context = eitri::Context::new();
    match context.run(source) {
        Ok(()) => "ok\n".to_owned(),
        Err(diagnostic) => {
            let label = diagnostic
                .primary()
                .expect("diagnostics locate their cause");
            assert_eq!(label.source, eitri::Context::SOURCE);
            let span = label.span.range();
            assert!(source.is_char_boundary(span.start));
            assert!(source.is_char_boundary(span.end));
            assert!(span.start <= span.end);
            let code = diagnostic.code.expect("compile diagnostics have codes");
            format!(
                "error[{code}] {}..{}: {}\n",
                span.start, span.end, diagnostic.message
            )
        }
    }
}

enum Status {
    Passed,
    Pending,
}

fn run_fixture(path: &Path) -> Result<Status, String> {
    let source = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let expected = fs::read_to_string(path.with_extension("expect"))
        .map_err(|error| format!("missing expectation: {error}"))?;
    let directives = parse_directives(&source)?;
    validate_expectation(&source, &expected)?;
    let actual = run_interpreter(&source);

    if let Some(task) = &directives.pending {
        // Until its task lands, a pending fixture must still be rejected: it
        // must never run, report success, or produce host effects.
        if actual == expected {
            return Err(format!("now passes; remove `pending: {task}`"));
        }
        if !actual.starts_with("error[") {
            return Err(format!("pending fixture was not rejected:\n{actual}"));
        }
        return Ok(Status::Pending);
    }
    if !directives.hosts.is_empty() || directives.budget.is_some() || directives.depth.is_some() {
        return Err("host, budget, and depth directives need T010; mark it pending".into());
    }
    if actual != expected {
        return Err(format!("--- expected\n{expected}--- actual\n{actual}"));
    }
    Ok(Status::Passed)
}

fn discover(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        match path.extension().and_then(|extension| extension.to_str()) {
            _ if path.is_dir() => discover(&path, sources),
            Some("eit") => sources.push(path),
            Some("expect") => assert!(
                path.with_extension("eit").is_file(),
                "orphan expectation: {}",
                path.display()
            ),
            _ => panic!("unexpected fixture entry: {}", path.display()),
        }
    }
}

#[test]
fn fixtures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut sources = Vec::new();
    discover(&root, &mut sources);
    sources.sort();
    assert!(
        !sources.is_empty(),
        "fixture discovery must not silently run zero cases"
    );

    // Bound concurrency without adding a runtime or sharing mutable test state.
    let workers = thread::available_parallelism().map_or(1, usize::from);
    let mut results = Vec::with_capacity(sources.len());
    for batch in sources.chunks(workers) {
        thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|path| scope.spawn(|| run_fixture(path)))
                .collect();
            for handle in handles {
                results.push(
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("panicked; see output above".into())),
                );
            }
        });
    }

    let mut failures = String::new();
    let mut pending = 0;
    for (path, result) in sources.iter().zip(results) {
        let name = path.strip_prefix(&root).unwrap().display();
        match result {
            Ok(Status::Passed) => {}
            Ok(Status::Pending) => pending += 1,
            Err(message) => failures += &format!("\nfixture {name}: {message}\n"),
        }
    }
    assert!(failures.is_empty(), "{failures}");
    eprintln!("{} fixtures, {pending} pending", sources.len());
}
