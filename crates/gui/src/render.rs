//! Turning a decoded layer into something GTK can draw.
//!
//! A layer can be 11520x5120. As RGB that is 177 MB, far more than is useful
//! for a screen preview, so layers are downscaled before they become a
//! texture. The scale is chosen from the layer size, not the widget size, so
//! the result can be cached and reused while the user resizes.

use cheapazsla_core::LayerImage;
use gtk::gdk;
use gtk::glib::Bytes;
use gtk::prelude::*;

/// Longest edge of a preview texture, in pixels. Comfortably above any
/// display it will be shown on, small enough to build in a few milliseconds.
const MAX_EDGE: u32 = 2048;

/// Downscale by an integer factor using box averaging.
///
/// Integer factors keep this cheap and avoid resampling artefacts that could
/// make a user think their layer has holes in it. Averaging rather than
/// nearest-neighbour matters: thin single-pixel features are common in resin
/// prints and nearest-neighbour makes them flicker in and out while scrubbing.
/// Downscale, returning the reduced image and how many source pixels were
/// exposed.
///
/// The count comes from this pass rather than a separate walk. Every pixel is
/// already being read here, and on a 12K panel a second pass is another 59
/// million reads for a number that is free to accumulate on the way past.
fn downscale(img: &LayerImage, factor: u32) -> (u32, u32, Vec<u8>, u64) {
    if factor <= 1 {
        let exposed = img.pixels.iter().filter(|&&p| p > 0).count() as u64;
        return (img.width, img.height, img.pixels.clone(), exposed);
    }
    let nw = (img.width / factor).max(1);
    let nh = (img.height / factor).max(1);
    let mut out = vec![0u8; (nw * nh) as usize];
    let mut exposed: u64 = 0;
    let f = factor as usize;
    let src_w = img.width as usize;
    for y in 0..nh as usize {
        for x in 0..nw as usize {
            let mut sum: u32 = 0;
            let mut count: u32 = 0;
            for dy in 0..f {
                let sy = y * f + dy;
                if sy >= img.height as usize {
                    break;
                }
                let row = sy * src_w;
                for dx in 0..f {
                    let sx = x * f + dx;
                    if sx >= src_w {
                        break;
                    }
                    let v = img.pixels[row + sx];
                    sum += v as u32;
                    exposed += (v > 0) as u64;
                    count += 1;
                }
            }
            out[y * nw as usize + x] = sum.checked_div(count).unwrap_or(0) as u8;
        }
    }
    (nw, nh, out, exposed)
}

/// How a layer's pixels map to millimetres, when the file records it.
///
/// Panels are frequently not square-pixelled. An 11520x5120 screen on a
/// 218.88x122.88 mm window is 19.0 um across and 24.0 um down, so drawing the
/// bitmap one pixel to one pixel stretches the part 26% too wide. For a tool
/// whose job is answering "does this look right", that is a correctness
/// problem rather than a cosmetic one.
#[derive(Clone, Copy)]
pub struct PixelSize {
    pub x_um: f32,
    pub y_um: f32,
}

impl PixelSize {
    /// Vertical pixels per horizontal pixel, for equal physical distance.
    fn aspect(&self) -> f32 {
        if self.x_um > 0.0 && self.y_um > 0.0 {
            self.y_um / self.x_um
        } else {
            1.0
        }
    }

    /// True when the difference is big enough to be worth correcting. Below
    /// this the resample costs more than it is worth.
    fn is_square(&self) -> bool {
        (self.aspect() - 1.0).abs() < 0.01
    }
}

/// Resample the height so the image is physically proportioned.
///
/// Only the vertical axis moves, and only ever upward in sample count, so no
/// detail is thrown away that the downscale did not already remove.
fn correct_aspect(w: u32, h: u32, grey: Vec<u8>, aspect: f32) -> (u32, u32, Vec<u8>) {
    let target_h = ((h as f32) * aspect).round().max(1.0) as u32;
    if target_h == h {
        return (w, h, grey);
    }
    let mut out = vec![0u8; (w * target_h) as usize];
    for y in 0..target_h {
        // Nearest source row. Linear would be smoother, but these are exposure
        // masks: blending rows invents grey where the print has a hard edge.
        let sy = ((y as f32 + 0.5) / aspect - 0.5)
            .round()
            .clamp(0.0, (h - 1) as f32) as u32;
        let src = (sy * w) as usize;
        let dst = (y * w) as usize;
        out[dst..dst + w as usize].copy_from_slice(&grey[src..src + w as usize]);
    }
    (w, target_h, out)
}

/// Build a texture for display, downscaling when the layer is large and
/// correcting for non-square pixels when the file says what they are.
///
/// Returns the texture and the factor it was reduced by, so the interface can
/// tell the user they are not looking at full resolution.
pub fn texture_for(img: &LayerImage, pixel: Option<PixelSize>) -> (gdk::Texture, u32, u64) {
    let longest = img.width.max(img.height);
    let factor = longest.div_ceil(MAX_EDGE).max(1);
    let (w, h, grey, exposed) = downscale(img, factor);
    let (w, h, grey) = match pixel {
        Some(p) if !p.is_square() => correct_aspect(w, h, grey, p.aspect()),
        _ => (w, h, grey),
    };

    // GTK has no 8-bit greyscale memory format, so expand to RGB. Tinting
    // slightly toward the accent colour reads better than pure white on the
    // dark background and matches how the exposed area actually behaves.
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for &g in &grey {
        let r = g;
        let gg = ((g as u16 * 245) / 255) as u8;
        let b = ((g as u16 * 220) / 255) as u8;
        rgb.extend_from_slice(&[r, gg, b]);
    }

    let texture = gdk::MemoryTexture::new(
        w as i32,
        h as i32,
        gdk::MemoryFormat::R8g8b8,
        &Bytes::from_owned(rgb),
        (w * 3) as usize,
    );
    (texture.upcast(), factor, exposed)
}

/// Human-readable byte size.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Human-readable duration.
pub fn human_time(s: u64) -> String {
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {sec}s")
    } else {
        format!("{sec}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> LayerImage {
        let mut i = LayerImage::blank(w, h);
        // A horizontal band, so a vertical resample is visible in the output.
        for x in 0..w as usize {
            i.pixels[(h as usize / 2) * w as usize + x] = 255;
        }
        i
    }

    #[test]
    fn square_pixels_are_left_alone() {
        let p = PixelSize {
            x_um: 50.0,
            y_um: 50.0,
        };
        assert!(p.is_square());
        let (w, h, px) = correct_aspect(10, 10, vec![0; 100], p.aspect());
        assert_eq!((w, h), (10, 10));
        assert_eq!(px.len(), 100);
    }

    #[test]
    fn a_taller_pixel_makes_a_taller_image() {
        // 19.0 x 24.0 um, the real geometry of a Saturn 4 Ultra panel: each
        // pixel covers more distance vertically, so the same pixel count spans
        // more millimetres and the image must grow to match.
        let p = PixelSize {
            x_um: 19.0,
            y_um: 24.0,
        };
        assert!(!p.is_square());
        let (w, h, px) = correct_aspect(100, 100, vec![7; 10_000], p.aspect());
        assert_eq!(w, 100, "width never changes");
        assert_eq!(h, 126, "100 * 24.0/19.0 rounds to 126");
        assert_eq!(px.len(), 100 * 126);
        assert!(
            px.iter().all(|&v| v == 7),
            "resampling must not invent values"
        );
    }

    #[test]
    fn correcting_gets_the_physical_proportions_right() {
        // A Saturn 4 Ultra layer: 11520x5120 px across 218.88x122.88 mm.
        let p = PixelSize {
            x_um: 19.0,
            y_um: 24.0,
        };
        let (w, h, _) = correct_aspect(1920, 853, vec![0; 1920 * 853], p.aspect());
        let shown = w as f32 / h as f32;
        let physical = 218.88 / 122.88;
        assert!(
            (shown - physical).abs() < 0.02,
            "shown aspect {shown:.3} should match the panel's {physical:.3}"
        );
    }

    #[test]
    fn resampling_keeps_hard_edges() {
        // Exposure masks are binary; blending rows would invent grey.
        let src = img(8, 8);
        let (_, _, px) = correct_aspect(8, 8, src.pixels, 1.5);
        assert!(
            px.iter().all(|&v| v == 0 || v == 255),
            "no intermediate values may appear"
        );
    }
}
