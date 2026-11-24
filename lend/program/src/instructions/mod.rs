//! Dispatch and the checks every instruction shares. An instruction's data is
//! `[tag] ++ wincode(args) ++ wincode(TransactIxData)`; its accounts are its
//! own signers then the shielded pool's transact layout (payer, output tree,
//! SPP program, system program, one input tree per tree context, nullifier
//! PDAs, owner signers), which is forwarded to SPP unchanged.
use pinocchio::{
    address::address_eq,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    AccountView, Address, ProgramResult,
};
use spr_hasher::primitives::solana_owner_identity;
use spr_interface::instruction::instruction_data::transact::TransactIxDataRef;
use spr_program::{cpi::SppTransactAccounts, program::PdaOwner, verifier::address_hash};
use wincode::{config::DefaultConfig, SchemaReadOwned};

use crate::{error::LendError, tag, NOW_WINDOW};

pub mod cancel_intent;
pub mod init_reserve;
pub mod intent;
// Earn.
pub mod redeem;
pub mod supply;
// Vault.
pub mod close;
pub mod deposit;
pub mod draw;
pub mod liquidate;
pub mod repay;
pub mod vault;

pub fn process_instruction(
    program_id: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
) -> ProgramResult {
    if !address_eq(program_id, &crate::ID) {
        return Err(ProgramError::IncorrectProgramId);
    }
    let (ix, data) = data
        .split_first()
        .ok_or(LendError::InvalidInstructionData)?;
    match *ix {
        tag::INIT_RESERVE => init_reserve::process(accounts, data),
        tag::INTENT => intent::process(accounts, data),
        tag::CANCEL_INTENT => cancel_intent::process(accounts, data),
        tag::SUPPLY => supply::process(accounts, data),
        tag::REDEEM => redeem::process(accounts, data),
        tag::DEPOSIT => deposit::process(accounts, data),
        tag::DRAW => draw::process(accounts, data),
        tag::REPAY => repay::process(accounts, data),
        tag::LIQUIDATE => liquidate::process(accounts, data),
        tag::CLOSE => close::process(accounts, data),
        tag::DEPOSIT_ADD => deposit::process_add(accounts, data),
        tag::SET_TEST_PRICE => vault::set_test_price(accounts, data),
        _ => Err(LendError::UnknownInstruction.into()),
    }
}

/// `(args, transact, transact bytes)`: the transact data must parse and is
/// forwarded to SPP as it came.
pub(crate) fn parse<A: SchemaReadOwned<DefaultConfig, Dst = A>>(
    data: &[u8],
) -> Result<(A, TransactIxDataRef<'_>, &[u8]), ProgramError> {
    let mut rest = data;
    let args =
        wincode::deserialize_from(&mut rest).map_err(|_| LendError::InvalidInstructionData)?;
    let transact =
        TransactIxDataRef::from_bytes(rest).map_err(|_| LendError::InvalidInstructionData)?;
    Ok((args, transact, rest))
}

/// A program PDA `[seed, key, [bump]]` that owns notes.
pub(crate) struct Pda {
    pub address: Address,
    key: [u8; 32],
    bump: [u8; 1],
    seed: &'static [u8],
}

impl Pda {
    pub fn new(seed: &'static [u8], key: &[u8; 32], bump: u8) -> Result<Self, ProgramError> {
        // Only the canonical bump: another one derives another PDA for the
        // same key (a second pool PDA would own a second reserve of the mint).
        let (address, canonical) = Address::find_program_address(&[seed, key], &crate::ID);
        if bump != canonical {
            return Err(LendError::InvalidSeeds.into());
        }
        Ok(Self {
            address,
            key: *key,
            bump: [bump],
            seed,
        })
    }

    /// The PDA's shielded address with `viewing`: a circuit's public owner.
    pub fn shielded(&self, viewing: &[u8; 33]) -> Result<[u8; 32], ProgramError> {
        let owner = PdaOwner::new(&self.address).map_err(|_| LendError::InvalidSeeds)?;
        address_hash(owner.owner_hash(), viewing)
            .map_err(|_| LendError::InvalidInstructionData.into())
    }

    fn seeds(&self) -> [Seed<'_>; 3] {
        [
            Seed::from(self.seed),
            Seed::from(&self.key),
            Seed::from(&self.bump),
        ]
    }
}

/// Invokes SPP with `transact` over `spp_accounts`, signing for `pdas` (one
/// or two).
pub(crate) fn cpi(spp_accounts: &[AccountView], transact: &[u8], pdas: &[&Pda]) -> ProgramResult {
    let addresses: [&Address; 2] = [
        &pdas.first().ok_or(LendError::InvalidSeeds)?.address,
        &pdas.last().ok_or(LendError::InvalidSeeds)?.address,
    ];
    let spp = SppTransactAccounts::new(spp_accounts, &addresses[..pdas.len().min(2)])
        .map_err(ProgramError::from)?;
    match pdas {
        [one] => spp.invoke::<24>(transact, &[Signer::from(&one.seeds())]),
        [one, two] => spp.invoke::<24>(
            transact,
            &[Signer::from(&one.seeds()), Signer::from(&two.seeds())],
        ),
        _ => Err(LendError::InvalidSeeds.into()),
    }
}

/// The cluster's unix time.
pub(crate) fn clock() -> Result<i64, ProgramError> {
    Ok(Clock::get()?.unix_timestamp)
}

/// Requires `now <= clock <= now + NOW_WINDOW`: the public `now` a circuit
/// accrues to is chain time.
pub(crate) fn check_now(now: u64) -> ProgramResult {
    let clock = clock()?;
    let now = i64::try_from(now).map_err(|_| LendError::StaleNow)?;
    if now <= clock && clock <= now.saturating_add(NOW_WINDOW) {
        Ok(())
    } else {
        Err(LendError::StaleNow.into())
    }
}

pub(crate) fn verified(result: Result<(), spr_program::verifier::VerifyError>) -> ProgramResult {
    result.map_err(|_| LendError::InvalidProof.into())
}

/// Requires `account` to be the keeper and to have signed. Every instruction
/// that spends a reserve calls it on its keeper account.
pub(crate) fn require_keeper(account: &AccountView) -> ProgramResult {
    require_signer(account, &crate::KEEPER, LendError::NotKeeper)
}

pub(crate) fn require_admin(account: &AccountView) -> ProgramResult {
    require_signer(account, &crate::ADMIN, LendError::NotAdmin)
}

fn require_signer(account: &AccountView, expected: &Address, error: LendError) -> ProgramResult {
    if !account.is_signer() {
        return Err(LendError::MissingSignature.into());
    }
    if !address_eq(account.address(), expected) {
        return Err(error.into());
    }
    Ok(())
}

/// A wallet's owner identity, the value a circuit compares a note owner's
/// key against.
pub(crate) fn identity(wallet: &Address) -> Result<[u8; 32], ProgramError> {
    solana_owner_identity(wallet.as_array()).map_err(|_| LendError::InvalidInstructionData.into())
}

/// Requires `mint` to be the pool's: the circuits hard-code the tUSD pool's
/// curve, fee and minimum, so no other mint may have a reserve.
pub(crate) fn require_pool_mint(mint: &[u8; 32]) -> ProgramResult {
    if mint != crate::pool::TUSD_MINT.as_array() {
        return Err(LendError::UnknownMint.into());
    }
    Ok(())
}

/// The mint as two big-endian `u128` halves, as the circuits take it.
pub(crate) fn mint_halves(mint: &[u8; 32]) -> (u128, u128) {
    let (hi, lo) = mint.split_at(16);
    let half = |b: &[u8]| u128::from_be_bytes(b.try_into().unwrap_or([0; 16]));
    (half(hi), half(lo))
}
