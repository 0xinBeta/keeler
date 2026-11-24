//! Program error codes, `ProgramError::Custom(code)`. Each instruction owns a
//! range of 100 so pieces of work never collide:
//!
//! | Range | Owner |
//! | --- | --- |
//! | 20000-20099 | shared checks (`instructions/mod.rs`) |
//! | 20100 | init_reserve | 20200 intent | 20300 cancel_intent |
//! | 20400 | supply | 20500 redeem (Earn) |
//! | 20600 | deposit | 20700 draw | 20800 repay | 20900 liquidate | 21000 close (Vault) |
//!
//! The shielded pool's own errors (for example 7043, a nullifier or reserve
//! address already used) pass through the CPI unchanged.
use solana_program_error::ProgramError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum LendError {
    // Shared.
    InvalidInstructionData = 20_000,
    UnknownInstruction = 20_001,
    /// The program proof does not verify against the recomputed public inputs.
    InvalidProof = 20_002,
    /// The keeper did not sign where it must.
    NotKeeper = 20_003,
    NotAdmin = 20_004,
    /// The public `now` is not in `[clock - NOW_WINDOW, clock]`.
    StaleNow = 20_005,
    /// A PDA bump that does not derive a PDA.
    InvalidSeeds = 20_006,
    /// A signer the instruction needs did not sign.
    MissingSignature = 20_007,
    /// The account list is not the expected layout.
    InvalidAccounts = 20_008,

    // cancel_intent.
    /// Cancel is allowed only after the intent's expiry.
    NotExpired = 20_300,

    // supply.
    /// The clock is past the intent's expiry: only `cancel_intent` takes it.
    IntentExpired = 20_400,

    // Stubs: each later instruction answers this until it is built.
    DepositNotBuilt = 20_600,
    DrawNotBuilt = 20_700,
    RepayNotBuilt = 20_800,
    LiquidateNotBuilt = 20_900,
    CloseNotBuilt = 21_000,
}

impl From<LendError> for ProgramError {
    fn from(error: LendError) -> Self {
        ProgramError::Custom(error as u32)
    }
}
