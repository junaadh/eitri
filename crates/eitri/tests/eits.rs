//! `.eits` (T045): printing compiled code and assembling the text back
//! gives the same code, and golden listings of the benchmark workloads.
//!
//! Round trip: for every source that compiles, the text of the assembled
//! code equals the text of the compiled code, and so do the annotated
//! listings without source spans, which show every word. Running the
//! assembled code gives the same effects and outcome.
//!
//! Golden listings: `tests/eits/<workload>.eits`. Update them only with
//! `EITRI_BLESS=1 cargo test -p eitri --test eits`, then review the diff.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use eitri::PrintOptions;
use support::eits::{context, listing, round_trip};

fn collect(directory: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, paths);
        } else if path.extension().is_some_and(|e| e == "eit") {
            paths.push(path);
        }
    }
}

#[test]
fn every_compiling_fixture_round_trips() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    collect(&root.join("tests/fixtures"), &mut paths);
    collect(&root.join("benches/fixtures"), &mut paths);
    paths.sort();
    let mut compiled = 0;
    let mut failures = String::new();
    for path in &paths {
        let source = fs::read_to_string(path).unwrap();
        match round_trip(&source) {
            Ok(true) => compiled += 1,
            Ok(false) => {}
            Err(failure) => failures += &format!("\n{}: {failure}\n", path.display()),
        }
    }
    assert!(failures.is_empty(), "{failures}");
    assert!(compiled > 40, "only {compiled} sources compiled");
}

#[test]
fn golden_listings() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bless = std::env::var_os("EITRI_BLESS").is_some();
    let mut failures = String::new();
    for name in ["module", "arithmetic", "branching", "calls", "host_calls"] {
        let source = fs::read_to_string(root.join(format!("benches/fixtures/{name}.eit"))).unwrap();
        let mut context = context();
        let compiled = context.compile(&source).unwrap();
        let text = format!(
            "; {name}.eit\n\n{}",
            listing(&context, &compiled, PrintOptions::default())
        );
        let path = root.join(format!("tests/eits/{name}.eits"));
        if bless {
            fs::write(&path, &text).unwrap();
            continue;
        }
        match fs::read_to_string(&path) {
            Ok(expected) if expected == text => {}
            Ok(expected) => {
                failures +=
                    &format!("\n{name}.eits differs\n--- expected\n{expected}--- actual\n{text}")
            }
            Err(error) => failures += &format!("\n{name}.eits: {error}; run with EITRI_BLESS=1"),
        }
    }
    assert!(failures.is_empty(), "{failures}");
}
