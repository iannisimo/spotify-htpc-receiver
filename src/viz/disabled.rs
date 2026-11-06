//! The presets the user switched off: `disabled-presets.txt` next to `config.toml`.
//!
//! One path per line, relative to the preset folder; a trailing `/` disables a whole folder.
//! Lines that name nothing in the current collection are ignored when loading but kept when
//! saving, so swapping preset collections back and forth loses nothing.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisabledPresets {
    entries: BTreeSet<String>,
}

impl DisabledPresets {
    /// A missing or unreadable file gives an empty set (unreadable also logs a warning).
    pub fn load(path: &Path) -> DisabledPresets {
        match std::fs::read_to_string(path) {
            Ok(text) => DisabledPresets::from_lines(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => DisabledPresets::default(),
            Err(e) => {
                log::warn!("cannot read {}: {e}; no presets disabled", path.display());
                DisabledPresets::default()
            }
        }
    }

    pub fn from_lines(text: &str) -> DisabledPresets {
        DisabledPresets {
            entries: text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let mut text = String::new();
        for entry in &self.entries {
            text.push_str(entry);
            text.push('\n');
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// True if `relative` (a preset path below the preset folder) or any folder above it is
    /// listed.
    pub fn is_disabled(&self, relative: &Path) -> bool {
        let rel = relative.to_string_lossy();
        if self.entries.contains(rel.as_ref()) {
            return true;
        }
        rel.match_indices('/')
            .any(|(i, _)| self.entries.contains(&rel[..=i]))
    }

    pub fn insert(&mut self, entry: impl Into<String>) {
        self.entries.insert(entry.into());
    }

    pub fn contains(&self, entry: &str) -> bool {
        self.entries.contains(entry)
    }

    pub fn entries(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_folders_disable_their_presets() {
        let d = DisabledPresets::from_lines("A/\nB/b1.milk\n\n  C/sub/  \n");
        assert!(d.is_disabled(Path::new("A/a1.milk")));
        assert!(d.is_disabled(Path::new("A/sub/a2.milk")));
        assert!(d.is_disabled(Path::new("B/b1.milk")));
        assert!(!d.is_disabled(Path::new("B/b2.milk")));
        assert!(d.is_disabled(Path::new("C/sub/x.milk")));
        assert!(!d.is_disabled(Path::new("C/x.milk")));
        assert!(!d.is_disabled(Path::new("AA/x.milk")));
    }

    #[test]
    fn save_and_load_round_trip_sorted() {
        let dir = std::env::temp_dir().join(format!("htpc-disabled-{}", std::process::id()));
        let path = dir.join("nested/disabled-presets.txt");
        let d = DisabledPresets::from_lines("b.milk\nA/\n");
        d.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A/\nb.milk\n");
        assert_eq!(DisabledPresets::load(&path), d);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_empty() {
        assert!(DisabledPresets::load(Path::new("/nonexistent/disabled.txt")).is_empty());
    }
}
