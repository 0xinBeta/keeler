//! `redeem`: the keeper pays part or all of a receipt out of the pool's
//! reserve to the receipt's owner, who signs.
use pinocchio::{AccountView, ProgramResult};
use spr_account_checks::AccountIterator;
use spr_program::CompressedProof;
use wincode::{SchemaRead, SchemaWrite};

use super::{
    check_now, cpi, identity, mint_halves, parse, require_keeper, supply::first_nullifier,
    verified, Pda,
};
use crate::{verifier::redeem as v, POOL_SEED, USER_SEED};

#[derive(SchemaRead, SchemaWrite)]
pub struct RedeemArgs {
    pub proof: CompressedProof,
    pub mint: [u8; 32],
    /// The viewing key the reserve note is encrypted to.
    pub pool_viewing: [u8; 33],
    pub pool_bump: u8,
    /// The owner's viewing key, which the new receipt is encrypted to.
    pub owner_viewing: [u8; 33],
    /// The bump of the owner's user PDA.
    pub user_bump: u8,
    pub now: u64,
}

/// Accounts: keeper (signer, writable; also the SPP payer), owner (signer,
/// read-only), then the SPP transact accounts with owner signers `[user PDA,
/// pool PDA]`.
pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    require_keeper(iter.next_signer_mut("keeper")?)?;
    let owner = *iter.next_signer("owner")?.address();
    let (args, transact, bytes) = parse::<RedeemArgs>(data)?;
    check_now(args.now)?;
    let pool = Pda::new(POOL_SEED, &args.mint, args.pool_bump)?;
    let user = Pda::new(USER_SEED, owner.as_array(), args.user_bump)?;
    let (mint_hi, mint_lo) = mint_halves(&args.mint);
    verified(
        v::Verifier {
            proof: &args.proof,
            public_inputs: v::PublicInputs {
                pool_owner: pool.shielded(&args.pool_viewing)?,
                user_owner: user.shielded(&args.owner_viewing)?,
                owner_identity: identity(&owner)?,
                mint_hi,
                mint_lo,
                now: args.now,
                first_nullifier: first_nullifier(&transact)?,
            },
            private_tx_hash: transact.private_tx_hash,
            transfers: &[],
        }
        .verify(),
    )?;
    cpi(iter.remaining()?, bytes, &[&user, &pool])
}
