//! Shared core for Skylatch native clients.
//!
//! Native shells (Win32, Swift, GTK) own windowing, video decoding, rendering and audio output.
//! This crate owns the hub and direct transports, `NSL1` parsing, Opus audio decoding, and the
//! C ABI ([`ffi`]).

mod audio;
pub mod config;
pub mod ffi;
mod http;
pub mod mesh_vault;
pub mod parser;
mod session;
mod transport;
pub mod updater;

pub use audio::AudioPcm;
pub use config::{ConfigError, ConnectionConfig};
pub use parser::{FrameParser, ParseError};
pub use session::{ClientError, Event, Frame, SendError, SessionClient, TransportMode};
pub use skylatch_proto::{
    control::DeviceInfo,
    nsl1::{Header, InputStatus, Message, MonitorInfo},
};
pub use transport::list_devices;

pub use http::{
    create_pairing_code, create_pairing_code_with_invite, delete_device, delete_mesh_vault, enroll,
    get_layout, get_mesh_vault, hub_info, login, put_layout, put_mesh_vault, signup, VaultPutError,
};
pub use skylatch_proto::layout::{
    DevicePlacement, Facing, FurnitureItem, FurnitureKind, RoomLayout, WorkspaceLayout,
};
pub use skylatch_proto::mesh::{
    HubInfo, MeshError, MeshInviteCiphertext, MeshProfile, MeshVault, MeshVaultEnvelope,
};
pub use skylatch_proto::onboarding::{EnrollResponse, LoginResponse, PairingCodeResponse};
