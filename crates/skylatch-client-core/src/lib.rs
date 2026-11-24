//! Shared core for Skylatch native clients.
//!
//! Native shells (Win32, Swift, GTK) own windowing, decoding and rendering. This crate owns
//! everything protocol-shaped: connection configuration, the `NSL1` stream parser and a small
//! C ABI ([`ffi`]).

pub mod config;
pub mod ffi;
pub mod parser;

pub use config::{ConfigError, ConnectionConfig};
pub use parser::{FrameParser, ParseError};
pub use skylatch_proto::nsl1::{Header, Message};
