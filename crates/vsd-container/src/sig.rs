//! Signature wire format (spec §7.1). The cryptographic operations live
//! in `vsd-sign`; this module defines what goes in the SIGS chunk.
//!
//! A signature's target is a Merkle root over *meaning* — the manifest
//! hash (whole document) or a subtree object id — never byte ranges.
//! Container repacking and recompression cannot invalidate one.

use vsd_core::cbor::{MapBuilder, Value};
use vsd_core::object::ObjectId;
use vsd_core::Error as CoreError;

use crate::error::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigScope {
    /// Target is the manifest hash (the document id).
    Document,
    /// Target is a content-tree subtree object id.
    Subtree,
    /// Target is a filled field-layer object id.
    FieldLayer,
}

impl SigScope {
    pub fn as_str(self) -> &'static str {
        match self {
            SigScope::Document => "document",
            SigScope::Subtree => "subtree",
            SigScope::FieldLayer => "field-layer",
        }
    }

    pub fn parse(s: &str) -> Result<SigScope> {
        Ok(match s {
            "document" => SigScope::Document,
            "subtree" => SigScope::Subtree,
            "field-layer" => SigScope::FieldLayer,
            other => {
                return Err(CoreError::Schema(format!("unknown signature scope {other:?}")).into())
            }
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigAlg {
    Ed25519,
    /// Reserved (spec lists ecdsa-p256 and ml-dsa-65 for PQ readiness);
    /// not yet implemented by `vsd-sign`.
    EcdsaP256,
    MlDsa65,
}

impl SigAlg {
    pub fn as_str(self) -> &'static str {
        match self {
            SigAlg::Ed25519 => "ed25519",
            SigAlg::EcdsaP256 => "ecdsa-p256",
            SigAlg::MlDsa65 => "ml-dsa-65",
        }
    }

    pub fn parse(s: &str) -> Result<SigAlg> {
        Ok(match s {
            "ed25519" => SigAlg::Ed25519,
            "ecdsa-p256" => SigAlg::EcdsaP256,
            "ml-dsa-65" => SigAlg::MlDsa65,
            other => {
                return Err(
                    CoreError::Schema(format!("unknown signature algorithm {other:?}")).into(),
                )
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    pub scope: SigScope,
    pub target: ObjectId,
    pub alg: SigAlg,
    /// Raw public key (Ed25519: 32 bytes). X.509 chains are carried in
    /// `cert` when present; v0.1 verifies against the raw key.
    pub pubkey: Vec<u8>,
    pub cert: Option<Vec<u8>>,
    /// RFC 3161 token, if timestamped.
    pub timestamp: Option<Vec<u8>>,
    pub sig: Vec<u8>,
}

impl Signature {
    pub fn to_value(&self) -> Value {
        MapBuilder::new()
            .put("scope", Value::text(self.scope.as_str()))
            .put("target", self.target.to_value())
            .put("alg", Value::text(self.alg.as_str()))
            .put("pubkey", Value::Bytes(self.pubkey.clone()))
            .put_opt("cert", self.cert.clone().map(Value::Bytes))
            .put_opt("timestamp", self.timestamp.clone().map(Value::Bytes))
            .put("sig", Value::Bytes(self.sig.clone()))
            .build()
    }

    pub fn from_value(v: &Value) -> Result<Signature> {
        let text = |key: &str| -> Result<String> {
            v.get(key)
                .and_then(Value::as_text)
                .map(str::to_owned)
                .ok_or_else(|| CoreError::Schema(format!("signature: missing {key:?}")).into())
        };
        let bytes = |key: &str| -> Result<Vec<u8>> {
            v.get(key)
                .and_then(Value::as_bytes)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| CoreError::Schema(format!("signature: missing {key:?}")).into())
        };
        let opt_bytes = |key: &str| -> Result<Option<Vec<u8>>> {
            match v.get(key) {
                None => Ok(None),
                Some(x) => x.as_bytes().map(|b| Some(b.to_vec())).ok_or_else(|| {
                    CoreError::Schema(format!("signature: {key:?} must be bytes")).into()
                }),
            }
        };
        Ok(Signature {
            scope: SigScope::parse(&text("scope")?)?,
            target: ObjectId::from_value(
                v.get("target")
                    .ok_or_else(|| CoreError::Schema("signature: missing target".into()))?,
            )?,
            alg: SigAlg::parse(&text("alg")?)?,
            pubkey: bytes("pubkey")?,
            cert: opt_bytes("cert")?,
            timestamp: opt_bytes("timestamp")?,
            sig: bytes("sig")?,
        })
    }

    pub fn block_to_payload(sigs: &[Signature]) -> Result<Vec<u8>> {
        let v = Value::Array(sigs.iter().map(Signature::to_value).collect());
        Ok(v.encode()?)
    }

    pub fn block_from_payload(payload: &[u8]) -> Result<Vec<Signature>> {
        let v = Value::decode(payload)?;
        v.as_array()
            .ok_or_else(|| CoreError::Schema("SIGS payload must be an array".into()))?
            .iter()
            .map(Signature::from_value)
            .collect()
    }
}
