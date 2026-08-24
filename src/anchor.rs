//! Stable, persistable positions for selections and product-owned metadata.

use crate::clock::Version;
use crate::codec::{read_weight, write_weight, Reader};
use crate::error::{EngineError, ErrorCode};
use crate::limits::ResourceLimits;
use crate::replica::Replica;
use crate::weight::Weight;

/// Anchors are self-contained, so their weights carry an inline site.
const NO_SITE_CONTEXT: crate::weight::SiteId = 0;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affinity {
    /// The boundary remains immediately before its referenced item.
    Before = 1,
    /// The boundary remains immediately after its referenced item.
    After = 2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnchorTarget {
    Start,
    End,
    /// The insertion identity is retained so reuse of the same ESBT weight is
    /// not mistaken for the original item.
    Item {
        weight: Weight,
        counter: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub target: AnchorTarget,
    pub affinity: Affinity,
}

impl Anchor {
    pub const fn start() -> Self {
        Self {
            target: AnchorTarget::Start,
            affinity: Affinity::After,
        }
    }

    pub const fn end() -> Self {
        Self {
            target: AnchorTarget::End,
            affinity: Affinity::Before,
        }
    }

    pub fn at_index(replica: &Replica, index: usize, affinity: Affinity) -> Self {
        let index = index.min(replica.len());
        match affinity {
            Affinity::Before => replica
                .doc
                .get_at(index)
                .map(|(weight, _, counter)| Self {
                    target: AnchorTarget::Item {
                        weight: weight.clone(),
                        counter,
                    },
                    affinity,
                })
                .unwrap_or_else(Self::end),
            Affinity::After => index
                .checked_sub(1)
                .and_then(|previous| replica.doc.get_at(previous))
                .map(|(weight, _, counter)| Self {
                    target: AnchorTarget::Item {
                        weight: weight.clone(),
                        counter,
                    },
                    affinity,
                })
                .unwrap_or_else(Self::start),
        }
    }

    /// Resolve in UTF-16 units. Deleted identities collapse to the lower bound
    /// of their weight; affinity applies only while the exact identity is live.
    pub fn resolve(&self, replica: &Replica) -> usize {
        match &self.target {
            AnchorTarget::Start => 0,
            AnchorTarget::End => replica.len(),
            AnchorTarget::Item { weight, counter } => {
                if replica
                    .doc
                    .find(weight)
                    .is_some_and(|(_, live_counter)| live_counter == *counter)
                {
                    let index = replica.doc.index_of(weight).unwrap_or_else(|| {
                        // `find` and `index_of` traverse the same immutable
                        // tree, so this branch is defensive rather than normal.
                        replica.doc.lower_bound(weight)
                    });
                    index + usize::from(self.affinity == Affinity::After)
                } else {
                    replica.doc.lower_bound(weight)
                }
            }
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        crate::wire::Artifact::Anchor(self.clone()).encode()
    }

    pub(crate) fn encode_payload(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(self.affinity as u8);
        match &self.target {
            AnchorTarget::Start => out.push(1),
            AnchorTarget::End => out.push(2),
            AnchorTarget::Item { weight, counter } => {
                out.push(3);
                write_weight(&mut out, weight, NO_SITE_CONTEXT);
                out.extend_from_slice(&counter.to_le_bytes());
            }
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        Self::decode_with_limits(bytes, &ResourceLimits::wire_default()).ok()
    }

    pub fn decode_with_limits(bytes: &[u8], limits: &ResourceLimits) -> Result<Self, EngineError> {
        match crate::wire::Artifact::decode_with_limits(bytes, limits)? {
            crate::wire::Artifact::Anchor(anchor) => Ok(anchor),
            _ => Err(EngineError::new(
                ErrorCode::InvalidAnchor,
                "expected an ESBT anchor artifact",
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
                "anchor exceeds resource policy",
            ));
        }
        let mut reader = Reader::new(bytes);
        let affinity = match reader.u8()? {
            1 => Affinity::Before,
            2 => Affinity::After,
            _ => {
                return Err(EngineError::new(
                    ErrorCode::InvalidAnchor,
                    "invalid anchor affinity",
                ))
            }
        };
        let target = match reader.u8()? {
            1 => AnchorTarget::Start,
            2 => AnchorTarget::End,
            3 => {
                let weight = read_weight(&mut reader, limits, NO_SITE_CONTEXT)?;
                let counter = reader.u64()?;
                if counter == 0 {
                    return Err(EngineError::new(
                        ErrorCode::InvalidAnchor,
                        "anchor counter is zero",
                    ));
                }
                AnchorTarget::Item { weight, counter }
            }
            _ => {
                return Err(EngineError::new(
                    ErrorCode::InvalidAnchor,
                    "invalid anchor target",
                ))
            }
        };
        if !reader.is_finished() {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "anchor contains trailing bytes",
            ));
        }
        match (&target, affinity) {
            (AnchorTarget::Start, Affinity::After) | (AnchorTarget::End, Affinity::Before) => {}
            (AnchorTarget::Start | AnchorTarget::End, _) => {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "sentinel anchor has noncanonical affinity",
                ))
            }
            (AnchorTarget::Item { .. }, _) => {}
        }
        Ok(Self { target, affinity })
    }
}

/// A stable position plus the exact causal frontier at which it was captured.
/// Consumers can distinguish "not integrated yet" from an anchor that has
/// legitimately collapsed after its target was deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CausalPosition {
    pub version: Version,
    pub anchor: Anchor,
}

impl CausalPosition {
    pub fn new(version: Version, anchor: Anchor) -> Self {
        Self { version, anchor }
    }

    pub fn encode(&self) -> Vec<u8> {
        crate::wire::Artifact::CausalPosition(self.clone()).encode()
    }

    pub(crate) fn encode_payload(&self) -> Vec<u8> {
        let version = self.version.encode_payload();
        let anchor = self.anchor.encode_payload();
        let mut out = Vec::with_capacity(8 + version.len() + anchor.len());
        out.extend_from_slice(
            &u32::try_from(version.len())
                .expect("causal-position version exceeds u32")
                .to_le_bytes(),
        );
        out.extend_from_slice(&version);
        out.extend_from_slice(
            &u32::try_from(anchor.len())
                .expect("causal-position anchor exceeds u32")
                .to_le_bytes(),
        );
        out.extend_from_slice(&anchor);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        Self::decode_with_limits(bytes, &ResourceLimits::wire_default()).ok()
    }

    pub fn decode_with_limits(bytes: &[u8], limits: &ResourceLimits) -> Result<Self, EngineError> {
        match crate::wire::Artifact::decode_with_limits(bytes, limits)? {
            crate::wire::Artifact::CausalPosition(position) => Ok(position),
            _ => Err(EngineError::new(
                ErrorCode::InvalidAnchor,
                "expected an ESBT causal-position artifact",
            )),
        }
    }

    pub(crate) fn decode_payload_with_limits(
        bytes: &[u8],
        limits: &ResourceLimits,
    ) -> Result<Self, EngineError> {
        let mut reader = Reader::new(bytes);
        let version_length = reader.u32()? as usize;
        let version = Version::decode_payload_with_limits(reader.take(version_length)?, limits)?;
        let anchor_length = reader.u32()? as usize;
        let anchor = Anchor::decode_payload_with_limits(reader.take(anchor_length)?, limits)?;
        if !reader.is_finished() {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "causal position contains trailing bytes",
            ));
        }
        Ok(Self { version, anchor })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorRange {
    pub start: Anchor,
    pub end: Anchor,
}

impl AnchorRange {
    /// Resolve and normalize a range. Concurrent deletion can move the two
    /// boundaries together; reversed boundaries collapse instead of wrapping.
    pub fn resolve(&self, replica: &Replica) -> (usize, usize) {
        let start = self.start.resolve(replica);
        let end = self.end.resolve(replica);
        if start <= end {
            (start, end)
        } else {
            (end, end)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replica::ReplicaConfig;

    #[test]
    fn anchor_roundtrip_tracks_exact_item_and_collapses_after_delete() {
        let mut replica = Replica::new(1, ReplicaConfig::default());
        replica.local_insert_str(0, "abcd");
        let anchor = Anchor::at_index(&replica, 3, Affinity::Before);
        let decoded = Anchor::decode(&anchor.encode()).expect("decode anchor");

        replica.local_insert_str(0, "XYZ");
        assert_eq!(decoded.resolve(&replica), 6);
        replica.local_delete(6);
        assert_eq!(decoded.resolve(&replica), 6);
    }

    #[test]
    fn after_anchor_keeps_a_local_caret_attached_to_its_run() {
        let mut local = Replica::new(1, ReplicaConfig::default());
        let mut remote = Replica::new(2, ReplicaConfig::default());
        let a = local.local_insert(0, b'a' as u16);
        let x = remote.local_insert(0, b'x' as u16);
        local.receive(x);
        remote.receive(a);

        let caret = Anchor::at_index(&local, 1, Affinity::After);
        let index = caret.resolve(&local);
        let b = local.local_insert(index, b'b' as u16);
        remote.receive(b);
        assert!(matches!(local.text().as_str(), "abx" | "xab"));
        assert_eq!(local.text(), remote.text());
    }

    #[test]
    fn causal_position_roundtrips_under_the_shared_envelope() {
        let mut version = Version::default();
        version.note(7, 1);
        let position = CausalPosition::new(version, Anchor::start());
        assert_eq!(CausalPosition::decode(&position.encode()), Some(position));
    }
}
