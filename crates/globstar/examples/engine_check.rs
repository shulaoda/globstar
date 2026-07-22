//! Diagnostic: which production engine each pattern routes to.
//!
//! ```sh
//! cargo run --release -p globstar --example engine_check
//! ```

use globstar::Glob;

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
    println!("{:40} {:>12}", "pattern", "engine");
    for p in pats {
        let g = Glob::new(p).unwrap();
        println!("{:40} {:>12}", p, g.engine_name());
    }
    let pats = ["src/**/*.ts", "tests/**/*.ts", "lib/**/*.js"];
    let g = Glob::union(pats).unwrap();
    println!("{:40} {:>12}", "union(mixed-roots)", g.engine_name());
}
