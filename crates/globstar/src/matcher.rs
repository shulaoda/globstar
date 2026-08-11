use crate::DirMatch;

pub trait Matcher {
    fn is_match(&self, path: &[u8]) -> bool;

    fn match_dir(&self, dir_path: &[u8]) -> DirMatch;

    fn static_prefixes(&self) -> Vec<Vec<u8>>;
}
