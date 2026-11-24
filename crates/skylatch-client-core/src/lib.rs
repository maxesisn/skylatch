//! Shared core for Skylatch native clients.
//!
//! Native shells (Win32, Swift, GTK) own windowing, video decoding, rendering and audio output.
//! This crate owns the hub and direct transports, `NSL1` parsing, Opus audio decoding, and the
//! C ABI ([`ffi`]).

mod audio;
pub mod config;
pub mod ffi;
mod http;
pub mod parser;
mod session;
mod transport;

pub use audio::AudioPcm;
pub use config::{ConfigError, ConnectionConfig};
pub use parser::{FrameParser, ParseError};
pub use session::{ClientError, Event, Frame, SendError, SessionClient, TransportMode};
pub use skylatch_proto::{
    control::DeviceInfo,
    nsl1::{Header, InputStatus, Message, MonitorInfo},
};
pub use transport::list_devices;

pub use http::{create_pairing_code, enroll, get_layout, login, put_layout};
pub use skylatch_proto::layout::{
    DevicePlacement, Facing, FurnitureItem, FurnitureKind, RoomLayout, WorkspaceLayout,
};
pub use skylatch_proto::onboarding::{LoginResponse, PairingCodeResponse};
