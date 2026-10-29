//! Work done row by row over a picture, spread across the machine's cores.

use std::thread;

/// Fills `out`, `width` items a row, by calling `fill` on each row with its index. The rows are
/// shared out in bands, one for each core, so a whole map is drawn in a fraction of the time.
pub fn in_rows<T: Send>(out: &mut [T], width: usize, fill: impl Fn(usize, &mut [T]) + Sync) {
    if width == 0 || out.is_empty() {
        return;
    }
    let rows = out.len() / width;
    let cores = thread::available_parallelism().map_or(1, |cores| cores.get());
    let band = rows.div_ceil(cores.min(rows).max(1));
    let fill = &fill;
    thread::scope(|scope| {
        for (index, chunk) in out.chunks_mut(band * width).enumerate() {
            scope.spawn(move || {
                for (offset, row) in chunk.chunks_mut(width).enumerate() {
                    fill(index * band + offset, row);
                }
            });
        }
    });
}
