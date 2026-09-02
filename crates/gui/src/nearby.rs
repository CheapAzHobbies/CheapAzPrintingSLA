//! Readable files sitting in places the user already works in.
//!
//! The point is to skip the file dialog for the common case: a file was just
//! sliced into the usual folder, or a USB stick was plugged in with prints on
//! it. Both are one click away from being converted, and neither should
//! require navigating to a directory the program already knows about.
//!
//! Scanning is deliberately shallow and cheap. Only the extension is
//! consulted, never the file contents: identifying a format means opening and
//! reading, and doing that to every file on a mounted drive would stall the
//! interface for the sake of a list the user may not even look at. A wrong
//! guess here costs nothing, because opening the file re-detects it properly
//! and says so when the extension lied.

use cheapazsla_core::registry;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A candidate file, with enough context to show it without touching it again.
#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    pub size: u64,
    /// Display name of the format its extension claims, e.g. `SL1`.
    pub format: String,
    /// Where it was found, for the subtitle: a drive label or folder name.
    pub source: String,
    modified: Option<SystemTime>,
}

/// Directories are scanned one level deep only.
const MAX_PER_DIR: usize = 25;
/// Total across every directory, so a full drive cannot flood the list.
const MAX_TOTAL: usize = 40;

/// Files worth offering, newest first.
///
/// `exclude` is normally whatever is already queued: re-offering a file that
/// is on screen is noise.
pub fn scan(open_dir: Option<&Path>, exclude: &[PathBuf]) -> Vec<Found> {
    let mut roots: Vec<(PathBuf, String)> = Vec::new();

    if let Some(d) = open_dir {
        let label = d
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| d.display().to_string());
        roots.push((d.to_path_buf(), label));
    }
    for drive in crate::drives::mounted() {
        roots.push((drive.path, drive.name));
    }

    let mut out: Vec<Found> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for (dir, label) in roots {
        // The same folder can arrive twice: the open folder may itself sit on
        // a mounted drive.
        if seen.contains(&dir) {
            continue;
        }
        seen.push(dir.clone());
        out.extend(scan_one(&dir, &label, exclude));
        if out.len() >= MAX_TOTAL {
            break;
        }
    }

    out.sort_by_key(|f| std::cmp::Reverse(f.modified));
    out.truncate(MAX_TOTAL);
    out
}

fn scan_one(dir: &Path, label: &str, exclude: &[PathBuf]) -> Vec<Found> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if out.len() >= MAX_PER_DIR {
            break;
        }
        let path = entry.path();
        if exclude.contains(&path) {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let Some(handler) = registry::by_extension(ext) else {
            continue;
        };
        let info = handler.info();
        if !info.capabilities.reads {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        out.push(Found {
            path,
            size: meta.len(),
            format: info.name.to_string(),
            source: label.to_string(),
            modified: meta.modified().ok(),
        });
    }
    out
}
