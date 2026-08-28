//! CheapAzSLA desktop application.
//!
//! The window lands in a later phase. This crate depends on cheapazsla-core
//! and never the other way round.

fn main() {
    println!("CheapAzSLA {} (engine {})", env!("CARGO_PKG_VERSION"), cheapazsla_core::VERSION);
    println!("The desktop application is not implemented yet.");
}
