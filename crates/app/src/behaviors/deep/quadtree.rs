//! The t-SNE fit's repulsion by Barnes and Hut's approximation (van der Maaten, 2014): the
//! points sit in a quadtree, and a far cell pushes on a point as one body at its centre of
//! mass, so each step costs about n log n rather than every pair.

/// How small a cell must look from a point, its width over its distance, to count as one body.
const THETA: f64 = 0.5;
/// A cell this narrow keeps every point given it, so points in one place never split forever.
const NARROWEST: f64 = 1e-9;
/// No cell's children start at the root, so this marks a leaf.
const LEAF: u32 = 0;

struct Cell {
    centre: [f64; 2],
    half: f64,
    count: f64,
    /// The sum of its points' positions, which over `count` is their centre of mass.
    sum: [f64; 2],
    /// The first of its four children, or [`LEAF`].
    children: u32,
    /// A leaf's point, moved down when the leaf splits.
    point: u32,
}

impl Cell {
    fn new(centre: [f64; 2], half: f64) -> Cell {
        Cell {
            centre,
            half,
            count: 0.0,
            sum: [0.0; 2],
            children: LEAF,
            point: 0,
        }
    }

    fn quarter(&self, at: [f64; 2]) -> u32 {
        u32::from(at[0] >= self.centre[0]) + 2 * u32::from(at[1] >= self.centre[1])
    }

    /// Whether `at` lies in this cell, its lower edges in and its upper ones out, as
    /// [`Cell::quarter`] sends a point on an edge to the upper side.
    fn holds(&self, at: [f64; 2]) -> bool {
        (0..2).all(|axis| {
            at[axis] >= self.centre[axis] - self.half && at[axis] < self.centre[axis] + self.half
        })
    }
}

pub(crate) struct QuadTree<'a> {
    y: &'a [[f64; 2]],
    cells: Vec<Cell>,
}

/// What every other point does to one: the sum of its kernels, for the normalisation, and its
/// push, each kernel squared times the way from that point.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Repulsion {
    pub kernels: f64,
    pub push: [f64; 2],
}

impl<'a> QuadTree<'a> {
    pub fn of(y: &'a [[f64; 2]]) -> QuadTree<'a> {
        let (mut low, mut high) = ([f64::MAX; 2], [f64::MIN; 2]);
        for point in y {
            for axis in 0..2 {
                low[axis] = low[axis].min(point[axis]);
                high[axis] = high[axis].max(point[axis]);
            }
        }
        let centre = [(low[0] + high[0]) / 2.0, (low[1] + high[1]) / 2.0];
        let half = ((high[0] - low[0]).max(high[1] - low[1]) / 2.0).max(NARROWEST) * 1.0001;
        let mut cells = Vec::with_capacity(2 * y.len() + 1);
        cells.push(Cell::new(centre, half));
        let mut tree = QuadTree { y, cells };
        for at in 0..y.len() {
            tree.insert(at as u32);
        }
        tree
    }

    fn insert(&mut self, point: u32) {
        let at = self.y[point as usize];
        let mut cell = 0usize;
        loop {
            let here = &self.cells[cell];
            if here.children == LEAF && here.count > 0.0 && here.half >= NARROWEST {
                self.split(cell);
            }
            let here = &mut self.cells[cell];
            here.count += 1.0;
            here.sum[0] += at[0];
            here.sum[1] += at[1];
            if here.children == LEAF {
                if here.count == 1.0 {
                    here.point = point;
                }
                return;
            }
            cell = (here.children + here.quarter(at)) as usize;
        }
    }

    /// A leaf of one point becomes four, its point moving down into one of them.
    fn split(&mut self, cell: usize) {
        let (centre, half) = (self.cells[cell].centre, self.cells[cell].half / 2.0);
        let first = self.cells.len() as u32;
        for quarter in 0..4 {
            let x = centre[0] + if quarter % 2 == 1 { half } else { -half };
            let y = centre[1] + if quarter / 2 == 1 { half } else { -half };
            self.cells.push(Cell::new([x, y], half));
        }
        let moved = self.cells[cell].point;
        let at = self.y[moved as usize];
        let quarter = self.cells[cell].quarter(at);
        self.cells[cell].children = first;
        let child = &mut self.cells[(first + quarter) as usize];
        child.count = 1.0;
        child.sum = at;
        child.point = moved;
    }

    /// What every point but `point` does to it. `stack` is room for the walk, kept between
    /// calls so a fit's thousands of walks make no allocation each.
    pub fn repulsion(&self, point: usize, stack: &mut Vec<u32>) -> Repulsion {
        let mut out = Repulsion::default();
        let at = self.y[point];
        stack.clear();
        stack.push(0);
        while let Some(cell) = stack.pop() {
            let here = &self.cells[cell as usize];
            if here.count == 0.0 {
                continue;
            }
            let leaf = here.children == LEAF;
            let mass = [here.sum[0] / here.count, here.sum[1] / here.count];
            let way = [at[0] - mass[0], at[1] - mass[1]];
            let squared = way[0] * way[0] + way[1] * way[1];
            let width = 2.0 * here.half;
            if !leaf && width * width >= THETA * THETA * squared {
                stack.extend(here.children..here.children + 4);
                continue;
            }
            // Every point is in the tree, so a leaf whose cell holds this point holds it.
            let count = if leaf && here.holds(at) {
                here.count - 1.0
            } else {
                here.count
            };
            if count <= 0.0 {
                continue;
            }
            let kernel = 1.0 / (1.0 + squared);
            out.kernels += count * kernel;
            out.push[0] += count * kernel * kernel * way[0];
            out.push[1] += count * kernel * kernel * way[1];
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(y: &[[f64; 2]], point: usize) -> Repulsion {
        let mut out = Repulsion::default();
        for (other, at) in y.iter().enumerate() {
            if other == point {
                continue;
            }
            let way = [y[point][0] - at[0], y[point][1] - at[1]];
            let kernel = 1.0 / (1.0 + way[0] * way[0] + way[1] * way[1]);
            out.kernels += kernel;
            out.push[0] += kernel * kernel * way[0];
            out.push[1] += kernel * kernel * way[1];
        }
        out
    }

    #[test]
    fn the_approximation_is_close_to_every_pair() {
        let mut random = super::super::linalg::Seeded::new(11);
        let y: Vec<[f64; 2]> = (0..400)
            .map(|_| [10.0 * random.normal(), 10.0 * random.normal()])
            .collect();
        let tree = QuadTree::of(&y);
        let mut stack = Vec::new();
        for point in [0, 17, 399] {
            let (ours, all) = (tree.repulsion(point, &mut stack), exact(&y, point));
            assert!(
                (ours.kernels - all.kernels).abs() < 0.02 * all.kernels,
                "{ours:?} {all:?}"
            );
            let size = all.push[0].hypot(all.push[1]);
            let off = (ours.push[0] - all.push[0]).hypot(ours.push[1] - all.push[1]);
            assert!(off < 0.05 * size.max(1e-9), "{ours:?} {all:?}");
        }
    }

    #[test]
    fn points_in_one_place_stay_in_one_leaf() {
        let y = vec![[1.0, 1.0]; 50];
        let tree = QuadTree::of(&y);
        let one = tree.repulsion(3, &mut Vec::new());
        assert!((one.kernels - 49.0).abs() < 1e-9, "{one:?}");
        assert_eq!(one.push, [0.0, 0.0]);
    }
}
