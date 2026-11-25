//! End-to-end on a local network, started the way the example's tests start
//! theirs. Needs `source .tools/env`. Trader and dealers run in-process
//! through the same entry points `main.rs` calls.
use std::{
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, sleep, JoinHandle},
    time::{Duration, Instant},
};

use keeler::{
    chain::{hex, shielded, Chain, Status},
    config::{
        Asset, Common, DealerConfig, DealerRef, Http, KeySource, Network, PriceSource, TraderConfig,
    },
    dealer,
    deploy::{self, Plan, PlanToken},
    faucet,
    trader::{self, fair_amount, sign_quote, signed_by, Outcome},
};
use keeler_core::{
    price::apply_spread,
    rfq::{serve, Rfq},
};
use solana_address::Address;
use solana_keypair::Keypair;
use solana_signer::Signer;
use spr_client::{ComputeBudgetConfig, Rpc, SolanaRpc};
use spr_interface::pda;
use spr_program::instruction::{AssetDeposit, Deposit};
use spr_program_test::{
    fixture,
    localnet::{FixtureLocalnet, LocalnetPaths, LocalnetPorts},
    SprProgramTest,
};
use spr_transaction::SOL_MINT;
use swap_sdk::{shared::identity, state::OrderTerms};

/// Tests share the one local prover and the CPU; run them one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

const TRADER_SPL: u64 = fixture::PAYER_SPL_BALANCE;
const DEALER_SOL: u64 = 10_000_000_000;

struct Net {
    /// Dropping it stops the validator.
    _localnet: FixtureLocalnet,
    ports: LocalnetPorts,
    keys: PathBuf,
    spl: Address,
}

fn env_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} is not set; source .tools/env"))
}

fn deposit(localnet: &FixtureLocalnet, d: AssetDeposit) {
    let payer = fixture::payer();
    let ix = Deposit {
        tree: localnet.tree,
        depositor: payer.pubkey(),
        deposits: vec![d],
    }
    .instruction()
    .unwrap();
    let sig = localnet
        .client
        .rpc()
        .create_and_send_transaction(
            &[ix],
            payer.pubkey(),
            &[&payer],
            ComputeBudgetConfig::for_instruction_count(1),
        )
        .unwrap();
    localnet
        .client
        .confirm_private_transaction_sync(sig)
        .unwrap();
}

/// Actor 0 is the trader (all of the fixture's SPL), actors 1..=3 dealers
/// (one large SOL note each).
fn start(test: u16) -> Net {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target");
    let paths = LocalnetPaths {
        proving_keys_dir: PathBuf::from(std::env::var("HOME").unwrap())
            .join(".config/zolana/proving-keys"),
        scratch_dir: target.join("localnet"),
        ..LocalnetPaths::workspace()
    };
    let ports = LocalnetPorts::for_test(test).unwrap();
    let localnet = FixtureLocalnet::start(
        "keeler-e2e",
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
    assert_eq!(localnet.tree_id, 0);
    let keys = target.join(format!("localnet/keeler-keys-{test}"));
    // Dealer state from an earlier run names orders of another chain.
    let _ = std::fs::remove_dir_all(&keys);
    std::fs::create_dir_all(&keys).unwrap();
    let spl = fixture::spl_mint();
    for i in 0..fixture::ACTOR_COUNT {
        let actor = fixture::actor(i);
        std::fs::write(
            keys.join(format!("{i}.json")),
            serde_json::to_string(&actor.to_bytes().to_vec()).unwrap(),
        )
        .unwrap();
        let address = shielded(&actor).unwrap().shielded_address().unwrap();
        let d = if i == 0 {
            SprProgramTest::wallet_spl_shield_data(
                TRADER_SPL,
                &address,
                &spl,
                &fixture::payer_token_account(),
            )
        } else {
            SprProgramTest::wallet_sol_shield_data(DEALER_SOL, &address)
        };
        deposit(&localnet, d.unwrap());
    }
    Net {
        _localnet: localnet,
        ports,
        keys,
        spl,
    }
}

impl Net {
    fn common(&self, actor: u8) -> Common {
        self.priced(actor, 2)
    }

    /// `spl_price` USD per SPL token; SOL is 100 USD.
    fn priced(&self, actor: u8, spl_price: i64) -> Common {
        let asset = |mint: Address, price| Asset {
            mint: mint.to_string(),
            price: PriceSource::Fixed { price, expo: 0 },
        };
        Common {
            network: self.network(),
            key: KeySource::File(self.keys.join(format!("{actor}.json"))),
            assets: vec![asset(self.spl, spl_price), asset(SOL_MINT, 100)],
            pyth: None,
        }
    }

    fn network(&self) -> Network {
        Network::local(self.ports.rpc_url(), self.ports.photon_url())
    }

    fn chain(&self) -> Chain {
        Chain::connect(&self.network(), &[self.spl, SOL_MINT]).unwrap()
    }

    fn balances(&self, chain: &Chain, actor: u8) -> (u64, u64) {
        let kp = shielded(&fixture::actor(actor)).unwrap();
        (
            chain.balance(&kp, self.spl).unwrap(),
            chain.balance(&kp, SOL_MINT).unwrap(),
        )
    }
}

struct Dealer {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<anyhow::Result<()>>>,
    reference: DealerRef,
}

fn dealer_addr(test: u16, actor: u8) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 47_000 + test * 10 + actor as u16))
}

struct Terms {
    spread_bps: u16,
    quote_ttl_secs: u64,
    settle_margin_secs: u64,
    spl_price: i64,
}

const HONEST: Terms = Terms {
    spread_bps: 10,
    quote_ttl_secs: 30,
    settle_margin_secs: 5,
    spl_price: 2,
};

fn spawn_dealer(net: &Net, test: u16, actor: u8, t: Terms) -> Dealer {
    spawn_dealer_with(net, test, actor, t, None)
}

fn dealer_cfg(net: &Net, test: u16, actor: u8, t: Terms, http: Option<Http>) -> DealerConfig {
    DealerConfig {
        common: net.priced(actor, t.spl_price),
        listen: dealer_addr(test, actor),
        spread_bps: t.spread_bps,
        quote_ttl_secs: t.quote_ttl_secs,
        settle_margin_secs: t.settle_margin_secs,
        http,
        poll_secs: None,
        faucet: None,
        state_file: Some(net.keys.join(format!("dealer-state-{actor}.json"))),
        pause_file: None,
        settle_tolerance_bps: 0,
        limits: Vec::new(),
        program_id: swap_program::ID.to_string(),
        pubkey: Some(fixture::actor(actor).pubkey().to_string()),
        auto_pause_after: None,
        min_fee_lamports: None,
        lend: None,
    }
}

fn spawn_dealer_with(net: &Net, test: u16, actor: u8, t: Terms, http: Option<Http>) -> Dealer {
    let cfg = dealer_cfg(net, test, actor, t, http);
    cfg.check().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = thread::spawn(move || dealer::run(&cfg, &flag));
    let started = Instant::now();
    while TcpStream::connect(dealer_addr(test, actor)).is_err() {
        assert!(!handle.is_finished(), "dealer {actor} exited at start");
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "dealer {actor} never listened"
        );
        sleep(Duration::from_millis(200));
    }
    Dealer {
        stop,
        handle: Some(handle),
        reference: DealerRef {
            address: dealer_addr(test, actor),
            pubkey: fixture::actor(actor).pubkey().to_string(),
        },
    }
}

impl Dealer {
    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().unwrap().join().unwrap().unwrap();
    }
}

/// A failed test still stops its dealer threads.
impl Drop for Dealer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn trader_cfg(net: &Net, dealers: &[&Dealer]) -> TraderConfig {
    TraderConfig {
        common: net.common(0),
        dealers: dealers.iter().map(|d| d.reference.clone()).collect(),
        band_bps: 50,
        min_ttl_secs: 5,
        max_ttl_secs: 60,
    }
}

fn expected_quote(net: &Net, chain: &Chain, amount: u64, spread_bps: u16) -> u64 {
    let pricer = net.common(0).pricer().unwrap();
    let fair = fair_amount(&pricer, chain, net.spl, SOL_MINT, amount).unwrap();
    apply_spread(fair, spread_bps).unwrap()
}

#[test]
fn best_dealer_settles_twice() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let net = start(5);
    let chain = net.chain();
    let d1 = spawn_dealer(&net, 5, 1, HONEST);
    let d2 = spawn_dealer(
        &net,
        5,
        2,
        Terms {
            spread_bps: 30,
            ..HONEST
        },
    );
    // Prices SPL at 3 USD: the highest quote, 50% above fair, outside the band.
    let d3 = spawn_dealer(
        &net,
        5,
        3,
        Terms {
            spl_price: 3,
            ..HONEST
        },
    );
    let cfg = trader_cfg(&net, &[&d1, &d2, &d3]);

    for amount in [400_000_000, 200_000_000] {
        let before: Vec<_> = (0..4).map(|a| net.balances(&chain, a)).collect();
        let quoted = expected_quote(&net, &chain, amount, 10);
        let outcome = trader::swap(&cfg, net.spl, SOL_MINT, amount).unwrap();
        assert_eq!(outcome, Outcome::Settled { buy_amount: quoted });
        let after: Vec<_> = (0..4).map(|a| net.balances(&chain, a)).collect();
        assert_eq!(
            after[0],
            (before[0].0 - amount, before[0].1 + quoted),
            "trader"
        );
        assert_eq!(
            after[1],
            (before[1].0 + amount, before[1].1 - quoted),
            "10 bps dealer"
        );
        assert_eq!(after[2], before[2], "30 bps dealer");
        assert_eq!(after[3], before[3], "out-of-band dealer");
    }
    for d in [d1, d2, d3] {
        d.stop();
    }
}

#[test]
fn unsettled_order_is_refunded_once() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let net = start(6);
    let chain = net.chain();
    // A dealer that quotes and never settles: only the quote server, no
    // settle loop, so the outcome does not depend on timing.
    let amount = 400_000_000;
    chain
        .register(&shielded(&fixture::actor(1)).unwrap())
        .unwrap();
    let buy_amount = expected_quote(&net, &chain, amount, HONEST.spread_bps);
    let expiry = chain.now().unwrap() + 20;
    let listener = TcpListener::bind(dealer_addr(6, 1)).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let cluster = chain.cluster;
    let server = thread::spawn(move || {
        serve(listener, &flag, |rfq: Rfq| {
            sign_quote(&fixture::actor(1), cluster, &rfq, buy_amount, expiry)
                .map_err(|e| e.to_string())
        })
    });
    let cfg = TraderConfig {
        dealers: vec![DealerRef {
            address: dealer_addr(6, 1),
            pubkey: fixture::actor(1).pubkey().to_string(),
        }],
        ..trader_cfg(&net, &[])
    };
    let before = net.balances(&chain, 0);

    let outcome = trader::swap(&cfg, net.spl, SOL_MINT, amount).unwrap();
    assert_eq!(outcome, Outcome::Refunded);
    assert_eq!(net.balances(&chain, 0), before);
    assert_eq!(net.balances(&chain, 1), (0, DEALER_SOL));
    assert!(
        trader::refund(&cfg.common).unwrap().acted.is_empty(),
        "second refund must be a no-op"
    );
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap().unwrap();
}

#[test]
fn unquoted_order_is_not_settled() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let net = start(7);
    let chain = net.chain();
    let d1 = spawn_dealer(&net, 7, 1, HONEST);
    let trader_kp: Keypair = fixture::actor(0);
    let kp = shielded(&trader_kp).unwrap();
    chain.register(&kp).unwrap();

    let dealer_pk = fixture::actor(1).pubkey();
    let source = chain.notes(&kp, net.spl).unwrap().remove(0);
    let expiry = chain.now().unwrap() + 40;
    let terms = OrderTerms {
        destination_mint: chain.mint(SOL_MINT).unwrap(),
        destination_amount: 1_234_567,
        maker: kp.shielded_address().unwrap(),
        expiry,
        taker: Some(identity(&dealer_pk).unwrap()),
    };
    let order = chain
        .lock(
            &kp,
            terms,
            100_000_000,
            source,
            chain.resolve(dealer_pk).unwrap(),
        )
        .unwrap();

    // The same dealer settles a quoted swap meanwhile, so it is live and
    // scanning while the unquoted order sits there.
    let cfg = trader_cfg(&net, &[&d1]);
    let outcome = trader::swap(&cfg, net.spl, SOL_MINT, 200_000_000).unwrap();
    assert!(matches!(outcome, Outcome::Settled { .. }), "{outcome:?}");

    while chain.now().unwrap() <= expiry {
        assert_eq!(chain.status(&order).unwrap(), Status::Open);
        sleep(Duration::from_secs(1));
    }
    assert_eq!(chain.status(&order).unwrap(), Status::Open);
    let refunded = trader::refund(&cfg.common).unwrap().acted;
    assert_eq!(
        refunded,
        vec![(hex(&order.hash(0).unwrap()), Status::Cancelled)]
    );
    d1.stop();
}

#[test]
fn a_declined_last_look_makes_one_split_and_keeps_its_note() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let net = start(10);
    let chain = net.chain();
    let cfg = dealer_cfg(&net, 10, 1, HONEST, None);
    let dealer = shielded(&fixture::actor(1)).unwrap();
    chain.register(&dealer).unwrap();
    let trader = shielded(&fixture::actor(0)).unwrap();
    chain.register(&trader).unwrap();
    let dealer_pk = fixture::actor(1).pubkey();
    let want = 1_234_567;
    let order = chain
        .lock(
            &trader,
            OrderTerms {
                destination_mint: chain.mint(SOL_MINT).unwrap(),
                destination_amount: want,
                maker: trader.shielded_address().unwrap(),
                expiry: chain.now().unwrap() + 120,
                taker: Some(identity(&dealer_pk).unwrap()),
            },
            100_000_000,
            chain.notes(&trader, net.spl).unwrap().remove(0),
            chain.resolve(dealer_pk).unwrap(),
        )
        .unwrap();
    let notes = || {
        let mut a: Vec<u64> = chain
            .notes(&dealer, SOL_MINT)
            .unwrap()
            .iter()
            .map(|n| n.utxo.amount)
            .collect();
        a.sort_unstable();
        a
    };
    let settle = |looks: &[bool]| {
        let mut looks = looks.iter().copied();
        dealer::settle_with(&cfg, &chain, &dealer, &order, &mut || looks.next().unwrap()).unwrap()
    };
    assert_eq!(settle(&[false]), dealer::Settle::Wait);
    assert_eq!(notes(), vec![DEALER_SOL], "declined before any split");
    assert_eq!(settle(&[true, false]), dealer::Settle::Wait);
    assert_eq!(
        notes(),
        vec![want, DEALER_SOL - want],
        "one split, given back"
    );
    assert_eq!(settle(&[true, false]), dealer::Settle::Wait);
    assert_eq!(notes(), vec![want, DEALER_SOL - want], "no second split");
    assert_eq!(settle(&[true, true]), dealer::Settle::Took);
    assert_eq!(chain.status(&order).unwrap(), Status::Settled);
    let expiry = chain.now().unwrap() + 20;
    let late = chain
        .lock(
            &trader,
            OrderTerms {
                destination_mint: chain.mint(SOL_MINT).unwrap(),
                destination_amount: want,
                maker: trader.shielded_address().unwrap(),
                expiry,
                taker: Some(identity(&dealer_pk).unwrap()),
            },
            100_000_000,
            chain.notes(&trader, net.spl).unwrap().remove(0),
            chain.resolve(dealer_pk).unwrap(),
        )
        .unwrap();
    while chain.now().unwrap() <= expiry {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let again = chain.exact_note(&dealer, SOL_MINT, want).unwrap();
    assert_eq!(notes(), vec![DEALER_SOL - 2 * want], "claimed");
    assert!(chain.take(&dealer, &late, again).is_err());
    assert_eq!(
        notes(),
        vec![want, DEALER_SOL - 2 * want],
        "a take refused at its send gives its note back"
    );
}

#[test]
fn http_quote_settles() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let net = start(8);
    let chain = net.chain();
    const KEY: &str = "e2e-http-key-0123456789";
    let key_file = net.keys.join("http-key");
    std::fs::write(&key_file, KEY).unwrap();
    let http = SocketAddr::from(([127, 0, 0, 1], 47_100 + 8));
    let d1 = spawn_dealer_with(
        &net,
        8,
        1,
        HONEST,
        Some(Http {
            listen: http,
            key: KeySource::File(key_file),
        }),
    );
    let kp = shielded(&fixture::actor(0)).unwrap();
    chain.register(&kp).unwrap();
    let amount: u64 = 400_000_000;
    let web = reqwest::blocking::Client::new();
    let body = serde_json::json!({
        "sell_mint": net.spl.to_string(),
        "buy_mint": SOL_MINT.to_string(),
        "sell_amount": amount.to_string(),
        "trader": kp.pubkey().to_string(),
    });
    let post = |key: &str| {
        web.post(format!("http://{http}/quote"))
            .header("x-keeler-key", key)
            .json(&body)
            .send()
            .unwrap()
    };
    assert_eq!(post("wrong-key").status(), 401);
    let reply: serde_json::Value = post(KEY).json().unwrap();
    let buy_amount: u64 = reply["quote"]["buy_amount"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let expiry = reply["quote"]["expiry"].as_u64().unwrap();
    assert_eq!(
        buy_amount,
        expected_quote(&net, &chain, amount, HONEST.spread_bps)
    );
    let rfq = Rfq {
        sell_mint: net.spl.to_string(),
        buy_mint: SOL_MINT.to_string(),
        sell_amount: amount,
        trader: kp.pubkey().to_string(),
    };
    let signed = serde_json::from_value(serde_json::json!({
        "buy_amount": buy_amount,
        "expiry": expiry,
        "signature": reply["quote"]["signature"],
    }))
    .unwrap();
    assert!(signed_by(
        &fixture::actor(1).pubkey(),
        chain.cluster,
        &rfq,
        &signed
    ));
    let health: serde_json::Value = web
        .get(format!("http://{http}/health"))
        .header("x-keeler-key", KEY)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(health["ok"], true);
    assert!(health["now"].as_u64().unwrap() < expiry);

    // A restart between quote and lock: the quote comes back from the state
    // file, so the order still settles.
    d1.stop();
    let d1 = spawn_dealer_with(
        &net,
        8,
        1,
        HONEST,
        Some(Http {
            listen: http,
            key: KeySource::File(net.keys.join("http-key")),
        }),
    );

    // Lock against the HTTP quote, as the browser does.
    let before = net.balances(&chain, 0);
    let dealer_pk = fixture::actor(1).pubkey();
    let source = chain.notes(&kp, net.spl).unwrap().remove(0);
    let terms = OrderTerms {
        destination_mint: chain.mint(SOL_MINT).unwrap(),
        destination_amount: buy_amount,
        maker: kp.shielded_address().unwrap(),
        expiry,
        taker: Some(identity(&dealer_pk).unwrap()),
    };
    let order = chain
        .lock(
            &kp,
            terms,
            amount,
            source,
            chain.resolve(dealer_pk).unwrap(),
        )
        .unwrap();
    while chain.status(&order).unwrap() != Status::Settled {
        assert!(chain.now().unwrap() <= expiry, "not settled before expiry");
        sleep(Duration::from_secs(1));
    }
    assert_eq!(
        net.balances(&chain, 0),
        (before.0 - amount, before.1 + buy_amount)
    );
    d1.stop();
}

#[test]
fn deploy_tool_is_idempotent() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let net = start(9);
    let file = |name: &str, kp: &Keypair| {
        let path = net.keys.join(format!("{name}.json"));
        std::fs::write(
            &path,
            serde_json::to_string(&kp.to_bytes().to_vec()).unwrap(),
        )
        .unwrap();
        KeySource::File(path)
    };
    let seeded = |name: &str, seed: u8| file(name, &Keypair::new_from_array([seed; 32]));
    let payer = fixture::payer();
    let plan = Plan {
        network: net.network(),
        genesis_hash: deploy::genesis_hash(&net.ports.rpc_url()).unwrap(),
        deploy_key: file("deploy", &payer),
        program_key: seeded("program", 41),
        buffer_key: seeded("buffer", 42),
        program_so: env_path("KEELER_SWAP_PROGRAM_SO"),
        dealer_key: seeded("dealer", 43),
        faucet_key: seeded("faucet", 44),
        dealer_lamports: 500_000_000,
        faucet_lamports: 2_500_000_000,
        tokens: vec![
            PlanToken {
                symbol: "tUSD".into(),
                decimals: 6,
                key: seeded("tusd", 45),
                inventory: 1_000_000_000_000,
            },
            PlanToken {
                symbol: "tSOL".into(),
                decimals: 9,
                key: seeded("tsol", 46),
                inventory: 10_000_000_000_000,
            },
        ],
    };
    let rpc = SolanaRpc::new(net.ports.rpc_url());
    let key = |k: &KeySource| k.keypair().unwrap();
    let (program, buffer) = (key(&plan.program_key), key(&plan.buffer_key));
    let so = std::fs::read(&plan.program_so).unwrap();

    // A deploy that stopped after five buffer writes resumes from there.
    deploy::deploy_program(&rpc, &payer, &program, &buffer, &so, Some(5)).unwrap();
    let first = deploy::setup(&plan).unwrap();
    assert_eq!(first.program_id, program.pubkey().to_string());
    assert!(
        rpc.get_account(buffer.pubkey()).unwrap().is_none(),
        "the deploy closed the buffer"
    );

    let dealer = key(&plan.dealer_key);
    let faucet_key = key(&plan.faucet_key);
    let balances = || {
        [payer.pubkey(), dealer.pubkey(), faucet_key.pubkey()].map(|a| rpc.get_balance(a).unwrap())
    };
    let before = balances();
    assert_eq!(deploy::setup(&plan).unwrap(), first);
    assert_eq!(balances(), before, "a rerun sends nothing");

    let mints: Vec<Address> = first
        .mints
        .iter()
        .map(|(_, m, _)| m.parse().unwrap())
        .collect();
    let chain = Chain::connect(&net.network(), &mints).unwrap();
    let kp = shielded(&dealer).unwrap();
    for (mint, t) in mints.iter().zip(&plan.tokens) {
        assert_eq!(
            chain.balance(&kp, *mint).unwrap(),
            t.inventory,
            "{}",
            t.symbol
        );
        assert_eq!(chain.mint_decimals(*mint).unwrap(), t.decimals);
        let ata = pda::associated_token_address(&dealer.pubkey(), mint);
        let data = rpc.get_account(ata).unwrap().unwrap().data;
        assert_eq!(&data[64..72], &0u64.to_le_bytes(), "all deposited");
    }

    // The authority handoff refuses a missing or non-system account.
    let new = Keypair::new_from_array([47; 32]).pubkey();
    assert!(deploy::set_upgrade_authority(&rpc, &payer, &program.pubkey(), &new).is_err());
    assert!(deploy::set_upgrade_authority(&rpc, &payer, &program.pubkey(), &mints[0]).is_err());
    let ixs = faucet::instructions(&payer.pubkey(), &new, &[], 1_000_000);
    deploy::send(&rpc, &ixs, &payer, &[&payer]).unwrap();
    deploy::set_upgrade_authority(&rpc, &payer, &program.pubkey(), &new).unwrap();
    assert_eq!(
        deploy::upgrade_authority(&rpc, &program.pubkey()).unwrap(),
        Some(new)
    );
}
