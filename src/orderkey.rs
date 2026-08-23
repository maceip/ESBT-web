//! Order-preserving byte keys for ESBT weights (Extension 3).
//!
//! Yjs and Automerge have no identifier-allocation seam to plug into — their
//! ordering is origin/causality derived — but shipping frameworks *do*
//! expose a fractional-index / position-string slot (Figma-style ordered
//! sequences, Loro's movable-tree sibling order, sortable database keys).
//! That slot accepts any generator of totally ordered keys with a
//! mint-between operation. This module makes ESBT that generator: a `Weight`
//! is encoded to a byte string whose plain `memcmp` order equals
//! Definition 2's weight order, and `key_between` mints a fresh key strictly
//! inside any gap using the full ESBT allocator (bounded fractions, `sn`
//! ladder, `sc` paths, adaptive `Dmax`, pluggable strategy).
//!
//! Layout (all segments order-aligned):
//!
//! 1. The fraction as its Stern–Brocot L/R path in run-length form — the
//!    classical result that lexicographic path order is rational order. Runs
//!    strictly alternate. Each run is a marker byte (`L = 0x00`,
//!    `R = 0x02`) plus an order-preserving length-prefixed integer,
//!    complemented for `L` runs so longer `L` runs sort lower. The segment
//!    ends with a terminator `0x01` that sorts strictly between `L` and `R`,
//!    exactly where the node itself sits between its subtrees.
//! 2. `sn`, sign-biased big-endian.
//! 3. The sequence path: one `0x01`-marked order-preserving digit per
//!    component, closed by `0x00` so a shorter path precedes its extensions
//!    (Definition 5 as refined by `sc_cmp`).
//! 4. The site, big-endian.
//!
//! Keys are prefix-free and decodable; decoding re-encodes and compares so
//! only the canonical byte form of each weight is accepted.

use crate::allocator::Allocator;
use crate::error::{EngineError, ErrorCode};
use crate::fraction::Fraction;
use crate::limits::ResourceLimits;
use crate::weight::{SiteId, Weight};

const RUN_LEFT: u8 = 0x00;
const FRACTION_END: u8 = 0x01;
const RUN_RIGHT: u8 = 0x02;
const PATH_END: u8 = 0x00;
const PATH_DIGIT: u8 = 0x01;

/// `[significant-byte count][big-endian bytes]`: longer values sort after
/// shorter ones, so byte order equals numeric order.
fn push_ordered(out: &mut Vec<u8>, value: u64, complement: bool) {
    let significant = ((64 - (value | 1).leading_zeros()) as usize).div_ceil(8);
    let start = out.len();
    out.push(significant as u8);
    out.extend_from_slice(&value.to_be_bytes()[8 - significant..]);
    if complement {
        for byte in &mut out[start..] {
            *byte = !*byte;
        }
    }
}

fn read_ordered(bytes: &[u8], offset: &mut usize, complement: bool) -> Result<u64, EngineError> {
    let raw = *bytes
        .get(*offset)
        .ok_or_else(|| EngineError::malformed("truncated ordered integer"))?;
    let count = usize::from(if complement { !raw } else { raw });
    if count > 8 {
        return Err(EngineError::malformed("ordered integer is too wide"));
    }
    *offset += 1;
    let mut value = 0u64;
    for _ in 0..count {
        let byte = *bytes
            .get(*offset)
            .ok_or_else(|| EngineError::malformed("truncated ordered integer"))?;
        *offset += 1;
        value = (value << 8) | u64::from(if complement { !byte } else { byte });
    }
    Ok(value)
}

/// Stern–Brocot path of `p/q` as strictly alternating `(is_right, length)`
/// runs — the run-length form of the continued-fraction expansion.
fn stern_brocot_runs(fraction: Fraction) -> Vec<(bool, u64)> {
    let (mut p, mut q) = (fraction.p, fraction.q);
    let mut right = true;
    let mut runs: Vec<(bool, u64)> = Vec::new();
    while q != 0 {
        runs.push((right, (p / q) as u64));
        let remainder = p % q;
        p = q;
        q = remainder;
        right = !right;
    }
    if let Some(last) = runs.last_mut() {
        last.1 -= 1;
    }
    runs.retain(|(_, length)| *length > 0);
    runs
}

fn fraction_from_runs(runs: &[(bool, u64)]) -> Result<Fraction, EngineError> {
    if runs.is_empty() {
        return Ok(Fraction::ONE);
    }
    // Continued-fraction coefficients: a leading L run means a0 = 0, and the
    // final coefficient absorbs the tree/CF off-by-one.
    let mut coefficients: Vec<u64> = Vec::with_capacity(runs.len() + 1);
    if !runs[0].0 {
        coefficients.push(0);
    }
    for &(_, length) in runs {
        coefficients.push(length);
    }
    let last = coefficients.last_mut().expect("nonempty coefficients");
    *last = last
        .checked_add(1)
        .ok_or_else(|| EngineError::new(ErrorCode::IntegerOverflow, "fraction run overflow"))?;

    // Convergent recurrence: h₋₂/k₋₂ = 0/1, h₋₁/k₋₁ = 1/0.
    let (mut p_prev, mut q_prev) = (0i128, 1i128);
    let (mut p, mut q) = (1i128, 0i128);
    for &a in &coefficients {
        let a = i128::from(a);
        let p_next = a
            .checked_mul(p)
            .and_then(|value| value.checked_add(p_prev))
            .ok_or_else(|| EngineError::new(ErrorCode::IntegerOverflow, "fraction overflow"))?;
        let q_next = a
            .checked_mul(q)
            .and_then(|value| value.checked_add(q_prev))
            .ok_or_else(|| EngineError::new(ErrorCode::IntegerOverflow, "fraction overflow"))?;
        (p_prev, q_prev) = (p, q);
        (p, q) = (p_next, q_next);
    }
    if p < 1 || q < 1 || p > i64::MAX as i128 || q > i64::MAX as i128 {
        return Err(EngineError::new(
            ErrorCode::NonCanonicalEncoding,
            "key fraction is out of range",
        ));
    }
    Ok(Fraction {
        p: p as i64,
        q: q as i64,
    })
}

/// Encode a document weight as an order-preserving, prefix-free byte key.
pub fn order_key(weight: &Weight) -> Vec<u8> {
    let mut out = Vec::new();
    for (is_right, length) in stern_brocot_runs(weight.f) {
        out.push(if is_right { RUN_RIGHT } else { RUN_LEFT });
        push_ordered(&mut out, length, !is_right);
    }
    out.push(FRACTION_END);
    out.extend_from_slice(&((weight.sn as u64) ^ (1 << 63)).to_be_bytes());
    for &digit in &weight.sc {
        out.push(PATH_DIGIT);
        push_ordered(&mut out, u64::from(digit), false);
    }
    out.push(PATH_END);
    out.extend_from_slice(&weight.site.to_be_bytes());
    out
}

/// Exact inverse of `order_key`. Only the canonical byte form of a weight is
/// accepted: the decoded weight is re-encoded and compared.
pub fn weight_from_order_key(bytes: &[u8], limits: &ResourceLimits) -> Result<Weight, EngineError> {
    if bytes.len() > limits.max_message_bytes {
        return Err(EngineError::new(
            ErrorCode::MessageTooLarge,
            "order key exceeds resource policy",
        ));
    }
    let mut offset = 0usize;
    let mut runs: Vec<(bool, u64)> = Vec::new();
    loop {
        let marker = *bytes
            .get(offset)
            .ok_or_else(|| EngineError::malformed("truncated order key"))?;
        offset += 1;
        let is_right = match marker {
            FRACTION_END => break,
            RUN_LEFT => false,
            RUN_RIGHT => true,
            _ => return Err(EngineError::malformed("invalid fraction run marker")),
        };
        if runs.len() >= 128 {
            // A fraction with i64 terms has at most ~92 continued-fraction
            // coefficients; anything longer cannot decode to a valid weight.
            return Err(EngineError::malformed("order key fraction is too deep"));
        }
        if runs
            .last()
            .is_some_and(|(previous, _)| *previous == is_right)
        {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "fraction runs do not alternate",
            ));
        }
        let length = read_ordered(bytes, &mut offset, !is_right)?;
        if length == 0 {
            return Err(EngineError::new(
                ErrorCode::NonCanonicalEncoding,
                "fraction run is empty",
            ));
        }
        runs.push((is_right, length));
    }
    let fraction = fraction_from_runs(&runs)?;

    let sn_bytes = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| EngineError::malformed("truncated order key"))?;
    offset += 8;
    let sn = (u64::from_be_bytes(sn_bytes.try_into().expect("8 bytes")) ^ (1 << 63)) as i64;

    let mut sc = Vec::new();
    loop {
        let marker = *bytes
            .get(offset)
            .ok_or_else(|| EngineError::malformed("truncated order key"))?;
        offset += 1;
        match marker {
            PATH_END => break,
            PATH_DIGIT => {
                if sc.len() >= limits.max_identifier_depth {
                    return Err(EngineError::new(
                        ErrorCode::IdentifierTooDeep,
                        "order key path exceeds resource policy",
                    ));
                }
                let digit = read_ordered(bytes, &mut offset, false)?;
                if digit > u64::from(u32::MAX) {
                    return Err(EngineError::malformed("order key digit overflow"));
                }
                sc.push(digit as u32);
            }
            _ => return Err(EngineError::malformed("invalid path marker")),
        }
    }

    let site_bytes = bytes
        .get(offset..offset + 16)
        .ok_or_else(|| EngineError::malformed("truncated order key"))?;
    offset += 16;
    if offset != bytes.len() {
        return Err(EngineError::new(
            ErrorCode::NonCanonicalEncoding,
            "order key contains trailing bytes",
        ));
    }
    let site = u128::from_be_bytes(site_bytes.try_into().expect("16 bytes"));
    if site == 0 || sc.is_empty() {
        return Err(EngineError::new(
            ErrorCode::InvalidOperation,
            "order keys describe document weights only",
        ));
    }

    let weight = Weight::new(fraction, sn, sc, site);
    if order_key(&weight) != bytes {
        return Err(EngineError::new(
            ErrorCode::NonCanonicalEncoding,
            "order key is not the canonical encoding of its weight",
        ));
    }
    Ok(weight)
}

/// Mint a key strictly between two existing keys (`None` = document edge).
///
/// This is the framework-facing plug: any host that stores sortable byte
/// strings can call this instead of a fractional-index generator and gets
/// ESBT's bounded identifier growth in exchange.
pub fn key_between(
    allocator: &mut Allocator,
    left: Option<&[u8]>,
    right: Option<&[u8]>,
    site: SiteId,
    limits: &ResourceLimits,
) -> Result<Vec<u8>, EngineError> {
    if site == 0 {
        return Err(EngineError::new(
            ErrorCode::InvalidSiteId,
            "site 0 is reserved for sentinels",
        ));
    }
    let left = match left {
        Some(bytes) => weight_from_order_key(bytes, limits)?,
        None => Weight::begin(),
    };
    let right = match right {
        Some(bytes) => weight_from_order_key(bytes, limits)?,
        None => Weight::end(),
    };
    if left >= right {
        return Err(EngineError::new(
            ErrorCode::InvalidRange,
            "order keys are not an ascending gap",
        ));
    }
    let weight = allocator
        .create_weight(&left, &right, site)
        .ok_or_else(|| {
            EngineError::new(
                ErrorCode::AllocationExhausted,
                "the requested key gap has no available identifier",
            )
        })?;
    Ok(order_key(&weight))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ResourceLimits {
        ResourceLimits::default()
    }

    #[test]
    fn classical_stern_brocot_paths() {
        // 5/12 = LLRRL and 13/8 = RLRLR (Graham et al.; Niqui).
        assert_eq!(
            stern_brocot_runs(Fraction::new(5, 12)),
            vec![(false, 2), (true, 2), (false, 1)]
        );
        assert_eq!(
            stern_brocot_runs(Fraction::new(13, 8)),
            vec![(true, 1), (false, 1), (true, 1), (false, 1), (true, 1)]
        );
        assert_eq!(stern_brocot_runs(Fraction::ONE), vec![]);
        for fraction in [Fraction::new(5, 12), Fraction::new(13, 8), Fraction::ONE] {
            assert_eq!(
                fraction_from_runs(&stern_brocot_runs(fraction)).unwrap(),
                fraction
            );
        }
    }

    fn sample_weights() -> Vec<Weight> {
        let mut samples = Vec::new();
        let fractions = [
            Fraction::new(1, 4),
            Fraction::new(1, 3),
            Fraction::new(2, 5),
            Fraction::new(1, 2),
            Fraction::new(3, 5),
            Fraction::ONE,
            Fraction::new(13, 8),
            Fraction::new(3, 1),
            Fraction::new(40_000, 1),
            Fraction::new(46_368, 75_025),
        ];
        for fraction in fractions {
            for sn in [-3i64, 0, 7] {
                for sc in [vec![0u32], vec![0, 5], vec![1], vec![0, 5, 1 << 30]] {
                    for site in [1u128, 2, u128::MAX] {
                        samples.push(Weight::new(fraction, sn, sc.clone(), site));
                    }
                }
            }
        }
        samples
    }

    #[test]
    fn key_order_equals_weight_order_and_keys_are_prefix_free() {
        let weights = sample_weights();
        let keys: Vec<Vec<u8>> = weights.iter().map(order_key).collect();
        for (i, a) in weights.iter().enumerate() {
            for (j, b) in weights.iter().enumerate() {
                assert_eq!(
                    keys[i].cmp(&keys[j]),
                    a.cmp(b),
                    "byte order diverged for {a} vs {b}"
                );
                if i != j {
                    assert!(!keys[j].starts_with(&keys[i]), "{a} prefixes {b}");
                }
            }
        }
    }

    #[test]
    fn keys_roundtrip_and_reject_noncanonical_bytes() {
        for weight in sample_weights() {
            let key = order_key(&weight);
            assert_eq!(weight_from_order_key(&key, &limits()).unwrap(), weight);

            let mut trailing = key.clone();
            trailing.push(0);
            assert!(weight_from_order_key(&trailing, &limits()).is_err());
            assert!(weight_from_order_key(&key[..key.len() - 1], &limits()).is_err());
        }
        // An unreduced or out-of-order byte form never decodes: fabricate a
        // non-alternating run sequence.
        let mut bad = Vec::new();
        bad.push(RUN_RIGHT);
        push_ordered(&mut bad, 1, false);
        bad.push(RUN_RIGHT);
        push_ordered(&mut bad, 1, false);
        bad.push(FRACTION_END);
        assert!(weight_from_order_key(&bad, &limits()).is_err());
    }

    #[test]
    fn key_between_fills_any_gap_in_order() {
        let mut allocator = Allocator::new(5, 10, 3);
        let limits = limits();
        let first = key_between(&mut allocator, None, None, 1, &limits).unwrap();

        // Append run, prepend run, then repeated middle splits: the byte
        // order must stay strict throughout, across every allocation layer.
        let mut keys = vec![first];
        for _ in 0..24 {
            let last = keys.last().unwrap().clone();
            keys.push(key_between(&mut allocator, Some(&last), None, 1, &limits).unwrap());
        }
        for _ in 0..24 {
            let first = keys[0].clone();
            keys.insert(
                0,
                key_between(&mut allocator, None, Some(&first), 1, &limits).unwrap(),
            );
        }
        for _ in 0..24 {
            let mid = keys.len() / 2;
            let key = key_between(
                &mut allocator,
                Some(&keys[mid - 1]),
                Some(&keys[mid]),
                2,
                &limits,
            )
            .unwrap();
            keys.insert(mid, key);
        }
        for pair in keys.windows(2) {
            assert!(pair[0] < pair[1], "keys out of order");
        }
        assert!(key_between(&mut allocator, Some(&keys[1]), Some(&keys[0]), 1, &limits).is_err());
    }
}
