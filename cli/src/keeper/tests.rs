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
const ORIGIN: &str = "http://127.0.0.1:5300";

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
    /// What other keepers paid for, by keeper.
    peer_history: HashMap<Address, Vec<Note>>,
    next: u64,
    /// Checked at each send: the state file must already hold the build.
    state: Option<PathBuf>,
    /// Test prices posted: (vault, price, confidence).
    posted: Vec<(u64, u64, u64)>,
    /// Builds of these ops fail (no proof, no funds).
    fail: Vec<Op>,
    /// Sends are refused for good.
    reject: bool,
    /// Lands just before the next send, which it beats to an input: that
    /// send is refused.
    race: Option<Pending>,
    /// The `after` of each history and intents read.
    history_after: Vec<Option<String>>,
    intents_after: Vec<Option<String>>,
    /// The keeper's own debt token, and how often it was read.
    held: u64,
    held_reads: u32,
    /// History reads of peers fail.
    peer_history_fails: bool,
    /// This keeper's own history read past its mark ("mark") is empty.
    history_honours_mark: bool,
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
    fn intents(
        &self,
        wallet: &Address,
        after: Option<&str>,
        _: usize,
    ) -> Result<(Vec<Intent>, Option<String>)> {
        let mut w = self.w();
        w.intents_after.push(after.map(str::to_owned));
        let all = w.intents.get(wallet).cloned().unwrap_or_default();
        Ok((all, Some("cursor".into())))
    }
    fn quote(&self, _: u64) -> Result<Quote> {
        self.w().quote.ok_or_else(|| anyhow!("no price"))
    }
    fn set_price(&self, vault: u64, price: u64, confidence: u64) -> Result<String> {
        let mut w = self.w();
        w.posted.push((vault, price, confidence));
        w.quote = Some(Quote {
            low: price.saturating_sub(confidence),
            high: price + confidence,
            published: w.now as i64,
        });
        Ok("price".into())
    }
    fn build(&self, job: &Job, reserve: Option<&Note>, now: u64) -> Result<Built> {
        let mut w = self.w();
        if w.fail.contains(&job.op()) {
            bail!("the fake refuses to build a {:?}", job.op());
        }
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
                        receipt_note(
                            &wallet,
                            receipt.state::<ReceiptState>()?.supply_shares - amount,
                            n + 2,
                        ),
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
        let race = self.w().race.take();
        if let Some(winner) = race {
            self.land(&winner);
            return Err(anyhow::Error::new(Rejected("input spent".into())));
        }
        if self.w().reject {
            return Err(anyhow::Error::new(Rejected("preflight".into())));
        }
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
    fn debt_held(&self) -> Result<u64> {
        let mut w = self.w();
        w.held_reads += 1;
        Ok(w.held)
    }
    fn history(&self, payer: &Address, after: Option<&str>) -> Result<(Vec<Note>, Option<String>)> {
        let mut w = self.w();
        if let Some(list) = w.peer_history.get(payer) {
            if w.peer_history_fails {
                bail!("the RPC does not have a peer's transaction");
            }
            // A peer's mark moves with each note it adds.
            let mark = format!("peer-{}", list.len());
            let new = if after == Some(mark.as_str()) {
                Vec::new()
            } else {
                list.clone()
            };
            return Ok((new, Some(mark)));
        }
        w.history_after.push(after.map(str::to_owned));
        if w.history_honours_mark && after == Some("mark") {
            return Ok((Vec::new(), Some("mark".into())));
        }
        Ok((w.history.clone(), Some("mark".into())))
    }
}

fn lend_cfg(extra: &str) -> Lend {
    serde_json::from_str(&format!(
        r#"{{"program_id":"{}","network":"{NET}","origins":["{ORIGIN}"]{extra}}}"#,
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
        f.insert("origin".into(), ORIGIN.into());
        let text = lend_auth::authorisation_text(
            NET,
            ORIGIN,
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

/// An account read, as the page posts it, with the tickets it holds.
fn account_with(k: &Keeper<Fake>, u: &User, tickets: Value) -> (u16, Value) {
    let f = u.signed("POST", "/lend/account", &[], now_of(k));
    let mut b = serde_json::to_value(&f).unwrap();
    b["tickets"] = tickets;
    k.http(
        "POST",
        "/lend/account",
        &serde_json::to_vec(&b).unwrap(),
        false,
    )
}

fn account(k: &Keeper<Fake>, u: &User) -> (u16, Value) {
    account_with(k, u, json!([]))
}

struct Setup {
    fake: Fake,
    cfg: Lend,
    signer: Keypair,
    state: PathBuf,
    dir: PathBuf,
    /// The other keepers of the set.
    set: Vec<Address>,
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
            set: Vec::new(),
        }
    }

    fn keeper(&self) -> Keeper<'_, Fake> {
        Keeper::open_in(
            &self.cfg,
            &self.fake,
            &self.signer,
            self.state.clone(),
            &self.set,
        )
        .unwrap()
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
    k.http("POST", "/lend/ticket", &body(&f), false)
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
    k.http("POST", "/lend/build", &body(&f), false)
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
        r#","limits":{"tickets_per_wallet_hour":3,"open_per_wallet":2}"#,
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
    let saved = load_book(&s.state, &[s.signer.pubkey()], &keeler_lend::ID).unwrap();
    assert_eq!(saved.tickets.len(), 1);
    // Two open at a time per wallet, three an hour; the 422 above did not
    // count.
    assert_eq!(supply_ticket(&k, &u, 2 * USD).0, 200);
    let (st, v) = supply_ticket(&k, &u, 2 * USD);
    assert_eq!(st, 429, "open per wallet: {v}");
    s.fake.advance(601);
    assert_eq!(supply_ticket(&k, &u, 2 * USD).0, 200);
    let (st, v) = supply_ticket(&k, &u, 2 * USD);
    assert_eq!(st, 429, "per hour: {v}");
    s.fake.advance(HOUR);
    assert_eq!(supply_ticket(&k, &u, 2 * USD).0, 200);
    let other = s.user();
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
    assert_eq!(k.http("POST", "/lend/ticket", &body(&f), false).0, 200);
    assert_eq!(
        k.http("POST", "/lend/ticket", &body(&f), false).0,
        401,
        "replayed nonce"
    );
    let stale = u.signed("POST", "/lend/ticket", &extra, T0 - 121);
    assert_eq!(k.http("POST", "/lend/ticket", &body(&stale), false).0, 401);
    // A request signed for another path does not verify here.
    let other_path = u.signed("POST", "/lend/build", &extra, T0);
    assert_eq!(
        k.http("POST", "/lend/ticket", &body(&other_path), false).0,
        401
    );
    let mut forged = u.signed("POST", "/lend/ticket", &extra, T0);
    forged.insert("amount".into(), (3 * USD).to_string());
    assert_eq!(k.http("POST", "/lend/ticket", &body(&forged), false).0, 401);
    let mut expired = User::new(T0);
    expired.expiry = T0;
    s.fake.w().registered.insert(expired.addr());
    assert_eq!(
        k.http(
            "POST",
            "/lend/ticket",
            &body(&expired.signed("POST", "/lend/ticket", &extra, T0)),
            false
        )
        .0,
        401
    );
    // Another wallet's session authorisation does not carry over.
    let mut stolen = u.signed("POST", "/lend/ticket", &extra, T0);
    let thief = User::new(T0);
    stolen.insert("wallet".into(), thief.addr().to_string());
    assert_eq!(k.http("POST", "/lend/ticket", &body(&stolen), false).0, 401);
    let mut extra2 = extra.to_vec();
    extra2.push(("memo", "x".into()));
    assert_eq!(
        k.http(
            "POST",
            "/lend/ticket",
            &body(&u.signed("POST", "/lend/ticket", &extra2, T0)),
            false
        )
        .0,
        400
    );
    assert_eq!(k.http("POST", "/lend/ticket", b"not json", false).0, 400);
    assert_eq!(k.http("GET", "/lend/nothing", b"", false).0, 404);
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
    // Stale price: draws allow 65 s.
    s.fake.w().quote.as_mut().unwrap().published = (T0 - 66) as i64;
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
        vec![Op::Liquidate, Op::DepositAdd],
        "b's build lost the reserve to the liquidation: its reservation goes"
    );
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
    assert!(book.reserve.as_ref().unwrap().leaf.is_some());
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
fn redeem_takes_amount_or_shares() {
    let s = Setup::new("shares", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let pool = || ("pool", vault1().debt_mint.to_string());
    let op = || ("op", "redeem".to_owned());
    let both = [op(), pool(), ("amount", "1".into()), ("shares", "1".into())];
    assert_eq!(build_req(&k, &lender, &both).0, 400);
    assert_eq!(build_req(&k, &lender, &[op(), pool()]).0, 400);
    let over = [op(), pool(), ("shares", (100 * USD + 1).to_string())];
    assert_eq!(build_req(&k, &lender, &over).0, 422);
    // Every share: the whole receipt's worth leaves the reserve, and the
    // zero-share receipt leaves the book.
    let before = k.snapshot().unwrap().reserve.unwrap().amount;
    let all = [op(), pool(), ("shares", (100 * USD).to_string())];
    let (st, v) = build_req(&k, &lender, &all);
    assert_eq!(st, 200, "{v}");
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.land(&p);
    k.resolve().unwrap();
    let book = k.snapshot().unwrap();
    assert_eq!(book.reserve.unwrap().amount, before - 100 * USD);
    assert!(book.receipts.is_empty());
}

#[test]
fn redeem_spends_the_receipt_the_page_names() {
    let s = Setup::new("named", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    fund_intent(&s, &k, &lender, purpose::SUPPLY, 1, 5 * USD);
    assert_eq!(drain(&k), vec![Op::Supply]);
    let book = k.snapshot().unwrap();
    assert_eq!(book.receipts.len(), 2);
    // The bigger receipt, named: the keeper would otherwise pick the smaller.
    let big = book
        .receipts
        .iter()
        .max_by_key(|r| r.state::<ReceiptState>().unwrap().supply_shares)
        .unwrap();
    let req = |h: &str| {
        [
            ("op", "redeem".to_owned()),
            ("pool", vault1().debt_mint.to_string()),
            ("amount", USD.to_string()),
            ("receipt", h.to_owned()),
        ]
    };
    assert_eq!(build_req(&k, &lender, &req(&h("nope", 1))).0, 404);
    assert_eq!(build_req(&k, &lender, &req("zz")).0, 400);
    let (st, v) = build_req(&k, &lender, &req(&big.hash));
    assert_eq!(st, 200, "{v}");
    assert_eq!(k.snapshot().unwrap().pending[0].inputs[1], big.nullifier);
    // The account view states the pool's indices at chain time.
    let (st, v) = account(&k, &lender);
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["index"]["supply"], json!("1000000000000"));
    assert_eq!(v["index"]["at"], json!(now_of(&k)));
}

#[test]
fn shares_redeem_pays_what_burns_exactly_those_shares() {
    use keeler_lend_sdk::builders::redeem_amount_for_shares;
    for price in [
        1_000_000_000_000u64,
        1_000_000_000_001,
        1_234_567_890_123,
        3_000_000_000_000,
    ] {
        let r = ReserveState {
            supply_price: price,
            ..fresh_reserve(u64::MAX / 4, 0)
        };
        for shares in [1u64, 7, 999_999, 100 * USD, 123_456_789_012] {
            let amount = redeem_amount_for_shares(&r, shares, T0).unwrap();
            assert_eq!(amount, core::assets_of(shares, price).unwrap());
            assert_eq!(core::shares_to_burn(amount, price).unwrap(), shares);
        }
    }
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
    let intent = s.fake.intents(&u.addr(), None, 1).unwrap().0.remove(0);
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
    let p = load_book(&s.state, &[s.signer.pubkey()], &keeler_lend::ID)
        .unwrap()
        .pending[0]
        .clone();
    s.fake.land(&p);
    // An unspent receipt redeemed to zero on chain is not brought back.
    let lender_addr = parse_address(p.wallet.as_str()).unwrap();
    s.fake
        .w()
        .history
        .push(receipt_note(&lender_addr, 0, 9_999));
    let k = s.keeper();
    let book = k.snapshot().unwrap();
    assert!(book.pending.is_empty());
    assert_eq!(book.reserve.as_ref().unwrap().hash, p.notes[0].hash);
    assert_eq!(book.receipts.len(), 2);
    assert_eq!(book.positions.len(), 1);
    drop(k);
    // Killed between recording and sending: the build never lands and is
    // dropped after its window; the ticket is filled on a retry.
    let mut book = load_book(&s.state, &[s.signer.pubkey()], &keeler_lend::ID).unwrap();
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
    assert!(Keeper::open_in(&s.cfg, &s.fake, &s.signer, s.state.clone(), &s.set).is_err());
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
    let (st, v) = account(&k, &a);
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["receipts"], json!([]));
    assert_eq!(v["positions"][0]["position"], json!("7"));
    assert_eq!(v["positions"][0]["collateral"], json!(SOL.to_string()));
    let m = B64
        .decode(v["positions"][0]["message"].as_str().unwrap())
        .unwrap();
    assert_eq!(&m[..8], &SOL.to_le_bytes());
    assert_eq!(v["intents"][0]["status"], json!("filled"));
    let (_, v) = account(&k, &lender);
    assert_eq!(v["positions"], json!([]));
    assert_eq!(v["receipts"][0]["shares"], json!((100 * USD).to_string()));
    // A ticket never used turns refundable after its expiry.
    let (_, t) = supply_ticket(&k, &lender, 2 * USD);
    s.fake.advance(601);
    let (_, v) = account(&k, &lender);
    let i = v["intents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == t["id"])
        .unwrap()
        .clone();
    assert_eq!(i["status"], json!("refundable"));
    // Pools: coarse and bucketed.
    let (st, v) = k.http("GET", "/lend/pools", b"", false);
    assert_eq!(st, 200);
    let p = &v["pools"][0];
    assert_eq!(p["size"], json!("100000000"));
    assert_eq!(p["at"].as_u64().unwrap() % 600, 0);
    let first = v.clone();
    s.fake.advance(1);
    assert_eq!(
        k.http("GET", "/lend/pools", b"", false).1,
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
    assert!(require_test_network(&lend_cfg(r#","price":"pyth""#)).is_err());
    assert!(lend_cfg(r#","price_refresh_secs":90"#).check().is_err());
    assert!(lend_cfg(r#","poll_secs":0"#).check().is_err());
    let mut label = lend_cfg("");
    label.network = "Mainnet".into();
    assert!(label.check().is_err(), "not a plain lowercase name");
    label.network = "mainnet-beta".into();
    assert!(label.check().is_err(), "the test price on mainnet-beta");
}

#[test]
fn the_test_price_is_kept_fresh_and_follows_the_control_file() {
    let s = Setup::new("price", r#","price_refresh_secs":45"#);
    let k = s.keeper();
    let posted = || s.fake.w().posted.clone();
    // Fresh (published at T0): nothing to do.
    assert!(!k.refresh_price().unwrap());
    s.fake.advance(44);
    assert!(!k.refresh_price().unwrap());
    // 45 s old: the same price again.
    s.fake.advance(1);
    assert!(k.refresh_price().unwrap());
    assert_eq!(posted(), vec![(1, 10 * DOLLAR, 10_000_000)]);
    assert!(!k.refresh_price().unwrap());
    // `keeler lend-set-price` names a new price: posted at once, then kept.
    let want = TestPrice {
        vault: 1,
        price: 6 * DOLLAR,
        confidence: 1_000,
    };
    write_private(&price_file(&s.state), &serde_json::to_vec(&want).unwrap()).unwrap();
    assert!(k.refresh_price().unwrap());
    assert!(!k.refresh_price().unwrap());
    s.fake.advance(45);
    assert!(k.refresh_price().unwrap());
    assert_eq!(
        posted()[1..],
        [(1, 6 * DOLLAR, 1_000), (1, 6 * DOLLAR, 1_000)]
    );
    // No price on chain and no file: nothing to post.
    std::fs::remove_file(price_file(&s.state)).unwrap();
    s.fake.w().quote = None;
    assert!(!k.refresh_price().unwrap());
}

#[test]
fn the_price_is_never_posted_with_pyth() {
    let s = Setup::new("price-pyth", r#","price":"pyth""#);
    let k = s.keeper();
    s.fake.advance(600);
    assert!(!k.refresh_price().unwrap());
    assert!(s.fake.w().posted.is_empty());
}

/// A drew `borrow` against `collateral` in position 1, landed and settled.
fn borrower(s: &Setup, k: &Keeper<Fake>, collateral: u64, borrow: u64) -> User {
    let a = s.user();
    fund_intent(s, k, &a, purpose::DEPOSIT, 1, collateral);
    assert_eq!(drain(k), vec![Op::DepositOpen]);
    let (st, v) = draw(k, &a, 1, borrow);
    assert_eq!(st, 200, "{v}");
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.land(&p);
    k.resolve().unwrap();
    a
}

fn ticket_of<'b>(book: &'b Book, id: &str) -> &'b Ticket {
    book.tickets.iter().find(|t| t.id == id).unwrap()
}

#[test]
fn a_failing_liquidation_waits_and_the_queue_goes_on() {
    let s = Setup::new("liq-fail", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let _a = borrower(&s, &k, SOL, 7 * USD);
    let b = s.user();
    fund_intent(&s, &k, &b, purpose::DEPOSIT, 2, SOL);
    drain(&k);
    // a goes under, and its liquidation cannot be built (no funds, say).
    s.fake.w().fail.push(Op::Liquidate);
    s.fake.w().quote = Some(Quote {
        low: 8 * DOLLAR,
        high: 8 * DOLLAR + 1,
        published: now_of(&k) as i64,
    });
    fund_intent(&s, &k, &b, purpose::DEPOSIT, 2, SOL / 10);
    assert_eq!(
        drain(&k),
        vec![Op::DepositAdd],
        "the deposit is not held up"
    );
    assert_eq!(
        s.fake
            .w()
            .builds
            .iter()
            .filter(|o| **o == Op::Liquidate)
            .count(),
        0,
        "refused builds are not counted as built"
    );
    // Nor are owner builds.
    let (st, v) = redeem(&k, &lender, USD);
    assert_eq!(st, 200, "{v}");
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.land(&p);
    k.resolve().unwrap();
    // After the backoff it is tried again, and goes.
    s.fake.w().fail.clear();
    assert!(drain(&k).is_empty(), "still waiting");
    s.fake.advance(LIQUIDATION_BACKOFF);
    s.fake.w().quote.as_mut().unwrap().published = now_of(&k) as i64;
    assert_eq!(drain(&k), vec![Op::Liquidate]);
}

#[test]
fn liquidations_need_a_younger_price_than_the_program() {
    let s = Setup::new("liq-age", "");
    let k = s.keeper();
    with_liquidity(&s, &k);
    borrower(&s, &k, SOL, 7 * USD);
    s.fake.w().quote = Some(Quote {
        low: 8 * DOLLAR,
        high: 8 * DOLLAR + 1,
        published: (now_of(&k) - PRICE_LIQUIDATE - 1) as i64,
    });
    assert!(drain(&k).is_empty(), "it would land after 90 s");
    s.fake.w().quote.as_mut().unwrap().published += 1;
    assert_eq!(drain(&k), vec![Op::Liquidate]);
}

#[test]
fn a_rejected_send_is_dropped_at_once_and_retried_later() {
    let s = Setup::new("reject", "");
    let k = s.keeper();
    with_liquidity(&s, &k);
    let u = s.user();
    let id = fund_intent(&s, &k, &u, purpose::SUPPLY, 1, 3 * USD);
    s.fake.w().reject = true;
    assert_eq!(drain(&k), vec![Op::Supply], "sent once, then it backs off");
    let book = k.snapshot().unwrap();
    assert!(book.pending.is_empty(), "no wait for the window");
    let t = ticket_of(&book, &id);
    assert_eq!((t.attempts, t.status), (1, TicketStatus::Open));
    s.fake.w().reject = false;
    s.fake.advance(RETRY_SECS << 1);
    assert_eq!(drain(&k), vec![Op::Supply]);
    let book = k.snapshot().unwrap();
    assert_eq!(ticket_of(&book, &id).status, TicketStatus::Filled);
}

#[test]
fn failures_back_off_and_repays_are_never_given_up() {
    let s = Setup::new("retry", "");
    let k = s.keeper();
    with_liquidity(&s, &k);
    let a = borrower(&s, &k, SOL, 5 * USD);
    let lender = s.user();
    let supply = fund_intent(&s, &k, &lender, purpose::SUPPLY, 1, 2 * USD);
    let repay = fund_intent(&s, &k, &a, purpose::REPAY, 1, USD);
    s.fake.w().fail = vec![Op::Supply, Op::Repay];
    // A short outage gives nothing up.
    for _ in 0..5 {
        k.step().unwrap();
        s.fake.advance(1);
    }
    let book = k.snapshot().unwrap();
    assert_eq!(ticket_of(&book, &supply).attempts, 1, "backing off");
    // A long one gives up the supply (its maker refunds), not the repay.
    for _ in 0..8 {
        k.step().unwrap();
        s.fake.advance(30);
    }
    let book = k.snapshot().unwrap();
    let (t, r) = (ticket_of(&book, &supply), ticket_of(&book, &repay));
    assert_eq!((t.status, t.attempts), (TicketStatus::Failed, MAX_ATTEMPTS));
    assert_eq!(r.status, TicketStatus::Open);
    assert!(r.attempts >= MAX_ATTEMPTS);
    s.fake.w().fail.clear();
    s.fake.advance(RETRY_SECS << 5);
    assert_eq!(drain(&k), vec![Op::Repay]);
}

#[test]
fn a_build_whose_other_input_went_elsewhere_is_dropped() {
    let s = Setup::new("taken", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
    let p = k.snapshot().unwrap().pending[0].clone();
    // The receipt is spent by another transaction; the reserve is not.
    s.fake.w().spent.insert(hex32(&p.inputs[1]).unwrap());
    k.resolve().unwrap();
    let book = k.snapshot().unwrap();
    assert!(book.pending.is_empty(), "it can never land");
    assert!(book.receipts.is_empty());
    assert!(book.reserve.is_some());
}

#[test]
fn liquidity_is_checked_last_and_rounded() {
    let s = Setup::new("liquidity", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let rich = s.user();
    fund_intent(&s, &k, &rich, purpose::DEPOSIT, 1, 100 * SOL);
    let poor = s.user();
    fund_intent(&s, &k, &poor, purpose::DEPOSIT, 1, SOL);
    drain(&k);
    // Health, then liquidity: a small position learns nothing of the pool.
    assert_eq!(draw(&k, &poor, 1, 200 * USD).0, 423);
    assert_eq!(draw(&k, &rich, 1, 101 * USD).0, 410);
    assert_eq!(draw(&k, &rich, 1, 96 * USD).0, 410, "the 95% cap");
    assert_eq!(draw(&k, &poor, 1, 9_999).0, 422, "below the minimum borrow");
    let (st, v) = draw(&k, &rich, 1, 94_990_000);
    assert_eq!(st, 200, "{v}");
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.land(&p);
    k.resolve().unwrap();
    // 5.01 tUSD idle: 5.005 fits exactly, but is checked as 5.1.
    assert_eq!(redeem(&k, &lender, 5_005_000).0, 410);
    assert_eq!(
        redeem(&k, &lender, 9_999).0,
        422,
        "below the minimum redeem"
    );
    assert_eq!(redeem(&k, &lender, 5 * USD).0, 200);
}

#[test]
fn reservations_are_not_extended_and_a_lapse_cools_down() {
    let s = Setup::new("lapse", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let a = s.user();
    fund_intent(&s, &k, &a, purpose::DEPOSIT, 1, SOL);
    drain(&k);
    let t = now_of(&k);
    assert_eq!(draw(&k, &a, 1, USD).0, 200);
    s.fake.advance(10);
    assert_eq!(draw(&k, &a, 1, USD).0, 200, "a may build again");
    assert_eq!(k.snapshot().unwrap().reservation.unwrap().until, t + 20);
    s.fake.advance(10);
    assert_eq!(
        draw(&k, &a, 1, USD).0,
        409,
        "lapsed unused: not again at once"
    );
    assert_eq!(redeem(&k, &lender, USD).0, 200, "others go");
    s.fake.advance(21);
    assert_eq!(draw(&k, &a, 1, USD).0, 200);
}

#[test]
fn builds_per_wallet_are_limited() {
    let s = Setup::new("builds", r#","limits":{"builds_per_wallet_hour":2}"#);
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
    assert_eq!(redeem(&k, &lender, USD).0, 429);
    // A wallet with nothing to spend never gets that far.
    let b = s.user();
    assert_eq!(redeem(&k, &b, USD).0, 404);
    s.fake.advance(HOUR);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
}

#[test]
fn a_repay_worth_less_than_one_debt_share_is_refused() {
    let s = Setup::new("dust-repay", r#","limits":{"min_repay":1}"#);
    s.fake.w().history = vec![reserve_note(
        &ReserveState {
            borrow_price: 2_000_000_000_000,
            ..fresh_reserve(0, 0)
        },
        0,
        1,
    )];
    let k = s.keeper();
    with_liquidity(&s, &k);
    let a = borrower(&s, &k, SOL, 5 * USD);
    let (st, v) = repay_ticket(&k, &a, 1, 1);
    assert_eq!(st, 422, "{v}");
    assert!(v["error"].as_str().unwrap().contains("one debt share"));
    assert_eq!(repay_ticket(&k, &a, 1, 2).0, 200);
}

#[test]
fn state_and_pause_paths_may_start_at_home() {
    let cfg = lend_cfg(r#","state_file":"~/.local/state/keeler/k.json","pause_file":"/p/~/x""#);
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    assert_eq!(
        cfg.state_file.unwrap(),
        home.join(".local/state/keeler/k.json")
    );
    assert_eq!(cfg.pause_file.unwrap(), PathBuf::from("/p/~/x"));
}

#[test]
fn each_unused_reservation_doubles_the_wait() {
    let s = Setup::new("strikes", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let t = now_of(&k);
    let at = |secs: u64| {
        s.fake.w().now = t + secs;
        redeem(&k, &lender, USD).0
    };
    assert_eq!(at(0), 200);
    assert_eq!(at(20), 409, "lapsed unused once: 20 s");
    assert_eq!(at(39), 409);
    assert_eq!(at(40), 200);
    assert_eq!(at(99), 409, "twice: 40 s");
    assert_eq!(at(100), 200);
    assert_eq!(at(199), 409, "three times: 80 s");
    assert_eq!(at(200), 200);
    assert_eq!(cooldown_secs(20, 1), 20);
    assert_eq!(cooldown_secs(20, 8), 2_560);
    assert_eq!(cooldown_secs(20, 9), HOUR, "at most an hour");
    assert_eq!(cooldown_secs(120, 60), HOUR);
}

#[test]
fn a_build_that_lands_late_is_no_strike() {
    let s = Setup::new("forgiven", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.advance(20);
    assert_eq!(redeem(&k, &lender, USD).0, 409, "lapsed, not landed yet");
    s.fake.advance(1);
    s.fake.land(&p);
    k.resolve().unwrap();
    s.fake.advance(1);
    assert_eq!(redeem(&k, &lender, USD).0, 200, "it was used after all");
}

#[test]
fn a_strike_is_counted_from_the_book_not_a_stale_snapshot() {
    let s = Setup::new("strike-race", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.advance(21);
    let stale = k.snapshot().unwrap();
    assert!(stale.reservation.is_some());
    s.fake.land(&p);
    k.resolve().unwrap();
    k.strike_lapsed(now_of(&k));
    let w = lender.addr().to_string();
    assert!(
        !k.cooling(&w, now_of(&k)).unwrap(),
        "forgiven, not struck again"
    );
}

#[test]
fn rotating_wallets_hold_the_lane_little_of_an_hour() {
    let s = Setup::new("rotate", r#","limits":{"builds_per_wallet_hour":100000}"#);
    let k = s.keeper();
    let wallets: Vec<User> = (0..3).map(|_| with_liquidity(&s, &k)).collect();
    let mut held = HashSet::new();
    let t = now_of(&k);
    for second in 0..HOUR {
        s.fake.w().now = t + second;
        for u in &wallets {
            let w = u.addr().to_string();
            let mine = k
                .snapshot()
                .unwrap()
                .reservation
                .is_some_and(|r| r.wallet == w && r.until > t + second);
            if !mine && redeem(&k, u, USD).0 == 200 {
                held.insert(k.snapshot().unwrap().reservation.unwrap().until);
            }
        }
    }
    let lane_secs = held.len() as u64 * 20;
    assert!(lane_secs <= 3 * 160, "held {lane_secs} s of the hour");
    assert!(
        lane_secs >= 3 * 140,
        "honest retries still get in: {lane_secs} s"
    );
}

#[test]
fn after_an_unused_lapse_a_waiting_supply_goes_first() {
    let s = Setup::new("lapse-fill", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let other = with_liquidity(&s, &k);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
    let u = s.user();
    fund_intent(&s, &k, &u, purpose::SUPPLY, 1, 2 * USD);
    assert!(!k.step().unwrap(), "the supply waits for the reservation");
    s.fake.advance(20);
    assert_eq!(redeem(&k, &other, USD).0, 409, "the supply first");
    assert_eq!(drain(&k), vec![Op::Supply]);
    assert_eq!(redeem(&k, &other, USD).0, 200);
}

#[test]
fn a_refused_build_does_not_count_toward_the_hour() {
    let s = Setup::new("hour-409", r#","limits":{"builds_per_wallet_hour":2}"#);
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let other = with_liquidity(&s, &k);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
    for _ in 0..3 {
        assert_eq!(redeem(&k, &other, USD).0, 409);
    }
    s.fake.advance(20);
    assert_eq!(redeem(&k, &other, USD).0, 200);
    assert_eq!(redeem(&k, &other, USD).0, 200);
    assert_eq!(redeem(&k, &other, USD).0, 429);
}

#[test]
fn a_liquidation_beaten_to_the_reserve_by_an_owner_build_retries_at_once() {
    let s = Setup::new("liq-race", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    borrower(&s, &k, SOL, 7 * USD);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
    let owner_build = k.snapshot().unwrap().pending[0].clone();
    s.fake.advance(20);
    s.fake.w().quote = Some(Quote {
        low: 8 * DOLLAR,
        high: 8 * DOLLAR + 1,
        published: now_of(&k) as i64,
    });
    let u = s.user();
    let id = fund_intent(&s, &k, &u, purpose::SUPPLY, 1, 2 * USD);
    s.fake.w().race = Some(owner_build);
    assert!(k.step().is_ok());
    assert_eq!(
        s.fake.w().sent,
        vec![Op::Supply, Op::DepositOpen, Op::Liquidate]
    );
    let book = k.snapshot().unwrap();
    let t = ticket_of(&book, &id);
    assert_eq!(
        (t.attempts, t.retry_at),
        (0, 0),
        "no fill on the stale book"
    );
    assert_eq!(
        redeem(&k, &lender, USD).0,
        409,
        "the liquidation keeps its turn"
    );
    assert_eq!(drain(&k), vec![Op::Liquidate, Op::Supply], "no 60 s wait");
}

#[test]
fn a_late_build_landing_under_anothers_reservation_frees_the_lane() {
    let s = Setup::new("freed", "");
    let k = s.keeper();
    let a = with_liquidity(&s, &k);
    let b = with_liquidity(&s, &k);
    assert_eq!(redeem(&k, &a, USD).0, 200);
    let held = k.snapshot().unwrap().pending[0].clone();
    s.fake.advance(20);
    assert_eq!(redeem(&k, &a, USD).0, 409, "a lapsed");
    s.fake.advance(1);
    assert_eq!(redeem(&k, &b, USD).0, 200);
    s.fake.advance(19);
    s.fake.land(&held);
    k.resolve().unwrap();
    let book = k.snapshot().unwrap();
    assert!(book.pending.is_empty() && book.reservation.is_none());
    s.fake.advance(1);
    assert_eq!(redeem(&k, &b, USD).0, 200, "b is not cooling");
}

#[test]
fn a_liquidation_under_a_reservation_frees_the_lane() {
    let s = Setup::new("freed-liq", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    borrower(&s, &k, SOL, 7 * USD);
    assert_eq!(redeem(&k, &lender, USD).0, 200);
    s.fake.w().quote = Some(Quote {
        low: 8 * DOLLAR,
        high: 8 * DOLLAR + 1,
        published: now_of(&k) as i64,
    });
    assert_eq!(drain(&k), vec![Op::Liquidate]);
    let book = k.snapshot().unwrap();
    assert!(book.pending.is_empty() && book.reservation.is_none());
    s.fake.advance(20);
    let (st, v) = redeem(&k, &lender, USD);
    assert_eq!(st, 200, "the lender is not cooling: {v}");
}

#[test]
fn a_price_drop_since_the_last_pass_halts_borrowing_at_once() {
    let s = Setup::new("fresh-halt", "");
    let k = s.keeper();
    with_liquidity(&s, &k);
    borrower(&s, &k, SOL, 7 * USD);
    let c = s.user();
    fund_intent(&s, &k, &c, purpose::DEPOSIT, 3, 10 * SOL);
    drain(&k);
    s.fake.w().quote = Some(Quote {
        low: 5 * DOLLAR,
        high: 5 * DOLLAR + 1,
        published: now_of(&k) as i64,
    });
    let (st, v) = draw(&k, &c, 3, USD);
    assert_eq!(st, 403, "{v}");
}

#[test]
fn the_open_ticket_cap_counts_queued_intents_and_spares_repays() {
    let s = Setup::new("cap", r#","limits":{"max_open_intents":2}"#);
    let k = s.keeper();
    with_liquidity(&s, &k);
    let a = borrower(&s, &k, SOL, USD);
    // Two queued supplies whose fills fail, so their tickets stay open.
    s.fake.w().fail.push(Op::Supply);
    let x = s.user();
    let y = s.user();
    let ids = [
        fund_intent(&s, &k, &x, purpose::SUPPLY, 1, 2 * USD),
        fund_intent(&s, &k, &y, purpose::SUPPLY, 1, 2 * USD),
    ];
    k.step().unwrap();
    let book = k.snapshot().unwrap();
    assert!(ids.iter().all(|id| ticket_of(&book, id).seen));
    // Tickets without an intent do not count, but the queue is full now.
    let z = s.user();
    assert_eq!(supply_ticket(&k, &z, 2 * USD).0, 429);
    assert_eq!(deposit_ticket(&k, &z, 1, SOL).0, 429, "a new position");
    // A repay or a top-up of a position never waits for others.
    assert_eq!(repay_ticket(&k, &a, 1, USD).0, 200);
    assert_eq!(deposit_ticket(&k, &a, 1, SOL / 10).0, 200);
}

#[test]
fn tickets_issued_ahead_count_toward_the_listed_eight() {
    let s = Setup::new("ahead", r#","limits":{"open_per_wallet":20}"#);
    let k = s.keeper();
    let u = s.user();
    for i in 0..8 {
        assert_eq!(deposit_ticket(&k, &u, i, SOL).0, 200);
    }
    assert_eq!(deposit_ticket(&k, &u, 9, SOL).0, 429);
}

#[test]
fn racing_ticket_requests_cannot_pass_a_cap_together() {
    let s = Setup::new("racing", r#","limits":{"open_per_wallet":3}"#);
    let k = s.keeper();
    let u = s.user();
    let ok = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..8)
            .map(|_| sc.spawn(|| supply_ticket(&k, &u, 2 * USD).0))
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap())
            .filter(|st| *st == 200)
            .count()
    });
    assert_eq!(ok, 3);
}

#[test]
fn one_keeper_per_state_file() {
    let s = Setup::new("lock", "");
    let k = s.keeper();
    assert!(Keeper::open(&s.cfg, &s.fake, &s.signer, s.state.clone()).is_err());
    drop(k);
    s.keeper();
}

#[test]
fn history_and_intent_scans_go_on_from_their_marks() {
    let s = Setup::new("marks", "");
    let k = s.keeper();
    assert_eq!(
        k.snapshot()
            .unwrap()
            .scanned
            .get(&s.signer.pubkey().to_string())
            .map(String::as_str),
        Some("mark")
    );
    drop(k);
    let k = s.keeper();
    assert_eq!(s.fake.w().history_after, vec![None, Some("mark".into())]);
    let u = s.user();
    supply_ticket(&k, &u, 2 * USD);
    k.step().unwrap();
    k.step().unwrap();
    assert_eq!(s.fake.w().intents_after, vec![None, Some("cursor".into())]);
}

#[test]
fn the_slot_answers_503_until_the_keeper_is_up() {
    let s = Setup::new("slot", "");
    let cell = OnceLock::new();
    let slot = Slot(&cell);
    let (st, v) = slot.lend("GET", "/lend/pools", b"", false);
    // No `error` field: the Worker tries the next keeper.
    assert_eq!(st, 503);
    assert_eq!(
        v.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["starting"]
    );
    assert!(v["starting"].is_string());
    assert!(cell.set(s.keeper()).is_ok());
    assert_eq!(slot.lend("GET", "/lend/pools", b"", false).0, 200);
}

#[test]
fn coarse_up_rounds_up_to_two_figures() {
    assert_eq!(coarse_up(0), 0);
    assert_eq!(coarse_up(99), 99);
    assert_eq!(coarse_up(101), 110);
    assert_eq!(coarse_up(9_901), 10_000);
    assert_eq!(coarse_up(5_005_000), 5_100_000);
    assert_eq!(coarse_up(5_000_000), 5_000_000);
    assert_eq!(coarse_up(u64::MAX), u64::MAX);
}

/// What `k` holds now, as the chain's history would show it to anyone.
fn publish_notes(s: &Setup, k: &Keeper<Fake>) {
    let b = k.snapshot().unwrap();
    s.fake.w().history = b
        .reserve
        .iter()
        .chain(&b.receipts)
        .chain(&b.positions)
        .cloned()
        .collect();
}

/// A supply ticket as the page holds it: the request and the reply.
fn held_supply(reply: &Value, amount: u64) -> Value {
    json!({
        "id": reply["id"],
        "expiry": reply["expiry"],
        "signature": reply["signature"],
        "keeper": reply["keeper"],
        "purpose": "supply",
        "pool": vault1().debt_mint.to_string(),
        "amount": amount.to_string(),
    })
}

#[test]
fn a_peer_takes_on_a_ticket_and_the_issuer_closes_it() {
    let s = Setup::new("peers", "");
    let a = s.keeper();
    with_liquidity(&s, &a);
    publish_notes(&s, &a);
    let b_key = Keypair::new();
    let b = Keeper::open_in(
        &s.cfg,
        &s.fake,
        &b_key,
        s.dir.join("b.json"),
        &[s.signer.pubkey()],
    )
    .unwrap();
    assert!(b.snapshot().unwrap().reserve.is_some(), "b sees a's pool");
    let u = s.user();
    let (st, reply) = supply_ticket(&a, &u, 3 * USD);
    assert_eq!(st, 200, "{reply}");
    assert_eq!(reply["keeper"], json!(s.signer.pubkey().to_string()));
    let held = held_supply(&reply, 3 * USD);
    // Forged, tampered, too long-lived, or too many: nothing is taken on.
    let mut stranger = held.clone();
    stranger["keeper"] = json!(Keypair::new().pubkey().to_string());
    let mut more = held.clone();
    more["amount"] = json!((30 * USD).to_string());
    let mut late = held.clone();
    late["expiry"] = json!(now_of(&b) + 901);
    let (st, v) = account_with(&b, &u, json!([stranger, more, late]));
    assert_eq!(st, 200, "{v}");
    assert!(b.snapshot().unwrap().tickets.is_empty());
    assert_eq!(account_with(&b, &u, json!(vec![held.clone(); 9])).0, 400);
    // The real one: b keeps it, a's signature and all.
    assert_eq!(account_with(&b, &u, json!([held.clone()])).0, 200);
    let t = b.snapshot().unwrap().tickets;
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].keeper, Some(s.signer.pubkey().to_string()));
    // Both race to fill it; b's lands.
    s.fake.add_intent(
        &u.addr(),
        purpose::SUPPLY,
        1,
        3 * USD,
        reply["expiry"].as_u64().unwrap(),
    );
    s.fake.w().hold = true;
    assert_eq!(drain(&b), vec![Op::Supply]);
    assert_eq!(drain(&a), vec![Op::Supply]);
    let won = b.snapshot().unwrap().pending[0].clone();
    s.fake.land(&won);
    s.fake.w().hold = false;
    b.resolve().unwrap();
    publish_notes(&s, &b);
    assert!(drain(&a).is_empty());
    assert!(
        a.snapshot().unwrap().pending.is_empty(),
        "a's build lost the reserve"
    );
    // After the backoff a looks again and finds the intent spent.
    s.fake.advance(RETRY_SECS << 1);
    assert!(drain(&a).is_empty());
    let id = reply["id"].as_str().unwrap();
    let book = a.snapshot().unwrap();
    assert_eq!(ticket_of(&book, id).status, TicketStatus::Filled);
    let theirs = b.snapshot().unwrap();
    assert_eq!(book.reserve, theirs.reserve, "a follows b's reserve");
    assert_eq!(ticket_of(&theirs, id).status, TicketStatus::Filled);
}

#[test]
fn a_standby_liquidates_after_its_grace() {
    let s = Setup::new("standby", r#","role":"standby","standby_grace_secs":30"#);
    let k = s.keeper();
    with_liquidity(&s, &k);
    borrower(&s, &k, SOL, 7 * USD);
    let price = |dollars: u64| {
        let now = now_of(&k) as i64;
        s.fake.w().quote = Some(Quote {
            low: dollars * DOLLAR,
            high: dollars * DOLLAR + 1,
            published: now,
        });
    };
    price(8);
    assert!(drain(&k).is_empty(), "the grace starts");
    s.fake.advance(20);
    price(10);
    assert!(drain(&k).is_empty(), "healthy again: the grace restarts");
    s.fake.advance(1);
    price(8);
    drain(&k);
    s.fake.advance(29);
    price(8);
    assert!(drain(&k).is_empty(), "29 s past");
    s.fake.advance(1);
    price(8);
    assert_eq!(drain(&k), vec![Op::Liquidate]);
}

#[test]
fn bad_debt_beyond_the_surplus_halts_borrowing() {
    let s = Setup::new("bad-debt", "");
    let k = s.keeper();
    with_liquidity(&s, &k);
    borrower(&s, &k, SOL, 7 * USD);
    let c = s.user();
    fund_intent(&s, &k, &c, purpose::DEPOSIT, 3, 10 * SOL);
    drain(&k);
    let pools = k.http("GET", "/lend/pools", b"", false).1;
    assert_eq!(pools["pools"][0]["badDebt"], json!("0"));
    // The first position is worth $5 against $7 of debt, and cannot be
    // liquidated.
    s.fake.w().fail.push(Op::Liquidate);
    s.fake.w().quote = Some(Quote {
        low: 5 * DOLLAR,
        high: 5 * DOLLAR + 1,
        published: now_of(&k) as i64,
    });
    drain(&k);
    let (st, v) = draw(&k, &c, 3, USD);
    assert_eq!(st, 403, "{v}");
    assert!(v["error"].as_str().unwrap().contains("bad debt"));
    // The price back up: borrowing resumes.
    s.fake.w().quote = Some(Quote {
        low: 10 * DOLLAR,
        high: 10 * DOLLAR + 1,
        published: now_of(&k) as i64,
    });
    drain(&k);
    assert_eq!(draw(&k, &c, 3, USD).0, 200);
}

#[test]
fn an_old_single_scan_mark_is_this_keepers() {
    let s = Setup::new("old-mark", "");
    let old = json!({"reserve":null,"receipts":[],"positions":[],"tickets":[],
        "pending":[],"reservation":null,"scanned":"m1"});
    std::fs::write(&s.state, serde_json::to_vec(&old).unwrap()).unwrap();
    let k = s.keeper();
    assert_eq!(s.fake.w().history_after[0].as_deref(), Some("m1"));
    let marks = k.snapshot().unwrap().scanned;
    assert_eq!(
        marks.keys().collect::<Vec<_>>(),
        [&s.signer.pubkey().to_string()]
    );
}

#[test]
fn the_chain_must_be_the_network_named() {
    use crate::chain::Cluster;
    assert!(check_cluster(&lend_cfg(""), Cluster::Local).is_ok());
    assert!(check_cluster(&lend_cfg(""), Cluster::Devnet).is_err());
    let mut dev = lend_cfg("");
    dev.network = "devnet".into();
    assert!(check_cluster(&dev, Cluster::Devnet).is_ok());
    assert!(check_cluster(&dev, Cluster::Mainnet).is_err());
}

#[test]
fn caps_count_funded_tickets_and_hold_at_fill() {
    let s = Setup::new(
        "caps",
        r#","limits":{"max_pool_size":110000000,"max_supply_per_wallet":105000000,"max_total_collateral":3000000000,"max_position_collateral":2000000000,"max_position_debt":6000000,"min_position_debt":2000000}"#,
    );
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    // Pool 100 of 110; the lender has 100 of 105, and its own tickets count.
    assert_eq!(supply_ticket(&k, &lender, 6 * USD).0, 422, "per wallet");
    fund_intent(&s, &k, &lender, purpose::SUPPLY, 1, 5 * USD);
    assert_eq!(
        supply_ticket(&k, &lender, USD).0,
        422,
        "its own open ticket"
    );
    // Another wallet's ticket with no intent on chain blocks nobody.
    let other = s.user();
    let (st, idle) = supply_ticket(&k, &other, 6 * USD);
    assert_eq!(st, 200, "{idle}");
    // Once the lender's intent is seen, its ticket counts for everyone.
    s.pause(true);
    k.step().unwrap();
    s.pause(false);
    assert_eq!(
        supply_ticket(&k, &s.user(), 6 * USD).0,
        422,
        "a funded ticket"
    );
    assert_eq!(drain(&k), vec![Op::Supply]);
    // At 105, the other's 6, funded late, no longer fits.
    let expiry = idle["expiry"].as_u64().unwrap();
    s.fake
        .add_intent(&other.addr(), purpose::SUPPLY, 1, 6 * USD, expiry);
    assert!(drain(&k).is_empty(), "the pool size at fill");

    let a = s.user();
    assert_eq!(
        deposit_ticket(&k, &a, 1, 2 * SOL + 1).0,
        422,
        "per position"
    );
    fund_intent(&s, &k, &a, purpose::DEPOSIT, 1, 2 * SOL);
    assert_eq!(
        deposit_ticket(&k, &a, 2, SOL + 1).0,
        422,
        "its own tickets, in total"
    );
    let b = s.user();
    let (st, late) = deposit_ticket(&k, &b, 1, SOL + 1);
    assert_eq!(st, 200, "another's unfunded ticket does not count: {late}");
    assert_eq!(drain(&k), vec![Op::DepositOpen]);
    let c = s.user();
    fund_intent(&s, &k, &c, purpose::DEPOSIT, 1, SOL);
    assert_eq!(drain(&k), vec![Op::DepositOpen]);
    // The vault holds its 3 SOL: no new position, but a top-up, which may
    // save a position, is never held to the total.
    assert_eq!(
        deposit_ticket(&k, &s.user(), 1, SOL / 10).0,
        422,
        "in total"
    );
    fund_intent(&s, &k, &c, purpose::DEPOSIT, 1, SOL / 2);
    assert_eq!(drain(&k), vec![Op::DepositAdd]);
    // b's, funded now, no longer fits at fill.
    let expiry = late["expiry"].as_u64().unwrap();
    s.fake
        .add_intent(&b.addr(), purpose::DEPOSIT, 1, SOL + 1, expiry);
    assert!(drain(&k).is_empty(), "the collateral caps at fill");
    assert_eq!(draw(&k, &a, 1, USD).0, 422, "below the minimum debt");
    assert_eq!(draw(&k, &a, 1, 7 * USD).0, 422, "above the most debt");
    assert_eq!(draw(&k, &a, 1, 6 * USD).0, 200);
}

#[test]
fn the_minimum_debt_binds_borrows_and_part_repays_only() {
    let mut s = Setup::new("min-debt", r#","limits":{"min_position_debt":2000000}"#);
    let k = s.keeper();
    with_liquidity(&s, &k);
    let a = borrower(&s, &k, SOL, 3 * USD);
    // 1.5 of 3 would leave 1.5, under the 2 floor; 1, or all of it, is fine.
    assert_eq!(repay_ticket(&k, &a, 1, 3 * USD / 2).0, 422);
    assert_eq!(repay_ticket(&k, &a, 1, USD).0, 200);
    assert_eq!(repay_ticket(&k, &a, 1, 3 * USD).0, 200);
    // The floor raised past the debt: borrowing a little more is refused,
    // withdrawing collateral is not.
    drop(k);
    s.cfg = lend_cfg(r#","limits":{"min_position_debt":5000000}"#);
    let k = s.keeper();
    assert_eq!(draw(&k, &a, 1, USD).0, 422);
    let (st, v) = build_req(
        &k,
        &a,
        &[
            ("op", "draw".into()),
            ("vault", "1".into()),
            ("position", "1".into()),
            ("borrow", "0".into()),
            ("withdraw", (SOL / 10).to_string()),
        ],
    );
    assert_eq!(st, 200, "{v}");
}

/// A setup whose keeper set has one peer, with a history of its own.
fn with_peer(name: &str, extra: &str) -> (Setup, Address) {
    let mut s = Setup::new(name, extra);
    let peer = Keypair::new().pubkey();
    s.set = vec![peer];
    s.fake.w().peer_history.insert(peer, Vec::new());
    (s, peer)
}

#[test]
fn each_keeper_has_its_own_mark_and_a_peers_notes_join_the_book() {
    let (s, peer) = with_peer("peer-marks", "");
    let u = s.user();
    // The peer opened u's position 1 (a deposit spends no reserve).
    s.fake
        .w()
        .peer_history
        .insert(peer, vec![position_note(&u.addr(), 1, SOL, 0, 900)]);
    let k = s.keeper();
    let b = k.snapshot().unwrap();
    assert_eq!(b.positions.len(), 1, "the peer's note");
    let mark = |b: &Book, k: &Address| b.scanned.get(&k.to_string()).cloned();
    assert_eq!(mark(&b, &s.signer.pubkey()).as_deref(), Some("mark"));
    assert_eq!(mark(&b, &peer).as_deref(), Some("peer-1"));
    // Nothing new: nothing changes, nothing is saved.
    std::fs::remove_file(&s.state).unwrap();
    k.merge_history(&[peer], false).unwrap();
    assert!(!s.state.exists(), "a scan with nothing new saves nothing");
    assert_eq!(k.snapshot().unwrap(), b);
    // The peer tops it up: the old note is spent, the new one is in the
    // peer's history, and the reserve is untouched.
    {
        let mut w = s.fake.w();
        w.spent.insert(b.positions[0].nullifier().unwrap());
        w.peer_history
            .get_mut(&peer)
            .unwrap()
            .push(position_note(&u.addr(), 1, 2 * SOL, 0, 901));
    }
    k.step().unwrap();
    let after = k.snapshot().unwrap();
    assert_eq!(after.reserve, b.reserve);
    let amounts: Vec<u64> = after.positions.iter().map(|p| p.amount).collect();
    assert_eq!(amounts, [2 * SOL]);
    assert_eq!(mark(&after, &peer).as_deref(), Some("peer-2"));
    assert_eq!(mark(&after, &s.signer.pubkey()).as_deref(), Some("mark"));
}

#[test]
fn no_second_position_while_a_peers_successor_is_on_its_way() {
    let (s, peer) = with_peer("peer-topup", "");
    let k = s.keeper();
    let u = s.user();
    fund_intent(&s, &k, &u, purpose::DEPOSIT, 1, SOL);
    assert_eq!(drain(&k), vec![Op::DepositOpen]);
    let old = k.snapshot().unwrap().positions[0].clone();
    // A top-up; a peer spends the position before its new note is here.
    let id = fund_intent(&s, &k, &u, purpose::DEPOSIT, 1, SOL / 10);
    s.fake.w().spent.insert(old.nullifier().unwrap());
    assert!(drain(&k).is_empty(), "no second position 1");
    s.fake
        .w()
        .peer_history
        .get_mut(&peer)
        .unwrap()
        .push(position_note(&u.addr(), 1, 2 * SOL, 0, 900));
    assert_eq!(drain(&k), vec![Op::DepositAdd], "the top-up goes into it");
    assert_eq!(
        ticket_of(&k.snapshot().unwrap(), &id).status,
        TicketStatus::Filled
    );
}

#[test]
fn a_failed_read_of_a_peers_history_does_not_stop_a_liquidation() {
    let (s, _) = with_peer("peer-fail", "");
    let k = s.keeper();
    with_liquidity(&s, &k);
    borrower(&s, &k, SOL, 7 * USD);
    s.fake.w().peer_history_fails = true;
    s.fake.w().quote = Some(Quote {
        low: 8 * DOLLAR,
        high: 8 * DOLLAR + 1,
        published: now_of(&k) as i64,
    });
    assert_eq!(drain(&k), vec![Op::Liquidate]);
}

#[test]
fn a_book_that_does_not_add_up_lets_nothing_new_in() {
    let s = Setup::new("shares", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let u = borrower(&s, &k, SOL, 3 * USD);
    let other = s.user();
    fund_intent(&s, &k, &other, purpose::SUPPLY, 1, 5 * USD);
    assert_eq!(drain(&k), vec![Op::Supply]);
    // The book loses a receipt: its shares no longer add up to the reserve's.
    let lost = {
        let mut b = k.book().unwrap();
        let i = b
            .receipts
            .iter()
            .position(|r| r.owned_by(&other.addr()))
            .unwrap();
        b.receipts.remove(i)
    };
    k.resolve().unwrap();
    assert_eq!(supply_ticket(&k, &lender, 2 * USD).0, 403, "supply");
    assert_eq!(
        deposit_ticket(&k, &s.user(), 2, SOL).0,
        403,
        "a new position"
    );
    assert_eq!(draw(&k, &u, 1, USD).0, 403, "borrowing");
    // Repays, top-ups, withdrawals and redeems go on.
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
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.land(&p);
    k.resolve().unwrap();
    // The receipt back: lending opens again.
    k.book().unwrap().receipts.push(lost);
    k.resolve().unwrap();
    assert_eq!(supply_ticket(&k, &lender, 2 * USD).0, 200);
}

#[test]
fn a_reserve_lost_past_the_marks_is_found_by_reading_everything_again() {
    let s = Setup::new("lost-reserve", "");
    let k = s.keeper();
    let old = k.snapshot().unwrap().reserve.unwrap();
    let newer = reserve_note(&fresh_reserve(0, 0), 0, 77);
    {
        // Spent by a transaction older than the mark that this book missed.
        let mut w = s.fake.w();
        w.spent.insert(old.nullifier().unwrap());
        w.history = vec![old, newer.clone()];
        w.history_honours_mark = true;
        w.history_after.clear();
    }
    k.resolve().unwrap();
    assert_eq!(k.snapshot().unwrap().reserve, Some(newer));
    assert_eq!(s.fake.w().history_after, [Some("mark".into()), None]);
}

#[test]
fn a_landed_build_does_not_replace_a_newer_reserve() {
    let r1 = reserve_note(&fresh_reserve(0, 0), 0, 1);
    let r2 = reserve_note(&fresh_reserve(5, 0), 5, 2);
    let r3 = reserve_note(&fresh_reserve(9, 0), 9, 3);
    let p = Pending {
        op: Op::Supply,
        wallet: Keypair::new().pubkey().to_string(),
        spends: Some(r1.nullifier.clone()),
        inputs: vec![r1.nullifier.clone()],
        outputs: vec![r2.hash.clone()],
        notes: vec![r2.clone()],
        now: T0,
        ticket: None,
        owner_signed: false,
        signature: None,
    };
    // A merge already put r3, built on r2 by a peer, in the book.
    let mut book = Book {
        reserve: Some(r3.clone()),
        pending: vec![p.clone()],
        ..Book::default()
    };
    apply(&mut book, &p, &[]);
    assert_eq!(book.reserve, Some(r3));
    // Over the reserve it spent, its own output is taken.
    let mut book = Book {
        reserve: Some(r1),
        pending: vec![p.clone()],
        ..Book::default()
    };
    apply(&mut book, &p, &[]);
    assert_eq!(book.reserve.map(|r| r.hash), Some(r2.hash));
}

#[test]
fn the_workers_pause_header_stops_new_money_only() {
    let s = Setup::new("paused-header", "");
    let k = s.keeper();
    let lender = with_liquidity(&s, &k);
    let a = borrower(&s, &k, SOL, 3 * USD);
    let paused = |u: &User, path: &str, extra: &[(&str, String)]| {
        let f = u.signed("POST", path, extra, now_of(&k));
        k.http("POST", path, &body(&f), true)
    };
    let supply: Vec<(&str, String)> = vec![
        ("purpose", "supply".into()),
        ("pool", vault1().debt_mint.to_string()),
        ("amount", USD.to_string()),
    ];
    let deposit = |position: u64| -> Vec<(&str, String)> {
        vec![
            ("purpose", "deposit".into()),
            ("vault", "1".into()),
            ("position", position.to_string()),
            ("amount", (SOL / 10).to_string()),
        ]
    };
    let repay: Vec<(&str, String)> = vec![
        ("purpose", "repay".into()),
        ("vault", "1".into()),
        ("position", "1".into()),
        ("amount", USD.to_string()),
    ];
    let draw_of = |borrow: u64, withdraw: u64| -> Vec<(&str, String)> {
        vec![
            ("op", "draw".into()),
            ("vault", "1".into()),
            ("position", "1".into()),
            ("borrow", borrow.to_string()),
            ("withdraw", withdraw.to_string()),
        ]
    };
    assert_eq!(paused(&lender, "/lend/ticket", &supply).0, 403, "supply");
    assert_eq!(
        paused(&a, "/lend/ticket", &deposit(2)).0,
        403,
        "a new position"
    );
    assert_eq!(paused(&a, "/lend/ticket", &deposit(1)).0, 200, "a top-up");
    assert_eq!(paused(&a, "/lend/ticket", &repay).0, 200, "a repay");
    assert_eq!(
        paused(&a, "/lend/build", &draw_of(USD, 0)).0,
        403,
        "a borrow"
    );
    let (st, v) = paused(&a, "/lend/build", &draw_of(0, SOL / 10));
    assert_eq!(st, 200, "a withdrawal: {v}");
    let p = k.snapshot().unwrap().pending[0].clone();
    s.fake.land(&p);
    k.resolve().unwrap();
    s.fake.advance(21);
    let redeem: Vec<(&str, String)> = vec![
        ("op", "redeem".into()),
        ("pool", vault1().debt_mint.to_string()),
        ("amount", USD.to_string()),
    ];
    let (st, v) = paused(&lender, "/lend/build", &redeem);
    assert_eq!(st, 200, "a redeem: {v}");
    // Without the header, supply goes.
    assert_eq!(supply_ticket(&k, &lender, USD).0, 200);
}

#[test]
fn a_standby_posts_the_test_price_only_once_it_has_gone_stale() {
    let s = Setup::new(
        "standby-price",
        r#","role":"standby","price_refresh_secs":45"#,
    );
    let k = s.keeper();
    // Its own control file names another price: not posted over a fresh one.
    let want = TestPrice {
        vault: 1,
        price: 6 * DOLLAR,
        confidence: 1_000,
    };
    write_private(&price_file(&s.state), &serde_json::to_vec(&want).unwrap()).unwrap();
    assert!(!k.refresh_price().unwrap());
    s.fake.advance(59);
    assert!(!k.refresh_price().unwrap(), "the primary has 15 s more");
    s.fake.advance(1);
    assert!(k.refresh_price().unwrap());
}

#[test]
fn the_keepers_own_funds_are_checked_once_a_minute() {
    let s = Setup::new("funds", "");
    let k = s.keeper();
    with_liquidity(&s, &k);
    borrower(&s, &k, SOL, 3 * USD);
    let book = k.snapshot().unwrap();
    let r = accrued(book.reserve.as_ref().unwrap(), now_of(&k)).unwrap();
    assert_eq!(largest_debt(&book, &r).unwrap(), 3 * USD);
    let reads = || s.fake.w().held_reads;
    k.step().unwrap();
    assert_eq!(reads(), 0, "checked at the first pass, with nothing owed");
    s.fake.advance(FUNDS_CHECK_SECS);
    k.step().unwrap();
    k.step().unwrap();
    assert_eq!(reads(), 1);
    s.fake.advance(FUNDS_CHECK_SECS - 1);
    k.step().unwrap();
    assert_eq!(reads(), 1);
    s.fake.advance(1);
    k.step().unwrap();
    assert_eq!(reads(), 2);
}

/// A mainnet lend section, with `origins` and `min_position_debt`.
fn mainnet_lend(origins: &str, min_debt: u64) -> Value {
    serde_json::from_str(&format!(
        r#"{{"program_id":"{}","network":"mainnet","origins":{origins},"price":"pyth",
        "limits":{{"max_pool_size":1,"max_total_collateral":1,"max_position_collateral":1,
        "max_position_debt":1,"max_supply_per_wallet":1,"min_position_debt":{min_debt}}}}}"#,
        keeler_lend::ID
    ))
    .unwrap()
}

#[test]
fn mainnet_role_and_origin_settings_are_checked() {
    let lend = |v: Value| serde_json::from_value::<Lend>(v).unwrap();
    let main = lend(mainnet_lend(r#"["https://keeler.example"]"#, 1));
    assert!(main.check().is_ok(), "{:?}", main.check());
    assert!(
        lend(mainnet_lend(r#"["http://keeler.example"]"#, 1))
            .check()
            .is_err(),
        "http on mainnet"
    );
    assert!(
        lend(mainnet_lend(r#"["https://keeler.example"]"#, 0))
            .check()
            .is_err(),
        "no minimum debt on mainnet"
    );
    let mut uncapped = mainnet_lend(r#"["https://keeler.example"]"#, 1);
    uncapped["limits"]["max_pool_size"] = json!(0);
    assert!(
        lend(uncapped).check().is_err(),
        "a cap left at 0 on mainnet"
    );
    assert!(lend_cfg(r#","standby_grace_secs":601"#).check().is_err());
    assert!(lend_cfg(r#","role":"standby""#).check().is_ok());
    assert!(
        lend_cfg(r#","role":"standby","price_refresh_secs":61"#)
            .check()
            .is_err(),
        "a standby 15 s late would let the price pass 75 s"
    );
    let fee = crate::config::MAX_PRIORITY_FEE;
    assert!(lend_cfg(&format!(r#","liquidation_priority_fee":{fee}"#))
        .check()
        .is_ok());
    assert!(
        lend_cfg(&format!(r#","liquidation_priority_fee":{}"#, fee + 1))
            .check()
            .is_err()
    );
    let with = |rest: &str| {
        serde_json::from_str::<Lend>(&format!(
            r#"{{"program_id":"{}","network":"{NET}"{rest}}}"#,
            keeler_lend::ID
        ))
    };
    assert!(with(r#","origins":["https://a"],"role":"leader""#).is_err());
    assert!(
        with("").is_err() || with("").unwrap().check().is_err(),
        "origins are required"
    );
    for bad in [
        "[]",
        r#"["ftp://x"]"#,
        r#"["https://"]"#,
        r#"["https://a/b"]"#,
    ] {
        let c = with(&format!(r#","origins":{bad}"#)).unwrap();
        assert!(c.check().is_err(), "origins {bad}");
    }
}

#[test]
fn a_mainnet_keeper_takes_its_http_key_from_the_keychain_and_bounds_its_fee() {
    let cfg = |http_key: Value, fee: u64| -> KeeperConfig {
        serde_json::from_value(json!({
            "network": {"rpc_url": "http://127.0.0.1:8899", "indexer_url": "http://127.0.0.1:8784",
                "prover": "local", "priority_fee": fee},
            "key": {"keychain": "keeper"},
            "http": {"listen": "127.0.0.1:47000", "key": http_key},
            "lend": mainnet_lend(r#"["https://keeler.example"]"#, 1),
        }))
        .unwrap()
    };
    let keychain = json!({"keychain": "http"});
    assert!(cfg(keychain.clone(), 0).check().is_ok());
    assert!(cfg(json!({"file": "/tmp/key"}), 0).check().is_err());
    assert!(cfg(json!({"env": "HTTP_KEY"}), 0).check().is_err());
    let max = crate::config::MAX_PRIORITY_FEE;
    assert!(cfg(keychain.clone(), max).check().is_ok());
    assert!(cfg(keychain, max + 1).check().is_err());
}

#[test]
fn allow_remote_and_env_keys_are_for_devnet_only() {
    let cfg = |network: &str, helius: Value, http: Value| -> KeeperConfig {
        let mut lend = mainnet_lend(r#"["https://keeler.example"]"#, 1);
        lend["network"] = json!(network);
        let cluster = if network == "mainnet" {
            "mainnet"
        } else {
            "devnet"
        };
        serde_json::from_value(json!({
            "network": {"rpc_url": format!("https://{cluster}.helius-rpc.com"),
                "indexer_url": format!("https://{cluster}.helius-rpc.com/v1/spr"),
                "prover": {"helius": helius}},
            "key": {"env": "KEEPER_KEY"},
            "http": http,
            "lend": lend,
        }))
        .unwrap()
    };
    let env = json!({"env": "HELIUS"});
    let keychain = json!({"keychain": "helius"});
    let open = json!({"listen": "0.0.0.0:47501", "key": {"env": "HTTP"}, "allow_remote": true});
    let closed = json!({"listen": "0.0.0.0:47501", "key": {"env": "HTTP"}});
    let local = json!({"listen": "127.0.0.1:47501", "key": {"keychain": "http"}});
    assert!(cfg("devnet", env.clone(), open.clone()).check().is_ok());
    assert!(cfg("devnet", env.clone(), closed).check().is_err());
    assert!(cfg("mainnet", keychain.clone(), local.clone())
        .check()
        .is_ok());
    assert!(cfg("mainnet", env, local).check().is_err());
    let mut remote = open;
    remote["key"] = json!({"keychain": "http"});
    assert!(cfg("mainnet", keychain, remote).check().is_err());
}
