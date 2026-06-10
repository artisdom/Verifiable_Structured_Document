//! # vsd-tlog
//!
//! A transparency log for VSD document identities (ROADMAP 5d):
//! an append-only Merkle tree in the style of RFC 6962 (Certificate
//! Transparency), hashed with BLAKE3.
//!
//! What it buys: "this contract existed, in exactly this form, at the
//! time tree head N was signed" — without the log ever holding the
//! content. Entries are 32-byte document ids; the document id is itself
//! a Merkle commitment to every byte of content, so an inclusion proof
//! chains all the way from a signed tree head down to individual
//! paragraphs. Redaction proofs (spec §7.2) anchor to predecessors that
//! can be logged here.
//!
//! Hashing (RFC 6962 §2.1, domain-separated):
//! `leaf = BLAKE3(0x00 ‖ entry)`, `node = BLAKE3(0x01 ‖ left ‖ right)`.
//! The empty tree's head is `BLAKE3("")`.

#![forbid(unsafe_code)]

use std::io::Write as _;
use std::path::Path;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, TlogError>;

#[derive(Debug, Error)]
pub enum TlogError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("not a VSD transparency log (bad magic)")]
    BadMagic,

    #[error("log file is corrupt: {0}")]
    Corrupt(String),

    #[error("invalid proof: {0}")]
    BadProof(String),

    #[error("invalid argument: {0}")]
    BadArgument(String),
}

const MAGIC: &[u8; 8] = b"VSDTLOG1";
const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;

/// Domain separation for signed tree heads.
const STH_DOMAIN: &[u8] = b"VSD-TLOG-v1\0";

pub type Hash = [u8; 32];

fn leaf_hash(entry: &[u8; 32]) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(&[LEAF_PREFIX]);
    h.update(entry);
    *h.finalize().as_bytes()
}

fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(&[NODE_PREFIX]);
    h.update(left);
    h.update(right);
    *h.finalize().as_bytes()
}

/// The largest power of two strictly less than `n` (n ≥ 2).
fn split_point(n: usize) -> usize {
    let mut k = 1usize;
    while k * 2 < n {
        k *= 2;
    }
    k
}

/// An append-only log of 32-byte entries (document ids).
pub struct Log {
    entries: Vec<[u8; 32]>,
}

impl Log {
    pub fn new() -> Log {
        Log {
            entries: Vec::new(),
        }
    }

    pub fn size(&self) -> u64 {
        self.entries.len() as u64
    }

    pub fn entries(&self) -> &[[u8; 32]] {
        &self.entries
    }

    /// Append an entry; returns its index.
    pub fn append(&mut self, entry: [u8; 32]) -> u64 {
        self.entries.push(entry);
        self.entries.len() as u64 - 1
    }

    /// Merkle tree head over the first `n` entries (RFC 6962 MTH).
    pub fn root_at(&self, n: u64) -> Result<Hash> {
        let n = usize::try_from(n).map_err(|_| TlogError::BadArgument("size".into()))?;
        if n > self.entries.len() {
            return Err(TlogError::BadArgument(format!(
                "size {n} exceeds log size {}",
                self.entries.len()
            )));
        }
        Ok(mth(&self.entries[..n]))
    }

    pub fn root(&self) -> Hash {
        mth(&self.entries)
    }

    /// RFC 6962 §2.1.1 audit path for `index` within the first `n`
    /// entries.
    pub fn inclusion_proof(&self, index: u64, n: u64) -> Result<Vec<Hash>> {
        let (index, n) = (index as usize, n as usize);
        if n > self.entries.len() || index >= n {
            return Err(TlogError::BadArgument("index/size out of range".into()));
        }
        Ok(audit_path(index, &self.entries[..n]))
    }

    /// RFC 6962 §2.1.2 consistency proof between tree sizes `m` and `n`.
    pub fn consistency_proof(&self, m: u64, n: u64) -> Result<Vec<Hash>> {
        let (m, n) = (m as usize, n as usize);
        if n > self.entries.len() || m > n || m == 0 {
            return Err(TlogError::BadArgument("sizes out of range".into()));
        }
        Ok(consistency(m, &self.entries[..n]))
    }

    // --- persistence ------------------------------------------------------

    pub fn load(path: impl AsRef<Path>) -> Result<Log> {
        let bytes = std::fs::read(path)?;
        if bytes.len() < 8 || &bytes[..8] != MAGIC {
            return Err(TlogError::BadMagic);
        }
        let body = &bytes[8..];
        if body.len() % 32 != 0 {
            return Err(TlogError::Corrupt(
                "entry region not a multiple of 32".into(),
            ));
        }
        let entries = body
            .chunks_exact(32)
            .map(|c| {
                let mut e = [0u8; 32];
                e.copy_from_slice(c);
                e
            })
            .collect();
        Ok(Log { entries })
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let mut f = std::fs::File::create(path)?;
        f.write_all(MAGIC)?;
        for e in &self.entries {
            f.write_all(e)?;
        }
        f.sync_all()?;
        Ok(())
    }
}

impl Default for Log {
    fn default() -> Self {
        Self::new()
    }
}

fn mth(entries: &[[u8; 32]]) -> Hash {
    match entries.len() {
        0 => *blake3::hash(b"").as_bytes(),
        1 => leaf_hash(&entries[0]),
        n => {
            let k = split_point(n);
            node_hash(&mth(&entries[..k]), &mth(&entries[k..]))
        }
    }
}

fn audit_path(index: usize, entries: &[[u8; 32]]) -> Vec<Hash> {
    let n = entries.len();
    if n <= 1 {
        return Vec::new();
    }
    let k = split_point(n);
    if index < k {
        let mut path = audit_path(index, &entries[..k]);
        path.push(mth(&entries[k..]));
        path
    } else {
        let mut path = audit_path(index - k, &entries[k..]);
        path.push(mth(&entries[..k]));
        path
    }
}

/// RFC 6962 SUBPROOF(m, D[n], true).
fn consistency(m: usize, entries: &[[u8; 32]]) -> Vec<Hash> {
    fn subproof(m: usize, entries: &[[u8; 32]], complete: bool) -> Vec<Hash> {
        let n = entries.len();
        if m == n {
            return if complete {
                Vec::new()
            } else {
                vec![mth(entries)]
            };
        }
        let k = split_point(n);
        if m <= k {
            let mut p = subproof(m, &entries[..k], complete);
            p.push(mth(&entries[k..]));
            p
        } else {
            let mut p = subproof(m - k, &entries[k..], false);
            p.push(mth(&entries[..k]));
            p
        }
    }
    subproof(m, entries, true)
}

/// Verify an RFC 6962 §2.1.3 inclusion proof.
pub fn verify_inclusion(
    entry: &[u8; 32],
    index: u64,
    tree_size: u64,
    proof: &[Hash],
    root: &Hash,
) -> Result<()> {
    if index >= tree_size {
        return Err(TlogError::BadProof("index past tree size".into()));
    }
    let mut fn_ = index;
    let mut sn = tree_size - 1;
    let mut r = leaf_hash(entry);
    for p in proof {
        if sn == 0 {
            return Err(TlogError::BadProof("proof too long".into()));
        }
        if fn_ & 1 == 1 || fn_ == sn {
            r = node_hash(p, &r);
            while fn_ & 1 == 0 {
                fn_ >>= 1;
                sn >>= 1;
            }
        } else {
            r = node_hash(&r, p);
        }
        fn_ >>= 1;
        sn >>= 1;
    }
    if sn != 0 {
        return Err(TlogError::BadProof("proof too short".into()));
    }
    if r != *root {
        return Err(TlogError::BadProof("computed root does not match".into()));
    }
    Ok(())
}

/// Verify an RFC 6962 §2.1.4 consistency proof: the tree of size `n`
/// with head `new_root` is an append-only extension of the tree of size
/// `m` with head `old_root`.
pub fn verify_consistency(
    m: u64,
    n: u64,
    old_root: &Hash,
    new_root: &Hash,
    proof: &[Hash],
) -> Result<()> {
    if m == 0 || m > n {
        return Err(TlogError::BadProof("invalid sizes".into()));
    }
    if m == n {
        if proof.is_empty() && old_root == new_root {
            return Ok(());
        }
        return Err(TlogError::BadProof(
            "equal sizes must have equal roots".into(),
        ));
    }
    // If m is a power of two its old root appears implicitly.
    let mut proof = proof.to_vec();
    if m.is_power_of_two() {
        proof.insert(0, *old_root);
    }
    if proof.is_empty() {
        return Err(TlogError::BadProof("empty proof".into()));
    }

    let mut fn_ = m - 1;
    let mut sn = n - 1;
    while fn_ & 1 == 1 {
        fn_ >>= 1;
        sn >>= 1;
    }
    let mut fr = proof[0];
    let mut sr = proof[0];
    for p in &proof[1..] {
        if sn == 0 {
            return Err(TlogError::BadProof("proof too long".into()));
        }
        if fn_ & 1 == 1 || fn_ == sn {
            fr = node_hash(p, &fr);
            sr = node_hash(p, &sr);
            while fn_ & 1 == 0 && fn_ != 0 {
                fn_ >>= 1;
                sn >>= 1;
            }
        } else {
            sr = node_hash(&sr, p);
        }
        fn_ >>= 1;
        sn >>= 1;
    }
    if sn != 0 {
        return Err(TlogError::BadProof("proof too short".into()));
    }
    if fr != *old_root {
        return Err(TlogError::BadProof("old root mismatch".into()));
    }
    if sr != *new_root {
        return Err(TlogError::BadProof("new root mismatch".into()));
    }
    Ok(())
}

/// A signed tree head: the log operator's Ed25519 attestation that the
/// log had exactly this head at this size.
#[derive(Debug, Clone, PartialEq)]
pub struct SignedTreeHead {
    pub size: u64,
    pub root: Hash,
    pub pubkey: [u8; 32],
    pub sig: [u8; 64],
}

fn sth_message(size: u64, root: &Hash) -> Vec<u8> {
    let mut msg = Vec::with_capacity(STH_DOMAIN.len() + 8 + 32);
    msg.extend_from_slice(STH_DOMAIN);
    msg.extend_from_slice(&size.to_le_bytes());
    msg.extend_from_slice(root);
    msg
}

impl SignedTreeHead {
    /// Sign with a raw 32-byte Ed25519 seed (as `vsd keygen` stores).
    pub fn sign_with_seed(log: &Log, seed: &[u8; 32]) -> SignedTreeHead {
        Self::sign(log, &ed25519_dalek::SigningKey::from_bytes(seed))
    }

    pub fn sign(log: &Log, key: &ed25519_dalek::SigningKey) -> SignedTreeHead {
        use ed25519_dalek::Signer;
        let size = log.size();
        let root = log.root();
        let sig = key.sign(&sth_message(size, &root));
        SignedTreeHead {
            size,
            root,
            pubkey: key.verifying_key().to_bytes(),
            sig: sig.to_bytes(),
        }
    }

    pub fn verify(&self) -> Result<()> {
        use ed25519_dalek::Verifier;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.pubkey)
            .map_err(|e| TlogError::BadProof(format!("tree head key: {e}")))?;
        let sig = ed25519_dalek::Signature::from_bytes(&self.sig);
        key.verify(&sth_message(self.size, &self.root), &sig)
            .map_err(|_| TlogError::BadProof("tree head signature invalid".into()))
    }

    /// Serialize: `size_le(8) ‖ root(32) ‖ pubkey(32) ‖ sig(64)`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + 32 + 32 + 64);
        out.extend_from_slice(&self.size.to_le_bytes());
        out.extend_from_slice(&self.root);
        out.extend_from_slice(&self.pubkey);
        out.extend_from_slice(&self.sig);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<SignedTreeHead> {
        if bytes.len() != 8 + 32 + 32 + 64 {
            return Err(TlogError::Corrupt("tree head must be 136 bytes".into()));
        }
        Ok(SignedTreeHead {
            size: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            root: bytes[8..40].try_into().unwrap(),
            pubkey: bytes[40..72].try_into().unwrap(),
            sig: bytes[72..136].try_into().unwrap(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(i: u8) -> [u8; 32] {
        [i; 32]
    }

    #[test]
    fn inclusion_proofs_verify_for_every_index_and_size() {
        let mut log = Log::new();
        for size in 1..=20u8 {
            log.append(entry(size));
            let n = log.size();
            let root = log.root();
            for index in 0..n {
                let proof = log.inclusion_proof(index, n).unwrap();
                verify_inclusion(&entry(index as u8 + 1), index, n, &proof, &root)
                    .unwrap_or_else(|e| panic!("size {n} index {index}: {e}"));
                // The wrong entry must not verify.
                assert!(
                    verify_inclusion(&entry(99), index, n, &proof, &root).is_err(),
                    "size {n} index {index}: forged entry accepted"
                );
            }
        }
    }

    #[test]
    fn consistency_proofs_verify_across_all_growth_steps() {
        let mut log = Log::new();
        for i in 1..=20u8 {
            log.append(entry(i));
        }
        let n = log.size();
        let new_root = log.root();
        for m in 1..=n {
            let old_root = log.root_at(m).unwrap();
            let proof = log.consistency_proof(m, n).unwrap();
            verify_consistency(m, n, &old_root, &new_root, &proof)
                .unwrap_or_else(|e| panic!("m={m} n={n}: {e}"));
            // A different old root must fail.
            if m < n {
                assert!(
                    verify_consistency(m, n, &entry(7), &new_root, &proof).is_err(),
                    "m={m}: forged history accepted"
                );
            }
        }
    }

    #[test]
    fn tamper_with_history_is_detected() {
        let mut log = Log::new();
        for i in 1..=8u8 {
            log.append(entry(i));
        }
        let old_root = log.root_at(4).unwrap();
        // A "log" that rewrote entry 2 after the fact:
        let mut forged = Log::new();
        forged.append(entry(1));
        forged.append(entry(42)); // rewritten!
        for i in 3..=8u8 {
            forged.append(entry(i));
        }
        let proof = forged.consistency_proof(4, 8).unwrap();
        assert!(
            verify_consistency(4, 8, &old_root, &forged.root(), &proof).is_err(),
            "append-only violation must be detectable"
        );
    }

    #[test]
    fn signed_tree_head_roundtrip() {
        let mut log = Log::new();
        log.append(entry(1));
        log.append(entry(2));
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let sth = SignedTreeHead::sign(&log, &key);
        sth.verify().unwrap();

        let restored = SignedTreeHead::from_bytes(&sth.to_bytes()).unwrap();
        assert_eq!(restored, sth);

        let mut bad = sth.clone();
        bad.size += 1;
        assert!(bad.verify().is_err(), "size is covered by the signature");
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = std::env::temp_dir().join("vsd-tlog-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.tlog");
        let mut log = Log::new();
        for i in 1..=5u8 {
            log.append(entry(i));
        }
        log.save(&path).unwrap();
        let loaded = Log::load(&path).unwrap();
        assert_eq!(loaded.entries(), log.entries());
        assert_eq!(loaded.root(), log.root());
        std::fs::remove_file(&path).ok();
    }
}
