//! The vaults' fixed parameters (`docs/plan-lend.md`, Vault), their error
//! codes and the guard on the test price account. Owned by the Vault piece
//! (`lend/AGENTS.md`).
//!
//! Build parameters (base58 unless noted, defaults for local tests, see
//! `build.rs`): `KEELER_LEND_VAULT1_COLLATERAL_MINT` (default: SOL, the zero
//! address), `KEELER_LEND_VAULT1_DEBT_MINT` (default: the localnet fixture's
//! SPL mint), `KEELER_LEND_VAULT1_PRICE_ACCOUNT` (default: Pyth's SOL/USD
//! price update account) and `KEELER_LEND_VAULT1_FEED_ID` (64 hex, default:
//! Pyth's SOL/USD feed).
use solana_address::Address;
use solana_program_error::ProgramError;

// The test price account is for local networks and devnet: a build without
// the insecure test keys (every real deployment) or marked `mainnet` cannot
// have it.
#[cfg(all(feature = "test-price", not(feature = "insecure-test-setup")))]
compile_error!("the test-price feature lets the keeper set the oracle price: enable it only with insecure-test-setup, for local networks and devnet");
#[cfg(all(feature = "test-price", feature = "mainnet"))]
compile_error!(
    "the test-price feature lets the keeper set the oracle price: never in a mainnet build"
);

/// A vault: one collateral mint, one debt mint (whose reserve it borrows
/// from) and the oracle that prices the collateral in the debt token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vault {
    pub id: u64,
    pub collateral_mint: Address,
    pub debt_mint: Address,
    pub collateral_decimals: u8,
    pub debt_decimals: u8,
    /// The Pyth `PriceUpdateV2` account and the feed it must hold: the
    /// collateral's USD price (the debt token is valued at exactly 1 USD).
    pub price_account: Address,
    pub feed_id: [u8; 32],
    /// The smallest collateral a position may hold, unless it holds none.
    pub min_collateral: u64,
    /// The smallest debt a draw may leave, unless it leaves none.
    pub min_debt: u64,
}

/// tSOL (9 decimals) against tUSD (6 decimals).
pub const TSOL_TUSD: Vault = Vault {
    id: crate::vault::TSOL_TUSD,
    collateral_mint: crate::VAULT1_COLLATERAL_MINT,
    debt_mint: crate::VAULT1_DEBT_MINT,
    collateral_decimals: 9,
    debt_decimals: 6,
    price_account: crate::VAULT1_PRICE_ACCOUNT,
    feed_id: crate::VAULT1_FEED_ID,
    min_collateral: TSOL_TUSD_MIN_COLLATERAL,
    min_debt: TSOL_TUSD_MIN_DEBT,
};

/// 0.01 tSOL.
pub const TSOL_TUSD_MIN_COLLATERAL: u64 = 10_000_000;
/// 1 tUSD.
pub const TSOL_TUSD_MIN_DEBT: u64 = 1_000_000;

/// The vault with `id`.
pub fn by_id(id: u64) -> Option<&'static Vault> {
    match id {
        crate::vault::TSOL_TUSD => Some(&TSOL_TUSD),
        _ => None,
    }
}

/// Seed of the test price account `[TEST_PRICE_SEED, vault id (u64 LE)]`.
pub const TEST_PRICE_SEED: &[u8] = b"price";
/// The test price account: `price u64, confidence u64, publish time i64`,
/// little-endian, the price already in debt base units per collateral base
/// unit x 1e12.
pub const TEST_PRICE_LEN: usize = 24;
/// The oldest price `draw` and `liquidate` accept, in seconds.
pub const PRICE_MAX_AGE: i64 = 90;

/// Pyth's Solana receiver program, which owns every `PriceUpdateV2` account.
pub const PYTH_RECEIVER: Address =
    Address::from_str_const("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");

/// Vault error codes (`crate::error` ranges 20600-21099).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum VaultError {
    // 20600: deposit.
    /// The intent's expiry has passed.
    IntentExpired = 20_600,
    /// No vault has this id.
    UnknownVault = 20_601,
    /// The transact has no input.
    NoInputs = 20_602,

    // 20700: draw, and the price check it shares with liquidate.
    /// The price account is neither the vault's Pyth account nor (with
    /// `test-price`) its test price account.
    WrongPriceAccount = 20_700,
    /// The Pyth account is not owned by the Pyth receiver.
    WrongPriceOwner = 20_701,
    /// Not a `PriceUpdateV2`, or too short.
    NotPriceUpdate = 20_702,
    /// The update is not fully verified.
    PriceNotVerified = 20_703,
    /// The update holds another feed.
    WrongFeed = 20_704,
    /// Published more than `PRICE_MAX_AGE` from the clock.
    StalePrice = 20_705,
    /// A price at or below zero, or a confidence at or above the price.
    BadPrice = 20_706,
    /// The exponent or the scaled price does not fit.
    PriceOverflow = 20_707,
    /// `draw`: the bound is above `price - confidence`.
    BoundAbovePrice = 20_708,

    // 20750: set_test_price.
    /// The account is not the vault's test price PDA.
    WrongTestPriceAccount = 20_750,
    /// The test price account is unavailable in this build.
    TestPriceDisabled = 20_751,

    // 20900: liquidate.
    /// The bound is below `price + confidence`.
    BoundBelowPrice = 20_900,
}

impl From<VaultError> for ProgramError {
    fn from(error: VaultError) -> Self {
        ProgramError::Custom(error as u32)
    }
}
