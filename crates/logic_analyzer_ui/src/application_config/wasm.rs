use super::implementation::{ApplicationConfig, embedded_defaults};

pub(crate) fn load() -> ApplicationConfig {
    embedded_defaults()
}
