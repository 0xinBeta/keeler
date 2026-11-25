//! The keeper's [`Ledger`] on a real network, through the lend SDK's
//! builders, `spr-client` and plain JSON-RPC for the keeper's history.
use std::{str::FromStr, sync::Mutex, time::Duration};

use anyhow::{anyhow, bail, ensure, Context, Result};
use keeler_lend::{
    circuits::{
        DEPOSIT_POSITION_SLOT, DRAW_POSITION_SLOT, DRAW_RESERVE_SLOT, LIQUIDATE_POSITION_SLOT,
        LIQUIDATE_RESERVE_SLOT, REDEEM_RECEIPT_SLOT, REDEEM_RESERVE_SLOT, REPAY_POSITION_SLOT,
        REPAY_RESERVE_SLOT, SUPPLY_RECEIPT_SLOT, SUPPLY_RESERVE_SLOT,
    },
    instructions::{
        deposit::DepositArgs, liquidate::LiquidateArgs, repay::RepayArgs, supply::SupplyArgs,
    },
    tag,
    vaults::{self, PYTH_RECEIVER, TSOL_TUSD},
};
use keeler_lend_sdk::{
    builders::{self, recover_output, Prepared},
    note_data_hash, pool_pda, user_pda,
    vault::{self, test_price_account},
    IntentState, NoteKind, PositionState, ReceiptState, ReserveState,
};
use serde_json::{json, Value};
use solana_address::Address;
use solana_keypair::Keypair;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use spr_client::{compile_message, user_registry, HistoryFilters, PaginationToken, Rpc};
use spr_interface::event::OutputDataEncoding;
use spr_keypair::{ShieldedAddress, ShieldedKeypair};
use spr_program::ProgramOwner;
use spr_transaction::{
    data::{Data, DataRecord},
    instructions::transact::ConfidentialTransaction,
    serialization::confidential::{Confidential, ConfidentialOutputPlaintext},
    DecodeCx, EncryptedScheme, Mint, ShieldedTransaction, SppProofOutputUtxo, UtxoSerialization,
    WalletUtxo, SOL_ASSET_ID,
};

use crate::{
    chain::{budget, shielded, Chain},
    config::Network,
    keeper::{
        Built, Intent, Job, Ledger, LendPrice, Note, Op, Pending, Quote, Rejected, KIND_INTENT,
        KIND_POSITION, KIND_RECEIPT, KIND_RESERVE,
    },
    lend_auth::to_hex,
};

const TREE_ID: u16 = 0;
const BUDGET: u32 = 1_400_000;
/// Inputs a self-transfer merging liquidation funds may take.
const MAX_MERGE: usize = 4;

fn err(e: impl std::fmt::Debug) -> anyhow::Error {
    anyhow!("{e:?}")
}

pub struct LendChain {
    pub chain: Chain,
    key: Keypair,
    pub kp: ShieldedKeypair,
    rpc_url: String,
    /// One client for `rpc_call`, so a history replay keeps its connection.
    http: reqwest::blocking::Client,
    price: LendPrice,
    debt: Mint,
    /// Micro-lamports per compute unit on liquidations.
    liquidation_fee: u64,
    /// The same network at the liquidation fee, for the funding
    /// self-transfer; `None` when the fees are equal.
    fee_chain: Option<Chain>,
    /// The funds notes the last liquidation build claimed: given back when
    /// it fails or is refused.
    claims: Mutex<Vec<[u8; 32]>>,
}

impl LendChain {
    /// `kp` is the keeper's lend keypair: `key` under the pool viewing key.
    pub fn connect(
        network: &Network,
        key: &Keypair,
        kp: ShieldedKeypair,
        price: LendPrice,
        liquidation_fee: u64,
    ) -> Result<Self> {
        ensure!(
            kp.pubkey() == key.pubkey(),
            "the lend keypair is not the keeper key's"
        );
        let mints = [TSOL_TUSD.debt_mint, TSOL_TUSD.collateral_mint];
        let chain = Chain::connect(network, &mints)?;
        let debt = chain.mint(TSOL_TUSD.debt_mint)?;
        let fee_chain = if liquidation_fee == network.priority_fee {
            None
        } else {
            let mut at_fee = network.clone();
            at_fee.priority_fee = liquidation_fee;
            Some(Chain::connect(&at_fee, &mints)?)
        };
        Ok(Self {
            kp,
            key: key.insecure_clone(),
            rpc_url: network.urls()?.0,
            http: reqwest::blocking::Client::new(),
            chain,
            price,
            debt,
            liquidation_fee,
            fee_chain,
            claims: Mutex::default(),
        })
    }

    fn claim(&self, note: &WalletUtxo) {
        self.chain.claim(note);
        if let Ok(mut c) = self.claims.lock() {
            c.push(note.utxo_hash);
        }
    }

    /// Gives back what the last liquidation build claimed.
    fn release_claims(&self) {
        if let Ok(mut c) = self.claims.lock() {
            for h in c.drain(..) {
                self.chain.release(&h);
            }
        }
    }

    fn rpc(&self) -> &impl Rpc {
        self.chain.client.rpc()
    }

    fn owner(&self, wallet: Option<&str>) -> Result<ProgramOwner> {
        Ok(match wallet {
            None => pool_pda(&TSOL_TUSD.debt_mint).owner,
            Some(w) => user_pda(&crate::config::parse_address(w)?).owner,
        })
    }

    fn mint_of(&self, asset_id: u64) -> Result<Mint> {
        if asset_id == SOL_ASSET_ID {
            return Ok(Mint::SOL);
        }
        self.chain.registry.resolve(asset_id).map_err(err)
    }

    /// The note as an output, to hash or spend.
    fn output(&self, n: &Note) -> Result<SppProofOutputUtxo> {
        let owner = self.owner(n.wallet.as_deref())?;
        Ok(SppProofOutputUtxo {
            asset: self.mint_of(n.asset_id)?,
            amount: n.amount,
            blinding: hex_array(&n.blinding)?,
            data_hash: Some(hex_array(&n.data_hash)?),
            owner_address: Some(owner.address(self.kp.viewing_pubkey()).map_err(err)?),
            data: Data::new(vec![DataRecord::UtxoData(unhex(&n.data)?)]),
            ..Default::default()
        })
    }

    /// The note as an input; it must be indexed.
    fn utxo(&self, n: &Note) -> Result<WalletUtxo> {
        let owner = self.owner(n.wallet.as_deref())?;
        let leaf = n.leaf.context("the note is not in the tree yet")?;
        let u = owner.input(&self.output(n)?, TREE_ID, leaf).map_err(err)?;
        ensure!(
            to_hex(&u.utxo_hash) == n.hash,
            "a note opening does not match its hash"
        );
        Ok(u)
    }

    /// The record of output `out` of a build, owned by `wallet`'s user PDA
    /// (or the pool PDA).
    fn note(&self, kind: u8, wallet: Option<&Address>, out: &SppProofOutputUtxo) -> Result<Note> {
        let w = wallet.map(Address::to_string);
        let owner = self.owner(w.as_deref())?;
        let input = owner.input(out, TREE_ID, 0).map_err(err)?;
        Ok(Note {
            kind,
            wallet: w,
            owner: None,
            mint: out.asset.asset.to_string(),
            asset_id: out.asset.asset_id,
            amount: out.amount,
            blinding: to_hex(&out.blinding),
            data: to_hex(out.data.utxo_data().context("no note data")?),
            data_hash: to_hex(&out.data_hash.context("no data hash")?),
            hash: to_hex(&input.utxo_hash),
            nullifier: to_hex(&input.nullifier),
            leaf: None,
        })
    }

    /// A decrypted output checked against the hash on chain: `None` unless
    /// it is a reserve, receipt, position or intent of `owner` (`wallet`).
    fn classify(
        &self,
        plain: &ConfidentialOutputPlaintext,
        wallet: Option<&Address>,
        hash: &[u8; 32],
        leaf: u64,
    ) -> Option<Note> {
        let data = plain.data.utxo_data()?;
        let (kind, data_hash) = if wallet.is_none() {
            let (addr, state) = data.split_at_checked(32)?;
            borsh::from_slice::<ReserveState>(state).ok()?;
            (
                KIND_RESERVE,
                note_data_hash(NoteKind::Reserve, state, Some(addr.try_into().ok()?)).ok()?,
            )
        } else if borsh::from_slice::<ReceiptState>(data).is_ok() {
            (
                KIND_RECEIPT,
                note_data_hash(NoteKind::Receipt, data, None).ok()?,
            )
        } else if borsh::from_slice::<PositionState>(data).is_ok() {
            (
                KIND_POSITION,
                note_data_hash(NoteKind::Position, data, None).ok()?,
            )
        } else if borsh::from_slice::<IntentState>(data).is_ok() {
            (
                KIND_INTENT,
                note_data_hash(NoteKind::Intent, data, None).ok()?,
            )
        } else {
            return None;
        };
        let asset = self.mint_of(plain.asset_id).ok()?;
        let n = Note {
            kind,
            wallet: wallet.map(Address::to_string),
            owner: None,
            mint: asset.asset.to_string(),
            asset_id: plain.asset_id,
            amount: plain.amount,
            blinding: to_hex(&plain.blinding),
            data: to_hex(data),
            data_hash: to_hex(&data_hash),
            hash: to_hex(hash),
            nullifier: String::new(),
            leaf: Some(leaf),
        };
        let u = self.utxo(&n).ok()?;
        Some(Note {
            nullifier: to_hex(&u.nullifier),
            ..n
        })
    }

    fn resolve_owner(&self, wallet: &str) -> Result<ShieldedAddress> {
        self.chain.resolve(crate::config::parse_address(wallet)?)
    }

    fn price_account(&self) -> Address {
        match self.price {
            LendPrice::Test => test_price_account(TSOL_TUSD.id),
            LendPrice::Pyth => TSOL_TUSD.price_account,
        }
    }

    /// A keeper note of the debt token holding exactly `need`, the amount a
    /// liquidation pays: one already that size, else the smallest larger one
    /// split, else the largest few merged, by a self-transfer of `need`.
    /// Every note it picks is claimed, so the swap dealer (same process)
    /// leaves it alone.
    fn funds(&self, need: u64) -> Result<WalletUtxo> {
        let mut notes = self.chain.notes(&self.kp, TSOL_TUSD.debt_mint)?;
        notes.sort_by_key(|u| u.utxo.amount);
        let amounts: Vec<u64> = notes.iter().map(|u| u.utxo.amount).collect();
        let inputs: Vec<WalletUtxo> = match plan_funds(&amounts, need)? {
            Funding::Exact(i) => {
                self.claim(&notes[i]);
                return Ok(notes[i].clone());
            }
            Funding::Split(i) => vec![notes[i].clone()],
            Funding::Merge(ix) => ix.iter().map(|i| notes[*i].clone()).collect(),
        };
        log!(
            "lend: making a note of exactly the repay from {} keeper notes for a liquidation",
            inputs.len()
        );
        inputs.iter().for_each(|n| self.claim(n));
        let payer = self.kp.pubkey();
        let me = self.kp.shielded_address().map_err(err)?;
        let mut tx = ConfidentialTransaction::new(inputs, payer).map_err(err)?;
        tx.transfer(&me, TSOL_TUSD.debt_mint, need).map_err(err)?;
        let signed = spr_client::SignedPrivateTransaction {
            transaction: tx.encrypt(&self.kp).map_err(err)?,
            settlement_transfers: Vec::new(),
        };
        // At the liquidation's fee: it is half of the liquidation.
        let client = &self.fee_chain.as_ref().unwrap_or(&self.chain).client;
        spr_client::Submission::new(&signed, payer, &self.kp)
            .send_sync(client, &[&self.kp])
            .map_err(|e| anyhow!("funding self-transfer: {e:?}"))?;
        // The indexer may lag the landed transfer by a few seconds.
        for _ in 0..30 {
            if let Some(n) = self
                .chain
                .notes(&self.kp, TSOL_TUSD.debt_mint)?
                .into_iter()
                .find(|u| u.utxo.amount == need)
            {
                self.claim(&n);
                return Ok(n);
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        bail!("the note of exactly {need} was not found after the self-transfer")
    }

    /// Proves `p`: the SPP side through the network's configured prover
    /// (Helius on devnet), as the swap dealer does, the lend side here.
    fn prove(&self, p: Prepared) -> Result<solana_instruction::Instruction> {
        let client = &self.chain.client;
        p.prove_with(self.chain.tree, |spp| {
            client
                .prove_transact(spp, None, &keeler_lend_sdk::Authority(&self.kp))
                .map_err(|e| anyhow!("prove transact: {e:?}"))
        })
    }

    /// Proves `p`, compiles it with the keeper paying and signs as the
    /// keeper; other signers' slots stay empty for them to fill.
    fn finish(&self, p: Prepared, fee: u64) -> Result<Vec<u8>> {
        let ix = self.prove(p)?;
        let (blockhash, _) = self.rpc().get_latest_blockhash()?;
        let message = compile_message(
            &self.key.pubkey(),
            std::slice::from_ref(&ix),
            blockhash,
            budget(BUDGET, fee)?,
        )?;
        let keys = message.static_account_keys();
        ensure!(
            keys.first() == Some(&self.key.pubkey()),
            "the keeper must pay"
        );
        let n = usize::from(message.header().num_required_signatures);
        let mine = self.key.sign_message(&message.serialize());
        let signatures = keys[..n]
            .iter()
            .map(|k| {
                if *k == self.key.pubkey() {
                    mine
                } else {
                    Signature::default()
                }
            })
            .collect();
        let tx = VersionedTransaction {
            signatures,
            message,
        };
        // The wire format: a v1 transaction is its message, then its signatures (bincode is not).
        Ok(wincode::serialize(&tx)?)
    }

    /// JSON-RPC that `spr-client` does not wrap.
    fn rpc_call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let v: Value = self
            .http
            .post(&self.rpc_url)
            .timeout(Duration::from_secs(30))
            .json(&body)
            .send()
            .map_err(|_| anyhow!("{method} failed"))?
            .json()
            .map_err(|_| anyhow!("{method} returned no JSON"))?;
        if let Some(e) = v.get("error") {
            bail!("{method}: {e}");
        }
        Ok(v["result"].clone())
    }

    /// The lend instruction of transaction `sig`: its tag and the wallet it
    /// acts for. A transaction the RPC does not have is an error: skipping
    /// it could drop a note.
    fn lend_instruction(&self, sig: &str, payer: &Address) -> Result<Option<(u8, Address)>> {
        let tx = self.rpc_call(
            "getTransaction",
            json!([sig, { "encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "confirmed" }]),
        )?;
        ensure!(!tx.is_null(), "the RPC does not have transaction {sig}");
        lend_instruction_of(&tx, payer)
    }
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    ensure!(s.len().is_multiple_of(2), "odd hex");
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(Into::into))
        .collect()
}

fn hex_array<const N: usize>(s: &str) -> Result<[u8; N]> {
    crate::lend_auth::hex_bytes(s).ok_or_else(|| anyhow!("bad hex"))
}

/// The notes a keeper transaction of lend instruction `t` always makes that
/// are not among the `found` kinds: the slots `build` records for that op.
/// Every one is opened, a redeem's or a liquidation's emptied receipt or
/// position too.
pub(crate) fn missing_kinds(t: u8, found: &[u8]) -> Vec<&'static str> {
    let made: &[u8] = match t {
        tag::INIT_RESERVE => &[KIND_RESERVE],
        tag::SUPPLY | tag::REDEEM => &[KIND_RESERVE, KIND_RECEIPT],
        tag::DEPOSIT | tag::DEPOSIT_ADD => &[KIND_POSITION],
        tag::DRAW | tag::REPAY | tag::LIQUIDATE => &[KIND_RESERVE, KIND_POSITION],
        _ => &[],
    };
    made.iter()
        .filter(|k| !found.contains(k))
        .map(|k| match *k {
            KIND_RESERVE => "reserve",
            KIND_RECEIPT => "receipt",
            _ => "position",
        })
        .collect()
}

/// The lend instruction of a `getTransaction` reply, when the keeper paid
/// for it: only the keeper's own transactions carry its notes. Anyone can
/// name the keeper's address in a transaction, and with the public test
/// keys anyone can prove a lend note under their own PDA.
pub(crate) fn lend_instruction_of(tx: &Value, keeper: &Address) -> Result<Option<(u8, Address)>> {
    if !tx["meta"]["err"].is_null() {
        return Ok(None);
    }
    let msg = &tx["transaction"]["message"];
    let keys: Vec<Address> = msg["accountKeys"]
        .as_array()
        .context("no account keys")?
        .iter()
        .filter_map(|k| Address::from_str(k.as_str()?).ok())
        .collect();
    if keys.first() != Some(keeper) {
        return Ok(None);
    }
    for ix in msg["instructions"].as_array().context("no instructions")? {
        let program = ix["programIdIndex"]
            .as_u64()
            .and_then(|i| keys.get(i as usize));
        if program != Some(&keeler_lend::ID) {
            continue;
        }
        let data = bs58::decode(ix["data"].as_str().unwrap_or_default()).into_vec()?;
        let Some((&t, mut rest)) = data.split_first() else {
            continue;
        };
        let account = |i: usize| {
            ix["accounts"][i]
                .as_u64()
                .and_then(|k| keys.get(k as usize).copied())
        };
        let parsed = |a: Option<[u8; 32]>| {
            a.map(Address::new_from_array)
                .with_context(|| format!("a keeper lend instruction {t} whose args do not parse"))
                .map(Some)
        };
        let wallet = match t {
            tag::INIT_RESERVE => Some(Address::default()),
            tag::SUPPLY => parsed(
                wincode::deserialize_from::<SupplyArgs>(&mut rest)
                    .ok()
                    .map(|a| a.maker),
            )?,
            tag::DEPOSIT | tag::DEPOSIT_ADD => parsed(
                wincode::deserialize_from::<DepositArgs>(&mut rest)
                    .ok()
                    .map(|a| a.owner),
            )?,
            tag::REPAY => parsed(
                wincode::deserialize_from::<RepayArgs>(&mut rest)
                    .ok()
                    .map(|a| a.owner),
            )?,
            tag::LIQUIDATE => parsed(
                wincode::deserialize_from::<LiquidateArgs>(&mut rest)
                    .ok()
                    .map(|a| a.owner),
            )?,
            tag::REDEEM | tag::DRAW => account(1),
            _ => None,
        };
        if let Some(w) = wallet {
            return Ok(Some((t, w)));
        }
    }
    Ok(None)
}

/// Where a liquidation's note of exactly the repay comes from, by index into
/// the keeper's notes sorted by amount.
#[derive(Debug, PartialEq, Eq)]
enum Funding {
    Exact(usize),
    Split(usize),
    Merge(Vec<usize>),
}

/// `amounts` ascending: a note of exactly `need`, else the smallest larger
/// one, else the largest `MAX_MERGE` together when they cover it.
fn plan_funds(amounts: &[u64], need: u64) -> Result<Funding> {
    if let Some(i) = amounts.iter().position(|a| *a == need) {
        return Ok(Funding::Exact(i));
    }
    if let Some(i) = amounts.iter().position(|a| *a > need) {
        return Ok(Funding::Split(i));
    }
    let top: Vec<usize> = (0..amounts.len()).rev().take(MAX_MERGE).collect();
    let total: u128 = top.iter().map(|i| u128::from(amounts[*i])).sum();
    ensure!(
        total >= u128::from(need),
        "the keeper holds too little of the debt token to liquidate"
    );
    Ok(Funding::Merge(top))
}

/// Whether a send error (its debug text) says the transaction can never
/// land: refused at preflight, failed on chain, or not confirmed before its
/// blockhash expired. Anything else (a timeout, a lost connection) may still.
// shortcut: matches spr-client 0.10's error text; recheck on an upgrade.
pub(crate) fn never_lands(error: &str) -> bool {
    error.contains("SendTransactionPreflightFailure")
        || error.contains("kind: TransactionError")
        || error.contains("before its blockhash expired")
}

/// Data-bearing confidential slots with their decryption index, as the swap
/// SDK counts them, and the raw payload for the writer key.
#[allow(clippy::type_complexity)]
fn slots(tx: &ShieldedTransaction) -> Vec<(u32, [u8; 32], u64, [u8; 32], Vec<u8>, Vec<u8>)> {
    let mut next = 0u32;
    let mut out = Vec::new();
    for slot in &tx.output_slots {
        let Some(data) = slot.output_data() else {
            continue;
        };
        let index = next;
        next += 1;
        let OutputDataEncoding::Encrypted(mut blob) = data else {
            continue;
        };
        let Some(&scheme) = blob.first() else {
            continue;
        };
        if EncryptedScheme::from_byte(scheme).ok() != Some(EncryptedScheme::Confidential) {
            continue;
        }
        blob.drain(..1);
        out.push((
            index,
            slot.output_context.hash,
            slot.output_context.leaf_index,
            slot.view_tag,
            blob,
            slot.payload.clone(),
        ));
    }
    out
}

impl Ledger for LendChain {
    fn now(&self) -> Result<u64> {
        self.chain.now()
    }

    fn registered(&self, wallet: &Address) -> Result<bool> {
        Ok(
            user_registry::try_resolve_registered_address(self.rpc(), *wallet)
                .map_err(|e| anyhow!("resolve {wallet}: {e:?}"))?
                .is_some(),
        )
    }

    fn spent(&self, nullifiers: &[[u8; 32]]) -> Result<Vec<bool>> {
        let mut out = Vec::with_capacity(nullifiers.len());
        for chunk in nullifiers.chunks(100) {
            let pdas: Vec<Address> =
                spr_program::instruction::nullifier_pda_accounts(&self.chain.tree, chunk.iter())
                    .into_iter()
                    .map(|m| m.pubkey)
                    .collect();
            let accounts = self.rpc().get_multiple_accounts(pdas)?;
            out.extend(accounts.iter().map(Option::is_some));
        }
        Ok(out)
    }

    fn spender(&self, nullifier: &[u8; 32]) -> Result<Option<Vec<([u8; 32], u64)>>> {
        let page = self
            .chain
            .client
            .indexer()
            .get_shielded_transactions_by_nullifiers(
                vec![*nullifier],
                None,
                None,
                HistoryFilters::default(),
                None,
            )?;
        Ok(page
            .data
            .iter()
            .flat_map(|g| &g.events)
            .find(|tx| tx.nullifiers.contains(nullifier))
            .map(|tx| {
                tx.output_slots
                    .iter()
                    .filter(|s| s.output_context.tree_id == TREE_ID)
                    .map(|s| (s.output_context.hash, s.output_context.leaf_index))
                    .collect()
            }))
    }

    fn intents(
        &self,
        wallet: &Address,
        after: Option<&str>,
        pages: usize,
    ) -> Result<(Vec<Intent>, Option<String>)> {
        let pda = user_pda(wallet).owner;
        let tag = pda
            .address(self.kp.viewing_pubkey())
            .map_err(err)?
            .signing_pubkey
            .confidential_view_tag()
            .map_err(err)?;
        let mut found = Vec::new();
        // Pages run oldest first; the cursor is the last transaction read.
        let mut cursor: Option<PaginationToken> = after
            .map(|a| a.parse().map_err(|_| anyhow!("bad intent cursor")))
            .transpose()?;
        let mut last = cursor;
        for _ in 0..pages {
            let page = self
                .chain
                .client
                .indexer()
                .get_shielded_transactions_by_tags(
                    vec![tag],
                    cursor,
                    None,
                    HistoryFilters::default(),
                    None,
                )?;
            if let Some(g) = page.data.last() {
                last = Some(PaginationToken::new(g.slot, g.signature));
            }
            for tx in page.data.iter().flat_map(|g| &g.events) {
                for (index, hash, leaf, view_tag, body, _) in slots(tx) {
                    if view_tag != tag {
                        continue;
                    }
                    let cx = DecodeCx::for_slot(&self.kp.viewing_key, tx, index);
                    let Ok(plain) = Confidential::decode(&body, &cx) else {
                        continue;
                    };
                    let Some(note) = self.classify(&plain, Some(wallet), &hash, leaf) else {
                        continue;
                    };
                    if note.kind != KIND_INTENT {
                        continue;
                    }
                    let Ok(s) = note.state::<IntentState>() else {
                        continue;
                    };
                    found.push(Intent {
                        purpose: s.purpose,
                        target: s.target,
                        expiry: s.expiry,
                        note,
                    });
                }
            }
            cursor = page.continuation(cursor).map_err(err)?;
            if cursor.is_none() {
                break;
            }
        }
        Ok((found, cursor.or(last).map(|t| t.to_string())))
    }

    fn quote(&self, vault_id: u64) -> Result<Quote> {
        ensure!(vault_id == TSOL_TUSD.id, "no such vault");
        let account = self
            .rpc()
            .get_account(self.price_account())?
            .context("no price account")?;
        let d = &account.data;
        let u64_at = |i: usize| -> Result<u64> {
            Ok(u64::from_le_bytes(
                d.get(i..i + 8).context("short price")?.try_into()?,
            ))
        };
        match self.price {
            LendPrice::Test => {
                ensure!(
                    account.owner == keeler_lend::ID && d.len() == vaults::TEST_PRICE_LEN,
                    "not a test price account"
                );
                let (price, conf) = (u64_at(0)?, u64_at(8)?);
                // The program refuses these, so the keeper must not act on them.
                ensure!(price > 0 && conf < price, "the test price is refused");
                Ok(Quote {
                    low: price - conf,
                    high: price
                        .checked_add(conf)
                        .context("the test price is refused")?,
                    published: u64_at(16)? as i64,
                })
            }
            LendPrice::Pyth => {
                ensure!(
                    account.owner == PYTH_RECEIVER,
                    "the price account is not Pyth's"
                );
                ensure!(
                    d.get(..8) == Some(&[34, 241, 35, 99, 157, 126, 244, 205][..])
                        && d.get(40) == Some(&1)
                        && d.get(41..73) == Some(&TSOL_TUSD.feed_id[..]),
                    "not a fully verified price update for the vault's feed"
                );
                let price = u64_at(73)? as i64;
                let conf = u64_at(81)?;
                let expo = i32::from_le_bytes(d.get(89..93).context("short price")?.try_into()?);
                let published = u64_at(93)? as i64;
                // The program refuses a confidence as wide as the price.
                ensure!(
                    price > 0 && u128::from(conf) < price as u128,
                    "no usable price"
                );
                let shift = expo + 12 + i32::from(TSOL_TUSD.debt_decimals)
                    - i32::from(TSOL_TUSD.collateral_decimals);
                let scale = |x: u128, up: bool| -> Result<u64> {
                    let v = if shift >= 0 {
                        x.checked_mul(10u128.checked_pow(shift as u32).context("price scale")?)
                            .context("price overflow")?
                    } else {
                        let d = 10u128.checked_pow((-shift) as u32).context("price scale")?;
                        if up {
                            x.div_ceil(d)
                        } else {
                            x / d
                        }
                    };
                    Ok(u64::try_from(v)?)
                };
                let p = price as u128;
                Ok(Quote {
                    low: scale(p.saturating_sub(u128::from(conf)), false)?,
                    high: scale(p + u128::from(conf), true)?,
                    published,
                })
            }
        }
    }

    fn set_price(&self, vault: u64, price: u64, confidence: u64) -> Result<String> {
        set_test_price(self, vault, price, confidence)
    }

    fn build(&self, job: &Job, reserve: Option<&Note>, now: u64) -> Result<Built> {
        if let Ok(mut c) = self.claims.lock() {
            c.clear();
        }
        let out = self.build_job(job, reserve, now);
        if out.is_err() {
            self.release_claims();
        }
        out
    }

    fn send(&self, tx: &[u8]) -> Result<String> {
        let tx: VersionedTransaction = wincode::deserialize(tx)?;
        match self.rpc().process_transaction(tx) {
            Ok(sig) => {
                if let Ok(mut c) = self.claims.lock() {
                    c.clear();
                }
                Ok(sig.to_string())
            }
            Err(e) => {
                let text = format!("{e:?}");
                Err(if never_lands(&text) {
                    self.release_claims();
                    anyhow::Error::new(Rejected(text))
                } else {
                    anyhow!("{text}")
                })
            }
        }
    }

    fn debt_held(&self) -> Result<u64> {
        self.chain.balance(&self.kp, TSOL_TUSD.debt_mint)
    }

    fn history(&self, payer: &Address, after: Option<&str>) -> Result<(Vec<Note>, Option<String>)> {
        self.history_of(payer, after)
    }
}

impl LendChain {
    #[allow(clippy::type_complexity)]
    fn build_job(&self, job: &Job, reserve: Option<&Note>, now: u64) -> Result<Built> {
        let salt = u128::from_le_bytes({
            let mut b = [0; 16];
            getrandom::getrandom(&mut b).map_err(|e| anyhow!("randomness: {e}"))?;
            b
        });
        let kp = &self.kp;
        let res = || -> Result<(Note, WalletUtxo)> {
            let r = reserve.context("no reserve")?;
            Ok((r.clone(), self.utxo(r)?))
        };
        // (op, wallet, prepared, spent notes, [(kind, slot, owner is the user)], index)
        let (op, wallet, p, spent, slots, index): (
            Op,
            String,
            Prepared,
            Vec<String>,
            Vec<(u8, usize)>,
            Option<u64>,
        ) = match job {
            Job::Supply { intent } => {
                let w = intent
                    .note
                    .wallet
                    .clone()
                    .context("intent without wallet")?;
                let (r, ru) = res()?;
                let p = builders::supply(
                    kp,
                    &self.resolve_owner(&w)?,
                    ru,
                    self.utxo(&intent.note)?,
                    now,
                    salt,
                    TREE_ID,
                )?;
                (
                    Op::Supply,
                    w,
                    p,
                    vec![r.nullifier, intent.note.nullifier.clone()],
                    vec![
                        (KIND_RESERVE, SUPPLY_RESERVE_SLOT),
                        (KIND_RECEIPT, SUPPLY_RECEIPT_SLOT),
                    ],
                    None,
                )
            }
            Job::DepositOpen { intent } => {
                let w = intent
                    .note
                    .wallet
                    .clone()
                    .context("intent without wallet")?;
                let p = vault::deposit_open(
                    kp,
                    &self.resolve_owner(&w)?,
                    self.utxo(&intent.note)?,
                    TSOL_TUSD.id,
                    now,
                    TREE_ID,
                )?;
                (
                    Op::DepositOpen,
                    w,
                    p,
                    vec![intent.note.nullifier.clone()],
                    vec![(KIND_POSITION, DEPOSIT_POSITION_SLOT)],
                    None,
                )
            }
            Job::DepositAdd { position, intent } => {
                let w = position.wallet.clone().context("position without wallet")?;
                let p = vault::deposit_add(
                    kp,
                    &self.resolve_owner(&w)?,
                    self.utxo(position)?,
                    self.utxo(&intent.note)?,
                    TSOL_TUSD.id,
                    now,
                    TREE_ID,
                )?;
                (
                    Op::DepositAdd,
                    w,
                    p,
                    vec![position.nullifier.clone(), intent.note.nullifier.clone()],
                    vec![(KIND_POSITION, DEPOSIT_POSITION_SLOT)],
                    None,
                )
            }
            Job::Repay { position, intent } => {
                let w = position.wallet.clone().context("position without wallet")?;
                let (r, ru) = res()?;
                let p = vault::repay(
                    kp,
                    &self.resolve_owner(&w)?,
                    ru,
                    self.utxo(position)?,
                    self.utxo(&intent.note)?,
                    TSOL_TUSD.id,
                    now,
                    salt,
                    TREE_ID,
                )?;
                (
                    Op::Repay,
                    w,
                    p,
                    vec![
                        r.nullifier,
                        position.nullifier.clone(),
                        intent.note.nullifier.clone(),
                    ],
                    vec![
                        (KIND_POSITION, REPAY_POSITION_SLOT),
                        (KIND_RESERVE, REPAY_RESERVE_SLOT),
                    ],
                    None,
                )
            }
            Job::Liquidate { position, bound } => {
                let w = position.wallet.clone().context("position without wallet")?;
                let (r, ru) = res()?;
                let s: PositionState = position.state()?;
                // The funds note holds exactly the repay, sized as the
                // builder sizes it at the same `now`.
                let plan = keeler_core::lend::liquidate(
                    s.debt_shares,
                    vault::accrued(&r.state()?, now)?.borrow_price,
                    position.amount,
                    *bound,
                    &keeler_core::lend::TSOL_TUSD_VAULT,
                )
                .map_err(|e| anyhow!("{e}"))?;
                let funds = self.funds(plan.repay)?;
                let funds_nullifier = to_hex(&funds.nullifier);
                let (p, _plan) = vault::liquidate(
                    kp,
                    &self.resolve_owner(&w)?,
                    ru,
                    self.utxo(position)?,
                    funds,
                    self.price_account(),
                    TSOL_TUSD.id,
                    now,
                    *bound,
                    salt,
                    TREE_ID,
                )?;
                (
                    Op::Liquidate,
                    w,
                    p,
                    vec![r.nullifier, position.nullifier.clone(), funds_nullifier],
                    vec![
                        (KIND_POSITION, LIQUIDATE_POSITION_SLOT),
                        (KIND_RESERVE, LIQUIDATE_RESERVE_SLOT),
                    ],
                    None,
                )
            }
            Job::Redeem { receipt, amount } => {
                let w = receipt.wallet.clone().context("receipt without wallet")?;
                let (r, ru) = res()?;
                let index = vault::accrued(&r.state()?, now)?.supply_price;
                let p = builders::redeem(
                    kp,
                    &self.resolve_owner(&w)?,
                    ru,
                    self.utxo(receipt)?,
                    *amount,
                    now,
                    salt,
                    TREE_ID,
                )?;
                (
                    Op::Redeem,
                    w,
                    p,
                    vec![r.nullifier, receipt.nullifier.clone()],
                    vec![
                        (KIND_RECEIPT, REDEEM_RECEIPT_SLOT),
                        (KIND_RESERVE, REDEEM_RESERVE_SLOT),
                    ],
                    Some(index),
                )
            }
            Job::Draw {
                position,
                borrow,
                withdraw,
                bound,
            } => {
                let w = position.wallet.clone().context("position without wallet")?;
                let (r, ru) = res()?;
                let index = vault::accrued(&r.state()?, now)?.borrow_price;
                let p = vault::draw(
                    kp,
                    &self.resolve_owner(&w)?,
                    ru,
                    self.utxo(position)?,
                    *borrow,
                    *withdraw,
                    self.price_account(),
                    TSOL_TUSD.id,
                    now,
                    *bound,
                    salt,
                    TREE_ID,
                )?;
                (
                    Op::Draw,
                    w,
                    p,
                    vec![r.nullifier, position.nullifier.clone()],
                    vec![
                        (KIND_POSITION, DRAW_POSITION_SLOT),
                        (KIND_RESERVE, DRAW_RESERVE_SLOT),
                    ],
                    Some(index),
                )
            }
        };
        let user = crate::config::parse_address(&wallet)?;
        let mut notes = Vec::new();
        for (kind, slot) in slots {
            let out = p.outputs.get(slot).context("missing output slot")?;
            let owner = if kind == KIND_RESERVE {
                None
            } else {
                Some(&user)
            };
            notes.push(self.note(kind, owner, out)?);
        }
        let outputs: Vec<String> = p
            .spp
            .external_data
            .outputs
            .iter()
            .map(|o| o.utxo_hash)
            .filter(|h| *h != [0; 32])
            .map(|h| to_hex(&h))
            .collect();
        let spends = reserve
            .filter(|_| !matches!(op, Op::DepositOpen | Op::DepositAdd))
            .map(|r| r.nullifier.clone());
        let fee = if op == Op::Liquidate {
            self.liquidation_fee
        } else {
            self.chain.priority_fee
        };
        let tx = self.finish(p, fee)?;
        Ok(Built {
            pending: Pending {
                op,
                wallet,
                spends,
                inputs: spent,
                outputs,
                notes,
                now,
                ticket: None,
                owner_signed: false,
                signature: None,
            },
            tx,
            index,
        })
    }

    fn history_of(
        &self,
        payer: &Address,
        after: Option<&str>,
    ) -> Result<(Vec<Note>, Option<String>)> {
        let mut sigs = Vec::new();
        // The mark moves only to a finalized signature: one merely confirmed
        // could still be dropped, and is read again next time.
        let mut newest = None;
        let mut before: Option<String> = None;
        // A node that does not know the mark ignores `until` and pages the
        // whole history: past the first page, stop below the mark's slot.
        let mut floor: Option<u64> = None;
        let mut oldest: Option<String> = None;
        let mut ended_short = false;
        'pages: loop {
            let mut opts = json!({ "limit": 1000, "commitment": "confirmed" });
            if let Some(b) = &before {
                opts["before"] = json!(b);
            }
            if let Some(a) = after {
                opts["until"] = json!(a);
            }
            let page =
                self.rpc_call("getSignaturesForAddress", json!([payer.to_string(), opts]))?;
            let page = page.as_array().context("no signatures")?.clone();
            for s in &page {
                if after.is_some_and(|a| s["signature"] == a)
                    || floor.is_some_and(|f| s["slot"].as_u64().is_some_and(|x| x < f))
                {
                    break 'pages;
                }
                oldest = s["signature"].as_str().map(str::to_owned);
                if newest.is_none() && s["confirmationStatus"] == "finalized" {
                    newest = s["signature"].as_str().map(str::to_owned);
                }
                if s["err"].is_null() {
                    if let Some(sig) = s["signature"].as_str() {
                        sigs.push(sig.to_owned());
                    }
                }
            }
            match page.last().and_then(|s| s["signature"].as_str()) {
                Some(last) if page.len() == 1000 => before = Some(last.to_owned()),
                _ => {
                    ended_short = true;
                    break;
                }
            }
            if let (Some(a), None) = (after, floor) {
                floor = self.rpc_call(
                    "getSignatureStatuses",
                    json!([[a], { "searchTransactionHistory": true }]),
                )?["value"][0]["slot"]
                    .as_u64();
            }
        }
        if ended_short {
            self.check_history_reaches(payer, oldest.as_deref(), after)?;
        }
        let mut found = Vec::new();
        for sig in sigs {
            let Some((t, wallet)) = self.lend_instruction(&sig, payer)? else {
                continue;
            };
            let signature = Signature::from_str(&sig).map_err(err)?;
            // A lagging indexer must not make recovery skip a note: wait for
            // it, then refuse to start rather than rebuild without it.
            let mut tries = 0;
            let indexed = loop {
                let indexed = self
                    .chain
                    .client
                    .indexer()
                    .get_shielded_transactions_by_signature(signature, None)?;
                if !indexed.transactions.is_empty() {
                    break indexed;
                }
                tries += 1;
                ensure!(tries < 60, "the indexer does not have {sig} after 60 s");
                if tries == 1 {
                    log!("lend: waiting for the indexer to have {sig}");
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            };
            let tx = &indexed.transactions[0].transaction;
            let (Some(first), Some(salt)) = (tx.nullifiers.first(), tx.salt) else {
                bail!(
                    "keeper transaction {sig} has no nullifier or salt; its notes cannot be opened"
                );
            };
            let mut kinds = Vec::new();
            for (index, hash, leaf, _, _, payload) in slots(tx) {
                let plain = std::iter::once(index)
                    .chain(0..8)
                    .find_map(|i| recover_output(&self.kp, first, &payload, salt, i).ok());
                let Some(plain) = plain else { continue };
                // The reserve, then the wallet's notes.
                let note = self.classify(&plain, None, &hash, leaf).or_else(|| {
                    (t != tag::INIT_RESERVE)
                        .then(|| self.classify(&plain, Some(&wallet), &hash, leaf))
                        .flatten()
                });
                if let Some(n) = note {
                    kinds.push(n.kind);
                    if n.kind != KIND_INTENT {
                        found.push(n);
                    }
                }
            }
            let lost = missing_kinds(t, &kinds);
            ensure!(
                lost.is_empty(),
                "keeper transaction {sig} has no readable {}; the scan does not move past it",
                lost.join(" or ")
            );
        }
        Ok((found, newest))
    }

    /// Refuses a signature list that stopped short: the one before `oldest`
    /// (the newest, with none read) must be the mark `after`, or none
    /// without a mark.
    fn check_history_reaches(
        &self,
        payer: &Address,
        oldest: Option<&str>,
        after: Option<&str>,
    ) -> Result<()> {
        let mut opts = json!({ "limit": 1, "commitment": "confirmed" });
        if let Some(o) = oldest {
            opts["before"] = json!(o);
        }
        let next = self.rpc_call("getSignaturesForAddress", json!([payer.to_string(), opts]))?;
        let next = next.as_array().context("no signatures")?;
        let next = next.first().and_then(|s| s["signature"].as_str());
        ensure!(
            next == after,
            "the RPC's signature list of {payer} stops short of {}; it is read again next time",
            after.unwrap_or("its first transaction")
        );
        Ok(())
    }
}

/// `keeler lend-init-reserve`: the admin co-signs the pool's first reserve.
pub fn init_reserve(chain: &LendChain, admin: &Keypair) -> Result<String> {
    ensure!(
        admin.pubkey() == keeler_lend::ADMIN,
        "the admin key is not the program's ADMIN"
    );
    let now = chain.now()?;
    let p = builders::init_reserve(
        &chain.kp,
        admin.pubkey(),
        chain.debt,
        now,
        u128::from(now),
        TREE_ID,
    )?;
    let ix = chain.prove(p)?;
    let sig = chain.rpc().create_and_send_transaction(
        std::slice::from_ref(&ix),
        chain.key.pubkey(),
        &[&chain.key, admin],
        budget(BUDGET, chain.chain.priority_fee)?,
    )?;
    Ok(sig.to_string())
}

/// `keeler lend-set-price`: the keeper writes the vault's test price.
pub fn set_test_price(
    chain: &LendChain,
    vault_id: u64,
    price: u64,
    confidence: u64,
) -> Result<String> {
    let now = i64::try_from(chain.now()?)?;
    let ix = vault::set_test_price(chain.key.pubkey(), vault_id, price, confidence, now)?;
    let sig = chain.rpc().create_and_send_transaction(
        std::slice::from_ref(&ix),
        chain.key.pubkey(),
        &[&chain.key],
        budget(200_000, chain.chain.priority_fee)?,
    )?;
    Ok(sig.to_string())
}

/// Whether the keeper's lend address is its plain shielded wallet (no
/// separate pool key: the swap dealer's wallet).
fn lend_address_is_wallet(chain: &LendChain) -> Result<bool> {
    Ok(shielded(&chain.key)?.viewing_pubkey() == chain.kp.viewing_pubkey())
}

/// `keeler lend-fund`: shields `amount` of the debt token from `from` (the
/// keeper key's token account) to the keeper's lend address, where its
/// liquidations pay from.
pub fn fund(chain: &LendChain, amount: u64, from: Address) -> Result<String> {
    use spr_interface::pda;
    use spr_program::instruction::{AssetDeposit, Deposit, DepositAsset, DepositSplAccounts};
    ensure!(amount > 0, "nothing to fund");
    let mint = TSOL_TUSD.debt_mint;
    ensure!(
        mint != spr_transaction::SOL_MINT,
        "the debt token is not an SPL token"
    );
    let to = chain.kp.shielded_address().map_err(err)?;
    let ix = Deposit {
        tree: chain.chain.tree,
        depositor: chain.key.pubkey(),
        deposits: vec![AssetDeposit {
            asset: DepositAsset::Spl(DepositSplAccounts {
                mint,
                user_token: from,
                token_program: pda::spl_token_program_id(),
            }),
            view_tag: to.viewing_pubkey.u(),
            owner: to.owner_hash().map_err(err)?,
            amount,
            memo: None,
        }],
    }
    .instruction()
    .map_err(err)?;
    let sig = chain.rpc().create_and_send_transaction(
        &[ix],
        chain.key.pubkey(),
        &[&chain.key],
        budget(200_000, chain.chain.priority_fee)?,
    )?;
    chain
        .chain
        .client
        .confirm_private_transaction_sync(sig)
        .map_err(err)?;
    Ok(sig.to_string())
}

/// The keeper key's associated token account of the debt token.
pub fn debt_token_account(chain: &LendChain) -> Address {
    spr_interface::pda::associated_token_address(&chain.key.pubkey(), &TSOL_TUSD.debt_mint)
}

/// `keeler lend-balance`: the debt token and the collateral held privately
/// at the keeper's lend address.
pub fn balances(chain: &LendChain) -> Result<(u64, u64)> {
    Ok((
        chain.chain.balance(&chain.kp, TSOL_TUSD.debt_mint)?,
        chain.chain.balance(&chain.kp, TSOL_TUSD.collateral_mint)?,
    ))
}

/// `keeler lend-sweep`: unshields the collateral liquidations paid to the
/// keeper's lend address to the keeper key's public account, a few notes per
/// transaction; the signatures.
pub fn sweep(chain: &LendChain) -> Result<Vec<String>> {
    use spr_client::{SignedPrivateTransaction, Submission};
    ensure!(
        !lend_address_is_wallet(chain)?,
        "the lend address is the keeper key's own wallet (no separate lend.pool_key): \
         its collateral is the swap dealer's inventory, not swept"
    );
    let mint = TSOL_TUSD.collateral_mint;
    let me = chain.key.pubkey();
    let token_program =
        (mint != spr_transaction::SOL_MINT).then(spr_interface::pda::spl_token_program_id);
    let mut sigs = Vec::new();
    // Claimed notes are hidden from the next scan, so each round takes new ones.
    for _ in 0..32 {
        let mut notes = chain.chain.notes(&chain.kp, mint)?;
        // Empty change notes cannot be withdrawn (ZeroInterfaceTransferAmount); they are left.
        notes.retain(|u| u.utxo.amount > 0);
        if notes.is_empty() {
            break;
        }
        notes.sort_by_key(|u| std::cmp::Reverse(u.utxo.amount));
        notes.truncate(MAX_MERGE);
        let total: u64 = notes.iter().map(|u| u.utxo.amount).sum();
        notes.iter().for_each(|n| chain.chain.claim(n));
        let mut tx = ConfidentialTransaction::new(notes, me).map_err(err)?;
        let accounts = tx
            .withdraw_to(mint, total, me, token_program)
            .map_err(err)?;
        let signed = SignedPrivateTransaction {
            transaction: tx.encrypt(&chain.kp).map_err(err)?,
            settlement_transfers: vec![accounts],
        };
        let sig = Submission::new(&signed, me, &chain.kp)
            .send_sync(&chain.chain.client, &[&chain.kp])
            .map_err(|e| anyhow!("sweep: {e:?}"))?;
        log!("lend: swept {total} of {mint} to {me}: {sig}");
        sigs.push(sig.to_string());
    }
    Ok(sigs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `getTransaction` reply: one lend `redeem` for `wallet`, paid by `payer`.
    fn redeem_tx(payer: &Address, wallet: &Address) -> Value {
        json!({
            "meta": { "err": null },
            "transaction": { "message": {
                "accountKeys": [payer.to_string(), wallet.to_string(), keeler_lend::ID.to_string()],
                "instructions": [{
                    "programIdIndex": 2,
                    "accounts": [0, 1],
                    "data": bs58::encode([tag::REDEEM]).into_string(),
                }],
            }},
        })
    }

    #[test]
    fn only_keeper_paid_transactions_carry_notes() {
        let keeper = Keypair::new().pubkey();
        let wallet = Keypair::new().pubkey();
        assert_eq!(
            lend_instruction_of(&redeem_tx(&keeper, &wallet), &keeper).unwrap(),
            Some((tag::REDEEM, wallet))
        );
        // Anyone may name the keeper in a transaction they pay for, with a
        // note proved under their own PDA: it is not the keeper's.
        let mut forged = redeem_tx(&wallet, &keeper);
        assert_eq!(lend_instruction_of(&forged, &keeper).unwrap(), None);
        forged["transaction"]["message"]["accountKeys"][1] = json!(keeper.to_string());
        assert_eq!(lend_instruction_of(&forged, &keeper).unwrap(), None);
        // A failed one is not either.
        let mut failed = redeem_tx(&keeper, &wallet);
        failed["meta"]["err"] = json!({ "InstructionError": [0, "Custom"] });
        assert_eq!(lend_instruction_of(&failed, &keeper).unwrap(), None);
        let mut garbled = redeem_tx(&keeper, &wallet);
        garbled["transaction"]["message"]["instructions"][0]["data"] =
            json!(bs58::encode([tag::SUPPLY, 1, 2]).into_string());
        assert!(lend_instruction_of(&garbled, &keeper).is_err());
    }

    #[test]
    fn a_keeper_transaction_must_show_every_note_it_made() {
        let (r, c, p, i) = (KIND_RESERVE, KIND_RECEIPT, KIND_POSITION, KIND_INTENT);
        assert!(missing_kinds(tag::INIT_RESERVE, &[r]).is_empty());
        assert!(missing_kinds(tag::SUPPLY, &[c, r]).is_empty());
        assert_eq!(missing_kinds(tag::REDEEM, &[r]), ["receipt"]);
        assert!(missing_kinds(tag::DEPOSIT, &[p, i]).is_empty());
        assert_eq!(missing_kinds(tag::DEPOSIT_ADD, &[i]), ["position"]);
        assert_eq!(missing_kinds(tag::DRAW, &[]), ["reserve", "position"]);
        assert_eq!(missing_kinds(tag::REPAY, &[p]), ["reserve"]);
        assert!(missing_kinds(tag::LIQUIDATE, &[p, r]).is_empty());
        assert!(missing_kinds(tag::CLOSE, &[]).is_empty());
    }

    /// The strict history scan over a real network, from no mark, for every
    /// keeper of the compiled set, read-only: no state file, no lock, nothing
    /// sent. `KEELER_KEEPER_CONFIG` names a keeper config; run with the
    /// deployment's build parameters in the environment, e.g.
    /// `KEELER_KEEPER_CONFIG=<config> cargo test -p keeler --lib -- --ignored strict_history`.
    #[test]
    #[ignore = "reads a real network's whole keeper history"]
    fn strict_history_scan_reads_every_keeper() {
        let path = std::env::var("KEELER_KEEPER_CONFIG").expect("KEELER_KEEPER_CONFIG");
        let cfg = crate::keeper::KeeperConfig::load(&path).unwrap();
        let p = crate::keeper::connect(&cfg.lend, &cfg.network, &cfg.key).unwrap();
        for k in keeler_lend::KEEPERS {
            let (notes, newest) = p.chain.history(k, None).unwrap();
            let count = |kind| notes.iter().filter(|n| n.kind == kind).count();
            println!(
                "keeper {k}: {} reserves, {} receipts, {} positions read; mark {newest:?}",
                count(KIND_RESERVE),
                count(KIND_RECEIPT),
                count(KIND_POSITION)
            );
        }
    }

    #[test]
    fn sends_that_can_never_land() {
        assert!(never_lands(
            "SolanaRpcTransaction { operation: \"process_transaction\", source: Error { request: \
             Some(SendTransaction), kind: RpcError(RpcResponseError { code: -32002, message: \"x\", \
             data: SendTransactionPreflightFailure(..) }) } }"
        ));
        assert!(never_lands(
            "Error { request: None, kind: TransactionError(InstructionError(0, Custom(1))) }"
        ));
        assert!(never_lands(
            "Rpc(\"transaction 1 was not confirmed before its blockhash expired\")"
        ));
        assert!(!never_lands(
            "Error { request: Some(SendTransaction), kind: Reqwest(timed out) }"
        ));
    }

    #[test]
    fn liquidation_funds_are_exactly_the_repay() {
        assert_eq!(plan_funds(&[5, 10, 20], 10).unwrap(), Funding::Exact(1));
        // The smallest larger note is split.
        assert_eq!(plan_funds(&[5, 12, 20], 10).unwrap(), Funding::Split(1));
        // Else the largest few, merged.
        assert_eq!(
            plan_funds(&[1, 2, 3, 4, 5], 12).unwrap(),
            Funding::Merge(vec![4, 3, 2, 1])
        );
        assert!(plan_funds(&[1, 2, 3, 4, 5], 15).is_err());
        assert!(plan_funds(&[], 1).is_err());
    }
}
