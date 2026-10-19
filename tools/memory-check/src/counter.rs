//! A counting global allocator: the net heap bytes a build leaves behind.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

struct Counter;

unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        ALLOCATED.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: Counter = Counter;

/// `f`'s value and the net heap bytes it allocated.
pub fn measure_heap<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = ALLOCATED.load(Ordering::SeqCst);
    let val = f();
    let after = ALLOCATED.load(Ordering::SeqCst);
    (val, (after as isize - before as isize).max(0) as usize)
}

/// The median of `n` samples of `sample`, which dampens the allocator's
/// noise around a boundary allocation.
pub fn median_of(n: usize, mut sample: impl FnMut() -> usize) -> usize {
    let mut samples: Vec<usize> = (0..n).map(|_| sample()).collect();
    samples.sort_unstable();
    samples[samples.len() / 2]
}
