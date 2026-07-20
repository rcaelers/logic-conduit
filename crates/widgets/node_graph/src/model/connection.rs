use serde::{Deserialize, Serialize};

use super::ids::SocketId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub from: SocketId,
    pub to: SocketId,
}
