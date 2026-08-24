//! Join/leave state transfer. Paper §4: sites may join at any time.
//! Epidemic of live ops is not enough for a late replica; it needs the
//! current document plus the delete log L (Scenario 2).

use crate::clock::Version;
use crate::codec::{
    longest_common_prefix, read_weight_parts, write_uvarint, write_weight_parts, Reader,
    SiteContext,
};
use crate::error::{EngineError, ErrorCode};
use crate::limits::ResourceLimits;
use crate::op::Op;
use crate::update::{OperationRef, Update};
use crate::weight::{SiteId, Weight};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Smallest possible encoded atom: flags, p, q, site index, unit, counter.
const MIN_ATOM_BYTES: usize = 7;
/// Smallest possible encoded delete-log entry: flags, p, q, site, counter.
const MIN_DELETE_BYTES: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Atom {
    pub weight: Weight,
    pub unit: u16,
    pub counter: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub atoms: Vec<Atom>,
    /// Tombstones retained only while the targeted insertion counter has not
    /// arrived. Once that insertion is received, its operation receipt makes
    /// future retries idempotent and the tombstone can be discarded.
    pub delete_log: Vec<(Weight, u64)>,
    pub version: Version,
    /// Gap-aware insertion-counter receipts. Insertion counters are independent
    /// from transport operation sequences because deletes also consume the
    /// latter. Tracking both prevents counter reuse after snapshots while still
    /// distinguishing an unseen counter from a later sparse receipt.
    pub insertions: Version,
}

impl Snapshot {
    /// Sorted, strictly ascending table of every site owning a weight in
    /// this snapshot. Both weight lists reference it by varint index.
    fn site_table(&self) -> Vec<SiteId> {
        let mut sites = BTreeSet::new();
        for atom in &self.atoms {
            sites.insert(atom.weight.site);
        }
        for (weight, _) in &self.delete_log {
            sites.insert(weight.site);
        }
        sites.into_iter().collect()
    }

    pub fn encode(&self) -> Vec<u8> {
        crate::wire::Artifact::CompactSnapshot(self.clone()).encode()
    }

    pub(crate) fn encode_payload(&self) -> Vec<u8> {
        let mut b = Vec::new();
        let insertions = self.insertions.encode_payload();
        b.extend_from_slice(&(insertions.len() as u32).to_le_bytes());
        b.extend_from_slice(&insertions);

        let sites = self.site_table();
        write_uvarint(&mut b, sites.len() as u64);
        for site in &sites {
            b.extend_from_slice(&site.to_le_bytes());
        }

        // Atoms are canonically sorted by weight, so consecutive sequence
        // paths share long prefixes (typing runs share their entire root).
        // Front-code each path against its predecessor (Extension 2).
        write_uvarint(&mut b, self.atoms.len() as u64);
        let mut previous_path: &[u32] = &[];
        for a in &self.atoms {
            let shared = longest_common_prefix(previous_path, &a.weight.sc);
            write_weight_parts(&mut b, &a.weight, SiteContext::Table(&sites), Some(shared));
            b.extend_from_slice(&a.unit.to_le_bytes());
            write_uvarint(&mut b, a.counter);
            previous_path = &a.weight.sc;
        }

        write_uvarint(&mut b, self.delete_log.len() as u64);
        let mut previous_path: &[u32] = &[];
        for (w, c) in &self.delete_log {
            let shared = longest_common_prefix(previous_path, &w.sc);
            write_weight_parts(&mut b, w, SiteContext::Table(&sites), Some(shared));
            write_uvarint(&mut b, *c);
            previous_path = &w.sc;
        }

        let ve = self.version.encode_payload();
        b.extend_from_slice(&(ve.len() as u32).to_le_bytes());
        b.extend_from_slice(&ve);
        b
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        Self::decode_with_limits(buf, &ResourceLimits::wire_default()).ok()
    }

    pub fn decode_with_limits(buf: &[u8], limits: &ResourceLimits) -> Result<Self, EngineError> {
        match crate::wire::Artifact::decode_with_limits(buf, limits)? {
            crate::wire::Artifact::CompactSnapshot(snapshot) => Ok(snapshot),
            _ => Err(EngineError::new(
                ErrorCode::MalformedEncoding,
                "expected an ESBT compact snapshot artifact",
            )),
        }
    }

    pub(crate) fn decode_payload_with_limits(
        buf: &[u8],
        limits: &ResourceLimits,
    ) -> Result<Self, EngineError> {
        if buf.len() > limits.max_message_bytes {
            return Err(EngineError::new(
                ErrorCode::MessageTooLarge,
                "snapshot exceeds message byte limit",
            ));
        }
        let mut reader = Reader::new(buf);
        let insertion_length = reader.u32()? as usize;
        let insertions =
            Version::decode_payload_with_limits(reader.take(insertion_length)?, limits)?;

        let site_count = reader.uvarint()? as usize;
        if site_count > limits.max_version_sites || site_count > reader.remaining() / 16 {
            return Err(EngineError::new(
                ErrorCode::TooManyVersionSites,
                "snapshot site table exceeds its bounds",
            ));
        }
        let mut sites = Vec::with_capacity(site_count);
        let mut previous_site = None;
        for _ in 0..site_count {
            let site = reader.u128()?;
            if site == 0 || previous_site.is_some_and(|previous| previous >= site) {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "snapshot site table is zero, duplicated, or out of order",
                ));
            }
            previous_site = Some(site);
            sites.push(site);
        }
        let mut referenced_sites = vec![false; sites.len()];

        let na = reader.uvarint()? as usize;
        if na > limits.max_snapshot_items {
            return Err(EngineError::new(
                ErrorCode::TooManySnapshotItems,
                "snapshot has too many live atoms",
            ));
        }
        // Bound the allocation by
        // the bytes actually present before trusting the declared count.
        if na > reader.remaining() / MIN_ATOM_BYTES {
            return Err(EngineError::malformed("impossible snapshot atom count"));
        }
        let mut atoms = Vec::with_capacity(na);
        let mut previous_weight: Option<Weight> = None;
        let mut previous_path: Vec<u32> = Vec::new();
        for _ in 0..na {
            let w = read_weight_parts(
                &mut reader,
                limits,
                SiteContext::Table(&sites),
                Some(&previous_path),
            )?;
            if let Ok(index) = sites.binary_search(&w.site) {
                referenced_sites[index] = true;
            }
            let unit = reader.u16()?;
            let c = reader.uvarint()?;
            if c == 0
                || previous_weight
                    .as_ref()
                    .is_some_and(|previous| previous >= &w)
            {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "snapshot atoms are not canonical",
                ));
            }
            previous_weight = Some(w.clone());
            previous_path = w.sc.clone();
            atoms.push(Atom {
                weight: w,
                unit,
                counter: c,
            });
        }

        let nd = reader.uvarint()? as usize;
        let total_items = na.checked_add(nd).ok_or_else(|| {
            EngineError::new(ErrorCode::IntegerOverflow, "snapshot item count overflow")
        })?;
        if total_items > limits.max_snapshot_items || nd > limits.max_deferred_deletes {
            return Err(EngineError::new(
                ErrorCode::TooManySnapshotItems,
                "snapshot delete log exceeds resource policy",
            ));
        }
        if nd > reader.remaining() / MIN_DELETE_BYTES {
            return Err(EngineError::malformed(
                "impossible snapshot delete-log count",
            ));
        }
        let mut delete_log = Vec::with_capacity(nd);
        let mut previous_delete: Option<(Weight, u64)> = None;
        let mut previous_path: Vec<u32> = Vec::new();
        for _ in 0..nd {
            let w = read_weight_parts(
                &mut reader,
                limits,
                SiteContext::Table(&sites),
                Some(&previous_path),
            )?;
            if let Ok(index) = sites.binary_search(&w.site) {
                referenced_sites[index] = true;
            }
            let c = reader.uvarint()?;
            previous_path = w.sc.clone();
            let item = (w, c);
            if c == 0
                || previous_delete
                    .as_ref()
                    .is_some_and(|previous| previous >= &item)
            {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "snapshot delete log is not canonical",
                ));
            }
            previous_delete = Some(item.clone());
            delete_log.push(item);
        }
        if referenced_sites.iter().any(|used| !used) {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "snapshot site table has unused entries",
            ));
        }

        let vl = reader.u32()? as usize;
        let vb = reader.take(vl)?;
        let version = Version::decode_payload_with_limits(vb, limits)?;
        if !reader.is_finished() {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "snapshot contains trailing bytes",
            ));
        }

        let deleted: HashSet<_> = delete_log.iter().cloned().collect();
        if atoms
            .iter()
            .any(|atom| deleted.contains(&(atom.weight.clone(), atom.counter)))
        {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "snapshot atom is also in its delete log",
            ));
        }
        let mut insertion_identities = HashSet::with_capacity(atoms.len() + delete_log.len());
        if atoms.iter().any(|atom| {
            !insertions.contains(atom.weight.site, atom.counter)
                || !insertion_identities.insert((atom.weight.site, atom.counter))
        }) {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "snapshot live atoms do not have unique insertion receipts",
            ));
        }
        if delete_log.iter().any(|(weight, counter)| {
            insertions.contains(weight.site, *counter)
                || !insertion_identities.insert((weight.site, *counter))
        }) {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "snapshot contains a resolved or duplicated deferred deletion",
            ));
        }

        Ok(Self {
            atoms,
            delete_log,
            version,
            insertions,
        })
    }
}

/// Restart-complete archive: a materialized state plus every operation not
/// compacted into `history_floor` and the exact subset still pending.
///
/// A compact `Snapshot` is the merge base. A `FullSnapshot` additionally
/// preserves the journal needed for replay, reconnect export, and causal gaps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FullSnapshot {
    pub state: Snapshot,
    pub history_floor: Version,
    retained_update: Update,
    pub pending_operations: Vec<OperationRef>,
}

impl FullSnapshot {
    pub fn new(
        state: Snapshot,
        history_floor: Version,
        retained_operations: Vec<Op>,
        pending_operations: Vec<OperationRef>,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            state,
            history_floor,
            retained_update: Update::new(retained_operations)?,
            pending_operations,
        })
    }

    pub fn retained_operations(&self) -> &[Op] {
        self.retained_update.operations()
    }

    pub fn encode(&self) -> Vec<u8> {
        crate::wire::Artifact::FullSnapshot(self.clone()).encode()
    }

    pub(crate) fn encode_payload(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let state = self.state.encode_payload();
        out.extend_from_slice(&(state.len() as u32).to_le_bytes());
        out.extend_from_slice(&state);

        let floor = self.history_floor.encode_payload();
        out.extend_from_slice(&(floor.len() as u32).to_le_bytes());
        out.extend_from_slice(&floor);

        let operations = self.retained_update.encode_payload();
        out.extend_from_slice(&(operations.len() as u32).to_le_bytes());
        out.extend_from_slice(&operations);

        out.extend_from_slice(&(self.pending_operations.len() as u32).to_le_bytes());
        for identity in &self.pending_operations {
            out.extend_from_slice(&identity.origin.to_le_bytes());
            out.extend_from_slice(&identity.sequence.to_le_bytes());
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        Self::decode_with_limits(bytes, &ResourceLimits::wire_default()).ok()
    }

    pub fn decode_with_limits(bytes: &[u8], limits: &ResourceLimits) -> Result<Self, EngineError> {
        match crate::wire::Artifact::decode_with_limits(bytes, limits)? {
            crate::wire::Artifact::FullSnapshot(snapshot) => Ok(snapshot),
            _ => Err(EngineError::new(
                ErrorCode::MalformedEncoding,
                "expected an ESBT full snapshot artifact",
            )),
        }
    }

    pub(crate) fn decode_payload_with_limits(
        bytes: &[u8],
        limits: &ResourceLimits,
    ) -> Result<Self, EngineError> {
        if bytes.len() > limits.max_message_bytes {
            return Err(EngineError::new(
                ErrorCode::MessageTooLarge,
                "full snapshot exceeds resource policy",
            ));
        }
        let mut reader = Reader::new(bytes);
        let state_length = reader.u32()? as usize;
        let state = Snapshot::decode_payload_with_limits(reader.take(state_length)?, limits)?;
        let floor_length = reader.u32()? as usize;
        let history_floor =
            Version::decode_payload_with_limits(reader.take(floor_length)?, limits)?;
        if !history_floor.is_contiguous() || !state.version.covers(&history_floor) {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "full snapshot history floor is not a covered causal prefix",
            ));
        }

        let operations_length = reader.u32()? as usize;
        let operations = Update::decode_payload_with_limit(
            reader.take(operations_length)?,
            limits,
            limits.max_retained_operations,
        )?;
        if operations.len() > limits.max_retained_operations {
            return Err(EngineError::new(
                ErrorCode::TooManyOperations,
                "full snapshot retains too many operations",
            ));
        }

        let pending_count = reader.u32()? as usize;
        if pending_count > limits.max_pending_operations || pending_count > reader.remaining() / 24
        {
            return Err(EngineError::new(
                ErrorCode::TooManyPendingOperations,
                "full snapshot pending count exceeds its bounds",
            ));
        }
        let retained: HashSet<_> = operations.identities().into_iter().collect();
        let mut pending_operations = Vec::with_capacity(pending_count);
        let mut previous = None;
        for _ in 0..pending_count {
            let identity = OperationRef::new(reader.u128()?, reader.u64()?);
            if identity.origin == 0
                || identity.sequence == 0
                || previous.is_some_and(|value| value >= identity)
                || !retained.contains(&identity)
            {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "full snapshot pending identities are not canonical",
                ));
            }
            previous = Some(identity);
            pending_operations.push(identity);
        }
        if !reader.is_finished() {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "full snapshot contains trailing bytes",
            ));
        }

        let mut represented = history_floor.clone();
        let mut insertion_bindings: HashMap<(SiteId, u64), (&Weight, u16)> = state
            .atoms
            .iter()
            .map(|atom| ((atom.weight.site, atom.counter), (&atom.weight, atom.unit)))
            .collect();
        let mut retained_insertions = HashSet::new();
        for operation in operations.operations() {
            if !state.version.contains(operation.origin, operation.seq) {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "retained operation is absent from snapshot version",
                ));
            }
            if matches!(operation.kind, crate::op::OpKind::Ins { .. })
                && operation.origin != operation.weight.site
            {
                return Err(EngineError::new(
                    ErrorCode::InvalidOperation,
                    "retained insertion origin does not own its ESBT weight",
                ));
            }
            if matches!(operation.kind, crate::op::OpKind::Ins { .. })
                && !state
                    .insertions
                    .contains(operation.weight.site, operation.counter)
            {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "full snapshot insertion receipts do not cover a retained insertion",
                ));
            }
            if let crate::op::OpKind::Ins { unit } = operation.kind {
                let identity = (operation.weight.site, operation.counter);
                if !retained_insertions.insert(identity) {
                    return Err(EngineError::new(
                        ErrorCode::OperationIdentityConflict,
                        "full snapshot repeats an insertion counter",
                    ));
                }
                if insertion_bindings
                    .insert(identity, (&operation.weight, unit))
                    .is_some_and(|(weight, existing_unit)| {
                        weight != &operation.weight || existing_unit != unit
                    })
                {
                    return Err(EngineError::new(
                        ErrorCode::OperationIdentityConflict,
                        "full snapshot binds one insertion counter to multiple weights",
                    ));
                }
            }
            represented.note(operation.origin, operation.seq);
        }
        if !represented.covers(&state.version) {
            return Err(EngineError::new(
                ErrorCode::MissingLocalHistory,
                "full snapshot omits receipts above its history floor",
            ));
        }

        Ok(Self {
            state,
            history_floor,
            retained_update: operations,
            pending_operations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replica::{Replica, ReplicaConfig};
    use crate::wire::Artifact;

    fn sample_snapshot() -> Snapshot {
        let mut replica = Replica::new(7, ReplicaConfig::default());
        replica.local_insert_str(0, "Hello");
        replica.local_delete(1);
        replica.snapshot()
    }

    #[test]
    fn snapshot_and_messages_roundtrip_exactly() {
        let snapshot = sample_snapshot();
        assert_eq!(snapshot.encode(), snapshot.encode());
        assert_eq!(Snapshot::decode(&snapshot.encode()), Some(snapshot.clone()));

        let artifacts = [Artifact::CompactSnapshot(snapshot)];
        for artifact in artifacts {
            assert_eq!(Artifact::decode(&artifact.encode()), Some(artifact));
        }
    }

    #[test]
    fn decoder_rejects_wrong_version_trailing_and_impossible_lengths() {
        let artifact = Artifact::CompactSnapshot(sample_snapshot()).encode();

        let mut wrong_version = artifact.clone();
        wrong_version[4..6].copy_from_slice(&(crate::wire::WIRE_FORMAT_VERSION + 1).to_le_bytes());
        assert!(Artifact::decode(&wrong_version).is_none());

        let mut trailing = artifact.clone();
        trailing.push(0);
        assert!(Artifact::decode(&trailing).is_none());

        let mut impossible_length = artifact.clone();
        impossible_length[7..11].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Artifact::decode(&impossible_length).is_none());
    }

    #[test]
    fn every_truncation_is_rejected_without_panicking() {
        let artifact = Artifact::CompactSnapshot(sample_snapshot()).encode();
        for end in 0..artifact.len() {
            let result = std::panic::catch_unwind(|| Artifact::decode(&artifact[..end]));
            assert!(result.is_ok(), "decoder panicked at byte {end}");
            assert!(
                result.unwrap().is_none(),
                "accepted truncation at byte {end}"
            );
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic_the_public_message_decoder() {
        let mut state = 0x9e37_79b9u32;
        for length in 0..512 {
            let mut bytes = vec![0u8; length];
            for byte in &mut bytes {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *byte = state as u8;
            }
            let decoded = std::panic::catch_unwind(|| Artifact::decode(&bytes));
            assert!(decoded.is_ok(), "decoder panicked on {length} random bytes");
        }
    }

    #[test]
    fn decoder_rejects_noncanonical_or_self_deleted_state() {
        let snapshot = sample_snapshot();

        let mut unsorted = snapshot.clone();
        unsorted.atoms.reverse();
        assert!(Snapshot::decode(&unsorted.encode()).is_none());

        let mut self_deleted = snapshot.clone();
        let live = &self_deleted.atoms[0];
        self_deleted
            .delete_log
            .push((live.weight.clone(), live.counter));
        self_deleted.delete_log.sort();
        assert!(Snapshot::decode(&self_deleted.encode()).is_none());
    }
}
