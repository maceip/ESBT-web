//! Stable, typed failures returned by the production document API.

use core::fmt;

/// Machine-readable error code shared by native Rust and the Wasm adapter.
///
/// Values are explicit because WIT lifts them into the typed `engine-error`
/// record and products may persist them in diagnostics.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidSiteId = 1,
    InvalidRange = 2,
    AllocationExhausted = 3,
    MalformedEncoding = 4,
    UnsupportedFormatVersion = 5,
    NonCanonicalEncoding = 6,
    MessageTooLarge = 7,
    TooManyOperations = 8,
    IdentifierTooDeep = 9,
    TooManyVersionSites = 10,
    TooManySparseReceipts = 11,
    TooManySnapshotItems = 12,
    TooManyPendingOperations = 13,
    TooManyDeferredDeletes = 14,
    DocumentTooLarge = 15,
    IntegerOverflow = 16,
    OperationIdentityConflict = 17,
    SnapshotHasSequenceGaps = 18,
    SnapshotNotCausallyClosed = 19,
    MissingLocalHistory = 20,
    HistoryUnavailable = 21,
    TransactionAlreadyActive = 22,
    NoActiveTransaction = 23,
    InvalidHandle = 24,
    InvalidAnchor = 25,
    UndoUnavailable = 26,
    RedoUnavailable = 27,
    InvalidOperation = 28,
    SnapshotStateConflict = 29,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineError {
    pub code: ErrorCode,
    pub detail: &'static str,
}

impl EngineError {
    pub const fn new(code: ErrorCode, detail: &'static str) -> Self {
        Self { code, detail }
    }

    pub const fn malformed(detail: &'static str) -> Self {
        Self::new(ErrorCode::MalformedEncoding, detail)
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "esbt {:?}: {}", self.code, self.detail)
    }
}

impl std::error::Error for EngineError {}
