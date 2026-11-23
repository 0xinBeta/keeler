//! The local stack for the web app, used by `scripts/localnet.sh`; run with
//! `.tools/env` sourced.
//!
//! `localnet start <offset> <dir>`: boots the network on the default ports
//! plus `offset` (the validator is left running), creates tUSD and tSOL with
//! the faucet key as mint authority, gives the dealer one large private note
//! of each, and writes the dealer's keys and config plus `keeler-web.json`
//! into `dir`. `localnet stop <offset> <dir>` stops the validator and Photon.
//! All keys are the fixture's public test keys: local network only.
use std::{path::PathBuf, str::FromStr};

use anyhow::{bail, ensure, Context, Result};
use keeler::{chain::shielded, faucet};
use serde_json::json;
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use spr_client::{ComputeBudgetConfig, Rpc};
use spr_interface::pda;
use spr_program::instruction::{CreateSplInterface, Deposit};
use spr_program_test::{
    fixture,
    localnet::{FixtureLocalnet, LocalnetPaths, LocalnetPorts, LocalnetValidator},
    SprProgramTest,
};

/// Known on purpose: the local Worker and the tests send it.
const HTTP_KEY: &str = "keeler-localnet-dealer-key";
const MINT_LEN: usize = 82;
/// (symbol, decimals, seed, dealer inventory, faucet amount, USD price)
const TOKENS: [(&str, u8, [u8; 32], u64, u64, i64); 2] = [
    ("tUSD", 6, *b"keeler localnet tUSD mint\0\0\0\0\0\0\0", 1_000_000_000_000, 1_000_000_000, 1),
    ("tSOL", 9, *b"keeler localnet tSOL mint\0\0\0\0\0\0\0", 10_000_000_000_000, 10_000_000_000, 150),
];

fn paths(dir: &std::path::Path) -> Result<LocalnetPaths> {
    Ok(LocalnetPaths {
        proving_keys_dir: PathBuf::from(std::env::var("HOME")?).join(".config/zolana/proving-keys"),
        scratch_dir: dir.to_path_buf(),
        ..LocalnetPaths::workspace()
    })
}

fn env_path(name: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .with_context(|| format!("{name} is not set; source .tools/env"))
}

fn send(net: &FixtureLocalnet, ixs: &[Instruction], payer: &Keypair, signers: &[&dyn Signer]) -> Result<()> {
    net.client
        .rpc()
        .create_and_send_transaction(
            ixs,
            payer.pubkey(),
            signers,
            ComputeBudgetConfig::for_instruction_count(ixs.len()),
        )
        .map_err(|e| anyhow::anyhow!("send: {e:?}"))?;
    Ok(())
}

/// A system `CreateAccount` and an SPL `InitializeMint2` with no freeze
/// authority, by their fixed layouts.
fn create_mint(payer: &Address, mint: &Address, authority: &Address, decimals: u8, rent: u64) -> [Instruction; 2] {
    let mut create = vec![0, 0, 0, 0];
    create.extend_from_slice(&rent.to_le_bytes());
    create.extend_from_slice(&(MINT_LEN as u64).to_le_bytes());
    create.extend_from_slice(pda::spl_token_program_id().as_ref());
    let mut init = vec![20, decimals];
    init.extend_from_slice(authority.as_ref());
    init.push(0);
    [
        Instruction {
            program_id: Address::from_str("11111111111111111111111111111111").expect("valid address"),
            accounts: vec![AccountMeta::new(*payer, true), AccountMeta::new(*mint, true)],
            data: create,
        },
        Instruction {
            program_id: pda::spl_token_program_id(),
            accounts: vec![AccountMeta::new(*mint, false)],
            data: init,
        },
    ]
}

fn start(offset: u16, dir: PathBuf) -> Result<()> {
    let ports = LocalnetPorts::offset(offset);
    let net = FixtureLocalnet::start(
        "keeler-web",
        ports,
        vec![
            (swap_program::ID, env_path("KEELER_SWAP_PROGRAM_SO")?),
            (
                spr_user_registry_interface::user_registry_program_id(),
                env_path("USER_REGISTRY_PROGRAM_PATH")?,
            ),
        ],
        &paths(&dir)?,
    )?;
    ensure!(net.tree_id == 0, "default tree id is {}", net.tree_id);
    let payer = fixture::payer();
    let dealer = fixture::actor(1);
    let faucet_key = fixture::actor(2);
    let rent = net
        .client
        .get_minimum_balance_for_rent_exemption(MINT_LEN)
        .map_err(|e| anyhow::anyhow!("rent: {e:?}"))?;
    let dealer_address = shielded(&dealer)?
        .shielded_address()
        .map_err(|e| anyhow::anyhow!("dealer address: {e:?}"))?;
    let mut assets = Vec::new();
    for (symbol, decimals, seed, inventory, _, _) in TOKENS {
        let mint = Keypair::new_from_array(seed);
        let mut ixs = create_mint(&payer.pubkey(), &mint.pubkey(), &faucet_key.pubkey(), decimals, rent).to_vec();
        // The protocol authority pays the asset registry's rent and the
        // fixture leaves it empty.
        ixs.extend(faucet::instructions(&payer.pubkey(), &fixture::protocol_authority().pubkey(), &[], 1_000_000_000));
        ixs.push(
            CreateSplInterface {
                authority: fixture::protocol_authority().pubkey(),
                mint: mint.pubkey(),
                token_program: pda::spl_token_program_id(),
            }
            .instruction(),
        );
        send(&net, &ixs, &payer, &[&payer, &mint, &fixture::protocol_authority()])
            .with_context(|| format!("create {symbol}"))?;
        // The dealer's inventory: minted to its token account, then one
        // private note of it.
        let ixs = faucet::instructions(&faucet_key.pubkey(), &dealer.pubkey(), &[(mint.pubkey(), inventory)], 0);
        send(&net, &ixs, &faucet_key, &[&faucet_key])?;
        let token = pda::associated_token_address(&dealer.pubkey(), &mint.pubkey());
        let deposit = Deposit {
            tree: net.tree,
            depositor: dealer.pubkey(),
            deposits: vec![SprProgramTest::wallet_spl_shield_data(inventory, &dealer_address, &mint.pubkey(), &token)?],
        }
        .instruction()?;
        let sig = net
            .client
            .rpc()
            .create_and_send_transaction(&[deposit], dealer.pubkey(), &[&dealer], ComputeBudgetConfig::for_instruction_count(1))
            .map_err(|e| anyhow::anyhow!("deposit {symbol}: {e:?}"))?;
        net.client
            .confirm_private_transaction_sync(sig)
            .map_err(|e| anyhow::anyhow!("confirm deposit {symbol}: {e:?}"))?;
        assets.push((symbol, decimals, mint.pubkey()));
    }

    let write_key = |name: &str, kp: &Keypair| -> Result<PathBuf> {
        let path = dir.join(name);
        std::fs::write(&path, serde_json::to_string(&kp.to_bytes().to_vec())?)?;
        Ok(path)
    };
    let dealer_key = write_key("dealer-key.json", &dealer)?;
    let faucet_file = write_key("faucet-key.json", &faucet_key)?;
    let http_key = dir.join("http-key");
    std::fs::write(&http_key, HTTP_KEY)?;
    let http = format!("127.0.0.1:{}", 47_500 + offset);
    let prover = std::env::var("ZOLANA_PROVER_URL").unwrap_or_else(|_| "http://127.0.0.1:3001".into());
    let config = json!({
        "network": {"rpc_url": ports.rpc_url(), "indexer_url": ports.photon_url(), "prover": "local"},
        "key": {"file": dealer_key},
        "assets": TOKENS.iter().zip(&assets).map(|(t, (_, _, mint))| json!({
            "mint": mint.to_string(), "price": {"price": t.5, "expo": 0}
        })).collect::<Vec<_>>(),
        "listen": format!("127.0.0.1:{}", 47_000 + offset),
        "spread_bps": 10,
        "quote_ttl_secs": 180,
        "settle_margin_secs": 20,
        "http": {"listen": http, "key": {"file": http_key}},
        "faucet": {
            "key": {"file": faucet_file},
            "tokens": TOKENS.iter().zip(&assets).map(|(t, (_, _, mint))| json!({
                "mint": mint.to_string(), "amount": t.4
            })).collect::<Vec<_>>(),
            "lamports": 20_000_000,
            "per_wallet_daily": 10,
            "daily_cap": 1000,
        },
    });
    std::fs::write(dir.join("dealer-config.json"), serde_json::to_string_pretty(&config)?)?;
    let web = json!({
        "network": "localnet",
        "rpcUrl": ports.rpc_url(),
        "indexerUrl": ports.photon_url(),
        "proverUrl": prover,
        "programId": swap_program::ID.to_string(),
        "assets": assets.iter().map(|(symbol, decimals, mint)| json!({
            "mint": mint.to_string(), "symbol": symbol, "decimals": decimals
        })).collect::<Vec<_>>(),
        "reference": TOKENS.iter().zip(&assets).map(|(t, (_, _, mint))| (mint.to_string(), json!({"price": t.5, "expo": 0}))).collect::<serde_json::Map<_, _>>(),
        "dealer": {"pubkey": dealer.pubkey().to_string(), "url": format!("http://{http}"), "key": HTTP_KEY},
        "faucet": true,
    });
    std::fs::write(dir.join("keeler-web.json"), serde_json::to_string_pretty(&web)? + "\n")?;
    // The validator and Photon outlive this process; `stop` ends them.
    std::mem::forget(net);
    Ok(())
}

fn stop(offset: u16, dir: PathBuf) -> Result<()> {
    let paths = paths(&dir)?;
    let scratch = dir.join(format!("keeler-web-{}", LocalnetPorts::offset(offset).rpc));
    LocalnetValidator {
        cli_bin: paths.cli_bin,
        working_dir: paths.working_dir,
        ports: LocalnetPorts::offset(offset),
        account_dir: scratch.join("accounts"),
        log_dir: scratch.join("logs"),
        programs: Vec::new(),
        slot_time: None,
    }
    .stop()?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [cmd, offset, dir] = args.as_slice() else {
        bail!("usage: localnet start|stop <offset> <dir>");
    };
    let offset: u16 = offset.parse().context("offset")?;
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    match cmd.as_str() {
        "start" => start(offset, dir),
        "stop" => stop(offset, dir),
        _ => bail!("usage: localnet start|stop <offset> <dir>"),
    }
}
