//! Repeated compile/run/dispose (T012): after warm-up, a reused context
//! retains exactly the same heap however many more runs it does, and a
//! context that is created, used, and dropped leaves nothing behind.
//!
//! The corpus is every fixture (successes, faults, and rejections) plus
//! the benchmark workloads, under both interpreters. Live bytes come from a
//! counting allocator installed in this test binary only, which holds one
//! test so no other test allocates concurrently.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering::Relaxed};

use eitri::{Context, Host};

/// Counts live bytes. A measurement tool for this binary, not part of Eitri.
struct Counting;

static LIVE: AtomicI64 = AtomicI64::new(0);

// SAFETY: forwards every call unchanged to `System`, which upholds the
// `GlobalAlloc` contract; the counter has no effect on allocation.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
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
        LIVE.fetch_add(new_size as i64 - layout.size() as i64, Relaxed);
        // SAFETY: `ptr` came from `System` with `layout`; forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The harness `test` module's declarations, as the fixtures import it.
const TEST_MODULE: &str = "\
pub fn emit(value: i64) -> void
pub fn check(value: bool) -> bool
pub fn id(value: i64) -> i64
pub fn fail() -> void
";

/// Accepts every call and returns its first argument, if any, so results
/// stay in range for `bool`. Allocates nothing.
struct Echo;

impl Host for Echo {
    fn call(&mut self, _: u32, args: &[u64], results: &mut [u64]) -> Result<(), String> {
        if let (Some(result), Some(&arg)) = (results.first_mut(), args.first()) {
            *result = arg;
        }
        Ok(())
    }
}

fn collect(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, sources);
        } else if path.extension().is_some_and(|e| e == "eit") {
            sources.push(path);
        }
    }
}

fn context(threaded: bool) -> Context {
    let mut context = Context::new();
    context.set_threaded(threaded);
    // Small enough that non-terminating fixtures stop quickly.
    context.set_budget(10_000);
    context
        .register_host_module("test", TEST_MODULE)
        .expect("the harness module declares cleanly");
    context
}

/// One pass over the corpus: compile, execute where it compiled, drop.
/// Returns how many sources compiled, so the pass cannot silently be empty.
fn pass(context: &mut Context, corpus: &[String]) -> usize {
    let mut compiled = 0;
    for source in corpus {
        if let Ok(code) = context.compile(source) {
            compiled += 1;
            // Faults are expected for some fixtures; only memory matters here.
            let _ = context.execute(&code, &mut Echo);
        }
    }
    compiled
}

#[test]
fn repeated_runs_retain_no_growing_memory() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    collect(&root.join("tests/fixtures"), &mut paths);
    collect(&root.join("benches/fixtures"), &mut paths);
    paths.sort();
    let corpus: Vec<String> = paths
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect();
    assert!(corpus.len() > 100, "the corpus was found");

    let mut retained = Vec::new();
    for threaded in [false, true] {
        // A fresh context leaves nothing behind.
        let baseline = LIVE.load(Relaxed);
        for _ in 0..3 {
            let mut fresh = context(threaded);
            assert!(pass(&mut fresh, &corpus) > 30);
            drop(fresh);
            assert_eq!(LIVE.load(Relaxed), baseline, "a dropped context leaked");
        }

        // A reused context reaches its high-water mark, then stays there.
        let mut reused = context(threaded);
        pass(&mut reused, &corpus);
        pass(&mut reused, &corpus);
        let warm = LIVE.load(Relaxed);
        for round in 0..20 {
            pass(&mut reused, &corpus);
            let live = LIVE.load(Relaxed);
            assert_eq!(
                live,
                warm,
                "round {round}: retained {} bytes more than after warm-up",
                live - warm
            );
        }
        drop(reused);
        assert_eq!(LIVE.load(Relaxed), baseline, "a dropped context leaked");
        // Printed only after the checks: stderr's first use allocates.
        retained.push(warm - baseline);
    }
    eprintln!(
        "{} sources; a warm context retains {} bytes (match), {} bytes (threaded)",
        corpus.len(),
        retained[0],
        retained[1]
    );
}
