//! Turning a failure into something the user can act on (§28).
//!
//! An error that says what went wrong but not what to do about it is half an
//! error message. Every suggestion here is a concrete next step, ordered with
//! the most likely cause first, and none of them is "contact support".
//!
//! Suggestions are derived from the error together with what can be observed
//! about the file, so a truncated download and a file that was never a print
//! file give different advice even when they fail the same way.

use crate::error::{Error, FormatError};
use std::path::Path;

/// One thing worth trying.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// The action, phrased as an instruction.
    pub action: String,
    /// Why it might help. Empty when the action speaks for itself.
    pub because: String,
}

impl Suggestion {
    fn new(action: &str, because: &str) -> Self {
        Self {
            action: action.to_string(),
            because: because.to_string(),
        }
    }
}

/// What can be seen about the file itself, used to sharpen the advice.
#[derive(Debug, Clone, Default)]
pub struct FileFacts {
    pub size: Option<u64>,
    /// True when the path is under a mount point that is not the home
    /// filesystem, which usually means removable media.
    pub on_removable: bool,
    /// Extension as written, lowercased and without the dot.
    pub extension: Option<String>,
    /// Format the contents actually look like, when anything matched.
    pub detected: Option<String>,
}

impl FileFacts {
    /// Gather what can be learned cheaply from the path.
    pub fn observe(path: &Path) -> Self {
        let size = std::fs::metadata(path).ok().map(|m| m.len());
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        let on_removable = looks_removable(path);
        Self {
            size,
            on_removable,
            extension,
            detected: None,
        }
    }

    /// A print file smaller than this almost certainly holds no layers.
    /// A single 1080p layer compresses to more than this on its own.
    fn suspiciously_small(&self) -> bool {
        self.size.map(|s| s < 512 * 1024).unwrap_or(false)
    }

    fn is_empty(&self) -> bool {
        self.size.map(|s| s == 0).unwrap_or(false)
    }
}

/// Whether a path is plausibly on removable media.
///
/// Compares the filesystem the file is on against the one the user's home is
/// on. A different filesystem is not proof of a USB stick, but it is the only
/// signal available without asking the desktop, and it is right often enough
/// to be worth a suggestion.
///
/// The earlier version simply asked whether the path was outside the home
/// directory, which called /tmp removable and told people to copy a file to
/// the computer it was already on.
#[cfg(unix)]
fn looks_removable(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let (Ok(here), Ok(home)) = (std::fs::metadata(path), std::fs::metadata(&home)) else {
        return false;
    };
    if here.dev() == home.dev() {
        return false;
    }
    // A different filesystem that is also a well-known pseudo-filesystem is
    // not removable media.
    let text = path.to_string_lossy();
    !(text.starts_with("/tmp")
        || text.starts_with("/dev")
        || text.starts_with("/proc")
        || text.starts_with("/sys")
        || text.starts_with("/run/user"))
}

#[cfg(not(unix))]
fn looks_removable(_path: &Path) -> bool {
    false
}

/// Suggestions for a failure, most likely cause first.
pub fn for_error(error: &Error, facts: &FileFacts) -> Vec<Suggestion> {
    let mut out = match error {
        Error::Format(f) => for_format_error(f, facts),
        Error::UnknownFormat => unknown_format(facts),
        Error::UnsupportedConversion { from, to } => vec![
            Suggestion::new(
                &format!("Choose an output format other than {to}"),
                &format!("CheapAzSLA cannot write {to} yet, so there is nothing to convert {from} into."),
            ),
            Suggestion::new(
                "Check the format list in Settings",
                "It shows which formats can be read and which can be written.",
            ),
        ],
        Error::LayerOutOfRange { index, count } => vec![Suggestion::new(
            "Reopen the file",
            &format!("Layer {index} was requested but the file holds {count}. The file may have changed on disk while it was open."),
        )],
        Error::Io { source, .. } => io_suggestions(source, facts),
    };
    out.extend(context_suggestions(facts));
    out.dedup_by(|a, b| a.action == b.action);
    out
}

fn io_suggestions(source: &std::io::Error, facts: &FileFacts) -> Vec<Suggestion> {
    use std::io::ErrorKind::*;
    match source.kind() {
        NotFound => vec![
            Suggestion::new(
                "Check the file is still there",
                "It may have been moved, renamed or deleted since it was added.",
            ),
            Suggestion::new(
                "Reconnect the drive it was on",
                "A removable drive that has been unplugged takes its files with it.",
            ),
        ],
        PermissionDenied => vec![
            Suggestion::new(
                "Check you have permission to read the file",
                "Files copied from another machine sometimes arrive owned by a different user.",
            ),
            Suggestion::new(
                "Copy it somewhere you own, such as your home folder",
                "",
            ),
        ],
        _ if facts.on_removable => vec![Suggestion::new(
            "Copy the file to your computer and try again",
            "Reads directly from a removable drive fail more often, especially if it is being written to at the same time.",
        )],
        _ => vec![Suggestion::new(
            "Try opening the file again",
            "The read failed part-way, which is sometimes transient.",
        )],
    }
}

fn unknown_format(facts: &FileFacts) -> Vec<Suggestion> {
    let mut v = Vec::new();
    if facts.is_empty() {
        v.push(Suggestion::new(
            "Check the file is not empty",
            "It contains no data at all, so the copy or export it came from did not finish.",
        ));
        return v;
    }
    v.push(Suggestion::new(
        "Check this is a sliced file rather than a model",
        "CheapAzSLA reads what a slicer produces. An STL, OBJ, 3MF or STEP file is a model and has no layers in it.",
    ));
    if let Some(ext) = &facts.extension {
        if matches!(
            ext.as_str(),
            "stl" | "obj" | "3mf" | "step" | "stp" | "gcode"
        ) {
            v.insert(
                0,
                Suggestion::new(
                    &format!("Slice this .{ext} first"),
                    "Open it in PrusaSlicer, Lychee or Chitubox, slice it, and convert what comes out.",
                ),
            );
        }
        if ext == "gcode" {
            v.insert(
                0,
                Suggestion::new(
                    "This looks like a filament print",
                    "CheapAzSLA only handles resin and DLP files. G-code is for filament printers.",
                ),
            );
        }
    }
    v.push(Suggestion::new(
        "Check the file finished copying",
        "A partly copied file often has a valid name and nothing usable inside.",
    ));
    v
}

fn for_format_error(error: &FormatError, facts: &FileFacts) -> Vec<Suggestion> {
    match error {
        FormatError::MissingField(field) if field.contains("layer images") => {
            let mut v = vec![
                Suggestion::new(
                    "Export the file again from your slicer",
                    "The archive holds the settings and previews but no layer images, which is what an interrupted export looks like.",
                ),
            ];
            if facts.suspiciously_small() {
                v.insert(
                    0,
                    Suggestion::new(
                        "Check the file size",
                        "A real print file is megabytes. This one is far too small to hold any layers.",
                    ),
                );
            }
            v.push(Suggestion::new(
                "If it came from a conversion, convert it again",
                "A conversion that was interrupted can leave a file with everything except the layers.",
            ));
            v.push(Suggestion::new(
                "Check there was room on the drive it was written to",
                "A disk that filled up mid-write produces exactly this.",
            ));
            v
        }
        FormatError::MissingField(field) => vec![
            Suggestion::new(
                "Export the file again from your slicer",
                &format!("{field} is required and this file does not have it."),
            ),
            Suggestion::new(
                "Check the slicer finished writing before the file was copied",
                "",
            ),
        ],
        FormatError::BadMagic => vec![
            Suggestion::new(
                "Let CheapAzSLA identify the file",
                "The contents do not match the format its name claims. Opening it anyway will detect what it really is.",
            ),
            Suggestion::new(
                "Check the file was not renamed",
                "Changing an extension does not change what is inside.",
            ),
            Suggestion::new(
                "Check the file finished copying",
                "A truncated file often fails on its header.",
            ),
        ],
        FormatError::Truncated { .. } => vec![
            Suggestion::new(
                "Copy the file again",
                "It ends before it should, which is what an interrupted copy or download leaves behind.",
            ),
            Suggestion::new(
                "If it came from a removable drive, do not unplug it until the copy finishes",
                "A drive removed early leaves the last part of the file unwritten.",
            ),
            Suggestion::new(
                "Compare the size against the original",
                "A truncated file is smaller than the one it was copied from.",
            ),
        ],
        FormatError::OffsetOutOfBounds { .. } => vec![
            Suggestion::new(
                "Copy the file again",
                "It points at data past its own end, so part of it is missing or damaged.",
            ),
            Suggestion::new(
                "Check the drive it is stored on",
                "Repeated damage to files from one drive is worth investigating.",
            ),
        ],
        FormatError::AllocationTooLarge { declared, .. } => vec![
            Suggestion::new(
                "Treat this file as damaged",
                &format!("It claims {declared} bytes of layer data, which is not plausible. CheapAzSLA refused rather than trying to allocate it."),
            ),
            Suggestion::new("Export or copy it again", ""),
        ],
        FormatError::UnsupportedVersion { format, version } => vec![
            Suggestion::new(
                "Update CheapAzSLA",
                &format!("This is {format} version {version}, which this build does not know about."),
            ),
            Suggestion::new(
                "Re-export from your slicer at an older format version",
                "Most slicers let you choose which revision to write.",
            ),
        ],
        FormatError::InvalidValue { field, value, .. } => vec![
            Suggestion::new(
                "Export the file again from your slicer",
                &format!("{field} is {value}, which cannot be right."),
            ),
            Suggestion::new(
                "Check the print profile in your slicer",
                "A profile with a missing or zero value can produce a file like this.",
            ),
        ],
        FormatError::LayerDecode(_) => vec![
            Suggestion::new(
                "Copy or export the file again",
                "A layer image inside it is damaged.",
            ),
            Suggestion::new(
                "Try opening it in the slicer that made it",
                "If that fails too, the file is damaged rather than merely unsupported.",
            ),
        ],
        FormatError::Other(_) => vec![Suggestion::new(
            "Export or copy the file again",
            "The file could not be read as the format it appears to be.",
        )],
    }
}

/// Advice that depends on where the file is rather than what went wrong.
fn context_suggestions(facts: &FileFacts) -> Vec<Suggestion> {
    let mut v = Vec::new();
    if facts.on_removable {
        v.push(Suggestion::new(
            "Copy the file to your computer and open that copy",
            "Removable drives are the usual source of half-written files.",
        ));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn facts(size: u64) -> FileFacts {
        FileFacts {
            size: Some(size),
            ..Default::default()
        }
    }

    #[test]
    fn a_missing_layer_error_suggests_re_exporting() {
        let e: Error = FormatError::MissingField("layer images".into()).into();
        let s = for_error(&e, &facts(37_000));
        assert!(!s.is_empty());
        let text = s
            .iter()
            .map(|x| x.action.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            text.contains("size"),
            "a tiny file should be called out first: {text}"
        );
        assert!(text.contains("Export the file again"), "{text}");
    }

    #[test]
    fn a_large_file_missing_layers_does_not_blame_its_size() {
        let e: Error = FormatError::MissingField("layer images".into()).into();
        let s = for_error(&e, &facts(80 * 1024 * 1024));
        let text = s
            .iter()
            .map(|x| x.action.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(!text.contains("Check the file size"), "{text}");
    }

    #[test]
    fn a_model_file_is_told_to_slice_it_first() {
        let mut f = facts(1000);
        f.extension = Some("stl".into());
        let s = for_error(&Error::UnknownFormat, &f);
        assert!(s[0].action.contains("Slice"), "got {:?}", s[0]);
    }

    #[test]
    fn a_gcode_file_is_told_it_is_the_wrong_kind_of_printer() {
        let mut f = facts(1000);
        f.extension = Some("gcode".into());
        let s = for_error(&Error::UnknownFormat, &f);
        let text = s
            .iter()
            .map(|x| x.action.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(text.contains("filament"), "{text}");
    }

    #[test]
    fn an_empty_file_says_so_rather_than_guessing() {
        let s = for_error(&Error::UnknownFormat, &facts(0));
        assert_eq!(s.len(), 1);
        assert!(s[0].action.contains("not empty"));
    }

    #[test]
    fn a_file_on_a_removable_drive_gets_the_extra_hint() {
        let f = FileFacts {
            size: Some(1_000_000),
            on_removable: true,
            ..Default::default()
        };
        let s = for_error(
            &FormatError::Truncated {
                offset: 0,
                expected: 10,
                actual: 0,
            }
            .into(),
            &f,
        );
        let text = s
            .iter()
            .map(|x| x.action.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(text.contains("Copy the file to your computer"), "{text}");
    }

    #[test]
    fn every_error_produces_at_least_one_suggestion() {
        // An error with no advice is the thing this module exists to prevent.
        let cases: Vec<Error> = vec![
            Error::UnknownFormat,
            Error::UnsupportedConversion {
                from: "goo".into(),
                to: "ctb".into(),
            },
            Error::LayerOutOfRange { index: 9, count: 3 },
            FormatError::BadMagic.into(),
            FormatError::Truncated {
                offset: 1,
                expected: 2,
                actual: 0,
            }
            .into(),
            FormatError::MissingField("layerHeight".into()).into(),
            FormatError::MissingField("layer images".into()).into(),
            FormatError::OffsetOutOfBounds {
                offset: 1,
                length: 2,
                file_size: 1,
            }
            .into(),
            FormatError::AllocationTooLarge {
                declared: 1 << 40,
                limit: 1 << 29,
            }
            .into(),
            FormatError::UnsupportedVersion {
                format: "goo".into(),
                version: "99".into(),
            }
            .into(),
            FormatError::InvalidValue {
                field: "layerHeight".into(),
                value: "-1".into(),
                reason: "negative".into(),
            }
            .into(),
            FormatError::LayerDecode("bad png".into()).into(),
            FormatError::Other("something".into()).into(),
        ];
        for e in cases {
            let s = for_error(&e, &facts(1_000_000));
            assert!(!s.is_empty(), "no suggestion for {e:?}");
            for sug in &s {
                assert!(!sug.action.is_empty(), "empty action for {e:?}");
            }
        }
    }

    #[test]
    fn a_file_in_tmp_is_not_called_removable() {
        // /tmp is frequently its own filesystem, and telling someone to copy a
        // file to the computer it is already on is not advice.
        let f = FileFacts::observe(&PathBuf::from("/tmp"));
        assert!(!f.on_removable);
    }

    #[test]
    fn a_file_in_the_home_directory_is_not_removable() {
        if let Some(home) = std::env::var_os("HOME") {
            let f = FileFacts::observe(&PathBuf::from(home));
            assert!(!f.on_removable);
        }
    }

    #[test]
    fn observing_a_missing_file_does_not_panic() {
        let f = FileFacts::observe(&PathBuf::from("/definitely/not/here.sl1"));
        assert_eq!(f.size, None);
        assert_eq!(f.extension.as_deref(), Some("sl1"));
    }
}
