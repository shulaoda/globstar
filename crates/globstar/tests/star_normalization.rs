use globstar::{CompileOptions, Glob};

#[test]
fn adjacent_segment_stars_are_semantically_idempotent() {
    let pairs = [("a**b", "a*b"), ("***a", "*a"), ("a***", "a*")];
    let paths: &[&[u8]] = &[b"", b"ab", b"axb", b"a/b", b".a", b"a.b", b"aaab"];
    for dot in [false, true] {
        let options = CompileOptions::default().dot(dot);
        for (redundant, canonical) in pairs {
            let left = Glob::new_with(redundant, options).unwrap();
            let right = Glob::new_with(canonical, options).unwrap();
            for path in paths {
                assert_eq!(
                    left.is_match(path),
                    right.is_match(path),
                    "dot={dot} {redundant:?} vs {canonical:?} on {path:?}"
                );
            }
        }
    }
}
