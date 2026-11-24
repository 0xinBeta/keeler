//! The keeper's side of `docs/lend-api.md`: the session authorisation, the
//! per-request signature, freshness and replay, and the ticket bytes. Pure:
//! the caller supplies chain time.
use std::collections::{BTreeMap, HashMap};

use serde_json::Value;
use sha2::{Digest, Sha256};
use solana_address::Address;
use spr_keypair::PublicKey;

use crate::config::parse_address;

/// The longest a session may run past the keeper's chain time.
pub const SESSION_MAX_SECS: u64 = 86_400;
/// A request's timestamp may be this far from chain time, either side.
pub const FRESH_SECS: u64 = 120;
/// Nonces are remembered for twice the freshness window.
pub const NONCE_SECS: u64 = 2 * FRESH_SECS;
/// Remembered nonces at most; past this, requests are refused until old
/// ones expire (a flood cannot evict a live nonce).
const MAX_NONCES: usize = 100_000;

/// The fields every signed request carries.
pub const SIGNED_FIELDS: [&str; 7] = [
    "wallet",
    "sessionKey",
    "sessionExpiry",
    "sessionAuth",
    "nonce",
    "timestamp",
    "signature",
];

/// The request's fields as decimal or verbatim strings, from a JSON object
/// whose values are strings or unsigned integers. `None` when a key or value
/// breaks the canonical form.
pub fn fields_from_json(body: &[u8]) -> Option<BTreeMap<String, String>> {
    let Value::Object(map) = serde_json::from_slice::<Value>(body).ok()? else {
        return None;
    };
    let mut out = BTreeMap::new();
    for (k, v) in map {
        let v = match v {
            Value::String(s) => s,
            Value::Number(n) => n.as_u64()?.to_string(),
            _ => return None,
        };
        out.insert(k, v);
    }
    canonical_ok(&out).then_some(out)
}

/// The fields of a GET query. Values are base58, hex or digits, so any
/// percent or plus escape is refused rather than decoded.
pub fn fields_from_query(url: &str) -> Option<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let Some((_, query)) = url.split_once('?') else {
        return Some(out);
    };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=')?;
        if v.contains(['%', '+']) || out.insert(k.to_owned(), v.to_owned()).is_some() {
            return None;
        }
    }
    canonical_ok(&out).then_some(out)
}

fn canonical_ok(fields: &BTreeMap<String, String>) -> bool {
    fields.iter().all(|(k, v)| {
        !k.is_empty()
            && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !v.contains(['=', '\n'])
    })
}

/// Every field but `signature`, `nonce` and `timestamp`, sorted by key bytes
/// (a `BTreeMap` of `String` sorts so), each `key=value\n`.
pub fn canonical(fields: &BTreeMap<String, String>) -> String {
    fields
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "signature" | "nonce" | "timestamp"))
        .map(|(k, v)| format!("{k}={v}\n"))
        .collect()
}

pub fn request_bytes(
    method: &str,
    path: &str,
    nonce: &[u8; 16],
    timestamp: u64,
    canonical: &str,
) -> Vec<u8> {
    let mut m = format!("keeler-lend-request-v1\n{method}\n{path}\n").into_bytes();
    m.extend(nonce);
    m.extend(timestamp.to_le_bytes());
    m.extend(Sha256::digest(canonical.as_bytes()));
    m
}

/// Unix seconds as ISO 8601 UTC to the second.
pub fn iso8601(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

pub fn authorisation_text(
    network: &str,
    program: &Address,
    wallet: &Address,
    session: &Address,
    expiry: u64,
) -> String {
    format!(
        "Keeler Lend: allow this browser session\n\nThis lets this browser send lending requests for this wallet to the Keeler keeper until the time below, without asking you each time. It cannot move funds: every transaction still needs your wallet's approval.\n\nNetwork: {network}\nProgram: {program}\nWallet: {wallet}\nSession key: {session}\nExpires: {expiry} ({})",
        iso8601(expiry)
    )
}

pub fn hex_bytes<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != 2 * N || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decimal(s: &str) -> Option<u64> {
    (!s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn verify(key: &Address, message: &[u8], sig: &[u8; 64]) -> bool {
    PublicKey::from_ed25519(key.as_array()).verify_message(message, sig)
}

/// Nonces seen, by wallet, with the chain time they may be forgotten.
#[derive(Default)]
pub struct Nonces(HashMap<(Address, [u8; 16]), u64>);

/// Checks a signed request in the contract's order and returns its wallet,
/// or the reason (every failure is a 401).
pub fn authenticate(
    fields: &BTreeMap<String, String>,
    method: &str,
    path: &str,
    now: u64,
    network: &str,
    program: &Address,
    nonces: &mut Nonces,
) -> Result<Address, &'static str> {
    let get = |k: &str| {
        fields
            .get(k)
            .map(String::as_str)
            .ok_or("missing signed fields")
    };
    let wallet = parse_address(get("wallet")?).map_err(|_| "bad wallet")?;
    let session = parse_address(get("sessionKey")?).map_err(|_| "bad session key")?;
    let expiry = decimal(get("sessionExpiry")?).ok_or("bad session expiry")?;
    let auth = hex_bytes::<64>(get("sessionAuth")?).ok_or("bad session authorisation")?;
    let nonce = hex_bytes::<16>(get("nonce")?).ok_or("bad nonce")?;
    let timestamp = decimal(get("timestamp")?).ok_or("bad timestamp")?;
    let signature = hex_bytes::<64>(get("signature")?).ok_or("bad signature")?;
    if expiry <= now || expiry > now.saturating_add(SESSION_MAX_SECS) {
        return Err("the session has expired or runs too long");
    }
    let text = authorisation_text(network, program, &wallet, &session, expiry);
    if !verify(&wallet, text.as_bytes(), &auth) {
        return Err("the session is not authorised by this wallet");
    }
    let message = request_bytes(method, path, &nonce, timestamp, &canonical(fields));
    if !verify(&session, &message, &signature) {
        return Err("the request signature does not verify");
    }
    if timestamp.abs_diff(now) > FRESH_SECS {
        return Err("the request is stale");
    }
    // Kept through `until`: a request stamped `now + FRESH_SECS` is still
    // fresh at exactly `now + NONCE_SECS`.
    nonces.0.retain(|_, until| *until >= now);
    if nonces.0.contains_key(&(wallet, nonce)) {
        return Err("the request was replayed");
    }
    if nonces.0.len() >= MAX_NONCES {
        return Err("too many requests");
    }
    nonces.0.insert((wallet, nonce), now + NONCE_SECS);
    Ok(wallet)
}

/// `"keeler-lend-ticket-v1" ‖ program ‖ wallet ‖ id ‖ purpose ‖ target ‖
/// amount ‖ expiry`: 126 bytes.
pub fn ticket_bytes(
    program: &Address,
    wallet: &Address,
    id: &[u8; 16],
    purpose: u8,
    target: u64,
    amount: u64,
    expiry: u64,
) -> Vec<u8> {
    let mut m = b"keeler-lend-ticket-v1".to_vec();
    m.extend(program.as_array());
    m.extend(wallet.as_array());
    m.extend(id);
    m.push(purpose);
    m.extend(target.to_le_bytes());
    m.extend(amount.to_le_bytes());
    m.extend(expiry.to_le_bytes());
    m
}

pub fn ticket_signed_by(keeper: &Address, message: &[u8], sig: &[u8; 64]) -> bool {
    verify(keeper, message, sig)
}

#[cfg(test)]
mod tests {
    use solana_keypair::Keypair;
    use solana_signer::Signer;

    use super::*;

    fn fixture() -> Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../web/tests/fixtures/lend-requests.json"
        );
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn fields(v: &Value) -> BTreeMap<String, String> {
        fields_from_json(v.to_string().as_bytes()).unwrap()
    }

    struct Fx {
        v: Value,
        now: u64,
        network: String,
        program: Address,
    }

    fn fx() -> Fx {
        let v = fixture();
        Fx {
            now: v["now"].as_u64().unwrap(),
            network: v["network"].as_str().unwrap().into(),
            program: parse_address(v["program_id"].as_str().unwrap()).unwrap(),
            v,
        }
    }

    impl Fx {
        fn check(&self, i: usize, now: u64, nonces: &mut Nonces) -> Result<Address, &'static str> {
            let r = &self.v["requests"][i];
            authenticate(
                &fields(&r["fields"]),
                r["method"].as_str().unwrap(),
                r["path"].as_str().unwrap(),
                now,
                &self.network,
                &self.program,
                nonces,
            )
        }
    }

    #[test]
    fn the_page_fixture_verifies_byte_for_byte() {
        let f = fx();
        let s = &f.v["session"];
        let wallet = parse_address(f.v["wallet"]["address"].as_str().unwrap()).unwrap();
        let session = parse_address(s["key"].as_str().unwrap()).unwrap();
        let text = authorisation_text(
            &f.network,
            &f.program,
            &wallet,
            &session,
            s["expiry"].as_u64().unwrap(),
        );
        assert_eq!(text, s["authorisation_text"].as_str().unwrap());
        let mut nonces = Nonces::default();
        for (i, r) in f.v["requests"].as_array().unwrap().iter().enumerate() {
            let fl = fields(&r["fields"]);
            assert_eq!(canonical(&fl), r["canonical"].as_str().unwrap());
            let nonce = hex_bytes::<16>(&fl["nonce"]).unwrap();
            let bytes = request_bytes(
                r["method"].as_str().unwrap(),
                r["path"].as_str().unwrap(),
                &nonce,
                fl["timestamp"].parse().unwrap(),
                &canonical(&fl),
            );
            assert_eq!(to_hex(&bytes), r["signed_message_hex"].as_str().unwrap());
            assert_eq!(f.check(i, f.now, &mut nonces), Ok(wallet), "request {i}");
        }
        // The ticket the page verifies.
        let t = &f.v["ticket"];
        let keeper =
            Keypair::new_from_array(hex_bytes(t["keeper_seed_hex"].as_str().unwrap()).unwrap());
        assert_eq!(keeper.pubkey().to_string(), t["keeper"].as_str().unwrap());
        let tf = &t["fields"];
        let m = ticket_bytes(
            &parse_address(tf["programId"].as_str().unwrap()).unwrap(),
            &parse_address(tf["wallet"].as_str().unwrap()).unwrap(),
            &hex_bytes(tf["id"].as_str().unwrap()).unwrap(),
            tf["purpose"].as_u64().unwrap() as u8,
            tf["target"].as_str().unwrap().parse().unwrap(),
            tf["amount"].as_str().unwrap().parse().unwrap(),
            tf["expiry"].as_u64().unwrap(),
        );
        assert_eq!(m.len(), 126);
        assert_eq!(to_hex(&m), t["message_hex"].as_str().unwrap());
        let sig = keeper.sign_message(&m);
        assert_eq!(to_hex(sig.as_ref()), t["signature_hex"].as_str().unwrap());
        assert!(ticket_signed_by(
            &keeper.pubkey(),
            &m,
            sig.as_ref().try_into().unwrap()
        ));
    }

    #[test]
    fn the_query_form_verifies_too() {
        let f = fx();
        let r = &f.v["requests"][0];
        let q: Vec<String> = r["fields"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_str().map_or(v.to_string(), str::to_owned)))
            .collect();
        let got = fields_from_query(&format!("/lend/account?{}", q.join("&"))).unwrap();
        assert_eq!(got, fields(&r["fields"]));
        assert!(fields_from_query("/lend/account?a=1%3D").is_none());
        assert!(fields_from_query("/lend/account?a=1&a=2").is_none());
    }

    #[test]
    fn replay_stale_expired_and_forged_are_refused() {
        let f = fx();
        let mut nonces = Nonces::default();
        assert!(f.check(1, f.now, &mut nonces).is_ok());
        assert_eq!(
            f.check(1, f.now + 10, &mut nonces),
            Err("the request was replayed")
        );
        // Stale either side of the window.
        let mut fresh = Nonces::default();
        assert_eq!(
            f.check(1, f.now + FRESH_SECS + 1, &mut fresh),
            Err("the request is stale")
        );
        assert_eq!(
            f.check(1, f.now - FRESH_SECS - 1, &mut fresh),
            Err("the request is stale")
        );
        // Expired, or authorised for too long a session.
        let expiry = f.v["session"]["expiry"].as_u64().unwrap();
        assert!(f
            .check(1, expiry, &mut fresh)
            .unwrap_err()
            .contains("expired"));
        assert!(f
            .check(1, expiry - SESSION_MAX_SECS - 1, &mut fresh)
            .unwrap_err()
            .contains("expired"));
        // A changed field breaks the request signature; a changed expiry the
        // authorisation; another network's text too.
        let r = &f.v["requests"][1];
        let mut changed = fields(&r["fields"]);
        changed.insert("amount".into(), "250000001".into());
        let run = |fl: &BTreeMap<String, String>, network: &str| {
            authenticate(
                fl,
                "POST",
                "/lend/ticket",
                f.now,
                network,
                &f.program,
                &mut Nonces::default(),
            )
        };
        assert_eq!(
            run(&changed, "devnet"),
            Err("the request signature does not verify")
        );
        let mut later = fields(&r["fields"]);
        later.insert("sessionExpiry".into(), (expiry + 1).to_string());
        assert_eq!(
            run(&later, "devnet"),
            Err("the session is not authorised by this wallet")
        );
        assert_eq!(
            run(&fields(&r["fields"]), "mainnet"),
            Err("the session is not authorised by this wallet")
        );
        // Signed for another path.
        assert!(authenticate(
            &fields(&r["fields"]),
            "POST",
            "/lend/build",
            f.now,
            "devnet",
            &f.program,
            &mut Nonces::default()
        )
        .is_err());
        let mut missing = fields(&r["fields"]);
        missing.remove("nonce");
        assert_eq!(run(&missing, "devnet"), Err("missing signed fields"));
    }

    #[test]
    fn a_nonce_outlives_its_request() {
        let f = fx();
        let ts: u64 = fields(&f.v["requests"][1]["fields"])["timestamp"]
            .parse()
            .unwrap();
        let mut nonces = Nonces::default();
        assert!(f.check(1, ts - FRESH_SECS, &mut nonces).is_ok());
        assert_eq!(
            f.check(1, ts + FRESH_SECS, &mut nonces),
            Err("the request was replayed")
        );
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_800_086_000), "2027-01-16T07:53:20Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
    }
}
