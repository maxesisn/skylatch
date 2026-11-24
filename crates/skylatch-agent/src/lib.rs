//! Skylatch host agent: registers outbound with `skylatch-hub` and bridges `NSL1` sessions to the
//! local host engine.

pub mod cli;
pub mod engine;
pub mod hardware;
pub mod hub;
