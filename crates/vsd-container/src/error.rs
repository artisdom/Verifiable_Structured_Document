use thiserror::Error;

pub type Result<T> = std::result::Result<T, ContainerError>;

#[derive(Debug, Error)]
pub enum ContainerError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("not a VSD file (bad magic); possibly corrupted in transfer")]
    BadMagic,

    #[error("unsupported VSD major version {0} (reader supports {1})")]
    UnsupportedMajor(u16, u16),

    #[error("file size mismatch: header says {expected} bytes, file has {actual} — truncated or appended-to")]
    SizeMismatch { expected: u64, actual: u64 },

    #[error("chunk {fourcc}: payload checksum mismatch — corrupted")]
    ChecksumMismatch { fourcc: String },

    #[error("chunk structure: {0}")]
    Structure(String),

    #[error("unknown critical chunk {0}; refusing to skip")]
    UnknownCritical(String),

    #[error("compressed chunk but zstd support not compiled in")]
    ZstdUnavailable,

    #[error("zstd: {0}")]
    Zstd(String),

    #[error(transparent)]
    Core(#[from] vsd_core::Error),
}
