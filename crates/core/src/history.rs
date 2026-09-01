//! Conversion history (§30).
//!
//! Records what was converted, where it went and whether it worked. Paths and
//! metadata only: files are never copied, so history costs almost nothing and
//! can never fill a disk.
//!
//! Entries survive the files they describe. A drive gets unplugged, an output
//! gets deleted, and the entry should still be readable rather than vanishing
//! or erroring, so availability is checked when it is read rather than stored.

use std::path::{Path, PathBuf};

/// How an attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    Failed,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Complete => "complete",
            Outcome::Failed => "failed",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "failed" => Outcome::Failed,
            _ => Outcome::Complete,
        }
    }
}

/// One conversion.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Seconds since the epoch.
    pub when: u64,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub from_format: String,
    pub to_format: String,
    pub layers: u32,
    pub outcome: Outcome,
    /// Why it failed, when it did.
    pub detail: String,
}

impl Entry {
    /// True when the output is still where it was left.
    pub fn output_exists(&self) -> bool {
        self.destination.is_file()
    }

    /// True when the source is still where it was.
    pub fn source_exists(&self) -> bool {
        self.source.is_file()
    }

    pub fn source_name(&self) -> String {
        name_of(&self.source)
    }

    pub fn destination_name(&self) -> String {
        name_of(&self.destination)
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Most recent first. Capped so the file cannot grow without bound.
const MAX_ENTRIES: usize = 200;

/// The stored history.
#[derive(Debug, Clone, Default)]
pub struct History {
    pub entries: Vec<Entry>,
}

impl History {
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
        Some(base.join("cheapazsla").join("history.tsv"))
    }

    /// Load, treating any unreadable or malformed line as absent rather than
    /// failing: a damaged history must never stop the program starting.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let entries = text
            .lines()
            .filter_map(parse_line)
            .take(MAX_ENTRIES)
            .collect();
        Self { entries }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Written into one buffer rather than collected from formatted
        // pieces, which allocates a string per entry and throws each away.
        let mut body = String::new();
        for e in self.entries.iter().take(MAX_ENTRIES) {
            use std::fmt::Write;
            let _ = writeln!(
                body,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                e.when,
                e.outcome.as_str(),
                e.from_format,
                e.to_format,
                e.layers,
                escape(&e.source.to_string_lossy()),
                escape(&e.destination.to_string_lossy()),
                escape(&e.detail),
            );
        }
        std::fs::write(path, body)
    }

    /// Add an entry at the front and persist.
    pub fn record(&mut self, entry: Entry) {
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_ENTRIES);
        let _ = self.save();
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.entries.len() {
            self.entries.remove(index);
            let _ = self.save();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        let _ = self.save();
    }
}

/// Tabs and newlines are the record separators, so they cannot appear in a
/// field. Paths legally may contain a tab, so they are escaped rather than
/// assumed safe.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn parse_line(line: &str) -> Option<Entry> {
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() < 7 {
        return None;
    }
    Some(Entry {
        when: f[0].parse().ok()?,
        outcome: Outcome::parse(f[1]),
        from_format: f[2].to_string(),
        to_format: f[3].to_string(),
        layers: f[4].parse().unwrap_or(0),
        source: PathBuf::from(unescape(f[5])),
        destination: PathBuf::from(unescape(f[6])),
        detail: f.get(7).map(|s| unescape(s)).unwrap_or_default(),
    })
}

/// Seconds since the epoch, for stamping a new entry.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> Entry {
        Entry {
            when: 1_700_000_000,
            source: PathBuf::from(format!("/in/{name}.sl1")),
            destination: PathBuf::from(format!("/out/{name}.goo")),
            from_format: "sl1".into(),
            to_format: "goo".into(),
            layers: 42,
            outcome: Outcome::Complete,
            detail: String::new(),
        }
    }

    #[test]
    fn newest_entries_come_first() {
        let mut h = History::default();
        h.entries.insert(0, entry("a"));
        h.entries.insert(0, entry("b"));
        assert_eq!(h.entries[0].source_name(), "b.sl1");
    }

    #[test]
    fn a_line_survives_a_round_trip() {
        let e = entry("model");
        let line = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            e.when,
            e.outcome.as_str(),
            e.from_format,
            e.to_format,
            e.layers,
            escape(&e.source.to_string_lossy()),
            escape(&e.destination.to_string_lossy()),
            escape(&e.detail)
        );
        let back = parse_line(&line).expect("parse");
        assert_eq!(back.source, e.source);
        assert_eq!(back.destination, e.destination);
        assert_eq!(back.layers, 42);
        assert_eq!(back.outcome, Outcome::Complete);
    }

    #[test]
    fn a_path_containing_a_tab_survives() {
        // Legal on Linux, and it would otherwise split the record.
        let mut e = entry("x");
        e.source = PathBuf::from("/in/we\tird.sl1");
        let line = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t",
            e.when,
            e.outcome.as_str(),
            e.from_format,
            e.to_format,
            e.layers,
            escape(&e.source.to_string_lossy()),
            escape(&e.destination.to_string_lossy()),
        );
        let back = parse_line(&line).expect("parse");
        assert_eq!(back.source, PathBuf::from("/in/we\tird.sl1"));
    }

    #[test]
    fn a_malformed_line_is_skipped_not_fatal() {
        assert!(parse_line("garbage").is_none());
        assert!(parse_line("").is_none());
        assert!(parse_line("1\tcomplete\tsl1").is_none());
    }

    #[test]
    fn missing_files_are_reported_rather_than_hidden() {
        let e = entry("gone");
        assert!(!e.output_exists(), "a deleted output must read as missing");
        assert!(!e.source_exists());
    }
}
