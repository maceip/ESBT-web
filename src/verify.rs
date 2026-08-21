//! Paper Situations 1–3, NEWSEQ examples, Alg. 3, SEC, join.

use crate::allocator::Allocator;
use crate::fraction::Fraction;
use crate::newseq::newseq;
use crate::replica::{Replica, ReplicaConfig};
use crate::weight::Weight;

pub fn run_all() -> (u32, u32, String) {
    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut log = String::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        if ok {
            pass += 1;
            log.push_str(&format!("PASS  {name}\n"));
        } else {
            fail += 1;
            log.push_str(&format!("FAIL  {name} — {detail}\n"));
        }
    };

    {
        let mut a = Allocator::new(5, 10, 3);
        let w1 = Weight::new(Fraction::new(1, 4), 0, vec![0], 1);
        let w2 = Weight::new(Fraction::new(2, 3), 0, vec![0], 1);
        let r1 = a.create_weight(&w1, &w2, 1);
        check(
            "S1/S2 first right sn=1 at 1/4",
            r1.f == Fraction::new(1, 4) && r1.sn == 1,
            &r1.to_string(),
        );
        let r2 = a.create_weight(&r1, &w2, 1);
        let l1 = a.create_weight(&Weight::begin(), &w1, 1);
        let l2 = a.create_weight(&Weight::begin(), &l1, 1);
        check(
            "S2 ladder -2..2",
            l2.sn == -2 && l1.sn == -1 && r2.sn == 2 && l2 < l1 && l1 < w1 && w1 < r1 && r1 < r2,
            "",
        );
    }
    {
        let mut a = Allocator::new(5, 10, 3);
        let w0 = Weight::new(Fraction::new(1, 4), 0, vec![0], 1);
        let w1 = Weight::new(Fraction::new(1, 4), 1, vec![0], 1);
        let mid = a.create_weight(&w0, &w1, 2);
        check("S3 NEWSEQ between sn 0 and 1", w0 < mid && mid < w1, &mid.to_string());
    }
    check("NEWSEQ ex1", newseq(&[3], &[7], 10, 3, 2) == vec![5], "");
    check("NEWSEQ ex2", newseq(&[3], &[4], 10, 3, 2) == vec![3, 5], "");

    {
        let mut a = Allocator::new(10, 10, 3);
        let w1 = Weight::new(Fraction::new(1, 3), 0, vec![0], 1);
        let w2 = Weight::new(Fraction::new(1, 2), 0, vec![0], 1);
        let w = a.create_weight(&w1, &w2, 1);
        check("paper 2/5 mediant", w.f == Fraction::new(2, 5) && w.sn == 0, "");
    }

    let cfg = ReplicaConfig {
        dmax: 5,
        base: 10,
        depth: 3,
    };
    {
        let mut a = Replica::new(1, cfg.clone());
        let mut b = Replica::new(2, cfg.clone());
        let ins = a.local_insert(0, 'A');
        let del = a.local_delete(0).unwrap();
        b.receive(del);
        let waited = b.pending.len() == 1;
        b.receive(ins);
        check(
            "Alg3 DEL-before-INS",
            waited && a.hash_state() == b.hash_state() && a.text().is_empty(),
            "",
        );
    }
    {
        let mut a = Replica::new(1, cfg.clone());
        let mut b = Replica::new(2, cfg.clone());
        let i1 = a.local_insert(0, 'A');
        let d1 = a.local_delete(0).unwrap();
        let i2 = a.local_insert(0, 'B');
        b.receive(i2);
        b.receive(d1);
        b.receive(i1);
        check(
            "Scenario 3 weight reuse + counter",
            a.text() == "B" && b.text() == "B",
            &format!("a={} b={}", a.text(), b.text()),
        );
    }
    {
        let mut a = Replica::new(1, ReplicaConfig::default());
        let mut b = Replica::new(2, ReplicaConfig::default());
        let mut c = Replica::new(3, ReplicaConfig::default());
        let s = a.local_insert(0, '·');
        b.receive(s.clone());
        c.receive(s);
        let ia = a.local_insert(0, 'A');
        let ib = b.local_insert(1, 'B');
        let ic = c.local_insert(1, 'C');
        for r in [&mut a, &mut b, &mut c] {
            r.receive(ia.clone());
            r.receive(ib.clone());
            r.receive(ic.clone());
        }
        check(
            "3-replica SEC",
            a.hash_state() == b.hash_state() && b.hash_state() == c.hash_state(),
            "",
        );
    }
    {
        let mut a = Replica::new(1, cfg.clone());
        a.local_insert_str(0, "Hello");
        a.local_delete_range(1, 2);
        let mut j = Replica::new(9, cfg);
        j.install_snapshot(&a.snapshot());
        check("late join snapshot", j.text() == a.text() && j.text() == "Hlo", "");
    }

    (pass, fail, log)
}

#[cfg(test)]
mod tests {
    #[test]
    fn paper_suite() {
        let (p, f, log) = super::run_all();
        assert_eq!(f, 0, "{log}");
        assert!(p >= 8, "{log}");
    }
}
