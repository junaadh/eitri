//! Profiling driver: lexes the reference config in a loop for a fixed time,
//! for use under a sampling profiler (Instruments, perf, samply):
//!
//! `CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release -p eitri-syntax --example lex_profile`
//!
//! Prints ns per source byte so a profile can be tied to a throughput.

use std::hint::black_box;
use std::time::{Duration, Instant};

use eitri_base::{Interner, SourceId};
use eitri_diag::Diagnostics;
use eitri_syntax::{LexScratch, lex};

fn main() {
    let seconds: u64 = std::env::args().nth(1).map_or(5, |s| s.parse().unwrap());
    let root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/examples/editor-config"
    );
    let mut files = Vec::new();
    for name in [
        "init",
        "options",
        "keymaps",
        "autocmds",
        "plugins",
        "language_servers",
        "statusline",
        "theme",
        "util",
        "host/editor",
        "host/lsp",
        "host/pkg",
    ] {
        files.push(std::fs::read_to_string(format!("{root}/{name}.eit")).unwrap());
    }
    let bytes: usize = files.iter().map(String::len).sum();
    let mut interner = Interner::new();
    let mut scratch = LexScratch::new();
    let mut runs = 0u64;
    let mut tokens = 0usize;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        for _ in 0..100 {
            interner.reset();
            let mut diagnostics = Diagnostics::new(100);
            for file in &files {
                tokens += lex(
                    black_box(file),
                    SourceId::from_u32(0),
                    &mut interner,
                    &mut diagnostics,
                    &mut scratch,
                )
                .len();
            }
            runs += 1;
        }
    }
    let ns = start.elapsed().as_nanos() as f64;
    println!(
        "{runs} runs, {:.3} ns/byte, {:.1} ns/token ({} tokens/run)",
        ns / (runs as f64 * bytes as f64),
        ns / tokens as f64,
        black_box(tokens) as u64 / runs
    );
}
