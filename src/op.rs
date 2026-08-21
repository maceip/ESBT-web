//! INS(w, e, c) and DEL(w, c). Paper §4.2 / §5.2.

use crate::fraction::Fraction;
use crate::weight::{SiteId, Weight};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpKind {
    Ins { ch: char },
    Del,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Op {
    pub kind: OpKind,
    pub weight: Weight,
    pub counter: u64,
    pub origin: SiteId,
    pub seq: u64,
}

impl Op {
    pub fn ins(weight: Weight, ch: char, counter: u64, origin: SiteId, seq: u64) -> Self {
        Op {
            kind: OpKind::Ins { ch },
            weight,
            counter,
            origin,
            seq,
        }
    }

    pub fn del(weight: Weight, counter: u64, origin: SiteId, seq: u64) -> Self {
        Op {
            kind: OpKind::Del,
            weight,
            counter,
            origin,
            seq,
        }
    }

    pub fn is_ins(&self) -> bool {
        matches!(self.kind, OpKind::Ins { .. })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.push(match self.kind {
            OpKind::Ins { .. } => 1,
            OpKind::Del => 2,
        });
        b.extend_from_slice(&self.origin.to_le_bytes());
        b.extend_from_slice(&self.seq.to_le_bytes());
        b.extend_from_slice(&self.counter.to_le_bytes());
        b.extend_from_slice(&self.weight.f.p.to_le_bytes());
        b.extend_from_slice(&self.weight.f.q.to_le_bytes());
        b.extend_from_slice(&self.weight.sn.to_le_bytes());
        b.extend_from_slice(&self.weight.site.to_le_bytes());
        let sl = self.weight.sc.len() as u16;
        b.extend_from_slice(&sl.to_le_bytes());
        for d in &self.weight.sc {
            b.extend_from_slice(&d.to_le_bytes());
        }
        if let OpKind::Ins { ch } = self.kind {
            b.extend_from_slice(&(ch as u32).to_le_bytes());
        }
        b
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 1 + 4 + 8 + 8 + 8 + 8 + 8 + 4 + 2 {
            return None;
        }
        let tag = buf[0];
        let mut i = 1;
        let take = |i: &mut usize, n: usize| -> Option<&[u8]> {
            if *i + n > buf.len() {
                return None;
            }
            let s = &buf[*i..*i + n];
            *i += n;
            Some(s)
        };
        let origin = u32::from_le_bytes(take(&mut i, 4)?.try_into().ok()?);
        let seq = u64::from_le_bytes(take(&mut i, 8)?.try_into().ok()?);
        let counter = u64::from_le_bytes(take(&mut i, 8)?.try_into().ok()?);
        let p = i64::from_le_bytes(take(&mut i, 8)?.try_into().ok()?);
        let q = i64::from_le_bytes(take(&mut i, 8)?.try_into().ok()?);
        let sn = i64::from_le_bytes(take(&mut i, 8)?.try_into().ok()?);
        let site = u32::from_le_bytes(take(&mut i, 4)?.try_into().ok()?);
        let sl = u16::from_le_bytes(take(&mut i, 2)?.try_into().ok()?) as usize;
        let mut sc = Vec::with_capacity(sl);
        for _ in 0..sl {
            sc.push(u32::from_le_bytes(take(&mut i, 4)?.try_into().ok()?));
        }
        let weight = Weight::new(Fraction { p, q }, sn, sc, site);
        match tag {
            1 => {
                let cp = u32::from_le_bytes(take(&mut i, 4)?.try_into().ok()?);
                Some(Op::ins(weight, char::from_u32(cp)?, counter, origin, seq))
            }
            2 => Some(Op::del(weight, counter, origin, seq)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let w = Weight::new(Fraction::new(1, 4), 2, vec![0, 5], 3);
        let op = Op::ins(w, 'λ', 9, 3, 4);
        assert_eq!(Op::decode(&op.encode()).unwrap(), op);
    }
}
