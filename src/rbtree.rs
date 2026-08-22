//! Order-statistic Red–Black tree keyed by Weight. Paper §4.1, O(log n).

use crate::weight::Weight;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    Red,
    Black,
}

#[derive(Clone)]
struct Node {
    weight: Weight,
    unit: u16,
    counter: u64,
    color: Color,
    left: Option<usize>,
    right: Option<usize>,
    parent: Option<usize>,
    size: usize,
}

#[derive(Clone, Default)]
pub struct DocTree {
    nodes: Vec<Node>,
    root: Option<usize>,
    free: Vec<usize>,
}

impl DocTree {
    pub fn len(&self) -> usize {
        self.root.map(|r| self.nodes[r].size).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn alloc(&mut self, n: Node) -> usize {
        if let Some(i) = self.free.pop() {
            self.nodes[i] = n;
            i
        } else {
            let i = self.nodes.len();
            self.nodes.push(n);
            i
        }
    }

    fn sz(&self, i: Option<usize>) -> usize {
        i.map(|x| self.nodes[x].size).unwrap_or(0)
    }

    fn pull(&mut self, i: usize) {
        let l = self.nodes[i].left;
        let r = self.nodes[i].right;
        self.nodes[i].size = 1 + self.sz(l) + self.sz(r);
    }

    fn color(&self, i: Option<usize>) -> Color {
        i.map(|x| self.nodes[x].color).unwrap_or(Color::Black)
    }

    fn rotate_left(&mut self, x: usize) {
        let y = self.nodes[x].right.expect("rl");
        self.nodes[x].right = self.nodes[y].left;
        if let Some(yl) = self.nodes[y].left {
            self.nodes[yl].parent = Some(x);
        }
        self.nodes[y].parent = self.nodes[x].parent;
        match self.nodes[x].parent {
            None => self.root = Some(y),
            Some(p) if self.nodes[p].left == Some(x) => self.nodes[p].left = Some(y),
            Some(p) => self.nodes[p].right = Some(y),
        }
        self.nodes[y].left = Some(x);
        self.nodes[x].parent = Some(y);
        self.pull(x);
        self.pull(y);
    }

    fn rotate_right(&mut self, x: usize) {
        let y = self.nodes[x].left.expect("rr");
        self.nodes[x].left = self.nodes[y].right;
        if let Some(yr) = self.nodes[y].right {
            self.nodes[yr].parent = Some(x);
        }
        self.nodes[y].parent = self.nodes[x].parent;
        match self.nodes[x].parent {
            None => self.root = Some(y),
            Some(p) if self.nodes[p].left == Some(x) => self.nodes[p].left = Some(y),
            Some(p) => self.nodes[p].right = Some(y),
        }
        self.nodes[y].right = Some(x);
        self.nodes[x].parent = Some(y);
        self.pull(x);
        self.pull(y);
    }

    pub fn insert(&mut self, weight: Weight, unit: u16, counter: u64) -> bool {
        if self.find(&weight).is_some() {
            return false;
        }
        let z = self.alloc(Node {
            weight,
            unit,
            counter,
            color: Color::Red,
            left: None,
            right: None,
            parent: None,
            size: 1,
        });
        let mut y = None;
        let mut x = self.root;
        while let Some(xi) = x {
            y = x;
            self.nodes[xi].size += 1;
            if self.nodes[z].weight < self.nodes[xi].weight {
                x = self.nodes[xi].left;
            } else {
                x = self.nodes[xi].right;
            }
        }
        self.nodes[z].parent = y;
        match y {
            None => self.root = Some(z),
            Some(yi) if self.nodes[z].weight < self.nodes[yi].weight => {
                self.nodes[yi].left = Some(z)
            }
            Some(yi) => self.nodes[yi].right = Some(z),
        }
        self.insert_fix(z);
        true
    }

    fn insert_fix(&mut self, mut z: usize) {
        while self.color(self.nodes[z].parent) == Color::Red {
            let p = match self.nodes[z].parent {
                Some(p) => p,
                None => break,
            };
            let g = match self.nodes[p].parent {
                Some(g) => g,
                None => break,
            };
            if Some(p) == self.nodes[g].left {
                let u = self.nodes[g].right;
                if self.color(u) == Color::Red {
                    self.nodes[p].color = Color::Black;
                    if let Some(ui) = u {
                        self.nodes[ui].color = Color::Black;
                    }
                    self.nodes[g].color = Color::Red;
                    z = g;
                } else {
                    if Some(z) == self.nodes[p].right {
                        z = p;
                        self.rotate_left(z);
                    }
                    let p = self.nodes[z].parent.unwrap();
                    let g = self.nodes[p].parent.unwrap();
                    self.nodes[p].color = Color::Black;
                    self.nodes[g].color = Color::Red;
                    self.rotate_right(g);
                }
            } else {
                let u = self.nodes[g].left;
                if self.color(u) == Color::Red {
                    self.nodes[p].color = Color::Black;
                    if let Some(ui) = u {
                        self.nodes[ui].color = Color::Black;
                    }
                    self.nodes[g].color = Color::Red;
                    z = g;
                } else {
                    if Some(z) == self.nodes[p].left {
                        z = p;
                        self.rotate_right(z);
                    }
                    let p = self.nodes[z].parent.unwrap();
                    let g = self.nodes[p].parent.unwrap();
                    self.nodes[p].color = Color::Black;
                    self.nodes[g].color = Color::Red;
                    self.rotate_left(g);
                }
            }
        }
        if let Some(r) = self.root {
            self.nodes[r].color = Color::Black;
        }
    }

    pub fn find(&self, w: &Weight) -> Option<(u16, u64)> {
        let mut x = self.root;
        while let Some(xi) = x {
            match w.cmp(&self.nodes[xi].weight) {
                core::cmp::Ordering::Less => x = self.nodes[xi].left,
                core::cmp::Ordering::Greater => x = self.nodes[xi].right,
                core::cmp::Ordering::Equal => {
                    return Some((self.nodes[xi].unit, self.nodes[xi].counter))
                }
            }
        }
        None
    }

    pub fn contains(&self, w: &Weight) -> bool {
        self.find(w).is_some()
    }

    pub fn get_at(&self, mut index: usize) -> Option<(&Weight, u16, u64)> {
        let mut x = self.root?;
        loop {
            let ls = self.sz(self.nodes[x].left);
            if index < ls {
                x = self.nodes[x].left?;
            } else if index == ls {
                let n = &self.nodes[x];
                return Some((&n.weight, n.unit, n.counter));
            } else {
                index -= ls + 1;
                x = self.nodes[x].right?;
            }
        }
    }

    /// Rank of an exact live weight.
    pub fn index_of(&self, weight: &Weight) -> Option<usize> {
        let mut node = self.root;
        let mut before = 0usize;
        while let Some(index) = node {
            let left_size = self.sz(self.nodes[index].left);
            match weight.cmp(&self.nodes[index].weight) {
                core::cmp::Ordering::Less => node = self.nodes[index].left,
                core::cmp::Ordering::Greater => {
                    before = before.saturating_add(left_size).saturating_add(1);
                    node = self.nodes[index].right;
                }
                core::cmp::Ordering::Equal => return Some(before.saturating_add(left_size)),
            }
        }
        None
    }

    /// Rank where `weight` is or would be inserted. This remains meaningful
    /// after that weight is deleted and is the basis of stable anchors.
    pub fn lower_bound(&self, weight: &Weight) -> usize {
        let mut node = self.root;
        let mut before = 0usize;
        let mut result = self.len();
        while let Some(index) = node {
            let left_size = self.sz(self.nodes[index].left);
            if self.nodes[index].weight < *weight {
                before = before.saturating_add(left_size).saturating_add(1);
                node = self.nodes[index].right;
            } else {
                result = before.saturating_add(left_size);
                node = self.nodes[index].left;
            }
        }
        result
    }

    pub fn delete(&mut self, w: &Weight) -> bool {
        let mut z = self.root;
        while let Some(zi) = z {
            match w.cmp(&self.nodes[zi].weight) {
                core::cmp::Ordering::Less => z = self.nodes[zi].left,
                core::cmp::Ordering::Greater => z = self.nodes[zi].right,
                core::cmp::Ordering::Equal => {
                    self.delete_idx(zi);
                    return true;
                }
            }
        }
        false
    }

    fn transplant(&mut self, u: usize, v: Option<usize>) {
        match self.nodes[u].parent {
            None => self.root = v,
            Some(p) if self.nodes[p].left == Some(u) => self.nodes[p].left = v,
            Some(p) => self.nodes[p].right = v,
        }
        if let Some(vi) = v {
            self.nodes[vi].parent = self.nodes[u].parent;
        }
    }

    fn minimum(&self, mut x: usize) -> usize {
        while let Some(l) = self.nodes[x].left {
            x = l;
        }
        x
    }

    fn pull_upward(&mut self, mut node: Option<usize>) {
        while let Some(index) = node {
            self.pull(index);
            node = self.nodes[index].parent;
        }
    }

    fn delete_idx(&mut self, z: usize) {
        let mut y = z;
        let mut removed_color = self.nodes[y].color;
        let x;
        let x_parent;
        let x_was_left;

        if self.nodes[z].left.is_none() {
            x = self.nodes[z].right;
            x_parent = self.nodes[z].parent;
            x_was_left = x_parent.is_some_and(|parent| self.nodes[parent].left == Some(z));
            self.transplant(z, x);
            self.pull_upward(x_parent);
        } else if self.nodes[z].right.is_none() {
            x = self.nodes[z].left;
            x_parent = self.nodes[z].parent;
            x_was_left = x_parent.is_some_and(|parent| self.nodes[parent].left == Some(z));
            self.transplant(z, x);
            self.pull_upward(x_parent);
        } else {
            y = self.minimum(self.nodes[z].right.unwrap());
            removed_color = self.nodes[y].color;
            x = self.nodes[y].right;
            if self.nodes[y].parent == Some(z) {
                x_parent = Some(y);
                x_was_left = false;
                if let Some(xi) = x {
                    self.nodes[xi].parent = Some(y);
                }
            } else {
                let old_parent = self.nodes[y].parent.expect("successor parent");
                x_parent = Some(old_parent);
                // A non-immediate in-order successor is the left child of its
                // old parent. Retain that side explicitly because a missing
                // child has no node from which delete fix-up can recover it.
                x_was_left = true;
                self.transplant(y, x);
                self.pull_upward(Some(old_parent));
                self.nodes[y].right = self.nodes[z].right;
                if let Some(r) = self.nodes[y].right {
                    self.nodes[r].parent = Some(y);
                }
            }
            let z_parent = self.nodes[z].parent;
            self.transplant(z, Some(y));
            self.nodes[y].left = self.nodes[z].left;
            if let Some(l) = self.nodes[y].left {
                self.nodes[l].parent = Some(y);
            }
            self.nodes[y].color = self.nodes[z].color;
            self.pull(y);
            self.pull_upward(z_parent);
        }

        if removed_color == Color::Black {
            self.delete_fix(x, x_parent, x_was_left);
        }
        self.free.push(z);
    }

    fn delete_fix(
        &mut self,
        mut x: Option<usize>,
        missing_parent: Option<usize>,
        missing_was_left: bool,
    ) {
        while x != self.root && self.color(x) == Color::Black {
            let (p, is_left) = match x {
                Some(index) => {
                    let Some(parent) = self.nodes[index].parent else {
                        break;
                    };
                    (parent, self.nodes[parent].left == Some(index))
                }
                None => {
                    let Some(parent) = missing_parent else {
                        break;
                    };
                    (parent, missing_was_left)
                }
            };
            if is_left {
                let mut w = self.nodes[p].right;
                if self.color(w) == Color::Red {
                    if let Some(wi) = w {
                        self.nodes[wi].color = Color::Black;
                    }
                    self.nodes[p].color = Color::Red;
                    self.rotate_left(p);
                    w = self.nodes[p].right;
                }
                let wl = w.and_then(|wi| self.nodes[wi].left);
                let wr = w.and_then(|wi| self.nodes[wi].right);
                if self.color(wl) == Color::Black && self.color(wr) == Color::Black {
                    if let Some(wi) = w {
                        self.nodes[wi].color = Color::Red;
                    }
                    x = Some(p);
                } else {
                    if self.color(wr) == Color::Black {
                        if let Some(wli) = wl {
                            self.nodes[wli].color = Color::Black;
                        }
                        if let Some(wi) = w {
                            self.nodes[wi].color = Color::Red;
                            self.rotate_right(wi);
                        }
                        w = self.nodes[p].right;
                    }
                    if let Some(wi) = w {
                        self.nodes[wi].color = self.nodes[p].color;
                        if let Some(r) = self.nodes[wi].right {
                            self.nodes[r].color = Color::Black;
                        }
                    }
                    self.nodes[p].color = Color::Black;
                    self.rotate_left(p);
                    x = self.root;
                }
            } else {
                let mut w = self.nodes[p].left;
                if self.color(w) == Color::Red {
                    if let Some(wi) = w {
                        self.nodes[wi].color = Color::Black;
                    }
                    self.nodes[p].color = Color::Red;
                    self.rotate_right(p);
                    w = self.nodes[p].left;
                }
                let wl = w.and_then(|wi| self.nodes[wi].left);
                let wr = w.and_then(|wi| self.nodes[wi].right);
                if self.color(wl) == Color::Black && self.color(wr) == Color::Black {
                    if let Some(wi) = w {
                        self.nodes[wi].color = Color::Red;
                    }
                    x = Some(p);
                } else {
                    if self.color(wl) == Color::Black {
                        if let Some(wri) = wr {
                            self.nodes[wri].color = Color::Black;
                        }
                        if let Some(wi) = w {
                            self.nodes[wi].color = Color::Red;
                            self.rotate_left(wi);
                        }
                        w = self.nodes[p].left;
                    }
                    if let Some(wi) = w {
                        self.nodes[wi].color = self.nodes[p].color;
                        if let Some(l) = self.nodes[wi].left {
                            self.nodes[l].color = Color::Black;
                        }
                    }
                    self.nodes[p].color = Color::Black;
                    self.rotate_right(p);
                    x = self.root;
                }
            }
        }
        if let Some(xi) = x {
            self.nodes[xi].color = Color::Black;
        }
    }

    pub fn text(&self) -> String {
        String::from_utf16_lossy(&self.units())
    }

    /// Visible document elements in CodeMirror's native index space.
    ///
    /// JavaScript strings and CodeMirror positions count UTF-16 code units,
    /// not Unicode scalar values. Keeping one unit per tree item avoids an
    /// O(document length) offset translation on every browser edit.
    pub fn units(&self) -> Vec<u16> {
        let mut out = Vec::with_capacity(self.len());
        self.walk(self.root, &mut |n| out.push(n.unit));
        out
    }

    pub fn atoms(&self) -> Vec<(Weight, u16, u64)> {
        let mut out = Vec::with_capacity(self.len());
        self.walk(self.root, &mut |n| {
            out.push((n.weight.clone(), n.unit, n.counter))
        });
        out
    }

    fn walk(&self, i: Option<usize>, f: &mut impl FnMut(&Node)) {
        if let Some(i) = i {
            self.walk(self.nodes[i].left, f);
            f(&self.nodes[i]);
            self.walk(self.nodes[i].right, f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fraction::Fraction;

    #[test]
    fn mixed_ops_stay_ordered() {
        let mut t = DocTree::default();
        let ids: Vec<_> = (1..40)
            .map(|i| Weight::new(Fraction::new(i, i + 1), 0, vec![0], 1))
            .collect();
        for (i, w) in ids.iter().enumerate() {
            t.insert(w.clone(), u16::from(b'a' + (i as u8 % 26)), i as u64);
        }
        assert_eq!(t.len(), 39);
        for i in (0..39).step_by(2) {
            t.delete(&ids[i]);
        }
        let text = t.text();
        assert_eq!(text.encode_utf16().count(), t.len());
        let atoms = t.atoms();
        for w in atoms.windows(2) {
            assert!(w[0].0 < w[1].0);
        }
    }

    #[test]
    fn rank_and_lower_bound_survive_deletion() {
        let mut tree = DocTree::default();
        let weights: Vec<_> = (1..=5)
            .map(|i| Weight::new(Fraction::new(i, 6), 0, vec![0], 1))
            .collect();
        for (index, weight) in weights.iter().enumerate() {
            assert!(tree.insert(weight.clone(), b'a' as u16 + index as u16, 1));
        }
        assert_eq!(tree.index_of(&weights[2]), Some(2));
        assert_eq!(tree.lower_bound(&weights[2]), 2);
        assert!(tree.delete(&weights[2]));
        assert_eq!(tree.index_of(&weights[2]), None);
        assert_eq!(tree.lower_bound(&weights[2]), 2);
    }
}
