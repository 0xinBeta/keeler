use std::{process::ExitCode, sync::atomic::AtomicBool};

use anyhow::{bail, Result};
use keeler::{
    config::{parse_address, DealerConfig, TraderConfig},
    dealer, keeper, lend_chain, trader,
};

const USAGE: &str = "usage: keeler swap <config> <sell_mint> <buy_mint> <amount>
       keeler dealer <config>
       keeler refund <config>
       keeler keeper <keeper config>
       keeler lend-init-reserve <keeper config>
       keeler lend-set-price <keeper config> <vault> <price x1e12> <confidence x1e12>";

fn main() -> ExitCode {
    // A panic in any thread ends the process, so the supervisor restarts the
    // dealer rather than leave quoting up with settling dead. This is
    // `panic = "abort"` without the profile setting, which breaks
    // `cargo test --release`. The message is redacted: it may quote an RPC
    // URL with the API key.
    std::panic::set_hook(Box::new(|info| {
        keeler::config::log(format_args!("panic: {info}"));
        std::process::abort();
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            keeler::config::log(format_args!("error: {e:#}"));
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    match args {
        [cmd, cfg, sell, buy, amount] if cmd == "swap" => {
            let cfg = TraderConfig::load(cfg)?;
            let amount = amount.parse()?;
            match trader::swap(&cfg, parse_address(sell)?, parse_address(buy)?, amount)? {
                trader::Outcome::Settled { buy_amount } => {
                    println!("settled: received {buy_amount}")
                }
                trader::Outcome::Refunded => println!("not settled before expiry: refunded"),
            }
        }
        [cmd, cfg] if cmd == "dealer" => {
            dealer::run(&DealerConfig::load(cfg)?, &AtomicBool::new(false))?;
        }
        [cmd, cfg] if cmd == "keeper" => {
            keeper::run_standalone(&keeper::KeeperConfig::load(cfg)?, &AtomicBool::new(false))?;
        }
        [cmd, cfg] if cmd == "lend-init-reserve" => {
            let cfg = keeper::KeeperConfig::load(cfg)?;
            let admin = cfg
                .lend
                .admin_key
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("lend.admin_key is not set"))?
                .keypair()?;
            let p = keeper::connect(&cfg.lend, &cfg.network, &cfg.key)?;
            println!(
                "reserve created: {}",
                lend_chain::init_reserve(&p.chain, &admin)?
            );
        }
        [cmd, cfg, vault, price, confidence] if cmd == "lend-set-price" => {
            let cfg = keeper::KeeperConfig::load(cfg)?;
            keeper::require_test_network(&cfg.lend)?;
            let p = keeper::connect(&cfg.lend, &cfg.network, &cfg.key)?;
            let want = keeper::TestPrice {
                vault: vault.parse()?,
                price: price.parse()?,
                confidence: confidence.parse()?,
            };
            // A running keeper re-posts what this file names, so the price stays set.
            keeper::write_private(&keeper::price_file(&p.state), &serde_json::to_vec(&want)?)?;
            let sig =
                lend_chain::set_test_price(&p.chain, want.vault, want.price, want.confidence)?;
            println!("test price set: {sig}");
        }
        [cmd, cfg] if cmd == "refund" => {
            let done = trader::refund(&TraderConfig::load(cfg)?.common)?;
            if done.found == 0 {
                println!("no orders found for the configured assets");
            } else if done.acted.is_empty() {
                println!(
                    "nothing to refund ({} orders, none expired and unspent)",
                    done.found
                );
            }
            for (order, status) in done.acted {
                println!("order {order}: {status:?}");
            }
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}
