//! The little linear algebra the map needs, on plain rows: a seeded random source, sparse rows,
//! orthonormal columns, the eigenvectors of a small symmetric matrix, and a truncated SVD.

/// A seeded random source, so a map comes out the same each time from the same summaries.
pub(crate) struct Seeded(u64);

impl Seeded {
    pub fn new(seed: u64) -> Seeded {
        Seeded(seed)
    }

    /// SplitMix64.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// In 0..1.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// From a normal distribution with mean 0 and deviation 1 (Box and Muller).
    pub fn normal(&mut self) -> f64 {
        let u = self.unit().max(f64::MIN_POSITIVE);
        let v = self.unit();
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }

    /// In 0..n.
    pub fn below(&mut self, n: usize) -> usize {
        (self.unit() * n as f64) as usize % n.max(1)
    }
}

/// A row of a sparse matrix: column and value, columns ascending.
pub(crate) type SparseRow = Vec<(usize, f64)>;

/// Makes `columns` (each a column of the same length) orthonormal in place, by modified
/// Gram-Schmidt. A column that falls to nothing is left as zeros.
pub(crate) fn orthonormalize(columns: &mut [Vec<f64>]) {
    for at in 0..columns.len() {
        let (done, rest) = columns.split_at_mut(at);
        let column = &mut rest[0];
        for earlier in done.iter() {
            let along = dot(earlier, column);
            for (value, e) in column.iter_mut().zip(earlier) {
                *value -= along * e;
            }
        }
        let norm = dot(column, column).sqrt();
        if norm > 1e-12 {
            column.iter_mut().for_each(|value| *value /= norm);
        } else {
            column.iter_mut().for_each(|value| *value = 0.0);
        }
    }
}

pub(crate) fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The eigenvalues and eigenvectors of the symmetric `matrix` (rows), largest first, by
/// Jacobi rotations. Each vector is a column of the second result, `vectors[row][k]`.
pub(crate) fn symmetric_eigen(matrix: &[Vec<f64>]) -> (Vec<f64>, Vec<Vec<f64>>) {
    let n = matrix.len();
    let mut a: Vec<Vec<f64>> = matrix.to_vec();
    let mut v: Vec<Vec<f64>> = (0..n)
        .map(|i| (0..n).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
        .collect();
    for _sweep in 0..100 {
        let off: f64 = (0..n)
            .flat_map(|i| (0..n).filter(move |&j| j != i).map(move |j| (i, j)))
            .map(|(i, j)| a[i][j] * a[i][j])
            .sum();
        if off < 1e-18 {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                if a[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for row in a.iter_mut() {
                    let (akp, akq) = (row[p], row[q]);
                    row[p] = c * akp - s * akq;
                    row[q] = s * akp + c * akq;
                }
                let (upper, lower) = a.split_at_mut(q);
                for (apk, aqk) in upper[p].iter_mut().zip(lower[0].iter_mut()) {
                    let (x, y) = (*apk, *aqk);
                    *apk = c * x - s * y;
                    *aqk = s * x + c * y;
                }
                for row in v.iter_mut() {
                    let (vkp, vkq) = (row[p], row[q]);
                    row[p] = c * vkp - s * vkq;
                    row[q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&x, &y| a[y][y].total_cmp(&a[x][x]));
    let values = order.iter().map(|&k| a[k][k]).collect();
    let vectors = (0..n)
        .map(|row| order.iter().map(|&k| v[row][k]).collect())
        .collect();
    (values, vectors)
}

/// The rows of sparse `rows` (with `width` columns) in their top `k` directions: `U Σ` of a
/// truncated SVD, found by a seeded randomized range finder with two power iterations.
pub(crate) fn truncated_svd(
    rows: &[SparseRow],
    width: usize,
    k: usize,
    seed: u64,
) -> Vec<Vec<f64>> {
    let n = rows.len();
    let k = k.min(n).min(width);
    if k == 0 {
        return vec![Vec::new(); n];
    }
    let wide = (k + 10).min(n).min(width);
    let mut random = Seeded::new(seed);
    // Ω is width × wide; Y = XΩ, column by column.
    let omega: Vec<Vec<f64>> = (0..width)
        .map(|_| (0..wide).map(|_| random.normal()).collect())
        .collect();
    let mut y: Vec<Vec<f64>> = (0..wide)
        .map(|c| {
            rows.iter()
                .map(|row| row.iter().map(|&(j, x)| x * omega[j][c]).sum())
                .collect()
        })
        .collect();
    orthonormalize(&mut y);
    for _ in 0..2 {
        // Y = X (Xᵀ Y)
        let xt_y: Vec<Vec<f64>> = y
            .iter()
            .map(|column| {
                let mut out = vec![0.0; width];
                for (row, &weight) in rows.iter().zip(column) {
                    for &(j, x) in row {
                        out[j] += x * weight;
                    }
                }
                out
            })
            .collect();
        y = xt_y
            .iter()
            .map(|column| {
                rows.iter()
                    .map(|row| row.iter().map(|&(j, x)| x * column[j]).sum())
                    .collect()
            })
            .collect();
        orthonormalize(&mut y);
    }
    // B = Qᵀ X is wide × width; B Bᵀ is wide × wide.
    let b: Vec<Vec<f64>> = y
        .iter()
        .map(|column| {
            let mut out = vec![0.0; width];
            for (row, &weight) in rows.iter().zip(column) {
                for &(j, x) in row {
                    out[j] += x * weight;
                }
            }
            out
        })
        .collect();
    let bbt: Vec<Vec<f64>> = b
        .iter()
        .map(|left| b.iter().map(|right| dot(left, right)).collect())
        .collect();
    let (values, vectors) = symmetric_eigen(&bbt);
    // U Σ = Q U_B Σ_B, and Σ_B² are the eigenvalues of B Bᵀ.
    (0..n)
        .map(|i| {
            (0..k)
                .map(|c| {
                    let sigma = values[c].max(0.0).sqrt();
                    let along: f64 = (0..wide).map(|r| y[r][i] * vectors[r][c]).sum();
                    along * sigma
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_numbers() {
        let mut one = Seeded::new(7);
        let mut two = Seeded::new(7);
        for _ in 0..5 {
            assert_eq!(one.next_u64(), two.next_u64());
        }
        let mean: f64 = (0..4000).map(|_| one.normal()).sum::<f64>() / 4000.0;
        assert!(mean.abs() < 0.1, "{mean}");
    }

    #[test]
    fn jacobi_finds_a_symmetric_matrixs_eigenvalues_largest_first() {
        let (values, vectors) = symmetric_eigen(&[vec![2.0, 1.0], vec![1.0, 2.0]]);
        assert!((values[0] - 3.0).abs() < 1e-9 && (values[1] - 1.0).abs() < 1e-9);
        let first = [vectors[0][0], vectors[1][0]];
        assert!((first[0].abs() - 0.5f64.sqrt()).abs() < 1e-9);
        assert!((first[0] - first[1]).abs() < 1e-9);
    }

    #[test]
    fn the_svd_keeps_the_distances_of_rows_that_fit_in_k_directions() {
        // Three rows in two directions: their distances survive the reduction.
        let rows: Vec<SparseRow> = vec![
            vec![(0, 1.0), (1, 1.0)],
            vec![(0, 1.0), (1, 1.0)],
            vec![(2, 2.0)],
        ];
        let reduced = truncated_svd(&rows, 3, 2, 7);
        let distance = |a: &[f64], b: &[f64]| -> f64 {
            a.iter()
                .zip(b)
                .map(|(x, y)| (x - y).powi(2))
                .sum::<f64>()
                .sqrt()
        };
        assert!(distance(&reduced[0], &reduced[1]) < 1e-9);
        let far = distance(&reduced[0], &reduced[2]);
        assert!((far - (2.0f64 + 4.0).sqrt()).abs() < 1e-6, "{far}");
    }
}
