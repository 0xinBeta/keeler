//! Mollusk setup for the examples' program tests.
//!
//! Include it with `#[path = ".../test-utils/mollusk.rs"] mod mollusk;`. It
//! needs `mollusk-svm`, `solana-account`, `solana-instruction`,
//! `solana-program-error` and `solana-pubkey`.
#![allow(dead_code)]

use std::sync::Once;

use mollusk_svm::{result::ProgramResult, Mollusk};
use solana_account::Account;
use solana_instruction::Instruction;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

static SETUP: Once = Once::new();

/// A Mollusk instance with the SBF program `program_name` from `sbf_out_dir`
/// loaded at `program_id`. The first call in a process sets `SBF_OUT_DIR` and
/// quiets `RUST_LOG` unless the caller set it.
pub fn mollusk_with_program(
    sbf_out_dir: &str,
    program_id: [u8; 32],
    program_name: &str,
) -> (Mollusk, Pubkey) {
    SETUP.call_once(|| {
        std::env::set_var("SBF_OUT_DIR", sbf_out_dir);
        if std::env::var_os("RUST_LOG").is_none() {
            std::env::set_var("RUST_LOG", "error");
        }
    });
    let program_id = Pubkey::new_from_array(program_id);
    let mut mollusk = Mollusk::default();
    mollusk.add_program(&program_id, program_name);
    (mollusk, program_id)
}

/// Viewing keys that are not the pool's: junk, fixture actor 0's own (what a
/// keeper ignoring the pool key would write), and the pool key off by a bit.
pub const WRONG_VIEWING: [[u8; 33]; 3] = [
    [2; 33],
    [
        0x04, 0xa1, 0xf6, 0xf6, 0x00, 0xc8, 0x61, 0x48, 0x0e, 0xc3, 0x2e, 0xa3, 0x74, 0xb7, 0xc7,
        0xcb, 0xf9, 0x4b, 0xe2, 0xa7, 0x8f, 0x71, 0x6f, 0xcc, 0x52, 0x71, 0x87, 0x13, 0x3d, 0x60,
        0xa8, 0x23, 0x0f,
    ],
    {
        let mut v = keeler_lend::POOL_VIEWING;
        v[32] ^= 1;
        v
    },
];

/// `ix` with its `pool_viewing` argument (the one copy of `POOL_VIEWING` in
/// its data) replaced by `viewing`.
pub fn with_viewing(ix: &Instruction, viewing: [u8; 33]) -> Instruction {
    let mut ix = ix.clone();
    let at = ix
        .data
        .windows(33)
        .position(|w| w == keeler_lend::POOL_VIEWING)
        .expect("pool_viewing in the data");
    ix.data[at..at + 33].copy_from_slice(&viewing);
    ix
}

/// Assert `ix` reaches `reached` (the check after the pool key's) and fails
/// with `WrongPoolViewing` under every key in [`WRONG_VIEWING`].
#[track_caller]
pub fn only_the_pool_viewing_key(
    mollusk: &Mollusk,
    ix: &Instruction,
    accounts: &[(Pubkey, Account)],
    reached: ProgramError,
) {
    expect_err_exact(mollusk, ix, accounts, reached);
    let wrong = ProgramError::Custom(keeler_lend::error::LendError::WrongPoolViewing as u32);
    for viewing in WRONG_VIEWING {
        expect_err_exact(mollusk, &with_viewing(ix, viewing), accounts, wrong.clone());
    }
}

/// Assert the instruction fails with exactly `expected`.
#[track_caller]
pub fn expect_err_exact(
    mollusk: &Mollusk,
    instruction: &Instruction,
    accounts: &[(Pubkey, Account)],
    expected: ProgramError,
) {
    let result = mollusk.process_instruction(instruction, accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Failure(expected),
        "unexpected program result"
    );
}
