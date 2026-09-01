//! Print a fingerprint of a file's layers, for comparing one file against
//! another — including against a file decoded by somebody else's reader.
//!
//!     cargo run --example fingerprint -- FILE [--seven] [INDEX...]
//!
//! `--seven` folds each pixel to the seven bits CTB stores and back, which is
//! what a value looks like after a trip through that format. Without it the
//! pixels are compared as they are.

use cheapazsla_core::registry;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("file");
    let rest: Vec<String> = args.collect();
    let seven = rest.iter().any(|a| a == "--seven");
    let wanted: Vec<u32> = rest
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(|a| a.parse().unwrap())
        .collect();

    let opened = registry::open(std::path::Path::new(&path)).expect("open");
    let (w, h) = opened.layers.dimensions();
    let count = opened.layers.layer_count();
    println!("resolution {w}x{h}, {count} layers");

    let wanted = if wanted.is_empty() {
        vec![0, 1, count / 2, count - 1]
    } else {
        wanted
    };
    for i in wanted {
        let img = opened.layers.layer(i).expect("layer");
        let fold = |p: u8| if seven { (p >> 1) << 1 | (p >> 7) } else { p };
        let lit = img.pixels.iter().filter(|&&p| fold(p) > 0).count();
        let sum: u64 = img.pixels.iter().map(|&p| fold(p) as u64).sum();
        println!("layer {i}: {} px, {lit} lit, sum {sum}", img.pixels.len());
    }
}
