//! Algorithm 2 — CREATE_WEIGHT, plus the site Tracker (Definition 4).

use crate::fraction::Fraction;
use crate::newseq::{newseq, newseq_unbounded};
use crate::weight::{SiteId, Weight};
use std::collections::HashMap;

type FractionKey = (i64, i64);
type SequenceBounds = (i64, i64);
type TrackerChange = (FractionKey, Option<SequenceBounds>);

/// Tracker : f ↦ (snL, snR). Only fractions that have hit Dmax are tracked.
#[derive(Clone, Debug, Default)]
pub struct Tracker {
    map: HashMap<FractionKey, SequenceBounds>,
    undo: Option<Vec<TrackerChange>>,
}

impl Tracker {
    fn pair(&mut self, f: Fraction) -> (i64, i64) {
        let key = (f.p, f.q);
        if let Some(value) = self.map.get(&key) {
            return *value;
        }
        if let Some(undo) = self.undo.as_mut() {
            undo.push((key, None));
        }
        self.map.insert(key, (0, 0));
        (0, 0)
    }

    fn set(&mut self, f: Fraction, sn_l: i64, sn_r: i64) {
        let key = (f.p, f.q);
        if let Some(undo) = self.undo.as_mut() {
            undo.push((key, self.map.get(&key).copied()));
        }
        self.map.insert(key, (sn_l, sn_r));
    }

    fn begin(&mut self) -> bool {
        if self.undo.is_some() {
            return false;
        }
        self.undo = Some(Vec::new());
        true
    }

    fn commit(&mut self) {
        self.undo = None;
    }

    fn rollback(&mut self) {
        let Some(changes) = self.undo.take() else {
            return;
        };
        for (key, previous) in changes.into_iter().rev() {
            match previous {
                Some(value) => {
                    self.map.insert(key, value);
                }
                None => {
                    self.map.remove(&key);
                }
            }
        }
    }

    fn transaction_active(&self) -> bool {
        self.undo.is_some()
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

    fn site_digit(&self, site: SiteId) -> u32 {
        1 + (site % u128::from(self.base - 1)) as u32
    }

    /// Fixed-width, prefix-free base-`base` representation of the full site
    /// identity. Unlike the paper's one-digit depth tie, this remains distinct
    /// for sites that collide modulo `base - 1` and is therefore suitable for
    /// reserving a complete local typing run.
    pub fn site_discriminator(&self, site: SiteId) -> Vec<u32> {
        let radix = u128::from(self.base.max(2));
        let mut remaining_max = u128::MAX;
        let mut width = 0usize;
        while remaining_max != 0 {
            remaining_max /= radix;
            width += 1;
        }

        let mut value = site;
        let mut digits = vec![1u32; width];
        for digit in digits.iter_mut().rev() {
            *digit = (value % radix) as u32 + 1;
            value /= radix;
        }
        digits
    }

    pub(crate) fn begin_transaction(&mut self) -> bool {
        self.tracker.begin()
    }

    pub(crate) fn commit_transaction(&mut self) {
        self.tracker.commit();
    }

    pub(crate) fn rollback_transaction(&mut self) {
        self.tracker.rollback();
    }

    pub(crate) fn transaction_active(&self) -> bool {
        self.tracker.transaction_active()
    }

    /// Allocate a weight strictly between `left` and `right`.
    ///
    /// `None` means the tuple order has no admissible value in this exact gap
    /// (most notably neighbors differing only by their final site tie-break).
    /// Callers surface that condition as typed allocation exhaustion; they must
    /// never widen the requested gap or insert an out-of-range weight.
    pub fn create_weight(&mut self, left: &Weight, right: &Weight, site: SiteId) -> Option<Weight> {
        debug_assert!(left < right, "CREATE_WEIGHT requires w1 prec w2");
        let between = |weight: Weight| (left < &weight && &weight < right).then_some(weight);
        let fm = left.f.mediant(right.f);

        // The mediant is usable only if it strictly separates the fractions.
        if self.mediant_fits(fm) && left.f < fm && fm < right.f {
            if let Some(weight) = between(Weight::new(fm, 0, vec![self.site_digit(site)], site)) {
                return Some(weight);
            }
        }

        // Lines 13-16: fallback fraction. Sentinel 0/1 -> use the right.
        let fb = if left.f.is_begin() { right.f } else { left.f };
        let (sn_l, sn_r) = self.tracker.pair(fb);
        let left_at_fb = !left.f.is_begin() && left.f == fb;

        // Right allocation: one step above the tracker and actual neighbor.
        let right_base = if left_at_fb { sn_r.max(left.sn) } else { sn_r };
        let sn = right_base.saturating_add(1);
        if fb < right.f || (fb == right.f && sn < right.sn) {
            if let Some(weight) = between(Weight::new(fb, sn, left.sc.clone(), site)) {
                self.tracker.set(fb, sn_l, sn);
                return Some(weight);
            }
        }

        // Left allocation: one step below the tracker and actual neighbor.
        if fb == right.f {
            let sn = sn_l.min(right.sn).saturating_sub(1);
            if !left_at_fb || sn > left.sn {
                if let Some(weight) = between(Weight::new(fb, sn, left.sc.clone(), site)) {
                    self.tracker.set(fb, sn, sn_r);
                    return Some(weight);
                }
            }
        }

        // Interior of an sn ladder: choose an integer strictly between.
        if left_at_fb && fb == right.f {
            let gap = (right.sn as i128) - (left.sn as i128);
            if gap > 1 {
                let sn = ((left.sn as i128) + gap / 2) as i64;
                if let Some(weight) =
                    between(Weight::new(fb, sn, vec![self.site_digit(site)], site))
                {
                    return Some(weight);
                }
            }
        }

        // Sequence path (Situation 3): same fraction, no sn room.
        let sc = newseq(&left.sc, &right.sc, self.base, self.depth, site);
        if let Some(weight) = between(Weight::new(fb, left.sn, sc, site)) {
            return Some(weight);
        }

        // Past DEPTH the constant site tie may recycle a path. Retry without
        // the cap and validate the complete candidate.
        let sc = newseq_unbounded(&left.sc, &right.sc, self.base);
        if let Some(weight) = between(Weight::new(fb, left.sn, sc, site)) {
            return Some(weight);
        }

        // A site value may sort between otherwise identical weights. If not,
        // this is a true twin pinch and the exact gap is exhausted.
        between(Weight::new(fb, left.sn, left.sc.clone(), site))
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
        let w = a.create_weight(&w1, &w2, 1).expect("allocate");
        assert_eq!(w.f, Fraction::new(1, 4));
        assert_eq!(w.sn, 1);
    }

    #[test]
    fn situation2_sn_ladder() {
        let mut a = Allocator::new(5, 10, 3);
        let w1 = Weight::new(Fraction::new(1, 4), 0, vec![0], 1);
        let w2 = Weight::new(Fraction::new(2, 3), 0, vec![0], 1);
        let r1 = a.create_weight(&w1, &w2, 1).expect("right 1");
        let r2 = a.create_weight(&r1, &w2, 1).expect("right 2");
        let l1 = a.create_weight(&Weight::begin(), &w1, 1).expect("left 1");
        let l2 = a.create_weight(&Weight::begin(), &l1, 1).expect("left 2");
        assert_eq!((r1.sn, r2.sn, l1.sn, l2.sn), (1, 2, -1, -2));
        assert!(l2 < l1 && l1 < w1 && w1 < r1 && r1 < r2 && r2 < w2);
    }

    #[test]
    fn situation3_path() {
        let mut a = Allocator::new(5, 10, 3);
        let w0 = Weight::new(Fraction::new(1, 4), 0, vec![0], 1);
        let w1 = Weight::new(Fraction::new(1, 4), 1, vec![0], 1);
        let mid = a.create_weight(&w0, &w1, 2).expect("midpoint");
        assert!(w0 < mid && mid < w1);
        assert_eq!(mid.sn, 0);
    }

    #[test]
    fn fraction_layer_example() {
        let mut a = Allocator::new(10, 10, 3);
        let w1 = Weight::new(Fraction::new(1, 3), 0, vec![0], 1);
        let w2 = Weight::new(Fraction::new(1, 2), 0, vec![0], 1);
        let w = a.create_weight(&w1, &w2, 1).expect("mediant");
        assert_eq!(w.f, Fraction::new(2, 5));
        assert_eq!(w.sn, 0);
        assert_eq!(w.site, 1);
        assert_eq!(w.sc, vec![a.site_digit(1)]);
    }

    #[test]
    fn equal_fraction_neighbors_allocate_between_their_paths() {
        let mut allocator = Allocator::new(5, 10, 3);
        let left = Weight::new(Fraction::ONE, 0, vec![0, 9], 9);
        let right = Weight::new(Fraction::ONE, 0, vec![0, 9, 1], 9);

        let weight = allocator
            .create_weight(&left, &right, 1)
            .expect("path midpoint");

        assert!(left < weight && weight < right, "allocated {weight}");
        assert_eq!(weight.f, Fraction::ONE);
    }
}
