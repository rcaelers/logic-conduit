#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

#[derive(Clone)]
#[wasm_bindgen]
pub struct WebHandle {
    runner: eframe::WebRunner,
}

#[wasm_bindgen]
impl WebHandle {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
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
                    let mut app = logic_analyzer_ui::App::new_with_graph(cc, graph);
                    app.ensure_decoder_panel_count(2);
                    Ok(Box::new(app))
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
