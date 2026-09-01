//! Write a CTB from any readable file, bypassing the capability flag.
//!
//! CTB writing is implemented but not offered, because UVtools will not read
//! what other open implementations produce either. This exists so that can be
//! re-tested against real files without turning it back on for everyone.
//!
//!     cargo run --example write_ctb -- INPUT OUTPUT.ctb

use cheapazsla_core::registry;

fn main() {
    let mut a = std::env::args().skip(1);
    let input = a.next().expect("input file");
    let output = a.next().expect("output file");

    let opened = registry::open(std::path::Path::new(&input)).expect("open");
    registry::by_id("ctb")
        .expect("ctb")
        .write(
            std::path::Path::new(&output),
            &opened.print,
            opened.layers.as_ref(),
        )
        .expect("write");
    println!(
        "wrote {output}: {} layers at {}x{}",
        opened.print.layer_count(),
        opened.print.geometry.resolution_x,
        opened.print.geometry.resolution_y
    );
}
