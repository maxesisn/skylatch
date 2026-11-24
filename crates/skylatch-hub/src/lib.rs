//! Skylatch central relay hub.
//!
//! Hosts keep one outbound WebSocket to `/ws/host`. Clients authenticate on `/ws/client`, pick a
//! device and receive its `NSL1` stream relayed through the hub. Slow clients lose frames, never
//! the connection: see [`gate::FrameGate`].

pub mod config;
pub mod gate;
pub mod registry;
pub mod server;

pub use config::HubConfig;
pub use registry::Registry;
pub use server::{router, AppState};
