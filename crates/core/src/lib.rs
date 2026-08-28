//! CheapAzSLA conversion and inspection engine.
//!
//! This crate is the whole engine. It has no GUI dependencies and is used
//! unchanged by both the desktop application and the command line tool, which
//! is what keeps them from drifting apart (§32).
//!
//! The design is a hub and spoke (§9): every reader produces a
//! [`model::PrintFile`], and every writer consumes one. Adding a format costs
//! one handler rather than one converter per existing format.
//!
//! ```text
//! input -> detect -> parse -> PrintFile -> validate -> write -> output
//! ```

pub mod convert;
pub mod error;
pub mod format;
pub mod formats;
pub mod layers;
pub mod limits;
pub mod model;
pub mod registry;
pub mod settings;

pub use error::{Error, FormatError, Result};
pub use format::{Capabilities, Confidence, Detection, FormatHandler, FormatInfo, OpenedFile};
pub use layers::{CachedLayers, InMemoryLayers, LayerProvider};
pub use model::{Exposure, Geometry, LayerImage, LayerInfo, Lift, PrintFile, Thumbnail};

/// Version of the engine, from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
