//! The `supply` circuit (Earn; see `lend/AGENTS.md`): reserve, intent -> reserve, receipt; keeper-proved, authorised by the intent.
//!
//! Not built yet. Follow `intent.rs` and `cancel_intent.rs`: recompute the
//! reserve address with `reserve_address`, pin every note with `assert_note`,
//! accrue with `math::accrue` first and end with `math::refresh`.
