//! A valid, proved instruction resent with one argument changed, for the
//! local-network tests: the program must refuse it with a specific error.
//! Include it with `#[path = "tamper.rs"] mod tamper;`.
#![allow(dead_code)]

use solana_address::Address;
use solana_instruction::Instruction;
use wincode::{config::DefaultConfig, SchemaReadOwned, SchemaWrite};

/// `LendError` codes as the RPC prints them (`custom program error: 0x…`).
pub const INVALID_PROOF: &str = "0x4e22";
pub const INVALID_SEEDS: &str = "0x4e26";
pub const NOT_EXPIRED: &str = "0x4f4c";
pub const UNKNOWN_VAULT: &str = "0x5079";

/// `ix` (`[tag] ++ wincode(A) ++ transact`) with its arguments edited; the
/// proof and the transact stay as they were.
pub fn tampered<A>(ix: &Instruction, edit: impl FnOnce(&mut A)) -> Instruction
where
    A: SchemaReadOwned<DefaultConfig, Dst = A> + SchemaWrite<DefaultConfig, Src = A>,
{
    let (tag, mut rest) = ix.data.split_first().expect("a tag");
    let mut args: A = wincode::deserialize_from(&mut rest).expect("the arguments");
    edit(&mut args);
    let mut data = vec![*tag];
    data.extend(wincode::serialize(&args).expect("the arguments"));
    data.extend_from_slice(rest);
    assert_ne!(data, ix.data, "the edit changes nothing");
    Instruction { data, ..ix.clone() }
}

/// The canonical bump of the PDA `[seed, key]`.
pub fn canonical(seed: &[u8], key: &[u8; 32]) -> u8 {
    Address::find_program_address(&[seed, key], &keeler_lend_sdk::ID).1
}

/// A bump below the canonical one that still derives a PDA.
pub fn non_canonical(seed: &[u8], key: &[u8; 32]) -> u8 {
    (0..canonical(seed, key))
        .rev()
        .find(|b| {
            Address::create_program_address(&[seed, key, &[*b]], &keeler_lend_sdk::ID).is_ok()
        })
        .expect("a non-canonical bump")
}

/// Requires `sent` to be refused with the program error `code`.
pub fn refused_with<T: core::fmt::Debug>(
    what: &str,
    sent: anyhow::Result<T>,
    code: &str,
) -> anyhow::Result<()> {
    match sent {
        Ok(sig) => anyhow::bail!("{what}: accepted ({sig:?}), expected {code}"),
        Err(e) => {
            let e = format!("{e:#}");
            anyhow::ensure!(e.contains(code), "{what}: expected {code}, got: {e}");
            println!("{what}: refused ({code})");
            Ok(())
        }
    }
}
