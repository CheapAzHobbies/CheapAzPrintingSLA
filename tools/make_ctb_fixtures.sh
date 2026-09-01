#!/usr/bin/env bash
# Regenerate the CTB reference files in crates/core/tests/data.
#
# They are built by catibo, an independent reverse engineering of the format,
# so that the CTB tests check the reader against somebody else's understanding
# rather than only against its own. See that directory's README.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$root/crates/core/tests/data"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

git clone --depth 1 https://github.com/cbiffle/catibo "$work/catibo"
mkdir -p "$work/catibo/examples"

cat > "$work/catibo/examples/make_reference.rs" <<'RS'
use catibo::output::{encode_rle7_slice, Builder};
use catibo::Magic;

const W: u32 = 64;
const H: u32 = 32;

fn layer_pixels(kind: u32) -> Vec<u8> {
    (0..W * H)
        .map(|i| match kind {
            0 => 0u8,
            1 => 254u8,
            2 => ((i * 7) % 256) as u8,
            _ => {
                let (x, y) = ((i % W) as f32, (i / W) as f32);
                let (cx, cy) = (W as f32 / 2.0, H as f32 / 2.0);
                let r = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
                if r < W as f32 / 3.0 { 254 } else { 0 }
            }
        })
        .collect()
}

fn build(path: &str, key: u32) {
    let mut b = Builder::for_revision(Magic::CTB, 3);
    b.printer_out_mm([218.88, 122.88, 260.0])
        .resolution([W, H])
        .layer_height_mm(0.05)
        .exposure_s(2.5)
        .bot_exposure_s(30.0)
        .bot_layer_count(2)
        .light_off_time_s(0.5)
        .bot_light_off_time_s(1.0)
        .lift_dist_mm(6.0)
        .lift_speed_mmpm(80.0)
        .bot_lift_dist_mm(5.0)
        .bot_lift_speed_mmpm(65.0)
        .retract_speed_mmpm(150.0)
        .print_volume_ml(12.5)
        .print_mass_g(14.0)
        .print_time_s(600)
        .pwm_level(255)
        .bot_pwm_level(180)
        .encryption_key(key)
        .overall_height_mm(0.2);
    for i in 0..4u32 {
        let mut encoded = Vec::new();
        encode_rle7_slice(layer_pixels(i).into_iter().peekable(), key, i, &mut encoded);
        b.layer(0.05 * (i + 1) as f32, 2.5, 0.5, encoded);
    }
    b.write(std::fs::File::create(path).unwrap()).unwrap();
    println!("wrote {path}");
}

fn main() {
    let out = std::env::args().nth(1).expect("output directory");
    build(&format!("{out}/catibo-plain.ctb"), 0);
    build(&format!("{out}/catibo-encrypted.ctb"), 0x1234_5678);
}
RS

(cd "$work/catibo" && cargo run --release --example make_reference -- "$out")
echo "fixtures refreshed in $out"
