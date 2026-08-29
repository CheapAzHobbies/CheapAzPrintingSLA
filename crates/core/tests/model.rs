//! Tests for the common data model and the safety limits.

use cheapazsla_core::error::{Error, FormatError};
use cheapazsla_core::layers::{CachedLayers, InMemoryLayers, LayerProvider};
use cheapazsla_core::limits;
use cheapazsla_core::model::*;
use std::collections::BTreeMap;

fn geometry() -> Geometry {
    Geometry {
        resolution_x: 11520,
        resolution_y: 5120,
        display_width_mm: Some(218.88),
        display_height_mm: Some(122.88),
        machine_z_mm: Some(220.0),
    }
}

fn print_file(layers: Vec<LayerInfo>) -> PrintFile {
    PrintFile {
        source_format: "test".into(),
        geometry: geometry(),
        exposure: Exposure {
            layer_height_mm: 0.05,
            exposure_s: 2.5,
            bottom_exposure_s: Some(30.0),
            bottom_layers: Some(5),
            light_off_delay_s: None,
            bottom_light_off_delay_s: None,
            transition_layers: None,
            light_pwm: Some(255),
            bottom_light_pwm: Some(255),
        },
        lift: Lift::default(),
        layers,
        thumbnails: vec![],
        print_time_s: None,
        material_volume_ml: None,
        material_grams: None,
        material_name: None,
        machine_name: None,
        extra: BTreeMap::new(),
    }
}

fn layer_at(z: f32) -> LayerInfo {
    LayerInfo {
        z_mm: z,
        exposure_s: None,
        light_off_delay_s: None,
        lift_height_mm: None,
        lift_speed_mm_min: None,
    }
}

#[test]
fn pixel_size_is_derived_from_display_and_resolution() {
    let (x, y) = geometry().pixel_size_um().expect("both dimensions known");
    assert!((x - 19.0).abs() < 0.1, "got {x}");
    assert!((y - 24.0).abs() < 0.1, "got {y}");
}

#[test]
fn pixel_size_is_absent_when_display_size_is_unknown() {
    let g = Geometry {
        display_width_mm: None,
        ..geometry()
    };
    assert!(g.pixel_size_um().is_none(), "must not invent a value (§13)");
}

#[test]
fn pixel_size_does_not_divide_by_zero() {
    let g = Geometry {
        resolution_x: 0,
        ..geometry()
    };
    assert!(g.pixel_size_um().is_none());
}

#[test]
fn bottom_layers_use_bottom_exposure() {
    let p = print_file((0..10).map(|i| layer_at(0.05 * (i + 1) as f32)).collect());
    assert_eq!(
        p.effective_exposure_s(0),
        Some(30.0),
        "layer 0 is a bottom layer"
    );
    assert_eq!(
        p.effective_exposure_s(4),
        Some(30.0),
        "layer 4 is the last bottom layer"
    );
    assert_eq!(
        p.effective_exposure_s(5),
        Some(2.5),
        "layer 5 is a normal layer"
    );
}

#[test]
fn a_per_layer_override_wins_over_bottom_rules() {
    let mut layers: Vec<_> = (0..10).map(|i| layer_at(0.05 * (i + 1) as f32)).collect();
    layers[1].exposure_s = Some(12.0);
    let p = print_file(layers);
    assert_eq!(p.effective_exposure_s(1), Some(12.0));
}

#[test]
fn exposure_of_a_layer_past_the_end_is_none() {
    let p = print_file(vec![layer_at(0.05)]);
    assert_eq!(p.effective_exposure_s(99), None);
}

#[test]
fn height_comes_from_the_top_layer() {
    let p = print_file(vec![layer_at(0.05), layer_at(0.10), layer_at(0.15)]);
    assert_eq!(p.layer_count(), 3);
    assert_eq!(p.height_mm(), Some(0.15));
}

#[test]
fn an_empty_print_has_no_height() {
    assert_eq!(print_file(vec![]).height_mm(), None);
}

#[test]
fn blank_layers_report_themselves_as_blank() {
    let img = LayerImage::blank(64, 32);
    assert_eq!(img.pixels.len(), 64 * 32);
    assert!(img.is_blank());
    assert_eq!(img.exposed_pixels(0), 0);
}

#[test]
fn exposed_pixels_respects_the_threshold() {
    let mut img = LayerImage::blank(4, 1);
    img.pixels = vec![0, 10, 128, 255];
    assert!(!img.is_blank());
    assert_eq!(img.exposed_pixels(0), 3);
    assert_eq!(img.exposed_pixels(100), 2);
    assert_eq!(img.exposed_pixels(254), 1);
}

// --- safety limits (§42) ---

#[test]
fn a_range_inside_the_file_is_accepted() {
    assert!(limits::check_range(0, 100, 100).is_ok());
    assert!(limits::check_range(50, 50, 100).is_ok());
}

#[test]
fn a_range_past_the_end_is_rejected() {
    let err = limits::check_range(50, 51, 100).unwrap_err();
    assert!(matches!(
        err,
        Error::Format(FormatError::OffsetOutOfBounds { .. })
    ));
}

#[test]
fn an_offset_and_length_that_would_overflow_are_rejected() {
    // A crafted file can pick values whose sum wraps to look in bounds.
    let err = limits::check_range(u64::MAX, 10, 100).unwrap_err();
    assert!(matches!(
        err,
        Error::Format(FormatError::OffsetOutOfBounds { .. })
    ));
}

#[test]
fn an_oversized_allocation_is_refused() {
    assert!(limits::check_allocation(1024).is_ok());
    let err = limits::check_allocation(limits::MAX_ALLOCATION + 1).unwrap_err();
    assert!(matches!(
        err,
        Error::Format(FormatError::AllocationTooLarge { .. })
    ));
}

#[test]
fn zero_and_absurd_resolutions_are_refused() {
    assert!(limits::check_resolution(1920, 1080).is_ok());
    assert!(limits::check_resolution(0, 1080).is_err());
    assert!(limits::check_resolution(1920, 0).is_err());
    assert!(limits::check_resolution(100_000, 100_000).is_err());
}

#[test]
fn an_absurd_layer_count_is_refused() {
    assert_eq!(limits::check_layer_count(5000).unwrap(), 5000);
    assert!(limits::check_layer_count(limits::MAX_LAYERS as u64 + 1).is_err());
}

// --- lazy layer access (§15) ---

fn provider(n: u32) -> InMemoryLayers {
    let layers = (0..n)
        .map(|i| {
            let mut img = LayerImage::blank(8, 8);
            img.pixels[0] = i as u8;
            img
        })
        .collect();
    InMemoryLayers::new(layers, 8, 8)
}

#[test]
fn layers_can_be_fetched_by_index() {
    let p = provider(5);
    assert_eq!(p.layer_count(), 5);
    assert_eq!(p.dimensions(), (8, 8));
    assert_eq!(p.layer(3).unwrap().pixels[0], 3);
}

#[test]
fn fetching_a_layer_past_the_end_is_an_error_not_a_panic() {
    let err = provider(5).layer(99).unwrap_err();
    assert!(matches!(
        err,
        Error::LayerOutOfRange {
            index: 99,
            count: 5
        }
    ));
}

#[test]
fn the_cache_holds_recent_layers_and_evicts_the_oldest() {
    let cached = CachedLayers::new(provider(20), 3);
    for i in 0..3 {
        cached.layer(i).unwrap();
    }
    assert_eq!(cached.cached_len(), 3);
    cached.layer(4).unwrap();
    assert_eq!(cached.cached_len(), 3, "capacity is respected");
    // Still correct after eviction.
    assert_eq!(cached.layer(0).unwrap().pixels[0], 0);
    assert_eq!(cached.layer(4).unwrap().pixels[0], 4);
}

#[test]
fn clearing_the_cache_empties_it() {
    let cached = CachedLayers::new(provider(10), 4);
    cached.layer(1).unwrap();
    assert_eq!(cached.cached_len(), 1);
    cached.clear();
    assert_eq!(cached.cached_len(), 0);
    assert_eq!(
        cached.layer(1).unwrap().pixels[0],
        1,
        "still readable after a clear"
    );
}

// --- capability honesty (§47) ---

/// A handler that advertises writing must actually write.
///
/// The interface builds its output format list from these flags, so a format
/// claiming a capability it does not have puts an option in front of the user
/// that fails when chosen. This test exists because that happened.
#[test]
fn every_format_claiming_to_write_can_actually_write() {
    use cheapazsla_core::layers::InMemoryLayers;
    use cheapazsla_core::registry;
    use std::collections::BTreeMap;

    let print = PrintFile {
        source_format: "test".into(),
        geometry: Geometry {
            resolution_x: 64,
            resolution_y: 32,
            display_width_mm: Some(12.8),
            display_height_mm: Some(6.4),
            machine_z_mm: Some(100.0),
        },
        exposure: Exposure {
            layer_height_mm: 0.05,
            exposure_s: 2.5,
            bottom_exposure_s: Some(30.0),
            bottom_layers: Some(2),
            light_off_delay_s: Some(0.5),
            bottom_light_off_delay_s: None,
            transition_layers: Some(1),
            light_pwm: Some(255),
            bottom_light_pwm: Some(255),
        },
        lift: Lift::default(),
        layers: (0..4)
            .map(|i| LayerInfo {
                z_mm: 0.05 * (i + 1) as f32,
                exposure_s: None,
                light_off_delay_s: None,
                lift_height_mm: None,
                lift_speed_mm_min: None,
            })
            .collect(),
        thumbnails: vec![],
        print_time_s: Some(120),
        material_volume_ml: Some(1.5),
        material_grams: None,
        material_name: None,
        machine_name: Some("Test".into()),
        extra: BTreeMap::new(),
    };
    let images: Vec<LayerImage> = (0..4)
        .map(|i| {
            let mut img = LayerImage::blank(64, 32);
            // Mix black, white and grey so every chunk type is exercised.
            for (n, px) in img.pixels.iter_mut().enumerate() {
                *px = match (n + i) % 3 {
                    0 => 0,
                    1 => 255,
                    _ => 128,
                };
            }
            img
        })
        .collect();
    let provider = InMemoryLayers::new(images, 64, 32);

    let dir = tempfile::tempdir().unwrap();
    for handler in registry::handlers() {
        let info = handler.info();
        if !info.capabilities.writes {
            continue;
        }
        let path = dir.path().join(format!("out.{}", info.extension));
        let result = handler.write(&path, &print, &provider);
        assert!(
            result.is_ok(),
            "{} advertises writes: true but writing failed: {:?}",
            info.name,
            result.err()
        );
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert!(size > 0, "{} wrote an empty file", info.name);
    }
}

/// Likewise for reading.
#[test]
fn every_format_claiming_to_read_has_a_working_opener() {
    use cheapazsla_core::registry;
    let dir = tempfile::tempdir().unwrap();
    for handler in registry::handlers() {
        let info = handler.info();
        if !info.capabilities.reads {
            continue;
        }
        // Opening nonsense must fail with an error, never a panic, and never
        // by claiming success.
        let path = dir.path().join(format!("junk.{}", info.extension));
        std::fs::write(&path, vec![0x00; 512]).unwrap();
        assert!(
            handler.open(&path).is_err(),
            "{} claims to read but accepted 512 zero bytes as a valid file",
            info.name
        );
    }
}
