//! The verifier's memory (found by an audit): it kept a full frame state
//! per instruction, so 1,000 instructions in a function naming `%r65535`
//! peaked at about 65 MB. It keeps one per basic block now. Peak live bytes
//! come from a counting allocator installed in this test binary only.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write;
use std::sync::atomic::{AtomicI64, Ordering::Relaxed};

struct Peak;

static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

fn grew(by: i64) {
    let live = LIVE.fetch_add(by, Relaxed) + by;
    PEAK.fetch_max(live, Relaxed);
}

// SAFETY: forwards every call unchanged to `System`, which upholds the
// `GlobalAlloc` contract; the counters have no effect on allocation.
unsafe impl GlobalAlloc for Peak {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        grew(layout.size() as i64);
        // SAFETY: same layout contract as our caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as i64, Relaxed);
        // SAFETY: `ptr` came from `System.alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        grew(new_size as i64 - layout.size() as i64);
        // SAFETY: `ptr` came from `System` with `layout`; forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Peak = Peak;

#[test]
fn verifying_a_wide_frame_does_not_keep_a_state_per_instruction() {
    let mut text = "define void @<module>() frame(65536) {\n    %r65535 = ldi i64 7\n".to_owned();
    for i in 0..1000 {
        writeln!(text, "    %r{} = mov %r65535", i % 16).unwrap();
    }
    text += "    ret\n}\n";
    let mut context = eitri::Context::new();
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let compiled = context.assemble(&text).expect("valid and verified");
    let peak = PEAK.load(Relaxed) - before;
    assert_eq!(compiled.function_count(), 1);
    // One state per block: a few frames of 65,536 one-byte slots, not one
    // per instruction (about 65 MB before the fix).
    assert!(
        peak < 4 << 20,
        "peak {peak} bytes while assembling and verifying"
    );
}
