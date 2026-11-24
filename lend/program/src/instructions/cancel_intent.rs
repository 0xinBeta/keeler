//! `cancel_intent`: after its expiry, the maker takes an unfilled intent's
//! funds back.
use pinocchio::{AccountView, ProgramResult};
use spr_account_checks::AccountIterator;
use spr_program::CompressedProof;
use wincode::{SchemaRead, SchemaWrite};

use super::{clock, cpi, identity, parse, verified, Pda};
use crate::{error::LendError, verifier::cancel_intent as v, USER_SEED};

#[derive(SchemaRead, SchemaWrite)]
pub struct CancelIntentArgs {
    pub proof: CompressedProof,
    /// The viewing key of the intent's owner address (as at `intent`).
    pub viewing: [u8; 33],
    pub user_bump: u8,
    /// The intent's expiry, which the proof commits.
    pub expiry: u64,
}

/// Accounts: user (signer, writable; the maker and the SPP payer), then the
/// SPP transact accounts with the user PDA as the owner signer.
pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    let user = *iter.next_signer_mut("user")?.address();
    let (args, transact, bytes) = parse::<CancelIntentArgs>(data)?;
    let expiry = i64::try_from(args.expiry).map_err(|_| LendError::NotExpired)?;
    if clock()? <= expiry {
        return Err(LendError::NotExpired.into());
    }
    let pda = Pda::new(USER_SEED, user.as_array(), args.user_bump)?;
    verified(
        v::Verifier {
            proof: &args.proof,
            public_inputs: v::PublicInputs {
                user_owner: pda.shielded(&args.viewing)?,
                maker_identity: identity(&user)?,
                expiry: args.expiry,
            },
            private_tx_hash: transact.private_tx_hash,
            transfers: &[],
        }
        .verify(),
    )?;
    cpi(iter.remaining()?, bytes, &[&pda])
}
