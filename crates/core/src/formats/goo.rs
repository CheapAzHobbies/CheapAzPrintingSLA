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
use crate::format::{Capabilities, Confidence, Detection, FormatHandler, FormatInfo, OpenedFile};
use crate::layers::LayerProvider;
use crate::limits;
use crate::model::*;
use std::collections::BTreeMap;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const ID: &str = "goo";

/// `V3.0` then the fixed magic tag.
const VERSION: &[u8; 4] = b"V3.0";
const MAGIC: [u8; 8] = [0x07, 0x00, 0x00, 0x00, 0x44, 0x4C, 0x50, 0x00];
const ENDING: [u8; 11] = [
    0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x44, 0x4C, 0x50, 0x00,
];
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
        reads: true,
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

/// Byte offset of the parameter block: two previews of fixed size, each
/// followed by a two byte delimiter.
const PARAMS: u64 = 194 + 0x6920 + 2 + 0x29108 + 2;
/// Bytes of layer record preceding each image payload.
const LAYER_PREAMBLE_LEN: u64 = 2 + 15 * 4 + 2 + 2;

/// A cursor over a file that refuses to read outside it.
struct Reader {
    data: Vec<u8>,
    pos: usize,
}

impl Reader {
    fn need(&self, n: usize) -> Result<()> {
        if self.pos + n > self.data.len() {
            return Err(FormatError::Truncated {
                offset: self.pos as u64,
                expected: n as u64,
                actual: self.data.len().saturating_sub(self.pos) as u64,
            }
            .into());
        }
        Ok(())
    }
    fn u8(&mut self) -> Result<u8> {
        self.need(1)?;
        let v = self.data[self.pos];
        self.pos += 1;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16> {
        self.need(2)?;
        let v = u16::from_be_bytes(self.data[self.pos..self.pos + 2].try_into().unwrap());
        self.pos += 2;
        Ok(v)
    }
    fn u32(&mut self) -> Result<u32> {
        self.need(4)?;
        let v = u32::from_be_bytes(self.data[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        Ok(v)
    }
    fn f32(&mut self) -> Result<f32> {
        self.need(4)?;
        let v = f32::from_be_bytes(self.data[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        Ok(v)
    }
    fn text(&mut self, width: usize) -> Result<String> {
        self.need(width)?;
        let raw = &self.data[self.pos..self.pos + width];
        self.pos += width;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        Ok(String::from_utf8_lossy(&raw[..end]).trim().to_string())
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.need(n)?;
        self.pos += n;
        Ok(())
    }
}

/// Where each layer's encoded payload lives, found once when the file opens so
/// layers can be fetched later without rescanning.
struct GooLayers {
    path: PathBuf,
    /// (offset, length) of each payload, excluding magic and checksum.
    spans: Vec<(u64, u32)>,
    width: u32,
    height: u32,
}

impl LayerProvider for GooLayers {
    fn layer_count(&self) -> u32 {
        self.spans.len() as u32
    }
    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn layer(&self, index: u32) -> Result<LayerImage> {
        let &(offset, len) = self
            .spans
            .get(index as usize)
            .ok_or(Error::LayerOutOfRange {
                index,
                count: self.spans.len() as u32,
            })?;
        let mut file = std::fs::File::open(&self.path).map_err(|e| Error::Io {
            path: self.path.clone(),
            source: e,
        })?;
        file.seek(SeekFrom::Start(offset)).map_err(|e| Error::Io {
            path: self.path.clone(),
            source: e,
        })?;
        let cap = limits::check_allocation(len as u64)?;
        let mut buf = vec![0u8; cap];
        file.read_exact(&mut buf).map_err(|e| Error::Io {
            path: self.path.clone(),
            source: e,
        })?;
        let pixels = goo_rle::decode(&buf, (self.width as usize) * (self.height as usize))
            .map_err(|e| FormatError::LayerDecode(format!("layer {index}: {e}")))?;
        if pixels.len() != (self.width as usize) * (self.height as usize) {
            return Err(FormatError::LayerDecode(format!(
                "layer {index} decoded {} pixels, expected {}",
                pixels.len(),
                self.width as usize * self.height as usize
            ))
            .into());
        }
        Ok(LayerImage {
            width: self.width,
            height: self.height,
            pixels,
        })
    }
}

/// Read the header and locate every layer payload.
fn parse(path: &Path) -> Result<(PrintFile, GooLayers, Vec<String>)> {
    let data = std::fs::read(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let file_size = data.len() as u64;
    if data.len() < 12 || &data[..4] != VERSION || data[4..12] != MAGIC {
        return Err(FormatError::BadMagic.into());
    }
    let mut r = Reader { data, pos: 12 };
    let software = r.text(32)?;
    let software_version = r.text(24)?;
    let file_time = r.text(24)?;
    let printer_name = r.text(32)?;
    let _printer_type = r.text(32)?;
    let profile = r.text(32)?;
    let _aa = r.u16()?;
    let _grey = r.u16()?;
    let _blur = r.u16()?;

    r.pos = PARAMS as usize;
    let total_layers = limits::check_layer_count(r.u32()? as u64)?;
    let xres = r.u16()? as u32;
    let yres = r.u16()? as u32;
    limits::check_resolution(xres, yres)?;
    let _x_mirror = r.u8()?;
    let _y_mirror = r.u8()?;
    let x_size = r.f32()?;
    let y_size = r.f32()?;
    let z_size = r.f32()?;
    let layer_thickness = r.f32()?;
    if !(layer_thickness.is_finite() && layer_thickness > 0.0) {
        return Err(FormatError::InvalidValue {
            field: "layer_thickness".into(),
            value: layer_thickness.to_string(),
            reason: "must be a positive number".into(),
        }
        .into());
    }
    let common_exposure = r.f32()?;
    let _delay_mode = r.u8()?;
    let turn_off_time = r.f32()?;
    r.skip(6 * 4)?; // before/after lift and retract waits
    let bottom_exposure = r.f32()?;
    let bottom_layers = r.u32()?;
    let bottom_lift_distance = r.f32()?;
    let bottom_lift_speed = r.f32()?;
    let lift_distance = r.f32()?;
    let lift_speed = r.f32()?;
    let _bottom_retract_distance = r.f32()?;
    let bottom_retract_speed = r.f32()?;
    let _retract_distance = r.f32()?;
    let retract_speed = r.f32()?;
    r.skip(8 * 4)?; // second lift and retract
    let bottom_pwm = r.u16()?;
    let light_pwm = r.u16()?;
    let _advance = r.u8()?;
    let printing_time = r.u32()?;
    let volume = r.f32()?;
    let weight = r.f32()?;
    let _price = r.f32()?;
    r.skip(8)?; // price unit
    let layer_offset = r.u32()? as u64;
    let _gray_scale = r.u8()?;
    let transition = r.u16()?;

    let mut warnings = Vec::new();
    if layer_offset != r.pos as u64 {
        warnings.push(format!(
            "the header says layers start at {layer_offset} but the fields end at {}",
            r.pos
        ));
    }
    limits::check_range(layer_offset, 0, file_size)?;

    // Walk the layer records, recording each payload and its own metadata.
    let mut spans = Vec::with_capacity(total_layers as usize);
    let mut layer_infos = Vec::with_capacity(total_layers as usize);
    let mut cursor = layer_offset;
    for index in 0..total_layers {
        limits::check_range(cursor, LAYER_PREAMBLE_LEN + 4, file_size)?;
        let mut lr = Reader {
            data: std::mem::take(&mut r.data),
            pos: cursor as usize,
        };
        let _pause = lr.u16()?;
        let _pause_z = lr.f32()?;
        let z = lr.f32()?;
        let exposure = lr.f32()?;
        let off_time = lr.f32()?;
        lr.skip(3 * 4)?;
        let l_lift = lr.f32()?;
        let l_lift_speed = lr.f32()?;
        lr.skip(2 * 4)?;
        lr.skip(4 * 4)?;
        let _pwm = lr.u16()?;
        lr.skip(2)?; // delimiter
        let data_size = lr.u32()?;
        if data_size < 2 {
            return Err(FormatError::InvalidValue {
                field: format!("layer {index} data_size"),
                value: data_size.to_string(),
                reason: "must cover at least the magic byte and the checksum".into(),
            }
            .into());
        }
        let payload_len = data_size - 2;
        let magic_pos = lr.pos as u64;
        limits::check_range(magic_pos, data_size as u64 + 2, file_size)?;
        if lr.data[magic_pos as usize] != goo_rle::IMAGE_MAGIC {
            return Err(FormatError::InvalidValue {
                field: format!("layer {index} image magic"),
                value: format!("0x{:02x}", lr.data[magic_pos as usize]),
                reason: format!("expected 0x{:02x}", goo_rle::IMAGE_MAGIC),
            }
            .into());
        }
        let payload_start = magic_pos + 1;
        let stored_checksum = lr.data[(payload_start + payload_len as u64) as usize];
        let payload =
            &lr.data[payload_start as usize..(payload_start + payload_len as u64) as usize];
        if goo_rle::checksum(payload) != stored_checksum {
            warnings.push(format!("layer {index} checksum does not match its data"));
        }
        spans.push((payload_start, payload_len));
        layer_infos.push(LayerInfo {
            z_mm: z,
            exposure_s: Some(exposure),
            light_off_delay_s: Some(off_time),
            lift_height_mm: Some(l_lift),
            lift_speed_mm_min: Some(l_lift_speed),
        });
        cursor = payload_start + payload_len as u64 + 1 + 2;
        r.data = lr.data;
    }

    let mut extra = BTreeMap::new();
    if !software.is_empty() {
        extra.insert(
            "goo.software".into(),
            format!("{software} {software_version}"),
        );
    }
    if !file_time.is_empty() {
        extra.insert("goo.file_time".into(), file_time);
    }
    if !profile.is_empty() {
        extra.insert("goo.profile".into(), profile);
    }

    let print = PrintFile {
        source_format: ID.to_string(),
        geometry: Geometry {
            resolution_x: xres,
            resolution_y: yres,
            display_width_mm: (x_size > 0.0).then_some(x_size),
            display_height_mm: (y_size > 0.0).then_some(y_size),
            machine_z_mm: (z_size > 0.0).then_some(z_size),
        },
        exposure: Exposure {
            layer_height_mm: layer_thickness,
            exposure_s: common_exposure,
            bottom_exposure_s: Some(bottom_exposure),
            bottom_layers: Some(bottom_layers),
            light_off_delay_s: (turn_off_time > 0.0).then_some(turn_off_time),
            bottom_light_off_delay_s: None,
            transition_layers: (transition > 0).then_some(transition as u32),
            light_pwm: Some(light_pwm.min(255) as u8),
            bottom_light_pwm: Some(bottom_pwm.min(255) as u8),
        },
        lift: Lift {
            lift_height_mm: (lift_distance > 0.0).then_some(lift_distance),
            lift_speed_mm_min: (lift_speed > 0.0).then_some(lift_speed),
            bottom_lift_height_mm: (bottom_lift_distance > 0.0).then_some(bottom_lift_distance),
            bottom_lift_speed_mm_min: (bottom_lift_speed > 0.0).then_some(bottom_lift_speed),
            retract_speed_mm_min: (retract_speed > 0.0).then_some(retract_speed),
            bottom_retract_speed_mm_min: (bottom_retract_speed > 0.0)
                .then_some(bottom_retract_speed),
        },
        layers: layer_infos,
        thumbnails: Vec::new(), // previews are RGB565; decoding them is not needed to convert
        print_time_s: (printing_time > 0).then_some(printing_time as u64),
        material_volume_ml: (volume > 0.0).then_some(volume),
        material_grams: (weight > 0.0).then_some(weight),
        material_name: None,
        machine_name: (!printer_name.is_empty()).then_some(printer_name),
        extra,
    };

    Ok((
        print,
        GooLayers {
            path: path.to_path_buf(),
            spans,
            width: xres,
            height: yres,
        },
        warnings,
    ))
}

/// Fixed-width text field, NUL padded and NUL terminated.
fn text(out: &mut Vec<u8>, s: &str, width: usize) {
    let bytes = s.as_bytes();
    let take = bytes.len().min(width.saturating_sub(1));
    out.extend_from_slice(&bytes[..take]);
    out.extend(std::iter::repeat_n(0u8, width - take));
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

    fn validate(&self, path: &Path) -> Result<Vec<String>> {
        let (_, _, warnings) = parse(path)?;
        Ok(warnings)
    }

    fn open(&self, path: &Path) -> Result<OpenedFile> {
        let (print, layers, _) = parse(path)?;
        Ok(OpenedFile {
            print,
            layers: Box::new(layers),
        })
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
        // Decoding the source image and run-length encoding it is the whole
        // cost of a conversion, and each layer is independent, so it runs on
        // a pool. Records still have to reach the file in order, which is
        // what `in_order` guarantees; see pipeline.rs for how far ahead the
        // workers are allowed to get.
        let workers = crate::pipeline::workers_for(expected_pixels);
        crate::pipeline::in_order(
            count,
            workers,
            |index| {
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
                        "layer {index} encoded {covered} pixels but the panel needs \
                         {expected_pixels}"
                    ))
                    .into());
                }
                Ok(payload)
            },
            |index, payload| {
                let z = print
                    .layers
                    .get(index as usize)
                    .map(|l| l.z_mm)
                    .unwrap_or_else(|| e.layer_height_mm * (index + 1) as f32);
                let exposure = print.effective_exposure_s(index).unwrap_or(e.exposure_s);
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
                Ok(())
            },
        )?;

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
