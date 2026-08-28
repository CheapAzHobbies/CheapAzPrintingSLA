//! SL1 to GOO conversion end to end (§13, §40).

use cheapazsla_core::convert;
use std::path::PathBuf;

fn real_sl1() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("CHEAPAZSLA_REAL_SL1").ok()?);
    p.exists().then_some(p)
}

#[test]
fn converts_a_real_sl1_into_a_goo() {
    let Some(src) = real_sl1() else {
        eprintln!("skipped: set CHEAPAZSLA_REAL_SL1");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let dst = convert::destination_for(&src, "goo", Some(dir.path())).expect("destination");
    assert_eq!(dst.extension().unwrap(), "goo", "extension is swapped, stem kept");

    let plan = convert::plan(&src, "goo", &dst).expect("plan");
    println!("  {} -> {}", plan.from.name, plan.to.name);
    println!("  {} layers", plan.layer_count);
    for l in &plan.losses {
        println!("  loses: {} ({})", l.what, l.because);
    }

    convert::run(&plan).expect("convert");
    let written = std::fs::metadata(&dst).unwrap().len();
    println!("  wrote {written} bytes");
    assert!(written > 1000, "output is implausibly small");

    // The header must describe itself consistently.
    let bytes = std::fs::read(&dst).unwrap();
    assert_eq!(&bytes[..4], b"V3.0");
    assert_eq!(&bytes[4..12], &[0x07, 0x00, 0x00, 0x00, 0x44, 0x4C, 0x50, 0x00]);
    assert_eq!(
        &bytes[bytes.len() - 11..],
        &[0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x44, 0x4C, 0x50, 0x00],
        "ending marker"
    );

    let params = 194 + 0x6920 + 2 + 0x29108 + 2;
    let be32 = |o: usize| u32::from_be_bytes(bytes[o..o + 4].try_into().unwrap());
    let be16 = |o: usize| u16::from_be_bytes(bytes[o..o + 2].try_into().unwrap());
    assert_eq!(be32(params), plan.layer_count, "layer count in header");

    let off_field = params + 4 + 2 + 2 + 1 + 1 + 4 * 4 + 4 + 1 + 4 * 7 + 4 + 4 + 4 * 16
        + 2 + 2 + 1 + 4 + 4 + 4 + 4 + 8;
    let layer_offset = be32(off_field) as usize;
    println!(
        "  header says layers start at {layer_offset}, resolution {}x{}",
        be16(params + 4),
        be16(params + 6)
    );
    assert_eq!(
        layer_offset,
        off_field + 4 + 1 + 2,
        "offset_of_layer_content must point just past the header"
    );
    assert!(layer_offset < bytes.len(), "offset must be inside the file");

    // Every layer's checksum must agree with its payload.
    let mut p = layer_offset;
    for i in 0..plan.layer_count {
        p += 2 + 15 * 4 + 2 + 2; // layer record before the image
        let size = u32::from_be_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
        assert_eq!(bytes[p + 4], 0x55, "layer {i} image magic");
        let payload = &bytes[p + 5..p + 4 + size - 1];
        let stored = bytes[p + 4 + size - 1];
        assert_eq!(
            cheapazsla_core::formats::goo_rle::checksum(payload),
            stored,
            "layer {i} checksum"
        );
        assert_eq!(&bytes[p + 4 + size..p + 6 + size], &[0x0D, 0x0A], "layer {i} delimiter");
        p += 4 + size + 2;
    }
    assert_eq!(p, bytes.len() - 11, "layers must end exactly at the trailer");
    println!("  all {} layers verified", plan.layer_count);

    std::fs::copy(&dst, "/tmp/claude-1000/-home-bao/b9cc5c32-916f-482d-9377-c2783027e087/scratchpad/mine.goo").ok();
}

#[test]
fn round_trips_pixels_through_the_encoder() {
    // Encode then decode a known pattern: what comes back must be identical.
    use cheapazsla_core::formats::goo_rle;
    let mut pixels = vec![0u8; 1000];
    for (i, p) in pixels.iter_mut().enumerate() {
        *p = match i % 200 {
            0..=49 => 0,
            50..=99 => 255,
            100..=149 => 128,
            _ => (i % 256) as u8,
        };
    }
    let (payload, covered) = goo_rle::encode(&pixels, 0);
    assert_eq!(covered, pixels.len() as u64, "every pixel must be covered");
    let back = goo_rle::decode(&payload, pixels.len()).expect("decode");
    assert_eq!(back, pixels, "encode then decode must be lossless");
}

#[test]
fn a_long_run_is_split_across_chunks_correctly() {
    use cheapazsla_core::formats::goo_rle;
    for len in [15usize, 16, 4095, 4096, 1_048_576] {
        let pixels = vec![0xFFu8; len];
        let (payload, covered) = goo_rle::encode(&pixels, 0);
        assert_eq!(covered, len as u64, "run of {len}");
        let back = goo_rle::decode(&payload, len).expect("decode");
        assert_eq!(back.len(), len, "run of {len} decoded to the wrong length");
        assert!(back.iter().all(|&p| p == 0xFF), "run of {len} changed value");
    }
}

#[test]
fn every_layer_survives_the_conversion_pixel_for_pixel() {
    use cheapazsla_core::formats::goo_rle;
    use cheapazsla_core::layers::LayerProvider;
    use cheapazsla_core::registry;

    let Some(src) = real_sl1() else {
        eprintln!("skipped: set CHEAPAZSLA_REAL_SL1");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let dst = convert::destination_for(&src, "goo", Some(dir.path())).unwrap();
    let plan = convert::plan(&src, "goo", &dst).unwrap();
    convert::run(&plan).unwrap();

    let bytes = std::fs::read(&dst).unwrap();
    let opened = registry::open(&src).unwrap();
    let (w, h) = opened.layers.dimensions();
    let pixels_per_layer = (w * h) as usize;

    let params = 194 + 0x6920 + 2 + 0x29108 + 2;
    let off_field = params + 4 + 2 + 2 + 1 + 1 + 4 * 4 + 4 + 1 + 4 * 7 + 4 + 4 + 4 * 16
        + 2 + 2 + 1 + 4 + 4 + 4 + 4 + 8;
    let mut p = u32::from_be_bytes(bytes[off_field..off_field + 4].try_into().unwrap()) as usize;

    for index in 0..plan.layer_count {
        p += 2 + 15 * 4 + 2 + 2;
        let size = u32::from_be_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
        let payload = &bytes[p + 5..p + 4 + size - 1];
        let decoded = goo_rle::decode(payload, pixels_per_layer)
            .unwrap_or_else(|e| panic!("layer {index} decode: {e}"));
        assert_eq!(decoded.len(), pixels_per_layer, "layer {index} pixel count");

        let original = opened.layers.layer(index).unwrap();
        assert_eq!(decoded, original.pixels, "layer {index} differs after conversion");
        p += 4 + size + 2;
    }
    println!(
        "  all {} layers identical after SL1 -> GOO ({} pixels each)",
        plan.layer_count, pixels_per_layer
    );
}

#[test]
fn conversion_reports_progress_for_every_layer() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let Some(src) = real_sl1() else {
        eprintln!("skipped: set CHEAPAZSLA_REAL_SL1");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let dst = convert::destination_for(&src, "goo", Some(dir.path())).unwrap();
    let plan = convert::plan(&src, "goo", &dst).unwrap();

    let seen = Arc::new(AtomicU32::new(0));
    let last = Arc::new(AtomicU32::new(0));
    let reported_total = Arc::new(AtomicU32::new(0));
    {
        let (seen, last, reported_total) = (seen.clone(), last.clone(), reported_total.clone());
        convert::run_with_progress(&plan, move |done, total| {
            seen.fetch_add(1, Ordering::Relaxed);
            last.store(done, Ordering::Relaxed);
            reported_total.store(total, Ordering::Relaxed);
        })
        .expect("convert");
    }

    let n = seen.load(Ordering::Relaxed);
    assert_eq!(n, plan.layer_count, "one report per layer");
    assert_eq!(last.load(Ordering::Relaxed), plan.layer_count, "final report is the last layer");
    assert_eq!(reported_total.load(Ordering::Relaxed), plan.layer_count, "total is reported");
    println!("  {n} progress reports for {} layers", plan.layer_count);
}
