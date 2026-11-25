//! Every signing step of a network setup, in one idempotent pass with keys
//! from their key sources (the Keychain on devnet), so no secret is ever
//! written to a file. Each step reads the chain first and does only what is
//! missing, so a rerun after any failure finishes the job:
//!
//! 1. fund the dealer and faucet from the deploy key;
//! 2. deploy the swap program with the upgradeable loader through a buffer
//!    whose key is kept, resuming the writes, then check the deployed bytes;
//! 3. create each test mint (the faucet is its authority) and its pool
//!    interface;
//! 4. register the dealer and bring its private balance of each mint up to
//!    the inventory: mint the shortfall to its token account, deposit it.
//!
//! [`deploy_one`] does step 2 alone for a [`ProgramPlan`] (one named
//! program, nothing else), after checking the wallet holds what the deploy
//! still needs ([`estimate`]).
use std::{path::PathBuf, str::FromStr, time::Duration};

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use spr_client::{ComputeBudgetConfig, Rpc, SolanaRpc};
use spr_interface::pda;
use spr_program::instruction::{
    AssetDeposit, CreateSplInterface, Deposit, DepositAsset, DepositSplAccounts,
};

use spr_transaction::SOL_MINT;

use crate::{
    chain::{shielded, Chain},
    config::{redact, KeySource, Network},
    faucet,
};

pub const LOADER: &str = "BPFLoaderUpgradeab1e11111111111111111111111";
const SYSTEM: &str = "11111111111111111111111111111111";
const RENT_SYSVAR: &str = "SysvarRent111111111111111111111111111111111";
const CLOCK_SYSVAR: &str = "SysvarC1ock11111111111111111111111111111111";
pub const MINT_LEN: usize = 82;
/// Loader v3 headers: buffer (tag, authority option), program data (tag,
/// slot, authority option), program (tag, program data address).
const BUFFER_HEADER: usize = 37;
const PROGRAM_DATA_HEADER: usize = 45;
const PROGRAM_LEN: usize = 36;
/// Bytes per buffer write; with its accounts and compute budget the
/// transaction stays under the 1232-byte limit.
const CHUNK: usize = 900;
/// Top a balance up only once it falls below this share of its target, so
/// fees spent since the last run do not cause a transfer on every rerun.
const TOP_UP_BELOW_PCT: u64 = 90;
/// The base fee; the tool sets no priority fee.
const LAMPORTS_PER_SIGNATURE: u64 = 5_000;

/// What one setup does. Keys are key sources; on devnet, Keychain items.
#[derive(Deserialize)]
pub struct Plan {
    pub network: Network,
    /// Refuse unless the RPC reports this genesis hash.
    pub genesis_hash: String,
    /// Pays for everything and is the program's upgrade authority.
    pub deploy_key: KeySource,
    pub program_key: KeySource,
    pub buffer_key: KeySource,
    pub program_so: PathBuf,
    pub dealer_key: KeySource,
    pub faucet_key: KeySource,
    pub dealer_lamports: u64,
    pub faucet_lamports: u64,
    pub tokens: Vec<PlanToken>,
}

#[derive(Deserialize)]
pub struct PlanToken {
    pub symbol: String,
    pub decimals: u8,
    pub key: KeySource,
    /// The dealer's private balance to reach, in base units.
    pub inventory: u64,
}

/// One program deployed on its own, with its own Keychain items.
#[derive(Deserialize)]
pub struct ProgramPlan {
    /// For log lines and the summary, e.g. "lend".
    pub name: String,
    pub network: Network,
    pub genesis_hash: String,
    /// Pays and is the upgrade authority.
    pub deploy_key: KeySource,
    pub program_key: KeySource,
    pub buffer_key: KeySource,
    pub program_so: PathBuf,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct ProgramSummary {
    pub name: String,
    pub program_id: String,
    pub upgrade_authority: String,
}

/// What deploying `so_bytes` of program costs, in lamports.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Estimate {
    pub so_bytes: usize,
    /// Comes back to the wallet when the deploy closes the buffer.
    pub buffer_rent: u64,
    pub program_data_rent: u64,
    pub program_rent: u64,
    /// Base fees of every transaction, once each.
    pub fees: u64,
    /// Out of the wallet at once, just before the deploy closes the buffer.
    pub peak: u64,
    pub balance: u64,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Summary {
    pub program_id: String,
    pub dealer: String,
    pub faucet: String,
    /// (symbol, mint, decimals)
    pub mints: Vec<(String, String, u8)>,
}

pub fn address(s: &str) -> Address {
    Address::from_str(s).expect("valid address")
}

fn err(e: impl std::fmt::Debug) -> anyhow::Error {
    anyhow!(redact(&format!("{e:?}")))
}

pub fn send(
    rpc: &SolanaRpc,
    ixs: &[Instruction],
    payer: &Keypair,
    signers: &[&Keypair],
) -> Result<()> {
    let signers: Vec<&dyn Signer> = signers.iter().map(|k| *k as &dyn Signer).collect();
    rpc.create_and_send_transaction(
        ixs,
        payer.pubkey(),
        &signers,
        ComputeBudgetConfig::for_instruction_count(ixs.len()),
    )
    .map_err(err)?;
    Ok(())
}

/// A system `CreateAccount` of `space` bytes owned by `owner`.
pub fn create_account(
    payer: &Address,
    account: &Address,
    lamports: u64,
    space: usize,
    owner: &Address,
) -> Instruction {
    let mut data = vec![0, 0, 0, 0];
    data.extend_from_slice(&lamports.to_le_bytes());
    data.extend_from_slice(&(space as u64).to_le_bytes());
    data.extend_from_slice(owner.as_ref());
    Instruction {
        program_id: address(SYSTEM),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*account, true),
        ],
        data,
    }
}

/// `CreateAccount` and an SPL `InitializeMint2` with no freeze authority.
pub fn create_mint(
    payer: &Address,
    mint: &Address,
    authority: &Address,
    decimals: u8,
    rent: u64,
) -> [Instruction; 2] {
    let mut init = vec![20, decimals];
    init.extend_from_slice(authority.as_ref());
    init.push(0);
    [
        create_account(payer, mint, rent, MINT_LEN, &pda::spl_token_program_id()),
        Instruction {
            program_id: pda::spl_token_program_id(),
            accounts: vec![AccountMeta::new(*mint, false)],
            data: init,
        },
    ]
}

fn loader_ix(tag: u32, rest: &[u8], accounts: Vec<AccountMeta>) -> Instruction {
    Instruction {
        program_id: address(LOADER),
        accounts,
        data: [&tag.to_le_bytes()[..], rest].concat(),
    }
}

pub fn program_data_address(program: &Address) -> Address {
    Address::find_program_address(&[program.as_ref()], &address(LOADER)).0
}

/// Whether program data `data` holds exactly `so`.
fn holds(data: &[u8], header: usize, so: &[u8]) -> bool {
    data.len() >= header + so.len()
        && data[header..header + so.len()] == *so
        && data[header + so.len()..].iter().all(|b| *b == 0)
}

/// Deploys `so` at `program`'s address through `buffer`, or checks it is
/// already there. `max_writes` stops after that many buffer writes (tests).
pub fn deploy_program(
    rpc: &SolanaRpc,
    payer: &Keypair,
    program: &Keypair,
    buffer: &Keypair,
    so: &[u8],
    max_writes: Option<usize>,
) -> Result<()> {
    let loader = address(LOADER);
    let program_id = program.pubkey();
    let program_data = program_data_address(&program_id);
    let check_deployed = || -> Result<()> {
        let data = rpc
            .get_account(program_data)
            .map_err(err)?
            .ok_or_else(|| anyhow!("program {program_id} has no program data"))?
            .data;
        ensure!(
            holds(&data, PROGRAM_DATA_HEADER, so),
            "program {program_id} is deployed with other bytes; this tool does not upgrade"
        );
        Ok(())
    };
    if let Some(a) = rpc.get_account(program_id).map_err(err)? {
        ensure!(
            a.owner == loader,
            "{program_id} exists and is not an upgradeable program"
        );
        check_deployed()?;
        log!("deploy: program {program_id} is deployed with these bytes");
        return Ok(());
    }

    let buffer_id = buffer.pubkey();
    let buffer_len = BUFFER_HEADER + so.len();
    let data = match rpc.get_account(buffer_id).map_err(err)? {
        Some(a) => {
            ensure!(
                a.owner == loader
                    && a.data.len() == buffer_len
                    && a.data[..5] == [1, 0, 0, 0, 1]
                    && a.data[5..37] == *payer.pubkey().as_ref(),
                "buffer {buffer_id} does not fit this program or this authority; close it or use another buffer key"
            );
            log!("deploy: resuming buffer {buffer_id}");
            a.data
        }
        None => {
            let rent = rpc
                .get_minimum_balance_for_rent_exemption(buffer_len)
                .map_err(err)?;
            let init = loader_ix(
                0,
                &[],
                vec![
                    AccountMeta::new(buffer_id, false),
                    AccountMeta::new_readonly(payer.pubkey(), false),
                ],
            );
            send(
                rpc,
                &[
                    create_account(&payer.pubkey(), &buffer_id, rent, buffer_len, &loader),
                    init,
                ],
                payer,
                &[payer, buffer],
            )
            .context("create the buffer")?;
            log!("deploy: created buffer {buffer_id}");
            vec![0; buffer_len]
        }
    };
    let mut writes = 0;
    let chunks = so.len().div_ceil(CHUNK);
    for (i, chunk) in so.chunks(CHUNK).enumerate() {
        let at = BUFFER_HEADER + i * CHUNK;
        if data[at..at + chunk.len()] == *chunk {
            continue;
        }
        if max_writes.is_some_and(|m| writes >= m) {
            return Ok(());
        }
        let mut rest = ((i * CHUNK) as u32).to_le_bytes().to_vec();
        rest.extend_from_slice(&(chunk.len() as u64).to_le_bytes());
        rest.extend_from_slice(chunk);
        let write = loader_ix(
            1,
            &rest,
            vec![
                AccountMeta::new(buffer_id, false),
                AccountMeta::new_readonly(payer.pubkey(), true),
            ],
        );
        send(rpc, &[write], payer, &[payer]).with_context(|| format!("write chunk {i}"))?;
        writes += 1;
        if writes % 20 == 0 {
            log!("deploy: wrote chunk {} of {chunks}", i + 1);
        }
    }
    let written = rpc
        .get_account(buffer_id)
        .map_err(err)?
        .ok_or_else(|| anyhow!("buffer {buffer_id} vanished"))?;
    ensure!(
        holds(&written.data, BUFFER_HEADER, so),
        "buffer {buffer_id} does not hold the program after writing; rerun"
    );

    let rent = rpc
        .get_minimum_balance_for_rent_exemption(PROGRAM_LEN)
        .map_err(err)?;
    let deploy = loader_ix(
        2,
        &(so.len() as u64).to_le_bytes(),
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(program_data, false),
            AccountMeta::new(program_id, false),
            AccountMeta::new(buffer_id, false),
            AccountMeta::new_readonly(address(RENT_SYSVAR), false),
            AccountMeta::new_readonly(address(CLOCK_SYSVAR), false),
            AccountMeta::new_readonly(address(SYSTEM), false),
            AccountMeta::new_readonly(payer.pubkey(), true),
        ],
    );
    send(
        rpc,
        &[
            create_account(&payer.pubkey(), &program_id, rent, PROGRAM_LEN, &loader),
            deploy,
        ],
        payer,
        &[payer, program],
    )
    .context("deploy the program")?;
    check_deployed()?;
    log!("deploy: deployed program {program_id}");
    Ok(())
}

/// The upgrade authority recorded for `program`, `None` if immutable.
pub fn upgrade_authority(rpc: &SolanaRpc, program: &Address) -> Result<Option<Address>> {
    let data = rpc
        .get_account(program_data_address(program))
        .map_err(err)?
        .ok_or_else(|| anyhow!("{program} has no program data"))?
        .data;
    ensure!(
        data.len() >= PROGRAM_DATA_HEADER && data[..4] == [3, 0, 0, 0],
        "{program} is not an upgradeable program"
    );
    Ok(
        (data[12] == 1)
            .then(|| Address::new_from_array(data[13..45].try_into().expect("32 bytes"))),
    )
}

/// Hands `program`'s upgrade authority from `current` to `new`, which must
/// be an existing system-owned account, and reads it back.
pub fn set_upgrade_authority(
    rpc: &SolanaRpc,
    current: &Keypair,
    program: &Address,
    new: &Address,
) -> Result<()> {
    let account = rpc
        .get_account(*new)
        .map_err(err)?
        .ok_or_else(|| anyhow!("{new} does not exist; refusing"))?;
    ensure!(
        account.owner == address(SYSTEM),
        "{new} is owned by {}, not the system program; refusing",
        account.owner
    );
    match upgrade_authority(rpc, program)? {
        Some(a) if a == *new => {
            log!("{program}'s upgrade authority is already {new}");
            return Ok(());
        }
        Some(a) if a == current.pubkey() => {}
        Some(a) => bail!("{program}'s upgrade authority is {a}, not this key"),
        None => bail!("{program} is immutable"),
    }
    hand_over(rpc, current, program, new)
}

/// Sends the loader's `SetAuthority` and reads the authority back.
fn hand_over(rpc: &SolanaRpc, current: &Keypair, program: &Address, new: &Address) -> Result<()> {
    let ix = loader_ix(
        4,
        &[],
        vec![
            AccountMeta::new(program_data_address(program), false),
            AccountMeta::new_readonly(current.pubkey(), true),
            AccountMeta::new_readonly(*new, false),
        ],
    );
    send(rpc, &[ix], current, &[current])?;
    let got = upgrade_authority(rpc, program)?;
    ensure!(
        got == Some(*new),
        "upgrade authority reads back as {got:?}, not {new}"
    );
    Ok(())
}

/// Mainnet-beta's genesis hash.
pub const MAINNET_GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";

/// Squads v4, checked against its published source (github.com/Squads-Protocol/v4,
/// branch main, read 2026-11-25): `declare_id!` in
/// programs/squads_multisig_program/src/lib.rs; the `Multisig` account in
/// src/state/multisig.rs (Anchor: 8-byte discriminator, then create_key,
/// config_authority, threshold u16, time_lock u32, transaction_index u64,
/// stale_transaction_index u64, rent_collector Option<Pubkey>, bump u8,
/// members Vec<{key, permissions mask u8}>; Vote = 1 << 1); the seeds in
/// src/state/seeds.rs and instructions/vault_transaction_create.rs: multisig
/// ["multisig", "multisig", create_key], vault ["multisig", multisig,
/// "vault", vault_index u8]. The program is deployed at the same address on
/// mainnet and devnet.
pub const SQUADS_V4: &str = "SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf";
const SQUADS_VOTE: u8 = 1 << 1;

#[derive(Debug, PartialEq, Eq)]
pub struct Multisig {
    /// The all-zero address for an autonomous multisig; otherwise one key
    /// that can change members and threshold on its own.
    pub config_authority: Address,
    pub threshold: u16,
    pub time_lock: u32,
    /// (key, permission mask)
    pub members: Vec<(Address, u8)>,
}

/// A Squads v4 `Multisig` account's data.
pub fn parse_multisig(data: &[u8]) -> Result<Multisig> {
    use sha2::{Digest, Sha256};
    let disc = Sha256::digest(b"account:Multisig");
    ensure!(
        data.len() >= 8 && data[..8] == disc[..8],
        "not a Squads v4 multisig account"
    );
    let at = |i: usize, n: usize| -> Result<&[u8]> {
        data.get(i..i + n)
            .ok_or_else(|| anyhow!("Squads multisig account is truncated"))
    };
    let key = |i| -> Result<Address> { Ok(Address::new_from_array(at(i, 32)?.try_into()?)) };
    let config_authority = key(40)?;
    let threshold = u16::from_le_bytes(at(72, 2)?.try_into()?);
    let time_lock = u32::from_le_bytes(at(74, 4)?.try_into()?);
    // 78: transaction_index, 86: stale_transaction_index, 94: rent_collector.
    let mut i = match at(94, 1)?[0] {
        0 => 95,
        1 => 127,
        t => bail!("Squads multisig: bad rent_collector tag {t}"),
    } + 1; // bump
    let n = u32::from_le_bytes(at(i, 4)?.try_into()?) as usize;
    i += 4;
    let mut members = Vec::with_capacity(n.min(64));
    for _ in 0..n {
        members.push((key(i)?, at(i + 32, 1)?[0]));
        i += 33;
    }
    Ok(Multisig {
        config_authority,
        threshold,
        time_lock,
        members,
    })
}

/// Squads v4 vault `index` of `multisig`.
pub fn squads_vault(multisig: &Address, index: u8) -> Address {
    Address::find_program_address(
        &[b"multisig", multisig.as_ref(), b"vault", &[index]],
        &address(SQUADS_V4),
    )
    .0
}

/// Refuses unless `vault` is vault `index` of `multisig`, a Squads v4
/// multisig read from chain with threshold at least 2, no config authority
/// and enough voters to reach the threshold.
pub fn check_squads_vault(
    rpc: &SolanaRpc,
    vault: &Address,
    multisig: &Address,
    index: u8,
) -> Result<Multisig> {
    let derived = squads_vault(multisig, index);
    ensure!(
        derived == *vault,
        "{vault} is not vault {index} of Squads multisig {multisig} (that is {derived}); refusing"
    );
    let a = rpc
        .get_account(*multisig)
        .map_err(err)?
        .ok_or_else(|| anyhow!("Squads multisig {multisig} does not exist; refusing"))?;
    ensure!(
        a.owner == address(SQUADS_V4),
        "{multisig} is owned by {}, not Squads v4 {SQUADS_V4}; refusing",
        a.owner
    );
    let m = parse_multisig(&a.data)?;
    squads_policy(&m).map_err(|e| anyhow!("Squads multisig {multisig} {e}; refusing"))?;
    Ok(m)
}

fn squads_policy(m: &Multisig) -> Result<()> {
    ensure!(
        m.threshold >= 2,
        "has threshold {}; at least 2 required",
        m.threshold
    );
    ensure!(
        m.config_authority == Address::new_from_array([0; 32]),
        "has config authority {}, which can change its members and threshold alone; \
         use an autonomous multisig",
        m.config_authority
    );
    let voters = m.members.iter().filter(|(_, p)| p & SQUADS_VOTE != 0).count();
    ensure!(
        voters >= m.threshold as usize,
        "has {voters} voters for threshold {}",
        m.threshold
    );
    Ok(())
}

/// The program bytes deployed at `program` (program data past its header).
pub fn deployed_bytes(rpc: &SolanaRpc, program: &Address) -> Result<Vec<u8>> {
    upgrade_authority(rpc, program)?;
    let data = rpc
        .get_account(program_data_address(program))
        .map_err(err)?
        .ok_or_else(|| anyhow!("{program} has no program data"))?
        .data;
    Ok(data[PROGRAM_DATA_HEADER..].to_vec())
}

/// The 33-byte viewing public key (hex) of the Solana key in `key`.
pub fn viewing_pubkey_hex(key: &Keypair) -> Result<String> {
    Ok(shielded(key)?
        .viewing_pubkey()
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// A mainnet deploy, written by `scripts/deploy-mainnet.sh`.
#[derive(Deserialize)]
pub struct MainnetPlan {
    pub name: String,
    pub network: Network,
    pub genesis_hash: String,
    pub deploy_key: KeySource,
    pub program_key: KeySource,
    pub buffer_key: KeySource,
    pub program_so: PathBuf,
    /// The deploy key's address, so a cost estimate needs no key.
    pub payer: String,
    pub program_id: String,
    pub multisig: Option<String>,
    #[serde(default)]
    pub vault_index: u8,
}

/// Any plan's network and current upgrade authority.
#[derive(Deserialize)]
pub struct AuthorityPlan {
    pub network: Network,
    pub genesis_hash: String,
    pub deploy_key: KeySource,
}

impl MainnetPlan {
    fn program_plan(&self) -> ProgramPlan {
        ProgramPlan {
            name: self.name.clone(),
            network: self.network.clone(),
            genesis_hash: self.genesis_hash.clone(),
            deploy_key: self.deploy_key.clone(),
            program_key: self.program_key.clone(),
            buffer_key: self.buffer_key.clone(),
            program_so: self.program_so.clone(),
        }
    }

    /// Every check that needs no secret but the RPC key, then the cost.
    fn check(&self) -> Result<(SolanaRpc, Estimate)> {
        if self.genesis_hash == MAINNET_GENESIS {
            for k in [&self.deploy_key, &self.program_key, &self.buffer_key] {
                ensure!(
                    matches!(k, KeySource::Keychain(_)),
                    "mainnet keys come from the Keychain, not a file; refusing"
                );
            }
        }
        let rpc = self.program_plan().rpc()?;
        if let Some(m) = &self.multisig {
            let m = address_arg(m)?;
            let vault = squads_vault(&m, self.vault_index);
            let ms = check_squads_vault(&rpc, &vault, &m, self.vault_index)?;
            log!(
                "squads: vault {vault} of multisig {m}: threshold {} of {} members, time lock {} s",
                ms.threshold,
                ms.members.len(),
                ms.time_lock
            );
        }
        let payer = address_arg(&self.payer)?;
        let e = estimate(&rpc, &payer, self.program_plan().so()?.len())?;
        log_estimate(&self.name, &payer, &e);
        Ok((rpc, e))
    }
}

fn address_arg(s: &str) -> Result<Address> {
    Address::from_str(s).map_err(|_| anyhow!("{s:?} is not a base58 address"))
}

fn load<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    serde_json::from_str(&text).map_err(|e| anyhow!("plan {path}: {e}"))
}

pub const DEPLOY_USAGE: &str = "usage: keeler deploy viewing-key <keychain item>
       keeler deploy check <mainnet plan>
       keeler deploy program <mainnet plan> <write deployed bytes to>
       keeler deploy hand-over <plan> <program> <squads multisig> <vault index>";

/// `keeler deploy ...`, for `scripts/deploy-mainnet.sh`,
/// `scripts/deploy-lend-devnet.sh` and `scripts/set-upgrade-authority.sh`.
/// Prints results on stdout, never a secret.
pub fn cli(args: &[String]) -> Result<()> {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["viewing-key", item] => {
            println!(
                "{}",
                viewing_pubkey_hex(&KeySource::Keychain(item.to_string()).keypair()?)?
            );
        }
        ["check", path] => {
            let (_, e) = load::<MainnetPlan>(path)?.check()?;
            println!("{}", serde_json::to_string_pretty(&e)?);
        }
        ["program", path, out] => {
            let plan: MainnetPlan = load(path)?;
            let (rpc, _) = plan.check()?;
            ensure!(
                plan.multisig.is_some(),
                "a deploy needs the Squads multisig that takes the upgrade authority"
            );
            let deployer = plan.deploy_key.keypair()?.pubkey();
            ensure!(
                deployer.to_string() == plan.payer,
                "the deploy key is {deployer}, not the plan's payer {}",
                plan.payer
            );
            let program = plan.program_key.keypair()?.pubkey();
            ensure!(
                program.to_string() == plan.program_id,
                "the program key is {program}, not the plan's program {}",
                plan.program_id
            );
            // deploy_one checks the balance before sending anything.
            let summary = deploy_one(&plan.program_plan())?;
            std::fs::write(out, deployed_bytes(&rpc, &program)?)
                .with_context(|| format!("write {out}"))?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        ["hand-over", path, program, multisig, index] => {
            let plan: AuthorityPlan = load(path)?;
            let rpc = checked_rpc(&plan.network, &plan.genesis_hash)?;
            let (program, multisig) = (address_arg(program)?, address_arg(multisig)?);
            let index: u8 = index.parse().context("vault index")?;
            let vault = squads_vault(&multisig, index);
            let m = check_squads_vault(&rpc, &vault, &multisig, index)?;
            let current = plan.deploy_key.keypair()?;
            match upgrade_authority(&rpc, &program)? {
                Some(a) if a == vault => log!("{program}'s upgrade authority is already {vault}"),
                Some(a) if a == current.pubkey() => hand_over(&rpc, &current, &program, &vault)?,
                Some(a) => bail!("{program}'s upgrade authority is {a}, not this key"),
                None => bail!("{program} is immutable"),
            }
            println!(
                "{}",
                json!({
                    "program_id": program.to_string(),
                    "upgrade_authority": vault.to_string(),
                    "multisig": multisig.to_string(),
                    "vault_index": index,
                    "threshold": m.threshold,
                    "members": m.members.len(),
                    "time_lock": m.time_lock,
                })
            );
        }
        _ => bail!("{DEPLOY_USAGE}"),
    }
    Ok(())
}

/// Tops `who` up to `target` lamports from `payer` once below 90% of it.
fn fund(rpc: &SolanaRpc, payer: &Keypair, who: &Address, target: u64, name: &str) -> Result<()> {
    let have = rpc.get_balance(*who).map_err(err)?;
    if have >= target / 100 * TOP_UP_BELOW_PCT {
        log!("deploy: {name} {who} has {have} lamports");
        return Ok(());
    }
    let ixs = faucet::instructions(&payer.pubkey(), who, &[], target - have);
    send(rpc, &ixs, payer, &[payer]).with_context(|| format!("fund the {name}"))?;
    log!("deploy: funded the {name} {who} to {target} lamports");
    Ok(())
}

/// Creates the mint or checks the one there has these decimals and this
/// authority.
fn ensure_mint(
    rpc: &SolanaRpc,
    payer: &Keypair,
    mint: &Keypair,
    authority: &Address,
    decimals: u8,
) -> Result<()> {
    let id = mint.pubkey();
    if let Some(a) = rpc.get_account(id).map_err(err)? {
        ensure!(
            a.owner == pda::spl_token_program_id()
                && a.data.len() >= MINT_LEN
                && a.data[..4] == [1, 0, 0, 0]
                && a.data[4..36] == *authority.as_ref()
                && a.data[44] == decimals,
            "{id} exists but is not a mint with {decimals} decimals and authority {authority}"
        );
        return Ok(());
    }
    let rent = rpc
        .get_minimum_balance_for_rent_exemption(MINT_LEN)
        .map_err(err)?;
    send(
        rpc,
        &create_mint(&payer.pubkey(), &id, authority, decimals, rent),
        payer,
        &[payer, mint],
    )
    .with_context(|| format!("create mint {id}"))?;
    log!("deploy: created mint {id}");
    Ok(())
}

fn ensure_interface(rpc: &SolanaRpc, payer: &Keypair, mint: &Address) -> Result<()> {
    if rpc
        .get_account(pda::spl_asset_registry(mint))
        .map_err(err)?
        .is_some()
    {
        return Ok(());
    }
    let ix = CreateSplInterface {
        authority: payer.pubkey(),
        mint: *mint,
        token_program: pda::spl_token_program_id(),
    }
    .instruction();
    send(rpc, &[ix], payer, &[payer])
        .with_context(|| format!("create the pool interface of {mint}"))?;
    log!("deploy: created the pool interface of {mint}");
    Ok(())
}

/// The token amount in an SPL token account, 0 if there is none.
fn token_balance(rpc: &SolanaRpc, account: &Address) -> Result<u64> {
    Ok(match rpc.get_account(*account).map_err(err)? {
        Some(a) if a.data.len() >= 72 => u64::from_le_bytes(a.data[64..72].try_into()?),
        _ => 0,
    })
}

/// Brings the dealer's private balance of `mint` up to `inventory`.
fn ensure_inventory(
    chain: &Chain,
    dealer: &Keypair,
    faucet_key: &Keypair,
    mint: &Address,
    inventory: u64,
) -> Result<()> {
    let kp = shielded(dealer)?;
    let private = chain.balance(&kp, *mint)?;
    if private >= inventory {
        log!("deploy: dealer holds {private} of {mint} privately");
        return Ok(());
    }
    let need = inventory - private;
    let rpc = chain.client.rpc();
    let ata = pda::associated_token_address(&dealer.pubkey(), mint);
    let public = token_balance(rpc, &ata)?;
    if public < need {
        // Supply, not the indexer's view, decides what to mint: a deposit
        // that landed but is not indexed yet must not be minted again.
        let data = rpc
            .get_account(*mint)
            .map_err(err)?
            .ok_or_else(|| anyhow!("mint {mint} is missing"))?
            .data;
        ensure!(data.len() >= MINT_LEN, "{mint} is not a mint account");
        let supply = u64::from_le_bytes(data[36..44].try_into()?);
        ensure!(
            public + inventory.saturating_sub(supply) >= need,
            "{mint}: supply {supply} already covers the inventory but the dealer holds \
             {private} privately and {public} publicly; a deposit may still be indexing; \
             wait and rerun"
        );
        let ixs = faucet::instructions(
            &faucet_key.pubkey(),
            &dealer.pubkey(),
            &[(*mint, need - public)],
            0,
        );
        send(rpc, &ixs, faucet_key, &[faucet_key]).with_context(|| format!("mint {mint}"))?;
        log!("deploy: minted {} of {mint} to the dealer", need - public);
    }
    let to = kp
        .shielded_address()
        .map_err(|e| anyhow!("dealer address: {e:?}"))?;
    let deposit = Deposit {
        tree: chain.tree,
        depositor: dealer.pubkey(),
        deposits: vec![AssetDeposit {
            asset: DepositAsset::Spl(DepositSplAccounts {
                mint: *mint,
                user_token: ata,
                token_program: pda::spl_token_program_id(),
            }),
            view_tag: to.viewing_pubkey.u(),
            owner: to.owner_hash().map_err(|e| anyhow!("owner hash: {e:?}"))?,
            amount: need,
            memo: None,
        }],
    }
    .instruction()
    .map_err(err)?;
    let sig = rpc
        .create_and_send_transaction(
            &[deposit],
            dealer.pubkey(),
            &[dealer],
            ComputeBudgetConfig::for_instruction_count(1),
        )
        .map_err(err)
        .with_context(|| format!("deposit {mint}"))?;
    chain
        .client
        .confirm_private_transaction_sync(sig)
        .map_err(err)?;
    // The indexer may lag the confirmation.
    for _ in 0..60 {
        if chain.balance(&kp, *mint)? >= inventory {
            log!("deploy: deposited {need} of {mint}");
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    bail!("the deposit of {mint} landed but the indexer does not show it yet; rerun to check")
}

/// The RPC's genesis hash, through a plain JSON-RPC call.
pub fn genesis_hash(rpc_url: &str) -> Result<String> {
    let reply: serde_json::Value = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?
        .post(rpc_url)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "getGenesisHash"}))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| anyhow!("genesis hash: {}", redact(&e.to_string())))?;
    reply["result"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("no genesis hash in the RPC reply"))
}

/// `network`'s RPC, after checking it is the cluster with genesis `want`.
fn checked_rpc(network: &Network, want: &str) -> Result<SolanaRpc> {
    let (rpc_url, _) = network.urls()?;
    let got = genesis_hash(&rpc_url)?;
    ensure!(
        got == want,
        "the RPC's genesis hash is {got}, not {want}; refusing"
    );
    Ok(SolanaRpc::new(rpc_url))
}

impl Plan {
    /// The plan's RPC, after checking it is the plan's cluster.
    pub fn rpc(&self) -> Result<SolanaRpc> {
        checked_rpc(&self.network, &self.genesis_hash)
    }
}

impl ProgramPlan {
    /// The plan's RPC, after checking it is the plan's cluster.
    pub fn rpc(&self) -> Result<SolanaRpc> {
        checked_rpc(&self.network, &self.genesis_hash)
    }

    pub fn so(&self) -> Result<Vec<u8>> {
        std::fs::read(&self.program_so)
            .with_context(|| format!("read {}", self.program_so.display()))
    }
}

/// Lamports as SOL, exactly.
pub fn sol(lamports: u64) -> String {
    format!(
        "{}.{:09} SOL",
        lamports / 1_000_000_000,
        lamports % 1_000_000_000
    )
}

/// What a fresh deploy of `so_bytes` costs `payer`, with its balance; reads
/// rent from the chain and needs no key.
pub fn estimate(rpc: &SolanaRpc, payer: &Address, so_bytes: usize) -> Result<Estimate> {
    let rent = |len| rpc.get_minimum_balance_for_rent_exemption(len).map_err(err);
    let buffer_rent = rent(BUFFER_HEADER + so_bytes)?;
    let program_data_rent = rent(PROGRAM_DATA_HEADER + so_bytes)?;
    let program_rent = rent(PROGRAM_LEN)?;
    // Create the buffer and deploy take two signatures each, a write one.
    let signatures = 4 + so_bytes.div_ceil(CHUNK) as u64;
    let fees = signatures * LAMPORTS_PER_SIGNATURE;
    Ok(Estimate {
        so_bytes,
        buffer_rent,
        program_data_rent,
        program_rent,
        fees,
        peak: buffer_rent + program_data_rent + program_rent + fees,
        balance: rpc.get_balance(*payer).map_err(err)?,
    })
}

/// Prints `e` for a person deciding whether to fund the wallet.
pub fn log_estimate(name: &str, payer: &Address, e: &Estimate) {
    log!(
        "deploy {name}: {} bytes; buffer rent {} (refunded at deploy), program data rent {}, \
         program rent {}, fees {}",
        e.so_bytes,
        sol(e.buffer_rent),
        sol(e.program_data_rent),
        sol(e.program_rent),
        sol(e.fees)
    );
    log!(
        "deploy {name}: needs {} at peak; wallet {payer} holds {}",
        sol(e.peak),
        sol(e.balance)
    );
}

/// Deploys the plan's one program, or checks it is already there with
/// these bytes. Before sending anything it checks the wallet holds what the
/// deploy still needs: the peak, or for a resumed buffer the program's rent
/// and the fees of the writes and deploy left.
pub fn deploy_one(plan: &ProgramPlan) -> Result<ProgramSummary> {
    let name = &plan.name;
    let rpc = plan.rpc()?;
    let deployer = plan.deploy_key.keypair()?;
    let program = plan.program_key.keypair()?;
    let buffer = plan.buffer_key.keypair()?;
    let so = plan.so()?;
    let e = estimate(&rpc, &deployer.pubkey(), so.len())?;
    log_estimate(name, &deployer.pubkey(), &e);
    // A deployed program needs nothing; deploy_program checks its bytes.
    if rpc.get_account(program.pubkey()).map_err(err)?.is_none() {
        let need = match rpc.get_account(buffer.pubkey()).map_err(err)? {
            None => e.peak,
            // Its rent and the writes that landed are spent already.
            Some(a) => {
                let left = so
                    .chunks(CHUNK)
                    .enumerate()
                    .filter(|(i, c)| {
                        let at = BUFFER_HEADER + i * CHUNK;
                        a.data.get(at..at + c.len()) != Some(*c)
                    })
                    .count() as u64;
                e.program_data_rent + e.program_rent + (left + 2) * LAMPORTS_PER_SIGNATURE
            }
        };
        ensure!(
            e.balance >= need,
            "deploy {name}: the wallet holds {} but the deploy still needs {}; fund {} \
             and rerun; nothing sent",
            sol(e.balance),
            sol(need),
            deployer.pubkey()
        );
    }
    deploy_program(&rpc, &deployer, &program, &buffer, &so, None)?;
    let authority = upgrade_authority(&rpc, &program.pubkey())?
        .ok_or_else(|| anyhow!("{} is immutable", program.pubkey()))?;
    Ok(ProgramSummary {
        name: name.clone(),
        program_id: program.pubkey().to_string(),
        upgrade_authority: authority.to_string(),
    })
}

/// Runs every step of `plan`.
pub fn setup(plan: &Plan) -> Result<Summary> {
    let rpc = plan.rpc()?;
    let deployer = plan.deploy_key.keypair()?;
    let program = plan.program_key.keypair()?;
    let buffer = plan.buffer_key.keypair()?;
    let dealer = plan.dealer_key.keypair()?;
    let faucet_key = plan.faucet_key.keypair()?;
    // A bad indexer or tree fails here, before anything is spent.
    Chain::connect(&plan.network, &[])?
        .balance(&shielded(&dealer)?, SOL_MINT)
        .context("indexer check")?;
    log!(
        "deploy: deploy key {}, balance {} lamports",
        deployer.pubkey(),
        rpc.get_balance(deployer.pubkey()).map_err(err)?
    );

    fund(
        &rpc,
        &deployer,
        &dealer.pubkey(),
        plan.dealer_lamports,
        "dealer",
    )?;
    fund(
        &rpc,
        &deployer,
        &faucet_key.pubkey(),
        plan.faucet_lamports,
        "faucet",
    )?;

    let so = std::fs::read(&plan.program_so)
        .with_context(|| format!("read {}", plan.program_so.display()))?;
    deploy_program(&rpc, &deployer, &program, &buffer, &so, None)?;

    let mut mints = Vec::new();
    for t in &plan.tokens {
        let mint = t.key.keypair()?;
        ensure_mint(&rpc, &deployer, &mint, &faucet_key.pubkey(), t.decimals)?;
        ensure_interface(&rpc, &deployer, &mint.pubkey())?;
        mints.push((t, mint.pubkey()));
    }

    let ids: Vec<Address> = mints.iter().map(|(_, m)| *m).collect();
    let chain = Chain::connect(&plan.network, &ids)?;
    chain.register(&shielded(&dealer)?)?;
    for (t, mint) in &mints {
        ensure_inventory(&chain, &dealer, &faucet_key, mint, t.inventory)
            .with_context(|| format!("{} inventory", t.symbol))?;
    }
    Ok(Summary {
        program_id: program.pubkey().to_string(),
        dealer: dealer.pubkey().to_string(),
        faucet: faucet_key.pubkey().to_string(),
        mints: mints
            .iter()
            .map(|(t, m)| (t.symbol.clone(), m.to_string(), t.decimals))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_layouts() {
        let a = Address::new_from_array([1; 32]);
        let ix = create_account(&a, &a, 5, 36, &address(LOADER));
        assert_eq!(ix.data.len(), 52);
        assert_eq!(&ix.data[..4], &[0, 0, 0, 0]);
        assert_eq!(&ix.data[12..20], &36u64.to_le_bytes());
        let w = loader_ix(1, &[9], vec![]);
        assert_eq!(w.data, vec![1, 0, 0, 0, 9]);
        // An empty deployed tail must be zeros.
        let mut data = vec![0; PROGRAM_DATA_HEADER];
        data.extend([1, 2, 3, 0, 0]);
        assert!(holds(&data, PROGRAM_DATA_HEADER, &[1, 2, 3]));
        assert!(!holds(&data, PROGRAM_DATA_HEADER, &[1, 2]));
        assert!(!holds(&data, PROGRAM_DATA_HEADER, &[1, 2, 3, 4, 5, 6]));
    }

    #[test]
    fn plan_parses_keychain_names_only() {
        let plan: Plan = serde_json::from_value(json!({
            "network": {"rpc_url": "https://devnet.helius-rpc.com", "indexer_url": "https://devnet.helius-rpc.com/v1/spr", "prover": {"helius": {"keychain": "k"}}},
            "genesis_hash": "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
            "deploy_key": {"keychain": "keeler-devnet-deploy"},
            "program_key": {"keychain": "keeler-devnet-program"},
            "buffer_key": {"keychain": "keeler-devnet-buffer"},
            "program_so": "x.so",
            "dealer_key": {"keychain": "keeler-devnet-dealer"},
            "faucet_key": {"keychain": "keeler-devnet-faucet"},
            "dealer_lamports": 500000000u64,
            "faucet_lamports": 2500000000u64,
            "tokens": [{"symbol": "tSOL", "decimals": 9, "key": {"keychain": "keeler-devnet-tsol-mint"}, "inventory": 10000000000000u64}]
        }))
        .unwrap();
        assert_eq!(plan.tokens[0].inventory, 10_000_000_000_000);
    }

    #[test]
    fn program_plan_parses_and_sol_prints_exactly() {
        let plan: ProgramPlan = serde_json::from_value(json!({
            "name": "lend",
            "network": {"rpc_url": "https://devnet.helius-rpc.com", "indexer_url": "https://devnet.helius-rpc.com/v1/spr", "prover": {"helius": {"keychain": "k"}}},
            "genesis_hash": "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
            "deploy_key": {"keychain": "keeler-devnet-deploy"},
            "program_key": {"keychain": "keeler-devnet-lend-program"},
            "buffer_key": {"keychain": "keeler-devnet-lend-buffer"},
            "program_so": "keeler_lend.so"
        }))
        .unwrap();
        assert_eq!(plan.name, "lend");
        assert_eq!(sol(12_345_678_901), "12.345678901 SOL");
        assert_eq!(sol(5_000), "0.000005000 SOL");
    }

    /// A Squads v4 multisig account laid out as `Multisig::size` describes.
    fn multisig_bytes(config: [u8; 32], threshold: u16, rent: bool, members: &[u8]) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        let mut d = Sha256::digest(b"account:Multisig")[..8].to_vec();
        d.extend([7; 32]); // create_key
        d.extend(config);
        d.extend(threshold.to_le_bytes());
        d.extend(30u32.to_le_bytes());
        d.extend([0; 16]);
        if rent {
            d.push(1);
            d.extend([9; 32]);
        } else {
            d.push(0);
        }
        d.push(255); // bump
        d.extend((members.len() as u32).to_le_bytes());
        for (i, mask) in members.iter().enumerate() {
            d.extend([i as u8 + 1; 32]);
            d.push(*mask);
        }
        d
    }

    #[test]
    fn squads_multisig_parses_and_the_policy_holds() {
        let m = parse_multisig(&multisig_bytes([0; 32], 2, true, &[7, 2, 1])).unwrap();
        assert_eq!(m.threshold, 2);
        assert_eq!(m.time_lock, 30);
        assert_eq!(m.members.len(), 3);
        assert_eq!(m.members[2], (Address::new_from_array([3; 32]), 1));
        squads_policy(&m).unwrap();
        let none = parse_multisig(&multisig_bytes([0; 32], 2, false, &[7, 7])).unwrap();
        assert_eq!(none.members[1].0, Address::new_from_array([2; 32]));
        squads_policy(&none).unwrap();

        let refused = |b: Vec<u8>| squads_policy(&parse_multisig(&b).unwrap()).unwrap_err().to_string();
        assert!(refused(multisig_bytes([0; 32], 1, false, &[7, 7])).contains("threshold 1"));
        assert!(refused(multisig_bytes([5; 32], 2, false, &[7, 7])).contains("config authority"));
        // Only one member may vote.
        assert!(refused(multisig_bytes([0; 32], 2, false, &[7, 5])).contains("1 voters"));

        let mut wrong = multisig_bytes([0; 32], 2, false, &[7, 7]);
        wrong[0] ^= 1;
        assert!(parse_multisig(&wrong).is_err());
        let full = multisig_bytes([0; 32], 2, false, &[7, 7]);
        assert!(parse_multisig(&full[..full.len() - 1]).is_err());
    }

    #[test]
    fn squads_vault_matches_mainnet() {
        // A mainnet multisig whose rent_collector is its vault 0 (read 2026-11-25).
        assert_eq!(
            squads_vault(&address("1cKogaUTm7Xm3JqRZAZb372y64Z4FEPS1W9JjSKQN2x"), 0),
            address("2Hfkz8iZ3z6aUrdgotTWUHpT7CSWgW6LfeiUhaLMJUdx")
        );
    }

    #[test]
    fn the_fixture_pool_key_gives_the_compiled_fixture_viewing_key() {
        let kp = Keypair::new_from_array(keeler_lend_sdk::FIXTURE_POOL_SEED);
        assert_eq!(
            viewing_pubkey_hex(&kp).unwrap(),
            "04eff66782e8d5e74f22a134d513919285e4b1b1c1fcee15a7d31e3b4f926cf011"
        );
    }
}
