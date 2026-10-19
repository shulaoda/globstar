//! A counting global allocator: the heap bytes live, and the most that
//! were live at once, for what a build leaves behind and what it peaks
//! at. The two memory binaries and the memory tests include this file.
use std::alloc::{GlobalAlloc, Layout, System};
use std::mem::size_of_val;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

pub static LIVE: AtomicUsize = AtomicUsize::new(0);
pub static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counter;

unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
        PEAK.fetch_max(live, Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: Counter = Counter;

/// What a matcher keeps once it has matched every bench path: its own
/// size and the heap it holds, as the median of `trials` builds (which
/// dampens the allocator's noise around a boundary allocation).
pub fn kept<T>(trials: usize, build: impl Fn() -> T, matches: impl Fn(&T)) -> usize {
    let mut samples: Vec<usize> = (0..trials)
        .map(|_| {
            let before = LIVE.load(Relaxed);
            let m = build();
            matches(&m);
            size_of_val(&m) + LIVE.load(Relaxed).saturating_sub(before)
        })
        .collect();
    samples.sort_unstable();
    samples[samples.len() / 2]
}
