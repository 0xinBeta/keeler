//! Thin calls into `swap-sdk` and `spr-client`. Every "now" here is chain
//! time from the Clock sysvar.
use std::{str::FromStr, time::Duration};

use anyhow::{anyhow, ensure, Context, Result};
use solana_address::Address;
use solana_instruction::Instruction;
use solana_signer::Signer;
use spr_client::{
    fetch_asset_id, user_registry, ComputeBudgetConfig, HistoryFilters, IndexerRpcConfig,
    ProofDataSource, ProverClient, Rpc, SignedPrivateTransaction, SolanaRpc, SprClient, SprIndexer,
    Submission, UtxoScan,
};
use spr_keypair::{ShieldedAddress, ShieldedKeypair};
use spr_transaction::{
    instructions::transact::ConfidentialTransaction, AssetRegistry, Mint, WalletUtxo, SOL_MINT,
};
use swap_sdk::{
    index::{index_maker, index_taker, MakerOrder, TakerOrder},
    instructions::{
        cancel::CancelTransactionParams, make::MakeTransactionParams, take::TakeTransactionParams,
    },
    state::{OrderTerms, OrderUtxo},
};

/// A swap or a split carries a proof and an SPP transact: the whole budget.
const COMPUTE_UNIT_LIMIT: u32 = 1_400_000;
const CLOCK_SYSVAR: &str = "SysvarC1ock11111111111111111111111111111111";
/// The SDK hashes discovered orders under tree id 0 only.
const TREE_ID: u16 = 0;

pub struct Chain {
    pub client: SprClient<SolanaRpc>,
    pub tree: Address,
    pub registry: AssetRegistry,
}

/// Where an order stands, read from its nullifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Open,
    Settled,
    Cancelled,
}

impl Chain {
    /// Connects, checks the default tree is tree id 0, and registers the
    /// pool's asset id of every mint in `mints`.
    pub fn connect(rpc_url: &str, indexer_url: &str, mints: &[Address]) -> Result<Self> {
        // The prover is the local one (`ZOLANA_PROVER_URL`); proof data comes
        // from our own indexer.
        let client = SprClient::new(
            SolanaRpc::new(rpc_url),
            SprIndexer::new(indexer_url),
            ProverClient::default(),
        )
        .with_proof_data_source(ProofDataSource::Client);
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
        let ts = data
            .get(32..40)
            .ok_or_else(|| anyhow!("short Clock sysvar"))?;
        Ok(u64::try_from(i64::from_le_bytes(ts.try_into()?))?)
    }

    pub fn mint_decimals(&self, mint: Address) -> Result<u8> {
        if mint == SOL_MINT {
            return Ok(9);
        }
        let account = self
            .client
            .rpc()
            .get_account(mint)?
            .ok_or_else(|| anyhow!("mint {mint} not found"))?;
        // SPL mint layout: authority option (36), supply (8), then decimals.
        ensure!(account.data.len() >= 82, "{mint} is not a mint account");
        Ok(account.data[44])
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
            .filter(|u| u.utxo.ring_program_id.is_none())
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
    /// larger note when there is none.
    pub fn exact_note(
        &self,
        kp: &ShieldedKeypair,
        mint: Address,
        amount: u64,
    ) -> Result<WalletUtxo> {
        let notes = self.notes(kp, mint)?;
        if let Some(note) = notes.iter().find(|u| u.utxo.amount == amount) {
            return Ok(note.clone());
        }
        let source = notes
            .into_iter()
            .filter(|u| u.utxo.amount > amount)
            .min_by_key(|u| u.utxo.amount)
            .ok_or_else(|| anyhow!("no note of {mint} covers {amount}"))?;
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
        self.notes(kp, mint)?
            .into_iter()
            .find(|u| u.utxo.amount == amount)
            .ok_or_else(|| anyhow!("split note of {amount} not found after self-transfer"))
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
        let ix = make
            .prove(self.client.indexer(), self.tree, kp)?
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
        let ix = take
            .prove(self.client.indexer(), self.tree, kp)?
            .instruction()?;
        self.send(kp, ix)
    }

    pub fn cancel(&self, kp: &ShieldedKeypair, order: &OrderUtxo) -> Result<()> {
        let ix = CancelTransactionParams {
            order_utxo: order.clone(),
            order_leaf_index: self.leaf_index(order)?,
            payer: kp.pubkey(),
            tree_id: TREE_ID,
        }
        .build(kp)?
        .prove(self.client.indexer(), self.tree)?
        .instruction()?;
        self.send(kp, ix)
    }

    /// Sends one instruction as a v1 message and waits until it is indexed.
    fn send(&self, kp: &ShieldedKeypair, ix: Instruction) -> Result<()> {
        let sig = self.client.rpc().create_and_send_transaction(
            std::slice::from_ref(&ix),
            kp.pubkey(),
            &[kp],
            ComputeBudgetConfig::new(COMPUTE_UNIT_LIMIT),
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

    /// Every order ever designated to `kp`; none found in `wait` is empty.
    pub fn taker_orders(&self, kp: &ShieldedKeypair, wait: Duration) -> Result<Vec<TakerOrder>> {
        empty_on_timeout(index_taker(
            kp,
            &self.registry,
            self.client.indexer(),
            self.client.rpc(),
            wait,
        ))
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
