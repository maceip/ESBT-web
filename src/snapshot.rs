//! Join/leave state transfer. Paper §4: sites may join at any time.
//! Epidemic of live ops is not enough for a late replica; it needs the
//! current document plus the delete log L (Scenario 2).

use crate::clock::Version;
use crate::fraction::Fraction;
use crate::op::Op;
use crate::weight::{SiteId, Weight};

#[derive(Clone, Debug)]
pub struct Atom {
    pub weight: Weight,
    pub ch: char,
    pub counter: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub atoms: Vec<Atom>,
    pub delete_log: Vec<(Weight, u64)>,
    pub version: Version,
    pub site: SiteId,
    pub counter: u64,
}

impl Snapshot {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&self.site.to_le_bytes());
        b.extend_from_slice(&self.counter.to_le_bytes());
        b.extend_from_slice(&(self.atoms.len() as u32).to_le_bytes());
        for a in &self.atoms {
            write_weight(&mut b, &a.weight);
            b.extend_from_slice(&(a.ch as u32).to_le_bytes());
            b.extend_from_slice(&a.counter.to_le_bytes());
        }
        b.extend_from_slice(&(self.delete_log.len() as u32).to_le_bytes());
        for (w, c) in &self.delete_log {
            write_weight(&mut b, w);
            b.extend_from_slice(&c.to_le_bytes());
        }
        let ve = self.version.encode();
        b.extend_from_slice(&(ve.len() as u32).to_le_bytes());
        b.extend_from_slice(&ve);
        b
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 16 {
            return None;
        }
        let mut i = 0;
        let site = u32::from_le_bytes(read_bytes(buf, &mut i, 4)?.try_into().ok()?);
        let counter = u64::from_le_bytes(read_bytes(buf, &mut i, 8)?.try_into().ok()?);
        let na = u32::from_le_bytes(read_bytes(buf, &mut i, 4)?.try_into().ok()?) as usize;
        let mut atoms = Vec::with_capacity(na);
        for _ in 0..na {
            let w = read_weight(buf, &mut i)?;
            let cp = u32::from_le_bytes(read_bytes(buf, &mut i, 4)?.try_into().ok()?);
            let c = u64::from_le_bytes(read_bytes(buf, &mut i, 8)?.try_into().ok()?);
            atoms.push(Atom {
                weight: w,
                ch: char::from_u32(cp)?,
                counter: c,
            });
        }
        let nd = u32::from_le_bytes(read_bytes(buf, &mut i, 4)?.try_into().ok()?) as usize;
        let mut delete_log = Vec::with_capacity(nd);
        for _ in 0..nd {
            let w = read_weight(buf, &mut i)?;
            let c = u64::from_le_bytes(read_bytes(buf, &mut i, 8)?.try_into().ok()?);
            delete_log.push((w, c));
        }
        let vl = u32::from_le_bytes(read_bytes(buf, &mut i, 4)?.try_into().ok()?) as usize;
        let vb = read_bytes(buf, &mut i, vl)?;
        let version = Version::decode(vb)?;
        Some(Snapshot {
            atoms,
            delete_log,
            version,
            site,
            counter,
        })
    }
}

fn read_bytes<'a>(buf: &'a [u8], i: &mut usize, n: usize) -> Option<&'a [u8]> {
    if *i + n > buf.len() {
        return None;
    }
    let s = &buf[*i..*i + n];
    *i += n;
    Some(s)
}

fn write_weight(b: &mut Vec<u8>, w: &Weight) {
    b.extend_from_slice(&w.f.p.to_le_bytes());
    b.extend_from_slice(&w.f.q.to_le_bytes());
    b.extend_from_slice(&w.sn.to_le_bytes());
    b.extend_from_slice(&w.site.to_le_bytes());
    b.extend_from_slice(&(w.sc.len() as u16).to_le_bytes());
    for d in &w.sc {
        b.extend_from_slice(&d.to_le_bytes());
    }
}

fn read_weight(buf: &[u8], i: &mut usize) -> Option<Weight> {
    let take = |i: &mut usize, n: usize| -> Option<&[u8]> {
        if *i + n > buf.len() {
            return None;
        }
        let s = &buf[*i..*i + n];
        *i += n;
        Some(s)
    };
    let p = i64::from_le_bytes(take(i, 8)?.try_into().ok()?);
    let q = i64::from_le_bytes(take(i, 8)?.try_into().ok()?);
    let sn = i64::from_le_bytes(take(i, 8)?.try_into().ok()?);
    let site = u32::from_le_bytes(take(i, 4)?.try_into().ok()?);
    let sl = u16::from_le_bytes(take(i, 2)?.try_into().ok()?) as usize;
    let mut sc = Vec::with_capacity(sl);
    for _ in 0..sl {
        sc.push(u32::from_le_bytes(take(i, 4)?.try_into().ok()?));
    }
    Some(Weight::new(Fraction { p, q }, sn, sc, site))
}

/// Envelope on the epidemic mesh.
#[derive(Clone, Debug)]
pub enum Message {
    Op(Op),
    Hello { site: SiteId, version: Version },
    Snapshot(Snapshot),
    Need { from: SiteId, version: Version },
}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Message::Op(op) => {
                let mut b = vec![1];
                let e = op.encode();
                b.extend_from_slice(&(e.len() as u32).to_le_bytes());
                b.extend_from_slice(&e);
                b
            }
            Message::Hello { site, version } => {
                let mut b = vec![2];
                b.extend_from_slice(&site.to_le_bytes());
                let v = version.encode();
                b.extend_from_slice(&(v.len() as u32).to_le_bytes());
                b.extend_from_slice(&v);
                b
            }
            Message::Snapshot(s) => {
                let mut b = vec![3];
                let e = s.encode();
                b.extend_from_slice(&(e.len() as u32).to_le_bytes());
                b.extend_from_slice(&e);
                b
            }
            Message::Need { from, version } => {
                let mut b = vec![4];
                b.extend_from_slice(&from.to_le_bytes());
                let v = version.encode();
                b.extend_from_slice(&(v.len() as u32).to_le_bytes());
                b.extend_from_slice(&v);
                b
            }
        }
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.is_empty() {
            return None;
        }
        match buf[0] {
            1 => {
                if buf.len() < 5 {
                    return None;
                }
                let n = u32::from_le_bytes(buf[1..5].try_into().ok()?) as usize;
                Some(Message::Op(Op::decode(&buf[5..5 + n])?))
            }
            2 => {
                if buf.len() < 9 {
                    return None;
                }
                let site = u32::from_le_bytes(buf[1..5].try_into().ok()?);
                let n = u32::from_le_bytes(buf[5..9].try_into().ok()?) as usize;
                let version = Version::decode(&buf[9..9 + n])?;
                Some(Message::Hello { site, version })
            }
            3 => {
                if buf.len() < 5 {
                    return None;
                }
                let n = u32::from_le_bytes(buf[1..5].try_into().ok()?) as usize;
                Some(Message::Snapshot(Snapshot::decode(&buf[5..5 + n])?))
            }
            4 => {
                if buf.len() < 9 {
                    return None;
                }
                let from = u32::from_le_bytes(buf[1..5].try_into().ok()?);
                let n = u32::from_le_bytes(buf[5..9].try_into().ok()?) as usize;
                let version = Version::decode(&buf[9..9 + n])?;
                Some(Message::Need { from, version })
            }
            _ => None,
        }
    }
}
