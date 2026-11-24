//! `supply` and `redeem`: the program's refusals that need no real proof, on
//! the SBF build in `lend/target/deploy` (build it first, see
//! `lend/AGENTS.md`). The circuit's refusals are in `sdk/tests/earn.rs`.
mod mollusk;

use keeler_lend::{
    error::LendError,
    instructions::{redeem::RedeemArgs, supply::SupplyArgs},
    tag, KEEPER, POOL_SEED, USER_SEED,
};
use mollusk::{expect_err_exact, mollusk_with_program};
use mollusk_svm::{result::ProgramResult, Mollusk};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;
use spr_interface::{
    instruction::instruction_data::transact::{
        CircuitId, InputUtxo, TransactIxData, TransactProof, TreeContext,
    },
    N_PUBLIC_SLOTS,
};
use spr_program::CompressedProof;

const SBF_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../target/deploy");
const MINT: [u8; 32] = keeler_lend::pool::TUSD_MINT.to_bytes();
const NOW: i64 = 1_800_000_000;

fn setup() -> Mollusk {
    let (mut mollusk, _) =
        mollusk_with_program(SBF_DIR, *keeler_lend::ID.as_array(), "keeler_lend");
    mollusk.sysvars.clock.unix_timestamp = NOW;
    mollusk
}

fn pk(a: &solana_address::Address) -> Pubkey {
    Pubkey::new_from_array(*a.as_array())
}

fn account() -> Account {
    Account {
        lamports: 1_000_000_000,
        ..Account::default()
    }
}

fn code(e: LendError) -> ProgramError {
    ProgramError::Custom(e as u32)
}

/// A transact that parses, spending `inputs` nullifiers.
fn transact(inputs: usize) -> Vec<u8> {
    wincode::serialize(&TransactIxData {
        expiry_unix_ts: u64::MAX,
        tx_viewing_pk: [2; 33],
        salt: [3; 16],
        interface_transfers: Vec::new(),
        data_hash: None,
        ring_data_hash: None,
        outputs: Vec::new(),
        messages: Vec::new(),
        private_tx_hash: [1; 32],
        circuit: CircuitId::ConfidentialSigner(2, 3, N_PUBLIC_SLOTS as u8),
        proof: TransactProof::zeroed(),
        inputs: (0..inputs)
            .map(|i| InputUtxo {
                nullifier_hash: [i as u8 + 1; 32],
                tree_index: 0,
            })
            .collect(),
        tree_contexts: vec![TreeContext {
            utxo_tree_root_index: 0,
            nullifier_tree_root_index: 0,
        }],
    })
    .unwrap()
}

fn proof() -> CompressedProof {
    CompressedProof {
        a: [0; 32],
        b: [0; 64],
        c: [0; 32],
    }
}

fn bump(seed: &[u8], key: &[u8; 32]) -> u8 {
    Pubkey::find_program_address(&[seed, key], &pk(&keeler_lend::ID)).1
}

fn data(ix: u8, args: Vec<u8>, inputs: usize) -> Vec<u8> {
    let mut data = vec![ix];
    data.extend(args);
    data.extend(transact(inputs));
    data
}

fn run(mollusk: &Mollusk, ix: &Instruction, accounts: &[(Pubkey, Account)]) -> ProgramResult {
    mollusk.process_instruction(ix, accounts).program_result
}

fn supply_args(maker: &Pubkey, now: i64, expiry: i64) -> SupplyArgs {
    SupplyArgs {
        proof: proof(),
        mint: MINT,
        pool_viewing: [2; 33],
        pool_bump: bump(POOL_SEED, &MINT),
        maker: maker.to_bytes(),
        owner_viewing: [2; 33],
        user_bump: bump(USER_SEED, &maker.to_bytes()),
        now: now as u64,
        expiry: expiry as u64,
    }
}

fn supply(
    keeper: Pubkey,
    signed: bool,
    args: &SupplyArgs,
    inputs: usize,
) -> (Instruction, Vec<(Pubkey, Account)>) {
    let ix = Instruction {
        program_id: pk(&keeler_lend::ID),
        accounts: vec![AccountMeta::new(keeper, signed)],
        data: data(tag::SUPPLY, wincode::serialize(args).unwrap(), inputs),
    };
    (ix, vec![(keeper, account())])
}

fn redeem_args(owner: &Pubkey, now: i64) -> RedeemArgs {
    RedeemArgs {
        proof: proof(),
        mint: MINT,
        pool_viewing: [2; 33],
        pool_bump: bump(POOL_SEED, &MINT),
        owner_viewing: [2; 33],
        user_bump: bump(USER_SEED, &owner.to_bytes()),
        now: now as u64,
    }
}

fn redeem(
    keeper: Pubkey,
    owner: Pubkey,
    signed: (bool, bool),
    args: &RedeemArgs,
    inputs: usize,
) -> (Instruction, Vec<(Pubkey, Account)>) {
    let ix = Instruction {
        program_id: pk(&keeler_lend::ID),
        accounts: vec![
            AccountMeta::new(keeper, signed.0),
            AccountMeta::new_readonly(owner, signed.1),
        ],
        data: data(tag::REDEEM, wincode::serialize(args).unwrap(), inputs),
    };
    (ix, vec![(keeper, account()), (owner, account())])
}

#[test]
fn supply_needs_the_keeper() {
    let mollusk = setup();
    let maker = Pubkey::new_unique();
    let args = supply_args(&maker, NOW, NOW + 60);
    let (ix, accounts) = supply(Pubkey::new_unique(), true, &args, 2);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::NotKeeper));
    let (ix, accounts) = supply(pk(&KEEPER), false, &args, 2);
    assert!(matches!(
        run(&mollusk, &ix, &accounts),
        ProgramResult::Failure(_)
    ));
    // With the keeper it reaches the proof.
    let (ix, accounts) = supply(pk(&KEEPER), true, &args, 2);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::InvalidProof));
}

#[test]
fn supply_refuses_a_stale_now_and_an_expired_intent() {
    let mollusk = setup();
    let maker = Pubkey::new_unique();
    for now in [NOW - 121, NOW + 1] {
        let (ix, accounts) = supply(pk(&KEEPER), true, &supply_args(&maker, now, NOW + 60), 2);
        expect_err_exact(&mollusk, &ix, &accounts, code(LendError::StaleNow));
    }
    for expiry in [NOW - 1, NOW - 120] {
        let (ix, accounts) = supply(pk(&KEEPER), true, &supply_args(&maker, NOW, expiry), 2);
        expect_err_exact(&mollusk, &ix, &accounts, code(LendError::IntentExpired));
    }
    // Expiring this second is still in time; `now` may trail the clock.
    for (now, expiry) in [(NOW, NOW), (NOW - 120, NOW), (NOW, i64::MAX)] {
        let (ix, accounts) = supply(pk(&KEEPER), true, &supply_args(&maker, now, expiry), 2);
        expect_err_exact(&mollusk, &ix, &accounts, code(LendError::InvalidProof));
    }
    let (ix, accounts) = supply(
        pk(&KEEPER),
        true,
        &SupplyArgs {
            expiry: u64::MAX,
            ..supply_args(&maker, NOW, NOW)
        },
        2,
    );
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::InvalidProof));
}

#[test]
fn supply_refuses_a_bad_bump_and_a_transact_without_inputs() {
    let mollusk = setup();
    let maker = Pubkey::new_unique();
    let (ix, accounts) = supply(pk(&KEEPER), true, &supply_args(&maker, NOW, NOW), 0);
    expect_err_exact(
        &mollusk,
        &ix,
        &accounts,
        code(LendError::InvalidInstructionData),
    );
    let id = pk(&keeler_lend::ID);
    let bad = (0..=u8::MAX)
        .rev()
        .find(|b| {
            Pubkey::create_program_address(&[USER_SEED, &maker.to_bytes(), &[*b]], &id).is_err()
        })
        .unwrap();
    let args = SupplyArgs {
        user_bump: bad,
        ..supply_args(&maker, NOW, NOW)
    };
    let (ix, accounts) = supply(pk(&KEEPER), true, &args, 2);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::InvalidSeeds));
}

#[test]
fn redeem_needs_the_keeper_and_the_owner_signing() {
    let mollusk = setup();
    let owner = Pubkey::new_unique();
    let args = redeem_args(&owner, NOW);
    let (ix, accounts) = redeem(Pubkey::new_unique(), owner, (true, true), &args, 2);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::NotKeeper));
    // The owner as the keeper: refused.
    let (ix, accounts) = redeem(owner, owner, (true, true), &args, 2);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::NotKeeper));
    for signed in [(false, true), (true, false)] {
        let (ix, accounts) = redeem(pk(&KEEPER), owner, signed, &args, 2);
        let result = run(&mollusk, &ix, &accounts);
        assert!(
            matches!(result, ProgramResult::Failure(_))
                && result != ProgramResult::Failure(code(LendError::InvalidProof)),
            "{signed:?}: {result:?}"
        );
    }
    let (ix, accounts) = redeem(pk(&KEEPER), owner, (true, true), &args, 2);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::InvalidProof));
}

#[test]
fn redeem_refuses_a_stale_now_and_a_transact_without_inputs() {
    let mollusk = setup();
    let owner = Pubkey::new_unique();
    for now in [NOW - 121, NOW + 1] {
        let (ix, accounts) = redeem(
            pk(&KEEPER),
            owner,
            (true, true),
            &redeem_args(&owner, now),
            2,
        );
        expect_err_exact(&mollusk, &ix, &accounts, code(LendError::StaleNow));
    }
    let (ix, accounts) = redeem(
        pk(&KEEPER),
        owner,
        (true, true),
        &redeem_args(&owner, NOW),
        0,
    );
    expect_err_exact(
        &mollusk,
        &ix,
        &accounts,
        code(LendError::InvalidInstructionData),
    );
    let (mut ix, accounts) = redeem(
        pk(&KEEPER),
        owner,
        (true, true),
        &redeem_args(&owner, NOW),
        2,
    );
    ix.data.truncate(40);
    expect_err_exact(
        &mollusk,
        &ix,
        &accounts,
        code(LendError::InvalidInstructionData),
    );
}

/// A bump below the canonical one that still derives a PDA.
fn non_canonical(seed: &[u8], key: &[u8; 32]) -> u8 {
    let id = pk(&keeler_lend::ID);
    (0..bump(seed, key))
        .rev()
        .find(|b| Pubkey::create_program_address(&[seed, key, &[*b]], &id).is_ok())
        .unwrap()
}

#[test]
fn supply_and_redeem_refuse_a_non_canonical_bump_and_a_foreign_mint() {
    let mollusk = setup();
    let user = Pubkey::new_unique();
    let foreign = [9; 32];
    let keeper = pk(&KEEPER);
    let supplies = [
        (
            SupplyArgs {
                pool_bump: non_canonical(POOL_SEED, &MINT),
                ..supply_args(&user, NOW, NOW)
            },
            LendError::InvalidSeeds,
        ),
        (
            SupplyArgs {
                user_bump: non_canonical(USER_SEED, &user.to_bytes()),
                ..supply_args(&user, NOW, NOW)
            },
            LendError::InvalidSeeds,
        ),
        (
            SupplyArgs {
                mint: foreign,
                pool_bump: bump(POOL_SEED, &foreign),
                ..supply_args(&user, NOW, NOW)
            },
            LendError::UnknownMint,
        ),
    ];
    for (args, err) in supplies {
        let (ix, accounts) = supply(keeper, true, &args, 2);
        expect_err_exact(&mollusk, &ix, &accounts, code(err));
    }
    let redeems = [
        (
            RedeemArgs {
                pool_bump: non_canonical(POOL_SEED, &MINT),
                ..redeem_args(&user, NOW)
            },
            LendError::InvalidSeeds,
        ),
        (
            RedeemArgs {
                user_bump: non_canonical(USER_SEED, &user.to_bytes()),
                ..redeem_args(&user, NOW)
            },
            LendError::InvalidSeeds,
        ),
        (
            RedeemArgs {
                mint: foreign,
                pool_bump: bump(POOL_SEED, &foreign),
                ..redeem_args(&user, NOW)
            },
            LendError::UnknownMint,
        ),
    ];
    for (args, err) in redeems {
        let (ix, accounts) = redeem(keeper, user, (true, true), &args, 2);
        expect_err_exact(&mollusk, &ix, &accounts, code(err));
    }
}
