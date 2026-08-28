//! Parsing a real PrusaSlicer SL1 file (§41).
//!
//! Set CHEAPAZSLA_REAL_SL1 to a genuine slicer output to run these. They are
//! skipped when it is unset so the suite still passes on a clean checkout.

use cheapazsla_core::format::Confidence;
use cheapazsla_core::layers::LayerProvider;
use cheapazsla_core::registry;
use std::path::PathBuf;

fn real_file() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("CHEAPAZSLA_REAL_SL1").ok()?);
    p.exists().then_some(p)
}

macro_rules! real_or_skip {
    () => {
        match real_file() {
            Some(p) => p,
            None => {
                eprintln!("skipped: set CHEAPAZSLA_REAL_SL1 to a real .sl1 file");
                return;
            }
        }
    };
}

#[test]
fn detects_a_real_sl1_with_high_confidence() {
    let path = real_or_skip!();
    let id = registry::identify(&path).expect("identify");
    assert_eq!(id.detection.format_id, "sl1");
    assert_eq!(id.detection.confidence, Confidence::High);
    assert!(!id.extension_mismatch);
    println!("  reason: {}", id.detection.reason);
}

#[test]
fn reads_metadata_from_a_real_sl1() {
    let path = real_or_skip!();
    let opened = registry::open(&path).expect("open");
    let p = &opened.print;

    assert_eq!(p.source_format, "sl1");
    assert!(p.layer_count() > 0, "must find layers");
    assert!(p.exposure.layer_height_mm > 0.0);
    assert!(p.exposure.exposure_s > 0.0);
    assert!(p.geometry.resolution_x > 0 && p.geometry.resolution_y > 0);

    // Z must increase monotonically and match the layer height.
    let h = p.exposure.layer_height_mm;
    for (i, l) in p.layers.iter().enumerate() {
        let expect = h * (i + 1) as f32;
        assert!(
            (l.z_mm - expect).abs() < 1e-3,
            "layer {i} z is {} but should be {expect}",
            l.z_mm
        );
    }

    println!(
        "  {}x{}  {} layers @ {}mm  exposure {}s (bottom {:?}s)",
        p.geometry.resolution_x,
        p.geometry.resolution_y,
        p.layer_count(),
        p.exposure.layer_height_mm,
        p.exposure.exposure_s,
        p.exposure.bottom_exposure_s
    );
    if let Some((x, y)) = p.geometry.pixel_size_um() {
        println!("  pixel size {x:.1} x {y:.1} um");
    }
    println!("  print time {:?}s  material {:?}ml", p.print_time_s, p.material_volume_ml);
}

#[test]
fn decodes_real_layer_bitmaps_at_the_declared_size() {
    let path = real_or_skip!();
    let opened = registry::open(&path).expect("open");
    let (w, h) = opened.layers.dimensions();
    assert_eq!((w, h), (opened.print.geometry.resolution_x, opened.print.geometry.resolution_y));

    let first = opened.layers.layer(0).expect("first layer");
    assert_eq!(first.width, w);
    assert_eq!(first.height, h);
    assert_eq!(first.pixels.len(), w as usize * h as usize);

    let last = opened.layers.layer(opened.print.layer_count() - 1).expect("last layer");
    assert_eq!(last.pixels.len(), w as usize * h as usize);

    // A real print has exposed geometry somewhere in the middle of the stack.
    let mid = opened.layers.layer(opened.print.layer_count() / 2).expect("middle layer");
    assert!(!mid.is_blank(), "a middle layer of a real print should expose something");
    println!("  middle layer exposes {} pixels", mid.exposed_pixels(0));
}

#[test]
fn a_layer_past_the_end_of_a_real_file_errors_cleanly() {
    let path = real_or_skip!();
    let opened = registry::open(&path).expect("open");
    let err = opened.layers.layer(opened.print.layer_count() + 500).unwrap_err();
    assert!(matches!(err, cheapazsla_core::Error::LayerOutOfRange { .. }));
}

#[test]
fn validation_of_a_real_file_reports_no_problems() {
    let path = real_or_skip!();
    let handler = registry::by_id("sl1").unwrap();
    let warnings = handler.validate(&path).expect("validate");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
}
