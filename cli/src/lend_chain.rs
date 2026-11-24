//! The keeper's [`Ledger`] on a real network, through the lend SDK's
//! builders, `spr-client` and plain JSON-RPC for the keeper's history.
use std::{str::FromStr, time::Duration};

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
use spr_client::{compile_message, user_registry, ComputeBudgetConfig, HistoryFilters, Rpc};
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
    chain::{shielded, Chain},
    config::Network,
    keeper::{
        Built, Intent, Job, Ledger, LendPrice, Note, Op, Pending, Quote, KIND_INTENT,
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
    price: LendPrice,
    debt: Mint,
}

impl LendChain {
    pub fn connect(network: &Network, key: &Keypair, price: LendPrice) -> Result<Self> {
        let chain = Chain::connect(network, &[TSOL_TUSD.debt_mint, TSOL_TUSD.collateral_mint])?;
        let debt = chain.mint(TSOL_TUSD.debt_mint)?;
        Ok(Self {
            kp: shielded(key)?,
            key: key.insecure_clone(),
            rpc_url: network.urls()?.0,
            chain,
            price,
            debt,
        })
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

    /// The smallest keeper note of the debt token worth `need`, merging
    /// notes with a self-transfer when none is.
    fn funds(&self, need: u64) -> Result<WalletUtxo> {
        let mut notes = self.chain.notes(&self.kp, TSOL_TUSD.debt_mint)?;
        notes.sort_by_key(|u| u.utxo.amount);
        if let Some(n) = notes.iter().find(|u| u.utxo.amount >= need) {
            return Ok(n.clone());
        }
        let top: Vec<WalletUtxo> = notes.into_iter().rev().take(MAX_MERGE).collect();
        let total: u64 = top.iter().map(|u| u.utxo.amount).sum();
        ensure!(
            total >= need,
            "the keeper holds too little of the debt token to liquidate"
        );
        log!("lend: merging {} keeper notes for a liquidation", top.len());
        let payer = self.kp.pubkey();
        let me = self.kp.shielded_address().map_err(err)?;
        let mut tx = ConfidentialTransaction::new(top, payer).map_err(err)?;
        tx.transfer(&me, TSOL_TUSD.debt_mint, total).map_err(err)?;
        let signed = spr_client::SignedPrivateTransaction {
            transaction: tx.encrypt(&self.kp).map_err(err)?,
            settlement_transfers: Vec::new(),
        };
        spr_client::Submission::new(&signed, payer, &self.kp)
            .send_sync(&self.chain.client, &[&self.kp])
            .map_err(|e| anyhow!("merge: {e:?}"))?;
        self.chain
            .notes(&self.kp, TSOL_TUSD.debt_mint)?
            .into_iter()
            .find(|u| u.utxo.amount >= need)
            .context("the merged note was not found")
    }

    /// Proves `p`, compiles it with the keeper paying and signs as the
    /// keeper; other signers' slots stay empty for them to fill.
    fn finish(&self, p: Prepared) -> Result<Vec<u8>> {
        let ix = p.prove(self.chain.client.indexer(), self.chain.tree, &self.kp)?;
        let (blockhash, _) = self.rpc().get_latest_blockhash()?;
        let message = compile_message(
            &self.key.pubkey(),
            std::slice::from_ref(&ix),
            blockhash,
            ComputeBudgetConfig::new(BUDGET),
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
        Ok(bincode::serialize(&tx)?)
    }

    /// JSON-RPC that `spr-client` does not wrap.
    fn rpc_call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let v: Value = reqwest::blocking::Client::new()
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
    /// acts for.
    fn lend_instruction(&self, sig: &str) -> Result<Option<(u8, Address)>> {
        let tx = self.rpc_call(
            "getTransaction",
            json!([sig, { "encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "confirmed" }]),
        )?;
        if tx.is_null() || !tx["meta"]["err"].is_null() {
            return Ok(None);
        }
        let msg = &tx["transaction"]["message"];
        let keys: Vec<Address> = msg["accountKeys"]
            .as_array()
            .context("no account keys")?
            .iter()
            .filter_map(|k| Address::from_str(k.as_str()?).ok())
            .collect();
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
            let wallet = match t {
                tag::INIT_RESERVE => Some(Address::default()),
                tag::SUPPLY => wincode::deserialize_from::<SupplyArgs>(&mut rest)
                    .ok()
                    .map(|a| Address::new_from_array(a.maker)),
                tag::DEPOSIT | tag::DEPOSIT_ADD => {
                    wincode::deserialize_from::<DepositArgs>(&mut rest)
                        .ok()
                        .map(|a| Address::new_from_array(a.owner))
                }
                tag::REPAY => wincode::deserialize_from::<RepayArgs>(&mut rest)
                    .ok()
                    .map(|a| Address::new_from_array(a.owner)),
                tag::LIQUIDATE => wincode::deserialize_from::<LiquidateArgs>(&mut rest)
                    .ok()
                    .map(|a| Address::new_from_array(a.owner)),
                tag::REDEEM | tag::DRAW => account(1),
                _ => None,
            };
            if let Some(w) = wallet {
                return Ok(Some((t, w)));
            }
        }
        Ok(None)
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

    fn intents(&self, wallet: &Address) -> Result<Vec<Intent>> {
        let pda = user_pda(wallet).owner;
        let tag = pda
            .address(self.kp.viewing_pubkey())
            .map_err(err)?
            .signing_pubkey
            .confidential_view_tag()
            .map_err(err)?;
        let mut found = Vec::new();
        let mut cursor = None;
        loop {
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
        let nullifiers: Vec<[u8; 32]> = found
            .iter()
            .map(|i| i.note.nullifier())
            .collect::<Result<_>>()?;
        if nullifiers.is_empty() {
            return Ok(found);
        }
        let spent = self.spent(&nullifiers)?;
        Ok(found
            .into_iter()
            .zip(spent)
            .filter(|(_, s)| !s)
            .map(|(i, _)| i)
            .collect())
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
                Ok(Quote {
                    low: price.saturating_sub(conf),
                    high: price.saturating_add(conf),
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
                ensure!(price > 0, "no price");
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

    #[allow(clippy::type_complexity)]
    fn build(&self, job: &Job, reserve: Option<&Note>, now: u64) -> Result<Built> {
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
                let owed = keeler_core::lend::debt_of(
                    s.debt_shares,
                    vault::accrued(&r.state()?, now)?.borrow_price,
                )
                .map_err(|e| anyhow!("{e}"))?;
                let funds = self.funds(owed)?;
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
        let tx = self.finish(p)?;
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

    fn send(&self, tx: &[u8]) -> Result<String> {
        let tx: VersionedTransaction = bincode::deserialize(tx)?;
        Ok(self.rpc().process_transaction(tx)?.to_string())
    }

    fn history(&self) -> Result<Vec<Note>> {
        let mut sigs = Vec::new();
        let mut before: Option<String> = None;
        loop {
            let mut opts = json!({ "limit": 1000, "commitment": "confirmed" });
            if let Some(b) = &before {
                opts["before"] = json!(b);
            }
            let page = self.rpc_call(
                "getSignaturesForAddress",
                json!([self.key.pubkey().to_string(), opts]),
            )?;
            let page = page.as_array().context("no signatures")?.clone();
            for s in &page {
                if s["err"].is_null() {
                    if let Some(sig) = s["signature"].as_str() {
                        sigs.push(sig.to_owned());
                    }
                }
            }
            match page.last().and_then(|s| s["signature"].as_str()) {
                Some(last) if page.len() == 1000 => before = Some(last.to_owned()),
                _ => break,
            }
        }
        let mut found = Vec::new();
        for sig in sigs {
            let Some((t, wallet)) = self.lend_instruction(&sig)? else {
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
                continue;
            };
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
                if let Some(n) = note.filter(|n| n.kind != KIND_INTENT) {
                    found.push(n);
                }
            }
        }
        Ok(found)
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
    let ix = p.prove(chain.chain.client.indexer(), chain.chain.tree, &chain.kp)?;
    let sig = chain.rpc().create_and_send_transaction(
        std::slice::from_ref(&ix),
        chain.key.pubkey(),
        &[&chain.key, admin],
        ComputeBudgetConfig::new(BUDGET),
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
        ComputeBudgetConfig::new(200_000),
    )?;
    Ok(sig.to_string())
}
