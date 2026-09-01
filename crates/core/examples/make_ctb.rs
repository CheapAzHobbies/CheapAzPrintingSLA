//! Write a CTB with a given number of layers, for testing other readers
//! against.
//!
//!     cargo run --example make_ctb -- OUT.ctb LAYERS [WIDTH HEIGHT]

use cheapazsla_core::layers::InMemoryLayers;
use cheapazsla_core::model::*;
use cheapazsla_core::registry;
use std::collections::BTreeMap;

fn main() {
    let mut a = std::env::args().skip(1);
    let out = a.next().expect("output path");
    let count: u32 = a.next().expect("layer count").parse().unwrap();
    let w: u32 = a.next().map(|s| s.parse().unwrap()).unwrap_or(256);
    let h: u32 = a.next().map(|s| s.parse().unwrap()).unwrap_or(128);

    let images: Vec<LayerImage> = (0..count)
        .map(|i| LayerImage {
            width: w,
            height: h,
            pixels: (0..w * h)
                .map(|p| if (p / w + i) % 8 < 4 { 254 } else { 0 })
                .collect(),
        })
        .collect();

    let print = PrintFile {
        source_format: "test".into(),
        geometry: Geometry {
            resolution_x: w,
            resolution_y: h,
            display_width_mm: Some(218.88),
            display_height_mm: Some(122.88),
            machine_z_mm: Some(260.0),
        },
        exposure: Exposure {
            layer_height_mm: 0.05,
            exposure_s: 2.5,
            bottom_exposure_s: Some(30.0),
            bottom_layers: Some(4),
            light_off_delay_s: Some(0.5),
            bottom_light_off_delay_s: Some(1.0),
            transition_layers: None,
            light_pwm: Some(255),
            bottom_light_pwm: Some(255),
        },
        lift: Lift {
            lift_height_mm: Some(6.0),
            lift_speed_mm_min: Some(80.0),
            bottom_lift_height_mm: Some(5.0),
            bottom_lift_speed_mm_min: Some(65.0),
            retract_speed_mm_min: Some(150.0),
            bottom_retract_speed_mm_min: Some(150.0),
        },
        layers: (0..count)
            .map(|i| LayerInfo {
                z_mm: 0.05 * (i + 1) as f32,
                exposure_s: None,
                light_off_delay_s: None,
                lift_height_mm: None,
                lift_speed_mm_min: None,
            })
            .collect(),
        thumbnails: Vec::new(),
        print_time_s: Some(600),
        material_volume_ml: Some(12.5),
        material_grams: Some(14.0),
        material_name: None,
        machine_name: None,
        extra: BTreeMap::new(),
    };

    registry::by_id("ctb")
        .expect("ctb")
        .write(
            std::path::Path::new(&out),
            &print,
            &InMemoryLayers::new(images, w, h),
        )
        .expect("write");
    println!("wrote {out}: {count} layers at {w}x{h}");
}
