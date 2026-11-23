//! `spl_interface <payer-keypair-file> <mint>`: registers `mint` with the
//! pool (its asset id and SPL interface) unless it already is, at the RPC
//! URL in `KEELER_RPC_URL`. Used by `scripts/deploy-devnet.sh`.
use anyhow::{anyhow, bail, Context, Result};
use keeler::config::{parse_address, redact, KeySource};
use solana_signer::Signer;
use spr_client::{ComputeBudgetConfig, Rpc, SolanaRpc};
use spr_interface::pda;
use spr_program::instruction::CreateSplInterface;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [key, mint] = args.as_slice() else {
        bail!("usage: spl_interface <payer-keypair-file> <mint>");
    };
    let payer = KeySource::File(key.into()).keypair()?;
    let mint = parse_address(mint)?;
    let rpc = SolanaRpc::new(std::env::var("KEELER_RPC_URL").context("KEELER_RPC_URL")?);
    let registry = pda::spl_asset_registry(&mint);
    if rpc
        .get_account(registry)
        .map_err(|e| anyhow!(redact(&format!("{e:?}"))))?
        .is_some()
    {
        eprintln!("{mint} is already registered");
        return Ok(());
    }
    let ix = CreateSplInterface {
        authority: payer.pubkey(),
        mint,
        token_program: pda::spl_token_program_id(),
    }
    .instruction();
    rpc.create_and_send_transaction(
        &[ix],
        payer.pubkey(),
        &[&payer],
        ComputeBudgetConfig::for_instruction_count(1),
    )
    .map_err(|e| anyhow!("register {mint}: {}", redact(&format!("{e:?}"))))?;
    eprintln!("registered {mint}");
    Ok(())
}
