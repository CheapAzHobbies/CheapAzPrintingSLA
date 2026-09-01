//! CTB reading: detection, hostile input, and a file built to the layout the
//! reader expects (phase 19).
//!
//! Everything here is checked against a file this project built. That proves
//! the reader is self-consistent and that it refuses broken input; it does not
//! prove the layout matches what Chitubox writes. The test at the bottom does
//! that, and skips until a real file is provided:
//!
//!     CHEAPAZSLA_REAL_CTB=/path/to/from-chitubox.ctb cargo test -p cheapazsla-core

use cheapazsla_core::format::Confidence;
use cheapazsla_core::formats::ctb_rle;
use cheapazsla_core::registry;
use std::path::PathBuf;

const MAGIC: u32 = 0x12FD_0086;

/// The same keyed XOR stream the reader undoes, so a test file can be built
/// the way Chitubox builds one.
fn encipher(data: &mut [u8], key: u32, iv: u32) {
    if key == 0 {
        return;
    }
    let step = key.wrapping_mul(0x2D83_CDAC).wrapping_add(0xD8A8_3423);
    let mut state = iv
        .wrapping_mul(0x1E15_30CD)
        .wrapping_add(0xEC3D_47CD)
        .wrapping_mul(step);
    for chunk in data.chunks_mut(4) {
        for (i, byte) in chunk.iter_mut().enumerate() {
            *byte ^= (state >> (i * 8)) as u8;
        }
        state = state.wrapping_add(step);
    }
}
const HEADER: usize = 0x70;
const PARAMS: usize = 0x28;
const LAYER_ENTRY: usize = 36;

/// Build a CTB laid out the way the reader expects.
struct Builder {
    version: u32,
    width: u32,
    height: u32,
    layers: Vec<Vec<u8>>,
    encryption_key: u32,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            version: 3,
            width: 8,
            height: 4,
            layers: vec![vec![0u8; 32], vec![255u8; 32]],
            encryption_key: 0,
        }
    }
}

impl Builder {
    fn build(&self) -> Vec<u8> {
        let mut head = vec![0u8; HEADER];
        let put32 = |b: &mut [u8], at: usize, v: u32| {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        };
        let putf = |b: &mut [u8], at: usize, v: f32| {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        };
        let put16 = |b: &mut [u8], at: usize, v: u16| {
            b[at..at + 2].copy_from_slice(&v.to_le_bytes());
        };

        let params_at = HEADER;
        let table_at = params_at + PARAMS;
        let data_at = table_at + self.layers.len() * LAYER_ENTRY;

        put32(&mut head, 0x00, MAGIC);
        put32(&mut head, 0x04, self.version);
        putf(&mut head, 0x08, 218.88);
        putf(&mut head, 0x0C, 122.88);
        putf(&mut head, 0x10, 260.0);
        putf(&mut head, 0x20, 0.05);
        putf(&mut head, 0x24, 2.5);
        putf(&mut head, 0x28, 30.0);
        putf(&mut head, 0x2C, 0.5);
        put32(&mut head, 0x30, 1);
        put32(&mut head, 0x34, self.width);
        put32(&mut head, 0x38, self.height);
        put32(&mut head, 0x3C, 0); // no large preview
        put32(&mut head, 0x40, table_at as u32);
        put32(&mut head, 0x44, self.layers.len() as u32);
        put32(&mut head, 0x48, 0); // no small preview
        put32(&mut head, 0x4C, 4321);
        put32(&mut head, 0x54, params_at as u32);
        put32(&mut head, 0x58, PARAMS as u32);
        put16(&mut head, 0x60, 255);
        put16(&mut head, 0x62, 200);
        put32(&mut head, 0x64, self.encryption_key);

        let mut params = vec![0u8; PARAMS];
        putf(&mut params, 0x00, 5.0); // bottom lift height
        putf(&mut params, 0x04, 65.0); // bottom lift speed
        putf(&mut params, 0x08, 6.0); // lift height
        putf(&mut params, 0x0C, 80.0); // lift speed
        putf(&mut params, 0x10, 150.0); // retract speed
        putf(&mut params, 0x14, 14.19); // volume
        putf(&mut params, 0x18, 15.9); // weight

        let mut table = vec![0u8; self.layers.len() * LAYER_ENTRY];
        let mut blobs: Vec<u8> = Vec::new();
        for (i, pixels) in self.layers.iter().enumerate() {
            let (mut payload, _) = ctb_rle::encode(pixels);
            // Chitubox enciphers each layer with its index as the IV. The
            // cipher is symmetric, so applying it here produces exactly what a
            // real file holds.
            encipher(&mut payload, self.encryption_key, i as u32);
            let at = i * LAYER_ENTRY;
            putf(&mut table, at, 0.05 * (i as f32 + 1.0));
            putf(&mut table, at + 0x04, 2.5);
            putf(&mut table, at + 0x08, 0.5);
            put32(&mut table, at + 0x0C, (data_at + blobs.len()) as u32);
            put32(&mut table, at + 0x10, payload.len() as u32);
            blobs.extend_from_slice(&payload);
        }

        let mut out = head;
        out.extend_from_slice(&params);
        out.extend_from_slice(&table);
        out.extend_from_slice(&blobs);
        out
    }

    fn write(&self) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.ctb");
        std::fs::write(&path, self.build()).unwrap();
        (dir, path)
    }
}

#[test]
fn a_ctb_header_is_recognised_by_its_magic() {
    let (_d, path) = Builder::default().write();
    let d = registry::identify(&path).expect("identify").detection;
    assert_eq!(d.format_id, "ctb");
    assert_eq!(d.confidence, Confidence::High, "{}", d.reason);
}

#[test]
fn the_extension_alone_is_not_enough() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lying.ctb");
    std::fs::write(&path, b"this is not a print file at all").unwrap();
    // Either nothing claims it, or CTB claims it only weakly on the name.
    if let Ok(found) = registry::identify(&path) {
        assert!(
            found.detection.confidence < Confidence::Medium,
            "a file with the wrong contents must not be claimed confidently: {}",
            found.detection.reason
        );
    }
}

#[test]
fn metadata_comes_back_as_it_was_written() {
    let (_d, path) = Builder::default().write();
    let opened = registry::open(&path).expect("open");
    let p = &opened.print;
    assert_eq!(p.source_format, "ctb");
    assert_eq!(p.geometry.resolution_x, 8);
    assert_eq!(p.geometry.resolution_y, 4);
    assert_eq!(p.exposure.layer_height_mm, 0.05);
    assert_eq!(p.exposure.exposure_s, 2.5);
    assert_eq!(p.exposure.bottom_exposure_s, Some(30.0));
    assert_eq!(p.exposure.bottom_layers, Some(1));
    assert_eq!(p.lift.lift_height_mm, Some(6.0));
    assert_eq!(p.lift.bottom_lift_height_mm, Some(5.0));
    assert_eq!(p.print_time_s, Some(4321));
    assert_eq!(p.layer_count(), 2);
    assert_eq!(opened.layers.dimensions(), (8, 4));
}

#[test]
fn layers_decode_to_the_pixels_that_went_in() {
    let (_d, path) = Builder::default().write();
    let opened = registry::open(&path).expect("open");
    let first = opened.layers.layer(0).expect("layer 0");
    assert_eq!(first.pixels.len(), 32);
    assert!(first.pixels.iter().all(|&p| p == 0), "layer 0 is all black");
    let second = opened.layers.layer(1).expect("layer 1");
    assert!(
        second.pixels.iter().all(|&p| p == 255),
        "layer 1 is all white"
    );
}

#[test]
fn asking_past_the_last_layer_is_an_error_not_a_panic() {
    let (_d, path) = Builder::default().write();
    let opened = registry::open(&path).expect("open");
    assert!(opened.layers.layer(2).is_err());
    assert!(opened.layers.layer(u32::MAX).is_err());
}

#[test]
fn an_enciphered_file_reads_the_same_as_a_plain_one() {
    // Files from Chitubox itself have their layer data obfuscated. This is the
    // common case, not the exotic one, so it has to give identical pixels.
    let plain = Builder::default();
    let (_d1, plain_path) = plain.write();
    let (_d2, secret_path) = Builder {
        encryption_key: 0xDEAD_BEEF,
        ..Default::default()
    }
    .write();

    // The files must genuinely differ on disk, or the test proves nothing.
    assert_ne!(
        std::fs::read(&plain_path).unwrap(),
        std::fs::read(&secret_path).unwrap(),
        "the enciphered file must not be byte-identical to the plain one"
    );

    let a = registry::open(&plain_path).expect("open plain");
    let b = registry::open(&secret_path).expect("open enciphered");
    assert_eq!(a.print.layer_count(), b.print.layer_count());
    for i in 0..a.print.layer_count() {
        assert_eq!(
            a.layers.layer(i).unwrap().pixels,
            b.layers.layer(i).unwrap().pixels,
            "layer {i} differs between the plain and enciphered files"
        );
    }
}

#[test]
fn each_layer_is_enciphered_with_its_own_index() {
    // Two identical layers must decipher correctly despite having different
    // bytes on disk, which is what proves the index is used as the IV.
    let mut builder = Builder {
        encryption_key: 0x0BAD_F00D,
        ..Default::default()
    };
    builder.layers = vec![vec![0x40u8; 32], vec![0x40u8; 32]];
    let (_d, path) = builder.write();
    let opened = registry::open(&path).expect("open");
    let first = opened.layers.layer(0).unwrap().pixels;
    let second = opened.layers.layer(1).unwrap().pixels;
    assert_eq!(first, second, "identical layers must decipher identically");
    assert!(first.iter().all(|&p| p == 0x40 | 0x01 || p == 0x40));
}

#[test]
fn an_unsupported_version_is_named_in_the_error() {
    let (_d, path) = Builder {
        version: 9,
        ..Default::default()
    }
    .write();
    let err = registry::open(&path).expect_err("must refuse");
    assert!(err.to_string().contains('9'), "{err}");
}

#[test]
fn a_truncated_file_does_not_panic() {
    let full = Builder::default().build();
    for cut in [0usize, 1, 4, 8, 0x40, HEADER - 1, HEADER, HEADER + 8] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cut.ctb");
        std::fs::write(&path, &full[..cut.min(full.len())]).unwrap();
        // Any outcome is fine except a panic; most will be errors.
        let _ = registry::open(&path);
        let _ = registry::identify(&path);
    }
}

#[test]
fn a_layer_pointing_outside_the_file_is_rejected() {
    let mut bytes = Builder::default().build();
    let table_at = HEADER + PARAMS;
    // Send layer 0's data off the end of the file.
    bytes[table_at + 0x0C..table_at + 0x10].copy_from_slice(&0xFFFF_0000u32.to_le_bytes());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.ctb");
    std::fs::write(&path, &bytes).unwrap();
    let err = registry::open(&path).expect_err("must refuse");
    assert!(err.to_string().contains("outside the file"), "{err}");
}

#[test]
fn an_absurd_layer_count_is_refused_without_allocating_for_it() {
    let mut bytes = Builder::default().build();
    bytes[0x44..0x48].copy_from_slice(&u32::MAX.to_le_bytes());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.ctb");
    std::fs::write(&path, &bytes).unwrap();
    assert!(registry::open(&path).is_err());
}

#[test]
fn a_zero_resolution_is_refused() {
    let mut bytes = Builder::default().build();
    bytes[0x34..0x38].copy_from_slice(&0u32.to_le_bytes());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flat.ctb");
    std::fs::write(&path, &bytes).unwrap();
    assert!(registry::open(&path).is_err());
}

#[test]
fn ctb_is_offered_for_reading_only() {
    // The writer below works and is tested, but UVtools will not read what it
    // produces, so nothing offers it. See the note at the top of ctb.rs.
    let info = registry::by_id("ctb").expect("registered").info();
    assert!(info.capabilities.reads);
    assert!(!info.capabilities.writes);
}

#[test]
fn a_written_ctb_reads_back_with_the_same_metadata_and_pixels() {
    let mut builder = Builder {
        width: 32,
        height: 16,
        ..Default::default()
    };
    builder.layers = vec![
        vec![0u8; 32 * 16],
        vec![254u8; 32 * 16],
        (0..32 * 16).map(|i| ((i * 3) % 256) as u8).collect(),
    ];
    let (_d, src) = builder.write();

    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("out.ctb");
    let before = registry::open(&src).expect("open source");
    registry::by_id("ctb")
        .expect("ctb")
        .write(&dst, &before.print, before.layers.as_ref())
        .expect("write");
    let after = registry::open(&dst).expect("open written");

    assert_eq!(after.print.geometry.resolution_x, 32);
    assert_eq!(after.print.geometry.resolution_y, 16);
    assert_eq!(after.print.exposure.layer_height_mm, 0.05);
    assert_eq!(after.print.exposure.exposure_s, 2.5);
    assert_eq!(after.print.exposure.bottom_exposure_s, Some(30.0));
    assert_eq!(after.print.exposure.bottom_layers, Some(1));
    assert_eq!(after.print.lift.lift_height_mm, Some(6.0));
    assert_eq!(after.print.print_time_s, Some(4321));
    assert_eq!(after.print.layer_count(), before.print.layer_count());

    for i in 0..before.print.layer_count() {
        assert_eq!(
            after.layers.layer(i).unwrap().pixels,
            before.layers.layer(i).unwrap().pixels,
            "layer {i} changed through a write and read"
        );
    }
}

#[test]
fn writing_no_layers_is_refused() {
    // Nothing should call this, but a zero-layer file would be a file the
    // printer cannot do anything with.
    use cheapazsla_core::layers::InMemoryLayers;

    // Reuse a real file's metadata rather than inventing one; only the layers
    // are the point here.
    let (_d, src) = Builder::default().write();
    let print = registry::open(&src).expect("open").print;
    let handler = registry::by_id("ctb").expect("registered");
    let dir = tempfile::tempdir().unwrap();
    let empty = InMemoryLayers::new(Vec::new(), 16, 16);
    let err = handler
        .write(&dir.path().join("empty.ctb"), &print, &empty)
        .expect_err("must refuse");
    assert!(err.to_string().contains("no layers"), "{err}");
}

#[test]
fn a_ctb_converts_to_goo_pixel_for_pixel() {
    // Reading is only half the job: the point of reading CTB is converting it.
    // This runs a real conversion and compares what comes out the far side
    // with what went in, through two independent codecs.
    use cheapazsla_core::convert;

    let mut builder = Builder {
        width: 64,
        height: 32,
        ..Default::default()
    };
    // A pattern with runs, isolated pixels and a grey ramp, so a wrong
    // encoding of any of the three shows up.
    builder.layers = vec![
        vec![0u8; 64 * 32],
        vec![254u8; 64 * 32],
        (0..64 * 32).map(|i| ((i * 7) % 256) as u8).collect(),
    ];
    let (_d, path) = builder.write();

    let dir = tempfile::tempdir().unwrap();
    let dst = convert::destination_for(&path, "goo", Some(dir.path())).expect("destination");
    let plan = convert::plan(&path, "goo", &dst).expect("plan");
    convert::run(&plan).expect("convert");

    let source = registry::open(&path).expect("open ctb");
    let written = registry::open(&dst).expect("open goo");
    assert_eq!(written.print.layer_count(), source.print.layer_count());
    assert_eq!(written.layers.dimensions(), source.layers.dimensions());

    for i in 0..source.print.layer_count() {
        let before = source.layers.layer(i).expect("ctb layer");
        let after = written.layers.layer(i).expect("goo layer");
        assert_eq!(
            before.pixels, after.pixels,
            "layer {i} changed between CTB and GOO"
        );
    }
}

/// The one that matters: a file Chitubox actually produced.
///
/// Everything above proves the reader agrees with itself. Only this proves the
/// layout is right. It skips rather than fails when no file is available, so
/// the suite stays green on a machine without one — but CTB is not "supported"
/// until this has run.
#[test]
fn a_real_chitubox_file_reads() {
    let Ok(path) = std::env::var("CHEAPAZSLA_REAL_CTB") else {
        eprintln!("skipped: set CHEAPAZSLA_REAL_CTB to a file from Chitubox");
        return;
    };
    let path = PathBuf::from(path);
    let d = registry::identify(&path).expect("identify").detection;
    assert_eq!(d.format_id, "ctb", "detected as {} instead", d.format_id);

    let opened = registry::open(&path).expect("open");
    let p = &opened.print;
    println!("  {} layers", p.layer_count());
    println!(
        "  {}x{}, {} mm layers, {} s exposure",
        p.geometry.resolution_x,
        p.geometry.resolution_y,
        p.exposure.layer_height_mm,
        p.exposure.exposure_s
    );

    assert!(p.layer_count() > 0, "a real file has layers");
    assert!(
        p.exposure.layer_height_mm > 0.0 && p.exposure.layer_height_mm < 1.0,
        "layer height {} mm is not plausible — the header layout is wrong",
        p.exposure.layer_height_mm
    );
    assert!(
        p.exposure.exposure_s > 0.0 && p.exposure.exposure_s < 1000.0,
        "exposure {} s is not plausible — the header layout is wrong",
        p.exposure.exposure_s
    );

    // Every layer must decode to exactly the panel size. This is the check
    // that catches a wrong run-length encoding: a wrong one runs out of data
    // or overruns the layer almost immediately.
    let (w, h) = opened.layers.dimensions();
    let expected = w as usize * h as usize;
    for i in 0..p.layer_count() {
        let img = opened
            .layers
            .layer(i)
            .unwrap_or_else(|e| panic!("layer {i}: {e}"));
        assert_eq!(img.pixels.len(), expected, "layer {i} is the wrong size");
    }
    println!("  all {} layers decoded to {w}x{h}", p.layer_count());
}
