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
    /// Last modified, for telling a fresh file from a stale one.
    pub modified: Option<SystemTime>,
}

/// A place Quick Access can look, and whether it is switched on.
#[derive(Debug, Clone)]
pub struct Source {
    pub path: PathBuf,
    /// Shown in the picker and as each file's origin.
    pub label: String,
    /// Stable identity for the off-list: a folder's path, or `drive:LABEL`.
    pub key: String,
    pub enabled: bool,
    /// Drives come and go and cannot be removed from the list by hand;
    /// folders were added deliberately and can be.
    pub removable_entry: bool,
}

/// Directories are scanned one level deep only.
const MAX_PER_DIR: usize = 25;
/// Total across every directory, so a full drive cannot flood the list.
const MAX_TOTAL: usize = 40;

/// Everywhere Quick Access could look, in the order it will look.
///
/// The folder the file chooser starts from comes first, then folders the user
/// added, then every mounted drive. Anything named in `off` is listed but not
/// scanned, so turning a source back on does not mean finding it again.
pub fn sources(open_dir: Option<&Path>, extra: &[PathBuf], off: &[String]) -> Vec<Source> {
    let name_of = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string())
    };
    let mut out: Vec<Source> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();

    let mut push = |path: PathBuf, label: String, key: String, removable_entry: bool| {
        // The same folder can arrive twice: the open folder may itself sit on
        // a mounted drive, or be listed again as an added folder.
        if seen.contains(&path) {
            return;
        }
        seen.push(path.clone());
        let enabled = !off.contains(&key);
        out.push(Source {
            path,
            label,
            key,
            enabled,
            removable_entry,
        });
    };

    if let Some(d) = open_dir {
        push(
            d.to_path_buf(),
            name_of(d),
            d.to_string_lossy().into_owned(),
            false,
        );
    }
    for d in extra {
        push(
            d.clone(),
            name_of(d),
            d.to_string_lossy().into_owned(),
            true,
        );
    }
    for drive in crate::drives::mounted() {
        let key = format!("drive:{}", drive.name);
        push(drive.path, drive.name, key, false);
    }
    out
}

/// Files worth offering, newest first, from the sources that are switched on.
///
/// `exclude` is normally whatever is already queued: re-offering a file that
/// is on screen is noise.
pub fn scan(sources: &[Source], exclude: &[PathBuf]) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    for source in sources.iter().filter(|s| s.enabled) {
        out.extend(scan_one(&source.path, &source.label, exclude));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A scratch directory holding the given files, removed on drop.
    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str, files: &[&str]) -> Self {
            let base = std::env::temp_dir().join(format!(
                "cheapazsla-nearby-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            fs::create_dir_all(&base).expect("temp dir");
            for f in files {
                fs::write(base.join(f), b"x").expect("write");
            }
            Self(base)
        }
        fn source(&self, enabled: bool) -> Source {
            Source {
                path: self.0.clone(),
                label: "Scratch".into(),
                key: self.0.to_string_lossy().into_owned(),
                enabled,
                removable_entry: true,
            }
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(found: &[Found]) -> Vec<String> {
        found
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn only_readable_formats_are_offered() {
        // .sl1 and .goo are readable; the rest are not print files at all and
        // must not be suggested for conversion.
        let d = Dir::new("formats", &["a.sl1", "b.goo", "notes.txt", "photo.png"]);
        let mut got = names(&scan(&[d.source(true)], &[]));
        got.sort();
        assert_eq!(got, vec!["a.sl1", "b.goo"]);
    }

    #[test]
    fn a_source_that_is_switched_off_is_not_read() {
        let d = Dir::new("off", &["a.sl1"]);
        assert!(scan(&[d.source(false)], &[]).is_empty());
    }

    #[test]
    fn files_already_queued_are_not_offered_again() {
        let d = Dir::new("exclude", &["a.sl1", "b.sl1"]);
        let queued = vec![d.0.join("a.sl1")];
        assert_eq!(names(&scan(&[d.source(true)], &queued)), vec!["b.sl1"]);
    }

    #[test]
    fn a_directory_that_does_not_exist_is_not_an_error() {
        // An unplugged drive is exactly this case, and it must not panic or
        // take the rest of the sources down with it.
        let missing = Source {
            path: PathBuf::from("/nonexistent-cheapazsla-test-path"),
            label: "Gone".into(),
            key: "gone".into(),
            enabled: true,
            removable_entry: false,
        };
        let d = Dir::new("missing", &["a.sl1"]);
        assert_eq!(names(&scan(&[missing, d.source(true)], &[])), vec!["a.sl1"]);
    }

    #[test]
    fn the_newest_file_is_offered_first() {
        let d = Dir::new("order", &[]);
        fs::write(d.0.join("old.sl1"), b"x").unwrap();
        // Filesystem timestamps can share a tick, so the ordering is forced
        // rather than assumed from write order.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let f = fs::File::options()
            .write(true)
            .open(d.0.join("old.sl1"))
            .unwrap();
        f.set_modified(old).unwrap();
        fs::write(d.0.join("new.sl1"), b"x").unwrap();
        assert_eq!(
            names(&scan(&[d.source(true)], &[])),
            vec!["new.sl1", "old.sl1"]
        );
    }

    #[test]
    fn each_file_says_where_it_came_from() {
        let d = Dir::new("origin", &["a.sl1"]);
        let found = scan(&[d.source(true)], &[]);
        assert_eq!(found[0].source, "Scratch");
        // The handler's display name, not the extension: this is what the row
        // shows, and "PrusaSlicer SL1" is more use than "SL1" when the point
        // is recognising where a file came from.
        assert_eq!(found[0].format, "PrusaSlicer SL1");
    }
}
