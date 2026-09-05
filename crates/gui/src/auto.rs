//! WatchDog: convert on sight, deliver on plug-in.
//!
//! Called WatchDog everywhere a person can see it, and `auto` everywhere in
//! here - the settings keys included, which are not renamed because renaming
//! them would throw away what anyone has already configured.
//!
//! Watching a folder and converting what lands in it is the one feature here
//! that acts without being asked, so the whole design is about being able to
//! see what it did and stop it doing the wrong thing.
//!
//! There are two stages, deliberately separate. A file that appears is
//! converted straight away, into a staging area this module owns; the result
//! is copied to the drive when the drive is there. Doing it in one step would
//! mean nothing happens while the drive is out, and a half-written file if it
//! is pulled mid-copy.
//!
//! Nothing here touches the interface, so the awkward parts - deciding a file
//! has finished being written, keeping the staging area from growing without
//! bound - can be tested without a window.

use cheapazsla_core::registry;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Where a converted file waits for its drive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Staging {
    /// On disk, under the user's data directory. Survives a reboot, which is
    /// the reason it is the default: a file converted and not yet collected
    /// is work, and work should not evaporate because the machine restarted.
    Disk,
    /// In memory, by way of /dev/shm. No disk writes at all, and gone on
    /// reboot - which is the trade, and it is stated where it is chosen.
    Ram,
    /// Nowhere. Nothing is converted until the drive is plugged in, and then
    /// it is converted and copied in one go. Costs a wait at the printer and
    /// stores not one byte in the meantime.
    OnDemand,
}

impl Staging {
    pub fn from_id(id: &str) -> Self {
        match id {
            "ram" => Staging::Ram,
            "wait" => Staging::OnDemand,
            _ => Staging::Disk,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Staging::Ram => "ram",
            Staging::OnDemand => "wait",
            Staging::Disk => "disk",
        }
    }
}

/// Memory the machine could give away right now, in MB.
///
/// `MemAvailable` rather than `MemFree`: free memory on a healthy Linux box is
/// nearly zero, because anything spare is doing something useful as cache.
/// Available is what could be handed over without swapping, which is the only
/// number that answers "is there room for this".
pub fn available_ram_mb() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb / 1024);
        }
    }
    None
}

/// Whether staging in memory is a reasonable thing to do right now.
///
/// Asked afresh each time rather than decided once at startup, because the
/// answer changes: a machine with room to spare at nine in the morning may be
/// running a slicer and a browser by lunchtime.
pub fn ram_is_sensible(budget_mb: u64, wanted_bytes: u64) -> bool {
    let wanted_mb = wanted_bytes.div_ceil(1024 * 1024);
    if wanted_mb > budget_mb {
        return false;
    }
    // Kept well clear of the edge. Filling the last of available memory to
    // save a disk write is a bad trade at any budget.
    available_ram_mb().is_some_and(|have| have > wanted_mb + 512)
}

/// Where a file can actually wait, given what was asked for and what this
/// machine can do about it.
///
/// Every choice here can be unavailable on somebody's computer, so none of
/// them is taken on trust. Memory needs /dev/shm, which a minimal system may
/// not mount, and needs room to spare that a small machine may not have. Disk
/// needs a data directory, which needs HOME, which is not guaranteed either.
///
/// When neither will do, the answer is to convert nothing until the drive is
/// plugged in. That is always possible: it stores nothing, so nothing can be
/// missing. A feature that cannot fall back to something is a feature that
/// breaks on a machine you have never seen.
pub fn resolve_staging(asked: Staging, budget_mb: u64, wanted_bytes: u64) -> Staging {
    let works = |mode: Staging| usable_dir(mode).is_some();
    match asked {
        Staging::OnDemand => Staging::OnDemand,
        Staging::Ram => {
            if works(Staging::Ram) && ram_is_sensible(budget_mb, wanted_bytes) {
                Staging::Ram
            } else if works(Staging::Disk) {
                Staging::Disk
            } else {
                Staging::OnDemand
            }
        }
        Staging::Disk => {
            if works(Staging::Disk) {
                Staging::Disk
            } else if works(Staging::Ram) && ram_is_sensible(budget_mb, wanted_bytes) {
                Staging::Ram
            } else {
                Staging::OnDemand
            }
        }
    }
}

/// The staging directory, made and proven writable.
///
/// A directory that exists is not the same as one that can be written to: a
/// full disk, a read-only home, a /dev/shm mounted noexec and tiny. The only
/// honest test is to write something, so it writes something.
pub fn usable_dir(mode: Staging) -> Option<PathBuf> {
    let dir = staging_dir(mode)?;
    std::fs::create_dir_all(&dir).ok()?;
    let probe = dir.join(".writable");
    std::fs::write(&probe, b"x").ok()?;
    let _ = std::fs::remove_file(&probe);
    Some(dir)
}

/// Where converted files wait.
pub fn staging_dir(mode: Staging) -> Option<PathBuf> {
    match mode {
        Staging::OnDemand => None,
        Staging::Ram => {
            let shm = Path::new("/dev/shm");
            shm.is_dir()
                .then(|| shm.join(format!("cheapazsla-{}", users_own_id())))
        }
        Staging::Disk => dirs_data().map(|d| d.join("pending")),
    }
}

fn users_own_id() -> String {
    // Two people on one machine must not share a staging directory in /dev/shm,
    // which is world-writable.
    std::env::var("UID")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "user".into())
}

fn dirs_data() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|d| d.join("cheapazsla"))
}

/// A file that has finished being written.
///
/// A slicer creates its output and then fills it over several seconds, so
/// "the file appeared" is not the same question as "the file is ready". This
/// asks whether size and modification time have both stopped moving, which is
/// the only signal available without the writing program's cooperation.
pub struct Settling {
    pub path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
    still_since: SystemTime,
    first_seen: SystemTime,
}

impl Settling {
    pub fn watch(path: PathBuf) -> Option<Self> {
        let meta = std::fs::metadata(&path).ok()?;
        Some(Self {
            path,
            size: meta.len(),
            modified: meta.modified().ok(),
            still_since: SystemTime::now(),
            first_seen: SystemTime::now(),
        })
    }

    /// Whether this has been going on long enough to be something else.
    ///
    /// A file that never stops changing is not a slice being exported - it is
    /// a log, or a download, or a virtual machine's disk. Watching it forever
    /// means waking up every second forever, which on a slow machine is a cost
    /// somebody pays for nothing.
    pub fn given_up(&self, after: Duration) -> bool {
        self.first_seen
            .elapsed()
            .map(|d| d > after)
            .unwrap_or(false)
    }

    /// Look again. True once it has been unchanged for `quiet`.
    pub fn settled(&mut self, quiet: Duration) -> bool {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return false;
        };
        let modified = meta.modified().ok();
        if meta.len() != self.size || modified != self.modified {
            self.size = meta.len();
            self.modified = modified;
            self.still_since = SystemTime::now();
            return false;
        }
        // A zero-length file is a file that has been created and not yet
        // written to, whatever its timestamps say.
        if self.size == 0 {
            return false;
        }
        self.still_since
            .elapsed()
            .map(|d| d >= quiet)
            .unwrap_or(false)
    }
}

/// What a staging directory currently holds.
pub struct Waiting {
    pub files: Vec<PathBuf>,
    pub bytes: u64,
}

pub fn waiting(dir: &Path) -> Waiting {
    let mut files = Vec::new();
    let mut bytes = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            bytes += meta.len();
            files.push(e.path());
        }
    }
    files.sort();
    Waiting { files, bytes }
}

/// Drop what has been waiting too long, and then the oldest of what is left
/// until the staging area is under its cap.
///
/// Both limits exist for the same reason: a converted file nobody collected is
/// not worth the disk it sits on, and an area that only ever grows is how
/// somebody loses forty gigabytes without knowing where it went.
pub fn prune(dir: &Path, cap_bytes: u64, keep: Duration) -> (usize, u64) {
    let mut dropped = 0;
    let mut freed = 0;

    let mut kept: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let when = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let stale = when.elapsed().map(|d| d > keep).unwrap_or(false);
        if stale {
            if std::fs::remove_file(e.path()).is_ok() {
                dropped += 1;
                freed += meta.len();
            }
            continue;
        }
        kept.push((e.path(), meta.len(), when));
    }

    // Oldest first, so what goes to make room is what has been waiting longest.
    kept.sort_by_key(|(_, _, when)| *when);
    let mut total: u64 = kept.iter().map(|(_, size, _)| size).sum();
    for (path, size, _) in kept {
        if total <= cap_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= size;
            dropped += 1;
            freed += size;
        }
    }
    (dropped, freed)
}

/// Move a staged file onto the drive, leaving nothing behind on success.
///
/// Written to a temporary name and renamed into place, so a drive pulled
/// mid-copy leaves an obvious fragment rather than a file the printer will
/// try to read. The staged copy is only dropped once the real one is there.
pub fn deliver(staged: &Path, into: &Path) -> Result<PathBuf, String> {
    let name = staged
        .file_name()
        .ok_or_else(|| "no file name".to_string())?;
    let final_path = into.join(name);
    let part = into.join(format!(".{}.part", name.to_string_lossy()));

    std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
    std::fs::copy(staged, &part).map_err(|e| e.to_string())?;
    std::fs::rename(&part, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        e.to_string()
    })?;
    std::fs::remove_file(staged).map_err(|e| e.to_string())?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "cheapazsla-auto-{tag}-{}-{:?}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            fs::create_dir_all(&p).expect("temp dir");
            Self(p)
        }
        fn file(&self, name: &str, bytes: usize) -> PathBuf {
            let p = self.0.join(name);
            fs::write(&p, vec![b'x'; bytes]).expect("write");
            p
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_still_being_written_is_not_ready() {
        let d = Dir::new("settle");
        let p = d.file("growing.sl1", 10);
        let mut s = Settling::watch(p.clone()).expect("watching");
        // Nothing has settled the instant it is seen.
        assert!(!s.settled(Duration::from_millis(50)));
        fs::write(&p, vec![b'x'; 4096]).expect("grow");
        assert!(!s.settled(Duration::from_millis(0)), "it just changed");
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            s.settled(Duration::from_millis(10)),
            "quiet long enough now"
        );
    }

    #[test]
    fn a_file_that_never_settles_is_eventually_abandoned() {
        let d = Dir::new("forever");
        let p = d.file("busy.sl1", 10);
        let s = Settling::watch(p).expect("watching");
        assert!(!s.given_up(Duration::from_secs(600)));
        assert!(s.given_up(Duration::from_nanos(1)));
    }

    #[test]
    fn an_empty_file_is_never_ready() {
        let d = Dir::new("empty");
        let p = d.file("nothing.sl1", 0);
        let mut s = Settling::watch(p).expect("watching");
        std::thread::sleep(Duration::from_millis(20));
        assert!(!s.settled(Duration::from_millis(1)));
    }

    #[test]
    fn the_cap_takes_the_oldest_first() {
        let d = Dir::new("cap");
        for name in ["a.goo", "b.goo", "c.goo"] {
            d.file(name, 1000);
            std::thread::sleep(Duration::from_millis(1100));
        }
        let (dropped, freed) = prune(&d.0, 2500, Duration::from_secs(3600));
        assert_eq!(dropped, 1);
        assert_eq!(freed, 1000);
        let left = waiting(&d.0);
        assert_eq!(left.files.len(), 2);
        assert!(
            !left.files.iter().any(|p| p.ends_with("a.goo")),
            "the oldest is the one that goes"
        );
    }

    #[test]
    fn nothing_is_dropped_while_it_fits() {
        let d = Dir::new("fits");
        d.file("a.goo", 100);
        d.file("b.goo", 100);
        assert_eq!(prune(&d.0, 10_000, Duration::from_secs(3600)), (0, 0));
        assert_eq!(waiting(&d.0).files.len(), 2);
    }

    #[test]
    fn delivery_leaves_no_staged_copy_and_no_part_file() {
        let from = Dir::new("from");
        let to = Dir::new("to");
        let staged = from.file("print.goo", 2048);
        let landed = deliver(&staged, &to.0).expect("delivered");
        assert!(landed.is_file());
        assert!(!staged.exists(), "the staged copy goes");
        let leftovers: Vec<_> = fs::read_dir(&to.0)
            .expect("read")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "no half-written file is left behind");
    }

    #[test]
    fn memory_that_will_not_fit_falls_back_to_disk() {
        // A budget smaller than the file rules memory out whatever the machine
        // has spare, and disk is the next thing tried rather than giving up.
        let landed = resolve_staging(Staging::Ram, 1, 500 * 1024 * 1024);
        assert_ne!(landed, Staging::Ram);
    }

    #[test]
    fn asking_to_wait_is_always_honoured() {
        // The one answer that cannot fail, so nothing may override it.
        assert_eq!(
            resolve_staging(Staging::OnDemand, 4096, 1024),
            Staging::OnDemand
        );
    }

    #[test]
    fn on_demand_stages_nowhere() {
        assert!(staging_dir(Staging::OnDemand).is_none());
        assert_eq!(Staging::from_id("wait"), Staging::OnDemand);
        assert_eq!(Staging::from_id("ram"), Staging::Ram);
        assert_eq!(Staging::from_id("anything else"), Staging::Disk);
    }

    #[test]
    fn memory_is_refused_when_the_budget_is_smaller_than_the_file() {
        assert!(!ram_is_sensible(10, 50 * 1024 * 1024));
    }
}

/// Whether a file that has turned up in the watched folder is one to convert.
///
/// Two ways to be no, and the second one matters more than it looks.
///
/// A format nothing here can read is not this program's business. And a file
/// that is *already in the format being written* is, when the folder being
/// watched is also the folder being written to, this arrangement's own output.
/// Convert it and the result lands in the folder as well, is noticed, and is
/// converted again - each pass leaving another copy behind. That is not a slow
/// leak. It fills the folder as fast as the disk will take it, under names
/// like "print (1) (1) (2).goo", and it does it while nobody is watching,
/// which is the whole reason this feature has to be careful.
///
/// Asked of a bare extension rather than a path so it can be checked in both
/// places that need it - when a file is noticed, and again before it is
/// converted - and tested without a folder or a window.
pub fn worth_converting(ext: &str, to: &str) -> bool {
    let Some(handler) = registry::by_extension(ext) else {
        return false;
    };
    let info = handler.info();
    info.capabilities.reads && info.id != to
}

#[cfg(test)]
mod loop_tests {
    use super::*;

    #[test]
    fn a_file_in_the_format_being_written_is_left_alone() {
        // The exact shape that filled a Downloads folder with copies: watch a
        // folder, write GOO into that same folder, and every GOO already there
        // - including the ones just written - looks like new work.
        assert!(!worth_converting("goo", "goo"));
        assert!(!worth_converting("sl1", "sl1"));
    }

    #[test]
    fn a_file_in_another_format_is_still_converted() {
        assert!(worth_converting("sl1", "goo"));
        assert!(worth_converting("goo", "ctb"));
    }

    #[test]
    fn a_format_nothing_can_read_is_not_our_business() {
        assert!(!worth_converting("txt", "goo"));
        assert!(!worth_converting("", "goo"));
    }

    #[test]
    fn the_extension_is_taken_case_insensitively() {
        // Slicers on other platforms write .GOO, and a capital letter is not a
        // different format.
        assert!(!worth_converting("GOO", "goo"));
        assert!(worth_converting("SL1", "goo"));
    }
}
