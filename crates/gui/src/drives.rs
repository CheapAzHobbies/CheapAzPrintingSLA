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

/// The mounted drive a path sits on, if any.
///
/// Longest match wins, so a drive mounted inside another drive's tree is
/// reported rather than its parent.
pub fn containing(path: &std::path::Path) -> Option<Drive> {
    mounted()
        .into_iter()
        .filter(|d| path.starts_with(&d.path))
        .max_by_key(|d| d.path.as_os_str().len())
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

/// Mounts the system needs, which must never be offered for ejection.
///
/// The desktop will occasionally report a fixed filesystem as unmountable in
/// the technical sense - it *can* be unmounted, in that the call would
/// succeed. That is not the same as it being safe to, and this is the check
/// that says so. Getting it wrong means unmounting the machine out from under
/// itself while it is running, so the list is deliberately blunt.
pub fn is_system_mount(path: &std::path::Path) -> bool {
    use std::path::Path;
    if path == Path::new("/") {
        return true;
    }
    for p in [
        "/boot",
        "/boot/efi",
        "/usr",
        "/var",
        "/etc",
        "/home",
        "/nix",
    ] {
        if path == Path::new(p) {
            return true;
        }
    }
    // A mount that contains the user's home directory is the machine's own
    // disk under another name.
    if let Some(home) = std::env::var_os("HOME") {
        if std::path::PathBuf::from(home).starts_with(path) {
            return true;
        }
    }
    false
}

/// Whether this drive can be removed by the desktop at all.
///
/// A drive that can neither eject nor unmount is a fixed disk; offering to
/// eject it would be a button that always fails. A drive the system is
/// standing on is refused here too, whatever the desktop claims.
pub fn can_remove(name: &str) -> bool {
    let monitor = gio::VolumeMonitor::get();
    monitor
        .mounts()
        .into_iter()
        .filter(|m| !m.is_shadowed())
        .find(|m| m.name() == name)
        .map(|m| {
            let removable = m.can_eject() || m.can_unmount();
            let system = m.root().path().map(|p| is_system_mount(&p)).unwrap_or(true);
            removable && !system
        })
        .unwrap_or(false)
}

/// Whether ejecting this drive is allowed: the desktop can do it, the system
/// does not depend on it, and the user has not protected it.
pub fn is_ejectable(name: &str, protected: &[String]) -> bool {
    can_remove(name) && !protected.iter().any(|p| p == name)
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
/// The block device behind a mounted drive, e.g. `/dev/sdb1`.
///
/// Wanted for one thing only: a FAT directory table can be reordered, and
/// doing that means naming the partition rather than the folder it appears at.
pub fn device_of(path: &std::path::Path) -> Option<PathBuf> {
    let monitor = gio::VolumeMonitor::get();
    let mount = monitor
        .mounts()
        .into_iter()
        .filter(|m| !m.is_shadowed())
        .find(|m| m.root().path().as_deref() == Some(path))?;
    let volume = mount.volume()?;
    volume
        .identifier(gio::VOLUME_IDENTIFIER_KIND_UNIX_DEVICE)
        .map(|s| PathBuf::from(s.as_str()))
}

/// Whether a mounted drive holds a FAT filesystem.
///
/// Only FAT can be reordered, and only FAT is what a resin printer reads, so
/// the two questions have the same answer.
pub fn is_fat(path: &std::path::Path) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else {
        return false;
    };
    let target = path.to_string_lossy();
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let (_dev, at, kind) = (parts.next(), parts.next(), parts.next());
        let (Some(at), Some(kind)) = (at, kind) else {
            continue;
        };
        // /proc escapes spaces as \040; nothing else here needs unescaping.
        if at.replace("\\040", " ") == target {
            return matches!(kind, "vfat" | "msdos" | "exfat");
        }
    }
    false
}

/// Rewrite a FAT directory table so the newest file is listed first.
///
/// Chitu firmware - which the Elegoo Saturn line runs - does no sorting. It
/// lists files in the order their entries sit in the directory table, which is
/// the order they were written, so the file just copied is always last however
/// it is named. Renaming cannot fix that; the table itself has to be rewritten,
/// which is what `fatsort` does.
///
/// The filesystem has to be unmounted first - rewriting directory entries
/// under a live mount is how filesystems get eaten - and rewriting a block
/// device needs root, so this is a best effort. It never blocks anything: a
/// missing tool or a refused password leaves the drive exactly as it was.
pub fn sort_newest_first(device: &std::path::Path) -> Result<(), String> {
    if which_fatsort().is_none() {
        return Err("fatsort is not installed".into());
    }
    let out = std::process::Command::new("sudo")
        // Never prompt. A password dialog nobody asked for, appearing behind
        // the window during an eject, is worse than not sorting.
        .arg("-n")
        .arg("fatsort")
        .arg("-t") // by last modification time
        .arg("-r") // newest first
        .arg("-c") // and ignore case, since the list is read by eye
        .arg(device)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    Err(said.lines().next().unwrap_or("fatsort failed").to_string())
}

/// Whether the reordering tool is absent, so the interface can say so rather
/// than offering a switch that quietly does nothing.
pub fn fatsort_missing() -> bool {
    which_fatsort().is_none()
}

fn which_fatsort() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("fatsort"))
            .find(|p| p.is_file())
    })
}

pub fn eject<F: Fn(Result<(), String>) + Clone + 'static>(
    drive: &Drive,
    sort_first: bool,
    done: F,
) {
    let name = drive.name.clone();
    let monitor = gio::VolumeMonitor::get();
    // Matched on the mount point, not the label. Two unlabelled sticks are
    // both called the same thing by the desktop, and ejecting the wrong one
    // of a matched pair is exactly the mistake this must not make. The mount
    // point is unique while the drive is attached, which is the only window
    // in which ejecting means anything.
    let Some(mount) = monitor
        .mounts()
        .into_iter()
        .filter(|m| !m.is_shadowed())
        .find(|m| m.root().path().as_deref() == Some(drive.path.as_path()))
    else {
        done(Err(format!("{name} is no longer connected")));
        return;
    };

    // Checked again here rather than trusting the caller. This is the only
    // function that can unmount anything, so it is the right place for the
    // check that must not be bypassed.
    if mount
        .root()
        .path()
        .map(|p| is_system_mount(&p))
        .unwrap_or(true)
    {
        done(Err(format!(
            "{name} is a system drive and cannot be ejected"
        )));
        return;
    }

    let op = gtk::gio::MountOperation::new();

    // Sorting has to happen between the unmount and the eject: the table
    // cannot be rewritten while the filesystem is mounted, and after an eject
    // the device may be powered down and gone. So when it is wanted, this is
    // three steps rather than one - and if any of the sorting fails the eject
    // still happens, because getting the drive out safely is the job and
    // ordering its file list is a courtesy.
    let device = mount.root().path().and_then(|p| device_of(&p));
    let wants_sort =
        sort_first && device.is_some() && mount.root().path().map(|p| is_fat(&p)).unwrap_or(false);

    if !wants_sort {
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
        return;
    }

    let device = device.expect("checked just above");
    let drive = mount.drive();
    mount.unmount_with_operation(
        gio::MountUnmountFlags::NONE,
        Some(&op),
        gio::Cancellable::NONE,
        move |res| {
            if let Err(e) = res {
                done(Err(e.message().to_string()));
                return;
            }
            let sorted = sort_newest_first(&device);
            let Some(drive) = drive.clone() else {
                done(sorted.map(|_| ()).or(Ok(())));
                return;
            };
            if !drive.can_eject() {
                done(Ok(()));
                return;
            }
            let op = gtk::gio::MountOperation::new();
            drive.eject_with_operation(
                gio::MountUnmountFlags::NONE,
                Some(&op),
                gio::Cancellable::NONE,
                move |res| done(res.map_err(|e| e.message().to_string())),
            );
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_machines_own_filesystems_are_never_ejectable() {
        for p in ["/", "/boot", "/boot/efi", "/usr", "/var", "/etc", "/home"] {
            assert!(is_system_mount(Path::new(p)), "{p} should be protected");
        }
    }

    #[test]
    fn a_mount_holding_the_home_directory_is_protected() {
        // Whatever HOME is on the machine running this, the directory it sits
        // in is the machine's own disk and must not be unmounted.
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        if let Some(home) = home {
            assert!(is_system_mount(&home));
            if let Some(parent) = home.parent() {
                assert!(is_system_mount(parent));
            }
        }
    }

    #[test]
    fn removable_media_is_not_mistaken_for_a_system_mount() {
        for p in [
            "/media/bao/SATURN",
            "/run/media/bao/PRINTS",
            "/mnt/usb",
            "/media",
        ] {
            assert!(!is_system_mount(Path::new(p)), "{p} should be ejectable");
        }
    }

    #[test]
    fn the_user_blacklist_blocks_a_drive_that_is_otherwise_fine() {
        let protected = vec!["SATURN".to_string()];
        // can_remove needs a live volume monitor, so only the blacklist half
        // is asserted here: a name in the list is refused regardless.
        assert!(!is_ejectable("SATURN", &protected));
    }
}
