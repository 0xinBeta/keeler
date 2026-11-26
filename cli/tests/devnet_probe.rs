//! Read-only devnet checks, run by hand with the Keychain's Helius key:
//! `cargo test -p keeler --test devnet_probe -- --ignored --nocapture`.
//! Nothing here sends a transaction: simulation only, signatures unchecked.
use keeler::{
    config::{redact, KeySource, Network, Prover},
    deploy::{address, create_mint, MINT_LEN},
};
use solana_address::Address;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcSimulateTransactionConfig;
use solana_signature::Signature;
use solana_signer::{Signer, SignerError};
use spr_client::{compile_message, sign_transaction, ComputeBudgetConfig, Rpc, SolanaRpc};
use spr_interface::pda;
use spr_program::instruction::CreateSplInterface;

/// The funded devnet deploy wallet.
const DEPLOY: &str = "8o2VNvAjjFCwwmt2eP8nMSuxPgbRVUCQ6gUZKnPukx95";

/// Signs as `DEPLOY` with a blank signature, for simulation without
/// signature checks.
struct Unsigned(Address);

impl Signer for Unsigned {
    fn try_pubkey(&self) -> Result<Address, SignerError> {
        Ok(self.0)
    }
    fn try_sign_message(&self, _: &[u8]) -> Result<Signature, SignerError> {
        Ok(Signature::default())
    }
    fn is_interactive(&self) -> bool {
        false
    }
}

#[test]
#[ignore]
fn devnet_pool_interface_takes_any_authority() {
    let net = Network {
        rpc_url: "https://devnet.helius-rpc.com".into(),
        indexer_url: "https://devnet.helius-rpc.com/v1/spr".into(),
        prover: Prover::Helius(KeySource::Keychain("keeler-helius-api-key".into())),
        priority_fee: 0,
    };
    let (url, _) = net.urls().unwrap();
    let rpc = SolanaRpc::new(url.clone());
    let payer = address(DEPLOY);
    // A new mint, created in the same simulated transaction, as the
    // deploy tool does.
    let mint = solana_keypair::Keypair::new().pubkey();
    let rent = rpc
        .get_minimum_balance_for_rent_exemption(MINT_LEN)
        .unwrap();
    let mut ixs = create_mint(&payer, &mint, &payer, 6, rent).to_vec();
    ixs.push(
        CreateSplInterface {
            authority: payer,
            mint,
            token_program: pda::spl_token_program_id(),
        }
        .instruction(),
    );
    let (blockhash, _) = rpc.get_latest_blockhash().unwrap();
    let message = compile_message(
        &payer,
        &ixs,
        blockhash,
        ComputeBudgetConfig::for_instruction_count(3),
    )
    .unwrap();
    let tx = sign_transaction(message, &[&Unsigned(payer), &Unsigned(mint)]).unwrap();
    let sim = RpcClient::new(url)
        .simulate_transaction_with_config(
            &tx,
            RpcSimulateTransactionConfig {
                sig_verify: false,
                replace_recent_blockhash: true,
                ..Default::default()
            },
        )
        .map_err(|e| redact(&e.to_string()))
        .unwrap()
        .value;
    for line in sim.logs.iter().flatten() {
        eprintln!("{line}");
    }
    assert!(
        sim.err.is_none(),
        "CreateSplInterface fails on devnet: {:?}",
        sim.err
    );
}
