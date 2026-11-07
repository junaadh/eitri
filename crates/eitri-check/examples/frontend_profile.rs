//! Frontend timing driver: lexes, parses, and checks a synthetic program
//! in the first slice, reporting each stage's share. For profiling:
//!
//! `CARGO_PROFILE_RELEASE_DEBUG=true cargo run --release -p eitri-check --example frontend_profile [seconds]`

use std::hint::black_box;
use std::time::{Duration, Instant};

use eitri_base::{Interner, SourceId};
use eitri_check::{CheckScratch, Hosts, Input, check};
use eitri_diag::Diagnostics;
use eitri_syntax::{LexScratch, ModuleKind, ParseScratch, lex, parse};

/// A program in the first slice, in the style of the fixtures.
fn workload() -> String {
    let mut text = String::from("import test from \"test\"\n");
    for i in 0..40 {
        text += &format!(
            "fn step{i}(value: i64, limit: i64) -> i64 {{
    var total = value
    var index = 0
    while index < limit {{
        if total % 2 == 0 {{
            total = total / 2 + index
        }} else {{
            total = total &* 3 &+ 1
        }}
        index += 1
    }}
    let flag = total > limit && index != 0 || !(value == 0)
    return if flag {{ total - step{j}(index, 1) }} else {{ -total }}
}}
let result{i} = step{i}({i}, 10) + 0x10 * 2
test.emit(result{i})
",
            j = (i + 1) % 40
        );
    }
    text
}

fn main() {
    let seconds: u64 = std::env::args().nth(1).map_or(3, |s| s.parse().unwrap());
    let text = workload();
    let source = SourceId::from_u32(0);
    let mut interner = Interner::new();
    let mut hosts = Hosts::new();
    hosts
        .register("test", "pub fn emit(value: i64) -> void\n", &mut interner)
        .unwrap();
    let mut lex_scratch = LexScratch::new();
    let mut parse_scratch = ParseScratch::new();
    let mut check_scratch = CheckScratch::new();
    let (mut lex_ns, mut parse_ns, mut check_ns) = (0u128, 0u128, 0u128);
    let mut runs = 0u64;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        interner.reset();
        let mut diagnostics = Diagnostics::new(100);
        let t0 = Instant::now();
        let tokens = lex(
            black_box(&text),
            source,
            &mut interner,
            &mut diagnostics,
            &mut lex_scratch,
        );
        let t1 = Instant::now();
        let ast = parse(
            &text,
            source,
            tokens,
            ModuleKind::Script,
            &mut diagnostics,
            &mut parse_scratch,
        );
        let t2 = Instant::now();
        let input = Input {
            text: &text,
            source,
            tokens,
            ast,
            interner: &interner,
            hosts: &hosts,
        };
        let checked = check(&input, &mut diagnostics, &mut check_scratch);
        let t3 = Instant::now();
        assert!(diagnostics.is_empty(), "the workload must check cleanly");
        black_box(checked.bindings.len());
        lex_ns += (t1 - t0).as_nanos();
        parse_ns += (t2 - t1).as_nanos();
        check_ns += (t3 - t2).as_nanos();
        runs += 1;
    }
    let bytes = text.len() as f64 * runs as f64;
    let total = (lex_ns + parse_ns + check_ns) as f64;
    for (name, ns) in [("lex", lex_ns), ("parse", parse_ns), ("check", check_ns)] {
        println!(
            "{name:<6} {:6.3} ns/byte  {:5.1}%",
            ns as f64 / bytes,
            100.0 * ns as f64 / total
        );
    }
    println!(
        "total  {:6.3} ns/byte  ({} bytes, {:.1} µs per run, {runs} runs)",
        total / bytes,
        text.len(),
        total / runs as f64 / 1000.0
    );
}
