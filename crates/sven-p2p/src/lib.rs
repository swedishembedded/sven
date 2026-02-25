pub mod config;
pub mod discovery;
pub mod error;
pub mod log_layer;
pub mod node;
pub mod protocol;
pub mod relay;

pub mod behaviour;
pub mod transport;

pub use config::{P2pConfig, RelayConfig};
pub use error::P2pError;
pub use node::{P2pEvent, P2pHandle, P2pNode, RoomState};
pub use protocol::types::{
    AgentCard, ContentBlock, LogEntry, P2pRequest, P2pResponse, TaskRequest, TaskResponse,
    TaskStatus,
};
