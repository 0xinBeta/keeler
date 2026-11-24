//! What the Vault circuits share: the vault's rules by id, the position
//! checks, the blinding seed rule and the health comparisons
//! (`core/src/lend.rs`: `collateral_value`, `can_draw`, `is_liquidatable`,
//! `above_max_limit`).
use spr_program::{
    circuit::{CheckedTransaction, DataUtxo, Utxo, UtxoTrait},
    poseidon, require, require_eq, CircuitError, Field, TxContext, UtxoOwner,
};

use super::{
    assert_mint,
    math::{self, VaultParams, BPS, TSOL_TUSD_VAULT},
    PositionState,
};
use crate::vaults::{TSOL_TUSD_MIN_COLLATERAL, TSOL_TUSD_MIN_DEBT};

/// Domain of the blinding seed `Poseidon(LPOS, position blinding)` of every
/// transaction that spends a position.
pub const POSITION_BLINDING_DOMAIN: u32 = u32::from_be_bytes(*b"LPOS");
/// Domain of the blinding seed `Poseidon(LDEP, intent blinding)` of
/// `deposit_open`, which spends no position.
pub const DEPOSIT_BLINDING_DOMAIN: u32 = u32::from_be_bytes(*b"LDEP");

/// `floor(a x b / d)` for `a x b < 2^NB`, `0 < d < 2^DB`: the same check as
/// `math`'s private one (a hinted quotient from the two factors, pinned by
/// `q x d + r == a x b` and `r < d`). A copy, not `math`'s made public:
/// sharing its instantiations changes the Earn circuits' witness programs.
pub fn div_floor<const NB: usize, const DB: usize>(a: Field, b: Field, d: Field) -> Field {
    const { assert!(NB + DB <= 252) };
    let zero = d == Field::zero();
    require(!zero);
    let d = if zero { Field::from(1u8) } else { d };
    let ([q], [r]) = Field::hint_mulmod_divmod::<1>([a], [b], [d], 128);
    require_eq(q * d + r, a * b);
    require(r.lt::<DB>(d));
    let _ = q.to_bits::<128>();
    q
}

/// A vault's rules: its factors and minimums.
pub struct VaultRules {
    pub params: VaultParams,
    pub min_collateral: u64,
    pub min_debt: u64,
}

/// The rules of vault `id`; refused for an unknown id.
pub fn vault_rules(id: u64) -> VaultRules {
    // shortcut: one vault (tSOL/tUSD); select by id when a second is added.
    require(id == crate::vault::TSOL_TUSD);
    VaultRules {
        params: TSOL_TUSD_VAULT,
        min_collateral: TSOL_TUSD_MIN_COLLATERAL,
        min_debt: TSOL_TUSD_MIN_DEBT,
    }
}

/// Requires the transaction's blinding seed to be `Poseidon(domain, note
/// blinding)`: whoever holds `note`'s opening derives every output's
/// blinding from the published first nullifier.
pub fn assert_seed(tx: &TxContext, note: &Utxo, domain: u32) -> Result<(), CircuitError> {
    require_eq(
        tx.blinding_seed,
        poseidon(&[Field::from(domain), note.blinding])?,
    );
    Ok(())
}

/// Requires the circuit's first nullifier to be the published one, which
/// the program reads from the transact.
pub fn assert_first_nullifier(checked: &CheckedTransaction, published: Field) {
    require_eq(checked.first_nullifier, published);
}

/// Requires a position to be owned by `user` (the owner's user PDA), to hold
/// the vault's collateral `(coll_hi, coll_lo)` and to name `vault`.
pub fn assert_position(
    position: &DataUtxo<PositionState>,
    user: &UtxoOwner,
    coll_hi: u128,
    coll_lo: u128,
    vault: u64,
) {
    require_eq(position.owner(), *user);
    assert_mint(&position.asset(), coll_hi, coll_lo);
    require_eq(position.vault, vault);
}

/// `amount` is zero or at least `min`.
pub fn zero_or_at_least(amount: u64, min: u64) -> bool {
    amount == 0 || amount >= min
}

fn f(x: u64) -> Field {
    Field::from(x)
}

/// `floor(collateral x bound / 1e12)`.
pub fn value(collateral: u64, bound: u64) -> u64 {
    math::assets_of(collateral, bound)
}

/// `debt x 1e4 > factor x value`: both sides are under 2^78.
fn above(debt: u64, value: u64, factor_bps: u64) -> bool {
    (f(debt) * f(BPS)).gt::<80>(f(factor_bps) * f(value))
}

pub fn can_draw(debt: u64, value: u64, v: &VaultParams) -> bool {
    !above(debt, value, v.cf_bps)
}

pub fn is_liquidatable(debt: u64, value: u64, v: &VaultParams) -> bool {
    above(debt, value, v.lt_bps)
}

pub fn above_max_limit(debt: u64, value: u64, v: &VaultParams) -> bool {
    above(debt, value, v.lml_bps)
}

/// `debt x 1e4 >= lt x value`: liquidation leaves the position at or past
/// the threshold, never healthier.
pub fn at_or_past_threshold(debt: u64, value: u64, v: &VaultParams) -> bool {
    !(f(v.lt_bps) * f(value)).gt::<80>(f(debt) * f(BPS))
}

#[cfg(all(test, not(xark)))]
mod tests {
    use super::*;

    const VECTORS: &str = include_str!("../../../../web/tests/fixtures/lend-vectors.json");

    fn run<T>(run: impl FnOnce() -> T) -> Option<T> {
        spr_program::checked(|| Ok(run())).ok()
    }

    fn num(v: &serde_json::Value) -> u64 {
        v.as_str().expect("decimal string").parse().expect("u64")
    }

    /// The health vectors, through the circuit comparisons.
    #[test]
    fn passes_the_health_vectors() {
        let all: serde_json::Value = serde_json::from_str(VECTORS).unwrap();
        let mut checked = 0;
        for case in all["cases"].as_array().unwrap() {
            let i = &case["in"];
            let got = match case["fn"].as_str().unwrap() {
                "collateral_value" => {
                    run(|| value(num(&i["collateral"]), num(&i["price"]))).map(|x| x.to_string())
                }
                name @ ("can_draw" | "is_liquidatable" | "above_max_limit") => {
                    let v = &i["vault"];
                    let params = VaultParams {
                        cf_bps: num(&v["cf_bps"]),
                        lt_bps: num(&v["lt_bps"]),
                        lml_bps: num(&v["lml_bps"]),
                        penalty_bps: num(&v["penalty_bps"]),
                    };
                    let (debt, value) = (num(&i["debt"]), num(&i["value"]));
                    run(|| match name {
                        "can_draw" => can_draw(debt, value, &params),
                        "is_liquidatable" => is_liquidatable(debt, value, &params),
                        _ => above_max_limit(debt, value, &params),
                    })
                    .map(|b| b.to_string())
                }
                _ => continue,
            };
            checked += 1;
            let want = match &case["out"] {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Bool(b) => Some(b.to_string()),
                serde_json::Value::Null => None,
                other => Some(other.to_string()),
            };
            assert_eq!(got, want, "{} / {}", case["fn"], case["name"]);
        }
        assert_eq!(checked, 10, "vectors ran");
    }

    #[test]
    fn threshold_is_inclusive() {
        let v = TSOL_TUSD_VAULT;
        assert_eq!(run(|| at_or_past_threshold(85, 100, &v)), Some(true));
        assert_eq!(run(|| at_or_past_threshold(84, 100, &v)), Some(false));
        assert_eq!(run(|| is_liquidatable(85, 100, &v)), Some(false));
        assert_eq!(run(|| is_liquidatable(86, 100, &v)), Some(true));
    }
}
