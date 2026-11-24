//! Which requirement refused, natively and in the compiled circuit.
//!
//! - [`refused`]: a builder's native refusal names the failed `require`'s
//!   location; the line there must contain the expected text.
//! - [`compiled_refuses`]: valid proof inputs pass the checked compiled
//!   circuit (`proof::checked_circuit`, the committed `circuit.xbc`), and the
//!   same inputs with some leaves changed fail it at a constraint whose
//!   source line contains the expected text. The witness solver stops at the
//!   first failing constraint, so the rule is what refuses, not the public
//!   hash at the end.
//!
//! Include it with `mod refusal;`.
#![allow(dead_code)]

use std::path::Path;

use keeler_lend_sdk::{
    note_data_hash,
    proof::{checked_circuit, CircuitId},
    NoteKind, ReserveState,
};
use spr_keypair::ShieldedKeypair;
use spr_program::{DataUtxo, ProgramProofInputs};
use spr_transaction::WalletUtxo;
use xark::prover::{Fr, ProveInputs};

/// The source line `line` (1-based) of `path`: absolute, or under `lend/`.
fn source_line(path: &str, line: usize) -> Option<String> {
    let lend = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let text = std::fs::read_to_string(lend.join(path)).ok()?;
    Some(text.lines().nth(line.checked_sub(1)?)?.to_owned())
}

/// `(path, line)` of the first source location in `message`: a `….rs` path
/// and the number after it, `file.rs:line` (compiled) or `file.rs", line:
/// N` (native).
fn location(message: &str) -> Option<(String, usize)> {
    let end = message.find(".rs")? + 3;
    let start = message[..end]
        .rfind(|c: char| c.is_whitespace() || c == '"')
        .map_or(0, |i| i + 1);
    let line: String = message[end..]
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    Some((message[start..end].to_owned(), line.parse().ok()?))
}

/// Requires `message` to name a location in `file` (a path suffix, e.g.
/// `"circuits/liquidate.rs"`) whose source line contains `needle`.
#[track_caller]
fn at(message: &str, file: &str, needle: &str) {
    let (path, line) =
        location(message).unwrap_or_else(|| panic!("no source location in: {message}"));
    let text = source_line(&path, line)
        .unwrap_or_else(|| panic!("no source line {path}:{line}\n{message}"));
    assert!(
        path.ends_with(file) && text.contains(needle),
        "expected a refusal in {file} at a line with `{needle}`, got {path}:{line} `{}`\n{message}",
        text.trim()
    );
}

/// Requires `result` to be a native requirement failure at a line of `file`
/// containing `needle`.
#[track_caller]
pub fn refused<T>(result: anyhow::Result<T>, file: &str, needle: &str) {
    let Err(error) = result else {
        panic!("accepted; expected a refusal in {file} at `{needle}`");
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("RequirementFailed"),
        "not a requirement failure: {message}"
    );
    at(&message, file, needle);
}

/// Requires `result` to be a builder's own refusal (before the circuit)
/// whose message contains `text`.
#[track_caller]
pub fn refused_because<T>(result: anyhow::Result<T>, text: &str) {
    let Err(error) = result else {
        panic!("accepted; expected a refusal: {text}");
    };
    let message = format!("{error:#}");
    assert!(message.contains(text), "expected `{text}`, got: {message}");
}

/// Proof inputs as named leaves, for [`compiled_refuses`].
struct Leaves(Vec<(String, Fr)>);

impl ProveInputs for Leaves {
    fn into_inputs(self) -> Vec<(String, Fr)> {
        self.0
    }
}

pub fn leaves(inputs: &ProgramProofInputs) -> Vec<(String, Fr)> {
    inputs.clone().into_inputs()
}

/// The leaves of `from` under `prefix` (e.g. `"program.public.user_owner"`),
/// to put into other inputs.
pub fn under(from: &ProgramProofInputs, prefix: &str) -> Vec<(String, Fr)> {
    let found: Vec<_> = leaves(from)
        .into_iter()
        .filter(|(name, _)| name == prefix || name.starts_with(&format!("{prefix}.")))
        .collect();
    assert!(!found.is_empty(), "no leaf under {prefix}");
    found
}

/// `inputs` with each named leaf set to its new value.
fn changed(inputs: &ProgramProofInputs, changes: &[(String, Fr)]) -> Vec<(String, Fr)> {
    let mut all = leaves(inputs);
    let mut any = false;
    for (name, value) in changes {
        let leaf = all
            .iter_mut()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no leaf {name}"));
        any |= leaf.1 != *value;
        leaf.1 = *value;
    }
    assert!(any, "the changes change nothing");
    all
}

/// A spent note's state replaced by `state`, with the note's data hash
/// `data_hash` to match (`note_data_hash`): `state_leaf` and `utxo_leaf` are
/// e.g. `"program.private.position_state"` and `"…position_utxo"`.
pub fn note_state<S: xark::input::Input>(
    state_leaf: &str,
    state: &S,
    utxo_leaf: &str,
    data_hash: [u8; 32],
) -> Vec<(String, Fr)> {
    let mut all = xark::input::leaves(state, state_leaf);
    all.push((format!("{utxo_leaf}.data_hash"), fr(data_hash)));
    all
}

/// A spent `name` note (`"position"`, `"receipt"`, `"intent"`, `"reserve"`)
/// with `state`, and its data hash to match (`address` for the reserve).
pub fn state_leaves<S: xark::input::Input + borsh::BorshSerialize>(
    name: &str,
    kind: NoteKind,
    state: &S,
    address: Option<[u8; 32]>,
) -> Vec<(String, Fr)> {
    let bytes = borsh::to_vec(state).unwrap();
    note_state(
        &format!("program.private.{name}_state"),
        state,
        &format!("program.private.{name}_utxo"),
        note_data_hash(kind, &bytes, address.as_ref()).unwrap(),
    )
}

/// The spent reserve `r` with its state edited, at its own address or at
/// `elsewhere`.
pub fn reserve_leaves(
    r: &WalletUtxo,
    edit: impl FnOnce(&mut ReserveState),
    elsewhere: Option<[u8; 32]>,
) -> Vec<(String, Fr)> {
    let decoded = DataUtxo::<ReserveState>::try_from(r.clone()).unwrap();
    let mut state = decoded.data;
    edit(&mut state);
    let address = elsewhere.or(decoded.address);
    state_leaves("reserve", NoteKind::Reserve, &state, address)
}

/// The refusals every circuit that spends the reserve `r` shares, at `now`:
/// a reserve at another address (its data hash commits the address), and
/// one last updated after `now` (accrual cannot be skipped by moving time
/// back).
pub fn reserve_refusals<P: xark::input::Input, T>(
    c: &Compiled<'_, P, T>,
    r: &WalletUtxo,
    now: u64,
) {
    c.refuses(
        reserve_leaves(r, |_| {}, Some([1; 32])),
        "utxo/data.rs",
        "require_eq(input.data_hash, expected)",
        "spend_with_hash → require_eq",
    );
    c.refuses(
        reserve_leaves(r, |s| s.last_update = now + 1, None),
        "circuits/math.rs",
        "require(now >= state.last_update)",
        "accrue → require",
    );
}

/// The wallet's owner identity, as the program computes it from a signer.
pub fn identity_of(k: &ShieldedKeypair) -> Fr {
    keeler_lend_sdk::identity(&k.shielded_address().unwrap().solana_address().unwrap())
        .unwrap()
        .into()
}

/// 32 big-endian bytes as a field element.
pub fn fr(bytes: [u8; 32]) -> Fr {
    spr_program::from_bytes(&bytes)
        .expect("a field element")
        .into()
}

/// One leaf changed: `(name, value)`.
pub fn set(name: &str, value: impl Into<Fr>) -> Vec<(String, Fr)> {
    vec![(name.to_owned(), value.into())]
}

/// A packed leaf (`"program.public.{now,bound}"`: the fields from the low
/// bits up, each at its type's width) with the `width`-bit field at bit
/// `offset` set to `value`.
pub fn field(
    inputs: &ProgramProofInputs,
    leaf: &str,
    offset: usize,
    width: usize,
    value: u128,
) -> Vec<(String, Fr)> {
    use ark_ff::PrimeField;
    let old = leaves(inputs)
        .into_iter()
        .find(|(n, _)| n == leaf)
        .unwrap_or_else(|| panic!("no leaf {leaf}"))
        .1;
    let mut limbs = old.into_bigint();
    for bit in 0..width {
        let (limb, at) = ((offset + bit) / 64, (offset + bit) % 64);
        let on = bit < 128 && (value >> bit) & 1 == 1;
        limbs.0[limb] = (limbs.0[limb] & !(1 << at)) | (u64::from(on) << at);
    }
    let new = Fr::from_bigint(limbs).expect("a field element");
    vec![(leaf.to_owned(), new)]
}

/// Requires the compiled circuit `id` to accept `inputs`, and to refuse them
/// with `changes`:
///
/// - natively, the program `P` read from the same changed leaves fails at a
///   `require` in `file` whose source line contains `needle`;
/// - the compiled circuit fails at a constraint whose call chain (the
///   diagnostic's `in a → b` line) ends with `chain`, e.g. `"execute →
///   require"`, so it refuses at the rule and not only at the public hash
///   check at the end, which any changed witness would also fail. A
///   non-zero check compiles to an inverse with no call chain; `chain` is
///   then `"→ check <leaf>"`, the input the diagnostic names.
pub struct Compiled<'a, P, T> {
    pub id: CircuitId,
    pub inputs: &'a ProgramProofInputs,
    pub execute: fn(&P) -> Result<T, spr_program::CircuitError>,
}

impl<P: xark::input::Input, T> Compiled<'_, P, T> {
    /// See [`compiled_refuses`].
    #[track_caller]
    pub fn refuses(&self, changes: Vec<(String, Fr)>, file: &str, needle: &str, chain: &str) {
        compiled_refuses(
            self.id,
            self.inputs,
            changes,
            self.execute,
            file,
            needle,
            chain,
        );
    }
}

#[track_caller]
pub fn compiled_refuses<P: xark::input::Input, T>(
    id: CircuitId,
    inputs: &ProgramProofInputs,
    changes: Vec<(String, Fr)>,
    execute: fn(&P) -> Result<T, spr_program::CircuitError>,
    file: &str,
    needle: &str,
    chain: &str,
) {
    std::env::set_var("NO_COLOR", "1");
    let bad = changed(inputs, &changes);
    let fields: Vec<spr_program::Field> = bad
        .iter()
        .filter(|(name, _)| name != "public_hash")
        .map(|(_, v)| (*v).into())
        .collect();
    let program: P = xark::input::from_fields(&fields);
    let native = spr_program::checked(|| execute(&program).map(|_| ()));
    let native = native.expect_err("changed inputs accepted natively");
    at(&format!("{native:?}"), file, needle);

    let (_dir, circuit) = checked_circuit(id).expect("the checked circuit");
    circuit
        .check_satisfied(Leaves(leaves(inputs)))
        .unwrap_or_else(|e| panic!("{}: valid inputs refused: {e}", id.name()));
    let error = circuit
        .check_satisfied(Leaves(bad))
        .err()
        .unwrap_or_else(|| panic!("{}: changed inputs accepted", id.name()))
        .to_string();
    let line = error
        .lines()
        .find_map(|l| l.trim().strip_prefix("in "))
        .unwrap_or_default();
    assert!(
        error.contains("witness does not satisfy the circuit")
            && (line.ends_with(chain) || (chain.starts_with("→ check ") && error.contains(chain))),
        "{}: expected a failure in `… {chain}`, got `{line}`:\n{error}",
        id.name()
    );
}
