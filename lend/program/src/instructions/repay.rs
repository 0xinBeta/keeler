//! `repay` (Vault; see `lend/AGENTS.md`): reserve, position, intent -> reserve, position, change; keeper-proved, authorised by the intent.
//!
//! A stub until built. It owns error codes 20800-20899 in `error.rs` and
//! `tag::REPAY`. Reuse `super::{parse, check_now, require_keeper, cpi,
//! Pda}`; an instruction that spends a reserve calls `require_keeper`.
use pinocchio::{AccountView, ProgramResult};

use crate::error::LendError;

pub fn process(_accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    Err(LendError::RepayNotBuilt.into())
}
