//! Compile-time options for [`crate::Glob`]. See ADR-006.

/// Options that affect how a glob pattern is compiled.
///
/// Per ADR-006 / D-006, the semantic options are minimal by design.
/// Syntax constructs (brace, globstar) are always enabled.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CompileOptions {
    /// If `true` (default at the `Glob` layer), wildcards `*` / `?` / `[^x]`
    /// treat leading `.` at segment boundaries as an ordinary byte.
    ///
    /// If `false`, those wildcards cannot consume a leading `.` at a
    /// path-segment boundary (Bash-style dotfile protection).
    ///
    /// Note: `Walk` overrides this default to `false` — filesystem
    /// traversal contexts usually expect hidden files to be skipped.
    /// See GLOB_SPEC.md §11.1 / §12.4.
    pub dot: bool,
    /// If `true`, ASCII letters (`A-Z` / `a-z`) match regardless of case.
    /// Default `false`.
    ///
    /// **Scope**: ASCII only — non-ASCII bytes (UTF-8 multi-byte sequences,
    /// Latin-1 etc.) are compared verbatim. Users who need Unicode case
    /// folding should normalize both pattern and path to a canonical form
    /// before calling. See GLOB_SPEC.md §11.3 / §12.5.
    ///
    /// **Performance**: applied at compile time where possible (class items
    /// are expanded once at lowering); literal byte comparisons use
    /// `eq_ignore_ascii_case` at match time.
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

/// Return the ASCII case-alternate of a byte, or the byte itself if it is
/// not an ASCII letter. Shared by class-item expansion and runtime
/// literal compares.
///
/// Bit 5 (`0x20`) is the case-toggle bit in ASCII: `A`(0x41) ↔ `a`(0x61),
/// `Z`(0x5A) ↔ `z`(0x7A). Setting it lowercases an upper letter; clearing
/// it uppercases a lower letter.
#[inline]
pub fn ascii_case_alt(b: u8) -> u8 {
    match b {
        b'A'..=b'Z' => b | 0x20,
        b'a'..=b'z' => b & !0x20,
        _ => b,
    }
}
