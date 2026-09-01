//! Phrozen PHZ run-length encoding, the "7a" scheme.
//!
//! Simpler than CTB's and less efficient. A byte with its top bit set is a
//! pixel, seven bits of grey in the rest of it. A byte with its top bit clear
//! is a count of *further* copies of the pixel before it. So `0x80` is one
//! black pixel and `0x80 0x7F` is a hundred and twenty-eight of them.
//!
//! Two habits of the proprietary encoder are copied here rather than improved
//! on. Repeat counts never exceed `0x7D`, though nothing in the format says
//! `0x7E` and `0x7F` are reserved; and runs stop at each half scanline instead
//! of carrying across the image. Both make files perhaps a tenth larger than
//! they need to be. Neither is known to be required, and this is a format for
//! a printer nobody here owns, so it matches what the printer is known to
//! accept rather than what ought to work.

use crate::error::{FormatError, Result};

/// The most further-copies one count byte carries, as the proprietary encoder
/// uses it.
const MAX_REPEAT: usize = 0x7D;

/// Seven-bit grey to eight, so full scale stays full scale.
#[inline]
fn expand(seven: u8) -> u8 {
    (seven << 1) | (seven >> 6)
}

#[inline]
fn narrow(eight: u8) -> u8 {
    eight >> 1
}

/// Decode one layer into `expected` pixels.
pub fn decode(data: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let mut last: Option<u8> = None;
    for (i, &byte) in data.iter().enumerate() {
        if byte & 0x80 != 0 {
            let grey = expand(byte & 0x7F);
            last = Some(grey);
            if out.len() == expected {
                return Err(too_many(expected));
            }
            out.push(grey);
        } else {
            let Some(grey) = last else {
                return Err(FormatError::LayerDecode(format!(
                    "a repeat count at byte {i} has no pixel before it"
                ))
                .into());
            };
            let run = byte as usize;
            if out.len() + run > expected {
                return Err(too_many(expected));
            }
            out.extend(std::iter::repeat_n(grey, run));
        }
    }
    if out.len() != expected {
        return Err(FormatError::LayerDecode(format!(
            "runs describe {} pixels but the layer holds {expected}",
            out.len()
        ))
        .into());
    }
    Ok(out)
}

fn too_many(expected: usize) -> crate::error::Error {
    FormatError::LayerDecode(format!(
        "runs describe more than the {expected} pixels the layer holds"
    ))
    .into()
}

/// Encode one layer, returning the payload and the pixels it covers.
///
/// `width` is needed because runs stop at each half scanline; pass 0 to let
/// them run freely, which the format permits but the printer has not been
/// observed to be given.
pub fn encode(pixels: &[u8], width: u32) -> (Vec<u8>, u64) {
    let mut out = Vec::with_capacity(pixels.len() / 4);
    let mut covered = 0u64;

    // Half a scanline, or the whole image when no width is given.
    let stripe = if width >= 2 {
        (width / 2) as usize
    } else {
        pixels.len().max(1)
    };

    let mut start = 0usize;
    while start < pixels.len() {
        let end = (start + stripe).min(pixels.len());
        let mut i = start;
        while i < end {
            let seven = narrow(pixels[i]);
            let mut run = 1usize;
            while i + run < end && narrow(pixels[i + run]) == seven {
                run += 1;
            }
            i += run;
            covered += run as u64;

            out.push(0x80 | seven);
            let mut left = run - 1;
            while left > 0 {
                let take = left.min(MAX_REPEAT);
                out.push(take as u8);
                left -= take;
            }
        }
        start = end;
    }
    (out, covered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(pixels: &[u8], width: u32) {
        let (payload, covered) = encode(pixels, width);
        assert_eq!(covered, pixels.len() as u64, "every pixel must be covered");
        let back = decode(&payload, pixels.len()).expect("decode");
        let expected: Vec<u8> = pixels.iter().map(|&p| expand(narrow(p))).collect();
        assert_eq!(back, expected);
    }

    #[test]
    fn the_documented_examples_decode() {
        // 0x80 is one black pixel; 0x80 0x7F is a hundred and twenty-eight.
        assert_eq!(decode(&[0x80], 1).unwrap(), vec![0]);
        assert_eq!(decode(&[0x80, 0x7F], 128).unwrap(), vec![0u8; 128]);
        // Top bit set, seven bits of grey: 0xFF is white.
        assert_eq!(decode(&[0xFF], 1).unwrap(), vec![255]);
    }

    #[test]
    fn flat_layers_survive() {
        round_trip(&vec![0u8; 64 * 32], 64);
        round_trip(&vec![254u8; 64 * 32], 64);
    }

    #[test]
    fn runs_longer_than_one_count_byte_survive() {
        for len in [1usize, 2, 0x7D, 0x7E, 0x7F, 0x80, 1000, 100_000] {
            round_trip(&vec![0xFEu8; len], 0);
        }
    }

    #[test]
    fn a_grey_ramp_survives_to_seven_bits() {
        let pixels: Vec<u8> = (0..=255u8).collect();
        round_trip(&pixels, 0);
    }

    #[test]
    fn alternating_pixels_survive() {
        let pixels: Vec<u8> = (0..1000)
            .map(|i| if i % 2 == 0 { 0 } else { 254 })
            .collect();
        round_trip(&pixels, 40);
    }

    #[test]
    fn no_repeat_count_exceeds_what_the_printer_is_given() {
        let (payload, _) = encode(&vec![0u8; 10_000], 0);
        for &b in &payload {
            if b & 0x80 == 0 {
                assert!(
                    b as usize <= MAX_REPEAT,
                    "repeat count {b:#04x} is too large"
                );
            }
        }
    }

    #[test]
    fn runs_stop_at_each_half_scanline() {
        // A flat image 64 wide: every run should cover at most 32 pixels, so a
        // row is two runs rather than one.
        let (payload, _) = encode(&vec![0u8; 64 * 4], 64);
        let pixels = payload.iter().filter(|b| *b & 0x80 != 0).count();
        assert_eq!(pixels, 8, "four rows of 64 should be eight half-scanlines");
    }

    #[test]
    fn a_repeat_before_any_pixel_is_an_error() {
        let err = decode(&[0x05], 10).expect_err("must fail");
        assert!(err.to_string().contains("no pixel before it"), "{err}");
    }

    #[test]
    fn runs_may_not_overrun_the_layer() {
        let (payload, _) = encode(&vec![0u8; 500], 0);
        assert!(decode(&payload, 100).is_err());
        assert!(decode(&payload, 900).is_err());
    }

    #[test]
    fn an_empty_layer_is_only_valid_when_nothing_is_expected() {
        assert_eq!(decode(&[], 0).unwrap(), Vec::<u8>::new());
        assert!(decode(&[], 5).is_err());
    }
}
