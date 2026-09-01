//! The handler registry and format detection (§4, §11).
//!
//! This is the single place that knows which formats exist. The GUI and the
//! CLI both ask it, so adding a handler here makes the format appear in both
//! without touching either (§10).

use crate::error::{Error, Result};
use crate::format::{Confidence, Detection, FormatHandler, FormatInfo, OpenedFile, DETECT_BYTES};
use crate::formats::ctb::CtbHandler;
use crate::formats::goo::GooHandler;
use crate::formats::phz::PhzHandler;
use crate::formats::sl1::Sl1Handler;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Every handler compiled into this build.
pub fn handlers() -> Vec<&'static dyn FormatHandler> {
    // Registering a new format is one line here (§45).
    static SL1: Sl1Handler = Sl1Handler;
    static GOO: GooHandler = GooHandler;
    static CTB: CtbHandler = CtbHandler;
    static PHZ: PhzHandler = PhzHandler;
    vec![&SL1, &GOO, &CTB, &PHZ]
}

/// Handlers that can read, for the input format list.
pub fn readable() -> Vec<&'static FormatInfo> {
    handlers()
        .into_iter()
        .map(|h| h.info())
        .filter(|i| i.capabilities.reads)
        .collect()
}

/// Handlers that can write, for the output format list.
pub fn writable() -> Vec<&'static FormatInfo> {
    handlers()
        .into_iter()
        .map(|h| h.info())
        .filter(|i| i.capabilities.writes)
        .collect()
}

/// Look up a handler by its stable id, e.g. `"sl1"`.
pub fn by_id(id: &str) -> Option<&'static dyn FormatHandler> {
    handlers()
        .into_iter()
        .find(|h| h.info().id.eq_ignore_ascii_case(id))
}

/// Look up a handler by extension, primary or alias.
pub fn by_extension(ext: &str) -> Option<&'static dyn FormatHandler> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    handlers().into_iter().find(|h| {
        let i = h.info();
        i.extension == ext || i.aliases.iter().any(|a| *a == ext)
    })
}

/// The outcome of identifying a file.
#[derive(Debug, Clone)]
pub struct Identified {
    pub detection: Detection,
    /// True when the extension disagrees with what the contents say (§11).
    pub extension_mismatch: bool,
    /// Every candidate that scored above [`Confidence::None`], best first.
    pub alternatives: Vec<Detection>,
}

/// Identify a file by asking every handler and taking the most confident.
///
/// The extension is a hint only. When contents and extension disagree, the
/// contents win and `extension_mismatch` is set so the caller can warn.
pub fn identify(path: &Path) -> Result<Identified> {
    let mut head = vec![0u8; DETECT_BYTES];
    let read = {
        let mut f = File::open(path).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        f.read(&mut head).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?
    };
    head.truncate(read);

    let mut scored: Vec<Detection> = handlers()
        .into_iter()
        .map(|h| h.detect(path, &head))
        .filter(|d| d.confidence > Confidence::None)
        .collect();
    // Most confident first.
    scored.sort_by_key(|d| std::cmp::Reverse(d.confidence));

    let best = scored.first().cloned().ok_or(Error::UnknownFormat)?;

    let ext_says = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(by_extension)
        .map(|h| h.info().id);
    let extension_mismatch = matches!(ext_says, Some(id) if id != best.format_id);

    Ok(Identified {
        alternatives: scored,
        extension_mismatch,
        detection: best,
    })
}

/// Identify then open in one step.
pub fn open(path: &Path) -> Result<OpenedFile> {
    let id = identify(path)?;
    let handler = by_id(id.detection.format_id).ok_or(Error::UnknownFormat)?;
    handler.open(path)
}

/// Open a file as a named format, whatever detection thinks of it (§21).
///
/// Detection reads the contents rather than the name, which is right almost
/// always and wrong occasionally: a format can be a container another format
/// also uses, a file can be truncated before the part that identifies it, and
/// two formats can share a marker. When someone knows better than the
/// detector, they need a way to say so.
pub fn open_as(path: &Path, format_id: &str) -> Result<OpenedFile> {
    let handler = by_id(format_id).ok_or(Error::UnknownFormat)?;
    if !handler.info().capabilities.reads {
        return Err(Error::UnsupportedConversion {
            from: handler.info().name.into(),
            to: "anything".into(),
        });
    }
    handler.open(path)
}
