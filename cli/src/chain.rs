//! Thin calls into `swap-sdk` and `spr-client`. Every "now" here is chain
//! time from the Clock sysvar.
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, ensure, Context, Result};
use solana_address::Address;
use solana_instruction::Instruction;
use solana_signer::Signer;
use spr_client::{
    fetch_asset_id,
    prover::tee::{SignedTeePolicy, TeeNetwork},
    user_registry, ComputeBudgetConfig, HistoryFilters, IndexerRpcConfig, ProofAuthority,
    ProofDataSource, ProverClient, Rpc, SignedPrivateTransaction, SolanaRpc, SprClient, SprIndexer,
    Submission, UtxoScan,
};
use spr_interface::instruction::instruction_data::transact::TransactIxData;
use spr_keypair::{ShieldedAddress, ShieldedKeypair};
use spr_program::ProgramOwner;
use spr_transaction::{
    instructions::transact::{ConfidentialTransaction, SppProofInputs},
    AssetRegistry, Mint, WalletUtxo, SOL_MINT,
};
use swap_sdk::{
    index::{index_maker, scan_taker, MakerOrder, TakerOrder},
    instructions::{
        cancel::{Cancel, CancelTransactionParams},
        make::{Make, MakeTransactionParams},
        take::{Take, TakeTransactionParams},
    },
    proof::{self, CircuitId},
    shared::TakeAuthority,
    state::{OrderTerms, OrderUtxo},
};

use crate::config::{Network, Prover};

/// A swap or a split carries a proof and an SPP transact: the whole budget.
const COMPUTE_UNIT_LIMIT: u32 = 1_400_000;
pub const CLOCK_SYSVAR: &str = "SysvarC1ock11111111111111111111111111111111";
/// The SDK hashes discovered orders under tree id 0 only.
const TREE_ID: u16 = 0;

/// The cluster, from its genesis hash: what the quote signature's network
/// byte and every mainnet refusal go by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cluster {
    Local,
    Devnet,
    Mainnet,
}

pub const MAINNET_GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
// Each checked against the cluster's public RPC (`getGenesisHash`), 2026-11-25.
pub const DEVNET_GENESIS: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
const TESTNET_GENESIS: &str = "4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY";

impl Cluster {
    pub fn of_genesis(hash: &str) -> Result<Self> {
        Ok(match hash {
            MAINNET_GENESIS => Cluster::Mainnet,
            DEVNET_GENESIS => Cluster::Devnet,
            TESTNET_GENESIS => bail!("testnet is not supported"),
            _ => Cluster::Local,
        })
    }

    /// The cluster a config's network name says: any name with `mainnet`
    /// in it, any with `devnet`, else a local network.
    pub fn of_name(name: &str) -> Self {
        if name.contains("mainnet") {
            Cluster::Mainnet
        } else if name.contains("devnet") {
            Cluster::Devnet
        } else {
            Cluster::Local
        }
    }

    /// The quote signature's network byte.
    pub fn byte(self) -> u8 {
        self as u8
    }
}

/// The cluster `rpc_url` serves, from `getGenesisHash`.
pub fn cluster_at(rpc_url: &str) -> Result<Cluster> {
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "getGenesisHash" });
    let v: serde_json::Value = reqwest::blocking::Client::new()
        .post(rpc_url)
        .timeout(Duration::from_secs(30))
        .json(&body)
        .send()
        .map_err(|_| anyhow!("getGenesisHash failed"))?
        .json()
        .map_err(|_| anyhow!("getGenesisHash returned no JSON"))?;
    Cluster::of_genesis(v["result"].as_str().context("no genesis hash")?)
}

/// A compute budget of `cu_limit` with `micro_lamports` per unit on top.
pub fn budget(cu_limit: u32, micro_lamports: u64) -> Result<ComputeBudgetConfig> {
    let b = ComputeBudgetConfig::new(cu_limit);
    let fee =
        u64::try_from((u128::from(cu_limit) * u128::from(micro_lamports)).div_ceil(1_000_000))
            .map_err(|_| anyhow!("the priority fee overflows"))?;
    Ok(if fee == 0 {
        b
    } else {
        b.with_priority_fee(fee)
    })
}

pub struct Chain {
    pub client: SprClient<SolanaRpc>,
    pub tree: Address,
    pub registry: AssetRegistry,
    pub cluster: Cluster,
    /// `network.priority_fee`, micro-lamports per compute unit.
    pub priority_fee: u64,
    /// Which network this is among the process's connections (a hash of the
    /// RPC URL): claims on one do not hide another's notes.
    scope: u64,
    /// Mint decimals never change.
    decimals: Mutex<HashMap<Address, u8>>,
}

/// Notes one part of this process picked to spend, by hash, with when. The
/// swap dealer and the lend keeper share one wallet in one process, so
/// [`Chain::notes`] hides a claimed note from both until its spend has
/// landed or died and the indexer has caught up. Scoped by network: local
/// test networks in one process hold fixture notes with the same hashes.
static CLAIMED: Mutex<Vec<(u64, [u8; 32], Instant)>> = Mutex::new(Vec::new());
const CLAIM: Duration = Duration::from_secs(180);

/// Unix time from the Clock sysvar's data.
pub fn clock_time(data: &[u8]) -> Result<i64> {
    let ts = data
        .get(32..40)
        .ok_or_else(|| anyhow!("short Clock sysvar"))?;
    Ok(i64::from_le_bytes(ts.try_into()?))
}

/// Where an order stands, read from its nullifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Open,
    Settled,
    Cancelled,
}

impl Chain {
    /// Claims `note` for a spend about to be built.
    pub fn claim(&self, note: &WalletUtxo) {
        if let Ok(mut c) = CLAIMED.lock() {
            c.retain(|(_, _, at)| at.elapsed() < CLAIM);
            c.push((self.scope, note.utxo_hash, Instant::now()));
        }
    }

    /// Gives back a claim whose spend failed before it could land.
    pub fn release(&self, hash: &[u8; 32]) {
        if let Ok(mut c) = CLAIMED.lock() {
            c.retain(|(scope, h, _)| !(*scope == self.scope && h == hash));
        }
    }

    fn claimed(&self, note: &WalletUtxo) -> bool {
        CLAIMED.lock().is_ok_and(|c| {
            c.iter().any(|(scope, h, at)| {
                *scope == self.scope && *h == note.utxo_hash && at.elapsed() < CLAIM
            })
        })
    }

    /// Connects, checks the default tree is tree id 0, and registers the
    /// pool's asset id of every mint in `mints`.
    pub fn connect(network: &Network, mints: &[Address]) -> Result<Self> {
        let (rpc_url, indexer_url) = network.urls()?;
        let scope = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            rpc_url.hash(&mut h);
            h.finish()
        };
        let cluster = cluster_at(&rpc_url)?;
        let client = match &network.prover {
            // The local prover (`ZOLANA_PROVER_URL`); proof data comes from
            // our own indexer.
            Prover::Local => SprClient::new(
                SolanaRpc::new(rpc_url),
                SprIndexer::new(&indexer_url),
                ProverClient::default(),
            )
            .with_proof_data_source(ProofDataSource::Client),
            // Helius serves the prover at the indexer URL, attested under the
            // network's signed TEE policy, and fetches proof data itself.
            Prover::Helius { .. } => {
                let tee = match reqwest::Url::parse(&network.rpc_url)?.host_str() {
                    Some("devnet.helius-rpc.com") => TeeNetwork::Devnet,
                    Some("mainnet.helius-rpc.com") => TeeNetwork::Mainnet,
                    _ => bail!("the Helius prover needs a devnet or mainnet Helius rpc_url"),
                };
                let prover = ProverClient::new(indexer_url.clone())
                    .with_signed_tee(SignedTeePolicy::for_network(tee));
                SprClient::new(
                    SolanaRpc::new(rpc_url),
                    SprIndexer::new(&indexer_url),
                    prover,
                )
                .with_proof_data_source(ProofDataSource::Prover)
            }
        };
        // The client's own sends (self-transfers) pay the same rate.
        let client = match budget(client.compute_budget().cu_limit, network.priority_fee)?
            .priority_fee_lamports
        {
            Some(fee) => client.with_priority_fee(fee),
            None => client,
        };
        let tree = spr_interface::pda::tree(TREE_ID);
        let account = client
            .rpc()
            .get_account(tree)?
            .ok_or_else(|| anyhow!("default tree {tree} is missing"))?;
        let tree_id = spr_interface::state::tree::read_tree_id(&account.data);
        ensure!(
            tree_id == Some(TREE_ID),
            "default tree has id {tree_id:?}, need 0"
        );
        let mut registry = AssetRegistry::default();
        for mint in mints {
            if *mint != SOL_MINT {
                let id = fetch_asset_id(client.rpc(), *mint)
                    .with_context(|| format!("asset id of {mint}"))?;
                registry.insert(id, *mint)?;
            }
        }
        Ok(Self {
            client,
            tree,
            registry,
            cluster,
            priority_fee: network.priority_fee,
            scope,
            decimals: Mutex::default(),
        })
    }

    /// Chain time: the Clock sysvar's `unix_timestamp`.
    pub fn now(&self) -> Result<u64> {
        let clock = Address::from_str(CLOCK_SYSVAR).expect("valid sysvar address");
        let data = self
            .client
            .rpc()
            .get_account(clock)?
            .ok_or_else(|| anyhow!("no Clock sysvar"))?
            .data;
        Ok(u64::try_from(clock_time(&data)?)?)
    }

    pub fn mint_decimals(&self, mint: Address) -> Result<u8> {
        if mint == SOL_MINT {
            return Ok(9);
        }
        if let Some(d) = self
            .decimals
            .lock()
            .ok()
            .and_then(|c| c.get(&mint).copied())
        {
            return Ok(d);
        }
        let account = self
            .client
            .rpc()
            .get_account(mint)?
            .ok_or_else(|| anyhow!("mint {mint} not found"))?;
        // SPL mint layout: authority option (36), supply (8), then decimals.
        ensure!(account.data.len() >= 82, "{mint} is not a mint account");
        if let Ok(mut c) = self.decimals.lock() {
            c.insert(mint, account.data[44]);
        }
        Ok(account.data[44])
    }

    /// Public SOL of `address`, in lamports.
    pub fn lamports(&self, address: Address) -> Result<u64> {
        Ok(self.client.rpc().get_balance(address)?)
    }

    pub fn mint(&self, mint: Address) -> Result<Mint> {
        Ok(self.registry.mint(&mint)?)
    }

    /// Spendable default-ring notes of `mint`.
    pub fn notes(&self, kp: &ShieldedKeypair, mint: Address) -> Result<Vec<WalletUtxo>> {
        let scan = UtxoScan::new(kp, &self.registry).fetch(self.client.indexer())?;
        Ok(scan
            .spendable
            .balances
            .get_balance(mint)
            .map(|b| b.utxos.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|u| u.utxo.ring_program_id.is_none() && !self.claimed(u))
            .collect())
    }

    pub fn balance(&self, kp: &ShieldedKeypair, mint: Address) -> Result<u64> {
        Ok(self.notes(kp, mint)?.iter().map(|u| u.utxo.amount).sum())
    }

    pub fn register(&self, kp: &ShieldedKeypair) -> Result<()> {
        user_registry::ensure_registered(self.client.rpc(), kp, kp)
            .map_err(|e| anyhow!("register: {e:?}"))?;
        Ok(())
    }

    pub fn resolve(&self, pubkey: Address) -> Result<ShieldedAddress> {
        user_registry::try_resolve_registered_address(self.client.rpc(), pubkey)
            .map_err(|e| anyhow!("resolve {pubkey}: {e:?}"))?
            .map(|r| r.address)
            .ok_or_else(|| anyhow!("{pubkey} is not in the user registry"))
    }

    /// A note of exactly `amount`, making one with a self-transfer from a
    /// larger note when there is none. Both are claimed.
    pub fn exact_note(
        &self,
        kp: &ShieldedKeypair,
        mint: Address,
        amount: u64,
    ) -> Result<WalletUtxo> {
        let notes = self.notes(kp, mint)?;
        if let Some(note) = notes.iter().find(|u| u.utxo.amount == amount) {
            self.claim(note);
            return Ok(note.clone());
        }
        let source = notes
            .into_iter()
            .filter(|u| u.utxo.amount > amount)
            .min_by_key(|u| u.utxo.amount)
            .ok_or_else(|| anyhow!("no note of {mint} covers {amount}"))?;
        self.claim(&source);
        let payer = kp.pubkey();
        let me = kp.shielded_address()?;
        let mut tx = ConfidentialTransaction::new(vec![source], payer)?;
        if mint == SOL_MINT {
            tx.transfer_sol(&me, amount)?;
        } else {
            tx.transfer(&me, mint, amount)?;
        }
        let signed = SignedPrivateTransaction {
            transaction: tx.encrypt(kp)?,
            settlement_transfers: Vec::new(),
        };
        Submission::new(&signed, payer, kp)
            .send_sync(&self.client, &[kp])
            .map_err(|e| anyhow!("self-transfer: {e:?}"))?;
        let note = self
            .notes(kp, mint)?
            .into_iter()
            .find(|u| u.utxo.amount == amount)
            .ok_or_else(|| anyhow!("split note of {amount} not found after self-transfer"))?;
        self.claim(&note);
        Ok(note)
    }

    /// Locks `source_amount` of `source` into an order designated to `taker`.
    pub fn lock(
        &self,
        kp: &ShieldedKeypair,
        terms: OrderTerms,
        source_amount: u64,
        source: WalletUtxo,
        taker: ShieldedAddress,
    ) -> Result<OrderUtxo> {
        let payer = kp.pubkey();
        let make = MakeTransactionParams {
            terms,
            source_amount,
            source,
            taker_address: taker,
            payer,
        }
        .build(kp)?;
        let order = make.order_utxo.clone();
        let ix = Make {
            payer,
            viewing_pubkey: make.viewing_pubkey,
            tree: self.tree,
            spp_proof: self.prove_spp(make.spp_proof_inputs, kp)?,
            make_proof: proof::prove(CircuitId::Make, &make.proof_inputs)?,
        }
        .instruction()?;
        self.send(kp, ix)?;
        Ok(order)
    }

    pub fn take(&self, kp: &ShieldedKeypair, order: &OrderUtxo, note: WalletUtxo) -> Result<()> {
        let take = TakeTransactionParams {
            order_utxo: order.clone(),
            order_leaf_index: self.leaf_index(order)?,
            taker_input: note,
            payer: kp.pubkey(),
            tree_id: TREE_ID,
        }
        .build(kp)?;
        let ix = Take {
            payer: kp.pubkey(),
            maker: take.maker,
            tree: self.tree,
            spp_proof: self.prove_spp(take.spp_proof_inputs, &TakeAuthority(kp))?,
            take_proof: proof::prove(CircuitId::Take, &take.proof_inputs)?,
        }
        .instruction()?;
        self.send(kp, ix)
    }

    pub fn cancel(&self, kp: &ShieldedKeypair, order: &OrderUtxo) -> Result<()> {
        let cancel = CancelTransactionParams {
            order_utxo: order.clone(),
            order_leaf_index: self.leaf_index(order)?,
            payer: kp.pubkey(),
            tree_id: TREE_ID,
        }
        .build(kp)?;
        // `build` checked the terms name this maker.
        let ix = Cancel {
            maker: kp.pubkey(),
            payer: kp.pubkey(),
            tree: self.tree,
            order_expiry: order.terms.expiry,
            spp_proof: self.prove_spp(cancel.spp_proof_inputs, &ProgramOwner::nullifier_key())?,
            cancel_proof: proof::prove(CircuitId::Cancel, &cancel.proof_inputs)?,
        }
        .instruction()?;
        self.send(kp, ix)
    }

    /// Proves the SPP transact through this client's prover (the swap SDK's
    /// own `prove` always uses the local one), and checks the proof covers
    /// the transaction the swap proof covers.
    fn prove_spp(
        &self,
        inputs: SppProofInputs,
        authority: &dyn ProofAuthority,
    ) -> Result<TransactIxData> {
        let hash = inputs
            .padding_independent_private_tx_hash()
            .map_err(|e| anyhow!("private tx hash: {e:?}"))?;
        let spp = self
            .client
            .prove_transact(inputs, None, authority)
            .map_err(|e| anyhow!("prove transact: {e:?}"))?;
        ensure!(
            spp.private_tx_hash == hash,
            "the SPP proof and the swap proof cover different transactions"
        );
        Ok(spp)
    }

    /// Sends one instruction as a v1 message and waits until it is indexed.
    fn send(&self, kp: &ShieldedKeypair, ix: Instruction) -> Result<()> {
        let sig = self.client.rpc().create_and_send_transaction(
            std::slice::from_ref(&ix),
            kp.pubkey(),
            &[kp],
            budget(COMPUTE_UNIT_LIMIT, self.priority_fee)?,
        )?;
        self.client
            .confirm_private_transaction_sync(sig)
            .map_err(|e| anyhow!("confirm {sig}: {e:?}"))?;
        Ok(())
    }

    /// The order's leaf index; waits for the indexer to have it.
    fn leaf_index(&self, order: &OrderUtxo) -> Result<u64> {
        let proofs =
            self.client
                .indexer()
                .get_merkle_proofs(self.tree, vec![order.hash(TREE_ID)?], None)?;
        Ok(proofs
            .proofs
            .first()
            .ok_or_else(|| anyhow!("order not indexed"))?
            .leaf_index)
    }

    pub fn nullifier(&self, order: &OrderUtxo) -> Result<[u8; 32]> {
        Ok(order
            .to_input_utxo(TREE_ID, self.leaf_index(order)?)?
            .nullifier)
    }

    /// Spent or not, and if spent whether by a take (the maker's derived
    /// payout is a tree-0 leaf the spending transaction appended) or by a
    /// cancel. Read as of the current slot.
    pub fn status(&self, order: &OrderUtxo) -> Result<Status> {
        let nullifier = self.nullifier(order)?;
        let at = IndexerRpcConfig::at_slot(self.client.rpc().get_slot()?);
        let page = self
            .client
            .indexer()
            .get_shielded_transactions_by_nullifiers(
                vec![nullifier],
                None,
                None,
                HistoryFilters::default(),
                Some(at),
            )?;
        let Some(spend) = page
            .data
            .iter()
            .flat_map(|g| &g.events)
            .find(|tx| tx.nullifiers.contains(&nullifier))
        else {
            return Ok(Status::Open);
        };
        let payout = order
            .derived_destination_output(&nullifier)?
            .hash(TREE_ID)
            .map_err(|e| anyhow!("payout hash: {e:?}"))?;
        // The indexer answers getMerkleProofs for a missing leaf with a
        // generic error, so read the leaves the spend appended instead.
        let paid = spend
            .output_slots
            .iter()
            .any(|s| s.output_context.hash == payout && s.output_context.tree_id == TREE_ID);
        Ok(if paid {
            Status::Settled
        } else {
            Status::Cancelled
        })
    }

    /// Orders designated to `kp` whose order hash is not in `skip`. The
    /// SDK's `index_taker` resolves every order's maker on every call; this
    /// skips known orders before that, so a poll costs one indexer scan plus
    /// lookups for new orders only.
    // shortcut: still pages through every tagged transaction each call;
    // keep the last cursor if order history grows large.
    pub fn taker_orders(
        &self,
        kp: &ShieldedKeypair,
        skip: &HashSet<[u8; 32]>,
    ) -> Result<Vec<TakerOrder>> {
        let tag = kp
            .signing_pubkey()
            .confidential_view_tag()
            .map_err(|e| anyhow!("view tag: {e:?}"))?;
        let mut found = Vec::new();
        let mut cursor = None;
        loop {
            let page = self.client.indexer().get_shielded_transactions_by_tags(
                vec![tag],
                cursor,
                None,
                HistoryFilters::default(),
                None,
            )?;
            for tx in page.data.iter().flat_map(|group| &group.events) {
                let Some(candidate) = scan_taker(tx, kp, &self.registry)? else {
                    continue;
                };
                if skip.contains(&candidate.order_utxo_hash) {
                    continue;
                }
                // As the SDK does: a maker without a registry record, or an
                // order that does not rebuild to its leaf, is skipped.
                let Some(maker) = self.registered_maker(candidate.maker_pubkey)? else {
                    continue;
                };
                if let Ok(order) = candidate.into_order(maker, &self.registry) {
                    found.push(order);
                }
            }
            cursor = page
                .continuation(cursor)
                .map_err(|e| anyhow!("indexer page: {e:?}"))?;
            if cursor.is_none() {
                return Ok(found);
            }
        }
    }

    /// The SDK's `resolve_maker`: only a record the user registry owns counts.
    fn registered_maker(&self, maker: Address) -> Result<Option<ShieldedAddress>> {
        let (record, _) = spr_user_registry_interface::user_record_pda(&maker);
        let owned =
            self.client.rpc().get_account(record)?.is_some_and(|a| {
                a.owner == spr_user_registry_interface::user_registry_program_id()
            });
        if !owned {
            return Ok(None);
        }
        match user_registry::try_resolve_registered_address(self.client.rpc(), maker) {
            Ok(r) => Ok(r.map(|r| r.address)),
            Err(spr_client::ClientError::AddressResolution(_)) => Ok(None),
            Err(e) => Err(anyhow!("resolve maker {maker}: {e:?}")),
        }
    }

    /// Every order `kp` ever made; none found in `wait` is empty.
    pub fn maker_orders(&self, kp: &ShieldedKeypair, wait: Duration) -> Result<Vec<MakerOrder>> {
        empty_on_timeout(index_maker(kp, &self.registry, self.client.indexer(), wait))
    }
}

/// The SDK's discovery errors when it finds nothing before its timeout.
// shortcut: matches the SDK's message text; upgrade if swap-sdk gets a typed
// timeout error.
fn empty_on_timeout<T>(found: Result<Vec<T>>) -> Result<Vec<T>> {
    match found {
        Err(e) if e.to_string().starts_with("timed out discovering") => Ok(Vec::new()),
        other => other,
    }
}

/// The Solana keypair as the shielded keypair the SDK signs with.
pub fn shielded(kp: &solana_keypair::Keypair) -> Result<ShieldedKeypair> {
    ShieldedKeypair::from_keypair(kp).map_err(|e| anyhow!("shielded keypair: {e:?}"))
}

pub fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clusters_by_genesis_and_name() {
        assert_eq!(
            Cluster::of_genesis(MAINNET_GENESIS).unwrap(),
            Cluster::Mainnet
        );
        assert_eq!(
            Cluster::of_genesis(DEVNET_GENESIS).unwrap(),
            Cluster::Devnet
        );
        assert!(Cluster::of_genesis(TESTNET_GENESIS).is_err());
        assert_eq!(Cluster::of_genesis("local").unwrap(), Cluster::Local);
        assert_eq!(Cluster::of_name("mainnet-beta"), Cluster::Mainnet);
        assert_eq!(Cluster::of_name("devnet"), Cluster::Devnet);
        assert_eq!(Cluster::of_name("localnet"), Cluster::Local);
        assert_eq!(
            [Cluster::Local, Cluster::Devnet, Cluster::Mainnet].map(Cluster::byte),
            [0, 1, 2]
        );
    }

    #[test]
    fn priority_fee_is_per_compute_unit() {
        assert_eq!(budget(1_400_000, 0).unwrap().priority_fee_lamports, None);
        // 1,000 micro-lamports a unit over 1.4M units: 1,400 lamports.
        assert_eq!(
            budget(1_400_000, 1_000).unwrap().priority_fee_lamports,
            Some(1_400)
        );
        assert_eq!(budget(200_000, 1).unwrap().priority_fee_lamports, Some(1));
        assert!(
            budget(u32::MAX, u64::MAX).is_err(),
            "an overflow is an error"
        );
    }
}
