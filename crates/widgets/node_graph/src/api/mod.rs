mod builtins;
mod control;
mod node;
mod socket;
std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "file_dialog_web.rs"]
        mod file_dialog;
    }
    _ => {
        #[path = "file_dialog_native.rs"]
        mod file_dialog;
    }
}

pub use builtins::{
    AnySocket, BoolSocket, BoolValue, EnumValue, FileSocket, FileValue, FloatSocket, FloatValue,
    IntSocket, IntValue, StrSocket, StringValue,
};
pub use control::InlineControl;
pub use node::{InputDef, NodeDef, OutputDef, PanelSection, PropDef, SocketTypeIdentity};
pub use socket::{SocketDef, SocketWithControlDef};
