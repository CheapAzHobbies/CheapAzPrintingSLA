//! Write a file's preview images out as PNGs, to see what a reader made of
//! them. A wrong offset or byte order gives noise, which a pixel count will
//! not tell you but a glance will.
//!
//!     cargo run --example thumbnails -- FILE OUT_DIR

use cheapazsla_core::registry;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("file");
    let out = args.next().unwrap_or_else(|| ".".into());

    let opened = registry::open(std::path::Path::new(&path)).expect("open");
    if opened.print.thumbnails.is_empty() {
        println!("no previews in this file");
        return;
    }
    for (i, t) in opened.print.thumbnails.iter().enumerate() {
        let name = format!("{out}/preview-{i}-{}x{}.png", t.width, t.height);
        let file = std::fs::File::create(&name).expect("create");
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), t.width, t.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .expect("header")
            .write_image_data(&t.rgb)
            .expect("data");
        println!("wrote {name}");
    }
}
