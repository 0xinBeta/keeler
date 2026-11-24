//! `redeem` (Earn; see `lend/AGENTS.md`): reserve, receipt -> reserve, payout, receipt; keeper-proved, owner signs.
//!
//! A stub until built. It owns error codes 20500-20599 in `error.rs` and
//! `tag::REDEEM`. Reuse `super::{parse, check_now, require_keeper, cpi,
//! Pda}`; an instruction that spends a reserve calls `require_keeper`.
use pinocchio::{AccountView, ProgramResult};

use crate::error::LendError;

pub fn process(_accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    Err(LendError::RedeemNotBuilt.into())
}
