//! E15: does keeping lexer buffers between runs (what a memory context or
//! arena would provide) pay for itself?
//!
//! Compares a fresh `LexScratch` per run (every buffer allocated and freed)
//! with one kept between runs (the shipped path, D44), which removes every
//! allocation after warm-up and is the upper bound on any arena's saving.
//!
//! Run: `cargo bench -p eitri-syntax --bench lex_alloc`
//!
//! Method: optimized build; each workload's result is checked (token count)
//! and passed through `black_box`. Batched timings: 201 batches sized to
//! about 1 ms, the two strategies interleaved batch by batch to cancel
//! drift; the median and spread of batch means are reported (batch means
//! are not single-run tails). Single-run timings: 20,001 individually timed
//! runs per cell for p50/p99, with the measured timer overhead reported
//! alongside. Raw single-run samples are written to
//! `target/bench-results/lex_alloc.csv`. Allocation counts come from a
//! counting allocator installed in this benchmark binary only.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use eitri_base::{Interner, SourceId};
use eitri_diag::Diagnostics;
use eitri_syntax::{LexScratch, lex};

/// Counts allocations so the report can attribute time to them. A
/// measurement tool for this binary, not part of Eitri.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards every call unchanged to `System`, which upholds the
// `GlobalAlloc` contract; the counters have no effect on allocation.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size() as u64, Relaxed);
        // SAFETY: same layout contract as our caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        BYTES.fetch_add(new_size as u64, Relaxed);
        // SAFETY: `ptr` came from `System` with `layout`; forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const SOURCE: SourceId = SourceId::from_u32(0);

struct Workload {
    name: String,
    files: Vec<String>,
}

fn workloads() -> Vec<Workload> {
    let root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/examples/editor-config"
    );
    let names = [
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
    ];
    let read = |name: &str| std::fs::read_to_string(format!("{root}/{name}.eit")).unwrap();
    let config: Vec<String> = names.iter().map(|name| read(name)).collect();
    let mut all = vec![
        Workload {
            name: "tiny (1 line)".into(),
            files: vec!["let answer = 42\n".into()],
        },
        Workload {
            name: "util.eit (39 lines)".into(),
            files: vec![read("util")],
        },
        Workload {
            name: "plugins.eit (130 lines)".into(),
            files: vec![read("plugins")],
        },
        Workload {
            name: "whole config (12 files, 839 lines)".into(),
            files: config.clone(),
        },
    ];
    let prose = "the quick brown fox jumps over the lazy dog; ".repeat(8);
    let unit = format!(
        "/* {prose}\n   {prose} */\nlet p = r\"{prose}\"\nlet q = r\"\"\"\n{prose}\n\"\"\"\n"
    );
    all.push(Workload {
        name: "comments and raw strings (296 KB)".into(),
        files: vec![unit.repeat(200)],
    });
    all.push(Workload {
        name: "large (config x16, 415 KB)".into(),
        files: vec![config.concat().repeat(16)],
    });
    all
}

#[derive(Clone, Copy, PartialEq)]
enum Strategy {
    Ephemeral,
    Reused,
}

impl Strategy {
    fn name(self) -> &'static str {
        match self {
            Strategy::Ephemeral => "ephemeral",
            Strategy::Reused => "reused",
        }
    }
}

struct State {
    interner: Interner,
    scratch: LexScratch,
}

/// One compilation's lexing of every file in the workload; returns the
/// token count as the checked, observable result.
#[inline(never)]
fn run(strategy: Strategy, state: &mut State, files: &[String]) -> usize {
    state.interner.reset();
    let mut diagnostics = Diagnostics::new(100);
    let mut count = 0;
    for file in files {
        count += match strategy {
            Strategy::Ephemeral => {
                let mut scratch = LexScratch::new();
                lex(
                    black_box(file),
                    SOURCE,
                    &mut state.interner,
                    &mut diagnostics,
                    &mut scratch,
                )
                .len()
            }
            Strategy::Reused => lex(
                black_box(file),
                SOURCE,
                &mut state.interner,
                &mut diagnostics,
                &mut state.scratch,
            )
            .len(),
        };
    }
    assert!(diagnostics.is_empty());
    black_box(count)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let mut csv = String::from("workload,strategy,run_ns\n");
    let timer_overhead = {
        let mut samples: Vec<f64> = (0..20_001)
            .map(|_| {
                let start = Instant::now();
                black_box(());
                start.elapsed().as_nanos() as f64
            })
            .collect();
        samples.sort_by(f64::total_cmp);
        percentile(&samples, 0.5)
    };
    println!("timer overhead (median of empty timed regions): {timer_overhead:.0} ns\n");
    println!(
        "{:<36} {:<10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>8} {:>10}",
        "workload",
        "strategy",
        "batch p50",
        "batch min",
        "batch max",
        "run p50",
        "run p99",
        "allocs",
        "bytes"
    );

    for workload in workloads() {
        let mut states = [Strategy::Ephemeral, Strategy::Reused].map(|_| State {
            interner: Interner::new(),
            scratch: LexScratch::new(),
        });
        let strategies = [Strategy::Ephemeral, Strategy::Reused];

        // Warm up, and check both strategies produce the same result.
        let expected = run(Strategy::Ephemeral, &mut states[0], &workload.files);
        for (i, &strategy) in strategies.iter().enumerate() {
            for _ in 0..200 {
                assert_eq!(run(strategy, &mut states[i], &workload.files), expected);
            }
        }

        // Size batches to about 1 ms.
        let once = {
            let start = Instant::now();
            for _ in 0..100 {
                run(Strategy::Ephemeral, &mut states[0], &workload.files);
            }
            start.elapsed() / 100
        };
        let batch = (Duration::from_millis(1).as_nanos() / once.as_nanos().max(1)).max(1) as u32;

        let mut batch_means = [Vec::new(), Vec::new()];
        for _ in 0..201 {
            for (i, &strategy) in strategies.iter().enumerate() {
                let start = Instant::now();
                for _ in 0..batch {
                    run(strategy, &mut states[i], &workload.files);
                }
                batch_means[i].push(start.elapsed().as_nanos() as f64 / batch as f64);
            }
        }

        let single_runs = if once < Duration::from_micros(200) {
            20_001
        } else {
            501
        };
        let mut runs = [Vec::new(), Vec::new()];
        for _ in 0..single_runs {
            for (i, &strategy) in strategies.iter().enumerate() {
                let start = Instant::now();
                run(strategy, &mut states[i], &workload.files);
                runs[i].push(start.elapsed().as_nanos() as f64);
            }
        }

        for (i, &strategy) in strategies.iter().enumerate() {
            let before = (ALLOCATIONS.load(Relaxed), BYTES.load(Relaxed));
            run(strategy, &mut states[i], &workload.files);
            let allocations = ALLOCATIONS.load(Relaxed) - before.0;
            let bytes = BYTES.load(Relaxed) - before.1;

            for sample in &runs[i] {
                csv += &format!("\"{}\",{},{}\n", workload.name, strategy.name(), sample);
            }
            batch_means[i].sort_by(f64::total_cmp);
            runs[i].sort_by(f64::total_cmp);
            let b = &batch_means[i];
            println!(
                "{:<36} {:<10} {:>8.0}ns {:>8.0}ns {:>8.0}ns {:>8.0}ns {:>8.0}ns {:>8} {:>10}",
                workload.name,
                strategy.name(),
                percentile(b, 0.5),
                b[0],
                b[b.len() - 1],
                percentile(&runs[i], 0.5),
                percentile(&runs[i], 0.99),
                allocations,
                bytes
            );
        }
        let ratio = percentile(&batch_means[1], 0.5) / percentile(&batch_means[0], 0.5);
        println!("{:<36} reused / ephemeral (batch p50): {:.3}\n", "", ratio);
    }

    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/bench-results");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::File::create(format!("{dir}/lex_alloc.csv"))
        .unwrap()
        .write_all(csv.as_bytes())
        .unwrap();
    println!("raw single-run samples: target/bench-results/lex_alloc.csv");
}
