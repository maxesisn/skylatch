//! Skylatch host agent: registers outbound with `skylatch-hub` and bridges `NSL1` sessions to the
//! local host engine.

pub mod cli;
pub mod engine;
mod engine_win_session;
pub mod hardware;
pub mod hub;

mod direct;
pub mod join;
pub mod service;
