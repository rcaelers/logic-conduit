#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

unsafe extern "C" {
    fn __wasm_call_ctors();
}

fn initialize_compile_time_inventories() {
    static INITIALIZE: std::sync::Once = std::sync::Once::new();
    #[cfg(feature = "example-plugin")]
    std::hint::black_box(example_plugin::force_link());
    INITIALIZE.call_once(|| {
        // SAFETY: the linker synthesizes this function for the current WASM
        // module. `Once` guarantees constructors run before the first
        // inventory read and are not repeated by later JS calls.
        unsafe { __wasm_call_ctors() };
    });
}

#[derive(Clone)]
#[wasm_bindgen]
pub struct WebHandle {
    runner: eframe::WebRunner,
}

#[wasm_bindgen]
impl WebHandle {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        initialize_compile_time_inventories();
        eframe::WebLogger::init(log::LevelFilter::Debug).ok();
        Self {
            runner: eframe::WebRunner::new(),
        }
    }

    #[wasm_bindgen]
    pub async fn start(&self, canvas: web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
        self.runner
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| {
                    let graph: node_graph::GraphState =
                        serde_json::from_str(include_str!("../data/wasm_decoder_demo.json"))
                            .expect("web application demo graph is valid");
                    Ok(Box::new(logic_analyzer_ui::App::new_with_graph(cc, graph)))
                }),
            )
            .await
    }

    #[wasm_bindgen]
    pub fn destroy(&self) {
        self.runner.destroy();
    }
}

impl Default for WebHandle {
    fn default() -> Self {
        Self::new()
    }
}
