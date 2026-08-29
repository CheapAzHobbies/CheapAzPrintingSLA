//! Inspect a resin print file and optionally export a layer as a PNG.
//!
//! ```text
//! cargo run --example inspect -- <file> [layer index] [out.png]
//! ```
//!
//! Uses only the public API, so it doubles as a check that the engine is
//! usable as a library.

use cheapazsla_core::layers::LayerProvider;
use cheapazsla_core::registry;
use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: inspect <file> [layer] [out.png]");
        std::process::exit(2);
    };
    let path = Path::new(&path);
    let layer_idx: Option<u32> = args.next().and_then(|s| s.parse().ok());
    let out_png = args.next();

    // --- identify (§11) -------------------------------------------------
    let id = match registry::identify(path) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("could not identify this file: {e}");
            std::process::exit(1);
        }
    };
    println!("DETECTION");
    println!("  format      {}", id.detection.format_id);
    println!("  confidence  {:?}", id.detection.confidence);
    println!("  reason      {}", id.detection.reason);
    if id.extension_mismatch {
        println!("  WARNING     the extension disagrees with the contents");
    }

    // --- validate -------------------------------------------------------
    let handler = registry::by_id(id.detection.format_id).unwrap();
    match handler.validate(path) {
        Ok(w) if w.is_empty() => println!("\nVALIDATION\n  no problems found"),
        Ok(w) => {
            println!("\nVALIDATION");
            for line in w {
                println!("  warning: {line}");
            }
        }
        Err(e) => {
            eprintln!("\nvalidation failed: {e}");
            std::process::exit(1);
        }
    }

    // --- metadata (§24: only what exists) --------------------------------
    let opened = match registry::open(path) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("could not open: {e}");
            std::process::exit(1);
        }
    };
    let p = &opened.print;
    println!("\nPRINT");
    println!(
        "  resolution      {} x {}",
        p.geometry.resolution_x, p.geometry.resolution_y
    );
    if let (Some(w), Some(h)) = (p.geometry.display_width_mm, p.geometry.display_height_mm) {
        println!("  display         {w} x {h} mm");
    }
    if let Some((x, y)) = p.geometry.pixel_size_um() {
        println!("  pixel size      {x:.2} x {y:.2} um");
    }
    println!("  layers          {}", p.layer_count());
    println!("  layer height    {} mm", p.exposure.layer_height_mm);
    if let Some(h) = p.height_mm() {
        println!("  print height    {h:.2} mm");
    }
    println!("  exposure        {} s", p.exposure.exposure_s);
    show(
        "  bottom exposure",
        p.exposure.bottom_exposure_s.map(|v| format!("{v} s")),
    );
    show(
        "  bottom layers  ",
        p.exposure.bottom_layers.map(|v| v.to_string()),
    );
    show(
        "  transition     ",
        p.exposure.transition_layers.map(|v| format!("{v} layers")),
    );
    show("  print time     ", p.print_time_s.map(fmt_time));
    show(
        "  material       ",
        p.material_volume_ml.map(|v| format!("{v} ml")),
    );
    show("  material name  ", p.material_name.clone());
    show("  printer        ", p.machine_name.clone());
    println!("  thumbnails      {}", p.thumbnails.len());
    if !p.extra.is_empty() {
        println!("\nPRESERVED FOR CONVERSION ({} values)", p.extra.len());
        for (k, v) in p.extra.iter().take(6) {
            println!("  {k} = {v}");
        }
    }

    // --- layer ----------------------------------------------------------
    let idx = layer_idx
        .unwrap_or(p.layer_count() / 2)
        .min(p.layer_count().saturating_sub(1));
    match opened.layers.layer(idx) {
        Ok(img) => {
            let exposed = img.exposed_pixels(0);
            let total = img.width as u64 * img.height as u64;
            println!("\nLAYER {idx} of {}", p.layer_count());
            println!("  size            {} x {}", img.width, img.height);
            println!(
                "  exposed pixels  {exposed} ({:.4}% of the panel)",
                exposed as f64 / total as f64 * 100.0
            );
            show(
                "  exposure       ",
                p.effective_exposure_s(idx).map(|v| format!("{v} s")),
            );
            if let Some(out) = out_png {
                match write_png(&out, &img) {
                    Ok(_) => println!("  written to      {out}"),
                    Err(e) => eprintln!("  could not write {out}: {e}"),
                }
            }
        }
        Err(e) => eprintln!("\ncould not read layer {idx}: {e}"),
    }
}

fn show(label: &str, value: Option<String>) {
    // §13: absent stays absent, it is never printed as a zero.
    println!("{label} {}", value.unwrap_or_else(|| "not recorded".into()));
}

fn fmt_time(s: u64) -> String {
    format!("{}h {}m {}s", s / 3600, (s % 3600) / 60, s % 60)
}

fn write_png(path: &str, img: &cheapazsla_core::LayerImage) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let w = std::io::BufWriter::new(file);
    let mut enc = png::Encoder::new(w, img.width, img.height);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer
        .write_image_data(&img.pixels)
        .map_err(std::io::Error::other)?;
    Ok(())
}
