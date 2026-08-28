//! Hostile and broken input (§42).
//!
//! Every case here must produce an error a user could act on. None of them
//! may panic, hang, or allocate without bound.

use cheapazsla_core::format::Confidence;
use cheapazsla_core::registry;
use std::io::Write;
use std::path::PathBuf;
use zip::write::SimpleFileOptions;

/// Build an SL1-shaped archive from the given entries.
fn archive(name: &str, entries: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for (n, data) in entries {
        zip.start_file(*n, SimpleFileOptions::default()).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
    (dir, path)
}

fn tiny_png() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, 4, 4);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[0u8; 16]).unwrap();
    }
    out
}

const GOOD_CONFIG: &[u8] = b"layerHeight = 0.05\nexpTime = 2.5\nnumSlow = 1\n";

#[test]
fn an_empty_file_is_not_identified_as_anything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.sl1");
    std::fs::write(&path, b"").unwrap();
    assert!(registry::identify(&path).is_err(), "an empty file must not identify");
}

#[test]
fn random_bytes_named_sl1_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("junk.sl1");
    std::fs::write(&path, vec![0xAB; 4096]).unwrap();
    assert!(registry::identify(&path).is_err(), "contents decide, not the name (§11)");
}

#[test]
fn a_truncated_zip_is_low_confidence_and_fails_to_open() {
    let (_d, good) = archive("ok.sl1", &[("config.ini", GOOD_CONFIG), ("a00000.png", &tiny_png())]);
    let bytes = std::fs::read(&good).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cut.sl1");
    std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();

    // The name still says SL1 and the header is still a ZIP magic, so this is
    // the ambiguous case of §11: report low confidence rather than claim
    // nothing was found, and let opening be the thing that fails.
    match registry::identify(&path) {
        Ok(id) => {
            assert_eq!(id.detection.format_id, "sl1");
            assert_eq!(id.detection.confidence, Confidence::Low);
        }
        Err(_) => {} // also acceptable
    }
    assert!(registry::open(&path).is_err(), "a truncated archive must not open");
}

#[test]
fn an_archive_with_no_layers_is_refused_with_a_clear_message() {
    let (_d, path) = archive("nolayers.sl1", &[("config.ini", GOOD_CONFIG)]);
    let err = registry::open(&path).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("layer images"), "unhelpful message: {msg}");
}

#[test]
fn a_missing_layer_height_is_reported_by_name() {
    let (_d, path) = archive(
        "noheight.sl1",
        &[("config.ini", b"expTime = 2.5\n"), ("a00000.png", &tiny_png())],
    );
    let err = registry::open(&path).unwrap_err();
    assert!(err.to_string().contains("layerHeight"), "got: {err}");
}

#[test]
fn a_negative_layer_height_is_refused() {
    let (_d, path) = archive(
        "neg.sl1",
        &[
            ("config.ini", b"layerHeight = -0.05\nexpTime = 2.5\n"),
            ("a00000.png", &tiny_png()),
        ],
    );
    let err = registry::open(&path).unwrap_err();
    assert!(err.to_string().contains("positive"), "got: {err}");
}

#[test]
fn a_layer_that_is_not_a_png_fails_to_decode_cleanly() {
    let (_d, path) = archive(
        "badpng.sl1",
        &[("config.ini", GOOD_CONFIG), ("a00000.png", b"not a png at all")],
    );
    // Opening reads geometry from the first layer, so it fails there.
    let err = registry::open(&path).unwrap_err();
    assert!(!err.to_string().is_empty());
}

#[test]
fn a_declared_layer_count_that_disagrees_produces_a_warning_not_a_failure() {
    let (_d, path) = archive(
        "mismatch.sl1",
        &[
            ("config.ini", b"layerHeight = 0.05\nexpTime = 2.5\nnumFast = 99\nnumSlow = 1\n"),
            ("a00000.png", &tiny_png()),
        ],
    );
    let handler = registry::by_id("sl1").unwrap();
    let warnings = handler.validate(&path).expect("validation should succeed");
    assert_eq!(warnings.len(), 1, "expected one warning, got {warnings:?}");
    assert!(warnings[0].contains("100") && warnings[0].contains("1"), "got: {warnings:?}");
}

#[test]
fn a_zip_bomb_style_entry_is_capped_by_the_allocation_limit() {
    // A highly compressible entry that would expand to far more than the cap.
    let huge = vec![0u8; 64 * 1024 * 1024];
    let (_d, path) = archive(
        "bomb.sl1",
        &[("config.ini", GOOD_CONFIG), ("a00000.png", &huge)],
    );
    // Must terminate and error rather than exhaust memory.
    let err = registry::open(&path).unwrap_err();
    assert!(!err.to_string().is_empty());
}

#[test]
fn an_sl1_named_with_the_wrong_extension_is_still_identified_by_content() {
    let (_d, path) = archive(
        "actually_sl1.goo",
        &[("config.ini", GOOD_CONFIG), ("a00000.png", &tiny_png())],
    );
    let id = registry::identify(&path).expect("content should win over the name");
    assert_eq!(id.detection.format_id, "sl1");
}
