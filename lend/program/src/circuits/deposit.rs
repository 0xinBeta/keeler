//! The `deposit` circuit (Vault; see `lend/AGENTS.md`): intent, position -> position; keeper-proved, authorised by the intent. A data input cannot be optional (SPIKE.md item 3), so opening and adding need separate circuits.
//!
//! Not built yet. Follow `intent.rs` and `cancel_intent.rs`: recompute the
//! reserve address with `reserve_address`, pin every note with `assert_note`,
//! accrue with `math::accrue` first and end with `math::refresh`.
