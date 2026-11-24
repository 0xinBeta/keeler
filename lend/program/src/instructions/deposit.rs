//! `deposit` (Vault; see `lend/AGENTS.md`): intent, position -> position; keeper-proved, authorised by the intent. A data input cannot be optional (SPIKE.md item 3), so opening and adding need separate circuits.
//!
//! A stub until built. It owns error codes 20600-20699 in `error.rs` and
//! `tag::DEPOSIT`. Reuse `super::{parse, check_now, require_keeper, cpi,
//! Pda}`; an instruction that spends a reserve calls `require_keeper`.
use pinocchio::{AccountView, ProgramResult};

use crate::error::LendError;

pub fn process(_accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    Err(LendError::DepositNotBuilt.into())
}
