//! CTB read against files another implementation wrote.
//!
//! Every other CTB test in this suite reads a file CheapAzSLA built, so it can
//! only show the reader agrees with itself. These two files came from
//! [catibo](https://github.com/cbiffle/catibo), a separate reverse engineering
//! of the format whose author verified it by printing from it. If the header
//! offsets, the run-length encoding or the layer cipher were wrong, these
//! would fail.
//!
//! See `data/README.md` for how they were made.

use cheapazsla_core::registry;
use std::path::PathBuf;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// The patterns catibo was asked to encode, in layer order. Seven bits of grey
/// is all CTB holds, so what comes back is the input with its lowest bit
/// folded away — the same rounding the format applies to anyone's file.
fn expected_layer(index: u32) -> Vec<u8> {
    let seven = |v: u8| (v >> 1) << 1 | (v >> 7);
    (0..WIDTH * HEIGHT)
        .map(|i| match index {
            0 => seven(0),
            1 => seven(254),
            2 => seven(((i * 7) % 256) as u8),
            _ => {
                let (x, y) = ((i % WIDTH) as f32, (i / WIDTH) as f32);
                let (cx, cy) = (WIDTH as f32 / 2.0, HEIGHT as f32 / 2.0);
                let r = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
                seven(if r < WIDTH as f32 / 3.0 { 254 } else { 0 })
            }
        })
        .collect()
}

fn check(name: &str) {
    let path = fixture(name);
    let found = registry::identify(&path).expect("identify");
    assert_eq!(
        found.detection.format_id, "ctb",
        "{name} was taken for {}",
        found.detection.format_id
    );

    let opened = registry::open(&path).expect("open");
    let p = &opened.print;

    // Metadata, against what catibo was told to write.
    assert_eq!(p.geometry.resolution_x, WIDTH);
    assert_eq!(p.geometry.resolution_y, HEIGHT);
    assert_eq!(p.geometry.display_width_mm, Some(218.88));
    assert_eq!(p.geometry.display_height_mm, Some(122.88));
    assert_eq!(p.geometry.machine_z_mm, Some(260.0));
    assert_eq!(p.exposure.layer_height_mm, 0.05);
    assert_eq!(p.exposure.exposure_s, 2.5);
    assert_eq!(p.exposure.bottom_exposure_s, Some(30.0));
    assert_eq!(p.exposure.bottom_layers, Some(2));
    assert_eq!(p.exposure.light_pwm, Some(255));
    assert_eq!(p.exposure.bottom_light_pwm, Some(180));
    assert_eq!(p.lift.lift_height_mm, Some(6.0));
    assert_eq!(p.lift.lift_speed_mm_min, Some(80.0));
    assert_eq!(p.lift.bottom_lift_height_mm, Some(5.0));
    assert_eq!(p.lift.bottom_lift_speed_mm_min, Some(65.0));
    assert_eq!(p.lift.retract_speed_mm_min, Some(150.0));
    assert_eq!(p.material_volume_ml, Some(12.5));
    assert_eq!(p.material_grams, Some(14.0));
    assert_eq!(p.print_time_s, Some(600));
    assert_eq!(p.layer_count(), 4);
    assert_eq!(opened.layers.dimensions(), (WIDTH, HEIGHT));

    // Per-layer values come from the layer table, not the header.
    for (i, layer) in p.layers.iter().enumerate() {
        let z = 0.05 * (i + 1) as f32;
        assert!(
            (layer.z_mm - z).abs() < 1e-5,
            "layer {i} z is {} not {z}",
            layer.z_mm
        );
        assert_eq!(layer.exposure_s, Some(2.5));
        assert_eq!(layer.light_off_delay_s, Some(0.5));
    }

    // And the pixels, which is what the run-length encoding and the cipher
    // both come down to.
    for i in 0..4u32 {
        let image = opened
            .layers
            .layer(i)
            .unwrap_or_else(|e| panic!("{name} layer {i}: {e}"));
        let want = expected_layer(i);
        assert_eq!(
            image.pixels.len(),
            want.len(),
            "{name} layer {i} is the wrong size"
        );
        let wrong = image
            .pixels
            .iter()
            .zip(&want)
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(wrong, 0, "{name} layer {i}: {wrong} pixels differ");
    }
}

#[test]
fn a_file_written_by_catibo_reads() {
    check("catibo-plain.ctb");
}

#[test]
fn an_obfuscated_file_written_by_catibo_reads() {
    // The same content with the layer data put through the keyed XOR stream
    // that files from Chitubox use. Same pixels must come back.
    check("catibo-encrypted.ctb");
}

#[test]
fn the_two_reference_files_differ_on_disk() {
    // Otherwise the pair above would prove nothing about the cipher.
    let plain = std::fs::read(fixture("catibo-plain.ctb")).unwrap();
    let secret = std::fs::read(fixture("catibo-encrypted.ctb")).unwrap();
    assert_ne!(plain, secret);
}

#[test]
fn a_reference_file_converts_to_goo_pixel_for_pixel() {
    use cheapazsla_core::convert;
    let src = fixture("catibo-encrypted.ctb");
    let dir = tempfile::tempdir().unwrap();
    let dst = convert::destination_for(&src, "goo", Some(dir.path())).expect("destination");
    let plan = convert::plan(&src, "goo", &dst).expect("plan");
    convert::run(&plan).expect("convert");

    let written = registry::open(&dst).expect("open goo");
    for i in 0..4u32 {
        assert_eq!(
            written.layers.layer(i).unwrap().pixels,
            expected_layer(i),
            "layer {i} changed on the way to GOO"
        );
    }
}
