//! Algorithm 2 — CREATE_WEIGHT, plus the site Tracker (Definition 4) and the
//! Extension 1 adaptive `Dmax` controller.

use crate::codec::encoded_weight_len;
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

/// Hard ceiling for any `Dmax`. It preserves the `i128` cross-multiplication
/// headroom in `Fraction::cmp_rat` and matches the paper's evaluated 32-bit
/// fraction space (§8.3.1).
pub const DMAX_HARD_CEILING: i64 = 1 << 31;

/// Extension 1 (paper §10): tune `Dmax` from observed editing dynamics.
///
/// `Dmax` is a purely local allocation policy — Definition 2's total order
/// and the convergence theorems never consult it — so each replica may adapt
/// it independently and over time without coordination. The controller below
/// is a magnitude-discriminating hill-climb with hysteresis: near-miss
/// rejections (linear boundary drift) justify raising the bound, overshoot
/// rejections (exponential middle-insertion pinches) never do, and every
/// raise is a probe that is reverted if the observed identifier byte cost
/// regresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdaptiveDmaxConfig {
    /// `Dmax` never adapts below this bound.
    pub floor: i64,
    /// `Dmax` never adapts above this bound (clamped to the hard ceiling).
    pub ceiling: i64,
    /// Fraction-layer decisions per adjustment window.
    pub window: u32,
    /// Windows to hold still after a reverted probe (hysteresis).
    pub holdoff_windows: u32,
}

impl Default for AdaptiveDmaxConfig {
    fn default() -> Self {
        AdaptiveDmaxConfig {
            floor: 16,
            ceiling: DMAX_HARD_CEILING,
            window: 256,
            holdoff_windows: 4,
        }
    }
}

/// A raise under evaluation: where to fall back to and what identifier cost
/// looked like before the raise.
#[derive(Clone, Copy, Debug)]
struct DmaxProbe {
    previous_dmax: i64,
    baseline_cost: u64,
}

#[derive(Clone, Debug)]
struct AdaptiveDmax {
    config: AdaptiveDmaxConfig,
    /// Fraction-layer decisions observed in the current window.
    decisions: u32,
    near_misses: u32,
    overshoots: u32,
    /// EWMA of encoded identifier bytes, fixed-point with 8 fractional bits
    /// and a 1/8 smoothing step. Integer arithmetic keeps the controller
    /// bit-identical across native and Wasm builds.
    cost_ewma: u64,
    probe: Option<DmaxProbe>,
    holdoff: u32,
}

/// Rejected mediants within this factor of `Dmax` are near misses: the kind
/// of linear fraction growth (boundary editing) that a larger bound would
/// have absorbed. Anything larger is an exponential pinch that belongs to
/// the `sn`/`sc` layers by design.
const NEAR_MISS_FACTOR: i64 = 8;
const COST_FRACTION_BITS: u32 = 8;
const COST_SMOOTHING_SHIFT: u32 = 3;

impl AdaptiveDmax {
    fn new(mut config: AdaptiveDmaxConfig) -> Self {
        config.floor = config.floor.clamp(2, DMAX_HARD_CEILING);
        config.ceiling = config.ceiling.clamp(config.floor, DMAX_HARD_CEILING);
        config.window = config.window.max(1);
        AdaptiveDmax {
            config,
            decisions: 0,
            near_misses: 0,
            overshoots: 0,
            cost_ewma: 0,
            probe: None,
            holdoff: 0,
        }
    }

    fn observe_cost(&mut self, encoded_bytes: usize) {
        let sample = (encoded_bytes as u64) << COST_FRACTION_BITS;
        if self.cost_ewma == 0 {
            self.cost_ewma = sample;
        } else {
            self.cost_ewma = self.cost_ewma - (self.cost_ewma >> COST_SMOOTHING_SHIFT)
                + (sample >> COST_SMOOTHING_SHIFT);
        }
    }

    /// One fraction-layer decision: the mediant strictly separated the
    /// neighbor fractions, so the fraction layer had jurisdiction.
    fn observe_decision(&mut self, mediant: Fraction, dmax: i64, accepted: bool) {
        self.decisions = self.decisions.saturating_add(1);
        if !accepted {
            let magnitude = mediant.p.max(mediant.q);
            if magnitude < dmax.saturating_mul(NEAR_MISS_FACTOR) {
                self.near_misses = self.near_misses.saturating_add(1);
            } else {
                self.overshoots = self.overshoots.saturating_add(1);
            }
        }
    }

    /// Window-boundary step. Returns the `Dmax` the allocator should use.
    fn adapt(&mut self, current_dmax: i64) -> i64 {
        if self.decisions < self.config.window {
            return current_dmax;
        }
        let rejections = self.near_misses + self.overshoots;
        let near_dominated = self.near_misses * 2 > rejections;
        let pressured = u64::from(rejections) * 4 >= u64::from(self.decisions);
        self.decisions = 0;
        self.near_misses = 0;
        self.overshoots = 0;

        let mut dmax = current_dmax;
        if let Some(probe) = self.probe.take() {
            // Revert a raise that made identifiers more expensive, and hold
            // still afterwards so the controller cannot oscillate.
            let regression_bound = probe.baseline_cost + probe.baseline_cost / 8;
            if probe.baseline_cost != 0 && self.cost_ewma > regression_bound {
                self.holdoff = self.config.holdoff_windows;
                return probe.previous_dmax.max(self.config.floor);
            }
        }
        if self.holdoff > 0 {
            self.holdoff -= 1;
            return dmax;
        }
        if pressured && near_dominated && dmax < self.config.ceiling {
            self.probe = Some(DmaxProbe {
                previous_dmax: dmax,
                baseline_cost: self.cost_ewma,
            });
            dmax = dmax.saturating_mul(2).min(self.config.ceiling);
        }
        dmax
    }
}

#[derive(Clone, Debug)]
pub struct Allocator {
    pub dmax: i64,
    pub base: u32,
    pub depth: u32,
    pub tracker: Tracker,
    adaptive: Option<AdaptiveDmax>,
}

impl Allocator {
    pub fn new(dmax: i64, base: u32, depth: u32) -> Self {
        Allocator {
            dmax: dmax.max(2),
            base: base.max(2),
            depth: depth.max(1),
            tracker: Tracker::default(),
            adaptive: None,
        }
    }

    /// Enable Extension 1 adaptation. The current `Dmax` is clamped into the
    /// configured band and then evolves with the observed workload. The
    /// controller state is deliberately outside the tracker's transaction
    /// journal: it is a local heuristic, so counting an allocation that a
    /// rolled-back transaction discards is harmless, while replaying it
    /// would couple ordering-invariant state to product-level undo.
    pub fn enable_adaptive_dmax(&mut self, config: AdaptiveDmaxConfig) {
        let controller = AdaptiveDmax::new(config);
        self.dmax = self
            .dmax
            .clamp(controller.config.floor, controller.config.ceiling);
        self.adaptive = Some(controller);
    }

    /// The `Dmax` currently in force (adaptive or static).
    pub fn current_dmax(&self) -> i64 {
        self.dmax
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
        if self.adaptive.is_some() {
            let mediant = left.f.mediant(right.f);
            let separates = !mediant.is_begin()
                && !mediant.is_end()
                && left.f < mediant
                && mediant < right.f;
            if separates {
                let accepted = self.mediant_fits(mediant);
                let dmax = self.dmax;
                if let Some(controller) = self.adaptive.as_mut() {
                    controller.observe_decision(mediant, dmax, accepted);
                }
            }
        }
        let allocated = self.create_weight_inner(left, right, site);
        if let (Some(weight), Some(controller)) = (&allocated, self.adaptive.as_mut()) {
            controller.observe_cost(encoded_weight_len(weight, site));
            self.dmax = controller.adapt(self.dmax);
        }
        allocated
    }

    fn create_weight_inner(&mut self, left: &Weight, right: &Weight, site: SiteId) -> Option<Weight> {
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

    fn adaptive(window: u32, holdoff: u32) -> AdaptiveDmaxConfig {
        AdaptiveDmaxConfig {
            floor: 8,
            ceiling: DMAX_HARD_CEILING,
            window,
            holdoff_windows: holdoff,
        }
    }

    #[test]
    fn boundary_pressure_raises_dmax_but_a_static_allocator_stays_put() {
        let mut adaptive_alloc = Allocator::new(8, 10, 3);
        adaptive_alloc.enable_adaptive_dmax(adaptive(16, 4));
        let mut static_alloc = Allocator::new(8, 10, 3);

        for allocator in [&mut adaptive_alloc, &mut static_alloc] {
            // Prepend workload: every insertion lands between BEGIN and the
            // current first weight, so fraction magnitudes grow linearly and
            // rejections are near misses.
            let mut right = Weight::end();
            for _ in 0..200 {
                let weight = allocator
                    .create_weight(&Weight::begin(), &right, 1)
                    .expect("prepend allocation");
                assert!(Weight::begin() < weight && weight < right);
                right = weight;
            }
        }

        assert!(
            adaptive_alloc.current_dmax() >= 64,
            "adaptive dmax stuck at {}",
            adaptive_alloc.current_dmax()
        );
        assert_eq!(static_alloc.current_dmax(), 8);
    }

    #[test]
    fn overshoot_rejections_never_raise_dmax() {
        let mut allocator = Allocator::new(8, 10, 3);
        allocator.enable_adaptive_dmax(adaptive(8, 4));

        // A neighbor whose fraction already dwarfs the bound: the mediant
        // magnitude is far past NEAR_MISS_FACTOR × Dmax every time, which is
        // the exponential-pinch signature raising Dmax cannot fix.
        let mut left = Weight::new(Fraction::new(100, 1), 0, vec![0], 1);
        for _ in 0..32 {
            let weight = allocator
                .create_weight(&left, &Weight::end(), 1)
                .expect("sn-layer allocation");
            assert!(left < weight && weight < Weight::end());
            left = weight;
        }
        assert_eq!(allocator.current_dmax(), 8);
    }

    #[test]
    fn regressive_probe_reverts_and_holds_off() {
        let mut allocator = Allocator::new(8, 10, 3);
        allocator.enable_adaptive_dmax(adaptive(4, 1));

        let cheap_left = Weight::new(Fraction::new(30, 1), 0, vec![0], 1);
        let run_cheap_window = |allocator: &mut Allocator| {
            for _ in 0..4 {
                allocator
                    .create_weight(&cheap_left, &Weight::end(), 1)
                    .expect("cheap near-miss allocation");
            }
        };

        // Window 1: near-miss pressure with cheap identifiers → probe to 16.
        run_cheap_window(&mut allocator);
        assert_eq!(allocator.current_dmax(), 16);

        // Window 2: still near-miss pressured, but the gap's fallback copies
        // an expensive deep path into every identifier. Cost regresses, so
        // the probe must revert.
        let expensive_left = Weight::new(
            Fraction::new(30, 1),
            5,
            (0..64).map(|digit| (1 << 28) + digit).collect(),
            1,
        );
        let expensive_right = Weight::new(Fraction::new(31, 1), 0, vec![0], 1);
        for _ in 0..4 {
            let weight = allocator
                .create_weight(&expensive_left, &expensive_right, 1)
                .expect("expensive fallback allocation");
            assert!(expensive_left < weight && weight < expensive_right);
        }
        assert_eq!(allocator.current_dmax(), 8, "regressive probe not reverted");

        // Window 3: pressure continues but hysteresis holds the bound still.
        run_cheap_window(&mut allocator);
        assert_eq!(allocator.current_dmax(), 8, "holdoff ignored");

        // Window 4: holdoff expired; the controller may probe again.
        run_cheap_window(&mut allocator);
        assert_eq!(allocator.current_dmax(), 16);
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
