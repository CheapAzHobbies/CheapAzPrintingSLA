//! Elegoo GOO (§12, §13).
//!
//! Layout is fixed-position and big-endian throughout: a header carrying the
//! print parameters and two preview images, then one record per layer holding
//! that layer's motion and exposure settings followed by its run-length
//! encoded bitmap, then an eleven byte trailer.
//!
//! The offsets were taken from Elegoo's published specification together with
//! the ImHex pattern from the mslicer project, and checked against a real
//! Elegoo file: the header's own `offset_of_layer_content` field agrees with
//! the position computed by walking the fields, and the stored layer checksum
//! agrees with the complement of the payload byte sum. See the credits in the
//! README.

use super::goo_rle;
use crate::error::{Error, FormatError, Result};
use crate::format::{
    Capabilities, Confidence, Detection, FormatHandler, FormatInfo, OpenedFile,
};
use crate::layers::LayerProvider;
use crate::limits;
use crate::model::*;
use std::io::{BufWriter, Write};
use std::path::Path;

pub const ID: &str = "goo";

/// `V3.0` then the fixed magic tag.
const VERSION: &[u8; 4] = b"V3.0";
const MAGIC: [u8; 8] = [0x07, 0x00, 0x00, 0x00, 0x44, 0x4C, 0x50, 0x00];
const ENDING: [u8; 11] = [0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x44, 0x4C, 0x50, 0x00];
const DELIM: [u8; 2] = [0x0D, 0x0A];

const SMALL_W: u32 = 116;
const SMALL_H: u32 = 116;
const BIG_W: u32 = 290;
const BIG_H: u32 = 290;

static INFO: FormatInfo = FormatInfo {
    id: ID,
    name: "Elegoo GOO",
    extension: "goo",
    aliases: &[],
    description: "The format Elegoo's Saturn and Mars printers read. Layer images are \
                  run-length encoded, so files stay small, and Elegoo publish the \
                  specification, which is rarer than it should be.",
    limitations: &[
        "Preview images are fixed at 116x116 and 290x290 pixels",
        "Stores no material name",
    ],
    capabilities: Capabilities {
        reads: false, // reading lands in a later phase; writing is what unblocks conversion
        writes: true,
        per_layer_exposure: true,
        per_layer_lift: true,
        thumbnails: true,
        max_thumbnails: 2,
        print_time: true,
        material_volume: true,
        machine_name: true,
    },
};

pub struct GooHandler;

/// Fixed-width text field, NUL padded and NUL terminated.
fn text(out: &mut Vec<u8>, s: &str, width: usize) {
    let bytes = s.as_bytes();
    let take = bytes.len().min(width.saturating_sub(1));
    out.extend_from_slice(&bytes[..take]);
    out.extend(std::iter::repeat(0u8).take(width - take));
}

fn be_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn be_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn be_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Scale a thumbnail into an RGB565 buffer of the given size.
///
/// Nearest neighbour: these are 116 and 290 pixel previews on a printer's
/// screen, and the difference from anything better is not visible.
fn preview_rgb565(thumb: Option<&Thumbnail>, w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 2) as usize];
    let Some(t) = thumb else {
        return out; // no preview available: leave it black rather than invent one
    };
    if t.width == 0 || t.height == 0 || t.rgb.len() < (t.width * t.height * 3) as usize {
        return out;
    }
    for y in 0..h {
        let sy = y * t.height / h;
        for x in 0..w {
            let sx = x * t.width / w;
            let si = ((sy * t.width + sx) * 3) as usize;
            let (r, g, b) = (t.rgb[si], t.rgb[si + 1], t.rgb[si + 2]);
            let v: u16 = (((r as u16) >> 3) << 11) | (((g as u16) >> 2) << 5) | ((b as u16) >> 3);
            let di = ((y * w + x) * 2) as usize;
            out[di..di + 2].copy_from_slice(&v.to_be_bytes());
        }
    }
    out
}

/// Byte length of one layer record before its image payload.
const LAYER_PREAMBLE: usize = 2 + 15 * 4 + 2 + 2;

impl FormatHandler for GooHandler {
    fn info(&self) -> &'static FormatInfo {
        &INFO
    }

    fn detect(&self, path: &Path, data: &[u8]) -> Detection {
        let named = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("goo"))
            .unwrap_or(false);
        if data.len() >= 12 && &data[..4] == VERSION && data[4..12] == MAGIC {
            Detection {
                format_id: ID,
                confidence: Confidence::High,
                reason: "Begins with the GOO version marker and magic tag".into(),
            }
        } else if named {
            Detection {
                format_id: ID,
                confidence: Confidence::Low,
                reason: "Named like a GOO file, but the header does not match".into(),
            }
        } else {
            Detection {
                format_id: ID,
                confidence: Confidence::None,
                reason: "No GOO header".into(),
            }
        }
    }

    fn validate(&self, _path: &Path) -> Result<Vec<String>> {
        Err(FormatError::Other(
            "reading GOO files is not implemented yet; CheapAzSLA can write them but not open them"
                .into(),
        )
        .into())
    }

    fn open(&self, _path: &Path) -> Result<OpenedFile> {
        Err(FormatError::Other(
            "reading GOO files is not implemented yet; CheapAzSLA can write them but not open them"
                .into(),
        )
        .into())
    }

    fn write(&self, path: &Path, print: &PrintFile, layers: &dyn LayerProvider) -> Result<()> {
        let count = layers.layer_count();
        if count == 0 {
            return Err(FormatError::Other("there are no layers to write".into()).into());
        }
        limits::check_layer_count(count as u64)?;
        let (w, h) = layers.dimensions();
        limits::check_resolution(w, h)?;
        if w > u16::MAX as u32 || h > u16::MAX as u32 {
            return Err(FormatError::InvalidValue {
                field: "resolution".into(),
                value: format!("{w}x{h}"),
                reason: "GOO stores resolution in 16 bits".into(),
            }
            .into());
        }

        let e = &print.exposure;
        let l = &print.lift;
        let bottom_layers = e.bottom_layers.unwrap_or(0);
        let bottom_exposure = e.bottom_exposure_s.unwrap_or(e.exposure_s);

        let mut head: Vec<u8> = Vec::with_capacity(200_000);
        head.extend_from_slice(VERSION);
        head.extend_from_slice(&MAGIC);
        text(&mut head, "CheapAzSLA", 32);
        text(&mut head, crate::VERSION, 24);
        text(&mut head, &now_string(), 24);
        text(&mut head, print.machine_name.as_deref().unwrap_or(""), 32);
        text(&mut head, "DLP", 32);
        text(&mut head, "CheapAzSLA", 32);
        be_u16(&mut head, 1); // anti aliasing level
        be_u16(&mut head, 1); // grey level
        be_u16(&mut head, 0); // blur level

        let small = print.thumbnails.iter().min_by_key(|t| t.pixel_count());
        let big = print.thumbnails.iter().max_by_key(|t| t.pixel_count());
        head.extend_from_slice(&preview_rgb565(small, SMALL_W, SMALL_H));
        head.extend_from_slice(&DELIM);
        head.extend_from_slice(&preview_rgb565(big, BIG_W, BIG_H));
        head.extend_from_slice(&DELIM);

        be_u32(&mut head, count);
        be_u16(&mut head, w as u16);
        be_u16(&mut head, h as u16);
        head.push(0); // x mirror
        head.push(0); // y mirror
        be_f32(&mut head, print.geometry.display_width_mm.unwrap_or(0.0));
        be_f32(&mut head, print.geometry.display_height_mm.unwrap_or(0.0));
        be_f32(&mut head, print.geometry.machine_z_mm.unwrap_or(0.0));
        be_f32(&mut head, e.layer_height_mm);
        be_f32(&mut head, e.exposure_s);
        head.push(0); // exposure delay mode: use the light off time below
        be_f32(&mut head, e.light_off_delay_s.unwrap_or(0.0));
        for _ in 0..6 {
            be_f32(&mut head, 0.0); // before/after lift and retract waits
        }
        be_f32(&mut head, bottom_exposure);
        be_u32(&mut head, bottom_layers);
        be_f32(&mut head, l.bottom_lift_height_mm.unwrap_or(0.0));
        be_f32(&mut head, l.bottom_lift_speed_mm_min.unwrap_or(0.0));
        be_f32(&mut head, l.lift_height_mm.unwrap_or(0.0));
        be_f32(&mut head, l.lift_speed_mm_min.unwrap_or(0.0));
        be_f32(&mut head, l.bottom_lift_height_mm.unwrap_or(0.0));
        be_f32(&mut head, l.bottom_retract_speed_mm_min.unwrap_or(0.0));
        be_f32(&mut head, l.lift_height_mm.unwrap_or(0.0));
        be_f32(&mut head, l.retract_speed_mm_min.unwrap_or(0.0));
        for _ in 0..8 {
            be_f32(&mut head, 0.0); // second lift and retract, unused
        }
        be_u16(&mut head, e.bottom_light_pwm.unwrap_or(255) as u16);
        be_u16(&mut head, e.light_pwm.unwrap_or(255) as u16);
        head.push(0); // advance mode
        be_u32(&mut head, print.print_time_s.unwrap_or(0) as u32);
        be_f32(&mut head, print.material_volume_ml.unwrap_or(0.0));
        be_f32(&mut head, print.material_grams.unwrap_or(0.0));
        be_f32(&mut head, 0.0); // price
        text(&mut head, "$", 8);
        // offset_of_layer_content points just past this header. The remaining
        // fields are a u32, a u8 and a u16, so their size is known here.
        let layer_offset = head.len() + 4 + 1 + 2;
        be_u32(&mut head, layer_offset as u32);
        head.push(0); // grey scale level
        be_u16(&mut head, e.transition_layers.unwrap_or(0) as u16);
        debug_assert_eq!(head.len(), layer_offset);

        let file = std::fs::File::create(path).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let mut out = BufWriter::new(file);
        let io = |e: std::io::Error| Error::Io {
            path: path.to_path_buf(),
            source: e,
        };
        out.write_all(&head).map_err(io)?;

        let expected_pixels = w as u64 * h as u64;
        for index in 0..count {
            let img = layers.layer(index)?;
            if img.width != w || img.height != h {
                return Err(FormatError::Other(format!(
                    "layer {index} is {}x{} but the print is {w}x{h}",
                    img.width, img.height
                ))
                .into());
            }
            let (payload, covered) = goo_rle::encode(&img.pixels, w);
            // Every pixel must be accounted for. A short final run leaves
            // whatever was already in the printer's buffer on the screen.
            if covered != expected_pixels {
                return Err(FormatError::Other(format!(
                    "layer {index} encoded {covered} pixels but the panel needs {expected_pixels}"
                ))
                .into());
            }

            let z = print
                .layers
                .get(index as usize)
                .map(|l| l.z_mm)
                .unwrap_or_else(|| e.layer_height_mm * (index + 1) as f32);
            let exposure = print
                .effective_exposure_s(index)
                .unwrap_or(e.exposure_s);
            let is_bottom = index < bottom_layers;

            let mut rec: Vec<u8> = Vec::with_capacity(LAYER_PREAMBLE);
            be_u16(&mut rec, 0); // pause flag
            be_f32(&mut rec, 0.0); // pause position
            be_f32(&mut rec, z);
            be_f32(&mut rec, exposure);
            be_f32(&mut rec, e.light_off_delay_s.unwrap_or(0.0));
            for _ in 0..3 {
                be_f32(&mut rec, 0.0); // before/after lift, after retract waits
            }
            let (lift_h, lift_v) = if is_bottom {
                (l.bottom_lift_height_mm, l.bottom_lift_speed_mm_min)
            } else {
                (l.lift_height_mm, l.lift_speed_mm_min)
            };
            be_f32(&mut rec, lift_h.unwrap_or(0.0));
            be_f32(&mut rec, lift_v.unwrap_or(0.0));
            be_f32(&mut rec, 0.0); // second lift distance
            be_f32(&mut rec, 0.0); // second lift speed
            be_f32(&mut rec, lift_h.unwrap_or(0.0));
            be_f32(
                &mut rec,
                if is_bottom {
                    l.bottom_retract_speed_mm_min.unwrap_or(0.0)
                } else {
                    l.retract_speed_mm_min.unwrap_or(0.0)
                },
            );
            be_f32(&mut rec, 0.0); // second retract distance
            be_f32(&mut rec, 0.0); // second retract speed
            let pwm = if is_bottom {
                e.bottom_light_pwm.unwrap_or(255)
            } else {
                e.light_pwm.unwrap_or(255)
            };
            be_u16(&mut rec, pwm as u16);
            rec.extend_from_slice(&DELIM);
            debug_assert_eq!(rec.len(), LAYER_PREAMBLE);
            out.write_all(&rec).map_err(io)?;

            // data_size counts the magic byte, the payload and the checksum.
            be_u32_w(&mut out, (payload.len() + 2) as u32).map_err(io)?;
            out.write_all(&[goo_rle::IMAGE_MAGIC]).map_err(io)?;
            out.write_all(&payload).map_err(io)?;
            out.write_all(&[goo_rle::checksum(&payload)]).map_err(io)?;
            out.write_all(&DELIM).map_err(io)?;
        }

        out.write_all(&ENDING).map_err(io)?;
        out.flush().map_err(io)?;
        Ok(())
    }
}

fn be_u32_w<W: Write>(w: &mut W, v: u32) -> std::io::Result<()> {
    w.write_all(&v.to_be_bytes())
}

/// `YYYY-MM-DD HH:MM:SS` in UTC, without pulling in a date library.
fn now_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let (h, m, s) = ((secs % 86_400) / 3600, (secs % 3600) / 60, secs % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

/// Days since the epoch to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
