use super::SocketId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub from: SocketId,
    pub to: SocketId,
}
