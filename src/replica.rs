//! Algorithm 3 — per-replica control, plus join snapshot and op log.

use crate::allocator::Allocator;
use crate::clock::Version;
use crate::op::{Op, OpKind};
use crate::rbtree::DocTree;
use crate::snapshot::{Atom, Snapshot};
use crate::weight::{SiteId, Weight};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Debug)]
pub struct ReplicaConfig {
    pub dmax: i64,
    pub base: u32,
    pub depth: u32,
}

impl Default for ReplicaConfig {
    fn default() -> Self {
        ReplicaConfig {
            dmax: 1 << 16,
            base: (1u32 << 31) - 1,
            depth: 256,
        }
    }
}

pub type WeightKey = Weight;

#[derive(Clone)]
pub struct Replica {
    pub site: SiteId,
    pub alloc: Allocator,
    pub doc: DocTree,
    pub pending: VecDeque<Op>,
    pub delete_log: HashSet<(WeightKey, u64)>,
    pub counter_map: HashMap<WeightKey, u64>,
    pub counter: u64,
    pub version: Version,
    /// Reliable log: (origin, seq) → op. Needed so a peer can retransmit.
    pub log: HashMap<(SiteId, u64), Op>,
    pub local_seq: u64,
}

impl Replica {
    pub fn new(site: SiteId, cfg: ReplicaConfig) -> Self {
        assert!(site != 0, "site 0 is reserved for sentinels");
        Replica {
            site,
            alloc: Allocator::new(cfg.dmax, cfg.base, cfg.depth),
            doc: DocTree::default(),
            pending: VecDeque::new(),
            delete_log: HashSet::new(),
            counter_map: HashMap::new(),
            counter: 0,
            version: Version::default(),
            log: HashMap::new(),
            local_seq: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.doc.len()
    }

    pub fn text(&self) -> String {
        self.doc.text()
    }

    fn neighbors(&self, index: usize) -> (Weight, Weight) {
        let left = if index == 0 {
            Weight::begin()
        } else {
            self.doc
                .get_at(index - 1)
                .map(|(w, _, _)| w.clone())
                .unwrap_or_else(Weight::begin)
        };
        let right = if index >= self.doc.len() {
            Weight::end()
        } else {
            self.doc
                .get_at(index)
                .map(|(w, _, _)| w.clone())
                .unwrap_or_else(Weight::end)
        };
        (left, right)
    }

    fn stamp(&mut self) -> u64 {
        self.local_seq += 1;
        self.version.note(self.site, self.local_seq);
        self.local_seq
    }

    pub fn local_insert(&mut self, index: usize, ch: char) -> Op {
        let index = index.min(self.doc.len());
        let (left, right) = self.neighbors(index);
        let w = self.alloc.create_weight(&left, &right, self.site);
        self.counter += 1;
        let c = self.counter;
        self.counter_map.insert(w.clone(), c);
        let seq = self.stamp();
        let op = Op::ins(w, ch, c, self.site, seq);
        self.log.insert((self.site, seq), op.clone());
        self.apply_ready(&op);
        op
    }

    pub fn local_delete(&mut self, index: usize) -> Option<Op> {
        if index >= self.doc.len() {
            return None;
        }
        let (w, _, _) = self.doc.get_at(index)?;
        let w = w.clone();
        let c = *self.counter_map.get(&w)?;
        let seq = self.stamp();
        let op = Op::del(w.clone(), c, self.site, seq);
        self.delete_log.insert((w, c));
        self.log.insert((self.site, seq), op.clone());
        self.apply_ready(&op);
        Some(op)
    }

    pub fn local_insert_str(&mut self, index: usize, s: &str) -> Vec<Op> {
        let mut i = index.min(self.doc.len());
        let mut out = Vec::new();
        for ch in s.chars() {
            out.push(self.local_insert(i, ch));
            i += 1;
        }
        out
    }

    pub fn local_delete_range(&mut self, start: usize, n: usize) -> Vec<Op> {
        let mut out = Vec::new();
        for _ in 0..n {
            if start >= self.doc.len() {
                break;
            }
            if let Some(op) = self.local_delete(start) {
                out.push(op);
            }
        }
        out
    }

    pub fn receive(&mut self, op: Op) {
        if op.origin == self.site {
            return;
        }
        if self.log.contains_key(&(op.origin, op.seq)) {
            return;
        }
        self.log.insert((op.origin, op.seq), op.clone());
        self.version.note(op.origin, op.seq);
        self.pending.push_back(op);
        self.drain();
    }

    pub fn drain(&mut self) {
        let mut progressed = true;
        while progressed {
            progressed = false;
            let mut i = 0;
            while i < self.pending.len() {
                if self.is_causally_ready(&self.pending[i]) {
                    let op = self.pending.remove(i).unwrap();
                    if matches!(op.kind, OpKind::Del) {
                        self.delete_log
                            .insert((op.weight.clone(), op.counter));
                    }
                    self.apply_ready(&op);
                    progressed = true;
                } else if self.should_ignore(&self.pending[i]) {
                    let op = self.pending.remove(i).unwrap();
                    if matches!(op.kind, OpKind::Del) {
                        self.delete_log.insert((op.weight.clone(), op.counter));
                    }
                    progressed = true;
                } else {
                    i += 1;
                }
            }
        }
    }

    /// Algorithm 3, refined by Scenario 3: a deletion targets (ω, c),
    /// not ω alone. After reuse, S may contain ω with a newer counter.
    pub fn is_causally_ready(&self, op: &Op) -> bool {
        match op.kind {
            OpKind::Ins { .. } => true,
            OpKind::Del => match self.doc.find(&op.weight) {
                Some((_, c)) if c == op.counter => true,
                _ => false,
            },
        }
    }

    fn should_ignore(&self, op: &Op) -> bool {
        if !matches!(op.kind, OpKind::Del) {
            return false;
        }
        if self.delete_log.contains(&(op.weight.clone(), op.counter)) {
            return true;
        }
        match self.doc.find(&op.weight) {
            Some((_, c)) if c != op.counter => true,
            None => false,
            Some(_) => false,
        }
    }

    fn apply_ready(&mut self, op: &Op) {
        match op.kind {
            OpKind::Ins { ch } => {
                if self.delete_log.contains(&(op.weight.clone(), op.counter)) {
                    return;
                }
                if self.doc.contains(&op.weight) {
                    return;
                }
                self.doc
                    .insert(op.weight.clone(), ch, op.counter);
                self.counter_map
                    .insert(op.weight.clone(), op.counter);
            }
            OpKind::Del => {
                self.doc.delete(&op.weight);
                self.counter_map.remove(&op.weight);
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            atoms: self
                .doc
                .atoms()
                .into_iter()
                .map(|(weight, ch, counter)| Atom {
                    weight,
                    ch,
                    counter,
                })
                .collect(),
            delete_log: self.delete_log.iter().cloned().collect(),
            version: self.version.clone(),
            site: self.site,
            counter: self.counter,
        }
    }

    pub fn install_snapshot(&mut self, snap: &Snapshot) {
        self.doc = DocTree::default();
        self.counter_map.clear();
        self.delete_log.clear();
        self.pending.clear();
        for a in &snap.atoms {
            self.doc
                .insert(a.weight.clone(), a.ch, a.counter);
            self.counter_map
                .insert(a.weight.clone(), a.counter);
        }
        for (w, c) in &snap.delete_log {
            self.delete_log.insert((w.clone(), *c));
        }
        for (&s, &n) in &snap.version.next {
            self.version.note(s, n);
        }
    }

    pub fn ops_in_range(&self, site: SiteId, from: u64, to: u64) -> Vec<Op> {
        let mut out = Vec::new();
        for seq in from..=to {
            if let Some(op) = self.log.get(&(site, seq)) {
                out.push(op.clone());
            }
        }
        out
    }

    pub fn hash_state(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        let mix = |h: &mut u64, b: u8| {
            *h ^= b as u64;
            *h = h.wrapping_mul(0x100000001b3);
        };
        for (w, ch, c) in self.doc.atoms() {
            for x in [w.f.p as u64, w.f.q as u64, w.sn as u64, w.site as u64, c] {
                for b in x.to_le_bytes() {
                    mix(&mut h, b);
                }
            }
            for d in &w.sc {
                for b in d.to_le_bytes() {
                    mix(&mut h, b);
                }
            }
            for b in (ch as u32).to_le_bytes() {
                mix(&mut h, b);
            }
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ReplicaConfig {
        ReplicaConfig {
            dmax: 5,
            base: 10,
            depth: 3,
        }
    }

    #[test]
    fn causal_and_concurrent() {
        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());
        let ins = a.local_insert(0, 'A');
        let del = a.local_delete(0).unwrap();
        b.receive(del);
        assert_eq!(b.pending.len(), 1);
        b.receive(ins);
        assert_eq!(a.text(), b.text());
        assert_eq!(a.hash_state(), b.hash_state());
    }

    #[test]
    fn late_join_snapshot() {
        let mut a = Replica::new(1, cfg());
        a.local_insert_str(0, "Hello");
        a.local_delete_range(1, 2);
        let mut c = Replica::new(3, cfg());
        c.install_snapshot(&a.snapshot());
        assert_eq!(c.text(), a.text());
        let extra = a.local_insert(3, '!');
        c.receive(extra);
        assert_eq!(c.text(), a.text());
    }

    #[test]
    fn three_way_sec() {
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
        assert_eq!(a.hash_state(), b.hash_state());
        assert_eq!(b.hash_state(), c.hash_state());
    }

    #[test]
    fn reuse_after_delete() {
        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());
        let i1 = a.local_insert(0, 'A');
        let d1 = a.local_delete(0).unwrap();
        let i2 = a.local_insert(0, 'B');
        b.receive(i2.clone());
        b.receive(d1.clone());
        b.receive(i1.clone());
        assert_eq!(a.text(), "B");
        assert_eq!(b.text(), "B");
        assert_ne!(i1.counter, i2.counter);
    }
}
