//! Skylatch wire protocol.
//!
//! * [`nsl1`] — the binary data plane (frames, ACKs, input, cursor, monitors, clipboard).
//! * [`control`] — the JSON control plane between hosts, clients and the hub.

pub mod control;
pub mod credentials;
pub mod layout;
pub mod nsl1;
pub mod onboarding;

/// Crate version, exposed for the C ABI and diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
