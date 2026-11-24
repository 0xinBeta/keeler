//! Loads the checksum-pinned Xark artifacts and proves with them (as the swap
//! SDK does).
use std::path::PathBuf;

use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use spr_program::{
    proof::{compressed_proof, SolanaVerifyingKey},
    CompressedProof, ProgramProofInputs,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitId {
    InitReserve,
    Intent,
    CancelIntent,
    Supply,
    Redeem,
    DepositOpen, DepositAdd, Draw, Repay, Liquidate, Close,
}

impl CircuitId {
    pub const ALL: [Self; 11] = [Self::InitReserve, Self::Intent, Self::CancelIntent, Self::Supply, Self::Redeem, Self::DepositOpen, Self::DepositAdd, Self::Draw, Self::Repay, Self::Liquidate, Self::Close];

    /// The directory `zolana zk compile` writes under [`keys_dir`].
    pub fn name(self) -> &'static str {
        match self {
            Self::InitReserve => "init_reserve",
            Self::Intent => "intent",
            Self::CancelIntent => "cancel_intent",
            Self::Supply => "supply",
            Self::Redeem => "redeem",
            Self::DepositOpen => "deposit_open", Self::DepositAdd => "deposit_add", Self::Draw => "draw",
            Self::Repay => "repay", Self::Liquidate => "liquidate", Self::Close => "close",
        }
    }
}

/// `KEELER_LEND_KEYS_DIR`, else `lend/target/zk/keeler-lend`.
pub fn keys_dir() -> PathBuf {
    std::env::var_os("KEELER_LEND_KEYS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/zk/keeler-lend")
        })
}

/// The committed manifest `lend/lend-keys.CHECKSUM`, written by
/// `scripts/setup-lend.sh`.
fn artifact_checksum(id: CircuitId, artifact: &str) -> Result<[u8; 32]> {
    let filename = format!("{}/{artifact}", id.name());
    let digest = include_str!("../../lend-keys.CHECKSUM")
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            (parts.next()? == filename).then_some(hash)
        })
        .with_context(|| format!("no checksum for {filename}; run scripts/setup-lend.sh"))?;
    ensure!(digest.len() == 64, "invalid lend artifact checksum");
    let mut bytes = [0; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digest[2 * i..2 * i + 2], 16)?;
    }
    Ok(bytes)
}

fn checked_artifact(id: CircuitId, artifact: &str) -> Result<Vec<u8>> {
    let path = keys_dir().join(id.name()).join(artifact);
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading {}: run scripts/setup-lend.sh", path.display()))?;
    ensure!(
        <[u8; 32]>::from(Sha256::digest(&bytes)) == artifact_checksum(id, artifact)?,
        "lend artifact checksum mismatch: {}",
        path.display()
    );
    Ok(bytes)
}

/// The circuit from a private copy of its checked bytecode and proving key:
/// Xark rebuilds a circuit whose sources look newer, so the copy (no `deps`)
/// proves exactly the checked bytes. Keep the directory alive while proving.
pub fn checked_circuit(id: CircuitId) -> Result<(tempfile::TempDir, xark::prover::Circuit)> {
    let private = tempfile::Builder::new().prefix("lend-circuit-").tempdir()?;
    let dir = private.path().join(id.name());
    std::fs::create_dir(&dir)?;
    for artifact in ["circuit.xbc", "pk.bin"] {
        std::fs::write(dir.join(artifact), checked_artifact(id, artifact)?)?;
    }
    // Source lines only label failing constraints in error messages.
    if let Ok(sources) = std::fs::read(keys_dir().join(id.name()).join("sources.json")) {
        std::fs::write(dir.join("sources.json"), sources)?;
    }
    let circuit = xark::prover::circuit_at(&dir);
    Ok((private, circuit))
}

pub fn verifying_key(id: CircuitId) -> Result<SolanaVerifyingKey> {
    SolanaVerifyingKey::from_xark(&checked_artifact(id, "vk.bin")?).map_err(anyhow::Error::msg)
}

/// Proves `inputs` and checks the proof against the checked verifying key.
pub fn prove(id: CircuitId, inputs: &ProgramProofInputs) -> Result<CompressedProof> {
    let key = verifying_key(id)?;
    let (_private, circuit) = checked_circuit(id)?;
    let proven = circuit.prove(inputs.clone())?;
    let proof = compressed_proof(&proven.proof).map_err(anyhow::Error::msg)?;
    let encoded = inputs.encoded();
    let hash = encoded.get("public_hash").context("missing public hash")?;
    let hash = spr_program::from_decimal(hash)?;
    spr_program::verifier::verify_groth16(&proof, spr_program::to_bytes(&hash), &key.as_key())
        .map_err(|error| anyhow::anyhow!("invalid {} proof: {error:?}", id.name()))?;
    Ok(proof)
}
