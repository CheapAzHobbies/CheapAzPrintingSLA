//! Chitubox CTB run-length encoding.
//!
//! One byte carries a seven-bit grey value in its low bits and a flag in its
//! top bit saying whether a run length follows. A run of one is just the byte.
//! Longer runs put the length in one to four following bytes, and the top bits
//! of the first of those say how many there are:
//!
//! | first byte | length is | bits available |
//! |---|---|---|
//! | `0xxxxxxx` | that byte | 7 |
//! | `10xxxxxx` | this and one more | 14 |
//! | `110xxxxx` | this and two more | 21 |
//! | `1110xxxx` | this and three more | 28 |
//!
//! Grey is stored in seven bits, so an eight-bit image loses its lowest bit on
//! the way in. That is the format's own limit rather than a shortcut here:
//! CTB has nowhere to put the missing bit.

use crate::error::{FormatError, Result};

/// Seven-bit grey to eight. 0x7F must come back as 0xFF or white would decode
/// slightly grey and every layer would be a shade off.
#[inline]
fn expand(seven: u8) -> u8 {
    (seven << 1) | (seven >> 6)
}

/// Eight-bit grey to the seven the format stores.
#[inline]
fn narrow(eight: u8) -> u8 {
    eight >> 1
}

/// Decode one layer. `expected` is how many pixels the layer must contain.
///
/// Refuses to produce more pixels than expected rather than growing a buffer
/// from a length field in an untrusted file.
pub fn decode(data: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let mut i = 0usize;
    while i < data.len() {
        let code = data[i];
        i += 1;
        let grey = expand(code & 0x7F);
        let run = if code & 0x80 == 0 {
            1usize
        } else {
            let first = *data.get(i).ok_or_else(|| {
                FormatError::LayerDecode("run length is cut off at the end of the layer".into())
            })?;
            i += 1;
            // The count of extra bytes is written in unary in the top bits.
            let (extra, mut value) = match first {
                _ if first & 0x80 == 0 => (0, (first & 0x7F) as usize),
                _ if first & 0xC0 == 0x80 => (1, (first & 0x3F) as usize),
                _ if first & 0xE0 == 0xC0 => (2, (first & 0x1F) as usize),
                _ if first & 0xF0 == 0xE0 => (3, (first & 0x0F) as usize),
                _ => {
                    return Err(FormatError::LayerDecode(format!(
                        "run length byte {first:#04x} is not a valid length prefix"
                    ))
                    .into())
                }
            };
            for _ in 0..extra {
                let b = *data.get(i).ok_or_else(|| {
                    FormatError::LayerDecode("run length is cut off at the end of the layer".into())
                })?;
                i += 1;
                value = (value << 8) | b as usize;
            }
            value
        };

        if run == 0 {
            return Err(FormatError::LayerDecode("a run covers no pixels".into()).into());
        }
        if out.len() + run > expected {
            return Err(FormatError::LayerDecode(format!(
                "runs describe more than the {expected} pixels the layer holds"
            ))
            .into());
        }
        out.extend(std::iter::repeat_n(grey, run));
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

/// Encode one layer, returning the payload and the number of pixels covered.
///
/// The pixel count is returned so the caller can check the whole panel was
/// described. A layer that stops early leaves whatever the printer had in its
/// buffer on the screen for that exposure.
pub fn encode(pixels: &[u8]) -> (Vec<u8>, u64) {
    let mut out = Vec::with_capacity(pixels.len() / 8);
    let mut covered = 0u64;
    let mut i = 0usize;
    while i < pixels.len() {
        let seven = narrow(pixels[i]);
        let mut run = 1usize;
        while i + run < pixels.len() && narrow(pixels[i + run]) == seven {
            run += 1;
        }
        i += run;
        covered += run as u64;
        write_run(&mut out, seven, run);
    }
    (out, covered)
}

fn write_run(out: &mut Vec<u8>, seven: u8, run: usize) {
    let mut left = run;
    while left > 0 {
        // The longest a single run can express is 28 bits.
        let take = left.min(0x0FFF_FFFF);
        if take == 1 {
            out.push(seven);
        } else {
            out.push(seven | 0x80);
            if take <= 0x7F {
                out.push(take as u8);
            } else if take <= 0x3FFF {
                out.push(((take >> 8) as u8) | 0x80);
                out.push(take as u8);
            } else if take <= 0x1F_FFFF {
                out.push(((take >> 16) as u8) | 0xC0);
                out.push((take >> 8) as u8);
                out.push(take as u8);
            } else {
                out.push(((take >> 24) as u8) | 0xE0);
                out.push((take >> 16) as u8);
                out.push((take >> 8) as u8);
                out.push(take as u8);
            }
        }
        left -= take;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encoding then decoding must return exactly what went in, once the
    /// eighth bit the format cannot hold is accounted for.
    fn round_trip(pixels: &[u8]) {
        let (payload, covered) = encode(pixels);
        assert_eq!(covered, pixels.len() as u64, "every pixel must be covered");
        let back = decode(&payload, pixels.len()).expect("decode");
        let expected: Vec<u8> = pixels.iter().map(|&p| expand(narrow(p))).collect();
        assert_eq!(back, expected);
    }

    #[test]
    fn a_single_pixel_survives() {
        round_trip(&[0]);
        round_trip(&[255]);
        round_trip(&[128]);
    }

    #[test]
    fn runs_of_every_length_class_survive() {
        // One either side of each boundary in the length encoding.
        for len in [1usize, 2, 0x7F, 0x80, 0x3FFF, 0x4000, 0x1F_FFFF, 0x20_0000] {
            round_trip(&vec![0xFFu8; len]);
            round_trip(&vec![0x00u8; len]);
        }
    }

    #[test]
    fn black_and_white_alternating_survives() {
        let pixels: Vec<u8> = (0..1000)
            .map(|i| if i % 2 == 0 { 0 } else { 255 })
            .collect();
        round_trip(&pixels);
    }

    #[test]
    fn grey_ramp_survives_to_seven_bits() {
        let pixels: Vec<u8> = (0..=255u8).collect();
        round_trip(&pixels);
    }

    #[test]
    fn white_stays_white() {
        // 0x7F must expand back to 0xFF. Without that every white pixel comes
        // out at 254 and a whole print is a shade dim.
        assert_eq!(expand(narrow(255)), 255);
        assert_eq!(expand(narrow(0)), 0);
    }

    #[test]
    fn a_truncated_run_length_is_an_error() {
        // A run flag with nothing after it.
        let err = decode(&[0x80], 10).expect_err("must fail");
        assert!(err.to_string().contains("cut off"), "{err}");
    }

    #[test]
    fn runs_may_not_describe_more_than_the_layer_holds() {
        let (payload, _) = encode(&vec![0xFF; 500]);
        let err = decode(&payload, 100).expect_err("must fail");
        assert!(err.to_string().contains("more than"), "{err}");
    }

    #[test]
    fn runs_must_describe_the_whole_layer() {
        let (payload, _) = encode(&vec![0xFF; 100]);
        let err = decode(&payload, 500).expect_err("must fail");
        assert!(err.to_string().contains("but the layer holds"), "{err}");
    }

    #[test]
    fn a_zero_length_run_is_an_error() {
        // 0x80 says "run follows", 0x00 says the run is zero pixels long. A
        // file full of those would otherwise loop without producing anything.
        let err = decode(&[0x80, 0x00], 10).expect_err("must fail");
        assert!(err.to_string().contains("no pixels"), "{err}");
    }

    #[test]
    fn an_empty_layer_is_only_valid_when_nothing_is_expected() {
        assert_eq!(decode(&[], 0).expect("empty"), Vec::<u8>::new());
        assert!(decode(&[], 10).is_err());
    }
}
