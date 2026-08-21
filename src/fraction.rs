//! Stern–Brocot rationals. Sentinels: W_BEGIN = 0/1, W_END = 1/0 (paper p.3).

use core::cmp::Ordering;
use core::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Fraction {
    pub p: i64,
    pub q: i64,
}

impl Fraction {
    pub const BEGIN: Fraction = Fraction { p: 0, q: 1 };
    pub const END: Fraction = Fraction { p: 1, q: 0 };
    pub const ONE: Fraction = Fraction { p: 1, q: 1 };

    pub fn new(mut p: i64, mut q: i64) -> Self {
        if q == 0 {
            return Fraction::END;
        }
        if p == 0 {
            return Fraction::BEGIN;
        }
        if q < 0 {
            p = -p;
            q = -q;
        }
        let g = gcd(p.abs(), q);
        Fraction { p: p / g, q: q / g }
    }

    pub fn is_begin(self) -> bool {
        self.p == 0 && self.q != 0
    }

    pub fn is_end(self) -> bool {
        self.q == 0
    }

    pub fn mediant(self, other: Self) -> Self {
        Fraction::new(
            self.p.saturating_add(other.p),
            self.q.saturating_add(other.q),
        )
    }

    /// Cross-multiply. 0/1 < finite < 1/0.
    pub fn cmp_rat(self, other: Self) -> Ordering {
        if self.p == other.p && self.q == other.q {
            return Ordering::Equal;
        }
        if self.is_begin() {
            return Ordering::Less;
        }
        if other.is_begin() {
            return Ordering::Greater;
        }
        if self.is_end() {
            return Ordering::Greater;
        }
        if other.is_end() {
            return Ordering::Less;
        }
        let left = (self.p as i128) * (other.q as i128);
        let right = (other.p as i128) * (self.q as i128);
        left.cmp(&right)
    }
}

impl PartialOrd for Fraction {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Fraction {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_rat(*other)
    }
}

impl fmt::Display for Fraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.p, self.q)
    }
}

pub fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    if a == 0 {
        1
    } else {
        a.abs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_and_mediant() {
        assert!(Fraction::BEGIN < Fraction::ONE);
        assert!(Fraction::ONE < Fraction::END);
        assert!(Fraction::new(1, 4) < Fraction::new(2, 3));
        let m = Fraction::new(1, 4).mediant(Fraction::new(2, 3));
        assert_eq!(m, Fraction::new(3, 7));
        assert!(Fraction::new(1, 4) < m && m < Fraction::new(2, 3));
    }
}
