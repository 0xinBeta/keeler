use std::cmp::Reverse;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::price::within_band;

/// Request for quote. Mints and keys are base58 strings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rfq {
    pub sell_mint: String,
    pub buy_mint: String,
    pub sell_amount: u64,
    pub trader: String,
}

/// `expiry` is chain time (unix seconds from the Clock sysvar). `signature`
/// is the dealer's Ed25519 signature over [`quote_message`], 128 hex
/// characters on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    pub buy_amount: u64,
    pub expiry: u64,
    #[serde(with = "hex64")]
    pub signature: [u8; 64],
}

pub const QUOTE_DOMAIN: &[u8; 15] = b"keeler-quote-v2";
pub const QUOTE_MESSAGE_LEN: usize = 168;

/// What a dealer signs: the domain, the network byte (0 local, 1 devnet,
/// 2 mainnet), then the program ID, sell mint, buy mint and trader (32 bytes
/// each), then the sell amount, buy amount and expiry (u64 little-endian
/// each).
#[allow(clippy::too_many_arguments)]
pub fn quote_message(
    network: u8,
    program_id: &[u8; 32],
    sell_mint: &[u8; 32],
    buy_mint: &[u8; 32],
    trader: &[u8; 32],
    sell_amount: u64,
    buy_amount: u64,
    expiry: u64,
) -> [u8; QUOTE_MESSAGE_LEN] {
    let mut m = [0u8; QUOTE_MESSAGE_LEN];
    let parts: [&[u8]; 9] = [
        QUOTE_DOMAIN,
        &[network],
        program_id,
        sell_mint,
        buy_mint,
        trader,
        &sell_amount.to_le_bytes(),
        &buy_amount.to_le_bytes(),
        &expiry.to_le_bytes(),
    ];
    let mut at = 0;
    for part in parts {
        m[at..at + part.len()].copy_from_slice(part);
        at += part.len();
    }
    debug_assert_eq!(at, QUOTE_MESSAGE_LEN);
    m
}

/// A 64-byte array as exactly 128 hex characters.
mod hex64 {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let text = String::deserialize(d)?;
        let bad = || D::Error::custom("signature must be 128 hex characters");
        if text.len() != 128 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        let mut out = [0u8; 64];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
        }
        Ok(out)
    }
}

/// `{"quote":{...}}` or `{"error":"..."}`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Reply {
    Quote(Quote),
    Error(String),
}

/// Longest line either side accepts, newline included.
const MAX_LINE: usize = 4096;
const SERVE_TIMEOUT: Duration = Duration::from_secs(2);
const ACCEPT_POLL: Duration = Duration::from_millis(20);

/// Asks every dealer in parallel. Result is in dealer order; `None` for any
/// dealer that failed, timed out, refused or sent something unreadable.
pub fn request_quotes(dealers: &[SocketAddr], rfq: &Rfq, timeout: Duration) -> Vec<Option<Quote>> {
    thread::scope(|s| {
        let asks: Vec<_> = dealers
            .iter()
            .map(|addr| s.spawn(move || ask(*addr, rfq, timeout).ok().flatten()))
            .collect();
        asks.into_iter().map(|h| h.join().ok().flatten()).collect()
    })
}

fn ask(addr: SocketAddr, rfq: &Rfq, timeout: Duration) -> io::Result<Option<Quote>> {
    let deadline = Instant::now() + timeout;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_write_timeout(Some(timeout))?;
    write_line(&mut stream, rfq)?;
    let line = read_line(&mut stream, deadline)?;
    Ok(match serde_json::from_slice(&line)? {
        Reply::Quote(q) => Some(q),
        Reply::Error(_) => None,
    })
}

/// Serves quote requests one connection at a time until `stop` is set.
/// Refuses a listener that is not on loopback: the protocol is plain text.
pub fn serve(
    listener: TcpListener,
    stop: &AtomicBool,
    mut handler: impl FnMut(Rfq) -> Result<Quote, String>,
) -> io::Result<()> {
    let local = listener.local_addr()?;
    if !local.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to serve quotes on non-loopback address {local}"),
        ));
    }
    listener.set_nonblocking(true)?;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            // A bad client only loses its own connection.
            Ok((stream, _)) => {
                let _ = handle(stream, &mut handler);
            }
            // WouldBlock, or a transient accept error: wait and poll again.
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
    }
    Ok(())
}

fn handle(
    mut stream: TcpStream,
    handler: &mut impl FnMut(Rfq) -> Result<Quote, String>,
) -> io::Result<()> {
    // Accepted sockets inherit non-blocking mode on some platforms.
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(SERVE_TIMEOUT))?;
    let line = read_line(&mut stream, Instant::now() + SERVE_TIMEOUT)?;
    let reply = match serde_json::from_slice::<Rfq>(&line) {
        Ok(rfq) => match handler(rfq) {
            Ok(q) => Reply::Quote(q),
            // Bounded so the reply line stays under MAX_LINE even fully escaped.
            Err(msg) => Reply::Error(msg.chars().take(500).collect()),
        },
        Err(_) => Reply::Error("bad request".into()),
    };
    write_line(&mut stream, &reply)
}

fn write_line(stream: &mut TcpStream, msg: &impl Serialize) -> io::Result<()> {
    let mut buf = serde_json::to_vec(msg)?;
    buf.push(b'\n');
    stream.write_all(&buf)
}

/// Reads one `\n`-terminated line of at most `MAX_LINE` bytes before
/// `deadline`, so a slow-dripping peer cannot hold the caller past it.
fn read_line(stream: &mut TcpStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_read_timeout(Some(left))?;
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        match buf.iter().position(|&b| b == b'\n') {
            Some(i) if i < MAX_LINE => {
                buf.truncate(i);
                return Ok(buf);
            }
            _ if buf.len() >= MAX_LINE => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"))
            }
            _ => {}
        }
    }
}

/// Index of the largest in-band quote whose time to live is within
/// `[min_ttl_secs, max_ttl_secs]`; lowest index wins a tie.
pub fn best_quote(
    quotes: &[Option<Quote>],
    fair: u64,
    band_bps: u16,
    now: u64,
    min_ttl_secs: u64,
    max_ttl_secs: u64,
) -> Option<usize> {
    quotes
        .iter()
        .enumerate()
        .filter_map(|(i, q)| Some((i, (*q)?)))
        .filter(|(_, q)| within_band(q.buy_amount, fair, band_bps))
        .filter(|(_, q)| {
            q.expiry > now && (min_ttl_secs..=max_ttl_secs).contains(&(q.expiry - now))
        })
        .max_by_key(|&(i, q)| (q.buy_amount, Reverse(i)))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    const T: Duration = Duration::from_millis(300);

    fn rfq() -> Rfq {
        Rfq {
            sell_mint: "So11111111111111111111111111111111111111112".into(),
            buy_mint: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
            sell_amount: 2_000_000_000,
            trader: "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM".into(),
        }
    }

    fn q(buy_amount: u64, expiry: u64) -> Option<Quote> {
        Some(Quote {
            buy_amount,
            expiry,
            signature: [0; 64],
        })
    }

    /// Runs `serve` on 127.0.0.1 with `handler` while `f` runs, then stops it.
    fn with_server(
        handler: impl FnMut(Rfq) -> Result<Quote, String> + Send,
        f: impl FnOnce(SocketAddr),
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = AtomicBool::new(false);
        thread::scope(|s| {
            let server = s.spawn(|| serve(listener, &stop, handler));
            f(addr);
            stop.store(true, Ordering::Relaxed);
            server.join().unwrap().unwrap();
        });
    }

    /// A raw dealer that reads one line, waits `delay`, then writes `reply` as is.
    fn raw_dealer(reply: Vec<u8>, delay: Duration) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut r = io::BufReader::new(&stream);
            r.read_line(&mut String::new()).unwrap();
            thread::sleep(delay);
            let _ = (&stream).write_all(&reply);
        });
        addr
    }

    fn closed_port() -> SocketAddr {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
    }

    fn send_raw(addr: SocketAddr, bytes: &[u8]) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let _ = s.write_all(bytes);
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    #[test]
    fn amounts_are_json_numbers() {
        let quote = Quote {
            buy_amount: u64::MAX,
            expiry: 1_763_900_030,
            signature: [0xab; 64],
        };
        let json = serde_json::to_string(&Reply::Quote(quote)).unwrap();
        assert_eq!(
            json,
            format!(
                r#"{{"quote":{{"buy_amount":18446744073709551615,"expiry":1763900030,"signature":"{}"}}}}"#,
                "ab".repeat(64)
            )
        );
        match serde_json::from_str(&json).unwrap() {
            Reply::Quote(back) => assert_eq!(back, quote),
            Reply::Error(_) => panic!(),
        }
        let r = rfq();
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""sell_amount":2000000000"#), "{json}");
        assert_eq!(serde_json::from_str::<Rfq>(&json).unwrap(), r);
        assert_eq!(
            serde_json::to_string(&Reply::Error("no".into())).unwrap(),
            r#"{"error":"no"}"#
        );
    }

    #[test]
    fn signature_must_be_128_hex_characters() {
        let parse = |sig: &str| {
            serde_json::from_str::<Quote>(&format!(
                r#"{{"buy_amount":1,"expiry":2,"signature":"{sig}"}}"#
            ))
        };
        assert_eq!(parse(&"0F".repeat(64)).unwrap().signature, [0x0f; 64]);
        // Owned input too, as from a serde_json::Value.
        let v = serde_json::json!({"buy_amount": 1, "expiry": 2, "signature": "ab".repeat(64)});
        assert_eq!(
            serde_json::from_value::<Quote>(v).unwrap().signature,
            [0xab; 64]
        );
        assert!(parse(&"0f".repeat(63)).is_err());
        assert!(parse(&"0g".repeat(64)).is_err());
        assert!(serde_json::from_str::<Quote>(r#"{"buy_amount":1,"expiry":2}"#).is_err());
    }

    #[test]
    fn quote_message_layout() {
        let m = quote_message(2, &[1; 32], &[2; 32], &[3; 32], &[4; 32], 5, 6, 7);
        assert_eq!(m.len(), 168);
        assert_eq!(&m[..15], b"keeler-quote-v2");
        assert_eq!(m[15], 2);
        assert_eq!(&m[16..48], &[1; 32]);
        assert_eq!(&m[112..144], &[4; 32]);
        assert_eq!(&m[144..152], &5u64.to_le_bytes());
        assert_eq!(&m[152..160], &6u64.to_le_bytes());
        assert_eq!(&m[160..168], &7u64.to_le_bytes());
        // One network's signature is never another's.
        assert_ne!(
            m,
            quote_message(1, &[1; 32], &[2; 32], &[3; 32], &[4; 32], 5, 6, 7)
        );
    }

    #[test]
    fn protocol_good_and_error_replies() {
        let mut seen = Vec::new();
        with_server(
            |r| {
                seen.push(r.clone());
                if r.sell_amount == 1 {
                    Err("too small".into())
                } else {
                    Ok(Quote {
                        buy_amount: 299_100_000,
                        expiry: 1_763_900_030,
                        signature: [0; 64],
                    })
                }
            },
            |addr| {
                let good = request_quotes(&[addr, closed_port()], &rfq(), T);
                assert_eq!(good, vec![q(299_100_000, 1_763_900_030), None]);
                let small = Rfq {
                    sell_amount: 1,
                    ..rfq()
                };
                assert_eq!(request_quotes(&[addr], &small, T), vec![None]);
                assert_eq!(send_raw(addr, b"{\"sell_mint\":\"a\",\"buy_mint\":\"b\",\"sell_amount\":1,\"trader\":\"c\"}\n"),
                    "{\"error\":\"too small\"}\n");
            },
        );
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0], rfq());
    }

    #[test]
    fn client_rejects_bad_dealers() {
        let quote = format!(
            r#"{{"quote":{{"buy_amount":1,"expiry":2,"signature":"{}"}}}}"#,
            "00".repeat(64)
        );
        let quote = quote.as_bytes();
        // Valid JSON padded past the cap, so only the cap can reject it.
        let mut oversized = quote.to_vec();
        oversized.extend(std::iter::repeat_n(b' ', MAX_LINE));
        oversized.push(b'\n');
        let dealers = [
            raw_dealer([quote, b"\n"].concat(), Duration::ZERO),
            raw_dealer([quote, b"\n"].concat(), Duration::from_secs(1)), // slow
            raw_dealer(oversized, Duration::ZERO),
            raw_dealer(b"hello\n".to_vec(), Duration::ZERO),
            raw_dealer(quote.to_vec(), Duration::ZERO), // closes without newline
            raw_dealer(br#"{"error":"busy"}"#.to_vec(), Duration::ZERO),
        ];
        let start = Instant::now();
        let got = request_quotes(&dealers, &rfq(), T);
        assert!(
            start.elapsed() < Duration::from_millis(900),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(got, vec![q(1, 2), None, None, None, None, None]);
    }

    #[test]
    fn server_survives_bad_clients() {
        with_server(
            |_| {
                Ok(Quote {
                    buy_amount: 7,
                    expiry: 9,
                    signature: [0; 64],
                })
            },
            |addr| {
                assert_eq!(send_raw(addr, b"nope\n"), "{\"error\":\"bad request\"}\n");
                // Oversized request: dropped without calling the handler.
                assert_eq!(send_raw(addr, &vec![b'x'; MAX_LINE + 10]), "");
                // A client that connects and closes.
                drop(TcpStream::connect(addr).unwrap());
                assert_eq!(request_quotes(&[addr], &rfq(), T), vec![q(7, 9)]);
            },
        );
    }

    #[test]
    fn server_refuses_non_loopback() {
        let listener = TcpListener::bind("0.0.0.0:0").unwrap();
        let err = serve(listener, &AtomicBool::new(false), |_| Err(String::new())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn stop_ends_serve() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stop = AtomicBool::new(false);
        thread::scope(|s| {
            let server = s.spawn(|| serve(listener, &stop, |_| Err(String::new())));
            thread::sleep(Duration::from_millis(50));
            assert!(!server.is_finished());
            let start = Instant::now();
            stop.store(true, Ordering::Relaxed);
            server.join().unwrap().unwrap();
            assert!(start.elapsed() < Duration::from_millis(500));
        });
    }

    const NOW: u64 = 1_000;

    #[test]
    fn best_picks_max_lowest_index_on_tie() {
        let quotes = [q(990, 1_030), q(995, 1_030), None, q(995, 1_030)];
        assert_eq!(best_quote(&quotes, 1_000, 100, NOW, 10, 60), Some(1));
    }

    #[test]
    fn best_empty_and_all_none() {
        assert_eq!(best_quote(&[], 1_000, 100, NOW, 10, 60), None);
        assert_eq!(best_quote(&[None, None], 1_000, 100, NOW, 10, 60), None);
    }

    #[test]
    fn best_ttl_bounds() {
        let at = |expiry| best_quote(&[q(1_000, expiry)], 1_000, 100, NOW, 10, 60);
        assert_eq!(at(NOW + 10), Some(0));
        assert_eq!(at(NOW + 9), None);
        assert_eq!(at(NOW + 60), Some(0));
        assert_eq!(at(NOW + 61), None);
        assert_eq!(at(NOW), None);
        assert_eq!(at(NOW - 1), None);
        // min_ttl 0 still rejects an expiry at or before now
        assert_eq!(best_quote(&[q(1_000, NOW)], 1_000, 100, NOW, 0, 60), None);
    }

    #[test]
    fn best_out_of_band_both_sides() {
        // 20% under and 2% over fair lose to a worse in-band quote.
        let quotes = [q(800, 1_030), q(1_020, 1_030), q(995, 1_030)];
        assert_eq!(best_quote(&quotes, 1_000, 100, NOW, 10, 60), Some(2));
        assert_eq!(best_quote(&quotes[..2], 1_000, 100, NOW, 10, 60), None);
    }
}
