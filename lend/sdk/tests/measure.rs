//! The figures the local-network tests print for `lend/MEASUREMENTS.md`:
//! `PROOF <name>` (the lend proof as the instruction sends it, and the
//! native proving time of the lend circuit alone) and `TX <name>` (compute
//! units from the confirmed transaction, the v1 transaction size against
//! its limit, the instruction data and the account count).
//!
//! Include it with `#[path = "measure.rs"] mod measure;`.
#![allow(dead_code)]

use std::{process::Command, time::Instant};

use anyhow::{Context, Result};
use keeler_lend_sdk::{builders::Prepared, proof};
use solana_address::Address;
use solana_instruction::Instruction;
use solana_signature::Signature;
use spr_client::ComputeBudgetConfig;

/// The v1 transaction limit, `solana_message::v1::MAX_TRANSACTION_SIZE`.
pub const V1_TX_LIMIT: usize = 4096;
pub const BUDGET: u32 = 1_400_000;

/// Proves `p`'s lend circuit once more, timed, and prints its size as sent.
pub fn lend_proof(name: &str, p: &Prepared) -> Result<()> {
    let start = Instant::now();
    let proof = proof::prove(p.circuit(), &p.proof_inputs)?;
    let secs = start.elapsed().as_secs_f64();
    let bytes = wincode::serialize(&proof).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    println!(
        "PROOF {name}: lend proof {} bytes, native lend proving {secs:.2} s",
        bytes.len()
    );
    Ok(())
}

pub fn compute_units(rpc: &str, signature: &Signature) -> Result<u64> {
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

pub fn report(
    rpc: &str,
    name: &str,
    payer: &Address,
    ix: &Instruction,
    sig: &Signature,
) -> Result<()> {
    let size = spr_client::transaction_size(
        payer,
        std::slice::from_ref(ix),
        ComputeBudgetConfig::new(BUDGET),
    )
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    println!(
        "TX {name}: {} CU, v1 tx {} of {V1_TX_LIMIT} bytes, ix data {} bytes, {} accounts in the instruction, {} in the transaction",
        compute_units(rpc, sig)?,
        size.bytes,
        ix.data.len(),
        ix.accounts.len(),
        size.addresses
    );
    Ok(())
}
