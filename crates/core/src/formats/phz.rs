//! Phrozen PHZ (phase 20).
//!
//! Chitubox's older format, and closely related to CTB: same little-endian
//! layout, same preview encoding, same 36 byte layer table. What differs is
//! that the three header records CTB scatters through the file are collapsed
//! into one 216 byte record at the front, the layer bitmaps use a simpler and
//! rather less efficient run-length scheme, and the layer cipher is a variant
//! with different constants.
//!
//! The layout follows catibo's description, checked field by field against a
//! file UVtools produced; see the credits in the README.

use super::{ctb_preview, phz_rle};
use crate::error::{Error, FormatError, Result};
use crate::format::{Capabilities, Confidence, Detection, FormatHandler, FormatInfo, OpenedFile};
use crate::layers::LayerProvider;
use crate::limits;
use crate::model::*;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const ID: &str = "phz";

const MAGIC: u32 = 0x9FDA_83AE;
/// The only version seen in the wild.
const VERSION: u32 = 2;
/// The single header record, which everything else hangs off.
const HEADER_BYTES: usize = 0xD8;
const LAYER_ENTRY: usize = 36;
const IMAGE_HEADER: usize = 0x20;
/// What the Phrozen Sonic Mini expects to find. Changing it, catibo reports,
/// makes files unreadable, so it is written as found rather than reasoned
/// about.
const ENCRYPTION_MODE: u32 = 0x1C;

static INFO: FormatInfo = FormatInfo {
    id: ID,
    name: "Phrozen PHZ",
    extension: "phz",
    aliases: &[],
    description: "The format Chitubox wrote for the Phrozen Sonic Mini and its relatives. \
                  Older than CTB and related to it, with a simpler run-length scheme that \
                  makes files several times larger.",
    limitations: &[
        "Stores seven bits of grey per pixel, so an eight-bit image loses its lowest bit",
        "Layer data is several times larger than the same print in CTB",
        "Stores two preview images, at 400x300 and 200x125",
    ],
    capabilities: Capabilities {
        reads: true,
        writes: true,
        per_layer_exposure: true,
        per_layer_lift: false,
        thumbnails: true,
        max_thumbnails: 2,
        print_time: true,
        material_volume: true,
        machine_name: true,
    },
};

pub struct PhzHandler;

fn le_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le_f32(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn put_u16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_f32(b: &mut [u8], at: usize, v: f32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// The one header record. Field positions come from catibo's layout table.
#[derive(Debug, Clone, Copy)]
struct Header {
    layer_height_mm: f32,
    exposure_s: f32,
    bottom_exposure_s: f32,
    bottom_layers: u32,
    resolution_x: u32,
    resolution_y: u32,
    preview_large_offset: u32,
    layer_table_offset: u32,
    layer_count: u32,
    preview_small_offset: u32,
    print_time_s: u32,
    light_pwm: u16,
    bottom_light_pwm: u16,
    bed_x_mm: f32,
    bed_y_mm: f32,
    bed_z_mm: f32,
    encryption_key: u32,
    bottom_light_off_s: f32,
    light_off_s: f32,
    bottom_lift_height_mm: f32,
    bottom_lift_speed: f32,
    lift_height_mm: f32,
    lift_speed: f32,
    retract_speed: f32,
    volume_ml: f32,
    weight_g: f32,
    machine_name_offset: u32,
    machine_name_len: u32,
}

impl Header {
    fn parse(b: &[u8]) -> Result<Self> {
        if b.len() < HEADER_BYTES {
            return Err(FormatError::Truncated {
                offset: 0,
                expected: HEADER_BYTES as u64,
                actual: b.len() as u64,
            }
            .into());
        }
        if le_u32(b, 0x00) != MAGIC {
            return Err(FormatError::BadMagic.into());
        }
        let version = le_u32(b, 0x04);
        if version != VERSION {
            return Err(FormatError::UnsupportedVersion {
                format: "PHZ".into(),
                version: version.to_string(),
            }
            .into());
        }
        Ok(Self {
            layer_height_mm: le_f32(b, 0x08),
            exposure_s: le_f32(b, 0x0C),
            bottom_exposure_s: le_f32(b, 0x10),
            bottom_layers: le_u32(b, 0x14),
            resolution_x: le_u32(b, 0x18),
            resolution_y: le_u32(b, 0x1C),
            preview_large_offset: le_u32(b, 0x20),
            layer_table_offset: le_u32(b, 0x24),
            layer_count: le_u32(b, 0x28),
            preview_small_offset: le_u32(b, 0x2C),
            print_time_s: le_u32(b, 0x30),
            // 0x34 projection, 0x38 level set count.
            light_pwm: le_u16(b, 0x3C),
            bottom_light_pwm: le_u16(b, 0x3E),
            // 0x48 overall height, which the layer table also gives.
            bed_x_mm: le_f32(b, 0x4C),
            bed_y_mm: le_f32(b, 0x50),
            bed_z_mm: le_f32(b, 0x54),
            encryption_key: le_u32(b, 0x58),
            bottom_light_off_s: le_f32(b, 0x5C),
            light_off_s: le_f32(b, 0x60),
            // 0x64 repeats the bottom layer count.
            bottom_lift_height_mm: le_f32(b, 0x6C),
            bottom_lift_speed: le_f32(b, 0x70),
            lift_height_mm: le_f32(b, 0x74),
            lift_speed: le_f32(b, 0x78),
            retract_speed: le_f32(b, 0x7C),
            volume_ml: le_f32(b, 0x80),
            weight_g: le_f32(b, 0x84),
            // 0x88 cost.
            machine_name_offset: le_u32(b, 0x90),
            machine_name_len: le_u32(b, 0x94),
            // 0xB0 encryption mode, 0xB8 anti-alias level.
        })
    }
}

/// Undo the keyed XOR stream, the variant PHZ uses.
///
/// Same shape as CTB's — a degenerate linear congruential generator XORed a
/// word at a time — with different constants and one extra quirk: any key that
/// is a multiple of `0x4324` produces a keystream of zeroes and so no cipher
/// at all. Zero is one such key, which is how an unencrypted file is written.
fn uncipher(data: &mut [u8], key: u32, iv: u32) {
    let reduced = key % 0x4324;
    if reduced == 0 {
        return;
    }
    let step = reduced.wrapping_mul(0x34A3_2231);
    let mut state = (iv ^ 0x3FAD_2212)
        .wrapping_mul(reduced)
        .wrapping_mul(0x4910_913D);
    for chunk in data.chunks_mut(4) {
        for (i, byte) in chunk.iter_mut().enumerate() {
            *byte ^= (state >> (i * 8)) as u8;
        }
        state = state.wrapping_add(step);
    }
}

fn read_at(file: &mut std::fs::File, path: &Path, offset: u64, len: usize) -> Result<Vec<u8>> {
    let io = |e: std::io::Error| Error::Io {
        path: path.to_path_buf(),
        source: e,
    };
    file.seek(SeekFrom::Start(offset)).map_err(io)?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).map_err(io)?;
    Ok(buf)
}

struct PhzLayers {
    path: PathBuf,
    width: u32,
    height: u32,
    entries: Vec<(u32, u32)>,
    encryption_key: u32,
}

impl LayerProvider for PhzLayers {
    fn layer_count(&self) -> u32 {
        self.entries.len() as u32
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn layer(&self, index: u32) -> Result<LayerImage> {
        let (offset, size) = *self
            .entries
            .get(index as usize)
            .ok_or(Error::LayerOutOfRange {
                index,
                count: self.entries.len() as u32,
            })?;
        let mut file = std::fs::File::open(&self.path).map_err(|e| Error::Io {
            path: self.path.clone(),
            source: e,
        })?;
        let want = limits::check_allocation(size as u64)?;
        let mut data = read_at(&mut file, &self.path, offset as u64, want)?;
        uncipher(&mut data, self.encryption_key, index);
        let expected = self.width as usize * self.height as usize;
        let pixels = phz_rle::decode(&data, expected)
            .map_err(|e| FormatError::LayerDecode(format!("layer {index}: {e}")))?;
        Ok(LayerImage {
            width: self.width,
            height: self.height,
            pixels,
        })
    }
}

fn open_header(path: &Path) -> Result<(Header, std::fs::File, u64)> {
    let mut file = std::fs::File::open(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let size = file
        .metadata()
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?
        .len();
    let mut head = vec![0u8; HEADER_BYTES.min(size as usize)];
    file.read_exact(&mut head).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let header = Header::parse(&head)?;
    limits::check_layer_count(header.layer_count as u64)?;
    limits::check_resolution(header.resolution_x, header.resolution_y)?;
    limits::check_range(
        header.layer_table_offset as u64,
        header.layer_count as u64 * LAYER_ENTRY as u64,
        size,
    )?;
    Ok((header, file, size))
}

fn read_preview(
    file: &mut std::fs::File,
    path: &Path,
    at: u64,
    size: u64,
) -> Result<Option<Thumbnail>> {
    limits::check_range(at, IMAGE_HEADER as u64, size)?;
    let head = read_at(file, path, at, IMAGE_HEADER)?;
    let (w, h) = (le_u32(&head, 0x00), le_u32(&head, 0x04));
    let (offset, length) = (le_u32(&head, 0x08), le_u32(&head, 0x0C));
    if w == 0 || h == 0 || length == 0 {
        return Ok(None);
    }
    limits::check_thumbnail(w, h)?;
    limits::check_range(offset as u64, length as u64, size)?;
    let want = limits::check_allocation(length as u64)?;
    let data = read_at(file, path, offset as u64, want)?;
    let rgb = ctb_preview::decode(&data, (w * h) as usize)?;
    Ok(Some(Thumbnail {
        width: w,
        height: h,
        rgb,
    }))
}

fn positive(v: f32) -> Option<f32> {
    (v.is_finite() && v > 0.0).then_some(v)
}

impl FormatHandler for PhzHandler {
    fn info(&self) -> &'static FormatInfo {
        &INFO
    }

    fn detect(&self, path: &Path, data: &[u8]) -> Detection {
        if data.len() >= 8 && le_u32(data, 0) == MAGIC {
            let version = le_u32(data, 4);
            return if version == VERSION {
                Detection {
                    format_id: ID,
                    confidence: Confidence::High,
                    reason: format!("Phrozen PHZ header, version {version}"),
                }
            } else {
                Detection {
                    format_id: ID,
                    confidence: Confidence::Medium,
                    reason: format!("Phrozen header, but version {version} is not supported"),
                }
            };
        }
        if path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("phz"))
            .unwrap_or(false)
        {
            return Detection {
                format_id: ID,
                confidence: Confidence::Low,
                reason: "named .phz, but the header does not match".into(),
            };
        }
        Detection {
            format_id: ID,
            confidence: Confidence::None,
            reason: String::new(),
        }
    }

    fn validate(&self, path: &Path) -> Result<Vec<String>> {
        let (header, mut file, size) = open_header(path)?;
        let mut notes = Vec::new();
        if header.layer_count == 0 {
            notes.push("the file declares no layers".into());
        }
        if header.layer_height_mm <= 0.0 {
            notes.push(format!(
                "layer height is {} mm, which cannot be right",
                header.layer_height_mm
            ));
        }
        let table = read_at(
            &mut file,
            path,
            header.layer_table_offset as u64,
            header.layer_count as usize * LAYER_ENTRY,
        )?;
        for i in 0..header.layer_count as usize {
            let at = i * LAYER_ENTRY;
            let (offset, length) = (le_u32(&table, at + 0x0C), le_u32(&table, at + 0x10));
            if limits::check_range(offset as u64, length as u64, size).is_err() {
                notes.push(format!(
                    "layer {i} points outside the file (offset {offset}, {length} bytes)"
                ));
                break;
            }
        }
        Ok(notes)
    }

    fn open(&self, path: &Path) -> Result<OpenedFile> {
        let (header, mut file, size) = open_header(path)?;

        let table = read_at(
            &mut file,
            path,
            header.layer_table_offset as u64,
            header.layer_count as usize * LAYER_ENTRY,
        )?;
        let mut layers = Vec::with_capacity(header.layer_count as usize);
        let mut entries = Vec::with_capacity(header.layer_count as usize);
        for i in 0..header.layer_count as usize {
            let at = i * LAYER_ENTRY;
            let (offset, length) = (le_u32(&table, at + 0x0C), le_u32(&table, at + 0x10));
            limits::check_range(offset as u64, length as u64, size).map_err(|_| {
                FormatError::Other(format!(
                    "layer {i} points outside the file (offset {offset}, {length} bytes)"
                ))
            })?;
            layers.push(LayerInfo {
                z_mm: le_f32(&table, at),
                exposure_s: Some(le_f32(&table, at + 0x04)),
                light_off_delay_s: Some(le_f32(&table, at + 0x08)),
                lift_height_mm: None,
                lift_speed_mm_min: None,
            });
            entries.push((offset, length));
        }

        let mut thumbnails = Vec::new();
        for at in [header.preview_large_offset, header.preview_small_offset] {
            if at == 0 {
                continue;
            }
            match read_preview(&mut file, path, at as u64, size) {
                Ok(Some(t)) => thumbnails.push(t),
                Ok(None) => {}
                Err(e) => log::debug!("phz: preview at {at} ignored: {e}"),
            }
        }

        let machine_name = if header.machine_name_len > 0
            && limits::check_range(
                header.machine_name_offset as u64,
                header.machine_name_len as u64,
                size,
            )
            .is_ok()
        {
            let want = limits::check_allocation(header.machine_name_len as u64)?;
            read_at(&mut file, path, header.machine_name_offset as u64, want)
                .ok()
                .map(|b| {
                    String::from_utf8_lossy(&b)
                        .trim_end_matches('\0')
                        .to_string()
                })
                .filter(|s| !s.is_empty())
        } else {
            None
        };

        let print = PrintFile {
            source_format: ID.into(),
            geometry: Geometry {
                resolution_x: header.resolution_x,
                resolution_y: header.resolution_y,
                display_width_mm: positive(header.bed_x_mm),
                display_height_mm: positive(header.bed_y_mm),
                machine_z_mm: positive(header.bed_z_mm),
            },
            exposure: Exposure {
                layer_height_mm: header.layer_height_mm,
                exposure_s: header.exposure_s,
                bottom_exposure_s: positive(header.bottom_exposure_s),
                bottom_layers: Some(header.bottom_layers),
                light_off_delay_s: positive(header.light_off_s),
                bottom_light_off_delay_s: positive(header.bottom_light_off_s),
                transition_layers: None,
                light_pwm: Some(header.light_pwm.min(255) as u8),
                bottom_light_pwm: Some(header.bottom_light_pwm.min(255) as u8),
            },
            lift: Lift {
                lift_height_mm: positive(header.lift_height_mm),
                lift_speed_mm_min: positive(header.lift_speed),
                bottom_lift_height_mm: positive(header.bottom_lift_height_mm),
                bottom_lift_speed_mm_min: positive(header.bottom_lift_speed),
                retract_speed_mm_min: positive(header.retract_speed),
                bottom_retract_speed_mm_min: positive(header.retract_speed),
            },
            layers,
            thumbnails,
            print_time_s: (header.print_time_s > 0).then_some(header.print_time_s as u64),
            material_volume_ml: positive(header.volume_ml),
            material_grams: positive(header.weight_g),
            material_name: None,
            machine_name,
            extra: std::collections::BTreeMap::new(),
        };

        Ok(OpenedFile {
            print,
            layers: Box::new(PhzLayers {
                path: path.to_path_buf(),
                width: header.resolution_x,
                height: header.resolution_y,
                entries,
                encryption_key: header.encryption_key,
            }),
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

        let e = &print.exposure;
        let l = &print.lift;
        let bottom_layers = e.bottom_layers.unwrap_or(0);

        let big = ctb_preview::encode(
            &ctb_preview::fit(
                print.thumbnails.iter().max_by_key(|t| t.pixel_count()),
                ctb_preview::LARGE.0,
                ctb_preview::LARGE.1,
            ),
            ctb_preview::LARGE.0,
            ctb_preview::LARGE.1,
        );
        let small = ctb_preview::encode(
            &ctb_preview::fit(
                print.thumbnails.iter().min_by_key(|t| t.pixel_count()),
                ctb_preview::SMALL.0,
                ctb_preview::SMALL.1,
            ),
            ctb_preview::SMALL.0,
            ctb_preview::SMALL.1,
        );
        let machine = print.machine_name.clone().unwrap_or_default();

        let big_head_at = HEADER_BYTES;
        let big_data_at = big_head_at + IMAGE_HEADER;
        let small_head_at = big_data_at + big.len();
        let small_data_at = small_head_at + IMAGE_HEADER;
        let machine_at = small_data_at + small.len();
        let table_at = machine_at + machine.len();
        let layers_at = table_at + count as usize * LAYER_ENTRY;

        let mut head = vec![0u8; HEADER_BYTES];
        put_u32(&mut head, 0x00, MAGIC);
        put_u32(&mut head, 0x04, VERSION);
        put_f32(&mut head, 0x08, e.layer_height_mm);
        put_f32(&mut head, 0x0C, e.exposure_s);
        put_f32(&mut head, 0x10, e.bottom_exposure_s.unwrap_or(e.exposure_s));
        put_u32(&mut head, 0x14, bottom_layers);
        put_u32(&mut head, 0x18, w);
        put_u32(&mut head, 0x1C, h);
        put_u32(&mut head, 0x20, big_head_at as u32);
        put_u32(&mut head, 0x24, table_at as u32);
        put_u32(&mut head, 0x28, count);
        put_u32(&mut head, 0x2C, small_head_at as u32);
        put_u32(&mut head, 0x30, print.print_time_s.unwrap_or(0) as u32);
        put_u32(&mut head, 0x34, 1); // projection: normal
        put_u32(&mut head, 0x38, 1); // one set of layers, not an antialiased stack
        put_u16(&mut head, 0x3C, e.light_pwm.unwrap_or(255) as u16);
        put_u16(&mut head, 0x3E, e.bottom_light_pwm.unwrap_or(255) as u16);
        put_f32(&mut head, 0x48, print.height_mm().unwrap_or(0.0));
        put_f32(
            &mut head,
            0x4C,
            print.geometry.display_width_mm.unwrap_or(0.0),
        );
        put_f32(
            &mut head,
            0x50,
            print.geometry.display_height_mm.unwrap_or(0.0),
        );
        put_f32(&mut head, 0x54, print.geometry.machine_z_mm.unwrap_or(0.0));
        put_u32(&mut head, 0x58, 0); // no cipher
        put_f32(&mut head, 0x5C, e.bottom_light_off_delay_s.unwrap_or(0.0));
        put_f32(&mut head, 0x60, e.light_off_delay_s.unwrap_or(0.0));
        put_u32(&mut head, 0x64, bottom_layers);
        put_f32(&mut head, 0x6C, l.bottom_lift_height_mm.unwrap_or(0.0));
        put_f32(&mut head, 0x70, l.bottom_lift_speed_mm_min.unwrap_or(0.0));
        put_f32(&mut head, 0x74, l.lift_height_mm.unwrap_or(0.0));
        put_f32(&mut head, 0x78, l.lift_speed_mm_min.unwrap_or(0.0));
        put_f32(&mut head, 0x7C, l.retract_speed_mm_min.unwrap_or(0.0));
        put_f32(&mut head, 0x80, print.material_volume_ml.unwrap_or(0.0));
        put_f32(&mut head, 0x84, print.material_grams.unwrap_or(0.0));
        put_u32(
            &mut head,
            0x90,
            if machine.is_empty() {
                0
            } else {
                machine_at as u32
            },
        );
        put_u32(&mut head, 0x94, machine.len() as u32);
        put_u32(&mut head, 0xB0, ENCRYPTION_MODE);
        put_u32(&mut head, 0xB8, 1); // anti-alias level

        let image_head = |w: u32, h: u32, at: usize, len: usize| {
            let mut b = vec![0u8; IMAGE_HEADER];
            put_u32(&mut b, 0x00, w);
            put_u32(&mut b, 0x04, h);
            put_u32(&mut b, 0x08, at as u32);
            put_u32(&mut b, 0x0C, len as u32);
            b
        };

        let file = std::fs::File::create(path).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let mut out = std::io::BufWriter::new(file);
        let io = |e: std::io::Error| Error::Io {
            path: path.to_path_buf(),
            source: e,
        };
        out.write_all(&head).map_err(io)?;
        out.write_all(&image_head(
            ctb_preview::LARGE.0,
            ctb_preview::LARGE.1,
            big_data_at,
            big.len(),
        ))
        .map_err(io)?;
        out.write_all(&big).map_err(io)?;
        out.write_all(&image_head(
            ctb_preview::SMALL.0,
            ctb_preview::SMALL.1,
            small_data_at,
            small.len(),
        ))
        .map_err(io)?;
        out.write_all(&small).map_err(io)?;
        out.write_all(machine.as_bytes()).map_err(io)?;

        // The table sits in front of the bitmaps, so its space is reserved and
        // filled in once their lengths are known.
        let mut table = vec![0u8; count as usize * LAYER_ENTRY];
        out.write_all(&table).map_err(io)?;

        let expected_pixels = w as u64 * h as u64;
        let workers = crate::pipeline::workers_for(expected_pixels);
        let mut at = layers_at;
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
                let (payload, covered) = phz_rle::encode(&img.pixels, w);
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
                let off = if index < bottom_layers {
                    e.bottom_light_off_delay_s.or(e.light_off_delay_s)
                } else {
                    e.light_off_delay_s
                }
                .unwrap_or(0.0);
                let entry = index as usize * LAYER_ENTRY;
                put_f32(&mut table, entry, z);
                put_f32(&mut table, entry + 0x04, exposure);
                put_f32(&mut table, entry + 0x08, off);
                put_u32(&mut table, entry + 0x0C, at as u32);
                put_u32(&mut table, entry + 0x10, payload.len() as u32);
                at += payload.len();
                out.write_all(&payload).map_err(io)
            },
        )?;

        out.flush().map_err(io)?;
        let mut file = out.into_inner().map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e.into_error(),
        })?;
        file.seek(SeekFrom::Start(table_at as u64)).map_err(io)?;
        file.write_all(&table).map_err(io)?;
        file.flush().map_err(io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keystream words worked out independently from the published recurrence.
    #[test]
    fn the_keystream_matches_the_published_recurrence() {
        let mut data = vec![0u8; 12];
        uncipher(&mut data, 0x1234_5678, 3);
        let words: Vec<u32> = data
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(words, [0x181F_8D24, 0x6608_2F98, 0xB3F0_D20C]);
    }

    #[test]
    fn the_cipher_is_its_own_inverse() {
        let original: Vec<u8> = (0..=200u8).collect();
        let mut data = original.clone();
        uncipher(&mut data, 0x9999, 4);
        assert_ne!(data, original);
        uncipher(&mut data, 0x9999, 4);
        assert_eq!(data, original);
    }

    #[test]
    fn a_key_that_is_a_multiple_of_the_modulus_is_no_cipher_at_all() {
        // Any key that is 0 mod 0x4324 gives a keystream of zeroes. Zero is
        // one, which is how an unencrypted file is written; the others are a
        // weakness of the cipher rather than a choice.
        for key in [0u32, 0x4324, 0x8648] {
            let original: Vec<u8> = (0..32u8).collect();
            let mut data = original.clone();
            uncipher(&mut data, key, 1);
            assert_eq!(data, original, "key {key:#x} should leave the data alone");
        }
    }
}
