//! Median-split k-d tree over scan points for nearest-neighbour queries.

use artificer_geometry::Point3;

const LEAF_SIZE: usize = 8;

enum Node {
    Leaf {
        start: u32,
        end: u32,
    },
    Split {
        dim: u8,
        value: f64,
        left: u32,
        right: u32,
    },
}

pub struct KdTree3 {
    points: Vec<Point3>,
    order: Vec<u32>,
    nodes: Vec<Node>,
}

fn coordinate(point: Point3, dim: u8) -> f64 {
    match dim {
        0 => point.x,
        1 => point.y,
        _ => point.z,
    }
}

impl KdTree3 {
    pub fn build(points: Vec<Point3>) -> Self {
        let mut order: Vec<u32> = (0..points.len() as u32).collect();
        let mut nodes = Vec::new();
        if !points.is_empty() {
            let len = order.len();
            build_recursive(&points, &mut order, 0, len, &mut nodes);
        }
        Self {
            points,
            order,
            nodes,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Index of the nearest stored point and the squared distance to it.
    pub fn nearest(&self, query: Point3) -> Option<(u32, f64)> {
        if self.nodes.is_empty() {
            return None;
        }
        let mut best = (u32::MAX, f64::INFINITY);
        self.search(0, query, &mut best);
        (best.0 != u32::MAX).then_some(best)
    }

    fn search(&self, node: usize, query: Point3, best: &mut (u32, f64)) {
        match &self.nodes[node] {
            Node::Leaf { start, end } => {
                for &index in &self.order[*start as usize..*end as usize] {
                    let d = (self.points[index as usize] - query).length();
                    let d2 = d * d;
                    if d2 < best.1 {
                        *best = (index, d2);
                    }
                }
            }
            Node::Split {
                dim,
                value,
                left,
                right,
            } => {
                let delta = coordinate(query, *dim) - value;
                let (near, far) = if delta <= 0.0 {
                    (*left, *right)
                } else {
                    (*right, *left)
                };
                self.search(near as usize, query, best);
                if delta * delta < best.1 {
                    self.search(far as usize, query, best);
                }
            }
        }
    }
}

fn build_recursive(
    points: &[Point3],
    order: &mut [u32],
    start: usize,
    end: usize,
    nodes: &mut Vec<Node>,
) -> u32 {
    let index = nodes.len() as u32;
    if end - start <= LEAF_SIZE {
        nodes.push(Node::Leaf {
            start: start as u32,
            end: end as u32,
        });
        return index;
    }
    let slice = &mut order[start..end];
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for &i in slice.iter() {
        for dim in 0..3u8 {
            let c = coordinate(points[i as usize], dim);
            min[dim as usize] = min[dim as usize].min(c);
            max[dim as usize] = max[dim as usize].max(c);
        }
    }
    let dim = (0..3u8)
        .max_by(|&a, &b| {
            (max[a as usize] - min[a as usize]).total_cmp(&(max[b as usize] - min[b as usize]))
        })
        .unwrap_or(0);
    let mid = slice.len() / 2;
    slice.select_nth_unstable_by(mid, |&a, &b| {
        coordinate(points[a as usize], dim).total_cmp(&coordinate(points[b as usize], dim))
    });
    let value = coordinate(points[slice[mid] as usize], dim);
    nodes.push(Node::Split {
        dim,
        value,
        left: 0,
        right: 0,
    });
    let left = build_recursive(points, order, start, start + mid, nodes);
    let right = build_recursive(points, order, start + mid, end, nodes);
    if let Node::Split {
        left: l, right: r, ..
    } = &mut nodes[index as usize]
    {
        *l = left;
        *r = right;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_brute_force_on_a_grid_with_jitter() {
        let mut points = Vec::new();
        for i in 0..12 {
            for j in 0..12 {
                for k in 0..4 {
                    let jitter = ((i * 31 + j * 17 + k * 7) % 13) as f64 * 0.013;
                    points.push(Point3::new(
                        i as f64 + jitter,
                        j as f64 - jitter,
                        k as f64 * 2.0 + jitter,
                    ));
                }
            }
        }
        let tree = KdTree3::build(points.clone());
        for probe in 0..40 {
            let q = Point3::new(
                (probe % 11) as f64 + 0.4,
                (probe % 7) as f64 + 0.2,
                (probe % 5) as f64,
            );
            let (index, d2) = tree.nearest(q).unwrap();
            let brute = points
                .iter()
                .map(|p| {
                    let d = (*p - q).length();
                    d * d
                })
                .fold(f64::INFINITY, f64::min);
            let found = (points[index as usize] - q).length();
            assert!((found * found - brute).abs() < 1e-12);
            assert!((d2 - brute).abs() < 1e-12);
        }
    }

    #[test]
    fn empty_tree_returns_none() {
        assert!(
            KdTree3::build(Vec::new())
                .nearest(Point3::default())
                .is_none()
        );
    }
}
