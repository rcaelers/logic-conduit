use input_bindings::InputBindings;

use std::path::PathBuf;

pub(crate) fn load() -> InputBindings {
    super::embedded_defaults()
}

pub(crate) fn path() -> Option<PathBuf> {
    None
}
