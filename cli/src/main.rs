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
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
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
            if done.is_empty() {
                println!("nothing to refund");
            }
            for (order, status) in done {
                println!("order {order}: {status:?}");
            }
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}
