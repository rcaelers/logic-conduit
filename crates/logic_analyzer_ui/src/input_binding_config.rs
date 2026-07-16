use input_bindings::InputBindings;

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "input_binding_config/wasm.rs"]
        mod imp;
    }
    _ => {
        #[path = "input_binding_config/native.rs"]
        mod imp;
    }
}

pub(crate) use imp::{load, path};

fn embedded_defaults() -> InputBindings {
    InputBindings::from_json(include_str!("../config/input_bindings.json"))
        .expect("embedded application input bindings must be valid")
}
