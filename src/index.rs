//! A static spatial index over bounding boxes: a packed Hilbert R-tree (the Flatbush design).
//! Items are sorted along a Hilbert curve by the centers of their boxes and grouped 16 at a
//! time into a tree of boxes, built once; searches then visit only the parts of the tree that
//! overlap the query. Used for GeoJSON sources, so drawing and picking look at the features in
//! view instead of all of them.

/// Children per node.
const NODE_SIZE: usize = 16;

/// A box: [min_x, min_y, max_x, max_y].
pub type BBox = [f64; 4];

#[derive(Clone, Debug, Default)]
pub struct Index {
    /// Level 0 holds the items' boxes in Hilbert order; each level above holds one box per
    /// group of `NODE_SIZE` boxes below it. The last level is the root.
    levels: Vec<Vec<BBox>>,
    /// Item number for each box of level 0.
    ids: Vec<u32>,
}

impl Index {
    /// Build an index of `boxes` (item `i` is `boxes[i]`).
    pub fn new(boxes: &[BBox]) -> Self {
        if boxes.is_empty() {
            return Self::default();
        }
        let mut ext = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for b in boxes {
            ext = [ext[0].min(b[0]), ext[1].min(b[1]), ext[2].max(b[2]), ext[3].max(b[3])];
        }
        let (w, h) = ((ext[2] - ext[0]).max(1e-12), (ext[3] - ext[1]).max(1e-12));
        let max = (1u32 << 16) - 1;
        let mut order: Vec<(u32, u32)> = boxes
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let x = ((b[0] + b[2]) / 2.0 - ext[0]) / w * max as f64;
                let y = ((b[1] + b[3]) / 2.0 - ext[1]) / h * max as f64;
                (hilbert(x as u32, y as u32), i as u32)
            })
            .collect();
        order.sort_unstable();
        let ids: Vec<u32> = order.iter().map(|(_, i)| *i).collect();
        let mut levels = vec![ids.iter().map(|&i| boxes[i as usize]).collect::<Vec<BBox>>()];
        while levels.last().is_some_and(|l| l.len() > 1) {
            let below = levels.last().unwrap();
            let up: Vec<BBox> = below
                .chunks(NODE_SIZE)
                .map(|c| c.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]))
                .collect();
            levels.push(up);
        }
        Self { levels, ids }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The items whose boxes overlap `q`, in no particular order.
    pub fn search(&self, q: BBox) -> Vec<usize> {
        let mut out = Vec::new();
        let Some(top) = self.levels.len().checked_sub(1) else { return out };
        let overlaps = |b: &BBox| !(b[2] < q[0] || b[0] > q[2] || b[3] < q[1] || b[1] > q[3]);
        // (level, node) pairs to visit.
        let mut stack: Vec<(usize, usize)> = (0..self.levels[top].len()).map(|i| (top, i)).collect();
        while let Some((lvl, i)) = stack.pop() {
            if !overlaps(&self.levels[lvl][i]) {
                continue;
            }
            if lvl == 0 {
                out.push(self.ids[i] as usize);
            } else {
                let below = self.levels[lvl - 1].len();
                stack.extend((i * NODE_SIZE..((i + 1) * NODE_SIZE).min(below)).map(|c| (lvl - 1, c)));
            }
        }
        out
    }
}

/// Position of (x, y) along a Hilbert curve over a 2^16 x 2^16 grid.
fn hilbert(mut x: u32, mut y: u32) -> u32 {
    let mut d = 0u32;
    let mut s = 1u32 << 15;
    while s > 0 {
        let rx = u32::from(x & s > 0);
        let ry = u32::from(y & s > 0);
        d = d.wrapping_add(s.wrapping_mul(s).wrapping_mul((3 * rx) ^ ry));
        if ry == 0 {
            if rx == 1 {
                x = s.wrapping_mul(2).wrapping_sub(1).wrapping_sub(x);
                y = s.wrapping_mul(2).wrapping_sub(1).wrapping_sub(y);
            }
            std::mem::swap(&mut x, &mut y);
        }
        s >>= 1;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_exactly_the_overlapping_boxes() {
        // A 100x100 grid of unit boxes plus a few big ones.
        let mut boxes: Vec<BBox> = (0..10_000).map(|i| {
            let (x, y) = ((i % 100) as f64, (i / 100) as f64);
            [x, y, x + 0.5, y + 0.5]
        }).collect();
        boxes.push([-10.0, -10.0, 200.0, 200.0]);
        boxes.push([40.0, 40.0, 41.0, 60.0]);
        let index = Index::new(&boxes);
        assert_eq!(index.len(), boxes.len());
        for q in [[10.2, 10.2, 12.7, 11.1], [-5.0, -5.0, -1.0, -1.0], [40.6, 50.0, 40.7, 50.0], [0.0, 0.0, 99.5, 99.5]] {
            let mut got = index.search(q);
            got.sort_unstable();
            let want: Vec<usize> = boxes.iter().enumerate().filter(|(_, b)| !(b[2] < q[0] || b[0] > q[2] || b[3] < q[1] || b[1] > q[3])).map(|(i, _)| i).collect();
            assert_eq!(got, want, "{q:?}");
        }
        assert!(Index::new(&[]).search([0.0, 0.0, 1.0, 1.0]).is_empty());
    }
}
