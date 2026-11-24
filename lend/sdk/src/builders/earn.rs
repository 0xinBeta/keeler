//! Earn: `supply` and `redeem`, both built and proved by the keeper. Every
//! output's blinding derives from the user's note (intent or receipt) and the
//! first nullifier, which is that note's, so the owner can recompute them.
use anyhow::{ensure, Result};
use solana_instruction::AccountMeta;
use spr_keypair::{ShieldedAddress, ShieldedKeypair};
use spr_program::{poseidon, Field, TransactionOptions};
use spr_transaction::{
    serialization::confidential::{Confidential, ConfidentialOutputPlaintext},
    EncryptedScheme, WalletUtxo,
};

use keeler_lend::circuits::{
    IntentStateClient, ReceiptStateClient, RedeemClient, RedeemPrivateInputsClient,
    RedeemPublicInputsClient, ReserveStateClient, SupplyClient, SupplyPrivateInputsClient,
    SupplyPublicInputsClient, REDEEM_BLINDING_DOMAIN, SUPPLY_BLINDING_DOMAIN,
};

use super::{wallet_address, Args, Prepared};
use crate::{
    decode, err, identity, mint_halves, pool_pda, proof::CircuitId, tag, user_pda, IntentState,
    ReceiptState, RedeemArgs, ReserveState, SupplyArgs,
};

/// `Poseidon(domain, note blinding)`: the blinding seed the circuit requires.
pub fn earn_blinding_seed(domain: u32, note: &WalletUtxo) -> Result<[u8; 32]> {
    let blinding = spr_program::from_bytes(&note.utxo.blinding).map_err(err)?;
    let seed = poseidon(&[Field::from(domain), blinding]).map_err(err)?;
    Ok(spr_program::to_bytes(&seed))
}

/// The opening of output `slot` of a transaction `builder` built, read with
/// its writer key (the transaction viewing key of `first_nullifier`):
/// `payload` is the slot's published data (an indexer `OutputSlot::payload`
/// or a transact output's `data`), `salt` the transaction's. This is how the
/// keeper recovers a receipt or payout it encrypted to someone else.
pub fn recover_output(
    builder: &ShieldedKeypair,
    first_nullifier: &[u8; 32],
    payload: &[u8],
    salt: [u8; 16],
    slot: u32,
) -> Result<ConfidentialOutputPlaintext> {
    // Borsh `OutputDataEncoding::Encrypted(blob)`: variant 1, a u32 length,
    // then the blob, whose first byte is the scheme.
    let (&scheme, body) = payload
        .strip_prefix(&[1])
        .and_then(|rest| rest.get(4..)?.split_first())
        .ok_or_else(|| err("not an encrypted output"))?;
    ensure!(
        scheme == EncryptedScheme::Confidential.as_byte(),
        "not a confidential output"
    );
    let key = builder
        .get_transaction_viewing_key(first_nullifier)
        .map_err(err)?;
    Confidential::decrypt_with_tx_key(&key, body, salt, slot).map_err(err)
}

fn reserve_client(state: ReserveState) -> ReserveStateClient {
    ReserveStateClient {
        supply_shares: state.supply_shares,
        debt_shares: state.debt_shares,
        supply_price: state.supply_price,
        borrow_price: state.borrow_price,
        rate_bps: state.rate_bps,
        last_update: state.last_update,
        salt: state.salt,
    }
}

fn context(seed: [u8; 32], tree_id: u16) -> TransactionOptions {
    TransactionOptions {
        blinding_seed: Some(seed),
        output_tree_id: Some(tree_id),
    }
}

/// Fills `intent` (a supply intent, owned by the maker's user PDA) into
/// `reserve` (the pool's, owned by the pool PDA and encrypted to the keeper)
/// at `now`, under the keeper's fresh `salt`. `owner` is the maker's shielded
/// address: the receipt is encrypted to its viewing key. The keeper pays.
pub fn supply(
    keeper: &ShieldedKeypair,
    owner: &ShieldedAddress,
    reserve: WalletUtxo,
    intent: WalletUtxo,
    now: u64,
    salt: u128,
    tree_id: u16,
) -> Result<Prepared> {
    let keeper_key = wallet_address(keeper)?;
    let mint = reserve.utxo.asset;
    let pool = pool_pda(&mint.asset);
    let maker = owner.solana_address().map_err(err)?;
    let user = user_pda(&maker);
    let pool_viewing = keeper.viewing_pubkey();
    let (mint_hi, mint_lo) = mint_halves(&mint.asset);
    let state: IntentState = decode(&intent)?;
    let expiry = state.expiry;
    let first_nullifier = intent.nullifier;
    let seed = earn_blinding_seed(SUPPLY_BLINDING_DOMAIN, &intent)?;
    let transaction = SupplyClient {
        private: SupplyPrivateInputsClient {
            tx_context: context(seed, tree_id),
            intent_utxo: intent,
            intent_state: IntentStateClient {
                maker: state.maker,
                purpose: state.purpose,
                target: state.target,
                expiry,
            },
            reserve_state: reserve_client(decode(&reserve)?),
            reserve_utxo: reserve,
            salt,
        },
        public: SupplyPublicInputsClient {
            pool_owner: pool.owner.address(pool_viewing).map_err(err)?,
            user_owner: user.owner.address(owner.viewing_pubkey).map_err(err)?,
            mint_hi,
            mint_lo,
            now,
            expiry,
            first_nullifier: spr_program::from_bytes(&first_nullifier).map_err(err)?,
        },
    }
    .prepare(&keeper.shielded_address().map_err(err)?, keeper_key)
    .map_err(err)?;
    let (mint_bytes, pool_bump, user_bump) = (mint.asset.to_bytes(), pool.bump, user.bump);
    let owner_viewing = *owner.viewing_pubkey.as_bytes();
    let args: Args = Box::new(move |proof| {
        wincode::serialize(&SupplyArgs {
            proof,
            mint: mint_bytes,
            pool_viewing: *pool_viewing.as_bytes(),
            pool_bump,
            maker: maker.to_bytes(),
            owner_viewing,
            user_bump,
            now,
            expiry,
        })
        .map_err(err)
    });
    let prepared = Prepared::new(
        transaction,
        keeper,
        CircuitId::Supply,
        tag::SUPPLY,
        args,
        vec![AccountMeta::new(keeper_key, true)],
        vec![user.address(), pool.address()],
    )?;
    ensure!(
        prepared.first_nullifier == first_nullifier,
        "the intent must be the first input"
    );
    Ok(prepared)
}

/// Pays `amount` of `reserve` against `receipt` to the receipt's owner,
/// `owner` (their shielded address: the payout and the new receipt are
/// encrypted to its viewing key), at `now` under the keeper's fresh `salt`.
/// The keeper pays and signs first; the owner co-signs as a read-only signer.
#[allow(clippy::too_many_arguments)]
pub fn redeem(
    keeper: &ShieldedKeypair,
    owner: &ShieldedAddress,
    reserve: WalletUtxo,
    receipt: WalletUtxo,
    amount: u64,
    now: u64,
    salt: u128,
    tree_id: u16,
) -> Result<Prepared> {
    let keeper_key = wallet_address(keeper)?;
    let mint = reserve.utxo.asset;
    let pool = pool_pda(&mint.asset);
    let owner_key = owner.solana_address().map_err(err)?;
    let user = user_pda(&owner_key);
    let pool_viewing = keeper.viewing_pubkey();
    let (mint_hi, mint_lo) = mint_halves(&mint.asset);
    let state: ReceiptState = decode(&receipt)?;
    let first_nullifier = receipt.nullifier;
    let seed = earn_blinding_seed(REDEEM_BLINDING_DOMAIN, &receipt)?;
    let transaction = RedeemClient {
        private: RedeemPrivateInputsClient {
            tx_context: context(seed, tree_id),
            receipt_utxo: receipt,
            receipt_state: ReceiptStateClient {
                owner: state.owner,
                pool: state.pool,
                supply_shares: state.supply_shares,
            },
            reserve_state: reserve_client(decode(&reserve)?),
            reserve_utxo: reserve,
            amount,
            salt,
            owner_address: *owner,
        },
        public: RedeemPublicInputsClient {
            pool_owner: pool.owner.address(pool_viewing).map_err(err)?,
            user_owner: user.owner.address(owner.viewing_pubkey).map_err(err)?,
            owner_identity: identity(&owner_key)?,
            mint_hi,
            mint_lo,
            now,
            first_nullifier: spr_program::from_bytes(&first_nullifier).map_err(err)?,
        },
    }
    .prepare(&keeper.shielded_address().map_err(err)?, keeper_key)
    .map_err(err)?;
    let (mint_bytes, pool_bump, user_bump) = (mint.asset.to_bytes(), pool.bump, user.bump);
    let owner_viewing = *owner.viewing_pubkey.as_bytes();
    let args: Args = Box::new(move |proof| {
        wincode::serialize(&RedeemArgs {
            proof,
            mint: mint_bytes,
            pool_viewing: *pool_viewing.as_bytes(),
            pool_bump,
            owner_viewing,
            user_bump,
            now,
        })
        .map_err(err)
    });
    let prepared = Prepared::new(
        transaction,
        keeper,
        CircuitId::Redeem,
        tag::REDEEM,
        args,
        vec![
            AccountMeta::new(keeper_key, true),
            AccountMeta::new_readonly(owner_key, true),
        ],
        vec![user.address(), pool.address()],
    )?;
    ensure!(
        prepared.first_nullifier == first_nullifier,
        "the receipt must be the first input"
    );
    Ok(prepared)
}

/// The payout that burns exactly `shares` at `now`: `assets_of(shares,
/// supply price accrued to now)`. The circuit burns `ceil(amount x 1e12 /
/// supply_price)`, which gives back exactly `shares` because the supply
/// price never falls below 1e12; this checks it.
pub fn redeem_amount_for_shares(reserve: &ReserveState, shares: u64, now: u64) -> Result<u64> {
    let price = crate::vault::accrued(reserve, now)?.supply_price;
    let amount = keeler_core::lend::assets_of(shares, price).map_err(err)?;
    ensure!(amount > 0, "those shares are worth nothing");
    ensure!(
        keeler_core::lend::shares_to_burn(amount, price).map_err(err)? == shares,
        "these shares cannot be redeemed exactly"
    );
    Ok(amount)
}

/// [`redeem`] burning exactly `shares` of `receipt` (the page's "withdraw
/// everything" leaves a receipt of zero shares).
#[allow(clippy::too_many_arguments)]
pub fn redeem_shares(
    keeper: &ShieldedKeypair,
    owner: &ShieldedAddress,
    reserve: WalletUtxo,
    receipt: WalletUtxo,
    shares: u64,
    now: u64,
    salt: u128,
    tree_id: u16,
) -> Result<Prepared> {
    let amount = redeem_amount_for_shares(&decode(&reserve)?, shares, now)?;
    redeem(keeper, owner, reserve, receipt, amount, now, salt, tree_id)
}
