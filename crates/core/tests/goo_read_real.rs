//! Reading real GOO files (§41).

use cheapazsla_core::layers::LayerProvider;
use cheapazsla_core::registry;
use std::path::PathBuf;

fn real(key: &str) -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var(key).ok()?);
    p.exists().then_some(p)
}

#[test]
fn reads_a_real_goo_written_by_other_software() {
    let Some(path) = real("CHEAPAZSLA_REAL_GOO") else {
        eprintln!("skipped: set CHEAPAZSLA_REAL_GOO");
        return;
    };
    let id = registry::identify(&path).expect("identify");
    assert_eq!(id.detection.format_id, "goo");

    let opened = registry::open(&path).expect("open");
    let p = &opened.print;
    println!(
        "  {}x{}  {} layers @ {}mm  exposure {}s (bottom {:?})",
        p.geometry.resolution_x,
        p.geometry.resolution_y,
        p.layer_count(),
        p.exposure.layer_height_mm,
        p.exposure.exposure_s,
        p.exposure.bottom_exposure_s
    );
    assert!(p.layer_count() > 0);
    assert!(p.exposure.layer_height_mm > 0.0);

    let mid = opened
        .layers
        .layer(p.layer_count() / 2)
        .expect("middle layer");
    assert_eq!(mid.width, p.geometry.resolution_x);
    assert_eq!(mid.height, p.geometry.resolution_y);
    assert!(!mid.is_blank(), "a middle layer should expose something");
    println!("  middle layer exposes {} pixels", mid.exposed_pixels(0));
}

#[test]
fn a_goo_and_the_sl1_it_came_from_agree_on_every_layer() {
    let (Some(goo), Some(sl1)) = (real("CHEAPAZSLA_REAL_GOO"), real("CHEAPAZSLA_REAL_SL1")) else {
        eprintln!("skipped");
        return;
    };
    let g = registry::open(&goo).expect("open goo");
    let s = registry::open(&sl1).expect("open sl1");
    assert_eq!(g.print.layer_count(), s.print.layer_count());
    for i in 0..s.print.layer_count() {
        let a = g.layers.layer(i).unwrap();
        let b = s.layers.layer(i).unwrap();
        assert_eq!(
            a.pixels, b.pixels,
            "layer {i} differs between the two formats"
        );
    }
    println!(
        "  all {} layers agree across formats",
        s.print.layer_count()
    );
}

#[test]
fn a_goo_we_wrote_reads_back_identically() {
    let Some(sl1) = real("CHEAPAZSLA_REAL_SL1") else {
        eprintln!("skipped");
        return;
    };
    use cheapazsla_core::convert;
    let dir = tempfile::tempdir().unwrap();
    let dst = convert::destination_for(&sl1, "goo", Some(dir.path())).unwrap();
    let plan = convert::plan(&sl1, "goo", &dst).unwrap();
    convert::run(&plan).unwrap();

    let original = registry::open(&sl1).unwrap();
    let back = registry::open(&dst).expect("read back our own goo");

    assert_eq!(back.print.layer_count(), original.print.layer_count());
    assert_eq!(
        back.print.geometry.resolution_x,
        original.print.geometry.resolution_x
    );
    assert_eq!(
        back.print.exposure.layer_height_mm,
        original.print.exposure.layer_height_mm
    );
    assert_eq!(
        back.print.exposure.exposure_s,
        original.print.exposure.exposure_s
    );
    assert_eq!(
        back.print.exposure.bottom_exposure_s,
        original.print.exposure.bottom_exposure_s
    );
    for i in 0..original.print.layer_count() {
        assert_eq!(
            back.layers.layer(i).unwrap().pixels,
            original.layers.layer(i).unwrap().pixels,
            "layer {i} changed"
        );
    }
    println!(
        "  SL1 -> GOO -> read back: {} layers identical",
        original.print.layer_count()
    );
}

#[test]
fn truncating_a_goo_is_an_error_not_a_panic() {
    let Some(path) = real("CHEAPAZSLA_REAL_GOO") else {
        eprintln!("skipped");
        return;
    };
    let bytes = std::fs::read(&path).unwrap();
    let dir = tempfile::tempdir().unwrap();
    for fraction in [2, 4, 8] {
        let cut = dir.path().join(format!("cut{fraction}.goo"));
        std::fs::write(&cut, &bytes[..bytes.len() / fraction]).unwrap();
        assert!(
            registry::open(&cut).is_err(),
            "1/{fraction} of a file must not open"
        );
    }
}
