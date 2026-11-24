//! Network setup with keys from their key sources (Keychain items on
//! devnet); see `keeler::deploy`. Used by `scripts/deploy-devnet.sh` and
//! `scripts/set-upgrade-authority.sh`.
//!
//! `deploy setup <plan.json>`: every setup step, idempotent; prints a JSON
//! summary (program ID, dealer, faucet, mints) on stdout.
//! `deploy program <program-plan.json>`: deploys one named program and
//! nothing else (`deploy::deploy_one`), idempotent; prints a JSON summary
//! (name, program ID, upgrade authority) on stdout. Stops before sending
//! anything if the deploy wallet holds less than the deploy still needs.
//! `deploy estimate <program-plan.json>`: prints what that deploy needs at
//! peak and the wallet's balance; reads the chain, sends nothing, and reads
//! only the deploy key (for its address), not the program or buffer keys.
//! `deploy keygen`: prints a new keypair as a JSON array on stdout, for the
//! caller to pipe into the Keychain.
//! `deploy set-authority <plan.json> <program> <new-authority>`: hands the
//! upgrade authority from the plan's deploy key to an existing system
//! account after you type its address again.
use std::io::BufRead;

use anyhow::{bail, ensure, Context, Result};
use keeler::{
    config::{parse_address, redact},
    deploy::{self, Plan, ProgramPlan},
};
use serde::de::DeserializeOwned;
use solana_signer::Signer;

fn plan<T: DeserializeOwned>(path: &str) -> Result<T> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("plan {path}: {e}"))
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["setup", path] => {
            let summary = deploy::setup(&plan::<Plan>(path)?)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
            Ok(())
        }
        ["program", path] => {
            let summary = deploy::deploy_one(&plan(path)?)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
            Ok(())
        }
        ["estimate", path] => {
            let plan: ProgramPlan = plan(path)?;
            let rpc = plan.rpc()?;
            let payer = plan.deploy_key.keypair()?.pubkey();
            let e = deploy::estimate(&rpc, &payer, plan.so()?.len())?;
            deploy::log_estimate(&plan.name, &payer, &e);
            if e.balance < e.peak {
                eprintln!("the wallet is short by {}", deploy::sol(e.peak - e.balance));
            }
            println!("{}", serde_json::to_string_pretty(&e)?);
            Ok(())
        }
        ["keygen"] => {
            let kp = solana_keypair::Keypair::new();
            println!("{}", serde_json::to_string(&kp.to_bytes().to_vec())?);
            Ok(())
        }
        ["set-authority", path, program, new] => {
            let plan: Plan = plan(path)?;
            let rpc = plan.rpc()?;
            let (program, new) = (parse_address(program)?, parse_address(new)?);
            eprintln!(
                "This hands the upgrade authority of {program} to {new}. The deploy key \
                 cannot take it back.\nType {new} to confirm:"
            );
            let mut typed = String::new();
            std::io::stdin().lock().read_line(&mut typed)?;
            ensure!(typed.trim() == new.to_string(), "not confirmed; nothing sent");
            deploy::set_upgrade_authority(&rpc, &plan.deploy_key.keypair()?, &program, &new)?;
            eprintln!("{program}'s upgrade authority is now {new}");
            Ok(())
        }
        _ => bail!(
            "usage: deploy setup <plan.json> | deploy program <program-plan.json> | \
             deploy estimate <program-plan.json> | deploy keygen | deploy set-authority <plan.json> <program> <new-authority>"
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
