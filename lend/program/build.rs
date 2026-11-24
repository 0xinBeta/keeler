//! The program ID, the admin and the keeper are build parameters, so a
//! deployment needs no edit to tracked files:
//!
//! - `KEELER_LEND_PROGRAM_ID` (default: the fixed local test ID)
//! - `KEELER_LEND_ADMIN`, `KEELER_LEND_KEEPER` (default: the local fixture's
//!   actors 3 and 0, funded on every `spr-program-test` localnet)
//!
//! Each is base58. They land in `$OUT_DIR/ids.rs`.
use std::{env, fs, path::Path};

const DEFAULTS: [(&str, &str); 3] = [
    (
        "KEELER_LEND_PROGRAM_ID",
        "A7qaBKJ1gJRcKwMWyyRdr9YhmbBmnMaemM9Sijah3EbC",
    ),
    (
        "KEELER_LEND_ADMIN",
        "7pzvLuh4xhYj55VQdGPdKMPh5JXnJvxD4e8mZPdQVmER",
    ),
    (
        "KEELER_LEND_KEEPER",
        "D9n49QguV8mnuUp9NiSx6Qg5XuyLqdedz6eN7vuTDi3E",
    ),
];

fn main() {
    let mut out = String::new();
    for (name, default) in DEFAULTS {
        println!("cargo:rerun-if-env-changed={name}");
        let value = env::var(name).unwrap_or_else(|_| default.to_owned());
        // Base58 of 32 bytes is 32 to 44 characters; anything else is a typo.
        assert!(
            (32..=44).contains(&value.len())
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() && !b"0OIl".contains(&b)),
            "{name}={value} is not a base58 address"
        );
        let line = match name {
            "KEELER_LEND_PROGRAM_ID" => format!("pinocchio::address::declare_id!(\"{value}\");\n"),
            _ => format!(
                "pub const {}: solana_address::Address =\n    solana_address::Address::from_str_const(\"{value}\");\n",
                &name["KEELER_LEND_".len()..]
            ),
        };
        out.push_str(&line);
    }
    let path = Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).join("ids.rs");
    fs::write(path, out).expect("write ids.rs");
}
