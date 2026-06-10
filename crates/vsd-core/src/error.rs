use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("CBOR: {0}")]
    Cbor(String),

    #[error("schema: {0}")]
    Schema(String),

    #[error("object {0} not found in store")]
    ObjectNotFound(crate::object::ObjectId),

    #[error("object hash mismatch: claimed {claimed}, actual {actual}")]
    HashMismatch {
        claimed: crate::object::ObjectId,
        actual: crate::object::ObjectId,
    },

    #[error("validation failed: {0}")]
    Validation(String),

    #[error("node path {0:?} does not resolve to a node")]
    BadNodePath(Vec<usize>),

    #[error("form expression: {0}")]
    Expr(String),

    #[error("unsupported VSD version {major}.{minor} (reader supports major {supported})")]
    UnsupportedVersion {
        major: u16,
        minor: u16,
        supported: u16,
    },
}
