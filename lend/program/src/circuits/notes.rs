//! The note states (see `docs/plan-lend.md`, Notes) and the pinning helpers
//! every circuit uses. The SDK hashes and Borsh-encodes the states; a state's
//! data hash is `Poseidon(DATA, state hash)`, the reserve's
//! `Poseidon(UNIQ, address, state hash)`.
use borsh::{BorshDeserialize, BorshSerialize};
use spr_program::{
    circuit::{NewAddress, UtxoTrait},
    input, poseidon, require, require_eq, Asset, CircuitError, ClientInputs, Field, UtxoOwner,
    UtxoState,
};

/// The pool's state, held by the reserve note: a `UniqueDataUtxo` owned by
/// the pool PDA at the address [`reserve_address`] derives from the mint. Its
/// balance is the idle liquidity.
#[input]
#[derive(Default, PartialEq, Eq, BorshSerialize, BorshDeserialize, UtxoState, ClientInputs)]
pub struct ReserveState {
    pub supply_shares: u64,
    pub debt_shares: u64,
    /// Token base units per supply share, x 1e12; starts at 1e12.
    pub supply_price: u64,
    /// Token base units per debt share, x 1e12; starts at 1e12.
    pub borrow_price: u64,
    /// The borrow rate stored at the last update, applied until the next.
    pub rate_bps: u64,
    /// Unix seconds of the last update.
    pub last_update: u64,
    /// Fresh randomness from the keeper at every operation, so nobody who
    /// knows a transaction's blinding seed can test guesses against the
    /// reserve's hash.
    pub salt: u128,
}

/// An Earn deposit: a zero-balance note owned by the user's PDA.
#[input]
#[derive(Default, PartialEq, Eq, BorshSerialize, BorshDeserialize, UtxoState, ClientInputs)]
pub struct ReceiptState {
    pub owner: UtxoOwner,
    /// The pool id ([`crate::pool`]).
    pub pool: u64,
    pub supply_shares: u64,
}

/// A borrow position: the collateral is the note's balance, owned by the
/// user's PDA.
#[input]
#[derive(Default, PartialEq, Eq, BorshSerialize, BorshDeserialize, UtxoState, ClientInputs)]
pub struct PositionState {
    pub owner: UtxoOwner,
    /// The vault id ([`crate::vault`]).
    pub vault: u64,
    pub position_id: u64,
    pub debt_shares: u64,
}

/// Funds going in, owned by the maker's PDA, until the keeper fills the
/// intent or the maker cancels it after `expiry`.
#[input]
#[derive(Default, PartialEq, Eq, BorshSerialize, BorshDeserialize, UtxoState, ClientInputs)]
pub struct IntentState {
    /// The owner of the spent source: receives every note and change the
    /// intent pays for, and the refund.
    pub maker: UtxoOwner,
    /// [`purpose`].
    pub purpose: u8,
    /// The pool id for `SUPPLY`, the position id for `DEPOSIT` and `REPAY`.
    pub target: u64,
    /// Unix seconds; cancel is allowed only after it.
    pub expiry: u64,
}

/// What an intent's funds are for.
pub mod purpose {
    pub const SUPPLY: u8 = 1;
    pub const DEPOSIT: u8 = 2;
    pub const REPAY: u8 = 3;

    pub fn is_valid(purpose: u8) -> bool {
        (SUPPLY..=REPAY).contains(&purpose)
    }
}

/// Domain of the reserve address seed: `Poseidon(RSRV, mint_hi, mint_lo)`.
pub const RESERVE_SEED_DOMAIN: u32 = u32::from_be_bytes(*b"RSRV");

/// The mint's 32 bytes as two big-endian `u128` halves, the form the program
/// passes it in as public inputs.
pub fn mint_halves(mint: &Asset) -> (u128, u128) {
    let mut hi = 0u128;
    let mut lo = 0u128;
    for (i, byte) in mint.iter().enumerate() {
        if i < 16 {
            hi = (hi << 8) | u128::from(*byte);
        } else {
            lo = (lo << 8) | u128::from(*byte);
        }
    }
    (hi, lo)
}

/// The reserve's address: the pool PDA's address for the seed
/// `Poseidon(RSRV, mint_hi, mint_lo)`. Every circuit recomputes it; it is
/// never a free witness.
pub fn reserve_address(pool_owner: &UtxoOwner, mint: &Asset) -> Result<Field, CircuitError> {
    Ok(NewAddress::new(pool_owner, &reserve_seed(mint)?)?.address())
}

pub fn reserve_seed(mint: &Asset) -> Result<Field, CircuitError> {
    let (hi, lo) = mint_halves(mint);
    poseidon(&[
        Field::from(RESERVE_SEED_DOMAIN),
        Field::from(hi),
        Field::from(lo),
    ])
}

/// Requires `mint` to be the public `(mint_hi, mint_lo)`.
pub fn assert_mint(mint: &Asset, mint_hi: u128, mint_lo: u128) {
    let (hi, lo) = mint_halves(mint);
    require_eq(hi, mint_hi);
    require_eq(lo, mint_lo);
}

/// Requires a note to hold `asset` and belong to `owner`.
pub fn assert_note(note: &impl UtxoTrait, owner: &UtxoOwner, asset: &Asset) {
    require_eq(note.owner(), *owner);
    require_eq(note.asset(), *asset);
}

/// Requires the intent to be live at `now` and made for `purpose` and
/// `target`.
pub fn assert_intent(intent: &IntentState, purpose: u8, target: u64, now: u64) {
    require_eq(intent.purpose, purpose);
    require_eq(intent.target, target);
    require(now <= intent.expiry);
}
