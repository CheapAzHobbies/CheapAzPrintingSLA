//! CTB preview images, which the printer shows when choosing a file.
//!
//! Raster order, RGB565, little-endian, with one change: the low bit of green
//! is taken away to act as a run flag. A pixel with that bit set is followed
//! by a second word of the form `0x3nnn` whose low twelve bits say how many
//! *further* copies of the pixel follow, so `0x3000` is a run of one and means
//! the same thing as not setting the flag at all.
//!
//! Runs cross rows freely: reaching the right edge simply continues on the
//! left of the next line. That is unlike the layer encoding in GOO, where a
//! run may not cross a row, and the difference is the sort of thing that is
//! only obvious once it has cost an afternoon.

use crate::error::{FormatError, Result};

/// The sizes found in files from the proprietary slicer. They are not the same
/// aspect ratio, so one cannot simply be scaled from the other.
pub const LARGE: (u32, u32) = (400, 300);
pub const SMALL: (u32, u32) = (200, 125);

const RUN_FLAG: u16 = 1 << 5;
/// Every run length word has this in its top nibble.
const RUN_TAG: u16 = 0x3000;
const RUN_MAX: u16 = 0x0FFF;

fn pack(r: u8, g: u8, b: u8) -> u16 {
    // Green keeps five bits, not six: the sixth is the run flag.
    (((r as u16) >> 3) << 11) | (((g as u16) >> 3) << 6) | ((b as u16) >> 3)
}

fn unpack(v: u16) -> (u8, u8, u8) {
    let r = ((v >> 11) & 0x1F) as u8;
    let g = ((v >> 6) & 0x1F) as u8;
    let b = (v & 0x1F) as u8;
    // Repeat the top bits into the bottom so full scale stays full scale.
    let widen = |c: u8| (c << 3) | (c >> 2);
    (widen(r), widen(g), widen(b))
}

/// Encode `rgb` (three bytes per pixel, `width * height` of them).
pub fn encode(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let count = (width * height) as usize;
    let mut out = Vec::with_capacity(count);
    let mut i = 0usize;
    while i < count {
        let at = i * 3;
        let value = pack(rgb[at], rgb[at + 1], rgb[at + 2]);
        let mut run = 1usize;
        while i + run < count {
            let next = (i + run) * 3;
            if pack(rgb[next], rgb[next + 1], rgb[next + 2]) != value {
                break;
            }
            run += 1;
        }
        i += run;

        // A run word can carry 0xFFF further copies, so a longer stretch is
        // written as several runs.
        while run > 0 {
            let here = run.min(RUN_MAX as usize + 1);
            if here == 1 {
                out.extend_from_slice(&value.to_le_bytes());
            } else {
                out.extend_from_slice(&(value | RUN_FLAG).to_le_bytes());
                out.extend_from_slice(&(RUN_TAG | (here as u16 - 1)).to_le_bytes());
            }
            run -= here;
        }
    }
    out
}

/// Decode into three bytes per pixel. `expected` is `width * height`.
pub fn decode(data: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected * 3);
    let mut i = 0usize;
    while i + 1 < data.len() {
        let value = u16::from_le_bytes([data[i], data[i + 1]]);
        i += 2;
        let mut run = 1usize;
        if value & RUN_FLAG != 0 {
            if i + 1 >= data.len() {
                return Err(FormatError::LayerDecode(
                    "a preview run length is cut off at the end of the image".into(),
                )
                .into());
            }
            let length = u16::from_le_bytes([data[i], data[i + 1]]);
            i += 2;
            run = (length & RUN_MAX) as usize + 1;
        }
        if out.len() / 3 + run > expected {
            return Err(FormatError::LayerDecode(format!(
                "preview runs describe more than the {expected} pixels it holds"
            ))
            .into());
        }
        let (r, g, b) = unpack(value);
        for _ in 0..run {
            out.extend_from_slice(&[r, g, b]);
        }
    }
    if out.len() != expected * 3 {
        return Err(FormatError::LayerDecode(format!(
            "preview runs describe {} pixels but it holds {expected}",
            out.len() / 3
        ))
        .into());
    }
    Ok(out)
}

/// Scale an arbitrary thumbnail into `w` by `h`, nearest neighbour.
///
/// Black when there is nothing to scale, rather than inventing a picture.
pub fn fit(thumb: Option<&crate::model::Thumbnail>, w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 3) as usize];
    let Some(t) = thumb else { return out };
    if t.width == 0 || t.height == 0 || t.rgb.len() < (t.width * t.height * 3) as usize {
        return out;
    }
    for y in 0..h {
        let sy = y * t.height / h;
        for x in 0..w {
            let sx = x * t.width / w;
            let si = ((sy * t.width + sx) * 3) as usize;
            let di = ((y * w + x) * 3) as usize;
            out[di..di + 3].copy_from_slice(&t.rgb[si..si + 3]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(rgb: &[u8], w: u32, h: u32) {
        let encoded = encode(rgb, w, h);
        let back = decode(&encoded, (w * h) as usize).expect("decode");
        // RGB565 keeps five bits per channel, so compare against what survives
        // that rather than against the original.
        let expected: Vec<u8> = rgb
            .chunks_exact(3)
            .flat_map(|p| {
                let (r, g, b) = unpack(pack(p[0], p[1], p[2]));
                [r, g, b]
            })
            .collect();
        assert_eq!(back, expected);
    }

    #[test]
    fn a_flat_image_survives() {
        round_trip(&vec![0u8; 40 * 30 * 3], 40, 30);
        round_trip(&vec![255u8; 40 * 30 * 3], 40, 30);
    }

    #[test]
    fn a_pattern_with_runs_and_singles_survives() {
        let (w, h) = (64u32, 16u32);
        let mut rgb = Vec::new();
        for i in 0..w * h {
            if i % 17 == 0 {
                rgb.extend_from_slice(&[255, 0, 0]);
            } else if (i / 8) % 2 == 0 {
                rgb.extend_from_slice(&[0, 0, 0]);
            } else {
                rgb.extend_from_slice(&[16, 200, 96]);
            }
        }
        round_trip(&rgb, w, h);
    }

    #[test]
    fn a_run_longer_than_one_word_can_hold_is_split() {
        // 0xFFF is the most a single run word carries, so a flat image with
        // more pixels than that must still come back whole.
        let (w, h) = (200u32, 125u32);
        assert!(w * h > 0x0FFF, "the test needs a run longer than one word");
        round_trip(&vec![7u8; (w * h * 3) as usize], w, h);
    }

    #[test]
    fn the_run_flag_does_not_leak_into_green() {
        // Green gives up its low bit. If the flag were read as colour, a run
        // of green would come back a shade brighter than a single pixel of it.
        let single = decode(&encode(&[0, 200, 0, 1, 1, 1], 2, 1), 2).unwrap();
        let run = decode(&encode(&[0, 200, 0, 0, 200, 0], 2, 1), 2).unwrap();
        assert_eq!(single[..3], run[..3], "a run changed the colour");
    }

    #[test]
    fn both_documented_sizes_round_trip() {
        for (w, h) in [LARGE, SMALL] {
            let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i % 251) as u8).collect();
            round_trip(&rgb, w, h);
        }
    }

    #[test]
    fn a_truncated_preview_is_an_error_not_a_panic() {
        let encoded = encode(&vec![0u8; 100 * 3], 10, 10);
        assert!(decode(&encoded[..encoded.len() - 1], 100).is_err());
        assert!(decode(&encoded, 1000).is_err());
    }

    #[test]
    fn scaling_a_missing_thumbnail_gives_black_rather_than_noise() {
        let out = fit(None, 8, 8);
        assert_eq!(out, vec![0u8; 8 * 8 * 3]);
    }
}
