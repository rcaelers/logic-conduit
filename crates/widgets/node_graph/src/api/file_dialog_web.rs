use super::builtins::FileFilter;

pub(super) const AVAILABLE: bool = false;

pub(super) fn pick(_title: &str, _filters: &[FileFilter], _save: bool) -> Option<String> {
    None
}
