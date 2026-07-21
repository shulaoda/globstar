//! Diagnostic: which engine each crate routes a pattern to.
//!
//! ```sh
//! cargo run --release -p globstar --example engine_check
//! ```

use globstar::Glob;
use globstar_segment::SegGlob;

fn main() {
    let pats = [
        "src/main.rs",
        "src/*.ts",
        "src/**/*.ts",
        "**/*.{ts,tsx,js,jsx}",
        "**/*.md",
        "src/**/n*d[k-m]e?txt",
        "src/**/{tob,crazy}/?*.{png,txt}",
        "{src/**,lib/*}",
        "a/{**,x}/b",
        "a{**,x}b",
        "a\\/b*",
    ];
    println!("{:40} {:>12} {:>12}", "pattern", "globstar", "segment");
    for p in pats {
        let g = Glob::new(p).unwrap();
        let s = SegGlob::new(p).unwrap();
        println!("{:40} {:>12} {:>12}", p, g.engine_name(), s.engine_name());
    }
    let pats = ["src/**/*.ts", "tests/**/*.ts", "lib/**/*.js"];
    let g = Glob::union(pats).unwrap();
    let s = SegGlob::union(pats).unwrap();
    println!(
        "{:40} {:>12} {:>12}",
        "union(mixed-roots)",
        g.engine_name(),
        s.engine_name()
    );
}
