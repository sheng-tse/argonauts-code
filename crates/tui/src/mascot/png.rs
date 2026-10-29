//! Just enough PNG for the mascot's pictures: reading the 8-bit RGBA files the generator writes,
//! fitting one into a box of another size, and writing the result.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::Crc;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
/// Bytes per pixel: red, green, blue and alpha.
const PIXEL: usize = 4;

/// A picture as rows of straight-alpha RGBA pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// A part of a picture, in its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rgba {
    pub fn transparent(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height * PIXEL],
        }
    }

    fn at(&self, x: usize, y: usize) -> &[u8] {
        let start = (y * self.width + x) * PIXEL;
        &self.pixels[start..start + PIXEL]
    }
}

/// The picture in `bytes`, when it is an 8-bit RGBA PNG without interlacing, as the generator
/// writes them. Anything else is `None`.
pub fn decode(bytes: &[u8]) -> Option<Rgba> {
    let mut rest = bytes.strip_prefix(&SIGNATURE)?;
    let mut size = None;
    let mut packed = Vec::new();
    while rest.len() >= 12 {
        let length = usize::try_from(u32::from_be_bytes(rest[..4].try_into().ok()?)).ok()?;
        let kind = &rest[4..8];
        let data = rest.get(8..8 + length)?;
        match kind {
            b"IHDR" => {
                let number = |at: usize| -> Option<usize> {
                    usize::try_from(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?)).ok()
                };
                // Eight bits a channel, RGBA, deflate, adaptive filters, no interlacing.
                if data.get(8..13)? != [8, 6, 0, 0, 0] {
                    return None;
                }
                size = Some((number(0)?, number(4)?));
            }
            b"IDAT" => packed.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        rest = rest.get(12 + length..)?;
    }
    let (width, height) = size?;
    let mut filtered = Vec::new();
    ZlibDecoder::new(packed.as_slice())
        .read_to_end(&mut filtered)
        .ok()?;
    let stride = width * PIXEL;
    if filtered.len() != height * (stride + 1) {
        return None;
    }
    let mut pixels = vec![0u8; height * stride];
    for y in 0..height {
        let row = &filtered[y * (stride + 1)..(y + 1) * (stride + 1)];
        let (filter, row) = (row[0], &row[1..]);
        let (done, current) = pixels.split_at_mut(y * stride);
        let above = (y > 0).then(|| &done[(y - 1) * stride..]);
        let current = &mut current[..stride];
        for x in 0..stride {
            let left = if x >= PIXEL { current[x - PIXEL] } else { 0 };
            let up = above.map_or(0, |above| above[x]);
            let corner = match above {
                Some(above) if x >= PIXEL => above[x - PIXEL],
                _ => 0,
            };
            let predicted = match filter {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => paeth(left, up, corner),
                _ => return None,
            };
            current[x] = row[x].wrapping_add(predicted);
        }
    }
    Some(Rgba {
        width,
        height,
        pixels,
    })
}

fn paeth(left: u8, up: u8, corner: u8) -> u8 {
    let estimate = i16::from(left) + i16::from(up) - i16::from(corner);
    let from = |value: u8| (estimate - i16::from(value)).abs();
    let (to_left, to_up, to_corner) = (from(left), from(up), from(corner));
    if to_left <= to_up && to_left <= to_corner {
        left
    } else if to_up <= to_corner {
        up
    } else {
        corner
    }
}

/// `image` as an 8-bit RGBA PNG, packed as small as it goes.
pub fn encode(image: &Rgba) -> Vec<u8> {
    encode_packed(image, Compression::best())
}

/// [`encode`] for a picture drawn again as you point at it, packed fast rather than small.
pub fn encode_quickly(image: &Rgba) -> Vec<u8> {
    encode_packed(image, Compression::fast())
}

/// [`encode_quickly`], given up as soon as `stop` says so, which it asks every few rows: a
/// picture no longer wanted costs at most a millisecond or two more.
pub fn encode_quickly_unless(image: &Rgba, stop: impl Fn() -> bool) -> Option<Vec<u8>> {
    const ROWS_AT_ONCE: usize = 16;
    let stride = image.width * PIXEL;
    let mut packed = ZlibEncoder::new(Vec::new(), Compression::fast());
    let mut filtered = Vec::with_capacity(ROWS_AT_ONCE * (stride + 1));
    for (at, row) in image
        .pixels
        .chunks(stride.max(1))
        .take(image.height)
        .enumerate()
    {
        if at % ROWS_AT_ONCE == 0 {
            if stop() {
                return None;
            }
            packed.write_all(&filtered).ok()?;
            filtered.clear();
        }
        filtered.push(0);
        filtered.extend_from_slice(row);
    }
    packed.write_all(&filtered).ok()?;
    Some(png_of(image, &packed.finish().ok()?))
}

fn encode_packed(image: &Rgba, compression: Compression) -> Vec<u8> {
    let stride = image.width * PIXEL;
    let mut filtered = Vec::with_capacity(image.height * (stride + 1));
    for row in image.pixels.chunks(stride.max(1)).take(image.height) {
        filtered.push(0);
        filtered.extend_from_slice(row);
    }
    let mut packed = ZlibEncoder::new(Vec::new(), compression);
    // Writing to memory can't fail.
    let _ = packed.write_all(&filtered);
    let packed = packed.finish().unwrap_or_default();
    png_of(image, &packed)
}

/// The PNG of `image` from its rows already filtered and packed.
fn png_of(image: &Rgba, packed: &[u8]) -> Vec<u8> {
    let mut header = Vec::with_capacity(13);
    for side in [image.width, image.height] {
        header.extend_from_slice(&u32::try_from(side).unwrap_or(u32::MAX).to_be_bytes());
    }
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut out = SIGNATURE.to_vec();
    for (kind, data) in [
        (b"IHDR", header.as_slice()),
        (b"IDAT", packed),
        (b"IEND", &[]),
    ] {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = Crc::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    }
    out
}

/// `from` of `image` drawn into `to` of a transparent picture `width` by `height`, each new pixel
/// the average of the ones it covers, weighed by how much of each it covers.
pub fn fitted(image: &Rgba, from: Area, width: usize, height: usize, to: Area) -> Rgba {
    let mut out = Rgba::transparent(width, height);
    if from.width == 0 || from.height == 0 || to.width == 0 || to.height == 0 {
        return out;
    }
    let step_x = from.width as f64 / to.width as f64;
    let step_y = from.height as f64 / to.height as f64;
    for ty in 0..to.height.min(height.saturating_sub(to.y)) {
        let (y0, y1) = (ty as f64 * step_y, (ty + 1) as f64 * step_y);
        for tx in 0..to.width.min(width.saturating_sub(to.x)) {
            let (x0, x1) = (tx as f64 * step_x, (tx + 1) as f64 * step_x);
            // Colours add up weighed by their alpha, so a transparent pixel adds none.
            let mut sum = [0f64; 4];
            let mut covered = 0f64;
            for sy in y0.floor() as usize..(y1.ceil() as usize).min(from.height) {
                let dy = (y1.min(sy as f64 + 1.0) - y0.max(sy as f64)).max(0.0);
                for sx in x0.floor() as usize..(x1.ceil() as usize).min(from.width) {
                    let dx = (x1.min(sx as f64 + 1.0) - x0.max(sx as f64)).max(0.0);
                    let weight = dx * dy;
                    let pixel = image.at(from.x + sx, from.y + sy);
                    let alpha = f64::from(pixel[3]) / 255.0;
                    for (channel, value) in sum.iter_mut().zip(&pixel[..3]) {
                        *channel += f64::from(*value) * alpha * weight;
                    }
                    sum[3] += alpha * weight;
                    covered += weight;
                }
            }
            if covered == 0.0 || sum[3] == 0.0 {
                continue;
            }
            let start = ((to.y + ty) * width + to.x + tx) * PIXEL;
            let pixel = &mut out.pixels[start..start + PIXEL];
            for (channel, value) in pixel.iter_mut().zip(&sum[..3]) {
                *channel = (value / sum[3]).round().clamp(0.0, 255.0) as u8;
            }
            pixel[3] = (sum[3] / covered * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkered(width: usize, height: usize) -> Rgba {
        let mut image = Rgba::transparent(width, height);
        for (at, pixel) in image.pixels.chunks_mut(PIXEL).enumerate() {
            let (x, y) = (at % width, at / width);
            if (x + y) % 2 == 0 {
                pixel.copy_from_slice(&[232, 163, 61, 255]);
            }
        }
        image
    }

    #[test]
    fn a_picture_reads_back_as_it_was_written() {
        let image = checkered(7, 5);
        assert_eq!(decode(&encode(&image)), Some(image));
    }

    #[test]
    fn every_filter_the_generator_may_use_reads_back() {
        // The generator's files use all five filters; each of its pictures reads at its size.
        let bytes = include_bytes!("../../assets/mascots/pictures/a-council-hydra-18x42.png");
        let image = decode(bytes).expect("an RGBA picture");
        assert_eq!((image.width, image.height), (162, 126));
        assert!(image.pixels.chunks(PIXEL).any(|pixel| pixel[3] == 255));
        assert!(image.pixels.chunks(PIXEL).any(|pixel| pixel[3] == 0));
    }

    #[test]
    fn other_pngs_are_refused() {
        assert_eq!(decode(b"GIF89a"), None);
        let mut grey = encode(&checkered(2, 2));
        // Colour type 0, grey, in the header.
        grey[25] = 0;
        assert_eq!(decode(&grey), None);
    }

    #[test]
    fn fitting_halves_a_picture_into_the_middle_of_a_box() {
        let image = checkered(4, 4);
        let whole = Area {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        };
        let middle = Area {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        };
        let out = fitted(&image, whole, 4, 2, middle);
        // Each new pixel covers two lit pixels and two clear ones: amber at half its alpha.
        assert_eq!(out.at(1, 0), [232, 163, 61, 128]);
        assert_eq!(out.at(2, 1), [232, 163, 61, 128]);
        assert_eq!(out.at(0, 0), [0, 0, 0, 0]);
        assert_eq!(out.at(3, 1), [0, 0, 0, 0]);
    }
}
