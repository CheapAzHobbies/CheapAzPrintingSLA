//! Error types for the conversion engine.
//!
//! Every message here is written to be shown to a user (§28): it says what
//! went wrong in plain language. Technical detail belongs in the `context`
//! field, which the GUI shows under "Details".

use std::path::PathBuf;

/// Result alias used throughout the engine.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Format(#[from] FormatError),

    #[error("this file is not in a format CheapAzSLA recognises")]
    UnknownFormat,

    #[error("{from} files cannot be converted to {to}")]
    UnsupportedConversion { from: String, to: String },

    #[error("layer {index} is out of range (the file has {count} layers)")]
    LayerOutOfRange { index: u32, count: u32 },
}

impl Error {
    /// One line saying what went wrong, in the words someone would use to
    /// describe it to a colleague.
    ///
    /// `Display` gives the particulars - byte offsets, expected lengths - and
    /// those belong in the panel someone opens on purpose. This is for the
    /// glance: it says which of the handful of things that can go wrong did,
    /// and nothing else.
    pub fn headline(&self) -> &'static str {
        match self {
            Error::Io { .. } => "This file could not be read from disk",
            Error::UnknownFormat => "This is not a print file CheapAzSLA knows",
            Error::UnsupportedConversion { .. } => "This pair of formats cannot be converted",
            Error::LayerOutOfRange { .. } => "That layer is not in this file",
            Error::Format(f) => f.headline(),
        }
    }
}

/// A problem with the contents of a print file.
///
/// Input files are untrusted (§42). These variants exist so a malformed file
/// produces a clear message instead of a panic or a silent misread.
#[derive(Debug, Clone, thiserror::Error)]
pub enum FormatError {
    #[error("the file is truncated: expected {expected} bytes at offset {offset}, found {actual}")]
    Truncated {
        offset: u64,
        expected: u64,
        actual: u64,
    },

    #[error("the file header is not valid for this format")]
    BadMagic,

    #[error("unsupported {format} version {version}")]
    UnsupportedVersion { format: String, version: String },

    #[error("a required value is missing from the file: {0}")]
    MissingField(String),

    #[error("a value in the file is not usable: {field} = {value} ({reason})")]
    InvalidValue {
        field: String,
        value: String,
        reason: String,
    },

    #[error("the file points to data outside itself (offset {offset}, length {length}, file is {file_size} bytes)")]
    OffsetOutOfBounds {
        offset: u64,
        length: u64,
        file_size: u64,
    },

    #[error("the file declares {declared} bytes of layer data, which is beyond the {limit} byte safety limit")]
    AllocationTooLarge { declared: u64, limit: u64 },

    #[error("layer data could not be decoded: {0}")]
    LayerDecode(String),

    #[error("{0}")]
    Other(String),
}

impl FormatError {
    /// The same glance-level line, for a problem inside the file rather than
    /// with the file. Every one of these means the same thing to the person
    /// holding it - the file is not going to open - so they say what is wrong
    /// with it without asking anyone to care about offsets.
    pub fn headline(&self) -> &'static str {
        match self {
            FormatError::Truncated { .. } => "This file is incomplete",
            FormatError::BadMagic => "This file is not the format it claims to be",
            FormatError::UnsupportedVersion { .. } => "This file is a version not yet supported",
            FormatError::MissingField(_)
            | FormatError::InvalidValue { .. }
            | FormatError::OffsetOutOfBounds { .. }
            | FormatError::AllocationTooLarge { .. }
            | FormatError::LayerDecode(_)
            | FormatError::Other(_) => "This file is damaged and cannot be read",
        }
    }
}
