use input_bindings::InputBindings;

use super::implementation::embedded_defaults;

pub(crate) fn load() -> InputBindings {
    embedded_defaults()
}
