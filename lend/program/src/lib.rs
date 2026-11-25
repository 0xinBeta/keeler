//! Keeler Lend: a pool with private amounts (`docs/plan-lend.md`).
// Exactly one of `insecure-test-setup` (local networks and devnet) and
// `mainnet`. A mainnet build also needs verifying keys from a real setup
// (the committed ones refuse to compile without insecure-test-setup) and
// every build parameter set (build.rs).
#[cfg(all(
    not(feature = "build-circuits"),
    not(feature = "insecure-test-setup"),
    not(feature = "mainnet")
))]
compile_error!("enable exactly one of the insecure-test-setup and mainnet features");
#[cfg(all(feature = "insecure-test-setup", feature = "mainnet"))]
compile_error!("enable exactly one of the insecure-test-setup and mainnet features");

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
    /// The tUSD pool's mint, the only one with a reserve: vault 1's debt mint.
    pub const TUSD_MINT: solana_address::Address = crate::VAULT1_DEBT_MINT;
}

/// Vault ids (`PositionState::vault`).
pub mod vault {
    pub const TSOL_TUSD: u64 = 1;
}

/// How far the public `now` may trail the cluster clock, in seconds.
pub const NOW_WINDOW: i64 = 120;

// `ID`, `ADMIN`, `KEEPERS`, `POOL_VIEWING` and the vault's mints and oracle,
// from the build environment (see build_params.rs).
include!(concat!(env!("OUT_DIR"), "/ids.rs"));

#[cfg(test)]
#[path = "../build_params.rs"]
mod build_params;

#[cfg(all(feature = "bpf-entrypoint", not(feature = "build-circuits"), not(xark)))]
mod entrypoint {
    pinocchio::entrypoint!(crate::instructions::process_instruction);
}
