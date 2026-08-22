//! Small exact reader and the canonical ESBT weight codec shared by wire
//! types (engine format version 2).
//!
//! Extension 2 (paper §10): the sequence path and its neighbors are the only
//! variable-length identifier components, and format v1 spent fixed-width
//! fields on all of them. Version 2 removes only redundancy the engine can
//! prove and re-verify at decode time:
//!
//! - the default path `sc = [0]` is implicit (the paper's own §8.3.1 remark);
//! - `sn = 0` is implicit;
//! - an operation's weight omits its 16-byte site when it equals the
//!   operation origin (an invariant `import_operation` already enforces for
//!   insertions);
//! - all counts, fraction terms, and path digits are canonical LEB128
//!   varints — a non-minimal encoding is rejected, preserving the
//!   one-state-one-byte-sequence rule the decoders rely on.
//!
//! Sorted containers (snapshot atoms and the delete log) additionally
//! front-code each sequence path against its predecessor; that context lives
//! in `snapshot.rs`, built from the primitives here.

use crate::error::{EngineError, ErrorCode};
use crate::fraction::Fraction;
use crate::limits::ResourceLimits;
use crate::weight::{SiteId, Weight};

/// Minimum encoded weight: flags + p + q with everything else implicit.
pub(crate) const MIN_WEIGHT_BYTES: usize = 3;

const FLAG_SN_PRESENT: u8 = 1 << 0;
const FLAG_SC_PRESENT: u8 = 1 << 1;
const FLAG_SITE_INLINE: u8 = 1 << 2;
const FLAG_KNOWN: u8 = FLAG_SN_PRESENT | FLAG_SC_PRESENT | FLAG_SITE_INLINE;

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

    /// Canonical LEB128. Non-minimal encodings and values above 64 bits are
    /// rejected so equal states have exactly one byte representation.
    pub(crate) fn uvarint(&mut self) -> Result<u64, EngineError> {
        let mut value: u64 = 0;
        for index in 0..10usize {
            let byte = self.u8()?;
            let group = u64::from(byte & 0x7f);
            if index == 9 && group > 1 {
                return Err(EngineError::new(
                    ErrorCode::IntegerOverflow,
                    "varint exceeds 64 bits",
                ));
            }
            value |= group << (index * 7);
            if byte & 0x80 == 0 {
                if index > 0 && group == 0 {
                    return Err(EngineError::new(
                        ErrorCode::NonCanonicalEncoding,
                        "varint is not minimal",
                    ));
                }
                return Ok(value);
            }
        }
        Err(EngineError::malformed("unterminated varint"))
    }

    pub(crate) fn ivarint(&mut self) -> Result<i64, EngineError> {
        Ok(unzigzag(self.uvarint()?))
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

pub(crate) fn write_uvarint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let group = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(group);
            return;
        }
        out.push(group | 0x80);
    }
}

pub(crate) fn write_ivarint(out: &mut Vec<u8>, value: i64) {
    write_uvarint(out, zigzag(value));
}

pub(crate) fn uvarint_len(mut value: u64) -> usize {
    let mut length = 1;
    while value >= 0x80 {
        value >>= 7;
        length += 1;
    }
    length
}

fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

fn unzigzag(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

/// How a weight's owning site travels on the wire.
#[derive(Clone, Copy)]
pub(crate) enum SiteContext<'a> {
    /// Omit the 16-byte site when it equals the surrounding context (an
    /// operation's origin). Context 0 means "no context": always inline.
    Origin(SiteId),
    /// Reference a sorted, strictly ascending site table by varint index.
    Table(&'a [SiteId]),
}

fn is_default_path(sc: &[u32]) -> bool {
    sc == [0]
}

/// Shared body: flags, fraction, sequence number, and — unless the caller
/// front-codes it — the sequence path. `shared_prefix` is the exact longest
/// common prefix with the container's previous path when front-coding, or
/// `None` for self-contained weights.
pub(crate) fn write_weight_parts(
    out: &mut Vec<u8>,
    weight: &Weight,
    site: SiteContext<'_>,
    shared_prefix: Option<usize>,
) {
    let mut flags = 0u8;
    if weight.sn != 0 {
        flags |= FLAG_SN_PRESENT;
    }
    if !is_default_path(&weight.sc) {
        flags |= FLAG_SC_PRESENT;
    }
    let inline_site = match site {
        SiteContext::Origin(context) => context == 0 || weight.site != context,
        SiteContext::Table(_) => false,
    };
    if inline_site {
        flags |= FLAG_SITE_INLINE;
    }
    out.push(flags);
    write_uvarint(out, weight.f.p as u64);
    write_uvarint(out, weight.f.q as u64);
    if flags & FLAG_SN_PRESENT != 0 {
        write_ivarint(out, weight.sn);
    }
    if flags & FLAG_SC_PRESENT != 0 {
        match shared_prefix {
            Some(shared) => {
                write_uvarint(out, shared as u64);
                write_uvarint(out, (weight.sc.len() - shared) as u64);
                for &digit in &weight.sc[shared..] {
                    write_uvarint(out, u64::from(digit));
                }
            }
            None => {
                write_uvarint(out, weight.sc.len() as u64);
                for &digit in &weight.sc {
                    write_uvarint(out, u64::from(digit));
                }
            }
        }
    }
    match site {
        SiteContext::Origin(_) => {
            if inline_site {
                out.extend_from_slice(&weight.site.to_le_bytes());
            }
        }
        SiteContext::Table(table) => {
            let index = table
                .binary_search(&weight.site)
                .expect("site table covers every encoded weight");
            write_uvarint(out, index as u64);
        }
    }
}

/// Exact inverse of `write_weight_parts`. `previous_path` supplies the
/// front-coding context; the decoder recomputes the true longest common
/// prefix and rejects any encoding that did not use it, so front-coded
/// containers stay canonical.
pub(crate) fn read_weight_parts(
    reader: &mut Reader<'_>,
    limits: &ResourceLimits,
    site: SiteContext<'_>,
    previous_path: Option<&[u32]>,
) -> Result<Weight, EngineError> {
    let flags = reader.u8()?;
    if flags & !FLAG_KNOWN != 0 {
        return Err(EngineError::new(
            ErrorCode::NonCanonicalEncoding,
            "weight flags contain unknown bits",
        ));
    }
    if matches!(site, SiteContext::Table(_)) && flags & FLAG_SITE_INLINE != 0 {
        return Err(EngineError::new(
            ErrorCode::NonCanonicalEncoding,
            "table-coded weight inlines its site",
        ));
    }
    let p = reader.uvarint()?;
    let q = reader.uvarint()?;
    if p == 0 || q == 0 || p > i64::MAX as u64 || q > i64::MAX as u64 {
        return Err(EngineError::new(
            ErrorCode::NonCanonicalEncoding,
            "weight fraction is not positive and finite",
        ));
    }
    let (p, q) = (p as i64, q as i64);
    let fraction = Fraction::new(p, q);
    if fraction != (Fraction { p, q }) {
        return Err(EngineError::new(
            ErrorCode::NonCanonicalEncoding,
            "weight fraction is not reduced",
        ));
    }

    let sn = if flags & FLAG_SN_PRESENT != 0 {
        let sn = reader.ivarint()?;
        if sn == 0 {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "zero sequence number must be implicit",
            ));
        }
        sn
    } else {
        0
    };

    let sc = if flags & FLAG_SC_PRESENT != 0 {
        let sc = match previous_path {
            Some(previous) => {
                let shared = reader.uvarint()? as usize;
                let suffix = reader.uvarint()? as usize;
                let total = shared.checked_add(suffix).ok_or_else(|| {
                    EngineError::new(ErrorCode::IntegerOverflow, "identifier length overflow")
                })?;
                if shared > previous.len() || total == 0 {
                    return Err(EngineError::new(
                        ErrorCode::NonCanonicalEncoding,
                        "front-coded path prefix is invalid",
                    ));
                }
                check_path_budget(reader, limits, total, suffix)?;
                let mut sc = Vec::with_capacity(total);
                sc.extend_from_slice(&previous[..shared]);
                for _ in 0..suffix {
                    let digit = reader.uvarint()?;
                    if digit > u64::from(u32::MAX) {
                        return Err(EngineError::malformed("identifier digit overflow"));
                    }
                    sc.push(digit as u32);
                }
                let true_shared = longest_common_prefix(previous, &sc);
                if shared != true_shared.min(total) {
                    return Err(EngineError::new(
                        ErrorCode::NonCanonicalEncoding,
                        "front-coded path does not use the exact common prefix",
                    ));
                }
                sc
            }
            None => {
                let total = reader.uvarint()? as usize;
                if total == 0 {
                    return Err(EngineError::new(
                        ErrorCode::NonCanonicalEncoding,
                        "explicit sequence path must not be empty",
                    ));
                }
                check_path_budget(reader, limits, total, total)?;
                let mut sc = Vec::with_capacity(total);
                for _ in 0..total {
                    let digit = reader.uvarint()?;
                    if digit > u64::from(u32::MAX) {
                        return Err(EngineError::malformed("identifier digit overflow"));
                    }
                    sc.push(digit as u32);
                }
                sc
            }
        };
        if is_default_path(&sc) {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "default sequence path must be implicit",
            ));
        }
        sc
    } else {
        vec![0]
    };

    let site = match site {
        SiteContext::Origin(context) => {
            if flags & FLAG_SITE_INLINE != 0 {
                let site = reader.u128()?;
                if site == 0 || site == context {
                    return Err(EngineError::new(
                        ErrorCode::NonCanonicalEncoding,
                        "inline weight site is zero or should be implicit",
                    ));
                }
                site
            } else {
                if context == 0 {
                    return Err(EngineError::new(
                        ErrorCode::InvalidOperation,
                        "weight requires a site but none is in context",
                    ));
                }
                context
            }
        }
        SiteContext::Table(table) => {
            let index = reader.uvarint()? as usize;
            *table.get(index).ok_or_else(|| {
                EngineError::malformed("weight references a missing site table entry")
            })?
        }
    };

    Ok(Weight::new(fraction, sn, sc, site))
}

fn check_path_budget(
    reader: &Reader<'_>,
    limits: &ResourceLimits,
    total: usize,
    wire_digits: usize,
) -> Result<(), EngineError> {
    if total > limits.max_identifier_depth {
        return Err(EngineError::new(
            ErrorCode::IdentifierTooDeep,
            "identifier path exceeds resource policy",
        ));
    }
    // Every wire digit is at least one byte; reject impossible counts before
    // allocating attacker-controlled capacity.
    if wire_digits > reader.remaining() {
        return Err(EngineError::malformed("truncated identifier path"));
    }
    Ok(())
}

pub(crate) fn longest_common_prefix(a: &[u32], b: &[u32]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

/// Self-contained weight with an operation-origin site context.
pub(crate) fn write_weight(out: &mut Vec<u8>, weight: &Weight, origin: SiteId) {
    write_weight_parts(out, weight, SiteContext::Origin(origin), None);
}

pub(crate) fn read_weight(
    reader: &mut Reader<'_>,
    limits: &ResourceLimits,
    origin: SiteId,
) -> Result<Weight, EngineError> {
    let weight = read_weight_parts(reader, limits, SiteContext::Origin(origin), None)?;
    if weight.site == Weight::EMPTY_SITE {
        return Err(EngineError::new(
            ErrorCode::InvalidOperation,
            "document weights require a nonzero site",
        ));
    }
    Ok(weight)
}

/// Encoded size of a self-contained weight, used as the identifier-cost
/// signal by the adaptive allocator without materializing bytes.
pub(crate) fn encoded_weight_len(weight: &Weight, origin: SiteId) -> usize {
    let mut length = 1 + uvarint_len(weight.f.p as u64) + uvarint_len(weight.f.q as u64);
    if weight.sn != 0 {
        length += uvarint_len(zigzag(weight.sn));
    }
    if !is_default_path(&weight.sc) {
        length += uvarint_len(weight.sc.len() as u64);
        for &digit in &weight.sc {
            length += uvarint_len(u64::from(digit));
        }
    }
    if origin == 0 || weight.site != origin {
        length += 16;
    }
    length
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(weight: &Weight, origin: SiteId) -> Weight {
        let mut bytes = Vec::new();
        write_weight(&mut bytes, weight, origin);
        assert_eq!(bytes.len(), encoded_weight_len(weight, origin));
        let mut reader = Reader::new(&bytes);
        let decoded = read_weight(&mut reader, &ResourceLimits::default(), origin).expect("decode");
        assert!(reader.is_finished());
        decoded
    }

    #[test]
    fn weight_roundtrips_with_implicit_and_explicit_fields() {
        let defaulted = Weight::new(Fraction::new(1, 2), 0, vec![0], 7);
        assert_eq!(roundtrip(&defaulted, 7), defaulted);
        assert_eq!(encoded_weight_len(&defaulted, 7), 3);

        let full = Weight::new(Fraction::new(355, 113), -9, vec![0, 5, 1 << 20], 9);
        assert_eq!(roundtrip(&full, 7), full);
        assert_eq!(roundtrip(&full, 0), full);
    }

    #[test]
    fn varints_must_be_minimal() {
        let mut padded = Reader::new(&[0x80, 0x00]);
        assert!(padded.uvarint().is_err());
        let mut minimal = Reader::new(&[0x00]);
        assert_eq!(minimal.uvarint().unwrap(), 0);
        let mut max = Vec::new();
        write_uvarint(&mut max, u64::MAX);
        let mut reader = Reader::new(&max);
        assert_eq!(reader.uvarint().unwrap(), u64::MAX);
    }

    #[test]
    fn implicit_fields_reject_explicit_defaults() {
        // sn = 0 written explicitly.
        let mut bytes = vec![FLAG_SN_PRESENT, 1, 2, 0];
        let mut reader = Reader::new(&bytes);
        assert!(read_weight(&mut reader, &ResourceLimits::default(), 7).is_err());

        // sc = [0] written explicitly.
        bytes = vec![FLAG_SC_PRESENT, 1, 2, 1, 0];
        let mut reader = Reader::new(&bytes);
        assert!(read_weight(&mut reader, &ResourceLimits::default(), 7).is_err());

        // Inline site equal to the context origin.
        let mut inline = vec![FLAG_SITE_INLINE, 1, 2];
        inline.extend_from_slice(&7u128.to_le_bytes());
        let mut reader = Reader::new(&inline);
        assert!(read_weight(&mut reader, &ResourceLimits::default(), 7).is_err());
    }

    #[test]
    fn front_coding_requires_the_exact_common_prefix() {
        let limits = ResourceLimits::default();
        let previous = vec![3u32, 4, 5];
        let weight = Weight::new(Fraction::new(1, 2), 0, vec![3, 4, 9], 7);

        let mut bytes = Vec::new();
        write_weight_parts(
            &mut bytes,
            &weight,
            SiteContext::Table(&[7]),
            Some(longest_common_prefix(&previous, &weight.sc)),
        );
        let mut reader = Reader::new(&bytes);
        let decoded =
            read_weight_parts(&mut reader, &limits, SiteContext::Table(&[7]), Some(&previous))
                .expect("front-coded decode");
        assert_eq!(decoded, weight);
        assert!(reader.is_finished());

        // A lazy encoder that shares fewer digits than it could is rejected.
        let mut lazy = Vec::new();
        write_weight_parts(&mut lazy, &weight, SiteContext::Table(&[7]), Some(1));
        let mut reader = Reader::new(&lazy);
        assert!(read_weight_parts(&mut reader, &limits, SiteContext::Table(&[7]), Some(&previous))
            .is_err());
    }
}
