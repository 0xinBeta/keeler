//! `intent`: the user moves a note's funds into an intent owned by their
//! user PDA, for the keeper to fill.
use pinocchio::{AccountView, ProgramResult};
use spr_account_checks::AccountIterator;
use spr_program::CompressedProof;
use wincode::{SchemaRead, SchemaWrite};

use super::{cpi, identity, mint_halves, parse, verified, Pda};
use crate::{verifier::intent as v, USER_SEED};

#[derive(SchemaRead, SchemaWrite)]
pub struct IntentArgs {
    pub proof: CompressedProof,
    /// The intent's mint, which the source note must hold.
    pub mint: [u8; 32],
    /// The viewing key the intent note is encrypted to (the keeper's, so it
    /// can fill it).
    pub viewing: [u8; 33],
    pub user_bump: u8,
}

/// Accounts: user (signer, writable; the maker and the SPP payer), then the
/// SPP transact accounts with owner signers `[user, user PDA]`.
pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    let user = *iter.next_signer_mut("user")?.address();
    let (args, transact, bytes) = parse::<IntentArgs>(data)?;
    let pda = Pda::new(USER_SEED, user.as_array(), args.user_bump)?;
    let (mint_hi, mint_lo) = mint_halves(&args.mint);
    verified(
        v::Verifier {
            proof: &args.proof,
            public_inputs: v::PublicInputs {
                user_owner: pda.shielded(&args.viewing)?,
                maker_identity: identity(&user)?,
                mint_hi,
                mint_lo,
            },
            private_tx_hash: transact.private_tx_hash,
            transfers: &[],
        }
        .verify(),
    )?;
    cpi(iter.remaining()?, bytes, &[&pda])
}
