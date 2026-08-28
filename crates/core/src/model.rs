//! The common resin print representation (§9, §13).
//!
//! Every format is parsed into this model, and every writer consumes it.
//! That keeps the number of code paths linear in the number of formats
//! rather than quadratic in the number of format pairs.
//!
//! Two rules govern this module:
//!
//! 1. Anything a format may not provide is an [`Option`]. A missing value is
//!    never represented as zero, because zero is a legitimate exposure time
//!    and a legitimate lift height.
//! 2. Units appear in field names. Formats disagree about millimetres versus
//!    microns and seconds versus milliseconds, and normalising on read is the
//!    only way to stop that leaking into conversions.

use std::collections::BTreeMap;

/// Physical and pixel geometry of the print area.
#[derive(Debug, Clone, PartialEq)]
pub struct Geometry {
    /// Layer bitmap width in pixels.
    pub resolution_x: u32,
    /// Layer bitmap height in pixels.
    pub resolution_y: u32,
    /// Physical width of the LCD in millimetres, when the format records it.
    pub display_width_mm: Option<f32>,
    /// Physical height of the LCD in millimetres, when the format records it.
    pub display_height_mm: Option<f32>,
    /// Maximum Z travel in millimetres, when the format records it.
    pub machine_z_mm: Option<f32>,
}

impl Geometry {
    /// Pixel size in micrometres, if the physical display size is known.
    pub fn pixel_size_um(&self) -> Option<(f32, f32)> {
        let w = self.display_width_mm?;
        let h = self.display_height_mm?;
        if self.resolution_x == 0 || self.resolution_y == 0 {
            return None;
        }
        Some((
            w * 1000.0 / self.resolution_x as f32,
            h * 1000.0 / self.resolution_y as f32,
        ))
    }

    /// Number of pixels in one layer. Used for allocation checks (§42).
    pub fn pixel_count(&self) -> u64 {
        self.resolution_x as u64 * self.resolution_y as u64
    }
}

/// Exposure settings that apply to the print as a whole.
#[derive(Debug, Clone, PartialEq)]
pub struct Exposure {
    /// Nominal layer height in millimetres.
    pub layer_height_mm: f32,
    /// Exposure time for a normal layer, in seconds.
    pub exposure_s: f32,
    /// Exposure time for a bottom layer, in seconds.
    pub bottom_exposure_s: Option<f32>,
    /// How many layers at the base use the bottom settings.
    pub bottom_layers: Option<u32>,
    /// Delay after exposure before the lift begins, in seconds.
    pub light_off_delay_s: Option<f32>,
    /// Bottom-layer equivalent of `light_off_delay_s`.
    pub bottom_light_off_delay_s: Option<f32>,
    /// How many layers fade from bottom to normal exposure.
    pub transition_layers: Option<u32>,
    /// LED power, 0-255, when the format records it.
    pub light_pwm: Option<u8>,
    /// Bottom-layer equivalent of `light_pwm`.
    pub bottom_light_pwm: Option<u8>,
}

/// Lift and retract motion, which not every format records.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lift {
    pub lift_height_mm: Option<f32>,
    pub lift_speed_mm_min: Option<f32>,
    pub bottom_lift_height_mm: Option<f32>,
    pub bottom_lift_speed_mm_min: Option<f32>,
    pub retract_speed_mm_min: Option<f32>,
    pub bottom_retract_speed_mm_min: Option<f32>,
}

impl Lift {
    /// True when the format supplied nothing at all.
    pub fn is_empty(&self) -> bool {
        *self == Lift::default()
    }
}

/// Per-layer values. Formats that store settings only once leave these `None`,
/// and consumers fall back to the print-wide [`Exposure`].
#[derive(Debug, Clone, PartialEq)]
pub struct LayerInfo {
    /// Absolute Z height of this layer in millimetres.
    pub z_mm: f32,
    /// Exposure override for this layer, in seconds.
    pub exposure_s: Option<f32>,
    /// Light-off delay override for this layer, in seconds.
    pub light_off_delay_s: Option<f32>,
    /// Lift override for this layer.
    pub lift_height_mm: Option<f32>,
    /// Lift speed override for this layer.
    pub lift_speed_mm_min: Option<f32>,
}

/// An embedded preview image, stored as 8-bit RGB.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// RGB triples, `width * height * 3` bytes.
    pub rgb: Vec<u8>,
}

impl Thumbnail {
    pub fn pixel_count(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

/// A decoded layer bitmap: 8-bit greyscale, one byte per pixel.
///
/// Resin layers are exposure masks. Most formats store one bit or one byte per
/// pixel; normalising to 8-bit greyscale on read means writers have a single
/// input representation to handle.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerImage {
    pub width: u32,
    pub height: u32,
    /// One byte per pixel, `width * height` long. 0 is unexposed.
    pub pixels: Vec<u8>,
}

impl LayerImage {
    /// A fully unexposed layer of the given size.
    pub fn blank(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width as usize).saturating_mul(height as usize)],
        }
    }

    /// True when no pixel would be exposed. Useful for validation (§40).
    pub fn is_blank(&self) -> bool {
        self.pixels.iter().all(|&p| p == 0)
    }

    /// How many pixels are above the given threshold.
    pub fn exposed_pixels(&self, threshold: u8) -> u64 {
        self.pixels.iter().filter(|&&p| p > threshold).count() as u64
    }
}

/// Everything CheapAzSLA understands about a print file, minus the layer
/// bitmaps themselves, which are fetched lazily (§15).
#[derive(Debug, Clone)]
pub struct PrintFile {
    /// Identifier of the format this was read from.
    pub source_format: String,
    pub geometry: Geometry,
    pub exposure: Exposure,
    pub lift: Lift,
    /// One entry per layer, in print order. Length is the layer count.
    pub layers: Vec<LayerInfo>,
    pub thumbnails: Vec<Thumbnail>,
    /// Estimated print time in seconds, when the format records it.
    pub print_time_s: Option<u64>,
    /// Resin volume in millilitres, when the format records it.
    pub material_volume_ml: Option<f32>,
    /// Resin mass in grams, when the format records it.
    pub material_grams: Option<f32>,
    /// Resin name, when the format records it.
    pub material_name: Option<String>,
    /// Printer model, when the format records it.
    pub machine_name: Option<String>,
    /// Values that belong to the source format and have no home in this model.
    /// Preserved so conversions can report what they are dropping (§14).
    pub extra: BTreeMap<String, String>,
}

impl PrintFile {
    pub fn layer_count(&self) -> u32 {
        self.layers.len() as u32
    }

    /// Total Z height of the print, from the topmost layer.
    pub fn height_mm(&self) -> Option<f32> {
        self.layers.last().map(|l| l.z_mm)
    }

    /// Effective exposure for a layer, applying bottom-layer rules when the
    /// layer carries no override of its own.
    pub fn effective_exposure_s(&self, index: u32) -> Option<f32> {
        let layer = self.layers.get(index as usize)?;
        if let Some(e) = layer.exposure_s {
            return Some(e);
        }
        let bottom = self.exposure.bottom_layers.unwrap_or(0);
        if index < bottom {
            self.exposure.bottom_exposure_s.or(Some(self.exposure.exposure_s))
        } else {
            Some(self.exposure.exposure_s)
        }
    }
}
