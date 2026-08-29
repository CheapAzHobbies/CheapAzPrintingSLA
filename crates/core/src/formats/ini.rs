//! Minimal reader for the `key = value` files inside an SL1 archive.
//!
//! PrusaSlicer writes flat, section-less files with spaces around the
//! separator and values that may be empty. A full INI parser would be more
//! than this needs, and would accept shapes these files never contain.

use std::collections::BTreeMap;

/// Parse `key = value` lines. Later duplicates win, blank lines and `#`/`;`
/// comments are skipped, and values keep their internal spacing.
pub fn parse(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with(';')
            || line.starts_with('[')
        {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        out.insert(k.trim().to_string(), v.trim().to_string());
    }
    out
}

/// Look up a key and parse it, returning `None` when absent or unparseable.
/// Absent and malformed both mean "this file does not tell us", which the
/// model represents as `None` rather than a fabricated default (§13).
pub fn get<T: std::str::FromStr>(map: &BTreeMap<String, String>, key: &str) -> Option<T> {
    map.get(key).and_then(|v| {
        let v = v.trim();
        if v.is_empty() {
            None
        } else {
            v.parse().ok()
        }
    })
}

/// Look up a non-empty string value.
pub fn get_str(map: &BTreeMap<String, String>, key: &str) -> Option<String> {
    map.get(key)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spaced_pairs() {
        let m = parse("expTime = 10\nlayerHeight = 0.05\n");
        assert_eq!(m.get("expTime").map(String::as_str), Some("10"));
        assert_eq!(get::<f32>(&m, "layerHeight"), Some(0.05));
    }

    #[test]
    fn empty_values_read_as_absent() {
        // Real SL1 files ship `materialName = ` with nothing after it.
        let m = parse("materialName = \nprintProfile =\n");
        assert_eq!(get_str(&m, "materialName"), None);
        assert_eq!(get_str(&m, "printProfile"), None);
    }

    #[test]
    fn comments_sections_and_junk_are_skipped() {
        let m = parse("# note\n; note\n[section]\nnot a pair\nkey = value\n");
        assert_eq!(m.len(), 1);
        assert_eq!(get_str(&m, "key").as_deref(), Some("value"));
    }

    #[test]
    fn a_malformed_number_is_absent_rather_than_zero() {
        let m = parse("expTime = banana\n");
        assert_eq!(
            get::<f32>(&m, "expTime"),
            None,
            "must not silently become 0.0"
        );
    }

    #[test]
    fn values_may_contain_equals_and_spaces() {
        let m = parse("fileCreationTimestamp = 2026-08-25 at 05:40:32 UTC\n");
        assert_eq!(
            get_str(&m, "fileCreationTimestamp").as_deref(),
            Some("2026-08-25 at 05:40:32 UTC")
        );
    }
}
