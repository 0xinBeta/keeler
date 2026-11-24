//! The keeper without a chain: a fake [`Ledger`] that lands what it is sent
//! (or not), and wallets that sign requests the way the page does.
use std::collections::{HashMap, HashSet};

use borsh::BorshSerialize;
use keeler_lend_sdk::IntentState;

use super::*;

const T0: u64 = 1_900_000_000;
const USD: u64 = 1_000_000;
const SOL: u64 = 1_000_000_000;
const DOLLAR: u64 = 1_000_000_000;
const NET: &str = "localnet";

#[derive(Default)]
struct World {
    now: u64,
    registered: HashSet<Address>,
    spent: HashSet<[u8; 32]>,
    spenders: HashMap<[u8; 32], Vec<([u8; 32], u64)>>,
    intents: HashMap<Address, Vec<Intent>>,
    quote: Option<Quote>,
    /// Built transactions by their wire bytes.
    built: HashMap<Vec<u8>, Pending>,
    /// What was built and what was sent, in order.
    builds: Vec<Op>,
    sent: Vec<Op>,
    /// Sends land unless this is set.
    hold: bool,
    history: Vec<Note>,
    next: u64,
    /// Checked at each send: the state file must already hold the build.
    state: Option<PathBuf>,
}

#[derive(Default)]
struct Fake(Mutex<World>);

fn h(tag: &str, n: u64) -> String {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&n.to_le_bytes());
    b[8..8 + tag.len().min(24)].copy_from_slice(&tag.as_bytes()[..tag.len().min(24)]);
    to_hex(&b)
}

fn note(kind: u8, wallet: Option<&Address>, amount: u64, data: Vec<u8>, n: u64) -> Note {
    Note {
        kind,
        wallet: wallet.map(Address::to_string),
        owner: None,
        mint: if kind == KIND_POSITION {
            vault1().collateral_mint
        } else {
            vault1().debt_mint
        }
        .to_string(),
        asset_id: 0,
        amount,
        blinding: h("blinding", n),
        data: to_hex(&data),
        data_hash: h("data", n),
        hash: h("hash", n),
        nullifier: h("nullifier", n),
        leaf: Some(n),
    }
}

fn reserve_note(s: &ReserveState, idle: u64, n: u64) -> Note {
    let mut data = vec![7; 32];
    s.serialize(&mut data).unwrap();
    note(KIND_RESERVE, None, idle, data, n)
}

fn fresh_reserve(supply: u64, debt: u64) -> ReserveState {
    ReserveState {
        supply_shares: supply,
        debt_shares: debt,
        supply_price: 1_000_000_000_000,
        borrow_price: 1_000_000_000_000,
        rate_bps: 0,
        last_update: T0,
        salt: 0,
    }
}

fn position_note(w: &Address, id: u64, collateral: u64, debt_shares: u64, n: u64) -> Note {
    let s = PositionState {
        owner: Default::default(),
        vault: 1,
        position_id: id,
        debt_shares,
    };
    note(
        KIND_POSITION,
        Some(w),
        collateral,
        borsh::to_vec(&s).unwrap(),
        n,
    )
}

fn receipt_note(w: &Address, shares: u64, n: u64) -> Note {
    let s = ReceiptState {
        owner: Default::default(),
        pool: 1,
        supply_shares: shares,
    };
    note(KIND_RECEIPT, Some(w), 0, borsh::to_vec(&s).unwrap(), n)
}

impl Fake {
    fn w(&self) -> MutexGuard<'_, World> {
        self.0.lock().unwrap()
    }

    fn add_intent(&self, wallet: &Address, purpose: u8, target: u64, amount: u64, expiry: u64) {
        let mut w = self.w();
        w.next += 1;
        let n = 10_000 + w.next;
        let s = IntentState {
            maker: Default::default(),
            purpose,
            target,
            expiry,
        };
        let mut i = note(
            KIND_INTENT,
            Some(wallet),
            amount,
            borsh::to_vec(&s).unwrap(),
            n,
        );
        if purpose == purpose::DEPOSIT {
            i.mint = vault1().collateral_mint.to_string();
        }
        w.intents.entry(*wallet).or_default().push(Intent {
            note: i,
            purpose,
            target,
            expiry,
        });
    }

    /// Lands `p` as the chain would: its inputs spent, its outputs appended.
    fn land(&self, p: &Pending) {
        let mut w = self.w();
        w.next += 1;
        let leaf = 500 + w.next * 10;
        let outs: Vec<([u8; 32], u64)> = p
            .notes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.hash().unwrap(), leaf + i as u64))
            .collect();
        for i in &p.inputs {
            let n = hex32(i).unwrap();
            w.spent.insert(n);
            w.spenders.insert(n, outs.clone());
        }
        let wallet = parse_address(&p.wallet).unwrap();
        if let Some(list) = w.intents.get_mut(&wallet) {
            list.retain(|i| !p.inputs.contains(&i.note.nullifier));
        }
    }

    fn advance(&self, secs: u64) {
        self.w().now += secs;
    }
}

impl Ledger for Fake {
    fn now(&self) -> Result<u64> {
        Ok(self.w().now)
    }
    fn registered(&self, wallet: &Address) -> Result<bool> {
        Ok(self.w().registered.contains(wallet))
    }
    fn spent(&self, nullifiers: &[[u8; 32]]) -> Result<Vec<bool>> {
        let w = self.w();
        Ok(nullifiers.iter().map(|n| w.spent.contains(n)).collect())
    }
    fn spender(&self, nullifier: &[u8; 32]) -> Result<Option<Vec<([u8; 32], u64)>>> {
        Ok(self.w().spenders.get(nullifier).cloned())
    }
    fn intents(&self, wallet: &Address) -> Result<Vec<Intent>> {
        Ok(self.w().intents.get(wallet).cloned().unwrap_or_default())
    }
    fn quote(&self, _: u64) -> Result<Quote> {
        self.w().quote.ok_or_else(|| anyhow!("no price"))
    }
    fn build(&self, job: &Job, reserve: Option<&Note>, now: u64) -> Result<Built> {
        let mut w = self.w();
        w.next += 1;
        let n = w.next * 100;
        let r = || reserve.cloned().context("no reserve");
        let new_reserve = |idle: u64, supply: i64, debt: i64| -> Result<Note> {
            let mut s: ReserveState = r()?.state()?;
            s.supply_shares = s
                .supply_shares
                .checked_add_signed(supply)
                .context("shares")?;
            s.debt_shares = s.debt_shares.checked_add_signed(debt).context("shares")?;
            s.last_update = now;
            s.salt = u128::from(n);
            Ok(reserve_note(&s, idle, n + 1))
        };
        let (op, wallet, inputs, notes) = match job {
            Job::Supply { intent } => {
                let wallet = parse_address(intent.note.wallet.as_ref().unwrap())?;
                let res = r()?;
                (
                    Op::Supply,
                    wallet,
                    vec![res.nullifier.clone(), intent.note.nullifier.clone()],
                    vec![
                        new_reserve(
                            res.amount + intent.note.amount,
                            intent.note.amount as i64,
                            0,
                        )?,
                        receipt_note(&wallet, intent.note.amount, n + 2),
                    ],
                )
            }
            Job::DepositOpen { intent } => {
                let wallet = parse_address(intent.note.wallet.as_ref().unwrap())?;
                (
                    Op::DepositOpen,
                    wallet,
                    vec![intent.note.nullifier.clone()],
                    vec![position_note(
                        &wallet,
                        intent.target,
                        intent.note.amount,
                        0,
                        n + 2,
                    )],
                )
            }
            Job::DepositAdd { position, intent } => {
                let wallet = parse_address(position.wallet.as_ref().unwrap())?;
                let s: PositionState = position.state()?;
                (
                    Op::DepositAdd,
                    wallet,
                    vec![position.nullifier.clone(), intent.note.nullifier.clone()],
                    vec![position_note(
                        &wallet,
                        s.position_id,
                        position.amount + intent.note.amount,
                        s.debt_shares,
                        n + 2,
                    )],
                )
            }
            Job::Repay { position, .. } | Job::Liquidate { position, .. } => {
                let wallet = parse_address(position.wallet.as_ref().unwrap())?;
                let s: PositionState = position.state()?;
                let res = r()?;
                let (op, mut inputs) = match job {
                    Job::Repay { intent, .. } => (Op::Repay, vec![intent.note.nullifier.clone()]),
                    _ => (Op::Liquidate, vec![h("funds", n)]),
                };
                inputs.insert(0, position.nullifier.clone());
                inputs.insert(0, res.nullifier.clone());
                (
                    op,
                    wallet,
                    inputs,
                    vec![
                        new_reserve(res.amount, 0, -(s.debt_shares as i64))?,
                        position_note(&wallet, s.position_id, position.amount, 0, n + 2),
                    ],
                )
            }
            Job::Redeem { receipt, amount } => {
                let wallet = parse_address(receipt.wallet.as_ref().unwrap())?;
                let res = r()?;
                (
                    Op::Redeem,
                    wallet,
                    vec![res.nullifier.clone(), receipt.nullifier.clone()],
                    vec![
                        new_reserve(res.amount - amount, -(*amount as i64), 0)?,
                        receipt_note(&wallet, 0, n + 2),
                    ],
                )
            }
            Job::Draw {
                position,
                borrow,
                withdraw,
                ..
            } => {
                let wallet = parse_address(position.wallet.as_ref().unwrap())?;
                let s: PositionState = position.state()?;
                let res = r()?;
                (
                    Op::Draw,
                    wallet,
                    vec![res.nullifier.clone(), position.nullifier.clone()],
                    vec![
                        new_reserve(res.amount - borrow, 0, *borrow as i64)?,
                        position_note(
                            &wallet,
                            s.position_id,
                            position.amount - withdraw,
                            s.debt_shares + borrow,
                            n + 2,
                        ),
                    ],
                )
            }
        };
        let spends = (!matches!(op, Op::DepositOpen | Op::DepositAdd))
            .then(|| reserve.unwrap().nullifier.clone());
        let pending = Pending {
            op,
            wallet: wallet.to_string(),
            spends,
            inputs,
            outputs: notes.iter().map(|n| n.hash.clone()).collect(),
            notes,
            now,
            ticket: None,
            owner_signed: false,
            signature: None,
        };
        let tx = n.to_le_bytes().to_vec();
        w.built.insert(tx.clone(), pending.clone());
        w.builds.push(op);
        Ok(Built {
            pending,
            tx,
            index: None,
        })
    }
    fn send(&self, tx: &[u8]) -> Result<String> {
        let (p, hold, state) = {
            let mut w = self.w();
            let p = w.built.get(tx).cloned().context("unknown tx")?;
            w.sent.push(p.op);
            (p, w.hold, w.state.clone())
        };
        if let Some(path) = state {
            let book: Book = serde_json::from_slice(&std::fs::read(path)?)?;
            assert!(
                book.pending
                    .iter()
                    .any(|q| q.inputs == p.inputs && q.op == p.op),
                "sent before it was recorded"
            );
        }
        if !hold {
            self.land(&p);
        }
        Ok("sig".into())
    }
    fn history(&self) -> Result<Vec<Note>> {
        Ok(self.w().history.clone())
    }
}

fn lend_cfg(extra: &str) -> Lend {
    serde_json::from_str(&format!(
        r#"{{"program_id":"{}","network":"{NET}"{extra}}}"#,
        keeler_lend::ID
    ))
    .unwrap()
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("keeler-keeper-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct User {
    wallet: Keypair,
    session: Keypair,
    expiry: u64,
}

impl User {
    fn new(now: u64) -> Self {
        Self {
            wallet: Keypair::new(),
            session: Keypair::new(),
            expiry: now + 80_000,
        }
    }

    fn addr(&self) -> Address {
        self.wallet.pubkey()
    }

    fn signed(
        &self,
        method: &str,
        path: &str,
        extra: &[(&str, String)],
        ts: u64,
    ) -> BTreeMap<String, String> {
        let mut f = BTreeMap::new();
        f.insert("wallet".into(), self.addr().to_string());
        f.insert("sessionKey".into(), self.session.pubkey().to_string());
        f.insert("sessionExpiry".into(), self.expiry.to_string());
        let text = lend_auth::authorisation_text(
            NET,
            &keeler_lend::ID,
            &self.addr(),
            &self.session.pubkey(),
            self.expiry,
        );
        f.insert(
            "sessionAuth".into(),
            to_hex(self.wallet.sign_message(text.as_bytes()).as_ref()),
        );
        for (k, v) in extra {
            f.insert((*k).into(), v.clone());
        }
        let nonce: [u8; 16] = random().unwrap();
        let m = lend_auth::request_bytes(method, path, &nonce, ts, &lend_auth::canonical(&f));
        f.insert("nonce".into(), to_hex(&nonce));
        f.insert("timestamp".into(), ts.to_string());
        f.insert(
            "signature".into(),
            to_hex(self.session.sign_message(&m).as_ref()),
        );
        f
    }
}

fn body(f: &BTreeMap<String, String>) -> Vec<u8> {
    serde_json::to_vec(f).unwrap()
}

fn query(f: &BTreeMap<String, String>) -> String {
    let q: Vec<String> = f.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("/lend/account?{}", q.join("&"))
}

struct Setup {
    fake: Fake,
    cfg: Lend,
    signer: Keypair,
    state: PathBuf,
    dir: PathBuf,
}

impl Setup {
    fn new(name: &str, extra: &str) -> Self {
        let dir = temp(name);
        let fake = Fake::default();
        {
            let mut w = fake.w();
            w.now = T0;
            w.quote = Some(Quote {
                low: 10 * DOLLAR - 10_000_000,
                high: 10 * DOLLAR + 10_000_000,
                published: T0 as i64,
            });
            w.history = vec![reserve_note(&fresh_reserve(0, 0), 0, 1)];
        }
        let state = dir.join("state.json");
        let pause = dir.join("pause");
        Self {
            fake,
            cfg: lend_cfg(&format!(r#","pause_file":"{}"{extra}"#, pause.display())),
            signer: Keypair::new(),
            state,
            dir,
        }
    }

    fn keeper(&self) -> Keeper<'_, Fake> {
        Keeper::open(&self.cfg, &self.fake, &self.signer, self.state.clone()).unwrap()
    }

    fn user(&self) -> User {
        let u = User::new(T0);
        self.fake.w().registered.insert(u.addr());
        u
    }

    fn pause(&self, on: bool) {
        let p = self.dir.join("pause");
        if on {
            std::fs::write(p, b"").unwrap();
        } else {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn now_of(k: &Keeper<Fake>) -> u64 {
    k.ledger.now().unwrap()
}

fn ticket(k: &Keeper<Fake>, u: &User, extra: &[(&str, String)]) -> (u16, Value) {
    let f = u.signed("POST", "/lend/ticket", extra, now_of(k));
    k.http("POST", "/lend/ticket", &body(&f))
}

fn supply_ticket(k: &Keeper<Fake>, u: &User, amount: u64) -> (u16, Value) {
    ticket(
        k,
        u,
        &[
            ("purpose", "supply".into()),
            ("pool", vault1().debt_mint.to_string()),
            ("amount", amount.to_string()),
        ],
    )
}

fn deposit_ticket(k: &Keeper<Fake>, u: &User, position: u64, amount: u64) -> (u16, Value) {
    ticket(
        k,
        u,
        &[
            ("purpose", "deposit".into()),
            ("vault", "1".into()),
            ("position", position.to_string()),
            ("amount", amount.to_string()),
        ],
    )
}

fn repay_ticket(k: &Keeper<Fake>, u: &User, position: u64, amount: u64) -> (u16, Value) {
    ticket(
        k,
        u,
        &[
            ("purpose", "repay".into()),
            ("vault", "1".into()),
            ("position", position.to_string()),
            ("amount", amount.to_string()),
        ],
    )
}

fn build_req(k: &Keeper<Fake>, u: &User, extra: &[(&str, String)]) -> (u16, Value) {
    let f = u.signed("POST", "/lend/build", extra, now_of(k));
    k.http("POST", "/lend/build", &body(&f))
}

fn draw(k: &Keeper<Fake>, u: &User, position: u64, borrow: u64) -> (u16, Value) {
    build_req(
        k,
        u,
        &[
            ("op", "draw".into()),
            ("vault", "1".into()),
            ("position", position.to_string()),
            ("borrow", borrow.to_string()),
            ("withdraw", "0".into()),
        ],
    )
}

fn redeem(k: &Keeper<Fake>, u: &User, amount: u64) -> (u16, Value) {
    build_req(
        k,
        u,
        &[
            ("op", "redeem".into()),
            ("pool", vault1().debt_mint.to_string()),
            ("amount", amount.to_string()),
        ],
    )
}

/// Ticket and intent for `u`, as the page would make them.
fn fund_intent(
    s: &Setup,
    k: &Keeper<Fake>,
    u: &User,
    purpose: u8,
    target: u64,
    amount: u64,
) -> String {
    let (st, v) = match purpose {
        purpose::SUPPLY => supply_ticket(k, u, amount),
        purpose::DEPOSIT => deposit_ticket(k, u, target, amount),
        _ => repay_ticket(k, u, target, amount),
    };
    assert_eq!(st, 200, "{v}");
    s.fake.add_intent(
        &u.addr(),
        purpose,
        target,
        amount,
        v["expiry"].as_u64().unwrap(),
    );
    v["id"].as_str().unwrap().to_owned()
}

/// Runs passes until nothing is left to do.
fn drain(k: &Keeper<Fake>) -> Vec<Op> {
    let before = k.ledger.w().sent.len();
    for _ in 0..20 {
        k.step().unwrap();
    }
    k.ledger.w().sent[before..].to_vec()
}

/// A pool with 100 tUSD supplied by a lender.
fn with_liquidity(s: &Setup, k: &Keeper<Fake>) -> User {
    let lender = s.user();
    fund_intent(s, k, &lender, purpose::SUPPLY, 1, 100 * USD);
    assert_eq!(drain(k), vec![Op::Supply]);
    lender
}

#[test]
fn tickets_are_signed_saved_and_limited() {
    let s = Setup::new(
        "tickets",
        r#","limits":{"tickets_per_wallet_hour":3,"max_open_intents":5}"#,
    );
    let k = s.keeper();
    let u = s.user();
    // Unregistered wallets are told so.
    let stranger = User::new(T0);
    let (st, v) = supply_ticket(&k, &stranger, 2 * USD);
    assert_eq!(st, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("registry"));
    // Below the program's minimum.
    assert_eq!(supply_ticket(&k, &u, USD - 1).0, 422);
    // A good one: signed by the keeper over the contract's bytes, and saved.
    let (st, v) = supply_ticket(&k, &u, 2 * USD);
    assert_eq!(st, 200, "{v}");
    let id: [u8; 16] = hex_bytes(v["id"].as_str().unwrap()).unwrap();
    let expiry = v["expiry"].as_u64().unwrap();
    assert_eq!(expiry, T0 + 600);
    let m = lend_auth::ticket_bytes(
        &keeler_lend::ID,
        &u.addr(),
        &id,
        purpose::SUPPLY,
        1,
        2 * USD,
        expiry,
    );
    let sig: [u8; 64] = hex_bytes(v["signature"].as_str().unwrap()).unwrap();
    assert!(lend_auth::ticket_signed_by(&s.signer.pubkey(), &m, &sig));
    let saved = load_book(&s.state, &s.signer.pubkey(), &keeler_lend::ID).unwrap();
    assert_eq!(saved.tickets.len(), 1);
    // Per-wallet hourly limit: the 422 above did not count, so 2 more.
    assert_eq!(supply_ticket(&k, &u, 2 * USD).0, 200);
    assert_eq!(supply_ticket(&k, &u, 2 * USD).0, 200);
    assert_eq!(supply_ticket(&k, &u, 2 * USD).0, 429);
    s.fake.advance(HOUR);
    // Those three expired, so the open cap counts only new ones.
    for _ in 0..3 {
        assert_eq!(supply_ticket(&k, &u, 2 * USD).0, 200);
    }
    let other = s.user();
    assert_eq!(supply_ticket(&k, &other, 2 * USD).0, 200);
    assert_eq!(supply_ticket(&k, &other, 2 * USD).0, 200);
    let (st, v) = supply_ticket(&k, &other, 2 * USD);
    assert_eq!(st, 429, "max open: {v}");
    // Unknown pool, vault and purpose.
    let bad = ticket(
        &k,
        &other,
        &[
            ("purpose", "supply".into()),
            ("pool", Address::default().to_string()),
            ("amount", "5000000".into()),
        ],
    );
    assert_eq!(bad.0, 404);
    assert_eq!(
        ticket(
            &k,
            &other,
            &[("purpose", "borrow".into()), ("amount", "5".into())]
        )
        .0,
        400
    );
    assert_eq!(
        repay_ticket(&k, &other, 1, USD).0,
        404,
        "no position to repay"
    );
}

#[test]
fn signed_requests_refuse_replay_stale_expired_forged_and_unknown_fields() {
    let s = Setup::new("auth", "");
    let k = s.keeper();
    let u = s.user();
    let extra = [
        ("purpose", "supply".into()),
        ("pool", vault1().debt_mint.to_string()),
        ("amount", (2 * USD).to_string()),
    ];
    let f = u.signed("POST", "/lend/ticket", &extra, T0);
    assert_eq!(k.http("POST", "/lend/ticket", &body(&f)).0, 200);
    assert_eq!(
        k.http("POST", "/lend/ticket", &body(&f)).0,
        401,
        "replayed nonce"
    );
    let stale = u.signed("POST", "/lend/ticket", &extra, T0 - 121);
    assert_eq!(k.http("POST", "/lend/ticket", &body(&stale)).0, 401);
    // A request signed for another path does not verify here.
    let other_path = u.signed("POST", "/lend/build", &extra, T0);
    assert_eq!(k.http("POST", "/lend/ticket", &body(&other_path)).0, 401);
    let mut forged = u.signed("POST", "/lend/ticket", &extra, T0);
    forged.insert("amount".into(), (3 * USD).to_string());
    assert_eq!(k.http("POST", "/lend/ticket", &body(&forged)).0, 401);
    let mut expired = User::new(T0);
    expired.expiry = T0;
    s.fake.w().registered.insert(expired.addr());
    assert_eq!(
        k.http(
            "POST",
            "/lend/ticket",
            &body(&expired.signed("POST", "/lend/ticket", &extra, T0))
        )
        .0,
        401
    );
    // Another wallet's session authorisation does not carry over.
    let mut stolen = u.signed("POST", "/lend/ticket", &extra, T0);
    let thief = User::new(T0);
    stolen.insert("wallet".into(), thief.addr().to_string());
    assert_eq!(k.http("POST", "/lend/ticket", &body(&stolen)).0, 401);
    let mut extra2 = extra.to_vec();
    extra2.push(("memo", "x".into()));
    assert_eq!(
        k.http(
            "POST",
            "/lend/ticket",
            &body(&u.signed("POST", "/lend/ticket", &extra2, T0))
        )
        .0,
        400
    );
    assert_eq!(k.http("POST", "/lend/ticket", b"not json").0, 400);
    assert_eq!(k.http("GET", "/lend/nothing", b"").0, 404);
}

#[test]
fn the_queue_runs_liquidations_then_repays_and_deposits_then_supply() {
    let s = Setup::new("queue", "");
    let k = s.keeper();
    with_liquidity(&s, &k);
    let a = s.user();
    let b = s.user();
    let c = s.user();
    // a: a position that a price drop makes liquidatable; b: one to repay.
    fund_intent(&s, &k, &a, purpose::DEPOSIT, 1, SOL);
    fund_intent(&s, &k, &b, purpose::DEPOSIT, 1, SOL);
    assert_eq!(drain(&k), vec![Op::DepositOpen, Op::DepositOpen]);
    assert_eq!(draw(&k, &a, 1, 7 * USD).0, 200);
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.land(&p);
    s.fake.advance(1);
    assert_eq!(
        draw(&k, &b, 1, 2 * USD).0,
        409,
        "a's draw has not settled yet"
    );
    k.resolve().unwrap();
    // a's draw landed: the reservation ends with it.
    let (st, v) = draw(&k, &b, 1, 2 * USD);
    assert_eq!(st, 200, "{v}");
    let p = k
        .snapshot()
        .unwrap()
        .pending
        .iter()
        .find(|p| p.op == Op::Draw)
        .unwrap()
        .clone();
    s.fake.land(&p);
    k.resolve().unwrap();
    // Queue: supply (c), repay (b), and a's position under water.
    fund_intent(&s, &k, &c, purpose::SUPPLY, 1, 5 * USD);
    fund_intent(&s, &k, &b, purpose::REPAY, 1, USD);
    s.fake.w().quote = Some(Quote {
        low: 8 * DOLLAR - 10_000_000,
        high: 8 * DOLLAR + 10_000_000,
        published: now_of(&k) as i64,
    });
    assert_eq!(drain(&k), vec![Op::Liquidate, Op::Repay, Op::Supply]);
    let book = k.snapshot().unwrap();
    assert!(
        book.pending.is_empty()
            && book
                .tickets
                .iter()
                .all(|t| t.status == TicketStatus::Filled)
    );
}

#[test]
fn unticketed_dust_and_mismatched_intents_are_ignored() {
    let s = Setup::new("dust", "");
    let k = s.keeper();
    let u = s.user();
    // No ticket at all, and a flood of them.
    for i in 0..50 {
        s.fake
            .add_intent(&u.addr(), purpose::SUPPLY, 1, 1 + i, T0 + 600);
    }
    // A ticket whose intent differs in amount, and one in target.
    let (st, v) = supply_ticket(&k, &u, 2 * USD);
    assert_eq!(st, 200);
    let expiry = v["expiry"].as_u64().unwrap();
    s.fake
        .add_intent(&u.addr(), purpose::SUPPLY, 1, 2 * USD + 1, expiry);
    s.fake
        .add_intent(&u.addr(), purpose::SUPPLY, 2, 2 * USD, expiry);
    s.fake
        .add_intent(&u.addr(), purpose::SUPPLY, 1, 2 * USD, expiry + 1);
    assert!(drain(&k).is_empty());
    assert!(s.fake.w().builds.is_empty());
    // The matching one is filled, once.
    s.fake
        .add_intent(&u.addr(), purpose::SUPPLY, 1, 2 * USD, expiry);
    assert_eq!(drain(&k), vec![Op::Supply]);
    // Too close to its expiry: left for the maker to refund.
    let (_, v) = supply_ticket(&k, &u, 3 * USD);
    s.fake.add_intent(
        &u.addr(),
        purpose::SUPPLY,
        1,
        3 * USD,
        v["expiry"].as_u64().unwrap(),
    );
    s.fake.advance(600 - FILL_MARGIN);
    assert!(drain(&k).is_empty());
}

#[test]
fn pause_stops_new_money_only() {
    let s = Setup::new("pause", "");
    let k = s.keeper();
    with_liquidity(&s, &k);
    let u = s.user();
    let lender = s.user();
    fund_intent(&s, &k, &u, purpose::DEPOSIT, 1, SOL);
    fund_intent(&s, &k, &lender, purpose::SUPPLY, 1, 10 * USD);
    assert_eq!(drain(&k), vec![Op::DepositOpen, Op::Supply]);
    let (st, _) = draw(&k, &u, 1, 3 * USD);
    assert_eq!(st, 200);
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.land(&p);
    k.resolve().unwrap();
    // A supply ticket issued before the pause is not filled during it.
    let (_, v) = supply_ticket(&k, &lender, 4 * USD);
    s.fake.add_intent(
        &lender.addr(),
        purpose::SUPPLY,
        1,
        4 * USD,
        v["expiry"].as_u64().unwrap(),
    );
    s.pause(true);
    assert_eq!(supply_ticket(&k, &lender, 2 * USD).0, 403);
    assert_eq!(deposit_ticket(&k, &u, 2, SOL).0, 403, "a new position");
    assert_eq!(draw(&k, &u, 1, USD).0, 403, "borrowing");
    assert_eq!(drain(&k), vec![]);
    // Repay, add collateral, redeem and a healthy withdrawal still work.
    fund_intent(&s, &k, &u, purpose::REPAY, 1, USD);
    fund_intent(&s, &k, &u, purpose::DEPOSIT, 1, SOL / 10);
    assert_eq!(drain(&k), vec![Op::Repay, Op::DepositAdd]);
    let (st, v) = build_req(
        &k,
        &u,
        &[
            ("op", "draw".into()),
            ("vault", "1".into()),
            ("position", "1".into()),
            ("borrow", "0".into()),
            ("withdraw", (SOL / 10).to_string()),
        ],
    );
    assert_eq!(st, 200, "{v}");
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.land(&p);
    k.resolve().unwrap();
    s.fake.advance(21);
    let (st, v) = redeem(&k, &lender, USD);
    assert_eq!(st, 200, "{v}");
    s.pause(false);
}

#[test]
fn owner_builds_check_ownership_health_and_reserve_the_pool() {
    let s = Setup::new("owner", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let a = s.user();
    let b = s.user();
    fund_intent(&s, &k, &a, purpose::DEPOSIT, 1, SOL);
    assert_eq!(drain(&k), vec![Op::DepositOpen]);
    // b asks about a's note: there is none of b's.
    assert_eq!(draw(&k, &b, 1, USD).0, 404);
    assert_eq!(redeem(&k, &b, USD).0, 404);
    // Unhealthy (cap 0.8 x $10 x 1 SOL = 8 tUSD less the margin), too
    // little in the pool, below the minimum debt.
    assert_eq!(draw(&k, &a, 1, 8 * USD).0, 423);
    assert_eq!(draw(&k, &a, 1, USD / 2).0, 422);
    assert_eq!(redeem(&k, &lender, 101 * USD).0, 422);
    // Stale price.
    s.fake.w().quote.as_mut().unwrap().published = (T0 - 61) as i64;
    assert_eq!(draw(&k, &a, 1, USD).0, 412);
    s.fake.w().quote.as_mut().unwrap().published = T0 as i64;
    // A good draw: the reply carries what the page checks.
    let (st, v) = draw(&k, &a, 1, 5 * USD);
    assert_eq!(st, 200, "{v}");
    let pending = k.snapshot().unwrap().pending;
    assert_eq!(pending.len(), 1);
    assert!(pending[0].owner_signed);
    assert_eq!(v["outputs"], json!(pending[0].outputs));
    assert_eq!(v["now"], json!(T0));
    let bound: u64 = v["bound"].as_str().unwrap().parse().unwrap();
    assert_eq!(bound, (10 * DOLLAR - 10_000_000) / 10_000 * 9_950);
    assert_eq!(v["index"], json!("1000000000000"));
    assert!(!v["transaction"].as_str().unwrap().is_empty());
    // The pool is reserved for a: b waits, a may build again.
    assert_eq!(redeem(&k, &lender, USD).0, 409);
    assert_eq!(draw(&k, &a, 1, 4 * USD).0, 200);
    // Never sent: after the reservation, others proceed; after the
    // window, the build is dropped.
    s.fake.advance(20);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
    s.fake.advance(DEAD_AFTER + 1);
    k.resolve().unwrap();
    assert!(k.snapshot().unwrap().pending.is_empty());
    assert_eq!(redeem(&k, &lender, USD).0, 200);
}

#[test]
fn a_liquidation_preempts_a_reservation() {
    let s = Setup::new("preempt", "");
    let k = s.keeper();
    with_liquidity(&s, &k);
    let a = s.user();
    let b = s.user();
    fund_intent(&s, &k, &a, purpose::DEPOSIT, 1, SOL);
    fund_intent(&s, &k, &b, purpose::DEPOSIT, 1, SOL);
    drain(&k);
    assert_eq!(draw(&k, &a, 1, 7 * USD).0, 200);
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.land(&p);
    s.fake.advance(21);
    k.resolve().unwrap();
    // b reserves the pool, then a's position goes under.
    assert_eq!(redeem(&k, &b, USD).0, 404);
    assert_eq!(draw(&k, &b, 1, USD).0, 200);
    fund_intent(&s, &k, &b, purpose::DEPOSIT, 1, SOL);
    s.fake.w().quote = Some(Quote {
        low: 8 * DOLLAR,
        high: 8 * DOLLAR + 1,
        published: now_of(&k) as i64,
    });
    assert_eq!(
        drain(&k),
        vec![Op::Liquidate],
        "the deposit waits for b's reservation"
    );
    // b's build lost the reserve to the liquidation: it is dropped.
    let book = k.snapshot().unwrap();
    assert!(book.pending.is_empty(), "{:?}", book.pending);
}

#[test]
fn racing_builds_on_one_reserve_resolve_by_output_hash() {
    let s = Setup::new("race", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let a = s.user();
    fund_intent(&s, &k, &a, purpose::DEPOSIT, 1, SOL);
    drain(&k);
    // Two builds on the same reserve: a's draw, then (after a's reservation)
    // the lender's redeem. The redeem lands.
    assert_eq!(draw(&k, &a, 1, 2 * USD).0, 200);
    s.fake.advance(20);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
    let book = k.snapshot().unwrap();
    assert_eq!(book.pending.len(), 2);
    assert_eq!(book.pending[0].spends, book.pending[1].spends);
    let winner = book
        .pending
        .iter()
        .find(|p| p.op == Op::Redeem)
        .unwrap()
        .clone();
    s.fake.land(&winner);
    k.resolve().unwrap();
    let book = k.snapshot().unwrap();
    assert!(book.pending.is_empty());
    assert_eq!(book.reserve.as_ref().unwrap().hash, winner.notes[0].hash);
    assert_eq!(book.reserve.as_ref().unwrap().leaf.is_some(), true);
    let pos: PositionState = book.positions[0].state().unwrap();
    assert_eq!(pos.debt_shares, 0, "a's draw did not land");
    // A full redeem leaves an empty receipt out of the book... the fake's
    // new receipt has 0 shares.
    assert!(book
        .receipts
        .iter()
        .all(|r| r.state::<ReceiptState>().unwrap().supply_shares > 0));
}

#[test]
fn records_come_before_sends_and_a_failed_save_sends_nothing() {
    let s = Setup::new("records", "");
    let k = s.keeper();
    s.fake.w().state = Some(s.state.clone());
    with_liquidity(&s, &k);
    let u = s.user();
    fund_intent(&s, &k, &u, purpose::SUPPLY, 1, 3 * USD);
    // The state file cannot be written: the build is not sent.
    std::fs::create_dir_all(s.state.with_extension("tmp")).unwrap();
    let sent = s.fake.w().sent.len();
    let intent = s.fake.intents(&u.addr()).unwrap().remove(0);
    assert!(k.execute(Job::Supply { intent }, None).is_err());
    assert_eq!(s.fake.w().sent.len(), sent, "nothing is sent unrecorded");
    assert!(k.snapshot().unwrap().pending.is_empty());
    std::fs::remove_dir_all(s.state.with_extension("tmp")).unwrap();
    assert_eq!(drain(&k), vec![Op::Supply]);
}

#[test]
fn restarts_recover_from_the_file_and_the_chain() {
    let s = Setup::new("restart", "");
    {
        let k = s.keeper();
        let lender = with_liquidity(&s, &k);
        let u = s.user();
        fund_intent(&s, &k, &u, purpose::DEPOSIT, 1, SOL);
        drain(&k);
        // Killed after recording and sending, before landing was seen.
        s.fake.w().hold = true;
        fund_intent(&s, &k, &lender, purpose::SUPPLY, 1, 2 * USD);
        assert_eq!(drain(&k), vec![Op::Supply]);
        assert_eq!(k.snapshot().unwrap().pending.len(), 1);
    }
    // It landed while the keeper was down.
    let p = load_book(&s.state, &s.signer.pubkey(), &keeler_lend::ID)
        .unwrap()
        .pending[0]
        .clone();
    s.fake.land(&p);
    let k = s.keeper();
    let book = k.snapshot().unwrap();
    assert!(book.pending.is_empty());
    assert_eq!(book.reserve.as_ref().unwrap().hash, p.notes[0].hash);
    assert_eq!(book.receipts.len(), 2);
    assert_eq!(book.positions.len(), 1);
    drop(k);
    // Killed between recording and sending: the build never lands and is
    // dropped after its window; the ticket is filled on a retry.
    let mut book = load_book(&s.state, &s.signer.pubkey(), &keeler_lend::ID).unwrap();
    let mut ghost = p.clone();
    ghost.inputs = vec![book.reserve.as_ref().unwrap().nullifier.clone()];
    ghost.spends = ghost.inputs.first().cloned();
    ghost.notes[0].hash = h("ghost", 1);
    book.pending.push(ghost);
    // A ticket nobody signed is dropped on load.
    book.tickets.push(Ticket {
        signature: "00".repeat(64),
        ..book.tickets[0].clone()
    });
    write_private(&s.state, &serde_json::to_vec(&book).unwrap()).unwrap();
    let k = s.keeper();
    assert_eq!(k.snapshot().unwrap().pending.len(), 1);
    assert_eq!(k.snapshot().unwrap().tickets.len(), book.tickets.len() - 1);
    s.fake.w().hold = false;
    s.fake.advance(DEAD_AFTER + 1);
    k.resolve().unwrap();
    assert!(k.snapshot().unwrap().pending.is_empty());
    drop(k);
    // An empty state file: everything comes back from the chain's history
    // (the fake's: every note the keeper made), the spent ones left out.
    let all = {
        let w = s.fake.w();
        let mut notes: Vec<Note> = w.built.values().flat_map(|p| p.notes.clone()).collect();
        notes.extend(w.history.clone());
        notes
    };
    s.fake.w().history = all;
    std::fs::remove_file(&s.state).unwrap();
    let k = s.keeper();
    let fresh = k.snapshot().unwrap();
    assert_eq!(
        fresh.reserve.as_ref().map(|r| &r.hash),
        Some(&p.notes[0].hash)
    );
    assert_eq!(fresh.receipts.len(), 2);
    assert_eq!(fresh.positions.len(), 1);
    assert!(fresh.tickets.is_empty(), "tickets are not on chain");
    drop(k);
    // Two unspent reserves: refuse to guess.
    std::fs::remove_file(&s.state).unwrap();
    s.fake
        .w()
        .history
        .push(reserve_note(&fresh_reserve(0, 0), 0, 99_999));
    assert!(Keeper::open(&s.cfg, &s.fake, &s.signer, s.state.clone()).is_err());
}

#[test]
fn account_and_pools_views() {
    let s = Setup::new("views", r#","limits":{"bucket_secs":600}"#);
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let a = s.user();
    fund_intent(&s, &k, &a, purpose::DEPOSIT, 7, SOL);
    drain(&k);
    let (_, v) = repay_ticket(&k, &a, 7, USD);
    assert!(v["error"].is_string(), "no debt to repay");
    let f = a.signed("GET", "/lend/account", &[], now_of(&k));
    let (st, v) = k.http("GET", &query(&f), b"");
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["receipts"], json!([]));
    assert_eq!(v["positions"][0]["position"], json!("7"));
    assert_eq!(v["positions"][0]["collateral"], json!(SOL.to_string()));
    let m = B64
        .decode(v["positions"][0]["message"].as_str().unwrap())
        .unwrap();
    assert_eq!(&m[..8], &SOL.to_le_bytes());
    assert_eq!(v["intents"][0]["status"], json!("filled"));
    let f = lender.signed("GET", "/lend/account", &[], now_of(&k));
    let (_, v) = k.http("GET", &query(&f), b"");
    assert_eq!(v["positions"], json!([]));
    assert_eq!(v["receipts"][0]["shares"], json!((100 * USD).to_string()));
    // A ticket never used turns refundable after its expiry.
    let (_, t) = supply_ticket(&k, &lender, 2 * USD);
    s.fake.advance(601);
    let f = lender.signed("GET", "/lend/account", &[], now_of(&k));
    let (_, v) = k.http("GET", &query(&f), b"");
    let i = v["intents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == t["id"])
        .unwrap()
        .clone();
    assert_eq!(i["status"], json!("refundable"));
    // Pools: coarse and bucketed.
    let (st, v) = k.http("GET", "/lend/pools", b"");
    assert_eq!(st, 200);
    let p = &v["pools"][0];
    assert_eq!(p["size"], json!("100000000"));
    assert_eq!(p["at"].as_u64().unwrap() % 600, 0);
    let first = v.clone();
    s.fake.advance(1);
    assert_eq!(
        k.http("GET", "/lend/pools", b"").1,
        first,
        "same bucket, same figures"
    );
}

#[test]
fn coarse_keeps_two_significant_figures() {
    assert_eq!(coarse(0), 0);
    assert_eq!(coarse(99), 99);
    assert_eq!(coarse(123), 120);
    assert_eq!(coarse(125), 130);
    assert_eq!(coarse(9_949), 9_900);
    assert_eq!(coarse(9_950), 10_000);
    assert_eq!(coarse(123_456_789), 120_000_000);
    assert_eq!(coarse(u64::MAX), coarse(u64::MAX));
}

#[test]
fn config_limits_are_checked() {
    assert!(lend_cfg("").check().is_ok());
    assert!(lend_cfg(r#","limits":{"ticket_secs":1000}"#)
        .check()
        .is_err());
    assert!(lend_cfg(r#","limits":{"reservation_secs":1}"#)
        .check()
        .is_err());
    let mut wrong = lend_cfg("");
    wrong.program_id = Address::default().to_string();
    assert!(wrong.check().is_err());
    assert!(serde_json::from_str::<Lend>(r#"{"program_id":"x","network":"n","nope":1}"#).is_err());
    let mut main = lend_cfg("");
    main.network = "mainnet".into();
    assert!(main.check().is_err(), "the test price on mainnet");
    assert!(require_test_network(&main).is_err());
}
