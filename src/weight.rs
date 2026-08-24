//! Definition 1 (Weight) and Definition 2 (total order).

use crate::allocator::DMAX_HARD_CEILING;
use crate::fraction::Fraction;
use crate::{EngineError, ErrorCode};
use core::cmp::Ordering;
use core::fmt;

/// Collision-resistant replica identity.
///
/// Product identity is intentionally outside the CRDT. This random 128-bit
/// value only separates independently minted ESBT operation streams.
pub type SiteId = u128;

/// W = ⟨f, sn, sc, δ⟩. Sentinels use δ = ∅ encoded as 0.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Weight {
    pub f: Fraction,
    pub sn: i64,
    pub sc: Vec<u32>,
    pub site: SiteId,
}

impl Weight {
    pub const EMPTY_SITE: SiteId = 0;

    pub fn begin() -> Self {
        Self::new(Fraction::BEGIN, 0, vec![0], Self::EMPTY_SITE)
    }

    pub fn end() -> Self {
        Self::new(Fraction::END, 0, vec![0], Self::EMPTY_SITE)
    }

    pub fn root() -> Self {
        Self::new(Fraction::ONE, 0, vec![0], Self::EMPTY_SITE)
    }

    pub fn new(f: Fraction, sn: i64, sc: Vec<u32>, site: SiteId) -> Self {
        Weight {
            f,
            sn,
            sc: if sc.is_empty() { vec![0] } else { sc },
            site,
        }
    }

    /// Validate a non-sentinel identifier admitted to a document.
    ///
    /// This is the single invariant check used by native `Update` values,
    /// canonical wire decoding, snapshots, anchors, and `Document` admission.
    /// Keeping it here prevents a forged native value from bypassing checks
    /// that happen to exist in the byte decoder.
    pub fn validate_document_identifier(
        &self,
        max_depth: Option<usize>,
    ) -> Result<(), EngineError> {
        if self.site == Self::EMPTY_SITE {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "document weight has the reserved zero site",
            ));
        }
        if self.f.p <= 0 || self.f.q <= 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "document weight fraction must be positive and finite",
            ));
        }
        if Fraction::new(self.f.p, self.f.q) != self.f {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "document weight fraction must be reduced",
            ));
        }
        if self.f.p > DMAX_HARD_CEILING || self.f.q > DMAX_HARD_CEILING {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "document weight fraction exceeds the global Dmax ceiling",
            ));
        }
        if self.sc.is_empty() {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "document identifier path is empty",
            ));
        }
        if max_depth.is_some_and(|maximum| self.sc.len() > maximum) {
            return Err(EngineError::new(
                ErrorCode::IdentifierTooDeep,
                "document identifier exceeds the depth limit",
            ));
        }
        Ok(())
    }
}

/// Definition 5. If one path is a proper prefix, the shorter precedes
/// (needed for Situation 3: [0] ≺ [0,5]).
pub fn sc_cmp(a: &[u32], b: &[u32]) -> Ordering {
    let n = a.len().min(b.len());
    for i in 0..n {
        match a[i].cmp(&b[i]) {
            Ordering::Equal => {}
            o => return o,
        }
    }
    a.len().cmp(&b.len())
}

impl PartialOrd for Weight {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Weight {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.f.cmp(&other.f) {
            Ordering::Equal => {}
            o => return o,
        }
        match self.sn.cmp(&other.sn) {
            Ordering::Equal => {}
            o => return o,
        }
        match sc_cmp(&self.sc, &other.sc) {
            Ordering::Equal => {}
            o => return o,
        }
        self.site.cmp(&other.site)
    }
}

impl fmt::Display for Weight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "⟨{},{},{:?},δ{}⟩", self.f, self.sn, self.sc, self.site)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition2() {
        let a = Weight::new(Fraction::new(1, 4), 0, vec![0], 1);
        let mid = Weight::new(Fraction::new(1, 4), 0, vec![0, 5], 1);
        let b = Weight::new(Fraction::new(1, 4), 1, vec![0], 1);
        let c = Weight::new(Fraction::new(2, 3), 0, vec![0], 1);
        assert!(a < mid && mid < b && b < c);
        let d = Weight::new(Fraction::new(1, 4), 1, vec![0], 2);
        assert!(b < d);
    }

    #[test]
    fn document_identifier_rejects_a_fraction_above_the_global_ceiling() {
        let weight = Weight::new(
            Fraction::new(DMAX_HARD_CEILING + 1, DMAX_HARD_CEILING),
            0,
            vec![0],
            1,
        );
        let error = weight
            .validate_document_identifier(None)
            .expect_err("fraction above Dmax ceiling");
        assert_eq!(error.code, ErrorCode::InvalidOperation);
    }
}
