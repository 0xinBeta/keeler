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

/// What one setup does. Keys are key sources; on devnet, Keychain items.
#[derive(Deserialize)]
pub struct Plan {
    pub network: Network,
    /// Refuse unless the RPC reports this genesis hash.
    #[serde(default)]
    pub genesis_hash: Option<String>,
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
fn genesis_hash(rpc_url: &str) -> Result<String> {
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

/// Runs every step of `plan`.
pub fn setup(plan: &Plan) -> Result<Summary> {
    let (rpc_url, _) = plan.network.urls()?;
    if let Some(want) = &plan.genesis_hash {
        let got = genesis_hash(&rpc_url)?;
        ensure!(
            got == *want,
            "the RPC's genesis hash is {got}, not {want}; refusing"
        );
    }
    let rpc = SolanaRpc::new(rpc_url);
    let deployer = plan.deploy_key.keypair()?;
    let program = plan.program_key.keypair()?;
    let buffer = plan.buffer_key.keypair()?;
    let dealer = plan.dealer_key.keypair()?;
    let faucet_key = plan.faucet_key.keypair()?;
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
}
