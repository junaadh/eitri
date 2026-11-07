//! T011: source-to-result latency for the first slice.
//!
//! For each workload in `benches/fixtures/*.eit`, from fresh source text:
//! compile (lex, parse, check, lower) and first execution of the fresh
//! code, timed separately within one run, and their sum as the end-to-end
//! latency. Every run's result reaches the host through `bench.emit` and is
//! checked against the workload's known answer, so no work can disappear.
//!
//! Run: `cargo bench -p eitri --bench source_to_result [samples]`
//!
//! Cases:
//! - **reused**: one initialized context (host module registered, scratch
//!   warmed by 1,000 runs) compiles and runs the source again each sample:
//!   the embedding case of a long-lived host.
//! - **fresh**: a new context per sample, then one compile and run in it.
//!   Context creation and host registration are timed separately and
//!   excluded from that sample's compile/execute/total.
//!
//! Single-run samples are individually timed (p50/p99 over `samples`,
//! default 20,001); the timer's own cost is measured and reported, not
//! subtracted. Raw samples go to `target/bench-results/source_to_result.csv`.
//! Allocation counts and bytes come from a counting allocator installed in
//! this benchmark binary only. Retained bytes are the heap the context
//! holds after warm-up, compiled code dropped: its scratch high-water.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::hint::black_box;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use eitri::{Context, Host};

/// Counts allocations and live bytes. A measurement tool for this binary,
/// not part of Eitri.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicI64 = AtomicI64::new(0);

// SAFETY: forwards every call unchanged to `System`, which upholds the
// `GlobalAlloc` contract; the counters have no effect on allocation.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size() as u64, Relaxed);
        LIVE.fetch_add(layout.size() as i64, Relaxed);
        // SAFETY: same layout contract as our caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as i64, Relaxed);
        // SAFETY: `ptr` came from `System.alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        BYTES.fetch_add(new_size as u64, Relaxed);
        LIVE.fetch_add(new_size as i64 - layout.size() as i64, Relaxed);
        // SAFETY: `ptr` came from `System` with `layout`; forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const BENCH_MODULE: &str = "pub fn emit(value: i64) -> void\npub fn id(value: i64) -> i64\n";

/// The workloads and their answers (checked independently of Eitri).
const WORKLOADS: &[(&str, i64)] = &[
    ("module", 754),
    ("arithmetic", 7_914_953_978_373_240_671),
    ("branching", 1622),
    ("calls", 610),
    ("host_calls", 499_500),
];

struct BenchHost {
    emit: u32,
    emitted: Option<i64>,
}

impl Host for BenchHost {
    fn call(&mut self, index: u32, args: &[u64], results: &mut [u64]) -> Result<(), String> {
        if index == self.emit {
            self.emitted = Some(args[0] as i64);
        } else {
            // `bench.id`: the minimal scalar round trip.
            results[0] = args[0];
        }
        Ok(())
    }
}

fn initialized() -> (Context, BenchHost) {
    let mut context = Context::new();
    // EITRI_THREADED=0 selects the `match` interpreter; threaded is the default.
    context.set_threaded(std::env::var_os("EITRI_THREADED").is_none_or(|v| v != "0"));
    context
        .register_host_module("bench", BENCH_MODULE)
        .expect("the bench module declares cleanly");
    let emit = context.host_index("bench", "emit").expect("declared");
    (
        context,
        BenchHost {
            emit,
            emitted: None,
        },
    )
}

#[derive(Clone, Copy)]
struct Sample {
    compile: u64,
    execute: u64,
}

/// One compile and first execution, timed in parts; panics on a wrong
/// answer so a broken run is never reported as fast.
fn sample(context: &mut Context, host: &mut BenchHost, source: &str, answer: i64) -> Sample {
    host.emitted = None;
    let t0 = Instant::now();
    let compiled = context
        .compile(black_box(source))
        .expect("workloads compile");
    let t1 = Instant::now();
    context.execute(&compiled, host).expect("workloads run");
    let t2 = Instant::now();
    assert_eq!(host.emitted, Some(answer));
    drop(black_box(compiled));
    Sample {
        compile: (t1 - t0).as_nanos() as u64,
        execute: (t2 - t1).as_nanos() as u64,
    }
}

struct Stats {
    p50: u64,
    p99: u64,
    min: u64,
}

fn stats(values: &mut [u64]) -> Stats {
    values.sort_unstable();
    let at = |q: usize| values[(values.len() - 1) * q / 100];
    Stats {
        p50: at(50),
        p99: at(99),
        min: values[0],
    }
}

fn ns(value: u64) -> String {
    if value >= 10_000 {
        format!("{:.1} µs", value as f64 / 1000.0)
    } else {
        format!("{value} ns")
    }
}

fn command(program: &str, args: &[&str]) -> String {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unavailable".into(), |s| s.trim().replace('\n', "; "))
}

fn main() {
    let samples: usize = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .map_or(20_001, |s| s.parse().expect("samples is a number"));
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut csv = String::from("workload,case,compile_ns,execute_ns\n");

    println!("# source_to_result");
    println!(
        "cpu: {}",
        command("sysctl", &["-n", "machdep.cpu.brand_string"])
    );
    println!(
        "os: {} {}",
        command("sw_vers", &["-productName"]),
        command("sw_vers", &["-productVersion"])
    );
    println!("rustc: {}", command("rustc", &["-vV"]));
    println!("profile: release (workspace defaults), samples per cell: {samples}");
    println!(
        "interpreter: {}",
        if std::env::var_os("EITRI_THREADED").is_none_or(|v| v != "0") {
            "threaded"
        } else {
            "match"
        }
    );
    let mut timer = Vec::with_capacity(samples);
    for _ in 0..samples {
        let t0 = Instant::now();
        let t1 = Instant::now();
        timer.push((t1 - t0).as_nanos() as u64);
    }
    let timer = stats(&mut timer);
    // On Apple silicon `Instant` ticks at 24 MHz (about 41.7 ns), so single
    // readings are quantized to that step.
    println!(
        "timer overhead (one reading pair): p50 {} p99 {}\n",
        ns(timer.p50),
        ns(timer.p99)
    );

    println!(
        "| workload | bytes | case | compile p50 / p99 | execute p50 / p99 | total p50 / p99 | total min |"
    );
    println!("|---|---|---|---|---|---|---|");
    let mut footprints = String::new();
    for &(name, answer) in WORKLOADS {
        let path = root.join(format!("benches/fixtures/{name}.eit"));
        let source = std::fs::read_to_string(&path).expect("workload source");

        // Reused: one initialized context, warmed up.
        let before = LIVE.load(Relaxed);
        let (mut context, mut host) = initialized();
        for _ in 0..1000 {
            sample(&mut context, &mut host, &source, answer);
        }
        let retained = LIVE.load(Relaxed) - before;
        let (allocations, bytes) = (ALLOCATIONS.load(Relaxed), BYTES.load(Relaxed));
        sample(&mut context, &mut host, &source, answer);
        let allocations = ALLOCATIONS.load(Relaxed) - allocations;
        let bytes = BYTES.load(Relaxed) - bytes;
        let compiled = context.compile(&source).expect("workloads compile");
        let mut reused = Vec::with_capacity(samples);
        for _ in 0..samples {
            reused.push(sample(&mut context, &mut host, &source, answer));
        }
        drop(context);

        // Fresh: a new context per sample.
        let mut fresh = Vec::with_capacity(samples);
        let mut creation = Vec::with_capacity(samples);
        let fresh_allocations = ALLOCATIONS.load(Relaxed);
        for _ in 0..samples {
            let t0 = Instant::now();
            let (mut context, mut host) = initialized();
            creation.push((Instant::now() - t0).as_nanos() as u64);
            fresh.push(sample(&mut context, &mut host, &source, answer));
        }
        let fresh_allocations = (ALLOCATIONS.load(Relaxed) - fresh_allocations) / samples as u64;

        for (case, runs) in [("reused", &reused), ("fresh", &fresh)] {
            let mut compile: Vec<u64> = runs.iter().map(|s| s.compile).collect();
            let mut execute: Vec<u64> = runs.iter().map(|s| s.execute).collect();
            let mut total: Vec<u64> = runs.iter().map(|s| s.compile + s.execute).collect();
            for s in runs.iter() {
                let _ = writeln!(csv, "{name},{case},{},{}", s.compile, s.execute);
            }
            let (c, e, t) = (stats(&mut compile), stats(&mut execute), stats(&mut total));
            println!(
                "| {name} | {} | {case} | {} / {} | {} / {} | {} / {} | {} |",
                source.len(),
                ns(c.p50),
                ns(c.p99),
                ns(e.p50),
                ns(e.p99),
                ns(t.p50),
                ns(t.p99),
                ns(t.min),
            );
        }
        let creation = stats(&mut creation);
        let _ = writeln!(
            footprints,
            "| {name} | {} | {} | {} | {} | {allocations} ({bytes} B) | {fresh_allocations} | {retained} B | {} / {} |",
            compiled.function_count(),
            compiled.code_bytes(),
            compiled.metadata_bytes(),
            compiled.max_frame_slots(),
            ns(creation.p50),
            ns(creation.p99),
        );
    }
    println!(
        "\n| workload | functions | code bytes | metadata bytes | max frame slots | allocations per reused run | allocations per fresh run (incl. creation) | context retained after warm-up | context creation p50 / p99 |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    print!("{footprints}");

    let out = root.join("../../target/bench-results");
    std::fs::create_dir_all(&out).expect("results directory");
    let out = out.canonicalize().expect("results directory");
    let path = out.join("source_to_result.csv");
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(csv.as_bytes()))
        .expect("write raw samples");
    println!("\nraw samples: {}", path.display());
}
