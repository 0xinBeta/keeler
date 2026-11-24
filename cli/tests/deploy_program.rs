//! The single-program deploy (`deploy program`) on a local validator, port
//! offset 2600, no prover or indexer. Needs `SHIELDED_POOL_PROGRAM_PATH`
//! and `ZOLANA_CLI_BIN` (`.tools/env`) and the lend `.so`
//! (`scripts/setup-lend.sh`; `KEELER_LEND_PROGRAM_SO` overrides its path).
use std::path::{Path, PathBuf};

use keeler::{
    config::{KeySource, Network},
    deploy::{self, ProgramPlan},
    faucet,
};
use solana_address::Address;
use solana_keypair::Keypair;
use solana_signer::Signer;
use spr_client::{Rpc, SolanaRpc};
use spr_interface::SHIELDED_POOL_PROGRAM_ID;
use spr_program_test::{
    fixture,
    localnet::{LocalnetPaths, LocalnetPorts, LocalnetValidator},
};

const PORT_OFFSET: u16 = 2600;

/// Stops the validator however the test ends.
struct Stop(LocalnetValidator);

impl Drop for Stop {
    fn drop(&mut self) {
        let _ = self.0.stop();
    }
}

fn env_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} is not set; source .tools/env"))
}

fn key_file(dir: &Path, name: &str, kp: &Keypair) -> KeySource {
    let path = dir.join(format!("{name}.json"));
    std::fs::write(
        &path,
        serde_json::to_string(&kp.to_bytes().to_vec()).unwrap(),
    )
    .unwrap();
    KeySource::File(path)
}

#[test]
fn deploy_one_program_resumes_and_refuses() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let so_path = std::env::var_os("KEELER_LEND_PROGRAM_SO")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("lend/target/deploy/keeler_lend.so"));
    let so = std::fs::read(&so_path)
        .unwrap_or_else(|_| panic!("no {}; run scripts/setup-lend.sh", so_path.display()));
    let ports = LocalnetPorts::offset(PORT_OFFSET);
    let scratch = root.join(format!("target/localnet/keeler-deploy-one-{}", ports.rpc));
    let _ = std::fs::remove_dir_all(&scratch);
    let account_dir = scratch.join("accounts");
    let spp = env_path("SHIELDED_POOL_PROGRAM_PATH");
    fixture::write_test_fixture(&spp, &account_dir).unwrap();
    let paths = LocalnetPaths::workspace();
    let validator = LocalnetValidator {
        cli_bin: env_path("ZOLANA_CLI_BIN"),
        working_dir: paths.working_dir,
        ports,
        account_dir,
        log_dir: scratch.join("logs"),
        programs: vec![(Address::new_from_array(SHIELDED_POOL_PROGRAM_ID), spp)],
        slot_time: Some(std::time::Duration::from_millis(50)),
    };
    let _stop = Stop(validator);
    _stop.0.start().unwrap();

    let keys = scratch.join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let rpc = SolanaRpc::new(ports.rpc_url());
    let genesis = deploy::genesis_hash(&ports.rpc_url()).unwrap();
    let rich = fixture::payer();
    // A wallet of its own, funded below the peak first.
    let deployer = Keypair::new_from_array([51; 32]);
    let program = Keypair::new_from_array([52; 32]);
    let buffer = Keypair::new_from_array([53; 32]);
    let plan = |genesis: &str, so: &Path| ProgramPlan {
        name: "lend".into(),
        network: Network::local(ports.rpc_url(), ports.photon_url()),
        genesis_hash: genesis.into(),
        deploy_key: key_file(&keys, "deploy", &deployer),
        program_key: key_file(&keys, "program", &program),
        buffer_key: key_file(&keys, "buffer", &buffer),
        program_so: so.into(),
    };
    let good = plan(&genesis, &so_path);
    let e = deploy::estimate(&rpc, &deployer.pubkey(), so.len()).unwrap();
    assert_eq!(e.balance, 0);
    assert_eq!(
        e.peak,
        e.buffer_rent + e.program_data_rent + e.program_rent + e.fees
    );
    eprintln!("lend .so {} bytes: peak {}", so.len(), deploy::sol(e.peak));
    let fund = |lamports: u64| {
        let ixs = faucet::instructions(&rich.pubkey(), &deployer.pubkey(), &[], lamports);
        deploy::send(&rpc, &ixs, &rich, &[&rich]).unwrap();
    };
    let balance = || rpc.get_balance(deployer.pubkey()).unwrap();

    // Another cluster's genesis: refused before any key is read or anything sent.
    let wrong = plan("EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG", &so_path);
    let msg = deploy::deploy_one(&wrong).unwrap_err().to_string();
    assert!(msg.contains("genesis hash"), "{msg}");

    // Short by one lamport: refused, nothing sent.
    fund(e.peak - 1);
    let msg = format!("{:#}", deploy::deploy_one(&good).unwrap_err());
    assert!(msg.contains("still needs"), "{msg}");
    assert_eq!(balance(), e.peak - 1);
    assert!(rpc.get_account(buffer.pubkey()).unwrap().is_none());

    // Interrupted after five buffer writes, then resumed by deploy_one with
    // the buffer's rent counted as already spent.
    fund(1);
    deploy::deploy_program(&rpc, &deployer, &program, &buffer, &so, Some(5)).unwrap();
    assert!(rpc.get_account(buffer.pubkey()).unwrap().is_some());
    assert!(rpc.get_account(program.pubkey()).unwrap().is_none());
    let first = deploy::deploy_one(&good).unwrap();
    assert_eq!(first.program_id, program.pubkey().to_string());
    assert_eq!(first.upgrade_authority, deployer.pubkey().to_string());
    assert!(
        rpc.get_account(buffer.pubkey()).unwrap().is_none(),
        "the deploy closed the buffer"
    );
    let spent = e.peak - balance();
    assert!(
        spent <= e.program_data_rent + e.program_rent + e.fees,
        "spent {spent}, more than the estimate after the buffer refund"
    );

    // A rerun finds it deployed and sends nothing.
    let before = balance();
    assert_eq!(deploy::deploy_one(&good).unwrap(), first);
    assert_eq!(balance(), before, "a rerun sends nothing");

    // Other bytes at the same address: refused, nothing sent.
    let other = scratch.join("other.so");
    let mut changed = so.clone();
    *changed.last_mut().unwrap() ^= 1;
    std::fs::write(&other, &changed).unwrap();
    let msg = format!(
        "{:#}",
        deploy::deploy_one(&plan(&genesis, &other)).unwrap_err()
    );
    assert!(msg.contains("other bytes"), "{msg}");
    assert_eq!(balance(), before);
}
