//! `repay`: the keeper fills a repay intent into the maker's position. No
//! price and no owner signature: the intent authorises it.
use pinocchio::{AccountView, ProgramResult};
use spr_account_checks::AccountIterator;
use spr_program::CompressedProof;
use wincode::{SchemaRead, SchemaWrite};

use super::{
    check_now, cpi, parse, require_keeper,
    vault::{check_expiry, first_nullifier, mints, vault},
    verified, Pda,
};
use crate::{verifier::repay as v, POOL_SEED, USER_SEED};

#[derive(SchemaRead, SchemaWrite)]
pub struct RepayArgs {
    pub proof: CompressedProof,
    pub vault: u64,
    pub pool_viewing: [u8; 33],
    pub pool_bump: u8,
    /// The maker, whose user PDA owns the intent and the position.
    pub owner: [u8; 32],
    /// The maker's viewing key, which the position is encrypted to.
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
    let (args, transact, bytes) = parse::<RepayArgs>(data)?;
    check_now(args.now)?;
    check_expiry(args.expiry)?;
    let vault = vault(args.vault)?;
    let (debt_hi, debt_lo, coll_hi, coll_lo) = mints(vault);
    let pool = Pda::new(POOL_SEED, vault.debt_mint.as_array(), args.pool_bump)?;
    let user = Pda::new(USER_SEED, &args.owner, args.user_bump)?;
    verified(
        v::Verifier {
            proof: &args.proof,
            public_inputs: v::PublicInputs {
                pool_owner: pool.shielded(&args.pool_viewing)?,
                user_owner: user.shielded(&args.owner_viewing)?,
                debt_hi,
                debt_lo,
                coll_hi,
                coll_lo,
                vault: args.vault,
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
