//! UVJ, an open interchange format (phase 20, tier 2).
//!
//! A ZIP holding a JSON manifest, two preview images and one 8-bit greyscale
//! PNG per layer. Nothing is packed, obfuscated or reverse engineered: the
//! layout is published, and anyone can open a file with an unzip tool and look
//! at what is actually in it.
//!
//! That makes it the useful thing to convert *to* when a print has gone wrong
//! and nobody can say why, and the natural lossless middle step between two
//! formats that each drop something different. It is also the easiest format
//! here to test against, being the only one whose contents can be checked
//! without trusting any of this code.
//!
//! ```text
//! config.json          the manifest
//! preview/tiny.png     small preview
//! preview/huge.png     large preview
//! slice/00000000.png   one per layer, eight digits, in print order
//! ```

use super::sl1::decode_png_grey;
use crate::error::{Error, FormatError, Result};
use crate::format::{Capabilities, Confidence, Detection, FormatHandler, FormatInfo, OpenedFile};
use crate::layers::LayerProvider;
use crate::limits;
use crate::model::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const ID: &str = "uvj";

const MANIFEST: &str = "config.json";

static INFO: FormatInfo = FormatInfo {
    id: ID,
    name: "UVJ",
    extension: "uvj",
    aliases: &[],
    description: "An open interchange format: a ZIP of greyscale PNG layers and a JSON \
                  manifest. Nothing is packed or obfuscated, so a file can be opened with \
                  an unzip tool and read directly.",
    limitations: &[
        "Larger than the binary formats, since layers are PNG rather than run-length encoded",
        "Not a format any printer reads: it is for looking at prints and moving them about",
    ],
    capabilities: Capabilities {
        reads: true,
        writes: true,
        per_layer_exposure: true,
        per_layer_lift: false,
        thumbnails: true,
        max_thumbnails: 2,
        print_time: false,
        material_volume: false,
        machine_name: false,
    },
};

pub struct UvjHandler;

fn layer_name(index: u32) -> String {
    format!("slice/{index:08}.png")
}

/// Read a number out of the manifest, whatever shape it arrived in.
///
/// Written by other tools as well as this one, so an integer where a float was
/// expected is a difference in taste rather than a broken file.
fn num(v: &Value, path: &[&str]) -> Option<f64> {
    let mut at = v;
    for key in path {
        at = at.get(key)?;
    }
    at.as_f64()
}

fn positive(v: Option<f64>) -> Option<f32> {
    v.filter(|n| n.is_finite() && *n > 0.0).map(|n| n as f32)
}

struct UvjLayers {
    path: PathBuf,
    width: u32,
    height: u32,
    count: u32,
}

impl LayerProvider for UvjLayers {
    fn layer_count(&self) -> u32 {
        self.count
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn layer(&self, index: u32) -> Result<LayerImage> {
        if index >= self.count {
            return Err(Error::LayerOutOfRange {
                index,
                count: self.count,
            });
        }
        // Reopening per layer keeps the provider Send without a mutex, and the
        // page cache makes the repeat opens cheap.
        let file = std::fs::File::open(&self.path).map_err(|e| Error::Io {
            path: self.path.clone(),
            source: e,
        })?;
        let mut zip = zip::ZipArchive::new(file)
            .map_err(|e| FormatError::Other(format!("archive could not be opened: {e}")))?;
        let name = layer_name(index);
        let mut entry = zip.by_name(&name).map_err(|_| {
            FormatError::LayerDecode(format!("layer {index} is missing from the archive"))
        })?;
        let cap = limits::check_allocation(entry.size())?;
        let mut buf = Vec::with_capacity(cap.min(8 * 1024 * 1024));
        entry
            .read_to_end(&mut buf)
            .map_err(|e| FormatError::LayerDecode(format!("layer {index}: {e}")))?;
        let img = decode_png_grey(&buf, index)?;
        if img.width != self.width || img.height != self.height {
            return Err(FormatError::LayerDecode(format!(
                "layer {index} is {}x{} but the manifest says {}x{}",
                img.width, img.height, self.width, self.height
            ))
            .into());
        }
        Ok(img)
    }
}

fn read_manifest(path: &Path) -> Result<Value> {
    let file = std::fs::File::open(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| FormatError::Other(format!("archive could not be opened: {e}")))?;
    let entry = zip
        .by_name(MANIFEST)
        .map_err(|_| FormatError::MissingField(MANIFEST.into()))?;
    let cap = limits::check_allocation(entry.size())?;
    let mut text = String::new();
    entry
        .take(cap as u64)
        .read_to_string(&mut text)
        .map_err(|e| FormatError::Other(format!("{MANIFEST} could not be read: {e}")))?;
    serde_json::from_str(&text)
        .map_err(|e| FormatError::Other(format!("{MANIFEST} is not valid JSON: {e}")).into())
}

impl FormatHandler for UvjHandler {
    fn info(&self) -> &'static FormatInfo {
        &INFO
    }

    fn detect(&self, path: &Path, data: &[u8]) -> Detection {
        let named = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("uvj"))
            .unwrap_or(false);
        // A ZIP, like SL1, so the contents rather than the magic decide.
        if data.len() < 4 || &data[..2] != b"PK" {
            return Detection {
                format_id: ID,
                confidence: Confidence::None,
                reason: String::new(),
            };
        }
        match read_manifest(path) {
            Ok(v) if v.get("Properties").is_some() => Detection {
                format_id: ID,
                confidence: Confidence::High,
                reason: "ZIP archive holding a UVJ manifest".into(),
            },
            _ if named => Detection {
                format_id: ID,
                confidence: Confidence::Low,
                reason: "named .uvj, but it holds no UVJ manifest".into(),
            },
            _ => Detection {
                format_id: ID,
                confidence: Confidence::None,
                reason: String::new(),
            },
        }
    }

    fn validate(&self, path: &Path) -> Result<Vec<String>> {
        let manifest = read_manifest(path)?;
        let mut notes = Vec::new();
        let count = num(&manifest, &["Properties", "Size", "Layers"]).unwrap_or(0.0) as u32;
        if count == 0 {
            notes.push("the manifest declares no layers".into());
        }
        let listed = manifest
            .get("Layers")
            .and_then(|l| l.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if listed != count as usize {
            notes.push(format!(
                "the manifest says {count} layers but lists {listed} of them"
            ));
        }
        let file = std::fs::File::open(path).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        if let Ok(mut zip) = zip::ZipArchive::new(file) {
            for i in 0..count {
                if zip.by_name(&layer_name(i)).is_err() {
                    notes.push(format!("layer {i} is missing from the archive"));
                    break;
                }
            }
        }
        Ok(notes)
    }

    fn open(&self, path: &Path) -> Result<OpenedFile> {
        let manifest = read_manifest(path)?;
        let size = ["Properties", "Size"];
        let w = num(&manifest, &[size[0], size[1], "X"])
            .ok_or_else(|| FormatError::MissingField("Properties.Size.X".into()))?
            as u32;
        let h = num(&manifest, &[size[0], size[1], "Y"])
            .ok_or_else(|| FormatError::MissingField("Properties.Size.Y".into()))?
            as u32;
        limits::check_resolution(w, h)?;
        let count = num(&manifest, &[size[0], size[1], "Layers"]).unwrap_or(0.0) as u32;
        let count = limits::check_layer_count(count as u64)?;
        let layer_height = num(&manifest, &[size[0], size[1], "LayerHeight"]).unwrap_or(0.0) as f32;

        let exposure_s = num(&manifest, &["Properties", "Exposure", "LightOnTime"]).unwrap_or(0.0);
        let bottom_count = num(&manifest, &["Properties", "Bottom", "Count"]).unwrap_or(0.0) as u32;

        let mut layers = Vec::with_capacity(count as usize);
        let listed = manifest.get("Layers").and_then(|l| l.as_array());
        for i in 0..count as usize {
            let entry = listed.and_then(|a| a.get(i));
            let z = entry
                .and_then(|e| num(e, &["Z"]))
                .unwrap_or_else(|| (layer_height as f64) * (i + 1) as f64);
            layers.push(LayerInfo {
                z_mm: z as f32,
                exposure_s: entry
                    .and_then(|e| num(e, &["Exposure", "LightOnTime"]))
                    .map(|v| v as f32),
                light_off_delay_s: entry
                    .and_then(|e| num(e, &["Exposure", "LightOffTime"]))
                    .map(|v| v as f32),
                lift_height_mm: None,
                lift_speed_mm_min: None,
            });
        }

        let mut thumbnails = Vec::new();
        if let Ok(file) = std::fs::File::open(path) {
            if let Ok(mut zip) = zip::ZipArchive::new(file) {
                for name in ["preview/huge.png", "preview/tiny.png"] {
                    let Ok(mut entry) = zip.by_name(name) else {
                        continue;
                    };
                    let Ok(cap) = limits::check_allocation(entry.size()) else {
                        continue;
                    };
                    let mut buf = Vec::with_capacity(cap.min(8 * 1024 * 1024));
                    if entry.read_to_end(&mut buf).is_err() {
                        continue;
                    }
                    // A preview is decoration; a broken one is dropped rather
                    // than costing somebody their layers.
                    if let Some(t) = decode_preview(&buf) {
                        thumbnails.push(t);
                    }
                }
            }
        }

        let print = PrintFile {
            source_format: ID.into(),
            geometry: Geometry {
                resolution_x: w,
                resolution_y: h,
                display_width_mm: positive(num(&manifest, &[size[0], size[1], "Millimeter", "X"])),
                display_height_mm: positive(num(&manifest, &[size[0], size[1], "Millimeter", "Y"])),
                machine_z_mm: None,
            },
            exposure: Exposure {
                layer_height_mm: layer_height,
                exposure_s: exposure_s as f32,
                bottom_exposure_s: positive(num(
                    &manifest,
                    &["Properties", "Bottom", "LightOnTime"],
                )),
                bottom_layers: Some(bottom_count),
                light_off_delay_s: positive(num(
                    &manifest,
                    &["Properties", "Exposure", "LightOffTime"],
                )),
                bottom_light_off_delay_s: positive(num(
                    &manifest,
                    &["Properties", "Bottom", "LightOffTime"],
                )),
                transition_layers: None,
                light_pwm: num(&manifest, &["Properties", "Exposure", "LightPWM"])
                    .map(|v| v.clamp(0.0, 255.0) as u8),
                bottom_light_pwm: num(&manifest, &["Properties", "Bottom", "LightPWM"])
                    .map(|v| v.clamp(0.0, 255.0) as u8),
            },
            lift: Lift {
                lift_height_mm: positive(num(&manifest, &["Properties", "Exposure", "LiftHeight"])),
                lift_speed_mm_min: positive(num(
                    &manifest,
                    &["Properties", "Exposure", "LiftSpeed"],
                )),
                bottom_lift_height_mm: positive(num(
                    &manifest,
                    &["Properties", "Bottom", "LiftHeight"],
                )),
                bottom_lift_speed_mm_min: positive(num(
                    &manifest,
                    &["Properties", "Bottom", "LiftSpeed"],
                )),
                retract_speed_mm_min: positive(num(
                    &manifest,
                    &["Properties", "Exposure", "RetractSpeed"],
                )),
                bottom_retract_speed_mm_min: positive(num(
                    &manifest,
                    &["Properties", "Bottom", "RetractSpeed"],
                )),
            },
            layers,
            thumbnails,
            print_time_s: None,
            material_volume_ml: None,
            material_grams: None,
            material_name: None,
            machine_name: None,
            extra: std::collections::BTreeMap::new(),
        };

        Ok(OpenedFile {
            print,
            layers: Box::new(UvjLayers {
                path: path.to_path_buf(),
                width: w,
                height: h,
                count,
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
        let io = |err: std::io::Error| Error::Io {
            path: path.to_path_buf(),
            source: err,
        };

        let file = std::fs::File::create(path).map_err(io)?;
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
        // Stored rather than deflated: PNG is already compressed, so deflating
        // it again costs time and saves nothing worth having.
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let deflated = zip::write::SimpleFileOptions::default();
        let zip_err =
            |err: zip::result::ZipError| Error::from(FormatError::Other(format!("writing: {err}")));

        let exposure_block = |on: f32,
                              off: f32,
                              pwm: u8,
                              lift: Option<f32>,
                              speed: Option<f32>,
                              retract: Option<f32>| {
            json!({
                "WaitTimeBeforeCure": 0,
                "LightOffTime": off,
                "LightOnTime": on,
                "LightPWM": pwm,
                "WaitTimeAfterCure": 0,
                "LiftHeight": lift.unwrap_or(0.0),
                "LiftSpeed": speed.unwrap_or(0.0),
                "LiftHeight2": 0,
                "LiftSpeed2": 0,
                "WaitTimeAfterLift": 0,
                "RetractHeight": lift.unwrap_or(0.0),
                "RetractSpeed": retract.unwrap_or(0.0),
                "RetractHeight2": 0,
                "RetractSpeed2": 0
            })
        };

        let bottom_layers = e.bottom_layers.unwrap_or(0);
        let normal = exposure_block(
            e.exposure_s,
            e.light_off_delay_s.unwrap_or(0.0),
            e.light_pwm.unwrap_or(255),
            l.lift_height_mm,
            l.lift_speed_mm_min,
            l.retract_speed_mm_min,
        );
        let bottom = exposure_block(
            e.bottom_exposure_s.unwrap_or(e.exposure_s),
            e.bottom_light_off_delay_s.unwrap_or(0.0),
            e.bottom_light_pwm.unwrap_or(255),
            l.bottom_lift_height_mm,
            l.bottom_lift_speed_mm_min,
            l.bottom_retract_speed_mm_min,
        );

        let mut layer_entries = Vec::with_capacity(count as usize);
        for i in 0..count {
            let z = print
                .layers
                .get(i as usize)
                .map(|l| l.z_mm)
                .unwrap_or_else(|| e.layer_height_mm * (i + 1) as f32);
            let block = if i < bottom_layers {
                bottom.clone()
            } else {
                normal.clone()
            };
            layer_entries.push(json!({ "Z": z, "Exposure": block }));
        }

        let mut properties = json!({
            "Size": {
                "X": w,
                "Y": h,
                "Millimeter": {
                    "X": print.geometry.display_width_mm.unwrap_or(0.0),
                    "Y": print.geometry.display_height_mm.unwrap_or(0.0)
                },
                "Layers": count,
                "LayerHeight": e.layer_height_mm
            },
            "Exposure": normal,
            "Bottom": {},
            "Vendor": {},
            "AntiAliasLevel": 1
        });
        // The bottom block carries a layer count the others do not.
        if let Some(map) = bottom.as_object() {
            let mut with_count = map.clone();
            with_count.insert("Count".into(), json!(bottom_layers));
            properties["Bottom"] = Value::Object(with_count);
        }
        let manifest = json!({ "Properties": properties, "Layers": layer_entries });

        zip.start_file(MANIFEST, deflated).map_err(zip_err)?;
        zip.write_all(serde_json::to_string_pretty(&manifest).unwrap().as_bytes())
            .map_err(io)?;

        for (name, thumb) in [
            (
                "preview/huge.png",
                print.thumbnails.iter().max_by_key(|t| t.pixel_count()),
            ),
            (
                "preview/tiny.png",
                print.thumbnails.iter().min_by_key(|t| t.pixel_count()),
            ),
        ] {
            let Some(t) = thumb else { continue };
            let Some(png) = encode_preview(t) else {
                continue;
            };
            zip.start_file(name, stored).map_err(zip_err)?;
            zip.write_all(&png).map_err(io)?;
        }

        // Encoding the PNGs is the whole cost and every layer is independent,
        // so it runs on a pool while a single writer keeps the archive in
        // order.
        let workers = crate::pipeline::workers_for(w as u64 * h as u64);
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
                let mut out = Vec::new();
                {
                    let mut enc = png::Encoder::new(&mut out, w, h);
                    enc.set_color(png::ColorType::Grayscale);
                    enc.set_depth(png::BitDepth::Eight);
                    let mut writer = enc
                        .write_header()
                        .map_err(|e| FormatError::Other(format!("layer {index}: {e}")))?;
                    writer
                        .write_image_data(&img.pixels)
                        .map_err(|e| FormatError::Other(format!("layer {index}: {e}")))?;
                }
                Ok(out)
            },
            |index, png_bytes| {
                zip.start_file(layer_name(index), stored).map_err(zip_err)?;
                zip.write_all(&png_bytes).map_err(io)
            },
        )?;

        zip.finish().map_err(zip_err)?;
        Ok(())
    }
}

fn decode_preview(bytes: &[u8]) -> Option<Thumbnail> {
    let mut reader = png::Decoder::new(bytes).read_info().ok()?;
    let (w, h) = (reader.info().width, reader.info().height);
    limits::check_thumbnail(w, h).ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).ok()?;
    let used = &buf[..frame.buffer_size()];
    let rgb: Vec<u8> = match frame.color_type {
        png::ColorType::Rgb => used.to_vec(),
        png::ColorType::Rgba => used
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        png::ColorType::Grayscale => used.iter().flat_map(|&g| [g, g, g]).collect(),
        _ => return None,
    };
    (rgb.len() == (w * h * 3) as usize).then_some(Thumbnail {
        width: w,
        height: h,
        rgb,
    })
}

fn encode_preview(t: &Thumbnail) -> Option<Vec<u8>> {
    if t.width == 0 || t.height == 0 || t.rgb.len() != (t.width * t.height * 3) as usize {
        return None;
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, t.width, t.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(&t.rgb).ok()?;
    }
    Some(out)
}
