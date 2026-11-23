//! Keeler: a private quote router over the Privacy Rings swap example.

/// `eprintln!` with any API key in the text redacted.
macro_rules! log {
    ($($t:tt)*) => { $crate::config::log(format_args!($($t)*)) };
}

pub mod chain;
pub mod config;
pub mod dealer;
pub mod deploy;
pub mod faucet;
pub mod trader;
