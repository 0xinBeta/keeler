//! The Vault builders: `deposit_open`, `deposit_add`, `draw`, `repay`,
//! `liquidate` (keeper-built, the keeper pays), `close` (the owner alone)
//! and `set_test_price`. Like [`crate::builders`], each runs the circuit
//! natively, so a request the circuit refuses fails here, and returns a
//! [`Prepared`] whose outputs (slot constants in `keeler_lend::circuits`)
//! and first nullifier a caller checks before proving.
//!
//! Outputs are encrypted to their owner's viewing key: the position to the
//! owner (through `owner`'s viewing key on the user PDA), payouts and change
//! to the wallet that receives them, the reserve to the keeper. The keeper,
//! as the transaction's sender, can read every one with its transaction
//! viewing key ([`read_outputs`]).
use anyhow::{ensure, Context, Result};
use keeler_core::lend as core;
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use spr_keypair::{ShieldedAddress, ShieldedKeypair};
use spr_program::{Field, TransactionOptions};
use spr_transaction::{
    instructions::transact::SppProofInputs,
    serialization::confidential::{Confidential, ConfidentialOutputPlaintext},
    EncryptedScheme, WalletUtxo,
};

use keeler_lend::{
    circuits::{
        vault::{DEPOSIT_BLINDING_DOMAIN, POSITION_BLINDING_DOMAIN},
        CloseClient, ClosePrivateInputsClient, ClosePublicInputsClient, DepositAddClient,
        DepositAddPrivateInputsClient, DepositOpenClient, DepositOpenPrivateInputsClient,
        DepositOpenPublicInputsClient, DrawClient, DrawPrivateInputsClient, DrawPublicInputsClient,
        IntentStateClient, LiquidateClient, LiquidatePrivateInputsClient,
        LiquidatePublicInputsClient, PositionStateClient, RepayClient, RepayPrivateInputsClient,
        RepayPublicInputsClient, ReserveStateClient,
    },
    instructions::{
        close::CloseArgs,
        deposit::DepositArgs,
        draw::DrawArgs,
        liquidate::LiquidateArgs,
        repay::RepayArgs,
        vault::{test_price_address, SetTestPriceArgs},
    },
    vaults::{self, Vault},
};

use crate::{
    builders::{Args, Prepared, OWNER_TX_SECS},
    decode, err, identity, mint_halves, pool_pda,
    proof::CircuitId,
    tag, user_pda, IntentState, PositionState, ReserveState,
};

/// The vault with `id`.
pub fn vault(id: u64) -> Result<&'static Vault> {
    vaults::by_id(id).with_context(|| format!("no vault {id}"))
}

/// `Poseidon(domain, blinding)` as the bytes `TransactionOptions` takes.
pub fn blinding_seed(domain: u32, blinding: &[u8; 32]) -> Result<[u8; 32]> {
    let seed = spr_program::poseidon(&[
        Field::from(domain),
        spr_program::from_bytes(blinding).map_err(err)?,
    ])
    .map_err(err)?;
    Ok(spr_program::to_bytes(&seed))
}

fn options(domain: u32, first: &WalletUtxo, tree_id: u16) -> Result<TransactionOptions> {
    Ok(TransactionOptions {
        blinding_seed: Some(blinding_seed(domain, &first.utxo.blinding)?),
        output_tree_id: Some(tree_id),
    })
}

fn nullifier(first: &WalletUtxo) -> Result<Field> {
    spr_program::from_bytes(&first.nullifier).map_err(err)
}

fn wallet(address: &ShieldedAddress) -> Result<Address> {
    address.solana_address().map_err(err)
}

fn position_client(s: &PositionState) -> PositionStateClient {
    PositionStateClient {
        owner: s.owner,
        vault: s.vault,
        position_id: s.position_id,
        debt_shares: s.debt_shares,
    }
}

fn intent_client(s: &IntentState) -> IntentStateClient {
    IntentStateClient {
        maker: s.maker,
        purpose: s.purpose,
        target: s.target,
        expiry: s.expiry,
    }
}

fn reserve_client(s: &ReserveState) -> ReserveStateClient {
    ReserveStateClient {
        supply_shares: s.supply_shares,
        debt_shares: s.debt_shares,
        supply_price: s.supply_price,
        borrow_price: s.borrow_price,
        rate_bps: s.rate_bps,
        last_update: s.last_update,
        salt: s.salt,
    }
}

/// Opens a position from a deposit intent of `owner` (the maker's wallet
/// address; its viewing key is the position's). The keeper pays.
pub fn deposit_open(
    keeper: &ShieldedKeypair,
    owner: &ShieldedAddress,
    intent: WalletUtxo,
    vault_id: u64,
    now: u64,
    tree_id: u16,
) -> Result<Prepared> {
    let v = vault(vault_id)?;
    let (coll_hi, coll_lo) = mint_halves(&v.collateral_mint);
    let owner_key = wallet(owner)?;
    let pda = user_pda(&owner_key);
    let state: IntentState = decode(&intent)?;
    let transaction = DepositOpenClient {
        private: DepositOpenPrivateInputsClient {
            tx_context: options(DEPOSIT_BLINDING_DOMAIN, &intent, tree_id)?,
            intent_utxo: intent.clone(),
            intent_state: intent_client(&state),
        },
        public: DepositOpenPublicInputsClient {
            user_owner: pda.owner.address(owner.viewing_pubkey).map_err(err)?,
            coll_hi,
            coll_lo,
            vault: vault_id,
            now,
            expiry: state.expiry,
            first_nullifier: nullifier(&intent)?,
        },
    }
    .prepare(
        &keeper.shielded_address().map_err(err)?,
        keeper_key(keeper)?,
    )
    .map_err(err)?;
    deposit_prepared(
        keeper,
        transaction,
        CircuitId::DepositOpen,
        tag::DEPOSIT,
        owner,
        pda,
        vault_id,
        now,
        state.expiry,
    )
}

/// Adds a deposit intent to `owner`'s position `position` (the intent must
/// name it). The keeper pays.
pub fn deposit_add(
    keeper: &ShieldedKeypair,
    owner: &ShieldedAddress,
    position: WalletUtxo,
    intent: WalletUtxo,
    vault_id: u64,
    now: u64,
    tree_id: u16,
) -> Result<Prepared> {
    let v = vault(vault_id)?;
    let (coll_hi, coll_lo) = mint_halves(&v.collateral_mint);
    let owner_key = wallet(owner)?;
    let pda = user_pda(&owner_key);
    let state: IntentState = decode(&intent)?;
    let position_state: PositionState = decode(&position)?;
    let transaction = DepositAddClient {
        private: DepositAddPrivateInputsClient {
            tx_context: options(POSITION_BLINDING_DOMAIN, &position, tree_id)?,
            position_utxo: position.clone(),
            position_state: position_client(&position_state),
            intent_utxo: intent,
            intent_state: intent_client(&state),
        },
        public: DepositOpenPublicInputsClient {
            user_owner: pda.owner.address(owner.viewing_pubkey).map_err(err)?,
            coll_hi,
            coll_lo,
            vault: vault_id,
            now,
            expiry: state.expiry,
            first_nullifier: nullifier(&position)?,
        },
    }
    .prepare(
        &keeper.shielded_address().map_err(err)?,
        keeper_key(keeper)?,
    )
    .map_err(err)?;
    deposit_prepared(
        keeper,
        transaction,
        CircuitId::DepositAdd,
        tag::DEPOSIT_ADD,
        owner,
        pda,
        vault_id,
        now,
        state.expiry,
    )
}

fn keeper_key(keeper: &ShieldedKeypair) -> Result<Address> {
    wallet(&keeper.shielded_address().map_err(err)?)
}

#[allow(clippy::too_many_arguments)]
fn deposit_prepared(
    keeper: &ShieldedKeypair,
    transaction: spr_program::ProgramTransaction,
    circuit: CircuitId,
    ix_tag: u8,
    owner: &ShieldedAddress,
    pda: crate::Pda,
    vault_id: u64,
    now: u64,
    expiry: u64,
) -> Result<Prepared> {
    let owner_bytes = wallet(owner)?.to_bytes();
    let owner_viewing = *owner.viewing_pubkey.as_bytes();
    let bump = pda.bump;
    let args: Args = Box::new(move |proof| {
        wincode::serialize(&DepositArgs {
            proof,
            vault: vault_id,
            owner: owner_bytes,
            owner_viewing,
            user_bump: bump,
            now,
            expiry,
        })
        .map_err(err)
    });
    let head = vec![AccountMeta::new(keeper_key(keeper)?, true)];
    Prepared::new(
        transaction,
        keeper,
        circuit,
        ix_tag,
        args,
        head,
        vec![pda.address()],
    )
}

/// Borrows `borrow` and withdraws `withdraw` (either may be zero) from
/// `owner`'s position. The keeper pays and signs; `owner`'s wallet co-signs
/// as a read-only signer (not the fee payer). `price` is the oracle account
/// the program reads; `bound` must be at most its `price - confidence`.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    keeper: &ShieldedKeypair,
    owner: &ShieldedAddress,
    reserve: WalletUtxo,
    position: WalletUtxo,
    borrow: u64,
    withdraw: u64,
    price: Address,
    vault_id: u64,
    now: u64,
    bound: u64,
    salt: u128,
    tree_id: u16,
) -> Result<Prepared> {
    let v = vault(vault_id)?;
    let (debt_hi, debt_lo) = mint_halves(&v.debt_mint);
    let (coll_hi, coll_lo) = mint_halves(&v.collateral_mint);
    let owner_key = wallet(owner)?;
    let user = user_pda(&owner_key);
    let pool = pool_pda(&v.debt_mint);
    let pool_viewing = keeper.viewing_pubkey();
    let position_state: PositionState = decode(&position)?;
    let reserve_state: ReserveState = decode(&reserve)?;
    let debt_mint = reserve.utxo.asset;
    let mut transaction = DrawClient {
        private: DrawPrivateInputsClient {
            tx_context: options(POSITION_BLINDING_DOMAIN, &position, tree_id)?,
            position_utxo: position.clone(),
            position_state: position_client(&position_state),
            reserve_utxo: reserve,
            reserve_state: reserve_client(&reserve_state),
            debt_mint,
            owner_address: *owner,
            borrow,
            withdraw,
            salt,
        },
        public: DrawPublicInputsClient {
            pool_owner: pool.owner.address(pool_viewing).map_err(err)?,
            user_owner: user.owner.address(owner.viewing_pubkey).map_err(err)?,
            owner_identity: identity(&owner_key)?,
            debt_hi,
            debt_lo,
            coll_hi,
            coll_lo,
            vault: vault_id,
            now,
            bound,
            first_nullifier: nullifier(&position)?,
        },
    }
    .prepare(
        &keeper.shielded_address().map_err(err)?,
        keeper_key(keeper)?,
    )
    .map_err(err)?;
    transaction.finalized = transaction
        .finalized
        .with_expiry_unix_ts(now + OWNER_TX_SECS);
    let (pool_bump, user_bump) = (pool.bump, user.bump);
    let owner_viewing = *owner.viewing_pubkey.as_bytes();
    let args: Args = Box::new(move |proof| {
        wincode::serialize(&DrawArgs {
            proof,
            vault: vault_id,
            pool_viewing: *pool_viewing.as_bytes(),
            pool_bump,
            owner_viewing,
            user_bump,
            now,
            bound,
        })
        .map_err(err)
    });
    let head = vec![
        AccountMeta::new(keeper_key(keeper)?, true),
        AccountMeta::new_readonly(owner_key, true),
        AccountMeta::new_readonly(price, false),
    ];
    Prepared::new(
        transaction,
        keeper,
        CircuitId::Draw,
        tag::DRAW,
        args,
        head,
        vec![user.address(), pool.address()],
    )
}

/// Fills `owner`'s repay intent into the position it names. The keeper pays.
#[allow(clippy::too_many_arguments)]
pub fn repay(
    keeper: &ShieldedKeypair,
    owner: &ShieldedAddress,
    reserve: WalletUtxo,
    position: WalletUtxo,
    intent: WalletUtxo,
    vault_id: u64,
    now: u64,
    salt: u128,
    tree_id: u16,
) -> Result<Prepared> {
    let v = vault(vault_id)?;
    let (debt_hi, debt_lo) = mint_halves(&v.debt_mint);
    let (coll_hi, coll_lo) = mint_halves(&v.collateral_mint);
    let owner_key = wallet(owner)?;
    let user = user_pda(&owner_key);
    let pool = pool_pda(&v.debt_mint);
    let pool_viewing = keeper.viewing_pubkey();
    let position_state: PositionState = decode(&position)?;
    let reserve_state: ReserveState = decode(&reserve)?;
    let intent_state: IntentState = decode(&intent)?;
    let expiry = intent_state.expiry;
    let transaction = RepayClient {
        private: RepayPrivateInputsClient {
            tx_context: options(POSITION_BLINDING_DOMAIN, &position, tree_id)?,
            position_utxo: position.clone(),
            position_state: position_client(&position_state),
            reserve_utxo: reserve,
            reserve_state: reserve_client(&reserve_state),
            intent_utxo: intent,
            intent_state: intent_client(&intent_state),
            maker_address: *owner,
            salt,
        },
        public: RepayPublicInputsClient {
            pool_owner: pool.owner.address(pool_viewing).map_err(err)?,
            user_owner: user.owner.address(owner.viewing_pubkey).map_err(err)?,
            debt_hi,
            debt_lo,
            coll_hi,
            coll_lo,
            vault: vault_id,
            now,
            expiry,
            first_nullifier: nullifier(&position)?,
        },
    }
    .prepare(
        &keeper.shielded_address().map_err(err)?,
        keeper_key(keeper)?,
    )
    .map_err(err)?;
    let (pool_bump, user_bump) = (pool.bump, user.bump);
    let owner_bytes = owner_key.to_bytes();
    let owner_viewing = *owner.viewing_pubkey.as_bytes();
    let args: Args = Box::new(move |proof| {
        wincode::serialize(&RepayArgs {
            proof,
            vault: vault_id,
            pool_viewing: *pool_viewing.as_bytes(),
            pool_bump,
            owner: owner_bytes,
            owner_viewing,
            user_bump,
            now,
            expiry,
        })
        .map_err(err)
    });
    let head = vec![AccountMeta::new(keeper_key(keeper)?, true)];
    Prepared::new(
        transaction,
        keeper,
        CircuitId::Repay,
        tag::REPAY,
        args,
        head,
        vec![user.address(), pool.address()],
    )
}

/// The reserve accrued to `now` with the reference maths.
pub fn accrued(reserve: &ReserveState, now: u64) -> Result<core::Reserve> {
    let r = core::Reserve {
        supply_shares: reserve.supply_shares,
        debt_shares: reserve.debt_shares,
        supply_price: reserve.supply_price,
        borrow_price: reserve.borrow_price,
        rate_bps: u32::try_from(reserve.rate_bps)?,
        last_update: reserve.last_update,
    };
    core::accrue(&r, now, core::TUSD_POOL.fee_bps).map_err(err)
}

/// Liquidates `owner`'s position at `bound` (at least the oracle's `price +
/// confidence`) by the largest amount the rules allow, computed natively
/// with `core::lend::liquidate`, from the keeper's own debt-token note
/// `funds`. The keeper pays and receives the change and the collateral.
#[allow(clippy::too_many_arguments)]
pub fn liquidate(
    keeper: &ShieldedKeypair,
    owner: &ShieldedAddress,
    reserve: WalletUtxo,
    position: WalletUtxo,
    funds: WalletUtxo,
    price: Address,
    vault_id: u64,
    now: u64,
    bound: u64,
    salt: u128,
    tree_id: u16,
) -> Result<(Prepared, core::Liquidation)> {
    let position_state: PositionState = decode(&position)?;
    let reserve_state: ReserveState = decode(&reserve)?;
    let r = accrued(&reserve_state, now)?;
    let plan = core::liquidate(
        position_state.debt_shares,
        r.borrow_price,
        position.utxo.amount,
        bound,
        &core::TSOL_TUSD_VAULT,
    )
    .map_err(err)?;
    let prepared = liquidate_with(
        keeper, owner, reserve, position, funds, plan.repay, price, vault_id, now, bound, salt,
        tree_id,
    )?;
    Ok((prepared, plan))
}

/// [`liquidate`] repaying exactly `repay`, for tests of the circuit's rules.
#[allow(clippy::too_many_arguments)]
pub fn liquidate_with(
    keeper: &ShieldedKeypair,
    owner: &ShieldedAddress,
    reserve: WalletUtxo,
    position: WalletUtxo,
    funds: WalletUtxo,
    repay: u64,
    price: Address,
    vault_id: u64,
    now: u64,
    bound: u64,
    salt: u128,
    tree_id: u16,
) -> Result<Prepared> {
    let v = vault(vault_id)?;
    let (debt_hi, debt_lo) = mint_halves(&v.debt_mint);
    let (coll_hi, coll_lo) = mint_halves(&v.collateral_mint);
    let owner_key = wallet(owner)?;
    let user = user_pda(&owner_key);
    let pool = pool_pda(&v.debt_mint);
    let pool_viewing = keeper.viewing_pubkey();
    let position_state: PositionState = decode(&position)?;
    let reserve_state: ReserveState = decode(&reserve)?;
    let transaction = LiquidateClient {
        private: LiquidatePrivateInputsClient {
            tx_context: options(POSITION_BLINDING_DOMAIN, &position, tree_id)?,
            position_utxo: position.clone(),
            position_state: position_client(&position_state),
            reserve_utxo: reserve,
            reserve_state: reserve_client(&reserve_state),
            funds,
            repay,
            salt,
        },
        public: LiquidatePublicInputsClient {
            pool_owner: pool.owner.address(pool_viewing).map_err(err)?,
            user_owner: user.owner.address(owner.viewing_pubkey).map_err(err)?,
            debt_hi,
            debt_lo,
            coll_hi,
            coll_lo,
            vault: vault_id,
            now,
            bound,
            first_nullifier: nullifier(&position)?,
        },
    }
    .prepare(
        &keeper.shielded_address().map_err(err)?,
        keeper_key(keeper)?,
    )
    .map_err(err)?;
    let (pool_bump, user_bump) = (pool.bump, user.bump);
    let owner_bytes = owner_key.to_bytes();
    let owner_viewing = *owner.viewing_pubkey.as_bytes();
    let args: Args = Box::new(move |proof| {
        wincode::serialize(&LiquidateArgs {
            proof,
            vault: vault_id,
            pool_viewing: *pool_viewing.as_bytes(),
            pool_bump,
            owner: owner_bytes,
            owner_viewing,
            user_bump,
            now,
            bound,
        })
        .map_err(err)
    });
    let head = vec![
        AccountMeta::new(keeper_key(keeper)?, true),
        AccountMeta::new_readonly(price, false),
    ];
    Prepared::new(
        transaction,
        keeper,
        CircuitId::Liquidate,
        tag::LIQUIDATE,
        args,
        head,
        vec![user.address(), pool.address()],
    )
}

/// Closes `user`'s position with no debt, paying all of its collateral to
/// `user`, who builds, proves and pays: no keeper.
pub fn close(
    user: &ShieldedKeypair,
    position: WalletUtxo,
    vault_id: u64,
    tree_id: u16,
) -> Result<Prepared> {
    let v = vault(vault_id)?;
    let (coll_hi, coll_lo) = mint_halves(&v.collateral_mint);
    let address = user.shielded_address().map_err(err)?;
    let user_key = wallet(&address)?;
    let pda = user_pda(&user_key);
    let state: PositionState = decode(&position)?;
    let viewing = user.viewing_pubkey();
    let transaction = CloseClient {
        private: ClosePrivateInputsClient {
            tx_context: TransactionOptions {
                blinding_seed: None,
                output_tree_id: Some(tree_id),
            },
            position_utxo: position,
            position_state: position_client(&state),
        },
        public: ClosePublicInputsClient {
            user_owner: pda.owner.address(viewing).map_err(err)?,
            owner_identity: identity(&user_key)?,
            coll_hi,
            coll_lo,
            vault: vault_id,
        },
    }
    .prepare(&address, user_key)
    .map_err(err)?;
    let bump = pda.bump;
    let args: Args = Box::new(move |proof| {
        wincode::serialize(&CloseArgs {
            proof,
            vault: vault_id,
            viewing: *viewing.as_bytes(),
            user_bump: bump,
        })
        .map_err(err)
    });
    let head = vec![AccountMeta::new(user_key, true)];
    Prepared::new(
        transaction,
        user,
        CircuitId::Close,
        tag::CLOSE,
        args,
        head,
        vec![pda.address()],
    )
}

/// The test price account of vault `id` (`test-price` builds).
pub fn test_price_account(vault_id: u64) -> Address {
    test_price_address(vault_id).0
}

/// Sets vault `vault_id`'s test price account (creating it on first use):
/// `price` and `confidence` in debt base units per collateral base unit x
/// 1e12, `published` in unix seconds. The keeper signs and pays.
pub fn set_test_price(
    keeper: Address,
    vault_id: u64,
    price: u64,
    confidence: u64,
    published: i64,
) -> Result<Instruction> {
    let mut data = vec![tag::SET_TEST_PRICE];
    data.extend(
        wincode::serialize(&SetTestPriceArgs {
            vault: vault_id,
            price,
            confidence,
            published,
        })
        .map_err(err)?,
    );
    Ok(Instruction {
        program_id: crate::ID,
        accounts: vec![
            AccountMeta::new(keeper, true),
            AccountMeta::new(test_price_account(vault_id), false),
            AccountMeta::new_readonly(Address::default(), false),
        ],
        data,
    })
}

/// Every output of `spp` decrypted with the sender's transaction viewing key
/// (the keeper's, for a keeper-built transaction), in slot order; `None` for
/// an empty slot.
pub fn read_outputs(
    sender: &ShieldedKeypair,
    first_nullifier: &[u8; 32],
    spp: &SppProofInputs,
) -> Result<Vec<Option<ConfidentialOutputPlaintext>>> {
    let key = sender
        .get_transaction_viewing_key(first_nullifier)
        .map_err(err)?;
    let data = &spp.external_data;
    ensure!(
        key.pubkey().as_bytes() == &data.tx_viewing_pk,
        "not this sender's transaction"
    );
    data.outputs
        .iter()
        .enumerate()
        .map(|(slot, output)| {
            output
                .data
                .as_ref()
                .map(|payload| {
                    // Borsh `OutputDataEncoding::Encrypted(blob)`: variant 1, a
                    // u32 length, then the blob, whose first byte is the scheme.
                    let (&scheme, body) = payload
                        .strip_prefix(&[1])
                        .and_then(|rest| rest.get(4..)?.split_first())
                        .context("not an encrypted output")?;
                    ensure!(
                        scheme == EncryptedScheme::Confidential.as_byte(),
                        "not a confidential output"
                    );
                    Confidential::decrypt_with_tx_key(&key, body, data.salt, slot as u32)
                        .map_err(err)
                })
                .transpose()
        })
        .collect()
}
