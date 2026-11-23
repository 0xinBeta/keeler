//! Boots a fixture local network, gives dealer 1 a SOL note and runs it as a
//! real `keeler dealer`, registers dealer 2 (which never settles), writes
//! `localnet.json` into the directory given as the first argument, and runs
//! until stdin closes. Needs `source .tools/env`, `ZOLANA_PORT_OFFSET` and
//! `ZOLANA_PROVER_URL`.
use std::{
    io::Read,
    net::SocketAddr,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use keeler::{
    chain::{shielded, Chain},
    config::{Asset, Common, DealerConfig, KeySource, PriceSource},
    dealer,
};
use solana_signer::Signer;
use spr_client::{ComputeBudgetConfig, Rpc};
use spr_program::instruction::Deposit;
use spr_program_test::{
    fixture,
    localnet::{FixtureLocalnet, LocalnetPaths, LocalnetPorts},
    SprProgramTest,
};
use spr_transaction::SOL_MINT;

const DEALER_SOL: u64 = 10_000_000_000;
const DEALER_LISTEN: &str = "127.0.0.1:47950";
/// Reference prices the dealer and the stand-in Worker share: SOL 100 USD,
/// the fixture token 2 USD.
const SOL_PRICE: i64 = 100;
const SPL_PRICE: i64 = 2;

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is not set")))
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("usage: keeler-web-localnet <dir>"));
    std::fs::create_dir_all(&out).unwrap();
    let paths = LocalnetPaths {
        proving_keys_dir: env_path("HOME").join(".config/zolana/proving-keys"),
        scratch_dir: out.join("scratch"),
        ..LocalnetPaths::workspace()
    };
    // Test 0: the prover resolves proof data against this checkout's Photon.
    let ports = LocalnetPorts::for_test(0).unwrap();
    let localnet = FixtureLocalnet::start(
        "keeler-web",
        ports,
        vec![
            (swap_program::ID, env_path("KEELER_SWAP_PROGRAM_SO")),
            (
                spr_user_registry_interface::user_registry_program_id(),
                env_path("USER_REGISTRY_PROGRAM_PATH"),
            ),
        ],
        &paths,
    )
    .unwrap();

    let payer = fixture::payer();
    let dealer_kp = fixture::actor(1);
    let address = shielded(&dealer_kp).unwrap().shielded_address().unwrap();
    let ix = Deposit {
        tree: localnet.tree,
        depositor: payer.pubkey(),
        deposits: vec![SprProgramTest::wallet_sol_shield_data(DEALER_SOL, &address).unwrap()],
    }
    .instruction()
    .unwrap();
    let sig = localnet
        .client
        .rpc()
        .create_and_send_transaction(&[ix], payer.pubkey(), &[&payer], ComputeBudgetConfig::for_instruction_count(1))
        .unwrap();
    localnet.client.confirm_private_transaction_sync(sig).unwrap();

    let spl = fixture::spl_mint();
    let chain = Chain::connect(&ports.rpc_url(), &ports.photon_url(), &[spl, SOL_MINT]).unwrap();
    let idle = fixture::actor(2);
    chain.register(&shielded(&idle).unwrap()).unwrap();

    let key_file = out.join("dealer-1.json");
    std::fs::write(&key_file, serde_json::to_string(&dealer_kp.to_bytes().to_vec()).unwrap()).unwrap();
    let asset = |mint: String, price| Asset { mint, price: PriceSource::Fixed { price, expo: 0 } };
    let cfg = DealerConfig {
        common: Common {
            rpc_url: ports.rpc_url(),
            indexer_url: ports.photon_url(),
            key: KeySource::File(key_file),
            assets: vec![asset(spl.to_string(), SPL_PRICE), asset(SOL_MINT.to_string(), SOL_PRICE)],
            pyth: None,
        },
        listen: DEALER_LISTEN.parse::<SocketAddr>().unwrap(),
        spread_bps: 10,
        quote_ttl_secs: 40,
        settle_margin_secs: 5,
    };
    cfg.check().unwrap();
    static STOP: AtomicBool = AtomicBool::new(false);
    let handle = thread::spawn(move || dealer::run(&cfg, &STOP));
    while std::net::TcpStream::connect(DEALER_LISTEN).is_err() {
        assert!(!handle.is_finished(), "dealer exited at start");
        thread::sleep(std::time::Duration::from_millis(200));
    }

    let info = serde_json::json!({
        "rpcUrl": ports.rpc_url(),
        "photonUrl": ports.photon_url(),
        "proverUrl": std::env::var("ZOLANA_PROVER_URL").unwrap(),
        "splMint": spl.to_string(),
        "prices": { spl.to_string(): SPL_PRICE, SOL_MINT.to_string(): SOL_PRICE },
        "payerSecret": payer.to_bytes().to_vec(),
        "dealer": { "pubkey": dealer_kp.pubkey().to_string(), "listen": DEALER_LISTEN },
        "idleDealer": { "pubkey": idle.pubkey().to_string() },
    });
    std::fs::write(out.join("localnet.json"), info.to_string()).unwrap();
    println!("ready");

    // The parent closes stdin to stop us; dropping `localnet` stops the validator.
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
    STOP.store(true, Ordering::Relaxed);
    let _ = handle.join();
    drop(localnet);
}
