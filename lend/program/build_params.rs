//! The build parameters, read and checked by `build.rs`. A plain module with
//! no dependencies so the checks have unit tests: `lib.rs` includes it under
//! `cfg(test)` (`cargo test -p keeler-lend --lib build_params`).
//!
//! - `KEELER_LEND_PROGRAM_ID`, `KEELER_LEND_ADMIN` (base58)
//! - `KEELER_LEND_KEEPERS`: 1 to 4 base58 addresses, comma separated, no
//!   duplicates; any of them may sign as the keeper
//! - `KEELER_LEND_POOL_VIEWING`: the pool viewing public key, 66 hex (33
//!   bytes, the X25519 tag `04` then the key); every `pool_viewing` argument
//!   must equal it
//! - the first vault's mints and oracle (see `src/vaults.rs`):
//!   `KEELER_LEND_VAULT1_COLLATERAL_MINT`, `KEELER_LEND_VAULT1_DEBT_MINT`,
//!   `KEELER_LEND_VAULT1_PRICE_ACCOUNT` (base58) and
//!   `KEELER_LEND_VAULT1_FEED_ID` (64 hex)
//!
//! Without `mainnet`, an unset parameter takes its local default (the
//! fixture keys below). With `mainnet`, every parameter must be set, even
//! where the default is the real value (SOL, Pyth's SOL/USD account and
//! feed), and no admin, keeper or pool key may be a fixture key, nor the debt
//! mint the localnet fixture's.

pub const PROGRAM_ID: &str = "KEELER_LEND_PROGRAM_ID";
pub const ADMIN: &str = "KEELER_LEND_ADMIN";
pub const KEEPERS: &str = "KEELER_LEND_KEEPERS";
pub const POOL_VIEWING: &str = "KEELER_LEND_POOL_VIEWING";
pub const COLLATERAL_MINT: &str = "KEELER_LEND_VAULT1_COLLATERAL_MINT";
pub const DEBT_MINT: &str = "KEELER_LEND_VAULT1_DEBT_MINT";
pub const PRICE_ACCOUNT: &str = "KEELER_LEND_VAULT1_PRICE_ACCOUNT";
pub const FEED_ID: &str = "KEELER_LEND_VAULT1_FEED_ID";
/// Replaced by `KEELER_LEND_KEEPERS`; refused so an old script cannot build
/// with the fixture keepers by mistake.
pub const LEGACY_KEEPER: &str = "KEELER_LEND_KEEPER";

/// Every variable the build reads, with its local default.
pub const DEFAULTS: [(&str, &str); 8] = [
    (PROGRAM_ID, "A7qaBKJ1gJRcKwMWyyRdr9YhmbBmnMaemM9Sijah3EbC"),
    // spr-program-test's fixture::actor(3).
    (ADMIN, "7pzvLuh4xhYj55VQdGPdKMPh5JXnJvxD4e8mZPdQVmER"),
    // fixture::actor(0) and fixture::payer(), both funded on every localnet.
    (
        KEEPERS,
        "D9n49QguV8mnuUp9NiSx6Qg5XuyLqdedz6eN7vuTDi3E,3bYhss3i8Y4W99hUsDCM7Yn9bozL5wKuFTyCY4jPJD5a",
    ),
    // The viewing key of the fixture pool key (keeler_lend_sdk::FIXTURE_POOL_SEED).
    (
        POOL_VIEWING,
        "04eff66782e8d5e74f22a134d513919285e4b1b1c1fcee15a7d31e3b4f926cf011",
    ),
    // SOL.
    (COLLATERAL_MINT, "11111111111111111111111111111111"),
    // The localnet fixture's SPL mint.
    (DEBT_MINT, "2moKCsA6LcKd7e6v1zCNAMFkoYiExZVGfEagvAZD1rYh"),
    // Pyth's SOL/USD push account and feed.
    (
        PRICE_ACCOUNT,
        "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE",
    ),
    (
        FEED_ID,
        "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
    ),
];

/// Addresses whose secrets are public: spr-program-test's actors 0 to 3,
/// payer and protocol authority, the fixture pool key and the local program
/// ID. A mainnet build refuses any of them as the program, admin or keeper.
pub const FIXTURE_KEYS: [&str; 8] = [
    "D9n49QguV8mnuUp9NiSx6Qg5XuyLqdedz6eN7vuTDi3E",
    "7hyqokrUCaqGeSEPhLSPvJMXRgYUdFmLyVrwntT7cQBc",
    "C1ENYfXx6FGY6jmmtw3o9gDH6UEWZvvKcVhxoJVeobpm",
    "7pzvLuh4xhYj55VQdGPdKMPh5JXnJvxD4e8mZPdQVmER",
    "3bYhss3i8Y4W99hUsDCM7Yn9bozL5wKuFTyCY4jPJD5a",
    "HF3DijLymE9gVYeG3kX1N19WZXu5BH5yWBgsvnVtm7tw",
    "8AAosp3AbbVuYYzbnhQhruyFi3fvAfk2wsz2xAi2knUn",
    "A7qaBKJ1gJRcKwMWyyRdr9YhmbBmnMaemM9Sijah3EbC",
];

pub const MAX_KEEPERS: usize = 4;

#[derive(Debug, PartialEq, Eq)]
pub struct Params {
    pub program_id: String,
    pub admin: String,
    pub keepers: Vec<String>,
    pub pool_viewing: [u8; 33],
    pub collateral_mint: String,
    pub debt_mint: String,
    pub price_account: String,
    pub feed_id: [u8; 32],
}

/// Reads the parameters through `var` (the build environment) and checks
/// them; `mainnet` is the feature of the same name.
pub fn read(var: impl Fn(&str) -> Option<String>, mainnet: bool) -> Result<Params, String> {
    if var(LEGACY_KEEPER).is_some() {
        return Err(format!(
            "{LEGACY_KEEPER} is replaced by {KEEPERS} (comma separated)"
        ));
    }
    let get = |name: &str| -> Result<String, String> {
        match var(name) {
            Some(value) => Ok(value),
            None if mainnet => Err(format!("a mainnet build needs {name} set explicitly")),
            None => Ok(DEFAULTS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, d)| d.to_string())
                .expect("a default for every parameter")),
        }
    };
    let address = |name: &str| -> Result<String, String> {
        let value = get(name)?;
        base58(name, &value)?;
        Ok(value)
    };
    let keepers: Vec<String> = get(KEEPERS)?
        .split(',')
        .map(|k| k.trim().to_string())
        .collect();
    for k in &keepers {
        base58(KEEPERS, k)?;
    }
    if keepers.len() > MAX_KEEPERS {
        return Err(format!(
            "{KEEPERS} has {} keepers, at most {MAX_KEEPERS}",
            keepers.len()
        ));
    }
    // Base58 of 32 bytes is canonical, so equal addresses are equal strings.
    if (1..keepers.len()).any(|i| keepers[..i].contains(&keepers[i])) {
        return Err(format!("{KEEPERS} lists a keeper twice"));
    }
    let params = Params {
        program_id: address(PROGRAM_ID)?,
        admin: address(ADMIN)?,
        keepers,
        pool_viewing: hex::<33>(POOL_VIEWING, &get(POOL_VIEWING)?)?,
        collateral_mint: address(COLLATERAL_MINT)?,
        debt_mint: address(DEBT_MINT)?,
        price_account: address(PRICE_ACCOUNT)?,
        feed_id: hex::<32>(FEED_ID, &get(FEED_ID)?)?,
    };
    if params.pool_viewing[0] != 0x04 {
        return Err(format!("{POOL_VIEWING} must start with the X25519 tag 04"));
    }
    if mainnet {
        let fixture = |name: &str, value: &str| -> Result<(), String> {
            if FIXTURE_KEYS.contains(&value) {
                Err(format!(
                    "{name}={value} is a public test key, refused in a mainnet build"
                ))
            } else {
                Ok(())
            }
        };
        fixture(PROGRAM_ID, &params.program_id)?;
        fixture(ADMIN, &params.admin)?;
        for k in &params.keepers {
            fixture(KEEPERS, k)?;
        }
        if get_default(POOL_VIEWING) == hex_string(&params.pool_viewing) {
            return Err(format!(
                "{POOL_VIEWING} is the fixture pool key's, refused in a mainnet build"
            ));
        }
        // The localnet fixture's mint: its authority's secret is public.
        if params.debt_mint == get_default(DEBT_MINT) {
            return Err(format!(
                "{DEBT_MINT} is the localnet fixture's mint, refused in a mainnet build"
            ));
        }
    }
    Ok(params)
}

fn get_default(name: &str) -> &'static str {
    DEFAULTS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or("", |(_, d)| d)
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Base58 of 32 bytes is 32 to 44 characters; anything else is a typo. The
/// program's `from_str_const` decodes it and fails the build on a bad one.
fn base58(name: &str, value: &str) -> Result<(), String> {
    if (32..=44).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() && !b"0OIl".contains(&b))
    {
        Ok(())
    } else {
        Err(format!("{name}: {value:?} is not a base58 address"))
    }
}

fn hex<const N: usize>(name: &str, value: &str) -> Result<[u8; N], String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    if value.len() != 2 * N || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{name}: {value:?} is not {N} bytes of hex"));
    }
    let mut out = [0; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&value[2 * i..2 * i + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// The mainnet SOL/USDC vault with real (non-fixture) keys.
    fn mainnet() -> HashMap<&'static str, String> {
        HashMap::from([
            (PROGRAM_ID, "KeeLendProgram1111111111111111111111111111".into()),
            (ADMIN, "SQDSvau1tAdmin11111111111111111111111111111".into()),
            (
                KEEPERS,
                "Keeper1PrimaryHost11111111111111111111111111,Keeper2StandbyHost1111111111111111111111111"
                    .into(),
            ),
            (
                POOL_VIEWING,
                "04a1f6f600c861480ec32ea374b7c7cbf94be2a78f716fcc527187133d60a8230f".into(),
            ),
            (COLLATERAL_MINT, "11111111111111111111111111111111".into()),
            (DEBT_MINT, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into()),
            (PRICE_ACCOUNT, "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE".into()),
            (
                FEED_ID,
                "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d".into(),
            ),
        ])
    }

    fn run(env: &HashMap<&'static str, String>, mainnet: bool) -> Result<Params, String> {
        read(|n| env.get(n).cloned(), mainnet)
    }

    #[test]
    fn real_sol_usdc_parameters_pass_on_mainnet() {
        let p = run(&mainnet(), true).unwrap();
        assert_eq!(p.keepers.len(), 2);
        assert_eq!(p.debt_mint, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        assert_eq!(p.pool_viewing[0], 0x04);
        assert_eq!(p.feed_id[0], 0xef);
    }

    #[test]
    fn the_local_defaults_are_two_fixture_keepers_and_a_third_pool_key() {
        let p = run(&HashMap::new(), false).unwrap();
        assert_eq!(p.keepers, [FIXTURE_KEYS[0], FIXTURE_KEYS[4]]);
        assert_eq!(p.admin, FIXTURE_KEYS[3]);
        assert_eq!(hex_string(&p.pool_viewing), get_default(POOL_VIEWING));
    }

    #[test]
    fn every_missing_variable_fails_on_mainnet() {
        for (name, _) in DEFAULTS {
            let mut env = mainnet();
            env.remove(name);
            let e = run(&env, true).unwrap_err();
            assert!(e.contains(name), "{e}");
            // Not on a local build: the default fills it.
            run(&env, false).unwrap();
        }
    }

    #[test]
    fn each_fixture_key_fails_as_admin_keeper_or_program_on_mainnet() {
        for key in FIXTURE_KEYS {
            for (name, value) in [
                (ADMIN, key.to_string()),
                (KEEPERS, key.to_string()),
                (
                    KEEPERS,
                    format!("Keeper1PrimaryHost11111111111111111111111111,{key}"),
                ),
                (PROGRAM_ID, key.to_string()),
            ] {
                let mut env = mainnet();
                env.insert(name, value);
                let e = run(&env, true).unwrap_err();
                assert!(e.contains("public test key"), "{name}: {e}");
            }
        }
        let mut env = mainnet();
        env.insert(POOL_VIEWING, get_default(POOL_VIEWING).into());
        assert!(run(&env, true).unwrap_err().contains("fixture pool key"));
        let mut env = mainnet();
        env.insert(DEBT_MINT, get_default(DEBT_MINT).into());
        assert!(run(&env, true).unwrap_err().contains("fixture's mint"));
        run(&env, false).unwrap();
    }

    #[test]
    fn keeper_lists() {
        let k = "Keeper1PrimaryHost11111111111111111111111111";
        for (list, ok) in [
            (k.to_string(), true),
            (format!("{k},{k}"), false),
            (
                format!("{k}, Keeper2StandbyHost1111111111111111111111111,{k}"),
                false,
            ),
            (String::new(), false),
            (format!("{k},"), false),
            (
                [1, 2, 3, 4]
                    .map(|i| format!("Keeper{i}Host111111111111111111111111111111"))
                    .join(","),
                true,
            ),
            (
                [1, 2, 3, 4, 5]
                    .map(|i| format!("Keeper{i}Host111111111111111111111111111111"))
                    .join(","),
                false,
            ),
        ] {
            let mut env = mainnet();
            env.insert(KEEPERS, list.clone());
            assert_eq!(run(&env, true).is_ok(), ok, "{list}");
        }
    }

    #[test]
    fn malformed_values_and_the_old_keeper_variable_fail() {
        for (name, value) in [
            (POOL_VIEWING, "04ab".to_string()),
            (POOL_VIEWING, format!("05{}", "11".repeat(32))),
            (POOL_VIEWING, format!("04{}", "zz".repeat(32))),
            (FEED_ID, "ef".into()),
            (ADMIN, "not base58 0OIl".into()),
            (DEBT_MINT, "short".into()),
            (LEGACY_KEEPER, FIXTURE_KEYS[0].into()),
        ] {
            let mut env = HashMap::new();
            env.insert(name, value);
            assert!(run(&env, false).is_err(), "{name}");
        }
    }
}
