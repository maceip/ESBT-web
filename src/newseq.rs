//! Algorithm 1 — NEWSEQ(left, right, base, DEPTH), with a pluggable digit
//! allocation strategy (Extension 3).

use crate::weight::SiteId;

/// How a new sequence-path digit is chosen inside an available gap.
///
/// The strategy is a per-site local policy: whichever digit is chosen, the
/// resulting weight is validated strictly between its neighbors, so replicas
/// running different strategies still converge. `Midpoint` is the paper's
/// Algorithm 1. The boundary strategies translate LSEQ's boundary+ and
/// boundary− allocators into the ESBT sequence-path space, and
/// `AlternatingByDepth` is LSEQ's strategy alternation made deterministic by
/// depth parity instead of cached coin flips, preserving this engine's
/// reproducibility and rollback guarantees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AllocationStrategy {
    /// Bisect the gap (paper Algorithm 1 line 16).
    #[default]
    Midpoint,
    /// Allocate close above the left neighbor, at most `boundary` away —
    /// dense for append-leaning workloads.
    BoundaryLow(u32),
    /// Allocate close below the right neighbor, at most `boundary` away —
    /// dense for prepend-leaning workloads.
    BoundaryHigh(u32),
    /// Boundary-low at even depths, boundary-high at odd depths.
    AlternatingByDepth(u32),
}

impl AllocationStrategy {
    /// Choose a digit strictly inside `(lv, rv)`; `interval = rv − lv − 1`
    /// is positive when called.
    fn choose(self, lv: u32, rv: u32, interval: u32, depth: usize) -> u32 {
        let step = match self {
            AllocationStrategy::Midpoint => interval / 2 + 1,
            AllocationStrategy::BoundaryLow(boundary) => boundary.max(1).min(interval),
            AllocationStrategy::BoundaryHigh(boundary) => {
                interval - boundary.max(1).min(interval) + 1
            }
            AllocationStrategy::AlternatingByDepth(boundary) => {
                let bounded = boundary.max(1).min(interval);
                if depth.is_multiple_of(2) {
                    bounded
                } else {
                    interval - bounded + 1
                }
            }
        };
        let mut new_val = lv.saturating_add(step);
        if new_val >= rv {
            new_val = rv.saturating_sub(1);
        }
        new_val.max(lv.saturating_add(1))
    }
}

pub fn newseq(
    left: &[u32],
    right: &[u32],
    base: u32,
    max_depth: u32,
    site_id: SiteId,
    strategy: AllocationStrategy,
) -> Vec<u32> {
    let mut sc = Vec::new();
    let mut depth: usize = 0;
    let max_d = max_depth.max(1) as usize;
    let base = base.max(2);

    loop {
        let lv = if depth < left.len() { left[depth] } else { 0 };
        let rv = if depth < right.len() {
            right[depth]
        } else {
            base
        };
        let interval = rv.saturating_sub(lv).saturating_sub(1);

        if interval > 0 {
            sc.push(strategy.choose(lv, rv, interval, depth));
            return sc;
        }

        sc.push(lv);
        depth += 1;
        if depth >= max_d {
            // paper: tie = 1 + (siteId mod (base − 1))
            let tie = 1 + (site_id % u128::from(base - 1)) as u32;
            sc.push(tie);
            return sc;
        }
    }
}

/// NEWSEQ without the fixed-depth tie fallback.
///
/// The capped algorithm's tie digit is constant per site, so a saturated
/// prefix can reproduce a path that site already minted. Callers verify the
/// returned candidate against the complete neighboring weights; if no path
/// exists, allocation fails for that exact document gap.
pub fn newseq_unbounded(
    left: &[u32],
    right: &[u32],
    base: u32,
    strategy: AllocationStrategy,
) -> Vec<u32> {
    let mut sc = Vec::new();
    let base = base.max(2);
    let limit = left.len().max(right.len()).saturating_add(1);

    for depth in 0..=limit {
        let lv = left.get(depth).copied().unwrap_or(0);
        let rv = right.get(depth).copied().unwrap_or(base);
        let interval = rv.saturating_sub(lv).saturating_sub(1);

        if interval > 0 {
            sc.push(strategy.choose(lv, rv, interval, depth));
            return sc;
        }
        sc.push(lv);
    }

    sc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_examples() {
        let midpoint = AllocationStrategy::Midpoint;
        assert_eq!(newseq(&[3], &[7], 10, 3, 2, midpoint), vec![5]);
        assert_eq!(newseq(&[3], &[4], 10, 3, 2, midpoint), vec![3, 5]);
    }

    #[test]
    fn strategies_stay_strictly_inside_the_gap() {
        let strategies = [
            AllocationStrategy::Midpoint,
            AllocationStrategy::BoundaryLow(4),
            AllocationStrategy::BoundaryHigh(4),
            AllocationStrategy::AlternatingByDepth(4),
        ];
        for strategy in strategies {
            for (left, right) in [
                (vec![3u32], vec![7u32]),
                (vec![3], vec![4]),
                (vec![0], vec![1]),
                (vec![9], vec![9, 1]),
            ] {
                let path = newseq(&left, &right, 10, 3, 2, strategy);
                assert!(
                    left.as_slice() < path.as_slice(),
                    "{strategy:?}: {path:?} not above {left:?}"
                );
                assert!(
                    path.as_slice() < right.as_slice(),
                    "{strategy:?}: {path:?} not below {right:?}"
                );
            }
        }
    }

    #[test]
    fn boundary_strategies_hug_their_side() {
        assert_eq!(
            newseq(&[10], &[90], 100, 3, 2, AllocationStrategy::BoundaryLow(4)),
            vec![14]
        );
        assert_eq!(
            newseq(&[10], &[90], 100, 3, 2, AllocationStrategy::BoundaryHigh(4)),
            vec![86]
        );
        let alternating = AllocationStrategy::AlternatingByDepth(4);
        assert_eq!(newseq(&[10], &[90], 100, 3, 2, alternating), vec![14]);
        // Depth 1 gap: same digits at depth 0, so allocation descends.
        assert_eq!(
            newseq(&[10, 10], &[10, 90], 100, 3, 2, alternating),
            vec![10, 86]
        );
    }

    #[test]
    fn unbounded_walk_moves_past_a_recycled_depth_tie() {
        let left = [0, 6, 2, 2];
        let right = [0, 6, 3];
        let candidate = newseq_unbounded(&left, &right, 10, AllocationStrategy::Midpoint);

        assert!(left.as_slice() < candidate.as_slice());
        assert!(candidate.as_slice() < right.as_slice());
    }
}
