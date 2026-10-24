//! A counting global allocator: the heap bytes live, and the most that
//! were live at once. The memory binaries and tests include this file.
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
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
    // The change in size; a shrink wraps, which subtracts.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let grow = new_size.wrapping_sub(layout.size());
        let live = LIVE.fetch_add(grow, Relaxed).wrapping_add(grow);
        PEAK.fetch_max(live, Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static A: Counter = Counter;

/// What a matcher keeps once it has matched each of `paths`, and the most
/// it held at once from the start of its build through those matches: its
/// own size and the heap, each the median of `trials` builds.
pub fn kept_and_peak<T>(
    trials: usize,
    paths: &[&str],
    build: impl Fn() -> T,
    is_match: impl Fn(&T, &str) -> bool,
) -> (usize, usize) {
    let (mut kept, mut peak): (Vec<usize>, Vec<usize>) = (0..trials)
        .map(|_| {
            let before = LIVE.load(Relaxed);
            PEAK.store(before, Relaxed);
            let m = build();
            for path in paths {
                black_box(is_match(&m, path));
            }
            let size = size_of_val(&m);
            (
                size + LIVE.load(Relaxed).saturating_sub(before),
                size + PEAK.load(Relaxed) - before,
            )
        })
        .unzip();
    kept.sort_unstable();
    peak.sort_unstable();
    (kept[trials / 2], peak[trials / 2])
}
