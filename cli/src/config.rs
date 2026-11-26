//! One JSON config per role. Neither the config nor any key read through it
//! is ever printed.
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, process::Command, str::FromStr};

use anyhow::{anyhow, ensure, Context, Result};
use keeler_core::price::{parse_pyth_account, Price};
use serde::Deserialize;
use solana_address::Address;
use solana_keypair::Keypair;
use spr_client::Rpc;

/// Where a secret comes from. `file` is for local test keys only; `env`
/// names an environment variable, for a container that has no Keychain.
/// Mainnet takes only `keychain`.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeySource {
    File(PathBuf),
    Keychain(String),
    Env(String),
}

impl KeySource {
    pub fn is_keychain(&self) -> bool {
        matches!(self, KeySource::Keychain(_))
    }

    pub fn secret(&self) -> Result<String> {
        Ok(match self {
            KeySource::File(path) => std::fs::read_to_string(path)
                .with_context(|| format!("read key file {}", path.display()))?
                .trim()
                .to_owned(),
            KeySource::Keychain(name) => keychain_secret(name)?,
            KeySource::Env(name) => env_secret(name)?,
        })
    }

    /// A Solana keypair JSON array.
    pub fn keypair(&self) -> Result<Keypair> {
        let bytes: Vec<u8> = serde_json::from_str(&self.secret()?)
            .map_err(|_| anyhow!("key is not a keypair JSON array"))?;
        Keypair::try_from(bytes.as_slice()).map_err(|_| anyhow!("key is not a valid keypair"))
    }
}

/// Who proves the pool's transact.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Prover {
    /// The local prover (`ZOLANA_PROVER_URL`), with proof data from our indexer.
    Local,
    /// Helius's attested prover, which serves at the indexer URL. The key
    /// source holds the API key, bare or as a Helius URL with an `api-key`
    /// parameter: `{"helius":{"keychain":"<item>"}}` or
    /// `{"helius":{"env":"<variable>"}}`.
    Helius(KeySource),
}

/// The highest priority fee a config may set, micro-lamports per compute
/// unit: 7,000,000 lamports (0.007 SOL) on a 1.4M-unit transaction.
pub const MAX_PRIORITY_FEE: u64 = 5_000_000;

#[derive(Clone, Deserialize)]
pub struct Network {
    pub rpc_url: String,
    pub indexer_url: String,
    pub prover: Prover,
    /// Micro-lamports per compute unit on every transaction sent; default 0.
    #[serde(default)]
    pub priority_fee: u64,
}

impl Network {
    pub fn local(rpc_url: String, indexer_url: String) -> Self {
        Self {
            rpc_url,
            indexer_url,
            prover: Prover::Local,
            priority_fee: 0,
        }
    }

    /// The RPC and indexer URLs to call, with the Helius key added. They may
    /// hold the key: never print them unredacted.
    pub fn urls(&self) -> Result<(String, String)> {
        match &self.prover {
            Prover::Local => Ok((self.rpc_url.clone(), self.indexer_url.clone())),
            Prover::Helius(source) => {
                // The key only ever goes to Helius.
                helius_host(&self.rpc_url)?;
                helius_host(&self.indexer_url)?;
                let key = helius_key(&source.secret()?)?;
                Ok((
                    with_api_key(&self.rpc_url, &key)?,
                    with_api_key(&self.indexer_url, &key)?,
                ))
            }
        }
    }

    /// Whether every secret this network reads comes from the Keychain, as
    /// mainnet requires.
    pub fn key_from_keychain(&self) -> bool {
        match &self.prover {
            Prover::Local => true,
            Prover::Helius(source) => source.is_keychain(),
        }
    }

    pub fn check(&self) -> Result<()> {
        ensure!(
            self.priority_fee <= MAX_PRIORITY_FEE,
            "network.priority_fee must be at most {MAX_PRIORITY_FEE} micro-lamports per compute unit"
        );
        if let Prover::Helius(_) = self.prover {
            for url in [&self.rpc_url, &self.indexer_url] {
                ensure!(
                    !url.contains("api-key"),
                    "put the Helius key in its key source, not the URL"
                );
                helius_host(url)?;
            }
        }
        Ok(())
    }
}

/// Fails unless `url` is https on a Helius host.
fn helius_host(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).map_err(|_| anyhow!("invalid network URL"))?;
    ensure!(
        parsed.scheme() == "https"
            && matches!(
                parsed.host_str(),
                Some("devnet.helius-rpc.com" | "mainnet.helius-rpc.com")
            ),
        "with the Helius prover, rpc_url and indexer_url must be https on devnet.helius-rpc.com or mainnet.helius-rpc.com"
    );
    Ok(())
}

/// The key from a Keychain value that is either the key or a Helius URL.
fn helius_key(secret: &str) -> Result<String> {
    let key = match reqwest::Url::parse(secret) {
        Ok(url) => url
            .query_pairs()
            .find(|(k, _)| k == "api-key")
            .map(|(_, v)| v.into_owned())
            .ok_or_else(|| anyhow!("the Helius Keychain URL has no api-key parameter"))?,
        Err(_) => secret.to_owned(),
    };
    ensure!(
        !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "the Helius API key has an unexpected form"
    );
    Ok(key)
}

fn with_api_key(url: &str, key: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(url).map_err(|_| anyhow!("invalid network URL"))?;
    url.query_pairs_mut().append_pair("api-key", key);
    Ok(url.to_string())
}

/// `text` with every `api-key=` value replaced, for logs and errors that may
/// quote a URL.
pub fn redact(text: &str) -> String {
    const MARK: &str = "api-key=";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(MARK) {
        let (head, tail) = rest.split_at(i + MARK.len());
        out.push_str(head);
        out.push_str("REDACTED");
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '%'))
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Prints a log line with any API key redacted.
pub fn log(msg: std::fmt::Arguments) {
    eprintln!("{}", redact(&msg.to_string()));
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum PriceSource {
    Fixed { price: i64, expo: i32 },
    Pyth { pyth: PythFeed },
}

/// A Pyth price feed account on chain and the feed it must carry.
#[derive(Clone, Deserialize)]
pub struct PythFeed {
    pub account: String,
    /// 64 hex characters, with or without `0x`.
    pub feed_id: String,
}

#[derive(Clone, Deserialize)]
pub struct Asset {
    pub mint: String,
    pub price: PriceSource,
}

/// Freshness and confidence limits for every Pyth price.
#[derive(Clone, Deserialize)]
pub struct Pyth {
    pub max_age_secs: i64,
    pub max_conf_bps: u16,
}

/// What both roles need to reach the chain and price assets.
#[derive(Clone, Deserialize)]
pub struct Common {
    pub network: Network,
    pub key: KeySource,
    pub assets: Vec<Asset>,
    #[serde(default)]
    pub pyth: Option<Pyth>,
}

#[derive(Clone, Deserialize)]
pub struct DealerRef {
    pub address: SocketAddr,
    /// The dealer's Solana public key; its shielded address is resolved from
    /// the user registry by this key, never taken from the quote reply.
    pub pubkey: String,
}

#[derive(Clone, Deserialize)]
pub struct TraderConfig {
    #[serde(flatten)]
    pub common: Common,
    pub dealers: Vec<DealerRef>,
    pub band_bps: u16,
    pub min_ttl_secs: u64,
    pub max_ttl_secs: u64,
}

#[derive(Clone, Deserialize)]
pub struct DealerConfig {
    #[serde(flatten)]
    pub common: Common,
    pub listen: SocketAddr,
    pub spread_bps: u16,
    pub quote_ttl_secs: u64,
    pub settle_margin_secs: u64,
    /// The HTTP API next to the TCP one.
    #[serde(default)]
    pub http: Option<Http>,
    /// Seconds between settle passes; default 1 with the local prover, else 5.
    #[serde(default)]
    pub poll_secs: Option<u64>,
    #[serde(default)]
    pub faucet: Option<Faucet>,
    /// Quote book and bound orders, reloaded on start. Default:
    /// `~/.local/state/keeler/dealer-<pubkey>.json`.
    #[serde(default)]
    pub state_file: Option<PathBuf>,
    /// While this file exists the dealer gives no new quotes; it still
    /// settles bound orders.
    #[serde(default)]
    pub pause_file: Option<PathBuf>,
    /// Last look: settle only while the quoted amount is at most the fair
    /// amount at the current price plus this much.
    #[serde(default)]
    pub settle_tolerance_bps: u16,
    #[serde(default)]
    pub limits: Vec<SellLimit>,
    /// The swap program this config is for; must be the one the binary was
    /// built with.
    pub program_id: String,
    /// The dealer key's public key, when the config names it.
    #[serde(default)]
    pub pubkey: Option<String>,
    /// Consecutive failed settles that pause quoting; default 3.
    #[serde(default)]
    pub auto_pause_after: Option<u32>,
    /// No quotes while the dealer's fee SOL is below this; default 0.01 SOL.
    #[serde(default)]
    pub min_fee_lamports: Option<u64>,
    /// The Keeler Lend keeper, served on the same HTTP server (`/lend/*`).
    #[serde(default)]
    pub lend: Option<crate::keeper::Lend>,
}

/// Limits on what traders sell to the dealer in `mint`.
#[derive(Clone, Deserialize)]
pub struct SellLimit {
    pub mint: String,
    /// The largest sell amount one order may have.
    #[serde(default)]
    pub max_sell_amount: Option<u64>,
    /// The most sold over bound orders in any rolling hour of chain time.
    #[serde(default)]
    pub hourly_sell_cap: Option<u64>,
}

#[derive(Clone, Deserialize)]
pub struct Http {
    pub listen: SocketAddr,
    /// The shared secret every request carries in `x-keeler-key`.
    pub key: KeySource,
    /// Accept requests from any address, not only loopback; every route
    /// still needs the key. For a host whose own gateway is the only way
    /// in. Refused on mainnet.
    #[serde(default)]
    pub allow_remote: bool,
}

impl Http {
    /// The listen address is loopback unless `allow_remote` is set.
    pub fn check(&self) -> Result<()> {
        ensure!(
            self.allow_remote || self.listen.ip().is_loopback(),
            "http.listen must be a loopback address unless http.allow_remote is set"
        );
        Ok(())
    }
}

/// Test tokens and fee SOL for a wallet, paid by `key`, which is the mint
/// authority of every token and is neither the dealer nor the deploy key.
#[derive(Clone, Deserialize)]
pub struct Faucet {
    pub key: KeySource,
    pub tokens: Vec<FaucetToken>,
    pub lamports: u64,
    pub per_wallet_daily: u32,
    pub daily_cap: u32,
}

#[derive(Clone, Deserialize)]
pub struct FaucetToken {
    pub mint: String,
    pub amount: u64,
}

impl TraderConfig {
    pub fn load(path: &str) -> Result<Self> {
        Self::from_json(&read(path)?).with_context(|| format!("config {path}"))
    }

    pub fn from_json(text: &str) -> Result<Self> {
        let cfg: Self = parse_json(text)?;
        cfg.check()?;
        Ok(cfg)
    }

    pub fn check(&self) -> Result<()> {
        // A band of 10,000 bps would accept a quote of 0.
        ensure!(self.band_bps < 10_000, "band_bps must be below 10000");
        ensure!(
            self.min_ttl_secs <= self.max_ttl_secs,
            "min_ttl_secs above max_ttl_secs"
        );
        ensure!(!self.dealers.is_empty(), "no dealers configured");
        self.common.check()
    }
}

impl DealerConfig {
    pub fn load(path: &str) -> Result<Self> {
        Self::from_json(&read(path)?).with_context(|| format!("config {path}"))
    }

    pub fn from_json(text: &str) -> Result<Self> {
        let cfg: Self = parse_json(text)?;
        cfg.check()?;
        Ok(cfg)
    }

    pub fn check(&self) -> Result<()> {
        ensure!(self.spread_bps <= 10_000, "spread_bps above 10000");
        ensure!(
            self.settle_margin_secs < self.quote_ttl_secs,
            "settle_margin_secs must be below quote_ttl_secs"
        );
        // The quote protocol is plain text.
        ensure!(
            self.listen.ip().is_loopback(),
            "listen must be a loopback address"
        );
        if let Some(http) = &self.http {
            http.check()?;
        }
        if let Some(f) = &self.faucet {
            for t in &f.tokens {
                parse_address(&t.mint)?;
            }
        }
        if let Some(l) = &self.lend {
            ensure!(
                self.http.is_some(),
                "the lend keeper needs the http section"
            );
            l.check()?;
        }
        for l in &self.limits {
            parse_address(&l.mint)?;
        }
        ensure!(
            self.settle_tolerance_bps <= 10_000,
            "settle_tolerance_bps above 10000"
        );
        parse_address(&self.program_id)?;
        if let Some(p) = &self.pubkey {
            parse_address(p)?;
        }
        self.common.check()
    }
}

fn read(path: &str) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("read config {path}"))
}

/// serde's messages can echo the offending value, which may be a secret, so
/// only the error category and position are reported.
fn parse_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|e| {
        anyhow!(
            "invalid config ({:?} error at line {} column {})",
            e.classify(),
            e.line(),
            e.column()
        )
    })
}

impl Common {
    fn check(&self) -> Result<()> {
        for asset in &self.assets {
            parse_address(&asset.mint)?;
            if let PriceSource::Pyth { pyth } = &asset.price {
                ensure!(
                    self.pyth.is_some(),
                    "asset {} uses a Pyth price but no pyth section is set",
                    asset.mint
                );
                parse_address(&pyth.account)?;
                feed_id(&pyth.feed_id)?;
            }
        }
        self.network.check()
    }

    pub fn keypair(&self) -> Result<Keypair> {
        self.key.keypair()
    }

    pub fn mints(&self) -> Result<Vec<Address>> {
        self.assets.iter().map(|a| parse_address(&a.mint)).collect()
    }

    /// Fails unless every mint in `mints` is a configured asset.
    pub fn require(&self, mints: &[Address]) -> Result<()> {
        let configured = self.mints()?;
        for m in mints {
            ensure!(configured.contains(m), "no price configured for {m}");
        }
        Ok(())
    }

    pub fn pricer(&self) -> Result<Pricer> {
        let mut fixed = HashMap::new();
        let mut feeds = HashMap::new();
        for asset in &self.assets {
            let mint = parse_address(&asset.mint)?;
            match &asset.price {
                PriceSource::Fixed { price, expo } => {
                    fixed.insert(
                        mint,
                        Price {
                            price: *price,
                            expo: *expo,
                        },
                    );
                }
                PriceSource::Pyth { pyth } => {
                    feeds.insert(
                        mint,
                        (parse_address(&pyth.account)?, feed_id(&pyth.feed_id)?),
                    );
                }
            }
        }
        Ok(Pricer {
            fixed,
            feeds,
            pyth: self.pyth.clone(),
        })
    }
}

/// USD prices of the configured assets.
pub struct Pricer {
    fixed: HashMap<Address, Price>,
    /// Mint to (Pyth price account, feed id).
    feeds: HashMap<Address, (Address, [u8; 32])>,
    pyth: Option<Pyth>,
}

impl Pricer {
    /// The prices of `mints`, each of which must be configured; that is
    /// checked before any network call. Pyth prices come from their accounts
    /// and the Clock sysvar in one `getMultipleAccounts`, checked against
    /// chain time.
    pub fn prices(&self, rpc: &impl Rpc, mints: &[Address]) -> Result<Vec<Price>> {
        let mut wanted = Vec::new();
        for m in mints {
            if !self.fixed.contains_key(m) {
                let feed = self
                    .feeds
                    .get(m)
                    .ok_or_else(|| anyhow!("no price configured for {m}"))?;
                wanted.push((*m, *feed));
            }
        }
        let mut pyth = HashMap::new();
        if !wanted.is_empty() {
            let limits = self
                .pyth
                .as_ref()
                .ok_or_else(|| anyhow!("no pyth section"))?;
            let mut addrs = vec![parse_address(crate::chain::CLOCK_SYSVAR)?];
            addrs.extend(wanted.iter().map(|(_, (account, _))| *account));
            let accounts = rpc.get_multiple_accounts(addrs)?;
            ensure!(accounts.len() == wanted.len() + 1, "short account reply");
            let clock = accounts[0]
                .as_ref()
                .ok_or_else(|| anyhow!("no Clock sysvar"))?;
            let now = crate::chain::clock_time(&clock.data)?;
            for ((mint, (account, feed)), got) in wanted.iter().zip(&accounts[1..]) {
                let a = got
                    .as_ref()
                    .ok_or_else(|| anyhow!("Pyth account {account} not found"))?;
                let price = parse_pyth_account(
                    a.owner.as_array(),
                    &a.data,
                    feed,
                    now,
                    limits.max_age_secs,
                    limits.max_conf_bps,
                )
                .with_context(|| format!("Pyth account {account}"))?;
                pyth.insert(*mint, price);
            }
        }
        mints
            .iter()
            .map(|m| self.fixed.get(m).or_else(|| pyth.get(m)).copied())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("price missing"))
    }
}

pub fn parse_address(s: &str) -> Result<Address> {
    Address::from_str(s).map_err(|_| anyhow!("{s} is not a base58 address"))
}

fn feed_id(id: &str) -> Result<[u8; 32]> {
    let hex = id.strip_prefix("0x").unwrap_or(id);
    ensure!(
        hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "Pyth feed_id {id} is not 64 hex characters"
    );
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)?;
    }
    Ok(out)
}

/// The trimmed value of the environment variable `name`. An unset or empty
/// variable is an error that names the variable, never a value.
fn env_secret(name: &str) -> Result<String> {
    let value =
        std::env::var(name).map_err(|_| anyhow!("environment variable {name} is not set"))?;
    let value = value.trim();
    ensure!(!value.is_empty(), "environment variable {name} is empty");
    Ok(value.to_owned())
}

/// Reads a secret from the macOS Keychain. The name is passed as an argument,
/// never through a shell, and the secret is never logged.
pub fn keychain_secret(name: &str) -> Result<String> {
    let out = Command::new("security")
        .args(["find-generic-password", "-s", name, "-w"])
        .output()
        .context("run security")?;
    ensure!(out.status.success(), "no Keychain item named {name}");
    let secret =
        String::from_utf8(out.stdout).map_err(|_| anyhow!("Keychain item {name} is not UTF-8"))?;
    Ok(secret.trim_end_matches('\n').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mainnet SOL/USD.
    const PYTH_SOL_ACCOUNT: &str = "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE";
    const B: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";

    fn dealer_json(extra: &str) -> String {
        format!(
            r#"{{"network":{{"rpc_url":"http://127.0.0.1:8899","indexer_url":"http://127.0.0.1:8784","prover":"local"}},
            "key":{{"file":"k.json"}},"assets":[{{"mint":"11111111111111111111111111111111","price":{{"price":1,"expo":0}}}}],
            "listen":"127.0.0.1:47001","spread_bps":10,"program_id":"{id}",{extra}}}"#,
            id = swap_program::ID
        )
    }

    fn trader_json(band: u16, feed_id: &str) -> String {
        format!(
            r#"{{"network":{{"rpc_url":"http://127.0.0.1:8899","indexer_url":"http://127.0.0.1:8784","prover":"local"}},
            "key":{{"file":"k.json"}},"assets":[{{"mint":"11111111111111111111111111111111","price":{{"pyth":{{"account":"{PYTH_SOL_ACCOUNT}","feed_id":"{feed_id}"}}}}}}],
            "pyth":{{"max_age_secs":60,"max_conf_bps":100}},
            "dealers":[{{"address":"127.0.0.1:47001","pubkey":"11111111111111111111111111111111"}}],
            "band_bps":{band},"min_ttl_secs":5,"max_ttl_secs":60}}"#
        )
    }

    #[test]
    fn settle_margin_must_be_below_quote_ttl() {
        let ok = r#""quote_ttl_secs":30,"settle_margin_secs":29"#;
        let bad = r#""quote_ttl_secs":30,"settle_margin_secs":30"#;
        assert!(DealerConfig::from_json(&dealer_json(ok)).is_ok());
        assert!(DealerConfig::from_json(&dealer_json(bad)).is_err());
    }

    #[test]
    fn non_loopback_listen_is_rejected() {
        let cfg = dealer_json(r#""quote_ttl_secs":30,"settle_margin_secs":5"#)
            .replace("127.0.0.1:47001", "0.0.0.0:47001");
        assert!(DealerConfig::from_json(&cfg).is_err());
    }

    #[test]
    fn env_key_source_reads_one_trimmed_variable() {
        let src: KeySource = serde_json::from_str(r#"{"env":"KEELER_TEST_ENV_KEY"}"#).unwrap();
        assert!(!src.is_keychain());
        let unset = format!("{:#}", src.secret().err().unwrap());
        assert_eq!(unset, "environment variable KEELER_TEST_ENV_KEY is not set");
        std::env::set_var("KEELER_TEST_ENV_KEY", "  \n");
        let empty = format!("{:#}", src.secret().err().unwrap());
        assert_eq!(empty, "environment variable KEELER_TEST_ENV_KEY is empty");
        std::env::set_var("KEELER_TEST_ENV_KEY", " SECRET-value\n");
        assert_eq!(src.secret().unwrap(), "SECRET-value");
        std::env::set_var("KEELER_TEST_ENV_KEY", "SECRET-not-a-keypair");
        let err = format!("{:#}", src.keypair().err().unwrap());
        assert!(!err.contains("SECRET"), "{err}");
        std::env::remove_var("KEELER_TEST_ENV_KEY");
    }

    #[test]
    fn helius_key_source_takes_the_old_and_new_shapes() {
        let prover = |p: &str| serde_json::from_str::<Prover>(p).unwrap();
        assert!(matches!(
            prover(r#"{"helius":{"keychain":"k"}}"#),
            Prover::Helius(KeySource::Keychain(k)) if k == "k"
        ));
        assert!(matches!(
            prover(r#"{"helius":{"env":"HELIUS"}}"#),
            Prover::Helius(KeySource::Env(v)) if v == "HELIUS"
        ));
        assert!(matches!(prover(r#""local""#), Prover::Local));
        let net = |p: &str| Network {
            rpc_url: "https://devnet.helius-rpc.com".into(),
            indexer_url: "https://devnet.helius-rpc.com/v1/spr".into(),
            prover: prover(p),
            priority_fee: 0,
        };
        assert!(net(r#"{"helius":{"keychain":"k"}}"#).key_from_keychain());
        assert!(!net(r#"{"helius":{"env":"H"}}"#).key_from_keychain());
        assert!(!net(r#"{"helius":{"file":"h"}}"#).key_from_keychain());
        assert!(net(r#""local""#).key_from_keychain());
        std::env::set_var(
            "KEELER_TEST_HELIUS",
            "https://devnet.helius-rpc.com/?api-key=ab-12",
        );
        let (rpc, indexer) = net(r#"{"helius":{"env":"KEELER_TEST_HELIUS"}}"#)
            .urls()
            .unwrap();
        std::env::remove_var("KEELER_TEST_HELIUS");
        assert_eq!(rpc, "https://devnet.helius-rpc.com/?api-key=ab-12");
        assert_eq!(
            indexer,
            "https://devnet.helius-rpc.com/v1/spr?api-key=ab-12"
        );
    }

    #[test]
    fn allow_remote_opens_only_the_http_listener() {
        let http = |listen: &str, allow: &str| {
            dealer_json(&format!(
                r#""quote_ttl_secs":30,"settle_margin_secs":5,
                "http":{{"listen":"{listen}","key":{{"env":"K"}}{allow}}}"#
            ))
        };
        let open = DealerConfig::from_json(&http("0.0.0.0:47500", r#","allow_remote":true"#));
        assert!(open.unwrap().http.unwrap().allow_remote);
        assert!(DealerConfig::from_json(&http("0.0.0.0:47500", "")).is_err());
        assert!(
            DealerConfig::from_json(&http("0.0.0.0:47500", r#","allow_remote":false"#)).is_err()
        );
        let local = DealerConfig::from_json(&http("127.0.0.1:47500", "")).unwrap();
        assert!(!local.http.unwrap().allow_remote);
        let tcp_open = http("0.0.0.0:47500", r#","allow_remote":true"#)
            .replace("127.0.0.1:47001", "0.0.0.0:47001");
        assert!(
            DealerConfig::from_json(&tcp_open).is_err(),
            "the quote protocol stays on loopback"
        );
        let no_key = dealer_json(
            r#""quote_ttl_secs":30,"settle_margin_secs":5,
            "http":{"listen":"0.0.0.0:47500","allow_remote":true}"#,
        );
        assert!(DealerConfig::from_json(&no_key).is_err());
    }

    #[test]
    fn deployed_devnet_configs_parse() {
        let dealer: DealerConfig =
            parse_json(include_str!("../../deploy/devnet/dealer.json")).unwrap();
        let http = dealer.http.as_ref().unwrap();
        assert!(http.allow_remote);
        http.check().unwrap();
        dealer.common.check().unwrap();
        assert!(dealer.lend.unwrap().admin_key.is_none());
        let standby: crate::keeper::KeeperConfig =
            parse_json(include_str!("../../deploy/devnet/standby.json")).unwrap();
        assert!(standby.http.allow_remote);
        standby.http.check().unwrap();
        standby.network.check().unwrap();
        assert!(standby.lend.admin_key.is_none());
    }

    #[test]
    fn band_must_be_below_10000() {
        assert!(TraderConfig::from_json(&trader_json(9_999, B)).is_ok());
        assert!(TraderConfig::from_json(&trader_json(10_000, B)).is_err());
    }

    #[test]
    fn parse_error_does_not_echo_values() {
        let cfg = dealer_json(r#""quote_ttl_secs":30,"settle_margin_secs":5"#).replace(
            r#"{"file":"k.json"}"#,
            r#""SECRET-5Kd3NBUAdUnhyzenEwVLy9pBKxSwXvE9FMPyR4UKZvpe6E3AgLr""#,
        );
        let err = format!("{:#}", DealerConfig::from_json(&cfg).err().unwrap());
        assert!(!err.contains("SECRET"), "{err}");
        let cfg = dealer_json(r#""quote_ttl_secs":"SECRET-ttl","settle_margin_secs":5"#);
        let err = format!("{:#}", DealerConfig::from_json(&cfg).err().unwrap());
        assert!(!err.contains("SECRET"), "{err}");
    }

    #[test]
    fn dealer_http_and_faucet_sections_parse() {
        let cfg = dealer_json(
            r#""quote_ttl_secs":30,"settle_margin_secs":5,
            "http":{"listen":"127.0.0.1:47100","key":{"keychain":"k"}},
            "faucet":{"key":{"file":"f.json"},"tokens":[{"mint":"11111111111111111111111111111111","amount":5}],
              "lamports":20000000,"per_wallet_daily":1,"daily_cap":100}"#,
        );
        let cfg = DealerConfig::from_json(&cfg).unwrap();
        assert_eq!(cfg.faucet.unwrap().tokens[0].amount, 5);
        let open = dealer_json(
            r#""quote_ttl_secs":30,"settle_margin_secs":5,
            "http":{"listen":"0.0.0.0:47100","key":{"keychain":"k"}}"#,
        );
        assert!(DealerConfig::from_json(&open).is_err());
    }

    #[test]
    fn helius_network_takes_the_key_from_a_url_or_bare() {
        assert_eq!(
            helius_key("https://mainnet.helius-rpc.com/?api-key=ab-12").unwrap(),
            "ab-12"
        );
        assert_eq!(helius_key("ab-12").unwrap(), "ab-12");
        assert!(helius_key("https://mainnet.helius-rpc.com/").is_err());
        assert!(helius_key("a b").is_err());
        assert_eq!(
            with_api_key("https://devnet.helius-rpc.com/v1/spr", "ab-12").unwrap(),
            "https://devnet.helius-rpc.com/v1/spr?api-key=ab-12"
        );
        let net = |url: &str| Network {
            rpc_url: url.into(),
            indexer_url: "https://devnet.helius-rpc.com/v1/spr".into(),
            prover: Prover::Helius(KeySource::Keychain("k".into())),
            priority_fee: 0,
        };
        assert!(net("https://devnet.helius-rpc.com").check().is_ok());
        assert!(net("http://devnet.helius-rpc.com").check().is_err());
        assert!(net("https://devnet.helius-rpc.com.evil.example")
            .check()
            .is_err());
        let mut other = net("https://devnet.helius-rpc.com");
        other.indexer_url = "https://indexer.example/v1/spr".into();
        assert!(
            other.check().is_err(),
            "the key must not go to another host"
        );
        assert!(other.urls().is_err());
        assert!(net("https://devnet.helius-rpc.com/?api-key=x")
            .check()
            .is_err());
    }

    #[test]
    fn redact_hides_every_api_key() {
        assert_eq!(
            redact("GET https://h/?api-key=ab-12&x=1 and https://h/v1/spr?api-key=cd%2D3)"),
            "GET https://h/?api-key=REDACTED&x=1 and https://h/v1/spr?api-key=REDACTED)"
        );
        assert_eq!(redact("no key here"), "no key here");
    }

    #[test]
    fn pyth_feed_must_be_an_account_and_64_hex() {
        assert_eq!(
            feed_id(&format!("0x{}", B.to_uppercase())).unwrap()[0],
            0xef
        );
        assert!(feed_id(&B[1..]).is_err());
        let bad_account = trader_json(50, B).replace(PYTH_SOL_ACCOUNT, "not-base58");
        assert!(TraderConfig::from_json(&bad_account).is_err());
        assert!(TraderConfig::from_json(&trader_json(50, &B[..62])).is_err());
        let no_section =
            trader_json(50, B).replace(r#""pyth":{"max_age_secs":60,"max_conf_bps":100},"#, "");
        assert!(TraderConfig::from_json(&no_section).is_err());
    }

    #[test]
    fn receiver_program_constant_matches_its_address() {
        assert_eq!(
            parse_address("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ")
                .unwrap()
                .as_array(),
            &keeler_core::price::PYTH_RECEIVER
        );
    }

    /// Reads mainnet's SOL/USD account through Helius (key from the
    /// Keychain, never printed). Run by hand:
    /// `cargo test -p keeler --lib -- --ignored mainnet_pyth`.
    #[test]
    #[ignore]
    fn mainnet_pyth_sol_usd() {
        let net = Network {
            rpc_url: "https://mainnet.helius-rpc.com".into(),
            indexer_url: "https://mainnet.helius-rpc.com/v1/spr".into(),
            prover: Prover::Helius(KeySource::Keychain("keeler-helius-api-key".into())),
            priority_fee: 0,
        };
        let (rpc_url, _) = net.urls().unwrap();
        let common = Common {
            network: net,
            key: KeySource::File("unused.json".into()),
            assets: vec![Asset {
                mint: "11111111111111111111111111111111".into(),
                price: PriceSource::Pyth {
                    pyth: PythFeed {
                        account: PYTH_SOL_ACCOUNT.into(),
                        feed_id: B.into(),
                    },
                },
            }],
            pyth: Some(Pyth {
                max_age_secs: 120,
                max_conf_bps: 100,
            }),
        };
        let prices = common
            .pricer()
            .unwrap()
            .prices(
                &spr_client::SolanaRpc::new(rpc_url),
                &[parse_address("11111111111111111111111111111111").unwrap()],
            )
            .map_err(|e| redact(&format!("{e:#}")))
            .unwrap();
        let usd = prices[0].price as f64 * 10f64.powi(prices[0].expo);
        eprintln!("SOL/USD {usd}");
        assert!(usd > 1.0 && usd < 100_000.0);
    }
}
