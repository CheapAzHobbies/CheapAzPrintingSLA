//! Run-length encoding for GOO layer images.
//!
//! A control byte is `aabbcccc`:
//!
//! ```text
//! aa    chunk type    00 black (0x00)   01 grey value follows
//!                     10 difference     11 white (0xFF)
//! bb    length size   00 4-bit   01 12-bit   10 20-bit   11 28-bit
//! cccc  low 4 bits of the run length
//! ```
//!
//! Longer lengths continue into following bytes:
//!
//! ```text
//!  4-bit  len = c
//! 12-bit  len = c + (b1 << 4)
//! 20-bit  len = c + (b1 << 12) + (b2 << 4)
//! 28-bit  len = c + (b1 << 20) + (b2 << 12) + (b3 << 4)
//! ```
//!
//! The encoder here emits only the black, white and grey chunk types. The
//! difference type would shave a little size off gradient-heavy layers, but
//! resin masks are overwhelmingly flat black and flat white, and leaving it
//! out removes a class of encoder bug for very little cost.
//!
//! Every pixel must be covered. A short final run leaves whatever was
//! previously in the printer's buffer on screen, which is why the encoder
//! tracks its own pixel total and the caller checks it.

/// Marker byte that precedes the encoded payload of every layer.
pub const IMAGE_MAGIC: u8 = 0x55;

const TYPE_BLACK: u8 = 0b00 << 6;
const TYPE_GREY: u8 = 0b01 << 6;
const TYPE_WHITE: u8 = 0b11 << 6;

/// Largest run expressible in one chunk, by length-size selector.
const MAX_4: u32 = 0x0F;
const MAX_12: u32 = 0x0FFF;
const MAX_20: u32 = 0x000F_FFFF;
const MAX_28: u32 = 0x0FFF_FFFF;

/// Append one run of `value` repeated `len` times.
///
/// Runs longer than a single chunk can express are split, which is why this
/// loops rather than assuming one chunk is enough.
fn push_run(out: &mut Vec<u8>, value: u8, mut len: u32) {
    while len > 0 {
        let take = len.min(MAX_28);
        let ty = match value {
            0x00 => TYPE_BLACK,
            0xFF => TYPE_WHITE,
            _ => TYPE_GREY,
        };
        let head_index = out.len();
        out.push(0); // control byte, filled in below
                     // A grey chunk carries its value in the byte immediately after the
                     // control byte, ahead of the length bytes. Putting it after them
                     // instead desynchronises every chunk that follows, which shows up
                     // only on layers that contain grey pixels at all.
        if ty == TYPE_GREY {
            out.push(value);
        }
        let size_bits = if take <= MAX_4 {
            0b00
        } else if take <= MAX_12 {
            out.push(((take >> 4) & 0xFF) as u8);
            0b01
        } else if take <= MAX_20 {
            out.push(((take >> 12) & 0xFF) as u8);
            out.push(((take >> 4) & 0xFF) as u8);
            0b10
        } else {
            out.push(((take >> 20) & 0xFF) as u8);
            out.push(((take >> 12) & 0xFF) as u8);
            out.push(((take >> 4) & 0xFF) as u8);
            0b11
        };
        out[head_index] = ty | (size_bits << 4) | (take as u8 & 0x0F);
        len -= take;
    }
}

/// Encode one layer's pixels.
///
/// `width` matters: the format walks the image row by row, and a run may not
/// continue past the end of a row into the next one. Encoders that ignore
/// this produce files their own decoder will happily read back and a printer
/// will reject. UVtools reports it as "RLE run exceeds the image bounds".
///
/// Returns the payload without the leading magic byte or trailing checksum,
/// and the number of pixels it accounts for so the caller can confirm the
/// whole panel is covered.
pub fn encode(pixels: &[u8], width: u32) -> (Vec<u8>, u64) {
    let mut out = Vec::with_capacity(pixels.len() / 32 + 16);
    let mut covered: u64 = 0;
    let row = if width == 0 {
        pixels.len()
    } else {
        width as usize
    };

    for line in pixels.chunks(row) {
        let mut i = 0usize;
        while i < line.len() {
            let value = line[i];
            let mut run = 1u32;
            while (i + run as usize) < line.len() && line[i + run as usize] == value && run < MAX_28
            {
                run += 1;
            }
            push_run(&mut out, value, run);
            covered += run as u64;
            i += run as usize;
        }
    }
    (out, covered)
}

/// Checksum of an encoded payload: the bitwise complement of the byte sum.
///
/// Verified against layer 0 of a real Elegoo file, where the stored checksum
/// is 0x8E and the complement of the payload sum is 0x8E.
pub fn checksum(payload: &[u8]) -> u8 {
    !payload.iter().fold(0u8, |a, b| a.wrapping_add(*b))
}

/// Decode a payload back to pixels. Used to verify what the encoder produced.
pub fn decode(payload: &[u8], expected_pixels: usize) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(expected_pixels);
    let mut i = 0usize;
    let mut previous: u8 = 0;
    while i < payload.len() {
        let head = payload[i];
        i += 1;
        let ty = head >> 6;

        // The difference type reuses bits 5-4 for its own purpose rather than
        // as a length selector, so it is handled before the length is read.
        if ty == 0b10 {
            let subtract = head & 0b0010_0000 != 0;
            let diff = head & 0x0F;
            let len: u32 = if head & 0b0001_0000 != 0 {
                if i >= payload.len() {
                    return Err("difference chunk has no length byte".into());
                }
                let l = payload[i] as u32;
                i += 1;
                l
            } else {
                1
            };
            let value = if subtract {
                previous.saturating_sub(diff)
            } else {
                previous.saturating_add(diff)
            };
            if out.len() as u64 + len as u64 > expected_pixels as u64 {
                return Err(format!("runs describe more than {expected_pixels} pixels"));
            }
            out.extend(std::iter::repeat_n(value, len as usize));
            previous = value;
            continue;
        }

        let size = (head >> 4) & 0b11;

        // The grey value precedes the length bytes.
        let mut grey: Option<u8> = None;
        if ty == 0b01 {
            if i >= payload.len() {
                return Err("grey chunk has no value byte".into());
            }
            grey = Some(payload[i]);
            i += 1;
        }

        let mut len = (head & 0x0F) as u32;
        let extra = size as usize;
        if i + extra > payload.len() {
            return Err("run length runs past the end of the payload".into());
        }
        match size {
            0 => {}
            1 => {
                len += (payload[i] as u32) << 4;
                i += 1;
            }
            2 => {
                len += ((payload[i] as u32) << 12) + ((payload[i + 1] as u32) << 4);
                i += 2;
            }
            _ => {
                len += ((payload[i] as u32) << 20)
                    + ((payload[i + 1] as u32) << 12)
                    + ((payload[i + 2] as u32) << 4);
                i += 3;
            }
        }
        let value = match ty {
            0b00 => 0x00,
            0b11 => 0xFF,
            0b01 => grey.expect("read above"),
            _ => unreachable!("difference chunks are handled above"),
        };
        previous = value;
        if out.len() as u64 + len as u64 > expected_pixels as u64 {
            return Err(format!("runs describe more than {expected_pixels} pixels"));
        }
        out.extend(std::iter::repeat_n(value, len as usize));
    }
    Ok(out)
}
