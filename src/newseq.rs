//! Algorithm 1 — NEWSEQ(left, right, base, DEPTH).

use crate::weight::SiteId;

pub fn newseq(left: &[u32], right: &[u32], base: u32, max_depth: u32, site_id: SiteId) -> Vec<u32> {
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
            let mut new_val = lv.saturating_add(interval / 2).saturating_add(1);
            if new_val >= rv {
                new_val = rv.saturating_sub(1);
            }
            sc.push(new_val);
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
pub fn newseq_unbounded(left: &[u32], right: &[u32], base: u32) -> Vec<u32> {
    let mut sc = Vec::new();
    let base = base.max(2);
    let limit = left.len().max(right.len()).saturating_add(1);

    for depth in 0..=limit {
        let lv = left.get(depth).copied().unwrap_or(0);
        let rv = right.get(depth).copied().unwrap_or(base);
        let interval = rv.saturating_sub(lv).saturating_sub(1);

        if interval > 0 {
            let mut new_val = lv.saturating_add(interval / 2).saturating_add(1);
            if new_val >= rv {
                new_val = rv.saturating_sub(1);
            }
            sc.push(new_val);
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
        assert_eq!(newseq(&[3], &[7], 10, 3, 2), vec![5]);
        assert_eq!(newseq(&[3], &[4], 10, 3, 2), vec![3, 5]);
    }

    #[test]
    fn unbounded_walk_moves_past_a_recycled_depth_tie() {
        let left = [0, 6, 2, 2];
        let right = [0, 6, 3];
        let candidate = newseq_unbounded(&left, &right, 10);

        assert!(left.as_slice() < candidate.as_slice());
        assert!(candidate.as_slice() < right.as_slice());
    }
}
