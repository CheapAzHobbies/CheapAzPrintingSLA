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
fn downscale(img: &LayerImage, factor: u32) -> (u32, u32, Vec<u8>) {
    if factor <= 1 {
        return (img.width, img.height, img.pixels.clone());
    }
    let nw = (img.width / factor).max(1);
    let nh = (img.height / factor).max(1);
    let mut out = vec![0u8; (nw * nh) as usize];
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
                    sum += img.pixels[row + sx] as u32;
                    count += 1;
                }
            }
            out[y * nw as usize + x] = if count > 0 { (sum / count) as u8 } else { 0 };
        }
    }
    (nw, nh, out)
}

/// Build a texture for display, downscaling when the layer is large.
///
/// Returns the texture and the factor it was reduced by, so the interface can
/// tell the user they are not looking at full resolution.
pub fn texture_for(img: &LayerImage) -> (gdk::Texture, u32) {
    let longest = img.width.max(img.height);
    let factor = ((longest + MAX_EDGE - 1) / MAX_EDGE).max(1);
    let (w, h, grey) = downscale(img, factor);

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
    (texture.upcast(), factor)
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
