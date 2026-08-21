//! Algorithm 1 — NEWSEQ(left, right, base, DEPTH).

pub fn newseq(left: &[u32], right: &[u32], base: u32, max_depth: u32, site_id: u32) -> Vec<u32> {
    let mut sc = Vec::new();
    let mut depth: usize = 0;
    let max_d = max_depth.max(1) as usize;
    let base = base.max(2);

    loop {
        let lv = if depth < left.len() { left[depth] } else { 0 };
        let rv = if depth < right.len() { right[depth] } else { base };
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
            let tie = 1 + (site_id % (base - 1));
            sc.push(tie);
            return sc;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_examples() {
        assert_eq!(newseq(&[3], &[7], 10, 3, 2), vec![5]);
        assert_eq!(newseq(&[3], &[4], 10, 3, 2), vec![3, 5]);
    }
}
