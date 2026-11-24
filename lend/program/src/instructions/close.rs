//! `close`: the owner alone, from the browser, takes all the collateral of a
//! position with no debt. No keeper, no reserve, no price.
use pinocchio::{AccountView, ProgramResult};
use spr_account_checks::AccountIterator;
use spr_program::CompressedProof;
use wincode::{SchemaRead, SchemaWrite};

use super::{
    cpi, identity, parse, verified,
    vault::{mints, vault},
    Pda,
};
use crate::{verifier::close as v, USER_SEED};

#[derive(SchemaRead, SchemaWrite)]
pub struct CloseArgs {
    pub proof: CompressedProof,
    pub vault: u64,
    /// The viewing key of the position's owner address (any: the check is on
    /// the owner).
    pub viewing: [u8; 33],
    pub user_bump: u8,
}

/// Accounts: owner (signer, writable; also the SPP payer), then the SPP
/// transact accounts with the user PDA as the owner signer.
pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    let owner = *iter.next_signer_mut("owner")?.address();
    let (args, transact, bytes) = parse::<CloseArgs>(data)?;
    let vault = vault(args.vault)?;
    let (_, _, coll_hi, coll_lo) = mints(vault);
    let user = Pda::new(USER_SEED, owner.as_array(), args.user_bump)?;
    verified(
        v::Verifier {
            proof: &args.proof,
            public_inputs: v::PublicInputs {
                user_owner: user.shielded(&args.viewing)?,
                owner_identity: identity(&owner)?,
                coll_hi,
                coll_lo,
                vault: args.vault,
            },
            private_tx_hash: transact.private_tx_hash,
            transfers: &[],
        }
        .verify(),
    )?;
    cpi(iter.remaining()?, bytes, &[&user])
}
