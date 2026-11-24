//! `init_reserve`: creates a mint's reserve, once (a second one reuses the
//! reserve address, which SPP refuses as `NullifierAlreadyQueued`).
use pinocchio::{AccountView, ProgramResult};
use spr_account_checks::AccountIterator;
use spr_program::CompressedProof;
use wincode::{SchemaRead, SchemaWrite};

use super::{
    check_now, cpi, mint_halves, parse, require_admin, require_keeper, require_pool_mint, verified,
    Pda,
};
use crate::{verifier::init_reserve as v, POOL_SEED};

#[derive(SchemaRead, SchemaWrite)]
pub struct InitReserveArgs {
    pub proof: CompressedProof,
    pub mint: [u8; 32],
    /// The viewing key the reserve note is encrypted to (the keeper's).
    pub pool_viewing: [u8; 33],
    pub pool_bump: u8,
    pub now: u64,
}

/// Accounts: keeper (signer, writable; also the SPP payer), admin (signer),
/// then the SPP transact accounts with the pool PDA as the owner signer.
pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    require_keeper(iter.next_signer_mut("keeper")?)?;
    require_admin(iter.next_signer("admin")?)?;
    let (args, transact, bytes) = parse::<InitReserveArgs>(data)?;
    check_now(args.now)?;
    require_pool_mint(&args.mint)?;
    let pool = Pda::new(POOL_SEED, &args.mint, args.pool_bump)?;
    let (mint_hi, mint_lo) = mint_halves(&args.mint);
    verified(
        v::Verifier {
            proof: &args.proof,
            public_inputs: v::PublicInputs {
                pool_owner: pool.shielded(&args.pool_viewing)?,
                mint_hi,
                mint_lo,
                now: args.now,
            },
            private_tx_hash: transact.private_tx_hash,
            transfers: &[],
        }
        .verify(),
    )?;
    cpi(iter.remaining()?, bytes, &[&pool])
}
