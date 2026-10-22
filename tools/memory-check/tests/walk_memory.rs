//! What a walk holds while it runs: counted by this binary's global
//! allocator.

use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;

use globstar_walk::Walk;

#[allow(dead_code)]
#[path = "../src/counter.rs"]
mod counter;
use counter::{LIVE, PEAK};

const MB: usize = 1 << 20;

// One test, so no other test thread allocates while it counts.
#[test]
fn a_walk_holds_little() {
    let root = std::env::temp_dir().join(format!("globstar-walk-memory-{}", std::process::id()));
    folders_not_entries(&root.join("entries"));
    #[cfg(unix)]
    each_link_once(&root.join("links"));
    std::fs::remove_dir_all(&root).unwrap();
}

// A folder's entries stream out as they are read; only its child
// folders wait: with 20,000 entries the heap stays small before the
// first one comes out.
fn folders_not_entries(root: &Path) {
    let dir = root.join("w");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..20_000 {
        std::fs::File::create(dir.join(format!("file-{i:05}.txt"))).unwrap();
    }
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let mut walk = Walk::new("w/*", root).unwrap();
    let first = walk.next().unwrap().unwrap();
    let held = PEAK.load(Relaxed) - base;
    let rest = walk.count();
    assert!(first.path().starts_with(&dir));
    assert_eq!(rest, 19_999);
    assert!(held < MB / 4, "{held} bytes held before the first entry");
}

// A folder waiting to be read shares the links on its way down with the
// folder it was found in: 2,000 folders below a chain of 16 links hold
// the 16 links once, not once each.
#[cfg(unix)]
fn each_link_once(root: &Path) {
    for i in 0..=16 {
        std::fs::create_dir_all(root.join(format!("r{i}"))).unwrap();
    }
    for i in 0..16 {
        let link = root.join(format!("r{i}/next"));
        std::os::unix::fs::symlink(format!("../r{}", i + 1), link).unwrap();
    }
    for i in 0..2_000 {
        std::fs::create_dir(root.join(format!("r16/s{i}"))).unwrap();
    }
    let pattern = format!("r0/{}*/*.txt", "next/".repeat(16));
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let found = Walk::new(&pattern, root).unwrap().count();
    let peak = PEAK.load(Relaxed) - base;
    assert_eq!(found, 0);
    assert!(peak < MB, "{peak} bytes at the peak");
}
