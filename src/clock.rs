//! Per-origin sequence numbers. Transport reliability only — not the
//! paper's causal metadata (that is the insertion counter c).

use crate::weight::SiteId;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Version {
    pub next: BTreeMap<SiteId, u64>,
}

impl Version {
    pub fn observed(&self, site: SiteId) -> u64 {
        self.next.get(&site).copied().unwrap_or(0)
    }

    pub fn note(&mut self, site: SiteId, seq: u64) {
        let e = self.next.entry(site).or_insert(0);
        if seq > *e {
            *e = seq;
        }
    }

    pub fn missing_after(&self, other: &Version) -> Vec<(SiteId, u64, u64)> {
        let mut out = Vec::new();
        for (&site, &theirs) in &other.next {
            let ours = self.observed(site);
            if theirs > ours {
                out.push((site, ours + 1, theirs));
            }
        }
        out
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&(self.next.len() as u32).to_le_bytes());
        for (&s, &n) in &self.next {
            b.extend_from_slice(&s.to_le_bytes());
            b.extend_from_slice(&n.to_le_bytes());
        }
        b
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 4 {
            return None;
        }
        let n = u32::from_le_bytes(buf[0..4].try_into().ok()?) as usize;
        let mut i = 4;
        let mut next = BTreeMap::new();
        for _ in 0..n {
            if i + 12 > buf.len() {
                return None;
            }
            let s = u32::from_le_bytes(buf[i..i + 4].try_into().ok()?);
            let v = u64::from_le_bytes(buf[i + 4..i + 12].try_into().ok()?);
            next.insert(s, v);
            i += 12;
        }
        Some(Version { next })
    }
}
