//! `draw`: the owner borrows, withdraws collateral, or both. The keeper
//! builds and pays; the owner co-signs (read-only, not the fee payer). The
//! price account is always required, even for a withdrawal from a position
//! with no debt, so a pure withdrawal looks like a borrow.
use pinocchio::{AccountView, ProgramResult};
use spr_account_checks::AccountIterator;
use spr_program::CompressedProof;
use wincode::{SchemaRead, SchemaWrite};

use super::{
    check_now, cpi, identity, parse, require_keeper, verified,
    vault::{check_draw_bound, first_nullifier, mints, vault},
    Pda,
};
use crate::{verifier::draw as v, POOL_SEED, USER_SEED};

#[derive(SchemaRead, SchemaWrite)]
pub struct DrawArgs {
    pub proof: CompressedProof,
    pub vault: u64,
    /// The viewing key the reserve is encrypted to.
    pub pool_viewing: [u8; 33],
    pub pool_bump: u8,
    /// The owner's viewing key, which the position is encrypted to.
    pub owner_viewing: [u8; 33],
    pub user_bump: u8,
    pub now: u64,
    /// The health check's collateral price; at most the oracle's `price -
    /// confidence`.
    pub bound: u64,
}

/// Accounts: keeper (signer, writable; also the SPP payer), owner (signer,
/// read-only), the price account, then the SPP transact accounts with owner
/// signers `[user PDA, pool PDA]`.
pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    require_keeper(iter.next_signer_mut("keeper")?)?;
    let owner = *iter.next_signer("owner")?.address();
    let price = iter.next_account("price")?;
    let (args, transact, bytes) = parse::<DrawArgs>(data)?;
    check_now(args.now)?;
    let vault = vault(args.vault)?;
    check_draw_bound(price, vault, args.bound)?;
    let (debt_hi, debt_lo, coll_hi, coll_lo) = mints(vault);
    let pool = Pda::new(POOL_SEED, vault.debt_mint.as_array(), args.pool_bump)?;
    let user = Pda::new(USER_SEED, owner.as_array(), args.user_bump)?;
    verified(
        v::Verifier {
            proof: &args.proof,
            public_inputs: v::PublicInputs {
                pool_owner: pool.shielded(&args.pool_viewing)?,
                user_owner: user.shielded(&args.owner_viewing)?,
                owner_identity: identity(&owner)?,
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
