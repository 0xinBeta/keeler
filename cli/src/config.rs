//! One JSON config per role. Neither the config nor any key read through it
//! is ever printed.
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, process::Command, str::FromStr};

use anyhow::{anyhow, bail, ensure, Context, Result};
use keeler_core::price::{parse_hermes, Price};
use serde::Deserialize;
use solana_address::Address;
use solana_keypair::Keypair;

/// Where a secret comes from. `file` is for local test keys only.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeySource {
    File(PathBuf),
    Keychain(String),
}

/// The Hermes API key, read from the Keychain item `keychain`.
#[derive(Clone, Deserialize)]
pub struct ApiKey {
    pub keychain: String,
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum PriceSource {
    Fixed { price: i64, expo: i32 },
    Pyth { pyth: String },
}

#[derive(Clone, Deserialize)]
pub struct Asset {
    pub mint: String,
    pub price: PriceSource,
}

#[derive(Clone, Deserialize)]
pub struct Pyth {
    pub url: String,
    pub api_key: ApiKey,
    pub max_age_secs: i64,
    pub max_conf_bps: u16,
}

/// What both roles need to reach the chain and price assets.
#[derive(Clone, Deserialize)]
pub struct Common {
    pub rpc_url: String,
    pub indexer_url: String,
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
            if matches!(asset.price, PriceSource::Pyth { .. }) && self.pyth.is_none() {
                bail!(
                    "asset {} uses a Pyth price but no pyth section is set",
                    asset.mint
                );
            }
        }
        if let Some(pyth) = &self.pyth {
            ensure!(pyth.url.starts_with("https://"), "pyth.url must be https");
        }
        Ok(())
    }

    pub fn keypair(&self) -> Result<Keypair> {
        let json = match &self.key {
            KeySource::File(path) => std::fs::read_to_string(path)
                .with_context(|| format!("read key file {}", path.display()))?,
            KeySource::Keychain(name) => keychain_secret(name)?,
        };
        let bytes: Vec<u8> =
            serde_json::from_str(&json).map_err(|_| anyhow!("key is not a keypair JSON array"))?;
        Keypair::try_from(bytes.as_slice()).map_err(|_| anyhow!("key is not a valid keypair"))
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

    /// The price source, with the Hermes key and HTTP client set up once.
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
                    feeds.insert(mint, feed_id(pyth));
                }
            }
        }
        let hermes = match (&self.pyth, feeds.is_empty()) {
            (Some(pyth), false) => Some(Hermes {
                pyth: pyth.clone(),
                key: keychain_secret(&pyth.api_key.keychain)?,
                http: reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(5))
                    .build()?,
            }),
            _ => None,
        };
        Ok(Pricer {
            fixed,
            feeds,
            hermes,
        })
    }
}

struct Hermes {
    pyth: Pyth,
    key: String,
    http: reqwest::blocking::Client,
}

/// USD prices of the configured assets.
pub struct Pricer {
    fixed: HashMap<Address, Price>,
    feeds: HashMap<Address, String>,
    hermes: Option<Hermes>,
}

impl Pricer {
    /// The prices of `mints`, each of which must be configured; that is
    /// checked before any network call. Pyth prices are checked against wall
    /// time, as Hermes publishes in wall time.
    pub fn prices(&self, mints: &[Address]) -> Result<Vec<Price>> {
        let mut wanted = Vec::new();
        for m in mints {
            if !self.fixed.contains_key(m) {
                let id = self
                    .feeds
                    .get(m)
                    .ok_or_else(|| anyhow!("no price configured for {m}"))?;
                wanted.push((*m, id.clone()));
            }
        }
        let mut pyth = HashMap::new();
        if !wanted.is_empty() {
            let h = self
                .hermes
                .as_ref()
                .ok_or_else(|| anyhow!("no pyth section"))?;
            let ids: Vec<String> = wanted.iter().map(|(_, id)| id.clone()).collect();
            let json = h
                .http
                .get(hermes_url(&h.pyth.url, &ids))
                .bearer_auth(&h.key)
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.text())
                .context("Hermes request")?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as i64;
            pyth.extend(hermes_prices(
                &json,
                &wanted,
                now,
                h.pyth.max_age_secs,
                h.pyth.max_conf_bps,
            )?);
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

fn feed_id(id: &str) -> String {
    id.trim_start_matches("0x").to_ascii_lowercase()
}

fn hermes_url(base: &str, ids: &[String]) -> String {
    let query: Vec<String> = ids.iter().map(|id| format!("ids[]={id}")).collect();
    format!(
        "{}/v2/updates/price/latest?{}&parsed=true",
        base.trim_end_matches('/'),
        query.join("&")
    )
}

/// Maps a Hermes reply to `feeds`. Every requested feed must be present, and
/// no feed may be published more than `max_age_secs` in the future, which
/// `parse_hermes` treats as fresh.
fn hermes_prices(
    json: &str,
    feeds: &[(Address, String)],
    now: i64,
    max_age_secs: i64,
    max_conf_bps: u16,
) -> Result<Vec<(Address, Price)>> {
    let reply: serde_json::Value = serde_json::from_str(json).context("bad Hermes reply")?;
    for feed in reply["parsed"].as_array().into_iter().flatten() {
        let published = feed["price"]["publish_time"].as_i64().unwrap_or(i64::MIN);
        ensure!(
            published <= now.saturating_add(max_age_secs),
            "Pyth feed {} published in the future",
            feed["id"]
        );
    }
    let parsed: HashMap<String, Price> = parse_hermes(json, now, max_age_secs, max_conf_bps)?
        .into_iter()
        .collect();
    feeds
        .iter()
        .map(|(mint, id)| {
            parsed
                .get(id)
                .map(|p| (*mint, *p))
                .ok_or_else(|| anyhow!("Pyth feed {id} missing from the Hermes reply"))
        })
        .collect()
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

    const A: &str = "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43";
    const B: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";

    fn reply(feeds: &[(&str, i64)]) -> String {
        let parsed: Vec<_> = feeds
            .iter()
            .map(|(id, t)| {
                serde_json::json!({"id": id, "price": {"price": "100", "conf": "1", "expo": 0, "publish_time": t}})
            })
            .collect();
        serde_json::json!({ "parsed": parsed }).to_string()
    }

    fn feeds() -> Vec<(Address, String)> {
        vec![
            (
                Address::new_from_array([1; 32]),
                feed_id(&format!("0x{}", A.to_uppercase())),
            ),
            (Address::new_from_array([2; 32]), feed_id(B)),
        ]
    }

    fn dealer_json(extra: &str) -> String {
        format!(
            r#"{{"rpc_url":"http://127.0.0.1:8899","indexer_url":"http://127.0.0.1:8784",
            "key":{{"file":"k.json"}},"assets":[{{"mint":"11111111111111111111111111111111","price":{{"price":1,"expo":0}}}}],
            "listen":"127.0.0.1:47001","spread_bps":10,{extra}}}"#
        )
    }

    fn trader_json(band: u16, pyth_url: &str) -> String {
        format!(
            r#"{{"rpc_url":"http://127.0.0.1:8899","indexer_url":"http://127.0.0.1:8784",
            "key":{{"file":"k.json"}},"assets":[{{"mint":"11111111111111111111111111111111","price":{{"pyth":"aa"}}}}],
            "pyth":{{"url":"{pyth_url}","api_key":{{"keychain":"x"}},"max_age_secs":60,"max_conf_bps":100}},
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
    fn band_must_be_below_10000() {
        assert!(TraderConfig::from_json(&trader_json(9_999, "https://h.example")).is_ok());
        assert!(TraderConfig::from_json(&trader_json(10_000, "https://h.example")).is_err());
    }

    #[test]
    fn pyth_url_must_be_https() {
        assert!(TraderConfig::from_json(&trader_json(50, "http://h.example")).is_err());
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
    fn url_lists_every_feed() {
        let ids = vec!["aa".to_string(), "bb".to_string()];
        assert_eq!(
            hermes_url("https://hermes.example/", &ids),
            "https://hermes.example/v2/updates/price/latest?ids[]=aa&ids[]=bb&parsed=true"
        );
    }

    #[test]
    fn every_feed_present_maps_to_its_mint() {
        let got = hermes_prices(&reply(&[(A, 1000), (B, 1000)]), &feeds(), 1000, 60, 100).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, Address::new_from_array([1; 32]));
    }

    #[test]
    fn missing_feed_is_an_error() {
        let err = hermes_prices(&reply(&[(A, 1000)]), &feeds(), 1000, 60, 100).unwrap_err();
        assert!(err.to_string().contains(B), "{err}");
    }

    #[test]
    fn future_publish_time_beyond_max_age_is_an_error() {
        assert!(hermes_prices(&reply(&[(A, 1060), (B, 1000)]), &feeds(), 1000, 60, 100).is_ok());
        assert!(hermes_prices(&reply(&[(A, 1061), (B, 1000)]), &feeds(), 1000, 60, 100).is_err());
    }
}
