//! The conversion pipeline (§9, §14, §25).
//!
//! ```text
//! open -> validate -> compare capabilities -> write -> confirm
//! ```
//!
//! Conversion never silently discards information. Anything the destination
//! format cannot express is reported first so the caller can ask the user
//! whether to continue.

use crate::error::{Error, FormatError, Result};
use crate::format::FormatInfo;
use crate::model::PrintFile;
use crate::registry;
use std::path::{Path, PathBuf};

/// Something the destination format cannot carry across.
#[derive(Debug, Clone, PartialEq)]
pub struct Loss {
    /// What is being lost, in words a user will understand.
    pub what: String,
    /// Why, so the message is not just an assertion.
    pub because: String,
}

/// What a conversion would do, worked out before anything is written.
#[derive(Debug, Clone)]
pub struct Plan {
    pub from: &'static FormatInfo,
    pub to: &'static FormatInfo,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub layer_count: u32,
    /// Empty when everything survives.
    pub losses: Vec<Loss>,
    /// Warnings raised while validating the source.
    pub source_warnings: Vec<String>,
}

impl Plan {
    pub fn is_lossless(&self) -> bool {
        self.losses.is_empty()
    }
}

/// Work out what converting `source` to `to_format` at `destination` involves.
///
/// Does not write anything. Call [`run`] to carry the plan out.
pub fn plan(source: &Path, to_format: &str, destination: &Path) -> Result<Plan> {
    let id = registry::identify(source)?;
    let from = registry::by_id(id.detection.format_id).ok_or(Error::UnknownFormat)?;
    let to = registry::by_id(to_format).ok_or_else(|| Error::UnsupportedConversion {
        from: id.detection.format_id.to_string(),
        to: to_format.to_string(),
    })?;

    if !from.info().capabilities.reads {
        return Err(Error::UnsupportedConversion {
            from: from.info().name.to_string(),
            to: to.info().name.to_string(),
        });
    }
    if !to.info().capabilities.writes {
        return Err(Error::UnsupportedConversion {
            from: from.info().name.to_string(),
            to: to.info().name.to_string(),
        });
    }

    let source_warnings = from.validate(source).unwrap_or_default();
    let opened = from.open(source)?;

    Ok(Plan {
        from: from.info(),
        to: to.info(),
        source: source.to_path_buf(),
        destination: destination.to_path_buf(),
        layer_count: opened.print.layer_count(),
        losses: losses_between(&opened.print, from.info(), to.info()),
        source_warnings,
    })
}

/// Compare what the source file actually contains against what the
/// destination format can hold.
///
/// Only reports things the file genuinely has. Saying "per-layer exposure will
/// be lost" about a file that never had any would be noise.
pub fn losses_between(print: &PrintFile, from: &FormatInfo, to: &FormatInfo) -> Vec<Loss> {
    let mut out = Vec::new();
    let cap = to.capabilities;

    let has_per_layer_exposure = print.layers.iter().any(|l| l.exposure_s.is_some());
    if has_per_layer_exposure && !cap.per_layer_exposure {
        out.push(Loss {
            what: "Per-layer exposure times".into(),
            because: format!("{} stores one exposure for the whole print", to.name),
        });
    }

    let has_per_layer_lift = print
        .layers
        .iter()
        .any(|l| l.lift_height_mm.is_some() || l.lift_speed_mm_min.is_some());
    if has_per_layer_lift && !cap.per_layer_lift {
        out.push(Loss {
            what: "Per-layer lift settings".into(),
            because: format!("{} stores lift settings once for the print", to.name),
        });
    }

    if !print.thumbnails.is_empty() {
        if !cap.thumbnails {
            out.push(Loss {
                what: format!("{} preview image(s)", print.thumbnails.len()),
                because: format!("{} does not store previews", to.name),
            });
        } else if print.thumbnails.len() > cap.max_thumbnails {
            out.push(Loss {
                what: format!(
                    "{} of {} preview images",
                    print.thumbnails.len() - cap.max_thumbnails,
                    print.thumbnails.len()
                ),
                because: format!("{} stores at most {}", to.name, cap.max_thumbnails),
            });
        }
    }

    if print.material_name.is_some() && !to.capabilities.machine_name {
        out.push(Loss {
            what: "Resin name".into(),
            because: format!("{} has no field for it", to.name),
        });
    }
    if print.print_time_s.is_some() && !cap.print_time {
        out.push(Loss {
            what: "Estimated print time".into(),
            because: format!("{} does not record it", to.name),
        });
    }
    if print.material_volume_ml.is_some() && !cap.material_volume {
        out.push(Loss {
            what: "Resin volume".into(),
            because: format!("{} does not record it", to.name),
        });
    }

    // Values the source format carried that have no home in the model, and so
    // cannot be handed to any writer.
    if !print.extra.is_empty() {
        out.push(Loss {
            what: format!("{} setting(s) specific to {}", print.extra.len(), from.name),
            because: format!("they have no equivalent in {}", to.name),
        });
    }

    out
}

/// Carry out a plan. Overwrites `destination`, so the caller decides about
/// existing files first.
pub fn run(plan: &Plan) -> Result<()> {
    run_with_progress(plan, |_, _| {})
}

/// Carry out a plan, reporting `(layers_done, layers_total)` as it goes.
///
/// Progress comes from counting layer fetches rather than from each writer
/// reporting for itself, so a new format gets progress reporting for free.
pub fn run_with_progress(plan: &Plan, on_progress: impl Fn(u32, u32) + Send + Sync) -> Result<()> {
    let from = registry::by_id(plan.from.id).ok_or(Error::UnknownFormat)?;
    let to = registry::by_id(plan.to.id).ok_or(Error::UnknownFormat)?;

    let opened = from.open(&plan.source)?;
    let counted = crate::layers::ProgressLayers::new(opened.layers.as_ref(), on_progress);

    // Write to a temporary name beside the destination and rename only once
    // the whole file is there.
    //
    // A print file that stops halfway still opens, still reports a layer count
    // from its header, and still looks like a finished job in a file manager.
    // Killed mid-conversion, the old code left exactly that behind, and a
    // half-written file on a USB stick is something a person can carry to a
    // printer. Renaming within the same directory is atomic, so the
    // destination either does not exist or is complete.
    let temp = temp_path(&plan.destination);
    let result = to.write(&temp, &opened.print, &counted);

    if let Err(e) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    match std::fs::metadata(&temp) {
        Ok(m) if m.len() > 0 => {}
        Ok(_) => {
            let _ = std::fs::remove_file(&temp);
            return Err(FormatError::Other("the written file is empty".into()).into());
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            return Err(Error::Io {
                path: temp,
                source: e,
            });
        }
    }
    std::fs::rename(&temp, &plan.destination).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        Error::Io {
            path: plan.destination.clone(),
            source: e,
        }
    })
}

/// A sibling path to write to before the rename.
///
/// Deliberately in the same directory: rename is only atomic within a
/// filesystem, and a temporary directory may well be on a different one.
/// The leading dot keeps it out of the way if anything ever does interrupt
/// hard enough to leave it behind.
fn temp_path(destination: &Path) -> PathBuf {
    let dir = destination.parent().unwrap_or(Path::new("."));
    let name = destination
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!(".{name}.{stamp}.part"))
}

/// Swap a path's extension for the one belonging to `format_id`.
pub fn destination_for(source: &Path, format_id: &str, into: Option<&Path>) -> Option<PathBuf> {
    let info = registry::by_id(format_id)?.info();
    let stem = source.file_stem()?;
    let dir = into
        .map(Path::to_path_buf)
        .or_else(|| source.parent().map(Path::to_path_buf))?;
    let mut name = std::ffi::OsString::from(stem);
    name.push(".");
    name.push(info.extension);
    Some(dir.join(name))
}

/// A path that does not exist yet, by adding ` (1)`, ` (2)` and so on.
/// Backs the "Keep Both" choice when a file is already there.
pub fn unique_path(desired: &Path) -> PathBuf {
    if !desired.exists() {
        return desired.to_path_buf();
    }
    let dir = desired.parent().unwrap_or(Path::new("."));
    let stem = desired
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = desired
        .extension()
        .map(|s| s.to_string_lossy().into_owned());
    for n in 1..10_000 {
        let name = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    desired.to_path_buf()
}
