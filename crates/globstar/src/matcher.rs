use crate::DirMatch;

/// The matcher surface a filesystem walker consumes — implemented by
/// [`Glob`](crate::Glob). Object-safe, so walkers can hold
/// `Box<dyn Matcher>` when they mix matcher sources.
pub trait Matcher {
    /// Whole-path match: is `path` in the pattern's language?
    fn is_match(&self, path: &[u8]) -> bool;

    /// Directory-level verdict for pruning (see [`DirMatch`]).
    fn match_dir(&self, dir_path: &[u8]) -> DirMatch;

    /// Literal path prefixes a walker can seed traversal from. Every
    /// matching path starts with one of the returned byte strings.
    fn static_prefixes(&self) -> Vec<Vec<u8>>;
}
