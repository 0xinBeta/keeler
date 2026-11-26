//! The program's refusals that need no real proof, on the SBF build in
//! `lend/target/deploy` (build it first, see `lend/AGENTS.md`).
#[path = "../../../.tools/privacy-ring-examples/test-utils/mollusk.rs"]
mod mollusk;
mod pool_viewing;

use keeler_lend::{
    error::LendError,
    instructions::{
        cancel_intent::CancelIntentArgs, init_reserve::InitReserveArgs, intent::IntentArgs,
    },
    tag, ADMIN, KEEPERS, POOL_SEED, POOL_VIEWING, USER_SEED,
};
use mollusk::{expect_err_exact, mollusk_with_program};
use mollusk_svm::{result::ProgramResult, Mollusk};
use pool_viewing::only_the_pool_viewing_key;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;
use spr_interface::{
    instruction::instruction_data::transact::{
        CircuitId, TransactIxData, TransactProof, TreeContext,
    },
    N_PUBLIC_SLOTS,
};
use spr_program::CompressedProof;

const SBF_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../target/deploy");
const MINT: [u8; 32] = keeler_lend::pool::TUSD_MINT.to_bytes();
const NOW: i64 = 1_800_000_000;

fn setup() -> (Mollusk, Pubkey) {
    let (mut mollusk, id) =
        mollusk_with_program(SBF_DIR, *keeler_lend::ID.as_array(), "keeler_lend");
    mollusk.sysvars.clock.unix_timestamp = NOW;
    (mollusk, id)
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

fn transact() -> Vec<u8> {
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
        circuit: CircuitId::ConfidentialSigner(1, 2, N_PUBLIC_SLOTS as u8),
        proof: TransactProof::zeroed(),
        inputs: Vec::new(),
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

fn data(ix: u8, args: Vec<u8>) -> Vec<u8> {
    let mut data = vec![ix];
    data.extend(args);
    data.extend(transact());
    data
}

fn bump(seed: &[u8], key: &[u8; 32]) -> u8 {
    Pubkey::find_program_address(&[seed, key], &pk(&keeler_lend::ID)).1
}

fn init_reserve_args(now: u64) -> Vec<u8> {
    wincode::serialize(&InitReserveArgs {
        proof: proof(),
        mint: MINT,
        pool_viewing: POOL_VIEWING,
        pool_bump: bump(POOL_SEED, &MINT),
        now,
    })
    .unwrap()
}

fn init_reserve(
    keeper: Pubkey,
    admin: Pubkey,
    signers: bool,
    now: u64,
) -> (Instruction, Vec<(Pubkey, Account)>) {
    let ix = Instruction {
        program_id: pk(&keeler_lend::ID),
        accounts: vec![
            AccountMeta::new(keeper, signers),
            AccountMeta::new_readonly(admin, signers),
        ],
        data: data(tag::INIT_RESERVE, init_reserve_args(now)),
    };
    (ix, vec![(keeper, account()), (admin, account())])
}

fn run(mollusk: &Mollusk, ix: &Instruction, accounts: &[(Pubkey, Account)]) -> ProgramResult {
    mollusk.process_instruction(ix, accounts).program_result
}

#[test]
fn dispatch_refuses_empty_and_unknown_tags() {
    let (mollusk, id) = setup();
    let user = Pubkey::new_unique();
    let accounts = [(user, account())];
    for (bytes, err) in [
        (vec![], LendError::InvalidInstructionData),
        (vec![0], LendError::UnknownInstruction),
        (vec![13], LendError::UnknownInstruction),
    ] {
        let ix = Instruction::new_with_bytes(id, &bytes, vec![AccountMeta::new(user, true)]);
        expect_err_exact(&mollusk, &ix, &accounts, code(err));
    }
}

#[test]
fn init_reserve_needs_the_keeper_and_the_admin() {
    let (mollusk, _) = setup();
    let (keeper, admin, other) = (pk(&KEEPERS[0]), pk(&ADMIN), Pubkey::new_unique());
    let now = NOW as u64;
    let (ix, accounts) = init_reserve(other, admin, true, now);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::NotKeeper));
    let (ix, accounts) = init_reserve(keeper, other, true, now);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::NotAdmin));
    // Admin as keeper and keeper as admin: both are refused.
    let (ix, accounts) = init_reserve(admin, keeper, true, now);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::NotKeeper));
    // Unsigned: the account checks refuse it before the keeper check.
    let (ix, accounts) = init_reserve(keeper, admin, false, now);
    assert!(matches!(
        run(&mollusk, &ix, &accounts),
        ProgramResult::Failure(_)
    ));
}

#[test]
fn init_reserve_refuses_a_stale_or_future_now_and_a_bad_proof() {
    let (mollusk, _) = setup();
    let (keeper, admin) = (pk(&KEEPERS[0]), pk(&ADMIN));
    for now in [NOW - 121, NOW + 1] {
        let (ix, accounts) = init_reserve(keeper, admin, true, now as u64);
        expect_err_exact(&mollusk, &ix, &accounts, code(LendError::StaleNow));
    }
    let (ix, accounts) = init_reserve(keeper, admin, true, NOW as u64 - 60);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::InvalidProof));
}

#[test]
fn init_reserve_refuses_truncated_data() {
    let (mollusk, _) = setup();
    let (mut ix, accounts) = init_reserve(pk(&KEEPERS[0]), pk(&ADMIN), true, NOW as u64);
    ix.data = vec![tag::INIT_RESERVE, 1, 2, 3];
    expect_err_exact(
        &mollusk,
        &ix,
        &accounts,
        code(LendError::InvalidInstructionData),
    );
    // Args parse, the transact data after them does not.
    let mut data = vec![tag::INIT_RESERVE];
    data.extend(init_reserve_args(NOW as u64));
    data.extend(&transact()[..10]);
    ix.data = data;
    expect_err_exact(
        &mollusk,
        &ix,
        &accounts,
        code(LendError::InvalidInstructionData),
    );
}

/// A bump below the canonical one that still derives a PDA: another address
/// for the same seeds.
fn non_canonical(seed: &[u8], key: &[u8; 32]) -> u8 {
    let id = pk(&keeler_lend::ID);
    (0..bump(seed, key))
        .rev()
        .find(|b| Pubkey::create_program_address(&[seed, key, &[*b]], &id).is_ok())
        .unwrap()
}

#[test]
fn init_reserve_refuses_any_bump_but_the_canonical_one() {
    let (mollusk, _) = setup();
    let (mut ix, accounts) = init_reserve(pk(&KEEPERS[0]), pk(&ADMIN), true, NOW as u64);
    let id = pk(&keeler_lend::ID);
    let no_pda = (0..=u8::MAX)
        .rev()
        .find(|b| Pubkey::create_program_address(&[POOL_SEED, &MINT, &[*b]], &id).is_err())
        .unwrap();
    // A second pool PDA (and so a second reserve) for the same mint: refused.
    for bad in [no_pda, non_canonical(POOL_SEED, &MINT)] {
        let args = InitReserveArgs {
            proof: proof(),
            mint: MINT,
            pool_viewing: POOL_VIEWING,
            pool_bump: bad,
            now: NOW as u64,
        };
        ix.data = data(tag::INIT_RESERVE, wincode::serialize(&args).unwrap());
        expect_err_exact(&mollusk, &ix, &accounts, code(LendError::InvalidSeeds));
    }
}

#[test]
fn init_reserve_refuses_a_mint_other_than_the_pools() {
    let (mollusk, _) = setup();
    let (mut ix, accounts) = init_reserve(pk(&KEEPERS[0]), pk(&ADMIN), true, NOW as u64);
    let foreign = [9; 32];
    let args = InitReserveArgs {
        proof: proof(),
        mint: foreign,
        pool_viewing: POOL_VIEWING,
        pool_bump: bump(POOL_SEED, &foreign),
        now: NOW as u64,
    };
    ix.data = data(tag::INIT_RESERVE, wincode::serialize(&args).unwrap());
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::UnknownMint));
}

#[test]
fn init_reserve_takes_every_keeper_and_only_the_pool_viewing_key() {
    let (mollusk, _) = setup();
    let admin = pk(&ADMIN);
    assert!(
        (2..=4).contains(&KEEPERS.len()),
        "the test build has a keeper set"
    );
    for keeper in KEEPERS {
        let (ix, accounts) = init_reserve(pk(keeper), admin, true, NOW as u64 - 60);
        only_the_pool_viewing_key(&mollusk, &ix, &accounts, code(LendError::InvalidProof));
    }
    let (ix, accounts) = init_reserve(Pubkey::new_unique(), admin, true, NOW as u64 - 60);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::NotKeeper));
    // A keeper is not the admin.
    let (ix, accounts) = init_reserve(pk(&KEEPERS[1]), pk(&KEEPERS[0]), true, NOW as u64);
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::NotAdmin));
}

#[test]
fn intent_and_cancel_refuse_a_non_canonical_user_bump() {
    let (mollusk, _) = setup();
    let user = Pubkey::new_unique();
    let bad = non_canonical(USER_SEED, &user.to_bytes());
    let intent = IntentArgs {
        proof: proof(),
        mint: MINT,
        viewing: [2; 33],
        user_bump: bad,
    };
    let mut ix = Instruction {
        program_id: pk(&keeler_lend::ID),
        accounts: vec![AccountMeta::new(user, true)],
        data: data(tag::INTENT, wincode::serialize(&intent).unwrap()),
    };
    let accounts = [(user, account())];
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::InvalidSeeds));
    let cancel = CancelIntentArgs {
        proof: proof(),
        viewing: [2; 33],
        user_bump: bad,
        expiry: NOW as u64 - 1,
    };
    ix.data = data(tag::CANCEL_INTENT, wincode::serialize(&cancel).unwrap());
    expect_err_exact(&mollusk, &ix, &accounts, code(LendError::InvalidSeeds));
}

fn cancel(user: Pubkey, expiry: u64) -> Instruction {
    let args = CancelIntentArgs {
        proof: proof(),
        viewing: [2; 33],
        user_bump: bump(USER_SEED, &user.to_bytes()),
        expiry,
    };
    Instruction {
        program_id: pk(&keeler_lend::ID),
        accounts: vec![AccountMeta::new(user, true)],
        data: data(tag::CANCEL_INTENT, wincode::serialize(&args).unwrap()),
    }
}

#[test]
fn cancel_intent_waits_for_the_expiry() {
    let (mollusk, _) = setup();
    let user = Pubkey::new_unique();
    let accounts = [(user, account())];
    for expiry in [NOW as u64, NOW as u64 + 1, u64::MAX] {
        expect_err_exact(
            &mollusk,
            &cancel(user, expiry),
            &accounts,
            code(LendError::NotExpired),
        );
    }
    // After the expiry it reaches the proof.
    expect_err_exact(
        &mollusk,
        &cancel(user, NOW as u64 - 1),
        &accounts,
        code(LendError::InvalidProof),
    );
}

#[test]
fn intent_refuses_a_bad_proof() {
    let (mollusk, _) = setup();
    let user = Pubkey::new_unique();
    let args = IntentArgs {
        proof: proof(),
        mint: MINT,
        viewing: [2; 33],
        user_bump: bump(USER_SEED, &user.to_bytes()),
    };
    let ix = Instruction {
        program_id: pk(&keeler_lend::ID),
        accounts: vec![AccountMeta::new(user, true)],
        data: data(tag::INTENT, wincode::serialize(&args).unwrap()),
    };
    expect_err_exact(
        &mollusk,
        &ix,
        &[(user, account())],
        code(LendError::InvalidProof),
    );
}
