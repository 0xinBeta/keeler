//! `scripts/deploy-mainnet.sh` refusals. Each test runs a copy of the script
//! in a fresh git repository with stub `security` and `cargo` on the PATH, so
//! nothing reads a Keychain item, builds or touches a network. Reaching the
//! `cargo` stub means every check before the build passed.
//!
//! The ones marked `#[ignore]` need a read-only RPC (the Helius key in the
//! Keychain item `keeler-helius-api-key`) and a build; run them with
//! `cargo test -p keeler --test deploy_mainnet_refusals -- --ignored`.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

use sha2::{Digest, Sha256};

const A: &str = "11111111111111111111111111111111";
const B: &str = "SysvarRent111111111111111111111111111111111";
const POOL: &str = "04eff66782e8d5e74f22a134d513919285e4b1b1c1fcee15a7d31e3b4f926cf011";
const FEED: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const SOL_USD: &str = "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE";
const MARK: &str = "STUB-CARGO-REACHED";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "keeler-deploy-refusals-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        fs::create_dir_all(repo.join("scripts")).unwrap();
        fs::create_dir_all(repo.join(".tools")).unwrap();
        fs::copy(
            repo_root().join("scripts/deploy-mainnet.sh"),
            repo.join("scripts/deploy-mainnet.sh"),
        )
        .unwrap();
        fs::write(
            repo.join(".tools/env"),
            "export ZOLANA_CLI_BIN=/nonexistent\n",
        )
        .unwrap();
        // The script writes target/; it must not make the tree dirty.
        fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        stub(&bin, "security", "exit \"${STUB_SECURITY_RC:-44}\"");
        stub(&bin, "cargo", &format!("echo {MARK} >&2; exit 3"));
        let s = Sandbox { dir };
        s.commit();
        s
    }

    fn repo(&self) -> PathBuf {
        self.dir.join("repo")
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.repo().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }

    fn commit(&self) {
        for args in [
            &["init", "-q"][..],
            &["add", "-A"],
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "t",
                "--allow-empty",
            ],
        ] {
            let ok = Command::new("git")
                .args(args)
                .current_dir(self.repo())
                .status()
                .unwrap();
            assert!(ok.success());
        }
    }

    /// A manifest of `n` proving keys of circuits `c0`, `c1`..., written
    /// outside the repository, and a keys directory holding them.
    fn manifest(&self, n: usize) -> (PathBuf, PathBuf) {
        self.manifest_of(&numbered(n))
    }

    fn manifest_of(&self, circuits: &[String]) -> (PathBuf, PathBuf) {
        let keys = self.dir.join("keys");
        let mut lines = String::new();
        for c in circuits {
            fs::create_dir_all(keys.join(c)).unwrap();
            fs::write(keys.join(format!("{c}/pk.bin")), pk_body(c)).unwrap();
            lines += &format!("{}  {c}/pk.bin\n", pk_sha(c));
        }
        let m = self.dir.join("manifest.txt");
        fs::write(&m, lines).unwrap();
        (m, keys)
    }

    /// Real-setup verifying-key sources in `dir`, one per circuit, each
    /// naming the proving key `manifest_of` writes for `named[i]`.
    fn verifying_keys(&self, dir: &str, circuits: &[String], named: &[String]) {
        for (c, key) in circuits.iter().zip(named) {
            self.write(
                &format!("{dir}/{c}.vk.rs"),
                &format!(
                    "b\"insecure_test_setup\\x00false\\x00proving_key_sha256\\x00{}\\x00\"",
                    pk_sha(key)
                ),
            );
        }
    }

    /// Runs the script; returns (success, stderr).
    fn run(&self, args: &[&str], security_rc: i32) -> (bool, String) {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let out = Command::new("bash")
            .arg(self.repo().join("scripts/deploy-mainnet.sh"))
            .args(args)
            .env("PATH", path)
            .env("STUB_SECURITY_RC", security_rc.to_string())
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn refuses(&self, args: &[&str], want: &str) {
        let (ok, err) = self.run(args, 44);
        assert!(!ok, "{args:?} passed");
        assert!(!err.contains(MARK), "{args:?} reached the build:\n{err}");
        assert!(err.contains(want), "{args:?}: want {want:?} in:\n{err}");
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn numbered(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("c{i}")).collect()
}

fn pk_body(circuit: &str) -> String {
    format!("pk {circuit}")
}

fn pk_sha(circuit: &str) -> String {
    format!("{:x}", Sha256::digest(pk_body(circuit).as_bytes()))
}

fn stub(bin: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = bin.join(name);
    fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

/// lend's every build parameter, valid; `over` replaces or adds pairs.
fn lend_args<'a>(over: &[(&'a str, &'a str)]) -> Vec<&'a str> {
    let mut pairs: Vec<(&str, &str)> = vec![
        ("--program-id", A),
        ("--payer", A),
        ("--admin", A),
        ("--keepers", B),
        ("--pool-viewing", POOL),
        ("--collateral-mint", A),
        ("--debt-mint", USDC),
        ("--price-account", SOL_USD),
        ("--feed-id", FEED),
    ];
    for (k, v) in over {
        match pairs.iter_mut().find(|(n, _)| n == k) {
            Some(p) => p.1 = v,
            None => pairs.push((k, v)),
        }
    }
    let mut v = vec!["lend"];
    for (k, x) in pairs {
        if !x.is_empty() {
            v.extend([k, x]);
        }
    }
    v
}

#[test]
fn refuses_bad_parameters_before_anything_else() {
    let s = Sandbox::new();
    s.refuses(&["staking"], "swap|lend --check-so");
    s.refuses(
        &["lend", "--estimate", "--program-id", A, "--payer", A],
        "missing --admin",
    );
    let estimate = |over: &[(&'static str, &'static str)]| {
        let mut v = lend_args(over);
        v.push("--estimate");
        v
    };
    s.refuses(&estimate(&[("--payer", "0OIl")]), "not a base58 address");
    s.refuses(&estimate(&[("--keepers", "x,0")]), "--keepers");
    s.refuses(
        &estimate(&[(
            "--pool-viewing",
            "02eff66782e8d5e74f22a134d513919285e4b1b1c1fcee15a7d31e3b4f926cf011",
        )]),
        "starting 04",
    );
    s.refuses(
        &estimate(&[("--feed-id", "ef0d")]),
        "--feed-id is not 64 hex",
    );
    s.refuses(
        &estimate(&[("--admin", B)]),
        "--admin must be the deploy key",
    );
    s.refuses(
        &estimate(&[("--vault-index", "256")]),
        "--vault-index is 0 to 255",
    );
    s.refuses(
        &estimate(&[("--rpc-url", "https://api.mainnet-beta.solana.com")]),
        "Helius",
    );
    let mut both = estimate(&[]);
    both.push("--build-only");
    s.refuses(&both, "not both");
    s.refuses(&lend_args(&[("--multisig", A)]), "missing --manifest");
    s.refuses(
        &estimate(&[("--max-time-lock-secs", "2d")]),
        "--max-time-lock-secs is a number",
    );
}

#[test]
fn lend_refuses_any_vault_but_sol_usdc_before_the_build() {
    let s = Sandbox::new();
    let estimate = |over: &[(&'static str, &'static str)]| {
        let mut v = lend_args(over);
        v.push("--estimate");
        v
    };
    let other_feed = "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a";
    s.refuses(
        &estimate(&[("--feed-id", other_feed)]),
        &format!("--feed-id must be {FEED}"),
    );
    s.refuses(
        &estimate(&[("--price-account", B)]),
        &format!("--price-account must be {SOL_USD}"),
    );
    s.refuses(
        &estimate(&[("--collateral-mint", USDC)]),
        "--collateral-mint must be 11111111111111111111111111111111",
    );
    s.refuses(&estimate(&[("--debt-mint", A)]), "--debt-mint must be");
}

#[test]
fn refuses_verifying_keys_that_name_another_circuits_proving_key() {
    let s = Sandbox::new();
    let circuits = numbered(11);
    let mut swapped = circuits.clone();
    swapped.swap(3, 7);
    s.verifying_keys("lend/program/src/verifier", &circuits, &swapped);
    s.commit();
    let (m, keys) = s.manifest(11);
    let (m, keys) = (m.to_str().unwrap(), keys.to_str().unwrap());
    let real = [
        ("--multisig", A),
        ("--manifest", m),
        ("--keeper-config", m),
        ("--keys-dir", keys),
    ];
    s.refuses(
        &lend_args(&real),
        "c3.vk.rs is not the verifying key of the manifest's c3/pk.bin",
    );
    let mut build_only = lend_args(&[("--manifest", m)]);
    build_only.push("--build-only");
    s.refuses(&build_only, "c3.vk.rs is not the verifying key");
}

#[test]
fn refuses_a_circuit_the_manifest_does_not_list() {
    let s = Sandbox::new();
    let circuits = numbered(11);
    s.verifying_keys("lend/program/src/verifier", &circuits, &circuits);
    s.verifying_keys(
        "lend/program/src/verifier",
        &["extra".to_owned()],
        &circuits,
    );
    s.commit();
    let (m, _) = s.manifest(11);
    let mut build_only = lend_args(&[("--manifest", m.to_str().unwrap())]);
    build_only.push("--build-only");
    s.refuses(&build_only, "are not the manifest's circuits");
}

#[test]
fn refuses_a_dirty_tree() {
    let s = Sandbox::new();
    s.write("stray.txt", "x");
    let mut v = lend_args(&[]);
    v.push("--estimate");
    s.refuses(&v, "the git tree is not clean");
}

#[test]
fn refuses_before_the_ceremony() {
    let s = Sandbox::new();
    s.write(
        "lend/program/src/verifier/a.vk.rs",
        "b\"insecure_test_setup\\x00true\"",
    );
    s.commit();
    let (m, keys) = s.manifest(11);
    let (m, keys) = (m.to_str().unwrap(), keys.to_str().unwrap());
    s.refuses(
        &lend_args(&[
            ("--multisig", A),
            ("--manifest", m),
            ("--keeper-config", m),
            ("--keys-dir", keys),
        ]),
        "no key ceremony yet",
    );
    s.refuses(
        &[
            "swap",
            "--program-id",
            A,
            "--payer",
            A,
            "--multisig",
            A,
            "--manifest",
            m,
        ],
        "no ceremony/swap/make.vk.rs",
    );
}

#[test]
fn refuses_a_test_price_so_and_a_bad_manifest() {
    let s = Sandbox::new();
    let so = s.dir.join("x.so");
    fs::write(&so, b"\x7fELF...KEELER_LEND_TEST_PRICE_BUILD...").unwrap();
    let (m, _) = s.manifest(11);
    let (so, m) = (so.to_str().unwrap(), m.to_str().unwrap());
    s.refuses(
        &["lend", "--check-so", so, "--manifest", m],
        "has the test price",
    );
    // swap has 3 keys of its own, not 11.
    s.refuses(
        &["swap", "--check-so", so, "--manifest", m],
        "is not a ceremony manifest",
    );
    s.refuses(&["lend", "--check-so", so], "--check-so needs --manifest");
}

#[test]
fn lend_refuses_missing_keys_then_passes_every_check_before_the_build() {
    let s = Sandbox::new();
    let circuits = numbered(11);
    s.verifying_keys("lend/program/src/verifier", &circuits, &circuits);
    s.commit();
    let (m, keys) = s.manifest(11);
    let cfg = s.dir.join("keeper.json");
    fs::write(&cfg, "{}").unwrap();
    let (m, keys, cfg) = (
        m.to_str().unwrap(),
        keys.to_str().unwrap(),
        cfg.to_str().unwrap(),
    );
    let empty = s.dir.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let real = [
        ("--multisig", A),
        ("--manifest", m),
        ("--keeper-config", cfg),
        ("--keys-dir", keys),
    ];
    let mut wrong = real;
    wrong[3].1 = empty.to_str().unwrap();
    s.refuses(
        &lend_args(&wrong),
        "does not hold the manifest's proving keys",
    );
    let mut nocfg = real;
    nocfg[2].1 = "/nonexistent.json";
    s.refuses(&lend_args(&nocfg), "no keeper config");
    s.refuses(&lend_args(&real), "no Keychain item keeler-mainnet-deploy");
    let (ok, err) = s.run(&lend_args(&real), 0);
    assert!(!ok && err.contains(MARK), "did not reach the build:\n{err}");
}

#[test]
fn swap_passes_every_check_before_the_build() {
    let s = Sandbox::new();
    let circuits: Vec<String> = ["make", "take", "cancel"].map(String::from).into();
    s.verifying_keys("ceremony/swap", &circuits, &circuits);
    s.commit();
    let (m, _) = s.manifest_of(&circuits);
    let args = [
        "swap",
        "--program-id",
        A,
        "--payer",
        A,
        "--multisig",
        A,
        "--manifest",
        m.to_str().unwrap(),
    ];
    s.refuses(&args, "no Keychain item keeler-mainnet-deploy");
    let (ok, err) = s.run(&args, 0);
    assert!(!ok && err.contains(MARK), "did not reach the build:\n{err}");
}

/// The real script against devnet: `keeler deploy check` reads devnet's
/// genesis hash and refuses. Read-only; builds the tool.
#[test]
#[ignore = "needs a read-only RPC and a build"]
fn refuses_a_devnet_rpc() {
    let out = Command::new("bash")
        .arg(repo_root().join("scripts/deploy-mainnet.sh"))
        .args(["swap", "--estimate", "--program-id", A, "--payer", A])
        .args(["--rpc-url", "https://devnet.helius-rpc.com"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("genesis hash"),
        "{err}"
    );
}
