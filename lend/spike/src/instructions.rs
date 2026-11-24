//! Spike instructions: decode, check the Clock and the price account, verify
//! the proof, CPI SPP signed by the pool and user PDAs.
use pinocchio::{
    address::address_eq,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    AccountView, Address, ProgramResult,
};
use spr_account_checks::AccountIterator;
use spr_hasher::primitives::solana_owner_identity;
use spr_interface::instruction::instruction_data::transact::TransactIxDataRef;
use spr_program::{
    cpi::SppTransactAccounts, program::PdaOwner, verifier::address_hash, CompressedProof,
};
use wincode::{config::DefaultConfig, SchemaRead, SchemaReadOwned, SchemaWrite};

use crate::{tag, verifier, NOW_WINDOW, POOL_SEED, PRICE_ACCOUNT_LEN, PRICE_MAX_AGE, USER_SEED};

fn bad() -> ProgramError {
    ProgramError::InvalidInstructionData
}

fn parse<'a, A: SchemaReadOwned<DefaultConfig, Dst = A>>(
    data: &'a [u8],
) -> Result<(A, TransactIxDataRef<'a>, &'a [u8]), ProgramError> {
    let mut rest = data;
    let args = wincode::deserialize_from(&mut rest).map_err(|_| bad())?;
    let transact = TransactIxDataRef::from_bytes(rest).map_err(|_| bad())?;
    Ok((args, transact, rest))
}

/// A PDA `[seed, key, [bump]]` with its owner hash.
struct Pda {
    address: Address,
    key: [u8; 32],
    bump: [u8; 1],
    seed: &'static [u8],
}

impl Pda {
    fn new(seed: &'static [u8], key: &[u8; 32], bump: u8) -> Result<Self, ProgramError> {
        let address = Address::create_program_address(&[seed, key, &[bump]], &crate::ID)
            .map_err(|_| ProgramError::InvalidSeeds)?;
        Ok(Self {
            address,
            key: *key,
            bump: [bump],
            seed,
        })
    }

    fn shielded(&self, viewing: &[u8; 33]) -> Result<[u8; 32], ProgramError> {
        let owner = PdaOwner::new(&self.address).map_err(|_| bad())?;
        address_hash(owner.owner_hash(), viewing).map_err(|_| bad())
    }

    fn seeds(&self) -> [Seed<'_>; 3] {
        [
            Seed::from(self.seed),
            Seed::from(&self.key),
            Seed::from(&self.bump),
        ]
    }
}

fn cpi(spp_accounts: &[AccountView], transact: &[u8], pdas: &[&Pda]) -> ProgramResult {
    let addresses: [&Address; 2] = [
        &pdas.first().ok_or_else(bad)?.address,
        &pdas.last().ok_or_else(bad)?.address,
    ];
    let spp = SppTransactAccounts::new(spp_accounts, &addresses[..pdas.len().min(2)])
        .map_err(ProgramError::from)?;
    match pdas {
        [one] => spp.invoke::<20>(transact, &[Signer::from(&one.seeds())]),
        [one, two] => spp.invoke::<20>(
            transact,
            &[Signer::from(&one.seeds()), Signer::from(&two.seeds())],
        ),
        _ => Err(bad()),
    }
}

fn check_now(now: u64) -> ProgramResult {
    let clock = Clock::get()?.unix_timestamp;
    let now = i64::try_from(now).map_err(|_| bad())?;
    if now <= clock && clock <= now + NOW_WINDOW {
        Ok(())
    } else {
        Err(ProgramError::Custom(1))
    }
}

fn verified(result: Result<(), spr_program::verifier::VerifyError>) -> ProgramResult {
    result.map_err(|_| ProgramError::Custom(2))
}

fn halves(mint: &[u8; 32]) -> (u128, u128) {
    let mut hi = [0u8; 16];
    let mut lo = [0u8; 16];
    hi.copy_from_slice(mint.get(..16).unwrap_or(&[0; 16]));
    lo.copy_from_slice(mint.get(16..).unwrap_or(&[0; 16]));
    (u128::from_be_bytes(hi), u128::from_be_bytes(lo))
}

#[derive(SchemaRead, SchemaWrite)]
pub struct PoolArgs {
    pub proof: CompressedProof,
    pub mint: [u8; 32],
    pub pool_viewing: [u8; 33],
    pub pool_bump: u8,
    pub now: u64,
}

#[derive(SchemaRead, SchemaWrite)]
pub struct FundArgs {
    pub proof: CompressedProof,
    pub user_viewing: [u8; 33],
    pub user_bump: u8,
}

#[derive(SchemaRead, SchemaWrite)]
pub struct TouchArgs {
    pub proof: CompressedProof,
    pub mint: [u8; 32],
    pub pool_viewing: [u8; 33],
    pub pool_bump: u8,
    pub user_viewing: [u8; 33],
    pub user_bump: u8,
    pub now: u64,
    pub price_bound: u64,
}

pub fn process_instruction(
    program_id: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
) -> ProgramResult {
    if !address_eq(program_id, &crate::ID) {
        return Err(ProgramError::IncorrectProgramId);
    }
    let (ix, data) = data.split_first().ok_or_else(bad)?;
    match *ix {
        tag::INIT => init(accounts, data),
        tag::ACCRUE | tag::TOUCH | tag::FUND => spend(*ix, accounts, data),
        tag::SET_PRICE => set_price(accounts, data),
        _ => Err(bad()),
    }
}

fn init(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    iter.next_signer_mut("payer")?;
    let (args, transact, bytes) = parse::<PoolArgs>(data)?;
    check_now(args.now)?;
    let pool = Pda::new(POOL_SEED, &args.mint, args.pool_bump)?;
    let (mint_hi, mint_lo) = halves(&args.mint);
    verified(
        verifier::spike_init::Verifier {
            proof: &args.proof,
            public_inputs: verifier::spike_init::PublicInputs {
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

fn spend(ix: u8, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    let keeper = *iter.next_signer_mut("payer")?.address().as_array();
    match ix {
        tag::ACCRUE => {
            let (args, transact, bytes) = parse::<PoolArgs>(data)?;
            check_now(args.now)?;
            let pool = Pda::new(POOL_SEED, &args.mint, args.pool_bump)?;
            let (mint_hi, mint_lo) = halves(&args.mint);
            verified(
                verifier::spike_accrue::Verifier {
                    proof: &args.proof,
                    public_inputs: verifier::spike_accrue::PublicInputs {
                        pool_owner: pool.shielded(&args.pool_viewing)?,
                        keeper_identity: solana_owner_identity(&keeper).map_err(|_| bad())?,
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
        tag::FUND => {
            // The payer is the user here.
            let (args, transact, bytes) = parse::<FundArgs>(data)?;
            let user = Pda::new(USER_SEED, &keeper, args.user_bump)?;
            verified(
                verifier::spike_fund::Verifier {
                    proof: &args.proof,
                    public_inputs: verifier::spike_fund::PublicInputs {
                        user_owner: user.shielded(&args.user_viewing)?,
                        maker_identity: solana_owner_identity(&keeper).map_err(|_| bad())?,
                    },
                    private_tx_hash: transact.private_tx_hash,
                    transfers: &[],
                }
                .verify(),
            )?;
            cpi(iter.remaining()?, bytes, &[&user])
        }
        _ => {
            let user_key = *iter.next_signer("user")?.address().as_array();
            let price = iter.next_account("price")?;
            let (args, transact, bytes) = parse::<TouchArgs>(data)?;
            check_now(args.now)?;
            check_price(price, args.price_bound)?;
            let pool = Pda::new(POOL_SEED, &args.mint, args.pool_bump)?;
            let user = Pda::new(USER_SEED, &user_key, args.user_bump)?;
            let (mint_hi, mint_lo) = halves(&args.mint);
            verified(
                verifier::spike_touch::Verifier {
                    proof: &args.proof,
                    public_inputs: verifier::spike_touch::PublicInputs {
                        pool_owner: pool.shielded(&args.pool_viewing)?,
                        user_owner: user.shielded(&args.user_viewing)?,
                        maker_identity: solana_owner_identity(&user_key).map_err(|_| bad())?,
                        now: args.now,
                        price_bound: args.price_bound,
                        mint_hi,
                        mint_lo,
                    },
                    private_tx_hash: transact.private_tx_hash,
                    transfers: &[],
                }
                .verify(),
            )?;
            cpi(iter.remaining()?, bytes, &[&pool, &user])
        }
    }
}

/// `price - confidence >= bound`, at most `PRICE_MAX_AGE` old, in an account
/// this program owns.
fn check_price(account: &AccountView, bound: u64) -> ProgramResult {
    if !account.owned_by(&crate::ID) {
        return Err(ProgramError::IllegalOwner);
    }
    let data = account.try_borrow()?;
    let field = |i: usize| -> Result<[u8; 8], ProgramError> {
        data.get(i * 8..i * 8 + 8)
            .and_then(|b| b.try_into().ok())
            .ok_or(ProgramError::AccountDataTooSmall)
    };
    let price = u64::from_le_bytes(field(0)?);
    let confidence = u64::from_le_bytes(field(1)?);
    let published = i64::from_le_bytes(field(2)?);
    let clock = Clock::get()?.unix_timestamp;
    if clock - published > PRICE_MAX_AGE {
        return Err(ProgramError::Custom(3));
    }
    if price.checked_sub(confidence).ok_or(ProgramError::Custom(4))? < bound {
        return Err(ProgramError::Custom(4));
    }
    Ok(())
}

/// Test only: writes `price, confidence, now` into a price account this
/// program owns.
fn set_price(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let mut iter = AccountIterator::new(accounts);
    iter.next_signer_mut("payer")?;
    let account = iter.next_mut("price")?;
    if !account.owned_by(&crate::ID) || data.len() != 16 {
        return Err(bad());
    }
    let now = Clock::get()?.unix_timestamp;
    let mut bytes = account.try_borrow_mut()?;
    let out = bytes
        .get_mut(..PRICE_ACCOUNT_LEN)
        .ok_or(ProgramError::AccountDataTooSmall)?;
    let (head, tail) = out.split_at_mut(16);
    head.copy_from_slice(data);
    tail.copy_from_slice(&now.to_le_bytes());
    Ok(())
}
