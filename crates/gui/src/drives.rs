//! Mounted drives, discovered through GIO.
//!
//! Nothing here knows about `/media`, `/mnt` or `/run/media`. GIO's volume
//! monitor reports whatever the desktop has actually mounted, which is the
//! only approach that works across distributions and desktops, and it picks up
//! network shares for free.
//!
//! Drives are remembered by name rather than by path. A USB stick gets a
//! different mount point depending on the machine and what else is plugged in,
//! so a saved path goes stale while the label stays put.

use gtk::gio;
use gtk::prelude::*;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Drive {
    /// Label as the desktop shows it, e.g. `SATURN`.
    pub name: String,
    /// Where it is mounted right now.
    pub path: PathBuf,
    /// True for things that can be unplugged.
    pub removable: bool,
}

/// Every mount the session can see, removable first.
pub fn mounted() -> Vec<Drive> {
    let monitor = gio::VolumeMonitor::get();
    let mut out: Vec<Drive> = monitor
        .mounts()
        .into_iter()
        .filter(|m| !m.is_shadowed())
        .filter_map(|m| {
            let path = m.root().path()?;
            let removable = m.can_eject()
                || m.can_unmount()
                || m.drive().map(|d| d.is_removable()).unwrap_or(false);
            Some(Drive {
                name: m.name().to_string(),
                path,
                removable,
            })
        })
        .collect();
    out.sort_by(|a, b| b.removable.cmp(&a.removable).then(a.name.cmp(&b.name)));
    out
}

/// Find a mounted drive by the name it was pinned under.
pub fn by_name(name: &str) -> Option<Drive> {
    mounted().into_iter().find(|d| d.name == name)
}

/// The folder to write into for a pinned drive, creating the subfolder if the
/// user asked for one and it is not there yet.
///
/// Returns `None` when the drive is not connected, so the caller can say so
/// rather than silently writing somewhere else.
pub fn target_dir(name: &str, subfolder: &str) -> Option<PathBuf> {
    let drive = by_name(name)?;
    let sub = subfolder.trim().trim_matches('/');
    if sub.is_empty() {
        return Some(drive.path);
    }
    let dir = drive.path.join(sub);
    if !dir.is_dir() && std::fs::create_dir_all(&dir).is_err() {
        return Some(drive.path); // fall back to the root rather than failing
    }
    Some(dir)
}

/// Free and total bytes on the filesystem holding `dir`, when the OS says.
pub fn space(dir: &std::path::Path) -> Option<(u64, u64)> {
    let info = gio::File::for_path(dir)
        .query_filesystem_info("filesystem::free,filesystem::size", gio::Cancellable::NONE)
        .ok()?;
    Some((
        info.attribute_uint64("filesystem::free"),
        info.attribute_uint64("filesystem::size"),
    ))
}

/// Whether this drive can be removed by the desktop at all.
///
/// A drive that can neither eject nor unmount is a fixed disk; offering to
/// eject it would be a button that always fails.
pub fn can_remove(name: &str) -> bool {
    let monitor = gio::VolumeMonitor::get();
    monitor
        .mounts()
        .into_iter()
        .filter(|m| !m.is_shadowed())
        .find(|m| m.name() == name)
        .map(|m| m.can_eject() || m.can_unmount())
        .unwrap_or(false)
}

/// Eject a drive by name, reporting the outcome once the desktop is done.
///
/// Eject and unmount are different operations and a drive may support only
/// one: a USB stick usually unmounts, an optical drive ejects. We prefer
/// eject where it is offered and fall back to unmount, so the caller does not
/// have to know which kind it has.
///
/// The flush is the point of the whole feature. Pulling a stick with dirty
/// buffers is how a print file arrives at the printer truncated, and the
/// truncation shows up as a failed print rather than as a copy error.
pub fn eject<F: Fn(Result<(), String>) + 'static>(name: &str, done: F) {
    let monitor = gio::VolumeMonitor::get();
    let Some(mount) = monitor
        .mounts()
        .into_iter()
        .filter(|m| !m.is_shadowed())
        .find(|m| m.name() == name)
    else {
        done(Err(format!("{name} is no longer connected")));
        return;
    };

    let op = gtk::gio::MountOperation::new();
    if mount.can_eject() {
        mount.eject_with_operation(
            gio::MountUnmountFlags::NONE,
            Some(&op),
            gio::Cancellable::NONE,
            move |res| done(res.map_err(|e| e.message().to_string())),
        );
    } else {
        mount.unmount_with_operation(
            gio::MountUnmountFlags::NONE,
            Some(&op),
            gio::Cancellable::NONE,
            move |res| done(res.map_err(|e| e.message().to_string())),
        );
    }
}
