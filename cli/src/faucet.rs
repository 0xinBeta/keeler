//! Test tokens and fee SOL for a wallet, from a key that is the mint
//! authority of every faucet token. Off unless the dealer config has a
//! `faucet` section.
use std::{collections::HashMap, str::FromStr};

use anyhow::{anyhow, Result};
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use spr_client::{ComputeBudgetConfig, Rpc};
use spr_interface::pda;

use crate::{
    chain::Chain,
    config::{self, parse_address},
};

const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";

/// Claims per wallet and in total, per UTC day.
pub struct Limits {
    per_wallet: u32,
    cap: u32,
    day: u64,
    total: u32,
    wallets: HashMap<Address, u32>,
}

impl Limits {
    pub fn new(per_wallet: u32, cap: u32) -> Self {
        Self {
            per_wallet,
            cap,
            day: 0,
            total: 0,
            wallets: HashMap::new(),
        }
    }

    /// Counts one claim by `wallet` on `day`, or says which limit stops it.
    pub fn take(&mut self, wallet: Address, day: u64) -> Result<(), &'static str> {
        if day != self.day {
            self.day = day;
            self.total = 0;
            self.wallets.clear();
        }
        if self.total >= self.cap {
            return Err("faucet daily cap reached");
        }
        let used = self.wallets.entry(wallet).or_default();
        if *used >= self.per_wallet {
            return Err("this wallet has had its faucet claims for today");
        }
        *used += 1;
        self.total += 1;
        Ok(())
    }

    /// Gives back a claim whose transfer failed.
    pub fn undo(&mut self, wallet: Address) {
        if let Some(used) = self.wallets.get_mut(&wallet) {
            *used = used.saturating_sub(1);
            self.total = self.total.saturating_sub(1);
        }
    }
}

pub struct Faucet {
    key: Keypair,
    tokens: Vec<(Address, u64)>,
    lamports: u64,
}

impl Faucet {
    pub fn new(cfg: &config::Faucet) -> Result<Self> {
        Ok(Self {
            key: cfg.key.keypair()?,
            tokens: cfg
                .tokens
                .iter()
                .map(|t| Ok((parse_address(&t.mint)?, t.amount)))
                .collect::<Result<_>>()?,
            lamports: cfg.lamports,
        })
    }

    /// Mints every token to `wallet`'s associated token account and sends
    /// the fee SOL, in one transaction.
    pub fn pay(&self, chain: &Chain, wallet: Address) -> Result<()> {
        let ixs = instructions(&self.key.pubkey(), &wallet, &self.tokens, self.lamports);
        chain
            .client
            .rpc()
            .create_and_send_transaction(
                &ixs,
                self.key.pubkey(),
                &[&self.key],
                ComputeBudgetConfig::for_instruction_count(ixs.len()),
            )
            .map_err(|e| anyhow!("faucet transfer: {e:?}"))?;
        Ok(())
    }
}

/// Creates `wallet`'s token accounts, mints each token to them and sends
/// `lamports`. Hand-built by the fixed SPL layouts: the crates
/// that define them are not direct dependencies.
pub fn instructions(
    authority: &Address,
    wallet: &Address,
    tokens: &[(Address, u64)],
    lamports: u64,
) -> Vec<Instruction> {
    let system = Address::from_str(SYSTEM_PROGRAM).expect("valid address");
    let token_program = pda::spl_token_program_id();
    let mut ixs = Vec::new();
    for (mint, amount) in tokens {
        let ata = pda::associated_token_address(wallet, mint);
        ixs.push(Instruction {
            program_id: pda::associated_token_program_id(),
            accounts: vec![
                AccountMeta::new(*authority, true),
                AccountMeta::new(ata, false),
                AccountMeta::new_readonly(*wallet, false),
                AccountMeta::new_readonly(*mint, false),
                AccountMeta::new_readonly(system, false),
                AccountMeta::new_readonly(token_program, false),
            ],
            // CreateIdempotent
            data: vec![1],
        });
        let mut data = vec![7]; // MintTo
        data.extend_from_slice(&amount.to_le_bytes());
        ixs.push(Instruction {
            program_id: token_program,
            accounts: vec![
                AccountMeta::new(*mint, false),
                AccountMeta::new(ata, false),
                AccountMeta::new_readonly(*authority, true),
            ],
            data,
        });
    }
    let mut data = vec![2, 0, 0, 0]; // system Transfer
    data.extend_from_slice(&lamports.to_le_bytes());
    ixs.push(Instruction {
        program_id: system,
        accounts: vec![
            AccountMeta::new(*authority, true),
            AccountMeta::new(*wallet, false),
        ],
        data,
    });
    ixs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::new_from_array([b; 32])
    }

    #[test]
    fn per_wallet_limit_and_global_cap() {
        let mut l = Limits::new(1, 2);
        assert!(l.take(addr(1), 10).is_ok());
        assert!(l.take(addr(1), 10).is_err(), "second claim, same wallet");
        assert!(l.take(addr(2), 10).is_ok());
        assert_eq!(l.take(addr(3), 10), Err("faucet daily cap reached"));
        // A new day resets both.
        assert!(l.take(addr(1), 11).is_ok());
        // A failed transfer gives its claim back.
        l.undo(addr(1));
        assert!(l.take(addr(1), 11).is_ok());
    }

    #[test]
    fn instructions_have_the_spl_layouts() {
        let ixs = instructions(&addr(1), &addr(2), &[(addr(3), 5)], 9);
        assert_eq!(ixs.len(), 3);
        assert_eq!(ixs[0].data, vec![1]);
        assert_eq!(
            ixs[0].accounts[1].pubkey,
            pda::associated_token_address(&addr(2), &addr(3))
        );
        assert_eq!(ixs[1].data, [&[7][..], &5u64.to_le_bytes()].concat());
        assert!(ixs[1].accounts[2].is_signer);
        assert_eq!(ixs[2].data, [&[2, 0, 0, 0][..], &9u64.to_le_bytes()].concat());
    }
}
