//! The `close` circuit (Vault; see `lend/AGENTS.md`): position with no debt -> collateral to owner; browser-proved, owner signs.
//!
//! Not built yet. Follow `intent.rs` and `cancel_intent.rs`: recompute the
//! reserve address with `reserve_address`, pin every note with `assert_note`,
//! accrue with `math::accrue` first and end with `math::refresh`.
