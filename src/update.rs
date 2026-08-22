//! Canonical retry-safe update batches and room-facing application receipts.

use crate::clock::Version;
use crate::codec::Reader;
use crate::error::{EngineError, ErrorCode};
use crate::limits::ResourceLimits;
use crate::op::Op;
use crate::weight::SiteId;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationRef {
    pub origin: SiteId,
    pub sequence: u64,
}

impl OperationRef {
    pub const fn new(origin: SiteId, sequence: u64) -> Self {
        Self { origin, sequence }
    }
}

impl From<&Op> for OperationRef {
    fn from(operation: &Op) -> Self {
        Self::new(operation.origin, operation.seq)
    }
}

/// One ordered, retry-safe journal record.
///
/// Operations are encoded in ascending `(origin, sequence)` order. Their ESBT
/// effects do not depend on transport order, and requiring one order gives a
/// single byte representation for a transaction or reconnect delta.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Update {
    operations: Vec<Op>,
}

impl Update {
    pub fn new(mut operations: Vec<Op>) -> Result<Self, EngineError> {
        operations.sort_by_key(|operation| (operation.origin, operation.seq));
        Self::from_canonical_operations(operations)
    }

    fn from_canonical_operations(operations: Vec<Op>) -> Result<Self, EngineError> {
        let mut previous = None;
        for operation in &operations {
            let identity = (operation.origin, operation.seq);
            if operation.origin == 0 || operation.seq == 0 || operation.counter == 0 {
                return Err(EngineError::new(
                    ErrorCode::InvalidOperation,
                    "update contains a zero operation identity",
                ));
            }
            if previous.is_some_and(|value| value >= identity) {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "update operations are duplicated or out of order",
                ));
            }
            previous = Some(identity);
        }
        Ok(Self { operations })
    }

    pub fn operations(&self) -> &[Op] {
        &self.operations
    }

    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Payload encoding used inside the versioned `ESBM` envelope.
    pub(crate) fn encode_payload(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.operations.len() as u32).to_le_bytes());
        for operation in &self.operations {
            let encoded = operation.encode();
            out.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            out.extend_from_slice(&encoded);
        }
        out
    }

    pub(crate) fn decode_payload(
        bytes: &[u8],
        limits: &ResourceLimits,
    ) -> Result<Self, EngineError> {
        Self::decode_payload_with_limit(bytes, limits, limits.max_operations_per_update)
    }

    pub(crate) fn decode_payload_with_limit(
        bytes: &[u8],
        limits: &ResourceLimits,
        maximum_operations: usize,
    ) -> Result<Self, EngineError> {
        let mut reader = Reader::new(bytes);
        let count = reader.u32()? as usize;
        if count > maximum_operations {
            return Err(EngineError::new(
                ErrorCode::TooManyOperations,
                "operation collection exceeds resource policy",
            ));
        }
        // Every operation has a four-byte length and a nonempty body. Reject
        // impossible counts before allocating attacker-controlled capacity.
        if count > reader.remaining() / 5 {
            return Err(EngineError::malformed("impossible update operation count"));
        }

        let mut operations = Vec::with_capacity(count);
        for _ in 0..count {
            let length = reader.u32()? as usize;
            if length == 0 || length > reader.remaining() {
                return Err(EngineError::malformed("invalid operation length in update"));
            }
            operations.push(Op::decode_with_limits(reader.take(length)?, limits)?);
        }
        if !reader.is_finished() {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "update contains trailing bytes",
            ));
        }
        Self::from_canonical_operations(operations)
    }

    pub fn identities(&self) -> BTreeSet<OperationRef> {
        self.operations.iter().map(OperationRef::from).collect()
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Every newly accepted operation was integrated or deterministically
    /// suppressed during this call.
    Applied = 1,
    /// The update contained no new operation identities.
    Duplicate = 2,
    /// Every new operation remains causally blocked.
    Buffered = 3,
    /// The update combined applied, buffered, and/or duplicate operations.
    Mixed = 4,
    /// A canonical empty update.
    Noop = 5,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyReceipt {
    pub outcome: ApplyOutcome,
    /// Identities first admitted to this replica by this call.
    pub accepted_operations: Vec<OperationRef>,
    /// Newly admitted identities no longer pending after the call.
    pub applied_operations: Vec<OperationRef>,
    /// Newly admitted identities still waiting for causal prerequisites.
    pub buffered_operations: Vec<OperationRef>,
    /// Previously buffered identities that became ready because of this call.
    pub newly_ready_operations: Vec<OperationRef>,
    pub version: Version,
    pub visible_changed: bool,
    /// Exact canonical bytes a durable room should append. `None` means the
    /// update was empty or entirely duplicate and needs no second journal row.
    pub journal_bytes: Option<Vec<u8>>,
}

impl ApplyReceipt {
    /// Stable binary receipt returned by the Wasm ABI.
    ///
    /// `[version:u16][outcome:u8][visible:u8]`, four operation-ref lists,
    /// encoded version summary, and optional canonical journal bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_le_bytes());
        out.push(self.outcome as u8);
        out.push(u8::from(self.visible_changed));
        for identities in [
            &self.accepted_operations,
            &self.applied_operations,
            &self.buffered_operations,
            &self.newly_ready_operations,
        ] {
            out.extend_from_slice(&(identities.len() as u32).to_le_bytes());
            for identity in identities {
                out.extend_from_slice(&identity.origin.to_le_bytes());
                out.extend_from_slice(&identity.sequence.to_le_bytes());
            }
        }
        let version = self.version.encode();
        out.extend_from_slice(&(version.len() as u32).to_le_bytes());
        out.extend_from_slice(&version);
        if let Some(journal) = &self.journal_bytes {
            out.extend_from_slice(&(journal.len() as u32).to_le_bytes());
            out.extend_from_slice(journal);
        } else {
            out.extend_from_slice(&0u32.to_le_bytes());
        }
        out
    }
}
