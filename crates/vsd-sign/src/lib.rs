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
    #[cfg(feature = "keygen")]
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
    let msg = message(sig.scope, &sig.target);
    match sig.alg {
        SigAlg::Ed25519 => {
            let key = VerifyingKey::from_bytes(&sig.pubkey)?;
            let raw: [u8; 64] =
                sig.sig.as_slice().try_into().map_err(|_| {
                    SignError::InvalidKey("ed25519 signature must be 64 bytes".into())
                })?;
            let dalek_sig = ed25519_dalek::Signature::from_bytes(&raw);
            if key.0.verify(&msg, &dalek_sig).is_err() {
                return Ok(Verdict::Invalid("ed25519 verification failed".into()));
            }
        }
        SigAlg::HybridEd25519MlDsa65 => {
            if let Err(reason) = hybrid::verify_hybrid(&sig.pubkey, &sig.sig, &msg) {
                return Ok(Verdict::Invalid(reason));
            }
        }
        other => return Err(SignError::UnsupportedAlg(other.as_str().into())),
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

// ---------------------------------------------------------------------------
// Hybrid classical + post-quantum signatures (ROADMAP 5b)
// ---------------------------------------------------------------------------

/// Hybrid Ed25519 + ML-DSA-65 (FIPS 204) signing key.
///
/// Wire layout: `pubkey = ed25519_pk(32) ‖ mldsa_pk(1952)`,
/// `sig = ed25519_sig(64) ‖ mldsa_sig(3309)`. Verification requires
/// **both** components over the identical message — an attacker must
/// break Ed25519 *and* ML-DSA. Hybrid-by-default is the posture for
/// documents that must outlive the quantum transition.
pub struct HybridSigningKey {
    ed: ed25519_dalek::SigningKey,
    ml_sk: fips204::ml_dsa_65::PrivateKey,
    ml_pk_bytes: Vec<u8>,
}

pub mod hybrid {
    use fips204::traits::{SerDes, Verifier as _};

    pub const ED_PK_LEN: usize = 32;
    pub const ED_SIG_LEN: usize = 64;
    pub const ML_PK_LEN: usize = fips204::ml_dsa_65::PK_LEN; // 1952
    pub const ML_SK_LEN: usize = fips204::ml_dsa_65::SK_LEN; // 4032
    pub const ML_SIG_LEN: usize = fips204::ml_dsa_65::SIG_LEN; // 3309
    pub const PK_LEN: usize = ED_PK_LEN + ML_PK_LEN;
    pub const SIG_LEN: usize = ED_SIG_LEN + ML_SIG_LEN;

    /// Empty FIPS 204 context: domain separation already lives in the
    /// VSD message prefix, identically for both component algorithms.
    const ML_CTX: &[u8] = &[];

    #[cfg(feature = "keygen")]
    pub(crate) fn sign_ml(
        sk: &fips204::ml_dsa_65::PrivateKey,
        msg: &[u8],
    ) -> Result<[u8; ML_SIG_LEN], String> {
        use fips204::traits::Signer as _;
        sk.try_sign(msg, ML_CTX).map_err(|e| e.to_string())
    }

    /// Verify a concatenated hybrid signature; Err carries the reason.
    pub fn verify_hybrid(pubkey: &[u8], sig: &[u8], msg: &[u8]) -> Result<(), String> {
        if pubkey.len() != PK_LEN {
            return Err(format!("hybrid pubkey must be {PK_LEN} bytes"));
        }
        if sig.len() != SIG_LEN {
            return Err(format!("hybrid signature must be {SIG_LEN} bytes"));
        }
        // Classical component.
        let ed_pk: [u8; ED_PK_LEN] = pubkey[..ED_PK_LEN].try_into().unwrap();
        let ed_pk = ed25519_dalek::VerifyingKey::from_bytes(&ed_pk)
            .map_err(|e| format!("ed25519 component key: {e}"))?;
        let ed_sig: [u8; ED_SIG_LEN] = sig[..ED_SIG_LEN].try_into().unwrap();
        ed25519_dalek::Verifier::verify(
            &ed_pk,
            msg,
            &ed25519_dalek::Signature::from_bytes(&ed_sig),
        )
        .map_err(|_| "ed25519 component failed".to_string())?;
        // Post-quantum component.
        let ml_pk: [u8; ML_PK_LEN] = pubkey[ED_PK_LEN..].try_into().unwrap();
        let ml_pk = fips204::ml_dsa_65::PublicKey::try_from_bytes(ml_pk)
            .map_err(|e| format!("ml-dsa component key: {e}"))?;
        let ml_sig: [u8; ML_SIG_LEN] = sig[ED_SIG_LEN..].try_into().unwrap();
        if !ml_pk.verify(msg, &ml_sig, ML_CTX) {
            return Err("ml-dsa-65 component failed".into());
        }
        Ok(())
    }
}

impl HybridSigningKey {
    /// Generate from the OS CSPRNG.
    #[cfg(feature = "keygen")]
    pub fn generate() -> Result<HybridSigningKey> {
        use fips204::traits::{KeyGen, SerDes};
        let mut rng = rand_core::OsRng;
        let ed = ed25519_dalek::SigningKey::generate(&mut rng);
        let (ml_pk, ml_sk) = fips204::ml_dsa_65::KG::try_keygen_with_rng(&mut rng)
            .map_err(|e| SignError::InvalidKey(e.to_string()))?;
        Ok(HybridSigningKey {
            ed,
            ml_sk,
            ml_pk_bytes: ml_pk.into_bytes().to_vec(),
        })
    }

    /// Secret-key serialization:
    /// `ed25519_seed(32) ‖ mldsa_sk(4032) ‖ mldsa_pk(1952)`.
    pub fn to_bytes(&self) -> Vec<u8> {
        use fips204::traits::SerDes;
        let mut out = Vec::with_capacity(32 + hybrid::ML_SK_LEN + hybrid::ML_PK_LEN);
        out.extend_from_slice(&self.ed.to_bytes());
        out.extend_from_slice(&self.ml_sk.clone().into_bytes());
        out.extend_from_slice(&self.ml_pk_bytes);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<HybridSigningKey> {
        use fips204::traits::SerDes;
        let expect = 32 + hybrid::ML_SK_LEN + hybrid::ML_PK_LEN;
        if bytes.len() != expect {
            return Err(SignError::InvalidKey(format!(
                "hybrid secret key must be {expect} bytes, got {}",
                bytes.len()
            )));
        }
        let seed: [u8; 32] = bytes[..32].try_into().unwrap();
        let ml_sk_bytes: [u8; hybrid::ML_SK_LEN] =
            bytes[32..32 + hybrid::ML_SK_LEN].try_into().unwrap();
        let ml_sk = fips204::ml_dsa_65::PrivateKey::try_from_bytes(ml_sk_bytes)
            .map_err(|e| SignError::InvalidKey(e.to_string()))?;
        Ok(HybridSigningKey {
            ed: ed25519_dalek::SigningKey::from_bytes(&seed),
            ml_sk,
            ml_pk_bytes: bytes[32 + hybrid::ML_SK_LEN..].to_vec(),
        })
    }

    /// Public key: `ed25519_pk(32) ‖ mldsa_pk(1952)`.
    pub fn public_key_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(hybrid::PK_LEN);
        out.extend_from_slice(&self.ed.verifying_key().to_bytes());
        out.extend_from_slice(&self.ml_pk_bytes);
        out
    }

    /// (FIPS 204 hedged signing draws randomness, so hybrid *signing*
    /// sits behind the `keygen` feature; verification never does.)
    #[cfg(feature = "keygen")]
    pub fn sign_document(&self, doc: &Document) -> Result<Signature> {
        let target = doc.document_id()?;
        self.sign_target(SigScope::Document, target)
    }

    #[cfg(feature = "keygen")]
    pub fn sign_subtree(&self, subtree: ObjectId) -> Result<Signature> {
        self.sign_target(SigScope::Subtree, subtree)
    }

    #[cfg(feature = "keygen")]
    fn sign_target(&self, scope: SigScope, target: ObjectId) -> Result<Signature> {
        let msg = message(scope, &target);
        let ed_sig = self.ed.sign(&msg);
        let ml_sig = hybrid::sign_ml(&self.ml_sk, &msg).map_err(SignError::InvalidKey)?;
        let mut sig = Vec::with_capacity(hybrid::SIG_LEN);
        sig.extend_from_slice(&ed_sig.to_bytes());
        sig.extend_from_slice(&ml_sig);
        Ok(Signature {
            scope,
            target,
            alg: SigAlg::HybridEd25519MlDsa65,
            pubkey: self.public_key_bytes(),
            cert: None,
            timestamp: None,
            sig,
        })
    }
}

// ---------------------------------------------------------------------------
// X.509 certificate binding (ROADMAP 5a, partial)
// ---------------------------------------------------------------------------

/// What a certificate carried in `Signature::cert` asserts about the
/// signing key, after checking it actually binds that key.
#[derive(Debug, Clone)]
pub struct CertBinding {
    pub subject: String,
    pub issuer: String,
    /// Validity window, seconds since the Unix epoch. The *verifier*
    /// supplies the comparison time — the format has no clock.
    pub not_before: u64,
    pub not_after: u64,
    pub self_signed: bool,
}

/// Parse the certificate attached to a signature (DER, or PEM if it
/// starts with `-----`) and check that its SubjectPublicKeyInfo carries
/// exactly the signature's key (for hybrid signatures: the Ed25519
/// component, which is what today's PKI can certify).
///
/// This is *binding*, not full path validation: chain building to a
/// trust anchor, revocation, and policy checking remain open (ROADMAP
/// 5a). What this rules out is the cheap lie — presenting someone
/// else's certificate next to your key.
pub fn check_cert_binding(sig: &Signature, at_unix: Option<u64>) -> Result<CertBinding> {
    use der::Decode;

    let cert_bytes = sig
        .cert
        .as_deref()
        .ok_or_else(|| SignError::InvalidKey("signature carries no certificate".into()))?;

    let der_bytes;
    let der_slice: &[u8] = if cert_bytes.starts_with(b"-----") {
        let pem = std::str::from_utf8(cert_bytes)
            .map_err(|_| SignError::InvalidKey("PEM certificate is not UTF-8".into()))?;
        let (label, doc) = der::pem::decode_vec(pem.as_bytes())
            .map_err(|e| SignError::InvalidKey(format!("PEM decode: {e}")))?;
        if label != "CERTIFICATE" {
            return Err(SignError::InvalidKey(format!(
                "expected CERTIFICATE PEM, got {label}"
            )));
        }
        der_bytes = doc;
        &der_bytes
    } else {
        cert_bytes
    };

    let cert = x509_cert::Certificate::from_der(der_slice)
        .map_err(|e| SignError::InvalidKey(format!("X.509 parse: {e}")))?;
    let tbs = &cert.tbs_certificate;

    // The key the certificate certifies must be the signing key.
    let spki_key = tbs
        .subject_public_key_info
        .subject_public_key
        .as_bytes()
        .ok_or_else(|| SignError::InvalidKey("SPKI key is not byte-aligned".into()))?;
    let bound_key: &[u8] = match sig.alg {
        SigAlg::Ed25519 => &sig.pubkey,
        SigAlg::HybridEd25519MlDsa65 => &sig.pubkey[..hybrid::ED_PK_LEN.min(sig.pubkey.len())],
        _ => &sig.pubkey,
    };
    if spki_key != bound_key {
        return Err(SignError::InvalidKey(
            "certificate does not certify the signing key (SPKI mismatch)".into(),
        ));
    }

    let not_before = tbs.validity.not_before.to_unix_duration().as_secs();
    let not_after = tbs.validity.not_after.to_unix_duration().as_secs();
    if let Some(now) = at_unix {
        if now < not_before || now > not_after {
            return Err(SignError::InvalidKey(format!(
                "certificate not valid at t={now} (window {not_before}..{not_after})"
            )));
        }
    }

    Ok(CertBinding {
        subject: tbs.subject.to_string(),
        issuer: tbs.issuer.to_string(),
        not_before,
        not_after,
        self_signed: tbs.subject == tbs.issuer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_sign_verify_and_tamper() {
        let key = HybridSigningKey::generate().unwrap();
        let target = ObjectId([5u8; 32]);
        let sig = key.sign_subtree(target).unwrap();
        assert_eq!(sig.alg, SigAlg::HybridEd25519MlDsa65);
        assert_eq!(sig.pubkey.len(), hybrid::PK_LEN);
        assert_eq!(sig.sig.len(), hybrid::SIG_LEN);

        let msg = message(SigScope::Subtree, &target);
        assert!(hybrid::verify_hybrid(&sig.pubkey, &sig.sig, &msg).is_ok());

        // Tampering with EITHER component must fail.
        let mut bad_ed = sig.sig.clone();
        bad_ed[0] ^= 1;
        assert!(hybrid::verify_hybrid(&sig.pubkey, &bad_ed, &msg).is_err());
        let mut bad_ml = sig.sig.clone();
        bad_ml[hybrid::ED_SIG_LEN + 10] ^= 1;
        assert!(hybrid::verify_hybrid(&sig.pubkey, &bad_ml, &msg).is_err());
        // Wrong scope (domain separation) must fail.
        let other = message(SigScope::Document, &target);
        assert!(hybrid::verify_hybrid(&sig.pubkey, &sig.sig, &other).is_err());
    }

    #[test]
    fn hybrid_key_roundtrips_through_bytes() {
        let key = HybridSigningKey::generate().unwrap();
        let restored = HybridSigningKey::from_bytes(&key.to_bytes()).unwrap();
        assert_eq!(key.public_key_bytes(), restored.public_key_bytes());
        // The restored key produces verifiable signatures.
        let target = ObjectId([9u8; 32]);
        let sig = restored.sign_subtree(target).unwrap();
        let msg = message(SigScope::Subtree, &target);
        assert!(hybrid::verify_hybrid(&sig.pubkey, &sig.sig, &msg).is_ok());
    }

    /// Hand-assemble a self-signed Ed25519 certificate for `key`.
    fn self_signed_cert(key: &ed25519_dalek::SigningKey, subject: &str) -> Vec<u8> {
        use der::asn1::{BitString, UtcTime};
        use der::{Decode, Encode};
        use std::str::FromStr;
        use x509_cert::certificate::{CertificateInner, TbsCertificateInner, Version};
        use x509_cert::name::Name;
        use x509_cert::serial_number::SerialNumber;
        use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};
        use x509_cert::time::{Time, Validity};

        let ed25519_oid = der::asn1::ObjectIdentifier::new_unwrap("1.3.101.112");
        let alg = AlgorithmIdentifierOwned {
            oid: ed25519_oid,
            parameters: None,
        };
        let name = Name::from_str(subject).unwrap();
        let tbs: TbsCertificateInner = TbsCertificateInner {
            version: Version::V3,
            serial_number: SerialNumber::new(&[1]).unwrap(),
            signature: alg.clone(),
            issuer: name.clone(),
            validity: Validity {
                // 2020-01-01 .. 2040-01-01
                not_before: Time::UtcTime(
                    UtcTime::from_unix_duration(std::time::Duration::from_secs(1_577_836_800))
                        .unwrap(),
                ),
                not_after: Time::UtcTime(
                    UtcTime::from_unix_duration(std::time::Duration::from_secs(2_208_988_800))
                        .unwrap(),
                ),
            },
            subject: name,
            subject_public_key_info: SubjectPublicKeyInfoOwned {
                algorithm: alg.clone(),
                subject_public_key: BitString::from_bytes(&key.verifying_key().to_bytes()).unwrap(),
            },
            issuer_unique_id: None,
            subject_unique_id: None,
            extensions: None,
        };
        let tbs_der = tbs.to_der().unwrap();
        let cert_sig = key.sign(&tbs_der);
        let cert: CertificateInner = CertificateInner {
            tbs_certificate: TbsCertificateInner::from_der(&tbs_der).unwrap(),
            signature_algorithm: alg,
            signature: BitString::from_bytes(&cert_sig.to_bytes()).unwrap(),
        };
        cert.to_der().unwrap()
    }

    #[test]
    fn cert_binding_checks_spki_and_window() {
        let key = SigningKey::generate();
        let cert = self_signed_cert(&key.0, "CN=VSD Test Signer");
        let target = ObjectId([3u8; 32]);
        let mut sig = key.sign_subtree(target);
        sig.cert = Some(cert);

        // Inside the validity window: binding holds.
        let binding = check_cert_binding(&sig, Some(1_700_000_000)).unwrap();
        assert!(binding.subject.contains("VSD Test Signer"));
        assert!(binding.self_signed);

        // Outside the window: rejected.
        assert!(check_cert_binding(&sig, Some(2_300_000_000)).is_err());
        // No time supplied: window reported but not enforced.
        assert!(check_cert_binding(&sig, None).is_ok());

        // A certificate for a DIFFERENT key must be rejected.
        let other = SigningKey::generate();
        sig.cert = Some(self_signed_cert(&other.0, "CN=Mallory"));
        let err = check_cert_binding(&sig, None).unwrap_err();
        assert!(err.to_string().contains("SPKI mismatch"), "{err}");
    }

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
