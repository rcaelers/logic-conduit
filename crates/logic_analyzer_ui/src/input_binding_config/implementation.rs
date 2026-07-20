use input_bindings::InputBindings;

pub(crate) fn embedded_defaults() -> InputBindings {
    InputBindings::from_json(include_str!("../../config/input_bindings.json"))
        .expect("embedded application input bindings must be valid")
}
