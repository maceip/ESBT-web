//! Small exact reader and canonical ESBT weight codec shared by wire types.

use crate::error::{EngineError, ErrorCode};
use crate::fraction::Fraction;
use crate::limits::ResourceLimits;
use crate::weight::Weight;

pub(crate) const MIN_WEIGHT_BYTES: usize = 8 + 8 + 8 + 16 + 2 + 4;

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8], EngineError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| EngineError::new(ErrorCode::IntegerOverflow, "wire offset overflow"))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| EngineError::malformed("truncated input"))?;
        self.offset = end;
        Ok(value)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, EngineError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, EngineError> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| EngineError::malformed("invalid u16"))?,
        ))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, EngineError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| EngineError::malformed("invalid u32"))?,
        ))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, EngineError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| EngineError::malformed("invalid u64"))?,
        ))
    }

    pub(crate) fn u128(&mut self) -> Result<u128, EngineError> {
        Ok(u128::from_le_bytes(
            self.take(16)?
                .try_into()
                .map_err(|_| EngineError::malformed("invalid u128"))?,
        ))
    }

    pub(crate) fn i64(&mut self) -> Result<i64, EngineError> {
        Ok(i64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| EngineError::malformed("invalid i64"))?,
        ))
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

pub(crate) fn write_weight(out: &mut Vec<u8>, weight: &Weight) {
    out.extend_from_slice(&weight.f.p.to_le_bytes());
    out.extend_from_slice(&weight.f.q.to_le_bytes());
    out.extend_from_slice(&weight.sn.to_le_bytes());
    out.extend_from_slice(&weight.site.to_le_bytes());
    out.extend_from_slice(&(weight.sc.len() as u16).to_le_bytes());
    for component in &weight.sc {
        out.extend_from_slice(&component.to_le_bytes());
    }
}

pub(crate) fn read_weight(
    reader: &mut Reader<'_>,
    limits: &ResourceLimits,
) -> Result<Weight, EngineError> {
    let p = reader.i64()?;
    let q = reader.i64()?;
    let sn = reader.i64()?;
    let site = reader.u128()?;
    let path_len = usize::from(reader.u16()?);
    if site == 0 || path_len == 0 {
        return Err(EngineError::new(
            ErrorCode::InvalidOperation,
            "document weights require a nonzero site and path",
        ));
    }
    if path_len > limits.max_identifier_depth {
        return Err(EngineError::new(
            ErrorCode::IdentifierTooDeep,
            "identifier path exceeds resource policy",
        ));
    }
    let path_bytes = path_len.checked_mul(4).ok_or_else(|| {
        EngineError::new(ErrorCode::IntegerOverflow, "identifier length overflow")
    })?;
    if path_bytes > reader.remaining() {
        return Err(EngineError::malformed("truncated identifier path"));
    }
    let mut sc = Vec::with_capacity(path_len);
    for _ in 0..path_len {
        sc.push(reader.u32()?);
    }

    if p <= 0 || q <= 0 {
        return Err(EngineError::new(
            ErrorCode::NonCanonicalEncoding,
            "weight fraction is not positive and finite",
        ));
    }
    let fraction = Fraction::new(p, q);
    if fraction != (Fraction { p, q }) {
        return Err(EngineError::new(
            ErrorCode::NonCanonicalEncoding,
            "weight fraction is not reduced",
        ));
    }
    Ok(Weight::new(fraction, sn, sc, site))
}
