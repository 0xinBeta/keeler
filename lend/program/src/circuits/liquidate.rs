//! The `liquidate` circuit (Vault; see `lend/AGENTS.md`): reserve, position, liquidator's note -> reserve, position, collateral and change; keeper-proved.
//!
//! Not built yet. Follow `intent.rs` and `cancel_intent.rs`: recompute the
//! reserve address with `reserve_address`, pin every note with `assert_note`,
//! accrue with `math::accrue` first and end with `math::refresh`.
