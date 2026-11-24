//! `close` (Vault; see `lend/AGENTS.md`): position with no debt -> collateral to owner; browser-proved, owner signs.
//!
//! A stub until built. It owns error codes 21000-21099 in `error.rs` and
//! `tag::CLOSE`. Reuse `super::{parse, check_now, require_keeper, cpi,
//! Pda}`; an instruction that spends a reserve calls `require_keeper`.
use pinocchio::{AccountView, ProgramResult};

use crate::error::LendError;

pub fn process(_accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    Err(LendError::CloseNotBuilt.into())
}
