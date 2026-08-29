//! PrusaSlicer SL1 (§12).
//!
//! An SL1 file is a ZIP archive holding:
//!
//! ```text
//! config.ini            print settings written by the slicer
//! prusaslicer.ini       the full slicer configuration, including geometry
//! <jobDir>NNNNN.png     one 8-bit greyscale layer per file, zero padded
//! thumbnail/*.png       preview images, when the slicer wrote any
//! ```
//!
//! Layer count comes from counting the PNGs rather than trusting
//! `numFast + numSlow`, because the archive is the authority on what is
//! actually present. The declared count is cross-checked and reported as a
//! validation warning when it disagrees.

use super::ini;
use crate::error::{Error, FormatError, Result};
use crate::format::{Capabilities, Confidence, Detection, FormatHandler, FormatInfo, OpenedFile};
use crate::layers::LayerProvider;
use crate::limits;
use crate::model::*;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const ID: &str = "sl1";

static INFO: FormatInfo = FormatInfo {
    id: ID,
    name: "PrusaSlicer SL1",
    extension: "sl1",
    aliases: &["sl1s"],
    description: "The archive PrusaSlicer writes for SLA printers. A ZIP holding \
                  one greyscale PNG per layer plus the slicer's settings, which makes \
                  it the most inspectable of the resin formats.",
    limitations: &[
        "Stores one exposure time for the whole print, not per layer",
        "Records no lift or retract speeds",
        "Larger than binary formats, since layers are PNG rather than run-length encoded",
    ],
    capabilities: Capabilities {
        reads: true,
        writes: true,
        per_layer_exposure: false,
        per_layer_lift: false,
        thumbnails: true,
        max_thumbnails: 2,
        print_time: true,
        material_volume: true,
        machine_name: true,
    },
};

const ZIP_MAGIC: &[u8; 4] = b"PK\x03\x04";

pub struct Sl1Handler;

/// Names of the layer entries, in print order, plus the archive path.
struct Sl1Layers {
    path: PathBuf,
    entries: Vec<String>,
    width: u32,
    height: u32,
}

impl LayerProvider for Sl1Layers {
    fn layer_count(&self) -> u32 {
        self.entries.len() as u32
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn layer(&self, index: u32) -> Result<LayerImage> {
        let name = self
            .entries
            .get(index as usize)
            .ok_or(Error::LayerOutOfRange {
                index,
                count: self.entries.len() as u32,
            })?;
        // Reopening per layer keeps the provider Send without a mutex, and the
        // OS page cache makes the repeat opens cheap.
        let file = File::open(&self.path).map_err(|e| Error::Io {
            path: self.path.clone(),
            source: e,
        })?;
        let mut zip = zip::ZipArchive::new(file)
            .map_err(|e| FormatError::Other(format!("archive could not be opened: {e}")))?;
        let mut entry = zip.by_name(name).map_err(|e| {
            FormatError::Other(format!("layer {index} is missing from the archive: {e}"))
        })?;
        let declared = entry.size();
        let cap = limits::check_allocation(declared)?;
        let mut buf = Vec::with_capacity(cap.min(8 * 1024 * 1024));
        entry
            .read_to_end(&mut buf)
            .map_err(|e| FormatError::LayerDecode(format!("layer {index}: {e}")))?;
        decode_png_grey(&buf, index)
    }
}

/// Decode a PNG into 8-bit greyscale.
///
/// PrusaSlicer writes 8-bit greyscale, but the decoder accepts the other
/// colour types too rather than rejecting a file that is otherwise fine.
fn decode_png_grey(bytes: &[u8], index: u32) -> Result<LayerImage> {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder
        .read_info()
        .map_err(|e| FormatError::LayerDecode(format!("layer {index}: {e}")))?;
    let info = reader.info();
    let (w, h) = (info.width, info.height);
    limits::check_resolution(w, h)?;

    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|e| FormatError::LayerDecode(format!("layer {index}: {e}")))?;
    let bytes_used = &buf[..frame.buffer_size()];

    let pixels = match (frame.color_type, frame.bit_depth) {
        (png::ColorType::Grayscale, png::BitDepth::Eight) => bytes_used.to_vec(),
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => {
            bytes_used.chunks_exact(2).map(|p| p[0]).collect()
        }
        (png::ColorType::Rgb, png::BitDepth::Eight) => bytes_used
            .chunks_exact(3)
            .map(|p| luma(p[0], p[1], p[2]))
            .collect(),
        (png::ColorType::Rgba, png::BitDepth::Eight) => bytes_used
            .chunks_exact(4)
            .map(|p| luma(p[0], p[1], p[2]))
            .collect(),
        (ct, bd) => {
            return Err(FormatError::LayerDecode(format!(
                "layer {index} uses an unsupported PNG encoding ({ct:?}, {bd:?})"
            ))
            .into())
        }
    };

    let expected = w as usize * h as usize;
    if pixels.len() != expected {
        return Err(FormatError::LayerDecode(format!(
            "layer {index} decoded to {} pixels but its header declares {expected}",
            pixels.len()
        ))
        .into());
    }
    Ok(LayerImage {
        width: w,
        height: h,
        pixels,
    })
}

/// Job names end up in entry names, so keep them to characters that survive a
/// round trip through an archive and a printer's file browser.
fn sanitise_job_name(stem: &str) -> String {
    let cleaned: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "print".to_string()
    } else {
        cleaned
    }
}

fn encode_grey_png(img: &LayerImage) -> std::result::Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header()?;
        w.write_image_data(&img.pixels)?;
    }
    Ok(out)
}

fn encode_rgb_png(t: &Thumbnail) -> std::result::Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, t.width, t.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header()?;
        w.write_image_data(&t.rgb)?;
    }
    Ok(out)
}

fn luma(r: u8, g: u8, b: u8) -> u8 {
    // Rec. 601 luma, integer maths to avoid float rounding drift.
    ((r as u16 * 299 + g as u16 * 587 + b as u16 * 114) / 1000) as u8
}

/// What a scan of an SL1 archive yields: layer entry names in print order,
/// thumbnail entry names, and the two settings files already parsed.
type ArchiveIndex = (
    Vec<String>,
    Vec<String>,
    BTreeMap<String, String>,
    BTreeMap<String, String>,
);

/// Layer entry names in print order, and the thumbnails, from an archive.
fn index_archive(
    path: &Path,
) -> Result<(
    Vec<String>,
    Vec<String>,
    BTreeMap<String, String>,
    BTreeMap<String, String>,
)> {
    let file = File::open(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| FormatError::BadMagic)?;

    let mut layers: Vec<String> = Vec::new();
    let mut thumbs: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|e| FormatError::Other(format!("archive entry {i} is unreadable: {e}")))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let lower = name.to_ascii_lowercase();
        if !lower.ends_with(".png") {
            continue;
        }
        if lower.starts_with("thumbnail") || lower.contains("/thumbnail") {
            thumbs.push(name);
        } else {
            layers.push(name);
        }
    }
    // Zero-padded names sort correctly as strings, which is why the slicer
    // pads them. Sorting explicitly means archive order cannot mislead us.
    layers.sort();
    thumbs.sort();

    let config = read_text(&mut zip, "config.ini")?;
    let slicer = read_text(&mut zip, "prusaslicer.ini").unwrap_or_default();
    Ok((layers, thumbs, ini::parse(&config), ini::parse(&slicer)))
}

fn read_text<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<String> {
    let mut entry = zip
        .by_name(name)
        .map_err(|_| FormatError::MissingField(name.to_string()))?;
    let cap = limits::check_allocation(entry.size())?;
    let mut s = String::with_capacity(cap.min(1024 * 1024));
    entry
        .read_to_string(&mut s)
        .map_err(|e| FormatError::Other(format!("{name} is not readable as text: {e}")))?;
    Ok(s)
}

impl FormatHandler for Sl1Handler {
    fn info(&self) -> &'static FormatInfo {
        &INFO
    }

    fn detect(&self, path: &Path, data: &[u8]) -> Detection {
        let ext_matches = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| {
                let e = e.to_ascii_lowercase();
                e == "sl1" || e == "sl1s"
            })
            .unwrap_or(false);

        if data.len() < 4 || &data[..4] != ZIP_MAGIC {
            return Detection {
                format_id: ID,
                confidence: Confidence::None,
                reason: "Not a ZIP archive, so it cannot be an SL1 file".into(),
            };
        }

        // The extension alone is never enough (§11): confirm the archive
        // actually carries an SL1 payload.
        match index_archive(path) {
            Ok((layers, _, config, _)) if !layers.is_empty() && !config.is_empty() => Detection {
                format_id: ID,
                confidence: Confidence::High,
                reason: format!(
                    "ZIP archive containing config.ini and {} layer images",
                    layers.len()
                ),
            },
            Ok((layers, _, config, _)) if !config.is_empty() => Detection {
                format_id: ID,
                confidence: Confidence::Medium,
                reason: format!(
                    "ZIP archive with config.ini but {} layer images",
                    layers.len()
                ),
            },
            _ if ext_matches => Detection {
                format_id: ID,
                confidence: Confidence::Low,
                reason: "Named like an SL1 file, but its contents do not look like one".into(),
            },
            _ => Detection {
                format_id: ID,
                confidence: Confidence::None,
                reason: "A ZIP archive, but not an SL1 payload".into(),
            },
        }
    }

    fn validate(&self, path: &Path) -> Result<Vec<String>> {
        let (layers, _, config, _) = index_archive(path)?;
        let mut warnings = Vec::new();

        if layers.is_empty() {
            return Err(FormatError::MissingField("layer images".into()).into());
        }
        limits::check_layer_count(layers.len() as u64)?;

        // numFast + numSlow is what the slicer intended to write. Disagreeing
        // with the archive is worth reporting but not worth refusing: the
        // images present are what will actually print.
        let fast: u32 = ini::get(&config, "numFast").unwrap_or(0);
        let slow: u32 = ini::get(&config, "numSlow").unwrap_or(0);
        let declared = fast + slow;
        if declared != 0 && declared as usize != layers.len() {
            warnings.push(format!(
                "config.ini declares {declared} layers but the archive holds {}",
                layers.len()
            ));
        }
        if ini::get::<f32>(&config, "layerHeight").is_none() {
            warnings.push("config.ini does not record a layer height".into());
        }
        if ini::get::<f32>(&config, "expTime").is_none() {
            warnings.push("config.ini does not record an exposure time".into());
        }
        Ok(warnings)
    }

    fn open(&self, path: &Path) -> Result<OpenedFile> {
        let (layer_names, thumb_names, config, slicer) = index_archive(path)?;
        if layer_names.is_empty() {
            return Err(FormatError::MissingField("layer images".into()).into());
        }
        limits::check_layer_count(layer_names.len() as u64)?;

        // Geometry lives in prusaslicer.ini. Fall back to the first layer's
        // own header when that file is absent, since the bitmap cannot lie
        // about its own size.
        let (mut res_x, mut res_y) = (
            ini::get::<u32>(&slicer, "display_pixels_x").unwrap_or(0),
            ini::get::<u32>(&slicer, "display_pixels_y").unwrap_or(0),
        );
        let probe = Sl1Layers {
            path: path.to_path_buf(),
            entries: layer_names.clone(),
            width: res_x,
            height: res_y,
        };
        if res_x == 0 || res_y == 0 {
            let first = probe.layer(0)?;
            res_x = first.width;
            res_y = first.height;
        }
        limits::check_resolution(res_x, res_y)?;

        let layer_height: f32 = ini::get(&config, "layerHeight")
            .ok_or_else(|| FormatError::MissingField("layerHeight in config.ini".to_string()))?;
        if !(layer_height.is_finite() && layer_height > 0.0) {
            return Err(FormatError::InvalidValue {
                field: "layerHeight".into(),
                value: layer_height.to_string(),
                reason: "must be a positive number".into(),
            }
            .into());
        }
        let exposure_s: f32 = ini::get(&config, "expTime")
            .ok_or_else(|| FormatError::MissingField("expTime in config.ini".to_string()))?;

        let layers: Vec<LayerInfo> = (0..layer_names.len())
            .map(|i| LayerInfo {
                z_mm: layer_height * (i + 1) as f32,
                exposure_s: None,
                light_off_delay_s: None,
                lift_height_mm: None,
                lift_speed_mm_min: None,
            })
            .collect();

        // Anything from config.ini the model has no field for is preserved so
        // conversions can report what they drop (§14).
        let mut extra = BTreeMap::new();
        for (k, v) in &config {
            if !matches!(
                k.as_str(),
                "layerHeight"
                    | "expTime"
                    | "expTimeFirst"
                    | "numFade"
                    | "numFast"
                    | "numSlow"
                    | "printTime"
                    | "usedMaterial"
                    | "printerModel"
                    | "materialName"
                    | "jobDir"
            ) && !v.is_empty()
            {
                extra.insert(format!("sl1.{k}"), v.clone());
            }
        }

        let print = PrintFile {
            source_format: ID.to_string(),
            geometry: Geometry {
                resolution_x: res_x,
                resolution_y: res_y,
                display_width_mm: ini::get(&slicer, "display_width"),
                display_height_mm: ini::get(&slicer, "display_height"),
                machine_z_mm: ini::get(&slicer, "max_print_height"),
            },
            exposure: Exposure {
                layer_height_mm: layer_height,
                exposure_s,
                bottom_exposure_s: ini::get(&config, "expTimeFirst"),
                bottom_layers: ini::get(&config, "numSlow"),
                light_off_delay_s: None,
                bottom_light_off_delay_s: None,
                transition_layers: ini::get(&config, "numFade"),
                light_pwm: None,
                bottom_light_pwm: None,
            },
            lift: Lift::default(),
            layers,
            thumbnails: read_thumbnails(path, &thumb_names),
            print_time_s: ini::get::<f64>(&config, "printTime").map(|t| t.round() as u64),
            material_volume_ml: ini::get(&config, "usedMaterial"),
            material_grams: None,
            material_name: ini::get_str(&config, "materialName"),
            machine_name: ini::get_str(&config, "printerModel"),
            extra,
        };

        Ok(OpenedFile {
            print,
            layers: Box::new(Sl1Layers {
                path: path.to_path_buf(),
                entries: layer_names,
                width: res_x,
                height: res_y,
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

        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "print".to_string());
        // PrusaSlicer names layers after jobDir, and the printer matches them
        // by prefix, so the two must agree.
        let job = sanitise_job_name(&stem);

        let e = &print.exposure;
        let bottom_layers = e.bottom_layers.unwrap_or(0);
        let fast = count.saturating_sub(bottom_layers);

        let file = std::fs::File::create(path).map_err(|err| Error::Io {
            path: path.to_path_buf(),
            source: err,
        })?;
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let zerr = |err: zip::result::ZipError| -> Error {
            FormatError::Other(format!("could not write the archive: {err}")).into()
        };

        // config.ini. Only values we actually have are written; a field we
        // cannot fill is left out rather than given an invented default.
        let mut cfg = String::new();
        cfg.push_str("action = print\n");
        cfg.push_str(&format!("jobDir = {job}\n"));
        cfg.push_str(&format!("expTime = {}\n", e.exposure_s));
        if let Some(v) = e.bottom_exposure_s {
            cfg.push_str(&format!("expTimeFirst = {v}\n"));
        }
        cfg.push_str(&format!("layerHeight = {}\n", e.layer_height_mm));
        cfg.push_str(&format!("numFade = {}\n", e.transition_layers.unwrap_or(0)));
        cfg.push_str(&format!("numFast = {fast}\n"));
        cfg.push_str(&format!("numSlow = {bottom_layers}\n"));
        if let Some(t) = print.print_time_s {
            cfg.push_str(&format!("printTime = {t}\n"));
        }
        if let Some(v) = print.material_volume_ml {
            cfg.push_str(&format!("usedMaterial = {v}\n"));
        }
        if let Some(n) = &print.material_name {
            cfg.push_str(&format!("materialName = {n}\n"));
        }
        if let Some(n) = &print.machine_name {
            cfg.push_str(&format!("printerModel = {n}\n"));
        }
        cfg.push_str("prusaSlicerVersion = CheapAzSLA\n");
        // Values carried from the source that this model has no field for.
        for (k, v) in &print.extra {
            if let Some(key) = k.strip_prefix("sl1.") {
                cfg.push_str(&format!("{key} = {v}\n"));
            }
        }
        zip.start_file("config.ini", opts).map_err(zerr)?;
        zip.write_all(cfg.as_bytes()).map_err(|err| Error::Io {
            path: path.to_path_buf(),
            source: err,
        })?;

        // prusaslicer.ini carries the geometry a reader needs.
        let mut slicer = String::new();
        slicer.push_str(&format!("display_pixels_x = {w}\n"));
        slicer.push_str(&format!("display_pixels_y = {h}\n"));
        if let Some(v) = print.geometry.display_width_mm {
            slicer.push_str(&format!("display_width = {v}\n"));
        }
        if let Some(v) = print.geometry.display_height_mm {
            slicer.push_str(&format!("display_height = {v}\n"));
        }
        if let Some(v) = print.geometry.machine_z_mm {
            slicer.push_str(&format!("max_print_height = {v}\n"));
        }
        slicer.push_str(&format!("layer_height = {}\n", e.layer_height_mm));
        slicer.push_str(&format!("exposure_time = {}\n", e.exposure_s));
        if let Some(v) = e.bottom_exposure_s {
            slicer.push_str(&format!("initial_exposure_time = {v}\n"));
        }
        slicer.push_str("printer_technology = SLA\n");
        zip.start_file("prusaslicer.ini", opts).map_err(zerr)?;
        zip.write_all(slicer.as_bytes()).map_err(|err| Error::Io {
            path: path.to_path_buf(),
            source: err,
        })?;

        // Thumbnails, at the sizes PrusaSlicer uses.
        for (i, t) in print.thumbnails.iter().take(2).enumerate() {
            let name = format!("thumbnail/thumbnail{}x{}.png", t.width, t.height);
            let _ = i;
            if let Ok(png) = encode_rgb_png(t) {
                zip.start_file(name, opts).map_err(zerr)?;
                zip.write_all(&png).map_err(|err| Error::Io {
                    path: path.to_path_buf(),
                    source: err,
                })?;
            }
        }

        // Layers. Zero padded to five digits so lexical order is print order.
        for index in 0..count {
            let img = layers.layer(index)?;
            if img.width != w || img.height != h {
                return Err(FormatError::Other(format!(
                    "layer {index} is {}x{} but the print is {w}x{h}",
                    img.width, img.height
                ))
                .into());
            }
            let png = encode_grey_png(&img).map_err(|err| {
                FormatError::Other(format!("layer {index} could not be encoded: {err}"))
            })?;
            // Layer PNGs are already deflate-compressed, so storing them
            // uncompressed avoids paying for a second pass that gains nothing.
            let store: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file(format!("{job}{index:05}.png"), store)
                .map_err(zerr)?;
            zip.write_all(&png).map_err(|err| Error::Io {
                path: path.to_path_buf(),
                source: err,
            })?;
        }

        zip.finish().map_err(zerr)?;
        Ok(())
    }
}

/// Best-effort thumbnail decode. A broken preview is not worth failing an
/// otherwise readable file over, so failures are skipped silently.
fn read_thumbnails(path: &Path, names: &[String]) -> Vec<Thumbnail> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in names {
        let Ok(mut entry) = zip.by_name(name) else {
            continue;
        };
        if limits::check_allocation(entry.size()).is_err() {
            continue;
        }
        let mut buf = Vec::new();
        if entry.read_to_end(&mut buf).is_err() {
            continue;
        }
        if let Ok(t) = decode_thumbnail(&buf) {
            out.push(t);
        }
    }
    out
}

fn decode_thumbnail(bytes: &[u8]) -> Result<Thumbnail> {
    let mut reader = png::Decoder::new(bytes)
        .read_info()
        .map_err(|e| FormatError::Other(e.to_string()))?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w as u64 * h as u64 > limits::MAX_THUMBNAIL_PIXELS {
        return Err(FormatError::Other("thumbnail is implausibly large".into()).into());
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|e| FormatError::Other(e.to_string()))?;
    let used = &buf[..frame.buffer_size()];
    let rgb = match frame.color_type {
        png::ColorType::Rgb => used.to_vec(),
        png::ColorType::Rgba => used
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        png::ColorType::Grayscale => used.iter().flat_map(|&g| [g, g, g]).collect(),
        png::ColorType::GrayscaleAlpha => used
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0]])
            .collect(),
        _ => return Err(FormatError::Other("unsupported thumbnail encoding".into()).into()),
    };
    Ok(Thumbnail {
        width: w,
        height: h,
        rgb,
    })
}
