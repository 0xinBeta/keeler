//! The Vault instructions' refusals that need no real proof, and the price
//! check, on the SBF build in `lend/target/deploy` (a `test-price` build:
//! `scripts/setup-lend.sh` passes the feature).
mod mollusk;

use keeler_lend::{
    error::LendError,
    instructions::{
        close::CloseArgs,
        deposit::DepositArgs,
        draw::DrawArgs,
        liquidate::LiquidateArgs,
        repay::RepayArgs,
        vault::{test_price_address, SetTestPriceArgs, PRICE_UPDATE_V2},
    },
    tag,
    vaults::{VaultError, PYTH_RECEIVER, TSOL_TUSD},
    KEEPERS, POOL_VIEWING, POOL_SEED, USER_SEED,
};
use mollusk::{expect_err_exact, mollusk_with_program};
use mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk};
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
const NOW: i64 = 1_800_000_000;
/// $150 with Pyth's SOL/USD exponent, -8.
const PYTH_150: i64 = 15_000_000_000;
const PYTH_CONF: u64 = 10_000_000;
/// `PYTH_150 -/+ PYTH_CONF` scaled to tUSD base units per tSOL base unit x 1e12.
const LOW: u64 = 149_900_000_000;
const HIGH: u64 = 150_100_000_000;

fn setup() -> Mollusk {
    let (mut mollusk, _) =
        mollusk_with_program(SBF_DIR, *keeler_lend::ID.as_array(), "keeler_lend");
    mollusk.sysvars.clock.unix_timestamp = NOW;
    mollusk
}

fn pk(a: &solana_address::Address) -> Pubkey {
    Pubkey::new_from_array(*a.as_array())
}
fn id() -> Pubkey {
    pk(&keeler_lend::ID)
}
fn account() -> Account {
    Account {
        lamports: 1_000_000_000,
        ..Account::default()
    }
}
fn lend(e: LendError) -> ProgramError {
    ProgramError::Custom(e as u32)
}
fn vault_err(e: VaultError) -> ProgramError {
    ProgramError::Custom(e as u32)
}
fn bump(seed: &[u8], key: &[u8; 32]) -> u8 {
    Pubkey::find_program_address(&[seed, key], &id()).1
}
fn proof() -> CompressedProof {
    CompressedProof {
        a: [0; 32],
        b: [0; 64],
        c: [0; 32],
    }
}

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
        circuit: CircuitId::ConfidentialSigner(1, 2, N_PUBLIC_SLOTS as u8),
        proof: TransactProof::zeroed(),
        inputs: (0..inputs)
            .map(|i| InputUtxo {
                nullifier_hash: [i as u8 + 4; 32],
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

fn data(ix: u8, args: Vec<u8>, inputs: usize) -> Vec<u8> {
    let mut data = vec![ix];
    data.extend(args);
    data.extend(transact(inputs));
    data
}

fn run(mollusk: &Mollusk, ix: &Instruction, accounts: &[(Pubkey, Account)]) -> ProgramResult {
    mollusk.process_instruction(ix, accounts).program_result
}

fn pool_bump() -> u8 {
    bump(POOL_SEED, TSOL_TUSD.debt_mint.as_array())
}

// --- deposit, repay: keeper, now, expiry, vault, inputs ---

fn deposit(
    ix_tag: u8,
    keeper: Pubkey,
    vault: u64,
    now: u64,
    expiry: u64,
    inputs: usize,
) -> Instruction {
    let owner = [7; 32];
    let args = DepositArgs {
        proof: proof(),
        vault,
        owner,
        owner_viewing: [2; 33],
        user_bump: bump(USER_SEED, &owner),
        now,
        expiry,
    };
    Instruction {
        program_id: id(),
        accounts: vec![AccountMeta::new(keeper, true)],
        data: data(ix_tag, wincode::serialize(&args).unwrap(), inputs),
    }
}

#[test]
fn deposit_refusals_before_the_proof() {
    let mollusk = setup();
    let keeper = pk(&KEEPERS[0]);
    let other = Pubkey::new_unique();
    let now = NOW as u64;
    for ix_tag in [tag::DEPOSIT, tag::DEPOSIT_ADD] {
        let accounts = [(keeper, account()), (other, account())];
        let cases = [
            (
                deposit(ix_tag, other, 1, now, now, 1),
                lend(LendError::NotKeeper),
            ),
            (
                deposit(ix_tag, keeper, 1, now - 121, now, 1),
                lend(LendError::StaleNow),
            ),
            (
                deposit(ix_tag, keeper, 1, now + 1, now + 1, 1),
                lend(LendError::StaleNow),
            ),
            (
                deposit(ix_tag, keeper, 1, now - 10, now - 1, 1),
                vault_err(VaultError::IntentExpired),
            ),
            (
                deposit(ix_tag, keeper, 2, now, now, 1),
                vault_err(VaultError::UnknownVault),
            ),
            (
                deposit(ix_tag, keeper, 1, now, now, 0),
                vault_err(VaultError::NoInputs),
            ),
            // The expiry is the clock's second: still fillable.
            (
                deposit(ix_tag, keeper, 1, now, now, 1),
                lend(LendError::InvalidProof),
            ),
        ];
        for (ix, err) in cases {
            expect_err_exact(&mollusk, &ix, &accounts, err);
        }
    }
}

fn repay(keeper: Pubkey, now: u64, expiry: u64, inputs: usize) -> Instruction {
    let owner = [7; 32];
    let args = RepayArgs {
        proof: proof(),
        vault: 1,
        pool_viewing: POOL_VIEWING,
        pool_bump: pool_bump(),
        owner,
        owner_viewing: [2; 33],
        user_bump: bump(USER_SEED, &owner),
        now,
        expiry,
    };
    Instruction {
        program_id: id(),
        accounts: vec![AccountMeta::new(keeper, true)],
        data: data(tag::REPAY, wincode::serialize(&args).unwrap(), inputs),
    }
}

#[test]
fn repay_refusals_before_the_proof() {
    let mollusk = setup();
    let keeper = pk(&KEEPERS[0]);
    let other = Pubkey::new_unique();
    let now = NOW as u64;
    let accounts = [(keeper, account()), (other, account())];
    for (ix, err) in [
        (repay(other, now, now, 1), lend(LendError::NotKeeper)),
        (repay(keeper, now - 121, now, 1), lend(LendError::StaleNow)),
        (
            repay(keeper, now - 5, now - 1, 1),
            vault_err(VaultError::IntentExpired),
        ),
        (repay(keeper, now, now, 0), vault_err(VaultError::NoInputs)),
        (
            repay(keeper, now, now + 60, 1),
            lend(LendError::InvalidProof),
        ),
    ] {
        expect_err_exact(&mollusk, &ix, &accounts, err);
    }
}

/// A bump below the canonical one that still derives a PDA.
fn non_canonical(seed: &[u8], key: &[u8; 32]) -> u8 {
    (0..bump(seed, key))
        .rev()
        .find(|b| Pubkey::create_program_address(&[seed, key, &[*b]], &id()).is_ok())
        .unwrap()
}

#[test]
fn vault_instructions_refuse_a_non_canonical_bump() {
    let mollusk = setup();
    let keeper = pk(&KEEPERS[0]);
    let now = NOW as u64;
    let owner = [7; 32];
    let (bad_pool, bad_user) = (
        non_canonical(POOL_SEED, TSOL_TUSD.debt_mint.as_array()),
        non_canonical(USER_SEED, &owner),
    );
    let ix = |ix_tag: u8, args: Vec<u8>| Instruction {
        program_id: id(),
        accounts: vec![AccountMeta::new(keeper, true)],
        data: data(ix_tag, args, 1),
    };
    let accounts = [(keeper, account())];
    let deposit = DepositArgs {
        proof: proof(),
        vault: 1,
        owner,
        owner_viewing: [2; 33],
        user_bump: bad_user,
        now,
        expiry: now,
    };
    for ix_tag in [tag::DEPOSIT, tag::DEPOSIT_ADD] {
        expect_err_exact(
            &mollusk,
            &ix(ix_tag, wincode::serialize(&deposit).unwrap()),
            &accounts,
            lend(LendError::InvalidSeeds),
        );
    }
    let repay = |pool_bump, user_bump| RepayArgs {
        proof: proof(),
        vault: 1,
        pool_viewing: POOL_VIEWING,
        pool_bump,
        owner,
        owner_viewing: [2; 33],
        user_bump,
        now,
        expiry: now,
    };
    for args in [
        repay(bad_pool, bump(USER_SEED, &owner)),
        repay(pool_bump(), bad_user),
    ] {
        expect_err_exact(
            &mollusk,
            &ix(tag::REPAY, wincode::serialize(&args).unwrap()),
            &accounts,
            lend(LendError::InvalidSeeds),
        );
    }
    // Liquidate, past a good price check.
    let feed = pk(&TSOL_TUSD.price_account);
    let mut liq = liquidate(keeper, feed, now, HIGH);
    let args = LiquidateArgs {
        proof: proof(),
        vault: 1,
        pool_viewing: POOL_VIEWING,
        pool_bump: bad_pool,
        owner,
        owner_viewing: [2; 33],
        user_bump: bump(USER_SEED, &owner),
        now,
        bound: HIGH,
    };
    liq.data = data(tag::LIQUIDATE, wincode::serialize(&args).unwrap(), 1);
    expect_err_exact(
        &mollusk,
        &liq,
        &[(keeper, account()), (feed, good_pyth())],
        lend(LendError::InvalidSeeds),
    );
}

// --- the price account ---

fn pyth_data(price: i64, conf: u64, expo: i32, published: i64) -> Vec<u8> {
    let mut d = vec![0u8; 134];
    d[..8].copy_from_slice(&PRICE_UPDATE_V2);
    d[40] = 1;
    d[41..73].copy_from_slice(&TSOL_TUSD.feed_id);
    d[73..81].copy_from_slice(&price.to_le_bytes());
    d[81..89].copy_from_slice(&conf.to_le_bytes());
    d[89..93].copy_from_slice(&expo.to_le_bytes());
    d[93..101].copy_from_slice(&published.to_le_bytes());
    d
}

fn pyth_account(data: Vec<u8>) -> Account {
    Account {
        lamports: 1_000_000_000,
        data,
        owner: pk(&PYTH_RECEIVER),
        ..Account::default()
    }
}

fn good_pyth() -> Account {
    pyth_account(pyth_data(PYTH_150, PYTH_CONF, -8, NOW - 30))
}

fn draw(
    keeper: Pubkey,
    owner: Pubkey,
    owner_signs: bool,
    price: Pubkey,
    now: u64,
    bound: u64,
) -> Instruction {
    let args = DrawArgs {
        proof: proof(),
        vault: 1,
        pool_viewing: POOL_VIEWING,
        pool_bump: pool_bump(),
        owner_viewing: [2; 33],
        user_bump: bump(USER_SEED, &owner.to_bytes()),
        now,
        bound,
    };
    Instruction {
        program_id: id(),
        accounts: vec![
            AccountMeta::new(keeper, true),
            AccountMeta::new_readonly(owner, owner_signs),
            AccountMeta::new_readonly(price, false),
        ],
        data: data(tag::DRAW, wincode::serialize(&args).unwrap(), 1),
    }
}

fn liquidate(keeper: Pubkey, price: Pubkey, now: u64, bound: u64) -> Instruction {
    let owner = [7; 32];
    let args = LiquidateArgs {
        proof: proof(),
        vault: 1,
        pool_viewing: POOL_VIEWING,
        pool_bump: pool_bump(),
        owner,
        owner_viewing: [2; 33],
        user_bump: bump(USER_SEED, &owner),
        now,
        bound,
    };
    Instruction {
        program_id: id(),
        accounts: vec![
            AccountMeta::new(keeper, true),
            AccountMeta::new_readonly(price, false),
        ],
        data: data(tag::LIQUIDATE, wincode::serialize(&args).unwrap(), 1),
    }
}

/// Draw at `bound` with `price` as the vault's Pyth account.
fn draw_with(mollusk: &Mollusk, price: Account, bound: u64) -> ProgramResult {
    let (keeper, owner, feed) = (
        pk(&KEEPERS[0]),
        Pubkey::new_unique(),
        pk(&TSOL_TUSD.price_account),
    );
    run(
        mollusk,
        &draw(keeper, owner, true, feed, NOW as u64, bound),
        &[(keeper, account()), (owner, account()), (feed, price)],
    )
}

fn fails(e: ProgramError) -> ProgramResult {
    ProgramResult::Failure(e)
}

#[test]
fn draw_needs_the_keeper_and_the_owner() {
    let mollusk = setup();
    let (keeper, owner, feed) = (
        pk(&KEEPERS[0]),
        Pubkey::new_unique(),
        pk(&TSOL_TUSD.price_account),
    );
    let accounts = [(keeper, account()), (owner, account()), (feed, good_pyth())];
    let ix = draw(owner, keeper, true, feed, NOW as u64, LOW);
    expect_err_exact(&mollusk, &ix, &accounts, lend(LendError::NotKeeper));
    // Unsigned by the owner: the account checks refuse it.
    let ix = draw(keeper, owner, false, feed, NOW as u64, LOW);
    assert!(matches!(
        run(&mollusk, &ix, &accounts),
        ProgramResult::Failure(_)
    ));
    let ix = draw(keeper, owner, true, feed, NOW as u64 - 121, LOW);
    expect_err_exact(&mollusk, &ix, &accounts, lend(LendError::StaleNow));
    let ix = draw(keeper, owner, true, feed, NOW as u64, LOW);
    expect_err_exact(&mollusk, &ix, &accounts, lend(LendError::InvalidProof));
}

#[test]
fn draw_bound_at_most_price_minus_confidence() {
    let mollusk = setup();
    assert_eq!(
        draw_with(&mollusk, good_pyth(), LOW),
        fails(lend(LendError::InvalidProof))
    );
    assert_eq!(
        draw_with(&mollusk, good_pyth(), LOW + 1),
        fails(vault_err(VaultError::BoundAbovePrice))
    );
}

#[test]
fn liquidate_bound_at_least_price_plus_confidence() {
    let mollusk = setup();
    let (keeper, feed) = (pk(&KEEPERS[0]), pk(&TSOL_TUSD.price_account));
    let accounts = [(keeper, account()), (feed, good_pyth())];
    let other = Pubkey::new_unique();
    expect_err_exact(
        &mollusk,
        &liquidate(other, feed, NOW as u64, HIGH),
        &[(other, account()), (feed, good_pyth())],
        lend(LendError::NotKeeper),
    );
    expect_err_exact(
        &mollusk,
        &liquidate(keeper, feed, NOW as u64, HIGH - 1),
        &accounts,
        vault_err(VaultError::BoundBelowPrice),
    );
    expect_err_exact(
        &mollusk,
        &liquidate(keeper, feed, NOW as u64, HIGH),
        &accounts,
        lend(LendError::InvalidProof),
    );
}

#[test]
fn pyth_account_refusals() {
    let mollusk = setup();
    let bound = LOW;
    let check = |account: Account, err: VaultError| {
        assert_eq!(
            draw_with(&mollusk, account, bound),
            fails(vault_err(err)),
            "{err:?}"
        );
    };
    // Owner, discriminator, length, verification level, feed.
    let mut a = good_pyth();
    a.owner = Pubkey::new_unique();
    check(a, VaultError::WrongPriceOwner);
    let mut d = pyth_data(PYTH_150, PYTH_CONF, -8, NOW);
    d[0] ^= 1;
    check(pyth_account(d), VaultError::NotPriceUpdate);
    check(
        pyth_account(pyth_data(PYTH_150, PYTH_CONF, -8, NOW)[..100].to_vec()),
        VaultError::NotPriceUpdate,
    );
    let mut d = pyth_data(PYTH_150, PYTH_CONF, -8, NOW);
    d[40] = 0;
    check(pyth_account(d), VaultError::PriceNotVerified);
    let mut d = pyth_data(PYTH_150, PYTH_CONF, -8, NOW);
    d[50] ^= 1;
    check(pyth_account(d), VaultError::WrongFeed);
    // Age: 90 s either way is the limit.
    check(
        pyth_account(pyth_data(PYTH_150, PYTH_CONF, -8, NOW - 91)),
        VaultError::StalePrice,
    );
    check(
        pyth_account(pyth_data(PYTH_150, PYTH_CONF, -8, NOW + 91)),
        VaultError::StalePrice,
    );
    check(
        pyth_account(pyth_data(PYTH_150, PYTH_CONF, -8, i64::MIN)),
        VaultError::StalePrice,
    );
    for published in [NOW - 90, NOW + 90] {
        assert_eq!(
            draw_with(
                &mollusk,
                pyth_account(pyth_data(PYTH_150, PYTH_CONF, -8, published)),
                bound
            ),
            fails(lend(LendError::InvalidProof))
        );
    }
    // Non-positive prices, confidence as wide as the price, exponents that do not fit.
    for price in [0, -1, i64::MIN] {
        check(
            pyth_account(pyth_data(price, 0, -8, NOW)),
            VaultError::BadPrice,
        );
    }
    check(
        pyth_account(pyth_data(PYTH_150, PYTH_150 as u64, -8, NOW)),
        VaultError::BadPrice,
    );
    for expo in [30, i32::MAX, i32::MIN] {
        check(
            pyth_account(pyth_data(PYTH_150, PYTH_CONF, expo, NOW)),
            VaultError::PriceOverflow,
        );
    }
    // Any other account, even with a good update in it.
    let (keeper, owner, elsewhere) = (pk(&KEEPERS[0]), Pubkey::new_unique(), Pubkey::new_unique());
    assert_eq!(
        run(
            &mollusk,
            &draw(keeper, owner, true, elsewhere, NOW as u64, bound),
            &[
                (keeper, account()),
                (owner, account()),
                (elsewhere, good_pyth())
            ],
        ),
        fails(vault_err(VaultError::WrongPriceAccount))
    );
}

// --- the test price account ---

fn set_test_price(
    keeper: Pubkey,
    account: Pubkey,
    price: u64,
    conf: u64,
    published: i64,
) -> Instruction {
    let mut data = vec![tag::SET_TEST_PRICE];
    data.extend(
        wincode::serialize(&SetTestPriceArgs {
            vault: 1,
            price,
            confidence: conf,
            published,
        })
        .unwrap(),
    );
    Instruction {
        program_id: id(),
        accounts: vec![
            AccountMeta::new(keeper, true),
            AccountMeta::new(account, false),
            AccountMeta::new_readonly(keyed_account_for_system_program().0, false),
        ],
        data,
    }
}

#[test]
fn test_price_is_the_keepers_and_is_checked_like_pyth() {
    let mollusk = setup();
    let keeper = pk(&KEEPERS[0]);
    let test = pk(&test_price_address(1).0);
    let system = keyed_account_for_system_program();
    let empty = Account::default();
    let other = Pubkey::new_unique();
    expect_err_exact(
        &mollusk,
        &set_test_price(other, test, LOW, 0, NOW),
        &[(other, account()), (test, empty.clone()), system.clone()],
        lend(LendError::NotKeeper),
    );
    let wrong = Pubkey::new_unique();
    expect_err_exact(
        &mollusk,
        &set_test_price(keeper, wrong, LOW, 0, NOW),
        &[(keeper, account()), (wrong, empty.clone()), system.clone()],
        vault_err(VaultError::WrongTestPriceAccount),
    );
    // Created on first use, then written again in place.
    let result = mollusk.process_instruction(
        &set_test_price(keeper, test, 150_000_000_000, 100_000_000, NOW - 10),
        &[(keeper, account()), (test, empty), system.clone()],
    );
    assert_eq!(result.program_result, ProgramResult::Success);
    let created = result.get_account(&test).unwrap().clone();
    assert_eq!((created.owner, created.data.len()), (id(), 24));
    let result = mollusk.process_instruction(
        &set_test_price(keeper, test, 150_000_000_000, 100_000_000, NOW - 100),
        &[(keeper, account()), (test, created.clone()), system],
    );
    assert_eq!(result.program_result, ProgramResult::Success);
    let stale = result.get_account(&test).unwrap().clone();

    // Draw reads it: `price - conf` is the highest bound.
    let owner = Pubkey::new_unique();
    let draw_at = |price: Account, bound: u64| {
        run(
            &mollusk,
            &draw(keeper, owner, true, test, NOW as u64, bound),
            &[(keeper, account()), (owner, account()), (test, price)],
        )
    };
    assert_eq!(
        draw_at(created.clone(), 149_900_000_000),
        fails(lend(LendError::InvalidProof))
    );
    assert_eq!(
        draw_at(created.clone(), 149_900_000_001),
        fails(vault_err(VaultError::BoundAbovePrice))
    );
    assert_eq!(draw_at(stale, 1), fails(vault_err(VaultError::StalePrice)));
    // Not the program's: refused.
    let mut forged = created;
    forged.owner = Pubkey::new_unique();
    assert_eq!(
        draw_at(forged, 1),
        fails(vault_err(VaultError::WrongTestPriceAccount))
    );
}

#[test]
fn test_price_account_is_created_when_the_address_already_holds_lamports() {
    let mollusk = setup();
    let keeper = pk(&KEEPERS[0]);
    let test = pk(&test_price_address(1).0);
    let rent = mollusk.sysvars.rent.minimum_balance(24);
    // One lamport sent ahead (the keeper tops it up), and more than the rent.
    for held in [1, rent + 5] {
        let prefunded = Account {
            lamports: held,
            ..Account::default()
        };
        let result = mollusk.process_instruction(
            &set_test_price(keeper, test, 150_000_000_000, 0, NOW),
            &[
                (keeper, account()),
                (test, prefunded),
                keyed_account_for_system_program(),
            ],
        );
        assert_eq!(result.program_result, ProgramResult::Success, "{held}");
        let created = result.get_account(&test).unwrap();
        assert_eq!((created.owner, created.data.len()), (id(), 24));
        assert_eq!(created.lamports, held.max(rent));
        assert_eq!(&created.data[..8], &150_000_000_000u64.to_le_bytes());
    }
}

// --- close ---

#[test]
fn close_is_the_owners() {
    let mollusk = setup();
    let owner = Pubkey::new_unique();
    let close = |vault: u64, signs: bool| Instruction {
        program_id: id(),
        accounts: vec![AccountMeta::new(owner, signs)],
        data: data(
            tag::CLOSE,
            wincode::serialize(&CloseArgs {
                proof: proof(),
                vault,
                viewing: [2; 33],
                user_bump: bump(USER_SEED, &owner.to_bytes()),
            })
            .unwrap(),
            1,
        ),
    };
    let accounts = [(owner, account())];
    expect_err_exact(
        &mollusk,
        &close(2, true),
        &accounts,
        vault_err(VaultError::UnknownVault),
    );
    assert!(matches!(
        run(&mollusk, &close(1, false), &accounts),
        ProgramResult::Failure(_)
    ));
    expect_err_exact(
        &mollusk,
        &close(1, true),
        &accounts,
        lend(LendError::InvalidProof),
    );
}
