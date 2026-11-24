//! Keeler Lend: a pool with private amounts (`docs/plan-lend.md`). See
//! `../AGENTS.md` for which files belong to which piece of work.
#[cfg(all(not(feature = "build-circuits"), not(feature = "insecure-test-setup")))]
compile_error!("the lend verifying keys require the insecure-test-setup feature");

#[cfg(any(feature = "circuits", feature = "wasm"))]
pub mod circuits;
/// Output hashes and blindings for clients: the SDK and the browser package.
#[cfg(all(
    any(feature = "circuits", feature = "wasm"),
    not(xark),
    not(xark_client)
))]
pub mod client;
#[cfg(all(feature = "wasm", not(xark), not(xark_client)))]
mod wasm;

pub mod error;
#[cfg(all(not(feature = "build-circuits"), not(xark)))]
pub mod instructions;
pub mod vaults;
#[cfg(all(not(feature = "build-circuits"), not(xark)))]
#[rustfmt::skip] // The generator's exact output.
#[allow(clippy::useless_conversion)]
pub mod verifier;

/// One tag per instruction; the order is the plan's table.
pub mod tag {
    pub const INIT_RESERVE: u8 = 1;
    pub const INTENT: u8 = 2;
    pub const CANCEL_INTENT: u8 = 3;
    pub const SUPPLY: u8 = 4;
    pub const REDEEM: u8 = 5;
    pub const DEPOSIT: u8 = 6;
    pub const DRAW: u8 = 7;
    pub const REPAY: u8 = 8;
    pub const LIQUIDATE: u8 = 9;
    pub const CLOSE: u8 = 10;
    /// `deposit` into an existing position (`DEPOSIT` opens one).
    pub const DEPOSIT_ADD: u8 = 11;
    /// Local networks and devnet (`test-price`): the keeper sets a vault's
    /// test price account.
    pub const SET_TEST_PRICE: u8 = 12;
}

/// The pool PDA `[POOL_SEED, mint]`: owns the mint's reserve.
pub const POOL_SEED: &[u8] = b"pool";
/// The user PDA `[USER_SEED, wallet]`: owns the wallet's intents, receipts
/// and positions.
pub const USER_SEED: &[u8] = b"user";

/// Pool ids (`ReceiptState::pool`, a `SUPPLY` intent's target).
pub mod pool {
    pub const TUSD: u64 = 1;
}

/// Vault ids (`PositionState::vault`).
pub mod vault {
    pub const TSOL_TUSD: u64 = 1;
}

/// How far the public `now` may trail the cluster clock, in seconds.
pub const NOW_WINDOW: i64 = 120;

// `ID`, `ADMIN` and `KEEPER`, from the build environment (see build.rs).
include!(concat!(env!("OUT_DIR"), "/ids.rs"));

#[cfg(all(feature = "bpf-entrypoint", not(feature = "build-circuits"), not(xark)))]
mod entrypoint {
    pinocchio::entrypoint!(crate::instructions::process_instruction);
}
