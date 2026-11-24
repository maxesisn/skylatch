//! Skylatch wire protocol.
//!
//! * [`nsl1`] — the binary data plane (frames, ACKs, input, cursor, clipboard notices).
//! * [`control`] — the JSON control plane between hosts, clients and the hub.

pub mod control;
pub mod nsl1;

/// Crate version, exposed for the C ABI and diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
