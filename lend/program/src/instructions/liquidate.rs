//! `liquidate`: the keeper repays part or all of an unhealthy position from
//! its own note and takes collateral at the public bound. No owner
//! signature: the proof shows the position is past the threshold at a bound
//! no lower than the oracle's `price + confidence`.
use pinocchio::{AccountView, ProgramResult};
use spr_account_checks::AccountIterator;
use spr_program::CompressedProof;
use wincode::{SchemaRead, SchemaWrite};

use super::{
    check_now, cpi, parse, require_keeper, verified,
    vault::{check_liquidate_bound, first_nullifier, mints, vault},
    Pda,
};
use crate::{verifier::liquidate as v, POOL_SEED, USER_SEED};

#[derive(SchemaRead, SchemaWrite)]
pub struct LiquidateArgs {
    pub proof: CompressedProof,
    pub vault: u64,
    pub pool_viewing: [u8; 33],
    pub pool_bump: u8,
    /// The position's owner, whose user PDA owns it.
    pub owner: [u8; 32],
    /// The owner's viewing key, which the new position is encrypted to.
    pub owner_viewing: [u8; 33],
    pub user_bump: u8,
    pub now: u64,
    /// The check's and the payout's collateral price; at least the oracle's
    /// `price + confidence`.
    pub bound: u64,
}

/// Accounts: keeper (signer, writable; also the SPP payer and the
/// liquidator, owner of the spent debt-token note), the price account, then
/// the SPP transact accounts with owner signers `[user PDA, pool PDA,
/// keeper]`.
pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    require_keeper(iter.next_signer_mut("keeper")?)?;
    let price = iter.next_account("price")?;
    let (args, transact, bytes) = parse::<LiquidateArgs>(data)?;
    check_now(args.now)?;
    let vault = vault(args.vault)?;
    check_liquidate_bound(price, vault, args.bound)?;
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
                bound: args.bound,
                first_nullifier: first_nullifier(&transact)?,
            },
            private_tx_hash: transact.private_tx_hash,
            transfers: &[],
        }
        .verify(),
    )?;
    cpi(iter.remaining()?, bytes, &[&user, &pool])
}
