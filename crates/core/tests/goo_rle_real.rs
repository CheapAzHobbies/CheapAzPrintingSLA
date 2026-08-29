//! Verify the GOO RLE codec against a real Elegoo file (§41).
//!
//! The strongest check available: the same model exists as both an SL1 and a
//! GOO. Decode layer 0 out of the GOO with our codec and compare it to the
//! same layer read from the SL1. If the RLE reading is wrong the pixels will
//! not agree.
//!
//!   CHEAPAZSLA_REAL_GOO=<file.goo> CHEAPAZSLA_REAL_SL1=<same.sl1> cargo test

use cheapazsla_core::formats::goo_rle;
use cheapazsla_core::layers::LayerProvider;
use cheapazsla_core::registry;
use std::path::PathBuf;

fn env_file(key: &str) -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var(key).ok()?);
    p.exists().then_some(p)
}

/// Pull the header numbers and the first layer payload out of a GOO file.
fn first_layer_payload(bytes: &[u8]) -> (u32, u32, u32, Vec<u8>, u8) {
    let be32 = |o: usize| u32::from_be_bytes(bytes[o..o + 4].try_into().unwrap());
    let be16 = |o: usize| u16::from_be_bytes(bytes[o..o + 2].try_into().unwrap());

    // Fixed by the format: two previews of known size, each followed by 0x0D0A.
    let params = 194 + 0x6920 + 2 + 0x29108 + 2;
    let layers = be32(params);
    let xres = be16(params + 4) as u32;
    let yres = be16(params + 6) as u32;

    // offset_of_layer_content sits after the fixed run of parameters.
    let off_field = params + 4 + 2 + 2 + 1 + 1
        + 4 * 4          // x_size, y_size, z_size, layer_thickness
        + 4              // common_exposure_time
        + 1              // exposure_delay_mode
        + 4 * 7          // turn_off + six lift/retract times
        + 4 + 4          // bottom_exposure_time, bottom_layers
        + 4 * 16         // lift and retract distances and speeds
        + 2 + 2 + 1      // pwm x2, advance_mode
        + 4 + 4 + 4 + 4  // printing_time, volume, weight, price
        + 8; // price_unit
    let layer_content = be32(off_field) as usize;

    // LayerContent: u16 + 15 floats + u16 + 2 byte delimiter, then the image.
    let img = layer_content + 2 + 15 * 4 + 2 + 2;
    let data_size = be32(img) as usize;
    let _magic = bytes[img + 4];
    let payload = bytes[img + 5..img + 4 + data_size - 1].to_vec();
    let checksum = bytes[img + 4 + data_size - 1];
    (layers, xres, yres, payload, checksum)
}

#[test]
fn our_decoder_reproduces_a_real_goo_layer() {
    let Some(goo) = env_file("CHEAPAZSLA_REAL_GOO") else {
        eprintln!("skipped: set CHEAPAZSLA_REAL_GOO");
        return;
    };
    let bytes = std::fs::read(&goo).unwrap();
    let (layers, xres, yres, payload, checksum) = first_layer_payload(&bytes);
    println!(
        "  goo: {layers} layers, {xres}x{yres}, payload {} bytes",
        payload.len()
    );

    // The checksum must agree, or we are not looking at the payload we think.
    assert_eq!(
        goo_rle::checksum(&payload),
        checksum,
        "checksum mismatch: our understanding of the payload bounds is wrong"
    );

    let pixels = goo_rle::decode(&payload, (xres * yres) as usize).expect("decode");
    assert_eq!(
        pixels.len(),
        (xres * yres) as usize,
        "decoded pixel count must fill the panel exactly"
    );
    let exposed = pixels.iter().filter(|&&p| p > 0).count();
    println!("  decoded {} pixels, {exposed} exposed", pixels.len());
}

#[test]
fn a_goo_layer_matches_the_same_layer_from_the_sl1() {
    let (Some(goo), Some(sl1)) = (
        env_file("CHEAPAZSLA_REAL_GOO"),
        env_file("CHEAPAZSLA_REAL_SL1"),
    ) else {
        eprintln!("skipped: set CHEAPAZSLA_REAL_GOO and CHEAPAZSLA_REAL_SL1");
        return;
    };
    let bytes = std::fs::read(&goo).unwrap();
    let (_, xres, yres, payload, _) = first_layer_payload(&bytes);
    let from_goo = goo_rle::decode(&payload, (xres * yres) as usize).expect("decode");

    let opened = registry::open(&sl1).expect("open sl1");
    let from_sl1 = opened.layers.layer(0).expect("sl1 layer 0");
    assert_eq!(
        (from_sl1.width, from_sl1.height),
        (xres, yres),
        "the two files should describe the same panel"
    );

    let same: usize = from_goo
        .iter()
        .zip(from_sl1.pixels.iter())
        .filter(|(a, b)| a == b)
        .count();
    let pct = same as f64 / from_goo.len() as f64 * 100.0;
    println!("  {pct:.4}% of pixels identical between the GOO and the SL1");
    assert!(
        pct > 99.9,
        "layers should be the same image; only {pct:.4}% of pixels matched"
    );
}
