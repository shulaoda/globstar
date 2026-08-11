#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CompileOptions {
    /// When `false`, wildcards (`*` `?` `[^x]`) cannot consume a leading
    /// `.` — dotfile protection. `Glob` defaults to `true`, `Walk` to
    /// `false` (GLOB_SPEC.md §11.1 / §12.4).
    pub dot: bool,

    /// ASCII-only case folding, `false` by default; non-ASCII bytes
    /// compare verbatim (GLOB_SPEC.md §11.2 / §12.5).
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
