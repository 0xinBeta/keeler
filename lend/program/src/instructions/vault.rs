//! What the Vault instructions share: the vault by id, the published first
//! nullifier, the intent expiry, and the oracle check `draw` and `liquidate`
//! make before verifying their proof. Also `set_test_price`, the keeper's
//! price account for local networks and devnet (`test-price` feature).
use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    sysvars::{rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};
use spr_interface::instruction::instruction_data::transact::TransactIxDataRef;
use wincode::{SchemaRead, SchemaWrite};

use super::{clock, mint_halves, require_keeper};
use crate::{
    error::LendError,
    vaults::{
        self, Vault, VaultError, PRICE_MAX_AGE, PYTH_RECEIVER, TEST_PRICE_LEN, TEST_PRICE_SEED,
    },
};

/// The vault with `id`, or `UnknownVault`.
pub(crate) fn vault(id: u64) -> Result<&'static Vault, ProgramError> {
    vaults::by_id(id).ok_or_else(|| VaultError::UnknownVault.into())
}

/// The vault's mints as the circuits take them: `(debt_hi, debt_lo,
/// coll_hi, coll_lo)`.
pub(crate) fn mints(vault: &Vault) -> (u128, u128, u128, u128) {
    let (debt_hi, debt_lo) = mint_halves(vault.debt_mint.as_array());
    let (coll_hi, coll_lo) = mint_halves(vault.collateral_mint.as_array());
    (debt_hi, debt_lo, coll_hi, coll_lo)
}

/// The transact's first input nullifier, which the output blindings derive
/// from; each Vault circuit commits it as a public input.
pub(crate) fn first_nullifier(transact: &TransactIxDataRef<'_>) -> Result<[u8; 32], ProgramError> {
    Ok(transact
        .inputs
        .first()
        .ok_or(VaultError::NoInputs)?
        .nullifier_hash)
}

/// Refuses filling an intent after its expiry (the circuit checks `now`, the
/// program the clock, so a fill and a cancel never both pass).
pub(crate) fn check_expiry(expiry: u64) -> ProgramResult {
    let expiry = i64::try_from(expiry).unwrap_or(i64::MAX);
    if clock()? > expiry {
        return Err(VaultError::IntentExpired.into());
    }
    Ok(())
}

/// The oracle's collateral price in debt base units per collateral base unit
/// x 1e12: `low` is `price - confidence` rounded down, `high` is `price +
/// confidence` rounded up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Quote {
    pub low: u64,
    pub high: u64,
}

/// `draw`: requires `price - confidence >= bound`.
pub(crate) fn check_draw_bound(account: &AccountView, vault: &Vault, bound: u64) -> ProgramResult {
    if quote(account, vault)?.low < bound {
        return Err(VaultError::BoundAbovePrice.into());
    }
    Ok(())
}

/// `liquidate`: requires `price + confidence <= bound`.
pub(crate) fn check_liquidate_bound(
    account: &AccountView,
    vault: &Vault,
    bound: u64,
) -> ProgramResult {
    if quote(account, vault)?.high > bound {
        return Err(VaultError::BoundBelowPrice.into());
    }
    Ok(())
}

/// Reads the vault's price from `account`: its pinned Pyth account or, in a
/// `test-price` build, its test price account.
pub(crate) fn quote(account: &AccountView, vault: &Vault) -> Result<Quote, ProgramError> {
    if account.address() == &vault.price_account {
        return pyth_quote(account, vault);
    }
    #[cfg(feature = "test-price")]
    if account.address() == &test_price_address(vault.id).0 {
        return test_quote(account);
    }
    Err(VaultError::WrongPriceAccount.into())
}

/// Anchor's discriminator of `PriceUpdateV2`: `sha256("account:PriceUpdateV2")[..8]`.
pub const PRICE_UPDATE_V2: [u8; 8] = [34, 241, 35, 99, 157, 126, 244, 205];

/// A `PriceUpdateV2` with full verification: discriminator, write authority
/// (32), verification level (1 = full), feed id (32), price i64, confidence
/// u64, exponent i32, publish time i64, then fields not read here.
fn pyth_quote(account: &AccountView, vault: &Vault) -> Result<Quote, ProgramError> {
    if !account.owned_by(&PYTH_RECEIVER) {
        return Err(VaultError::WrongPriceOwner.into());
    }
    let data = account.try_borrow()?;
    if data.len() < 101 || data[..8] != PRICE_UPDATE_V2 {
        return Err(VaultError::NotPriceUpdate.into());
    }
    if data[40] != 1 {
        return Err(VaultError::PriceNotVerified.into());
    }
    if data[41..73] != vault.feed_id {
        return Err(VaultError::WrongFeed.into());
    }
    let word = |at: usize| -> [u8; 8] { data[at..at + 8].try_into().unwrap_or([0; 8]) };
    let price = i64::from_le_bytes(word(73));
    let confidence = u64::from_le_bytes(word(81));
    let exponent = i32::from_le_bytes(data[89..93].try_into().unwrap_or([0; 4]));
    let published = i64::from_le_bytes(word(93));
    check_age(published)?;
    let price = u64::try_from(price).map_err(|_| VaultError::BadPrice)?;
    // USD per collateral unit = price x 10^exponent; the debt token is 1 USD.
    let shift = i64::from(exponent) + 12 + i64::from(vault.debt_decimals)
        - i64::from(vault.collateral_decimals);
    bounds(price, confidence, |v, up| scale(v, shift, up))
}

/// Requires `|clock - published| <= PRICE_MAX_AGE`.
fn check_age(published: i64) -> ProgramResult {
    let age = clock()?
        .checked_sub(published)
        .ok_or(VaultError::StalePrice)?;
    if age.checked_abs().is_none_or(|a| a > PRICE_MAX_AGE) {
        return Err(VaultError::StalePrice.into());
    }
    Ok(())
}

/// `low = scale(price - confidence)` rounded down and `high = scale(price +
/// confidence)` rounded up; a price of zero, or a confidence as wide as the
/// price, is refused.
fn bounds(
    price: u64,
    confidence: u64,
    scale: impl Fn(u128, bool) -> Result<u64, ProgramError>,
) -> Result<Quote, ProgramError> {
    if price == 0 || confidence >= price {
        return Err(VaultError::BadPrice.into());
    }
    let (price, confidence) = (u128::from(price), u128::from(confidence));
    Ok(Quote {
        low: scale(price - confidence, false)?,
        high: scale(price + confidence, true)?,
    })
}

/// `value x 10^shift` as a `u64`, rounded `up` or down.
fn scale(value: u128, shift: i64, up: bool) -> Result<u64, ProgramError> {
    let power = |e: i64| {
        u32::try_from(e)
            .ok()
            .and_then(|e| 10u128.checked_pow(e))
            .ok_or(VaultError::PriceOverflow)
    };
    let scaled = if shift >= 0 {
        value
            .checked_mul(power(shift)?)
            .ok_or(VaultError::PriceOverflow)?
    } else {
        let divisor = power(-shift)?;
        let q = value / divisor;
        if up && !value.is_multiple_of(divisor) {
            q + 1
        } else {
            q
        }
    };
    u64::try_from(scaled).map_err(|_| VaultError::PriceOverflow.into())
}

/// The test price account of vault `id` and its bump.
pub fn test_price_address(id: u64) -> (Address, u8) {
    Address::find_program_address(&[TEST_PRICE_SEED, &id.to_le_bytes()], &crate::ID)
}

#[cfg(feature = "test-price")]
fn test_quote(account: &AccountView) -> Result<Quote, ProgramError> {
    if !account.owned_by(&crate::ID) {
        return Err(VaultError::WrongTestPriceAccount.into());
    }
    let data = account.try_borrow()?;
    if data.len() != TEST_PRICE_LEN {
        return Err(VaultError::WrongTestPriceAccount.into());
    }
    let word = |i: usize| -> [u8; 8] { data[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]) };
    let price = u64::from_le_bytes(word(0));
    let confidence = u64::from_le_bytes(word(1));
    check_age(i64::from_le_bytes(word(2)))?;
    // Already scaled; only the bounds' range is checked.
    bounds(price, confidence, |v, _| {
        u64::try_from(v).map_err(|_| VaultError::PriceOverflow.into())
    })
}

#[derive(SchemaRead, SchemaWrite)]
pub struct SetTestPriceArgs {
    pub vault: u64,
    /// Debt base units per collateral base unit x 1e12.
    pub price: u64,
    pub confidence: u64,
    /// Unix seconds.
    pub published: i64,
}

/// `set_test_price` (`test-price` builds): writes vault `vault`'s test price
/// account, creating it on first use. Accounts: keeper (signer, writable;
/// pays the rent), the test price PDA (writable), the system program.
pub fn set_test_price(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    if !cfg!(feature = "test-price") {
        return Err(VaultError::TestPriceDisabled.into());
    }
    #[cfg(feature = "test-price")]
    core::hint::black_box(&crate::vaults::TEST_PRICE_MARKER);
    let [keeper, account, ..] = accounts else {
        return Err(LendError::InvalidAccounts.into());
    };
    require_keeper(keeper)?;
    if !keeper.is_writable() || !account.is_writable() {
        return Err(LendError::InvalidAccounts.into());
    }
    let mut rest = data;
    let args: SetTestPriceArgs =
        wincode::deserialize_from(&mut rest).map_err(|_| LendError::InvalidInstructionData)?;
    vault(args.vault)?;
    let (expected, bump) = test_price_address(args.vault);
    if account.address() != &expected {
        return Err(VaultError::WrongTestPriceAccount.into());
    }
    if !account.owned_by(&crate::ID) {
        // Create it: the keeper tops it up to the rent, then the PDA signs to
        // allocate and assign itself. Not create-account, which refuses an
        // address someone already sent lamports to. The system program must
        // be among the transaction's accounts.
        let id = args.vault.to_le_bytes();
        let bump = [bump];
        let seeds = [
            Seed::from(TEST_PRICE_SEED),
            Seed::from(&id),
            Seed::from(&bump),
        ];
        let rent = Rent::get()?.try_minimum_balance(TEST_PRICE_LEN)?;
        let short = rent.saturating_sub(account.lamports());
        if short > 0 {
            let mut transfer = [0u8; 12];
            transfer[0] = 2;
            transfer[4..].copy_from_slice(&short.to_le_bytes());
            system(
                &transfer,
                &[
                    InstructionAccount::writable_signer(keeper.address()),
                    InstructionAccount::writable(account.address()),
                ],
                &[*keeper, *account],
                &[],
            )?;
        }
        let pda = [InstructionAccount::writable_signer(account.address())];
        let signer = [Signer::from(&seeds)];
        let mut allocate = [0u8; 12];
        allocate[0] = 8;
        allocate[4..].copy_from_slice(&(TEST_PRICE_LEN as u64).to_le_bytes());
        system(&allocate, &pda, &[*account], &signer)?;
        let mut assign = [0u8; 36];
        assign[0] = 1;
        assign[4..].copy_from_slice(crate::ID.as_array());
        system(&assign, &pda, &[*account], &signer)?;
    }
    let mut bytes = account.try_borrow_mut()?;
    let out = bytes
        .get_mut(..TEST_PRICE_LEN)
        .ok_or(ProgramError::AccountDataTooSmall)?;
    out[..8].copy_from_slice(&args.price.to_le_bytes());
    out[8..16].copy_from_slice(&args.confidence.to_le_bytes());
    out[16..].copy_from_slice(&args.published.to_le_bytes());
    Ok(())
}

/// A system program instruction (`data` starts with its u32 LE index).
fn system(
    data: &[u8],
    metas: &[InstructionAccount],
    accounts: &[AccountView],
    signers: &[Signer],
) -> ProgramResult {
    pinocchio::cpi::invoke_signed_with_bounds::<2, _>(
        &InstructionView {
            program_id: &SYSTEM_PROGRAM,
            data,
            accounts: metas,
        },
        accounts,
        signers,
    )
}

const SYSTEM_PROGRAM: Address = Address::new_from_array([0; 32]);
