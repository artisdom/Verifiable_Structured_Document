//! # vsd-sign
//!
//! Signing and verification for VSD (spec §7.1).
//!
//! Signatures cover **Merkle roots of meaning**: the manifest hash
//! (whole document) or a subtree object id. They survive container
//! repacking, recompression, and chunk reordering — none of which a PDF
//! byte-range signature tolerates.
//!
//! v0.1 implements Ed25519 over raw public keys. The wire format
//! reserves `ecdsa-p256` and `ml-dsa-65` (post-quantum) algorithm tags
//! and an X.509 `cert` field for PKI-anchored deployments.

#![forbid(unsafe_code)]

use ed25519_dalek::{Signer, Verifier};
use thiserror::Error;

use vsd_container::{SigAlg, SigScope, Signature};
use vsd_core::object::ObjectId;
use vsd_core::Document;

/// Domain-separation prefix: a VSD signature can never be confused with
/// a signature over the same 32 bytes in another protocol.
const DOMAIN: &[u8] = b"VSD-SIG-v1\0";

pub type Result<T> = std::result::Result<T, SignError>;

#[derive(Debug, Error)]
pub enum SignError {
    #[error("invalid key: {0}")]
    InvalidKey(String),

    #[error("signature verification failed")]
    BadSignature,

    #[error("algorithm {0} not implemented in this version")]
    UnsupportedAlg(String),

    #[error(transparent)]
    Core(#[from] vsd_core::Error),
}

/// An Ed25519 signing key (32-byte seed).
pub struct SigningKey(ed25519_dalek::SigningKey);

/// An Ed25519 verifying (public) key.
pub struct VerifyingKey(ed25519_dalek::VerifyingKey);

impl SigningKey {
    /// Generate from the OS CSPRNG.
    pub fn generate() -> SigningKey {
        let mut rng = rand_core::OsRng;
        SigningKey(ed25519_dalek::SigningKey::generate(&mut rng))
    }

    pub fn from_seed(seed: &[u8]) -> Result<SigningKey> {
        let seed: [u8; 32] = seed
            .try_into()
            .map_err(|_| SignError::InvalidKey("seed must be 32 bytes".into()))?;
        Ok(SigningKey(ed25519_dalek::SigningKey::from_bytes(&seed)))
    }

    pub fn seed(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        VerifyingKey(self.0.verifying_key())
    }

    /// Sign a whole document: the target is the manifest hash, i.e. a
    /// 32-byte commitment to every byte of content.
    pub fn sign_document(&self, doc: &Document) -> Result<Signature> {
        let target = doc.document_id()?;
        Ok(self.sign_target(SigScope::Document, target))
    }

    /// Sign a subtree: party A can sign §3 (the schedule of fees) while
    /// party B signs the whole document, independently verifiable.
    pub fn sign_subtree(&self, subtree: ObjectId) -> Signature {
        self.sign_target(SigScope::Subtree, subtree)
    }

    fn sign_target(&self, scope: SigScope, target: ObjectId) -> Signature {
        let msg = message(scope, &target);
        let sig = self.0.sign(&msg);
        Signature {
            scope,
            target,
            alg: SigAlg::Ed25519,
            pubkey: self.0.verifying_key().to_bytes().to_vec(),
            cert: None,
            timestamp: None,
            sig: sig.to_bytes().to_vec(),
        }
    }
}

impl VerifyingKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<VerifyingKey> {
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| SignError::InvalidKey("public key must be 32 bytes".into()))?;
        ed25519_dalek::VerifyingKey::from_bytes(&arr)
            .map(VerifyingKey)
            .map_err(|e| SignError::InvalidKey(e.to_string()))
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
}

fn message(scope: SigScope, target: &ObjectId) -> Vec<u8> {
    let mut msg = Vec::with_capacity(DOMAIN.len() + 1 + 32);
    msg.extend_from_slice(DOMAIN);
    msg.push(match scope {
        SigScope::Document => 0,
        SigScope::Subtree => 1,
        SigScope::FieldLayer => 2,
    });
    msg.extend_from_slice(target.as_slice());
    msg
}

/// Outcome of verifying one signature against a document.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Cryptographically valid and the target matches this document
    /// (or an object present in it, for subtree scope).
    Valid,
    /// Cryptographically valid, but the target is not this document —
    /// e.g. a signature carried over from a *predecessor* in an
    /// amendment chain. Valid history, not an endorsement of this revision.
    ValidForOtherTarget,
    /// The cryptography itself fails.
    Invalid(String),
}

/// Verify a signature in the context of a document.
pub fn verify(doc: &Document, sig: &Signature) -> Result<Verdict> {
    if sig.alg != SigAlg::Ed25519 {
        return Err(SignError::UnsupportedAlg(sig.alg.as_str().into()));
    }
    let key = VerifyingKey::from_bytes(&sig.pubkey)?;
    let raw: [u8; 64] = sig
        .sig
        .as_slice()
        .try_into()
        .map_err(|_| SignError::InvalidKey("ed25519 signature must be 64 bytes".into()))?;
    let dalek_sig = ed25519_dalek::Signature::from_bytes(&raw);
    let msg = message(sig.scope, &sig.target);
    if key.0.verify(&msg, &dalek_sig).is_err() {
        return Ok(Verdict::Invalid("ed25519 verification failed".into()));
    }

    // Cryptography holds; now bind the target to *this* document.
    let matches = match sig.scope {
        SigScope::Document => doc.document_id()? == sig.target,
        // A subtree or field-layer signature endorses an object that must
        // exist in (be reachable content of) this document.
        SigScope::Subtree | SigScope::FieldLayer => doc.store.contains(&sig.target),
    };
    Ok(if matches {
        Verdict::Valid
    } else {
        Verdict::ValidForOtherTarget
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip_on_raw_target() {
        let key = SigningKey::generate();
        let target = ObjectId([7u8; 32]);
        let sig = key.sign_subtree(target);
        let vkey = VerifyingKey::from_bytes(&sig.pubkey).unwrap();
        let raw: [u8; 64] = sig.sig.as_slice().try_into().unwrap();
        let ds = ed25519_dalek::Signature::from_bytes(&raw);
        assert!(vkey
            .0
            .verify(&message(SigScope::Subtree, &target), &ds)
            .is_ok());
        // Domain separation: same bytes under document scope must fail.
        assert!(vkey
            .0
            .verify(&message(SigScope::Document, &target), &ds)
            .is_err());
    }
}
