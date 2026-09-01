//! PHZ reading and writing (phase 20).
//!
//! The files here are written by this project, so these show the reader and
//! writer agree with each other and that broken input is refused. What shows
//! they agree with anyone else is the test at the bottom, against a file from
//! UVtools:
//!
//!     UVtools --cmd convert model.sl1 phz real.phz
//!     CHEAPAZSLA_REAL_PHZ=real.phz cargo test -p cheapazsla-core
//!
//! Give UVtools a copy, never a file you want to keep: it rewrites its input.

use cheapazsla_core::format::Confidence;
use cheapazsla_core::layers::InMemoryLayers;
use cheapazsla_core::model::*;
use cheapazsla_core::registry;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const W: u32 = 64;
const H: u32 = 32;

fn pattern(index: u32) -> Vec<u8> {
    (0..W * H)
        .map(|p| match index {
            0 => 0u8,
            1 => 254,
            2 => ((p * 7) % 256) as u8,
            _ => {
                if (p / W + index) % 8 < 4 {
                    254
                } else {
                    0
                }
            }
        })
        .collect()
}

/// Seven bits is all the format holds, so a value comes back rounded.
fn folded(index: u32) -> Vec<u8> {
    pattern(index)
        .iter()
        .map(|&p| (p >> 1) << 1 | (p >> 7))
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
            bottom_retract_speed_mm_min: Some(150.0),
        },
        layers: (0..4)
            .map(|i| LayerInfo {
                z_mm: 0.05 * (i + 1) as f32,
                exposure_s: None,
                light_off_delay_s: None,
                lift_height_mm: None,
                lift_speed_mm_min: None,
            })
            .collect(),
        thumbnails: vec![Thumbnail {
            width: 32,
            height: 32,
            rgb: std::iter::repeat_n([90u8, 140, 200], 32 * 32)
                .flatten()
                .collect(),
        }],
        print_time_s: Some(3600),
        material_volume_ml: Some(9.5),
        material_grams: Some(11.0),
        material_name: None,
        machine_name: Some("Phrozen Sonic Mini".into()),
        extra: BTreeMap::new(),
    };
    let images = (0..4)
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
    let path = dir.join("out.phz");
    registry::by_id("phz")
        .expect("phz")
        .write(&path, &print, &layers)
        .expect("write");
    path
}

#[test]
fn a_phz_is_recognised_by_its_magic() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_one(dir.path());
    let d = registry::identify(&path).expect("identify").detection;
    assert_eq!(d.format_id, "phz");
    assert_eq!(d.confidence, Confidence::High, "{}", d.reason);
}

#[test]
fn the_extension_alone_is_not_enough() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lying.phz");
    std::fs::write(&path, b"not a print file").unwrap();
    if let Ok(found) = registry::identify(&path) {
        assert!(found.detection.confidence < Confidence::Medium);
    }
}

#[test]
fn metadata_survives_a_write_and_a_read() {
    let dir = tempfile::tempdir().unwrap();
    let opened = registry::open(&write_one(dir.path())).expect("open");
    let p = &opened.print;
    assert_eq!(p.source_format, "phz");
    assert_eq!(p.geometry.resolution_x, W);
    assert_eq!(p.geometry.resolution_y, H);
    assert_eq!(p.geometry.display_width_mm, Some(68.04));
    assert_eq!(p.exposure.layer_height_mm, 0.05);
    assert_eq!(p.exposure.exposure_s, 2.5);
    assert_eq!(p.exposure.bottom_exposure_s, Some(30.0));
    assert_eq!(p.exposure.bottom_layers, Some(2));
    assert_eq!(p.exposure.light_pwm, Some(255));
    assert_eq!(p.exposure.bottom_light_pwm, Some(200));
    assert_eq!(p.lift.lift_height_mm, Some(6.0));
    assert_eq!(p.lift.bottom_lift_speed_mm_min, Some(65.0));
    assert_eq!(p.lift.retract_speed_mm_min, Some(150.0));
    assert_eq!(p.material_volume_ml, Some(9.5));
    assert_eq!(p.print_time_s, Some(3600));
    assert_eq!(p.machine_name.as_deref(), Some("Phrozen Sonic Mini"));
    assert_eq!(p.layer_count(), 4);
    assert_eq!(p.thumbnails.len(), 2, "both previews are written");
}

#[test]
fn every_layer_comes_back_pixel_for_pixel() {
    let dir = tempfile::tempdir().unwrap();
    let opened = registry::open(&write_one(dir.path())).expect("open");
    for i in 0..4u32 {
        assert_eq!(
            opened.layers.layer(i).unwrap().pixels,
            folded(i),
            "layer {i} changed"
        );
    }
}

#[test]
fn a_phz_converts_to_goo_pixel_for_pixel() {
    use cheapazsla_core::convert;
    let dir = tempfile::tempdir().unwrap();
    let src = write_one(dir.path());
    let dst = dir.path().join("out.goo");
    let plan = convert::plan(&src, "goo", &dst).expect("plan");
    convert::run(&plan).expect("convert");
    let written = registry::open(&dst).expect("open goo");
    for i in 0..4u32 {
        assert_eq!(
            written.layers.layer(i).unwrap().pixels,
            folded(i),
            "layer {i}"
        );
    }
}

#[test]
fn asking_past_the_last_layer_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let opened = registry::open(&write_one(dir.path())).expect("open");
    assert!(opened.layers.layer(4).is_err());
    assert!(opened.layers.layer(u32::MAX).is_err());
}

#[test]
fn a_truncated_file_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let full = std::fs::read(write_one(dir.path())).unwrap();
    for cut in [0usize, 4, 8, 0x40, 0xD7, 0xD8, 0x100] {
        let path = dir.path().join("cut.phz");
        std::fs::write(&path, &full[..cut.min(full.len())]).unwrap();
        let _ = registry::open(&path);
        let _ = registry::identify(&path);
    }
}

#[test]
fn a_layer_pointing_outside_the_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = std::fs::read(write_one(dir.path())).unwrap();
    let table = u32::from_le_bytes(bytes[0x24..0x28].try_into().unwrap()) as usize;
    bytes[table + 0x0C..table + 0x10].copy_from_slice(&0xFFFF_0000u32.to_le_bytes());
    let path = dir.path().join("bad.phz");
    std::fs::write(&path, &bytes).unwrap();
    let err = registry::open(&path).expect_err("must refuse");
    assert!(err.to_string().contains("outside the file"), "{err}");
}

#[test]
fn an_absurd_layer_count_is_refused_without_allocating_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = std::fs::read(write_one(dir.path())).unwrap();
    bytes[0x28..0x2C].copy_from_slice(&u32::MAX.to_le_bytes());
    let path = dir.path().join("huge.phz");
    std::fs::write(&path, &bytes).unwrap();
    assert!(registry::open(&path).is_err());
}

#[test]
fn an_unsupported_version_is_named_in_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = std::fs::read(write_one(dir.path())).unwrap();
    bytes[0x04..0x08].copy_from_slice(&9u32.to_le_bytes());
    let path = dir.path().join("v9.phz");
    std::fs::write(&path, &bytes).unwrap();
    let err = registry::open(&path).expect_err("must refuse");
    assert!(err.to_string().contains('9'), "{err}");
}

#[test]
fn the_encryption_mode_the_printer_expects_is_written() {
    // catibo reports that changing this makes files unreadable on the Sonic
    // Mini, so it is written as found rather than reasoned about.
    let dir = tempfile::tempdir().unwrap();
    let bytes = std::fs::read(write_one(dir.path())).unwrap();
    assert_eq!(
        u32::from_le_bytes(bytes[0xB0..0xB4].try_into().unwrap()),
        0x1C
    );
}

/// A file UVtools produced, which is the one that decides whether the layout
/// is right rather than merely self-consistent.
#[test]
fn a_real_phz_reads() {
    let Ok(path) = std::env::var("CHEAPAZSLA_REAL_PHZ") else {
        eprintln!("skipped: set CHEAPAZSLA_REAL_PHZ to a file from UVtools");
        return;
    };
    let path = PathBuf::from(path);
    let d = registry::identify(&path).expect("identify").detection;
    assert_eq!(d.format_id, "phz", "detected as {}", d.format_id);

    let opened = registry::open(&path).expect("open");
    let p = &opened.print;
    println!(
        "  {} layers at {}x{}",
        p.layer_count(),
        p.geometry.resolution_x,
        p.geometry.resolution_y
    );
    assert!(p.layer_count() > 0);
    assert!(
        p.exposure.layer_height_mm > 0.0 && p.exposure.layer_height_mm < 1.0,
        "layer height {} is not plausible",
        p.exposure.layer_height_mm
    );

    // Every layer must decode to exactly the panel size, which is the check a
    // wrong run-length scheme cannot survive.
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
