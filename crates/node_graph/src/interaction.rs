use crate::graph::{NodeId, SocketId};
use egui::{Pos2, Vec2};

#[derive(Default)]
pub enum InteractionState {
    #[default]
    Idle,
    DraggingNode {
        node_id: NodeId,
        offset: Vec2,
    },
    DraggingWire {
        from: SocketId,
        from_canvas: Pos2,
        current_canvas: Pos2,
    },
    Panning {
        last_screen: Pos2,
    },
    BoxSelecting {
        start_canvas: Pos2,
        current_canvas: Pos2,
    },
    /// Knife: Ctrl+right-drag draws a freeform path; wires it crosses are cut on release.
    CuttingWire {
        path: Vec<Pos2>, // canvas coordinates
    },
}

