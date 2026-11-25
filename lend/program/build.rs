//! The program ID, the admin, the keeper set, the pool viewing key and the
//! first vault's mints and oracle are build parameters, so a deployment
//! needs no edit to tracked files. `build_params.rs` lists them, their local
//! defaults and the checks (a `mainnet` build needs every one set and no
//! public test key). They land in `$OUT_DIR/ids.rs`.
use std::{env, fs, path::Path};

#[allow(dead_code)]
#[path = "build_params.rs"]
mod build_params;

fn main() {
    println!("cargo:rerun-if-changed=build_params.rs");
    for (name, _) in build_params::DEFAULTS {
        println!("cargo:rerun-if-env-changed={name}");
    }
    println!("cargo:rerun-if-env-changed={}", build_params::LEGACY_KEEPER);
    let mainnet = env::var_os("CARGO_FEATURE_MAINNET").is_some();
    let p =
        build_params::read(|name| env::var(name).ok(), mainnet).unwrap_or_else(|e| panic!("{e}"));

    let address = |name: &str, value: &str| {
        format!(
            "pub const {name}: solana_address::Address =\n    solana_address::Address::from_str_const(\"{value}\");\n"
        )
    };
    let bytes = |b: &[u8]| {
        b.iter()
            .map(|b| format!("0x{b:02x}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let keepers: Vec<String> = p
        .keepers
        .iter()
        .map(|k| format!("    solana_address::Address::from_str_const(\"{k}\"),\n"))
        .collect();
    let out = [
        format!("pinocchio::address::declare_id!(\"{}\");\n", p.program_id),
        address("ADMIN", &p.admin),
        format!(
            "/// Any of these may sign as the keeper.\npub const KEEPERS: &[solana_address::Address] = &[\n{}];\n",
            keepers.concat()
        ),
        format!(
            "/// The pool viewing key: every `pool_viewing` argument must equal it.\npub const POOL_VIEWING: [u8; 33] = [{}];\n",
            bytes(&p.pool_viewing)
        ),
        address("VAULT1_COLLATERAL_MINT", &p.collateral_mint),
        address("VAULT1_DEBT_MINT", &p.debt_mint),
        address("VAULT1_PRICE_ACCOUNT", &p.price_account),
        format!("pub const VAULT1_FEED_ID: [u8; 32] = [{}];\n", bytes(&p.feed_id)),
    ]
    .concat();
    let path = Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).join("ids.rs");
    fs::write(path, out).expect("write ids.rs");
}
