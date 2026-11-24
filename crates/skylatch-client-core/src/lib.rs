//! Shared core for Skylatch native clients.
//!
//! Native shells (Win32, Swift, GTK) own windowing, decoding and rendering. This crate owns
//! the hub and direct transports, `NSL1` parsing, and the C ABI ([`ffi`]).

pub mod config;
pub mod ffi;
pub mod parser;
mod session;
mod transport;

pub use config::{ConfigError, ConnectionConfig};
pub use parser::{FrameParser, ParseError};
pub use session::{ClientError, Event, Frame, SendError, SessionClient, TransportMode};
pub use skylatch_proto::{
    control::DeviceInfo,
    nsl1::{Header, Message},
};
pub use transport::list_devices;
