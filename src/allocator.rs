//! Algorithm 2 — CREATE_WEIGHT, plus the site Tracker (Definition 4).

use crate::fraction::Fraction;
use crate::newseq::newseq;
use crate::weight::{SiteId, Weight};
use std::collections::HashMap;

/// Tracker : f ↦ (snL, snR). Only fractions that have hit Dmax are tracked.
#[derive(Clone, Debug, Default)]
pub struct Tracker {
    map: HashMap<(i64, i64), (i64, i64)>,
}

impl Tracker {
    fn pair(&mut self, f: Fraction) -> (i64, i64) {
        *self.map.entry((f.p, f.q)).or_insert((0, 0))
    }

    fn set(&mut self, f: Fraction, sn_l: i64, sn_r: i64) {
        self.map.insert((f.p, f.q), (sn_l, sn_r));
    }
}

#[derive(Clone, Debug)]
pub struct Allocator {
    pub dmax: i64,
    pub base: u32,
    pub depth: u32,
    pub tracker: Tracker,
}

impl Allocator {
    pub fn new(dmax: i64, base: u32, depth: u32) -> Self {
        Allocator {
            dmax: dmax.max(2),
            base: base.max(2),
            depth: depth.max(1),
            tracker: Tracker::default(),
        }
    }

    /// Evaluation defaults (paper §8.1): base = 2^31-1, depth = 256.
    pub fn paper_eval() -> Self {
        Allocator::new(1 << 16, (1u32 << 31) - 1, 256)
    }

    /// Theorem 2: assign the mediant iff max(num, den) is within Dmax.
    ///
    /// Algorithm 2 line 10 is typeset with `or`, which would admit
    /// unbounded denominators (e.g. 1/10^9) and contradict line 9
    /// ("maximum num and den threshold"), Lemma 1's bound, Theorem 2,
    /// and Situation 1 (3/7 rejected at Dmax=5). The formal statements win.
    pub fn mediant_fits(&self, f: Fraction) -> bool {
        !f.is_begin() && !f.is_end() && f.p < self.dmax && f.q < self.dmax
    }

    pub fn create_weight(&mut self, left: &Weight, right: &Weight, site: SiteId) -> Weight {
        debug_assert!(left < right, "CREATE_WEIGHT requires w1 prec w2");
        let fm = left.f.mediant(right.f);

        if self.mediant_fits(fm) {
            return Weight::new(fm, 0, vec![0], site);
        }

        // Lines 13-16: fallback fraction. Sentinel 0/1 -> use the right.
        let fb = if left.f.is_begin() { right.f } else { left.f };
        let (sn_l, sn_r) = self.tracker.pair(fb);

        // Right allocation (lines 19-23)
        if fb < right.f || (fb == right.f && sn_r < right.sn) {
            let sn = sn_r + 1;
            self.tracker.set(fb, sn_l, sn);
            return Weight::new(fb, sn, left.sc.clone(), site);
        }

        // Left allocation (lines 24-28)
        if left.f < right.f && (sn_l - 1) < right.sn {
            let sn = sn_l - 1;
            self.tracker.set(fb, sn, sn_r);
            return Weight::new(fb, sn, left.sc.clone(), site);
        }

        // Sequence path (lines 29-32)
        let sc = newseq(&left.sc, &right.sc, self.base, self.depth, site);
        Weight::new(fb, left.sn, sc, site)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fraction::Fraction;

    #[test]
    fn situation1_rejects_3_over_7() {
        let mut a = Allocator::new(5, 10, 3);
        let w1 = Weight::new(Fraction::new(1, 4), 0, vec![0], 1);
        let w2 = Weight::new(Fraction::new(2, 3), 0, vec![0], 1);
        let w = a.create_weight(&w1, &w2, 1);
        assert_eq!(w.f, Fraction::new(1, 4));
        assert_eq!(w.sn, 1);
    }

    #[test]
    fn situation2_sn_ladder() {
        let mut a = Allocator::new(5, 10, 3);
        let w1 = Weight::new(Fraction::new(1, 4), 0, vec![0], 1);
        let w2 = Weight::new(Fraction::new(2, 3), 0, vec![0], 1);
        let r1 = a.create_weight(&w1, &w2, 1);
        let r2 = a.create_weight(&r1, &w2, 1);
        let l1 = a.create_weight(&Weight::begin(), &w1, 1);
        let l2 = a.create_weight(&Weight::begin(), &l1, 1);
        assert_eq!((r1.sn, r2.sn, l1.sn, l2.sn), (1, 2, -1, -2));
        assert!(l2 < l1 && l1 < w1 && w1 < r1 && r1 < r2 && r2 < w2);
    }

    #[test]
    fn situation3_path() {
        let mut a = Allocator::new(5, 10, 3);
        let w0 = Weight::new(Fraction::new(1, 4), 0, vec![0], 1);
        let w1 = Weight::new(Fraction::new(1, 4), 1, vec![0], 1);
        let mid = a.create_weight(&w0, &w1, 2);
        assert!(w0 < mid && mid < w1);
        assert_eq!(mid.sn, 0);
    }

    #[test]
    fn fraction_layer_example() {
        let mut a = Allocator::new(10, 10, 3);
        let w1 = Weight::new(Fraction::new(1, 3), 0, vec![0], 1);
        let w2 = Weight::new(Fraction::new(1, 2), 0, vec![0], 1);
        let w = a.create_weight(&w1, &w2, 1);
        assert_eq!(w, Weight::new(Fraction::new(2, 5), 0, vec![0], 1));
    }
}
