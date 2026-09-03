//! Persisted preferences (§31).
//!
//! Kept in `core` rather than the interface so the command line honours the
//! same choices. Stored as plain `key = value` text in the user's config
//! directory: it is small, human-readable, and hand-editable if something
//! goes wrong.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Preferences that survive a restart.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Warn before a conversion drops information it cannot carry.
    /// Turning this off is a deliberate choice the user makes in the dialog.
    pub warn_on_information_loss: bool,
    /// Confirm before replacing an existing file.
    pub confirm_overwrite: bool,
    /// Animate the interface: the sidebar folding, pages crossfading.
    ///
    /// Off means those changes happen at once rather than not at all. Some
    /// people find motion distracting, and on a slow machine an animation
    /// that cannot keep up is worse than none.
    pub animations: bool,
    /// Where converted files went last time, reused as the default.
    pub last_output_dir: Option<PathBuf>,
    /// Output format chosen last time.
    pub last_output_format: Option<String>,
    /// Recently used output folders, most recent first.
    pub recent_output_dirs: Vec<PathBuf>,
    /// Folder the Open dialog starts in. When unset the dialog starts
    /// wherever the last file was opened from.
    pub default_open_dir: Option<PathBuf>,
    /// Folder the last opened file came from, used when no default is set.
    pub last_open_dir: Option<PathBuf>,
    /// Names of drives the user has pinned, such as "SATURN" or "PRINTS".
    ///
    /// Stored by name rather than by path on purpose: a removable drive gets a
    /// different mount point depending on the machine, the desktop and what
    /// else is plugged in, so a remembered path goes stale while the label
    /// stays put. The interface resolves a name to a live mount when it needs
    /// one, and shows the drive as unavailable when it is not connected.
    pub pinned_volumes: Vec<String>,
    /// Subfolder to use inside a pinned drive, e.g. "prints". Empty means the
    /// root of the drive.
    pub pinned_subfolder: String,
    /// Point the output at a removable drive as soon as it is plugged in.
    ///
    /// Off by default: it changes where the next conversion lands without
    /// being asked, and a surprise destination is worse than an extra click.
    pub auto_lock_new_drives: bool,
    /// Offer readable files found in the open folder and on mounted drives,
    /// so a file can be picked without opening a file dialog for it.
    pub show_nearby_files: bool,
    /// Extra folders Quick Access looks in, beyond the one the file chooser
    /// starts from.
    pub quick_access_folders: Vec<PathBuf>,
    /// Sources Quick Access has been told to skip, by key: a folder's path,
    /// or `drive:LABEL` for a mounted drive.
    ///
    /// Stored as an off-list rather than an on-list so a drive plugged in for
    /// the first time is scanned without being enabled by hand, which is the
    /// behaviour that makes the feature worth having.
    pub quick_access_off: Vec<String>,
    /// Drives Quick Access has been told it may look in, by `drive:LABEL`.
    ///
    /// An on-list, unlike `quick_access_off`, and deliberately so: a drive
    /// that has just been plugged in is listed but not read until it is
    /// switched on by hand. Folders are few and chosen; drives are however
    /// many happen to be attached, and reading all of them by default fills
    /// the list with things nobody asked to see.
    pub quick_access_drives_on: Vec<String>,
    /// How many files the Quick Access list shows before it starts scrolling
    /// inside itself, rather than growing the page.
    pub quick_access_visible: u32,
    /// Sources taken off the "Look in" list altogether, by the same key.
    ///
    /// Distinct from `quick_access_off`, which is a source the user still
    /// wants listed and may switch back on. This one is for a place that
    /// should stop being offered at all - the folder is untouched on disk,
    /// it is simply not one of the choices any more. Adding the folder back
    /// through the picker clears it.
    pub quick_access_hidden: Vec<String>,
    /// Drives the user has marked as never ejectable, by name.
    ///
    /// This is a second lock, not the only one. Filesystems the system needs
    /// are refused whether or not they appear here, because the cost of
    /// getting it wrong is unmounting the machine out from under itself.
    pub never_eject: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Warning by default is the safe direction: a user who has not
            // expressed a preference should be told before data is dropped.
            warn_on_information_loss: true,
            confirm_overwrite: true,
            animations: true,
            last_output_dir: None,
            last_output_format: None,
            recent_output_dirs: Vec::new(),
            default_open_dir: None,
            last_open_dir: None,
            pinned_volumes: Vec::new(),
            pinned_subfolder: String::new(),
            auto_lock_new_drives: false,
            show_nearby_files: true,
            quick_access_folders: Vec::new(),
            quick_access_off: Vec::new(),
            quick_access_hidden: Vec::new(),
            quick_access_drives_on: Vec::new(),
            quick_access_visible: 5,
            never_eject: Vec::new(),
        }
    }
}

/// How many recent folders to remember. Short on purpose (§ recent locations).
const MAX_RECENT: usize = 5;

impl Settings {
    /// Path of the settings file, honouring `XDG_CONFIG_HOME`.
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("cheapazsla").join("settings.conf"))
    }

    /// Load, falling back to defaults for anything missing or unreadable.
    ///
    /// A corrupt settings file must never stop the program starting, so every
    /// failure here degrades to the default rather than propagating.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let map = parse(&text);
        let mut s = Self::default();
        if let Some(v) = map.get("warn_on_information_loss") {
            s.warn_on_information_loss = v == "true";
        }
        if let Some(v) = map.get("confirm_overwrite") {
            s.confirm_overwrite = v == "true";
        }
        if let Some(v) = map.get("animations") {
            s.animations = v == "true";
        }
        s.last_output_dir = map
            .get("last_output_dir")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        s.last_output_format = map
            .get("last_output_format")
            .filter(|v| !v.is_empty())
            .cloned();
        if let Some(v) = map.get("pinned_volumes") {
            s.pinned_volumes = v
                .split('\x1f')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect();
        }
        if let Some(v) = map.get("pinned_subfolder") {
            s.pinned_subfolder = v.clone();
        }
        if let Some(v) = map.get("auto_lock_new_drives") {
            s.auto_lock_new_drives = v == "true";
        }
        if let Some(v) = map.get("show_nearby_files") {
            s.show_nearby_files = v == "true";
        }
        if let Some(v) = map.get("quick_access_folders") {
            s.quick_access_folders = v
                .split('\x1f')
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect();
        }
        if let Some(v) = map.get("quick_access_off") {
            s.quick_access_off = v
                .split('\x1f')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect();
        }
        if let Some(v) = map.get("quick_access_drives_on") {
            s.quick_access_drives_on = v
                .split('\x1f')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect();
        }
        if let Some(v) = map.get("quick_access_visible") {
            // Clamped rather than trusted: a hand-edited zero would make the
            // list a scrollbar with nothing beside it.
            if let Ok(n) = v.parse::<u32>() {
                s.quick_access_visible = n.clamp(1, 40);
            }
        }
        if let Some(v) = map.get("quick_access_hidden") {
            s.quick_access_hidden = v
                .split('\x1f')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect();
        }
        if let Some(v) = map.get("never_eject") {
            s.never_eject = v
                .split('\x1f')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect();
        }
        s.default_open_dir = map
            .get("default_open_dir")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        s.last_open_dir = map
            .get("last_open_dir")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        if let Some(v) = map.get("recent_output_dirs") {
            s.recent_output_dirs = v
                .split('\x1f')
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .take(MAX_RECENT)
                .collect();
        }
        s
    }

    /// Write to disk, creating the directory if needed.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let recent = self
            .recent_output_dirs
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\x1f");
        let body = format!(
            "# CheapAzSLA settings\n\
             warn_on_information_loss = {}\n\
             confirm_overwrite = {}\n\
             animations = {}\n\
             last_output_dir = {}\n\
             last_output_format = {}\n\
             recent_output_dirs = {}\n\
             default_open_dir = {}\n\
             last_open_dir = {}\n\
             pinned_volumes = {}\n\
             pinned_subfolder = {}\n\
             auto_lock_new_drives = {}\n\
             show_nearby_files = {}\n\
             never_eject = {}\n\
             quick_access_folders = {}\n\
             quick_access_off = {}\n\
             quick_access_hidden = {}\n\
             quick_access_drives_on = {}\n\
             quick_access_visible = {}\n",
            self.warn_on_information_loss,
            self.confirm_overwrite,
            self.animations,
            self.last_output_dir
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            self.last_output_format.clone().unwrap_or_default(),
            recent,
            self.default_open_dir
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            self.last_open_dir
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            self.pinned_volumes.join("\x1f"),
            self.pinned_subfolder,
            self.auto_lock_new_drives,
            self.show_nearby_files,
            self.never_eject.join("\x1f"),
            self.quick_access_folders
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("\x1f"),
            self.quick_access_off.join("\x1f"),
            self.quick_access_hidden.join("\x1f"),
            self.quick_access_drives_on.join("\x1f"),
            self.quick_access_visible,
        );
        std::fs::write(path, body)
    }

    /// Record a folder as most recently used.
    pub fn remember_output_dir(&mut self, dir: &Path) {
        self.recent_output_dirs.retain(|p| p != dir);
        self.recent_output_dirs.insert(0, dir.to_path_buf());
        self.recent_output_dirs.truncate(MAX_RECENT);
        self.last_output_dir = Some(dir.to_path_buf());
    }

    /// Where the Open dialog should start: the chosen default if it still
    /// exists, otherwise wherever the last file came from.
    pub fn open_start_dir(&self) -> Option<PathBuf> {
        self.default_open_dir
            .clone()
            .filter(|d| d.is_dir())
            .or_else(|| self.last_open_dir.clone().filter(|d| d.is_dir()))
    }

    /// Pin a drive by name. Pinning one already pinned is a no-op.
    pub fn pin_volume(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() || self.pinned_volumes.iter().any(|v| v == name) {
            return;
        }
        self.pinned_volumes.push(name.to_string());
    }

    pub fn unpin_volume(&mut self, name: &str) {
        self.pinned_volumes.retain(|v| v != name);
    }

    pub fn is_pinned(&self, name: &str) -> bool {
        self.pinned_volumes.iter().any(|v| v == name)
    }

    /// Recent folders that still exist. A drive that has been unplugged is
    /// dropped from the list rather than offered and then failing (§ drive
    /// removal).
    pub fn available_recent_dirs(&self) -> Vec<PathBuf> {
        self.recent_output_dirs
            .iter()
            .filter(|p| p.is_dir())
            .cloned()
            .collect()
    }
}

fn parse(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_warn_before_dropping_information() {
        let s = Settings::default();
        assert!(
            s.warn_on_information_loss,
            "a user who has not chosen should be warned"
        );
        assert!(s.confirm_overwrite);
    }

    #[test]
    fn recent_dirs_are_most_recent_first_without_duplicates() {
        let mut s = Settings::default();
        s.remember_output_dir(Path::new("/a"));
        s.remember_output_dir(Path::new("/b"));
        s.remember_output_dir(Path::new("/a"));
        assert_eq!(
            s.recent_output_dirs,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
        assert_eq!(s.last_output_dir.as_deref(), Some(Path::new("/a")));
    }

    #[test]
    fn recent_dirs_are_capped() {
        let mut s = Settings::default();
        for i in 0..20 {
            s.remember_output_dir(Path::new(&format!("/d{i}")));
        }
        assert_eq!(s.recent_output_dirs.len(), MAX_RECENT);
    }

    #[test]
    fn the_open_dialog_prefers_the_chosen_default_then_the_last_used() {
        let mut s = Settings::default();
        assert_eq!(s.open_start_dir(), None, "nothing to go on yet");
        // Non-existent paths are ignored rather than handed to the dialog.
        s.default_open_dir = Some(PathBuf::from("/definitely/not/here"));
        s.last_open_dir = Some(PathBuf::from("/also/not/here"));
        assert_eq!(s.open_start_dir(), None);
        s.last_open_dir = Some(PathBuf::from("/tmp"));
        assert_eq!(s.open_start_dir(), Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn pinning_a_drive_is_idempotent_and_removable() {
        let mut s = Settings::default();
        s.pin_volume("SATURN");
        s.pin_volume("SATURN");
        s.pin_volume("  ");
        assert_eq!(s.pinned_volumes, vec!["SATURN".to_string()]);
        assert!(s.is_pinned("SATURN"));
        s.unpin_volume("SATURN");
        assert!(!s.is_pinned("SATURN"));
    }

    #[test]
    fn a_corrupt_settings_file_does_not_stop_startup() {
        let map = parse("this is not settings\n\0\0\0\n= = =\n");
        assert!(map.get("warn_on_information_loss").is_none());
        // load() would return defaults for all of these.
    }
}
