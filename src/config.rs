//! Canonical byte codec for document configuration.
//!
//! The Wasm ABI creates documents from an opaque, versioned config blob so
//! the browser client can reach every engine policy native callers already
//! have: `Dmax`, base, depth, the Extension 3 allocation strategy, the
//! Extension 1 adaptive-`Dmax` controller, and per-document resource
//! ceilings (`ResourceLimits` was always documented as instance-carried so
//! browsers could choose lower ceilings; this is the surface that finally
//! lets them). Decoding follows the engine's exact-bytes discipline:
//! canonical varints, no unknown flags, no trailing bytes.

use crate::allocator::AdaptiveDmaxConfig;
use crate::codec::{write_uvarint, Reader};
use crate::error::{EngineError, ErrorCode};
use crate::limits::ResourceLimits;
use crate::newseq::AllocationStrategy;
use crate::replica::ReplicaConfig;

const CONFIG_FORMAT_VERSION: u16 = 1;
const FLAG_ADAPTIVE: u8 = 1 << 0;
const FLAG_LIMITS: u8 = 1 << 1;
const FLAG_KNOWN: u8 = FLAG_ADAPTIVE | FLAG_LIMITS;

/// Everything a document constructor accepts, as one canonical value.
#[derive(Clone, Debug, Default)]
pub struct DocumentConfig {
    pub replica: ReplicaConfig,
    pub limits: ResourceLimits,
}

fn limit_fields(limits: &ResourceLimits) -> [usize; 12] {
    [
        limits.max_message_bytes,
        limits.max_operations_per_update,
        limits.max_identifier_depth,
        limits.max_version_sites,
        limits.max_sparse_receipts,
        limits.max_snapshot_items,
        limits.max_pending_operations,
        limits.max_deferred_deletes,
        limits.max_document_units,
        limits.max_allocation_attempts,
        limits.max_retained_operations,
        limits.max_undo_transactions,
    ]
}

fn read_usize(reader: &mut Reader<'_>) -> Result<usize, EngineError> {
    usize::try_from(reader.uvarint()?)
        .map_err(|_| EngineError::malformed("config value exceeds this platform's usize"))
}

fn read_positive_i64(reader: &mut Reader<'_>) -> Result<i64, EngineError> {
    let value = reader.uvarint()?;
    if value == 0 || value > i64::MAX as u64 {
        return Err(EngineError::malformed("config bound is out of range"));
    }
    Ok(value as i64)
}

fn read_u32(reader: &mut Reader<'_>) -> Result<u32, EngineError> {
    u32::try_from(reader.uvarint()?)
        .map_err(|_| EngineError::malformed("config value exceeds 32 bits"))
}

impl DocumentConfig {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&CONFIG_FORMAT_VERSION.to_le_bytes());
        let mut flags = 0u8;
        if self.replica.adaptive_dmax.is_some() {
            flags |= FLAG_ADAPTIVE;
        }
        flags |= FLAG_LIMITS;
        out.push(flags);
        write_uvarint(&mut out, self.replica.dmax.max(0) as u64);
        write_uvarint(&mut out, u64::from(self.replica.base));
        write_uvarint(&mut out, u64::from(self.replica.depth));
        match self.replica.strategy {
            AllocationStrategy::Midpoint => out.push(0),
            AllocationStrategy::BoundaryLow(boundary) => {
                out.push(1);
                write_uvarint(&mut out, u64::from(boundary));
            }
            AllocationStrategy::BoundaryHigh(boundary) => {
                out.push(2);
                write_uvarint(&mut out, u64::from(boundary));
            }
            AllocationStrategy::AlternatingByDepth(boundary) => {
                out.push(3);
                write_uvarint(&mut out, u64::from(boundary));
            }
        }
        if let Some(adaptive) = &self.replica.adaptive_dmax {
            write_uvarint(&mut out, adaptive.floor.max(0) as u64);
            write_uvarint(&mut out, adaptive.ceiling.max(0) as u64);
            write_uvarint(&mut out, u64::from(adaptive.window));
            write_uvarint(&mut out, u64::from(adaptive.holdoff_windows));
        }
        for field in limit_fields(&self.limits) {
            write_uvarint(&mut out, field as u64);
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, EngineError> {
        let mut reader = Reader::new(bytes);
        if reader.u16()? != CONFIG_FORMAT_VERSION {
            return Err(EngineError::new(
                ErrorCode::UnsupportedFormatVersion,
                "unsupported document config version",
            ));
        }
        let flags = reader.u8()?;
        if flags & !FLAG_KNOWN != 0 {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "document config has unknown flags",
            ));
        }
        let dmax = read_positive_i64(&mut reader)?;
        let base = read_u32(&mut reader)?;
        let depth = read_u32(&mut reader)?;
        let strategy = match reader.u8()? {
            0 => AllocationStrategy::Midpoint,
            1 => AllocationStrategy::BoundaryLow(read_u32(&mut reader)?),
            2 => AllocationStrategy::BoundaryHigh(read_u32(&mut reader)?),
            3 => AllocationStrategy::AlternatingByDepth(read_u32(&mut reader)?),
            _ => {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "unknown allocation strategy tag",
                ))
            }
        };
        let adaptive_dmax = if flags & FLAG_ADAPTIVE != 0 {
            Some(AdaptiveDmaxConfig {
                floor: read_positive_i64(&mut reader)?,
                ceiling: read_positive_i64(&mut reader)?,
                window: read_u32(&mut reader)?,
                holdoff_windows: read_u32(&mut reader)?,
            })
        } else {
            None
        };
        let limits = if flags & FLAG_LIMITS != 0 {
            ResourceLimits {
                max_message_bytes: read_usize(&mut reader)?,
                max_operations_per_update: read_usize(&mut reader)?,
                max_identifier_depth: read_usize(&mut reader)?,
                max_version_sites: read_usize(&mut reader)?,
                max_sparse_receipts: read_usize(&mut reader)?,
                max_snapshot_items: read_usize(&mut reader)?,
                max_pending_operations: read_usize(&mut reader)?,
                max_deferred_deletes: read_usize(&mut reader)?,
                max_document_units: read_usize(&mut reader)?,
                max_allocation_attempts: read_usize(&mut reader)?,
                max_retained_operations: read_usize(&mut reader)?,
                max_undo_transactions: read_usize(&mut reader)?,
            }
        } else {
            ResourceLimits::default()
        };
        if !reader.is_finished() {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "document config contains trailing bytes",
            ));
        }
        Ok(Self {
            replica: ReplicaConfig {
                dmax,
                base,
                depth,
                adaptive_dmax,
                strategy,
            },
            limits,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_defaults_and_full_configuration() {
        let defaults = DocumentConfig::default();
        let decoded = DocumentConfig::decode(&defaults.encode()).expect("decode defaults");
        assert_eq!(decoded.replica.dmax, defaults.replica.dmax);
        assert_eq!(decoded.limits, defaults.limits);
        assert!(decoded.replica.adaptive_dmax.is_none());

        let full = DocumentConfig {
            replica: ReplicaConfig {
                dmax: 64,
                base: 1_000,
                depth: 32,
                adaptive_dmax: Some(AdaptiveDmaxConfig {
                    floor: 32,
                    ceiling: 1 << 20,
                    window: 128,
                    holdoff_windows: 2,
                }),
                strategy: AllocationStrategy::AlternatingByDepth(64),
            },
            limits: ResourceLimits {
                max_document_units: 250_000,
                ..Default::default()
            },
        };
        let decoded = DocumentConfig::decode(&full.encode()).expect("decode full");
        assert_eq!(decoded.replica.dmax, 64);
        assert_eq!(
            decoded.replica.strategy,
            AllocationStrategy::AlternatingByDepth(64)
        );
        assert_eq!(
            decoded.replica.adaptive_dmax,
            Some(AdaptiveDmaxConfig {
                floor: 32,
                ceiling: 1 << 20,
                window: 128,
                holdoff_windows: 2,
            })
        );
        assert_eq!(decoded.limits.max_document_units, 250_000);
    }

    #[test]
    fn rejects_trailing_unknown_and_out_of_range_bytes() {
        let mut trailing = DocumentConfig::default().encode();
        trailing.push(0);
        assert!(DocumentConfig::decode(&trailing).is_err());

        let mut unknown_flags = DocumentConfig::default().encode();
        unknown_flags[2] |= 1 << 7;
        assert!(DocumentConfig::decode(&unknown_flags).is_err());

        let mut wrong_version = DocumentConfig::default().encode();
        wrong_version[0..2].copy_from_slice(&9u16.to_le_bytes());
        assert!(DocumentConfig::decode(&wrong_version).is_err());

        assert!(DocumentConfig::decode(&[]).is_err());
    }
}
