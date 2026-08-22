//! INS(ω, e, c) and DEL(ω, c). Paper §4.2 / §5.2.

use crate::codec::{read_weight, write_uvarint, write_weight, Reader, MIN_WEIGHT_BYTES};
use crate::error::{EngineError, ErrorCode};
use crate::limits::ResourceLimits;
use crate::weight::{SiteId, Weight};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpKind {
    /// One UTF-16 code unit, matching JavaScript and CodeMirror indexing.
    Ins {
        unit: u16,
    },
    Del,
}

/// Wire operation. `seq` is the origin's reliable-broadcast sequence
/// (transport). `counter` is the paper's insertion counter c.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Op {
    pub kind: OpKind,
    pub weight: Weight,
    pub counter: u64,
    pub origin: SiteId,
    pub seq: u64,
}

impl Op {
    pub fn ins(weight: Weight, unit: u16, counter: u64, origin: SiteId, seq: u64) -> Self {
        Op {
            kind: OpKind::Ins { unit },
            weight,
            counter,
            origin,
            seq,
        }
    }

    pub fn del(weight: Weight, counter: u64, origin: SiteId, seq: u64) -> Self {
        Op {
            kind: OpKind::Del,
            weight,
            counter,
            origin,
            seq,
        }
    }

    pub fn is_ins(&self) -> bool {
        matches!(self.kind, OpKind::Ins { .. })
    }

    /// `[tag][origin][seq varint][c varint][weight][utf16?]`
    ///
    /// The weight elides its site whenever it equals `origin` — always true
    /// for insertions — plus its default sequence path and zero sequence
    /// number (format v2, Extension 2).
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.push(match self.kind {
            OpKind::Ins { .. } => 1,
            OpKind::Del => 2,
        });
        b.extend_from_slice(&self.origin.to_le_bytes());
        write_uvarint(&mut b, self.seq);
        write_uvarint(&mut b, self.counter);
        write_weight(&mut b, &self.weight, self.origin);
        if let OpKind::Ins { unit } = self.kind {
            b.extend_from_slice(&unit.to_le_bytes());
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
                "operation exceeds message byte limit",
            ));
        }
        if buf.len() < 1 + 16 + 1 + 1 + MIN_WEIGHT_BYTES {
            return Err(EngineError::malformed("operation is too short"));
        }
        let mut reader = Reader::new(buf);
        let tag = reader.u8()?;
        let origin = reader.u128()?;
        let seq = reader.uvarint()?;
        let counter = reader.uvarint()?;

        // Site zero is reserved for sentinels. Sequence and insertion
        // counters start at one. Rejecting these values here keeps malformed
        // operations out of the replica rather than making every caller
        // rediscover the invariants.
        if origin == 0 || seq == 0 || counter == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "operation identities must be nonzero",
            ));
        }
        let weight = read_weight(&mut reader, limits, origin)?;
        let op = match tag {
            1 => {
                let unit = reader.u16()?;
                Op::ins(weight, unit, counter, origin, seq)
            }
            2 => Op::del(weight, counter, origin, seq),
            _ => {
                return Err(EngineError::new(
                    ErrorCode::InvalidOperation,
                    "unknown operation tag",
                ))
            }
        };
        if !reader.is_finished() {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "operation contains trailing bytes",
            ));
        }
        Ok(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fraction::Fraction;

    #[test]
    fn roundtrip() {
        let w = Weight::new(Fraction::new(1, 4), 2, vec![0, 5], 3);
        let op = Op::ins(w, 'λ' as u16, 9, 3, 4);
        assert_eq!(Op::decode(&op.encode()).unwrap(), op);

        let foreign = Weight::new(Fraction::new(3, 5), -1, vec![0], 7);
        let delete = Op::del(foreign, 2, 3, 5);
        assert_eq!(Op::decode(&delete.encode()).unwrap(), delete);
    }

    #[test]
    fn insertion_weight_site_is_implicit_in_the_origin() {
        let own = Op::ins(
            Weight::new(Fraction::new(1, 2), 0, vec![0], 3),
            'A' as u16,
            1,
            3,
            1,
        );
        let foreign = Op::del(Weight::new(Fraction::new(1, 2), 0, vec![0], 9), 1, 3, 1);
        assert_eq!(foreign.encode().len() - own.encode().len(), 16 - 2);
        assert_eq!(Op::decode(&own.encode()).unwrap().weight.site, 3);
        assert_eq!(Op::decode(&foreign.encode()).unwrap().weight.site, 9);
    }

    /// Byte-for-byte layout of the `roundtrip` insertion with small varints:
    /// `[tag][origin;16][seq][counter][flags][p][q][sn][sc_len][sc…][unit;2]`.
    fn manual_bytes(p: u8, q: u8) -> Vec<u8> {
        let mut b = vec![1u8];
        b.extend_from_slice(&3u128.to_le_bytes());
        b.push(4); // seq
        b.push(9); // counter
        b.push(0b11); // sn present, sc present, site implicit
        b.push(p);
        b.push(q);
        b.push(4); // zigzag(2)
        b.push(2); // sc length
        b.push(0);
        b.push(5);
        b.extend_from_slice(&('A' as u16).to_le_bytes());
        b
    }

    #[test]
    fn rejects_trailing_and_noncanonical_fields() {
        let w = Weight::new(Fraction::new(1, 4), 2, vec![0, 5], 3);
        let op = Op::ins(w, 'A' as u16, 9, 3, 4);
        assert_eq!(op.encode(), manual_bytes(1, 4));

        let mut trailing = op.encode();
        trailing.push(0);
        assert!(Op::decode(&trailing).is_none());

        let mut zero_origin = op.encode();
        zero_origin[1..17].copy_from_slice(&0u128.to_le_bytes());
        assert!(Op::decode(&zero_origin).is_none());

        // Numerator 2 and denominator 8 encode the same fraction as 1/4 but
        // are not the canonical representation emitted by the encoder.
        assert!(Op::decode(&manual_bytes(2, 8)).is_none());
    }
}
