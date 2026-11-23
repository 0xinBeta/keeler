use std::{process::ExitCode, sync::atomic::AtomicBool};

use anyhow::{bail, Result};
use keeler::{
    config::{parse_address, DealerConfig, TraderConfig},
    dealer, trader,
};

const USAGE: &str = "usage: keeler swap <config> <sell_mint> <buy_mint> <amount>
       keeler dealer <config>
       keeler refund <config>";

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
