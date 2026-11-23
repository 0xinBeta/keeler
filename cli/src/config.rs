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

#[derive(Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKey {
    Keychain(String),
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
        let cfg: Self = read_json(path)?;
        ensure!(cfg.band_bps <= 10_000, "band_bps above 10000");
        ensure!(
            cfg.min_ttl_secs <= cfg.max_ttl_secs,
            "min_ttl_secs above max_ttl_secs"
        );
        ensure!(!cfg.dealers.is_empty(), "no dealers configured");
        cfg.common.check()?;
        Ok(cfg)
    }
}

impl DealerConfig {
    pub fn load(path: &str) -> Result<Self> {
        let cfg: Self = read_json(path)?;
        ensure!(cfg.spread_bps <= 10_000, "spread_bps above 10000");
        cfg.common.check()?;
        Ok(cfg)
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read config {path}"))?;
    // serde's message names the field, not the value, so no secret leaks.
    serde_json::from_str(&text).with_context(|| format!("parse config {path}"))
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

    /// The USD price of every configured asset. Pyth prices are checked
    /// against wall time, as Hermes publishes in wall time.
    pub fn prices(&self) -> Result<HashMap<Address, Price>> {
        let mut prices = HashMap::new();
        let mut feeds = Vec::new();
        for asset in &self.assets {
            let mint = parse_address(&asset.mint)?;
            match &asset.price {
                PriceSource::Fixed { price, expo } => {
                    prices.insert(
                        mint,
                        Price {
                            price: *price,
                            expo: *expo,
                        },
                    );
                }
                PriceSource::Pyth { pyth } => feeds.push((mint, feed_id(pyth))),
            }
        }
        if feeds.is_empty() {
            return Ok(prices);
        }
        let pyth = self
            .pyth
            .as_ref()
            .ok_or_else(|| anyhow!("no pyth section"))?;
        let ids: Vec<String> = feeds.iter().map(|(_, id)| id.clone()).collect();
        let ApiKey::Keychain(name) = &pyth.api_key;
        let json = reqwest::blocking::Client::new()
            .get(hermes_url(&pyth.url, &ids))
            .bearer_auth(keychain_secret(name)?)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.text())
            .context("Hermes request")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        for (mint, price) in
            hermes_prices(&json, &feeds, now, pyth.max_age_secs, pyth.max_conf_bps)?
        {
            prices.insert(mint, price);
        }
        Ok(prices)
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
