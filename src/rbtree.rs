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
    ch: char,
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

    pub fn insert(&mut self, weight: Weight, ch: char, counter: u64) -> bool {
        if self.find(&weight).is_some() {
            return false;
        }
        let z = self.alloc(Node {
            weight,
            ch,
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

    pub fn find(&self, w: &Weight) -> Option<(char, u64)> {
        let mut x = self.root;
        while let Some(xi) = x {
            match w.cmp(&self.nodes[xi].weight) {
                core::cmp::Ordering::Less => x = self.nodes[xi].left,
                core::cmp::Ordering::Greater => x = self.nodes[xi].right,
                core::cmp::Ordering::Equal => {
                    return Some((self.nodes[xi].ch, self.nodes[xi].counter))
                }
            }
        }
        None
    }

    pub fn contains(&self, w: &Weight) -> bool {
        self.find(w).is_some()
    }

    pub fn get_at(&self, mut index: usize) -> Option<(&Weight, char, u64)> {
        let mut x = self.root?;
        loop {
            let ls = self.sz(self.nodes[x].left);
            if index < ls {
                x = self.nodes[x].left?;
            } else if index == ls {
                let n = &self.nodes[x];
                return Some((&n.weight, n.ch, n.counter));
            } else {
                index -= ls + 1;
                x = self.nodes[x].right?;
            }
        }
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

    fn delete_idx(&mut self, z: usize) {
        let mut y = z;
        let y_orig = self.nodes[y].color;
        let x;
        let mut xp = self.nodes[z].parent;

        if self.nodes[z].left.is_none() {
            x = self.nodes[z].right;
            self.transplant(z, x);
        } else if self.nodes[z].right.is_none() {
            x = self.nodes[z].left;
            self.transplant(z, x);
        } else {
            y = self.minimum(self.nodes[z].right.unwrap());
            let y_color = self.nodes[y].color;
            x = self.nodes[y].right;
            if self.nodes[y].parent == Some(z) {
                xp = Some(y);
                if let Some(xi) = x {
                    self.nodes[xi].parent = Some(y);
                }
            } else {
                xp = self.nodes[y].parent;
                self.transplant(y, x);
                self.nodes[y].right = self.nodes[z].right;
                if let Some(r) = self.nodes[y].right {
                    self.nodes[r].parent = Some(y);
                }
            }
            self.transplant(z, Some(y));
            self.nodes[y].left = self.nodes[z].left;
            if let Some(l) = self.nodes[y].left {
                self.nodes[l].parent = Some(y);
            }
            self.nodes[y].color = self.nodes[z].color;
            let _ = y_color;
        }

        let mut p = xp.or(x.and_then(|xi| self.nodes[xi].parent));
        if p.is_none() {
            p = self.root;
        }
        while let Some(pi) = p {
            self.pull(pi);
            p = self.nodes[pi].parent;
        }
        if let Some(r) = self.root {
            self.recompute(r);
        }

        if y_orig == Color::Black {
            self.delete_fix(x);
        }
        self.free.push(z);
    }

    fn recompute(&mut self, i: usize) -> usize {
        let ls = self.nodes[i].left.map(|l| self.recompute(l)).unwrap_or(0);
        let rs = self.nodes[i].right.map(|r| self.recompute(r)).unwrap_or(0);
        self.nodes[i].size = 1 + ls + rs;
        self.nodes[i].size
    }

    fn delete_fix(&mut self, mut x: Option<usize>) {
        while x != self.root && self.color(x) == Color::Black {
            let p = match x.and_then(|xi| self.nodes[xi].parent).or(self.root) {
                Some(p) => p,
                None => break,
            };
            let is_left = x == self.nodes[p].left || (x.is_none() && self.nodes[p].left.is_none());
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
        let mut s = String::with_capacity(self.len());
        self.walk(self.root, &mut |n| s.push(n.ch));
        s
    }

    pub fn atoms(&self) -> Vec<(Weight, char, u64)> {
        let mut out = Vec::with_capacity(self.len());
        self.walk(self.root, &mut |n| {
            out.push((n.weight.clone(), n.ch, n.counter))
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
            t.insert(
                w.clone(),
                char::from_u32(b'a' as u32 + (i as u32 % 26)).unwrap(),
                i as u64,
            );
        }
        assert_eq!(t.len(), 39);
        for i in (0..39).step_by(2) {
            t.delete(&ids[i]);
        }
        let text = t.text();
        assert_eq!(text.chars().count(), t.len());
        let atoms = t.atoms();
        for w in atoms.windows(2) {
            assert!(w[0].0 < w[1].0);
        }
    }
}
