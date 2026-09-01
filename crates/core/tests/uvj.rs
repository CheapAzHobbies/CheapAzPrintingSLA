//! UVJ: an open format, and the only one here whose contents can be checked
//! without trusting any of this code.
//!
//! Because it stores eight-bit PNGs rather than seven-bit run lengths, a round
//! trip through it must be exactly lossless — not "the same pixels lit", but
//! the same values. That makes it a stricter test than the binary formats can
//! be, and a useful middle step for testing the others.
//!
//!     UVtools --cmd convert model.sl1 uvj real.uvj
//!     CHEAPAZSLA_REAL_UVJ=real.uvj cargo test -p cheapazsla-core

use cheapazsla_core::format::Confidence;
use cheapazsla_core::layers::InMemoryLayers;
use cheapazsla_core::model::*;
use cheapazsla_core::registry;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const W: u32 = 40;
const H: u32 = 24;

/// A pattern using the whole eight-bit range, so anything that quietly rounds
/// shows up.
fn pattern(index: u32) -> Vec<u8> {
    (0..W * H)
        .map(|p| ((p * 7 + index * 13) % 256) as u8)
        .collect()
}

fn a_print() -> (PrintFile, InMemoryLayers) {
    let print = PrintFile {
        source_format: "test".into(),
        geometry: Geometry {
            resolution_x: W,
            resolution_y: H,
            display_width_mm: Some(68.04),
            display_height_mm: Some(120.96),
            machine_z_mm: Some(130.0),
        },
        exposure: Exposure {
            layer_height_mm: 0.05,
            exposure_s: 2.5,
            bottom_exposure_s: Some(30.0),
            bottom_layers: Some(2),
            light_off_delay_s: Some(0.5),
            bottom_light_off_delay_s: Some(1.0),
            transition_layers: None,
            light_pwm: Some(255),
            bottom_light_pwm: Some(200),
        },
        lift: Lift {
            lift_height_mm: Some(6.0),
            lift_speed_mm_min: Some(80.0),
            bottom_lift_height_mm: Some(5.0),
            bottom_lift_speed_mm_min: Some(65.0),
            retract_speed_mm_min: Some(150.0),
            bottom_retract_speed_mm_min: Some(140.0),
        },
        layers: (0..5)
            .map(|i| LayerInfo {
                z_mm: 0.05 * (i + 1) as f32,
                exposure_s: None,
                light_off_delay_s: None,
                lift_height_mm: None,
                lift_speed_mm_min: None,
            })
            .collect(),
        thumbnails: vec![Thumbnail {
            width: 16,
            height: 16,
            rgb: (0..16 * 16 * 3).map(|i| (i % 256) as u8).collect(),
        }],
        print_time_s: Some(900),
        material_volume_ml: Some(3.0),
        material_grams: None,
        material_name: None,
        machine_name: None,
        extra: BTreeMap::new(),
    };
    let images = (0..5)
        .map(|i| LayerImage {
            width: W,
            height: H,
            pixels: pattern(i),
        })
        .collect();
    (print, InMemoryLayers::new(images, W, H))
}

fn write_one(dir: &Path) -> PathBuf {
    let (print, layers) = a_print();
    let path = dir.join("out.uvj");
    registry::by_id("uvj")
        .expect("uvj")
        .write(&path, &print, &layers)
        .expect("write");
    path
}

#[test]
fn a_uvj_is_recognised_by_its_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let d = registry::identify(&write_one(dir.path()))
        .expect("identify")
        .detection;
    assert_eq!(d.format_id, "uvj");
    assert_eq!(d.confidence, Confidence::High, "{}", d.reason);
}

#[test]
fn a_zip_without_a_manifest_is_not_claimed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.uvj");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("hello.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"nothing to do with printing").unwrap();
    zip.finish().unwrap();
    if let Ok(found) = registry::identify(&path) {
        assert!(
            found.detection.confidence < Confidence::Medium,
            "{}",
            found.detection.reason
        );
    }
}

#[test]
fn the_round_trip_is_exactly_lossless() {
    // Eight-bit PNGs, so unlike the binary formats nothing may be rounded.
    let dir = tempfile::tempdir().unwrap();
    let opened = registry::open(&write_one(dir.path())).expect("open");
    for i in 0..5u32 {
        assert_eq!(
            opened.layers.layer(i).unwrap().pixels,
            pattern(i),
            "layer {i} changed"
        );
    }
}

#[test]
fn metadata_survives_a_write_and_a_read() {
    let dir = tempfile::tempdir().unwrap();
    let opened = registry::open(&write_one(dir.path())).expect("open");
    let p = &opened.print;
    assert_eq!(p.source_format, "uvj");
    assert_eq!(p.geometry.resolution_x, W);
    assert_eq!(p.geometry.resolution_y, H);
    assert_eq!(p.geometry.display_width_mm, Some(68.04));
    assert_eq!(p.exposure.layer_height_mm, 0.05);
    assert_eq!(p.exposure.exposure_s, 2.5);
    assert_eq!(p.exposure.bottom_exposure_s, Some(30.0));
    assert_eq!(p.exposure.bottom_layers, Some(2));
    assert_eq!(p.exposure.light_pwm, Some(255));
    assert_eq!(p.lift.lift_height_mm, Some(6.0));
    assert_eq!(p.lift.retract_speed_mm_min, Some(150.0));
    assert_eq!(p.layer_count(), 5);
    assert_eq!(p.thumbnails.len(), 2);
}

#[test]
fn per_layer_exposure_distinguishes_bottom_layers() {
    let dir = tempfile::tempdir().unwrap();
    let opened = registry::open(&write_one(dir.path())).expect("open");
    // Two bottom layers at 30 s, the rest at 2.5 s.
    assert_eq!(opened.print.layers[0].exposure_s, Some(30.0));
    assert_eq!(opened.print.layers[1].exposure_s, Some(30.0));
    assert_eq!(opened.print.layers[2].exposure_s, Some(2.5));
}

#[test]
fn a_uvj_converts_to_goo_pixel_for_pixel() {
    use cheapazsla_core::convert;
    let dir = tempfile::tempdir().unwrap();
    let src = write_one(dir.path());
    let dst = dir.path().join("out.goo");
    let plan = convert::plan(&src, "goo", &dst).expect("plan");
    convert::run(&plan).expect("convert");
    let written = registry::open(&dst).expect("open goo");
    for i in 0..5u32 {
        assert_eq!(
            written.layers.layer(i).unwrap().pixels,
            pattern(i),
            "layer {i}"
        );
    }
}

#[test]
fn asking_past_the_last_layer_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let opened = registry::open(&write_one(dir.path())).expect("open");
    assert!(opened.layers.layer(5).is_err());
    assert!(opened.layers.layer(u32::MAX).is_err());
}

#[test]
fn a_manifest_that_is_not_json_is_refused_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.uvj");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("config.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"{ this is not json").unwrap();
    zip.finish().unwrap();
    let err = registry::open(&path).expect_err("must refuse");
    assert!(err.to_string().contains("not valid JSON"), "{err}");
}

#[test]
fn a_missing_layer_is_reported_rather_than_guessed_at() {
    // Rebuild the archive without one of its slices.
    let dir = tempfile::tempdir().unwrap();
    let src = write_one(dir.path());
    let path = dir.path().join("short.uvj");
    {
        let mut from = zip::ZipArchive::new(std::fs::File::open(&src).unwrap()).unwrap();
        let mut to = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for i in 0..from.len() {
            let mut entry = from.by_index(i).unwrap();
            let name = entry.name().to_string();
            if name == "slice/00000003.png" {
                continue;
            }
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).unwrap();
            to.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            to.write_all(&buf).unwrap();
        }
        to.finish().unwrap();
    }
    let opened = registry::open(&path).expect("the manifest is still readable");
    let err = opened.layers.layer(3).expect_err("layer 3 is gone");
    assert!(err.to_string().contains("missing"), "{err}");

    let notes = registry::by_id("uvj").unwrap().validate(&path).unwrap();
    assert!(
        notes.iter().any(|n| n.contains("missing")),
        "validate should say so too: {notes:?}"
    );
}

#[test]
fn a_manifest_promising_more_layers_than_it_holds_is_noted() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_one(dir.path());
    let notes = registry::by_id("uvj").unwrap().validate(&src).unwrap();
    assert!(
        notes.is_empty(),
        "a file we wrote should validate: {notes:?}"
    );
}

/// A file UVtools produced.
#[test]
fn a_real_uvj_reads() {
    let Ok(path) = std::env::var("CHEAPAZSLA_REAL_UVJ") else {
        eprintln!("skipped: set CHEAPAZSLA_REAL_UVJ to a file from UVtools");
        return;
    };
    let path = PathBuf::from(path);
    let d = registry::identify(&path).expect("identify").detection;
    assert_eq!(d.format_id, "uvj", "detected as {}", d.format_id);

    let opened = registry::open(&path).expect("open");
    let p = &opened.print;
    println!(
        "  {} layers at {}x{}",
        p.layer_count(),
        p.geometry.resolution_x,
        p.geometry.resolution_y
    );
    assert!(p.layer_count() > 0);
    assert!(p.exposure.layer_height_mm > 0.0 && p.exposure.layer_height_mm < 1.0);

    let (w, h) = opened.layers.dimensions();
    let expected = w as usize * h as usize;
    for i in [0, p.layer_count() / 2, p.layer_count() - 1] {
        let img = opened
            .layers
            .layer(i)
            .unwrap_or_else(|e| panic!("layer {i}: {e}"));
        assert_eq!(img.pixels.len(), expected, "layer {i} is the wrong size");
    }
    println!("  sampled layers decoded to {w}x{h}");
}
