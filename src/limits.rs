//! Central resource policy for local edits and untrusted wire input.

/// Limits are deliberately carried by a document instance so a native room
/// and a browser can choose lower ceilings without changing the wire format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Maximum complete envelope accepted by any public decoder.
    pub max_message_bytes: usize,
    /// Maximum operations in one atomic update.
    pub max_operations_per_update: usize,
    /// Maximum components in an ESBT sequence path.
    pub max_identifier_depth: usize,
    /// Maximum distinct sites in a version summary.
    pub max_version_sites: usize,
    /// Maximum total sparse receipts in a version summary.
    pub max_sparse_receipts: usize,
    /// Maximum live atoms plus delete-log entries in one snapshot.
    pub max_snapshot_items: usize,
    /// Maximum causally blocked operations retained by one document.
    pub max_pending_operations: usize,
    /// Maximum released `(weight, counter)` identities retained by one document.
    pub max_deferred_deletes: usize,
    /// Maximum visible UTF-16 code units.
    pub max_document_units: usize,
    /// Maximum rightward gaps examined by one allocation.
    pub max_allocation_attempts: usize,
    /// Maximum operations retained for reconnect and delta export.
    pub max_retained_operations: usize,
    /// Maximum undo or redo transactions retained locally.
    pub max_undo_transactions: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: 16 * 1024 * 1024,
            max_operations_per_update: 100_000,
            max_identifier_depth: 1_024,
            max_version_sites: 65_536,
            max_sparse_receipts: 1_000_000,
            max_snapshot_items: 2_000_000,
            max_pending_operations: 250_000,
            max_deferred_deletes: 2_000_000,
            max_document_units: 2_000_000,
            max_allocation_attempts: 65_536,
            max_retained_operations: 4_000_000,
            max_undo_transactions: 10_000,
        }
    }
}

impl ResourceLimits {
    /// Conservative limits used by the compatibility decoders that do not
    /// receive a document-specific policy.
    pub fn wire_default() -> Self {
        Self::default()
    }
}
