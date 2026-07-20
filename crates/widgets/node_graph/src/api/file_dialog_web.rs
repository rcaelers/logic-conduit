use super::builtins::FileFilter;

pub(crate) const AVAILABLE: bool = false;

pub(crate) fn pick(_title: &str, _filters: &[FileFilter], _save: bool) -> Option<String> {
    None
}
