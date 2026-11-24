//! `deposit`: the keeper fills a deposit intent into a new position
//! (`tag::DEPOSIT`, circuit `DepositOpen`) or the maker's existing one
//! (`tag::DEPOSIT_ADD`, circuit `DepositAdd`). No reserve, no price.
use pinocchio::{AccountView, ProgramResult};
use spr_account_checks::AccountIterator;
use spr_program::CompressedProof;
use wincode::{SchemaRead, SchemaWrite};

use super::{
    check_now, cpi, parse, require_keeper, verified,
    vault::{check_expiry, first_nullifier, mints, vault},
    Pda,
};
use crate::{verifier, USER_SEED};

#[derive(SchemaRead, SchemaWrite)]
pub struct DepositArgs {
    pub proof: CompressedProof,
    pub vault: u64,
    /// The intent's maker, whose user PDA owns the intent and the position.
    pub owner: [u8; 32],
    /// The maker's viewing key, which the position is encrypted to.
    pub owner_viewing: [u8; 33],
    pub user_bump: u8,
    pub now: u64,
    /// The intent's expiry, which the proof commits.
    pub expiry: u64,
}

/// `DepositOpen`. Accounts: keeper (signer, writable; also the SPP payer),
/// then the SPP transact accounts with the user PDA as the owner signer.
pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    run(accounts, data, true)
}

/// `DepositAdd`, with the same accounts and data.
pub fn process_add(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    run(accounts, data, false)
}

fn run(accounts: &mut [AccountView], data: &[u8], open: bool) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    require_keeper(iter.next_signer_mut("keeper")?)?;
    let (args, transact, bytes) = parse::<DepositArgs>(data)?;
    check_now(args.now)?;
    check_expiry(args.expiry)?;
    let vault = vault(args.vault)?;
    let (_, _, coll_hi, coll_lo) = mints(vault);
    let user = Pda::new(USER_SEED, &args.owner, args.user_bump)?;
    let user_owner = user.shielded(&args.owner_viewing)?;
    let first_nullifier = first_nullifier(&transact)?;
    // Both circuits take the same public inputs, under their own keys.
    let result = if open {
        use verifier::deposit_open as v;
        v::Verifier {
            proof: &args.proof,
            public_inputs: v::PublicInputs {
                user_owner,
                coll_hi,
                coll_lo,
                vault: args.vault,
                now: args.now,
                expiry: args.expiry,
                first_nullifier,
            },
            private_tx_hash: transact.private_tx_hash,
            transfers: &[],
        }
        .verify()
    } else {
        use verifier::deposit_add as v;
        v::Verifier {
            proof: &args.proof,
            public_inputs: v::PublicInputs {
                user_owner,
                coll_hi,
                coll_lo,
                vault: args.vault,
                now: args.now,
                expiry: args.expiry,
                first_nullifier,
            },
            private_tx_hash: transact.private_tx_hash,
            transfers: &[],
        }
        .verify()
    };
    verified(result)?;
    cpi(iter.remaining()?, bytes, &[&user])
}
