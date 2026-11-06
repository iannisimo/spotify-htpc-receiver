//! Writing single settings back to `config.toml` without disturbing the rest of the file.

use std::fmt;
use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, Value, value};

use super::PairingMode;

#[derive(Debug, Clone, PartialEq)]
pub enum Setting {
    BeatSensitivity(f32),
    PresetDurationS(f64),
    Shuffle(bool),
    DelayMs(u32),
    Pairing(PairingMode),
}

#[derive(Debug)]
pub enum SaveError {
    /// The file exists but does not parse; it is left untouched.
    Invalid(String),
    Io(std::io::Error),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::Invalid(_) => f.write_str("Not saved: config.toml has an error"),
            SaveError::Io(e) => write!(f, "Not saved: {e}"),
        }
    }
}

impl std::error::Error for SaveError {}

impl From<std::io::Error> for SaveError {
    fn from(e: std::io::Error) -> Self {
        SaveError::Io(e)
    }
}

/// Sets one value in the file at `path`, keeping comments and keys it does not know. A missing
/// file (and its folder) is created; a file that fails to parse is never overwritten.
pub fn save_setting(path: &Path, setting: Setting) -> Result<(), SaveError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| SaveError::Invalid(e.to_string()))?;

    let (section, key, mut new) = match setting {
        Setting::BeatSensitivity(v) => ("visualizer", "beat_sensitivity", one_decimal(v.into())),
        Setting::PresetDurationS(v) => ("visualizer", "preset_duration_s", one_decimal(v)),
        Setting::Shuffle(on) => ("visualizer", "shuffle", Value::from(on)),
        Setting::DelayMs(ms) => ("visualizer", "delay_ms", Value::from(i64::from(ms))),
        Setting::Pairing(mode) => ("daemon", "pairing", Value::from(mode.as_str())),
    };

    let table = doc
        .entry(section)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| SaveError::Invalid(format!("`{section}` is not a table")))?;
    // A table that only exists because of `[section.sub]` is written out once it gains a key.
    table.set_implicit(false);
    // Replace the value in place, so the comments around the key and after the old value stay.
    if let Some(Item::Value(old)) = table.get_mut(key) {
        *new.decor_mut() = old.decor().clone();
        *old = new;
    } else {
        table.insert(key, value(new));
    }

    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// A float written with one decimal (`1.2`, `30.0`), free of f32 noise.
fn one_decimal(v: f64) -> Value {
    Value::from(format!("{v:.1}").parse::<f64>().unwrap_or(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("htpc-save-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn commented(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("config.toml");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config/commented.toml");
        std::fs::copy(fixture, &path).unwrap();
        path
    }

    #[test]
    fn save_keeps_comments_and_unknown_keys() {
        let dir = temp_dir("comments");
        let path = commented(&dir);
        save_setting(&path, Setting::DelayMs(45)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("delay_ms = 45"), "{text}");
        for comment in [
            "# My settings",
            "# not a real key",
            "# Keep the TV in sync",
            "# audio lag",
            "# measured on the laptop",
        ] {
            assert!(text.contains(comment), "lost {comment:?}: {text}");
        }
        assert!(text.contains("foo = 1"), "{text}");
        assert!(text.contains("shuffle = true"), "{text}");
    }

    #[test]
    fn save_creates_missing_file() {
        let dir = temp_dir("missing");
        let path = dir.join("nested/config.toml");
        save_setting(&path, Setting::Shuffle(false)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[visualizer]"), "{text}");
        assert!(!load(&path).visualizer.shuffle);
    }

    #[test]
    fn save_refuses_invalid_file() {
        let dir = temp_dir("invalid");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[[[\n").unwrap();
        let err = save_setting(&path, Setting::DelayMs(1)).unwrap_err();
        assert!(matches!(err, SaveError::Invalid(_)), "{err:?}");
        assert_eq!(err.to_string(), "Not saved: config.toml has an error");
        assert_eq!(std::fs::read(&path).unwrap(), b"[[[\n");
    }

    #[test]
    fn save_reports_io_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("readonly");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = save_setting(&dir.join("config.toml"), Setting::Shuffle(true));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result, Err(SaveError::Io(_))), "{result:?}");
    }

    #[test]
    fn save_pairing_writes_daemon_table() {
        let dir = temp_dir("pairing");
        let path = dir.join("config.toml");
        save_setting(&path, Setting::Pairing(PairingMode::On)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[daemon]") && text.contains("pairing = \"on\""),
            "{text}"
        );
        assert_eq!(load(&path).daemon.pairing, PairingMode::On);
    }

    #[test]
    fn save_pairing_keeps_other_daemon_keys() {
        let dir = temp_dir("daemon-keys");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[daemon]\naddress = \"127.0.0.1:9191\"\ndevice_name = \"Den\"\n",
        )
        .unwrap();
        save_setting(&path, Setting::Pairing(PairingMode::On)).unwrap();
        let cfg = load(&path);
        assert_eq!(cfg.daemon.pairing, PairingMode::On);
        assert_eq!(cfg.daemon.address, "127.0.0.1:9191");
        assert_eq!(cfg.daemon.device_name.as_deref(), Some("Den"));
    }

    #[test]
    fn floats_have_one_decimal() {
        let dir = temp_dir("floats");
        let path = dir.join("config.toml");
        save_setting(&path, Setting::BeatSensitivity(1.1)).unwrap();
        save_setting(&path, Setting::PresetDurationS(30.0)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("beat_sensitivity = 1.1\n"), "{text}");
        assert!(text.contains("preset_duration_s = 30.0\n"), "{text}");
    }
}
