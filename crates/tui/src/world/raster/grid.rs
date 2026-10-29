/// Numbers laid on a grid over a picture, read smoothly between its points: the fields the
/// world's ground grows from. Values are kept as `f32` and worked in `f64`, so a field
/// computed again comes out the same to the bit.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    width: usize,
    height: usize,
    values: Vec<f32>,
}

impl Grid {
    /// A grid of zeros.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            values: vec![0.0; width * height],
        }
    }

    /// A grid whose point (x, y) holds `value(x, y)`, filled row by row from the top left.
    pub fn from_fn(
        width: usize,
        height: usize,
        mut value: impl FnMut(usize, usize) -> f64,
    ) -> Self {
        let values = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| value(x, y) as f32)
            .collect();
        Self {
            width,
            height,
            values,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// The value at point (x, y), or 0 off the grid.
    pub fn get(&self, x: usize, y: usize) -> f64 {
        if x < self.width && y < self.height {
            f64::from(self.values[y * self.width + x])
        } else {
            0.0
        }
    }

    /// Sets the value at point (x, y); off the grid it changes nothing.
    pub fn set(&mut self, x: usize, y: usize, value: f64) {
        if x < self.width && y < self.height {
            self.values[y * self.width + x] = value as f32;
        }
    }

    /// The values row by row from the top left.
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    pub fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    /// The value at (x, y) in grid points, mixed from the four points round it, and held to
    /// the grid's edge beyond it. A grid narrower than two points each way gives its nearest
    /// point.
    pub fn sample(&self, x: f64, y: f64) -> f64 {
        if self.width < 2 || self.height < 2 {
            let nearest = |v: f64, size: usize| (v.max(0.0) as usize).min(size.saturating_sub(1));
            return self.get(nearest(x, self.width), nearest(y, self.height));
        }
        // Held just inside the last point, so the point after it can always be read.
        let x = hold(x, 0.0, self.width as f64 - 1.001);
        let y = hold(y, 0.0, self.height as f64 - 1.001);
        let (i, j) = (whole(x), whole(y));
        let (dx, dy) = (x - i as f64, y - j as f64);
        let at = j * self.width + i;
        let value = |index: usize| f64::from(self.values[index]);
        (value(at) * (1.0 - dx) + value(at + 1) * dx) * (1.0 - dy)
            + (value(at + self.width) * (1.0 - dx) + value(at + self.width + 1) * dx) * dy
    }

    /// The grid blurred by two rounds of box blurs `radius` points each way, across then
    /// down, with the edge points carried on past the edges.
    pub fn blurred(&self, radius: usize) -> Self {
        let (width, height) = (self.width, self.height);
        if width == 0 || height == 0 {
            return self.clone();
        }
        let size = (2 * radius + 1) as f64;
        let mut ahead = self.values.clone();
        let mut across = vec![0.0_f32; ahead.len()];
        let spread = radius as isize;
        let edge = |k: isize, size: usize| k.clamp(0, size as isize - 1) as usize;
        for _ in 0..2 {
            for row in (0..height).map(|j| j * width) {
                let mut sum = 0.0;
                for k in -spread..=spread {
                    sum += f64::from(ahead[row + edge(k, width)]);
                }
                for k in 0..width {
                    across[row + k] = (sum / size) as f32;
                    sum += f64::from(ahead[row + (k + radius + 1).min(width - 1)])
                        - f64::from(ahead[row + k.saturating_sub(radius)]);
                }
            }
            for k in 0..width {
                let mut sum = 0.0;
                for j in -spread..=spread {
                    sum += f64::from(across[edge(j, height) * width + k]);
                }
                for j in 0..height {
                    ahead[j * width + k] = (sum / size) as f32;
                    sum += f64::from(across[(j + radius + 1).min(height - 1) * width + k])
                        - f64::from(across[j.saturating_sub(radius) * width + k]);
                }
            }
        }
        Self {
            width,
            height,
            values: ahead,
        }
    }
}

/// `value` held to `from..=to`, letting not a number through, as the fields' own clamp does.
fn hold(value: f64, from: f64, to: f64) -> f64 {
    if value < from {
        from
    } else if value > to {
        to
    } else {
        value
    }
}

/// A held position's whole part; not a number reads as the first point.
fn whole(value: f64) -> usize {
    if value.is_nan() { 0 } else { value as usize }
}
