//! The lend spike: see `../SPIKE.md`. Forgeable test keys, local network only.
#[cfg(all(not(feature = "build-circuits"), not(feature = "insecure-test-setup")))]
compile_error!("the spike verifying keys require the insecure-test-setup feature");

#[cfg(any(feature = "circuits", feature = "wasm"))]
pub mod circuits;

#[cfg(all(not(feature = "build-circuits"), not(xark)))]
pub mod instructions;
#[cfg(all(not(feature = "build-circuits"), not(xark)))]
#[rustfmt::skip]
pub mod verifier;

pub mod tag {
    pub const INIT: u8 = 1;
    pub const ACCRUE: u8 = 2;
    pub const FUND: u8 = 3;
    pub const TOUCH: u8 = 4;
    pub const SET_PRICE: u8 = 5;
}

pub const POOL_SEED: &[u8] = b"pool";
pub const USER_SEED: &[u8] = b"user";

/// Price account layout: price u64, confidence u64, publish time i64, LE.
pub const PRICE_ACCOUNT_LEN: usize = 24;
pub const PRICE_MAX_AGE: i64 = 90;
pub const NOW_WINDOW: i64 = 120;

#[cfg(all(feature = "bpf-entrypoint", not(feature = "build-circuits"), not(xark)))]
mod entrypoint {
    pinocchio::entrypoint!(crate::instructions::process_instruction);
}

pinocchio::address::declare_id!("2qQ21JVMHLNj79rEMXZ45bkmyGw9V2YeD6z9hG1Z6MAx");
