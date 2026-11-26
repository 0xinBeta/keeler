//! The spike on a local network: see `../../../docs/notes-for-xark-builders.md`.
//!
//! Run with `ZOLANA_PORT_OFFSET` in 2100..2500 and the matching
//! `ZOLANA_PROVER_URL` (port 3001 + offset / 100), the tool paths from
//! `.tools/env`, and the SBF build in `lend/target/deploy`:
//! `cargo test --release -p lend-spike --features circuits --test spike -- --nocapture`.
#[path = "../../../.tools/privacy-ring-examples/test-utils/wallet.rs"]
mod wallet;

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use anyhow::{anyhow, ensure, Context, Result};
use lend_spike::{
    circuits::*,
    instructions::{FundArgs, PoolArgs, TouchArgs},
    tag, POOL_SEED, PRICE_ACCOUNT_LEN, USER_SEED,
};
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use spr_client::{ProofAuthority, Rpc, TransferInput};
use spr_interface::{
    instruction::instruction_data::transact::TransactIxData, SHIELDED_POOL_PROGRAM_ID,
};
use spr_keypair::ShieldedKeypair;
use spr_program::{
    instruction::nullifier_pda_accounts, proof::compressed_proof, CompressedProof,
    DataUtxo as ClientData, ProgramOwner, ProgramProofInputs, ProgramTransaction,
    TransactionOptions,
};
use spr_program_test::{
    fixture,
    localnet::{FixtureLocalnet, LocalnetPaths, LocalnetPorts},
    SprProgramTest,
};
use spr_transaction::{
    instructions::transact::SppProofInputs, AssetRegistry, Mint, SppProofOutputUtxo, WalletUtxo,
};
use wallet::{deposit, send, wait_for_merkle_proof, TestWallet};

const KEEPER_FUNDS: u64 = 1_000_000;
const USER_FUNDS: u64 = 1_000_000;
const ACCRUE_DEBT_SHARES: u64 = 100_000;
const POSITION_ID: u64 = 7;
const POSITION_AMOUNT: u64 = 200_000;
const POSITION_DEBT_SHARES: u64 = 50_000;
const PRICE: u64 = 1_000_000_001_000;
const CONFIDENCE: u64 = 1_000;
const PRICE_BOUND: u64 = 1_000_000_000_000;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn err(e: impl core::fmt::Debug) -> anyhow::Error {
    anyhow!("{e:?}")
}

fn localnet_paths() -> LocalnetPaths {
    LocalnetPaths {
        // Our own copy: the prover downloads the shapes it lacks into it.
        proving_keys_dir: root().join("target/proving-keys"),
        scratch_dir: root().join("target/localnet"),
        ..LocalnetPaths::workspace()
    }
}

/// The PDA owners, signers, and the zero nullifier key for their inputs.
struct Pda {
    owner: ProgramOwner,
    bump: u8,
}

impl Pda {
    fn new(seed: &[u8], key: &[u8; 32]) -> Self {
        let (_, bump) = Address::find_program_address(&[seed, key], &lend_spike::ID);
        Self {
            owner: ProgramOwner::find(&[seed, key], &lend_spike::ID),
            bump,
        }
    }
    fn pda(&self) -> Address {
        *self.owner.pda()
    }
}

/// PDA-owned inputs under the zero key, everything else under `wallet`.
struct Authority<'a>(&'a ShieldedKeypair);

impl ProofAuthority for Authority<'_> {
    fn complete_inputs(&self, inputs: &mut [TransferInput]) -> Result<(), spr_client::ClientError> {
        for input in inputs.iter_mut() {
            if input.nullifier_secret.is_some() {
                continue;
            }
            let input = core::slice::from_mut(input);
            if ProgramOwner::nullifier_key()
                .complete_inputs(input)
                .is_err()
            {
                self.0.complete_inputs(input)?;
            }
        }
        Ok(())
    }
}

struct Env {
    localnet: FixtureLocalnet,
    rpc_url: String,
    mint: Address,
    keeper: TestWallet,
    user: TestWallet,
}

/// A built transaction: the encrypted SPP inputs, the program proof inputs
/// and the outputs with their blindings.
struct Built {
    spp: SppProofInputs,
    proof_inputs: ProgramProofInputs,
    outputs: Vec<SppProofOutputUtxo>,
}

fn built(transaction: ProgramTransaction, sender: &ShieldedKeypair) -> Result<Built> {
    let finalized = transaction.finalized;
    let outputs = finalized.output_utxos().to_vec();
    let key = sender
        .get_transaction_viewing_key(&finalized.first_nullifier().map_err(err)?)
        .map_err(err)?;
    Ok(Built {
        spp: finalized.encrypt_with_viewing_key(&key).map_err(err)?,
        proof_inputs: transaction.proof_inputs,
        outputs,
    })
}

/// Proves `name` from the compiled artifacts, natively, and times it.
fn lend_proof(name: &str, inputs: &ProgramProofInputs) -> Result<CompressedProof> {
    // A private copy: Xark rebuilds a circuit whose sources look newer.
    let src = root().join("target/zk/lend-spike").join(name);
    let dir = root().join("target/spike-circuits").join(name);
    std::fs::create_dir_all(&dir)?;
    for file in ["circuit.xbc", "pk.bin", "sources.json"] {
        std::fs::copy(src.join(file), dir.join(file))?;
    }
    let started = Instant::now();
    let proven = xark::prover::circuit_at(&dir)
        .prove(inputs.clone())
        .map_err(err)?;
    println!("SPIKE {name} native proof: {:?}", started.elapsed());
    compressed_proof(&proven.proof).map_err(anyhow::Error::msg)
}

/// Proves the SPP side and returns it with its owner signers.
fn transact(
    env: &Env,
    spp: SppProofInputs,
    wallet: &ShieldedKeypair,
) -> Result<(TransactIxData, Vec<Address>)> {
    let owners = spp.owner_signer_pubkeys().map_err(err)?;
    let hash = spp.padding_independent_private_tx_hash().map_err(err)?;
    let started = Instant::now();
    let ix = env
        .localnet
        .client
        .indexer()
        .prove_transact(spp, &Authority(wallet))
        .map_err(err)?;
    println!(
        "SPIKE spp proof ({} in): {:?}",
        ix.inputs.len(),
        started.elapsed()
    );
    ensure!(
        ix.private_tx_hash == hash,
        "the two proofs cover different transactions"
    );
    Ok((ix, owners))
}

/// `[tag] args transact` with `head` accounts then the SPP layout; the PDAs
/// among the owner signers are passed as non-signers for the CPI to sign.
fn instruction(
    env: &Env,
    ix_tag: u8,
    args: Vec<u8>,
    head: Vec<AccountMeta>,
    payer: Address,
    spp: &TransactIxData,
    owners: &[Address],
    pdas: &[Address],
) -> Result<Instruction> {
    let tree = env.localnet.tree;
    let mut accounts = head;
    accounts.extend([
        AccountMeta::new(payer, true),
        AccountMeta::new(tree, false),
        AccountMeta::new_readonly(Address::new_from_array(SHIELDED_POOL_PROGRAM_ID), false),
        AccountMeta::new_readonly(Address::default(), false),
    ]);
    accounts.extend(
        spp.tree_contexts
            .iter()
            .map(|_| AccountMeta::new(tree, false)),
    );
    accounts.extend(nullifier_pda_accounts(
        &tree,
        spp.inputs.iter().map(|input| &input.nullifier_hash),
    ));
    for owner in owners {
        accounts.push(AccountMeta::new_readonly(*owner, !pdas.contains(owner)));
    }
    let mut data = vec![ix_tag];
    data.extend(args);
    data.extend(wincode::serialize(spp).map_err(err)?);
    Ok(Instruction {
        program_id: lend_spike::ID,
        accounts,
        data,
    })
}

/// Sends `ix` and prints its compute units and v1 size.
fn run(env: &Env, name: &str, signers: &[&dyn Signer], ix: Instruction) -> Result<()> {
    let payer = signers[0].pubkey();
    let size = spr_client::transaction_size(
        &payer,
        std::slice::from_ref(&ix),
        spr_client::ComputeBudgetConfig::new(1_400_000),
    )
    .map_err(err)?
    .bytes;
    let accounts = ix.accounts.len();
    let data = ix.data.len();
    let signature = send(env.localnet.client.rpc(), signers, ix)?;
    env.localnet
        .client
        .confirm_private_transaction_sync(signature)
        .map_err(|e| anyhow!("{name} indexed: {e:?}"))?;
    println!(
        "SPIKE {name}: {} CU, v1 tx {size} bytes, ix data {data} bytes, {accounts} accounts",
        compute_units(&env.rpc_url, &signature.to_string())?
    );
    Ok(())
}

fn compute_units(rpc: &str, signature: &str) -> Result<u64> {
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"getTransaction","params":["{signature}",{{"encoding":"json","maxSupportedTransactionVersion":1,"commitment":"confirmed"}}]}}"#
    );
    let out = Command::new("/usr/bin/curl")
        .args([
            "-s",
            "-H",
            "content-type: application/json",
            "-d",
            &body,
            rpc,
        ])
        .output()?;
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    value["result"]["meta"]["computeUnitsConsumed"]
        .as_u64()
        .with_context(|| format!("no compute units in {value}"))
}

/// The cluster's unix time, which the program checks `now` against.
fn chain_now(env: &Env) -> Result<u64> {
    let clock: Address = "SysvarC1ock11111111111111111111111111111111"
        .parse()
        .map_err(err)?;
    let account = env
        .localnet
        .client
        .rpc()
        .get_account(clock)?
        .context("no clock")?;
    let bytes: [u8; 8] = account.data[32..40].try_into()?;
    Ok(u64::try_from(i64::from_le_bytes(bytes))?)
}

fn halves(mint: &Address) -> (u128, u128) {
    let bytes = mint.to_bytes();
    let (hi, lo) = bytes.split_at(16);
    (
        u128::from_be_bytes(hi.try_into().expect("16")),
        u128::from_be_bytes(lo.try_into().expect("16")),
    )
}

/// The output at `slot` as a spendable PDA input, once indexed.
fn pda_input(env: &Env, owner: &ProgramOwner, output: &SppProofOutputUtxo) -> Result<WalletUtxo> {
    let tree_id = env.localnet.tree_id;
    let proof = wait_for_merkle_proof(
        env.localnet.client.indexer(),
        env.localnet.tree,
        output.hash(tree_id).map_err(err)?,
    )?;
    owner.input(output, tree_id, proof.leaf_index).map_err(err)
}

fn state<T>(utxo: &WalletUtxo) -> Result<T>
where
    ClientData<T>: TryFrom<WalletUtxo>,
    <ClientData<T> as TryFrom<WalletUtxo>>::Error: core::fmt::Debug,
{
    Ok(ClientData::<T>::try_from(utxo.clone()).map_err(err)?.data)
}

fn options(env: &Env) -> TransactionOptions {
    TransactionOptions {
        blinding_seed: None,
        output_tree_id: Some(env.localnet.tree_id),
    }
}

fn setup() -> Result<Env> {
    let ports = LocalnetPorts::for_test(0)?;
    let localnet = FixtureLocalnet::start(
        "lend-spike",
        ports,
        vec![(lend_spike::ID, root().join("target/deploy/lend_spike.so"))],
        &localnet_paths(),
    )?;
    let payer = fixture::payer();
    let mint = fixture::spl_mint();
    let mut assets = AssetRegistry::default();
    assets.insert(fixture::SPL_ASSET_ID, mint)?;
    let keeper = ShieldedKeypair::from_keypair(&fixture::actor(0))?;
    let user = ShieldedKeypair::from_keypair(&fixture::actor(1))?;
    for (wallet, amount) in [(&keeper, KEEPER_FUNDS), (&user, USER_FUNDS)] {
        deposit(
            &localnet,
            &payer,
            SprProgramTest::wallet_spl_shield_data(
                amount,
                &wallet.shielded_address()?,
                &mint,
                &fixture::payer_token_account(),
            )?,
        )?;
    }
    let keeper = TestWallet::scan(keeper, assets.clone(), &localnet.client)?;
    let user = TestWallet::scan(user, assets, &localnet.client)?;
    Ok(Env {
        rpc_url: ports.rpc_url(),
        localnet,
        mint,
        keeper,
        user,
    })
}

#[test]
fn spike() -> Result<()> {
    let env = setup()?;
    let mint = env.mint;
    let asset = Mint::new(mint, fixture::SPL_ASSET_ID);
    let (mint_hi, mint_lo) = halves(&mint);
    let keeper = &env.keeper.keypair;
    let user = &env.user.keypair;
    let keeper_key = keeper.pubkey();
    let user_key = user.pubkey();
    let pool = Pda::new(POOL_SEED, &mint.to_bytes());
    let user_pda = Pda::new(USER_SEED, &user_key.to_bytes());
    let pool_viewing = keeper.viewing_pubkey();
    let pool_address = pool.owner.address(pool_viewing).map_err(err)?;
    let user_viewing = user.viewing_pubkey();
    let user_address = user_pda.owner.address(user_viewing).map_err(err)?;
    let keeper_identity = spr_program::from_bytes(
        &spr_hasher::primitives::solana_owner_identity(&keeper_key.to_bytes()).map_err(err)?,
    )
    .map_err(err)?;
    let user_identity = spr_program::from_bytes(
        &spr_hasher::primitives::solana_owner_identity(&user_key.to_bytes()).map_err(err)?,
    )
    .map_err(err)?;
    let pool_args = |proof, now| {
        wincode::serialize(&PoolArgs {
            proof,
            mint: mint.to_bytes(),
            pool_viewing: *pool_viewing.as_bytes(),
            pool_bump: pool.bump,
            now,
        })
        .map_err(err)
    };

    // 1. The reserve at its unique address.
    let init = |salt: u128| -> Result<(Instruction, Vec<SppProofOutputUtxo>)> {
        let now = chain_now(&env)?;
        let b = built(
            SpikeInitClient {
                private: SpikeInitPrivateInputsClient {
                    tx_context: options(&env),
                    mint: asset,
                    salt,
                },
                public: SpikeInitPublicInputsClient {
                    pool_owner: pool_address,
                    mint_hi,
                    mint_lo,
                    now,
                },
            }
            .prepare(&keeper.shielded_address()?, keeper_key)
            .map_err(err)?,
            keeper,
        )?;
        let proof = lend_proof("spike_init", &b.proof_inputs)?;
        let (spp, owners) = transact(&env, b.spp, keeper)?;
        let ix = instruction(
            &env,
            tag::INIT,
            pool_args(proof, now)?,
            vec![AccountMeta::new(keeper_key, true)],
            keeper_key,
            &spp,
            &owners,
            &[pool.pda()],
        )?;
        Ok((ix, b.outputs))
    };
    let (init_ix, init_outputs) = init(1)?;
    // Built before the first init lands, so it proves the same fresh address.
    let (second_init, _) = init(2)?;
    run(&env, "init", &[keeper], init_ix)?;
    let second = send(env.localnet.client.rpc(), &[keeper], second_init);
    ensure!(
        second.is_err(),
        "a second reserve for the same mint was accepted"
    );
    println!(
        "SPIKE second init refused: {}",
        second.err().map(|e| e.to_string()).unwrap_or_default()
    );
    let reserve_output = |outputs: &[SppProofOutputUtxo]| -> Result<SppProofOutputUtxo> {
        outputs
            .iter()
            .find(|o| o.owner_address == Some(pool_address) && o.data_hash.is_some())
            .cloned()
            .context("no reserve output")
    };
    let mut reserve = pda_input(&env, &pool.owner, &reserve_output(&init_outputs)?)?;
    let mut reserve_state: ReserveState = state(&reserve)?;
    println!("SPIKE reserve after init: {reserve_state:?}");

    // 2. Spend and re-create it: keeper funding in, shares, accrual.
    let funding = env
        .keeper
        .spendable_utxo(mint, KEEPER_FUNDS)
        .context("no keeper note")?;
    let now = chain_now(&env)?;
    let accrue_inputs = || {
        (
            SpikeAccruePrivateInputsClient {
                tx_context: options(&env),
                reserve_utxo: reserve.clone(),
                reserve_state: reserve_client(&reserve_state),
                mint: asset,
                funding: funding.clone(),
                amount: KEEPER_FUNDS,
                debt_shares: ACCRUE_DEBT_SHARES,
                salt: 3,
            },
            SpikeAccruePublicInputsClient {
                pool_owner: pool_address,
                keeper_identity,
                mint_hi,
                mint_lo,
                now,
            },
        )
    };
    // The same transaction without the maths, proved natively only, prices it.
    let (private, public) = accrue_inputs();
    let moved = SpikeMoveClient { private, public }
        .prepare(&keeper.shielded_address()?, keeper_key)
        .map_err(err)?;
    lend_proof("spike_move", &moved.proof_inputs)?;
    let (private, public) = accrue_inputs();
    let b = built(
        SpikeAccrueClient { private, public }
            .prepare(&keeper.shielded_address()?, keeper_key)
            .map_err(err)?,
        keeper,
    )?;
    let proof = lend_proof("spike_accrue", &b.proof_inputs)?;
    let (spp, owners) = transact(&env, b.spp, keeper)?;
    let ix = instruction(
        &env,
        tag::ACCRUE,
        pool_args(proof, now)?,
        vec![AccountMeta::new(keeper_key, true)],
        keeper_key,
        &spp,
        &owners,
        &[pool.pda()],
    )?;
    run(&env, "accrue", &[keeper], ix)?;
    reserve = pda_input(&env, &pool.owner, &reserve_output(&b.outputs)?)?;
    reserve_state = state(&reserve)?;
    println!(
        "SPIKE reserve after accrue: {reserve_state:?}, amount {}",
        reserve.utxo.amount
    );
    ensure!(reserve.utxo.amount == KEEPER_FUNDS);

    // 3. A price account this program owns.
    let price = Keypair::new();
    let rpc = env.localnet.client.rpc();
    let mut create = vec![0u8; 4];
    create.extend(
        rpc.get_minimum_balance_for_rent_exemption(PRICE_ACCOUNT_LEN)?
            .to_le_bytes(),
    );
    create.extend((PRICE_ACCOUNT_LEN as u64).to_le_bytes());
    create.extend(lend_spike::ID.to_bytes());
    let mut set = vec![tag::SET_PRICE];
    set.extend(PRICE.to_le_bytes());
    set.extend(CONFIDENCE.to_le_bytes());
    rpc.create_and_send_transaction(
        &[
            Instruction {
                program_id: Address::default(),
                accounts: vec![
                    AccountMeta::new(keeper_key, true),
                    AccountMeta::new(price.pubkey(), true),
                ],
                data: create,
            },
            Instruction {
                program_id: lend_spike::ID,
                accounts: vec![
                    AccountMeta::new(keeper_key, true),
                    AccountMeta::new(price.pubkey(), false),
                ],
                data: set,
            },
        ],
        keeper_key,
        &[keeper as &dyn Signer, &price],
        spr_client::ComputeBudgetConfig::for_instruction_count(2),
    )?;

    // 4. The user funds an intent and a position under the user PDA.
    let fund = |intent_amount: u64,
                position_amount: u64,
                debt_shares: u64,
                user_wallet: &TestWallet|
     -> Result<Vec<SppProofOutputUtxo>> {
        let source = user_wallet
            .spendable_utxo(mint, intent_amount + position_amount)
            .context("no user note")?;
        let b = built(
            SpikeFundClient {
                private: SpikeFundPrivateInputsClient {
                    tx_context: options(&env),
                    source,
                    intent_amount,
                    position_amount,
                    position_id: POSITION_ID,
                    position_debt_shares: debt_shares,
                    expiry: chain_now(&env)? + 3_600,
                },
                public: SpikeFundPublicInputsClient {
                    user_owner: user_address,
                    maker_identity: user_identity,
                },
            }
            .prepare(&user.shielded_address()?, user_key)
            .map_err(err)?,
            user,
        )?;
        let proof = lend_proof("spike_fund", &b.proof_inputs)?;
        let (spp, owners) = transact(&env, b.spp, user)?;
        let args = wincode::serialize(&FundArgs {
            proof,
            user_viewing: *user_viewing.as_bytes(),
            user_bump: user_pda.bump,
        })
        .map_err(err)?;
        let ix = instruction(
            &env,
            tag::FUND,
            args,
            vec![AccountMeta::new(user_key, true)],
            user_key,
            &spp,
            &owners,
            &[user_pda.pda()],
        )?;
        run(&env, "fund", &[user], ix)?;
        Ok(b.outputs)
    };
    let user_data = |outputs: &[SppProofOutputUtxo]| -> Vec<SppProofOutputUtxo> {
        outputs
            .iter()
            .filter(|o| o.owner_address == Some(user_address) && o.data_hash.is_some())
            .cloned()
            .collect()
    };
    let outputs = fund(30_000, POSITION_AMOUNT, POSITION_DEBT_SHARES, &env.user)?;
    let [intent_out, position_out] =
        <[_; 2]>::try_from(user_data(&outputs)).map_err(|_| anyhow!("fund outputs"))?;
    let intent = pda_input(&env, &user_pda.owner, &intent_out)?;
    let mut position = pda_input(&env, &user_pda.owner, &position_out)?;

    // 5. The largest shape: reserve, position and intent under two PDAs,
    // the optional top-up, a keeper and a user signer.
    let touch = |reserve: &WalletUtxo,
                 intent: &WalletUtxo,
                 position: &WalletUtxo,
                 top_up: WalletUtxo,
                 top_up_amount: u64,
                 name: &str|
     -> Result<Vec<SppProofOutputUtxo>> {
        let now = chain_now(&env)?;
        let b = built(
            SpikeTouchClient {
                private: SpikeTouchPrivateInputsClient {
                    tx_context: options(&env),
                    reserve_utxo: reserve.clone(),
                    reserve_state: reserve_client(&state(reserve)?),
                    mint: asset,
                    intent_utxo: intent.clone(),
                    intent_state: intent_client(&state(intent)?),
                    position_utxo: position.clone(),
                    position_state: position_client(&state(position)?),
                    top_up,
                    top_up_amount,
                    position_id: POSITION_ID,
                    salt: 5,
                },
                public: SpikeTouchPublicInputsClient {
                    pool_owner: pool_address,
                    user_owner: user_address,
                    maker_identity: user_identity,
                    now,
                    price_bound: PRICE_BOUND,
                    mint_hi,
                    mint_lo,
                },
            }
            .prepare(&user.shielded_address()?, keeper_key)
            .map_err(err)?,
            user,
        )?;
        let proof = lend_proof("spike_touch", &b.proof_inputs)?;
        let (spp, owners) = transact(&env, b.spp, user)?;
        println!("SPIKE {name} owner signer slots: {owners:?} (pool {}, user pda {}, user {user_key}, keeper {keeper_key})", pool.pda(), user_pda.pda());
        let touch_ix = |now: u64, price_bound: u64| -> Result<Instruction> {
            let args = wincode::serialize(&TouchArgs {
                proof,
                mint: mint.to_bytes(),
                pool_viewing: *pool_viewing.as_bytes(),
                pool_bump: pool.bump,
                user_viewing: *user_viewing.as_bytes(),
                user_bump: user_pda.bump,
                now,
                price_bound,
            })
            .map_err(err)?;
            let head = vec![
                AccountMeta::new(keeper_key, true),
                AccountMeta::new_readonly(user_key, true),
                AccountMeta::new_readonly(price.pubkey(), false),
            ];
            instruction(
                &env,
                tag::TOUCH,
                args,
                head,
                keeper_key,
                &spp,
                &owners,
                &[pool.pda(), user_pda.pda()],
            )
        };
        // Both checks run before the proof: a bound above price - confidence,
        // and a `now` outside [clock - 120, clock].
        for (label, bad_now, bound, code) in [
            ("price below the bound", now, PRICE - CONFIDENCE + 1, "0x4"),
            ("stale now", now.saturating_sub(1_000), PRICE_BOUND, "0x1"),
            ("now ahead of the clock", now + 1_000, PRICE_BOUND, "0x1"),
        ] {
            let refused = send(
                env.localnet.client.rpc(),
                &[keeper, user],
                touch_ix(bad_now, bound)?,
            )
            .err()
            .with_context(|| format!("{label} was accepted"))?
            .to_string();
            ensure!(
                refused.contains(&format!("custom program error: {code}")),
                "{label}: {refused}"
            );
            println!("SPIKE {name}: {label} refused with {code}");
        }
        run(&env, name, &[keeper, user], touch_ix(now, PRICE_BOUND)?)?;
        Ok(b.outputs)
    };
    let dummy = WalletUtxo::dummy(env.localnet.tree_id).map_err(err)?;
    let outputs = touch(
        &reserve,
        &intent,
        &position,
        dummy,
        0,
        "touch (dummy top-up)",
    )?;
    reserve = pda_input(&env, &pool.owner, &reserve_output(&outputs)?)?;
    position = pda_input(
        &env,
        &user_pda.owner,
        user_data(&outputs).first().context("no position")?,
    )?;
    let after: PositionState = state(&position)?;
    println!(
        "SPIKE after touch 1: reserve {:?} amount {}, position {after:?}",
        state::<ReserveState>(&reserve)?,
        reserve.utxo.amount
    );
    ensure!(after.debt_shares < POSITION_DEBT_SHARES, "nothing repaid");

    // 6. The same shape with a real top-up note, repaying in full.
    let mut user_wallet = TestWallet::scan(
        ShieldedKeypair::from_keypair(&fixture::actor(1))?,
        env.user.registry.clone(),
        &env.localnet.client,
    )?;
    let outputs = fund(10_000, 0, 0, &user_wallet)?;
    let intent = pda_input(
        &env,
        &user_pda.owner,
        user_data(&outputs).first().context("no intent")?,
    )?;
    user_wallet.rescan(&env.localnet.client)?;
    let top_up = user_wallet
        .spendable_utxo(mint, 100_000)
        .context("no top-up note")?;
    let outputs = touch(
        &reserve,
        &intent,
        &position,
        top_up,
        100_000,
        "touch (real top-up)",
    )?;
    position = pda_input(
        &env,
        &user_pda.owner,
        user_data(&outputs).first().context("no position")?,
    )?;
    let after: PositionState = state(&position)?;
    println!("SPIKE after touch 2: position {after:?}");
    ensure!(after.debt_shares == 0, "not repaid in full");

    Ok(())
}

fn reserve_client(s: &ReserveState) -> ReserveStateClient {
    ReserveStateClient {
        supply_shares: s.supply_shares,
        debt_shares: s.debt_shares,
        supply_price: s.supply_price,
        borrow_price: s.borrow_price,
        rate_bps: s.rate_bps,
        last_update: s.last_update,
        salt: s.salt,
    }
}

fn intent_client(s: &IntentState) -> IntentStateClient {
    IntentStateClient {
        maker: s.maker,
        purpose: s.purpose,
        target: s.target,
        expiry: s.expiry,
    }
}

fn position_client(s: &PositionState) -> PositionStateClient {
    PositionStateClient {
        owner: s.owner,
        vault: s.vault,
        position_id: s.position_id,
        debt_shares: s.debt_shares,
    }
}
