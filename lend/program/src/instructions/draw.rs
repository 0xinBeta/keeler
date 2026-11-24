//! `draw` (Vault; see `lend/AGENTS.md`): reserve, position -> reserve, position, debt payout, collateral payout; keeper-proved, owner signs.
//!
//! A stub until built. It owns error codes 20700-20799 in `error.rs` and
//! `tag::DRAW`. Reuse `super::{parse, check_now, require_keeper, cpi,
//! Pda}`; an instruction that spends a reserve calls `require_keeper`.
use pinocchio::{AccountView, ProgramResult};

use crate::error::LendError;

pub fn process(_accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    Err(LendError::DrawNotBuilt.into())
}
