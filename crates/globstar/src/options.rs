//! Compile-time options for [`crate::Glob`].

/// Options that affect how a glob pattern is compiled. Deliberately
/// minimal: brace and globstar syntax are always on, so only these two
/// semantic switches exist.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CompileOptions {
    /// When `true` (the `Glob` default), wildcards (`*` `?` `[^x]`) may
    /// consume a leading `.` at a segment boundary; when `false` they
    /// cannot — Bash-style dotfile protection. `Walk` flips the default
    /// to `false`. See GLOB_SPEC.md §11.1 / §12.4.
    pub dot: bool,

    /// When `true`, ASCII letters match regardless of case (`false` by
    /// default). ASCII only — non-ASCII bytes compare verbatim, so callers
    /// needing Unicode folding must normalize pattern and path first.
    /// See GLOB_SPEC.md §11.3 / §12.5.
    pub case_insensitive: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            dot: true,
            case_insensitive: false,
        }
    }
}

impl CompileOptions {
    pub fn dot(mut self, v: bool) -> Self {
        self.dot = v;
        self
    }

    pub fn case_insensitive(mut self, v: bool) -> Self {
        self.case_insensitive = v;
        self
    }
}

/// The ASCII case-flip of a byte (`A`↔`a`), or the byte unchanged if it
/// isn't an ASCII letter. Bit `0x20` is ASCII's case bit: setting it
/// lowercases, clearing it uppercases.
#[inline]
pub fn ascii_case_alt(b: u8) -> u8 {
    match b {
        b'A'..=b'Z' => b | 0x20,
        b'a'..=b'z' => b & !0x20,
        _ => b,
    }
}
