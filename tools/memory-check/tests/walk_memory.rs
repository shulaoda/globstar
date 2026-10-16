//! What a walk holds while it runs: counted by this binary's global
//! allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use globstar_walk::Walk;

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
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
static ALLOC: Counting = Counting;

const MB: usize = 1 << 20;

// A folder's entries stream out as they are read; only its child
// folders wait. Buffering 20,000 entries took a few MB before the first
// one came out.
#[test]
fn a_walk_holds_folders_not_entries() {
    let root = std::env::temp_dir().join(format!("globstar-walk-memory-{}", std::process::id()));
    let dir = root.join("w");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..20_000 {
        std::fs::File::create(dir.join(format!("file-{i:05}.txt"))).unwrap();
    }
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let mut walk = Walk::new("w/*", root.as_path()).unwrap();
    let first = walk.next().unwrap().unwrap();
    let held = PEAK.load(Relaxed) - base;
    let rest = walk.count();
    std::fs::remove_dir_all(&root).unwrap();
    assert!(first.path().starts_with(&dir));
    assert_eq!(rest, 19_999);
    assert!(held < MB / 4, "{held} bytes held before the first entry");
}
