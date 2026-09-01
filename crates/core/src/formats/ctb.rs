//! Chitubox CTB (phase 19).
//!
//! Little-endian throughout, unlike GOO. A fixed header holds the print
//! parameters and points at four other places in the file: two preview images,
//! a block of print parameters, a block of slicer information, and a table of
//! layer definitions. Each layer definition points in turn at its own
//! run-length encoded bitmap.
//!
//! The layout follows the community description used by UVtools and mslicer;
//! see the credits in the README. Where this file says a field is unknown, it
//! means the published description does not name it, not that it is unused.
//!
//! Layer data in files from Chitubox itself is obfuscated with a keyed XOR
//! stream, described below. Handling it is what lets this read files people
//! actually have rather than only ones written by other open tools.
//!
//! Writing is implemented and tested but **not offered**, because UVtools will
//! not read what it produces and I have not worked out why.
//!
//! What is known: the layer table it writes is correct — z strictly
//! increasing, offsets contiguous, the last record ending exactly at the end
//! of the file — and its run-length payloads come out the same length, byte
//! for byte, as UVtools' own for the same input. catibo reads the files
//! completely and correctly, and so does this reader. But UVtools reads the
//! layer table as though it began somewhere else, reports impossible z values
//! and refuses the file. It does that for some resolutions and not others,
//! which no theory here explains.
//!
//! UVtools is what most people use to check a file before committing it to a
//! printer, so a file it rejects is not one to hand anybody, whatever this
//! reader thinks of it. The writer stays, with its tests, behind a capability
//! flag that says no.

use super::{ctb_preview, ctb_rle};
use crate::error::{Error, FormatError, Result};
use crate::format::{Capabilities, Confidence, Detection, FormatHandler, FormatInfo, OpenedFile};
use crate::layers::LayerProvider;
use crate::limits;
use crate::model::*;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const ID: &str = "ctb";

/// Chitubox's marker. The older Photon/CBDDLP files use 0x12FD0019 and are a
/// different enough layout that they are not claimed here.
const MAGIC: u32 = 0x12FD_0086;
/// The oldest and newest versions this understands.
const MIN_VERSION: u32 = 2;
const MAX_VERSION: u32 = 5;

/// Size of one entry in the layer table.
const LAYER_ENTRY: u64 = 36;
/// Size of the record in front of each preview image.
const IMAGE_HEADER: usize = 0x20;
/// Sizes of the two extension blocks, taken from files in the wild rather than
/// from how much of them this reads: a printer that expects a block of a given
/// size should get one.
const EXT_CONFIG_BYTES: usize = 0x3C;
const EXT2_BYTES: usize = 0x4C;

static INFO: FormatInfo = FormatInfo {
    id: ID,
    name: "Chitubox CTB",
    extension: "ctb",
    aliases: &[],
    description: "The format Chitubox slices to, and what most resin printers on the market \
                  read. Layer images are run-length encoded with seven bits of grey per pixel.",
    limitations: &[
        "Stores seven bits of grey per pixel, so an eight-bit image loses its lowest bit",
        "Stores two preview images, at 400x300 and 200x125",
        "Reading only: writing is implemented but UVtools will not read the result",
    ],
    capabilities: Capabilities {
        reads: true,
        writes: false,
        per_layer_exposure: true,
        per_layer_lift: true,
        thumbnails: true,
        max_thumbnails: 2,
        print_time: true,
        material_volume: true,
        machine_name: true,
    },
};

pub struct CtbHandler;

/// The fixed header, as far as this reads it.
#[derive(Debug, Clone, Copy)]
struct Header {
    version: u32,
    bed_x_mm: f32,
    bed_y_mm: f32,
    bed_z_mm: f32,
    layer_height_mm: f32,
    exposure_s: f32,
    bottom_exposure_s: f32,
    light_off_delay_s: f32,
    bottom_layers: u32,
    resolution_x: u32,
    resolution_y: u32,
    preview_large_offset: u32,
    layers_offset: u32,
    layer_count: u32,
    preview_small_offset: u32,
    print_time_s: u32,
    print_params_offset: u32,
    print_params_size: u32,
    light_pwm: u16,
    bottom_light_pwm: u16,
    encryption_key: u32,
    slicer_offset: u32,
    slicer_size: u32,
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

fn le_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le_f32(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// How many bytes of header this reads. Everything below is inside it.
const HEADER_BYTES: usize = 0x70;

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
        if !(MIN_VERSION..=MAX_VERSION).contains(&version) {
            return Err(FormatError::UnsupportedVersion {
                format: "CTB".into(),
                version: version.to_string(),
            }
            .into());
        }
        Ok(Self {
            version,
            bed_x_mm: le_f32(b, 0x08),
            bed_y_mm: le_f32(b, 0x0C),
            bed_z_mm: le_f32(b, 0x10),
            // 0x14 and 0x18 are unknown.
            // 0x1C is the total height, which the layer table also gives.
            layer_height_mm: le_f32(b, 0x20),
            exposure_s: le_f32(b, 0x24),
            bottom_exposure_s: le_f32(b, 0x28),
            light_off_delay_s: le_f32(b, 0x2C),
            bottom_layers: le_u32(b, 0x30),
            resolution_x: le_u32(b, 0x34),
            resolution_y: le_u32(b, 0x38),
            preview_large_offset: le_u32(b, 0x3C),
            layers_offset: le_u32(b, 0x40),
            layer_count: le_u32(b, 0x44),
            preview_small_offset: le_u32(b, 0x48),
            print_time_s: le_u32(b, 0x4C),
            // 0x50 is the projector type.
            print_params_offset: le_u32(b, 0x54),
            print_params_size: le_u32(b, 0x58),
            // 0x5C is the anti-alias level.
            light_pwm: le_u16(b, 0x60),
            bottom_light_pwm: le_u16(b, 0x62),
            encryption_key: le_u32(b, 0x64),
            slicer_offset: le_u32(b, 0x68),
            slicer_size: le_u32(b, 0x6C),
        })
    }
}

/// The print parameters block, which holds the lift settings.
#[derive(Debug, Clone, Copy, Default)]
struct PrintParams {
    bottom_lift_height_mm: f32,
    bottom_lift_speed: f32,
    lift_height_mm: f32,
    lift_speed: f32,
    retract_speed: f32,
    volume_ml: f32,
    weight_g: f32,
    bottom_light_off_delay_s: f32,
    light_off_delay_s: f32,
}

impl PrintParams {
    /// How much of the block this reads. The block itself is longer; the rest
    /// is padding as far as anyone has established.
    const READS: usize = 0x28;

    fn parse(b: &[u8]) -> Self {
        if b.len() < Self::READS {
            return Self::default();
        }
        Self {
            bottom_lift_height_mm: le_f32(b, 0x00),
            bottom_lift_speed: le_f32(b, 0x04),
            lift_height_mm: le_f32(b, 0x08),
            lift_speed: le_f32(b, 0x0C),
            retract_speed: le_f32(b, 0x10),
            volume_ml: le_f32(b, 0x14),
            weight_g: le_f32(b, 0x18),
            // 0x1C is cost.
            bottom_light_off_delay_s: le_f32(b, 0x20),
            light_off_delay_s: le_f32(b, 0x24),
        }
    }
}

/// One entry in the layer table.
#[derive(Debug, Clone, Copy)]
struct LayerEntry {
    z_mm: f32,
    exposure_s: f32,
    light_off_delay_s: f32,
    data_offset: u32,
    data_size: u32,
}

impl LayerEntry {
    fn parse(b: &[u8]) -> Self {
        Self {
            z_mm: le_f32(b, 0x00),
            exposure_s: le_f32(b, 0x04),
            light_off_delay_s: le_f32(b, 0x08),
            data_offset: le_u32(b, 0x0C),
            data_size: le_u32(b, 0x10),
        }
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

/// Decodes CTB layers on demand.
struct CtbLayers {
    path: PathBuf,
    width: u32,
    height: u32,
    /// Offset and size of each layer's encoded bitmap.
    entries: Vec<(u32, u32)>,
    /// Zero when the layer data is in the clear.
    encryption_key: u32,
}

impl LayerProvider for CtbLayers {
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
        // Reopening per layer keeps the provider Send without a mutex, and the
        // OS page cache makes the repeat opens cheap.
        let mut file = std::fs::File::open(&self.path).map_err(|e| Error::Io {
            path: self.path.clone(),
            source: e,
        })?;
        let want = limits::check_allocation(size as u64)?;
        let mut data = read_at(&mut file, &self.path, offset as u64, want)?;
        // Each layer is enciphered with its own index as the IV.
        uncipher(&mut data, self.encryption_key, index);
        let expected = self.width as usize * self.height as usize;
        let pixels = ctb_rle::decode(&data, expected)
            .map_err(|e| FormatError::LayerDecode(format!("layer {index}: {e}")))?;
        Ok(LayerImage {
            width: self.width,
            height: self.height,
            pixels,
        })
    }
}

/// Read the header and check everything it points at lies inside the file.
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
        header.layers_offset as u64,
        header.layer_count as u64 * LAYER_ENTRY,
        size,
    )?;
    Ok((header, file, size))
}

impl FormatHandler for CtbHandler {
    fn info(&self) -> &'static FormatInfo {
        &INFO
    }

    fn detect(&self, path: &Path, data: &[u8]) -> Detection {
        let extension_matches = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("ctb"))
            .unwrap_or(false);

        if data.len() >= 8 && le_u32(data, 0) == MAGIC {
            let version = le_u32(data, 4);
            if (MIN_VERSION..=MAX_VERSION).contains(&version) {
                return Detection {
                    format_id: ID,
                    confidence: Confidence::High,
                    reason: format!("Chitubox CTB header, version {version}"),
                };
            }
            return Detection {
                format_id: ID,
                confidence: Confidence::Medium,
                reason: format!("Chitubox header, but version {version} is not supported"),
            };
        }
        if extension_matches {
            return Detection {
                format_id: ID,
                confidence: Confidence::Low,
                reason: "named .ctb, but the header does not match".into(),
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
        if header.exposure_s <= 0.0 {
            notes.push(format!(
                "exposure is {} s, which cannot be right",
                header.exposure_s
            ));
        }

        // Every layer must point somewhere inside the file.
        let table = read_at(
            &mut file,
            path,
            header.layers_offset as u64,
            (header.layer_count as u64 * LAYER_ENTRY) as usize,
        )?;
        for i in 0..header.layer_count as usize {
            let entry = LayerEntry::parse(&table[i * LAYER_ENTRY as usize..]);
            if limits::check_range(entry.data_offset as u64, entry.data_size as u64, size).is_err()
            {
                notes.push(format!(
                    "layer {i} points outside the file (offset {}, {} bytes)",
                    entry.data_offset, entry.data_size
                ));
                // One message is enough; a file this broken will have many.
                break;
            }
        }
        Ok(notes)
    }

    fn open(&self, path: &Path) -> Result<OpenedFile> {
        let (header, mut file, size) = open_header(path)?;

        let params = if header.print_params_offset != 0 && header.print_params_size != 0 {
            limits::check_range(
                header.print_params_offset as u64,
                header.print_params_size as u64,
                size,
            )?;
            let want = limits::check_allocation(header.print_params_size as u64)?;
            PrintParams::parse(&read_at(
                &mut file,
                path,
                header.print_params_offset as u64,
                want,
            )?)
        } else {
            PrintParams::default()
        };

        let table = read_at(
            &mut file,
            path,
            header.layers_offset as u64,
            (header.layer_count as u64 * LAYER_ENTRY) as usize,
        )?;

        let mut layers = Vec::with_capacity(header.layer_count as usize);
        let mut entries = Vec::with_capacity(header.layer_count as usize);
        for i in 0..header.layer_count as usize {
            let e = LayerEntry::parse(&table[i * LAYER_ENTRY as usize..]);
            limits::check_range(e.data_offset as u64, e.data_size as u64, size).map_err(|_| {
                FormatError::Other(format!(
                    "layer {i} points outside the file (offset {}, {} bytes)",
                    e.data_offset, e.data_size
                ))
            })?;
            layers.push(LayerInfo {
                z_mm: e.z_mm,
                exposure_s: Some(e.exposure_s),
                light_off_delay_s: Some(e.light_off_delay_s),
                lift_height_mm: None,
                lift_speed_mm_min: None,
            });
            entries.push((e.data_offset, e.data_size));
        }

        // Both previews, when the file has them. A preview that will not
        // decode is dropped rather than failing the open: it is a picture, and
        // losing it should not cost somebody their layers.
        let mut thumbnails = Vec::new();
        for at in [header.preview_large_offset, header.preview_small_offset] {
            if at == 0 {
                continue;
            }
            match read_preview(&mut file, path, at as u64, size) {
                Ok(Some(t)) => thumbnails.push(t),
                Ok(None) => {}
                Err(e) => log::debug!("ctb: preview at {at} ignored: {e}"),
            }
        }

        let mut extra = std::collections::BTreeMap::new();
        extra.insert("ctb_version".into(), header.version.to_string());
        if header.slicer_offset != 0 && header.slicer_size != 0 {
            extra.insert(
                "ctb_slicer_block_bytes".into(),
                header.slicer_size.to_string(),
            );
        }

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
                light_off_delay_s: positive(header.light_off_delay_s)
                    .or(positive(params.light_off_delay_s)),
                bottom_light_off_delay_s: positive(params.bottom_light_off_delay_s),
                transition_layers: None,
                light_pwm: Some(header.light_pwm.min(255) as u8),
                bottom_light_pwm: Some(header.bottom_light_pwm.min(255) as u8),
            },
            lift: Lift {
                lift_height_mm: positive(params.lift_height_mm),
                lift_speed_mm_min: positive(params.lift_speed),
                bottom_lift_height_mm: positive(params.bottom_lift_height_mm),
                bottom_lift_speed_mm_min: positive(params.bottom_lift_speed),
                retract_speed_mm_min: positive(params.retract_speed),
                bottom_retract_speed_mm_min: positive(params.retract_speed),
            },
            layers,
            thumbnails,
            print_time_s: (header.print_time_s > 0).then_some(header.print_time_s as u64),
            material_volume_ml: positive(params.volume_ml),
            material_grams: positive(params.weight_g),
            material_name: None,
            machine_name: None,
            extra,
        };

        Ok(OpenedFile {
            print,
            layers: Box::new(CtbLayers {
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

        // Everything but the layer bitmaps is small, so it is laid out in
        // memory first and the offsets computed from the sizes. The bitmaps
        // are streamed afterwards, since a print can be gigabytes of them.
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

        let params_at = HEADER_BYTES;
        let ext2_at = params_at + EXT_CONFIG_BYTES;
        let big_head_at = ext2_at + EXT2_BYTES;
        let big_data_at = big_head_at + IMAGE_HEADER;
        let small_head_at = big_data_at + big.len();
        let small_data_at = small_head_at + IMAGE_HEADER;
        let table_at = small_data_at + small.len();
        let layers_at = table_at + count as usize * LAYER_ENTRY as usize;

        let mut head = vec![0u8; HEADER_BYTES];
        put_u32(&mut head, 0x00, MAGIC);
        put_u32(&mut head, 0x04, 3);
        put_f32(
            &mut head,
            0x08,
            print.geometry.display_width_mm.unwrap_or(0.0),
        );
        put_f32(
            &mut head,
            0x0C,
            print.geometry.display_height_mm.unwrap_or(0.0),
        );
        put_f32(&mut head, 0x10, print.geometry.machine_z_mm.unwrap_or(0.0));
        put_f32(&mut head, 0x1C, print.height_mm().unwrap_or(0.0));
        put_f32(&mut head, 0x20, e.layer_height_mm);
        put_f32(&mut head, 0x24, e.exposure_s);
        put_f32(&mut head, 0x28, e.bottom_exposure_s.unwrap_or(e.exposure_s));
        put_f32(&mut head, 0x2C, e.light_off_delay_s.unwrap_or(0.0));
        put_u32(&mut head, 0x30, bottom_layers);
        put_u32(&mut head, 0x34, w);
        put_u32(&mut head, 0x38, h);
        put_u32(&mut head, 0x3C, big_head_at as u32);
        put_u32(&mut head, 0x40, table_at as u32);
        put_u32(&mut head, 0x44, count);
        put_u32(&mut head, 0x48, small_head_at as u32);
        put_u32(&mut head, 0x4C, print.print_time_s.unwrap_or(0) as u32);
        put_u32(&mut head, 0x50, 1); // projection: normal rather than mirrored
        put_u32(&mut head, 0x54, params_at as u32);
        put_u32(&mut head, 0x58, EXT_CONFIG_BYTES as u32);
        put_u32(&mut head, 0x5C, 1); // anti-alias level
        put_u16(&mut head, 0x60, e.light_pwm.unwrap_or(255) as u16);
        put_u16(&mut head, 0x62, e.bottom_light_pwm.unwrap_or(255) as u16);
        put_u32(&mut head, 0x64, 0); // no cipher: the slicer's own opt-out
        put_u32(&mut head, 0x68, ext2_at as u32);
        put_u32(&mut head, 0x6C, EXT2_BYTES as u32);

        let mut params = vec![0u8; EXT_CONFIG_BYTES];
        put_f32(&mut params, 0x00, l.bottom_lift_height_mm.unwrap_or(0.0));
        put_f32(&mut params, 0x04, l.bottom_lift_speed_mm_min.unwrap_or(0.0));
        put_f32(&mut params, 0x08, l.lift_height_mm.unwrap_or(0.0));
        put_f32(&mut params, 0x0C, l.lift_speed_mm_min.unwrap_or(0.0));
        put_f32(&mut params, 0x10, l.retract_speed_mm_min.unwrap_or(0.0));
        put_f32(&mut params, 0x14, print.material_volume_ml.unwrap_or(0.0));
        put_f32(&mut params, 0x18, print.material_grams.unwrap_or(0.0));
        put_f32(&mut params, 0x20, e.bottom_light_off_delay_s.unwrap_or(0.0));
        put_f32(&mut params, 0x24, e.light_off_delay_s.unwrap_or(0.0));
        put_u32(&mut params, 0x28, bottom_layers);

        let mut ext2 = vec![0u8; EXT2_BYTES];
        put_u32(&mut ext2, 0x24, 0); // encryption mode: none
        put_u32(&mut ext2, 0x2C, 1); // anti-alias level

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
        out.write_all(&params).map_err(io)?;
        out.write_all(&ext2).map_err(io)?;
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

        // Reserve the layer table before the bitmaps rather than seeking past
        // it. The table's entries can only be written once every payload's
        // length is known, but the space has to exist first: without it the
        // bitmaps start where the table belongs, every offset in the table is
        // one table too far along, and writing the table at the end lands on
        // top of the first layers.
        let mut table = vec![0u8; count as usize * LAYER_ENTRY as usize];
        out.write_all(&table).map_err(io)?;

        // Encoding is the whole cost and every layer is independent, so it runs
        // on a pool while a single writer keeps the file in order.
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
                let (payload, covered) = ctb_rle::encode(&img.pixels);
                // Every pixel must be accounted for. A short layer leaves
                // whatever the printer had in its buffer on the screen.
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
                let off = print
                    .layers
                    .get(index as usize)
                    .and_then(|l| l.light_off_delay_s)
                    .or(e.light_off_delay_s)
                    .unwrap_or(0.0);
                let entry = index as usize * LAYER_ENTRY as usize;
                put_f32(&mut table, entry, z);
                put_f32(&mut table, entry + 0x04, exposure);
                put_f32(&mut table, entry + 0x08, off);
                put_u32(&mut table, entry + 0x0C, at as u32);
                put_u32(&mut table, entry + 0x10, payload.len() as u32);
                at += payload.len();
                out.write_all(&payload).map_err(io)
            },
        )?;

        // The table sits in front of the bitmaps, so it can only be filled in
        // once their lengths are known.
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

/// Read one preview: a small header, then the encoded pixels.
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

/// Undo the keyed XOR stream Chitubox puts over layer data.
///
/// A degenerate linear congruential generator produces a word of keystream at
/// a time, each XORed with a little-endian word of the data. It is symmetric,
/// so this both applies and removes it, and a key of zero means the data is
/// already in the clear.
///
/// The layer's own index is the initialisation vector, so layers cannot be
/// deciphered out of order or in bulk — each needs its own keystream.
///
/// The constant below is `0xD8A8_3423`. Catibo's prose documentation says
/// `0xD8A8_3424`, but its implementation — the one that round trips real
/// files — says `3423`, and an off-by-one there turns every layer into noise.
fn uncipher(data: &mut [u8], key: u32, iv: u32) {
    if key == 0 {
        return;
    }
    let step = key.wrapping_mul(0x2D83_CDAC).wrapping_add(0xD8A8_3423);
    let mut state = iv
        .wrapping_mul(0x1E15_30CD)
        .wrapping_add(0xEC3D_47CD)
        .wrapping_mul(step);
    for chunk in data.chunks_mut(4) {
        // A trailing part-word is treated as the start of a whole one, which
        // works because the cipher carries nothing between bits.
        for (i, byte) in chunk.iter_mut().enumerate() {
            *byte ^= (state >> (i * 8)) as u8;
        }
        state = state.wrapping_add(step);
    }
}

/// A value the format writes as zero when it has nothing to say.
fn positive(v: f32) -> Option<f32> {
    (v.is_finite() && v > 0.0).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keystream words worked out independently from the published recurrence,
    /// so this checks the arithmetic rather than checking the code against
    /// itself. XORing zeroes leaves the keystream in the clear.
    #[test]
    fn the_keystream_matches_the_published_recurrence() {
        let mut data = vec![0u8; 16];
        uncipher(&mut data, 0x1234_5678, 3);
        let words: Vec<u32> = data
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(words, [0xCEB6_859C, 0x232E_EA5F, 0x77A7_4F22, 0xCC1F_B3E5]);

        let mut data = vec![0u8; 12];
        uncipher(&mut data, 1, 0);
        let words: Vec<u32> = data
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(words, [0x6810_DBC3, 0x6E3C_DD92, 0x7468_DF61]);
    }

    #[test]
    fn the_cipher_is_its_own_inverse() {
        let original: Vec<u8> = (0..=200u8).collect();
        let mut data = original.clone();
        uncipher(&mut data, 0xABCD_1234, 7);
        assert_ne!(data, original, "enciphering must change the data");
        uncipher(&mut data, 0xABCD_1234, 7);
        assert_eq!(data, original, "applying it twice must return the original");
    }

    #[test]
    fn a_part_word_at_the_end_is_handled() {
        // Lengths either side of a word boundary, since the tail is the part
        // most likely to be got wrong.
        for len in [0usize, 1, 2, 3, 4, 5, 7, 8, 9] {
            let original: Vec<u8> = (0..len as u8).collect();
            let mut data = original.clone();
            uncipher(&mut data, 0x9999_1111, 2);
            uncipher(&mut data, 0x9999_1111, 2);
            assert_eq!(data, original, "length {len}");
        }
    }

    #[test]
    fn every_layer_gets_a_different_keystream() {
        // The layer index is the IV, so the same plaintext in two layers must
        // not encipher to the same bytes.
        let mut first = vec![0u8; 32];
        let mut second = vec![0u8; 32];
        uncipher(&mut first, 0x2222_3333, 0);
        uncipher(&mut second, 0x2222_3333, 1);
        assert_ne!(first, second);
    }

    #[test]
    fn a_key_of_zero_leaves_the_data_alone() {
        let original: Vec<u8> = (0..64u8).collect();
        let mut data = original.clone();
        uncipher(&mut data, 0, 5);
        assert_eq!(data, original);
    }
}
