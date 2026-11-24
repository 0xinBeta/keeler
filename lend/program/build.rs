//! The program ID, the admin and the keeper are build parameters, so a
//! deployment needs no edit to tracked files:
//!
//! - `KEELER_LEND_PROGRAM_ID` (default: the fixed local test ID)
//! - `KEELER_LEND_ADMIN`, `KEELER_LEND_KEEPER` (default: the local fixture's
//!   actors 3 and 0, funded on every `spr-program-test` localnet)
//!
//! - the first vault's mints and oracle (see `src/vaults.rs`):
//!   `KEELER_LEND_VAULT1_COLLATERAL_MINT`, `KEELER_LEND_VAULT1_DEBT_MINT`,
//!   `KEELER_LEND_VAULT1_PRICE_ACCOUNT` (defaults: SOL, the localnet
//!   fixture's SPL mint, Pyth's SOL/USD account) and
//!   `KEELER_LEND_VAULT1_FEED_ID` (64 hex; default: Pyth's SOL/USD feed)
//!
//! Each is base58 unless noted. They land in `$OUT_DIR/ids.rs`.
use std::{env, fs, path::Path};

const DEFAULTS: [(&str, &str); 6] = [
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
    (
        "KEELER_LEND_VAULT1_COLLATERAL_MINT",
        "11111111111111111111111111111111",
    ),
    ("KEELER_LEND_VAULT1_DEBT_MINT", "2moKCsA6LcKd7e6v1zCNAMFkoYiExZVGfEagvAZD1rYh"),
    (
        "KEELER_LEND_VAULT1_PRICE_ACCOUNT",
        "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE",
    ),
];
const FEED_ID: (&str, &str) = (
    "KEELER_LEND_VAULT1_FEED_ID",
    "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
);

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
    let (name, default) = FEED_ID;
    println!("cargo:rerun-if-env-changed={name}");
    let value = env::var(name).unwrap_or_else(|_| default.to_owned());
    let value = value.strip_prefix("0x").unwrap_or(&value);
    assert!(
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "{name}={value} is not 32 bytes of hex"
    );
    let bytes: Vec<String> = (0..32)
        .map(|i| format!("0x{}", &value[2 * i..2 * i + 2]))
        .collect();
    out.push_str(&format!(
        "pub const VAULT1_FEED_ID: [u8; 32] = [{}];\n",
        bytes.join(", ")
    ));
    let path = Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).join("ids.rs");
    fs::write(path, out).expect("write ids.rs");
}
