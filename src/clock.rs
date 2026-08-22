//! Per-origin sequence numbers. Transport reliability only — not the
//! paper's causal metadata (that is the insertion counter c).
//!
//! A site summary is the highest *contiguous* sequence prefix plus any
//! higher sequences already received. Recording only the maximum observed
//! sequence poisons reconnect state when `seq = 2` arrives before `seq = 1`:
//! the receiver must still advertise that sequence 1 is missing.

use crate::codec::Reader;
use crate::error::{EngineError, ErrorCode};
use crate::limits::ResourceLimits;
use crate::weight::SiteId;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SiteVersion {
    contiguous: u64,
    seen_after: BTreeSet<u64>,
}

#[derive(Clone)]
pub(crate) struct SiteReceiptCheckpoint(Option<SiteVersion>);

#[derive(Clone, Copy)]
pub(crate) struct ReceiptProjection {
    pub site_count: usize,
    pub sparse_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Version {
    sites: BTreeMap<SiteId, SiteVersion>,
    sparse_count: usize,
}

impl Version {
    fn note_state(state: &mut SiteVersion, seq: u64) {
        if seq == 0 || seq <= state.contiguous {
            return;
        }
        if state.contiguous.checked_add(1) == Some(seq) {
            state.contiguous = seq;
            while let Some(next) = state.contiguous.checked_add(1) {
                if !state.seen_after.remove(&next) {
                    break;
                }
                state.contiguous = next;
            }
        } else {
            state.seen_after.insert(seq);
        }
    }

    pub(crate) fn project_notes(
        &self,
        notes: impl IntoIterator<Item = (SiteId, u64)>,
    ) -> ReceiptProjection {
        let mut affected = BTreeMap::<SiteId, SiteVersion>::new();
        for (site, sequence) in notes {
            if sequence == 0 {
                continue;
            }
            let state = affected
                .entry(site)
                .or_insert_with(|| self.sites.get(&site).cloned().unwrap_or_default());
            Self::note_state(state, sequence);
        }

        let old_sparse: usize = affected
            .keys()
            .filter_map(|site| self.sites.get(site))
            .map(|state| state.seen_after.len())
            .sum();
        let new_sparse: usize = affected.values().map(|state| state.seen_after.len()).sum();
        let new_sites = affected
            .keys()
            .filter(|site| !self.sites.contains_key(site))
            .count();
        ReceiptProjection {
            site_count: self.sites.len().saturating_add(new_sites),
            sparse_count: self.sparse_count.saturating_sub(old_sparse) + new_sparse,
        }
    }

    pub(crate) fn checkpoint_site(&self, site: SiteId) -> SiteReceiptCheckpoint {
        SiteReceiptCheckpoint(self.sites.get(&site).cloned())
    }

    pub(crate) fn restore_site(&mut self, site: SiteId, checkpoint: SiteReceiptCheckpoint) {
        let before = self
            .sites
            .get(&site)
            .map(|state| state.seen_after.len())
            .unwrap_or(0);
        match checkpoint.0 {
            Some(state) => {
                self.sites.insert(site, state);
            }
            None => {
                self.sites.remove(&site);
            }
        }
        let after = self
            .sites
            .get(&site)
            .map(|state| state.seen_after.len())
            .unwrap_or(0);
        self.sparse_count = self.sparse_count.saturating_sub(before) + after;
    }

    /// Highest sequence for which every earlier sequence from `site` is known.
    ///
    /// This keeps the old method name for callers, but its meaning is
    /// deliberately a contiguous prefix rather than the maximum value seen.
    pub fn observed(&self, site: SiteId) -> u64 {
        self.sites
            .get(&site)
            .map(|state| state.contiguous)
            .unwrap_or(0)
    }

    /// Highest receipt represented for a site, including sparse arrivals.
    /// A restarted generator must advance past this value even when the
    /// contiguous reconnect prefix is lower.
    pub fn highest_seen(&self, site: SiteId) -> u64 {
        self.sites
            .get(&site)
            .map(|state| state.seen_after.last().copied().unwrap_or(state.contiguous))
            .unwrap_or(0)
    }

    pub fn contains(&self, site: SiteId, seq: u64) -> bool {
        if seq == 0 {
            return false;
        }
        let Some(state) = self.sites.get(&site) else {
            return false;
        };
        seq <= state.contiguous || state.seen_after.contains(&seq)
    }

    pub fn note(&mut self, site: SiteId, seq: u64) {
        if seq == 0 {
            return;
        }

        let (before, after) = {
            let state = self.sites.entry(site).or_default();
            if seq <= state.contiguous {
                return;
            }
            let before = state.seen_after.len();

            Self::note_state(state, seq);
            (before, state.seen_after.len())
        };
        self.sparse_count = self.sparse_count.saturating_sub(before) + after;
    }

    /// Union another receipt summary into this one without mistaking a sparse
    /// high sequence for proof that the sequences below it were received.
    pub fn merge(&mut self, other: &Self) {
        for (&site, incoming) in &other.sites {
            let current = self.sites.entry(site).or_default();
            let before = current.seen_after.len();
            current.contiguous = current.contiguous.max(incoming.contiguous);
            current
                .seen_after
                .extend(incoming.seen_after.iter().copied());
            current
                .seen_after
                .retain(|sequence| *sequence > current.contiguous);

            while let Some(next) = current.contiguous.checked_add(1) {
                if !current.seen_after.remove(&next) {
                    break;
                }
                current.contiguous = next;
            }
            self.sparse_count = self.sparse_count.saturating_sub(before) + current.seen_after.len();
        }
    }

    /// True when every advertised site is a causal prefix with no holes.
    /// Compact state snapshots are safe merge bases only at this boundary;
    /// sparse receipts still require their operation journal.
    pub fn is_contiguous(&self) -> bool {
        self.sites.values().all(|state| state.seen_after.is_empty())
    }

    pub fn site_count(&self) -> usize {
        self.sites.len()
    }

    pub fn sparse_receipt_count(&self) -> usize {
        self.sparse_count
    }

    /// Canonical receipt view in ascending site and sequence order.
    pub fn receipts(&self) -> impl Iterator<Item = (SiteId, u64, Vec<u64>)> + '_ {
        self.sites.iter().map(|(&site, state)| {
            (
                site,
                state.contiguous,
                state.seen_after.iter().copied().collect(),
            )
        })
    }

    /// Whether this summary contains every receipt represented by `other`.
    pub fn covers(&self, other: &Self) -> bool {
        other.sites.iter().all(|(&site, expected)| {
            let Some(actual) = self.sites.get(&site) else {
                return expected.contiguous == 0 && expected.seen_after.is_empty();
            };
            actual.contiguous >= expected.contiguous
                && expected
                    .seen_after
                    .iter()
                    .all(|&sequence| self.contains(site, sequence))
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&(self.sites.len() as u32).to_le_bytes());
        for (&site, state) in &self.sites {
            b.extend_from_slice(&site.to_le_bytes());
            b.extend_from_slice(&state.contiguous.to_le_bytes());
            b.extend_from_slice(&(state.seen_after.len() as u32).to_le_bytes());
            for &seq in &state.seen_after {
                b.extend_from_slice(&seq.to_le_bytes());
            }
        }
        b
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        Self::decode_with_limits(buf, &ResourceLimits::wire_default()).ok()
    }

    pub fn decode_with_limits(buf: &[u8], limits: &ResourceLimits) -> Result<Self, EngineError> {
        if buf.len() > limits.max_message_bytes {
            return Err(EngineError::new(
                ErrorCode::MessageTooLarge,
                "version summary exceeds message byte limit",
            ));
        }
        let mut reader = Reader::new(buf);
        let site_count = reader.u32()? as usize;
        if site_count > limits.max_version_sites {
            return Err(EngineError::new(
                ErrorCode::TooManyVersionSites,
                "version summary has too many sites",
            ));
        }
        // Each site needs at least site + prefix + sparse-count bytes. This
        // rejects impossible counts before doing attacker-controlled work.
        if site_count > reader.remaining() / 28 {
            return Err(EngineError::malformed("impossible version site count"));
        }
        let mut version = Version::default();
        let mut total_sparse = 0usize;
        let mut previous_site = None;

        for _ in 0..site_count {
            let site = reader.u128()?;
            if site == 0 || previous_site.is_some_and(|previous| previous >= site) {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "version sites are zero, duplicated, or out of order",
                ));
            }
            previous_site = Some(site);
            let contiguous = reader.u64()?;
            let sparse_count = reader.u32()? as usize;
            if version.sites.contains_key(&site) || (contiguous == 0 && sparse_count == 0) {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "duplicate or empty version site",
                ));
            }
            total_sparse = total_sparse.checked_add(sparse_count).ok_or_else(|| {
                EngineError::new(ErrorCode::IntegerOverflow, "sparse receipt count overflow")
            })?;
            if total_sparse > limits.max_sparse_receipts {
                return Err(EngineError::new(
                    ErrorCode::TooManySparseReceipts,
                    "version summary has too many sparse receipts",
                ));
            }
            if sparse_count > reader.remaining() / 8 {
                return Err(EngineError::malformed("impossible sparse receipt count"));
            }

            let mut seen_after = BTreeSet::new();
            let mut previous = contiguous;
            for _ in 0..sparse_count {
                let seq = reader.u64()?;
                // `contiguous + 1` would have folded into the prefix. Requiring
                // a larger, strictly increasing value keeps one canonical form.
                if seq <= previous || contiguous.checked_add(1) == Some(seq) {
                    return Err(EngineError::new(
                        ErrorCode::NonCanonicalEncoding,
                        "sparse receipts are not canonical",
                    ));
                }
                seen_after.insert(seq);
                previous = seq;
            }

            version.sites.insert(
                site,
                SiteVersion {
                    contiguous,
                    seen_after,
                },
            );
        }

        if !reader.is_finished() {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "version summary contains trailing bytes",
            ));
        }
        version.sparse_count = total_sparse;
        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_order_sequence_keeps_the_gap_visible() {
        let mut version = Version::default();
        version.note(7, 2);

        assert_eq!(version.observed(7), 0);
        assert!(!version.contains(7, 1));
        assert!(version.contains(7, 2));

        version.note(7, 1);
        assert_eq!(version.observed(7), 2);
        assert!(version.contains(7, 1));
        assert!(version.contains(7, 2));
    }

    #[test]
    fn filling_a_gap_advances_through_all_sparse_sequences() {
        let mut version = Version::default();
        for seq in [4, 2, 3, 1] {
            version.note(9, seq);
        }
        assert_eq!(version.observed(9), 4);
    }

    #[test]
    fn sparse_version_roundtrip_preserves_holes() {
        let mut version = Version::default();
        for seq in [1, 3, 5] {
            version.note(11, seq);
        }

        let decoded = Version::decode(&version.encode()).expect("decode version");
        assert_eq!(decoded, version);
        assert_eq!(decoded.observed(11), 1);
        assert!(!decoded.contains(11, 2));
        assert!(decoded.contains(11, 3));
        assert!(!decoded.contains(11, 4));
        assert!(decoded.contains(11, 5));
    }

    #[test]
    fn merge_unions_prefixes_and_sparse_receipts_without_hiding_holes() {
        let mut left = Version::default();
        for sequence in [1, 3, 7] {
            left.note(11, sequence);
        }
        let mut right = Version::default();
        for sequence in [1, 2, 4, 6] {
            right.note(11, sequence);
        }
        right.note(12, 1);

        left.merge(&right);

        assert_eq!(left.observed(11), 4);
        assert!(!left.contains(11, 5));
        assert!(left.contains(11, 6));
        assert!(left.contains(11, 7));
        assert_eq!(left.observed(12), 1);
        assert!(!left.is_contiguous());
        assert!(left.covers(&right));
        assert!(!right.covers(&left));
    }

    #[test]
    fn decoder_rejects_trailing_or_noncanonical_data() {
        let mut encoded = Version::default().encode();
        encoded.push(0);
        assert!(Version::decode(&encoded).is_none());

        // One site, prefix 0, and sparse sequence 1. Sequence 1 should have
        // been represented as the contiguous prefix instead.
        let mut noncanonical = Vec::new();
        noncanonical.extend_from_slice(&1u32.to_le_bytes());
        noncanonical.extend_from_slice(&7u32.to_le_bytes());
        noncanonical.extend_from_slice(&0u64.to_le_bytes());
        noncanonical.extend_from_slice(&1u32.to_le_bytes());
        noncanonical.extend_from_slice(&1u64.to_le_bytes());
        assert!(Version::decode(&noncanonical).is_none());
    }
}
