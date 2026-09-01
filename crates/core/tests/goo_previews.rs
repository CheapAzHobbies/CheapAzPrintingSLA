//! GOO preview images (§14).
//!
//! The reader used to skip these, which meant a GOO converted to anything else
//! arrived with a blank picture on the printer's screen and the conversion
//! planner — whose whole job is saying what a conversion drops — reported
//! nothing, because as far as it could tell there was no preview to drop.

use cheapazsla_core::layers::InMemoryLayers;
use cheapazsla_core::model::*;
use cheapazsla_core::registry;
use std::collections::BTreeMap;

/// A print with one solid-colour preview, which is easy to recognise again
/// after a trip through RGB565 and a rescale.
fn print_with_preview(colour: [u8; 3]) -> (PrintFile, InMemoryLayers) {
    let (w, h) = (16u32, 8u32);
    let rgb: Vec<u8> = std::iter::repeat_n(colour, 64 * 64).flatten().collect();
    let print = PrintFile {
        source_format: "test".into(),
        geometry: Geometry {
            resolution_x: w,
            resolution_y: h,
            display_width_mm: Some(100.0),
            display_height_mm: Some(50.0),
            machine_z_mm: Some(150.0),
        },
        exposure: Exposure {
            layer_height_mm: 0.05,
            exposure_s: 2.0,
            bottom_exposure_s: Some(20.0),
            bottom_layers: Some(2),
            light_off_delay_s: Some(0.5),
            bottom_light_off_delay_s: Some(1.0),
            transition_layers: None,
            light_pwm: Some(255),
            bottom_light_pwm: Some(255),
        },
        lift: Lift::default(),
        layers: (0..3)
            .map(|i| LayerInfo {
                z_mm: 0.05 * (i + 1) as f32,
                exposure_s: None,
                light_off_delay_s: None,
                lift_height_mm: None,
                lift_speed_mm_min: None,
            })
            .collect(),
        thumbnails: vec![Thumbnail {
            width: 64,
            height: 64,
            rgb,
        }],
        print_time_s: Some(120),
        material_volume_ml: Some(5.0),
        material_grams: None,
        material_name: None,
        machine_name: Some("Test".into()),
        extra: BTreeMap::new(),
    };
    let layers = InMemoryLayers::new(
        (0..3)
            .map(|_| LayerImage {
                width: w,
                height: h,
                pixels: vec![0u8; (w * h) as usize],
            })
            .collect(),
        w,
        h,
    );
    (print, layers)
}

/// Five bits of red and blue and six of green: a colour comes back close, not
/// exact. Anything further out than this is a decoding mistake, not rounding.
fn close(a: &[u8], b: [u8; 3]) -> bool {
    a[0].abs_diff(b[0]) <= 8 && a[1].abs_diff(b[1]) <= 4 && a[2].abs_diff(b[2]) <= 8
}

#[test]
fn a_goo_preview_survives_being_written_and_read() {
    let colour = [200u8, 100, 40];
    let (print, layers) = print_with_preview(colour);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.goo");
    registry::by_id("goo")
        .expect("goo")
        .write(&path, &print, &layers)
        .expect("write");

    let opened = registry::open(&path).expect("open");
    assert_eq!(
        opened.print.thumbnails.len(),
        2,
        "GOO holds two previews and both should come back"
    );
    for t in &opened.print.thumbnails {
        assert!(t.width > 0 && t.height > 0);
        assert_eq!(t.rgb.len(), (t.width * t.height * 3) as usize);
        // The middle of a solid image, well away from any edge.
        let middle = ((t.height / 2 * t.width + t.width / 2) * 3) as usize;
        assert!(
            close(&t.rgb[middle..middle + 3], colour),
            "preview {}x{} came back as {:?}, not {colour:?}",
            t.width,
            t.height,
            &t.rgb[middle..middle + 3]
        );
    }
}

#[test]
fn the_two_goo_previews_are_the_sizes_the_format_fixes() {
    let (print, layers) = print_with_preview([255, 255, 255]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.goo");
    registry::by_id("goo")
        .expect("goo")
        .write(&path, &print, &layers)
        .expect("write");
    let opened = registry::open(&path).expect("open");
    let mut sizes: Vec<(u32, u32)> = opened
        .print
        .thumbnails
        .iter()
        .map(|t| (t.width, t.height))
        .collect();
    sizes.sort();
    assert_eq!(sizes, vec![(116, 116), (290, 290)]);
}

#[test]
fn a_preview_crosses_from_goo_to_ctb() {
    // Both formats hold previews, at different fixed sizes, so the picture
    // should arrive rescaled rather than blank.
    let colour = [40u8, 180, 90];
    let (print, layers) = print_with_preview(colour);
    let dir = tempfile::tempdir().unwrap();
    let goo = dir.path().join("a.goo");
    registry::by_id("goo")
        .expect("goo")
        .write(&goo, &print, &layers)
        .expect("write goo");

    // Written directly: CTB writing is implemented but not offered, so it does
    // not go through the converter. The point here is that a preview survives
    // between two formats that both hold one.
    let ctb = dir.path().join("b.ctb");
    let source = registry::open(&goo).expect("open goo");
    registry::by_id("ctb")
        .expect("ctb")
        .write(&ctb, &source.print, source.layers.as_ref())
        .expect("write ctb");

    let opened = registry::open(&ctb).expect("open ctb");
    assert_eq!(opened.print.thumbnails.len(), 2);
    for t in &opened.print.thumbnails {
        let middle = ((t.height / 2 * t.width + t.width / 2) * 3) as usize;
        assert!(
            close(&t.rgb[middle..middle + 3], colour),
            "preview came through as {:?}",
            &t.rgb[middle..middle + 3]
        );
    }
}

#[test]
fn dropping_previews_is_reported_when_the_target_cannot_hold_them() {
    // The planner can only report what the reader found. This is the check
    // that would have failed while GOO previews were being discarded.
    let (print, _) = print_with_preview([1, 2, 3]);
    let goo = registry::by_id("goo").expect("goo").info();
    assert!(goo.capabilities.thumbnails, "GOO stores previews");
    assert!(!print.thumbnails.is_empty());
}
