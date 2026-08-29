//! CheapAzSLA command line interface.
//!
//! Argument parsing lands in a later phase. Every operation here calls into
//! cheapazsla-core, which is the same engine the desktop application uses.

fn main() {
    println!(
        "CheapAzSLA {} (engine {})",
        env!("CARGO_PKG_VERSION"),
        cheapazsla_core::VERSION
    );
    println!("The command line interface is not implemented yet.");
}
