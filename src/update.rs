//! Canonical retry-safe update batches and room-facing application receipts.

use crate::clock::Version;
use crate::codec::{
    longest_common_prefix, read_weight_parts, write_uvarint, write_weight_parts, Reader,
    SiteContext,
};
use crate::error::{EngineError, ErrorCode};
use crate::limits::ResourceLimits;
use crate::op::{Op, OpKind};
use crate::weight::SiteId;
use std::collections::BTreeSet;

/// Smallest possible encoded operation in an update payload:
/// tag, origin index, seq, counter, and a minimal weight (flags, p, q).
const MIN_PAYLOAD_OP_BYTES: usize = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationRef {
    pub origin: SiteId,
    pub sequence: u64,
}

/// One visible UTF-16 replacement, in coordinates of the document state
/// produced by the preceding edit in the same receipt. It is deliberately
/// separate from CRDT operations: consumers can patch an editor or preview
/// without decoding weights, scanning, or copying the whole document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleEdit {
    pub from: usize,
    pub to: usize,
    pub inserted: Vec<u16>,
}

impl VisibleEdit {
    pub fn new(from: usize, to: usize, inserted: Vec<u16>) -> Self {
        Self { from, to, inserted }
    }
}

/// Coalesce the overwhelmingly common typing/deletion runs while preserving
/// sequential coordinates for disjoint concurrent changes.
pub(crate) fn push_visible_edit(edits: &mut Vec<VisibleEdit>, edit: VisibleEdit) {
    if edit.from == edit.to && edit.inserted.is_empty() {
        return;
    }
    if let Some(previous) = edits.last_mut() {
        // Append a character to an insertion or replacement that was just
        // applied. `edit.from` is in the post-previous coordinate space.
        if edit.from == edit.to
            && !edit.inserted.is_empty()
            && edit.from == previous.from.saturating_add(previous.inserted.len())
        {
            previous.inserted.extend(edit.inserted);
            return;
        }
        // Repeatedly deleting at one cursor is one replacement against the
        // pre-run state.
        if edit.inserted.is_empty()
            && previous.inserted.is_empty()
            && edit.from == previous.from
            && edit.to >= edit.from
        {
            previous.to = previous.to.saturating_add(edit.to - edit.from);
            return;
        }
        // A replace is implemented as delete then insert at the same cursor.
        if previous.inserted.is_empty()
            && previous.to > previous.from
            && edit.from == edit.to
            && edit.from == previous.from
        {
            previous.inserted.extend(edit.inserted);
            return;
        }
    }
    edits.push(edit);
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

    /// Payload encoding used inside the versioned `ESBM` envelope
    /// (format v3, Extension 2 follow-up).
    ///
    /// Every 16-byte site appears once in a sorted dictionary; operations
    /// reference it by varint index and are self-delimiting, so the per-op
    /// length prefix of earlier formats is gone. Because operations are
    /// canonically sorted by `(origin, seq)`, a typing run's weights are
    /// adjacent, and each sequence path is front-coded against its
    /// predecessor exactly as snapshot atoms are.
    pub(crate) fn encode_payload(&self) -> Vec<u8> {
        let mut sites = BTreeSet::new();
        for operation in &self.operations {
            sites.insert(operation.origin);
            sites.insert(operation.weight.site);
        }
        let sites: Vec<SiteId> = sites.into_iter().collect();

        let mut out = Vec::new();
        write_uvarint(&mut out, sites.len() as u64);
        for site in &sites {
            out.extend_from_slice(&site.to_le_bytes());
        }
        write_uvarint(&mut out, self.operations.len() as u64);
        let mut previous_path: &[u32] = &[];
        for operation in &self.operations {
            out.push(match operation.kind {
                OpKind::Ins { .. } => 1,
                OpKind::Del => 2,
            });
            let origin_index = sites
                .binary_search(&operation.origin)
                .expect("site table covers every origin");
            write_uvarint(&mut out, origin_index as u64);
            write_uvarint(&mut out, operation.seq);
            write_uvarint(&mut out, operation.counter);
            let shared = longest_common_prefix(previous_path, &operation.weight.sc);
            write_weight_parts(
                &mut out,
                &operation.weight,
                SiteContext::Table(&sites),
                Some(shared),
            );
            if let OpKind::Ins { unit } = operation.kind {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            previous_path = &operation.weight.sc;
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
        let site_count = reader.uvarint()? as usize;
        if site_count > limits.max_version_sites || site_count > reader.remaining() / 16 {
            return Err(EngineError::new(
                ErrorCode::TooManyVersionSites,
                "update site table exceeds its bounds",
            ));
        }
        let mut sites = Vec::with_capacity(site_count);
        let mut previous_site = None;
        for _ in 0..site_count {
            let site = reader.u128()?;
            if site == 0 || previous_site.is_some_and(|previous| previous >= site) {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "update site table is zero, duplicated, or out of order",
                ));
            }
            previous_site = Some(site);
            sites.push(site);
        }
        let mut referenced_sites = vec![false; sites.len()];

        let count = reader.uvarint()? as usize;
        if count > maximum_operations {
            return Err(EngineError::new(
                ErrorCode::TooManyOperations,
                "operation collection exceeds resource policy",
            ));
        }
        // Every operation occupies at least MIN_PAYLOAD_OP_BYTES. Reject
        // impossible counts before allocating attacker-controlled capacity.
        if count > reader.remaining() / MIN_PAYLOAD_OP_BYTES {
            return Err(EngineError::malformed("impossible update operation count"));
        }

        let mut operations = Vec::with_capacity(count);
        let mut previous_path: Vec<u32> = Vec::new();
        for _ in 0..count {
            let tag = reader.u8()?;
            let origin_index = reader.uvarint()? as usize;
            let origin = *sites.get(origin_index).ok_or_else(|| {
                EngineError::malformed("operation references a missing site table entry")
            })?;
            referenced_sites[origin_index] = true;
            let seq = reader.uvarint()?;
            let counter = reader.uvarint()?;
            let weight = read_weight_parts(
                &mut reader,
                limits,
                SiteContext::Table(&sites),
                Some(&previous_path),
            )?;
            if let Ok(index) = sites.binary_search(&weight.site) {
                referenced_sites[index] = true;
            }
            previous_path = weight.sc.clone();
            let operation = match tag {
                1 => {
                    let unit = reader.u16()?;
                    Op::ins(weight, unit, counter, origin, seq)
                }
                2 => Op::del(weight, counter, origin, seq),
                _ => {
                    return Err(EngineError::new(
                        ErrorCode::InvalidOperation,
                        "unknown operation tag in update",
                    ))
                }
            };
            operations.push(operation);
        }
        if !reader.is_finished() {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "update contains trailing bytes",
            ));
        }
        if referenced_sites.iter().any(|used| !used) {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "update site table has unused entries",
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
    /// Exact visible replacements caused by this apply, without a full-text
    /// materialization. Buffered or duplicate operations contribute none.
    pub visible_edits: Vec<VisibleEdit>,
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
