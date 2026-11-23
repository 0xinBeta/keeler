//! Network setup with keys from their key sources (Keychain items on
//! devnet); see `keeler::deploy`. Used by `scripts/deploy-devnet.sh` and
//! `scripts/set-upgrade-authority.sh`.
//!
//! `deploy setup <plan.json>`: every setup step, idempotent; prints a JSON
//! summary (program ID, dealer, faucet, mints) on stdout.
//! `deploy keygen`: prints a new keypair as a JSON array on stdout, for the
//! caller to pipe into the Keychain.
//! `deploy set-authority <plan.json> <program> <new-authority>`: hands the
//! upgrade authority from the plan's deploy key to an existing system
//! account after you type its address again.
use std::io::BufRead;

use anyhow::{bail, ensure, Context, Result};
use keeler::{
    config::{parse_address, redact},
    deploy::{self, Plan},
};
use spr_client::SolanaRpc;

fn plan(path: &str) -> Result<Plan> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("plan {path}: {e}"))
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["setup", path] => {
            let summary = deploy::setup(&plan(path)?)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
            Ok(())
        }
        ["keygen"] => {
            let kp = solana_keypair::Keypair::new();
            println!("{}", serde_json::to_string(&kp.to_bytes().to_vec())?);
            Ok(())
        }
        ["set-authority", path, program, new] => {
            let plan = plan(path)?;
            let (program, new) = (parse_address(program)?, parse_address(new)?);
            eprintln!(
                "This hands the upgrade authority of {program} to {new}. The deploy key \
                 cannot take it back.\nType {new} to confirm:"
            );
            let mut typed = String::new();
            std::io::stdin().lock().read_line(&mut typed)?;
            ensure!(typed.trim() == new.to_string(), "not confirmed; nothing sent");
            let (rpc_url, _) = plan.network.urls()?;
            let rpc = SolanaRpc::new(rpc_url);
            deploy::set_upgrade_authority(&rpc, &plan.deploy_key.keypair()?, &program, &new)?;
            eprintln!("{program}'s upgrade authority is now {new}");
            Ok(())
        }
        _ => bail!(
            "usage: deploy setup <plan.json> | deploy keygen | deploy set-authority <plan.json> <program> <new-authority>"
        ),
    }
}

fn main() {
    if let Err(e) = run() {
        // Errors may quote an RPC URL, which holds the API key.
        eprintln!("error: {}", redact(&format!("{e:#}")));
        std::process::exit(1);
    }
}
