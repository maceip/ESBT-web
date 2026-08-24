//! Canonical ESBT artifact envelope.
//!
//! WIT is the host ABI. This module is the one deliberately smaller protocol
//! beneath it: durable or transmitted CRDT artifacts only. Every artifact has
//! exactly one outer frame and every payload is decoded under an explicit
//! resource policy.

use crate::anchor::{Anchor, CausalPosition};
use crate::clock::Version;
use crate::codec::Reader;
use crate::error::{EngineError, ErrorCode};
use crate::limits::ResourceLimits;
use crate::snapshot::{FullSnapshot, Snapshot};
use crate::update::Update;

pub const WIRE_MAGIC: &[u8; 4] = b"ESBT";
pub const WIRE_FORMAT_VERSION: u16 = 1;
pub const WIRE_HEADER_BYTES: usize = 4 + 2 + 1 + 4;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactKind {
    Update = 1,
    CompactSnapshot = 2,
    FullSnapshot = 3,
    Version = 4,
    Anchor = 5,
    CausalPosition = 6,
}

impl TryFrom<u8> for ArtifactKind {
    type Error = EngineError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Update),
            2 => Ok(Self::CompactSnapshot),
            3 => Ok(Self::FullSnapshot),
            4 => Ok(Self::Version),
            5 => Ok(Self::Anchor),
            6 => Ok(Self::CausalPosition),
            _ => Err(EngineError::new(
                ErrorCode::MalformedEncoding,
                "unknown ESBT artifact kind",
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Artifact {
    Update(Update),
    CompactSnapshot(Snapshot),
    FullSnapshot(FullSnapshot),
    Version(Version),
    Anchor(Anchor),
    CausalPosition(CausalPosition),
}

impl Artifact {
    pub fn kind(&self) -> ArtifactKind {
        match self {
            Self::Update(_) => ArtifactKind::Update,
            Self::CompactSnapshot(_) => ArtifactKind::CompactSnapshot,
            Self::FullSnapshot(_) => ArtifactKind::FullSnapshot,
            Self::Version(_) => ArtifactKind::Version,
            Self::Anchor(_) => ArtifactKind::Anchor,
            Self::CausalPosition(_) => ArtifactKind::CausalPosition,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let payload = match self {
            Self::Update(value) => value.encode_payload(),
            Self::CompactSnapshot(value) => value.encode_payload(),
            Self::FullSnapshot(value) => value.encode_payload(),
            Self::Version(value) => value.encode_payload(),
            Self::Anchor(value) => value.encode_payload(),
            Self::CausalPosition(value) => value.encode_payload(),
        };
        let payload_length = u32::try_from(payload.len())
            .expect("ESBT artifact payload exceeds the u32 envelope field");
        let mut encoded = Vec::with_capacity(WIRE_HEADER_BYTES + payload.len());
        encoded.extend_from_slice(WIRE_MAGIC);
        encoded.extend_from_slice(&WIRE_FORMAT_VERSION.to_le_bytes());
        encoded.push(self.kind() as u8);
        encoded.extend_from_slice(&payload_length.to_le_bytes());
        encoded.extend_from_slice(&payload);
        encoded
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        Self::decode_with_limits(bytes, &ResourceLimits::wire_default()).ok()
    }

    pub fn decode_with_limits(bytes: &[u8], limits: &ResourceLimits) -> Result<Self, EngineError> {
        if bytes.len() > limits.max_message_bytes {
            return Err(EngineError::new(
                ErrorCode::MessageTooLarge,
                "ESBT artifact exceeds the message byte limit",
            ));
        }
        let mut reader = Reader::new(bytes);
        if reader.take(WIRE_MAGIC.len())? != WIRE_MAGIC {
            return Err(EngineError::malformed("invalid ESBT artifact magic"));
        }
        if reader.u16()? != WIRE_FORMAT_VERSION {
            return Err(EngineError::new(
                ErrorCode::UnsupportedFormatVersion,
                "unsupported ESBT wire format version",
            ));
        }
        let kind = ArtifactKind::try_from(reader.u8()?)?;
        let payload_length = reader.u32()? as usize;
        let payload = reader.take(payload_length)?;
        if !reader.is_finished() {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "ESBT envelope length does not consume the artifact",
            ));
        }

        match kind {
            ArtifactKind::Update => Ok(Self::Update(Update::decode_payload(payload, limits)?)),
            ArtifactKind::CompactSnapshot => Ok(Self::CompactSnapshot(
                Snapshot::decode_payload_with_limits(payload, limits)?,
            )),
            ArtifactKind::FullSnapshot => Ok(Self::FullSnapshot(
                FullSnapshot::decode_payload_with_limits(payload, limits)?,
            )),
            ArtifactKind::Version => Ok(Self::Version(Version::decode_payload_with_limits(
                payload, limits,
            )?)),
            ArtifactKind::Anchor => Ok(Self::Anchor(Anchor::decode_payload_with_limits(
                payload, limits,
            )?)),
            ArtifactKind::CausalPosition => Ok(Self::CausalPosition(
                CausalPosition::decode_payload_with_limits(payload, limits)?,
            )),
        }
    }

    /// Parse and validate the complete artifact before reporting its kind.
    pub fn decode_kind(bytes: &[u8], limits: &ResourceLimits) -> Result<ArtifactKind, EngineError> {
        Ok(Self::decode_with_limits(bytes, limits)?.kind())
    }

    /// Classify a structurally complete envelope without decoding its payload.
    /// The receiving document performs the semantic decode under its own
    /// resource policy. This path allocates nothing and does not impose the
    /// conservative default document size on a configured larger document.
    pub fn classify(bytes: &[u8]) -> Result<ArtifactKind, EngineError> {
        let mut reader = Reader::new(bytes);
        if reader.take(WIRE_MAGIC.len())? != WIRE_MAGIC {
            return Err(EngineError::malformed("invalid ESBT artifact magic"));
        }
        if reader.u16()? != WIRE_FORMAT_VERSION {
            return Err(EngineError::new(
                ErrorCode::UnsupportedFormatVersion,
                "unsupported ESBT wire format version",
            ));
        }
        let kind = ArtifactKind::try_from(reader.u8()?)?;
        let payload_length = reader.u32()? as usize;
        reader.take(payload_length)?;
        if !reader.is_finished() {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "ESBT envelope length does not consume the artifact",
            ));
        }
        Ok(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_version_unknown_kind_trailing_and_every_truncation() {
        let artifact = Artifact::Version(Version::default()).encode();

        let mut wrong_version = artifact.clone();
        wrong_version[4..6].copy_from_slice(&(WIRE_FORMAT_VERSION + 1).to_le_bytes());
        assert!(Artifact::decode(&wrong_version).is_none());

        let mut unknown_kind = artifact.clone();
        unknown_kind[6] = 255;
        assert!(Artifact::decode(&unknown_kind).is_none());

        let mut trailing = artifact.clone();
        trailing.push(0);
        assert!(Artifact::decode(&trailing).is_none());

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
    fn arbitrary_bytes_never_panic() {
        let mut state = 0x9e37_79b9u32;
        for length in 0..512 {
            let mut bytes = vec![0u8; length];
            for byte in &mut bytes {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *byte = state as u8;
            }
            assert!(std::panic::catch_unwind(|| Artifact::decode(&bytes)).is_ok());
        }
    }
}
