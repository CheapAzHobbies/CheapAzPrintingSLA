//! The format handler interface (§10) and the registry the GUI reads (§10).
//!
//! Adding a format means implementing [`FormatHandler`] and registering it.
//! Nothing in the GUI enumerates formats itself, so a new handler appears in
//! the interface without any UI change.

use crate::error::Result;
use crate::layers::LayerProvider;
use crate::model::PrintFile;
use std::path::Path;

/// How certain format detection is (§11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// Nothing matched.
    None,
    /// Only the file extension matched. Contents were not conclusive.
    Low,
    /// Structure looks right but a definitive marker was absent.
    Medium,
    /// A magic number or equivalent structural marker matched.
    High,
}

/// The outcome of asking a handler whether a file is its format.
#[derive(Debug, Clone)]
pub struct Detection {
    pub format_id: &'static str,
    pub confidence: Confidence,
    /// Shown to the user, so it explains the evidence in plain language.
    pub reason: String,
}

/// What a format can represent. Used to work out what a conversion will drop
/// before it runs (§14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub reads: bool,
    pub writes: bool,
    pub per_layer_exposure: bool,
    pub per_layer_lift: bool,
    pub thumbnails: bool,
    pub max_thumbnails: usize,
    pub print_time: bool,
    pub material_volume: bool,
    pub machine_name: bool,
}

impl Capabilities {
    /// A conservative default: readable and writable, nothing optional.
    pub const fn minimal() -> Self {
        Self {
            reads: true,
            writes: true,
            per_layer_exposure: false,
            per_layer_lift: false,
            thumbnails: false,
            max_thumbnails: 0,
            print_time: false,
            material_volume: false,
            machine_name: false,
        }
    }
}

/// Static description of a format, shown in the format info popover (§23).
#[derive(Debug, Clone)]
pub struct FormatInfo {
    /// Stable identifier used in settings and on the CLI, e.g. `"sl1"`.
    pub id: &'static str,
    /// Human name, e.g. `"PrusaSlicer SL1"`.
    pub name: &'static str,
    /// Primary extension without the dot.
    pub extension: &'static str,
    /// Additional extensions this handler also reads.
    pub aliases: &'static [&'static str],
    /// One or two sentences for the info popover.
    pub description: &'static str,
    /// Things a user should know, e.g. what it cannot store.
    pub limitations: &'static [&'static str],
    pub capabilities: Capabilities,
}

/// A file opened for reading: its metadata plus lazy access to its layers.
pub struct OpenedFile {
    pub print: PrintFile,
    pub layers: Box<dyn LayerProvider>,
}

// Hand-written because a boxed trait object cannot derive Debug. Summarises
// the layers rather than trying to show them.
impl std::fmt::Debug for OpenedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (w, h) = self.layers.dimensions();
        f.debug_struct("OpenedFile")
            .field("format", &self.print.source_format)
            .field("layers", &self.layers.layer_count())
            .field("dimensions", &format_args!("{w}x{h}"))
            .finish()
    }
}

/// Implemented once per supported format.
pub trait FormatHandler: Send + Sync {
    /// Static description, including what this format can and cannot store.
    fn info(&self) -> &'static FormatInfo;

    /// Decide whether `data` is this format. `path` is advisory only: the
    /// extension may be wrong or absent, so structure decides (§11).
    fn detect(&self, path: &Path, data: &[u8]) -> Detection;

    /// Check structure without fully decoding. Returns human-readable
    /// warnings for anything odd but tolerable.
    fn validate(&self, path: &Path) -> Result<Vec<String>>;

    /// Read metadata and return lazy layer access.
    fn open(&self, path: &Path) -> Result<OpenedFile>;

    /// Write `print` to `path`, pulling bitmaps from `layers` as needed.
    fn write(&self, path: &Path, print: &PrintFile, layers: &dyn LayerProvider) -> Result<()>;
}

/// How many bytes a handler is given to sniff with. Enough for any header we
/// care about, small enough to read from a huge file instantly.
pub const DETECT_BYTES: usize = 8192;
