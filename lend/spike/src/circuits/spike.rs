use spr_program::{
    circuit::{
        CheckedTransaction, ConfidentialTransaction, DataUtxo, TokenUtxos, UniqueDataUtxo, Utxo,
        UtxoTrait,
    },
    input, require, require_eq, Asset, CircuitError, ClientInputs, Field, PublicInputs,
    ShieldedAddress, TxContext, ZkProgram,
};

use super::{
    assert_intent, assert_mint, assert_note,
    math::{self, accrue, refresh, PRICE_SCALE, TUSD_POOL},
    purpose, reserve_address, reserve_seed, IntentState, PositionState, ReserveState,
};

// ---------------------------------------------------------------- init

/// Creates the reserve at its unique address with an empty balance.
#[input]
#[derive(ZkProgram)]
pub struct SpikeInit {
    pub private: SpikeInitPrivateInputs,
    pub public: SpikeInitPublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct SpikeInitPrivateInputs {
    pub tx_context: TxContext,
    pub mint: Asset,
    pub salt: u128,
}

#[input]
#[derive(PublicInputs)]
pub struct SpikeInitPublicInputs {
    pub pool_owner: ShieldedAddress,
    pub mint_hi: u128,
    pub mint_lo: u128,
    pub now: u64,
}

impl SpikeInit {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        assert_mint(&p.mint, public.mint_hi, public.mint_lo);
        let mut reserve = UniqueDataUtxo::<ReserveState>::new_init(
            public.pool_owner.owner(),
            &reserve_seed(&p.mint)?,
        )?
        .with_asset(&p.mint)?;
        reserve.supply_price = PRICE_SCALE;
        reserve.borrow_price = PRICE_SCALE;
        reserve.last_update = public.now;
        reserve.salt = p.salt;
        ConfidentialTransaction::new(&p.tx_context, public)
            .with(reserve)
            .check()
    }
}

// ---------------------------------------------------------------- accrue

/// Spends the reserve and re-creates it with a changed balance and state:
/// the keeper's note funds it, supply and debt shares grow, interest accrues
/// to `now`.
#[input]
#[derive(ZkProgram)]
pub struct SpikeAccrue {
    pub private: SpikeAccruePrivateInputs,
    pub public: SpikeAccruePublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct SpikeAccruePrivateInputs {
    pub tx_context: TxContext,
    pub reserve_utxo: Utxo,
    pub reserve_state: ReserveState,
    pub mint: Asset,
    pub funding: Utxo,
    pub amount: u64,
    pub debt_shares: u64,
    pub salt: u128,
}

#[input]
#[derive(PublicInputs)]
pub struct SpikeAccruePublicInputs {
    pub pool_owner: ShieldedAddress,
    pub keeper_identity: Field,
    pub mint_hi: u128,
    pub mint_lo: u128,
    pub now: u64,
}

/// The same transaction as [`SpikeAccrue`] without the maths, to price it.
#[input]
#[derive(ZkProgram)]
pub struct SpikeMove {
    pub private: SpikeAccruePrivateInputs,
    pub public: SpikeAccruePublicInputs,
}

fn accrue_body(
    p: &SpikeAccruePrivateInputs,
    public: &SpikeAccruePublicInputs,
    maths: bool,
) -> Result<CheckedTransaction, CircuitError> {
    assert_mint(&p.mint, public.mint_hi, public.mint_lo);
    let pool = public.pool_owner.owner();
    let mut reserve = UniqueDataUtxo::new_mut(
        &p.reserve_utxo,
        &reserve_address(pool, &p.mint)?,
        &p.reserve_state,
    )?;
    assert_note(&reserve, pool, &p.mint);
    if maths {
        accrue(&mut reserve, TUSD_POOL.fee_bps, public.now);
    }

    let mut funding = TokenUtxos::new_mut(core::slice::from_ref(&p.funding))?;
    require_eq(funding.owner().key().identity()?, public.keeper_identity);
    funding.transfer(&mut reserve, &p.amount)?;

    if maths {
        let minted = math::shares_for_supply(p.amount, reserve.supply_price);
        reserve.supply_shares = math::add(reserve.supply_shares, minted);
        reserve.debt_shares = math::add(reserve.debt_shares, p.debt_shares);
        refresh(&mut reserve, &TUSD_POOL, p.salt);
    } else {
        reserve.last_update = public.now;
        reserve.salt = p.salt;
    }
    let _ = PRICE_SCALE;

    ConfidentialTransaction::new(&p.tx_context, public)
        .with(reserve)
        .with(funding)
        .check()
}

impl SpikeAccrue {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        accrue_body(&self.private, &self.public, true)
    }
}

impl SpikeMove {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        accrue_body(&self.private, &self.public, false)
    }
}

// ---------------------------------------------------------------- fund

/// The user's own note funds an intent and, for the spike, a position, both
/// owned by the user's PDA.
#[input]
#[derive(ZkProgram)]
pub struct SpikeFund {
    pub private: SpikeFundPrivateInputs,
    pub public: SpikeFundPublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct SpikeFundPrivateInputs {
    pub tx_context: TxContext,
    pub source: Utxo,
    pub intent_amount: u64,
    pub position_amount: u64,
    pub position_id: u64,
    pub position_debt_shares: u64,
    pub expiry: u64,
}

#[input]
#[derive(PublicInputs)]
pub struct SpikeFundPublicInputs {
    pub user_owner: ShieldedAddress,
    pub maker_identity: Field,
}

impl SpikeFund {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        let mut tokens = TokenUtxos::new_mut(core::slice::from_ref(&p.source))?;
        let maker = tokens.owner();
        require_eq(maker.key().identity()?, public.maker_identity);
        let asset = tokens.asset();

        let mut intent =
            DataUtxo::<IntentState>::new_init(public.user_owner.owner()).with_asset(&asset)?;
        require(p.intent_amount != 0);
        tokens.transfer(&mut intent, &p.intent_amount)?;
        intent.maker = maker;
        intent.purpose = purpose::REPAY;
        intent.target = p.position_id;
        intent.expiry = p.expiry;

        let mut position =
            DataUtxo::<PositionState>::new_init(public.user_owner.owner()).with_asset(&asset)?;
        tokens.transfer(&mut position, &p.position_amount)?;
        position.owner = maker;
        position.vault = 1;
        position.position_id = p.position_id;
        position.debt_shares = p.position_debt_shares;

        ConfidentialTransaction::new(&p.tx_context, public)
            .with(tokens)
            .with(intent)
            .with(position)
            .check()
    }
}

// ---------------------------------------------------------------- touch

/// The largest shape: spends the reserve (pool PDA), a position (user PDA,
/// optional) and an intent (user PDA); outputs reserve, position, change and
/// one zero-value padding output. The repay rule, with accrual.
#[input]
#[derive(ZkProgram)]
pub struct SpikeTouch {
    pub private: SpikeTouchPrivateInputs,
    pub public: SpikeTouchPublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct SpikeTouchPrivateInputs {
    pub tx_context: TxContext,
    pub reserve_utxo: Utxo,
    pub reserve_state: ReserveState,
    pub mint: Asset,
    pub intent_utxo: Utxo,
    pub intent_state: IntentState,
    pub position_utxo: Utxo,
    pub position_state: PositionState,
    /// The optional input: the maker's own note topping up the repayment, or
    /// `Utxo::dummy()`.
    pub top_up: Utxo,
    pub top_up_amount: u64,
    pub position_id: u64,
    pub salt: u128,
}

#[input]
#[derive(PublicInputs)]
pub struct SpikeTouchPublicInputs {
    pub pool_owner: ShieldedAddress,
    pub user_owner: ShieldedAddress,
    pub maker_identity: Field,
    pub now: u64,
    pub price_bound: u64,
    pub mint_hi: u128,
    pub mint_lo: u128,
}

impl SpikeTouch {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        assert_mint(&p.mint, public.mint_hi, public.mint_lo);
        let pool = public.pool_owner.owner();
        let user = public.user_owner.owner();

        // Reserve, accrued to now.
        let mut reserve = UniqueDataUtxo::new_mut(
            &p.reserve_utxo,
            &reserve_address(pool, &p.mint)?,
            &p.reserve_state,
        )?;
        assert_note(&reserve, pool, &p.mint);
        accrue(&mut reserve, TUSD_POOL.fee_bps, public.now);

        // Intent, made by the signer for this position.
        let mut intent = DataUtxo::new_mut(&p.intent_utxo, &p.intent_state)?.close();
        assert_note(&intent, user, &p.mint);
        let maker = intent.maker;
        require_eq(maker.key().identity()?, public.maker_identity);
        assert_intent(&intent, purpose::REPAY, p.position_id, public.now);

        // Position: a data input is fixed per circuit (new_init or new_mut,
        // never chosen by a witness), so it must already exist.
        let mut position = DataUtxo::new_mut(&p.position_utxo, &p.position_state)?;
        assert_note(&position, user, &p.mint);
        require_eq(position.owner, maker);
        require_eq(position.position_id, p.position_id);

        // The optional input: a maker's note, or a dummy slot.
        let mut top_up =
            TokenUtxos::new_mut(core::slice::from_ref(&p.top_up))?.with_optional(&maker, &p.mint);
        let mut repay = TokenUtxos::new_init(&maker, &p.mint);
        let all = intent.amount()?;
        intent.transfer(&mut repay, &all)?;
        top_up.transfer(&mut repay, &p.top_up_amount)?;

        // Repay.
        let amount = math::add(all, p.top_up_amount);
        let (left, change) = math::repay_shares(position.debt_shares, amount, reserve.borrow_price);
        let removed = math::sub(position.debt_shares, left);
        reserve.debt_shares = math::sub(reserve.debt_shares, removed);
        position.debt_shares = left;
        let paid = math::sub(amount, change);
        // What is left in `repay` (the change) goes back to the maker; zero
        // outputs are empty slots, which pads the shape.
        repay.transfer(&mut reserve, &paid)?;

        // A health check against the public bound.
        let debt = math::debt_of(position.debt_shares, reserve.borrow_price);
        let value = math::assets_of(position.amount()?, public.price_bound);
        require(debt <= value);

        refresh(&mut reserve, &TUSD_POOL, p.salt);
        ConfidentialTransaction::new(&p.tx_context, public)
            .with(reserve)
            .with(position)
            .with(intent)
            .with(top_up)
            .with(repay)
            .check()
    }
}
