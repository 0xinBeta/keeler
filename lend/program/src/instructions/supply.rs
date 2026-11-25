//! `supply`: the keeper fills a supply intent into the pool's reserve and
//! mints the maker a receipt.
use pinocchio::{error::ProgramError, AccountView, ProgramResult};
use spr_account_checks::AccountIterator;
use spr_program::CompressedProof;
use wincode::{SchemaRead, SchemaWrite};

use super::{
    check_now, clock, cpi, mint_halves, parse, require_keeper, require_pool_viewing, require_pool_mint, verified, Pda,
};
use crate::{error::LendError, verifier::supply as v, POOL_SEED, USER_SEED};

#[derive(SchemaRead, SchemaWrite)]
pub struct SupplyArgs {
    pub proof: CompressedProof,
    pub mint: [u8; 32],
    /// The viewing key the reserve note is encrypted to.
    pub pool_viewing: [u8; 33],
    pub pool_bump: u8,
    /// The intent's maker wallet, whose user PDA owns the intent and the
    /// receipt.
    pub maker: [u8; 32],
    /// The maker's viewing key, which the receipt is encrypted to.
    pub owner_viewing: [u8; 33],
    pub user_bump: u8,
    pub now: u64,
    /// The intent's expiry, which the proof commits.
    pub expiry: u64,
}

/// Accounts: keeper (signer, writable; also the SPP payer), then the SPP
/// transact accounts with owner signers `[user PDA, pool PDA]`.
pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    require_keeper(iter.next_signer_mut("keeper")?)?;
    let (args, transact, bytes) = parse::<SupplyArgs>(data)?;
    require_pool_viewing(&args.pool_viewing)?;
    check_now(args.now)?;
    let expiry = i64::try_from(args.expiry).unwrap_or(i64::MAX);
    if clock()? > expiry {
        return Err(LendError::IntentExpired.into());
    }
    require_pool_mint(&args.mint)?;
    let pool = Pda::new(POOL_SEED, &args.mint, args.pool_bump)?;
    let user = Pda::new(USER_SEED, &args.maker, args.user_bump)?;
    let (mint_hi, mint_lo) = mint_halves(&args.mint);
    verified(
        v::Verifier {
            proof: &args.proof,
            public_inputs: v::PublicInputs {
                pool_owner: pool.shielded(&args.pool_viewing)?,
                user_owner: user.shielded(&args.owner_viewing)?,
                mint_hi,
                mint_lo,
                now: args.now,
                expiry: args.expiry,
                first_nullifier: first_nullifier(&transact)?,
            },
            private_tx_hash: transact.private_tx_hash,
            transfers: &[],
        }
        .verify(),
    )?;
    cpi(iter.remaining()?, bytes, &[&user, &pool])
}

/// The transact's first input nullifier, which the output blindings derive
/// from.
pub(crate) fn first_nullifier(
    transact: &spr_interface::instruction::instruction_data::transact::TransactIxDataRef<'_>,
) -> Result<[u8; 32], ProgramError> {
    Ok(transact
        .inputs
        .first()
        .ok_or(LendError::InvalidInstructionData)?
        .nullifier_hash)
}
