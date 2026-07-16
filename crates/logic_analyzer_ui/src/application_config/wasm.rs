use std::path::PathBuf;

use super::ApplicationConfig;

pub(crate) fn load() -> ApplicationConfig {
    super::embedded_defaults()
}

pub(crate) fn path() -> Option<PathBuf> {
    None
}
